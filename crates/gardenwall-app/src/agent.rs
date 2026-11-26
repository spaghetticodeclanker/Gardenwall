//! Agent mode (ADR-014, ADR-016): the app side of the agent harness.
//!
//! - Folders are chosen with the native folder picker opened by the trusted
//!   core, never sent as paths by the webview (ADR-011 §2.2).
//! - Approval requests go to the approval store; the trusted dialog shows
//!   them; the agent continues only after a successful single-use claim.
//! - Commands run in the OS sandbox created for the session.
//! - Each session's transcript is saved, encrypted, in `conversations.db`
//!   as it happens. Resuming a transcript grants folders again through the
//!   native picker; the saved folder list is only shown, never trusted.
//! - Several sessions can be open at once, each
//!   with its own folders, sandbox, approval session, grants and
//!   transcript. Settings → Agent chooses how they share the model.

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gardenwall_agent::agent::{
    Agent, AgentEvent, ApprovalResult, Approver, BoxFuture, ContextBreakdown, Model, Restore,
    Shell, TurnEnd,
};
use gardenwall_agent::compaction::Summary;
use gardenwall_agent::prompt::ContextFile;
pub(crate) use gardenwall_agent::session_shell::SessionShell;
use gardenwall_agent::tools::Workspaces;
use gardenwall_inference::client::{
    ChatMessage, ChatRequest, ChatResponse, InferenceClient, Role, StreamEvent,
};
use gardenwall_policy::paths::RelPath;
use gardenwall_policy::tools::{Approval, SessionPolicy};
use gardenwall_store::approvals::{ApprovalKind, Outcome, ProposalId, ProposalState, SessionId};
use gardenwall_store::conversations::{EntryKind, TranscriptSummary, title_from_message};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::{AppState, now_ms};

type CmdResult<T> = Result<T, String>;

/// Largest AGENTS.md loaded into the system prompt.
const MAX_CONTEXT_FILE_BYTES: u64 = 64 * 1024;

pub struct AgentSession {
    pub agent: Agent,
    /// The session's sandbox, restartable (ADR-024 §3 A1).
    pub shell: Option<Arc<SessionShell>>,
    pub approval_session: SessionId,
    pub folders: Vec<PathBuf>,
    pub stop: Arc<AtomicBool>,
    /// The saved transcript, created with the first request.
    pub transcript: Option<String>,
    /// Saving failed once; the user was told and saving stopped.
    pub saving_failed: bool,
    /// What web access this session has, for display.
    pub web: Option<String>,
    /// A research session (no folders, no shell, web tools only) and how
    /// its web requests are approved (ADR-018).
    pub research: Option<crate::state::WebMode>,
    /// This session's visited-sites list.
    pub visits: Option<Arc<crate::web::visits::VisitLog>>,
    /// The attached document collection's name, for display.
    pub documents: Option<String>,
    /// MCP servers and their tool counts, for display.
    pub mcp: Option<String>,
    /// The attached collection, kept for `/clear`.
    pub docs: Option<Arc<dyn gardenwall_agent::agent::Documents>>,
    /// Custom commands from the granted folders, read at session start.
    pub commands: Vec<crate::skills::UserCommand>,
    /// Memories are used in this session (the user can turn them off).
    pub memories_on: bool,
    /// Suggestions from the `remember` tool, stored as pending after a turn.
    pub memory_queue: Arc<crate::memory::SuggestionQueue>,
    /// Output of the user's `!` commands, added to the next request.
    pub shared_output: Vec<String>,
    /// The project this session belongs to.
    pub project: Option<String>,
    /// Times this session's context was compacted (for the handoff
    /// suggestion), counted from its saved transcript when continued.
    pub compactions: usize,
    /// What this session continues from a reviewed handoff, linked when
    /// its transcript is created with the first request, and its title.
    pub handoff_from: Option<(crate::handoff::Origin, String)>,
}

/// The only tools a research session offers (it has no folders). An allow
/// list, so a tool added to Gardenwall later is never offered here by
/// accident.
const RESEARCH_TOOLS: &[&str] = &["web_search", "web_fetch", "todo_write", "search_documents"];

/// Most agent sessions open at once.
pub const MAX_SESSIONS: usize = 8;

/// An open agent session. The session itself is behind its own lock (held
/// for a whole turn); the rest can be read while it works.
pub struct LiveSession {
    /// Names the session in commands (random; not a transcript id).
    pub id: String,
    pub session: tokio::sync::Mutex<AgentSession>,
    pub approval_session: SessionId,
    /// Stops the running turn, or a turn waiting for its turn.
    pub stop: Arc<AtomicBool>,
    /// Messages queued while the agent works (Pi's steering).
    pub steering: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    /// Send now: cuts the reply being written (see `Agent::interrupt_handle`).
    pub interrupt: Arc<AtomicBool>,
    /// A turn is running.
    pub busy: AtomicBool,
    /// A turn is waiting for another session to finish ("one at a time").
    pub waiting: AtomicBool,
    pub folders: Vec<String>,
    pub created_at: i64,
    /// The project this session belongs to.
    pub project: Option<String>,
    /// The saved transcript and its title, for the session list.
    pub card: std::sync::Mutex<(Option<String>, String)>,
    /// What the session is doing, for the dashboard.
    pub activity: std::sync::Mutex<crate::activity::Activity>,
    /// The session's MCP servers, for prompts the user picks (kept outside
    /// the session lock, so a prompt can be filled in while a turn runs).
    pub mcp: Option<Arc<crate::mcp::AppMcp>>,
    /// The agent's open question card, if any (`ask_user`).
    pub question: Arc<QuestionSlot>,
    /// The agent's open "Run this yourself" card, if any.
    pub run_request: Arc<RunSlot>,
    /// The research sub-agent's open card (question, progress, findings).
    pub research: Arc<crate::research_agent::ResearchSlot>,
    /// The session's shell, readable while a turn runs (programs left
    /// running, for the sidebar and Home). Set again when a session without
    /// commands gets a sandbox on restart. The session's scratch folder
    /// lives inside it and goes after its sandbox.
    pub shell: std::sync::Mutex<Option<Arc<SessionShell>>>,
    /// A toolchain was added in Settings or on the project page after this
    /// session started: its banner offers a sandbox restart (ADR-024 §3 A1).
    pub restart_offer: std::sync::Mutex<Option<String>>,
    /// The toolchain the agent asked for that isn't on this PC: its install
    /// card (ADR-024 §3 A2).
    pub install_card: Arc<std::sync::Mutex<Option<String>>>,
    /// The toolchain ids this session's sandbox was started with.
    pub sandbox_toolchains: Arc<std::sync::Mutex<Vec<String>>>,
}

/// A session's private scratch folder under `agent-scratch`, deleted when
/// the session ends (ADR-022 hardening): nothing one session leaves there
/// (Git or Cargo settings, caches) reaches a later session.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn new(state: &AppState) -> Self {
        Self(
            state
                .paths
                .root
                .join("agent-scratch")
                .join(crate::commands::new_token()),
        )
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best effort: what can't be removed now goes at the next start.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl LiveSession {
    /// Programs the user let keep running in this session's sandbox.
    pub fn background(&self) -> Vec<String> {
        self.shell
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .map(|sh| sh.background())
            .unwrap_or_default()
    }
}

impl AgentSession {
    /// The shell for a turn or a command.
    pub fn shell(&self) -> Option<&dyn Shell> {
        self.shell.as_deref().map(|s| s as &dyn Shell)
    }
}

/// The open "Run this yourself" card and the user's decision. The card is
/// built from the core's checked request only.
#[derive(Default)]
pub struct RunSlot(std::sync::Mutex<RunState>);

impl RunSlot {
    /// A card is open and unanswered (Home's "Waiting for you").
    pub fn waiting_for_user(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|s| s.open.is_some() && s.answer.is_none())
    }
}

#[derive(Default)]
struct RunState {
    id: u64,
    open: Option<gardenwall_agent::run_request::RunRequest>,
    answer: Option<Option<gardenwall_agent::run_request::RunOutcome>>,
}

struct AppRunAsker {
    slot: Arc<RunSlot>,
    stop: Arc<AtomicBool>,
}

impl gardenwall_agent::run_request::RunAsker for AppRunAsker {
    fn ask_to_run<'a>(
        &'a self,
        request: &'a gardenwall_agent::run_request::RunRequest,
    ) -> BoxFuture<'a, Option<gardenwall_agent::run_request::RunOutcome>> {
        Box::pin(async move {
            if let Ok(mut s) = self.slot.0.lock() {
                s.id += 1;
                s.open = Some(request.clone());
                s.answer = None;
            }
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let mut s = self.slot.0.lock().ok()?;
                if self.stop.load(Ordering::SeqCst) {
                    s.open = None;
                    s.answer = None;
                    return None;
                }
                if let Some(answer) = s.answer.take() {
                    s.open = None;
                    return answer;
                }
            }
        })
    }
}

/// A "Run this yourself" card for the window.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCard {
    id: u64,
    request: gardenwall_agent::run_request::RunRequest,
}

/// The session's open "Run this yourself" card, if the agent is waiting.
#[tauri::command]
pub fn agent_run_request(state: State<'_, AppState>, sid: String) -> CmdResult<Option<RunCard>> {
    let live = live(&state, &sid)?;
    let s = live.run_request.0.lock().map_err(|_| "state unavailable")?;
    Ok(s.open
        .clone()
        .filter(|_| s.answer.is_none())
        .map(|request| RunCard { id: s.id, request }))
}

/// Answer the open card: `done` (with what the user chose to paste back,
/// capped) or not (skipped).
#[tauri::command]
pub fn agent_run_answer(
    state: State<'_, AppState>,
    sid: String,
    id: u64,
    done: bool,
    pasted: Option<String>,
) -> CmdResult<()> {
    use gardenwall_agent::run_request::{MAX_PASTED_CHARS, RunOutcome};
    let live = live(&state, &sid)?;
    let mut s = live.run_request.0.lock().map_err(|_| "state unavailable")?;
    if s.open.is_none() || s.id != id {
        return Err("that request is no longer open".into());
    }
    s.answer = Some(Some(if done {
        RunOutcome::Done {
            pasted: pasted
                .unwrap_or_default()
                .chars()
                .take(MAX_PASTED_CHARS)
                .collect(),
        }
    } else {
        RunOutcome::Skipped
    }));
    Ok(())
}

/// The agent's open question card and the user's answer to it. The card
/// is built from the checked questions only (plain text, capped lengths).
#[derive(Default)]
pub struct QuestionSlot(std::sync::Mutex<QuestionState>);

impl QuestionSlot {
    /// A card is open and unanswered (Home's "Waiting for you").
    pub fn waiting_for_user(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|s| s.open.is_some() && s.answer.is_none())
    }
}

#[derive(Default)]
struct QuestionState {
    /// Counts the cards of this session, so an answer can't land on a
    /// later card than the one the user saw.
    id: u64,
    open: Option<Vec<gardenwall_agent::agent::Question>>,
    /// The user's decision: answers, or `None` for "skip".
    answer: Option<Option<Vec<String>>>,
}

/// Longest answer the user can type instead of picking an option.
const MAX_ANSWER_CHARS: usize = 500;

struct AppAsker {
    slot: Arc<QuestionSlot>,
    stop: Arc<AtomicBool>,
}

impl gardenwall_agent::agent::Asker for AppAsker {
    fn ask<'a>(
        &'a self,
        questions: &'a [gardenwall_agent::agent::Question],
    ) -> BoxFuture<'a, Option<Vec<String>>> {
        Box::pin(async move {
            if let Ok(mut s) = self.slot.0.lock() {
                s.id += 1;
                s.open = Some(questions.to_vec());
                s.answer = None;
            }
            // Waits for the user, or for Stop.
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let mut s = self.slot.0.lock().ok()?;
                if self.stop.load(Ordering::SeqCst) {
                    s.open = None;
                    s.answer = None;
                    return None;
                }
                if let Some(answer) = s.answer.take() {
                    s.open = None;
                    return answer;
                }
            }
        })
    }
}

/// One granted folder's changes in a session.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderChanges {
    folder: String,
    files: Vec<gardenwall_fs::FileDiff>,
}

/// Everything this session changed with its file tools and commands, as
/// one diff per file from before the session first touched it to now. Read
/// from the core's checkpoints (commands' changes are recorded there after
/// each command, see `Workspaces::after_command`). Refused
/// while a turn runs (the files are still changing).
#[tauri::command]
pub async fn agent_changes(
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<Vec<FolderChanges>> {
    let live = live(&state, &sid)?;
    let guard = live
        .session
        .try_lock()
        .map_err(|_| "The agent is working. Look at its changes when it has finished.")?;
    Ok(guard
        .agent
        .session_changes()
        .into_iter()
        .map(|(folder, files)| FolderChanges { folder, files })
        .collect())
}

/// Put one file back to how it was before this session first changed it
/// (the Changes panel; `folder` and `path` as `agent_changes` gave them).
/// The core restores its own saved copy, as a checkpointed step, so Undo
/// reverses it. Returns the changes that remain.
#[tauri::command]
pub async fn agent_revert_file(
    state: State<'_, AppState>,
    sid: String,
    folder: String,
    path: String,
) -> CmdResult<Vec<FolderChanges>> {
    let live = live(&state, &sid)?;
    let guard = live
        .session
        .try_lock()
        .map_err(|_| "The agent is working. Put files back when it has finished.")?;
    if !guard.agent.workspaces.revert_file(&folder, &path)? {
        return Err("That file is already as it was before the session.".into());
    }
    Ok(guard
        .agent
        .session_changes()
        .into_iter()
        .map(|(folder, files)| FolderChanges { folder, files })
        .collect())
}

/// A question card for the window.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionCard {
    id: u64,
    questions: Vec<gardenwall_agent::agent::Question>,
}

/// The session's open question card, if the agent is waiting on one.
#[tauri::command]
pub fn agent_question(state: State<'_, AppState>, sid: String) -> CmdResult<Option<QuestionCard>> {
    let live = live(&state, &sid)?;
    let s = live.question.0.lock().map_err(|_| "state unavailable")?;
    Ok(s.open
        .clone()
        .filter(|_| s.answer.is_none())
        .map(|questions| QuestionCard {
            id: s.id,
            questions,
        }))
}

/// Answer the open question card (`answers`: one per question), or skip
/// it (`None`). The text is the user's own; it is trimmed and capped.
#[tauri::command]
pub fn agent_answer(
    state: State<'_, AppState>,
    sid: String,
    id: u64,
    answers: Option<Vec<String>>,
) -> CmdResult<()> {
    let live = live(&state, &sid)?;
    let mut s = live.question.0.lock().map_err(|_| "state unavailable")?;
    let Some(open) = s.open.as_ref().filter(|_| s.id == id) else {
        return Err("that question is no longer open".into());
    };
    let answers = match answers {
        None => None,
        Some(list) => {
            let cleaned: Vec<String> = list
                .iter()
                .map(|a| a.trim().chars().take(MAX_ANSWER_CHARS).collect())
                .collect();
            if cleaned.len() != open.len() || cleaned.iter().any(String::is_empty) {
                return Err("answer every question, or skip them all".into());
            }
            Some(cleaned)
        }
    };
    s.answer = Some(answers);
    Ok(())
}

/// The open sessions, oldest first.
pub fn live_sessions(state: &AppState) -> Vec<Arc<LiveSession>> {
    state.agents.lock().map(|l| l.clone()).unwrap_or_default()
}

pub(crate) fn live(state: &AppState, id: &str) -> CmdResult<Arc<LiveSession>> {
    live_sessions(state)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "that agent session has ended".to_string())
}

