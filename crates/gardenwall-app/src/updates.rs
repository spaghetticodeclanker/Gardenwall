//! Update center (ADR-017).
//!
//! - **Schedule** (§3.4): check each time the app opens
//!   (the default), only on request, or daily/weekly. A check only looks; it
//!   never downloads or installs. Offline mode blocks network checks.
//! - **Runtimes** (§2.2, §3.1 B): the builds this version pins, which are
//!   installed, which one is active, and installed builds it no longer pins
//!   (left by an earlier version), which can be removed. Choosing another
//!   installed build is the rollback.
//! - **Gardenwall**: the release file on GitHub Releases, read by
//!   `app_update` (§3.3, decided 2026-11-22). Offline mode skips it.
//! - **Models** (§2.3): checked one at a time on request, because each
//!   check contacts the hub.

use std::time::Duration;

use gardenwall_models::hub::RepoId;
use gardenwall_models::intake::ModelSource;
use gardenwall_runtime::store::RuntimeStore;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::state::{AppState, now_ms};

type CmdResult<T> = Result<T, String>;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// How often the scheduler wakes to see whether a check is due.
const SCHEDULER_TICK: Duration = Duration::from_secs(30 * 60);
/// Delay before the startup check, so it never slows the window.
const STARTUP_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateSchedule {
    /// Each time Gardenwall opens (recommended).
    #[default]
    OnLaunch,
    /// Only when the user asks.
    Manual,
    Daily,
    Weekly,
}

