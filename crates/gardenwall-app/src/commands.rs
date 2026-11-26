//! IPC commands. The webview is treated as semi-trusted (it renders model
//! output): every argument is validated here, file paths never come from the
//! webview, and errors are reduced to plain messages.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gardenwall_inference::client::{ChatMessage, ChatRequest, Role, StreamEvent};
use gardenwall_inference::repetition::RepetitionGuard;
// Launching is Windows-only until the other sandboxes run models (ADR-012).
#[cfg_attr(not(windows), allow(unused_imports))]
use gardenwall_inference::launch::{GpuOffload, KvCacheType, LaunchSpec};
#[cfg_attr(not(windows), allow(unused_imports))]
use gardenwall_inference::supervisor::{self, IsolationStatus, Timeouts};
use gardenwall_models::hub::{HubClient, HubConfig, RepoId};
use gardenwall_models::import::import_local;
use gardenwall_models::integrity::Sha256Digest;
use gardenwall_runtime::download::Downloader;
use gardenwall_runtime::pins;
use gardenwall_runtime::store::RuntimeStore;
use gardenwall_store::approvals::{ApprovalStore, AuditEntry, Id, ProposalView};
use gardenwall_store::conversations::{
    self, Conversation, ConversationStore, ConversationSummary, MessageRole, MessageStats,
    NewMessage, StoredMessage,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::library::{
    KvCache, LoadOptions, MOE_CACHE_CHOICES, ModelRecord, Speculation, Thinking, valid_sha,
};
use crate::state::{AppState, Generation, Loaded, now_ms};

type CmdResult<T> = Result<T, String>;

fn msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------- views ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeView {
    id: String,
    backend: String,
    download_mb: u64,
    installed: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LoadedView {
    sha256: String,
    display_name: String,
    isolation: String,
    context_window: u64,
    supports_tools: bool,
    build: Option<String>,
    /// An image projector is loaded: images can be attached in chat.
    vision: bool,
    /// Where the weights went, when a meaningful part is in system RAM.
    placement: Option<PlacementView>,
    /// How automatic MTP was decided on this load, if it applied.
    auto_mtp: Option<crate::state::AutoMtp>,
}

/// How much of the model runs from system RAM (from the runtime's load log;
/// display only).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlacementView {
    gpu_gib: f64,
    ram_gib: f64,
    /// Layers whose MoE experts are in system RAM, if the runtime said.
    expert_layers_in_ram: Option<u32>,
    /// `(on GPU, total)` layers, if the runtime said.
    layers: Option<(u32, u32)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    data_dir: String,
    offline: bool,
    /// Lockdown mode is on (`offline` is then on too).
    lockdown: bool,
    /// Leftover sandbox permissions from the last run are still being
    /// cleaned up; sessions start once it's done.
    cleaning_up: bool,
    generation: Generation,
    agent_settings: crate::state::AgentSettings,
    sandbox_available: bool,
    notices: Vec<String>,
    chat_encryption: crate::state::ChatEncryption,
    agent_tool_folders: Vec<String>,
    /// Tool folders the sandbox can't be given yet (an administrator must
    /// allow sandboxes to read them once); sessions leave them out.
    agent_tool_folders_unusable: Vec<String>,
    searxng_url: Option<String>,
    runtimes: Vec<RuntimeView>,
    models: Vec<ModelRecord>,
    loaded: Option<LoadedView>,
    /// PowerShell 7 found in its install folder (the agent's preferred
    /// shell), if any.
    powershell7: Option<String>,
    /// The all-users Python offered to the agent, if one is installed.
    python: Option<String>,
    /// An all-users Python the sandbox can't read yet (it needs a one-time
    /// read grant from an administrator), if any.
    python_unreadable: Option<String>,
    /// Git for Windows in Program Files that sandboxes can use and that
    /// isn't a tool folder yet ("Add Git" in Settings → Agent tools).
    git_to_add: Option<String>,
    /// The favourite model's SHA-256, if one is set and still in the library.
    favourite_model: Option<String>,
    /// Load the favourite model at start (Home is then skipped).
    load_on_launch: bool,
    /// Settings → Voice (release plan features 7 and 8).
    voice_settings: crate::speech::VoiceSettings,
    /// Microsoft's Visual C++ runtime files missing from Windows (the
    /// engines need them; the user installs Microsoft's package).
    vc_runtime_missing: Vec<String>,
    /// Toolchains found on this PC, and whether each is added to the
    /// agent's sandbox (Settings → Agent tools; ADR-022 §2B).
    toolchains: Vec<crate::toolchains::ToolchainView>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    done: u64,
    total: u64,
}

fn loaded_view(l: &Loaded) -> LoadedView {
    LoadedView {
        sha256: l.sha256.clone(),
        display_name: l.display_name.clone(),
        isolation: match l.server.isolation() {
            IsolationStatus::Sandboxed => "sandboxed".into(),
            IsolationStatus::Unsandboxed => "unsandboxed".into(),
        },
        context_window: l.props.context_window,
        supports_tools: l.props.supports_tools,
        build: l.props.build.clone(),
        vision: l.vision,
        placement: {
            let p = l.server.logs().placement();
            p.partly_in_ram().then(|| PlacementView {
                gpu_gib: p.gpu_mib / 1024.0,
                ram_gib: p.ram_mib / 1024.0,
                expert_layers_in_ram: p.overflowing_layers,
                layers: p.layers,
            })
        },
        auto_mtp: l.auto_mtp,
    }
}

/// Throttled progress reporting (at most ~10 updates per second).
pub(crate) fn progress_reporter(channel: Channel<Progress>) -> impl FnMut(u64, u64) -> bool {
    let mut last = Instant::now() - Duration::from_secs(1);
    move |done, total| {
        if done == total || last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            let _ = channel.send(Progress { done, total });
        }
        true
    }
}

pub(crate) fn platform_builds() -> Vec<&'static pins::RuntimeBuild> {
    pins::current_platform()
        .map(|(os, arch)| pins::builds_for(os, arch))
        .unwrap_or_default()
}

// ---------- state & settings ----------

#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> CmdResult<AppView> {
    let store = RuntimeStore::new(&state.paths.runtime_store);
    let runtimes = platform_builds()
        .into_iter()
        .map(|b| RuntimeView {
            id: b.id.into(),
            backend: format!("{:?}", b.backend),
            download_mb: b.download_size().div_ceil(1_000_000),
            installed: store.is_installed(b),
        })
        .collect();
    let loaded = engine_now(&state, |e| e.map(loaded_view)).await.flatten();
    let python = python();
    Ok(AppView {
        data_dir: state.paths.root.display().to_string(),
        offline: state.settings().offline,
        lockdown: state.settings().lockdown,
        cleaning_up: state.cleaning_up.load(std::sync::atomic::Ordering::SeqCst),
        generation: state.settings().generation,
        // As saved: this is what the settings page edits and sends back.
        agent_settings: state.saved_settings().agent,
        sandbox_available: cfg!(windows),
        notices: state.notices.lock().map(|n| n.clone()).unwrap_or_default(),
        chat_encryption: state.encryption,
        agent_tool_folders: state
            .settings()
            .agent_tool_folders
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        agent_tool_folders_unusable: unusable_tool_folders(&state.settings().agent_tool_folders),
        searxng_url: state.settings().searxng_url,
        runtimes,
        models: state.library.list(),
        loaded,
        powershell7: powershell7(),
        python: python.0,
        python_unreadable: python.1,
        git_to_add: git_to_add(&state.settings().agent_tool_folders)
            .map(|p| p.display().to_string()),
        favourite_model: state
            .saved_settings()
            .favourite_model
            .filter(|sha| state.library.get(sha).is_some()),
        load_on_launch: state.saved_settings().load_on_launch,
        voice_settings: state.saved_settings().voice,
        toolchains: crate::toolchains::views(&state.saved_settings().agent.toolchains),
        vc_runtime_missing: gardenwall_runtime::vcredist::missing()
            .into_iter()
            .map(String::from)
            .collect(),
    })
}

/// Why a model can't be the favourite, if it can't.
fn favourite_refusal(record: Option<&ModelRecord>) -> Option<&'static str> {
    match record {
        None => Some("model not found"),
        Some(r) if r.architecture == "clip" => {
            Some("this file is an image projector, not a model you can chat with")
        }
        Some(_) => None,
    }
}

/// Set or clear the favourite model and whether it loads when Gardenwall
/// starts (Home, the setup wizard, Settings → General).
#[tauri::command]
pub fn set_favourite_model(
    state: State<'_, AppState>,
    sha256: Option<String>,
    load_on_launch: bool,
) -> CmdResult<()> {
    if let Some(why) = sha256
        .as_ref()
        .and_then(|sha| favourite_refusal(state.library.get(sha).as_ref()))
    {
        return Err(why.into());
    }
    let load_on_launch = load_on_launch && sha256.is_some();
    state
        .update_settings(|s| {
            s.favourite_model = sha256;
            s.load_on_launch = load_on_launch;
        })
        .map(|_| ())
        .map_err(msg)
}

