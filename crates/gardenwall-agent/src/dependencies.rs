//! "Fetch dependencies": one step for the
//! internet part of a build. Agent commands have no network, so a project's
//! packages are fetched by the user. Instead of the model writing that
//! command, Gardenwall picks the standard one for the project from its file
//! names (nothing is read or run): lock files respected, install scripts and
//! plugins off, packages kept in the project where the sandbox can use them.
//! The command goes on the usual Run this yourself card, marked as written
//! by Gardenwall; Gardenwall never runs it. When the user says it ran, the
//! agent is told the offline next step.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::run_request::RunRequest;
use crate::tools::{Resolved, ToolDef, Workspaces};

pub const NAME: &str = "fetch_dependencies";

/// One way to fetch a project's packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub id: &'static str,
    /// "Rust (Cargo)", for the card and the agent.
    pub label: &'static str,
    /// The command(s) for the user, one per line.
    pub command: String,
    /// What the agent does next, offline.
    pub then: String,
    /// What the user should know before running it (shown on the card).
    pub warning: Option<&'static str>,
}

/// Gradle and MSBuild read the project's build files as code when they
/// fetch, so the card says so.
const GRADLE_WARNING: &str = "Gradle runs this project's build scripts (build.gradle(.kts), settings.gradle(.kts), gradle.properties, buildSrc) on your PC while it fetches. The agent's commands can't change them, and Gardenwall showed you each change its file tools made; if you're unsure, look at them before you run this.";
const DOTNET_WARNING: &str = "dotnet restore reads this project's MSBuild files (.csproj and the files they import) on your PC, and they can run commands. The agent's commands can't change the ones that already existed when its session started, and Gardenwall showed you each change its file tools made; if you're unsure, look at them before you run this.";

const STALE_LOCK_WARNING: &str = "Cargo.lock doesn't list every dependency in Cargo.toml yet, so this command adds the missing ones to it. Versions already in Cargo.lock stay as they are.";

/// Programs the fetch commands start, directly or through a batch launcher
/// (`composer.bat` runs `php`, `gradlew.bat` runs `java`). Command Prompt
/// and batch files look in the current folder before `PATH`, so a file with
/// one of these names in the folder would run instead, outside the sandbox.
const LAUNCHED: &[&str] = &[
    "node",
    "npm",
    "npx",
    "corepack",
    "php",
    "composer",
    "java",
    "javaw",
    "gradle",
    "python",
    "python3",
    "py",
    "pip",
    "pip3",
    "uv",
    "cargo",
    "rustc",
    "rustup",
    "go",
    "dotnet",
    "msbuild",
    "git",
    "cmd",
    "powershell",
    "pwsh",
];

/// What Windows runs by name without its extension (`PATHEXT`'s defaults,
/// plus PowerShell scripts).
const RUNNABLE: &[&str] = &[
    "exe", "com", "bat", "cmd", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msc", "ps1",
];

/// Files in `folder` (top level only) that would run instead of a program
/// the fetch command starts.
pub(crate) fn planted_programs(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower
                .rsplit_once('.')
                .is_some_and(|(stem, ext)| LAUNCHED.contains(&stem) && RUNNABLE.contains(&ext))
        })
        .collect();
    out.sort();
    out
}

/// Why running `plan_id`'s fetch command in `folder` could start a program
/// the project put there, outside the sandbox, or `None`. The agent's card
/// is refused and the project page shows it instead of the command.
pub fn hazard(folder: &Path, plan_id: &str) -> Option<String> {
    let planted = planted_programs(folder);
    if !planted.is_empty() {
        return Some(format!(
            "The folder has files named like the programs the fetch command starts ({}): in a Command Prompt or a batch launcher (composer.bat, gradlew.bat) they would run instead of the real programs.",
            planted.join(", ")
        ));
    }
    if plan_id == "node" {
        let keys = npmrc_programs(folder);
        if !keys.is_empty() {
            return Some(format!(
                "The folder's .npmrc sets {}, which makes npm start a program the project names.",
                keys.join(", ")
            ));
        }
    }
    None
}

/// npm settings that name a program npm starts (`git=` is used for git
/// dependencies even with install scripts off).
const NPMRC_PROGRAMS: &[&str] = &[
    "git",
    "script-shell",
    "shell",
    "node-gyp",
    "editor",
    "browser",
    "viewer",
];

/// The npm settings in `folder`'s `.npmrc` that name a program.
fn npmrc_programs(folder: &Path) -> Vec<String> {
    let path = folder.join(".npmrc");
    let small = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024);
    let Some(text) = small.then(|| std::fs::read_to_string(&path).ok()).flatten() else {
        // A link or a huge file: refuse rather than guess.
        return if std::fs::symlink_metadata(&path).is_ok() {
            vec![".npmrc (not a small plain file)".into()]
        } else {
            Vec::new()
        };
    };
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let key = l.split('=').next()?.trim().to_ascii_lowercase();
            NPMRC_PROGRAMS.contains(&key.as_str()).then_some(key)
        })
        .collect();
    out.dedup();
    out
}