/// Each open session's approval session and title (for the dialog).
pub fn approval_sessions(state: &AppState) -> Vec<(SessionId, String)> {
    let mut out: Vec<(SessionId, String)> = live_sessions(state)
        .iter()
        .map(|s| (s.approval_session, s.title()))
        .collect();
    // Chats with the Web switch on ask through the same dialog, and so do
    // research sub-agents while they run.
    out.extend(crate::chat_web::approval_sessions(state));
    if let Ok(r) = state.research_sessions.lock() {
        out.extend(r.iter().map(|(s, t)| (*s, t.clone())));
    }
    out
}

impl LiveSession {
    pub fn title(&self) -> String {
        self.card
            .lock()
            .map(|c| c.1.clone())
            .unwrap_or_else(|_| "Agent session".into())
    }

    fn set_card(&self, transcript: Option<String>, title: Option<String>) {
        if let Ok(mut c) = self.card.lock() {
            c.0 = transcript;
            if let Some(t) = title {
                c.1 = t;
            }
        }
    }
}

/// Marks a session busy for as long as it lives.
pub(crate) struct BusyGuard<'a>(pub(crate) &'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A session in the session list.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCard {
    id: String,
    title: String,
    folders: Vec<String>,
    transcript: Option<String>,
    project: Option<String>,
    busy: bool,
    waiting: bool,
    created_at: i64,
    /// Programs the user let keep running in the session's sandbox.
    background: Vec<String>,
}

/// The open sessions, oldest first.
#[tauri::command]
pub fn agent_sessions(state: State<'_, AppState>) -> Vec<SessionCard> {
    live_sessions(&state)
        .iter()
        .map(|s| {
            let (transcript, title) = s.card.lock().map(|c| c.clone()).unwrap_or_default();
            SessionCard {
                id: s.id.clone(),
                title,
                folders: s.folders.clone(),
                transcript,
                project: s.project.clone(),
                busy: s.busy.load(Ordering::SeqCst),
                waiting: s.waiting.load(Ordering::SeqCst),
                created_at: s.created_at,
                background: s.background(),
            }
        })
        .collect()
}

/// A compaction as saved in a transcript.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactionRecord {
    first_kept_seq: u64,
    tokens_before: u64,
    summary: Summary,
}

pub(crate) struct ClientModel(pub(crate) InferenceClient);

impl Model for ClientModel {
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

    fn pause(&self, delay: std::time::Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

fn kind_of(approval: &Approval) -> ApprovalKind {
    match approval {
        Approval::RevealSecret(_) => ApprovalKind::RevealSecret,
        Approval::WriteSecret(_) => ApprovalKind::WriteSecret,
        Approval::PersistenceRisk(_) => ApprovalKind::PersistenceRisk,
        Approval::OutsideRead => ApprovalKind::OutsideRead,
        Approval::OutsideWrite => ApprovalKind::OutsideWrite,
        Approval::NetworkCommand => ApprovalKind::NetworkCommand,
        Approval::Egress => ApprovalKind::Egress,
        Approval::McpTool => ApprovalKind::McpTool,
        Approval::RiskyCommand => ApprovalKind::RiskyCommand,
        Approval::HeldBack => ApprovalKind::HeldBack,
        Approval::KeepRunning => ApprovalKind::KeepRunning,
        Approval::AfterWeb => ApprovalKind::AfterWeb,
        Approval::InitRepository => ApprovalKind::InitRepository,
        Approval::AddToolchain => ApprovalKind::AddToolchain,
    }
}

/// Files approval requests and waits for the user's decision.
pub(crate) struct StoreApprover<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) session: SessionId,
    pub(crate) stop: Arc<AtomicBool>,
    /// The session's Send now flag: a waiting request is withdrawn.
    pub(crate) interrupt: Arc<AtomicBool>,
    /// The proposal claimed most recently, awaiting its outcome.
    pub(crate) claimed: std::sync::Mutex<Option<ProposalId>>,
}

impl Approver for StoreApprover<'_> {
    fn research(&self) -> Option<Box<dyn Approver + '_>> {
        crate::research_agent::ChildApprover::start(self).map(|a| Box::new(a) as Box<dyn Approver>)
    }

    fn decides_alone(&self, approval: &Approval, details: &str) -> bool {
        self.grant_decides(approval, details)
    }

    fn finished<'a>(&'a self, success: bool) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let claimed = self.claimed.lock().ok().and_then(|mut c| c.take());
            if let (Some(p), Ok(mut store)) = (claimed, self.state.approvals.lock()) {
                let outcome = if success {
                    Outcome::Completed
                } else {
                    Outcome::Failed
                };
                let _ = store.set_outcome(p, outcome, now_ms());
            }
        })
    }

    fn request<'a>(
        &'a self,
        approval: &'a Approval,
        details: &'a str,
        tool: &'a str,
    ) -> BoxFuture<'a, ApprovalResult> {
        Box::pin(async move {
            let granted = AtomicBool::new(false);
            let result = self.decide(approval, details, tool, &granted).await;
            // The visited list records how each web request was decided.
            if matches!(tool, "web_search" | "web_fetch")
                && let Some(log) = crate::state::visit_log(self.state, self.session)
            {
                use crate::web::visits::Allowed;
                log.decided(match result {
                    ApprovalResult::Approved if granted.load(Ordering::SeqCst) => Allowed::Granted,
                    ApprovalResult::Approved => Allowed::Approved,
                    ApprovalResult::Denied => Allowed::Denied,
                    ApprovalResult::Expired => Allowed::Expired,
                });
            }
            result
        })
    }
}

/// Whether a session grant covers `scope`. `fetch:public` is the research
/// grant: every public HTTPS page (each still validated, so never a private
/// address, plain http or a secret).
pub(crate) fn grant_covers(set: &std::collections::HashSet<String>, scope: &str) -> bool {
    set.contains(scope) || (scope.starts_with("fetch:https://") && set.contains("fetch:public"))
}

/// Whether a grant `scope` may decide (or be created from) a request of
/// `kind`: web scopes only for web requests, MCP scopes only for MCP calls.
/// Every other kind of request always reaches the dialog, whatever its
/// details say (defence in depth: details are built by the core).
pub(crate) fn grant_fits(kind: ApprovalKind, scope: &str) -> bool {
    match kind {
        ApprovalKind::Egress => crate::web::valid_grant(scope),
        ApprovalKind::McpTool => crate::mcp::valid_grant(scope),
        _ => false,
    }
}

impl StoreApprover<'_> {
    /// Whether a session grant the user made covers this request.
    fn grant_decides(&self, approval: &Approval, details: &str) -> bool {
        let scope = serde_json::from_str::<serde_json::Value>(details)
            .ok()
            .and_then(|d| d.get("grant").and_then(|g| g.as_str()).map(str::to_string));
        scope.is_some_and(|scope| {
            grant_fits(kind_of(approval), &scope)
                && self.state.egress_grants.lock().is_ok_and(|g| {
                    g.get(&self.session)
                        .is_some_and(|set| grant_covers(set, &scope))
                })
        })
    }

    /// File the proposal and wait for the decision; `granted` is set when a
    /// session grant decided it.
    async fn decide(
        &self,
        approval: &Approval,
        details: &str,
        tool: &str,
        granted_out: &AtomicBool,
    ) -> ApprovalResult {
        {
            let proposal = {
                let Ok(mut store) = self.state.approvals.lock() else {
                    return ApprovalResult::Expired;
                };
                let proposal = match store.propose_from_tool(
                    self.session,
                    kind_of(approval),
                    details,
                    tool,
                    now_ms(),
                ) {
                    Ok(p) => p,
                    Err(_) => return ApprovalResult::Expired,
                };
                // A session grant the user made replaces only the dialog:
                // the proposal, claim and audit record still happen.
                let granted = self.grant_decides(approval, details);
                if granted
                    && store
                        .approve_by_grant(proposal, self.session, now_ms())
                        .is_err()
                {
                    return ApprovalResult::Expired;
                }
                (proposal, granted)
            };
            let (proposal, granted) = proposal;
            granted_out.store(granted, Ordering::SeqCst);
            if !granted {
                crate::activity::approval_notice(self.state, self.session);
            }
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let Ok(mut store) = self.state.approvals.lock() else {
                    return ApprovalResult::Expired;
                };
                if self.stop.load(Ordering::SeqCst) || self.interrupt.load(Ordering::SeqCst) {
                    let _ = store.cancel(proposal, self.session, now_ms());
                    return ApprovalResult::Expired;
                }
                let _ = store.sweep(now_ms());
                match store.state(proposal) {
                    Ok(ProposalState::Pending) => continue,
                    Ok(ProposalState::Approved) => {
                        // The single-use claim must succeed before anything runs.
                        return match store.claim(proposal, self.session, now_ms()) {
                            Ok(_) => {
                                if let Ok(mut c) = self.claimed.lock() {
                                    *c = Some(proposal);
                                }
                                ApprovalResult::Approved
                            }
                            Err(_) => ApprovalResult::Expired,
                        };
                    }
                    Ok(ProposalState::Rejected) => return ApprovalResult::Denied,
                    _ => return ApprovalResult::Expired,
                }
            }
        }
    }
}

/// Events streamed to the agent view.
#[derive(Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentUiEvent {
    Content {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolStart {
        id: String,
        name: String,
        arguments: String,
        /// The `run_js` call whose program made this call.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    ToolEnd {
        id: String,
        output: String,
        is_error: bool,
    },
    ApprovalWaiting {
        tool: String,
    },
    Usage(UsageView),
    Compacting,
    Compacted {
        tokens_before: u64,
        summary: String,
    },
    CompactionFailed {
        message: String,
    },
    /// Replies became much slower, or the GPU's memory is nearly full.
    GpuWarning(crate::speed::GpuWarning),
    /// The transcript could not be saved; the session continues unsaved.
    NotSaved {
        message: String,
    },
    /// The agent's task list.
    Todos {
        todos: Vec<gardenwall_agent::agent::Todo>,
    },
    /// A request failed before any output and is retried.
    Retrying {
        attempt: u32,
        max: u32,
        delay_ms: u64,
        error: String,
    },
    /// A message sent while the agent worked was delivered to it.
    Steered {
        text: String,
    },
    /// Send now cut the reply being written (kept, marked interrupted).
    Interrupted,
    /// The project instruction files changed and the new ones are used.
    ContextChanged {
        files: Vec<String>,
    },
    /// "One at a time": waiting for another session to finish.
    Waiting,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownView {
    system: u64,
    tools: u64,
    summary: u64,
    conversation: u64,
    tool_results: u64,
}

impl From<ContextBreakdown> for BreakdownView {
    fn from(b: ContextBreakdown) -> Self {
        Self {
            system: b.system,
            tools: b.tools,
            summary: b.summary,
            conversation: b.conversation,
            tool_results: b.tool_results,
        }
    }
}

/// Context use for the context tracker (ADR-014 §2.4).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    prompt_tokens: Option<u64>,
    total_tokens: Option<u64>,
    context_window: Option<u64>,
    /// Compaction starts above this many tokens.
    compact_at: Option<u64>,
    /// Estimates (characters / 4) per part of the context.
    breakdown: BreakdownView,
    prompt_per_second: Option<f64>,
    generation_per_second: Option<f64>,
    ttft_ms: Option<u64>,
    draft_tokens: Option<u64>,
    draft_accepted: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    /// The session's id for later commands.
    pub id: String,
    pub title: String,
    pub busy: bool,
    /// The project this session belongs to.
    pub project: Option<String>,
    pub active: bool,
    pub folders: Vec<String>,
    pub shell: Option<String>,
    pub sandboxed: bool,
    /// Web access offered to the agent, if any.
    pub web: Option<String>,
    /// The attached document collection, if any.
    pub documents: Option<String>,
    /// MCP servers in use, if any.
    pub mcp: Option<String>,
    /// The saved transcript of this session, if any.
    pub transcript: Option<String>,
    /// Whether older messages of this session were compacted into a summary.
    pub compacted: bool,
    /// Planning mode: the agent may look but not change anything.
    pub plan_mode: bool,
    /// Skills offered to the agent (name and where they came from).
    pub skills: Vec<(String, String)>,
    /// Custom commands from the granted folders.
    pub commands: Vec<crate::skills::UserCommand>,
    /// Prompts from the session's MCP servers (`/mcp__<server>__<prompt>`).
    pub mcp_prompts: Vec<crate::mcp::McpPromptView>,
    /// Memories are used in this session.
    pub memories: bool,
    /// The agent's current task list.
    pub todos: Vec<gardenwall_agent::agent::Todo>,
    /// Project instruction files in the system prompt (AGENTS.md, CLAUDE.md).
    pub context_files: Vec<String>,
    /// Programs the user let keep running in the sandbox.
    pub background: Vec<String>,
    /// A research session's approval mode (`None`: not a research session).
    pub research: Option<crate::state::WebMode>,
    /// Web content was read: changes and commands ask first.
    pub web_tainted: bool,
    /// Searches and pages in the visited list.
    pub visits: usize,
    /// Secret files in the folders that the agent's commands can't reach.
    pub hidden_secrets: Vec<String>,
    /// Why this session has no commands, when its sandbox couldn't be set up.
    pub commands_off: Option<String>,
    /// Toolchains changed after this session started: the banner offers a
    /// sandbox restart (ADR-024 §3 A1).
    pub restart_offer: Option<String>,
    /// A toolchain the agent asked for isn't installed: where to get it.
    pub install_card: Option<crate::sandbox_restart::InstallCard>,
}

pub(crate) fn view_of(live: &LiveSession, s: &AgentSession) -> AgentView {
    view(live, s)
}

fn view(live: &LiveSession, s: &AgentSession) -> AgentView {
    AgentView {
        id: live.id.clone(),
        title: live.title(),
        busy: live.busy.load(Ordering::SeqCst),
        project: s.project.clone(),
        active: true,
        folders: s.folders.iter().map(|f| f.display().to_string()).collect(),
        shell: s.shell.as_ref().map(|sh| sh.name().to_string()),
        sandboxed: s.agent.policy.sandbox_available,
        web: s.web.clone(),
        documents: s.documents.clone(),
        mcp: s.mcp.clone(),
        transcript: s.transcript.clone(),
        compacted: s.agent.summary().is_some(),
        plan_mode: s.agent.plan_mode(),
        skills: s
            .agent
            .skills()
            .iter()
            .map(|k| (k.name.clone(), k.source.clone()))
            .collect(),
        commands: s.commands.clone(),
        mcp_prompts: live.mcp.as_ref().map(|m| m.prompts()).unwrap_or_default(),
        memories: s.memories_on,
        todos: s.agent.todos().to_vec(),
        context_files: s.agent.context_files(),
        background: s
            .shell
            .as_ref()
            .map(|sh| sh.background())
            .unwrap_or_default(),
        research: s.research,
        web_tainted: s.agent.web_tainted(),
        visits: s.visits.as_ref().map_or(0, |v| v.all().0.len()),
        hidden_secrets: s
            .shell
            .as_ref()
            .map(|sh| sh.hidden_files())
            .unwrap_or_default(),
        commands_off: s.agent.commands_off().map(str::to_string),
        restart_offer: live.restart_offer.lock().ok().and_then(|o| o.clone()),
        install_card: live
            .install_card
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .and_then(|id| crate::sandbox_restart::install_card(&id)),
    }
}

/// Stop the programs the user let keep running in a session's sandbox.
#[tauri::command]
pub async fn agent_stop_background(
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    if let Some(shell) = guard.shell.as_ref() {
        shell.stop_background();
    }
    Ok(view(&live, &guard))
}

