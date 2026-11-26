//! The command boundary's file check (ADR-011 §2.5): before each command the
//! protected locations of the granted folders are noted, and afterwards any
//! that a command created or changed are moved out of the project until the
//! user decides.
//!
//! On Windows the sandbox already keeps existing protected locations
//! read-only, so this catches what it can't: a protected location that
//! didn't exist yet (`.vscode/tasks.json` where there was no `.vscode`, the
//! hooks of a fresh `git init`, a new `.venv`), a folder above one moved
//! aside and rebuilt, new entries at the top of a virtual environment's
//! `site-packages`, and every change on Linux, where Landlock can't make
//! part of a grant read-only.
//!
//! It costs a few dozen file lookups per command: only the fixed list in
//! [`gardenwall_policy::protect::PROTECTED`] is looked at, never the whole
//! project. Links are never followed; a link placed at a protected location
//! is removed, not its target.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gardenwall_policy::protect::{Guard, PROTECTED};

/// Most entries listed inside one protected folder.
const MAX_ENTRIES: usize = 500;
/// Files up to this size are kept in memory, so a change can be undone.
const MAX_KEPT_BYTES: u64 = 256 * 1024;
/// Most bytes kept per snapshot.
const MAX_SNAPSHOT_BYTES: usize = 8 << 20;

/// One place the check looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// Relative to its granted folder, for messages ("`.vscode`").
    pub shown: String,
    /// Read-only for commands (protected by the sandbox where it can).
    pub read_only: bool,
}

/// The locations to protect and watch in `roots`, from the policy list.
/// Existing virtual environments contribute their parts (`pyvenv.cfg`,
/// `Scripts`/`bin` read-only; `site-packages` watched).
/// Build and toolchain files that choose what a build runs, protected below
/// the top of a project too (ADR-022 §8): a sub-crate's `build.rs`, a
/// vendored crate's, a nested `.cargo` or MSBuild import. Found by
/// [`nested_locations`] when a session starts.
const NESTED: &[&str] = &[
    "build.rs",
    ".cargo",
    "rust-toolchain",
    "rust-toolchain.toml",
    "Directory.Build.props",
    "Directory.Build.targets",
    "global.json",
    "NuGet.config",
    "gradlew",
    "gradlew.bat",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "buildSrc",
    "mvnw",
    "mvnw.cmd",
    ".mvn",
];

/// MSBuild project and import files, by their ending
/// : `dotnet restore`, which the user runs outside the sandbox
/// to fetch packages, evaluates them, and they can run commands. Existing
/// ones are protected at the top of a project and below it; the file tools
/// still change them with the user's approval. (`obj` is skipped: restore
/// doesn't import the files there, and builds run in the sandbox.)
const MSBUILD_ENDINGS: &[&str] = &[".csproj", ".fsproj", ".vbproj", ".props", ".targets"];

/// Whether `name` is an MSBuild project or import file.
fn is_msbuild(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    MSBUILD_ENDINGS
        .iter()
        .any(|e| lower.len() > e.len() && lower.ends_with(e))
}

/// Folders the walk skips: build output and dependency caches that are
/// rebuilt, and Git's own folder (protected separately).
const NESTED_SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "obj",
    "bin",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".gradle-home",
];

/// At most this many entries are looked at, and this many nested locations
/// kept (each is a permission change at session start).
const NESTED_MAX_ENTRIES: usize = 50_000;
const NESTED_MAX: usize = 300;