/// Gradle's command for the fetch: everything the build resolves (plugins
/// and dependencies, Gradle itself through the wrapper) goes into the
/// project's `.gradle-home`, without compiling or running the project
/// (`--write-verification-metadata … --dry-run` only resolves and records
/// checksums, in a separate dry-run file).
const GRADLE_FETCH: &str = ".\\gradlew.bat --gradle-user-home .gradle-home --no-daemon --write-verification-metadata sha256 --dry-run help";

/// The kinds of project with a fetch step, by the files they have.
pub const IDS: &[&str] = &["rust", "node", "python", "go", "dotnet", "gradle", "php"];

pub fn definition() -> ToolDef {
    ToolDef {
        name: NAME,
        description: "Ask the user to fetch this project's dependencies (crates, npm packages, Python wheels, Go modules, NuGet packages, Gradle builds for Java and Kotlin, Composer packages). Your commands have no network; Gardenwall shows the user its standard command for the project (install scripts off, lock files respected), waits until they ran it, then tells you how to build offline. Use this instead of writing the fetch command yourself with ask_user_to_run.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "folder": {"type": "string", "description": "The project's folder (default: the first project folder)"},
                "kind": {"type": "string", "enum": IDS, "description": "Which packages, when the folder has more than one kind"}
            }
        }),
        snippet: "Ask the user to fetch the project's dependencies (Gardenwall picks the command)",
        guidelines: &[
            "When a build needs packages from the internet, use fetch_dependencies; then build offline as its result says",
        ],
    }
}

fn has(root: &Path, name: &str) -> bool {
    root.join(name).is_file()
}

fn has_ext(root: &Path, exts: &[&str]) -> bool {
    std::fs::read_dir(root).is_ok_and(|d| {
        d.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            exts.iter().any(|x| n.ends_with(x))
        })
    })
}

/// Gradle's build files: any of them makes a folder a Gradle build.
const GRADLE_FILES: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
];

/// Whether [`plans`] would find a fetch step in `root`, by file names only
/// (nothing is read): for scans over many folders.
pub fn has_plans(root: &Path) -> bool {
    [
        "Cargo.toml",
        "package.json",
        "requirements.txt",
        "uv.lock",
        "go.mod",
        "composer.json",
    ]
    .iter()
    .chain(GRADLE_FILES)
    .any(|f| has(root, f))
        || has_ext(root, &[".csproj", ".fsproj", ".sln", ".slnx"])
}

/// The fetch steps that fit the folder `root`, by its file names (and, for
/// Rust, whether Cargo.lock lists what Cargo.toml names).
pub fn plans(root: &Path) -> Vec<Plan> {
    let mut out = Vec::new();
    if has(root, "Cargo.toml") {
        // A lock file that misses a dependency in Cargo.toml (added since
        // the last build) makes `--locked` fail for sure. Then the fetch
        // adds the missing ones; Cargo keeps every version already locked.
        let lock = has(root, "Cargo.lock");
        let stale = lock && !cargo_lock_missing(root).is_empty();
        out.push(Plan {
            id: "rust",
            label: "Rust (Cargo)",
            command: if lock && !stale {
                "cargo vendor --locked vendor".into()
            } else {
                "cargo vendor vendor".into()
            },
            then: format!(
                "The crates are in vendor\\.{} Point Cargo at them: put exactly this in .cargo\\config.toml with the write or edit tool (a protected file, so the user approves it), then build as usual (offline is already on):\n\n[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"",
                ignore_note("vendor")
            ),
            warning: stale.then_some(STALE_LOCK_WARNING),
        });
    }
    if has(root, "package.json") {
        let lock = has(root, "package-lock.json");
        let mut then = "node_modules is ready. Install scripts were off: if a package needs its build step (a native addon), run npm rebuild <package> in your sandbox. Run tools through npm scripts or npx (for example npx tsc).".to_string() + &ignore_note("node_modules");
        if has(root, "pnpm-lock.yaml") || has(root, "yarn.lock") {
            then.push_str(" This project also has a pnpm or Yarn lock file; Gardenwall used npm, because pnpm's linked node_modules can't be given to the sandbox.");
        }
        out.push(Plan {
            id: "node",
            label: "Node.js (npm)",
            command: if lock {
                "npm ci --ignore-scripts".into()
            } else {
                "npm install --ignore-scripts".into()
            },
            then,
            warning: None,
        });
    }
    if has(root, "requirements.txt") {
        out.push(Plan {
            id: "python",
            label: "Python (pip)",
            command: "python -m pip download --only-binary=:all: --dest wheels -r requirements.txt"
                .into(),
            then: python_then("requirements.txt"),
            warning: None,
        });
    } else if has(root, "uv.lock") {
        out.push(Plan {
            id: "python",
            label: "Python (uv)",
            command: "uv export --frozen --no-hashes --no-emit-project --output-file requirements.lock.txt\npython -m pip download --only-binary=:all: --dest wheels -r requirements.lock.txt".into(),
            then: python_then("requirements.lock.txt"),
            warning: None,
        });
    }
    if has(root, "go.mod") {
        out.push(Plan {
            id: "go",
            label: "Go (modules)",
            command: "go mod vendor".into(),
            then: "The modules are in vendor\\. Go uses them automatically (go build, go test, go run); never run go get or go mod tidy in your sandbox, which need the network.".to_string() + &ignore_note("vendor"),
            warning: None,
        });
    }
    if has_ext(root, &[".csproj", ".fsproj", ".sln", ".slnx"]) {
        out.push(Plan {
            id: "dotnet",
            label: ".NET (NuGet)",
            command: "dotnet restore --packages packages".into(),
            then: "The NuGet packages are in packages\\ and the project is restored. Build and test without restoring again: dotnet build --no-restore, dotnet test --no-restore, dotnet run --no-restore. If you change package references, call fetch_dependencies again.".to_string() + &ignore_note("packages"),
            warning: Some(DOTNET_WARNING),
        });
    }
    if GRADLE_FILES.iter().any(|f| has(root, f)) {
        let wrapper = has(root, "gradlew.bat");
        out.push(Plan {
            id: "gradle",
            label: "Java or Kotlin (Gradle)",
            command: if wrapper {
                GRADLE_FETCH.into()
            } else {
                format!("gradle wrapper\n{GRADLE_FETCH}")
            },
            then: format!(
                "Gradle and everything the build needs are in .gradle-home\\ (add .gradle-home/ to .gitignore, and delete gradle\\verification-metadata.dryrun.xml, the fetch's own record). Build offline with the wrapper, always with these options: .\\gradlew.bat --offline --no-daemon --gradle-user-home .gradle-home --no-configuration-cache --console=plain build (or test, run, or another task). If the build asks for a dependency or plugin that isn't there, call fetch_dependencies again.{}",
                if wrapper {
                    ""
                } else {
                    " The project had no Gradle wrapper, so the user needed Gradle installed to create it (gradle wrapper); if they couldn't, ask them to install Gradle from gradle.org."
                }
            ),
            warning: Some(GRADLE_WARNING),
        });
    }
    if has(root, "composer.json") {
        out.push(Plan {
            id: "php",
            label: "PHP (Composer)",
            command: "composer install --no-scripts --no-plugins".into(),
            then: "The packages are in vendor\\ (load them with require 'vendor/autoload.php'). Run their tools with php, for example php vendor/bin/phpunit: batch launchers can't run in your sandbox.".to_string() + &ignore_note("vendor"),
            warning: None,
        });
    }
    out
}

