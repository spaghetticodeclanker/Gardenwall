//! The secrets folder (ADR-011 §2.4): secret
//! files moved out of a project, to a place no session, command or MCP
//! server can be granted.
//!
//! - **Where:** `Gardenwall Secrets` in the user's home folder
//!   (`gardenwall_fs::secrets_home`), one folder per project folder. It is
//!   open to the user and the system only, and `Workspace::open` refuses it
//!   and everything in it, so it can't become a granted folder.
//! - **What moves:** only files the policy classifies as secrets, found by
//!   the same walk that hides them from commands
//!   (`gardenwall_agent::secrets::find`). The webview names them by their
//!   relative path; a name that isn't in a fresh walk is refused.
//! - **How:** each file is copied into the secrets folder (so it gets that
//!   folder's permissions, not the project's), compared, and only then
//!   removed from the project. Nothing is overwritten, in either direction.
//! - **Undo:** a moved file can be put back where it was.
//!
//! Only the user moves files, from the trusted UI. The model has no tool
//! for it and never learns where the files went.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::State;

use crate::mcp::plain;
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Files larger than this are left where they are (a key store this large
/// is not something to copy around silently).
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Secret files of one project folder.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderSecrets {
    pub folder: String,
    /// Secret files in the folder now (relative, with `/`). Commands can't
    /// touch them (Windows).
    pub in_project: Vec<String>,
    /// Files moved to the secrets folder earlier (same relative names).
    pub moved: Vec<String>,
    /// Where this folder's files are kept once moved.
    pub kept_in: String,
    /// How the user's own tools can load the moved files, when known.
    pub hints: Vec<String>,
}

/// The folder in `home` that holds what was moved out of `folder`: the
/// folder's name (letters, digits, `-`, `_`, space) and a short hash of
/// its full path, so two projects with the same name don't share one.
fn kept_in(home: &Path, folder: &Path) -> PathBuf {
    let name: String = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(40)
        .collect();
    let digest = Sha256::digest(folder.to_string_lossy().to_lowercase().as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let name = name.trim();
    home.join(if name.is_empty() {
        short
    } else {
        format!("{name}-{short}")
    })
}

/// `relative` (with `/`) as a path of plain names only.
fn plain_relative(relative: &str) -> CmdResult<PathBuf> {
    let path = PathBuf::from(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    if relative.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("{relative}: not a file in the project"));
    }
    Ok(path)
}

/// Regular files under `dir`, relative with `/`. Links are not followed.
fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let (Ok(kind), Some(name)) = (
                entry.file_type(),
                entry.file_name().to_str().map(String::from),
            ) else {
                continue;
            };
            let shown = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                dirs.push((entry.path(), shown));
            } else if kind.is_file() {
                out.push(shown);
            }
        }
    }
    out.sort();
    out
}

fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// Copy `from` to the new file `to` (never over an existing one), check the
/// copy, then remove `from`. The copy is a new file, so it takes the
/// permissions of the folder it lands in.
fn move_file(from: &Path, to: &Path) -> CmdResult<()> {
    let shown = |p: &Path| p.display().to_string();
    let meta = std::fs::symlink_metadata(from).map_err(|e| format!("{}: {e}", shown(from)))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", shown(from)));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} is too large to move", shown(from)));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", shown(parent)))?;
    }
    let copy = || -> std::io::Result<()> {
        let mut source = std::fs::File::open(from)?;
        let mut target = std::fs::File::create_new(to)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()
    };
    if let Err(e) = copy() {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            let _ = std::fs::remove_file(to);
        }
        return Err(format!("{}: {e}", shown(to)));
    }
    match same_contents(from, to) {
        Ok(true) => {}
        other => {
            let _ = std::fs::remove_file(to);
            return Err(format!(
                "{}: the copy didn't match ({other:?}); nothing was moved",
                shown(from)
            ));
        }
    }
    std::fs::remove_file(from).map_err(|e| {
        format!(
            "{} was copied to {} but couldn't be removed from the project: {e}",
            shown(from),
            shown(to)
        )
    })
}