/// Fill in an MCP prompt the user typed as `/mcp__<server>__<prompt> args`.
/// The text goes back to the composer for the user to review and send; it
/// is never sent on its own (a server's prompt is untrusted text).
#[tauri::command]
pub async fn agent_mcp_prompt(
    state: State<'_, AppState>,
    sid: String,
    command: String,
    args: String,
) -> CmdResult<String> {
    if command.len() > 128 || args.chars().count() > 16_000 {
        return Err("that prompt command is too long".into());
    }
    let live = live(&state, &sid)?;
    let hub = live.mcp.as_ref().ok_or("this session has no MCP servers")?;
    hub.fill_prompt(&command, &args).await
}

/// One open session. Waits while that session is working.
#[tauri::command]
pub async fn agent_state(state: State<'_, AppState>, sid: String) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    Ok(view(&live, &guard))
}

/// The `search_documents` tool for a collection the user chose.
fn attach_docs(
    app: &AppHandle,
    state: &AppState,
    collection: Option<String>,
) -> CmdResult<Option<Arc<dyn gardenwall_agent::agent::Documents>>> {
    let Some(id) = collection else {
        return Ok(None);
    };
    let name = with_conversations(state, |s| s.list_collections(None))?
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| c.name)
        .ok_or("that document collection no longer exists")?;
    Ok(Some(Arc::new(crate::documents::AppDocs {
        app: app.clone(),
        collection: id,
        name,
    })))
}

/// Open the native folder picker and start an agent session with the
/// chosen folders.
#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    collection: Option<String>,
    memories: Option<bool>,
) -> CmdResult<Option<AgentView>> {
    let docs = attach_docs(&app, &state, collection)?;
    room_for_session(&state)?;
    // In a project: its stored folders. Without
    // one: folders from the native picker, which become a new project.
    let (folders, project) = match project {
        Some(id) => {
            let (p, folders) = crate::projects::project_folders(&state, &id)?;
            (folders, p.id)
        }
        None => {
            let Some(picked) = pick_folders(app).await? else {
                return Ok(None);
            };
            let folders = picked
                .iter()
                .map(|f| crate::projects::checked(f))
                .collect::<CmdResult<Vec<_>>>()?;
            let p = crate::projects::create_for(&state, &folders)?;
            (folders, p.id)
        }
    };
    begin(
        &state,
        folders,
        None,
        docs,
        memories.unwrap_or(true),
        Some(project),
    )
    .await
    .map(Some)
}

/// Why no agent or research session starts in lockdown.
pub(crate) const LOCKED: &str = "Lockdown is on: only chat works, with no tools. Turn lockdown off (type UNLOCK) to use the agent again.";

fn room_for_session(state: &AppState) -> CmdResult<()> {
    if live_sessions(state).len() >= MAX_SESSIONS {
        return Err(format!(
            "at most {MAX_SESSIONS} agent sessions can be open; end one first"
        ));
    }
    Ok(())
}

/// Folders chosen in the native picker; `None` when cancelled.
pub(crate) async fn pick_folders(app: AppHandle) -> CmdResult<Option<Vec<PathBuf>>> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folders())
            .await
            .map_err(|e| e.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let folders: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    if folders.is_empty() {
        return Err("no folder was chosen".into());
    }
    Ok(Some(folders))
}

/// Start a session on `folders`, optionally continuing a saved transcript.
pub(crate) async fn begin(
    state: &AppState,
    folders: Vec<PathBuf>,
    resume: Option<(String, Restore)>,
    docs: Option<Arc<dyn gardenwall_agent::agent::Documents>>,
    memories_on: bool,
    project: Option<String>,
) -> CmdResult<AgentView> {
    // Lockdown is chat only: no agent or
    // research session starts. Checked again when it is added.
    if state.settings().lockdown {
        return Err(LOCKED.into());
    }
    room_for_session(state)?;
    let mut workspaces = Workspaces::new(state.paths.checkpoints.clone());
    let mut granted = Vec::new();
    for folder in &folders {
        let id = workspaces.grant(folder)?;
        granted.push(
            workspaces
                .root(id)
                .map(PathBuf::from)
                .unwrap_or_else(|| folder.clone()),
        );
    }
    let context = load_context(&workspaces);
    // A session without folders is a research session (ADR-018): no shell,
    // no file tools, no MCP or binary tools; only the web and a task list.
    let research = folders.is_empty();
    // Lockdown mode: no shell either (the policy refuses commands too).
    let scratch = ScratchDir::new(state);
    let (shell, commit_hooks, commands_off): MadeShell = if research || state.settings().lockdown {
        (None, None, None)
    } else {
        make_shell(state, &granted, scratch.path(), project.as_deref())
    };
    // The scratch folder goes with the sandbox that uses it (and is deleted
    // right here when there is none).
    let shell = shell.map(|sh| Arc::new(SessionShell::new(sh, Some(Box::new(scratch)))));
    let visits = Arc::new(crate::web::visits::VisitLog::default());
    let web = crate::web::AppWeb::new(state, visits.clone());
    if research && web.is_none() {
        return Err("Research needs web access: turn off offline mode in Settings first.".into());
    }
    // Sessions with folders read the web only through the research
    // sub-agent (set below), so the header says so.
    let web_label = web.as_ref().map(|w| {
        if research {
            w.describe()
        } else {
            format!(
                "{} through a research assistant (you approve each question)",
                w.describe()
            )
        }
    });
    let policy = SessionPolicy {
        workspaces: workspaces.ids().into_iter().collect(),
        revealed_secrets: Default::default(),
        sandbox_available: shell.is_some(),
        egress_available: web.is_some(),
        plan_mode: false,
        web_tainted: false,
        lockdown: state.settings().lockdown,
    };
    let approval_session = {
        let mut store = state
            .approvals
            .lock()
            .map_err(|_| "approval store unavailable")?;
        store.start_session(now_ms()).map_err(|e| e.to_string())?
    };
    let web: Option<Arc<dyn gardenwall_agent::agent::Web>> =
        web.map(|w| w as Arc<dyn gardenwall_agent::agent::Web>);
    let docs_label = docs.as_ref().map(|d| d.describe());
    // MCP servers start with the session and stop when it ends.
    let (mcp, mcp_errors) = if research {
        (None, Vec::new())
    } else {
        crate::mcp::AppMcp::start_enabled(state).await
    };
    for e in mcp_errors {
        state.add_notice(e);
    }
    let mcp_label = mcp.as_ref().map(|m| m.describe());
    let mcp_hub = mcp.clone();
    let mcp: Option<Arc<dyn gardenwall_agent::agent::Mcp>> =
        mcp.map(|m| m as Arc<dyn gardenwall_agent::agent::Mcp>);
    let skills = crate::skills::session_skills(state, &workspaces);
    let commands = crate::skills::project_commands(&workspaces);
    let docs_kept = docs.clone();
    let mut agent = Agent::new(
        workspaces,
        policy,
        shell.as_deref().map(|s| s as &dyn Shell),
        web,
        docs,
        mcp,
        &context,
    )
    .with_skills(skills);
    // Why commands are off, for the agent (the session view shows it too).
    agent.set_commands_off(commands_off);
    // What the agent has, and creating a Git repository on the user's yes
    // (ADR-022 §2A, §2C): sessions with folders and commands, not lockdown.
    if shell.is_some() {
        agent.set_tools_note(Some(tools_note(state, project.as_deref(), &granted)));
        agent.set_parent_folders(crate::parent_folders::session_ready(
            state,
            project.as_deref(),
            &granted,
        ));
        if !state.settings().lockdown {
            agent.set_repo_init(Some(Arc::new(AppRepoInit)));
        }
    }
    // Toolchains the agent may ask for (ADR-024 §3 A2): sessions with
    // folders and commands, not research or lockdown.
    let install_card: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let sandbox_toolchains = Arc::new(std::sync::Mutex::new(if shell.is_some() {
        crate::toolchains::added_for(&state.settings().agent, project.as_deref())
    } else {
        Vec::new()
    }));
    if let (Some(sh), Some(app)) = (&shell, state.app.get())
        && !state.settings().lockdown
    {
        agent.set_toolchain_host(Some(Arc::new(crate::sandbox_restart::AppToolchains {
            app: app.clone(),
            folders: granted.clone(),
            project: project.clone(),
            shell: sh.clone(),
            install_card: install_card.clone(),
            in_sandbox: sandbox_toolchains.clone(),
        })));
    }
    if research {
        agent.set_allowed_tools(RESEARCH_TOOLS.iter().map(|t| t.to_string()).collect());
        agent.set_research(true);
    } else {
        // The sandboxed binary analyzer (and Ghidra, when set up).
        agent.set_binary(Some(crate::binary::for_agent(state)));
    }
    let web_settings = state.settings().web;
    agent.web_limits = gardenwall_agent::agent::WebLimits {
        searches: web_settings.max_searches as usize,
        pages: web_settings.max_pages as usize,
    };
    if let Ok(mut v) = state.web_visits.lock() {
        v.insert(approval_session, visits.clone());
    }
    // Memories: the approved ones in the prompt, suggestions through the
    // `remember` tool (pending until the user accepts them).
    let memory_queue = crate::memory::new_queue();
    let memories_on = memories_on && !state.settings().lockdown;
    if memories_on {
        agent.set_memory_sink(Some(
            memory_queue.clone() as Arc<dyn gardenwall_agent::agent::MemorySink>
        ));
        agent.set_memories(crate::memory::agent_block(state, &granted));
    }
    let transcript = match resume {
        Some((id, restore)) => {
            agent.restore(restore);
            Some(id)
        }
        None => None,
    };
    // Checkpoint turns name the session, so a handoff lists its changes.
    agent.workspaces.set_session(transcript.clone());
    // A continued session keeps its visited list.
    if let Some(id) = &transcript {
        visits.restore(crate::web::saved_visits(
            state,
            gardenwall_store::conversations::VisitOwner::Agent(id),
        ));
    }
    let stop = agent.stop_handle();
    let steering = agent.steering_handle();
    let interrupt = agent.interrupt_handle();
    // Question cards (Settings → Agent): the agent may ask the user to
    // choose. Not for research sessions, which keep to their short tool list.
    let question = Arc::new(QuestionSlot::default());
    if state.settings().agent.ask_user && !research {
        agent.set_asker(Some(Arc::new(AppAsker {
            slot: question.clone(),
            stop: stop.clone(),
        })));
    }
    // "Run this yourself" cards: commands have
    // no network, so the agent may ask the user to run one. Not in research
    // sessions or lockdown mode.
    // Gardenwall's pre-commit secret check, when the session's git uses it.
    agent.set_commit_check(commit_hooks);
    let run_request = Arc::new(RunSlot::default());
    if state.settings().agent.run_requests && !research && !state.settings().lockdown {
        agent.set_run_asker(Some(Arc::new(AppRunAsker {
            slot: run_request.clone(),
            stop: stop.clone(),
        })));
    }
    // Web research goes through a sub-agent in sessions with folders (owner
    // decision 2026-11-21, ADR-019): the user approves each question and, by
    // default, reviews the findings. Research sessions read the web directly.
    let research_slot = Arc::new(crate::research_agent::ResearchSlot::default());
    if !research
        && !state.settings().lockdown
        && let Some(app) = state.app.get().cloned()
    {
        agent.set_research_host(Some(Arc::new(crate::research_agent::AppResearch {
            slot: research_slot.clone(),
            stop: stop.clone(),
            review_on: Box::new(move || {
                use tauri::Manager;
                app.state::<AppState>().settings().web.review_findings
            }),
            question: Default::default(),
        })));
    }
    let title = match &transcript {
        Some(id) => with_conversations(state, |s| s.get_transcript(id))
            .map(|t| t.summary.title)
            .unwrap_or_else(|_| "Agent session".into()),
        None if research => "New research".into(),
        None => format!(
            "New session · {}",
            granted
                .first()
                .and_then(|f| f.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
    };
    let folder_names: Vec<String> = granted.iter().map(|f| f.display().to_string()).collect();
    let card = (transcript.clone(), title);
    let shared_shell = shell.clone();
    let session = AgentSession {
        agent,
        shell,
        approval_session,
        folders: granted,
        stop: stop.clone(),
        transcript,
        saving_failed: false,
        web: web_label,
        documents: docs_label,
        mcp: mcp_label,
        docs: docs_kept,
        commands,
        memories_on,
        memory_queue,
        shared_output: Vec::new(),
        project: project.clone(),
        compactions: 0,
        handoff_from: None,
        // Research sessions start asking for each request; the mode chosen
        // at start (`agent_start_research`) or later sets the grant.
        research: research.then_some(crate::state::WebMode::Ask),
        visits: Some(visits),
    };
    let live = Arc::new(LiveSession {
        id: crate::commands::new_token(),
        session: tokio::sync::Mutex::new(session),
        approval_session,
        stop,
        steering,
        interrupt,
        busy: AtomicBool::new(false),
        waiting: AtomicBool::new(false),
        folders: folder_names,
        created_at: now_ms(),
        project,
        card: std::sync::Mutex::new(card),
        activity: std::sync::Mutex::new(Default::default()),
        mcp: mcp_hub,
        question,
        run_request,
        research: research_slot,
        shell: std::sync::Mutex::new(shared_shell),
        restart_offer: Default::default(),
        install_card,
        sandbox_toolchains,
    });
    let shown = {
        let guard = live.session.lock().await;
        view(&live, &guard)
    };
    {
        // Checked under the list's lock: lockdown sets the flag, then
        // empties the list under the same lock, so a session starting at
        // that moment is either refused here or ended there.
        let mut list = state
            .agents
            .lock()
            .map_err(|_| "session state unavailable")?;
        if state.settings().lockdown {
            return Err(LOCKED.into());
        }
        list.push(live.clone());
    }
    // Ready the server's cache while the user writes the first request.
    if let Some(app) = state.app.get() {
        spawn_warm_up(app.clone(), live);
    }
    Ok(shown)
}

/// The name and email the agent's commits carry
/// . `None` leaves git to its own settings inside the sandbox,
/// where it then asks for an identity: the user's own couldn't be read, or
/// no git is available.
#[cfg(any(windows, target_os = "linux"))]
fn git_identity(
    agent: &crate::state::AgentSettings,
    tools: &[PathBuf],
    first_folder: Option<&PathBuf>,
) -> Option<gardenwall_agent::git::Identity> {
    use crate::state::GitIdentity;
    use gardenwall_agent::git::{Identity, host_git, user_identity};
    match agent.git_identity {
        GitIdentity::Agent => Some(Identity::agent()),
        GitIdentity::Custom => Identity::new(&agent.git_name, &agent.git_email),
        GitIdentity::Mine => user_identity(&host_git(tools)?, first_folder.map(PathBuf::as_path)),
    }
}

/// Stage the pre-commit secret check: the
/// `gardenwall-gitscan` helper as `pre-commit` in a folder that only the
/// core writes (sandboxes get read and run access), with no allow marks
/// left over. Returns the folder.
#[cfg(windows)]
fn stage_commit_check(state: &AppState) -> std::io::Result<PathBuf> {
    use gardenwall_agent::commit_check::{ALLOW_DIR, HELPER, clear_allows};
    let dir = state.paths.root.join("githooks");
    let helper = crate::state::stage_helper(&dir, HELPER)?;
    let hook = dir.join("pre-commit");
    let same = std::fs::read(&hook).ok() == std::fs::read(&helper).ok();
    if !same {
        let tmp = dir.join("pre-commit.tmp");
        std::fs::copy(&helper, &tmp)?;
        std::fs::rename(&tmp, &hook)?;
    }
    std::fs::create_dir_all(dir.join(ALLOW_DIR))?;
    clear_allows(&dir);
    std::fs::canonicalize(&dir)
}

/// The git settings of the agent's commands, through the environment
/// (`GIT_CONFIG_*`), so they apply to those commands only and change
/// nothing in the project or the user's own git config:
/// - `core.safecrlf=false`: with Windows' usual `core.autocrlf`, `git add`
///   otherwise prints a line-ending warning per file (about 20,000 lines
///   for a vendored dependency tree), all of it read by the model;
/// - `core.hooksPath`: the commit check's folder, when it is staged.
#[cfg(windows)]
fn agent_git_config(
    hooks: Option<&std::path::Path>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut pairs = vec![("core.safecrlf".to_string(), "false".to_string())];
    if let Some(hooks) = hooks {
        let shown = gardenwall_agent::display::shown(hooks).replace('\\', "/");
        pairs.push(("core.hooksPath".to_string(), shown));
    }
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), pairs.len().to_string())];
    for (i, (key, value)) in pairs.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), value));
    }
    env.into_iter().map(|(k, v)| (k.into(), v.into())).collect()
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn make_shell(
    state: &AppState,
    folders: &[PathBuf],
    scratch: &std::path::Path,
    project: Option<&str>,
) -> MadeShell {
    let scratch = scratch.to_path_buf();
    let settings = state.settings();
    let mut tools = settings.agent_tool_folders;
    // A folder the sandbox can't be given (e.g. Node.js's MSI install, whose
    // permissions only an administrator can change) would make the grant,
    // and so the whole session, fail; it is left out and Settings → Agent
    // tools shows the one-time fix.
    #[cfg(windows)]
    tools.retain(|t| gardenwall_sandbox::sandbox_can_use(t));
    // Never a folder with credentials in it, even one added before this
    // check existed (ADR-022 hardening).
    tools.retain(|t| match crate::toolchains::grant_problem(t) {
        None => true,
        Some(problem) => {
            state.add_notice(format!(
                "A tool folder was left out of this session: {problem}"
            ));
            false
        }
    });
    // Python's folder joins the read-only tool folders. It is offered only
    // when every AppContainer can already read it, so no permission is
    // changed (see gardenwall_agent::python).
    if settings.agent.python
        && let Some(dir) = gardenwall_agent::python::find_python()
            .as_deref()
            .and_then(std::path::Path::parent)
        && !tools.iter().any(|t| t == dir)
    {
        tools.push(dir.to_path_buf());
    }
    #[allow(unused_mut)]
    let mut git_env = git_identity(&settings.agent, &tools, folders.first())
        .map(|id| id.env())
        .unwrap_or_default();
    // Toolchains the user added (ADR-022 §2B): their folders are read-only
    // tool folders, their caches private folders in the scratch area.
    let ids = crate::toolchains::added_for(&settings.agent, project);
    // "Folders above the project" (ADR-025): the folders this user owns are
    // held for the session; toolchains that need it join only when every
    // folder above is ready.
    let parent = crate::parent_folders::enabled(&settings.agent, project);
    if parent {
        crate::parent_folders::hold_for(state, folders);
    }
    let parent_ready = parent && crate::parent_folders::ready(folders);
    let ids: Vec<String> = ids
        .into_iter()
        .filter(|i| parent_ready || crate::toolchains::needs_parent_folders(i).is_none())
        .collect();
    for t in &crate::toolchains::for_session(&ids, folders.first().map(PathBuf::as_path)) {
        for f in &t.folders {
            #[cfg(windows)]
            if !gardenwall_sandbox::sandbox_can_use(f) {
                continue;
            }
            if crate::toolchains::grant_problem(f).is_some() {
                continue;
            }
            if !tools.contains(f) {
                tools.push(f.clone());
            }
        }
        git_env.extend(crate::toolchains::env_for(t, &scratch));
    }
    // The pre-commit secret check (Windows): its folder is a read-only tool
    // folder of the sandbox, and git is pointed at it. If it can't be set
    // up, commits are still scanned after they're made.
    #[cfg(windows)]
    let commit_hooks = match stage_commit_check(state) {
        Ok(dir) => {
            tools.push(dir.clone());
            Some(dir)
        }
        Err(e) => {
            state.add_notice(format!(
                "Gardenwall's check that stops secrets from being committed isn't available in this session ({e}). Commits are still scanned after they're made."
            ));
            None
        }
    };
    #[cfg(windows)]
    git_env.extend(agent_git_config(commit_hooks.as_deref()));
    #[cfg(not(windows))]
    let commit_hooks: Option<PathBuf> = None;
    // Python's start-up fix for the sandbox (ADR-024
    // ): a folder only the core writes, read-only for commands,
    // and the only entry on PYTHONPATH.
    #[cfg(windows)]
    match gardenwall_agent::preloads::stage(&state.paths.root.join("preloads")) {
        Ok(dir) => {
            git_env.extend(gardenwall_agent::preloads::env(&dir));
            tools.push(dir);
        }
        Err(e) => state.add_notice(format!(
            "Python's temporary folders may not work in this session's sandbox: Gardenwall couldn't prepare its fix ({e})."
        )),
    }
    match gardenwall_agent::sandbox_shell::SandboxShell::new_with_env(
        folders,
        &scratch,
        &tools,
        Some(state.paths.grants.clone()),
        match settings.agent.shell {
            crate::state::AgentShell::PowerShell => {
                gardenwall_agent::sandbox_shell::ShellKind::PowerShell
            }
            crate::state::AgentShell::WindowsPowerShell => {
                gardenwall_agent::sandbox_shell::ShellKind::WindowsPowerShell
            }
            crate::state::AgentShell::Cmd => gardenwall_agent::sandbox_shell::ShellKind::Cmd,
        },
        &git_env,
    ) {
        Ok(mut shell) => {
            // Held-back changes wait in the app's data folder, outside
            // every grant.
            shell.set_quarantine(state.paths.root.join("held-back"));
            for problem in shell.unprotected() {
                state.add_notice(format!(
                    "A protected location couldn't be made read-only for this session's commands ({problem}). Changes to it are still caught after each command."
                ));
            }
            if let Some(problem) = shell.secret_problem() {
                state.add_notice(problem.to_string());
            }
            (Some(Arc::new(shell)), commit_hooks, None)
        }
        // Shown on the session itself and told to the agent (see `begin`).
        Err(e) => (None, None, Some(commands_off_reason(&e))),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn make_shell(
    _state: &AppState,
    _folders: &[PathBuf],
    _scratch: &std::path::Path,
    _project: Option<&str>,
) -> MadeShell {
    (
        None,
        None,
        Some("this system has no sandbox for commands yet".into()),
    )
}

/// The agent's tools as its prompt lists them (ADR-022 §2C): what is in its
/// sandbox, and what is on this PC but not added. Names and versions only.
pub(crate) fn tools_note(state: &AppState, project: Option<&str>, granted: &[PathBuf]) -> String {
    let folder = granted.first().map(PathBuf::as_path);
    // "Folders above the project" (ADR-025): Ruby and Gradle need it.
    let parent = crate::parent_folders::session_ready(state, project, granted);
    let settings = state.settings();
    let folders = &settings.agent_tool_folders;
    let has = |exe: &str| {
        folders.iter().any(|f| {
            ["", "bin", "cmd"]
                .iter()
                .any(|sub| f.join(sub).join(exe).is_file())
        })
    };
    let mut in_sandbox = Vec::new();
    let mut on_pc = Vec::new();
    if has("git.exe") {
        in_sandbox.push("Git".to_string());
    } else if gardenwall_agent::git::find_git_for_windows().is_some() {
        on_pc.push("Git".to_string());
    }
    let detected = crate::toolchains::detect();
    // Python or Node.js reached without the toolchain (the older Python
    // setting, a tool folder), when no toolchain of that kind is found.
    for (id, name) in [("python", "Python"), ("node", "Node.js")] {
        if crate::toolchains::present_otherwise(&settings, id)
            && !detected.iter().any(|t| t.id == id)
        {
            in_sandbox.push(name.to_string());
        }
    }
    let ids = crate::toolchains::added_for(&settings.agent, project);
    let session = crate::toolchains::for_session(&ids, folder);
    for t in detected {
        // The version this session really gets (a project's Rust pin).
        let t = session.iter().find(|s| s.id == t.id).cloned().unwrap_or(t);
        let label = format!("{} {}", t.name, t.version);
        let usable_here = parent || crate::toolchains::needs_parent_folders(t.id).is_none();
        if ((ids.iter().any(|a| a == t.id)
            && t.folders
                .iter()
                .all(|f| crate::toolchains::grant_problem(f).is_none()))
            || crate::toolchains::present_otherwise(&settings, t.id))
            && usable_here
        {
            in_sandbox.push(label);
        } else {
            on_pc.push(label);
        }
    }
    crate::toolchains::note_with(&in_sandbox, &on_pc, parent)
}

/// Creates a Git repository outside the sandbox (ADR-022 §2A): Git for
/// Windows, fixed arguments, in a granted folder's root that the agent
/// crate has checked has no `.git` yet. Only on the user's approval.
pub(crate) struct AppRepoInit;

impl gardenwall_agent::repo_init::RepoInit for AppRepoInit {
    fn init<'a>(
        &'a self,
        root: &'a std::path::Path,
    ) -> gardenwall_agent::agent::BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let root = root.to_path_buf();
            tauri::async_runtime::spawn_blocking(move || init_repository(&root))
                .await
                .map_err(|e| e.to_string())?
        })
    }
}

