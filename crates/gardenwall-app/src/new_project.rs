//! The guided "New project…" flow (ADR-024 §5).
//!
//! Every step is the user's click; the core creates at most one empty
//! folder (inside a parent chosen in the native picker, with a checked
//! name), the project, its toolchains, and, if asked, a Git repository
//! (`git init -b main`, fixed arguments, ADR-022). Folders reach the core
//! only from the native picker: the webview gets a one-time token for the
//! folder it picked, never the power to name a path. Nothing new is
//! granted to the agent: the toolchains are the ones Gardenwall's table
//! knows, added exactly as on the project page.

use std::path::{Path, PathBuf};

use gardenwall_store::conversations::ProjectSummary;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// What the user can say they're building, and the toolchains each needs.
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub toolchains: &'static [&'static str],
    /// The first message, prefilled in the composer (never sent by itself).
    pub starter: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind {
        id: "rust",
        label: "Rust app",
        toolchains: &["rust"],
        starter: "Set up a new Rust application here: run `cargo init --vcs none`, build it, run it and run its tests. Then suggest next steps.",
    },
    Kind {
        id: "python",
        label: "Python script or app",
        toolchains: &["python"],
        starter: "Set up a new Python project here: a `pyproject.toml`, a package with a small entry point, and a test runnable with `python -m unittest`. Run it and its tests. Then suggest next steps.",
    },
    Kind {
        id: "node",
        label: "JavaScript or TypeScript (Node.js)",
        toolchains: &["node"],
        starter: "Set up a new Node.js project here: a `package.json` and a small entry point using `node:test` for tests. Run it and its tests. If I want TypeScript, tell me which packages to fetch with `npm install` first. Then suggest next steps.",
    },
    Kind {
        id: "java",
        label: "Java",
        toolchains: &["java"],
        starter: "Set up a new Java project here: a small class with a main method, compiled and run with `javac` and `java`. Then suggest next steps, including whether a build tool like Gradle or Maven would help.",
    },
    Kind {
        id: "kotlin",
        label: "Kotlin (Gradle)",
        toolchains: &["java"],
        starter: "Help me start a Kotlin app built with Gradle. Creating the Gradle wrapper needs Gradle and the internet, so first tell me the exact gradle init command to run myself in this folder (a Kotlin application with the Kotlin build script and JUnit tests). Then fetch the dependencies, build it and run its tests offline. Then suggest next steps.",
    },
    Kind {
        id: "dotnet",
        label: "C# / .NET",
        toolchains: &["dotnet"],
        starter: "Set up a new .NET console application here with `dotnet new console`, build it and run it. Then suggest next steps.",
    },
    Kind {
        id: "go",
        label: "Go",
        toolchains: &["go"],
        starter: "Set up a new Go module here: `go mod init`, a small main package and a test. Run it and its tests. Then suggest next steps.",
    },
    Kind {
        id: "cpp",
        label: "C or C++",
        toolchains: &["cpp"],
        starter: "Set up a new C++ project here: a small program built with `cl` (Visual Studio's compiler) and a script that builds it. Build it and run it. Then suggest next steps.",
    },
    Kind {
        id: "pyweb",
        label: "Python web app (Flask or Django)",
        toolchains: &["python"],
        starter: "Help me start a Python web app here. Ask me whether I want Flask or Django, then tell me the exact command to fetch it myself (your commands have no internet), set up the app and its tests, and run the tests. You can't open a browser or reach a running server, so tell me how to start it and look at it myself. Then suggest next steps.",
    },
    Kind {
        id: "web",
        label: "Website (HTML, CSS, JavaScript)",
        toolchains: &[],
        starter: "Set up a simple website here: index.html, style.css and script.js, with no build step. Tell me how to open it in my browser. Then suggest next steps.",
    },
    Kind {
        id: "php",
        label: "PHP",
        toolchains: &["php"],
        starter: "Set up a new PHP project here: a small script and a test I can run with `php`. Run them. Then suggest next steps, including whether Composer would help.",
    },
    Kind {
        id: "other",
        label: "Something else",
        toolchains: &[],
        starter: "This is a new project. Look at what's here and ask me what I want to build.",
    },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

