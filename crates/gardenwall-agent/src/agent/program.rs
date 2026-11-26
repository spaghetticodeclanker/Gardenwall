//! Code mode (ADR-014 §2.1, experimental): the `run_js` tool.
//!
//! The model writes one JavaScript program that calls its tools as
//! functions. The program runs somewhere untrusted (a [`ScriptHost`]; in the
//! app a sandboxed helper process) and can only ask for tool calls. Each one
//! goes through [`Agent::run_tool`], the same path as a call the model makes
//! directly: policy, the trusted approval, checkpoints, the shell's sandbox.
//! Only the program's output goes back to the model.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::truncate::truncate_head;

/// The tool's name.
pub const RUN_JS: &str = "run_js";
/// Most tool calls one program may make.
pub const MAX_PROGRAM_CALLS: usize = 200;
/// Most of a program's output the model gets back. Small on purpose: a
/// program is for working on data, not for moving it into the context.
pub const PROGRAM_OUTPUT_BYTES: usize = 8 * 1024;
pub const PROGRAM_OUTPUT_LINES: usize = 200;
/// Calls listed by name in a program's result (the rest are counted).
const SHOWN_CALLS: usize = 5;
/// Longest program text.
pub const MAX_PROGRAM_BYTES: usize = 64 * 1024;
/// The tools a program may call (plus the session's shell tool).
const FILE_TOOLS: [&str; 6] = ["read", "write", "edit", "ls", "find", "grep"];
const SHELL_TOOLS: [&str; 3] = ["bash", "powershell", "cmd"];

/// What a running program does next.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptStep {
    /// It asks for a tool call and waits for the result.
    Call { name: String, args: Value },
    /// It returned. `logs` is what it printed.
    Done { value: String, logs: String },
    /// It threw, didn't parse, hit a limit, or its host failed.
    Failed { message: String, logs: String },
}

/// One running program. Dropping it ends the program.
pub trait ScriptRun: Send {
    /// Give the program the result of its last call (`None` at the start)
    /// and wait for its next step.
    fn next<'a>(&'a mut self, reply: Option<(String, bool)>) -> BoxFuture<'a, ScriptStep>;
}

/// Runs model-written programs outside the trusted core.
pub trait ScriptHost: Send + Sync {
    /// Start `code` with `tools` as its callable functions.
    fn start(&self, code: &str, tools: &[String]) -> Result<Box<dyn ScriptRun>, String>;
}

pub(super) fn run_js_def(tools: &[&str]) -> ToolDef {
    let list = tools.join(", ");
    ToolDef {
        name: RUN_JS,
        description: format!(
            "Run a JavaScript program that calls your tools as functions ({list}), each taking the tool's arguments object. ls, find and grep return an array of lines, the others a string (read: the whole file). A failed call throws. No await or imports. Only what the program returns or logs comes back to you, with a count of the calls it made. Example: let n = 0; for (const p of find({{pattern: \"**/*.py\"}})) {{ const t = read({{path: p}}); if (t.includes(\"old\")) {{ write({{path: p, content: t.replaceAll(\"old\", \"new\")}}); n++; }} }} return n;"
        ),
        parameters: json!({
            "type": "object",
            "properties": {"code": {"type": "string", "description": "A function body"}},
            "required": ["code"]
        }),
        snippet: "Run a JavaScript program that calls your tools",
        guidelines: &[
            "Use run_js when a task needs many tool calls: to find, count or collect something across files (return only that, never whole files), or to make similar changes in many files (loop: read, change, write). Its result ends with the calls that really ran, so don't check the files again",
            "Don't use run_js for a single tool call or to wrap one shell command",
        ],
    }
}

/// Records that the user refused something a program asked for.
struct DenialWatch<'a> {
    inner: &'a dyn Approver,
    refused: AtomicBool,
}

impl Approver for DenialWatch<'_> {
    fn request<'a>(
        &'a self,
        approval: &'a Approval,
        details: &'a str,
        tool: &'a str,
    ) -> BoxFuture<'a, ApprovalResult> {
        Box::pin(async move {
            let details = from_program(details);
            let result = self.inner.request(approval, &details, tool).await;
            if result != ApprovalResult::Approved {
                self.refused.store(true, Ordering::SeqCst);
            }
            result
        })
    }

    fn finished<'a>(&'a self, success: bool) -> BoxFuture<'a, ()> {
        self.inner.finished(success)
    }

    fn decides_alone(&self, approval: &Approval, details: &str) -> bool {
        self.inner.decides_alone(approval, &from_program(details))
    }
}