/// `git init -b main` in `root`, outside the sandbox. Shared by the agent's
/// `init_repository` and the project page's Set up card.
pub(crate) fn init_repository(root: &std::path::Path) -> Result<String, String> {
    let git = gardenwall_agent::git::find_git_for_windows()
        .map(|g| g.join("cmd").join("git.exe"))
        .ok_or("Git for Windows isn't installed (Gardenwall looks in Program Files\\Git)")?;
    if root.join(".git").exists() {
        return Err("the folder already has a .git folder".into());
    }
    let mut command = std::process::Command::new(&git);
    command
        .args(["init", "-b", "main"])
        .current_dir(root)
        .stdin(std::process::Stdio::null());
    // Git variables in Gardenwall's own environment (GIT_DIR, GIT_WORK_TREE,
    // GIT_CONFIG_*) could send the repository elsewhere or change how it is
    // made: none reach this command.
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(&name);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flash from the app.
        command.creation_flags(0x0800_0000);
    }
    let out = command
        .output()
        .map_err(|e| format!("Git didn't start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git init failed: {}", err.trim()));
    }
    let shown = gardenwall_agent::display::shown(root);
    Ok(format!(
        "Gardenwall created a Git repository in {shown} (branch main). Git commands work in your sandbox now: git add, commit, status, log. The repository's settings and hooks are protected: commands can't change them."
    ))
}

/// The shell, the commit-check folder, and why commands are off (if they are).
pub(crate) type MadeShell = (Option<Arc<dyn Shell>>, Option<PathBuf>, Option<String>);

/// The sandbox's refusal in plain words, with what to do about it.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn commands_off_reason(error: &str) -> String {
    let fix = if error.contains("link or reparse point") {
        " A link (a junction or symbolic link) inside a granted folder could lead outside it, so the sandbox won't take the folder. Remove the link, or choose a folder without one."
    } else if error.contains("multiple hard links") {
        " A file with several hard links could also be reached from outside the folder, so the sandbox won't take the folder. Remove the extra link, or choose a folder without it."
    } else if error.contains("too many entries") {
        " The folder has too many files for the sandbox to check. Choose a smaller folder."
    } else {
        ""
    };
    format!("{error}.{fix}")
}

/// End a session: stop its turn, cancel its approvals, drop its grants.
/// Its sandbox ends (and its file grants are revoked) once a running turn
/// has stopped.
fn end_session(state: &AppState, id: &str) {
    let removed = state.agents.lock().ok().and_then(|mut list| {
        let i = list.iter().position(|s| s.id == id)?;
        Some(list.remove(i))
    });
    if let Some(old) = removed {
        old.stop.store(true, Ordering::SeqCst);
        release(state, &old);
    }
}

/// A session's approvals and grants end (ADR-011 §2.6).
fn release(state: &AppState, old: &LiveSession) {
    if let Ok(mut store) = state.approvals.lock() {
        let _ = store.end_session(old.approval_session, now_ms());
    }
    if let Ok(mut g) = state.egress_grants.lock() {
        g.remove(&old.approval_session);
    }
    if let Ok(mut v) = state.web_visits.lock() {
        v.remove(&old.approval_session);
    }
}

/// Lockdown (GW-019): end every session at once. Turns stop, the reply
/// being written is cut, the running command and every program in each
/// sandbox are killed, cards are withdrawn (they watch the stop flag), and
/// approvals and grants end. Returned so the caller can wait until each
/// turn has really ended; each sandbox goes when the last holder drops it.
pub(crate) fn end_all_sessions(state: &AppState) -> Vec<Arc<LiveSession>> {
    let all: Vec<Arc<LiveSession>> = state
        .agents
        .lock()
        .map(|mut list| std::mem::take(&mut *list))
        .unwrap_or_default();
    for old in &all {
        old.stop.store(true, Ordering::SeqCst);
        old.interrupt.store(true, Ordering::SeqCst);
        if let Ok(shell) = old.shell.lock()
            && let Some(shell) = shell.as_ref()
        {
            shell.kill_now();
        }
        release(state, old);
    }
    all
}

#[tauri::command]
pub fn agent_end(state: State<'_, AppState>, sid: String) -> CmdResult<()> {
    end_session(&state, &sid);
    Ok(())
}