/// The [`NESTED`] files and folders below the top of each root (the top is
/// covered by the policy list), and existing MSBuild project and import
/// files ([`MSBUILD_ENDINGS`]) at the top and below. Links are not followed.
pub fn nested_locations(roots: &[PathBuf]) -> Vec<Location> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    for root in roots {
        let mut dirs: Vec<(PathBuf, String)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Ok(kind) = e.file_type() else { continue };
                if kind.is_dir() && !NESTED_SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                    dirs.push((e.path(), name));
                } else if kind.is_file()
                    && is_msbuild(&name)
                    // Already on the policy list (Directory.Build.props).
                    && !PROTECTED.iter().any(|(rel, _)| rel.eq_ignore_ascii_case(&name))
                    && out.len() < NESTED_MAX
                {
                    out.push(Location {
                        path: e.path(),
                        shown: name,
                        read_only: true,
                    });
                }
            }
        }
        while let Some((dir, shown_dir)) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                seen += 1;
                if seen > NESTED_MAX_ENTRIES || out.len() >= NESTED_MAX {
                    return out;
                }
                let Ok(kind) = e.file_type() else { continue };
                if kind.is_symlink() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let shown = format!("{shown_dir}/{name}");
                // Fetched NuGet packages (`packages\`, from the .NET fetch)
                // hold hundreds of .props/.targets that restore doesn't read.
                let in_packages = shown_dir
                    .split('/')
                    .next()
                    .is_some_and(|top| top.eq_ignore_ascii_case("packages"));
                if NESTED.iter().any(|n| n.eq_ignore_ascii_case(&name))
                    || (kind.is_file() && is_msbuild(&name) && !in_packages)
                {
                    out.push(Location {
                        path: e.path(),
                        shown,
                        read_only: true,
                    });
                    continue;
                }
                if kind.is_dir() {
                    if name.eq_ignore_ascii_case("wrapper")
                        && shown_dir
                            .rsplit('/')
                            .next()
                            .is_some_and(|p| p.eq_ignore_ascii_case("gradle"))
                    {
                        out.push(Location {
                            path: e.path(),
                            shown,
                            read_only: true,
                        });
                    } else if !NESTED_SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                        dirs.push((e.path(), shown));
                    }
                }
            }
        }
    }
    out
}

pub fn locations(roots: &[PathBuf]) -> Vec<Location> {
    let mut out = Vec::new();
    for root in roots {
        let name = |rel: &str| rel.replace('\\', "/");
        // A `.git` file points Git elsewhere; it must not be redirected.
        if root.join(".git").is_file() {
            out.push(Location {
                path: root.join(".git"),
                shown: ".git".into(),
                read_only: true,
            });
        }
        for (rel, guard) in PROTECTED {
            let path = root.join(rel);
            match guard {
                Guard::ReadOnly => out.push(Location {
                    path,
                    shown: name(rel),
                    read_only: true,
                }),
                Guard::Venv => {
                    if !path.is_dir() {
                        out.push(Location {
                            path,
                            shown: name(rel),
                            read_only: false,
                        });
                        continue;
                    }
                    for part in ["pyvenv.cfg", "Scripts", "bin"] {
                        let p = path.join(part);
                        if p.exists() {
                            out.push(Location {
                                path: p,
                                shown: format!("{}/{part}", name(rel)),
                                read_only: true,
                            });
                        }
                    }
                    for sp in site_packages(&path) {
                        let shown = sp.strip_prefix(root).map_or_else(
                            |_| sp.display().to_string(),
                            |p| name(&p.to_string_lossy()),
                        );
                        out.push(Location {
                            path: sp,
                            shown,
                            read_only: false,
                        });
                    }
                }
            }
        }
    }
    out
}

/// `Lib/site-packages` (Windows) or `lib/python3.*/site-packages`.
fn site_packages(venv: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let win = venv.join("Lib").join("site-packages");
    if win.is_dir() {
        out.push(win);
    }
    if let Ok(entries) = std::fs::read_dir(venv.join("lib")) {
        for e in entries.flatten() {
            let sp = e.path().join("site-packages");
            if e.file_name().to_string_lossy().starts_with("python") && sp.is_dir() {
                out.push(sp);
            }
        }
    }
    out
}

/// What is at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Missing,
    Link,
    File {
        len: u64,
        modified: Option<std::time::SystemTime>,
        bytes: Option<Vec<u8>>,
    },
    Dir,
}

/// The state of every watched location and everything listed inside.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    states: BTreeMap<PathBuf, State>,
    /// Each path as shown in messages (`.git/hooks/pre-commit`).
    shown: BTreeMap<PathBuf, String>,
}

