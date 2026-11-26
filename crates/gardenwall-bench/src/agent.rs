//! S2 (agent task quality and speed) and S3 (several sessions at once).
//!
//! The Gardenwall arm is the real agent: the real tools, the real sandboxed
//! shell and compaction at the server's context window. Approvals are
//! granted automatically (benchmark only, so reaction time isn't timed).
//! The Pi arm runs a command template in an identical copy of the fixture
//! (see `bench/pi-wrapper.mjs`).

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gardenwall_agent::agent::{
    Agent, AgentEvent, ApprovalResult, Approver, BoxFuture, Model, Shell, TurnEnd,
};
use gardenwall_agent::prompt::{ContextFile, Skill};
#[cfg(any(windows, target_os = "linux"))]
use gardenwall_agent::sandbox_shell::ShellKind;

/// Where Gardenwall has no sandboxed shell (macOS), profiles still name one.
#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellKind {
    #[default]
    PowerShell,
    WindowsPowerShell,
    Cmd,
}
use gardenwall_inference::client::{ChatRequest, ChatResponse, InferenceClient, StreamEvent};
use gardenwall_policy::tools::{Approval, SessionPolicy};
use serde::{Deserialize, Serialize};

use crate::common::{self, Health, Kv, ServerConfig, Spec, Spread};
use crate::tasks::{Suite, Task};

/// Longest one run may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// A named set of settings for the agent suite (plan §4 S2).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    #[serde(skip)]
    pub shell: ShellKind,
    /// `None`: the template's default thinking.
    pub thinking: Option<String>,
    pub context: Option<u32>,
    pub kv: Kv,
    pub speculative: Spec,
    /// Runs the stock Pi arm instead of Gardenwall.
    pub pi: bool,
    /// Tools left out of the Gardenwall arm's offer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_tools: Vec<String>,
    /// Warm the server's prompt cache before the run, as the app does when a
    /// session opens (its time is recorded apart, not in the run's time).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warm_up: bool,
    /// Code mode (experimental, ADR-014 §2.1): the `run_js` tool is offered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code_mode: bool,
}

impl Profile {
    pub fn named(name: &str) -> Result<Self, String> {
        let mut p = Self {
            name: name.into(),
            shell: ShellKind::PowerShell,
            thinking: None,
            context: Some(32_768),
            kv: Kv::F16,
            speculative: Spec::Off,
            pi: false,
            hidden_tools: Vec::new(),
            warm_up: false,
            code_mode: false,
        };
        match name {
            "default" => {}
            "shell-ps51" => p.shell = ShellKind::WindowsPowerShell,
            "shell-cmd" => p.shell = ShellKind::Cmd,
            "thinking-off" => p.thinking = Some("off".into()),
            "thinking-low" => p.thinking = Some("low".into()),
            "thinking-medium" => p.thinking = Some("medium".into()),
            "ctx-8k" => p.context = Some(8_192),
            "ctx-16k" => p.context = Some(16_384),
            "kv-q8" => p.kv = Kv::Q8,
            "mtp" => p.speculative = Spec::Mtp,
            "pi" => p.pi = true,
            // The app's session warm-up: the first request's cache is ready.
            "warm-up" => p.warm_up = true,
            // One JavaScript program instead of many tool calls.
            "code-mode" => p.code_mode = true,
            // Pi's default tools only (read, bash, edit, write): no ls,
            // find, grep or task list (ADR-014 §2.1).
            "tools-pi" => {
                p.hidden_tools = ["ls", "find", "grep", "todo_write"]
                    .map(String::from)
                    .to_vec();
            }
            other => {
                return Err(format!(
                    "unknown profile {other} (default, shell-ps51, shell-cmd, thinking-off, thinking-low, thinking-medium, ctx-8k, ctx-16k, kv-q8, mtp, tools-pi, warm-up, code-mode, pi)"
                ));
            }
        }
        Ok(p)
    }

    /// The chat-template variables, as the app sets them per model.
    fn template_kwargs(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let v = match self.thinking.as_deref()? {
            "off" => serde_json::json!({"enable_thinking": false}),
            level => serde_json::json!({"reasoning_effort": level}),
        };
        v.as_object().cloned()
    }

    /// Profiles that can share one server start.
    fn server_key(&self) -> (Option<u32>, Kv, Spec) {
        (self.context, self.kv, self.speculative)
    }
}

