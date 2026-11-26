//! "Is this project ready?" (docs/internal/PROJECT-SETUP-PLAN.md A1-A4, owner
//! decision 2026-11-25: in 0.1.0-rc.5).
//!
//! For each folder of a project, the core looks at file names and a few
//! small text files (size-limited, never followed through links) and
//! reports:
//! - what the folder is and its parts (`apps/web`, `backend`), with the
//!   versions each pins (names only) and whether the matching toolchain is
//!   installed and turned on for the project;
//! - whether dependencies look fetched, with Gardenwall's fixed fetch
//!   command for the user to run (Gardenwall never runs it);
//! - Git, project instructions, secret files without a `.gitignore` entry,
//!   and "Folders above the project" when a toolchain needs it;
//! - the files that run code when something else happens (an install, a
//!   build, a test run, opening the folder in an editor, a Git commit), so
//!   the user knows what a command will really execute ("runs
//!   automatically", A2).
//!
//! Nothing here runs a program or changes a file, except
//! [`save_project_instructions`], which writes a new `AGENTS.md` the user
//! edited and saved, through the workspace handle. None of it reaches the
//! model: the report is for the user (decision A2.2: the agent isn't given
//! the "runs automatically" list in this release).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gardenwall_agent::dependencies::{self, Plan};
use gardenwall_policy::paths::{PathClass, PersistenceRule, RelPath, classify};
use serde::Serialize;
use tauri::State;

use crate::new_project::{Stack, stacks};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Largest version or settings file read (`.nvmrc`, `global.json`).
const MAX_SMALL_FILE: u64 = 64 * 1024;
/// Largest manifest read for scripts (`package.json`, `composer.json`).
const MAX_MANIFEST: u64 = 1024 * 1024;
/// How deep the "runs automatically" walk goes, and how many entries it
/// looks at, so a huge tree can't stall the page.
const WALK_DEPTH: usize = 6;
const WALK_ENTRIES: usize = 20_000;
/// At most this many "runs automatically" entries are listed.
const MAX_AUTO_RUN: usize = 80;
/// Longest command excerpt shown.
const MAX_EXCERPT: usize = 160;