/// Create `home` and the folder for one project folder inside it, open to
/// the user only.
fn prepare(home: &Path, kept: &Path) -> CmdResult<()> {
    let fresh = !home.exists();
    std::fs::create_dir_all(kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    // The permissions are set when the folder is made and checked again on
    // every use; a folder that can't be made private is not used.
    private(home).map_err(|e| {
        if fresh {
            let _ = std::fs::remove_dir_all(home);
        }
        format!("{} couldn't be made private: {e}", home.display())
    })
}

#[cfg(windows)]
fn private(dir: &Path) -> Result<(), String> {
    gardenwall_sandbox::windows::make_private(dir).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

/// What is in the project folder and what was moved out of it.
pub fn report(home: &Path, folder: &Path) -> FolderSecrets {
    let kept = kept_in(home, folder);
    let moved = files_under(&kept);
    let in_project: Vec<String> = gardenwall_agent::secrets::find(&[folder.to_path_buf()])
        .into_iter()
        .map(|s| s.shown)
        .collect();
    let hints = hints(folder, &kept, &moved);
    FolderSecrets {
        folder: folder.display().to_string(),
        in_project,
        moved,
        kept_in: kept.display().to_string(),
        hints,
    }
}

/// Move the named secret files of `folder` into the secrets folder.
pub fn move_out(home: &Path, folder: &Path, files: &[String]) -> CmdResult<FolderSecrets> {
    if gardenwall_fs::is_secret_store(folder) {
        return Err("that folder is itself a secrets folder".into());
    }
    let found = gardenwall_agent::secrets::find(&[folder.to_path_buf()]);
    let kept = kept_in(home, folder);
    prepare(home, &kept)?;
    for file in files {
        // Only a file the policy calls a secret, as found right now.
        let secret = found
            .iter()
            .find(|s| &s.shown == file)
            .ok_or_else(|| format!("{file}: not a secret file in this folder"))?;
        move_file(&secret.path, &kept.join(plain_relative(file)?))?;
    }
    Ok(report(home, folder))
}

/// Put the named files (or all, when `files` is empty) back in `folder`.
pub fn move_back(home: &Path, folder: &Path, files: &[String]) -> CmdResult<FolderSecrets> {
    let kept = kept_in(home, folder);
    let moved = files_under(&kept);
    let wanted: Vec<&String> = if files.is_empty() {
        moved.iter().collect()
    } else {
        files.iter().collect()
    };
    for file in wanted {
        if !moved.contains(file) {
            return Err(format!("{file}: not in the secrets folder"));
        }
        let relative = plain_relative(file)?;
        move_file(&kept.join(&relative), &folder.join(&relative))?;
    }
    remove_empty_dirs(&kept);
    Ok(report(home, folder))
}

/// Remove `dir` and its sub-folders where they are empty.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|k| k.is_dir()) {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// One line per way the user's own tools can load a moved environment
/// file, chosen from what the project contains. Advice only: nothing in
/// the project is changed.
fn hints(folder: &Path, kept: &Path, moved: &[String]) -> Vec<String> {
    let Some(env) = moved.iter().find(|f| {
        Path::new(f)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".env"))
    }) else {
        return Vec::new();
    };
    let path = kept.join(plain_relative(env).unwrap_or_default());
    let (path, dir) = (path.display(), kept.display());
    let read = |name: &str| {
        std::fs::File::open(folder.join(name)).ok().map(|f| {
            let mut text = String::new();
            let _ = f.take(1024 * 1024).read_to_string(&mut text);
            text
        })
    };
    let mut out = Vec::new();
    if let Some(package) = read("package.json") {
        if package.contains("\"next\"") {
            out.push("Next.js reads .env files only from the project folder. Start it with the values set instead, e.g. Node's `--env-file` through NODE_OPTIONS, or keep the file in the project (commands still can't read it).".to_string());
        }
        if package.contains("\"vite\"") {
            out.push(format!("Vite: set `envDir: \"{dir}\"` in vite.config."));
        }
        if package.contains("\"dotenv\"") {
            out.push(format!(
                "dotenv: `require(\"dotenv\").config({{ path: \"{path}\" }})`."
            ));
        }
        out.push(format!(
            "Node 20.6 or later: `node --env-file=\"{path}\" your-script.js`."
        ));
    }
    if [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ]
    .iter()
    .any(|f| folder.join(f).is_file())
    {
        out.push(format!(
            "Docker Compose: `docker compose --env-file \"{path}\" up`."
        ));
    }
    if ["pyproject.toml", "requirements.txt"]
        .iter()
        .any(|f| folder.join(f).is_file())
    {
        out.push(format!("python-dotenv: `load_dotenv(r\"{path}\")`."));
    }
    if out.is_empty() {
        out.push(format!(
            "Point your tool at \"{path}\", or set the values as environment variables before starting it."
        ));
    }
    out
}

fn home() -> CmdResult<PathBuf> {
    gardenwall_fs::secrets_home().ok_or_else(|| "your home folder couldn't be found".into())
}