/// One run of one task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub task: String,
    pub category: String,
    pub tier: u8,
    pub profile: String,
    pub arm: String,
    pub rep: usize,
    pub passed: bool,
    pub failures: Vec<String>,
    pub wall_ms: u64,
    /// Time to the first token of the first request.
    pub ttft_ms: Option<u64>,
    /// The first request's prompt (system prompt, tools and the request).
    pub first_prompt_tokens: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub requests: u32,
    /// Calls the model made itself (a program counts as one).
    pub tool_calls: u32,
    /// The tools called, in order (`js:` marks a call made by a program).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Code mode: programs run, and the tool calls they made.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub programs: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub program_calls: u32,
    /// The fullest the context got (prompt and reply of one request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_context_tokens: Option<u64>,
    pub edit_failures: u32,
    pub compactions: u32,
    pub approvals: u32,
    /// Requests the benchmark refused for the user (outside the task
    /// folder, network, MCP).
    #[serde(default)]
    pub denials: u32,
    pub end: String,
    pub error: Option<String>,
    /// The warm-up's time (`warm-up` profile), not part of `wall_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_up_ms: Option<u64>,
    #[serde(default)]
    pub health: Health,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The model through Gardenwall's own client (as the app uses it).
struct BenchModel(InferenceClient);