/// Whether a check is due. `at_startup` is true once, as the app opens.
pub fn due(schedule: UpdateSchedule, last_ms: Option<i64>, now: i64, at_startup: bool) -> bool {
    let interval = match schedule {
        UpdateSchedule::Manual => return false,
        UpdateSchedule::OnLaunch => return at_startup,
        UpdateSchedule::Daily => DAY_MS,
        UpdateSchedule::Weekly => 7 * DAY_MS,
    };
    // A clock that went backwards counts as due rather than never.
    last_ms.is_none_or(|last| now - last >= interval || now < last)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdateView {
    pub id: String,
    pub version: String,
    pub backend: String,
    pub download_mb: u64,
    pub installed: bool,
    /// Pinned by this version of Gardenwall (only pinned builds can run).
    pub pinned: bool,
    pub active: bool,
    /// The last smoke test on this machine, if any.
    pub smoke: Option<SmokeResult>,
    /// Published security problems in this release (fixed in a newer one).
    pub advisories: Vec<AdvisoryView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvisoryView {
    pub id: String,
    pub summary: String,
    /// The first release with the fix.
    pub fixed_in: String,
}

fn advisories(version: &str) -> Vec<AdvisoryView> {
    let Some(n) = build_number(version) else {
        return Vec::new();
    };
    gardenwall_runtime::pins::advisories_for(n)
        .into_iter()
        .map(|a| AdvisoryView {
            id: a.id.into(),
            summary: a.summary.into(),
            fixed_in: format!("b{}", a.fixed),
        })
        .collect()
}

/// Outcome of a runtime smoke test (ADR-017 §2.1): load a small model in
/// the sandbox, one completion, one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmokeResult {
    pub at_ms: i64,
    pub model: String,
    /// The model loaded and answered.
    pub completion: bool,
    /// The model made a well-formed tool call. Small models sometimes
    /// don't, so this alone is a warning, not a failure.
    pub tool_call: bool,
    pub error: Option<String>,
    pub seconds: u64,
}

fn smoke_path(state: &AppState, id: &str) -> std::path::PathBuf {
    state
        .paths
        .runtime_store
        .join("smoke")
        .join(format!("{id}.json"))
}

fn read_smoke(state: &AppState, id: &str) -> Option<SmokeResult> {
    let text = std::fs::read_to_string(smoke_path(state, id)).ok()?;
    serde_json::from_str(&text).ok()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReport {
    pub checked_at_ms: i64,
    pub offline: bool,
    pub app: crate::app_update::AppVersionView,
    pub runtimes: Vec<RuntimeUpdateView>,
    /// Updates worth the user's attention (badge count).
    pub available: usize,
    /// A security fix is available: shown as a banner until acted on.
    pub security: bool,
    /// The GPU driver is older than the vendor's latest security fix.
    /// Gardenwall never installs drivers; this only tells the user.
    pub gpu_driver: Option<crate::gpu_driver::DriverNotice>,
    /// The GPU driver was checked and has the latest fix.
    pub gpu_driver_current: Option<crate::gpu_driver::DriverCurrent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesView {
    pub schedule: UpdateSchedule,
    pub last_check_ms: Option<i64>,
    pub report: Option<UpdateReport>,
}

/// The runtime used to load models: the user's choice if it is still
/// installed and pinned, otherwise the first installed pinned build
/// (CUDA, then Vulkan, then CPU on Windows).
pub fn active_runtime(
    store: &RuntimeStore,
    builds: &[&'static gardenwall_runtime::pins::RuntimeBuild],
    chosen: Option<&str>,
) -> Option<&'static gardenwall_runtime::pins::RuntimeBuild> {
    let installed = |b: &&&gardenwall_runtime::pins::RuntimeBuild| store.is_installed(b);
    chosen
        .and_then(|id| builds.iter().filter(installed).find(|b| b.id == id))
        .or_else(|| builds.iter().find(installed))
        .copied()
}

/// llama.cpp release numbers (`b12283` → 12283), compared numerically.
fn build_number(version: &str) -> Option<u64> {
    version.strip_prefix('b')?.parse().ok()
}

/// Whether the runtime in use has a published security problem.
fn active_runtime_affected(runtimes: &[RuntimeUpdateView]) -> bool {
    runtimes
        .iter()
        .any(|r| r.active && !r.advisories.is_empty())
}

/// Build the report. Runtimes come from the pins built into this version
/// (ADR-017 §3.1 B); the Gardenwall row from what `app_update::refresh`
/// last found (call it first for a network check).
pub fn run_check(state: &AppState) -> UpdateReport {
    let settings = state.settings();
    let store = RuntimeStore::new(&state.paths.runtime_store);
    let builds = crate::commands::platform_builds();
    let active = active_runtime(&store, &builds, settings.active_runtime.as_deref()).map(|b| b.id);
    let mut runtimes: Vec<RuntimeUpdateView> = builds
        .iter()
        .map(|b| RuntimeUpdateView {
            id: b.id.into(),
            version: b.version.into(),
            backend: format!("{:?}", b.backend),
            download_mb: b.download_size().div_ceil(1_000_000),
            installed: store.is_installed(b),
            pinned: true,
            active: Some(b.id) == active,
            smoke: read_smoke(state, b.id),
            advisories: advisories(b.version),
        })
        .collect();
    for id in store.installed_ids() {
        if !runtimes.iter().any(|r| r.id == id) {
            let version: String = id.split('-').next().unwrap_or_default().into();
            runtimes.push(RuntimeUpdateView {
                advisories: advisories(&version),
                version,
                id,
                backend: String::new(),
                download_mb: 0,
                installed: true,
                pinned: false,
                active: false,
                smoke: None,
            });
        }
    }
    // A newer pinned build of the active runtime's backend that is not
    // installed is an update. (With one pinned version there is none yet.)
    let available = match runtimes.iter().find(|r| r.active) {
        Some(a) => runtimes
            .iter()
            .filter(|r| {
                r.pinned
                    && !r.installed
                    && r.backend == a.backend
                    && build_number(&r.version) > build_number(&a.version)
            })
            .count(),
        None => 0,
    };
    // The runtime in use has a published security problem that a newer
    // pinned build fixes: the banner stays until the user switches.
    let app = crate::app_update::view(state);
    let security = active_runtime_affected(&runtimes) || app.security;
    let (gpu_driver, gpu_driver_current) = crate::gpu_driver::status();
    let report = UpdateReport {
        checked_at_ms: now_ms(),
        offline: settings.offline,
        available: available
            + usize::from(gpu_driver.is_some())
            + usize::from(app.available.is_some()),
        app,
        runtimes,
        security,
        gpu_driver,
        gpu_driver_current,
    };
    let _ = state.update_settings(|s| s.last_update_check_ms = Some(report.checked_at_ms));
    if let Ok(mut r) = state.update_report.lock() {
        *r = Some(report.clone());
    }
    report
}

/// Run due checks: once at startup, then on each tick for daily and
/// weekly schedules. Started from the app's setup hook.
pub fn spawn_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        let mut at_startup = true;
        loop {
            let state = app.state::<AppState>();
            let s = state.settings();
            if due(
                s.update_schedule,
                s.last_update_check_ms,
                now_ms(),
                at_startup,
            ) {
                crate::app_update::refresh(&state).await;
                run_check(&state);
            }
            at_startup = false;
            tokio::time::sleep(SCHEDULER_TICK).await;
        }
    });
}