// ---- the report ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderReadiness {
    /// The folder's position in the project.
    pub index: usize,
    pub folder: String,
    pub git: bool,
    /// Git for Windows is installed, so Gardenwall can create a repository.
    pub can_init: bool,
    /// The project folder itself first (path ""), then its parts.
    pub parts: Vec<Part>,
    /// The toolchains the parts need.
    pub toolchains: Vec<ToolchainNeed>,
    /// `AGENTS.md` or `CLAUDE.md`, when the folder has one.
    pub instructions: Option<String>,
    /// How many secret patterns the folder's `.gitignore` doesn't cover
    /// (0 when it isn't a Git repository or covers them all).
    pub gitignore_missing: usize,
    /// Set when a toolchain here needs "Folders above the project".
    pub parent_folders: Option<ParentNeed>,
    pub auto_run: Vec<AutoRun>,
    /// Entries left out of `auto_run` (over the limit).
    pub auto_run_more: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    /// Relative to the folder, with `/` ("" for the folder itself).
    pub path: String,
    /// "Rust (Cargo.toml)".
    pub stacks: Vec<String>,
    pub pins: Vec<Pin>,
    pub fetch: Vec<Fetch>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    /// A toolchain id ("node").
    pub tool: String,
    /// "Node.js".
    pub name: String,
    /// What the project asks for ("22", "1.98", "lts/*").
    pub wants: String,
    /// The file that says so (".nvmrc").
    pub file: String,
    /// The version Gardenwall found on this PC.
    pub installed: Option<String>,
    /// Whether they agree; `None` when it can't tell (a range, a channel).
    pub matches: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fetch {
    pub id: String,
    pub label: String,
    /// Gardenwall's fixed command, one per line.
    pub command: String,
    /// Where to run it.
    pub folder: String,
    pub warning: Option<String>,
    /// The packages' usual folder is there.
    pub fetched: bool,
    /// Whether the project uses outside packages at all, when that can be
    /// told from its files (`None`: can't tell).
    pub needed: Option<bool>,
    /// Gradle needs "Folders above the project" to build in the sandbox.
    pub needs_parent_folders: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainNeed {
    pub id: String,
    pub name: String,
    /// The stacks that need it ("Node.js (package.json) in apps/web").
    pub needed_by: Vec<String>,
    /// Found on this PC.
    pub version: Option<String>,
    /// Added for this project or every project (or present another way).
    pub on: bool,
    /// Added for every project in Settings → Agent.
    pub everywhere: bool,
    /// Why the sandbox can't use the installed one.
    pub problem: Option<String>,
    pub install: Option<crate::toolchains::Install>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentNeed {
    pub needed_by: Vec<String>,
    pub on: bool,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRun {
    /// Relative to the folder, with `/`.
    pub path: String,
    /// When it runs, in a sentence.
    pub when: String,
    /// What it runs, when the file says (script names and commands, shown
    /// with invisible characters revealed).
    pub details: Vec<String>,
}

// ---- scanning a folder (no state; tested) ---------------------------------------

/// What a folder holds, from its files alone.
#[derive(Debug, Clone)]
pub struct Scan {
    pub parts: Vec<ScannedPart>,
    pub instructions: Option<String>,
    pub auto_run: Vec<AutoRun>,
    pub auto_run_more: usize,
}

#[derive(Debug, Clone)]
pub struct ScannedPart {
    pub path: String,
    pub dir: PathBuf,
    pub stacks: Vec<Stack>,
    pub pins: Vec<RawPin>,
    pub plans: Vec<Plan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPin {
    pub tool: &'static str,
    pub wants: String,
    pub file: String,
}

pub fn scan(folder: &Path) -> Scan {
    let top = stacks(folder);
    // Toolchains the folder itself covers (a Cargo or npm workspace, a .NET
    // solution, a Gradle build): its parts' copies of them aren't listed
    // again. Python's packages are per folder, so they always are.
    let covered: Vec<&str> = top
        .iter()
        .map(|s| s.toolchain)
        .filter(|t| *t != "python")
        .collect();
    let mut plans_by_part: BTreeMap<String, Vec<Plan>> =
        dependencies::project_plans(folder).into_iter().collect();
    let mut parts = vec![ScannedPart {
        path: String::new(),
        dir: folder.to_path_buf(),
        pins: pins(folder),
        plans: plans_by_part.remove("").unwrap_or_default(),
        stacks: top,
    }];
    for (rel, dir) in dependencies::part_folders(folder, |p| {
        dependencies::has_plans(p) || !stacks(p).is_empty()
    }) {
        let own: Vec<Stack> = stacks(&dir)
            .into_iter()
            .filter(|s| !covered.contains(&s.toolchain))
            .collect();
        let plans = plans_by_part.remove(&rel).unwrap_or_default();
        if own.is_empty() && plans.is_empty() {
            continue;
        }
        parts.push(ScannedPart {
            path: rel,
            pins: pins(&dir),
            plans,
            stacks: own,
            dir,
        });
    }
    let instructions = ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .find(|f| is_plain_file(&folder.join(f)))
        .map(String::from);
    let (auto_run, auto_run_more) = auto_run(folder);
    Scan {
        parts,
        instructions,
        auto_run,
        auto_run_more,
    }
}

/// A regular file (not a link, junction or folder).
fn is_plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// A small regular file's text, or `None`.
fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// A version as a name: short, and only the characters versions use.
fn version_name(raw: &str) -> Option<String> {
    let v = raw.trim().trim_matches(['"', '\'']).trim();
    let ok = !v.is_empty()
        && v.len() <= 40
        && v.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '.' | '-' | '_' | '+' | '/' | '*' | '~' | '^' | '>' | '<' | '='
                )
        });
    ok.then(|| v.to_string())
}

/// The versions `dir` pins, one per toolchain (a tool's own file wins over
/// `.tool-versions` and dev-container settings).
pub fn pins(dir: &Path) -> Vec<RawPin> {
    let mut out: Vec<RawPin> = Vec::new();
    let mut add = |tool: &'static str, wants: Option<String>, file: &str| {
        if let Some(wants) = wants
            && !out.iter().any(|p| p.tool == tool)
        {
            out.push(RawPin {
                tool,
                wants,
                file: file.to_string(),
            });
        }
    };
    let first_line = |name: &str| {
        read_small(&dir.join(name), MAX_SMALL_FILE)
            .and_then(|t| {
                t.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(String::from)
            })
            .and_then(|l| version_name(&l))
    };
    if let Some(pin) = crate::toolchains::rust_pin(dir) {
        let file = if is_plain_file(&dir.join("rust-toolchain.toml")) {
            "rust-toolchain.toml"
        } else {
            "rust-toolchain"
        };
        add("rust", Some(pin), file);
    }
    add("node", first_line(".nvmrc"), ".nvmrc");
    add("node", first_line(".node-version"), ".node-version");
    add("python", first_line(".python-version"), ".python-version");
    add("java", first_line(".java-version"), ".java-version");
    add("ruby", first_line(".ruby-version"), ".ruby-version");
    if let Some(sdk) = read_small(&dir.join("global.json"), MAX_SMALL_FILE)
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.pointer("/sdk/version")?.as_str().and_then(version_name))
    {
        add("dotnet", Some(sdk), "global.json");
    }
    if let Some(gomod) = read_small(&dir.join("go.mod"), MAX_MANIFEST) {
        let line = |key: &str| {
            gomod
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix(key).map(str::trim).map(String::from))
        };
        let toolchain = line("toolchain ").map(|t| t.trim_start_matches("go").to_string());
        add(
            "go",
            toolchain
                .or_else(|| line("go "))
                .and_then(|v| version_name(&v)),
            "go.mod",
        );
    }
    if let Some(text) = read_small(&dir.join(".tool-versions"), MAX_SMALL_FILE) {
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let (Some(name), Some(version)) = (words.next(), words.next()) else {
                continue;
            };
            if let Some(tool) = tool_for(name) {
                add(tool, version_name(version), ".tool-versions");
            }
        }
    }
    for file in [".devcontainer/devcontainer.json", ".devcontainer.json"] {
        let Some(value) = read_small(&dir.join(file), MAX_SMALL_FILE)
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&strip_json_comments(&t)).ok())
        else {
            continue;
        };
        if let Some(features) = value.get("features").and_then(|f| f.as_object()) {
            for (key, options) in features {
                // ghcr.io/devcontainers/features/node:1
                let name = key.rsplit('/').next().unwrap_or(key);
                let name = name.split(':').next().unwrap_or(name);
                let version = options.get("version").and_then(|v| v.as_str());
                if let (Some(tool), Some(v)) = (tool_for(name), version) {
                    add(tool, version_name(v), file);
                }
            }
        }
        // mcr.microsoft.com/devcontainers/python:1-3.12-bookworm
        if let Some(image) = value.get("image").and_then(|i| i.as_str()) {
            let last = image.rsplit('/').next().unwrap_or(image);
            if let Some((name, tag)) = last.split_once(':')
                && let Some(tool) = tool_for(name)
            {
                // The last number in the tag (`1-` is the image's own
                // version, the language version comes after it).
                let v = tag.split('-').rfind(|s| {
                    s.chars().next().is_some_and(|c| c.is_ascii_digit())
                        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
                });
                add(tool, v.and_then(version_name), file);
            }
        }
    }
    out
}

