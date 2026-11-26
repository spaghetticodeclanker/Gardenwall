//! Agent projects:
//! a project's folders are chosen once in the native picker and stored by
//! the core (sealed in `conversations.db`); new and continued sessions in
//! the project reuse them. The webview names projects by id only.
//!
//! Folders are checked again every time they are used: each must still
//! exist and pass the same breadth rules as any granted folder (no drive
//! roots, home or system folders).

use std::path::PathBuf;

use gardenwall_store::conversations::{MAX_PROJECT_FOLDERS, ProjectSummary};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn with_store<T>(
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

/// `folder` as the core grants it (canonical), or why it can't be.
pub fn checked(folder: &std::path::Path) -> CmdResult<PathBuf> {
    gardenwall_fs::Workspace::open(folder)
        .map(|w| w.root().to_path_buf())
        .map_err(|e| format!("{}: {e}", folder.display()))
}

/// Folders chosen in the native picker, checked; `None` when cancelled.
async fn pick(app: AppHandle) -> CmdResult<Option<Vec<String>>> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folders())
            .await
            .map_err(|e| e.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for p in picked.into_iter().filter_map(|p| p.into_path().ok()) {
        out.push(checked(&p)?.display().to_string());
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

fn default_name(folders: &[String]) -> String {
    folders
        .first()
        .and_then(|f| {
            PathBuf::from(f)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Project".into())
}

/// A project's folders, checked again for use. A folder that is gone or no
/// longer allowed stops the session from starting, with the reason.
pub fn project_folders(state: &AppState, id: &str) -> CmdResult<(ProjectSummary, Vec<PathBuf>)> {
    let project = with_store(state, |s| s.get_project(id))?;
    if project.folders.is_empty() {
        return Err(format!(
            "the folders of project \"{}\" can't be read; choose them again",
            project.name
        ));
    }
    let folders = project
        .folders
        .iter()
        .map(|f| checked(std::path::Path::new(f)))
        .collect::<CmdResult<Vec<_>>>()
        .map_err(|e| format!("project \"{}\": {e}. Edit its folders.", project.name))?;
    let _ = with_store(state, |s| s.touch_project(id));
    Ok((project, folders))
}

/// Create a project for folders chosen elsewhere (already checked).
pub fn create_for(state: &AppState, folders: &[PathBuf]) -> CmdResult<ProjectSummary> {
    let folders: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
    with_store(state, |s| {
        s.create_project(&default_name(&folders), &folders)
    })
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> CmdResult<Vec<ProjectSummary>> {
    with_store(&state, |s| s.list_projects())
}

/// Choose a project's folders in the native picker and create it.
#[tauri::command]
pub async fn create_project(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ProjectSummary>> {
    let Some(folders) = pick(app).await? else {
        return Ok(None);
    };
    with_store(&state, |s| {
        s.create_project(&default_name(&folders), &folders)
    })
    .map(Some)
}

#[tauri::command]
pub fn rename_project(state: State<'_, AppState>, id: String, name: String) -> CmdResult<()> {
    if name.trim().is_empty() {
        return Err("give the project a name".into());
    }
    with_store(&state, |s| s.rename_project(&id, &name))
}

/// Add folders chosen in the native picker. Open sessions keep the folders
/// they started with; new sessions get the new list.
#[tauri::command]
pub async fn add_project_folders(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<ProjectSummary> {
    let project = with_store(&state, |s| s.get_project(&id))?;
    let Some(added) = pick(app).await? else {
        return Ok(project);
    };
    let mut folders = project.folders.clone();
    for f in added {
        if !folders.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            folders.push(f);
        }
    }
    if folders.len() > MAX_PROJECT_FOLDERS {
        return Err(format!(
            "a project has at most {MAX_PROJECT_FOLDERS} folders"
        ));
    }
    with_store(&state, |s| {
        s.set_project_folders(&id, &folders)?;
        s.get_project(&id)
    })
}

/// Remove one of a project's folders (named by its position; the last one
/// can't be removed).
#[tauri::command]
pub fn remove_project_folder(
    state: State<'_, AppState>,
    id: String,
    index: usize,
) -> CmdResult<ProjectSummary> {
    let project = with_store(&state, |s| s.get_project(&id))?;
    if index >= project.folders.len() {
        return Err("that folder isn't in the project".into());
    }
    if project.folders.len() == 1 {
        return Err("a project needs at least one folder; delete the project instead".into());
    }
    let mut folders = project.folders;
    folders.remove(index);
    with_store(&state, |s| {
        s.set_project_folders(&id, &folders)?;
        s.get_project(&id)
    })
}

/// Delete a project. Its saved sessions are kept, outside any project.
#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    with_store(&state, |s| s.delete_project(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_are_named_after_their_first_folder() {
        assert_eq!(default_name(&[r"C:\work\game".into()]), "game");
        assert_eq!(default_name(&[]), "Project");
    }

    #[test]
    fn broad_folders_are_refused() {
        assert!(
            checked(std::path::Path::new(if cfg!(windows) {
                "C:\\"
            } else {
                "/"
            }))
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(checked(dir.path()).is_ok());
    }
}

/// A project folder and whether it has a Git repository (the project page's
/// Set up card, ADR-022 §2A).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSetup {
    pub folder: String,
    pub has_git: bool,
    /// Git for Windows is installed, so Gardenwall can create one.
    pub can_init: bool,
}

#[tauri::command]
pub fn project_setup(state: State<'_, AppState>, id: String) -> CmdResult<Vec<FolderSetup>> {
    let project = with_store(&state, |s| s.get_project(&id))?;
    let git = gardenwall_agent::git::find_git_for_windows().is_some();
    Ok(project
        .folders
        .iter()
        .map(|f| FolderSetup {
            folder: f.clone(),
            has_git: std::path::Path::new(f).join(".git").exists(),
            can_init: git,
        })
        .collect())
}

/// Create a Git repository in one of a project's folders (by position),
/// outside the sandbox with fixed arguments. The user asked for it on the
/// project page, so no approval card is needed.
#[tauri::command]
pub async fn init_project_repository(
    state: State<'_, AppState>,
    id: String,
    index: usize,
) -> CmdResult<String> {
    let project = with_store(&state, |s| s.get_project(&id))?;
    let folder = project
        .folders
        .get(index)
        .ok_or("that folder isn't in the project")?;
    let folder = checked(std::path::Path::new(folder))?;
    tauri::async_runtime::spawn_blocking(move || crate::agent::init_repository(&folder))
        .await
        .map_err(|e| e.to_string())?
}