pub(crate) fn view(state: &AppState) -> UpdatesView {
    let s = state.settings();
    // The Gardenwall row is read live: a download changes it between checks.
    let report = state.update_report.lock().ok().and_then(|r| r.clone());
    UpdatesView {
        schedule: s.update_schedule,
        last_check_ms: s.last_update_check_ms,
        report: report.map(|mut r| {
            r.app = crate::app_update::view(state);
            r
        }),
    }
}

#[tauri::command]
pub fn get_updates(state: State<'_, AppState>) -> CmdResult<UpdatesView> {
    Ok(view(&state))
}

/// "Check for updates" (works under every schedule).
#[tauri::command]
pub async fn check_updates(state: State<'_, AppState>) -> CmdResult<UpdatesView> {
    crate::app_update::refresh(&state).await;
    run_check(&state);
    Ok(view(&state))
}

#[tauri::command]
pub fn set_update_schedule(
    state: State<'_, AppState>,
    schedule: UpdateSchedule,
) -> CmdResult<UpdatesView> {
    state
        .update_settings(|s| s.update_schedule = schedule)
        .map_err(|e| e.to_string())?;
    Ok(view(&state))
}

/// Choose the runtime used for the next model load (also the rollback).
#[tauri::command]
pub fn set_active_runtime(state: State<'_, AppState>, id: String) -> CmdResult<UpdatesView> {
    let store = RuntimeStore::new(&state.paths.runtime_store);
    let build = crate::commands::platform_builds()
        .into_iter()
        .find(|b| b.id == id)
        .ok_or("this runtime is not available in this version of Gardenwall")?;
    if !store.is_installed(build) {
        return Err("install this runtime first".into());
    }
    state
        .update_settings(|s| s.active_runtime = Some(id))
        .map_err(|e| e.to_string())?;
    run_check(&state);
    Ok(view(&state))
}