/// A tool name in `.tool-versions` or a dev-container setting, as a
/// toolchain id.
fn tool_for(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "node" | "nodejs" | "javascript-node" | "typescript-node" => "node",
        "python" => "python",
        "go" | "golang" => "go",
        "rust" => "rust",
        "java" => "java",
        "dotnet" | "dotnet-core" => "dotnet",
        "ruby" => "ruby",
        "php" => "php",
        _ => return None,
    })
}

/// JSON with `//` and `/* */` comments (dev-container files) as plain JSON.
/// Strings are copied as they are.
fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for n in chars.by_ref() {
                    if last == '*' && n == '/' {
                        break;
                    }
                    last = n;
                }
            }
            _ => out.push(c),
        }
    }
    // Trailing commas, which dev-container files allow too.
    let mut cleaned = String::with_capacity(out.len());
    let bytes: Vec<char> = out.chars().collect();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            cleaned.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                cleaned.push(bytes[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            cleaned.push(c);
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                cleaned.push(c);
            }
        } else {
            cleaned.push(c);
        }
        i += 1;
    }
    cleaned
}

/// Whether a pinned version and the installed one agree: every number the
/// pin gives must match (`22` and `22.11.0`, `3.12` and `3.12.7`). `None`
/// for ranges, channels and aliases (`lts/*`, `stable`, `>=18`).
pub fn version_matches(wants: &str, installed: &str) -> Option<bool> {
    let w = wants
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim_start_matches("go")
        .trim_end_matches(".x")
        .trim_end_matches(".*");
    let parts: Vec<&str> = w.split('.').collect();
    if parts.is_empty()
        || !parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let have: Vec<&str> = installed
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    Some(
        parts.len() <= have.len()
            && parts
                .iter()
                .zip(&have)
                .all(|(a, b)| a.trim_start_matches('0') == b.trim_start_matches('0')),
    )
}

/// The packages' usual folder for a fetch step is there.
fn fetched(dir: &Path, id: &str) -> bool {
    let has = |d: &str| dir.join(d).is_dir();
    match id {
        "rust" | "go" | "php" => has("vendor"),
        "node" => has("node_modules"),
        "python" => has(".venv") || has("wheels"),
        "dotnet" => has("packages"),
        "gradle" => has(".gradle-home"),
        _ => false,
    }
}

/// Whether the part uses outside packages, when its files say so.
fn needed(dir: &Path, id: &str) -> Option<bool> {
    match id {
        "rust" => {
            let lock = read_small(&dir.join("Cargo.lock"), 16 * MAX_MANIFEST)?;
            Some(
                lock.contains("source = \"registry+")
                    || lock.contains("source = \"git+")
                    || lock.contains("source = \"sparse+"),
            )
        }
        "node" => {
            let v: serde_json::Value =
                serde_json::from_str(&read_small(&dir.join("package.json"), MAX_MANIFEST)?).ok()?;
            Some(
                ["dependencies", "devDependencies", "optionalDependencies"]
                    .iter()
                    .any(|k| {
                        v.get(k)
                            .and_then(|d| d.as_object())
                            .is_some_and(|d| !d.is_empty())
                    }),
            )
        }
        "python" => {
            let list = read_small(&dir.join("requirements.txt"), MAX_MANIFEST)?;
            Some(
                list.lines()
                    .map(str::trim)
                    .any(|l| !l.is_empty() && !l.starts_with('#')),
            )
        }
        "go" => {
            let gomod = read_small(&dir.join("go.mod"), MAX_MANIFEST)?;
            Some(gomod.lines().any(|l| l.trim_start().starts_with("require")))
        }
        _ => None,
    }
}

// ---- runs automatically (A2) ------------------------------------------------------

/// npm runs these on `npm install` / `npm ci` (or `npm test`, `npm pack`)
/// without being named.
const NPM_AUTO: &[&str] = &[
    "preinstall",
    "install",
    "postinstall",
    "prepublish",
    "preprepare",
    "prepare",
    "postprepare",
    "prepack",
    "postpack",
    "pretest",
    "posttest",
];
/// Composer runs these on install and update.
const COMPOSER_AUTO: &[&str] = &[
    "pre-install-cmd",
    "post-install-cmd",
    "pre-update-cmd",
    "post-update-cmd",
    "post-autoload-dump",
    "post-root-package-install",
    "post-create-project-cmd",
];
/// Dev-container commands, and where each runs.
const DEVCONTAINER_COMMANDS: &[(&str, &str)] = &[
    (
        "initializeCommand",
        "on your PC, before the container starts",
    ),
    ("onCreateCommand", "inside the container"),
    ("updateContentCommand", "inside the container"),
    ("postCreateCommand", "inside the container"),
    ("postStartCommand", "inside the container"),
    ("postAttachCommand", "inside the container"),
];

fn excerpt(text: &str) -> String {
    let (shown, _) = gardenwall_policy::protect::reveal_invisible(text.trim());
    let one_line: String = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > MAX_EXCERPT {
        format!(
            "{}…",
            one_line.chars().take(MAX_EXCERPT).collect::<String>()
        )
    } else {
        one_line
    }
}

fn command_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Object(o) => o
            .values()
            .map(command_text)
            .collect::<Vec<_>>()
            .join(" && "),
        _ => String::new(),
    }
}

