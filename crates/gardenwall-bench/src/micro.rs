//! S4: micro-benchmarks of the agent's plumbing. No model is needed, so
//! these are quick and could later run in CI. Each measure is repeated
//! and reported as median and p95 (milliseconds unless noted).

use std::path::Path;
use std::time::Instant;

use gardenwall_agent::agent::{Agent, Shell};
use gardenwall_agent::prompt::Skill;
use gardenwall_agent::tools::{self, ToolContext, ToolOutcome, Workspaces};
use gardenwall_policy::paths::{RelPath, classify};
use gardenwall_policy::tools::SessionPolicy;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{self, Spread};
use crate::fixtures::{Generator, Rng};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measure {
    /// Stable name, compared across runs (e.g. `shell.pwsh.command`).
    pub name: String,
    pub unit: String,
    pub spread: Option<Spread>,
    pub note: Option<String>,
}

fn measure(name: &str, values: &[f64]) -> Measure {
    let m = Measure {
        name: name.into(),
        unit: "ms".into(),
        spread: common::spread(values),
        note: None,
    };
    if let Some(s) = &m.spread {
        eprintln!(
            "[micro] {name:<34} median {:>9.3} ms   p95 {:>9.3} ms",
            s.median, s.p95
        );
    }
    m
}

fn failed(name: &str, why: String) -> Measure {
    eprintln!("[micro] {name:<34} skipped: {why}");
    Measure {
        name: name.into(),
        unit: "ms".into(),
        spread: None,
        note: Some(why),
    }
}

pub struct MicroOptions {
    /// Repetitions for the quick measures (slow ones use fewer).
    pub reps: usize,
    /// Size of the large repository (files).
    pub repo_files: usize,
    /// Large files added to it (10 MB each).
    pub big_files: usize,
}

pub fn run_micro(o: &MicroOptions) -> Result<Vec<Measure>, String> {
    let dir = common::TempDir::new("micro")?;
    let mut out = Vec::new();
    shells(&dir.0, o, &mut out);
    #[cfg(windows)]
    reconcile(&dir.0, &mut out);
    file_tools(&dir.0, o, &mut out)?;
    policy(o, &mut out);
    store(&dir.0, o, &mut out);
    prompt(&dir.0, o, &mut out);
    Ok(out)
}

// ---------- sandbox and shells ----------

