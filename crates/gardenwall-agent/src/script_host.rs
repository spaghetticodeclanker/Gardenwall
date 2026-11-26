//! Runs code-mode programs in the sandboxed `gardenwall-js` helper
//! (ADR-014 §2.1, ADR-012).
//!
//! The helper gets its own OS sandbox: it can read its own folder and
//! nothing else, has no network, and can't start processes. It talks to the
//! core over stdin and stdout only, one JSON message per line. Everything
//! it sends is untrusted: lines are bounded and parsed strictly here, and
//! a tool call it asks for is only a request (the agent decides).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gardenwall_sandbox::{
    Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy, SandboxedProcess,
};
use serde_json::{Value, json};

use crate::agent::{BoxFuture, ScriptHost, ScriptRun, ScriptStep};

/// Longest line accepted from the helper (a `write` call carries a file).
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// How long a program may compute between two messages. Time the core
/// spends running a call or waiting for the user doesn't count.
const SCRIPT_TIME: Duration = Duration::from_secs(10);
const MEMORY_BYTES: u64 = 256 << 20;
/// Longest tool name a program may ask for.
const MAX_NAME_BYTES: usize = 64;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
enum FromHelper {
    Call { name: String, args: Value },
    Done { value: String, logs: String },
    Failed { message: String, logs: String },
}

/// Parse one line from the helper. Anything unexpected is an error.
pub fn parse_step(line: &[u8]) -> Result<ScriptStep, String> {
    const BAD: &str = "The program sent something that isn't a valid message.";
    if line.len() > MAX_LINE_BYTES {
        return Err(BAD.into());
    }
    match serde_json::from_slice(line).map_err(|_| BAD.to_string())? {
        FromHelper::Call { name, args } => {
            if name.is_empty() || name.len() > MAX_NAME_BYTES || !args.is_object() {
                return Err(BAD.into());
            }
            Ok(ScriptStep::Call { name, args })
        }
        FromHelper::Done { value, logs } => Ok(ScriptStep::Done { value, logs }),
        FromHelper::Failed { message, logs } => Ok(ScriptStep::Failed { message, logs }),
    }
}

/// Starts each program in the helper, inside one sandbox kept for the
/// session.
pub struct SandboxScriptHost {
    helper: PathBuf,
    journal: Option<PathBuf>,
    script_time: Duration,
    sandbox: Arc<Mutex<Option<Sandbox>>>,
}

impl SandboxScriptHost {
    /// `helper` is the `gardenwall-js` executable, alone in its folder (the
    /// sandbox is granted that folder). `journal` is where the sandbox
    /// records the access it grants, so a crash can't leave it behind.
    pub fn new(helper: PathBuf, journal: Option<PathBuf>) -> Self {
        Self {
            helper,
            journal,
            script_time: SCRIPT_TIME,
            sandbox: Arc::default(),
        }
    }

    /// Change how long a program may compute between two messages.
    pub fn with_script_time(mut self, time: Duration) -> Self {
        self.script_time = time;
        self
    }

    fn policy(&self) -> Result<SandboxPolicy, String> {
        let dir = self
            .helper
            .parent()
            .ok_or("the program runner has no folder")?
            .to_path_buf();
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut env = Vec::new();
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            env.push(("SystemRoot".into(), root));
        }
        Ok(SandboxPolicy {
            profile: "Gardenwall.Script".into(),
            grants: vec![PathGrant {
                path: dir.clone(),
                access: Access::ReadExecute,
            }],
            network: Network::Denied,
            limits: Limits {
                memory_bytes: Some(MEMORY_BYTES),
                max_processes: 1,
                wall_time: None,
            },
            env,
            working_dir: Some(dir),
            grant_journal_dir: self.journal.clone(),
        })
    }
}

impl ScriptHost for SandboxScriptHost {
    fn start(&self, code: &str, tools: &[String]) -> Result<Box<dyn ScriptRun>, String> {
        let mut guard = self
            .sandbox
            .lock()
            .map_err(|_| "the program sandbox is unavailable".to_string())?;
        if guard.is_none() {
            // Fail closed: without a sandbox no program runs.
            let sandbox = Sandbox::new(self.policy()?)
                .map_err(|e| format!("the program sandbox failed: {e}"))?;
            *guard = Some(sandbox);
        }
        let sandbox = guard.as_ref().ok_or("the program sandbox is unavailable")?;
        let mut child = sandbox
            .spawn(&self.helper, &[])
            .map_err(|e| format!("the program runner failed to start: {e}"))?;
        let stdin = child.stdin.take().ok_or("no channel to the program")?;
        let stdout = child.stdout.take().ok_or("no channel from the program")?;
        if let Some(mut stderr) = child.stderr.take() {
            // Never read: drained so the helper can't block on it.
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            });
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take(MAX_LINE_BYTES as u64 + 1)
                    .read_until(b'\n', &mut line);
                match read {
                    Ok(n) if n > 0 && line.len() <= MAX_LINE_BYTES => {
                        if tx.send(line).is_err() {
                            return;
                        }
                    }
                    // End of output, an error, or a line that is too long.
                    _ => return,
                }
            }
        });
        let mut first = serde_json::to_vec(&json!({ "run": code, "tools": tools }))
            .map_err(|e| e.to_string())?;
        first.push(b'\n');
        let run = Run(Arc::new(Mutex::new(Inner {
            child,
            stdin: Some(stdin),
            lines: rx,
            sandbox: self.sandbox.clone(),
            script_time: self.script_time,
            over: false,
        })));
        if let Ok(mut inner) = run.0.lock() {
            inner.send(&first)?;
        }
        Ok(Box::new(run))
    }
}