/// The "runs automatically" entry for one file, if it is one.
fn auto_run_entry(folder: &Path, rel: &str) -> Option<AutoRun> {
    let path = RelPath::parse(rel).ok()?;
    let PathClass::PersistenceRisk(rule) = classify(&path) else {
        return None;
    };
    let name = path.file_name().to_ascii_lowercase();
    let file = folder.join(rel);
    let entry = |when: &str, details: Vec<String>| {
        Some(AutoRun {
            path: rel.to_string(),
            when: when.to_string(),
            details,
        })
    };
    match rule {
        PersistenceRule::PackageScripts => match name.as_str() {
            "package.json" | "composer.json" => {
                let v: serde_json::Value =
                    serde_json::from_str(&read_small(&file, MAX_MANIFEST)?).ok()?;
                let scripts = v.get("scripts")?.as_object()?;
                let (auto, when) = if name == "package.json" {
                    (
                        NPM_AUTO,
                        "npm runs these scripts by itself when the packages are installed (or on npm test, npm pack).",
                    )
                } else {
                    (
                        COMPOSER_AUTO,
                        "Composer runs these scripts by itself on install and update.",
                    )
                };
                let details: Vec<String> = auto
                    .iter()
                    .filter_map(|k| {
                        scripts
                            .get(*k)
                            .map(|v| format!("{k}: {}", excerpt(&command_text(v))))
                    })
                    .collect();
                if details.is_empty() {
                    return None;
                }
                entry(when, details)
            }
            "gemfile" => entry(
                "Bundler runs it as Ruby code whenever it reads the project's packages.",
                vec![],
            ),
            "rakefile" => entry("rake runs it as Ruby code for every task.", vec![]),
            _ => entry("The package manager loads it when it runs.", vec![]),
        },
        PersistenceRule::BuildScript => {
            let when = match name.as_str() {
                "build.rs" => "Cargo compiles and runs it before it builds the crate.",
                "setup.py" | "setup.cfg" => {
                    "pip and setuptools run it when the project is installed or built."
                }
                "pyproject.toml" => {
                    let text = read_small(&file, MAX_MANIFEST)?;
                    if !text.contains("[build-system]") {
                        return None;
                    }
                    "Its build backend runs when the project is installed or built."
                }
                "conftest.py" => "pytest runs it whenever tests are collected.",
                "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts"
                | "gradle.properties" => {
                    "Gradle runs it as code when it reads the build (also while fetching)."
                }
                "gradlew" | "gradlew.bat" | "gradle-wrapper.properties" | "gradle-wrapper.jar" => {
                    "The Gradle wrapper downloads and runs the Gradle it names."
                }
                "pom.xml" | "mvnw" | "mvnw.cmd" => {
                    "Maven runs the plugins it names when it builds."
                }
                "binding.gyp" => "node-gyp runs it when the native addon is built.",
                _ if path
                    .components()
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case("buildsrc")) =>
                {
                    "Gradle compiles and runs it when it reads the build."
                }
                _ if path
                    .components()
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(".mvn")) =>
                {
                    "Maven reads it when it starts (options and extensions)."
                }
                _ => "MSBuild runs its tasks when the project is restored or built.",
            };
            entry(when, vec![])
        }
        PersistenceRule::EditorTasks => match name.as_str() {
            "tasks.json" => {
                let text = read_small(&file, MAX_MANIFEST).unwrap_or_default();
                let details = if text.contains("folderOpen") {
                    vec![
                        "A task is set to run when the folder opens (runOn: folderOpen)."
                            .to_string(),
                    ]
                } else {
                    vec![]
                };
                entry(
                    "VS Code runs these tasks when you start them, or when the folder opens if a task says so.",
                    details,
                )
            }
            "launch.json" => entry("VS Code runs these when you start debugging.", vec![]),
            "devcontainer.json" | ".devcontainer.json" => {
                let v: Option<serde_json::Value> = read_small(&file, MAX_SMALL_FILE)
                    .and_then(|t| serde_json::from_str(&strip_json_comments(&t)).ok());
                let details = v
                    .map(|v| {
                        DEVCONTAINER_COMMANDS
                            .iter()
                            .filter_map(|(k, where_)| {
                                v.get(*k).map(|c| {
                                    format!("{k} ({where_}): {}", excerpt(&command_text(c)))
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                entry(
                    "Dev container tools run its commands when the container is created or opened.",
                    details,
                )
            }
            _ if name.ends_with(".code-workspace") => entry(
                "VS Code reads its tasks and settings when the workspace opens.",
                vec![],
            ),
            _ => None,
        },
        PersistenceRule::GitHookManager => entry(
            "Installs Git hooks: once installed, they run on commit or push.",
            vec![],
        ),
        PersistenceRule::ToolchainConfig => entry(
            "Changes which tool versions or programs run (a runner, a wrapper, a pinned version).",
            vec![],
        ),
        PersistenceRule::ShellProfile => entry("A shell runs it when it starts.", vec![]),
        PersistenceRule::Direnv => entry(
            "direnv runs it when you enter the folder in a terminal.",
            vec![],
        ),
        PersistenceRule::PythonStartup => {
            entry("Python runs it when it starts or imports it.", vec![])
        }
        // CI runs on a server, not on this PC; Git attributes and
        // submodules run nothing by themselves; agent instructions steer
        // agents and have their own warnings.
        PersistenceRule::Ci
        | PersistenceRule::GitAttributesOrModules
        | PersistenceRule::AgentInstructions
        | PersistenceRule::GitInternals => None,
    }
}

/// Git's own files that run programs: installed hooks and configuration
/// that names a program (`core.hooksPath`, `core.fsmonitor`, filter
/// drivers).
fn git_entries(folder: &Path) -> Vec<AutoRun> {
    let git = folder.join(".git");
    if !std::fs::symlink_metadata(&git).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(git.join("hooks")) {
        let mut hooks: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.ends_with(".sample"))
            .collect();
        hooks.sort();
        for hook in hooks {
            out.push(AutoRun {
                path: format!(".git/hooks/{hook}"),
                when: format!("Git runs this hook by itself ({hook})."),
                details: vec![],
            });
        }
    }
    if let Some(config) = read_small(&git.join("config"), MAX_SMALL_FILE) {
        let mut section = String::new();
        let mut details = Vec::new();
        for line in config.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line.to_ascii_lowercase();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let names_program = (section == "[core]"
                && matches!(
                    key.as_str(),
                    "hookspath" | "fsmonitor" | "sshcommand" | "pager" | "editor"
                ))
                || (section.starts_with("[filter ")
                    && matches!(key.as_str(), "clean" | "smudge" | "process"))
                || (section.starts_with("[diff ")
                    && matches!(key.as_str(), "textconv" | "command"));
            if names_program {
                details.push(format!(
                    "{} {key} = {}",
                    section.trim_matches(['[', ']']),
                    excerpt(value)
                ));
            }
        }
        if !details.is_empty() {
            out.push(AutoRun {
                path: ".git/config".into(),
                when: "Git runs the programs these settings name.".into(),
                details,
            });
        }
    }
    out
}

/// Folders the walk doesn't enter: Git's store (its files are reported
/// above), caches and fetched packages.
fn skip_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    gardenwall_fs::snapshot::SKIPPED_DIRS.contains(&lower.as_str())
        || matches!(
            lower.as_str(),
            "vendor" | "packages" | "wheels" | "dist" | "obj" | "bin"
        )
}

/// Every "runs automatically" entry in `folder`, and how many were left
/// out over the limit.
pub fn auto_run(folder: &Path) -> (Vec<AutoRun>, usize) {
    let mut out = git_entries(folder);
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(folder.to_path_buf(), String::new(), 0)];
    let mut seen = 0usize;
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            seen += 1;
            if seen > WALK_ENTRIES {
                stack.clear();
                break;
            }
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let Ok(kind) = e.file_type() else {
                continue;
            };
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if kind.is_dir() && !kind.is_symlink() {
                if depth + 1 < WALK_DEPTH && !skip_dir(&name) {
                    stack.push((e.path(), child, depth + 1));
                }
            } else if kind.is_file()
                && let Some(entry) = auto_run_entry(folder, &child)
            {
                out.push(entry);
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    let more = out.len().saturating_sub(MAX_AUTO_RUN);
    out.truncate(MAX_AUTO_RUN);
    (out, more)
}

// ---- project instructions (A3) --------------------------------------------------

/// Largest `AGENTS.md` saved from the draft (the same as `/memory`).
const MAX_INSTRUCTIONS: usize = 64 * 1024;

/// The build, run and test commands for a stack, as the sandbox runs them.
fn commands_for(stack: Stack, dir: &Path) -> Vec<String> {
    let has = |f: &str| dir.join(f).exists();
    match stack.toolchain {
        "rust" => vec!["Build: `cargo build`. Test: `cargo test`. Run: `cargo run`.".into()],
        "node" => {
            let scripts: Vec<String> = read_small(&dir.join("package.json"), MAX_MANIFEST)
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("scripts")?.as_object().map(|s| s.keys().cloned().collect()))
                .unwrap_or_default();
            let named: Vec<String> = ["build", "test", "lint", "start", "dev"]
                .iter()
                .filter(|s| scripts.iter().any(|k| k == *s))
                .map(|s| if *s == "test" || *s == "start" { format!("`npm {s}`") } else { format!("`npm run {s}`") })
                .collect();
            if named.is_empty() {
                vec!["Run scripts with `node`; tools from the project's packages with `npx` (for example `npx tsc`).".into()]
            } else {
                vec![format!("Scripts: {}. Tools from the project's packages run with `npx`.", named.join(", "))]
            }
        }
        "python" => {
            let uses_pytest = ["requirements.txt", "pyproject.toml", "requirements-dev.txt"].iter().any(|f| {
                read_small(&dir.join(f), MAX_MANIFEST).is_some_and(|t| t.contains("pytest"))
            });
            let python = if has(".venv") { "`.venv\\Scripts\\python`" } else { "`python`" };
            vec![format!(
                "Use {python}. Test: {}.",
                if uses_pytest { "`python -m pytest`" } else { "`python -m unittest`" }
            )]
        }
        "go" => vec!["Build: `go build ./...`. Test: `go test ./...`. Never `go get` or `go mod tidy` (they need the network).".into()],
        "dotnet" => vec!["Build: `dotnet build --no-restore`. Test: `dotnet test --no-restore`. Run: `dotnet run --no-restore`.".into()],
        "java" if has("gradlew.bat") || has("build.gradle") || has("build.gradle.kts") => vec![
            "Build and test with the Gradle wrapper, offline: `.\\gradlew.bat --offline --no-daemon --gradle-user-home .gradle-home --no-configuration-cache --console=plain build` (or `test`).".into(),
        ],
        "java" => vec!["Maven builds need the network: ask the user to run them.".into()],
        "php" => vec!["Run tools with `php`, for example `php vendor/bin/phpunit` (batch launchers can't run in the sandbox).".into()],
        "cpp" => vec!["Configure: `cmake -S . -B build`. Build: `cmake --build build`.".into()],
        "ruby" => vec!["Test: `bundle exec rake test` (Ruby needs \"Folders above the project\").".into()],
        _ => vec![],
    }
}

