//! Application state, data layout, and persisted settings.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gardenwall_inference::client::{RuntimeProps, Sampling};
use gardenwall_inference::supervisor::RunningServer;
use gardenwall_store::approvals::{ApprovalStore, SessionId};
use gardenwall_store::conversations::{self, ConversationStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library::Library;
use crate::speed::{GpuWarning, WarningKind, gpu_view};

/// User settings (non-secret).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "Offline / no hub": no model-hub requests of any kind (ADR-013 §2.7).
    pub offline: bool,
    /// Lockdown mode, for a model the user
    /// doesn't trust at all. Everything optional is switched off while it
    /// is on: see [`Settings::locked_down`] and
    /// `gardenwall_policy::tools::SessionPolicy::lockdown`.
    pub lockdown: bool,
    pub generation: Generation,
    /// Folders with programs the agent may run, granted read-only to its
    /// sandbox (e.g. a Python or Rust install).
    pub agent_tool_folders: Vec<PathBuf>,
    /// When to look for updates (ADR-017 §3.4).
    pub update_schedule: crate::updates::UpdateSchedule,
    /// Time of the last completed update check (ms since the epoch).
    pub last_update_check_ms: Option<i64>,
    /// Runtime build chosen in the update center; `None` picks the first
    /// installed one.
    pub active_runtime: Option<String>,
    /// SearXNG base URL for the agent's web search (Phase 4).
    pub searxng_url: Option<String>,
    /// Web research (ADR-018): which search provider, the approval mode new
    /// research sessions and Web-switch chats start in, and their limits.
    pub web: WebSettings,
    /// Embedding model for document search (a library model's SHA-256);
    /// `None` uses keyword search only.
    pub embedding_model: Option<String>,
    /// MCP servers the agent can use (Phase 5).
    pub mcp_servers: Vec<crate::mcp::McpServerConfig>,
    /// The optional Ghidra decompiler.
    pub binary: crate::binary::BinarySettings,
    /// Agent options from Pi's settings (compaction, retries, prompt).
    pub agent: AgentSettings,
    /// The user's favourite model (a library model's SHA-256), offered first
    /// on Home (UI redesign).
    pub favourite_model: Option<String>,
    /// Load the favourite model when Gardenwall starts and open the Agent
    /// workspace instead of Home.
    pub load_on_launch: bool,
    /// Settings → Voice: read aloud and voice input (release plan
    /// features 7 and 8).
    pub voice: crate::speech::VoiceSettings,
}

impl Settings {
    /// These settings as they apply in lockdown mode: offline (no model
    /// hub, update checks, web search or pages), no MCP servers, no extra
    /// programs for commands (tool folders, Python), no code mode and no
    /// decompiler. Sessions also get no shell and no memories, and the
    /// policy refuses everything that would ask to widen access.
    pub fn locked_down(mut self) -> Self {
        self.offline = true;
        self.searxng_url = None;
        for server in &mut self.mcp_servers {
            server.enabled = false;
        }
        self.agent_tool_folders.clear();
        self.agent.python = false;
        self.agent.code_mode = false;
        self.agent.parent_folders = false;
        self.agent.project_parent_folders.clear();
        self.binary = Default::default();
        self
    }
}

/// Settings → Agent (Pi parity).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentSettings {
    /// Summarize older messages when the context fills (Pi: on).
    pub compaction: bool,
    /// Free tokens kept for the reply; compaction starts below this (Pi:
    /// 16,384; capped to a quarter of small windows).
    pub reserve_tokens: u64,
    /// Recent tokens kept word for word (Pi: 20,000).
    pub keep_recent_tokens: u64,
    /// Retries of a request that fails before any output (Pi: 3).
    pub retries: u32,
    /// Added to the end of the agent's system prompt.
    pub append_system_prompt: String,
    /// How open agent sessions share the loaded model
    /// (all three, chosen in Settings).
    pub sessions: SessionMode,
    /// Model server slots in `Parallel` mode (2 to 4).
    pub parallel_slots: u32,
    /// The shell for agent commands on Windows
    /// (PowerShell by default). Applies to sessions started afterwards.
    pub shell: AgentShell,
    /// Offer an all-users Python in Program Files to the agent's commands,
    /// read-only inside the session's sandbox
    /// (on by default). Applies to sessions started afterwards.
    pub python: bool,
    /// System notifications while Gardenwall is in the background (owner
    /// decision 2026-11-19: all on, silent). Only the session's name and
    /// the event are shown, never model text.
    pub notify_finished: bool,
    pub notify_approval: bool,
    pub notify_failed: bool,
    /// Code mode (experimental):
    /// offer the agent `run_js`, one sandboxed JavaScript program that
    /// calls its tools, instead of many separate tool calls. Off by default.
    pub code_mode: bool,
    /// Question cards: offer the agent
    /// `ask_user`, which shows up to four multiple-choice questions and
    /// waits for the answers. Off by default until it has been measured
    /// with the benchmark (it adds a tool to every request). Applies to
    /// sessions started afterwards.
    pub ask_user: bool,
    /// "Run this yourself" cards: offer the
    /// agent `ask_user_to_run`, for a step that needs the network, which its
    /// own commands don't have. The core never runs the command. On by
    /// default; off in research sessions and lockdown mode.
    pub run_requests: bool,
    /// The name and email on the agent's git commits
    /// : the user's own by default, "Gardenwall Agent", or a
    /// custom one (`git_name`, `git_email`). Applies to sessions started
    /// afterwards.
    pub git_identity: GitIdentity,
    pub git_name: String,
    pub git_email: String,
    /// Steps (model round trips) before a long task pauses and asks
    /// whether to continue; 0 never asks.
    pub step_checkpoint: u32,
    /// Toolchains the user added to every project's agent sessions, by id
    /// (`rust`, `java`; ADR-022 §2B). Read-only for commands; caches are
    /// private.
    pub toolchains: Vec<String>,
    /// Toolchains added for one project only, by project id (ADR-022 §8).
    pub project_toolchains: std::collections::BTreeMap<String, Vec<String>>,
    /// "Folders above the project" for every project (ADR-025): the agent's
    /// sandbox can see the names in each folder above a project, not open,
    /// read or change anything in them. Off by default.
    pub parent_folders: bool,
    /// The same, for these projects only (by id).
    pub project_parent_folders: Vec<String>,
}