impl Model for BenchModel {
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_event: &'a mut (dyn FnMut(StreamEvent) -> ControlFlow<()> + Send),
    ) -> BoxFuture<'a, Result<ChatResponse, String>> {
        Box::pin(async move {
            self.0
                .chat(request, on_event)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn pause(&self, delay: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

/// Stands in for the user (benchmark arm only; ADR-014 §2.3), so their
/// reaction time isn't measured. It approves what a task can need inside
/// its own folder (secrets and persistence-risk files there) and refuses
/// everything else: reading or writing outside the task folder, network
/// commands, data leaving the machine and MCP tools. No task needs them,
/// and approving them let a lost model read the user's own files.
#[derive(Default)]
struct AutoApprove {
    approved: std::sync::atomic::AtomicU32,
    denied: std::sync::atomic::AtomicU32,
    /// Refuse everything (a task's `refuse_approvals`).
    refuse_all: bool,
}

fn bench_allows(approval: &Approval) -> bool {
    matches!(
        approval,
        Approval::RevealSecret(_) | Approval::WriteSecret(_) | Approval::PersistenceRisk(_)
    )
}

impl Approver for AutoApprove {
    fn request<'a>(
        &'a self,
        approval: &'a Approval,
        _details: &'a str,
        _tool: &'a str,
    ) -> BoxFuture<'a, ApprovalResult> {
        let result = if !self.refuse_all && bench_allows(approval) {
            self.approved.fetch_add(1, Ordering::SeqCst);
            ApprovalResult::Approved
        } else {
            self.denied.fetch_add(1, Ordering::SeqCst);
            ApprovalResult::Denied
        };
        Box::pin(async move { result })
    }
}

/// The agent's sandboxed shell, as in the app (none where Gardenwall has
/// no sandbox yet).
#[cfg(any(windows, target_os = "linux"))]
fn make_shell(work: &Path, root: &Path, kind: ShellKind) -> Option<Box<dyn Shell>> {
    // As the app does by default: an all-users Python, read-only.
    let tools: Vec<std::path::PathBuf> = gardenwall_agent::python::find_python()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .into_iter()
        .collect();
    gardenwall_agent::sandbox_shell::SandboxShell::new(
        &[work.to_path_buf()],
        &root.join("scratch"),
        &tools,
        Some(root.join("journal")),
        kind,
    )
    .ok()
    .map(|s| Box::new(s) as Box<dyn Shell>)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn make_shell(_work: &Path, _root: &Path, _kind: ShellKind) -> Option<Box<dyn Shell>> {
    None
}

/// The sandboxed program runner for code mode: the `gardenwall-js` helper
/// built next to this benchmark, copied alone into the run's folder (the
/// sandbox refuses a folder with hard links, which build folders have).
#[cfg(any(windows, target_os = "linux"))]
fn script_host(root: &Path) -> Result<Arc<dyn gardenwall_agent::agent::ScriptHost>, String> {
    let name = format!("gardenwall-js{}", std::env::consts::EXE_SUFFIX);
    let built = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(&name)))
        .filter(|p| p.is_file())
        .ok_or("code mode needs the program runner: cargo build --release -p gardenwall-js")?;
    let dir = root.join("js");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let helper = dir.join(&name);
    std::fs::copy(&built, &helper).map_err(|e| e.to_string())?;
    Ok(Arc::new(
        gardenwall_agent::script_host::SandboxScriptHost::new(helper, Some(root.join("journal"))),
    ))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn script_host(_root: &Path) -> Result<Arc<dyn gardenwall_agent::agent::ScriptHost>, String> {
    Err("code mode needs a sandbox, which this platform doesn't have yet".into())
}

/// Project instruction files in the fixture, as the app loads them.
fn context_files(work: &Path) -> Vec<ContextFile> {
    ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .filter_map(|f| {
            let path = work.join(f);
            std::fs::read_to_string(&path)
                .ok()
                .map(|content| ContextFile {
                    path: path.display().to_string(),
                    content,
                })
        })
        .collect()
}

pub async fn run_gardenwall(
    task: &Task,
    root: &Path,
    client: &InferenceClient,
    context: u64,
    profile: &Profile,
) -> Run {
    let mut run = Run::default();
    let work = root.join("work");
    let result: Result<(), String> = async {
        task.materialize(&work).map_err(|e| e.to_string())?;
        let mut ws = gardenwall_agent::tools::Workspaces::new(root.join("checkpoints"));
        let id = ws.grant(&work)?;
        let shell = make_shell(&work, root, profile.shell);
        let policy = SessionPolicy {
            workspaces: [id].into_iter().collect(),
            sandbox_available: shell.is_some(),
            ..SessionPolicy::default()
        };
        let mut agent = Agent::new(
            ws,
            policy,
            shell.as_deref(),
            None,
            None,
            None,
            &context_files(&work),
        );
        agent.context_window = Some(context);
        agent.template_kwargs = profile.template_kwargs();
        if !profile.hidden_tools.is_empty() {
            agent.set_hidden_tools(profile.hidden_tools.clone());
        }
        if profile.code_mode {
            agent.set_script_host(Some(script_host(root)?));
        }
        if !task.skills.is_empty() {
            agent.set_skills(
                task.skills
                    .iter()
                    .map(|s| Skill {
                        name: s.name.clone(),
                        description: s.description.clone(),
                        body: s.body.clone(),
                        source: "project".into(),
                        files: vec![],
                    })
                    .collect(),
            );
        }
        let model = BenchModel(client.clone());
        let approver = AutoApprove {
            refuse_all: task.refuse_approvals,
            ..AutoApprove::default()
        };
        let stop = agent.stop_handle();
        let mut answer = String::new();
        if profile.warm_up {
            // As the app does while the user writes the first request.
            agent.template_kwargs = profile.template_kwargs();
            let request = agent.warm_up_request(shell.as_deref());
            let took = gardenwall_agent::agent::warm_up(&model, &request).await;
            run.warm_up_ms = Some(took.as_millis() as u64);
        }
        let started = Instant::now();
        // GARDENWALL_BENCH_TRACE=1 prints each tool call and the start of
        // its output, to see why a run went wrong.
        let trace = std::env::var_os("GARDENWALL_BENCH_TRACE").is_some();
        let prompts: Vec<&str> = std::iter::once(task.prompt.as_str())
            .chain(task.followups.iter().map(String::as_str))
            .collect();
        for (i, prompt) in prompts.iter().enumerate() {
            agent.set_plan_mode(task.plan_first && i == 0);
            let mut sink = |e: AgentEvent| match e {
                AgentEvent::StepDone(s) => {
                    if trace {
                        eprintln!(
                            "  = step {}: prompt {} generated {}",
                            run.requests + 1,
                            s.prompt_tokens.unwrap_or(0),
                            s.generated_tokens.unwrap_or(0)
                        );
                    }
                    run.requests += 1;
                    if run.ttft_ms.is_none() {
                        run.ttft_ms = s.ttft_ms;
                    }
                    if run.first_prompt_tokens.is_none() {
                        run.first_prompt_tokens = s.prompt_tokens;
                    }
                    if let Some(p) = s.prompt_tokens {
                        *run.prompt_tokens.get_or_insert(0) += p;
                    }
                    if let Some(g) = s.generated_tokens {
                        *run.completion_tokens.get_or_insert(0) += g;
                    }
                    run.peak_context_tokens = run.peak_context_tokens.max(s.total_tokens);
                }
                AgentEvent::ToolStart {
                    name,
                    arguments,
                    parent: Some(_),
                    ..
                } => {
                    if trace {
                        eprintln!("    > {name} {}", clip(&arguments, 300));
                    }
                    run.program_calls += 1;
                    run.tools.push(format!("js:{name}"));
                }
                AgentEvent::ToolStart {
                    name, arguments, ..
                } => {
                    if trace {
                        // A program is shown whole: it is what is being judged.
                        let max = if name == "run_js" { 6000 } else { 600 };
                        eprintln!("  > {name} {}", clip(&arguments, max));
                    }
                    if name == "run_js" {
                        run.programs += 1;
                    }
                    run.tool_calls += 1;
                    run.tools.push(name);
                }
                AgentEvent::ToolEnd {
                    output, is_error, ..
                } if trace => {
                    eprintln!("  < error={is_error} {}", clip(&output, 800));
                    if is_error && (output.contains("Could not find") || output.contains("oldText"))
                    {
                        run.edit_failures += 1;
                    }
                }
                AgentEvent::ToolEnd {
                    output,
                    is_error: true,
                    ..
                } if output.contains("Could not find") || output.contains("oldText") => {
                    run.edit_failures += 1;
                }
                AgentEvent::Compacted { .. } => run.compactions += 1,
                AgentEvent::Message { message, .. }
                    if message.role == gardenwall_inference::client::Role::Assistant =>
                {
                    if trace {
                        let thinking = message.reasoning_content.as_deref().unwrap_or("");
                        eprintln!(
                            "  # thinking {} chars: {}",
                            thinking.len(),
                            clip(thinking, 400)
                        );
                        eprintln!("  # text: {}", clip(&message.content, 400));
                    }
                    if !message.content.trim().is_empty() {
                        answer = message.content.clone();
                    }
                }
                _ => {}
            };
            let left = RUN_TIMEOUT.saturating_sub(started.elapsed());
            let turn = tokio::time::timeout(
                left,
                agent.run_turn(prompt, &model, &approver, shell.as_deref(), &mut sink),
            )
            .await;
            run.end = match turn {
                Err(_) => {
                    stop.store(true, Ordering::SeqCst);
                    "timeout".into()
                }
                Ok(Err(e)) => {
                    // A failed run still took time; 0 would lower the medians.
                    run.wall_ms = started.elapsed().as_millis() as u64;
                    return Err(e);
                }
                Ok(Ok(TurnEnd::Answered)) => "answered".into(),
                Ok(Ok(TurnEnd::Stopped)) => "stopped".into(),
                Ok(Ok(TurnEnd::StepLimit)) => "step_limit".into(),
                Ok(Ok(TurnEnd::Repeating)) => "repeating".into(),
            };
            if run.end != "answered" {
                break;
            }
        }
        run.wall_ms = started.elapsed().as_millis() as u64;
        run.approvals = approver.approved.load(Ordering::SeqCst);
        run.denials = approver.denied.load(Ordering::SeqCst);
        run.failures = task.failures(&work, &answer);
        run.passed = run.failures.is_empty();
        Ok(())
    }
    .await;
    if let Err(e) = result {
        run.error = Some(e.clone());
        run.end = "error".into();
        run.failures = vec![e];
    }
    run
}

/// At most `max` characters of `s` on one line, for traces.
fn clip(s: &str, max: usize) -> String {
    let one_line = s.replace('\r', "").replace('\n', "⏎");
    match one_line.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &one_line[..i]),
        None => one_line,
    }
}