impl Snapshot {
    pub fn take(locations: &[Location]) -> Self {
        let mut snap = Self::default();
        let mut budget = MAX_SNAPSHOT_BYTES;
        for loc in locations {
            // site-packages: only its top level matters (`.pth` files and
            // `sitecustomize.py` run from there).
            let depth = if loc.shown.ends_with("site-packages") {
                1
            } else {
                usize::MAX
            };
            let mut stack = vec![(loc.path.clone(), 0usize)];
            let mut listed = 0usize;
            while let Some((p, d)) = stack.pop() {
                let state = state_of(&p, &mut budget);
                if state == State::Dir
                    && d < depth
                    && listed < MAX_ENTRIES
                    && let Ok(entries) = std::fs::read_dir(&p)
                {
                    for e in entries.flatten() {
                        listed += 1;
                        stack.push((e.path(), d + 1));
                        if listed >= MAX_ENTRIES {
                            break;
                        }
                    }
                }
                let suffix = p
                    .strip_prefix(&loc.path)
                    .map(|s| s.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let shown = if suffix.is_empty() {
                    loc.shown.clone()
                } else {
                    format!("{}/{suffix}", loc.shown)
                };
                snap.shown.insert(p.clone(), shown);
                snap.states.insert(p, state);
            }
        }
        snap
    }

    /// What changed since `self`, as seen in `after` (taken the same way).
    pub fn changes(&self, after: &Snapshot) -> Vec<Change> {
        let mut out = Vec::new();
        for (path, now) in &after.states {
            let before = self.states.get(path).unwrap_or(&State::Missing);
            let kind = match (before, now) {
                (_, State::Missing) => continue,
                (State::Missing, _) => ChangeKind::Created,
                (State::Dir, State::Dir) => continue,
                (
                    State::File {
                        len: a,
                        modified: ma,
                        bytes: ba,
                    },
                    State::File {
                        len: b,
                        modified: mb,
                        bytes: bb,
                    },
                ) => {
                    let same = match (ba, bb) {
                        (Some(x), Some(y)) => x == y,
                        _ => a == b && ma == mb,
                    };
                    if same {
                        continue;
                    }
                    ChangeKind::Modified
                }
                _ => ChangeKind::Replaced,
            };
            let previous = match before {
                State::File { bytes: Some(b), .. } => Some(b.clone()),
                _ => None,
            };
            out.push(Change {
                path: path.clone(),
                shown: after.shown.get(path).cloned().unwrap_or_default(),
                kind,
                link: matches!(now, State::Link),
                previous,
            });
        }
        // Parents before children; a created folder covers what's inside.
        out.sort_by(|a, b| a.path.cmp(&b.path));
        let mut kept: Vec<Change> = Vec::new();
        for c in out {
            let covered = kept.iter().any(|k| {
                k.kind == ChangeKind::Created && c.path.starts_with(&k.path) && c.path != k.path
            });
            if !covered {
                kept.push(c);
            }
        }
        kept
    }
}

fn state_of(path: &Path, budget: &mut usize) -> State {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return State::Missing;
    };
    if is_link(path, &meta) {
        return State::Link;
    }
    if meta.is_dir() {
        return State::Dir;
    }
    let len = meta.len();
    let bytes = (len <= MAX_KEPT_BYTES && (len as usize) <= *budget)
        .then(|| std::fs::read(path).ok())
        .flatten();
    if let Some(b) = &bytes {
        *budget -= b.len();
    }
    State::File {
        len,
        modified: meta.modified().ok(),
        bytes,
    }
}

/// Symlinks, junctions and other reparse points.
fn is_link(path: &Path, meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let _ = path;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    /// A file became a folder or link, or the other way round.
    Replaced,
}

/// One change a command made to a protected location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// Relative to its granted folder (`.vscode/tasks.json`).
    pub shown: String,
    pub kind: ChangeKind,
    /// The new entry is a link (only the link is ever moved, never its
    /// target).
    pub link: bool,
    /// The file's earlier contents, if small enough to keep.
    previous: Option<Vec<u8>>,
}

/// Changes moved out of the project, waiting for the user's decision.
#[derive(Debug)]
pub struct HeldBack {
    items: Vec<Held>,
}

#[derive(Debug)]
struct Held {
    change: Change,
    /// Where the new version waits (`None`: a link, removed).
    parked: Option<PathBuf>,
    /// What the new version looked like, for the approval card.
    preview: String,
    /// Whether the change really left the project (moved out, and what was
    /// there before put back). When it didn't, the change is still in the
    /// project and every message says so (review GW-004).
    contained: bool,
}