/// The project's folder at `index`, checked as for any use.
fn folder_of(state: &AppState, id: &str, index: usize) -> CmdResult<PathBuf> {
    let (_, folders) = crate::projects::project_folders(state, id)?;
    folders
        .get(index)
        .map(|f| plain(f))
        .ok_or_else(|| "that folder isn't in the project".into())
}

/// Secret files in each of a project's folders, and what was moved out.
#[tauri::command]
pub async fn project_secrets(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<FolderSecrets>> {
    let (_, folders) = crate::projects::project_folders(&state, &id)?;
    let home = home()?;
    tauri::async_runtime::spawn_blocking(move || {
        folders.iter().map(|f| report(&home, &plain(f))).collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Move secret files of one project folder to the secrets folder.
#[tauri::command]
pub async fn move_project_secrets(
    state: State<'_, AppState>,
    id: String,
    index: usize,
    files: Vec<String>,
) -> CmdResult<FolderSecrets> {
    let folder = folder_of(&state, &id, index)?;
    let home = home()?;
    tauri::async_runtime::spawn_blocking(move || move_out(&home, &folder, &files))
        .await
        .map_err(|e| e.to_string())?
}

/// Put moved files back (all of the folder's when `files` is empty).
#[tauri::command]
pub async fn restore_project_secrets(
    state: State<'_, AppState>,
    id: String,
    index: usize,
    files: Vec<String>,
) -> CmdResult<FolderSecrets> {
    let folder = folder_of(&state, &id, index)?;
    let home = home()?;
    tauri::async_runtime::spawn_blocking(move || move_back(&home, &folder, &files))
        .await
        .map_err(|e| e.to_string())?
}

/// Secret patterns a git project folder doesn't ignore yet.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IgnoreGap {
    /// The folder's position in the project.
    pub index: usize,
    pub folder: String,
    /// The `.gitignore` lines that would be added.
    pub lines: Vec<String>,
}

/// Append the lines `folder`'s ignore rules lack to its `.gitignore`, in a
/// marked block. The file is created if there is none; a link or anything
/// that isn't a regular file is refused. Returns how many lines were added.
pub fn add_to_gitignore(folder: &Path) -> CmdResult<usize> {
    let Some(lines) =
        gardenwall_agent::secrets::gitignore_missing(folder).filter(|l| !l.is_empty())
    else {
        return Ok(0);
    };
    let file = folder.join(".gitignore");
    let existing = match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_file() => {
            std::fs::read_to_string(&file).map_err(|e| format!(".gitignore: {e}"))?
        }
        Ok(_) => return Err(".gitignore is not a regular file; nothing was changed".into()),
        Err(_) => String::new(),
    };
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&gardenwall_agent::secrets::gitignore_block(&lines));
    std::fs::write(&file, text).map_err(|e| format!(".gitignore: {e}"))?;
    Ok(lines.len())
}

/// For each git folder of a project, the secret patterns it doesn't ignore.
#[tauri::command]
pub async fn project_gitignore(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<IgnoreGap>> {
    let (_, folders) = crate::projects::project_folders(&state, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        folders
            .iter()
            .enumerate()
            .filter_map(|(index, f)| {
                let folder = plain(f);
                let lines = gardenwall_agent::secrets::gitignore_missing(&folder)?;
                (!lines.is_empty()).then(|| IgnoreGap {
                    index,
                    folder: folder.display().to_string(),
                    lines,
                })
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Add the missing lines to one project folder's `.gitignore`.
#[tauri::command]
pub async fn add_project_gitignore(
    state: State<'_, AppState>,
    id: String,
    index: usize,
) -> CmdResult<usize> {
    let folder = folder_of(&state, &id, index)?;
    tauri::async_runtime::spawn_blocking(move || add_to_gitignore(&folder))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("shop");
        std::fs::create_dir_all(folder.join("api")).unwrap();
        std::fs::write(folder.join(".env"), "KEY=one").unwrap();
        std::fs::write(folder.join("api/.env.production"), "KEY=two").unwrap();
        std::fs::write(folder.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(
            folder.join("package.json"),
            r#"{"devDependencies":{"vite":"8"}}"#,
        )
        .unwrap();
        let home = tmp.path().join("Gardenwall Secrets");
        (tmp, folder, home)
    }

    #[test]
    fn secrets_move_out_and_back() {
        let (_tmp, folder, home) = project();
        let before = report(&home, &folder);
        assert_eq!(before.in_project, [".env", "api/.env.production"]);
        assert!(before.moved.is_empty() && before.hints.is_empty());

        let after = move_out(&home, &folder, &before.in_project).unwrap();
        assert!(after.in_project.is_empty());
        assert_eq!(after.moved, [".env", "api/.env.production"]);
        assert!(!folder.join(".env").exists() && !folder.join("api/.env.production").exists());
        let kept = PathBuf::from(&after.kept_in);
        assert!(kept.starts_with(&home));
        assert_eq!(
            std::fs::read_to_string(kept.join("api/.env.production")).unwrap(),
            "KEY=two"
        );
        assert!(
            after.hints.iter().any(|h| h.contains("envDir")),
            "{:?}",
            after.hints
        );

        // A new file of the same name in the project is not overwritten.
        std::fs::write(folder.join(".env"), "KEY=new").unwrap();
        assert!(move_back(&home, &folder, &[]).is_err());
        assert_eq!(
            std::fs::read_to_string(folder.join(".env")).unwrap(),
            "KEY=new"
        );
        std::fs::remove_file(folder.join(".env")).unwrap();

        let back = move_back(&home, &folder, &[]).unwrap();
        assert_eq!(back.in_project, [".env", "api/.env.production"]);
        assert!(back.moved.is_empty());
        assert_eq!(
            std::fs::read_to_string(folder.join(".env")).unwrap(),
            "KEY=one"
        );
        assert!(!kept.exists());
    }

    #[test]
    fn only_secret_files_of_the_folder_can_be_moved() {
        let (tmp, folder, home) = project();
        std::fs::write(tmp.path().join("outside.pem"), "x").unwrap();
        for name in [
            "main.rs",
            "../outside.pem",
            "missing/.env",
            "",
            "C:/Windows/win.ini",
        ] {
            assert!(move_out(&home, &folder, &[name.into()]).is_err(), "{name}");
        }
        assert!(folder.join("main.rs").exists() && tmp.path().join("outside.pem").exists());
        // Putting back only takes names that are in the secrets folder.
        assert!(move_back(&home, &folder, &["../x".into()]).is_err());
        assert!(move_back(&home, &folder, &[".env".into()]).is_err());
    }

    #[test]
    fn gitignore_lines_are_appended_once_and_only_for_git_projects() {
        let (_tmp, folder, _home) = project();
        assert_eq!(add_to_gitignore(&folder).unwrap(), 0);
        assert!(!folder.join(".gitignore").exists());
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        std::fs::write(folder.join(".gitignore"), "target/").unwrap();
        let added = add_to_gitignore(&folder).unwrap();
        assert!(added > 40, "{added}");
        let text = std::fs::read_to_string(folder.join(".gitignore")).unwrap();
        assert!(
            text.starts_with("target/\n\n# Gardenwall: secret files."),
            "{text}"
        );
        assert!(text.contains("\n.env\n") && text.ends_with('\n'));
        // A second time changes nothing.
        assert_eq!(add_to_gitignore(&folder).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(folder.join(".gitignore")).unwrap(),
            text
        );
    }

    #[test]
    fn projects_with_the_same_name_are_kept_apart() {
        let home = Path::new("home");
        let (a, b) = (
            kept_in(home, Path::new("/a/shop")),
            kept_in(home, Path::new("/b/shop")),
        );
        assert_ne!(a, b);
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shop-")
        );
        // Odd characters are dropped from the name; the hash still tells them apart.
        let odd = kept_in(home, Path::new("/a/..%$"));
        assert_eq!(odd.parent(), Some(home));
    }

    #[cfg(windows)]
    #[test]
    fn the_secrets_folder_is_private_and_never_granted() {
        let (_tmp, folder, home) = project();
        let after = move_out(&home, &folder, &[".env".into()]).unwrap();
        let out = std::process::Command::new("icacls")
            .arg(&home)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        // Two entries (the user and the system), none inherited.
        assert!(!text.contains("(I)"), "{text}");
        assert!(text.contains("SYSTEM"), "{text}");
        assert_eq!(text.matches("(F)").count(), 2, "{text}");
        // The moved file inherits them and nothing else.
        let file = PathBuf::from(&after.kept_in).join(".env");
        let out = std::process::Command::new("icacls")
            .arg(&file)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.matches("(I)(F)").count(), 2, "{text}");
        // Neither it nor anything inside it can be granted.
        for dir in [home.clone(), PathBuf::from(&after.kept_in)] {
            assert!(matches!(
                gardenwall_fs::Workspace::open(&dir),
                Err(gardenwall_fs::FsError::SecretStore(_))
            ));
        }
    }
}