/// Remove an installed runtime that is not in use.
#[tauri::command]
pub async fn remove_runtime(state: State<'_, AppState>, id: String) -> CmdResult<UpdatesView> {
    if state
        .engine
        .lock()
        .await
        .as_ref()
        .is_some_and(|l| l.runtime_id == id)
    {
        return Err("unload the model before removing the runtime it uses".into());
    }
    let store = RuntimeStore::new(&state.paths.runtime_store);
    store.remove(&id).map_err(|e| e.to_string())?;
    // Its option caches: `<id>.json` (older) and `<id>-<table>.json`.
    let features = state.paths.runtime_store.join("features");
    if let Ok(entries) = std::fs::read_dir(&features) {
        let fingerprinted = id.len() + 1 + 16 + ".json".len();
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == format!("{id}.json")
                || (name.starts_with(&format!("{id}-")) && name.len() == fingerprinted)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_file(smoke_path(&state, &id));
    if state.settings().active_runtime.as_deref() == Some(id.as_str()) {
        let _ = state.update_settings(|s| s.active_runtime = None);
    }
    run_check(&state);
    Ok(view(&state))
}

/// Whether a library file can answer a chat on its own: an image projector
/// (`clip`) only works next to a vision model, and embedding-only models
/// (the BERT family) can't chat or call tools.
fn can_be_tested(architecture: &str) -> bool {
    !matches!(
        architecture,
        "clip"
            | "bert"
            | "nomic-bert"
            | "nomic-bert-moe"
            | "jina-bert-v2"
            | "modern-bert"
            | "t5encoder"
    )
}

/// Test a runtime with the smallest model the user has approved: start it
/// in the sandbox, ask for a short answer and for one tool call, stop it.
/// Refused while a model is loaded, so the test has the GPU to itself.
#[tauri::command]
pub async fn test_runtime(state: State<'_, AppState>, id: String) -> CmdResult<UpdatesView> {
    use gardenwall_inference::client::{ChatMessage, ChatRequest, Role, Sampling};

    // Held for the whole test, so no model can load meanwhile.
    let engine = state.engine.lock().await;
    if engine.is_some() {
        return Err("unload the model first, so the test has the GPU to itself".into());
    }
    let model = state
        .library
        .list()
        .into_iter()
        .filter(|m| m.template_cleared() && can_be_tested(&m.architecture))
        .min_by_key(|m| m.size)
        .ok_or("download or import a model first; the test uses the smallest one")?;
    let started = std::time::Instant::now();
    let mut result = SmokeResult {
        at_ms: now_ms(),
        model: model.display_name.clone(),
        completion: false,
        tool_call: false,
        error: None,
        seconds: 0,
    };
    match crate::commands::start_server(
        &state,
        Some(&id),
        &model.sha256,
        Some(4096),
        crate::library::KvCache::F16,
        false,
        false,
    )
    .await
    {
        Err(e) => result.error = Some(e),
        Ok((server, _)) => {
            let client = server.client().clone();
            let sampling = Sampling {
                temperature: Some(0.0),
                max_tokens: Some(512),
                ..Sampling::default()
            };
            let answer = ChatRequest {
                messages: vec![ChatMessage::new(
                    Role::User,
                    "Reply with the single word: ready",
                )],
                tools: vec![],
                sampling: sampling.clone(),
                template_kwargs: None,
            };
            match client
                .chat(&answer, |_| std::ops::ControlFlow::Continue(()))
                .await
            {
                Ok(r) => {
                    result.completion = !(r.content.trim().is_empty() && r.reasoning.is_empty())
                }
                Err(e) => result.error = Some(e.to_string()),
            }
            if result.completion {
                let tool = serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "get_current_time",
                        "description": "Get the current time",
                        "parameters": {"type": "object", "properties": {}}
                    }
                });
                let call = ChatRequest {
                    messages: vec![ChatMessage::new(
                        Role::User,
                        "What time is it? Use the get_current_time tool.",
                    )],
                    tools: vec![tool],
                    sampling,
                    template_kwargs: None,
                };
                if let Ok(r) = client
                    .chat(&call, |_| std::ops::ControlFlow::Continue(()))
                    .await
                {
                    result.tool_call = r
                        .tool_calls
                        .iter()
                        .any(|c| c.function.name == "get_current_time");
                }
            }
            server.shutdown().await;
        }
    }
    drop(engine);
    result.seconds = started.elapsed().as_secs();
    let path = smoke_path(&state, &id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&result) {
        let _ = std::fs::write(&path, json);
    }
    run_check(&state);
    Ok(view(&state))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum ModelUpdateView {
    /// The repo's newest commit has the same file.
    UpToDate { commit: String },
    /// A different file is at the same path in the newest commit.
    Available {
        repo: String,
        path: String,
        commit: String,
        size: u64,
    },
    /// The file is no longer in the repo (or has no verifiable hash).
    Missing,
    /// Imported from a local file: nothing to check.
    NotFromHub,
}

