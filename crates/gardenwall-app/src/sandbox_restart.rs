//! Restarting an agent session's sandbox without losing the conversation
//! (ADR-024 §3 A1), and the agent's toolchain requests (§3 A2).
//!
//! A sandbox's folders are fixed when it starts, so a toolchain added after
//! a session started reaches it only through a new sandbox. The session
//! keeps one [`SessionShell`]; a restart builds a new sandbox for the same
//! folders with today's settings and swaps it in. The model never names a
//! folder: it asks for an id from Gardenwall's table, and the user decides.

use std::path::PathBuf;
use std::sync::Arc;

use gardenwall_agent::agent::{BoxFuture, Shell};
use gardenwall_agent::session_shell::{SessionShell, WAITING};
use gardenwall_agent::toolchain_request::{Status, ToolchainHost};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::agent::{
    AgentView, AppRepoInit, ScratchDir, live, live_sessions, make_shell, tools_note,
};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Build a new sandbox for `folders` and swap it into `shell`. The old one
/// (and its scratch folder) goes; programs the user let keep running stop.
/// If the new sandbox can't start, the old one stays in use.
pub(crate) fn restart_shell(
    state: &AppState,
    folders: &[PathBuf],
    project: Option<&str>,
    shell: &SessionShell,
) -> Result<(), String> {
    if shell.waiting() {
        return Err(WAITING.into());
    }
    let scratch = ScratchDir::new(state);
    let (made, _hooks, off) = make_shell(state, folders, scratch.path(), project);
    let made = made.ok_or_else(|| {
        format!(
            "the new sandbox couldn't start ({}); the old one is still in use",
            off.unwrap_or_default()
        )
    })?;
    shell.replace(made, Some(Box::new(scratch)))
}

/// The app's side of `request_toolchain` for one session.
pub(crate) struct AppToolchains {
    pub app: AppHandle,
    pub folders: Vec<PathBuf>,
    pub project: Option<String>,
    pub shell: Arc<SessionShell>,
    /// The session's install card (a toolchain id).
    pub install_card: Arc<std::sync::Mutex<Option<String>>>,
    /// The toolchain ids the session's sandbox has now.
    pub in_sandbox: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ToolchainHost for AppToolchains {
    fn status(&self, id: &str) -> Status {
        let state = self.app.state::<AppState>();
        if let Some(why) = crate::toolchains::needs_parent_folders(id)
            && !crate::parent_folders::session_ready(&state, self.project.as_deref(), &self.folders)
        {
            return Status::Unusable(format!(
                "{why} Ask the user to turn it on, or to run such commands with ask_user_to_run."
            ));
        }
        let folder = self.folders.first().map(PathBuf::as_path);
        let Some(t) = crate::toolchains::for_session(&[id.to_string()], folder)
            .into_iter()
            .next()
        else {
            let name = crate::toolchains::install_info(id).map_or(id, |i| i.name);
            return Status::Missing(name.to_string());
        };
        let label = format!("{} {}", t.name, t.version);
        if let Some(problem) = t
            .folders
            .iter()
            .find_map(|f| crate::toolchains::problem_for(t.name, f))
        {
            return Status::Unusable(problem);
        }
        if self
            .in_sandbox
            .lock()
            .is_ok_and(|ids| ids.iter().any(|i| i == id))
            || crate::toolchains::present_otherwise(&state.settings(), id)
        {
            return Status::Ready(label);
        }
        let agent = state.saved_settings().agent;
        if crate::toolchains::added_for(&agent, self.project.as_deref())
            .iter()
            .any(|i| i == id)
        {
            return Status::Added(label);
        }
        Status::Installed {
            label,
            folders: t
                .folders
                .iter()
                .map(|f| gardenwall_agent::display::shown(f))
                .collect(),
            every_project: self.project.is_none(),
            stops: self.shell.background(),
        }
    }

    fn add<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if self.shell.waiting() {
                return Err(WAITING.into());
            }
            let app = self.app.clone();
            let id = id.to_string();
            let project = self.project.clone();
            let folders = self.folders.clone();
            let shell = self.shell.clone();
            let note = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
                let state = app.state::<AppState>();
                add_toolchain(&state, &id, project.as_deref())?;
                crate::toolchains::warm(&state);
                restart_shell(&state, &folders, project.as_deref(), &shell)?;
                Ok(tools_note(&state, project.as_deref(), &folders))
            })
            .await
            .map_err(|e| e.to_string())??;
            let state = self.app.state::<AppState>();
            if let Ok(mut ids) = self.in_sandbox.lock() {
                *ids = crate::toolchains::added_for(
                    &state.saved_settings().agent,
                    self.project.as_deref(),
                );
            }
            // Other sessions on the same project are offered a restart.
            offer_restarts(&state);
            Ok(note)
        })
    }

    fn show_install(&self, id: &str) {
        if let Ok(mut card) = self.install_card.lock() {
            *card = Some(id.to_string());
        }
    }
}