struct Inner {
    child: SandboxedProcess,
    stdin: Option<std::fs::File>,
    lines: Receiver<Vec<u8>>,
    sandbox: Arc<Mutex<Option<Sandbox>>>,
    script_time: Duration,
    over: bool,
}

impl Inner {
    fn send(&mut self, line: &[u8]) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the program has ended")?;
        stdin
            .write_all(line)
            .and_then(|()| stdin.flush())
            .map_err(|_| "the program stopped listening".to_string())
    }

    /// End the helper and anything else in its sandbox.
    fn end(&mut self) {
        if self.over {
            return;
        }
        self.over = true;
        self.stdin = None;
        if let Ok(guard) = self.sandbox.lock()
            && let Some(sandbox) = guard.as_ref()
        {
            let _ = sandbox.terminate_all();
        }
        let _ = self.child.kill();
        let _ = self.child.wait_timeout(Some(Duration::from_secs(5)));
    }

    fn step(&mut self, reply: Option<(String, bool)>) -> ScriptStep {
        let failed = |message: &str| ScriptStep::Failed {
            message: message.to_string(),
            logs: String::new(),
        };
        if self.over {
            return failed("The program has ended.");
        }
        if let Some((result, error)) = reply {
            let sent = serde_json::to_vec(&json!({ "result": result, "error": error }))
                .map_err(|e| e.to_string())
                .and_then(|mut line| {
                    line.push(b'\n');
                    self.send(&line)
                });
            if sent.is_err() {
                self.end();
                return failed("The program ended unexpectedly.");
            }
        }
        let step = match self.lines.recv_timeout(self.script_time) {
            Ok(line) => parse_step(&line).unwrap_or_else(|e| failed(&e)),
            Err(RecvTimeoutError::Timeout) => failed(&format!(
                "It ran for more than {} seconds without finishing or calling a tool.",
                self.script_time.as_secs()
            )),
            // The helper exited without a last message: it crashed, ran
            // out of memory, or sent a line that was too long.
            Err(RecvTimeoutError::Disconnected) => {
                failed("It ended unexpectedly (it may have run out of memory).")
            }
        };
        if !matches!(step, ScriptStep::Call { .. }) {
            self.end();
        }
        step
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.end();
    }
}

struct Run(Arc<Mutex<Inner>>);

impl ScriptRun for Run {
    fn next<'a>(&'a mut self, reply: Option<(String, bool)>) -> BoxFuture<'a, ScriptStep> {
        let inner = self.0.clone();
        Box::pin(async move {
            let lost = || ScriptStep::Failed {
                message: "The program runner failed.".into(),
                logs: String::new(),
            };
            crate::sandbox_shell::run_on_thread(move || match inner.lock() {
                Ok(mut inner) => inner.step(reply),
                Err(_) => lost(),
            })
            .await
            .unwrap_or_else(lost)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_lines_are_parsed_strictly() {
        assert_eq!(
            parse_step(br#"{"call":{"name":"read","args":{"path":"a"}}}"#),
            Ok(ScriptStep::Call {
                name: "read".into(),
                args: json!({ "path": "a" })
            })
        );
        assert_eq!(
            parse_step(br#"{"done":{"value":"v","logs":"l"}}"#),
            Ok(ScriptStep::Done {
                value: "v".into(),
                logs: "l".into()
            })
        );
        for bad in [
            &br#"{"call":{"name":"read","args":{},"x":1}}"#[..],
            br#"{"call":{"name":"read","args":"a.txt"}}"#,
            br#"{"call":{"name":"","args":{}}}"#,
            br#"{"done":{"value":"v"}}"#,
            br#"{"other":{}}"#,
            b"not json",
            b"",
        ] {
            assert!(parse_step(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let long = format!(r#"{{"call":{{"name":"{}","args":{{}}}}}}"#, "n".repeat(65));
        assert!(parse_step(long.as_bytes()).is_err());
    }
}