#[cfg(any(windows, target_os = "linux"))]
fn shells(dir: &Path, o: &MicroOptions, out: &mut Vec<Measure>) {
    use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};
    let work = dir.join("shell-work");
    let _ = std::fs::create_dir_all(&work);
    let _ = std::fs::write(work.join("big.txt"), "x".repeat(1 << 20));
    let kinds: Vec<(&str, ShellKind)> = if cfg!(windows) {
        vec![
            ("pwsh", ShellKind::PowerShell),
            ("ps51", ShellKind::WindowsPowerShell),
            ("cmd", ShellKind::Cmd),
        ]
    } else {
        vec![("sh", ShellKind::PowerShell)]
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    for (label, kind) in kinds {
        if label == "pwsh" && gardenwall_agent::sandbox_shell::find_powershell7().is_none() {
            out.push(failed(
                "shell.pwsh.session",
                "PowerShell 7 isn't installed".into(),
            ));
            continue;
        }
        let make = || {
            SandboxShell::new(
                std::slice::from_ref(&work),
                &dir.join("shell-scratch"),
                &[],
                Some(dir.join("shell-journal")),
                kind,
            )
        };
        let mut setups = Vec::new();
        let mut shell = None;
        for _ in 0..o.reps.min(10) {
            let t = Instant::now();
            match make() {
                Ok(s) => {
                    setups.push(common::ms(t.elapsed()));
                    shell = Some(s);
                }
                Err(e) => {
                    out.push(failed(&format!("shell.{label}.session"), e));
                    break;
                }
            }
        }
        out.push(measure(&format!("shell.{label}.session"), &setups));
        let Some(shell) = shell else { continue };
        // A command alone, one that runs a program (as agents mostly do:
        // python, git, tests), and one with 1 MB of output.
        let (noop, program, big) = match label {
            "cmd" => ("exit 0", "whoami >nul", "type big.txt"),
            "sh" => ("true", "id >/dev/null", "cat big.txt"),
            _ => ("exit 0", "whoami | Out-Null", "Get-Content -Raw big.txt"),
        };
        for (what, cmd) in [("command", noop), ("program", program), ("output-1mb", big)] {
            let times: Vec<f64> = (0..o.reps.min(20))
                .filter_map(|_| {
                    let t = Instant::now();
                    rt.block_on(shell.run(cmd, None)).ok()?;
                    Some(common::ms(t.elapsed()))
                })
                .collect();
            out.push(measure(&format!("shell.{label}.{what}"), &times));
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn shells(_dir: &Path, _o: &MicroOptions, out: &mut Vec<Measure>) {
    out.push(failed("shell", "no sandbox on this platform".into()));
}

/// Startup cleanup of grants left by a crash: 50 stale grants, left by a
/// child process that exits without cleaning up (as a crash would).
#[cfg(windows)]
fn reconcile(dir: &Path, out: &mut Vec<Measure>) {
    let journal = dir.join("reconcile-journal");
    let mut times = Vec::new();
    for rep in 0..3 {
        let status = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg("internal-leak-grants")
                .arg(&journal)
                .arg(dir)
                .arg(rep.to_string())
                .status()
        });
        if !status.is_ok_and(|s| s.success()) {
            out.push(failed(
                "sandbox.reconcile-50",
                "couldn't leave stale grants".into(),
            ));
            return;
        }
        let t = Instant::now();
        match gardenwall_sandbox::reconcile_grant_journals(&journal) {
            Ok(r) if r.revoked == 50 => times.push(common::ms(t.elapsed())),
            Ok(r) => {
                out.push(failed(
                    "sandbox.reconcile-50",
                    format!(
                        "revoked {} of 50 ({} in use, {} kept)",
                        r.revoked, r.in_use, r.kept
                    ),
                ));
                return;
            }
            Err(e) => {
                out.push(failed("sandbox.reconcile-50", e.to_string()));
                return;
            }
        }
    }
    out.push(measure("sandbox.reconcile-50", &times));
}

/// The child half of [`reconcile`]: grant 50 folders and exit without
/// revoking (the journal stays behind).
#[cfg(windows)]
pub fn leak_grants(journal: &Path, dir: &Path, rep: &str) -> Result<(), String> {
    use gardenwall_sandbox::{Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy};
    let grants: Vec<PathGrant> = (0..50)
        .map(|i| {
            let p = dir.join(format!("reconcile-{rep}-{i}"));
            let _ = std::fs::create_dir_all(&p);
            PathGrant {
                path: p,
                access: Access::ReadWrite,
            }
        })
        .collect();
    let sandbox = Sandbox::new(SandboxPolicy {
        profile: format!("Gardenwall.Bench.Reconcile{rep}"),
        grants,
        network: Network::Denied,
        limits: Limits {
            memory_bytes: Some(64 << 20),
            max_processes: 1,
            wall_time: None,
        },
        env: vec![],
        working_dir: None,
        grant_journal_dir: Some(journal.to_path_buf()),
    })
    .map_err(|e| e.to_string())?;
    std::mem::forget(sandbox);
    std::process::exit(0)
}

// ---------- file tools ----------

fn exec(
    ws: &mut Workspaces,
    policy: &SessionPolicy,
    name: &str,
    args: serde_json::Value,
) -> Result<(), String> {
    let mut ctx = ToolContext {
        workspaces: ws,
        policy,
    };
    match tools::execute(name, &args, &mut ctx) {
        ToolOutcome::Done {
            is_error: false, ..
        } => Ok(()),
        ToolOutcome::Done { text, .. } => Err(text.chars().take(200).collect()),
        ToolOutcome::NeedsApproval { .. } => Ok(()),
    }
}

fn file_tools(dir: &Path, o: &MicroOptions, out: &mut Vec<Measure>) -> Result<(), String> {
    let repo = dir.join("repo");
    eprintln!(
        "[micro] generating a repository of {} files and {} × 10 MB …",
        o.repo_files, o.big_files
    );
    Generator::PythonPackage {
        dir: "src/app".into(),
        files: o.repo_files,
        seed: 42,
    }
    .write(&repo)
    .map_err(|e| e.to_string())?;
    let mut rng = Rng::new(7);
    let line: String = (0..12)
        .map(|_| rng.pick(crate::fixtures::WORDS))
        .collect::<Vec<_>>()
        .join(" ");
    for i in 0..o.big_files {
        let mut text = String::with_capacity(10 << 20);
        let mut n = 0;
        while text.len() < 10 << 20 {
            text.push_str(&format!("{n:08} {line}\n"));
            n += 1;
        }
        std::fs::write(repo.join(format!("data-{i:02}.log")), text).map_err(|e| e.to_string())?;
    }
    std::fs::write(repo.join("small.txt"), "alpha\nbeta\ngamma\n").map_err(|e| e.to_string())?;
    let mid = (0..60)
        .map(|i| format!("line {i} {line}\n"))
        .collect::<String>()
        + "TARGET = 1\n";
    std::fs::write(repo.join("mid.py"), &mid).map_err(|e| e.to_string())?;

    let mut ws = Workspaces::new(dir.join("checkpoints"));
    let id = ws.grant(&repo)?;
    let policy = SessionPolicy {
        workspaces: [id].into_iter().collect(),
        sandbox_available: false,
        ..SessionPolicy::default()
    };
    let reps = o.reps.min(20);
    let run = |name: &str, tool: &str, args: serde_json::Value, ws: &mut Workspaces| -> Measure {
        let mut times = Vec::new();
        for _ in 0..reps {
            let t = Instant::now();
            if let Err(e) = exec(ws, &policy, tool, args.clone()) {
                return failed(name, e);
            }
            times.push(common::ms(t.elapsed()));
        }
        measure(name, &times)
    };
    out.push(run(
        "tools.read.small",
        "read",
        json!({"path": "small.txt"}),
        &mut ws,
    ));
    if o.big_files > 0 {
        out.push(run(
            "tools.read.10mb-truncated",
            "read",
            json!({"path": "data-00.log"}),
            &mut ws,
        ));
        out.push(run(
            "tools.read.10mb-offset",
            "read",
            json!({"path": "data-00.log", "offset": 100_000, "limit": 200}),
            &mut ws,
        ));
    }
    out.push(run(
        "tools.grep.repo",
        "grep",
        json!({"pattern": "def compute_[a-z]+_7", "path": "src"}),
        &mut ws,
    ));
    out.push(run(
        "tools.find.repo",
        "find",
        json!({"pattern": "**/*.py"}),
        &mut ws,
    ));
    out.push(run(
        "tools.ls.repo",
        "ls",
        json!({"path": "src/app/amber"}),
        &mut ws,
    ));
    // Edits flip a value back and forth, so every call changes the file.
    let mut times = Vec::new();
    for i in 0..reps {
        let (from, to) = if i % 2 == 0 {
            ("TARGET = 1", "TARGET = 2")
        } else {
            ("TARGET = 2", "TARGET = 1")
        };
        let t = Instant::now();
        if let Err(e) = exec(
            &mut ws,
            &policy,
            "edit",
            json!({"path": "mid.py", "edits": [{"oldText": from, "newText": to}]}),
        ) {
            out.push(failed("tools.edit.one", e));
            break;
        }
        times.push(common::ms(t.elapsed()));
    }
    out.push(measure("tools.edit.one", &times));
    // A 50 MB write is checkpointed; then undone.
    let big = "y".repeat(50 << 20);
    std::fs::write(repo.join("big50.bin.txt"), "original").map_err(|e| e.to_string())?;
    let (mut writes, mut undos) = (Vec::new(), Vec::new());
    for i in 0..3 {
        ws.begin_turn(&format!("bench {i}"));
        let t = Instant::now();
        if let Err(e) = exec(
            &mut ws,
            &policy,
            "write",
            json!({"path": "big50.bin.txt", "content": big}),
        ) {
            out.push(failed("checkpoint.write-50mb", e));
            return Ok(());
        }
        writes.push(common::ms(t.elapsed()));
        let t = Instant::now();
        ws.undo_latest(id)?;
        undos.push(common::ms(t.elapsed()));
    }
    out.push(measure("checkpoint.write-50mb", &writes));
    out.push(measure("checkpoint.undo-50mb", &undos));
    // A read outside the workspace: canonicalised and refused (approval).
    let outside = dir.join("outside.txt");
    let _ = std::fs::write(&outside, "secret");
    out.push(run(
        "tools.read.outside-path",
        "read",
        json!({"path": outside.display().to_string()}),
        &mut ws,
    ));
    Ok(())
}

// ---------- policy ----------

fn policy(o: &MicroOptions, out: &mut Vec<Measure>) {
    let mut rng = Rng::new(3);
    let paths: Vec<String> = (0..10_000)
        .map(|i| match i % 10 {
            0 => ".env".into(),
            1 => format!(".github/workflows/{}.yml", rng.pick(crate::fixtures::WORDS)),
            2 => format!("src/{}/id_rsa", rng.pick(crate::fixtures::WORDS)),
            _ => format!(
                "src/{}/{}_{i}.py",
                rng.pick(crate::fixtures::WORDS),
                rng.pick(crate::fixtures::WORDS)
            ),
        })
        .collect();
    let times = common::time_n(o.reps.min(20), || {
        paths
            .iter()
            .filter_map(|p| RelPath::parse(p).ok())
            .map(|p| classify(&p))
            .filter(|c| !matches!(c, gardenwall_policy::paths::PathClass::Normal))
            .count()
    });
    out.push(measure("policy.classify-10k-paths", &times));
}

// ---------- store ----------

fn store(dir: &Path, o: &MicroOptions, out: &mut Vec<Measure>) {
    use gardenwall_store::approvals::{ApprovalKind, ApprovalStore, Outcome};
    use gardenwall_store::conversations::{ConversationStore, MessageRole, NewMessage};
    use gardenwall_store::crypto::FieldCipher;

    let key = zeroize::Zeroizing::new([7u8; 32]);
    let path = dir.join("conversations.db");
    let result = (|| -> Result<(), String> {
        let mut store = ConversationStore::open_with(&path, Some(FieldCipher::new(&key)))
            .map_err(|e| e.to_string())?;
        let msg = |role, text: &str| NewMessage {
            role,
            content: text.into(),
            reasoning: String::new(),
            stats: None,
        };
        let t = Instant::now();
        let mut ids = Vec::new();
        for i in 0..1000 {
            let c = store
                .create("You are helpful.")
                .map_err(|e| e.to_string())?;
            store
                .append(
                    &c.id,
                    msg(
                        MessageRole::User,
                        &format!("Question {i} about the harbor."),
                    ),
                )
                .map_err(|e| e.to_string())?;
            store
                .append(
                    &c.id,
                    msg(
                        MessageRole::Assistant,
                        "An answer of moderate length about it.",
                    ),
                )
                .map_err(|e| e.to_string())?;
            ids.push(c.id);
        }
        let mut m = measure("store.create-1000-chats-total", &[common::ms(t.elapsed())]);
        m.note = Some("one run: 1,000 chats with 2 messages each".into());
        out.push(m);
        out.push(measure(
            "store.list-1000-chats",
            &common::time_n(o.reps.min(20), || {
                store.list().map(|l| l.len()).unwrap_or(0)
            }),
        ));
        let long = &ids[0];
        for i in 0..1000 {
            store
                .append(
                    long,
                    msg(
                        MessageRole::User,
                        &format!("Follow-up {i}: {}", "detail ".repeat(20)),
                    ),
                )
                .map_err(|e| e.to_string())?;
            store
                .append(
                    long,
                    msg(
                        MessageRole::Assistant,
                        &"A longer reply with several sentences. ".repeat(8),
                    ),
                )
                .map_err(|e| e.to_string())?;
        }
        drop(store);
        let store = ConversationStore::open_with(&path, Some(FieldCipher::new(&key)))
            .map_err(|e| e.to_string())?;
        out.push(measure(
            "store.load-2000-message-chat",
            &common::time_n(o.reps.min(10), || {
                store.get(long).map(|c| c.messages.len()).unwrap_or(0)
            }),
        ));
        Ok(())
    })();
    if let Err(e) = result {
        out.push(failed("store.conversations", e));
    }
    let approvals = (|| -> Result<Vec<f64>, String> {
        let now = || common::now_unix() as i64 * 1000;
        let mut a =
            ApprovalStore::open(&dir.join("approvals.db"), now()).map_err(|e| e.to_string())?;
        let session = a.start_session(now()).map_err(|e| e.to_string())?;
        let mut times = Vec::new();
        for i in 0..o.reps.clamp(50, 200) {
            let t = Instant::now();
            let p = a
                .propose_from_tool(
                    session,
                    ApprovalKind::OutsideRead,
                    &format!("{{\"path\":\"C:/x/{i}.txt\"}}"),
                    "read",
                    now(),
                )
                .map_err(|e| e.to_string())?;
            a.decide(p, session, true, now())
                .map_err(|e| e.to_string())?;
            a.claim(p, session, now()).map_err(|e| e.to_string())?;
            a.set_outcome(p, Outcome::Completed, now())
                .map_err(|e| e.to_string())?;
            times.push(common::ms(t.elapsed()));
        }
        Ok(times)
    })();
    match approvals {
        Ok(t) => out.push(measure("store.approval-round-trip", &t)),
        Err(e) => out.push(failed("store.approval-round-trip", e)),
    }
}

// ---------- prompt building ----------

fn prompt(dir: &Path, o: &MicroOptions, out: &mut Vec<Measure>) {
    let work = dir.join("prompt-work");
    let _ = std::fs::create_dir_all(&work);
    let skills: Vec<Skill> = (0..50)
        .map(|i| Skill {
            name: format!("skill-{i}"),
            description: format!("Does task number {i} the project's way."),
            body: "Steps…".repeat(50),
            source: "project".into(),
            files: vec![],
        })
        .collect();
    let memories = "- The user prefers short answers.\n".repeat(110);
    let times = common::time_n(o.reps.min(20), || {
        let mut ws = Workspaces::new(dir.join("prompt-checkpoints"));
        let Ok(id) = ws.grant(&work) else { return 0 };
        let policy = SessionPolicy {
            workspaces: [id].into_iter().collect(),
            ..SessionPolicy::default()
        };
        let mut agent = Agent::new(ws, policy, None::<&dyn Shell>, None, None, None, &[]);
        agent.set_skills(skills.clone());
        agent.set_memories(Some(memories.clone()));
        1
    });
    out.push(measure("prompt.build-50-skills-memories", &times));
}