/// A starter `AGENTS.md` from what Gardenwall found, for the user to edit.
pub fn draft(scan: &Scan) -> String {
    let mut s = String::from(
        "# Project instructions\n\nNotes for the coding agent. Gardenwall wrote this draft from the project's files: edit it to match how you work.\n",
    );
    let many = scan.parts.iter().filter(|p| !p.stacks.is_empty()).count() > 1;
    for part in scan.parts.iter().filter(|p| !p.stacks.is_empty()) {
        if many {
            let title = if part.path.is_empty() {
                "The project folder"
            } else {
                part.path.as_str()
            };
            s.push_str(&format!("\n## {title}\n\n"));
        } else {
            s.push_str("\n## Build, run and test\n\n");
        }
        let labels: Vec<&str> = part.stacks.iter().map(|st| st.label).collect();
        s.push_str(&format!("- What it is: {}.\n", labels.join(", ")));
        let mut seen: Vec<&str> = Vec::new();
        for st in &part.stacks {
            if seen.contains(&st.toolchain) {
                continue;
            }
            seen.push(st.toolchain);
            for line in commands_for(*st, &part.dir) {
                s.push_str(&format!("- {line}\n"));
            }
        }
        for pin in &part.pins {
            s.push_str(&format!(
                "- {} version: {} (from `{}`).\n",
                tool_name(pin.tool),
                pin.wants,
                pin.file
            ));
        }
    }
    s.push_str("\n## Dependencies\n\n- Commands have no network. When the build needs packages, use `fetch_dependencies`: Gardenwall shows the user its standard command, then build offline as its result says.\n");
    s.push_str("\n## Conventions\n\n- (Code style, naming, branches, what not to change.)\n");
    s
}