impl HeldBack {
    /// Move each change out of the project into `quarantine` (outside every
    /// grant), and put back what was there before when it was kept. A step
    /// that fails is recorded, never reported as held back.
    pub fn hold(changes: Vec<Change>, quarantine: &Path) -> Self {
        let mut items = Vec::new();
        for (i, change) in changes.into_iter().enumerate() {
            let preview = preview(&change);
            let (parked, mut contained) = if change.link {
                (None, remove_entry(&change.path))
            } else {
                let dest = quarantine.join(format!("{i:03}"));
                let moved =
                    std::fs::create_dir_all(quarantine).is_ok() && park(&change.path, &dest);
                (moved.then_some(dest), moved)
            };
            if let Some(old) = &change.previous {
                contained &= std::fs::write(&change.path, old).is_ok();
            }
            items.push(Held {
                change,
                parked,
                preview,
                contained,
            });
        }
        Self { items }
    }

    /// Add another set of held-back changes to this one.
    pub fn extend(&mut self, other: HeldBack) {
        self.items.extend(other.items);
    }

    /// Changes that couldn't be held back and are still in the project, one
    /// line each.
    pub fn not_held(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|h| !h.contained)
            .map(|h| h.change.shown.clone())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// One line per change, for messages: "`.vscode/tasks.json` (created)".
    pub fn summary(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|h| {
                let rel = &h.change.shown;
                let what = match (h.change.kind, h.change.link) {
                    (_, true) => "link created",
                    (ChangeKind::Created, _) => "created",
                    (ChangeKind::Modified, _) => "changed",
                    (ChangeKind::Replaced, _) => "replaced",
                };
                if h.contained {
                    format!("{rel} ({what})")
                } else {
                    format!("{rel} ({what}; couldn't be held back, still in the project)")
                }
            })
            .collect()
    }

    /// What the card shows: each change with the start of its new content,
    /// hidden characters made visible.
    pub fn details(&self) -> String {
        let mut out = String::new();
        for (line, h) in self.summary().iter().zip(&self.items) {
            out.push_str(line);
            out.push('\n');
            if !h.preview.is_empty() {
                out.push_str(&h.preview);
                out.push('\n');
            }
        }
        gardenwall_policy::protect::reveal_invisible(&out).0
    }

    /// The paths that were created (to protect them once kept).
    pub fn created(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|h| h.change.kind == ChangeKind::Created && !h.change.link)
            .map(|h| h.change.path.clone())
            .collect()
    }

    /// Put the changes back (the user approved). Links are not recreated.
    pub fn keep(self) {
        for h in self.items {
            if let Some(parked) = h.parked {
                remove_entry(&h.change.path);
                let _ = park(&parked, &h.change.path);
            }
        }
    }

    /// Delete the held versions (the user refused, or nobody answered). A
    /// change that couldn't be held back is removed from the project now
    /// (or its earlier contents put back). Returns the ones that are still
    /// there.
    pub fn discard(self) -> Vec<String> {
        let mut left = Vec::new();
        for h in self.items {
            if let Some(parked) = &h.parked {
                remove_entry(parked);
            }
            if h.contained {
                continue;
            }
            let fixed = match &h.change.previous {
                Some(old) => std::fs::write(&h.change.path, old).is_ok(),
                None if h.change.kind == ChangeKind::Created || h.change.link => {
                    remove_entry(&h.change.path)
                }
                // A changed file whose earlier contents weren't kept.
                None => false,
            };
            if !fixed {
                left.push(h.change.shown.clone());
            }
        }
        left
    }
}

/// Move `from` to `to` (copying when they are on different volumes).
fn park(from: &Path, to: &Path) -> bool {
    if std::fs::rename(from, to).is_ok() {
        return true;
    }
    let copied = if from.is_dir() {
        copy_dir(from, to).is_ok()
    } else {
        std::fs::copy(from, to).is_ok()
    };
    copied && remove_entry(from)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let meta = std::fs::symlink_metadata(e.path())?;
        if is_link(&e.path(), &meta) {
            continue;
        }
        if meta.is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()))?;
        } else {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Remove a file, folder or link (a link's target is never touched).
/// Whether nothing is left at `path` afterwards.
fn remove_entry(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return true;
    };
    if is_link(path, &meta) {
        // A directory junction or symlink is removed as a directory entry.
        let _ = std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path));
    } else if meta.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
    std::fs::symlink_metadata(path).is_err()
}