/// Whose name agent commits carry.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitIdentity {
    /// Copied from the user's git configuration (the repository's, else
    /// the global one): only `user.name` and `user.email`.
    #[default]
    Mine,
    /// "Gardenwall Agent <agent@gardenwall.invalid>".
    Agent,
    /// `git_name` and `git_email`.
    Custom,
}

/// Settings → Web access (ADR-018). The Brave key isn't here: it is kept
/// wrapped by the OS (`Paths::brave_key`), never in settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WebSettings {
    pub provider: SearchProviderKind,
    /// The mode a research session or Web-switch chat starts in.
    pub default_mode: WebMode,
    /// Most searches and pages one session may use.
    pub max_searches: u32,
    pub max_pages: u32,
    /// Show a research sub-agent's findings for review before the agent
    /// gets them (on by default; off sends them
    /// straight back as unreviewed web content).
    pub review_findings: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            provider: SearchProviderKind::Searxng,
            default_mode: WebMode::Research,
            max_searches: 30,
            max_pages: 60,
            review_findings: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SearchProviderKind {
    #[default]
    Searxng,
    Brave,
}

/// How a research session's or Web-switch chat's requests are approved.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WebMode {
    /// Every search and every new page asks in the trusted dialog.
    Ask,
    /// One grant for the session: searches and public HTTPS pages run
    /// without asking, each still checked, logged and listed.
    #[default]
    Research,
}

/// The shell agent commands run in (Windows; Linux uses /bin/sh).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentShell {
    /// PowerShell 7 when it is installed, otherwise Windows PowerShell 5.1.
    #[default]
    PowerShell,
    /// Windows PowerShell 5.1 even when PowerShell 7 is installed.
    WindowsPowerShell,
    /// cmd.exe.
    Cmd,
}

/// How agent sessions share the one loaded model.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SessionMode {
    /// Any session can work; their requests queue at the model, one
    /// generating at a time, each with the full context window.
    #[default]
    Turns,
    /// Only one session works at a time; starting another waits.
    OneAtATime,
    /// The model server runs several slots, so sessions generate at the
    /// same time (sharing the context memory). Applies when the model is
    /// loaded next.
    Parallel,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            compaction: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            retries: 3,
            append_system_prompt: String::new(),
            sessions: SessionMode::Turns,
            parallel_slots: 2,
            shell: AgentShell::PowerShell,
            python: true,
            notify_finished: true,
            notify_approval: true,
            notify_failed: true,
            code_mode: false,
            ask_user: false,
            run_requests: true,
            git_identity: GitIdentity::Mine,
            git_name: String::new(),
            git_email: String::new(),
            step_checkpoint: 100,
            toolchains: Vec::new(),
            project_toolchains: Default::default(),
            parent_folders: false,
            project_parent_folders: Vec::new(),
        }
    }
}