/// Folders under a project that are never parts of it: tool output,
/// caches, fetched packages, and anything starting with a dot.
const NOT_PARTS: &[&str] = &[
    "vendor", "packages", "wheels", "dist", "build", "out", "bin", "obj",
];

/// How deep below the project folder parts are looked for (ADR decision
/// A4.1 of `docs/internal/PROJECT-SETUP-PLAN.md`: two levels, `apps/web`).
pub const PART_DEPTH: usize = 2;

/// At most this many folders are looked at, so a huge tree can't stall.
const MAX_PART_SCAN: usize = 2_000;

/// Sub-folders of `root` (one and two levels down, never the root itself)
/// for which `is_part` holds, as (forward-slash relative name, path).
/// Links and junctions are never followed; skipped folders
/// ([`gardenwall_fs::snapshot::SKIPPED_DIRS`], [`NOT_PARTS`], dot folders)
/// and everything below a part's own skipped folders are left out.
pub fn part_folders(root: &Path, is_part: impl Fn(&Path) -> bool) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, String, usize)> =
        std::collections::VecDeque::from([(root.to_path_buf(), String::new(), 0)]);
    let mut seen = 0usize;
    while let Some((dir, rel, depth)) = queue.pop_front() {
        if depth >= PART_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                !name.starts_with('.')
                    && !gardenwall_fs::snapshot::SKIPPED_DIRS.contains(&lower.as_str())
                    && !NOT_PARTS.contains(&lower.as_str())
            })
            .collect();
        children.sort();
        for (name, path) in children {
            seen += 1;
            if seen > MAX_PART_SCAN {
                queue.clear();
                break;
            }
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if is_part(&path) {
                out.push((child_rel.clone(), path.clone()));
            }
            queue.push_back((path, child_rel, depth + 1));
        }
    }
    out.sort();
    out
}

/// The fetch steps for each part of a project (ADR-024 follow-up A4): the
/// project folder itself first (relative name ""), then its sub-folders.
/// A sub-folder's step is left out when the folder itself already has the
/// same kind (a Cargo or npm workspace, a .NET solution or a Gradle build
/// covers its members), except Python, whose packages are listed per
/// folder.
pub fn project_plans(root: &Path) -> Vec<(String, Vec<Plan>)> {
    let top = plans(root);
    let covered: Vec<&'static str> = top
        .iter()
        .map(|p| p.id)
        .filter(|id| *id != "python")
        .collect();
    let mut out = Vec::new();
    if !top.is_empty() {
        out.push((String::new(), top));
    }
    for (rel, path) in part_folders(root, has_plans) {
        let own: Vec<Plan> = plans(&path)
            .into_iter()
            .filter(|p| !covered.contains(&p.id))
            .collect();
        if !own.is_empty() {
            out.push((rel, own));
        }
    }
    out
}