fn tool_name(id: &str) -> &'static str {
    match id {
        "rust" => "Rust",
        "node" => "Node.js",
        "python" => "Python",
        "go" => "Go",
        "java" => "Java",
        "dotnet" => ".NET SDK",
        "ruby" => "Ruby",
        "php" => "PHP",
        "cpp" => "C and C++",
        _ => "Tool",
    }
}

// ---- commands -------------------------------------------------------------------

use gardenwall_agent::display::shown;

/// The full report for one folder.
fn report(
    index: usize,
    folder: &Path,
    settings: &crate::state::Settings,
    project: &str,
    found: &[crate::toolchains::Found],
    can_init: bool,
    parent_ready: bool,
) -> FolderReadiness {
    let agent = &settings.agent;
    let scan = scan(folder);
    let added = crate::toolchains::added_for(agent, Some(project));
    let installed = |tool: &str| found.iter().find(|t| t.id == tool);
    let mut needs: Vec<ToolchainNeed> = Vec::new();
    let mut parent_need: Vec<String> = Vec::new();
    let mut parts = Vec::new();
    for part in &scan.parts {
        let place = if part.path.is_empty() {
            String::new()
        } else {
            format!(" in {}", part.path)
        };
        for st in &part.stacks {
            let by = format!("{}{place}", st.label);
            if let Some(n) = needs.iter_mut().find(|n| n.id == st.toolchain) {
                n.needed_by.push(by);
                continue;
            }
            let t = installed(st.toolchain);
            let info = crate::toolchains::install_info(st.toolchain);
            needs.push(ToolchainNeed {
                id: st.toolchain.into(),
                name: t
                    .map(|t| t.name)
                    .or(info.map(|i| i.name))
                    .unwrap_or(tool_name(st.toolchain))
                    .into(),
                needed_by: vec![by],
                version: t.map(|t| t.version.clone()),
                on: added.iter().any(|a| a == st.toolchain)
                    || crate::toolchains::present_otherwise(settings, st.toolchain),
                everywhere: agent.toolchains.iter().any(|a| a == st.toolchain),
                problem: t.and_then(|t| {
                    t.folders
                        .iter()
                        .find_map(|f| crate::toolchains::problem_for(t.name, f))
                }),
                install: info.cloned(),
            });
            if crate::toolchains::needs_parent_folders(st.toolchain).is_some() {
                parent_need.push(format!("{}{place}", tool_name(st.toolchain)));
            }
        }
        let pins = part
            .pins
            .iter()
            .map(|p| {
                // Rust follows the pin when rustup has that toolchain.
                let (have, matches) = if p.tool == "rust" {
                    match crate::toolchains::rust_with(Some(&p.wants)) {
                        Some(t) => (Some(t.version), Some(true)),
                        None => (
                            installed("rust").map(|t| t.version.clone()),
                            installed("rust").map(|_| false),
                        ),
                    }
                } else {
                    let have = installed(p.tool).map(|t| t.version.clone());
                    let matches = have.as_deref().and_then(|h| version_matches(&p.wants, h));
                    (have, matches)
                };
                Pin {
                    tool: p.tool.into(),
                    name: tool_name(p.tool).into(),
                    wants: p.wants.clone(),
                    file: if part.path.is_empty() {
                        p.file.clone()
                    } else {
                        format!("{}/{}", part.path, p.file)
                    },
                    installed: have,
                    matches,
                }
            })
            .collect();
        let fetch = part
            .plans
            .iter()
            .map(|plan| {
                if plan.id == "gradle" {
                    parent_need.push(format!("Gradle{place}"));
                }
                Fetch {
                    id: plan.id.into(),
                    label: plan.label.into(),
                    command: plan.command.clone(),
                    folder: shown(&part.dir),
                    // Something in the folder would run instead of the
                    // command's programs: say so before anything else.
                    warning: dependencies::hazard(&part.dir, plan.id)
                        .map(|why| {
                            format!("Don't run this until it's fixed. {why} Delete or rename them first.")
                        })
                        .or_else(|| plan.warning.map(String::from)),
                    fetched: fetched(&part.dir, plan.id),
                    needed: needed(&part.dir, plan.id),
                    needs_parent_folders: plan.id == "gradle",
                }
            })
            .collect();
        parts.push(Part {
            path: part.path.clone(),
            stacks: part.stacks.iter().map(|s| s.label.to_string()).collect(),
            pins,
            fetch,
        });
    }
    parent_need.dedup();
    FolderReadiness {
        index,
        folder: shown(folder),
        git: folder.join(".git").exists(),
        can_init,
        parts,
        toolchains: needs,
        instructions: scan.instructions,
        gitignore_missing: gardenwall_agent::secrets::gitignore_missing(folder)
            .map_or(0, |l| l.len()),
        parent_folders: (!parent_need.is_empty()).then(|| ParentNeed {
            needed_by: parent_need,
            on: crate::parent_folders::enabled(agent, Some(project)),
            ready: parent_ready,
        }),
        auto_run: scan.auto_run,
        auto_run_more: scan.auto_run_more,
    }
}