/// Add toolchain `id` to `project` (or, without a project, to every
/// project), as the user chose on the card.
fn add_toolchain(state: &AppState, id: &str, project: Option<&str>) -> Result<(), String> {
    let mut agent = state.saved_settings().agent;
    let list = match project {
        Some(p) => agent.project_toolchains.entry(p.to_string()).or_default(),
        None => &mut agent.toolchains,
    };
    if !list.iter().any(|t| t == id) {
        list.push(id.to_string());
    }
    agent.validate()?;
    state
        .update_settings(|s| s.agent = agent)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// After toolchains change in Settings or on a project page: each open
/// session whose sandbox no longer matches is offered a restart on its
/// banner (or the offer is withdrawn when it matches again).
pub(crate) fn offer_restarts(state: &AppState) {
    let settings = state.settings();
    let names = |ids: &[String]| -> String {
        let found = crate::toolchains::detect();
        let shown: Vec<String> = ids
            .iter()
            .map(|id| {
                found
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.name.to_string())
                    .or_else(|| crate::toolchains::install_info(id).map(|i| i.name.to_string()))
                    .unwrap_or_else(|| id.clone())
            })
            .collect();
        shown.join(", ")
    };
    for live in live_sessions(state) {
        // Research sessions have no sandbox; lockdown has no commands.
        if live.folders.is_empty() || settings.lockdown {
            continue;
        }
        let wanted = crate::toolchains::added_for(&settings.agent, live.project.as_deref());
        let have = live
            .sandbox_toolchains
            .lock()
            .map(|h| h.clone())
            .unwrap_or_default();
        let added: Vec<String> = wanted
            .iter()
            .filter(|w| !have.contains(w))
            .cloned()
            .collect();
        let removed: Vec<String> = have
            .iter()
            .filter(|h| !wanted.contains(h))
            .cloned()
            .collect();
        let offer = if !added.is_empty() {
            Some(format!(
                "{} added. Restart this session's sandbox to use {}.",
                names(&added),
                if added.len() == 1 { "it" } else { "them" }
            ))
        } else if !removed.is_empty() {
            Some(format!(
                "{} removed. Restart this session's sandbox to take {} away.",
                names(&removed),
                if removed.len() == 1 { "it" } else { "them" }
            ))
        } else {
            None
        };
        if let Ok(mut o) = live.restart_offer.lock() {
            *o = offer;
        }
    }
}