impl AgentSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1024..=262_144).contains(&self.reserve_tokens) {
            return Err("the reserve must be between 1,024 and 262,144 tokens".into());
        }
        if !(1024..=262_144).contains(&self.keep_recent_tokens) {
            return Err("recent tokens kept must be between 1,024 and 262,144".into());
        }
        if self.retries > 10 {
            return Err("at most 10 retries".into());
        }
        if self.step_checkpoint != 0 && !(10..=10_000).contains(&self.step_checkpoint) {
            return Err("the step checkpoint must be between 10 and 10,000 steps, or never".into());
        }
        if !(2..=4).contains(&self.parallel_slots) {
            return Err("parallel sessions must be between 2 and 4".into());
        }
        if self.append_system_prompt.len() > 32 * 1024 {
            return Err("the added instructions must be at most 32 KB".into());
        }
        // Toolchains are named only by the ids Gardenwall knows; anything
        // else is refused rather than stored (ADR-022).
        if let Some(t) = self
            .toolchains
            .iter()
            .chain(self.project_toolchains.values().flatten())
            .find(|t| !crate::toolchains::KNOWN.contains(&t.as_str()))
        {
            return Err(format!("unknown toolchain {t:?}"));
        }
        if self.project_toolchains.len() > 1000 {
            return Err("too many projects with their own toolchains".into());
        }
        if self.project_parent_folders.len() > 1000
            || self
                .project_parent_folders
                .iter()
                .any(|p| p.is_empty() || p.len() > 64)
        {
            return Err("the projects that see the folders above them aren't valid".into());
        }
        if self.git_identity == GitIdentity::Custom
            && gardenwall_agent::git::Identity::new(&self.git_name, &self.git_email).is_none()
        {
            return Err(
                "the commit name and email must each be one line of at most 200 characters, without < or >"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Generation defaults. `None` keeps the model's/runtime's own default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Generation {
    /// System prompt given to new conversations.
    pub default_system_prompt: String,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl Generation {
    pub fn validate(&self) -> Result<(), String> {
        fn range(v: Option<f32>, lo: f32, hi: f32, name: &str) -> Result<(), String> {
            match v {
                Some(x) if !(x.is_finite() && (lo..=hi).contains(&x)) => {
                    Err(format!("{name} must be between {lo} and {hi}"))
                }
                _ => Ok(()),
            }
        }
        range(self.temperature, 0.0, 5.0, "temperature")?;
        range(self.top_p, 0.0, 1.0, "top-p")?;
        range(self.min_p, 0.0, 1.0, "min-p")?;
        if self.top_k.is_some_and(|k| k > 1000) {
            return Err("top-k must be at most 1000".into());
        }
        if self.max_tokens.is_some_and(|n| n == 0 || n > 1_048_576) {
            return Err("maximum response length must be between 1 and 1048576 tokens".into());
        }
        if self.default_system_prompt.len() > conversations::MAX_SYSTEM_PROMPT_BYTES {
            return Err("system prompt is too large".into());
        }
        Ok(())
    }

    pub fn sampling(&self) -> Sampling {
        Sampling {
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            min_p: self.min_p,
            max_tokens: self.max_tokens,
            seed: None,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Paths {
    pub root: PathBuf,
    pub models: PathBuf,
    pub runtime_store: PathBuf,
    /// Contains only the staged relay binary (granted read/execute to the
    /// inference sandbox, so it must hold nothing else and no links).
    pub relay_dir: PathBuf,
    /// Contains only the staged page reader (granted read/execute to its
    /// sandbox), like `relay_dir`.
    pub parse_dir: PathBuf,
    /// Contains only the staged binary analyzer, like `parse_dir`.
    pub binary_dir: PathBuf,
    /// Contains only the staged code-mode program runner, like `parse_dir`.
    pub js_dir: PathBuf,
    /// Contains only the staged download helper, like `parse_dir`.
    pub fetch_dir: PathBuf,
    /// The Brave Search API key, wrapped by the OS (DPAPI).
    pub brave_key: PathBuf,
    /// Ghidra projects (one per analyzed file, reused) and scratch space.
    pub ghidra_work: PathBuf,
    pub work: PathBuf,
    /// Grant journals of running sandboxes; leftovers are revoked at startup.
    pub grants: PathBuf,
    /// Toolchain grants kept between runs (ADR-022 §8a): start-up cleanup
    /// leaves them, and `toolchains::warm` takes them over or revokes them.
    pub kept_grants: PathBuf,
    /// Per-turn checkpoints of agent edits (outside every workspace).
    pub checkpoints: PathBuf,
    settings_file: PathBuf,
    conversations_db: PathBuf,
    approvals_db: PathBuf,
}

impl Paths {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        let paths = Self {
            models: root.join("models"),
            runtime_store: root.join("runtime"),
            relay_dir: root.join("relay"),
            parse_dir: root.join("parse"),
            binary_dir: root.join("binary"),
            js_dir: root.join("js"),
            fetch_dir: root.join("fetch"),
            brave_key: root.join("keys").join("brave-search.key"),
            ghidra_work: root.join("ghidra"),
            work: root.join("work"),
            grants: root.join("sandbox-grants"),
            kept_grants: root.join("sandbox-kept"),
            checkpoints: root.join("checkpoints"),
            settings_file: root.join("settings.json"),
            conversations_db: root.join("conversations.db"),
            approvals_db: root.join("approvals.db"),
            root,
        };
        for dir in [
            &paths.models,
            &paths.runtime_store,
            &paths.relay_dir,
            &paths.work,
            &paths.grants,
            &paths.checkpoints,
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(paths)
    }
}

pub struct Loaded {
    pub sha256: String,
    pub display_name: String,
    pub server: RunningServer,
    pub props: RuntimeProps,
    /// The runtime build this model runs on.
    pub runtime_id: String,
    /// Started with an image projector (images can be attached).
    pub vision: bool,
    /// How automatic faster generation was decided on this load, for a
    /// model whose choice is automatic and that has an MTP layer.
    pub auto_mtp: Option<AutoMtp>,
}

/// The outcome of automatic MTP for one load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoMtp {
    /// On: with it, the whole model fits in GPU memory with enough context.
    On,
    /// Off: by the memory estimate there was no room, so it wasn't tried.
    NoRoomEstimated,
    /// Off: it was tried, but then part of the model would run from system
    /// RAM or the context would be under the minimum, so the model was
    /// loaded again without it.
    NoRoomMeasured,
}

pub struct AppState {
    pub paths: Paths,
    pub library: Library,
    pub settings: std::sync::Mutex<Settings>,
    pub engine: tokio::sync::Mutex<Option<Loaded>>,
    pub chats: std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>,
    pub conversations: std::sync::Mutex<ConversationStore>,
    /// Approval sessions and single-use approvals (ADR-016).
    pub approvals: std::sync::Mutex<ApprovalStore>,
    /// Open agent sessions, oldest first (each with its own folders,
    /// sandbox, approvals and transcript).
    pub agents: std::sync::Mutex<Vec<Arc<crate::agent::LiveSession>>>,
    /// Held by the working session in "one at a time" mode.
    pub agent_run: tokio::sync::Semaphore,
    /// Whether chats are stored encrypted.
    pub encryption: ChatEncryption,
    /// Things that happened at startup that the user should know about.
    pub notices: std::sync::Mutex<Vec<String>>,
    /// Toolchain folders the app keeps granted to the agent's sandbox while
    /// it runs, so sessions start without checking and granting them again
    /// (ADR-022 §8). Set by `toolchains::warm`.
    #[cfg(windows)]
    pub tool_holds: std::sync::Mutex<Vec<(PathBuf, gardenwall_sandbox::GrantHold)>>,
    /// Folders above projects the agent's sandbox may list (ADR-025), held
    /// while the setting is on. Set by `parent_folders::warm` and
    /// `parent_folders::hold_for`.
    #[cfg(windows)]
    pub parent_holds: std::sync::Mutex<Vec<gardenwall_sandbox::ParentListHold>>,
    /// The folder picked in the New project dialog (ADR-024 §5).
    pub new_project_pick: std::sync::Mutex<Option<crate::new_project::Picked>>,
    /// Generation speed of the loaded model, to spot GPU memory paging.
    pub speed: std::sync::Mutex<crate::speed::SpeedWatch>,
    /// GPU memory pressure checks.
    pub gpu: std::sync::Mutex<crate::speed::GpuWatch>,
    /// The latest update check (ADR-017).
    pub update_report: std::sync::Mutex<Option<crate::updates::UpdateReport>>,
    /// Gardenwall's own release check and a downloaded installer (ADR-017 §3.3).
    pub app_updates: crate::app_update::AppUpdates,
    /// Recent errors (memory only) and the session marker's finding.
    pub diagnostics: crate::diagnostics::Diagnostics,
    /// The backup chosen for a restore, and whether one is running.
    pub backup: crate::backup::BackupState,
    /// Voice input: the recording and the speech recogniser (feature 7).
    pub dictation: crate::dictation::DictationState,
    /// Access the user granted for the rest of an agent session (ADR-011
    /// §2.6), e.g. `search` or `fetch:https://docs.rs:443`, per approval
    /// session. Ends with the session and on restart.
    pub egress_grants: std::sync::Mutex<HashMap<SessionId, std::collections::HashSet<String>>>,
    /// Each agent session's visited-sites list (ADR-018), by its approval
    /// session; removed with the session.
    pub web_visits: std::sync::Mutex<HashMap<SessionId, Arc<crate::web::visits::VisitLog>>>,
    /// Chats whose Web switch is on (memory only: off after a restart).
    pub chat_webs: crate::chat_web::ChatWebs,
    /// The embedding model's server, started when document search needs it.
    pub embedder: tokio::sync::Mutex<Option<crate::documents::Embedder>>,
    /// Images chosen for the next chat or agent message, by token.
    pub pending_images: std::sync::Mutex<std::collections::HashMap<String, (String, Vec<u8>)>>,
    /// The open composer's channel for images dropped on the window.
    /// File analysis reports waiting for the next chat message, by token.
    pub pending_reports: std::sync::Mutex<std::collections::HashMap<String, (String, String)>>,
    pub drop_target:
        std::sync::Mutex<Option<(String, tauri::ipc::Channel<crate::attach::AttachReport>)>>,
    /// Finished / needs approval / failed events for the window's cards.
    pub attention: std::sync::Mutex<crate::activity::Attention>,
    /// The agent session on screen in the window, if any (no system
    /// notification for it while the window is visible).
    pub showing_session: std::sync::Mutex<Option<String>>,
    /// The app, for system notifications (set once at startup).
    pub app: std::sync::OnceLock<tauri::AppHandle>,
    /// Each chat's memory block, fixed when the chat is first used, so a
    /// new memory doesn't change an open chat's prompt (which would make
    /// the model read the whole conversation again).
    pub chat_memories: crate::memory::ChatMemories,
    /// Model downloads the user asked to stop, by the file's path in its
    /// repository, and how. Checked on each progress report.
    pub download_cancels:
        std::sync::Mutex<std::collections::HashMap<String, crate::commands::DownloadStop>>,
    /// Paused downloads, by the same key: what each keeps for resuming, so
    /// Cancel can delete it while nothing is running.
    pub paused_downloads:
        std::sync::Mutex<std::collections::HashMap<String, Vec<std::path::PathBuf>>>,
    /// Research sub-agents' approval sessions while they run (for the dialog).
    pub research_sessions: crate::research_agent::ResearchSessions,
    /// Ends open web access when offline mode or lockdown turns on (GW-019).
    pub net_stop: Arc<crate::web::NetStop>,
    /// Start-up cleanup of leftover sandbox permissions is still running
    /// (`startup_cleanup`); sandboxes wait for it.
    pub cleaning_up: std::sync::atomic::AtomicBool,
    /// The user chose to close: the close prompt isn't shown again.
    pub closing: std::sync::atomic::AtomicBool,
    /// Model downloads running now, by the same key as `download_cancels`
    /// (for the close prompt).
    pub active_downloads: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl AppState {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        let paths = Paths::new(root)?;
        let mut notices = Vec::new();
        // A restore unpacked before the restart is finished first, before
        // any store opens (release plan feature 3).
        crate::backup::apply_pending_restore(&paths.root, &mut notices);
        // The natural read-aloud voice tried on 2026-11-23 was removed
        // its downloaded files go too.
        for leftover in ["voice", "voice-work"] {
            let _ = fs::remove_dir_all(paths.root.join("speech").join(leftover));
        }
        // Gardenwall never needs administrator rights
        // : started from a PowerShell run as administrator,
        // what it creates belongs to Administrators instead of the user,
        // and Git in the agent's sandbox refuses those repositories.
        #[cfg(windows)]
        if gardenwall_sandbox::running_elevated() {
            notices.push(
                "Gardenwall is running as administrator, which it never needs. Folders and Git repositories it creates now belong to Administrators instead of you, and Git in the agent's sandbox refuses them (\"dubious ownership\"). Close Gardenwall and start it normally (not with Run as administrator, or from a terminal opened that way).".into(),
            );
        }
        // Leftover sandbox permissions are cleaned up off the main thread
        // (`startup_cleanup`, A Rust toolchain's
        // 72,000 files made the window "Not responding" at every start).
        // Each agent session has its own scratch folder, deleted when it
        // ends (ADR-022 hardening); none is open at start, so whatever is
        // left (a crash, or the shared folder of earlier versions) goes.
        // Moved aside here (instant) and deleted by the cleanup, so a big
        // one (Cargo's caches live there) never holds up the window.
        let scratch = paths.root.join("agent-scratch");
        if scratch.exists() {
            let aside = paths.root.join(format!("agent-scratch.old-{}", now_ms()));
            if fs::rename(&scratch, &aside).is_err() {
                let _ = fs::remove_dir_all(&scratch);
            }
        }
        // Opening runs restart recovery (ADR-007 §12) before anything else
        // can be approved.
        let approvals =
            ApprovalStore::open(&paths.approvals_db, now_ms()).map_err(std::io::Error::other)?;
        let recovered = approvals.recovery().clone();
        if recovered.cancelled_proposals > 0 {
            notices.push(format!(
                "{} unanswered approval request(s) from the last run were cancelled.",
                recovered.cancelled_proposals
            ));
        }
        let settings = load_settings(&paths.settings_file);
        let (cipher, encryption) = match gardenwall_keychain::load_or_create_key(
            &paths.root.join("chat.key"),
        ) {
            Ok(key) => (
                Some(gardenwall_store::crypto::FieldCipher::new(&key)),
                ChatEncryption::Encrypted,
            ),
            Err(gardenwall_keychain::KeyError::Unsupported) => (None, ChatEncryption::Unencrypted),
            Err(e) => {
                notices.push(format!(
                    "Saved chats could not be unlocked ({e}). They are kept unchanged and read-only."
                ));
                (None, ChatEncryption::Unencrypted)
            }
        };
        let conversations = ConversationStore::open_with(&paths.conversations_db, cipher)
            .map_err(std::io::Error::other)?;
        let encryption = if conversations.is_locked() {
            ChatEncryption::Locked
        } else {
            encryption
        };
        // Models admitted before these facts were recorded: read their
        // expert count and MTP layers once (header only, fast).
        let library = Library::new(paths.models.clone());
        library.backfill_header_facts();
        // Templates audited with older rules get the current audit.
        library.refresh_template_audits();
        Ok(Self {
            conversations: std::sync::Mutex::new(conversations),
            library: Library::new(paths.models.clone()),
            paths,
            settings: std::sync::Mutex::new(settings),
            engine: tokio::sync::Mutex::new(None),
            chats: std::sync::Mutex::new(HashMap::new()),
            notices: std::sync::Mutex::new(notices),
            #[cfg(windows)]
            tool_holds: std::sync::Mutex::new(Vec::new()),
            new_project_pick: std::sync::Mutex::new(None),
            #[cfg(windows)]
            parent_holds: std::sync::Mutex::new(Vec::new()),
            encryption,
            approvals: std::sync::Mutex::new(approvals),
            agents: std::sync::Mutex::new(Vec::new()),
            agent_run: tokio::sync::Semaphore::new(1),
            speed: std::sync::Mutex::new(Default::default()),
            gpu: std::sync::Mutex::new(Default::default()),
            update_report: std::sync::Mutex::new(None),
            app_updates: Default::default(),
            diagnostics: Default::default(),
            backup: Default::default(),
            dictation: Default::default(),
            egress_grants: std::sync::Mutex::new(Default::default()),
            web_visits: std::sync::Mutex::new(Default::default()),
            chat_webs: std::sync::Mutex::new(Default::default()),
            embedder: tokio::sync::Mutex::new(None),
            pending_images: std::sync::Mutex::new(Default::default()),
            drop_target: std::sync::Mutex::new(None),
            attention: std::sync::Mutex::new(Default::default()),
            showing_session: std::sync::Mutex::new(None),
            app: std::sync::OnceLock::new(),
            pending_reports: std::sync::Mutex::new(Default::default()),
            chat_memories: Default::default(),
            download_cancels: std::sync::Mutex::new(Default::default()),
            paused_downloads: std::sync::Mutex::new(Default::default()),
            research_sessions: std::sync::Mutex::new(Default::default()),
            net_stop: Arc::new(crate::web::NetStop::default()),
            cleaning_up: std::sync::atomic::AtomicBool::new(cfg!(windows)),
            closing: std::sync::atomic::AtomicBool::new(false),
            active_downloads: std::sync::Mutex::new(Default::default()),
        })
    }

    /// After a reply: report a sharp slowdown (with the GPU memory
    /// picture), or, at most every 20 s, a nearly full GPU.
    pub fn check_gpu(
        &self,
        model_pid: Option<u32>,
        tokens_per_second: Option<f64>,
        generated: Option<u64>,
    ) -> Option<GpuWarning> {
        let slowdown = match (tokens_per_second, generated) {
            (Some(tps), Some(n)) => self.speed.lock().ok()?.observe(tps, n),
            _ => None,
        };
        if let Some(s) = slowdown {
            let report = gpu_report();
            if let (Some(r), Ok(mut watch)) = (&report, self.gpu.lock()) {
                // Covered by this warning; not repeated as pressure.
                watch.pressure(r.under_pressure());
            }
            return Some(GpuWarning {
                kind: WarningKind::Slowdown,
                baseline: Some(s.baseline),
                current: Some(s.current),
                gpu: report.map(|r| gpu_view(&r, model_pid)),
            });
        }
        let mut watch = self.gpu.lock().ok()?;
        if !watch.due(Instant::now()) {
            return None;
        }
        let report = gpu_report()?;
        watch.pressure(report.under_pressure()).then(|| GpuWarning {
            kind: WarningKind::Pressure,
            baseline: None,
            current: None,
            gpu: Some(gpu_view(&report, model_pid)),
        })
    }

    /// A model was loaded: speeds and warnings start over.
    pub fn reset_gpu_watch(&self) {
        if let Ok(mut s) = self.speed.lock() {
            *s = Default::default();
        }
        if let Ok(mut g) = self.gpu.lock() {
            *g = Default::default();
        }
    }

    pub fn add_notice(&self, notice: String) {
        if let Ok(mut n) = self.notices.lock() {
            n.push(notice);
        }
    }

    /// The settings in force. In lockdown mode this is the saved settings
    /// with everything optional switched off, so every reader is covered
    /// without checking for lockdown itself.
    ///
    /// Fails closed: if the lock was poisoned (a panic while it was held),
    /// the settings may be half-changed, so everything optional is off as in
    /// lockdown until Gardenwall restarts.
    pub fn settings(&self) -> Settings {
        let (saved, poisoned) = self.read_settings();
        if saved.lockdown || poisoned {
            saved.locked_down()
        } else {
            saved
        }
    }

    /// Whether offline mode is in force (offline or lockdown), without
    /// copying the settings: for checks made many times a second, such as
    /// each download progress report. A poisoned lock counts as offline,
    /// as in [`AppState::settings`].
    pub fn offline_now(&self) -> bool {
        match self.settings.lock() {
            Ok(s) => s.offline || s.lockdown,
            Err(_) => true,
        }
    }

    /// The settings as saved, for the pages that edit them. Lockdown mode
    /// changes none of them; they apply again when it is turned off.
    pub fn saved_settings(&self) -> Settings {
        self.read_settings().0
    }

    /// The saved settings and whether the lock was poisoned. A poisoned lock
    /// still holds the real settings; defaults would quietly turn lockdown
    /// and offline mode off.
    fn read_settings(&self) -> (Settings, bool) {
        match self.settings.lock() {
            Ok(s) => (s.clone(), false),
            Err(poisoned) => (poisoned.into_inner().clone(), true),
        }
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> std::io::Result<Settings> {
        let mut guard = self
            .settings
            .lock()
            .map_err(|_| std::io::Error::other("settings lock poisoned"))?;
        f(&mut guard);
        let tmp = self.paths.settings_file.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(&*guard).map_err(std::io::Error::other)?,
        )?;
        fs::rename(&tmp, &self.paths.settings_file)?;
        Ok(guard.clone())
    }
}

fn load_settings(path: &Path) -> Settings {
    // Missing or unreadable settings fall back to defaults; a corrupt file
    // never enables anything (all defaults are the restrictive choice or
    // the documented default).
    fs::read(path)
        .ok()
        .filter(|b| b.len() < 64 * 1024)
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        // Out-of-range values from a hand-edited file are discarded.
        .map(|mut s| {
            if s.generation.validate().is_err() {
                s.generation = Generation::default();
            }
            s
        })
        .unwrap_or_default()
}

#[cfg_attr(not(windows), allow(dead_code))]
fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Copy the relay shipped next to the application into `relay_dir`, so the
/// sandbox grant covers a directory with exactly one, singly linked file
/// (build tools hard-link binaries, which the sandbox rightly refuses).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn stage_relay(relay_dir: &Path) -> std::io::Result<PathBuf> {
    stage_helper(relay_dir, "gardenwall-relay")
}

/// Copy a helper binary shipped next to the application into its own
/// directory, which then holds nothing else (it is granted to a sandbox).
/// The visited-sites list of an agent session, by its approval session.
pub fn visit_log(
    state: &AppState,
    session: SessionId,
) -> Option<Arc<crate::web::visits::VisitLog>> {
    state.web_visits.lock().ok()?.get(&session).cloned()
}

pub fn stage_helper(relay_dir: &Path, base: &str) -> std::io::Result<PathBuf> {
    let name = if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    };
    let name = name.as_str();
    let source = std::env::current_exe()?
        .parent()
        .map(|p| p.join(name))
        .ok_or_else(|| std::io::Error::other("cannot locate application directory"))?;
    if !source.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{base} was not found next to the application"),
        ));
    }
    let target = relay_dir.join(name);
    let up_to_date = target.is_file() && sha256_file(&target)? == sha256_file(&source)?;
    if !up_to_date {
        // Replace the whole directory so nothing else can linger in it.
        if relay_dir.exists() {
            fs::remove_dir_all(relay_dir)?;
        }
        fs::create_dir_all(relay_dir)?;
        let tmp = relay_dir.join(format!("{name}.tmp"));
        fs::copy(&source, &tmp)?;
        fs::rename(&tmp, &target)?;
        if sha256_file(&target)? != sha256_file(&source)? {
            return Err(std::io::Error::other(format!(
                "staged {base} does not match its source"
            )));
        }
    }
    Ok(target)
}