/// Stop the session's running turn (safe while `agent_send` runs), or its
/// turn waiting to start.
#[tauri::command]
pub fn agent_stop(state: State<'_, AppState>, sid: String) -> CmdResult<()> {
    live(&state, &sid)?.stop.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurnResult {
    pub end: &'static str,
    /// Memories the agent suggested this turn (now waiting for review).
    pub suggested: usize,
    /// Steering messages the turn ended before delivering; they go back to
    /// the composer (Pi returns queued messages to the editor).
    pub undelivered: Vec<String>,
    /// The request's text when it was stopped before anything ran: it was
    /// taken back out of the session and goes back to the message box.
    pub withdrawn: Option<String>,
    /// Why a fresh session from a handoff is worth suggesting now, if it is.
    pub handoff: Option<gardenwall_agent::handoff::Suggestion>,
    /// Times this session has been compacted.
    pub compactions: usize,
}

/// Send a request to the agent and stream its work.
#[tauri::command]
pub async fn agent_send(
    state: State<'_, AppState>,
    sid: String,
    text: String,
    images: Option<Vec<String>>,
    files: Option<Vec<String>>,
    on_event: Channel<AgentUiEvent>,
) -> CmdResult<AgentTurnResult> {
    let text = text.trim().to_string();
    if text.is_empty() || text.len() > 1 << 20 {
        return Err("the request must be non-empty and at most 1 MiB".into());
    }
    // Attached images are held by the core (picked, pasted or dropped);
    // the webview only names them by token.
    let tokens = images.unwrap_or_default();
    if tokens.len() > gardenwall_store::conversations::MAX_IMAGES_PER_MESSAGE {
        return Err("at most 4 images can be sent with a request".into());
    }
    let (client, context_window, model_pid, model_sha, vision) = {
        let engine = state.engine.lock().await;
        let loaded = engine.as_ref().ok_or("no model is loaded")?;
        (
            loaded.server.client().clone(),
            loaded.props.context_window,
            loaded.server.pid(),
            loaded.sha256.clone(),
            loaded.vision,
        )
    };
    if !tokens.is_empty() && !vision {
        return Err(
            "the loaded model can't see images; pair it with an image projector on the Models page"
                .into(),
        );
    }
    let live = live(&state, &sid)?;
    if live.busy.swap(true, Ordering::SeqCst) {
        return Err(
            "This session is still busy with another step. Try again when it finishes.".into(),
        );
    }
    let _busy = BusyGuard(&live.busy);
    live.stop.store(false, Ordering::SeqCst);
    if let Ok(mut a) = live.activity.lock() {
        a.start_turn();
    }
    // "One at a time": wait until no other session is working.
    let _permit = if state.settings().agent.sessions == crate::state::SessionMode::OneAtATime {
        match state.agent_run.try_acquire() {
            Ok(p) => Some(p),
            Err(_) => {
                let _ = on_event.send(AgentUiEvent::Waiting);
                live.waiting.store(true, Ordering::SeqCst);
                let permit = loop {
                    tokio::select! {
                        p = state.agent_run.acquire() => break p.ok(),
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {
                            if live.stop.load(Ordering::SeqCst) {
                                break None;
                            }
                        }
                    }
                };
                live.waiting.store(false, Ordering::SeqCst);
                match permit {
                    Some(p) => Some(p),
                    None => {
                        if let Ok(mut a) = live.activity.lock() {
                            a.end_turn(crate::activity::TurnOutcome::Stopped, None);
                        }
                        return Ok(AgentTurnResult {
                            end: "stopped",
                            suggested: 0,
                            undelivered: live
                                .steering
                                .lock()
                                .map(|mut q| q.drain(..).collect())
                                .unwrap_or_default(),
                            // Stopped while waiting: nothing reached the session.
                            withdrawn: Some(text),
                            handoff: None,
                            compactions: 0,
                        });
                    }
                }
            }
        }
    } else {
        None
    };
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    // Output of the user's `!` commands goes with this request.
    let text = if session.shared_output.is_empty() {
        text
    } else {
        let shared = std::mem::take(&mut session.shared_output);
        format!(
            "<user-shell>\nCommands I ran in the sandbox, with their output:\n{}\n</user-shell>\n\n{text}",
            shared.join("\n\n")
        )
    };
    // Taken only once the request can be sent, so a refusal above keeps
    // the attachments for another try.
    let attached: Vec<Vec<u8>> = {
        let mut pending = state
            .pending_images
            .lock()
            .map_err(|_| "state unavailable")?;
        tokens
            .iter()
            .filter_map(|t| pending.remove(t).map(|(_, bytes)| bytes))
            .collect()
    };
    let image_urls: Vec<String> = attached
        .iter()
        .filter_map(|b| {
            use base64::Engine;
            let mime = gardenwall_store::conversations::sniff_image(b)?;
            Some(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(b)
            ))
        })
        .collect();
    let mut unsaved_images = Some(attached);
    // Attached files go with the request as marked data blocks.
    let text = {
        let mut text = text;
        for block in crate::attach::take_blocks(&state, &files.unwrap_or_default())? {
            text.push_str(&block);
        }
        text
    };
    if let Ok(mut store) = state.approvals.lock() {
        // Sending a request is interactive activity.
        store
            .touch(session.approval_session, now_ms())
            .map_err(|e| e.to_string())?;
    }
    if prepare_request(&state, session, vision, &model_sha, context_window) {
        let _ = on_event.send(AgentUiEvent::ContextChanged {
            files: session.agent.context_files(),
        });
    }
    let compact_at = session
        .agent
        .context_window
        .map(|w| w.saturating_sub(session.agent.compaction.for_window(w).reserve_tokens));

    // The transcript is created with the first request.
    let mut name_it = false;
    if session.transcript.is_none() && !session.saving_failed {
        let folders: Vec<String> = session
            .folders
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        match with_conversations(&state, |s| s.create_transcript(&folders)) {
            Ok(t) => {
                if let Some(p) = &session.project {
                    let _ =
                        with_conversations(&state, |s| s.set_transcript_project(&t.id, Some(p)));
                }
                session.agent.workspaces.set_session(Some(t.id.clone()));
                live.set_card(Some(t.id.clone()), None);
                name_it = true;
                // A session started from a handoff links to what it
                // continues and keeps the title chosen for it.
                if let Some((origin, title)) = session.handoff_from.take() {
                    let _ = with_conversations(&state, |s| {
                        s.set_transcript_handoff(&t.id, origin.parent())
                    });
                    if with_conversations(&state, |s| s.rename_transcript(&t.id, &title)).is_ok() {
                        live.set_card(Some(t.id.clone()), Some(title_from_message(&title)));
                        name_it = false;
                    }
                }
                session.transcript = Some(t.id);
            }
            Err(e) => {
                session.saving_failed = true;
                let _ = on_event.send(AgentUiEvent::NotSaved { message: e });
            }
        }
    }
    let transcript = session.transcript.clone();
    let mut save_failed = false;
    let mut save = |seq: u64, kind: EntryKind, payload: String| {
        let Some(id) = transcript.as_deref() else {
            return;
        };
        if save_failed {
            return;
        }
        if let Err(e) = with_conversations(&state, |s| s.append_entry(id, seq, kind, &payload)) {
            save_failed = true;
            let _ = on_event.send(AgentUiEvent::NotSaved { message: e });
        }
    };

    let model = ClientModel(client);
    let approver = StoreApprover {
        state: &state,
        session: session.approval_session,
        stop: session.stop.clone(),
        interrupt: session.agent.interrupt_handle(),
        claimed: std::sync::Mutex::new(None),
    };
    let compacted = std::sync::atomic::AtomicUsize::new(0);
    let mut sink = |e: AgentEvent| {
        track(&live, &e, context_window);
        // A question card needs the user as an approval does.
        if matches!(&e, AgentEvent::ToolStart { name, .. } if name == "ask_user" || name == "ask_user_to_run")
        {
            crate::activity::notice(
                &state,
                Some(live.id.clone()),
                &live.title(),
                crate::activity::AttentionKind::Approval,
            );
        }
        let ui = match e {
            AgentEvent::Content(text) => AgentUiEvent::Content { text },
            AgentEvent::Reasoning(text) => AgentUiEvent::Reasoning { text },
            AgentEvent::ToolStart {
                id,
                name,
                arguments,
                parent,
            } => AgentUiEvent::ToolStart {
                id,
                name,
                arguments,
                parent,
            },
            AgentEvent::ToolEnd {
                id,
                output,
                is_error,
            } => AgentUiEvent::ToolEnd {
                id,
                output,
                is_error,
            },
            AgentEvent::ApprovalWaiting { tool } => AgentUiEvent::ApprovalWaiting { tool },
            AgentEvent::Message { seq, message } => {
                if name_it && message.role == Role::User {
                    name_it = false;
                    if let Some(id) = transcript.as_deref() {
                        let title = title_from_message(&message.content);
                        let _ = with_conversations(&state, |s| s.rename_transcript(id, &title));
                        live.set_card(Some(id.to_string()), Some(title));
                    }
                }
                // Images are saved apart from the entry (they can be much
                // larger than an entry may be).
                let mut entry = message;
                let has_images = !entry.images.is_empty();
                let images_of_entry = std::mem::take(&mut entry.images);
                if let Ok(json) = serde_json::to_string(&entry) {
                    save(seq, EntryKind::Message, json);
                }
                // The user's attachments, or images a tool returned.
                let bytes: Vec<Vec<u8>> = match unsaved_images.take() {
                    Some(b) if !b.is_empty() => b,
                    _ => image_urls_to_bytes(&images_of_entry),
                };
                if has_images
                    && entry.role == Role::User
                    && let Some(id) = transcript.as_deref()
                    && let Err(e) =
                        with_conversations(&state, |s| s.attach_agent_images(id, seq, &bytes))
                {
                    let _ = on_event.send(AgentUiEvent::NotSaved {
                        message: format!("the images weren't saved: {e}"),
                    });
                }
                return;
            }
            AgentEvent::StepDone(stats) => {
                // Speed varies with the text when speculating; then only
                // memory pressure is checked.
                if let Some(w) = state.check_gpu(
                    model_pid,
                    stats
                        .generation_per_second
                        .filter(|_| stats.draft_tokens.is_none()),
                    stats.generated_tokens,
                ) {
                    let _ = on_event.send(AgentUiEvent::GpuWarning(w));
                }
                AgentUiEvent::Usage(UsageView {
                    prompt_tokens: stats.prompt_tokens,
                    total_tokens: stats.total_tokens,
                    context_window: (context_window > 0).then_some(context_window),
                    compact_at,
                    breakdown: stats.breakdown.into(),
                    prompt_per_second: stats.prompt_per_second,
                    generation_per_second: stats.generation_per_second,
                    ttft_ms: stats.ttft_ms,
                    draft_tokens: stats.draft_tokens,
                    draft_accepted: stats.draft_accepted,
                })
            }
            AgentEvent::Compacting => AgentUiEvent::Compacting,
            AgentEvent::Compacted {
                tokens_before,
                first_kept_seq,
                summary,
            } => {
                compacted.fetch_add(1, Ordering::SeqCst);
                let text = summary.text.clone();
                let record = CompactionRecord {
                    first_kept_seq,
                    tokens_before,
                    summary,
                };
                if let Ok(json) = serde_json::to_string(&record) {
                    save(first_kept_seq, EntryKind::Compaction, json);
                }
                AgentUiEvent::Compacted {
                    tokens_before,
                    summary: text,
                }
            }
            AgentEvent::CompactionFailed(message) => AgentUiEvent::CompactionFailed { message },
            AgentEvent::Todos(todos) => AgentUiEvent::Todos { todos },
            AgentEvent::Retrying {
                attempt,
                max,
                delay_ms,
                error,
            } => AgentUiEvent::Retrying {
                attempt,
                max,
                delay_ms,
                error,
            },
            AgentEvent::Steered(text) => AgentUiEvent::Steered { text },
            AgentEvent::Interrupted => AgentUiEvent::Interrupted,
            AgentEvent::Security(flag) => {
                crate::activity::security_flag(&state, &live, &flag);
                return;
            }
            AgentEvent::Withdrawn { from_seq } => {
                if let Some(id) = transcript.as_deref() {
                    let _ = with_conversations(&state, |s| s.remove_entries_from(id, from_seq));
                }
                return;
            }
        };
        let _ = on_event.send(ui);
    };
    let shell_arc = session.shell.clone();
    let shell = shell_arc.as_deref().map(|s| s as &dyn Shell);
    let result = session
        .agent
        .run_turn_with_images(&text, image_urls, &model, &approver, shell, &mut sink)
        .await;
    let undelivered: Vec<String> = session
        .agent
        .steering_handle()
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    if save_failed {
        session.saving_failed = true;
    }
    // This turn's searches and pages, saved with the session (sealed).
    if let (Some(id), Some(visits)) = (session.transcript.clone(), session.visits.clone()) {
        let new = visits.take_new();
        if let Err(e) = crate::web::save_visits(
            &state,
            gardenwall_store::conversations::VisitOwner::Agent(&id),
            &new,
        ) {
            state.add_notice(format!("The visited-sites list couldn't be saved: {e}"));
        }
    }
    let end = match result {
        Ok(TurnEnd::Answered) => "answered",
        Ok(TurnEnd::Stopped) => "stopped",
        Ok(TurnEnd::StepLimit) => "step_limit",
        Ok(TurnEnd::Repeating) => "repeating",
        Err(e) => {
            turn_ended(
                &state,
                &live,
                crate::activity::TurnOutcome::Failed,
                Some(&e),
            );
            return Err(e);
        }
    };
    let (outcome, detail) = match end {
        "answered" => (crate::activity::TurnOutcome::Finished, None),
        "stopped" => (crate::activity::TurnOutcome::Stopped, None),
        "step_limit" => (
            crate::activity::TurnOutcome::Paused,
            Some("it paused at the step checkpoint"),
        ),
        _ => (
            crate::activity::TurnOutcome::Failed,
            Some("it was stopped for repeating itself"),
        ),
    };
    turn_ended(&state, &live, outcome, detail);
    let suggested =
        crate::memory::store_suggestions(&state, &session.memory_queue, &session.folders);
    session.compactions += compacted.load(Ordering::SeqCst);
    Ok(AgentTurnResult {
        end,
        suggested,
        undelivered,
        withdrawn: session.agent.take_withdrawn(),
        handoff: crate::handoff::agent_suggestion(session),
        compactions: session.compactions,
    })
}

/// The sandboxed runner for code-mode programs, or `None` where it can't
/// run (then `run_js` isn't offered: programs never run unsandboxed).
#[cfg(any(windows, target_os = "linux"))]
fn script_host(state: &AppState) -> Option<Arc<dyn gardenwall_agent::agent::ScriptHost>> {
    let helper = crate::state::stage_helper(&state.paths.js_dir, "gardenwall-js").ok()?;
    Some(Arc::new(
        gardenwall_agent::script_host::SandboxScriptHost::new(
            helper,
            Some(state.paths.grants.clone()),
        ),
    ))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn script_host(_state: &AppState) -> Option<Arc<dyn gardenwall_agent::agent::ScriptHost>> {
    None
}

/// Applies what every request uses from Settings and the Models page, and
/// reloads project instructions changed meanwhile (returns whether they
/// did). The warm-up uses the same, so its prompt matches the real one.
fn prepare_request(
    state: &AppState,
    session: &mut AgentSession,
    vision: bool,
    model_sha: &str,
    context_window: u64,
) -> bool {
    let settings = state.settings();
    let agent_settings = settings.agent;
    session.agent.vision = vision;
    session.agent.retries = agent_settings.retries;
    session.agent.max_steps = match agent_settings.step_checkpoint {
        0 => usize::MAX,
        n => n as usize,
    };
    session.agent.compaction = gardenwall_agent::compaction::CompactionSettings {
        enabled: agent_settings.compaction,
        reserve_tokens: agent_settings.reserve_tokens,
        keep_recent_tokens: agent_settings.keep_recent_tokens,
    };
    session
        .agent
        .set_append_prompt(Some(agent_settings.append_system_prompt));
    // Project instructions added or edited since the session started, and
    // a decompiler set up (or removed) in Settings meanwhile.
    let context = load_context(&session.agent.workspaces);
    let changed = session.agent.update_context(context);
    let analyzer = crate::binary::for_agent(state);
    if analyzer.can_decompile() != session.agent.can_decompile() {
        session.agent.set_binary(Some(analyzer));
    }
    // Code mode (experimental): `run_js` is offered only while the setting
    // is on and its sandboxed runner is available.
    if agent_settings.code_mode != session.agent.code_mode() {
        let host = if agent_settings.code_mode {
            script_host(state)
        } else {
            None
        };
        session.agent.set_script_host(host);
    }
    session.agent.sampling = settings.generation.sampling();
    // The model's thinking level, as set on the Models page.
    session.agent.template_kwargs = state
        .library
        .get(model_sha)
        .and_then(|r| r.thinking.template_kwargs());
    session.agent.context_window = (context_window > 0).then_some(context_window);
    changed
}

/// Prepares the model server's prompt cache for a session's next request
/// in the background (see `Agent::warm_up_request`): a new session in
/// another project otherwise reuses nothing on a hybrid model and its first
/// request takes about 0.9 s longer. Skipped when no model is loaded, when
/// the session is busy, and while another session is working, so it never
/// delays another session's work, takes the place of its cached prompt, or
/// gets around "one at a time".
pub(crate) fn spawn_warm_up(app: AppHandle, live: Arc<LiveSession>) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let state = app.state::<AppState>();
        if live_sessions(&state)
            .iter()
            .any(|s| s.busy.load(Ordering::SeqCst))
        {
            return;
        }
        let (client, context_window, model_sha, vision) = {
            let engine = state.engine.lock().await;
            let Some(loaded) = engine.as_ref() else {
                return;
            };
            (
                loaded.server.client().clone(),
                loaded.props.context_window,
                loaded.sha256.clone(),
                loaded.vision,
            )
        };
        let request = {
            let Ok(mut guard) = live.session.try_lock() else {
                return;
            };
            let session = &mut *guard;
            prepare_request(&state, session, vision, &model_sha, context_window);
            let shell = session.shell();
            session.agent.warm_up_request(shell)
        };
        let _ = gardenwall_agent::agent::warm_up(&ClientModel(client), &request).await;
    });
}