fn python_then(list: &str) -> String {
    format!(
        "The packages are in wheels\\ (built packages only, none ran any code). Install them offline into a virtual environment: python -m venv --without-pip .venv, then .venv\\Scripts\\python -m ensurepip, then .venv\\Scripts\\python -m pip install --no-index --find-links wheels -r {list}. If a package had no built version for this PC, the user's output says so: tell them, and suggest a version that has one.{}",
        ignore_note("wheels")
    )
}

/// The fetch leaves a copy of downloaded packages in the project: tens of
/// thousands of files that don't belong in Git (in testing, a commit
/// that included them took minutes and tripped the secret check on library
/// code). A project may already track the folder on purpose, so only the
/// untracked case is the agent's to fix.
fn ignore_note(folder: &str) -> String {
    format!(
        " The {folder}\\ folder is a copy of downloaded packages: if Git doesn't already track it, add /{folder}/ to .gitignore before committing."
    )
}

/// One dependency named in a Cargo.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CargoDep {
    /// The name it has in Cargo.toml (normalized like `name`).
    key: String,
    /// The package's name (its `package = "…"` when renamed), lower-case
    /// with `_` as `-`, the way Cargo compares names.
    name: String,
    /// Fetched from a registry or Git: not `path = …` or `workspace = true`
    /// (which may be a path), so it must end up in vendor\.
    fetched: bool,
}

fn crate_name(name: &str) -> String {
    name.trim()
        .trim_matches(['"', '\''])
        .to_ascii_lowercase()
        .replace('_', "-")
}

/// The value of `key = "…"` inside an inline table such as
/// `{ version = "1", package = "serde" }`.
fn inline_value<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    table
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']))
        })
}

/// The dependencies a Cargo.toml names, from a line-by-line read of its
/// dependency tables (`[dependencies]`, `[dev-dependencies]`,
/// `[target.….dependencies]`, `[workspace.dependencies]`, and
/// `[dependencies.name]` tables). Members of a workspace aren't read.
fn cargo_dependencies(manifest: &str) -> Vec<CargoDep> {
    enum Section {
        Other,
        /// A table of dependencies, one per line.
        List,
        /// `[dependencies.name]`: one dependency, its keys on the lines.
        One(usize),
    }
    let mut out: Vec<CargoDep> = Vec::new();
    let mut section = Section::Other;
    for line in manifest.lines() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_end_matches(']').trim();
            // `[workspace.dependencies]` only offers versions to members:
            // ones no member uses never reach Cargo.lock.
            section = if header.starts_with('[') || header.starts_with("workspace.") {
                Section::Other
            } else if header.ends_with("dependencies") {
                Section::List
            } else if let Some((table, name)) = header.rsplit_once('.')
                && table.ends_with("dependencies")
            {
                out.push(CargoDep {
                    key: crate_name(name),
                    name: crate_name(name),
                    fetched: true,
                });
                Section::One(out.len() - 1)
            } else {
                Section::Other
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match section {
            Section::Other => {}
            Section::List => {
                // `serde.version = "1"` is a dotted key for serde
                // (`json.package = "serde_json"` renames it).
                let (name, field) = match key.split_once('.') {
                    Some((n, f)) if !key.starts_with(['"', '\'']) => (n.trim(), Some(f.trim())),
                    _ => (key, None),
                };
                let table = value.starts_with('{');
                let renamed = match field {
                    Some("package") => Some(value.trim_matches(['"', '\''])),
                    Some(_) => None,
                    None => table.then(|| inline_value(value, "package")).flatten(),
                };
                let local = matches!(field, Some("path" | "workspace"))
                    || (table
                        && (inline_value(value, "path").is_some()
                            || inline_value(value, "workspace").is_some()));
                let key_name = crate_name(name);
                // Dotted keys of one dependency arrive on several lines:
                // they are found again by the key's own name.
                let found = out
                    .iter()
                    .position(|d| d.key == key_name)
                    .unwrap_or_else(|| {
                        out.push(CargoDep {
                            key: key_name.clone(),
                            name: key_name,
                            fetched: true,
                        });
                        out.len() - 1
                    });
                if let Some(r) = renamed {
                    out[found].name = crate_name(r);
                }
                out[found].fetched &= !local;
            }
            Section::One(i) => match key {
                "package" => out[i].name = crate_name(value),
                "path" | "workspace" => out[i].fetched = false,
                _ => {}
            },
        }
    }
    out
}

/// Largest manifest (Cargo.toml, package.json, go.mod, requirements) read.
const MAX_MANIFEST: u64 = 1024 * 1024;
/// Largest Cargo.lock read.
const MAX_LOCK: u64 = 16 * MAX_MANIFEST;