/// Stock Pi: the command template, split on spaces, with placeholders
/// replaced inside each argument (the prompt is never shell-parsed).
pub async fn run_pi(
    task: &Task,
    root: &Path,
    template: &str,
    url: &str,
    key: &str,
    model: &str,
) -> Run {
    let mut run = Run::default();
    let work = root.join("work");
    if let Err(e) = task.materialize(&work) {
        run.error = Some(e.to_string());
        run.end = "error".into();
        return run;
    }
    let args: Vec<String> = template
        .split_whitespace()
        .map(|a| {
            a.replace("{url}", url)
                .replace("{key}", key)
                .replace("{model}", model)
                .replace("{prompt}", &task.prompt)
        })
        .collect();
    let Some((program, rest)) = args.split_first() else {
        run.error = Some("empty --pi command".into());
        return run;
    };
    let started = Instant::now();
    let child = tokio::process::Command::new(program)
        .args(rest)
        .current_dir(&work)
        .env("GARDENWALL_BENCH_URL", url)
        .env("GARDENWALL_BENCH_KEY", key)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(RUN_TIMEOUT, child).await;
    run.wall_ms = started.elapsed().as_millis() as u64;
    let answer = match out {
        Err(_) => {
            run.end = "timeout".into();
            String::new()
        }
        Ok(Err(e)) => {
            run.end = "error".into();
            run.error = Some(format!("couldn't start Pi: {e}"));
            String::new()
        }
        Ok(Ok(o)) => {
            run.end = if o.status.success() {
                "answered".into()
            } else {
                format!("exit {:?}", o.status.code())
            };
            pi_stats(&mut run, &String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        }
    };
    run.failures = task.failures(&work, &answer);
    run.passed = run.failures.is_empty() && run.error.is_none();
    run
}

/// Counts a Pi wrapper may report as a last `GARDENWALL_BENCH_STATS {json}`
/// line on stderr (`bench/pi-wrapper.mjs` does), so both arms record the
/// same measures. Missing or malformed lines leave the counts empty.
pub fn pi_stats(run: &mut Run, stderr: &str) {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Stats {
        requests: u32,
        prompt_tokens: u64,
        completion_tokens: u64,
        tool_calls: u32,
        edit_failures: u32,
        compactions: u32,
        ttft_ms: Option<u64>,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        first_prompt_tokens: Option<u64>,
    }
    let Some(s) = stderr
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("GARDENWALL_BENCH_STATS "))
        .and_then(|j| serde_json::from_str::<Stats>(j).ok())
    else {
        return;
    };
    run.requests = s.requests;
    run.prompt_tokens = Some(s.prompt_tokens);
    run.completion_tokens = Some(s.completion_tokens);
    run.tool_calls = s.tool_calls;
    run.edit_failures = s.edit_failures;
    run.compactions = s.compactions;
    run.ttft_ms = s.ttft_ms;
    run.tools = s.tools;
    run.first_prompt_tokens = s.first_prompt_tokens;
}