/// Compare a hub model with the newest commit of its repo. Uses the hub
/// client, so the host allowlist and offline mode apply.
#[tauri::command]
pub async fn check_model_update(
    state: State<'_, AppState>,
    sha256: String,
) -> CmdResult<ModelUpdateView> {
    let record = state.library.get(&sha256).ok_or("model not found")?;
    let ModelSource::Hub { repo, path, .. } = &record.source else {
        return Ok(ModelUpdateView::NotFromHub);
    };
    let client = crate::commands::hub(&state)?;
    let repo_id = RepoId::parse(repo).map_err(|e| e.to_string())?;
    let resolved = client
        .resolve(&repo_id, "main")
        .await
        .map_err(|e| e.to_string())?;
    let Some(file) = resolved.files.iter().find(|f| &f.path == path) else {
        return Ok(ModelUpdateView::Missing);
    };
    if file.sha256.eq_ignore_ascii_case(&record.sha256) {
        return Ok(ModelUpdateView::UpToDate {
            commit: resolved.commit.as_str().into(),
        });
    }
    Ok(ModelUpdateView::Available {
        repo: repo.clone(),
        path: path.clone(),
        commit: resolved.commit.as_str().into(),
        size: file.size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_decide_when_checks_are_due() {
        let now = 100 * DAY_MS;
        use UpdateSchedule::*;
        assert!(due(OnLaunch, Some(now), now, true));
        assert!(!due(OnLaunch, None, now, false));
        assert!(!due(Manual, None, now, true));
        assert!(due(Daily, None, now, false));
        assert!(!due(Daily, Some(now - DAY_MS + 1), now, true));
        assert!(due(Daily, Some(now - DAY_MS), now, false));
        assert!(!due(Weekly, Some(now - 6 * DAY_MS), now, false));
        assert!(due(Weekly, Some(now - 7 * DAY_MS), now, false));
        // A clock set back is not a reason to stop checking.
        assert!(due(Weekly, Some(now + DAY_MS), now, false));
    }

    #[test]
    fn the_runtime_test_skips_projectors_and_embedding_models() {
        assert!(can_be_tested("qwen35"));
        assert!(can_be_tested("llama"));
        assert!(!can_be_tested("clip"));
        assert!(!can_be_tested("nomic-bert"));
    }

    #[test]
    fn release_numbers_compare_numerically() {
        assert!(build_number("b12283") > build_number("b9999"));
        assert_eq!(build_number("test"), None);
    }

    #[test]
    fn an_affected_release_lists_its_advisory_and_the_fix() {
        let old = advisories("b12283");
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].id, "CVE-2026-133710");
        assert_eq!(old[0].fixed_in, "b12556");
        assert!(advisories("b12556").is_empty());
        assert!(advisories("unknown").is_empty());
    }

    #[test]
    fn the_banner_shows_only_while_the_active_runtime_is_affected() {
        let row = |version: &str, active: bool| RuntimeUpdateView {
            id: format!("{version}-windows-x64-cuda13"),
            version: version.into(),
            backend: "Cuda".into(),
            download_mb: 1,
            installed: true,
            pinned: true,
            active,
            smoke: None,
            advisories: advisories(version),
        };
        // The affected build is in use.
        assert!(active_runtime_affected(&[
            row("b12283", true),
            row("b12556", false)
        ]));
        // The fixed build is in use; the old one is only installed.
        assert!(!active_runtime_affected(&[
            row("b12283", false),
            row("b12556", true)
        ]));
        assert!(!active_runtime_affected(&[]));
        // Every pinned build of the newest release is clear, so a user on
        // the newest release never sees the banner.
        let newest = gardenwall_runtime::pins::BUILDS
            .iter()
            .map(|b| b.build_number())
            .max()
            .unwrap();
        for b in gardenwall_runtime::pins::BUILDS {
            assert_eq!(
                advisories(b.version).is_empty(),
                b.build_number() == newest,
                "{}",
                b.id
            );
        }
    }

    #[test]
    fn the_default_is_to_check_each_time_the_app_opens() {
        assert_eq!(UpdateSchedule::default(), UpdateSchedule::OnLaunch);
        let s: crate::state::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.update_schedule, UpdateSchedule::OnLaunch);
    }
}