/// Follows a turn's events on the session's dashboard record. Only tool
/// names (reduced to a safe alphabet) and counts are kept.
fn track(live: &LiveSession, e: &AgentEvent, context_window: u64) {
    use crate::activity::{Step, tool_label};
    let Ok(mut a) = live.activity.lock() else {
        return;
    };
    match e {
        AgentEvent::Content(_) => a.set_step(Step::Writing),
        AgentEvent::Reasoning(_) => a.set_step(Step::Thinking),
        AgentEvent::ToolStart { name, .. } => a.set_step(Step::Tool(tool_label(name))),
        AgentEvent::ToolEnd { .. } => a.back_to_model(),
        AgentEvent::ApprovalWaiting { tool } => a.set_step(Step::Approval(tool_label(tool))),
        AgentEvent::StepDone(stats) => a.set_context(
            stats.total_tokens.or(stats.prompt_tokens),
            (context_window > 0).then_some(context_window),
        ),
        AgentEvent::Compacting => a.set_step(Step::Compacting),
        AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed(_) => a.back_to_model(),
        AgentEvent::Todos(todos) => a.set_todos(todos),
        AgentEvent::Retrying { .. } => a.set_step(Step::Retrying),
        _ => {}
    }
}

/// Records how a turn ended and tells the user when it finished or failed
/// (stopping is the user's own action, so it isn't announced).
fn turn_ended(
    state: &AppState,
    live: &LiveSession,
    outcome: crate::activity::TurnOutcome,
    detail: Option<&str>,
) {
    use crate::activity::{AttentionKind, TurnOutcome};
    if let Ok(mut a) = live.activity.lock() {
        a.end_turn(outcome, detail);
    }
    if outcome == crate::activity::TurnOutcome::Failed {
        // The reason only (redacted); never the session's title or messages.
        crate::diagnostics::record(
            state,
            "agent turn",
            detail.unwrap_or("stopped with a problem"),
        );
    }
    let kind = match outcome {
        TurnOutcome::Finished => AttentionKind::Finished,
        TurnOutcome::Failed => AttentionKind::Failed,
        TurnOutcome::Paused => AttentionKind::Paused,
        TurnOutcome::Stopped => return,
    };
    crate::activity::notice(state, Some(live.id.clone()), &live.title(), kind);
}