/// The first lines of a new text file (or a note for anything else).
fn preview(change: &Change) -> String {
    if change.link {
        let target = std::fs::read_link(&change.path)
            .map(|t| t.display().to_string())
            .unwrap_or_else(|_| "an unknown place".into());
        return format!("  a link to {target}");
    }
    let Ok(meta) = std::fs::symlink_metadata(&change.path) else {
        return String::new();
    };
    if meta.is_dir() {
        let names: Vec<String> = std::fs::read_dir(&change.path)
            .map(|d| {
                d.flatten()
                    .take(20)
                    .map(|e| format!("  {}", e.file_name().to_string_lossy()))
                    .collect()
            })
            .unwrap_or_default();
        return names.join("\n");
    }
    if meta.len() > MAX_KEPT_BYTES {
        return format!("  ({} bytes)", meta.len());
    }
    match std::fs::read(&change.path) {
        Ok(b) => match String::from_utf8(b) {
            Ok(text) => text
                .lines()
                .take(40)
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("  (binary, {} bytes)", e.as_bytes().len()),
        },
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-022 §8: build files below the top are found; build output and
    /// the top itself (covered by the policy list) are not.
    #[test]
    fn nested_build_files_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for f in [
            "build.rs",
            "crates/a/build.rs",
            "vendor/serde/build.rs",
            "app/Directory.Build.props",
            "tools/.cargo/config.toml",
            "android/gradle/wrapper/gradle-wrapper.properties",
            "target/debug/build/x/build.rs",
            "web/node_modules/y/build.rs",
            "crates/a/src/main.rs",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        let mut found: Vec<String> = nested_locations(std::slice::from_ref(&root))
            .into_iter()
            .map(|l| l.shown)
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                "android/gradle/wrapper",
                "app/Directory.Build.props",
                "crates/a/build.rs",
                "tools/.cargo",
                "vendor/serde/build.rs"
            ]
        );
    }

    /// MSBuild project and import files are protected wherever they are,
    /// except in build output and fetched NuGet packages
    /// (`dotnet restore` runs them outside the sandbox).
    #[test]
    fn msbuild_project_files_are_protected_at_any_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p");
        for f in [
            "App.sln",
            "App.csproj",
            "Directory.Build.props",
            "src/Lib/Lib.FSPROJ",
            "src/Lib/Custom.targets",
            "src/Web/Web.vbproj",
            "build/Shared.props",
            "src/Lib/obj/Lib.fsproj.nuget.g.props",
            "src/Lib/bin/Debug/x.targets",
            "packages/newtonsoft.json/13.0.3/build/n.targets",
            "src/Lib/Program.fs",
            "src/Lib/.props",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        let mut found: Vec<String> = nested_locations(std::slice::from_ref(&root))
            .into_iter()
            .map(|l| l.shown)
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                "App.csproj",
                "build/Shared.props",
                "src/Lib/Custom.targets",
                "src/Lib/Lib.FSPROJ",
                "src/Web/Web.vbproj",
            ]
        );
        // The top-level Directory.Build.props comes from the policy list.
        assert!(
            locations(std::slice::from_ref(&root))
                .iter()
                .any(|l| l.shown == "Directory.Build.props")
        );
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p");
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        std::fs::write(root.join("app.py"), "x").unwrap();
        let q = tmp.path().join("q");
        (tmp, root, q)
    }

    #[test]
    fn new_protected_files_are_held_back_and_can_be_kept_or_discarded() {
        let (_t, root, q) = setup();
        let locs = locations(std::slice::from_ref(&root));
        let before = Snapshot::take(&locs);
        std::fs::create_dir(root.join(".vscode")).unwrap();
        std::fs::write(
            root.join(".vscode").join("tasks.json"),
            "{\"runOn\":\"folderOpen\"}",
        )
        .unwrap();
        std::fs::write(root.join(".git").join("hooks").join("pre-commit"), "evil").unwrap();
        std::fs::write(root.join("app.py"), "ordinary change").unwrap();
        let changes = before.changes(&Snapshot::take(&locs));
        let shown: Vec<_> = changes.iter().map(|c| c.shown.clone()).collect();
        // The new folder covers its file; ordinary files aren't watched.
        assert_eq!(shown, [".git/hooks/pre-commit", ".vscode"], "{changes:?}");
        let held = HeldBack::hold(changes, &q);
        assert!(!root.join(".vscode").exists());
        assert!(!root.join(".git/hooks/pre-commit").exists());
        let details = held.details();
        assert!(
            details.contains(".vscode (created)") && details.contains("tasks.json"),
            "{details}"
        );
        assert!(
            details.contains(".git/hooks/pre-commit (created)"),
            "{details}"
        );
        held.keep();
        assert!(root.join(".vscode/tasks.json").exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".git/hooks/pre-commit")).unwrap(),
            "evil"
        );

        let before = Snapshot::take(&locations(std::slice::from_ref(&root)));
        std::fs::write(root.join(".vscode").join("settings.json"), "{}").unwrap();
        let held = HeldBack::hold(before.changes(&Snapshot::take(&locs)), &q.join("2"));
        assert!(!root.join(".vscode/settings.json").exists());
        held.discard();
        assert!(!root.join(".vscode/settings.json").exists());
        assert_eq!(
            std::fs::read_dir(q.join("2"))
                .map(|d| d.count())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn a_changed_protected_file_gets_its_old_contents_back() {
        let (_t, root, q) = setup();
        let locs = locations(std::slice::from_ref(&root));
        let before = Snapshot::take(&locs);
        std::fs::write(root.join(".git/config"), "[core]\nhooksPath = evil\n").unwrap();
        let held = HeldBack::hold(before.changes(&Snapshot::take(&locs)), &q);
        assert_eq!(
            std::fs::read_to_string(root.join(".git/config")).unwrap(),
            "[core]\n"
        );
        assert!(held.details().contains("hooksPath = evil"));
        held.keep();
        assert!(
            std::fs::read_to_string(root.join(".git/config"))
                .unwrap()
                .contains("evil")
        );
    }

    #[test]
    fn a_rebuilt_git_folder_is_caught() {
        let (_t, root, q) = setup();
        let locs = locations(std::slice::from_ref(&root));
        let before = Snapshot::take(&locs);
        std::fs::rename(root.join(".git"), root.join("git-old")).unwrap();
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join(".git/hooks/post-checkout"), "evil").unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        let changes = before.changes(&Snapshot::take(&locs));
        assert!(
            changes.iter().any(|c| c.path.ends_with("post-checkout")),
            "{changes:?}"
        );
        let _ = HeldBack::hold(changes, &q);
        assert!(!root.join(".git/hooks/post-checkout").exists());
    }

    #[test]
    fn venv_startup_files_are_caught() {
        let (_t, root, q) = setup();
        let sp = root.join(".venv/Lib/site-packages");
        std::fs::create_dir_all(sp.join("pkg")).unwrap();
        std::fs::write(root.join(".venv/pyvenv.cfg"), "home = x\n").unwrap();
        let locs = locations(std::slice::from_ref(&root));
        let before = Snapshot::take(&locs);
        std::fs::write(sp.join("evil.pth"), "import os").unwrap();
        std::fs::write(sp.join("pkg/mod.py"), "fine").unwrap();
        let changes = before.changes(&Snapshot::take(&locs));
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(changes[0].path.ends_with("evil.pth"));
        HeldBack::hold(changes, &q).discard();
        assert!(!sp.join("evil.pth").exists());
        assert!(sp.join("pkg/mod.py").exists());
    }

    /// A change that can't be moved out is never reported as held back
    /// (review GW-004), and refusing it removes it from the project.
    #[test]
    fn a_change_that_cant_be_held_back_says_so() {
        let (t, root, _q) = setup();
        // The quarantine path is a file, so nothing can be moved into it.
        let blocked = t.path().join("not-a-folder");
        std::fs::write(&blocked, "x").unwrap();
        let locs = locations(std::slice::from_ref(&root));
        let before = Snapshot::take(&locs);
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::write(root.join(".vscode/tasks.json"), "{}").unwrap();
        let held = HeldBack::hold(before.changes(&Snapshot::take(&locs)), &blocked);
        assert_eq!(held.not_held(), vec![".vscode".to_string()]);
        assert!(
            held.summary()[0].contains("still in the project"),
            "{:?}",
            held.summary()
        );
        assert!(root.join(".vscode/tasks.json").exists());
        assert!(held.discard().is_empty());
        assert!(!root.join(".vscode").exists());
    }

    #[test]
    fn nothing_changed_means_nothing_held() {
        let (_t, root, _q) = setup();
        let locs = locations(std::slice::from_ref(&root));
        let a = Snapshot::take(&locs);
        assert!(a.changes(&Snapshot::take(&locs)).is_empty());
    }
}