/// How chats are protected at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatEncryption {
    /// Encrypted with a key wrapped by the OS (DPAPI on Windows).
    Encrypted,
    /// No OS key protection on this platform yet.
    Unencrypted,
    /// Encrypted chats exist but their key is unavailable: read-only.
    Locked,
}

/// The GPU memory picture. A system query (DXGI, performance counters), so
/// on the app's multi-threaded runtime the thread is handed over while it
/// runs instead of stalling other tasks; elsewhere it simply runs.
fn gpu_report() -> Option<gardenwall_gpu::GpuReport> {
    let query = || gardenwall_gpu::report().ok();
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(query)
        }
        _ => query(),
    }
}

/// Stop the model servers so their sandbox grants are revoked: at exit,
/// and before an update's installer replaces the app.
pub async fn shutdown_servers(state: &AppState) {
    if let Some(loaded) = state.engine.lock().await.take() {
        loaded.server.shutdown().await;
    }
    if let Some(e) = state.embedder.lock().await.take() {
        e.server.shutdown().await;
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_persist_and_corrupt_files_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().into()).unwrap();
        assert!(!state.settings().offline);
        state.update_settings(|s| s.offline = true).unwrap();
        let reopened = AppState::new(dir.path().into()).unwrap();
        assert!(reopened.settings().offline);

        fs::write(dir.path().join("settings.json"), b"{not json").unwrap();
        let reopened = AppState::new(dir.path().into()).unwrap();
        assert_eq!(reopened.settings(), Settings::default());

        fs::write(
            dir.path().join("settings.json"),
            br#"{"offline":true,"generation":{"temperature":99}}"#,
        )
        .unwrap();
        let reopened = AppState::new(dir.path().into()).unwrap();
        assert!(reopened.settings().offline);
        assert_eq!(reopened.settings().generation, Generation::default());
    }

    #[test]
    fn favourite_model_settings_default_off_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        // A settings file from before the redesign has neither field.
        fs::write(dir.path().join("settings.json"), br#"{"offline":false}"#).unwrap();
        let state = AppState::new(dir.path().into()).unwrap();
        assert_eq!(state.saved_settings().favourite_model, None);
        assert!(!state.saved_settings().load_on_launch);
        state
            .update_settings(|s| {
                s.favourite_model = Some("ab".repeat(32));
                s.load_on_launch = true;
            })
            .unwrap();
        let reopened = AppState::new(dir.path().into()).unwrap();
        assert_eq!(
            reopened.saved_settings().favourite_model,
            Some("ab".repeat(32))
        );
        assert!(reopened.saved_settings().load_on_launch);
    }

    #[test]
    fn generation_settings_are_range_checked() {
        let ok = Generation {
            temperature: Some(0.7),
            top_p: Some(0.95),
            top_k: Some(20),
            min_p: Some(0.0),
            max_tokens: Some(4096),
            ..Generation::default()
        };
        assert!(ok.validate().is_ok());
        for bad in [
            Generation {
                temperature: Some(f32::NAN),
                ..ok.clone()
            },
            Generation {
                temperature: Some(-0.1),
                ..ok.clone()
            },
            Generation {
                top_p: Some(1.5),
                ..ok.clone()
            },
            Generation {
                top_k: Some(5000),
                ..ok.clone()
            },
            Generation {
                max_tokens: Some(0),
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod lockdown_tests {
    use super::*;

    #[test]
    fn lockdown_switches_everything_optional_off_without_changing_what_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        state
            .update_settings(|s| {
                s.searxng_url = Some("https://search.example".into());
                s.agent_tool_folders = vec![PathBuf::from("C:/tools")];
                s.agent.python = true;
                s.agent.code_mode = true;
                s.mcp_servers = vec![crate::mcp::McpServerConfig {
                    enabled: true,
                    ..Default::default()
                }];
                s.binary.ghidra_dir = Some(PathBuf::from("C:/ghidra"));
            })
            .unwrap();
        let open = state.settings();
        assert_eq!(open, state.saved_settings());

        state.update_settings(|s| s.lockdown = true).unwrap();
        let locked = state.settings();
        assert!(locked.lockdown && locked.offline);
        assert!(locked.searxng_url.is_none() && locked.agent_tool_folders.is_empty());
        assert!(!locked.agent.python && !locked.agent.code_mode);
        assert!(locked.mcp_servers.iter().all(|s| !s.enabled));
        assert!(locked.binary.ghidra_dir.is_none());
        // What is saved is untouched, so it all comes back afterwards.
        let saved = state.saved_settings();
        assert!(saved.agent.python && saved.mcp_servers[0].enabled && !saved.offline);
        state.update_settings(|s| s.lockdown = false).unwrap();
        assert_eq!(state.settings(), open);
    }

    #[test]
    fn a_poisoned_settings_lock_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        state
            .update_settings(|s| {
                s.lockdown = true;
                s.agent.python = true;
            })
            .unwrap();
        // A panic while the lock is held poisons it.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = state.update_settings(|_| panic!("simulated failure"));
        }));
        assert!(state.settings.is_poisoned());
        // Lockdown is kept, not replaced by defaults.
        let s = state.settings();
        assert!(s.lockdown && s.offline && !s.agent.python);
        // Even without lockdown saved, a poisoned lock switches optional
        // features off; the saved values are still readable for display.
        state.settings.clear_poison();
        state.update_settings(|s| s.lockdown = false).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = state.update_settings(|_| panic!("simulated failure"));
        }));
        let s = state.settings();
        assert!(s.offline && !s.agent.python);
        assert!(state.saved_settings().agent.python);
    }
}