/// Bytes of `data:` image URLs (images the agent's `read` returned).
fn image_urls_to_bytes(urls: &[String]) -> Vec<Vec<u8>> {
    use base64::Engine;
    urls.iter()
        .filter_map(|u| u.split_once(";base64,").map(|(_, b)| b))
        .filter_map(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .collect()
}

/// Queue a message for the running agent (Pi's steering): it is delivered
/// after the current step's tool calls, or continues the turn if the model
/// had finished. Fails when no turn is running (send it normally then).
#[tauri::command]
pub fn agent_steer(
    state: State<'_, AppState>,
    sid: String,
    text: String,
    now: Option<bool>,
) -> CmdResult<()> {
    let text = text.trim().to_string();
    if text.is_empty() || text.len() > 64 * 1024 {
        return Err("the message must be non-empty and at most 64 KiB".into());
    }
    let live = live(&state, &sid)?;
    if !live.busy.load(Ordering::SeqCst) {
        return Err("the agent isn't working; send the message normally".into());
    }
    let mut q = live.steering.lock().map_err(|_| "state unavailable")?;
    if q.len() >= 16 {
        return Err("too many queued messages".into());
    }
    q.push_back(text);
    // Send now: cut the reply being written.
    if now == Some(true) {
        live.interrupt.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Undo the newest agent turn in the first granted folder.
#[tauri::command]
pub async fn agent_undo(state: State<'_, AppState>, sid: String) -> CmdResult<Vec<String>> {
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    let Some(first) = session.agent.workspaces.ids().first().copied() else {
        return Ok(vec![]);
    };
    Ok(session
        .agent
        .workspaces
        .undo_latest(first)?
        .unwrap_or_default())
}

pub(crate) fn with_conversations<T>(
    state: &AppState,
    f: impl FnOnce(
        &mut gardenwall_store::conversations::ConversationStore,
    ) -> gardenwall_store::conversations::Result<T>,
) -> CmdResult<T> {
    let mut store = state
        .conversations
        .lock()
        .map_err(|_| "conversation storage unavailable")?;
    f(&mut store).map_err(|e| e.to_string())
}

/// Saved agent transcripts, newest first.
#[tauri::command]
pub fn agent_transcripts(state: State<'_, AppState>) -> CmdResult<Vec<TranscriptSummary>> {
    with_conversations(&state, |s| s.list_transcripts())
}

/// One transcript entry for display.
#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EntryView {
    Message {
        seq: u64,
        message: ChatMessage,
        /// When the message was saved (Unix seconds), for the time shown
        /// on hover (release plan feature 5).
        created_at: i64,
    },
    Compaction {
        tokens_before: u64,
        summary: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptView {
    #[serde(flatten)]
    summary: TranscriptSummary,
    entries: Vec<EntryView>,
}

type Parsed = (u64, EntryView, Option<CompactionRecord>);

/// Parsed entries of a saved transcript. Payloads that do not parse are
/// skipped rather than failing the whole transcript.
fn parsed_entries(state: &AppState, id: &str) -> CmdResult<(TranscriptSummary, Vec<Parsed>)> {
    let t = with_conversations(state, |s| s.get_transcript(id))?;
    // Images are kept apart from their entries; only user messages get them.
    let mut images = with_conversations(state, |s| s.agent_images(id))?;
    let entries = t
        .entries
        .into_iter()
        .filter_map(|e| match e.kind {
            EntryKind::Message => {
                serde_json::from_str::<ChatMessage>(&e.payload)
                    .ok()
                    .map(|mut message| {
                        message.images = match message.role {
                            Role::User => images
                                .remove(&e.seq)
                                .unwrap_or_default()
                                .iter()
                                .map(|i| i.data_url())
                                .collect(),
                            _ => Vec::new(),
                        };
                        (
                            e.seq,
                            EntryView::Message {
                                seq: e.seq,
                                message,
                                created_at: e.created_at,
                            },
                            None,
                        )
                    })
            }
            EntryKind::Compaction => serde_json::from_str::<CompactionRecord>(&e.payload)
                .ok()
                .map(|r| {
                    (
                        e.seq,
                        EntryView::Compaction {
                            tokens_before: r.tokens_before,
                            summary: r.summary.text.clone(),
                        },
                        Some(r),
                    )
                }),
        })
        .collect();
    Ok((t.summary, entries))
}

/// A saved transcript, for reading.
#[tauri::command]
pub fn agent_transcript(state: State<'_, AppState>, id: String) -> CmdResult<TranscriptView> {
    let (summary, entries) = parsed_entries(&state, &id)?;
    Ok(TranscriptView {
        summary,
        entries: entries.into_iter().map(|(_, v, _)| v).collect(),
    })
}

/// Project instructions from each folder's AGENTS.md and CLAUDE.md, as Pi
/// discovers them.
fn load_context(workspaces: &gardenwall_agent::tools::Workspaces) -> Vec<ContextFile> {
    let mut context = Vec::new();
    for file in ["AGENTS.md", "CLAUDE.md"] {
        let Ok(name) = RelPath::parse(file) else {
            continue;
        };
        for id in workspaces.ids() {
            if let Ok(Some(bytes)) = workspaces.read_optional(id, &name, MAX_CONTEXT_FILE_BYTES)
                && let Ok(text) = String::from_utf8(bytes)
            {
                context.push(ContextFile {
                    path: workspaces.absolute(id, Some(&name)).display().to_string(),
                    content: text,
                });
            }
        }
    }
    context
}

/// Read the project instructions, skills and custom commands again (Pi's
/// `/reload`), and apply Settings → Agent.
#[tauri::command]
pub async fn agent_reload(state: State<'_, AppState>, sid: String) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    let context = load_context(&session.agent.workspaces);
    session.agent.set_context(context);
    let skills = crate::skills::session_skills(&state, &session.agent.workspaces);
    session.agent.set_skills(skills);
    session.commands = crate::skills::project_commands(&session.agent.workspaces);
    session
        .agent
        .set_append_prompt(Some(state.settings().agent.append_system_prompt));
    if session.memories_on {
        let block = crate::memory::agent_block(&state, &session.folders);
        session.agent.set_memories(block);
    }
    Ok(view(&live, session))
}

/// Run a command the user typed (`!cmd`, or `!!cmd` with `share: false`)
/// in the session's sandbox. With `share`, the output is added to the next
/// request, as Pi includes it in the conversation.
#[tauri::command]
pub async fn agent_user_shell(
    state: State<'_, AppState>,
    sid: String,
    command: String,
    share: bool,
) -> CmdResult<UserShellResult> {
    let command = command.trim().to_string();
    if command.is_empty() || command.len() > 16 * 1024 {
        return Err("the command must be non-empty and at most 16 KiB".into());
    }
    let live = live(&state, &sid)?;
    let mut guard = live
        .session
        .try_lock()
        .map_err(|_| "wait for the agent to finish, then run the command")?;
    let session = &mut *guard;
    let shell = session
        .shell()
        .ok_or("commands are not available in this session")?;
    let (output, failed) = session.agent.run_user_command(&command, shell).await;
    if share {
        if session.shared_output.len() >= 8 {
            session.shared_output.remove(0);
        }
        session.shared_output.push(format!("$ {command}\n{output}"));
    }
    Ok(UserShellResult { output, failed })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserShellResult {
    output: String,
    failed: bool,
}

/// Files in the granted folders matching `query`, for `@` completion.
#[tauri::command]
pub async fn agent_complete_path(
    state: State<'_, AppState>,
    sid: String,
    query: String,
) -> CmdResult<Vec<String>> {
    if query.len() > 200 {
        return Ok(vec![]);
    }
    // While a turn runs, completion simply offers nothing.
    let Ok(live) = live(&state, &sid) else {
        return Ok(vec![]);
    };
    let Ok(mut guard) = live.session.try_lock() else {
        return Ok(vec![]);
    };
    let session = &mut *guard;
    let query = query.replace('\\', "/");
    Ok(session.agent.complete_path(&query, 20))
}

/// What the agent's context holds when a transcript is continued: the
/// newest compaction's summary and the messages it kept, plus later ones.
fn restore_from(entries: Vec<Parsed>) -> Restore {
    let next_seq = entries.iter().map(|(s, _, _)| s + 1).max().unwrap_or(1);
    let mut summary = None;
    let mut first_kept = 0;
    let mut messages: Vec<(u64, ChatMessage)> = Vec::new();
    for (seq, view, record) in entries {
        if let Some(r) = record {
            first_kept = r.first_kept_seq;
            summary = Some(r.summary);
            messages.retain(|(s, _)| *s >= first_kept);
        } else if let EntryView::Message { message, .. } = view
            && seq >= first_kept
        {
            messages.push((seq, message));
        }
    }
    Restore {
        summary,
        messages,
        next_seq,
    }
}

/// Continue a saved transcript. Folders are chosen again in the native
/// picker; the transcript's own folder list is never used to grant access.
#[tauri::command]
pub async fn agent_resume(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    collection: Option<String>,
    memories: Option<bool>,
) -> CmdResult<Option<AgentView>> {
    if live_sessions(&state).iter().any(|s| {
        s.card
            .lock()
            .is_ok_and(|c| c.0.as_deref() == Some(id.as_str()))
    }) {
        return Err("this session is already open".into());
    }
    let docs = attach_docs(&app, &state, collection)?;
    let (summary, entries) = parsed_entries(&state, &id)?;
    let compactions = entries.iter().filter(|(_, _, r)| r.is_some()).count();
    let restore = restore_from(entries);
    room_for_session(&state)?;
    // A session in a project continues with the project's folders; others
    // ask for folders, which then become a project.
    let (folders, project) = match &summary.project_id {
        Some(p) => {
            let (p, folders) = crate::projects::project_folders(&state, p)?;
            (folders, p.id)
        }
        None => {
            let Some(picked) = pick_folders(app).await? else {
                return Ok(None);
            };
            let folders = picked
                .iter()
                .map(|f| crate::projects::checked(f))
                .collect::<CmdResult<Vec<_>>>()?;
            let p = crate::projects::create_for(&state, &folders)?;
            with_conversations(&state, |s| {
                s.set_transcript_project(&summary.id, Some(&p.id))
            })?;
            (folders, p.id)
        }
    };
    let view = begin(
        &state,
        folders,
        Some((summary.id, restore)),
        docs,
        memories.unwrap_or(true),
        Some(project),
    )
    .await?;
    live(&state, &view.id)?.session.lock().await.compactions = compactions;
    Ok(Some(view))
}

/// Delete a saved transcript (not the one in use).
#[tauri::command]
pub async fn agent_delete_transcript(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    if live_sessions(&state).iter().any(|s| {
        s.card
            .lock()
            .is_ok_and(|c| c.0.as_deref() == Some(id.as_str()))
    }) {
        return Err("end this agent session before deleting its transcript".into());
    }
    with_conversations(&state, |s| s.delete_transcript(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sandbox's refusal is explained with what to do about it.
    #[test]
    fn commands_off_reasons_say_how_to_fix_them() {
        let link = commands_off_reason(
            r"sandbox policy rejected: path contains a link or reparse point: C:\work\ci",
        );
        assert!(link.starts_with(
            r"sandbox policy rejected: path contains a link or reparse point: C:\work\ci."
        ));
        assert!(link.contains("Remove the link"));
        assert!(
            commands_off_reason("file has multiple hard links: C:\\w\\a")
                .contains("Remove the extra link")
        );
        assert!(commands_off_reason("too many entries under C:\\w").contains("smaller folder"));
        assert_eq!(commands_off_reason("something else"), "something else.");
    }

    /// A reopened session shows when each message was sent (release plan
    /// feature 5): the saved time reaches the view, in Unix seconds.
    #[test]
    fn a_saved_session_keeps_its_message_times() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let id = {
            let store = state.conversations.lock().unwrap();
            let t = store.create_transcript(&[]).unwrap();
            for (seq, role) in [(0, Role::User), (1, Role::Assistant)] {
                let payload = serde_json::to_string(&ChatMessage::new(role, "hi")).unwrap();
                store
                    .append_entry(&t.id, seq, EntryKind::Message, &payload)
                    .unwrap();
            }
            t.id
        };
        let (_, entries) = parsed_entries(&state, &id).unwrap();
        assert_eq!(entries.len(), 2);
        for (_, view, _) in &entries {
            let EntryView::Message { created_at, .. } = view else {
                panic!("expected a message");
            };
            assert!(*created_at >= before && *created_at <= before + 60);
            let json = serde_json::to_value(view).unwrap();
            assert_eq!(json["createdAt"], *created_at);
        }
    }

    /// A grant decides only requests of its own kind: a research grant can
    /// never approve a file change, a command or a secret read.
    #[test]
    fn grants_fit_only_their_own_kind_of_request() {
        assert!(grant_fits(ApprovalKind::Egress, "search"));
        assert!(grant_fits(
            ApprovalKind::Egress,
            "fetch:https://docs.rs:443"
        ));
        assert!(grant_fits(ApprovalKind::McpTool, "mcp:mcp__fs__read"));
        assert!(!grant_fits(ApprovalKind::Egress, "mcp:mcp__fs__read"));
        assert!(!grant_fits(ApprovalKind::McpTool, "search"));
        for kind in ApprovalKind::ALL {
            if matches!(kind, ApprovalKind::Egress | ApprovalKind::McpTool) {
                continue;
            }
            for scope in [
                "search",
                "fetch:https://docs.rs:443",
                "mcp:mcp__fs__read",
                "toolchain:rust",
            ] {
                assert!(!grant_fits(kind, scope), "{kind:?} {scope}");
            }
        }
    }

    fn msg(seq: u64, role: Role, text: &str) -> Parsed {
        (
            seq,
            EntryView::Message {
                seq,
                message: ChatMessage::new(role, text),
                created_at: 0,
            },
            None,
        )
    }

    fn compaction(first: u64, text: &str) -> Parsed {
        (
            first,
            EntryView::Compaction {
                tokens_before: 1,
                summary: text.into(),
            },
            Some(CompactionRecord {
                first_kept_seq: first,
                tokens_before: 1,
                summary: Summary {
                    text: text.into(),
                    files: Default::default(),
                },
            }),
        )
    }

    #[test]
    fn restoring_keeps_messages_from_the_newest_compaction_on() {
        let r = restore_from(vec![
            msg(1, Role::User, "a"),
            msg(2, Role::Assistant, "b"),
            msg(3, Role::User, "c"),
            msg(4, Role::Assistant, "d"),
            compaction(3, "first"),
            msg(5, Role::User, "e"),
            msg(6, Role::Assistant, "f"),
            compaction(5, "second"),
            msg(7, Role::User, "g"),
        ]);
        assert_eq!(r.summary.map(|s| s.text).as_deref(), Some("second"));
        let seqs: Vec<u64> = r.messages.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, vec![5, 6, 7]);
        assert_eq!(r.next_seq, 8);

        let plain = restore_from(vec![msg(1, Role::User, "a")]);
        assert!(plain.summary.is_none());
        assert_eq!(plain.messages.len(), 1);
        assert_eq!(plain.next_seq, 2);
    }
}

// ---------- agent commands (/plan, /compact, /clear, /context, /memory) ----------

/// Use memories in this session or not (both the approved ones in the
/// prompt and the `remember` tool).
#[tauri::command]
pub async fn agent_set_memories(
    state: State<'_, AppState>,
    sid: String,
    on: bool,
) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    session.memories_on = on;
    if on {
        let block = crate::memory::agent_block(&state, &session.folders);
        session.agent.set_memory_sink(Some(
            session.memory_queue.clone() as Arc<dyn gardenwall_agent::agent::MemorySink>
        ));
        session.agent.set_memories(block);
    } else {
        session.agent.set_memory_sink(None);
        session.agent.set_memories(None);
    }
    Ok(view(&live, session))
}

/// Turn planning mode on or off (`/plan`). Enforced by the policy engine.
#[tauri::command]
pub async fn agent_set_plan_mode(
    state: State<'_, AppState>,
    sid: String,
    on: bool,
) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    session.agent.set_plan_mode(on);
    Ok(view(&live, session))
}

/// Compact now (`/compact [focus]`), streaming the compaction events.
#[tauri::command]
pub async fn agent_compact(
    state: State<'_, AppState>,
    sid: String,
    focus: Option<String>,
    on_event: Channel<AgentUiEvent>,
) -> CmdResult<bool> {
    let (client, context_window) = {
        let engine = state.engine.lock().await;
        let loaded = engine.as_ref().ok_or("no model is loaded")?;
        (loaded.server.client().clone(), loaded.props.context_window)
    };
    let focus = focus
        .map(|f| {
            f.chars()
                .filter(|c| !c.is_control())
                .take(500)
                .collect::<String>()
        })
        .filter(|f| !f.trim().is_empty());
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    session.agent.context_window = (context_window > 0).then_some(context_window);
    live.stop.store(false, Ordering::SeqCst);
    let transcript = session.transcript.clone();
    let model = ClientModel(client);
    let mut sink = |e: AgentEvent| {
        let ui = match e {
            AgentEvent::Compacting => AgentUiEvent::Compacting,
            AgentEvent::Compacted {
                tokens_before,
                first_kept_seq,
                summary,
            } => {
                if let Some(id) = transcript.as_deref() {
                    let text = summary.text.clone();
                    let record = CompactionRecord {
                        first_kept_seq,
                        tokens_before,
                        summary,
                    };
                    if let Ok(json) = serde_json::to_string(&record) {
                        let _ = with_conversations(&state, |s| {
                            s.append_entry(id, first_kept_seq, EntryKind::Compaction, &json)
                        });
                    }
                    AgentUiEvent::Compacted {
                        tokens_before,
                        summary: text,
                    }
                } else {
                    AgentUiEvent::Compacted {
                        tokens_before,
                        summary: summary.text,
                    }
                }
            }
            AgentEvent::CompactionFailed(message) => AgentUiEvent::CompactionFailed { message },
            _ => return,
        };
        let _ = on_event.send(ui);
    };
    let done = session
        .agent
        .compact_now(&model, focus.as_deref(), &mut sink)
        .await?;
    if done {
        session.compactions += 1;
    }
    Ok(done)
}

/// Start a fresh session on the same folders and documents (`/clear`).
#[tauri::command]
pub async fn agent_clear(state: State<'_, AppState>, sid: String) -> CmdResult<AgentView> {
    let (folders, docs, memories_on, project) = {
        let live = live(&state, &sid)?;
        let s = live.session.lock().await;
        (
            s.folders.clone(),
            s.docs.clone(),
            s.memories_on,
            s.project.clone(),
        )
    };
    end_session(&state, &sid);
    begin(&state, folders, None, docs, memories_on, project).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextView {
    /// Estimated tokens in the context now.
    tokens: u64,
    context_window: Option<u64>,
    breakdown: BreakdownView,
    compacted: bool,
}

/// The context breakdown (`/context`), estimated from text length.
#[tauri::command]
pub async fn agent_context(state: State<'_, AppState>, sid: String) -> CmdResult<ContextView> {
    let window = state
        .engine
        .lock()
        .await
        .as_ref()
        .map(|l| l.props.context_window);
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    let s = &*guard;
    Ok(ContextView {
        tokens: s.agent.context_tokens(),
        context_window: window,
        breakdown: s.agent.breakdown().into(),
        compacted: s.agent.summary().is_some(),
    })
}

/// Largest AGENTS.md edited with `/memory`.
const MAX_INSTRUCTIONS_BYTES: usize = 64 * 1024;

/// The first granted folder's AGENTS.md (`/memory`), or empty.
#[tauri::command]
pub async fn agent_instructions(state: State<'_, AppState>, sid: String) -> CmdResult<String> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    let s = &*guard;
    let id = *s
        .agent
        .workspaces
        .ids()
        .first()
        .ok_or("no folder is granted")?;
    let name = RelPath::parse("AGENTS.md").map_err(|e| e.to_string())?;
    Ok(s.agent
        .workspaces
        .read_optional(id, &name, MAX_CONTEXT_FILE_BYTES)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

/// Save the AGENTS.md the user edited in `/memory` (a trusted UI action,
/// checkpointed like an agent edit). It applies to sessions started next.
#[tauri::command]
pub async fn agent_save_instructions(
    state: State<'_, AppState>,
    sid: String,
    text: String,
) -> CmdResult<()> {
    if text.len() > MAX_INSTRUCTIONS_BYTES {
        return Err("AGENTS.md is larger than 64 KB".into());
    }
    let live = live(&state, &sid)?;
    let mut guard = live.session.lock().await;
    let s = &mut *guard;
    let id = *s
        .agent
        .workspaces
        .ids()
        .first()
        .ok_or("no folder is granted")?;
    let name = RelPath::parse("AGENTS.md").map_err(|e| e.to_string())?;
    s.agent.workspaces.begin_turn("Edit AGENTS.md");
    let result = s
        .agent
        .workspaces
        .write_checkpointed(id, &name, text.as_bytes());
    s.agent.workspaces.end_turn();
    result
}

// ---------- sessions: name, info, fork, clone, export, import (Pi parity) ----------

/// Rename the open session (`/name`).
#[tauri::command]
pub async fn agent_rename(
    state: State<'_, AppState>,
    sid: String,
    title: String,
) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    let session = &*guard;
    let id = session
        .transcript
        .as_deref()
        .ok_or("the session is saved with its first request; send one first")?;
    if title.trim().is_empty() {
        return Err("give the session a name".into());
    }
    with_conversations(&state, |s| s.rename_transcript(id, &title))?;
    live.set_card(Some(id.to_string()), Some(title_from_message(&title)));
    Ok(view(&live, session))
}

/// Rename a saved session from the session list (right-click → Rename),
/// whether or not it is open; an open one's card follows.
#[tauri::command]
pub fn rename_transcript(state: State<'_, AppState>, id: String, title: String) -> CmdResult<()> {
    if title.trim().is_empty() {
        return Err("give the session a name".into());
    }
    with_conversations(&state, |s| s.rename_transcript(&id, &title))?;
    for live in live_sessions(&state) {
        let held = live
            .card
            .lock()
            .is_ok_and(|c| c.0.as_deref() == Some(id.as_str()));
        if held {
            live.set_card(Some(id.clone()), Some(title_from_message(&title)));
        }
    }
    Ok(())
}

/// Facts about the open session (`/session`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    id: Option<String>,
    title: Option<String>,
    folders: Vec<String>,
    created_at: Option<i64>,
    user_messages: usize,
    assistant_messages: usize,
    tool_results: usize,
    compactions: usize,
    context_tokens: u64,
    context_window: Option<u64>,
    /// The session this one was forked or cloned from.
    parent_title: Option<String>,
}

#[tauri::command]
pub async fn agent_session_info(state: State<'_, AppState>, sid: String) -> CmdResult<SessionInfo> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    let session = &*guard;
    let mut info = SessionInfo {
        id: session.transcript.clone(),
        title: None,
        folders: session
            .folders
            .iter()
            .map(|f| f.display().to_string())
            .collect(),
        created_at: None,
        user_messages: 0,
        assistant_messages: 0,
        tool_results: 0,
        compactions: 0,
        context_tokens: session.agent.context_tokens(),
        context_window: session.agent.context_window,
        parent_title: None,
    };
    if let Some(id) = &session.transcript {
        let (summary, entries) = parsed_entries(&state, id)?;
        for (_, entry, record) in &entries {
            if record.is_some() {
                info.compactions += 1;
            } else if let EntryView::Message { message, .. } = entry {
                match message.role {
                    Role::User => info.user_messages += 1,
                    Role::Assistant => info.assistant_messages += 1,
                    Role::Tool => info.tool_results += 1,
                    Role::System => {}
                }
            }
        }
        info.parent_title = summary.parent_id.as_deref().and_then(|p| {
            with_conversations(&state, |s| s.get_transcript(p))
                .ok()
                .map(|t| t.summary.title)
        });
        info.title = Some(summary.title);
        info.created_at = Some(summary.created_at);
    }
    Ok(info)
}

/// A user message the session can be forked from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkPoint {
    seq: u64,
    text: String,
}

/// The user messages of the open session, newest last (`/fork` picker).
#[tauri::command]
pub async fn agent_fork_points(
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<Vec<ForkPoint>> {
    let live = live(&state, &sid)?;
    let guard = live.session.lock().await;
    let session = &*guard;
    let Some(id) = &session.transcript else {
        return Ok(vec![]);
    };
    let (_, entries) = parsed_entries(&state, id)?;
    Ok(entries
        .into_iter()
        .filter_map(|(seq, entry, _)| match entry {
            EntryView::Message { message, .. } if message.role == Role::User => Some(ForkPoint {
                seq,
                text: message.content.chars().take(2000).collect(),
            }),
            _ => None,
        })
        .collect())
}

/// The session after a fork or clone.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkResult {
    view: AgentView,
    transcript: TranscriptView,
    /// The message the fork was made before, for the composer.
    text: String,
}

/// Continue from before an earlier user message in a new saved session
/// (Pi's `/fork`), or from the end (`/clone`, `seq: None`). The original
/// stays as it was. Files the agent changed are not reverted.
#[tauri::command]
pub async fn agent_fork(
    state: State<'_, AppState>,
    sid: String,
    seq: Option<u64>,
) -> CmdResult<ForkResult> {
    let live = live(&state, &sid)?;
    let mut guard = live.session.try_lock().map_err(
        |_| "The agent is still working in this session. Fork it when it stops, or stop it first.",
    )?;
    let session = &mut *guard;
    let source = session
        .transcript
        .clone()
        .ok_or("nothing to fork yet: send a request first")?;
    let (summary, entries) = parsed_entries(&state, &source)?;
    let text = match seq {
        Some(at) => entries
            .iter()
            .find_map(|(s, e, _)| match e {
                EntryView::Message { message, .. } if *s == at && message.role == Role::User => {
                    Some(message.content.clone())
                }
                _ => None,
            })
            .ok_or("that message isn't in this session")?,
        None => String::new(),
    };
    let title = format!(
        "{} ({})",
        summary.title,
        if seq.is_some() { "fork" } else { "copy" }
    );
    let copy = with_conversations(&state, |s| s.copy_transcript(&source, seq, &title))?;
    let (copy_summary, copy_entries) = parsed_entries(&state, &copy.id)?;
    let shown: Vec<EntryView> = parsed_entries(&state, &copy.id)?
        .1
        .into_iter()
        .map(|(_, v, _)| v)
        .collect();
    session.agent.restore(restore_from(copy_entries));
    session.agent.workspaces.set_session(Some(copy.id.clone()));
    live.set_card(Some(copy.id.clone()), Some(title.clone()));
    session.transcript = Some(copy.id);
    session.saving_failed = false;
    Ok(ForkResult {
        view: view(&live, session),
        transcript: TranscriptView {
            summary: copy_summary,
            entries: shown,
        },
        text,
    })
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Only image data URLs of the accepted types, with matching bytes.
fn safe_image_url(url: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine;
    let rest = url.strip_prefix("data:")?;
    let (mime, b64) = rest.split_once(";base64,")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.len() > gardenwall_store::conversations::MAX_IMAGE_BYTES {
        return None;
    }
    let sniffed = gardenwall_store::conversations::sniff_image(&bytes)?;
    (sniffed == mime).then(|| (sniffed.to_string(), bytes))
}

/// A self-contained page: no scripts, nothing loaded from elsewhere (the
/// policy forbids it even if text were mis-escaped).
fn transcript_html(summary: &TranscriptSummary, entries: &[Parsed]) -> String {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
    );
    out.push_str(&format!(
        "<title>{}</title>\n<style>body{{font:15px/1.5 system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;color:#1d2520;background:#fbfcfa}}pre{{white-space:pre-wrap;word-break:break-word;margin:.3rem 0}}.user{{background:#e6efe8;border-radius:8px;padding:.5rem .8rem;margin:1rem 0}}.assistant{{margin:1rem 0}}details{{border:1px solid #d5ddd7;border-radius:6px;padding:.3rem .6rem;margin:.4rem 0}}summary{{cursor:pointer;color:#45594c}}.meta{{color:#61746a;font-size:.85rem}}img{{max-width:20rem;max-height:20rem;margin:.3rem;border-radius:4px}}code{{font-family:ui-monospace,monospace}}@media (prefers-color-scheme:dark){{body{{background:#121815;color:#dbe5de}}.user{{background:#1f2c24}}details{{border-color:#2f3d34}}summary,.meta{{color:#97ab9e}}}}</style></head><body>\n",
        html_escape(&summary.title)
    ));
    out.push_str(&format!(
        "<h1>{}</h1>\n<p class=\"meta\">Gardenwall agent session · folders: {} · exported from the encrypted store; this file is not encrypted.</p>\n",
        html_escape(&summary.title),
        html_escape(&summary.folders.join(", "))
    ));
    for (_, entry, _) in entries {
        match entry {
            EntryView::Compaction {
                tokens_before,
                summary,
            } => out.push_str(&format!(
                "<details><summary>Earlier messages were summarized (about {tokens_before} tokens before)</summary><pre>{}</pre></details>\n",
                html_escape(summary)
            )),
            EntryView::Message { message, .. } => match message.role {
                Role::System => {}
                Role::User => {
                    out.push_str("<div class=\"user\">");
                    for url in &message.images {
                        if safe_image_url(url).is_some() {
                            out.push_str(&format!("<img alt=\"attached image\" src=\"{}\">", html_escape(url)));
                        }
                    }
                    out.push_str(&format!("<pre>{}</pre></div>\n", html_escape(&message.content)));
                }
                Role::Assistant => {
                    out.push_str("<div class=\"assistant\">");
                    if let Some(r) = message.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
                        out.push_str(&format!(
                            "<details><summary>Reasoning</summary><pre>{}</pre></details>",
                            html_escape(r)
                        ));
                    }
                    if !message.content.is_empty() {
                        out.push_str(&format!("<pre>{}</pre>", html_escape(&message.content)));
                    }
                    for call in &message.tool_calls {
                        let args = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                            .ok()
                            .and_then(|v| serde_json::to_string_pretty(&v).ok())
                            .unwrap_or_else(|| call.function.arguments.clone());
                        out.push_str(&format!(
                            "<details><summary>Tool call: <code>{}</code></summary><pre>{}</pre></details>",
                            html_escape(&call.function.name),
                            html_escape(&args)
                        ));
                    }
                    out.push_str("</div>\n");
                }
                Role::Tool => out.push_str(&format!(
                    "<details><summary>Tool result</summary><pre>{}</pre></details>\n",
                    html_escape(&message.content)
                )),
            },
        }
    }
    out.push_str("</body></html>\n");
    out
}

/// A fenced code block that the text can't close early: the fence is one
/// backtick longer than the longest run of backticks inside.
fn md_fence(info: &str, text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{info}\n{}\n{fence}\n\n", text.trim_end())
}

/// The Markdown export: the same
/// content as the HTML export. The model's text is copied as written;
/// reasoning is quoted, tool calls and results are code blocks. Images are
/// noted, not embedded.
fn transcript_markdown(summary: &TranscriptSummary, entries: &[Parsed]) -> String {
    let mut out = format!(
        "# {}\n\nGardenwall agent session · folders: {} · exported from the encrypted store; this file is not encrypted.\n\n",
        crate::export::defang(
            &summary
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        ),
        summary.folders.join(", ")
    );
    for (_, entry, _) in entries {
        match entry {
            EntryView::Compaction {
                tokens_before,
                summary,
            } => {
                out.push_str(&format!(
                    "*Earlier messages were summarized (about {tokens_before} tokens before):*\n\n"
                ));
                out.push_str(&md_fence("text", summary));
            }
            EntryView::Message { message, .. } => match message.role {
                Role::System => {}
                Role::User => {
                    out.push_str("## You\n\n");
                    if !message.images.is_empty() {
                        let n = message.images.len();
                        out.push_str(&format!(
                            "*[{n} image{} attached]*\n\n",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    out.push_str(&crate::export::defang(message.content.trim_end()));
                    out.push_str("\n\n");
                }
                Role::Assistant => {
                    out.push_str("## Agent\n\n");
                    if let Some(r) = message
                        .reasoning_content
                        .as_deref()
                        .filter(|r| !r.trim().is_empty())
                    {
                        out.push_str("*Reasoning:*\n\n");
                        for line in crate::export::defang(r.trim_end()).lines() {
                            out.push_str(&format!("> {line}\n"));
                        }
                        out.push('\n');
                    }
                    if !message.content.trim().is_empty() {
                        out.push_str(&crate::export::defang(message.content.trim_end()));
                        out.push_str("\n\n");
                    }
                    for call in &message.tool_calls {
                        let args =
                            serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                                .ok()
                                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                                .unwrap_or_else(|| call.function.arguments.clone());
                        out.push_str(&format!(
                            "**Tool call:** `{}`\n\n",
                            call.function.name.replace('`', "'")
                        ));
                        out.push_str(&md_fence("json", &args));
                    }
                }
                Role::Tool => {
                    out.push_str("**Tool result:**\n\n");
                    out.push_str(&md_fence("text", &message.content));
                }
            },
        }
    }
    out
}

/// The JSONL export: a header line, then one line per entry (Pi exports
/// sessions as JSONL too; the format is Gardenwall's own).
fn transcript_jsonl(summary: &TranscriptSummary, entries: &[Parsed]) -> String {
    let mut lines = vec![
        serde_json::json!({
            "type": "gardenwall-session",
            "version": 1,
            "title": summary.title,
            "folders": summary.folders,
            "createdAt": summary.created_at,
        })
        .to_string(),
    ];
    for (seq, entry, record) in entries {
        let line = match (entry, record) {
            (_, Some(r)) => serde_json::json!({"type": "compaction", "seq": seq, "record": r}),
            (EntryView::Message { message, .. }, None) => {
                serde_json::json!({"type": "message", "seq": seq, "message": message})
            }
            _ => continue,
        };
        lines.push(line.to_string());
    }
    lines.join("\n") + "\n"
}

/// Save a session as HTML, Markdown or JSONL where the user chooses.
#[tauri::command]
pub async fn export_transcript(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    format: String,
) -> CmdResult<Option<String>> {
    let (ext, filter) = match format.as_str() {
        "html" => ("html", "Web page"),
        "md" => ("md", "Markdown"),
        "jsonl" => ("jsonl", "JSON Lines"),
        _ => return Err("export as html, md or jsonl".into()),
    };
    let (summary, entries) = parsed_entries(&state, &id)?;
    let body = match ext {
        "html" => transcript_html(&summary, &entries),
        "md" => transcript_markdown(&summary, &entries),
        _ => transcript_jsonl(&summary, &entries),
    };
    let name: String = summary
        .title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect::<String>()
        .trim()
        .to_string();
    let file_name = format!(
        "{}.{}",
        if name.is_empty() {
            "agent-session".into()
        } else {
            name
        },
        ext
    );
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter(filter, &[ext])
            .set_file_name(&file_name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// Largest session file imported.
const MAX_IMPORT_BYTES: u64 = 256 << 20;

/// Parse a JSONL export into entries and images, strictly. Everything in it
/// is untrusted: messages become context like any saved session, and the
/// folder list is shown only (continuing asks for folders again).
type Imported = (
    String,
    Vec<String>,
    Vec<(u64, EntryKind, String)>,
    Vec<(u64, Vec<gardenwall_store::conversations::ImageData>)>,
);

fn parse_import(text: &str) -> Result<Imported, String> {
    use base64::Engine;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: serde_json::Value = lines
        .next()
        .and_then(|l| serde_json::from_str(l).ok())
        .ok_or("this isn't a Gardenwall session file (no header)")?;
    if header["type"] != "gardenwall-session" || header["version"] != 1 {
        return Err("this isn't a Gardenwall session file (version 1)".into());
    }
    let title = header["title"]
        .as_str()
        .unwrap_or("Imported session")
        .chars()
        .take(200)
        .collect::<String>();
    let folders: Vec<String> = header["folders"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str())
                .take(64)
                .map(|f| f.chars().take(1000).collect())
                .collect()
        })
        .unwrap_or_default();
    let mut entries = Vec::new();
    let mut images = Vec::new();
    let mut last_seq = 0u64;
    for (n, line) in lines.enumerate() {
        if entries.len() >= gardenwall_store::conversations::MAX_ENTRIES {
            return Err("the session has too many entries".into());
        }
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 2))?;
        let seq = v["seq"]
            .as_u64()
            .filter(|s| *s >= 1 && *s < 1 << 53)
            .ok_or_else(|| format!("line {}: a sequence number is missing", n + 2))?;
        if seq < last_seq {
            return Err(format!("line {}: entries are out of order", n + 2));
        }
        last_seq = seq;
        match v["type"].as_str() {
            Some("message") => {
                let mut message: ChatMessage = serde_json::from_value(v["message"].clone())
                    .map_err(|e| format!("line {}: {e}", n + 2))?;
                if message.role == Role::System {
                    return Err(format!("line {}: system messages can't be imported", n + 2));
                }
                let urls = std::mem::take(&mut message.images);
                if !urls.is_empty() {
                    if message.role != Role::User || urls.len() > 4 {
                        return Err(format!("line {}: unexpected images", n + 2));
                    }
                    let mut list = Vec::new();
                    for u in &urls {
                        let (mime, bytes) = safe_image_url(u).ok_or_else(|| {
                            format!("line {}: an image isn't a PNG, JPEG, WebP or GIF", n + 2)
                        })?;
                        list.push(gardenwall_store::conversations::ImageData {
                            mime,
                            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                        });
                    }
                    images.push((seq, list));
                }
                let payload = serde_json::to_string(&message).map_err(|e| e.to_string())?;
                entries.push((seq, EntryKind::Message, payload));
            }
            Some("compaction") => {
                let record: CompactionRecord = serde_json::from_value(v["record"].clone())
                    .map_err(|e| format!("line {}: {e}", n + 2))?;
                let payload = serde_json::to_string(&record).map_err(|e| e.to_string())?;
                entries.push((seq, EntryKind::Compaction, payload));
            }
            _ => return Err(format!("line {}: unknown entry type", n + 2)),
        }
    }
    Ok((title, folders, entries, images))
}

/// Import a JSONL session file chosen in the native picker.
#[tauri::command]
pub async fn import_transcript(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<TranscriptSummary>> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Gardenwall session", &["jsonl"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMPORT_BYTES {
        return Err("the file is larger than 256 MB".into());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("the file isn't readable text: {e}"))?;
    let (title, folders, entries, images) = parse_import(&text)?;
    let title = format!("{title} (imported)");
    with_conversations(&state, |s| {
        s.import_transcript(&title, &folders, &entries, &images)
    })
    .map(Some)
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// Markdown export (ADR-022 §2D): messages, reasoning, tool calls and
    /// results; a result with backticks can't close its code block early.
    #[test]
    fn a_session_exports_as_markdown() {
        let summary = TranscriptSummary {
            id: "00000000-0000-4000-8000-000000000000".into(),
            title: "Rust to-do".into(),
            folders: vec!["C:/p".into()],
            created_at: 1,
            updated_at: 1,
            parent_id: None,
            fork_seq: None,
            project_id: None,
            handoff: false,
            parent_chat_id: None,
        };
        let msg = |seq: u64, m: ChatMessage| {
            (
                seq,
                EntryView::Message {
                    seq,
                    message: m,
                    created_at: 0,
                },
                None,
            )
        };
        let mut reply = ChatMessage::new(Role::Assistant, "Checking.");
        reply.reasoning_content = Some("Look first.\nThen act.".into());
        reply.tool_calls = vec![gardenwall_inference::client::ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: gardenwall_inference::client::FunctionCall {
                name: "powershell".into(),
                arguments: r#"{"command":"cargo --version"}"#.into(),
            },
        }];
        let mut result = ChatMessage::new(Role::Tool, "```\nnot a fence end\n```");
        result.tool_call_id = Some("c1".into());
        let entries: Vec<Parsed> = vec![
            msg(1, ChatMessage::new(Role::User, "Make a to-do app.")),
            msg(2, reply),
            msg(3, result),
        ];
        let md = transcript_markdown(&summary, &entries);
        assert!(md.starts_with("# Rust to-do\n"), "{md}");
        assert!(md.contains("## You\n\nMake a to-do app."));
        assert!(md.contains("> Look first.\n> Then act."));
        assert!(md.contains("**Tool call:** `powershell`"));
        assert!(md.contains("\"command\": \"cargo --version\""));
        assert!(
            md.contains("````text\n```\nnot a fence end\n```\n````"),
            "{md}"
        );
    }

    #[test]
    fn exports_are_escaped_and_imports_are_strict() {
        let summary = TranscriptSummary {
            id: "00000000-0000-4000-8000-000000000000".into(),
            title: "Fix <script>".into(),
            folders: vec!["C:/p".into()],
            created_at: 1,
            updated_at: 1,
            parent_id: None,
            fork_seq: None,
            project_id: None,
            handoff: false,
            parent_chat_id: None,
        };
        let mut user = ChatMessage::new(Role::User, "<img src=x onerror=alert(1)>");
        user.images = vec![
            "data:image/png;base64,iVBORw0KGgp4".into(),
            "data:text/html;base64,PGI+".into(),
        ];
        let entries: Vec<Parsed> = vec![(
            1,
            EntryView::Message {
                seq: 1,
                message: user,
                created_at: 0,
            },
            None,
        )];
        let html = transcript_html(&summary, &entries);
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("Fix &lt;script&gt;"));
        assert!(!html.contains("<script"));
        assert_eq!(
            html.matches("<img ").count(),
            1,
            "only the real PNG is shown"
        );

        // A round trip keeps the message and its image.
        let jsonl = transcript_jsonl(&summary, &entries);
        let (title, folders, got, images) =
            parse_import(&jsonl.replace(",\"data:text/html;base64,PGI+\"", "")).unwrap();
        assert_eq!(
            (title.as_str(), folders.len(), got.len()),
            ("Fix <script>", 1, 1)
        );
        assert_eq!(images[0].1[0].mime, "image/png");
        assert!(!got[0].2.contains("data:image"), "images are stored apart");

        // A disguised image, a system message, bad order: refused.
        assert!(parse_import(&jsonl).unwrap_err().contains("isn't a PNG"));
        let head = "{\"type\":\"gardenwall-session\",\"version\":1,\"title\":\"t\",\"folders\":[]}";
        assert!(parse_import(&format!("{head}\n{{\"type\":\"message\",\"seq\":1,\"message\":{{\"role\":\"system\",\"content\":\"x\"}}}}")).is_err());
        assert!(parse_import(&format!("{head}\n{{\"type\":\"message\",\"seq\":2,\"message\":{{\"role\":\"user\",\"content\":\"a\"}}}}\n{{\"type\":\"message\",\"seq\":1,\"message\":{{\"role\":\"user\",\"content\":\"b\"}}}}")).unwrap_err().contains("out of order"));
        assert!(parse_import("{\"type\":\"other\"}").is_err());

        // Mutated session files are refused or imported, never a crash.
        let seed = jsonl.replace(",\"data:text/html;base64,PGI+\"", "");
        gardenwall_fuzzkit::run_text("import", &[seed.as_str()], 3000, |t| {
            let _ = parse_import(t);
        });
        assert!(parse_import(&format!("{head}\n{{\"type\":\"run\",\"seq\":1}}")).is_err());
    }
}