/// Restart a session's sandbox from its banner (ADR-024 §3 A1): the same
/// folders with today's toolchains, the conversation kept. A session whose
/// commands were off gets a sandbox if one can start now. Refused while the
/// agent works or a command's card waits.
#[tauri::command]
pub async fn agent_restart_sandbox(
    app: AppHandle,
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<AgentView> {
    let live = live(&state, &sid)?;
    let mut guard = live
        .session
        .try_lock()
        .map_err(|_| "wait for the agent to finish, then restart its sandbox")?;
    let session = &mut *guard;
    if session.research.is_some() || session.folders.is_empty() || state.settings().lockdown {
        return Err("this session has no sandbox for commands".into());
    }
    let folders = session.folders.clone();
    let project = session.project.clone();
    match session.shell.clone() {
        Some(sh) => {
            let app2 = app.clone();
            let (f, p) = (folders.clone(), project.clone());
            tauri::async_runtime::spawn_blocking(move || {
                restart_shell(&app2.state::<AppState>(), &f, p.as_deref(), &sh)
            })
            .await
            .map_err(|e| e.to_string())??;
        }
        None => {
            let app2 = app.clone();
            let (f, p) = (folders.clone(), project.clone());
            let (sh, hooks) = tauri::async_runtime::spawn_blocking(
                move || -> Result<(Arc<SessionShell>, Option<PathBuf>), String> {
                    let state = app2.state::<AppState>();
                    let scratch = ScratchDir::new(&state);
                    let (made, hooks, off) = make_shell(&state, &f, scratch.path(), p.as_deref());
                    let made = made.ok_or_else(|| {
                        format!("the sandbox still can't start: {}", off.unwrap_or_default())
                    })?;
                    Ok((
                        Arc::new(SessionShell::new(made, Some(Box::new(scratch)))),
                        hooks,
                    ))
                },
            )
            .await
            .map_err(|e| e.to_string())??;
            session.shell = Some(sh.clone());
            if let Ok(mut s) = live.shell.lock() {
                *s = Some(sh.clone());
            }
            session.agent.set_commands_off(None);
            session.agent.set_commit_check(hooks);
            session.agent.set_repo_init(Some(Arc::new(AppRepoInit)));
            session
                .agent
                .set_toolchain_host(Some(Arc::new(AppToolchains {
                    app: app.clone(),
                    folders: folders.clone(),
                    project: project.clone(),
                    shell: sh,
                    install_card: live.install_card.clone(),
                    in_sandbox: live.sandbox_toolchains.clone(),
                })));
        }
    }
    if let Ok(mut ids) = live.sandbox_toolchains.lock() {
        *ids = crate::toolchains::added_for(&state.saved_settings().agent, project.as_deref());
    }
    session
        .agent
        .set_tools_note(Some(tools_note(&state, project.as_deref(), &folders)));
    session
        .agent
        .set_parent_folders(crate::parent_folders::session_ready(
            &state,
            project.as_deref(),
            &folders,
        ));
    let shell = session.shell.clone();
    session
        .agent
        .set_shell(shell.as_deref().map(|s| s as &dyn Shell));
    if let Ok(mut o) = live.restart_offer.lock() {
        *o = None;
    }
    Ok(crate::agent::view_of(&live, session))
}

/// The install card's Check again: forget what detection found and look
/// again. Returns the toolchain as found ("Ruby 3.4.1"), and closes the
/// card, or `None` when it still isn't there.
#[tauri::command]
pub fn agent_install_check(state: State<'_, AppState>, sid: String) -> CmdResult<Option<String>> {
    let live = live(&state, &sid)?;
    let Some(id) = live.install_card.lock().ok().and_then(|c| c.clone()) else {
        return Ok(None);
    };
    crate::toolchains::forget_detected();
    let found = crate::toolchains::detect()
        .into_iter()
        .find(|t| t.id == id)
        .map(|t| format!("{} {}", t.name, t.version));
    if found.is_some()
        && let Ok(mut c) = live.install_card.lock()
    {
        *c = None;
    }
    Ok(found)
}

/// Close the install card.
#[tauri::command]
pub fn agent_install_dismiss(state: State<'_, AppState>, sid: String) -> CmdResult<()> {
    let live = live(&state, &sid)?;
    if let Ok(mut c) = live.install_card.lock() {
        *c = None;
    }
    Ok(())
}

/// The install card for the session view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCard {
    pub id: String,
    pub name: String,
    pub url: String,
    pub steps: String,
}

pub(crate) fn install_card(id: &str) -> Option<InstallCard> {
    crate::toolchains::install_info(id).map(|i| InstallCard {
        id: i.id.into(),
        name: i.name.into(),
        url: i.url.into(),
        steps: i.steps.into(),
    })
}