/// A project file's text, when it is a plain file no larger than `max`:
/// the project's files are the model's, so a huge one is never read.
fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The dependencies `root`'s Cargo.toml names that its Cargo.lock doesn't
/// list. Empty when either file can't be read, so an unclear case keeps
/// `--locked` (the fetch then fails rather than changing the lock file).
fn cargo_lock_missing(root: &Path) -> Vec<String> {
    let (Some(manifest), Some(lock)) = (
        read_small(&root.join("Cargo.toml"), MAX_MANIFEST),
        read_small(&root.join("Cargo.lock"), MAX_LOCK),
    ) else {
        return Vec::new();
    };
    let locked: std::collections::HashSet<String> = lock
        .lines()
        .filter_map(|l| l.trim().strip_prefix("name"))
        .filter_map(|l| l.trim_start().strip_prefix('='))
        .map(crate_name)
        .collect();
    cargo_dependencies(&manifest)
        .into_iter()
        .map(|d| d.name)
        .filter(|n| !locked.contains(n))
        .collect()
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// Whether the user's fetch for `plan_id` in `root` happened, judged only
/// from what is on disk afterwards: `Err` says what is missing. Kinds
/// whose result can't be told apart from an empty project pass.
pub fn fetched(root: &Path, plan_id: &str) -> Result<(), String> {
    let read = |name: &str| read_small(&root.join(name), MAX_MANIFEST).unwrap_or_default();
    match plan_id {
        "rust" => {
            // A workspace member's Cargo.lock is in the workspace's folder,
            // above this one: then only vendor\ is checked.
            let missing = cargo_lock_missing(root);
            if !missing.is_empty() {
                return Err(format!(
                    "Cargo.lock still doesn't list {}",
                    missing.join(", ")
                ));
            }
            let needs_vendor = cargo_dependencies(&read("Cargo.toml"))
                .iter()
                .any(|d| d.fetched);
            if needs_vendor && !has_entries(&root.join("vendor")) {
                return Err("vendor\\ is missing or empty".into());
            }
        }
        "node" => {
            let package: Value = serde_json::from_str(&read("package.json")).unwrap_or_default();
            let any = ["dependencies", "devDependencies", "optionalDependencies"]
                .iter()
                .any(|k| {
                    package
                        .get(k)
                        .and_then(Value::as_object)
                        .is_some_and(|m| !m.is_empty())
                });
            if any && !has_entries(&root.join("node_modules")) {
                return Err("node_modules is missing or empty".into());
            }
        }
        "python" => {
            let list = if has(root, "requirements.txt") {
                "requirements.txt"
            } else {
                "requirements.lock.txt"
            };
            if !has(root, list) {
                return Err(format!("there is no {list}"));
            }
            let any = read(list).lines().any(|l| {
                let l = l.trim();
                !l.is_empty() && !l.starts_with('#')
            });
            if any && !has_entries(&root.join("wheels")) {
                return Err("wheels\\ is missing or empty".into());
            }
        }
        "go" => {
            let any = read("go.mod")
                .lines()
                .any(|l| l.trim_start().starts_with("require"));
            if any && !root.join("vendor").join("modules.txt").is_file() {
                return Err("vendor\\modules.txt is missing".into());
            }
        }
        "php" if !root.join("vendor").join("autoload.php").is_file() => {
            return Err("vendor\\autoload.php is missing".into());
        }
        "gradle" if !root.join(".gradle-home").is_dir() => {
            return Err(".gradle-home\\ is missing".into());
        }
        _ => {}
    }
    Ok(())
}

/// What the model is told after the user says the fetch ran: the plan's
/// offline next step when the files are there, otherwise that it didn't
/// work (`true`: an error).
pub fn after_fetch(root: &Path, plan: &Plan) -> (String, bool) {
    match fetched(root, plan.id) {
        Ok(()) => (plan.then.clone(), false),
        Err(why) => (
            format!(
                "But Gardenwall checked the folder and the fetch didn't finish: {why}. Don't build or point the build at the packages yet. Read the user's output, if they pasted it, for the reason; then call fetch_dependencies again (Gardenwall picks the command from the project as it is now), or tell the user what is blocked."
            ),
            true,
        ),
    }
}

/// The card for the model's request, the plan behind it, and the folder it
/// runs in. Errors go back to the model.
pub fn request(
    args: &Value,
    workspaces: &Workspaces,
    after_web: bool,
    parent_folders: bool,
) -> Result<(RunRequest, Plan, PathBuf), String> {
    let folder_arg = args
        .get("folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or(".");
    let folder = match workspaces.resolve(folder_arg)? {
        Resolved::Root(id) => workspaces.absolute(id, None),
        Resolved::Inside(id, rel) => workspaces.absolute(id, Some(&rel)),
        Resolved::Outside(_) => {
            return Err(format!(
                "{folder_arg} is outside the project's folders. Dependencies can be fetched only inside them."
            ));
        }
    };
    if !folder.is_dir() {
        return Err(format!("{folder_arg} is not a folder in the project."));
    }
    let mut found = plans(&folder);
    if found.is_empty() {
        // A project with several parts (apps/web, backend): name them, so
        // the model asks once per part.
        let parts: Vec<String> = project_plans(&folder)
            .into_iter()
            .map(|(rel, plans)| {
                let base = folder_arg.trim_end_matches(['/', '\\']);
                let shown = if base == "." {
                    rel
                } else {
                    format!("{base}/{rel}")
                };
                format!(
                    "{shown} ({})",
                    plans.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                )
            })
            .collect();
        if !parts.is_empty() {
            return Err(format!(
                "{folder_arg} has no packages of its own, but these folders in it do: {}. Call fetch_dependencies once for each, with folder set to it (and kind, when a folder has more than one).",
                parts.join("; ")
            ));
        }
        return Err(format!(
            "Gardenwall has no standard fetch step for {folder_arg}: it looks for Cargo.toml, package.json, requirements.txt or uv.lock, go.mod, a .NET project file, Gradle build files or composer.json, in the folder and two levels below it. For Python, list the packages in requirements.txt first. For Maven, Gardenwall has no offline step yet: ask the user with ask_user_to_run."
        ));
    }
    let kind = args.get("kind").and_then(Value::as_str).map(str::trim);
    let plan = match kind {
        Some(k) if !k.is_empty() => found
            .iter()
            .position(|p| p.id == k)
            .map(|i| found.swap_remove(i))
            .ok_or_else(|| {
                format!(
                    "{folder_arg} has no {k} project. It has: {}.",
                    found.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                )
            })?,
        _ if found.len() > 1 => {
            return Err(format!(
                "{folder_arg} has more than one kind of project ({}). Call fetch_dependencies once per kind, with kind set.",
                found.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
            ));
        }
        _ => found.remove(0),
    };
    // Gradle can't start in the sandbox unless the user turned on "Folders
    // above the project" (ADR-025): fetching for it would be wasted.
    if plan.id == "gradle" && !parent_folders {
        return Err("Gradle builds can't run in your sandbox in this project: they need \"Folders above the project\", which only the user can turn on (the project page, or Settings → Agent). Tell the user; until then they can build with Gradle themselves, and you can still read and edit the code.".into());
    }
    // The card says the command is Gardenwall's: refuse when something in
    // the folder would make it run a program the project put there.
    if let Some(why) = hazard(&folder, plan.id) {
        return Err(format!(
            "Gardenwall won't offer the fetch for {folder_arg}. {why} Fix that (delete or rename those files; ask the user to remove .npmrc lines), then call fetch_dependencies again."
        ));
    }
    let shown = crate::display::shown(&folder);
    Ok((
        RunRequest {
            command: plan.command.clone(),
            hidden: 0,
            reason: format!(
                "Fetch this project's {} dependencies, which needs the internet. Gardenwall's standard command: install scripts off, lock file respected.",
                plan.label
            ),
            folder: shown,
            warning: plan.warning.map(str::to_string),
            after_web,
            by_gardenwall: true,
        },
        plan,
        folder,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        dir
    }

    #[test]
    fn each_kind_gets_its_fixed_command() {
        let cases: &[(&[&str], &str, &str)] = &[
            (
                &["Cargo.toml", "Cargo.lock"],
                "rust",
                "cargo vendor --locked vendor",
            ),
            (&["Cargo.toml"], "rust", "cargo vendor vendor"),
            (
                &["package.json", "package-lock.json"],
                "node",
                "npm ci --ignore-scripts",
            ),
            (&["package.json"], "node", "npm install --ignore-scripts"),
            (
                &["requirements.txt", "uv.lock"],
                "python",
                "python -m pip download --only-binary=:all: --dest wheels -r requirements.txt",
            ),
            (&["go.mod"], "go", "go mod vendor"),
            (
                &["app.csproj"],
                "dotnet",
                "dotnet restore --packages packages",
            ),
            (
                &["composer.json"],
                "php",
                "composer install --no-scripts --no-plugins",
            ),
        ];
        for (files, id, command) in cases {
            let dir = folder(files);
            let p = plans(dir.path());
            assert_eq!(p.len(), 1, "{files:?}");
            assert_eq!((p[0].id, p[0].command.as_str()), (*id, *command));
        }
        let uv = plans(folder(&["pyproject.toml", "uv.lock"]).path());
        assert!(uv[0].command.starts_with("uv export --frozen"));
        assert!(plans(folder(&["pyproject.toml"]).path()).is_empty());
        assert!(plans(folder(&["pom.xml"]).path()).is_empty());
        let gradle = &plans(folder(&["build.gradle"]).path())[0];
        assert!(gradle.command.starts_with("gradle wrapper\n"));
        assert!(gradle.warning.is_some_and(|w| w.contains("build scripts")));
        assert!(plans(folder(&["app.csproj"]).path())[0].warning.is_some());
        assert!(plans(folder(&["go.mod"]).path())[0].warning.is_none());
        let rust = &plans(folder(&["Cargo.toml"]).path())[0];
        assert!(rust.then.contains("replace-with = \"vendored-sources\""));
        // The block to copy stays last; the package copy is named for
        // .gitignore before it, for every fetch that leaves one.
        assert!(rust.then.ends_with("directory = \"vendor\""));
        for (file, copy) in [
            ("Cargo.toml", "vendor"),
            ("go.mod", "vendor"),
            ("composer.json", "vendor"),
            ("package.json", "node_modules"),
            ("requirements.txt", "wheels"),
            ("app.csproj", "packages"),
        ] {
            let then = &plans(folder(&[file]).path())[0].then;
            assert!(
                then.contains(&format!("add /{copy}/ to .gitignore")),
                "{file}: {then}"
            );
        }
    }

    #[test]
    fn the_card_is_gardenwalls_and_needs_a_kind_when_there_are_two() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("package.json"), "").unwrap();
        let mut w = Workspaces::new(dir.path().join("cp"));
        w.grant(&root).unwrap();
        assert!(
            request(&json!({}), &w, false, false)
                .unwrap_err()
                .contains("more than one")
        );
        let (card, plan, _) = request(&json!({"kind": "node"}), &w, true, false).unwrap();
        assert!(card.by_gardenwall && card.after_web);
        assert_eq!(card.command, "npm install --ignore-scripts");
        assert_eq!(plan.id, "node");
        assert!(request(&json!({"kind": "go"}), &w, false, false).is_err());
        let outside = if cfg!(windows) { "C:/Windows" } else { "/etc" };
        assert!(request(&json!({"folder": outside}), &w, false, false).is_err());
        // Gradle needs "Folders above the project" (ADR-025).
        std::fs::write(root.join("settings.gradle.kts"), "").unwrap();
        std::fs::write(root.join("gradlew.bat"), "").unwrap();
        let err = request(&json!({"kind": "gradle"}), &w, false, false).unwrap_err();
        assert!(err.contains("Folders above the project"), "{err}");
        let (card, _, _) = request(&json!({"kind": "gradle"}), &w, false, true).unwrap();
        assert!(card.warning.is_some_and(|w| w.contains("build scripts")));
        // A planted launcher would run instead of the real program.
        std::fs::write(root.join("Java.BAT"), "").unwrap();
        let err = request(&json!({"kind": "gradle"}), &w, false, true).unwrap_err();
        assert!(err.contains("Java.BAT"), "{err}");
        std::fs::remove_file(root.join("Java.BAT")).unwrap();
        std::fs::write(root.join("javascript.bat"), "").unwrap();
        assert!(request(&json!({"kind": "gradle"}), &w, false, true).is_ok());
        // npm settings that start a program.
        std::fs::write(
            root.join(".npmrc"),
            "registry=https://r.example\ngit = ./x.exe\n",
        )
        .unwrap();
        let err = request(&json!({"kind": "node"}), &w, false, true).unwrap_err();
        assert!(err.contains("sets git"), "{err}");
        std::fs::write(root.join(".npmrc"), "save-exact=true\n").unwrap();
        assert!(request(&json!({"kind": "node"}), &w, false, true).is_ok());
    }

    fn files(root: &Path, list: &[&str]) {
        for f in list {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
    }

    #[test]
    fn parts_are_found_two_levels_down_and_workspaces_cover_members() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        files(
            root,
            &[
                "Cargo.toml",
                "crates/core/Cargo.toml",
                "apps/web/package.json",
                "backend/requirements.txt",
                "tools/py/requirements.txt",
                "a/b/c/go.mod",
                "node_modules/x/package.json",
                ".hidden/package.json",
                "vendor/dep/Cargo.toml",
            ],
        );
        std::fs::write(root.join("requirements.txt"), "").unwrap();
        let found: Vec<(String, Vec<&str>)> = project_plans(root)
            .into_iter()
            .map(|(rel, p)| (rel, p.iter().map(|p| p.id).collect()))
            .collect();
        assert_eq!(
            found,
            vec![
                (String::new(), vec!["rust", "python"]),
                ("apps/web".to_string(), vec!["node"]),
                ("backend".to_string(), vec!["python"]),
                ("tools/py".to_string(), vec!["python"]),
            ],
            "the Cargo workspace covers crates/core; three levels down, caches, dot folders and vendor are left out"
        );
    }

    #[test]
    fn a_folder_without_packages_names_its_parts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        files(&root, &["apps/web/package.json", "backend/go.mod"]);
        let mut w = Workspaces::new(dir.path().join("cp"));
        w.grant(&root).unwrap();
        let err = request(&json!({}), &w, false, false).unwrap_err();
        assert!(
            err.contains("apps/web (node)") && err.contains("backend (go)"),
            "{err}"
        );
        let (card, plan, _) = request(&json!({"folder": "apps/web"}), &w, false, false).unwrap();
        assert_eq!(plan.id, "node");
        assert!(card.folder.ends_with("web"), "{}", card.folder);
    }

    #[test]
    fn cargo_dependencies_are_read_from_every_table() {
        let deps = cargo_dependencies(
            "[package]\nname = \"app\"\nversion = \"0.1.0\" # not a dep\n\n\
             [dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n\
             tokio.version = \"1\"\nlocal = { path = \"../local\" }\n\
             json = { package = \"serde_json\", version = \"1\" }\n\
             shared = { workspace = true }\n\
             [dev-dependencies]\ntempfile = \"3\"\n\
             [target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"\n\
             [build-dependencies.Cc]\nversion = \"1\"\n\
             [dependencies.mine]\npath = \"mine\"\n\
             [[bin]]\nname = \"tool\"\n\
             [features]\ndefault = []\n\
             [workspace.dependencies]\nunused = \"1\"\n\
             [build-dependencies]\njs.package = \"serde_json5\"\njs.version = \"0.2\"\n\
             near.path = \"near\"\n",
        );
        let got: Vec<(&str, bool)> = deps.iter().map(|d| (d.name.as_str(), d.fetched)).collect();
        assert_eq!(
            got,
            vec![
                ("serde", true),
                ("tokio", true),
                ("local", false),
                ("serde-json", true),
                ("shared", false),
                ("tempfile", true),
                ("windows-sys", true),
                ("cc", true),
                ("mine", false),
                ("serde-json5", true),
                ("near", false),
            ],
            "[workspace.dependencies] is left out: unused ones never reach Cargo.lock"
        );
        // A scan's quick test agrees with plans().
        for files in [
            &["Cargo.toml"][..],
            &["uv.lock"],
            &["settings.gradle.kts"],
            &["app.fsproj"],
            &["pyproject.toml"],
            &["README.md"],
        ] {
            let dir = folder(files);
            assert_eq!(
                has_plans(dir.path()),
                !plans(dir.path()).is_empty(),
                "{files:?}"
            );
        }
    }

    #[test]
    fn a_workspace_members_fetch_checks_vendor_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"member\"\n\n[dependencies]\nserde = { workspace = true }\nrand = \"0.9\"\n",
        )
        .unwrap();
        assert_eq!(
            fetched(root, "rust").unwrap_err(),
            "vendor\\ is missing or empty"
        );
        files(root, &["vendor/rand/Cargo.toml"]);
        assert_eq!(
            fetched(root, "rust"),
            Ok(()),
            "its Cargo.lock is the workspace's"
        );
        // A huge file is never read: then nothing counts as missing.
        std::fs::write(root.join("Cargo.lock"), "x".repeat((MAX_LOCK + 1) as usize)).unwrap();
        assert!(cargo_lock_missing(root).is_empty());
    }

    #[test]
    fn a_stale_lock_drops_locked_and_a_failed_fetch_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"GWTest9\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n",
        )
        .unwrap();
        // What `cargo build` leaves before serde was added.
        std::fs::write(
            root.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"GWTest9\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(cargo_lock_missing(root), vec!["serde".to_string()]);
        let rust = &plans(root)[0];
        assert_eq!(rust.command, "cargo vendor vendor");
        assert_eq!(rust.warning, Some(STALE_LOCK_WARNING));
        let (text, failed) = after_fetch(root, rust);
        assert!(
            failed && text.contains("still doesn't list serde"),
            "{text}"
        );
        assert!(!text.contains("vendored-sources"));

        std::fs::write(
            root.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"GWTest9\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.229\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        let err = fetched(root, "rust").unwrap_err();
        assert!(err.contains("vendor"), "{err}");
        std::fs::create_dir_all(root.join("vendor").join("serde")).unwrap();
        assert_eq!(fetched(root, "rust"), Ok(()));
        let rust = &plans(root)[0];
        assert_eq!(rust.command, "cargo vendor --locked vendor");
        assert!(rust.warning.is_none());
        let (text, failed) = after_fetch(root, rust);
        assert!(!failed && text.contains("vendored-sources"), "{text}");
    }

    #[test]
    fn other_kinds_are_checked_only_when_they_have_packages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("package.json"), "{\"name\": \"x\"}").unwrap();
        assert_eq!(
            fetched(root, "node"),
            Ok(()),
            "no dependencies, nothing to fetch"
        );
        std::fs::write(
            root.join("package.json"),
            "{\"dependencies\": {\"left-pad\": \"1\"}}",
        )
        .unwrap();
        assert!(fetched(root, "node").is_err());
        files(root, &["node_modules/left-pad/package.json"]);
        assert_eq!(fetched(root, "node"), Ok(()));

        std::fs::write(root.join("requirements.txt"), "# none yet\n").unwrap();
        assert_eq!(fetched(root, "python"), Ok(()));
        std::fs::write(root.join("requirements.txt"), "requests==2.32\n").unwrap();
        assert!(fetched(root, "python").is_err());
        files(root, &["wheels/requests-2.32-py3-none-any.whl"]);
        assert_eq!(fetched(root, "python"), Ok(()));

        std::fs::write(root.join("go.mod"), "module x\n\ngo 1.23\n").unwrap();
        assert_eq!(fetched(root, "go"), Ok(()));
        std::fs::write(
            root.join("go.mod"),
            "module x\n\nrequire example.com/y v1.0.0\n",
        )
        .unwrap();
        assert!(fetched(root, "go").is_err());

        assert!(fetched(root, "php").is_err());
        assert!(fetched(root, "gradle").is_err());
        assert_eq!(fetched(root, "dotnet"), Ok(()), "not checked");
    }
}