/// Why `name` can't be a new folder's name, or `None`. One plain name: no
/// path separators or `..`, none of the characters Windows refuses, no
/// device names (`CON`, `NUL`, `COM1`, … with or without an extension), no
/// trailing dot or space, at most 100 characters.
pub fn name_problem(name: &str) -> Option<String> {
    let bad = |why: &str| {
        Some(format!(
            "\"{}\" can't be a folder name: {why}",
            name.chars().take(60).collect::<String>()
        ))
    };
    if name.trim().is_empty() {
        return Some("give the new folder a name".into());
    }
    if name.chars().count() > 100 {
        return bad("it's longer than 100 characters");
    }
    if name == "." || name == ".." || name.contains("..") {
        return bad("it can't contain \"..\"");
    }
    if name
        .chars()
        .any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
    {
        return bad("it can't contain < > : \" / \\ | ? * or control characters");
    }
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        return bad("it can't start with a space or end with a dot or space");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.chars().count() == 4
        && stem
            .chars()
            .nth(3)
            .is_some_and(|c| c.is_ascii_digit() || "¹²³".contains(c)));
    if device {
        return bad("Windows reserves that name for a device");
    }
    None
}

/// Create the new, empty folder `name` inside `parent` (which must exist).
/// An existing empty folder of that name is used as is; anything else that
/// exists is refused, and nothing is created anywhere else.
pub fn new_folder(parent: &Path, name: &str) -> CmdResult<PathBuf> {
    if let Some(p) = name_problem(name) {
        return Err(p);
    }
    if !parent.is_dir() {
        return Err(format!("{} isn't a folder", parent.display()));
    }
    let target = parent.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            let empty = std::fs::read_dir(&target)
                .map_err(|e| e.to_string())?
                .next()
                .is_none();
            if !empty {
                return Err(format!(
                    "{} already exists and isn't empty. Choose another name, or use it as an existing folder.",
                    target.display()
                ));
            }
        }
        Ok(_) => {
            return Err(format!("{} already exists", target.display()));
        }
        Err(_) => std::fs::create_dir(&target).map_err(|e| format!("{}: {e}", target.display()))?,
    }
    Ok(target)
}

/// What a folder already has, from file names only (nothing is run or
/// read).
pub fn detected(folder: &Path) -> (bool, bool, Vec<&'static str>) {
    let has_git = folder.join(".git").exists();
    let found = stacks(folder).into_iter().map(|s| s.label).collect();
    let empty = std::fs::read_dir(folder).is_ok_and(|mut d| d.next().is_none());
    (has_git, empty, found)
}

/// A kind of project recognised by a file name, and the toolchain it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stack {
    pub label: &'static str,
    /// A [`crate::toolchains::KNOWN`] id.
    pub toolchain: &'static str,
}

/// Files that say what a folder is (names only; nothing is read).
pub const MARKERS: &[(&str, Stack)] = &[
    (
        "Cargo.toml",
        Stack {
            label: "Rust (Cargo.toml)",
            toolchain: "rust",
        },
    ),
    (
        "package.json",
        Stack {
            label: "Node.js (package.json)",
            toolchain: "node",
        },
    ),
    (
        "pyproject.toml",
        Stack {
            label: "Python (pyproject.toml)",
            toolchain: "python",
        },
    ),
    (
        "requirements.txt",
        Stack {
            label: "Python (requirements.txt)",
            toolchain: "python",
        },
    ),
    (
        "go.mod",
        Stack {
            label: "Go (go.mod)",
            toolchain: "go",
        },
    ),
    (
        "pom.xml",
        Stack {
            label: "Java (pom.xml)",
            toolchain: "java",
        },
    ),
    (
        "build.gradle",
        Stack {
            label: "Java (build.gradle)",
            toolchain: "java",
        },
    ),
    (
        "build.gradle.kts",
        Stack {
            label: "Java (build.gradle.kts)",
            toolchain: "java",
        },
    ),
    (
        "Gemfile",
        Stack {
            label: "Ruby (Gemfile)",
            toolchain: "ruby",
        },
    ),
    (
        "composer.json",
        Stack {
            label: "PHP (composer.json)",
            toolchain: "php",
        },
    ),
    (
        "CMakeLists.txt",
        Stack {
            label: "C/C++ (CMakeLists.txt)",
            toolchain: "cpp",
        },
    ),
];

const DOTNET: Stack = Stack {
    label: "C# / .NET (.csproj or .sln)",
    toolchain: "dotnet",
};

/// What `folder` is, by its file names.
pub fn stacks(folder: &Path) -> Vec<Stack> {
    let mut found: Vec<Stack> = MARKERS
        .iter()
        .filter(|(f, _)| folder.join(f).is_file())
        .map(|(_, s)| *s)
        .collect();
    let dotnet = std::fs::read_dir(folder).is_ok_and(|d| {
        d.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.ends_with(".csproj") || n.ends_with(".sln") || n.ends_with(".slnx")
        })
    });
    if dotnet {
        found.push(DOTNET);
    }
    found
}