/// Per profile (and arm), per tier and overall.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub profile: String,
    pub arm: String,
    /// `None`: all tiers.
    pub tier: Option<u8>,
    pub runs: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub wall_ms: Option<Spread>,
    pub ttft_ms: Option<Spread>,
    pub mean_tool_calls: f64,
    pub mean_requests: f64,
    /// Code mode: programs per run, and the calls they made per run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_programs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_program_calls: Option<f64>,
    pub median_completion_tokens: Option<f64>,
    pub median_peak_context_tokens: Option<f64>,
    pub median_prompt_tokens: Option<f64>,
    pub median_first_prompt_tokens: Option<f64>,
    pub degraded_runs: usize,
}

pub fn summarize(runs: &[Run]) -> Vec<Summary> {
    let mut keys: Vec<(String, String)> = runs
        .iter()
        .map(|r| (r.profile.clone(), r.arm.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for (profile, arm) in keys {
        for tier in [None, Some(1), Some(2), Some(3)] {
            let mine: Vec<&Run> = runs
                .iter()
                .filter(|r| r.profile == profile && r.arm == arm)
                .filter(|r| tier.is_none_or(|t| r.tier == t))
                .collect();
            if mine.is_empty() {
                continue;
            }
            let f = |g: &dyn Fn(&Run) -> Option<f64>| -> Vec<f64> {
                mine.iter().filter_map(|r| g(r)).collect()
            };
            let passed = mine.iter().filter(|r| r.passed).count();
            out.push(Summary {
                profile: profile.clone(),
                arm: arm.clone(),
                tier,
                runs: mine.len(),
                passed,
                pass_rate: passed as f64 / mine.len() as f64,
                wall_ms: common::spread(&f(&|r| Some(r.wall_ms as f64))),
                ttft_ms: common::spread(&f(&|r| r.ttft_ms.map(|t| t as f64))),
                mean_tool_calls: mine.iter().map(|r| r.tool_calls as f64).sum::<f64>()
                    / mine.len() as f64,
                mean_requests: mine.iter().map(|r| r.requests as f64).sum::<f64>()
                    / mine.len() as f64,
                mean_programs: mine.iter().any(|r| r.programs > 0).then(|| {
                    mine.iter().map(|r| r.programs as f64).sum::<f64>() / mine.len() as f64
                }),
                mean_program_calls: mine.iter().any(|r| r.programs > 0).then(|| {
                    mine.iter().map(|r| r.program_calls as f64).sum::<f64>() / mine.len() as f64
                }),
                median_completion_tokens: common::spread(&f(&|r| {
                    r.completion_tokens.map(|t| t as f64)
                }))
                .map(|s| s.median),
                median_peak_context_tokens: common::spread(&f(&|r| {
                    r.peak_context_tokens.map(|t| t as f64)
                }))
                .map(|s| s.median),
                median_prompt_tokens: common::spread(&f(&|r| r.prompt_tokens.map(|t| t as f64)))
                    .map(|s| s.median),
                median_first_prompt_tokens: common::spread(&f(&|r| {
                    r.first_prompt_tokens.map(|t| t as f64)
                }))
                .map(|s| s.median),
                degraded_runs: mine.iter().filter(|r| r.health.degraded()).count(),
            });
        }
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReport {
    pub suite_version: u32,
    pub model: String,
    pub server_build: Option<String>,
    pub profiles: Vec<Profile>,
    pub summary: Vec<Summary>,
    pub runs: Vec<Run>,
}

pub struct AgentOptions {
    pub server: std::path::PathBuf,
    pub model: std::path::PathBuf,
    pub tasks: std::path::PathBuf,
    pub profiles: Vec<Profile>,
    pub tiers: Vec<u8>,
    pub only: Option<Vec<String>>,
    pub reps: usize,
    pub seed: u64,
    pub pi: Option<String>,
    /// Runs are appended here as they finish; runs already in it are kept
    /// and not repeated (resuming a paused run).
    pub partial: Option<std::path::PathBuf>,
}

type RunKey = (String, String, usize);

fn load_partial(path: &Path) -> Vec<Run> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn append_partial(path: &Path, run: &Run) {
    use std::io::Write;
    if let (Ok(mut f), Ok(line)) = (
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path),
        serde_json::to_string(run),
    ) {
        let _ = writeln!(f, "{line}");
    }
}

/// Run the agent suite: profiles grouped by server settings, tasks
/// shuffled within each group.
pub async fn run_suite(o: &AgentOptions) -> Result<AgentReport, String> {
    let suite = Suite::load(&o.tasks)?;
    let tasks: Vec<&Task> = suite
        .tasks
        .iter()
        .filter(|t| o.tiers.contains(&t.tier))
        .filter(|t| o.only.as_ref().is_none_or(|only| only.contains(&t.id)))
        .collect();
    if tasks.is_empty() {
        return Err("no tasks selected".into());
    }
    if o.profiles.iter().any(|p| p.pi) && o.pi.is_none() {
        return Err("the pi profile needs a Pi command (machine.json \"pi\" or --pi)".into());
    }
    let mut groups: Vec<Vec<&Profile>> = Vec::new();
    for p in &o.profiles {
        match groups
            .iter_mut()
            .find(|g| g[0].server_key() == p.server_key())
        {
            Some(g) => g.push(p),
            None => groups.push(vec![p]),
        }
    }
    let mut runs: Vec<Run> = o.partial.as_deref().map(load_partial).unwrap_or_default();
    if !runs.is_empty() {
        eprintln!("Resuming: {} run(s) already done are kept.", runs.len());
    }
    let done: std::collections::HashSet<RunKey> = runs
        .iter()
        .map(|r| (r.task.clone(), r.profile.clone(), r.rep))
        .collect();
    let (mut model_name, mut build) = (String::new(), None);
    for group in groups {
        let first = group[0];
        let mut plan: Vec<(&Task, &Profile, usize)> = Vec::new();
        for t in &tasks {
            for p in &group {
                if p.pi && !t.pi {
                    continue;
                }
                for rep in 0..o.reps {
                    plan.push((t, p, rep));
                }
            }
        }
        common::shuffle(&mut plan, o.seed);
        plan.retain(|(t, p, rep)| !done.contains(&(t.id.clone(), p.name.clone(), *rep)));
        if plan.is_empty() {
            continue;
        }
        if common::stop_requested() {
            return Err(common::PAUSED.into());
        }
        let mut cfg = ServerConfig::new(&o.server, &o.model);
        cfg.context = first.context;
        cfg.kv = first.kv;
        cfg.speculative = first.speculative;
        eprintln!("Starting the model server ({}) …", cfg.label());
        let started = common::start_server(&cfg).await?;
        model_name = started.model_name.clone();
        build = started.build.clone();
        let base = common::TempDir::new("agent")?;
        for (n, (task, profile, rep)) in plan.iter().enumerate() {
            if common::stop_requested() {
                started.server.shutdown().await;
                return Err(common::PAUSED.into());
            }
            let root = base
                .0
                .join(format!("{n:04}-{}-{}-{rep}", task.id, profile.name));
            let mut r = if profile.pi {
                let Some(url) = &started.url else {
                    return Err("the Pi arm needs an unsandboxed server".into());
                };
                run_pi(
                    task,
                    &root,
                    o.pi.as_deref().unwrap_or_default(),
                    url,
                    &started.secret,
                    &started.model_name,
                )
                .await
            } else {
                run_gardenwall(
                    task,
                    &root,
                    &started.client,
                    started.context_window,
                    profile,
                )
                .await
            };
            r.task = task.id.clone();
            r.category = task.category.clone();
            r.tier = task.tier;
            r.profile = profile.name.clone();
            r.arm = if profile.pi { "pi" } else { "gardenwall" }.into();
            r.rep = *rep;
            r.health = common::health();
            eprintln!(
                "[{}/{}] {:<24} {:<14} {} {:>6.1}s {}{}",
                n + 1,
                plan.len(),
                r.task,
                r.profile,
                if r.passed { "pass" } else { "FAIL" },
                r.wall_ms as f64 / 1000.0,
                if r.health.degraded() {
                    "(degraded) "
                } else {
                    ""
                },
                r.failures.first().map(String::as_str).unwrap_or("")
            );
            if let Some(p) = &o.partial {
                append_partial(p, &r);
            }
            runs.push(r);
            let _ = std::fs::remove_dir_all(&root);
        }
        started.server.shutdown().await;
    }
    let summary = summarize(&runs);
    for s in summary.iter().filter(|s| s.tier.is_none()) {
        eprintln!(
            "{} ({}): {}/{} passed ({:.0} %), median {:.1} s, {:.1} tool calls per run",
            s.profile,
            s.arm,
            s.passed,
            s.runs,
            s.pass_rate * 100.0,
            s.wall_ms.map_or(0.0, |w| w.median / 1000.0),
            s.mean_tool_calls
        );
    }
    Ok(AgentReport {
        suite_version: suite.version,
        model: model_name,
        server_build: build,
        profiles: o.profiles.clone(),
        summary,
        runs,
    })
}

// ---------- S3: several sessions at once ----------

/// One sessions-mode measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsRun {
    /// `turns`, `one-at-a-time` or `parallel`.
    pub mode: String,
    pub sessions: usize,
    pub slots: u32,
    pub rep: usize,
    pub total_ms: u64,
    /// Per session: wall time from the batch start (includes waiting).
    pub session_ms: Vec<u64>,
    /// Per session: time before it started working (one at a time).
    pub wait_ms: Vec<u64>,
    pub passed: usize,
    pub vram_gib: Option<f64>,
    pub health: Health,
}

pub struct SessionsOptions {
    pub server: std::path::PathBuf,
    pub model: std::path::PathBuf,
    pub tasks: std::path::PathBuf,
    pub counts: Vec<usize>,
    pub reps: usize,
}

/// Run tier-1 tasks as `n` sessions at once in each sessions mode.
pub async fn run_sessions(o: &SessionsOptions) -> Result<Vec<SessionsRun>, String> {
    let suite = Suite::load(&o.tasks)?;
    let tier1: Vec<Task> = suite.tasks.into_iter().filter(|t| t.tier == 1).collect();
    if tier1.is_empty() {
        return Err("no tier-1 tasks".into());
    }
    let profile = Profile::named("default")?;
    let max = *o.counts.iter().max().unwrap_or(&2);
    let mut out = Vec::new();
    // turns and one-at-a-time share a one-slot server; parallel needs slots.
    let mut servers: Vec<(&str, u32)> = vec![("turns", 1), ("one-at-a-time", 1)];
    for n in &o.counts {
        servers.push(("parallel", *n as u32));
    }
    let mut one_slot: Option<common::Started> = None;
    for (mode, slots) in servers {
        if mode == "parallel" || one_slot.is_none() {
            if let Some(s) = one_slot.take() {
                s.server.shutdown().await;
            }
            let mut cfg = ServerConfig::new(&o.server, &o.model);
            cfg.context = Some(32_768);
            cfg.parallel = slots;
            eprintln!("Starting the model server ({}) …", cfg.label());
            one_slot = Some(common::start_server(&cfg).await?);
        }
        let started = one_slot.as_ref().expect("started");
        let counts: Vec<usize> = if mode == "parallel" {
            vec![slots as usize]
        } else {
            o.counts.clone()
        };
        for n in counts {
            for rep in 0..o.reps {
                if common::stop_requested() {
                    // The server stops when it is dropped.
                    return Err(common::PAUSED.into());
                }
                let batch: Vec<Task> = (0..n)
                    .map(|i| tier1[(rep * max + i) % tier1.len()].clone())
                    .collect();
                let r = sessions_batch(mode, &batch, started, &profile, slots, rep).await?;
                eprintln!(
                    "{mode:<14} {n} sessions, {slots} slot(s), rep {rep}: total {:.1} s, {}/{} passed",
                    r.total_ms as f64 / 1000.0,
                    r.passed,
                    n
                );
                out.push(r);
            }
        }
    }
    if let Some(s) = one_slot {
        s.server.shutdown().await;
    }
    Ok(out)
}

async fn sessions_batch(
    mode: &str,
    batch: &[Task],
    started: &common::Started,
    profile: &Profile,
    slots: u32,
    rep: usize,
) -> Result<SessionsRun, String> {
    let base = common::TempDir::new("sessions")?;
    let t0 = Instant::now();
    let mut results: Vec<(u64, u64, bool)> = Vec::new();
    if mode == "one-at-a-time" {
        for (i, task) in batch.iter().enumerate() {
            let wait = t0.elapsed().as_millis() as u64;
            let r = run_gardenwall(
                task,
                &base.0.join(format!("s{i}")),
                &started.client,
                started.context_window,
                profile,
            )
            .await;
            results.push((t0.elapsed().as_millis() as u64, wait, r.passed));
        }
    } else {
        // Each session on its own thread and runtime, as the app's sessions
        // are independent; their requests meet at the model server.
        let handles: Vec<_> = batch
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let task = task.clone();
                let client = started.client.clone();
                let context = started.context_window;
                let profile = profile.clone();
                let root = base.0.join(format!("s{i}"));
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime");
                    let r = rt.block_on(run_gardenwall(&task, &root, &client, context, &profile));
                    (t0.elapsed().as_millis() as u64, r.passed)
                })
            })
            .collect();
        for h in handles {
            let (done, passed) = h.join().map_err(|_| "a session thread panicked")?;
            results.push((done, 0, passed));
        }
    }
    Ok(SessionsRun {
        mode: mode.into(),
        sessions: batch.len(),
        slots,
        rep,
        total_ms: t0.elapsed().as_millis() as u64,
        session_ms: results.iter().map(|r| r.0).collect(),
        wait_ms: results.iter().map(|r| r.1).collect(),
        passed: results.iter().filter(|r| r.2).count(),
        vram_gib: common::process_vram_gib(started.server.pid()),
        health: common::health(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bench_refuses_what_leaves_the_task_folder() {
        for a in [
            Approval::OutsideRead,
            Approval::OutsideWrite,
            Approval::NetworkCommand,
            Approval::Egress,
            Approval::McpTool,
        ] {
            assert!(!bench_allows(&a), "{a:?}");
        }
    }

    #[test]
    fn pi_wrapper_stats_are_read_from_the_last_line() {
        let mut run = Run::default();
        pi_stats(
            &mut run,
            "warning\nGARDENWALL_BENCH_STATS {\"requests\":3,\"promptTokens\":900,\"completionTokens\":80,\"toolCalls\":2,\"editFailures\":1,\"compactions\":0,\"ttftMs\":410,\"tools\":[\"read\",\"edit\"]}\n",
        );
        assert_eq!((run.requests, run.tool_calls, run.edit_failures), (3, 2, 1));
        assert_eq!(run.prompt_tokens, Some(900));
        assert_eq!(run.ttft_ms, Some(410));
        assert_eq!(run.tools, ["read", "edit"]);
        let mut run = Run::default();
        pi_stats(&mut run, "GARDENWALL_BENCH_STATS {broken");
        assert_eq!((run.requests, run.prompt_tokens), (0, None));
    }

    #[test]
    fn profiles_are_known_and_grouped_by_server() {
        assert!(Profile::named("nope").is_err());
        let a = Profile::named("default").unwrap();
        let b = Profile::named("shell-cmd").unwrap();
        let c = Profile::named("ctx-8k").unwrap();
        assert_eq!(a.server_key(), b.server_key());
        assert_ne!(a.server_key(), c.server_key());
        assert_eq!(
            Profile::named("thinking-off")
                .unwrap()
                .template_kwargs()
                .unwrap()["enable_thinking"],
            false
        );
        assert!(a.template_kwargs().is_none());
    }

    #[test]
    fn summaries_split_by_tier() {
        let mk = |tier, passed, wall| Run {
            task: "t".into(),
            tier,
            profile: "default".into(),
            arm: "gardenwall".into(),
            passed,
            wall_ms: wall,
            ..Default::default()
        };
        let s = summarize(&[mk(1, true, 1000), mk(2, false, 3000), mk(2, true, 2000)]);
        let all = s.iter().find(|s| s.tier.is_none()).unwrap();
        assert_eq!((all.runs, all.passed), (3, 2));
        let t2 = s.iter().find(|s| s.tier == Some(2)).unwrap();
        assert_eq!(t2.wall_ms.unwrap().median, 2500.0);
        assert!(s.iter().all(|s| s.tier != Some(3)));
    }
}
