//! The model picker: "Which model should I use?" (ROADMAP, Planned features
//! §6).
//!
//! Three sources, each usable alone: the starter list shipped with the app,
//! the hub ranking, and a research session. Rules for all of them:
//! - a recommendation never downloads anything: the user picks, and the
//!   file goes through the existing verified download;
//! - nothing says a model is safe or good, only what fits, what is popular
//!   and what a choice costs;
//! - hardware facts stay on this computer, except in the research question,
//!   whose exact words the user sees and can remove;
//! - lists are dated data files (`data/`), and nothing in code names a
//!   model, so the picker keeps working as models change.

pub mod card;
pub mod dates;
pub mod quant;
pub mod ranking;
pub mod research;
pub mod starter;

use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;

use crate::commands::{Progress, download_resolved, hub, pair_projector};
use crate::library::ModelRecord;
use crate::state::AppState;
use crate::tuning::Hardware;

type CmdResult<T> = Result<T, String>;

/// This computer (read-only, off the async runtime), or the same with the
/// GPU memory figure the user plans for, and how to describe it.
pub async fn hardware(vram_gib: Option<u32>) -> CmdResult<(Hardware, String)> {
    if vram_gib.is_some_and(|g| !(1..=1024).contains(&g)) {
        return Err("plan for between 1 and 1024 GiB of GPU memory".into());
    }
    let hw = tauri::async_runtime::spawn_blocking(Hardware::detect)
        .await
        .map_err(|e| e.to_string())?
        .planned(vram_gib);
    let mut text = hw.describe();
    if let Some(g) = vram_gib {
        text = format!(
            "Planned for {g} GiB of GPU memory (loading still measures the real memory) · {text}"
        );
    }
    Ok((hw, text))
}

/// The starter list with each entry's fit and trade-off card here.
#[tauri::command]
pub async fn starter_models(
    state: State<'_, AppState>,
    vram_gib: Option<u32>,
) -> CmdResult<starter::StarterView> {
    let (hw, text) = hardware(vram_gib).await?;
    Ok(starter::view(&hw, text, &|sha| {
        state.library.get(sha).is_some()
    }))
}

/// "Help me choose": three answers in, one or two entries and reasons out.
#[tauri::command]
pub async fn choose_model(
    answers: starter::Answers,
    vram_gib: Option<u32>,
) -> CmdResult<starter::Chosen> {
    let (hw, _) = hardware(vram_gib).await?;
    Ok(starter::choose(answers, &hw))
}

/// Download a starter entry exactly as reviewed: its pinned commit, and a
/// file whose SHA-256 and size match the list. A vision entry's projector
/// comes too, and is paired with the model. The usual checks (hash, GGUF
/// validation, template audit) still run.
#[tauri::command]
pub async fn download_starter(
    state: State<'_, AppState>,
    id: String,
    on_progress: Channel<Progress>,
) -> CmdResult<ModelRecord> {
    let entry = starter::list()
        .entry(&id)
        .ok_or("not on the starter list")?
        .clone();
    let client = hub(&state)?;
    let repo = gardenwall_models::hub::RepoId::parse(&entry.repo).map_err(|e| e.to_string())?;
    let resolved = client
        .resolve(&repo, &entry.commit)
        .await
        .map_err(|e| e.to_string())?;
    let changed = || {
        format!(
            "The hub's copy of {} no longer matches the reviewed file, so it wasn't downloaded. The ranking and search still offer the repository's current files.",
            entry.name
        )
    };
    if resolved.commit.as_str() != entry.commit {
        return Err(changed());
    }
    let matches = |path: &str, sha: &str, size: u64| {
        resolved
            .files
            .iter()
            .any(|f| f.path == path && f.sha256 == sha && f.size == size)
    };
    if !matches(&entry.path, &entry.sha256, entry.size)
        || entry
            .projector
            .as_ref()
            .is_some_and(|p| !matches(&p.path, &p.sha256, p.size))
    {
        return Err(changed());
    }
    let projector = match &entry.projector {
        Some(p) if state.library.get(&p.sha256).is_none() => Some(
            download_resolved(
                &state,
                &client,
                &resolved,
                &p.path,
                &entry.path,
                on_progress.clone(),
            )
            .await?
            .sha256,
        ),
        Some(p) => Some(p.sha256.clone()),
        None => None,
    };
    let model = match state.library.get(&entry.sha256) {
        Some(m) => m,
        None => {
            download_resolved(
                &state,
                &client,
                &resolved,
                &entry.path,
                &entry.path,
                on_progress,
            )
            .await?
        }
    };
    match projector {
        Some(p) if model.mmproj.as_deref() != Some(p.as_str()) => {
            pair_projector(&state, &model.sha256, Some(p))
        }
        _ => Ok(model),
    }
}

/// The hub ranking for one kind and sort (fixed parameters only).
#[tauri::command]
pub async fn hub_ranking(
    state: State<'_, AppState>,
    kind: String,
    sort: String,
) -> CmdResult<ranking::RankingView> {
    let client = hub(&state)?;
    ranking::ranking(&client, &kind, &sort).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchPrompt {
    #[serde(flatten)]
    pub question: research::ResearchQuestion,
    /// What stops a research session now, each said plainly (empty: ready).
    pub missing: Vec<Missing>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
    /// "model", "tools", "provider" or "offline": which page can fix it.
    pub what: &'static str,
    pub text: &'static str,
}

/// The prepared research question, and whether a research session can run.
#[tauri::command]
pub async fn model_research_prompt(
    state: State<'_, AppState>,
    answers: Option<starter::Answers>,
) -> CmdResult<ResearchPrompt> {
    let (hw, _) = hardware(None).await?;
    let question = research::question(answers, &hw, research::os_name(), dates::today());
    let mut missing = Vec::new();
    let settings = state.settings();
    if settings.offline {
        missing.push(Missing {
            what: "offline",
            text: if settings.lockdown {
                "Lockdown mode is on, so the web is off. Turn it off in Settings → Privacy."
            } else {
                "Offline mode is on, so the web is off. Turn it off in Settings → Privacy."
            },
        });
    } else if crate::web::configured_provider(&state).is_none() {
        missing.push(Missing {
            what: "provider",
            text: "No search provider is set up. Set one up in Settings → Web access.",
        });
    }
    match state.engine.lock().await.as_ref() {
        None => missing.push(Missing {
            what: "model",
            text: "No model is loaded. Load a model that can call tools first.",
        }),
        Some(l) if !l.props.supports_tools => missing.push(Missing {
            what: "tools",
            text: "The loaded model can't call tools, so it can't search. Load one that can.",
        }),
        Some(_) => {}
    }
    Ok(ResearchPrompt { question, missing })
}

/// Hub repositories a research session really opened (from its visited
/// list, never from the answer's text).
#[tauri::command]
pub async fn research_repositories(
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<Vec<String>> {
    let live = crate::agent::live(&state, &sid)?;
    let session = live.session.lock().await;
    let (visits, _) = session.visits.clone().unwrap_or_default().all();
    Ok(research::opened_repositories(&visits))
}