// ---- commands ---------------------------------------------------------------

/// A folder picked in the native picker, kept by the core.
pub struct Picked {
    token: String,
    path: PathBuf,
    /// A parent for a new folder (not used as the project folder itself).
    parent: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedView {
    pub token: String,
    pub folder: String,
    pub has_git: bool,
    pub empty: bool,
    pub detected: Vec<String>,
}

/// Pick a folder in the native picker: the project folder itself
/// (`parent: false`) or where a new folder goes (`parent: true`). `None`
/// when cancelled.
#[tauri::command]
pub async fn new_project_pick(
    app: AppHandle,
    state: State<'_, AppState>,
    parent: bool,
) -> CmdResult<Option<PickedView>> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    // A project folder passes the same breadth rules as any granted folder
    // (no drive roots, home or system folders); a parent may be broad.
    let path = if parent {
        std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        crate::projects::checked(&path)?
    };
    let (has_git, empty, found) = detected(&path);
    let token = crate::commands::new_token();
    let shown = gardenwall_agent::display::shown(&path);
    if let Ok(mut p) = state.new_project_pick.lock() {
        *p = Some(Picked {
            token: token.clone(),
            path,
            parent,
        });
    }
    Ok(Some(PickedView {
        token,
        folder: shown,
        has_git,
        empty,
        detected: found.into_iter().map(String::from).collect(),
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainStatus {
    pub id: String,
    pub name: String,
    /// "1.98.1" when found on this PC.
    pub version: Option<String>,
    /// Why the sandbox can't use it, when found but unusable.
    pub problem: Option<String>,
    pub install: Option<crate::toolchains::Install>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindView {
    pub id: String,
    pub label: String,
    pub toolchains: Vec<ToolchainStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProjectOptions {
    pub kinds: Vec<KindView>,
    /// Git for Windows is installed, so Gardenwall can create a repository.
    pub git: bool,
}

/// The kinds of project and their toolchains' status on this PC.
/// `recheck` forgets what detection found (the dialog's Check again).
#[tauri::command]
pub fn new_project_options(recheck: bool) -> NewProjectOptions {
    if recheck {
        crate::toolchains::forget_detected();
    }
    let found = crate::toolchains::detect();
    NewProjectOptions {
        kinds: KINDS
            .iter()
            .map(|k| KindView {
                id: k.id.into(),
                label: k.label.into(),
                toolchains: k
                    .toolchains
                    .iter()
                    .map(|id| {
                        let t = found.iter().find(|t| t.id == *id);
                        let info = crate::toolchains::install_info(id);
                        ToolchainStatus {
                            id: (*id).into(),
                            name: t
                                .map(|t| t.name)
                                .or(info.map(|i| i.name))
                                .unwrap_or(id)
                                .into(),
                            version: t.map(|t| t.version.clone()),
                            problem: t.and_then(|t| {
                                t.folders
                                    .iter()
                                    .find_map(|f| crate::toolchains::problem_for(t.name, f))
                            }),
                            install: info.cloned(),
                        }
                    })
                    .collect(),
            })
            .collect(),
        git: gardenwall_agent::git::find_git_for_windows().is_some(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProjectRequest {
    /// The token of the folder picked with `new_project_pick`.
    pub pick: String,
    /// Set when the picked folder is the parent of a new folder.
    pub new_folder_name: Option<String>,
    pub kind: String,
    pub toolchains: Vec<String>,
    pub init_git: bool,
    /// Add Gardenwall's secret-file patterns to `.gitignore` once there is
    /// a repository (found in testing: offered in the dialog, so nobody
    /// has to find it on the project page later).
    #[serde(default)]
    pub ignore_secrets: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProjectResult {
    pub project: ProjectSummary,
    /// The first message, for the composer (not sent).
    pub starter: String,
    /// What didn't work out (the repository, say), for the user.
    pub notes: Vec<String>,
}

/// Create the project as the dialog's summary says (ADR-024 §5).
#[tauri::command]
pub async fn create_guided_project(
    app: AppHandle,
    state: State<'_, AppState>,
    request: NewProjectRequest,
) -> CmdResult<NewProjectResult> {
    let kind = kind(&request.kind).ok_or("unknown kind of project")?;
    let mut toolchains: Vec<String> = Vec::new();
    for t in &request.toolchains {
        if !crate::toolchains::KNOWN.contains(&t.as_str()) {
            return Err(format!("unknown toolchain {t:?}"));
        }
        if !toolchains.contains(t) {
            toolchains.push(t.clone());
        }
    }
    let picked = state
        .new_project_pick
        .lock()
        .map_err(|_| "state unavailable")?
        .take()
        .filter(|p| p.token == request.pick)
        .ok_or("choose the folder again")?;
    let folder = match (&request.new_folder_name, picked.parent) {
        (Some(name), true) => new_folder(&picked.path, name.trim())?,
        (None, false) => picked.path,
        _ => return Err("choose the folder again".into()),
    };
    let folder = crate::projects::checked(&folder)?;
    let project = crate::projects::create_for(&state, std::slice::from_ref(&folder))?;
    if !toolchains.is_empty() {
        let mut agent = state.saved_settings().agent;
        agent
            .project_toolchains
            .insert(project.id.clone(), toolchains);
        agent.validate()?;
        state
            .update_settings(|s| s.agent = agent)
            .map_err(|e| e.to_string())?;
        crate::toolchains::warm_in_background(&app);
    }
    let mut notes = Vec::new();
    if request.init_git && !folder.join(".git").exists() {
        let f = folder.clone();
        match tauri::async_runtime::spawn_blocking(move || crate::agent::init_repository(&f))
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(_) => {}
            Err(e) => notes.push(format!(
                "The Git repository wasn't created ({e}). You can create it later on the project page."
            )),
        }
    }
    // Only lines the folder's ignore rules lack are added, in Gardenwall's
    // marked block; nothing happens without a repository.
    if request.ignore_secrets && folder.join(".git").exists() {
        let f = folder.clone();
        match tauri::async_runtime::spawn_blocking(move || crate::vault::add_to_gitignore(&f))
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(_) => {}
            Err(e) => notes.push(format!(
                "The secret-file list wasn't added to .gitignore ({e}). The project page offers it again."
            )),
        }
    }
    Ok(NewProjectResult {
        project,
        starter: kind.starter.into(),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_plain_names() {
        assert_eq!(name_problem("my-app"), None);
        assert_eq!(name_problem("My App 2"), None);
        for bad in [
            "", " ", "..", "a..b", "a/b", r"a\b", "C:", "x?", "con", "NUL.txt", "com1", "LPT9.md",
            "name.", "name ", " lead", "a\u{7}b",
        ] {
            assert!(name_problem(bad).is_some(), "{bad:?}");
        }
        // Superscript digits are device names too (two bytes in UTF-8).
        assert!(name_problem("COM¹").is_some());
        assert!(name_problem("lpt³.txt").is_some());
        assert!(name_problem(&"a".repeat(101)).is_some());
        assert_eq!(name_problem("console"), None, "only the device names");
    }

    #[test]
    fn a_new_folder_is_created_once_and_only_inside_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let made = new_folder(dir.path(), "app").unwrap();
        assert!(made.is_dir());
        assert_eq!(made.parent().unwrap(), dir.path());
        // An empty folder of that name is fine; a non-empty one isn't.
        assert!(new_folder(dir.path(), "app").is_ok());
        std::fs::write(made.join("x.txt"), "x").unwrap();
        assert!(
            new_folder(dir.path(), "app")
                .unwrap_err()
                .contains("isn't empty")
        );
        // A file of that name, a missing parent, and escapes are refused.
        std::fs::write(dir.path().join("f"), "x").unwrap();
        assert!(new_folder(dir.path(), "f").is_err());
        assert!(new_folder(&dir.path().join("missing"), "app").is_err());
        assert!(new_folder(dir.path(), "../out").is_err());
        assert!(!dir.path().parent().unwrap().join("out").exists());
    }

    #[test]
    fn kinds_name_known_toolchains_and_have_starters() {
        for k in KINDS {
            assert!(!k.starter.is_empty());
            for t in k.toolchains {
                assert!(crate::toolchains::KNOWN.contains(t), "{}: {t}", k.id);
            }
        }
        assert!(kind("rust").is_some_and(|k| k.starter.contains("cargo init --vcs none")));
        assert!(kind("cobol").is_none());
    }

    #[test]
    fn existing_folders_are_described_by_file_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detected(dir.path()), (false, true, vec![]));
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let (git, empty, found) = detected(dir.path());
        assert!(git && !empty);
        assert_eq!(found, vec!["Rust (Cargo.toml)"]);
    }
}