/// The readiness report for every folder of a project. `recheck` forgets
/// what toolchain detection found (after installing one).
#[tauri::command]
pub async fn project_readiness(
    state: State<'_, AppState>,
    id: String,
    recheck: bool,
) -> CmdResult<Vec<FolderReadiness>> {
    let (_, folders) = crate::projects::project_folders(&state, &id)?;
    let settings = state.saved_settings();
    tauri::async_runtime::spawn_blocking(move || {
        if recheck {
            crate::toolchains::forget_detected();
        }
        let found = crate::toolchains::detect();
        let can_init = gardenwall_agent::git::find_git_for_windows().is_some();
        let folders: Vec<PathBuf> = folders.iter().map(|f| plain(f)).collect();
        let parent_ready = crate::parent_folders::ready(&folders);
        folders
            .iter()
            .enumerate()
            .map(|(i, f)| report(i, f, &settings, &id, &found, can_init, parent_ready))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// A folder path without the `\\?\` prefix, so file checks behave as the
/// user's own tools do.
fn plain(path: &Path) -> PathBuf {
    PathBuf::from(shown(path))
}

fn folder_at(state: &AppState, id: &str, index: usize) -> CmdResult<PathBuf> {
    let (_, folders) = crate::projects::project_folders(state, id)?;
    let folder = folders
        .get(index)
        .ok_or("that folder isn't in the project")?;
    crate::projects::checked(&plain(folder))
}

/// A starter `AGENTS.md` for one of a project's folders (A3), for the user
/// to edit before saving.
#[tauri::command]
pub async fn project_instructions_draft(
    state: State<'_, AppState>,
    id: String,
    index: usize,
) -> CmdResult<String> {
    let folder = folder_at(&state, &id, index)?;
    tauri::async_runtime::spawn_blocking(move || draft(&scan(&plain(&folder))))
        .await
        .map_err(|e| e.to_string())
}

/// Save the `AGENTS.md` the user edited (A3). Only a new file: an existing
/// one is edited with `/memory` in a session, never replaced from here.
#[tauri::command]
pub fn save_project_instructions(
    state: State<'_, AppState>,
    id: String,
    index: usize,
    text: String,
) -> CmdResult<()> {
    if text.len() > MAX_INSTRUCTIONS {
        return Err("AGENTS.md is larger than 64 KB".into());
    }
    if text.trim().is_empty() {
        return Err("the instructions are empty".into());
    }
    let folder = folder_at(&state, &id, index)?;
    write_new_instructions(&folder, &text)
}

fn write_new_instructions(folder: &Path, text: &str) -> CmdResult<()> {
    let workspace = gardenwall_fs::Workspace::open(folder).map_err(|e| e.to_string())?;
    let name = RelPath::parse("AGENTS.md").map_err(|e| e.to_string())?;
    if workspace
        .read_optional(&name, MAX_INSTRUCTIONS as u64 * 16)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(
            "this folder already has an AGENTS.md; edit it with /memory in a session".into(),
        );
    }
    workspace
        .write(&name, text.as_bytes())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(root: &Path, list: &[(&str, &str)]) {
        for (f, text) in list {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    #[test]
    fn versions_match_on_the_numbers_the_pin_gives() {
        assert_eq!(version_matches("22", "22.11.0"), Some(true));
        assert_eq!(version_matches("v22.11", "22.11.0"), Some(true));
        assert_eq!(version_matches("20", "22.11.0"), Some(false));
        assert_eq!(version_matches("3.12", "3.12.7"), Some(true));
        assert_eq!(version_matches("3.12.8", "3.12.7"), Some(false));
        assert_eq!(version_matches("1.22.3", "go1.22.3"), Some(true));
        assert_eq!(version_matches("20.x", "20.1.0"), Some(true));
        assert_eq!(version_matches("lts/*", "22.11.0"), None);
        assert_eq!(version_matches(">=18", "22.11.0"), None);
        assert_eq!(version_matches("stable", "1.98.1"), None);
        assert_eq!(version_matches("8.0.400", "8.0.428"), Some(false));
        assert_eq!(version_matches("21", "21.0.12+7"), Some(true));
    }

    #[test]
    fn pins_are_read_as_names_and_a_tools_own_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        files(
            root,
            &[
                (".nvmrc", "# comment\nv22\n"),
                (".python-version", "3.12\n"),
                (
                    "global.json",
                    r#"{"sdk": {"version": "8.0.400", "rollForward": "latestFeature"}}"#,
                ),
                ("go.mod", "module x\n\ngo 1.22\n\ntoolchain go1.22.3\n"),
                (".tool-versions", "nodejs 18.0.0\nruby 3.3.0\nunknown 1\n"),
                (
                    ".devcontainer/devcontainer.json",
                    "{\n // comment\n \"image\": \"mcr.microsoft.com/devcontainers/java:1-21-bookworm\",\n \"features\": {\"ghcr.io/devcontainers/features/php:1\": {\"version\": \"8.3\"},},\n}",
                ),
                (".java-version", "$(rm -rf /)\n"),
            ],
        );
        let p = pins(root);
        let get = |t: &str| {
            p.iter()
                .find(|x| x.tool == t)
                .map(|x| (x.wants.as_str(), x.file.as_str()))
        };
        assert_eq!(
            get("node"),
            Some(("v22", ".nvmrc")),
            "the tool's own file wins over .tool-versions"
        );
        assert_eq!(get("python"), Some(("3.12", ".python-version")));
        assert_eq!(get("dotnet"), Some(("8.0.400", "global.json")));
        assert_eq!(
            get("go"),
            Some(("1.22.3", "go.mod")),
            "toolchain wins over the go line"
        );
        assert_eq!(get("ruby"), Some(("3.3.0", ".tool-versions")));
        assert_eq!(get("php"), Some(("8.3", ".devcontainer/devcontainer.json")));
        assert_eq!(
            get("java"),
            Some(("21", ".devcontainer/devcontainer.json")),
            "an odd .java-version is ignored"
        );
    }

    #[test]
    fn comments_and_trailing_commas_leave_strings_alone() {
        let text = "{\"a\": \"http://x/*y*/\", // c\n \"b\": [1, 2,], /* d */ \"c\": \"e,}\"}";
        let v: serde_json::Value = serde_json::from_str(&strip_json_comments(text)).unwrap();
        assert_eq!(v["a"], "http://x/*y*/");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["c"], "e,}");
    }

    #[test]
    fn runs_automatically_lists_what_runs_by_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        files(
            root,
            &[
                (
                    "package.json",
                    r#"{"scripts": {"postinstall": "node   setup.js", "build": "tsc", "test": "node --test"}}"#,
                ),
                (
                    "web/package.json",
                    r#"{"scripts": {"build": "vite build"}}"#,
                ),
                ("build.rs", "fn main() {}"),
                ("tests/conftest.py", ""),
                ("pyproject.toml", "[project]\nname = 'x'\n"),
                ("lib/pyproject.toml", "[build-system]\nrequires = []\n"),
                (
                    ".vscode/tasks.json",
                    r#"{"tasks": [{"runOptions": {"runOn": "folderOpen"}}]}"#,
                ),
                (".vscode/settings.json", "{}"),
                (
                    ".devcontainer/devcontainer.json",
                    r#"{"initializeCommand": "echo hi", "postCreateCommand": ["npm", "ci"]}"#,
                ),
                (".github/workflows/ci.yml", ""),
                ("AGENTS.md", ""),
                (".envrc", ""),
                ("node_modules/x/build.rs", ""),
                ("vendor/y/build.rs", ""),
                (".git/hooks/pre-commit", ""),
                (".git/hooks/pre-push.sample", ""),
                (
                    ".git/config",
                    "[core]\n\tfsmonitor = .git/monitor.exe\n\tbare = false\n[filter \"lfs\"]\n\tsmudge = git-lfs smudge -- %f\n",
                ),
            ],
        );
        let (list, more) = auto_run(root);
        assert_eq!(more, 0);
        let paths: Vec<&str> = list.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".devcontainer/devcontainer.json",
                ".envrc",
                ".git/config",
                ".git/hooks/pre-commit",
                ".vscode/tasks.json",
                "build.rs",
                "lib/pyproject.toml",
                "package.json",
                "tests/conftest.py",
            ],
            "CI, agent instructions, editor settings, scripts that don't run by themselves, a pyproject without a build system, fetched packages and sample hooks are left out"
        );
        let get = |p: &str| list.iter().find(|a| a.path == p).unwrap();
        assert_eq!(
            get("package.json").details,
            ["postinstall: node setup.js"],
            "test runs only when named (npm test); pretest and posttest would be listed"
        );
        assert!(get(".vscode/tasks.json").details[0].contains("folderOpen"));
        let dc = &get(".devcontainer/devcontainer.json").details;
        assert!(
            dc[0].starts_with("initializeCommand (on your PC") && dc[0].ends_with("echo hi"),
            "{dc:?}"
        );
        assert!(dc[1].ends_with("npm ci"));
        let git = &get(".git/config").details;
        assert_eq!(git.len(), 2, "{git:?}");
        assert!(git[0].contains("fsmonitor") && git[1].contains("smudge"));
    }

    #[test]
    fn invisible_characters_in_commands_are_shown() {
        let dir = tempfile::tempdir().unwrap();
        files(
            dir.path(),
            &[(
                "package.json",
                "{\"scripts\": {\"install\": \"node a.js\u{200b}\"}}",
            )],
        );
        let (list, _) = auto_run(dir.path());
        assert!(
            list[0].details[0].contains("[U+200B]"),
            "{:?}",
            list[0].details
        );
    }

    #[test]
    fn parts_pins_fetch_and_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        files(
            root,
            &[
                ("Cargo.toml", "[workspace]"),
                (
                    "Cargo.lock",
                    "[[package]]\nname = \"serde\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
                ),
                ("crates/a/Cargo.toml", ""),
                (
                    "apps/web/package.json",
                    r#"{"scripts": {"build": "vite build", "test": "vitest"}, "devDependencies": {"vite": "1"}}"#,
                ),
                ("apps/web/.nvmrc", "22"),
                ("apps/web/node_modules/.keep", ""),
                ("backend/requirements.txt", "# none\n"),
            ],
        );
        let s = scan(root);
        let paths: Vec<&str> = s.parts.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            ["", "apps/web", "backend"],
            "the Cargo workspace covers crates/a"
        );
        assert_eq!(
            s.parts[1].pins,
            vec![RawPin {
                tool: "node",
                wants: "22".into(),
                file: ".nvmrc".into()
            }]
        );
        assert!(fetched(&s.parts[1].dir, "node"));
        assert!(!fetched(&s.parts[0].dir, "rust"));
        assert_eq!(needed(&s.parts[0].dir, "rust"), Some(true));
        assert_eq!(needed(&s.parts[1].dir, "node"), Some(true));
        assert_eq!(
            needed(&s.parts[2].dir, "python"),
            Some(false),
            "only a comment"
        );
        assert_eq!(s.instructions, None);
        let d = draft(&s);
        assert!(
            d.contains("## apps/web") && d.contains("`npm run build`, `npm test`"),
            "{d}"
        );
        assert!(d.contains("Node.js version: 22 (from `.nvmrc`)"), "{d}");
        assert!(
            d.contains("`cargo test`") && d.contains("`python -m unittest`"),
            "{d}"
        );
        assert!(d.contains("fetch_dependencies"));
    }

    #[test]
    fn instructions_are_written_only_when_there_are_none() {
        let dir = tempfile::tempdir().unwrap();
        write_new_instructions(dir.path(), "# Notes\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
            "# Notes\n"
        );
        let err = write_new_instructions(dir.path(), "other").unwrap_err();
        assert!(err.contains("already has"), "{err}");
        assert_eq!(scan(dir.path()).instructions.as_deref(), Some("AGENTS.md"));
    }
}