/// GPU and system memory for Home's memory bars.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMemoryView {
    /// Absent where GPU status can't be read (other platforms, no access).
    gpu: Option<crate::speed::GpuView>,
    ram_total_bytes: Option<u64>,
    ram_available_bytes: Option<u64>,
}

/// Read the loaded model for a status view without waiting on a load.
/// Other users hold the engine lock only to copy a few fields, so a short
/// wait covers them; a load, an unload or a runtime test holds it for the
/// whole operation, and in each of those the slot is empty anyway (the
/// previous model is taken out first). `None` means the lock stayed busy.
pub(crate) async fn engine_now<T>(
    state: &AppState,
    f: impl FnOnce(Option<&Loaded>) -> T,
) -> Option<T> {
    let guard = tokio::time::timeout(std::time::Duration::from_millis(200), state.engine.lock())
        .await
        .ok()?;
    Some(f(guard.as_ref()))
}

/// Read-only GPU and RAM figures (DXGI and performance counters on
/// Windows), off the async runtime. Nothing here can change anything.
#[tauri::command]
pub async fn system_memory(state: State<'_, AppState>) -> CmdResult<SystemMemoryView> {
    // While a model loads, its memory counts under other programs until it
    // is ready; the figures themselves stay live.
    let model_pid = engine_now(&state, |e| e.and_then(|l| l.server.pid()))
        .await
        .flatten();
    tauri::async_runtime::spawn_blocking(move || {
        let gpu = gardenwall_gpu::report()
            .ok()
            .map(|r| crate::speed::gpu_view(&r, model_pid));
        let ram = gardenwall_gpu::system_memory().ok();
        SystemMemoryView {
            gpu,
            ram_total_bytes: ram.map(|r| r.0),
            ram_available_bytes: ram.map(|r| r.1),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Git for Windows when it is installed, every sandbox can already read it
/// (so no permission is changed) and it isn't a tool folder yet.
fn git_to_add(tool_folders: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    use gardenwall_agent::git::{find_git_for_windows, git_folder_ready};
    let git = find_git_for_windows()?;
    let canonical = std::fs::canonicalize(&git).ok()?;
    (git_folder_ready(&git) && !tool_folders.iter().any(|t| *t == canonical || *t == git))
        .then_some(canonical)
}

/// Add the installed Git for Windows as a read-only tool folder. The path
/// is found here in the core, never taken from the webview.
#[tauri::command]
pub fn add_git_tool_folder(state: State<'_, AppState>) -> CmdResult<()> {
    let git = git_to_add(&state.settings().agent_tool_folders)
        .ok_or("Git for Windows wasn't found in Program Files, or is already added")?;
    state
        .update_settings(|s| {
            if !s.agent_tool_folders.contains(&git) {
                s.agent_tool_folders.push(git);
            }
        })
        .map(|_| ())
        .map_err(msg)
}

/// Tool folders the sandbox can't be given (Windows): neither readable by
/// every AppContainer nor changeable by this user.
fn unusable_tool_folders(folders: &[std::path::PathBuf]) -> Vec<String> {
    #[cfg(windows)]
    {
        folders
            .iter()
            .filter(|p| !gardenwall_sandbox::sandbox_can_use(p))
            .map(|p| p.display().to_string())
            .collect()
    }
    #[cfg(not(windows))]
    {
        let _ = folders;
        Vec::new()
    }
}

/// The Python offered to the agent, and one the sandbox can't read yet.
fn python() -> (Option<String>, Option<String>) {
    #[cfg(any(windows, target_os = "linux"))]
    {
        use gardenwall_agent::python::{PythonStatus, python_status};
        match python_status() {
            PythonStatus::Ready(p) => (Some(p.display().to_string()), None),
            PythonStatus::NotReadable(p) => (None, Some(p.display().to_string())),
            PythonStatus::Missing => (None, None),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        (None, None)
    }
}

fn powershell7() -> Option<String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        gardenwall_agent::sandbox_shell::find_powershell7().map(|p| p.display().to_string())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// Offline mode. Turning it on also ends the web access already open
/// (GW-019): requests in flight are cancelled, sessions and chats can't
/// start another, and downloads stop at their next progress report (kept
/// for resuming).
#[tauri::command]
pub fn set_offline(state: State<'_, AppState>, offline: bool) -> CmdResult<()> {
    let result = state
        .update_settings(|s| s.offline = offline)
        .map(|_| ())
        .map_err(msg);
    if offline {
        state.net_stop.stop_all();
    }
    result
}

/// Choose a folder of programs the agent may run (read-only), using the
/// native folder picker opened here in the trusted core.
#[tauri::command]
pub async fn add_agent_tool_folder(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(msg)?;
    let Some(picked) = picked else { return Ok(()) };
    let path = picked.into_path().map_err(msg)?;
    // The same breadth rules as workspaces: never a drive, the home folder
    // or a system folder.
    let checked = gardenwall_fs::Workspace::open(&path).map_err(msg)?;
    let canonical = checked.root().to_path_buf();
    // Never a folder with credentials in it (ADR-022 hardening).
    let to_check = canonical.clone();
    if let Some(problem) =
        tauri::async_runtime::spawn_blocking(move || crate::toolchains::grant_problem(&to_check))
            .await
            .map_err(msg)?
    {
        return Err(problem);
    }
    state
        .update_settings(|s| {
            if !s.agent_tool_folders.contains(&canonical) {
                s.agent_tool_folders.push(canonical);
            }
        })
        .map(|_| ())
        .map_err(msg)
}

#[tauri::command]
pub fn remove_agent_tool_folder(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    state
        .update_settings(|s| {
            s.agent_tool_folders
                .retain(|p| p.display().to_string() != path)
        })
        .map(|_| ())
        .map_err(msg)
}

// ---------- runtime ----------

#[tauri::command]
pub async fn install_runtime(
    state: State<'_, AppState>,
    build_id: String,
    on_progress: Channel<Progress>,
) -> CmdResult<()> {
    let build = platform_builds()
        .into_iter()
        .find(|b| b.id == build_id)
        .ok_or("unknown runtime build for this platform")?;
    let store = RuntimeStore::new(&state.paths.runtime_store);
    let downloader = Downloader::new().map_err(msg)?;
    let result = store
        .install(build, &downloader, progress_reporter(on_progress))
        .await
        .map(|_| ())
        .map_err(msg);
    crate::diagnostics::noted(&state, "runtime install", result)
}

// ---------- model library ----------

fn parse_expected_sha(expected: Option<String>) -> CmdResult<Option<Sha256Digest>> {
    match expected
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    {
        None => Ok(None),
        Some(hex) => Sha256Digest::from_hex(&hex)
            .map(Some)
            .ok_or_else(|| "expected SHA-256 must be 64 hexadecimal characters".into()),
    }
}

/// Import a local GGUF file. The path comes from a native file dialog opened
/// by the backend, never from the webview.
#[tauri::command]
pub async fn import_model(
    app: AppHandle,
    state: State<'_, AppState>,
    expected_sha256: Option<String>,
    on_progress: Channel<Progress>,
) -> CmdResult<Option<ModelRecord>> {
    let expected = parse_expected_sha(expected_sha256)?;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Import a GGUF model")
            .add_filter("GGUF models", &["gguf"])
            .add_filter("All files", &["*"])
            .blocking_pick_file()
    })
    .await
    .map_err(msg)?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let source = picked
        .into_path()
        .map_err(|_| "unsupported file location".to_string())?;
    let dir = state.library.dir().to_path_buf();
    let report = progress_reporter(on_progress);
    // One part of a split model brings the whole set from its folder.
    if let Some(parts) = gardenwall_models::import::split_siblings(&source).map_err(msg)? {
        if expected.is_some() {
            return Err("an expected SHA-256 applies to a single file; leave it empty for a split model (its parts are checked against each other)".into());
        }
        let model = tauri::async_runtime::spawn_blocking(move || {
            gardenwall_models::import::import_split(&parts, &dir, report)
        })
        .await
        .map_err(msg)?
        .map_err(msg)?;
        return state.library.admit_split(&model).map(Some).map_err(msg);
    }
    let model = tauri::async_runtime::spawn_blocking(move || {
        import_local(&source, &dir, expected.as_ref(), report)
    })
    .await
    .map_err(msg)?
    .map_err(msg)?;
    state.library.admit(&model).map(Some).map_err(msg)
}

#[tauri::command]
pub fn acknowledge_template(state: State<'_, AppState>, sha256: String) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    record.template.acknowledged = true;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

#[tauri::command]
pub async fn delete_model(state: State<'_, AppState>, sha256: String) -> CmdResult<()> {
    if !valid_sha(&sha256) {
        return Err("invalid model id".into());
    }
    if state
        .engine
        .lock()
        .await
        .as_ref()
        .is_some_and(|l| l.sha256 == sha256)
    {
        return Err("unload the model before deleting it".into());
    }
    state.library.remove(&sha256).map_err(msg)?;
    #[cfg(windows)]
    {
        // Remove the model's persistent sandbox identity and cached state.
        let _ = supervisor::forget_model_sandbox(&sha256);
    }
    Ok(())
}

// ---------- hub ----------

pub(crate) fn hub(state: &AppState) -> CmdResult<HubClient> {
    let config = HubConfig {
        offline: state.settings().offline,
        ..HubConfig::hugging_face()
    };
    let client = HubClient::new(config).map_err(msg)?;
    // Download bytes are fetched by the sandboxed helper (ADR-013 §2.6);
    // digests and GGUF checks stay here.
    #[cfg(windows)]
    let client = client.with_fetcher(std::sync::Arc::new(
        crate::download_helper::SandboxedFetcher {
            helper_dir: state.paths.fetch_dir.clone(),
            journal: state.paths.grants.clone(),
        },
    ));
    Ok(client)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubListingView {
    repo: String,
    downloads: u64,
    likes: u64,
    gated: bool,
}

#[tauri::command]
pub async fn search_hub(
    state: State<'_, AppState>,
    query: String,
) -> CmdResult<Vec<HubListingView>> {
    let results = hub(&state)?.search(&query, 30).await.map_err(msg)?;
    Ok(results
        .into_iter()
        .map(|l| HubListingView {
            repo: l.repo.to_string(),
            downloads: l.downloads,
            likes: l.likes,
            gated: l.gated,
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoFileView {
    path: String,
    /// The whole model's size (all parts of a split model).
    size: u64,
    /// 1 for a single file; the part count of a split model.
    parts: u32,
    /// Fit label, trade-off card and best-fit mark, worked out locally.
    #[serde(flatten)]
    facts: crate::picker::card::FileFacts,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoView {
    repo: String,
    commit: String,
    license: Option<String>,
    gated: bool,
    files: Vec<RepoFileView>,
    skipped: usize,
    /// The computer the labels are for.
    hardware: String,
    /// What the labels can't know before download.
    fit_note: &'static str,
    /// Where the bits and quality words come from.
    quant_about: String,
    /// "Known publisher", and warnings (new, few downloads, gated), as in
    /// the ranking: a repository can come from a research session's pages.
    known_publisher: bool,
    warnings: Vec<String>,
}

/// A repository's models: single files as they are, and each complete split
/// set once (under its first part, with the total size). Incomplete sets
/// are left out. Each gets its fit on `hw`.
fn repo_file_views(
    resolved: &gardenwall_models::hub::ResolvedRepo,
    hw: &crate::tuning::Hardware,
) -> Vec<RepoFileView> {
    use gardenwall_models::split::parse_split_name;
    let listed: Vec<(String, u64, u32)> = resolved
        .files
        .iter()
        .filter_map(|f| match parse_split_name(&f.file_name) {
            None => Some((f.path.clone(), f.size, 1)),
            Some((_, 1, _)) => {
                let parts = resolved.split_set(&f.path).ok().flatten()?;
                Some((
                    f.path.clone(),
                    parts.iter().map(|p| p.size).sum(),
                    parts.len() as u32,
                ))
            }
            Some(_) => None,
        })
        .collect();
    let input: Vec<crate::picker::card::FileIn<'_>> = listed
        .iter()
        .map(|(path, size, _)| crate::picker::card::FileIn { path, size: *size })
        .collect();
    let facts = crate::picker::card::facts(&input, hw);
    listed
        .into_iter()
        .zip(facts)
        .map(|((path, size, parts), facts)| RepoFileView {
            path,
            size,
            parts,
            facts,
        })
        .collect()
}

/// A repository's files with their fit on this computer, or on
/// `vram_gib` of GPU memory when the user plans for another figure.
#[tauri::command]
pub async fn list_repo_files(
    state: State<'_, AppState>,
    repo: String,
    vram_gib: Option<u32>,
) -> CmdResult<RepoView> {
    let repo = RepoId::parse(repo.trim()).map_err(msg)?;
    let client = hub(&state)?;
    let (hw, hardware) = crate::picker::hardware(vram_gib).await?;
    let resolved = client.resolve(&repo, "main").await.map_err(msg)?;
    let row = crate::picker::ranking::repo_row(&resolved);
    Ok(RepoView {
        known_publisher: row.known_publisher,
        warnings: row.warnings,
        repo: resolved.repo.to_string(),
        commit: resolved.commit.as_str().into(),
        license: resolved.license.clone(),
        gated: resolved.gated,
        files: repo_file_views(&resolved, &hw),
        skipped: resolved.skipped,
        hardware,
        fit_note: crate::picker::card::BEFORE_DOWNLOAD,
        quant_about: {
            let t = crate::picker::quant::table();
            format!(
                "Bits per weight and quality words: Gardenwall's quantization table, {}.",
                crate::picker::dates::about(t.version, &t.reviewed)
            )
        },
    })
}

/// Download `path` from a resolved repository into the library: a split
/// model as its whole set (any part names it), else the single file.
pub(crate) async fn download_resolved(
    state: &AppState,
    client: &HubClient,
    resolved: &gardenwall_models::hub::ResolvedRepo,
    path: &str,
    cancel_key: &str,
    on_progress: Channel<Progress>,
) -> CmdResult<ModelRecord> {
    // A stop left over from an earlier download of this file doesn't
    // apply, and a resumed download is no longer paused.
    if let Ok(mut cancels) = state.download_cancels.lock() {
        cancels.remove(cancel_key);
    }
    if let Ok(mut paused) = state.paused_downloads.lock() {
        paused.remove(cancel_key);
    }
    // Listed while it runs, for the close prompt; taken off however this
    // returns.
    struct Running<'a>(&'a AppState, String);
    impl Drop for Running<'_> {
        fn drop(&mut self) {
            if let Ok(mut d) = self.0.active_downloads.lock() {
                d.remove(&self.1);
            }
        }
    }
    if let Ok(mut d) = state.active_downloads.lock() {
        d.insert(cancel_key.to_string());
    }
    let _running = Running(state, cancel_key.to_string());
    let mut report = progress_reporter(on_progress);
    let reporter = |done, total| {
        let stopped = state
            .download_cancels
            .lock()
            .map(|c| c.contains_key(cancel_key))
            .unwrap_or(false);
        // Offline mode or lockdown turned on: stop here, kept for resuming
        // (GW-019).
        !stopped && !state.offline_now() && report(done, total)
    };
    let (outcome, leftovers) = if let Some(parts) = resolved.split_set(path).map_err(msg)? {
        let leftovers = gardenwall_models::hub::partial_paths(&parts, state.library.dir(), true);
        let outcome = client
            .download_split(&parts, state.library.dir(), reporter)
            .await
            .map(Quarantined::Split);
        (outcome, leftovers)
    } else {
        let file = resolved
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or("file not found or not verifiable")?;
        let mut leftovers = gardenwall_models::hub::partial_paths(
            std::slice::from_ref(file),
            state.library.dir(),
            false,
        );
        #[cfg(windows)]
        if let Some(part) = leftovers.first()
            && let Ok(staging) = crate::download_helper::staging_for(part)
        {
            leftovers.push(staging);
        }
        let outcome = client
            .download(file, state.library.dir(), reporter)
            .await
            .map(Quarantined::File);
        (outcome, leftovers)
    };
    let stop = state
        .download_cancels
        .lock()
        .ok()
        .and_then(|mut c| c.remove(cancel_key));
    match outcome {
        Ok(Quarantined::File(model)) => state.library.admit(&model).map_err(msg),
        Ok(Quarantined::Split(model)) => state.library.admit_split(&model).map_err(msg),
        Err(gardenwall_models::hub::HubError::Cancelled) => match stop {
            Some(DownloadStop::Discard) => {
                discard_leftovers(&leftovers);
                Err("Download cancelled. Its partial file was deleted.".into())
            }
            _ => {
                if let Ok(mut paused) = state.paused_downloads.lock() {
                    paused
                        .entry(cancel_key.to_string())
                        .or_default()
                        .extend(leftovers);
                }
                Err("Download paused. What was downloaded is kept: resume to continue.".into())
            }
        },
        Err(other) => Err(msg(other)),
    }
}

enum Quarantined {
    File(gardenwall_models::intake::QuarantinedModel),
    Split(gardenwall_models::hub::QuarantinedSplit),
}

/// How the user stopped a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStop {
    /// Keep what was downloaded, for resuming.
    Pause,
    /// Delete what was downloaded.
    Discard,
}

/// Delete a cancelled download's partial files, resume state and staging
/// folders. The paths come from [`gardenwall_models::hub::partial_paths`]
/// (named by digest inside the library), never from the UI.
fn discard_leftovers(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
    }
}

fn download_key(path: &str) -> CmdResult<()> {
    if path.is_empty() || path.len() > 1024 {
        return Err("invalid file path".into());
    }
    Ok(())
}

/// Pause a running model download (`path` is the file's path in its
/// repository, as passed to the download; for a starter entry, its model
/// file, which also covers its projector). What was downloaded is kept:
/// downloading the same file again resumes.
#[tauri::command]
pub fn pause_download(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    download_key(&path)?;
    state
        .download_cancels
        .lock()
        .map_err(|_| "downloads are unavailable".to_string())?
        .insert(path, DownloadStop::Pause);
    Ok(())
}

/// Cancel a model download for good (`path` as for [`pause_download`]):
/// a running one stops and its partial file is deleted; a paused one's
/// partial file is deleted now.
#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    download_key(&path)?;
    let paused = state
        .paused_downloads
        .lock()
        .map_err(|_| "downloads are unavailable".to_string())?
        .remove(&path);
    match paused {
        Some(leftovers) => discard_leftovers(&leftovers),
        None => {
            state
                .download_cancels
                .lock()
                .map_err(|_| "downloads are unavailable".to_string())?
                .insert(path, DownloadStop::Discard);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn download_model(
    state: State<'_, AppState>,
    repo: String,
    path: String,
    on_progress: Channel<Progress>,
) -> CmdResult<ModelRecord> {
    let result = download_model_inner(&state, repo, path, on_progress).await;
    crate::diagnostics::noted(&state, "model download", result)
}

async fn download_model_inner(
    state: &State<'_, AppState>,
    repo: String,
    path: String,
    on_progress: Channel<Progress>,
) -> CmdResult<ModelRecord> {
    let client = hub(state)?;
    let repo = RepoId::parse(repo.trim()).map_err(msg)?;
    let resolved = client.resolve(&repo, "main").await.map_err(msg)?;
    download_resolved(state, &client, &resolved, &path, &path, on_progress).await
}

// ---------- inference ----------

/// The flags a runtime build accepts (ADR-017 §2.1). Read once per build
/// from its `--help`, in the sandbox, and cached beside the runtime store.
/// The cache can only narrow what is passed: unknown names are dropped, and
/// a cache missing a security flag refuses the launch.
#[cfg_attr(not(windows), allow(dead_code))]
async fn runtime_features(
    state: &AppState,
    installed: &gardenwall_runtime::store::InstalledRuntime,
    isolation: &supervisor::Isolation,
) -> CmdResult<gardenwall_inference::compat::RuntimeFeatures> {
    use gardenwall_inference::compat::RuntimeFeatures;
    let dir = state.paths.runtime_store.join("features");
    // Named after the flag table too, so flags added to Gardenwall later
    // are probed for instead of being taken as unsupported.
    let cache = dir.join(format!(
        "{}-{}.json",
        installed.build.id,
        RuntimeFeatures::table_fingerprint()
    ));
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(flags) = serde_json::from_str::<Vec<String>>(&text)
    {
        return Ok(RuntimeFeatures::from_known_flags(flags));
    }
    let features = supervisor::probe_features(&installed.server_exe, isolation)
        .await
        .map_err(|e| format!("could not check the runtime's options: {e}"))?;
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string(&features.known_flags()) {
        // A missing cache only means probing again next time.
        let _ = std::fs::write(&cache, json);
    }
    Ok(features)
}

/// Start a model on a verified runtime in the OS sandbox: the given build,
/// or the active one. Shared by loading and the runtime smoke test. Never
/// starts a model without a sandbox (ADR-012 §6).
pub(crate) async fn start_server(
    state: &AppState,
    runtime: Option<&str>,
    sha256: &str,
    context_size: Option<u32>,
    kv_cache: KvCache,
    embeddings: bool,
    try_auto_mtp: bool,
) -> CmdResult<(supervisor::RunningServer, String)> {
    let model_path = state.library.model_path(sha256).ok_or("invalid model id")?;
    // A split model's other parts, granted next to the first.
    let model_parts: Vec<std::path::PathBuf> = state
        .library
        .get(sha256)
        .map(|r| state.library.model_files(&r).into_iter().skip(1).collect())
        .unwrap_or_default();
    // A vision model's projector, if one was paired with it.
    let mmproj_path = state.library.mmproj_path(sha256);
    // Faster generation as chosen for the model; never for an embedding
    // server or a runtime smoke test, which must behave predictably.
    let speculative = if embeddings || runtime.is_some() {
        gardenwall_inference::launch::Speculative::Off
    } else {
        state.library.speculative(sha256, try_auto_mtp)?
    };
    // Automatic context never goes below the recommended minimum (capped
    // at what the model was trained for): 16K, and 32K for
    // Mixture-of-Experts models, where moving a few more experts to system
    // RAM costs far less than a context too small to use. A context the
    // user set by hand is used as it is, smaller or not.
    let min_fit_context = state.library.get(sha256).and_then(|r| {
        (!embeddings).then(|| {
            let trained = r
                .context_length
                .unwrap_or(u64::MAX)
                .min(u64::from(u32::MAX)) as u32;
            let min = if r.experts.unwrap_or(0) > 0 {
                gardenwall_inference::launch::MIN_FIT_CONTEXT_MOE
            } else {
                gardenwall_inference::launch::RECOMMENDED_MIN_CONTEXT
            };
            min.min(trained.max(512))
        })
    });
    // The expert cache likewise applies only to the model loaded for chat.
    let moe_cache_mib = match state.library.get(sha256) {
        Some(r) if !embeddings && runtime.is_none() && r.experts.unwrap_or(0) > 0 => {
            r.moe_cache_mib
        }
        _ => 0,
    };
    // Verify the runtime against its manifest before every launch.
    let store_root = state.paths.runtime_store.clone();
    let chosen = runtime
        .map(str::to_string)
        .or_else(|| state.settings().active_runtime);
    let explicit = runtime.is_some();
    let installed = tauri::async_runtime::spawn_blocking(move || {
        let store = RuntimeStore::new(store_root);
        let builds = platform_builds();
        let build = if explicit {
            builds
                .into_iter()
                .find(|b| Some(b.id) == chosen.as_deref() && store.is_installed(b))
        } else {
            crate::updates::active_runtime(&store, &builds, chosen.as_deref())
        };
        build.map(|b| store.verify(b))
    })
    .await
    .map_err(msg)?
    .ok_or("install an inference runtime first")?
    .map_err(msg)?;

    #[cfg(not(windows))]
    {
        let _ = (
            &model_path,
            &installed,
            context_size,
            kv_cache,
            embeddings,
            &mmproj_path,
            &speculative,
            moe_cache_mib,
            &model_parts,
            min_fit_context,
        );
        Err(
            "process sandboxing is not yet available on this platform, so Gardenwall will not load models here"
                .into(),
        )
    }

    #[cfg(windows)]
    {
        let relay_dir = state.paths.relay_dir.clone();
        let relay =
            tauri::async_runtime::spawn_blocking(move || crate::state::stage_relay(&relay_dir))
                .await
                .map_err(msg)?
                .map_err(msg)?;
        let isolation = supervisor::Isolation::Sandboxed {
            relay_exe: relay,
            grant_journal_dir: Some(state.paths.grants.clone()),
        };
        let features = runtime_features(state, &installed, &isolation).await?;
        let runtime_id = installed.build.id.to_string();
        let spec = LaunchSpec {
            server_exe: installed.server_exe,
            model: model_path,
            model_parts,
            min_fit_context,
            model_sha256: Some(sha256.to_string()),
            chat_template_file: None,
            context_size,
            gpu: GpuOffload::Auto,
            kv_cache: match kv_cache {
                KvCache::F16 => KvCacheType::F16,
                KvCache::Q8_0 => KvCacheType::Q8_0,
            },
            work_dir: state.paths.work.clone(),
            extra_env: vec![],
            features,
            embeddings,
            mmproj: mmproj_path,
            speculative,
            moe_cache_mib,
            // Settings → Agent → "Run sessions in parallel" (chat models only).
            parallel: {
                let agent = state.settings().agent;
                if !embeddings && agent.sessions == crate::state::SessionMode::Parallel {
                    agent.parallel_slots
                } else {
                    1
                }
            },
        };
        let server = supervisor::launch(&spec, &isolation, Timeouts::default())
            .await
            .map_err(msg)?;
        Ok((server, runtime_id))
    }
}

/// Remember a model's context size and cache precision without loading
/// it (they are used the next time it loads, however it is loaded).
#[tauri::command]
pub fn set_model_load(
    state: State<'_, AppState>,
    sha256: String,
    context_size: Option<u32>,
    kv_cache: KvCache,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    if let Some(n) = context_size
        && !(512..=1_048_576).contains(&n)
    {
        return Err("context size must be between 512 and 1048576 tokens".into());
    }
    record.load = LoadOptions {
        context_size,
        kv_cache,
    };
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// Load a model. `context_size: None` lets the runtime pick the largest
/// context that fits free GPU memory (up to the trained length). The choices
/// are remembered for the model.
#[tauri::command]
pub async fn load_model(
    state: State<'_, AppState>,
    sha256: String,
    context_size: Option<u32>,
    kv_cache: KvCache,
) -> CmdResult<LoadedView> {
    let result = load_model_inner(&state, sha256, context_size, kv_cache).await;
    crate::diagnostics::noted(&state, "model load", result)
}

async fn load_model_inner(
    state: &State<'_, AppState>,
    sha256: String,
    context_size: Option<u32>,
    kv_cache: KvCache,
) -> CmdResult<LoadedView> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    // The engine can't start without Microsoft's C++ runtime: say so,
    // rather than leave Windows to show a system error.
    let missing = gardenwall_runtime::vcredist::missing();
    if !missing.is_empty() {
        return Err(gardenwall_runtime::vcredist::message(&missing));
    }
    if record.architecture == "clip" {
        return Err(
            "this file is an image projector; pair it with a vision model on the Models page"
                .into(),
        );
    }
    if !record.template_cleared() {
        return Err("review and acknowledge this model's chat-template audit first".into());
    }
    state
        .library
        .model_path(&sha256)
        .ok_or("invalid model id")?;
    if let Some(n) = context_size
        && !(512..=1_048_576).contains(&n)
    {
        return Err("context size must be between 512 and 1048576 tokens".into());
    }
    let options = LoadOptions {
        context_size,
        kv_cache,
    };
    if record.load != options {
        record.load = options;
        state.library.save(&record).map_err(msg)?;
    }

    let mut engine = state.engine.lock().await;
    if let Some(previous) = engine.take() {
        previous.server.shutdown().await;
    }

    // Automatic faster generation: MTP when the
    // model has the layer and there is room for it with at least the
    // recommended context. A memory estimate skips loads bound to fail;
    // otherwise the model loads with MTP, and if `--fit` then had to put
    // part of it in system RAM or shrink the context below the minimum, it
    // loads again without. Checked at every load: `--fit` works from the
    // GPU memory free at that moment.
    let auto = record.speculation == crate::library::Speculation::Auto
        && record.mtp_layers.unwrap_or(0) > 0;
    let try_mtp = auto && {
        let extra = state
            .library
            .mmproj_path(&sha256)
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let rec = record.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::tuning::auto_mtp_may_fit(&rec, extra, &crate::tuning::Hardware::detect())
        })
        .await
        .unwrap_or(false)
    };
    let (mut server, mut runtime_id) =
        start_server(state, None, &sha256, context_size, kv_cache, false, try_mtp).await?;
    let mut props = server.client().props().await.map_err(msg)?;
    let mut auto_mtp = auto.then_some(if try_mtp {
        crate::state::AutoMtp::On
    } else {
        crate::state::AutoMtp::NoRoomEstimated
    });
    if try_mtp {
        let trained = record.context_length.unwrap_or(u64::MAX);
        let needed = u64::from(gardenwall_inference::launch::RECOMMENDED_MIN_CONTEXT).min(trained);
        // From the runtime's load log: a lie could only cost speed.
        let room = !server.logs().placement().partly_in_ram() && props.context_window >= needed;
        if !room {
            server.shutdown().await;
            (server, runtime_id) =
                start_server(state, None, &sha256, context_size, kv_cache, false, false).await?;
            props = server.client().props().await.map_err(msg)?;
            auto_mtp = Some(crate::state::AutoMtp::NoRoomMeasured);
        }
    }
    let vision = state.library.mmproj_path(&sha256).is_some();
    let loaded = Loaded {
        sha256,
        display_name: record.display_name,
        server,
        props,
        runtime_id,
        vision,
        auto_mtp,
    };
    let view = loaded_view(&loaded);
    *engine = Some(loaded);
    state.reset_gpu_watch();
    Ok(view)
}

#[tauri::command]
pub async fn unload_model(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(loaded) = state.engine.lock().await.take() {
        loaded.server.shutdown().await;
    }
    Ok(())
}

// ---------- generation settings ----------

#[tauri::command]
pub fn set_generation(state: State<'_, AppState>, generation: Generation) -> CmdResult<()> {
    generation.validate()?;
    state
        .update_settings(|s| s.generation = generation)
        .map(|_| ())
        .map_err(msg)
}

#[tauri::command]
pub fn set_agent_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: crate::state::AgentSettings,
) -> CmdResult<()> {
    settings.validate()?;
    state
        .update_settings(|s| s.agent = settings)
        .map(|_| ())
        .map_err(msg)?;
    crate::toolchains::warm_in_background(&app);
    crate::parent_folders::warm_in_background(&app);
    crate::sandbox_restart::offer_restarts(&state);
    Ok(())
}

/// The toolchains added for one project (ADR-022 §8). Ids are checked like
/// the agent settings; an empty list removes the project's entry.
#[tauri::command]
pub fn set_project_toolchains(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    toolchains: Vec<String>,
) -> CmdResult<()> {
    // The project must exist.
    with_store(&state, |s| s.get_project(&id))?;
    let mut agent = state.saved_settings().agent;
    let mut list: Vec<String> = Vec::new();
    for t in toolchains {
        if !list.contains(&t) {
            list.push(t);
        }
    }
    if list.is_empty() {
        agent.project_toolchains.remove(&id);
    } else {
        agent.project_toolchains.insert(id, list);
    }
    agent.validate()?;
    state
        .update_settings(|s| s.agent = agent)
        .map(|_| ())
        .map_err(msg)?;
    crate::toolchains::warm_in_background(&app);
    crate::sandbox_restart::offer_restarts(&state);
    Ok(())
}

// ---------- conversations ----------

fn with_store<T>(
    state: &AppState,
    f: impl FnOnce(&mut ConversationStore) -> conversations::Result<T>,
) -> CmdResult<T> {
    let mut store = state
        .conversations
        .lock()
        .map_err(|_| "conversation storage is unavailable".to_string())?;
    f(&mut store).map_err(msg)
}

/// Chats and agent sessions that contain `query`, for the Ctrl+K palette.
#[derive(Serialize)]
pub struct HistoryHits {
    chats: Vec<conversations::SearchHit>,
    sessions: Vec<conversations::SearchHit>,
}

/// Search the user's own saved chats and agent sessions. The text is
/// decrypted in memory for the search; nothing readable is written to
/// disk. Runs off the UI thread (a large history takes a moment).
#[tauri::command]
pub async fn search_history(state: State<'_, AppState>, query: String) -> CmdResult<HistoryHits> {
    const LIMIT: usize = 20;
    let query = query.trim().to_string();
    if !(2..=200).contains(&query.chars().count()) {
        return Ok(HistoryHits {
            chats: Vec::new(),
            sessions: Vec::new(),
        });
    }
    with_store(&state, |s| {
        Ok(HistoryHits {
            chats: s.search_chats(&query, LIMIT)?,
            sessions: s.search_transcripts(&query, LIMIT)?,
        })
    })
}

/// The unsent text kept for one composer (`owner` names it, e.g.
/// `chat:<id>`), or `None`.
#[tauri::command]
pub fn get_draft(state: State<'_, AppState>, owner: String) -> CmdResult<Option<String>> {
    with_store(&state, |s| s.draft(&owner))
}

/// Keep unsent text for one composer; empty text removes it. It is the
/// user's own typing, stored encrypted like the chats.
#[tauri::command]
pub fn set_draft(state: State<'_, AppState>, owner: String, text: String) -> CmdResult<()> {
    with_store(&state, |s| s.set_draft(&owner, &text))
}

#[tauri::command]
pub async fn list_conversations(state: State<'_, AppState>) -> CmdResult<Vec<ConversationSummary>> {
    with_store(&state, |s| s.list())
}

#[tauri::command]
pub async fn create_conversation(state: State<'_, AppState>) -> CmdResult<ConversationSummary> {
    let prompt = state.settings().generation.default_system_prompt;
    with_store(&state, |s| s.create(&prompt))
}

#[tauri::command]
pub async fn get_conversation(state: State<'_, AppState>, id: String) -> CmdResult<Conversation> {
    with_store(&state, |s| s.get(&id))
}

#[tauri::command]
pub async fn rename_conversation(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> CmdResult<()> {
    with_store(&state, |s| s.rename(&id, &title))
}

#[tauri::command]
pub async fn set_conversation_prompt(
    state: State<'_, AppState>,
    id: String,
    prompt: String,
) -> CmdResult<()> {
    with_store(&state, |s| s.set_system_prompt(&id, &prompt))
}

#[tauri::command]
pub async fn delete_conversation(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    crate::chat_web::end(&state, &id);
    with_store(&state, |s| s.delete(&id))
}

/// Remove a message and everything after it (to regenerate or edit).
#[tauri::command]
pub async fn truncate_conversation(
    state: State<'_, AppState>,
    id: String,
    from_message_id: i64,
) -> CmdResult<()> {
    with_store(&state, |s| {
        s.truncate_from(&id, from_message_id).map(|_| ())
    })
}

/// A new chat copying this one's messages before `before_message_id` (all
/// of them when `None`), linked to it; the original is not changed. Used to
/// branch from a message and to edit one without losing what came after.
#[tauri::command]
pub async fn branch_conversation(
    state: State<'_, AppState>,
    id: String,
    before_message_id: Option<i64>,
) -> CmdResult<ConversationSummary> {
    with_store(&state, |s| s.branch(&id, before_message_id))
}

/// Save a chat as a Markdown file where the user picks in the native save
/// dialog (opened here in the trusted core). The text comes from storage,
/// never from the webview. Returns the path written, or `None` if the user
/// cancelled.
#[tauri::command]
pub async fn export_conversation(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Option<String>> {
    let chat = with_store(&state, |s| s.get(&id))?;
    let markdown = crate::export::chat_markdown(&chat);
    let name = crate::export::file_name(&chat.summary.title);
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_file_name(&name)
            .add_filter("Markdown", &["md"])
            .blocking_save_file()
    })
    .await
    .map_err(msg)?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(msg)?;
    std::fs::write(&path, markdown).map_err(msg)?;
    Ok(Some(path.display().to_string()))
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    Content {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// With the Web switch on: what the reply is doing ("Searching: …").
    Step {
        text: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    /// The saved reply (also kept when generation was stopped part-way).
    message: Option<StoredMessage>,
    cancelled: bool,
    /// The message as typed, when it was stopped before the model wrote any
    /// answer: it was taken back out of the chat and goes back to the box.
    withdrawn: Option<String>,
    /// Set when this reply was much slower than earlier ones, or the GPU's
    /// memory is nearly full.
    gpu_warning: Option<crate::speed::GpuWarning>,
}

/// Send a message in a conversation and stream the reply. With `content:
/// None`, the model answers the conversation's last (user) message again.
/// The conversation history comes from storage, never from the webview.
#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    request_id: u64,
    conversation_id: String,
    content: Option<String>,
    images: Option<Vec<String>>,
    files: Option<Vec<String>>,
    on_event: Channel<ChatEvent>,
) -> CmdResult<SendResult> {
    let conversation_id = conversations::parse_id(&conversation_id).map_err(msg)?;
    // Attached images are held by the core (chosen in its file picker);
    // the webview only names them by token.
    let tokens = images.unwrap_or_default();
    if !tokens.is_empty() && content.is_none() {
        return Err("images can only be sent with a new message".into());
    }
    if let Some(text) = &content
        && (text.trim().is_empty() || text.len() > conversations::MAX_MESSAGE_BYTES)
    {
        return Err("message must be non-empty and at most 8 MiB".into());
    }

    // Check for a model before storing anything.
    let (client, context_window, model_name, model_pid, vision, model_sha, supports_tools) = {
        let engine = state.engine.lock().await;
        let loaded = engine.as_ref().ok_or("no model is loaded")?;
        (
            loaded.server.client().clone(),
            loaded.props.context_window,
            loaded.display_name.clone(),
            loaded.server.pid(),
            loaded.vision,
            loaded.sha256.clone(),
            loaded.props.supports_tools,
        )
    };
    if !tokens.is_empty() && !vision {
        return Err(
            "the loaded model can't see images; pair it with an image projector on the Models page"
                .into(),
        );
    }
    // Taken only once the message can be sent, so a refusal above keeps
    // the attachments for another try.
    let attached: Vec<Vec<u8>> = {
        let mut pending = state.pending_images.lock().map_err(msg)?;
        tokens
            .iter()
            .filter_map(|t| pending.remove(t).map(|(_, bytes)| bytes))
            .collect()
    };

    // Attached files are added to the stored message (the model keeps
    // seeing them in later turns).
    let blocks = crate::attach::take_blocks(&state, &files.unwrap_or_default())?;
    let typed = content.clone();
    let content = content.map(|mut text| {
        for block in &blocks {
            text.push_str(block);
        }
        text
    });
    let mut new_message = None;
    let conversation = with_store(&state, |s| {
        if let Some(text) = content {
            let stored = s.append(
                &conversation_id,
                NewMessage {
                    role: MessageRole::User,
                    content: text,
                    reasoning: String::new(),
                    stats: None,
                },
            )?;
            if !attached.is_empty() {
                s.attach_images(&conversation_id, stored.id, &attached)?;
            }
            new_message = Some(stored.id);
        }
        s.get(&conversation_id)
    })?;
    if conversation.messages.last().map(|m| m.role) != Some(MessageRole::User) {
        return Err("there is no message to respond to".into());
    }
    let mut messages = Vec::with_capacity(conversation.messages.len() + 1);
    // One system message (some templates allow only one, first): the chat's
    // own prompt, then the user's memories unless this chat turned them off.
    let memories = crate::memory::chat_block(&state, &conversation_id);
    let system = match (conversation.system_prompt.trim(), memories) {
        ("", None) => None,
        (prompt, None) => Some(prompt.to_string()),
        ("", Some(m)) => Some(m),
        (prompt, Some(m)) => Some(format!("{prompt}\n\n{m}")),
    };
    if let Some(system) = system {
        messages.push(ChatMessage::new(Role::System, system));
    }
    // Earlier reasoning is not sent back, as chat templates expect.
    messages.extend(conversation.messages.into_iter().map(|m| {
        let role = match m.role {
            MessageRole::User => Role::User,
            MessageRole::Assistant => Role::Assistant,
        };
        let mut message = ChatMessage::new(role, m.content);
        // Images go to the model only if it can see them.
        if vision {
            message.images = m.images.iter().map(|i| i.data_url()).collect();
        } else if !m.images.is_empty() {
            message
                .content
                .push_str("\n\n[The user attached an image, which this model can't see.]");
        }
        message
    }));

    // Document chat: quote the best passages from the attached collection
    // with the question (only in the request; the stored message is as typed).
    let collection = with_store(&state, |s| s.linked_collection(&conversation_id))?;
    let mut sources = Vec::new();
    if let (Some(collection), Some(last)) = (collection, messages.last_mut()) {
        sources =
            crate::documents::retrieve(&state, &collection, &last.content, crate::documents::TOP_K)
                .await?;
        last.content = crate::documents::with_sources(&last.content, &sources);
    }
    let source_refs: Vec<conversations::SourceRef> = sources
        .iter()
        .map(|s| conversations::SourceRef {
            file: s.file.clone(),
            part: s.part,
        })
        .collect();

    let cancel = Arc::new(AtomicBool::new(false));
    state
        .chats
        .lock()
        .map_err(msg)?
        .insert(request_id, cancel.clone());
    let request = ChatRequest {
        messages,
        sampling: state.settings().generation.sampling(),
        template_kwargs: state
            .library
            .get(&model_sha)
            .and_then(|r| r.thinking.template_kwargs()),
        ..ChatRequest::default()
    };
    // Web switch on (and the model can call tools): the reply runs through
    // the research harness, with its approvals and guardrails.
    if let Some(web) = crate::chat_web::active(&state, &conversation_id)
        && supports_tools
    {
        let result = crate::chat_web::answer(
            &state,
            &web,
            &conversation_id,
            client,
            request.messages,
            request.sampling,
            request.template_kwargs,
            cancel,
            &on_event,
        )
        .await;
        if let Ok(mut chats) = state.chats.lock() {
            chats.remove(&request_id);
        }
        let reply = result?;
        if reply.content.trim().is_empty() {
            if reply.cancelled
                && let Some(id) = new_message
            {
                with_store(&state, |s| {
                    s.truncate_from(&conversation_id, id).map(|_| ())
                })?;
                return Ok(SendResult {
                    message: None,
                    cancelled: true,
                    withdrawn: new_message.and(typed),
                    gpu_warning: None,
                });
            }
            return Err("the model gave no answer".into());
        }
        let message = with_store(&state, |s| {
            s.append(
                &conversation_id,
                NewMessage {
                    role: MessageRole::Assistant,
                    content: reply.content,
                    reasoning: reply.reasoning,
                    stats: Some(MessageStats {
                        model: Some(model_name),
                        context_window: Some(context_window),
                        ..reply.stats
                    }),
                },
            )
        })?;
        return Ok(SendResult {
            message: Some(message),
            cancelled: reply.cancelled,
            withdrawn: None,
            gpu_warning: None,
        });
    }
    let mut partial_content = String::new();
    let mut partial_reasoning = String::new();
    // A model stuck repeating itself is stopped instead of filling the
    // context; the message says why.
    let mut guards = (RepetitionGuard::new(), RepetitionGuard::new());
    let mut looped = false;
    let result = client
        .chat(&request, |event| {
            let ui = match event {
                StreamEvent::Content(text) => {
                    partial_content.push_str(&text);
                    looped |= guards.0.push(&text);
                    Some(ChatEvent::Content { text })
                }
                StreamEvent::Reasoning(text) => {
                    partial_reasoning.push_str(&text);
                    looped |= guards.1.push(&text);
                    Some(ChatEvent::Reasoning { text })
                }
                _ => None,
            };
            if let Some(ui) = ui {
                let _ = on_event.send(ui);
            }
            if cancel.load(Ordering::SeqCst) || looped {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .await;
    if let Ok(mut chats) = state.chats.lock() {
        chats.remove(&request_id);
    }

    let mut gpu_warning = None;
    let (reply, cancelled) = match result {
        Ok(r) => (
            {
                // Speculative decoding makes speed depend on the text, so
                // only memory pressure is checked when it ran.
                gpu_warning = state.check_gpu(
                    model_pid,
                    r.timings
                        .filter(|t| t.draft_n == 0)
                        .map(|t| t.predicted_per_second),
                    r.timings.map(|t| t.predicted_n),
                );
                NewMessage {
                    role: MessageRole::Assistant,
                    content: r.content,
                    reasoning: r.reasoning,
                    stats: Some(MessageStats {
                        model: Some(model_name),
                        finish_reason: r.finish_reason,
                        prompt_tokens: r.usage.map(|u| u.prompt_tokens),
                        completion_tokens: r.usage.map(|u| u.completion_tokens),
                        total_tokens: r.usage.map(|u| u.total_tokens),
                        context_window: Some(context_window),
                        prompt_per_second: r.timings.map(|t| t.prompt_per_second),
                        generation_per_second: r.timings.map(|t| t.predicted_per_second),
                        ttft_ms: r.time_to_first_token.map(|d| d.as_millis() as u64),
                        sources: source_refs.clone(),
                        draft_tokens: r.timings.map(|t| t.draft_n).filter(|n| *n > 0),
                        draft_accepted: r
                            .timings
                            .filter(|t| t.draft_n > 0)
                            .map(|t| t.draft_n_accepted),
                    }),
                }
            },
            false,
        ),
        Err(gardenwall_inference::client::ClientError::Cancelled) => (
            NewMessage {
                role: MessageRole::Assistant,
                content: partial_content,
                reasoning: partial_reasoning,
                stats: Some(MessageStats {
                    model: Some(model_name),
                    finish_reason: Some(if looped { "repetition" } else { "cancelled" }.into()),
                    context_window: Some(context_window),
                    ..MessageStats::default()
                }),
            },
            !looped,
        ),
        Err(e) => return Err(msg(e)),
    };
    // Stopped before any answer text (at most some thinking): the new
    // message is taken back out, so it isn't answered with the next one,
    // and the text goes back to the message box.
    if cancelled && reply.content.trim().is_empty() {
        if let Some(id) = new_message {
            with_store(&state, |s| {
                s.truncate_from(&conversation_id, id).map(|_| ())
            })?;
        }
        return Ok(SendResult {
            message: None,
            cancelled,
            withdrawn: new_message.and(typed),
            gpu_warning,
        });
    }
    let message = Some(with_store(&state, |s| s.append(&conversation_id, reply))?);
    Ok(SendResult {
        message,
        cancelled,
        withdrawn: None,
        gpu_warning,
    })
}

#[tauri::command]
pub fn cancel_chat(state: State<'_, AppState>, request_id: u64) -> CmdResult<()> {
    if let Some(flag) = state.chats.lock().map_err(msg)?.get(&request_id) {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

// ---------- approvals (ADR-016) ----------

fn with_approvals<T>(
    state: &AppState,
    f: impl FnOnce(&mut ApprovalStore) -> Result<T, gardenwall_store::approvals::ApprovalError>,
) -> CmdResult<T> {
    let mut store = state
        .approvals
        .lock()
        .map_err(|_| "approval store unavailable".to_string())?;
    f(&mut store).map_err(msg)
}

/// An approval request with the agent session it came from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    #[serde(flatten)]
    proposal: ProposalView,
    /// The session's name, so the dialog can say which session asks.
    session_title: String,
    /// The open agent session (its window id) the request belongs to, so the
    /// card can dock in that session (ADR-016 §4). `None` for chats with the
    /// Web switch and research sub-agents, which use the standalone card.
    agent_session: Option<String>,
}

/// Approval requests waiting for the user, from every open agent session.
#[tauri::command]
pub fn list_pending_approvals(state: State<'_, AppState>) -> CmdResult<Vec<PendingApproval>> {
    let sessions = crate::agent::approval_sessions(&state);
    let now = now_ms();
    let mut out = Vec::new();
    if sessions.is_empty() {
        return Ok(out);
    }
    // Expire overdue requests once per check, not once per session.
    with_approvals(&state, |a| a.sweep(now))?;
    let agents: Vec<_> = crate::agent::live_sessions(&state)
        .iter()
        .map(|s| (s.approval_session, s.id.clone()))
        .collect();
    for (session, title) in sessions {
        let pending = with_approvals(&state, |a| a.pending(session, now))?;
        let agent_session = agents
            .iter()
            .find(|(s, _)| *s == session)
            .map(|(_, id)| id.clone());
        out.extend(pending.into_iter().map(|proposal| PendingApproval {
            proposal,
            session_title: title.clone(),
            agent_session: agent_session.clone(),
        }));
    }
    Ok(out)
}

/// The user's answer from the trusted approval dialog. The proposal's own
/// session is looked up in the store; the webview never names a session.
#[tauri::command]
pub fn decide_approval(
    state: State<'_, AppState>,
    proposal_id: String,
    approve: bool,
    grant: Option<bool>,
) -> CmdResult<()> {
    let proposal = Id::from_hex(&proposal_id).ok_or("invalid approval id")?;
    let now = now_ms();
    let mut found = None;
    for (session, _) in crate::agent::approval_sessions(&state) {
        let pending = with_approvals(&state, |a| a.pending(session, now))?;
        if let Some(p) = pending.into_iter().find(|p| p.id == proposal_id) {
            found = Some((session, p));
            break;
        }
    }
    let (session, pending) = found.ok_or("this request is no longer waiting")?;
    // A session grant comes from the proposal's stored details, never from
    // the webview or the model (ADR-011 §2.6).
    let scope = if approve && grant == Some(true) {
        let scope = pending
            .details
            .as_deref()
            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .and_then(|d| d.get("grant").and_then(|g| g.as_str()).map(str::to_string))
            .filter(|g| crate::agent::grant_fits(pending.kind, g))
            .ok_or("this request can't be allowed for the whole session")?;
        Some(scope)
    } else {
        None
    };
    with_approvals(&state, |a| a.decide(proposal, session, approve, now_ms()))?;
    if let Some(scope) = scope
        && let Ok(mut grants) = state.egress_grants.lock()
    {
        grants.entry(session).or_default().insert(scope);
    }
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingImage {
    pub token: String,
    pub name: String,
    pub size: u64,
    /// For the user's own preview in the composer.
    pub preview: String,
}

/// Check an image (size, and type from its first bytes; it is never
/// decoded here) and hold it until it is sent, returning its token. The
/// same checks apply however the image arrived.
pub fn hold_image(state: &AppState, name: String, bytes: Vec<u8>) -> CmdResult<PendingImage> {
    use base64::Engine;
    if bytes.len() > conversations::MAX_IMAGE_BYTES {
        return Err("the image is larger than 10 MB".into());
    }
    let mime =
        conversations::sniff_image(&bytes).ok_or("this isn't a PNG, JPEG, WebP or GIF image")?;
    let token = new_token();
    let preview = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let size = bytes.len() as u64;
    let mut pending = state.pending_images.lock().map_err(msg)?;
    if pending.len() >= 16 {
        pending.clear();
    }
    pending.insert(token.clone(), (name.clone(), bytes));
    Ok(PendingImage {
        token,
        name,
        size,
        preview,
    })
}

/// Attach an image pasted into a composer (the
/// webview passes the bytes of the user's paste). The body is the raw image;
/// it gets the same checks as a picked file and is never decoded by the
/// core, only by the sandboxed runtime. The name is chosen here.
#[tauri::command]
pub fn attach_pasted_image(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<PendingImage> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("the pasted image must be sent as raw bytes".into());
    };
    if bytes.len() > conversations::MAX_IMAGE_BYTES {
        return Err("the image is larger than 10 MB".into());
    }
    hold_image(&state, "pasted image".into(), bytes.clone())
}

/// The open composer asks to receive dropped files; a newer request
/// replaces the older one. Returns a token for `unwatch_dropped_files`.
#[tauri::command]
pub fn watch_dropped_files(
    state: State<'_, AppState>,
    on_drop: tauri::ipc::Channel<crate::attach::AttachReport>,
) -> CmdResult<String> {
    let token = new_token();
    *state.drop_target.lock().map_err(msg)? = Some((token.clone(), on_drop));
    Ok(token)
}

/// The composer closed or was hidden: stop reading dropped files for it (a
/// newer composer's watch is kept).
#[tauri::command]
pub fn unwatch_dropped_files(state: State<'_, AppState>, token: String) -> CmdResult<()> {
    let mut target = state.drop_target.lock().map_err(msg)?;
    if target.as_ref().is_some_and(|(t, _)| *t == token) {
        *target = None;
    }
    Ok(())
}

/// Handle an OS file drop on the main window
/// (the core reads the paths the OS reports; the webview never names one).
/// Nothing is read unless a composer is watching.
pub fn on_files_dropped(state: &AppState, paths: &[std::path::PathBuf]) {
    let Ok(target) = state.drop_target.lock().map(|t| t.clone()) else {
        return;
    };
    let Some((_, target)) = target else {
        return;
    };
    // A closed composer's channel just fails; what was read then expires
    // with the other unsent attachments.
    let _ = target.send(crate::attach::attach_paths(state, paths));
}

pub fn new_token() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Pair a vision model with its image projector (`None` unpairs). Takes
/// effect the next time the model loads.
#[tauri::command]
pub fn set_model_projector(
    state: State<'_, AppState>,
    sha256: String,
    projector: Option<String>,
) -> CmdResult<ModelRecord> {
    pair_projector(&state, &sha256, projector)
}

pub(crate) fn pair_projector(
    state: &AppState,
    sha256: &str,
    projector: Option<String>,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(sha256).ok_or("model not found")?;
    if record.architecture == "clip" {
        return Err("choose the vision model, not the projector".into());
    }
    if let Some(p) = &projector {
        let proj = state.library.get(p).ok_or("projector not found")?;
        if proj.architecture != "clip" {
            return Err("that file is not an image projector (mmproj)".into());
        }
        // A projector made for another model fails when loading; say why now.
        if let (Ok(model), Ok(projector)) = (state.library.header(sha256), state.library.header(p))
            && let Some(why) = crate::vision::mismatch(&model, &projector)
        {
            return Err(why);
        }
    }
    record.mmproj = projector;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// The suggestion for `record`, reading the GPU and RAM figures off the
/// async runtime (they are system queries).
async fn suggestion_for(
    state: &AppState,
    record: &ModelRecord,
) -> CmdResult<crate::tuning::Suggestion> {
    let extra = state
        .library
        .mmproj_path(&record.sha256)
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len());
    let hw = tauri::async_runtime::spawn_blocking(crate::tuning::Hardware::detect)
        .await
        .map_err(msg)?;
    Ok(crate::tuning::suggest(record, extra, &hw))
}

/// Suggested settings for a model on this computer, each with its reason.
/// Read only: nothing changes until the user applies it.
#[tauri::command]
pub async fn suggest_settings(
    state: State<'_, AppState>,
    sha256: String,
) -> CmdResult<crate::tuning::Suggestion> {
    let record = state.library.get(&sha256).ok_or("model not found")?;
    if record.architecture == "clip" {
        return Err("an image projector has no settings of its own".into());
    }
    suggestion_for(&state, &record).await
}

/// Apply the suggestion (recomputed here, not taken from the window).
/// Takes effect the next time the model loads.
#[tauri::command]
pub async fn apply_suggested_settings(
    state: State<'_, AppState>,
    sha256: String,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    if record.architecture == "clip" {
        return Err("an image projector has no settings of its own".into());
    }
    let s = suggestion_for(&state, &record).await?;
    record.load = LoadOptions {
        context_size: s.context_size,
        kv_cache: s.kv_cache,
    };
    record.speculation = s.speculation;
    record.moe_cache_mib = s.moe_cache_mib;
    record.thinking = s.thinking;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// How much a model thinks before answering (for templates that support
/// thinking levels or turning thinking off). Applies to the next message.
#[tauri::command]
pub fn set_model_thinking(
    state: State<'_, AppState>,
    sha256: String,
    thinking: Thinking,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    let t = &record.template;
    match thinking {
        Thinking::Medium | Thinking::Low if !t.reasoning_levels => {
            return Err("this model's template doesn't support thinking levels".into());
        }
        Thinking::Off if !t.thinking_toggle => {
            return Err("this model's template can't turn thinking off".into());
        }
        _ => {}
    }
    record.thinking = thinking;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// Size the GPU cache for a MoE model's experts kept in system RAM (0 is
/// off). The fitting margin grows by the same amount, so the cache never
/// pushes GPU memory into the driver's paging. Takes effect on next load.
#[tauri::command]
pub fn set_model_expert_cache(
    state: State<'_, AppState>,
    sha256: String,
    mib: u32,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    if !MOE_CACHE_CHOICES.contains(&mib) {
        return Err("choose one of the offered cache sizes".into());
    }
    if mib > 0 && record.experts.unwrap_or(0) == 0 {
        return Err("only Mixture-of-Experts models have experts to cache".into());
    }
    record.moe_cache_mib = mib;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// Choose faster generation (speculative decoding) for a model: off,
/// draft-free, or a paired draft model. A draft must be a different, smaller
/// chat model with the same tokenizer; anything else is refused. Takes
/// effect the next time the model loads.
#[tauri::command]
pub async fn set_model_speculation(
    state: State<'_, AppState>,
    sha256: String,
    speculation: Speculation,
) -> CmdResult<ModelRecord> {
    let mut record = state.library.get(&sha256).ok_or("model not found")?;
    if record.architecture == "clip" {
        return Err("an image projector can't generate text".into());
    }
    if speculation == Speculation::Mtp && record.mtp_layers.unwrap_or(0) == 0 {
        return Err("this model has no built-in multi-token prediction layer".into());
    }
    if let Speculation::Draft { model } = &speculation {
        if *model == sha256 {
            return Err("a model can't draft for itself".into());
        }
        let draft = state.library.get(model).ok_or("draft model not found")?;
        if draft.architecture == "clip" {
            return Err("an image projector can't be a draft model".into());
        }
        if draft.size >= record.size {
            return Err("the draft model must be smaller than the main model".into());
        }
        // Read both headers off the async runtime: vocabularies are large.
        let (main, other) = (sha256.clone(), model.clone());
        let library = crate::library::Library::new(state.library.dir().to_path_buf());
        let (main_tok, draft_tok) = tauri::async_runtime::spawn_blocking(move || {
            Ok::<_, String>((library.tokenizer(&main)?, library.tokenizer(&other)?))
        })
        .await
        .map_err(msg)??;
        main_tok
            .check_draft(&draft_tok)
            .map_err(|why| format!("{} can't draft for this model: {why}", draft.display_name))?;
    }
    record.speculation = speculation;
    state.library.save(&record).map_err(msg)?;
    Ok(record)
}

/// The SearXNG URL for web search; `None` turns search off. Applies to
/// agent sessions started afterwards.
#[tauri::command]
pub fn set_searxng_url(state: State<'_, AppState>, url: Option<String>) -> CmdResult<()> {
    let url = match url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => Some(crate::web::validate_searxng(u)?.to_string()),
        None => None,
    };
    state
        .update_settings(|s| s.searxng_url = url)
        .map(|_| ())
        .map_err(msg)
}

/// The metadata-only security audit log, newest first.
#[tauri::command]
pub fn get_audit_log(state: State<'_, AppState>, limit: Option<u32>) -> CmdResult<Vec<AuditEntry>> {
    let limit = limit.unwrap_or(200).min(2000) as usize;
    with_approvals(&state, |a| a.audit_log(limit))
}

#[cfg(test)]
mod download_stop_tests {
    use super::discard_leftovers;

    #[test]
    fn cancel_deletes_partial_files_and_staging_folders() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("abc.part");
        let state = dir.path().join("abc.state");
        let staging = dir.path().join(".fetch-abc");
        let kept = dir.path().join("other.gguf");
        std::fs::write(&part, b"x").unwrap();
        std::fs::write(&state, b"x").unwrap();
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("download.part"), b"x").unwrap();
        std::fs::write(&kept, b"x").unwrap();
        // A path that doesn't exist (no .state.tmp) is fine.
        discard_leftovers(&[
            part.clone(),
            state.clone(),
            dir.path().join("abc.state.tmp"),
            staging.clone(),
        ]);
        assert!(!part.exists() && !state.exists() && !staging.exists());
        assert!(kept.exists());
    }
}

#[cfg(test)]
mod favourite_tests {
    use super::favourite_refusal;

    #[test]
    fn a_missing_model_cant_be_the_favourite() {
        assert_eq!(favourite_refusal(None), Some("model not found"));
    }
}

#[cfg(test)]
mod engine_tests {
    use super::engine_now;
    use crate::state::AppState;

    #[tokio::test]
    async fn status_reads_dont_wait_for_a_model_load() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        // Free: read at once (no model loaded).
        assert_eq!(engine_now(&state, |e| e.is_some()).await, Some(false));
        // Held as a load holds it: the read gives up instead of hanging.
        let held = state.engine.lock().await;
        let started = std::time::Instant::now();
        assert_eq!(engine_now(&state, |e| e.is_some()).await, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        drop(held);
    }
}