/// Mark an approval's stored details as asked for by a program, so the
/// trusted dialog can say so (`"source": "program"`). Details that aren't a
/// JSON object pass through unchanged.
fn from_program(details: &str) -> String {
    match serde_json::from_str::<Value>(details) {
        Ok(Value::Object(mut map)) => {
            map.insert("source".into(), Value::String("program".into()));
            Value::Object(map).to_string()
        }
        _ => details.to_string(),
    }
}

/// `edit src/a.py` or `powershell npm test`, for the list of finished calls.
fn call_label(name: &str, args: &Value) -> String {
    let detail = ["path", "command", "pattern"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .unwrap_or_default();
    let detail: String = detail
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name} {detail}")
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

impl Agent {
    /// Offer `run_js` backed by this host, or take it away with `None`.
    /// The system prompt is rebuilt only when the offer changes, so the
    /// model's cached prompt survives a call that changes nothing.
    pub fn set_script_host(&mut self, host: Option<Arc<dyn ScriptHost>>) {
        let changed = self.script.is_some() != host.is_some();
        self.script = host;
        if changed {
            self.rebuild_system_prompt();
        }
    }

    /// Leave program mode: direct calls get Pi's truncated reads and
    /// images again. Called when a program ends and at both ends of every
    /// turn, so a turn dropped mid-program can't leave it on.
    pub(super) fn end_program_state(&mut self) {
        self.in_program = false;
        self.workspaces.set_whole_reads(false);
    }

    /// Whether `run_js` is offered.
    pub fn code_mode(&self) -> bool {
        self.script.is_some()
    }

    /// The tools a program may call: the file tools and the shell, less
    /// any that are hidden.
    pub(super) fn program_tools(&self) -> Vec<&'static str> {
        self.base_defs
            .iter()
            .map(|d| d.name)
            .filter(|n| FILE_TOOLS.contains(n) || SHELL_TOOLS.contains(n))
            .filter(|n| !self.is_hidden(n))
            .collect()
    }

    pub(super) async fn run_program(
        &mut self,
        args: &Value,
        approver: &dyn Approver,
        shell: Option<&dyn Shell>,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> (String, bool) {
        let Some(host) = self.script.clone() else {
            return (format!("Unknown tool {RUN_JS}."), true);
        };
        if self.in_program {
            return ("A program can't start another program.".into(), true);
        }
        let Some(code) = args.get("code").and_then(Value::as_str) else {
            return ("run_js requires code.".into(), true);
        };
        if code.len() > MAX_PROGRAM_BYTES {
            return ("The program is too long.".into(), true);
        }
        let tools: Vec<String> = self
            .program_tools()
            .into_iter()
            .filter(|n| shell.is_some() || !SHELL_TOOLS.contains(n))
            .map(String::from)
            .collect();
        let mut run = match host.start(code, &tools) {
            Ok(run) => run,
            Err(e) => return (format!("The program couldn't start: {e}"), true),
        };

        let parent = self.call_id.clone();
        let watch = DenialWatch {
            inner: approver,
            refused: AtomicBool::new(false),
        };
        // One line per finished call, and how many of each tool succeeded
        // and failed: the model is told what really ran, so it needn't
        // spend steps checking.
        let mut finished: Vec<String> = Vec::new();
        let mut tally: Vec<(String, u32, u32)> = Vec::new();
        let mut reply = None;
        self.in_program = true;
        let (problem, logs, value) = loop {
            let (name, call_args) = match run.next(reply.take()).await {
                ScriptStep::Call { name, args } => (name, args),
                ScriptStep::Done { value, logs } => break (None, logs, value),
                ScriptStep::Failed { message, logs } => {
                    break (
                        Some(format!("The program failed: {message}")),
                        logs,
                        String::new(),
                    );
                }
            };
            if self.stop.load(Ordering::SeqCst) {
                break (
                    Some("Stopped by the user.".into()),
                    String::new(),
                    String::new(),
                );
            }
            if finished.len() >= MAX_PROGRAM_CALLS {
                break (
                    Some(format!(
                        "The program was stopped: it made more than {MAX_PROGRAM_CALLS} tool calls."
                    )),
                    String::new(),
                    String::new(),
                );
            }
            if !tools.contains(&name) {
                // The host offers only `tools`; anything else is refused
                // here too, whatever the program's host let through.
                reply = Some((format!("Unknown tool: {name}"), true));
                continue;
            }
            let id = format!("{parent}.{}", finished.len() + 1);
            let arguments = call_args.to_string();
            on_event(AgentEvent::ToolStart {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                parent: Some(parent.clone()),
            });
            // A program gets files whole: nothing it reads enters the context.
            self.workspaces.set_whole_reads(name == "read");
            let (text, is_error) =
                Box::pin(self.run_tool(&name, &arguments, &watch, shell, None, on_event)).await;
            self.workspaces.set_whole_reads(false);
            on_event(AgentEvent::ToolEnd {
                id,
                // For the user's card: a whole file read by a program is
                // shown cut, as a direct read would be.
                output: truncate_head(&text, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES).content,
                is_error,
            });
            finished.push(format!(
                "{}: {}",
                call_label(&name, &call_args),
                if is_error {
                    format!("failed: {}", first_line(&text))
                } else {
                    "ok".into()
                }
            ));
            match tally.iter_mut().find(|t| t.0 == name) {
                Some(t) if is_error => t.2 += 1,
                Some(t) => t.1 += 1,
                None => tally.push((name.clone(), u32::from(!is_error), u32::from(is_error))),
            }
            if watch.refused.load(Ordering::SeqCst) {
                break (
                    Some("The program was stopped: the user did not approve this call.".into()),
                    String::new(),
                    String::new(),
                );
            }
            // Hidden text is collapsed for a program as for the model: the
            // program may return it.
            let (text, _) = gardenwall_policy::protect::hide_tags(&text);
            reply = Some((text, is_error));
        };
        drop(run);
        self.end_program_state();

        let mut out = String::new();
        if let Some(problem) = &problem {
            out.push_str(problem);
            out.push('\n');
        }
        out.push_str(&logs);
        if !logs.is_empty() && !logs.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&value);
        let cut = truncate_head(out.trim_end(), PROGRAM_OUTPUT_LINES, PROGRAM_OUTPUT_BYTES);
        let mut out = cut.content;
        if cut.truncated_by.is_some() {
            out.push_str(&format!(
                "\n\n[Output cut to {} of {}. Do the work on the data inside the program and return only the answer or a short report.]",
                format_size(cut.output_bytes),
                format_size(cut.total_bytes)
            ));
        }
        // What ran, from the core's own record (added after the cut, so it
        // is always there).
        let counts = |pick: fn(&(String, u32, u32)) -> u32| -> String {
            tally
                .iter()
                .filter(|t| pick(t) > 0)
                .map(|t| format!("{} {}", t.0, pick(t)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let failed: Vec<&String> = finished
            .iter()
            .filter(|l| l.contains(": failed: "))
            .collect();
        if problem.is_some() {
            if finished.is_empty() {
                out.push_str("\nNo tool calls were made.");
            } else {
                out.push_str(&format!(
                    "\nTool calls that succeeded before it stopped (their changes remain): {}.",
                    Some(counts(|t| t.1))
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| "none".into())
                ));
                out.push_str(" The last calls:");
                let from = finished.len().saturating_sub(SHOWN_CALLS);
                for (i, line) in finished.iter().enumerate().skip(from) {
                    out.push_str(&format!("\n{}. {line}", i + 1));
                }
            }
        } else if !finished.is_empty() {
            if out.trim().is_empty() {
                out = "The program returned nothing.".into();
            }
            out.push_str(&format!(
                "\n\n[Tool calls that succeeded: {}.",
                counts(|t| t.1)
            ));
            if failed.is_empty() {
                out.push_str(" None failed.]");
            } else {
                out.push_str(&format!(" Failed: {}", failed.len()));
                for line in failed.iter().take(SHOWN_CALLS) {
                    out.push_str(&format!("; {line}"));
                }
                out.push(']');
            }
        } else if out.trim().is_empty() {
            out = "The program returned nothing and made no tool calls.".into();
        }
        (out, problem.is_some())
    }
}
