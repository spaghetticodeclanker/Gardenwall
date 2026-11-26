//! Undo for changes made by commands.
//!
//! The agent's file tools checkpoint a file before they change it. A
//! command can change anything in the granted folder, so before each
//! command a [`FileCache`] brings its copy of the folder up to date, and
//! after the command the folder is compared with it: every file the
//! command created, changed or deleted is recorded in the turn's checkpoint
//! with its earlier contents, so Undo and the Changes panel cover it like
//! a tool edit.
//!
//! Only files whose size or modified time changed since the last command
//! are copied again, so a command costs two quick walks plus copying what
//! changed. Generated folders (`.git`, `node_modules`, `target`, virtual
//! environments…) and secret files are left out, as are files over
//! [`SnapshotLimits::max_file_bytes`]; the report names what isn't covered.
//! The copies live outside the workspace, in the app's data folder, and are
//! deleted with the cache.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use gardenwall_policy::paths::{PathClass, RelPath, classify};
use sha2::{Digest, Sha256};

use crate::FsError;
use crate::workspace::Workspace;

/// Folders never walked: tool output, caches and git's own store. Changes
/// there are not undoable (they are rebuilt, or belong to git).
pub const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".gradle",
    ".gradle-home",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    // Packages copied in by fetch_dependencies (Rust, Go, PHP): tens of
    // thousands of files that alone would turn Undo off for the session.
    "vendor",
];

#[derive(Debug, Clone, Copy)]
pub struct SnapshotLimits {
    /// More files than this and the folder isn't covered at all.
    pub max_files: usize,
    /// Larger files aren't copied (changes to them can't be undone).
    pub max_file_bytes: u64,
    /// Total size of the copies; past it, further files aren't copied.
    pub max_total_bytes: u64,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_files: 20_000,
            max_file_bytes: 4 << 20,
            max_total_bytes: 256 << 20,
        }
    }
}

/// Size and modified time: how a change is noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified_ns: u128,
}

#[derive(Debug, Clone)]
struct Known {
    stamp: Stamp,
    /// The copy's file name in the cache, or `None` when it wasn't copied
    /// (too large, or the cache was full).
    blob: Option<String>,
}

/// The folder as it was when [`FileCache::prepare`] ran.
#[derive(Debug, Clone)]
pub struct Mark {
    files: BTreeMap<String, Stamp>,
}

/// A command's changes, recorded or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandChanges {
    /// Changed files with their earlier contents (`None`: the command
    /// created it).
    pub changed: Vec<(RelPath, Option<Vec<u8>>)>,
    /// Changed files that weren't copied, so can't be put back.
    pub uncovered: Vec<String>,
}

/// Copies of one granted folder's files, kept for one session.
#[derive(Debug)]
pub struct FileCache {
    dir: PathBuf,
    limits: SnapshotLimits,
    known: HashMap<String, Known>,
    total: u64,
}

impl FileCache {
    /// A new, empty cache in `dir` (outside every workspace). Anything
    /// already there is removed.
    pub fn create(dir: &Path, limits: SnapshotLimits) -> Result<Self, FsError> {
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|e| FsError::io(dir, e))?;
        }
        fs::create_dir_all(dir).map_err(|e| FsError::io(dir, e))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            limits,
            known: HashMap::new(),
            total: 0,
        })
    }

    /// Before a command: copy every file that changed since the last time,
    /// and return the folder's state. `Err` with a reason when the folder is
    /// too large to cover.
    pub fn prepare(&mut self, workspace: &Workspace) -> Result<Mark, String> {
        let files = walk(workspace.root(), self.limits.max_files)?;
        // Forget files that are gone, and their copies.
        let gone: Vec<String> = self
            .known
            .keys()
            .filter(|p| !files.contains_key(*p))
            .cloned()
            .collect();
        for path in gone {
            self.forget(&path);
        }
        for (path, stamp) in &files {
            if self.known.get(path).is_some_and(|k| k.stamp == *stamp) {
                continue;
            }
            self.forget(path);
            let blob = self.copy(workspace, path, *stamp);
            self.known.insert(
                path.clone(),
                Known {
                    stamp: *stamp,
                    blob,
                },
            );
        }
        Ok(Mark { files })
    }

    /// After a command: what it changed since `mark`, with the earlier
    /// contents from the cache.
    pub fn changes_since(&self, workspace: &Workspace, mark: &Mark) -> CommandChanges {
        // Over the file limit now: report nothing rather than guess.
        let Ok(now) = walk(workspace.root(), self.limits.max_files) else {
            return CommandChanges::default();
        };
        let mut out = CommandChanges::default();
        let paths: std::collections::BTreeSet<&String> =
            mark.files.keys().chain(now.keys()).collect();
        for path in paths {
            let (was, is) = (mark.files.get(path), now.get(path));
            if was == is {
                continue;
            }
            let Ok(rel) = RelPath::parse(path) else {
                continue;
            };
            match was {
                None => out.changed.push((rel, None)),
                Some(_) => match self.read_copy(path) {
                    Some(bytes) => out.changed.push((rel, Some(bytes))),
                    None => out.uncovered.push(path.clone()),
                },
            }
        }
        out
    }

    fn copy(&mut self, workspace: &Workspace, path: &str, stamp: Stamp) -> Option<String> {
        if stamp.size > self.limits.max_file_bytes
            || self.total + stamp.size > self.limits.max_total_bytes
        {
            return None;
        }
        let rel = RelPath::parse(path).ok()?;
        // Through the workspace handle: a link can't make it read elsewhere.
        let bytes = workspace
            .read_optional(&rel, self.limits.max_file_bytes)
            .ok()??;
        let name: String = Sha256::digest(path.as_bytes())[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = format!("{name}.copy");
        fs::write(self.dir.join(&name), &bytes).ok()?;
        self.total += bytes.len() as u64;
        Some(name)
    }

    fn forget(&mut self, path: &str) {
        if let Some(Known {
            blob: Some(blob),
            stamp,
        }) = self.known.remove(path)
        {
            let _ = fs::remove_file(self.dir.join(blob));
            self.total = self.total.saturating_sub(stamp.size);
        }
    }

    fn read_copy(&self, path: &str) -> Option<Vec<u8>> {
        let known = self.known.get(path)?;
        let bytes = fs::read(self.dir.join(known.blob.as_ref()?)).ok()?;
        (bytes.len() as u64 == known.stamp.size).then_some(bytes)
    }
}

impl Drop for FileCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Every covered file under `root` with its stamp. Links and junctions are
/// never followed; skipped folders and secret files are left out.
fn walk(root: &Path, max_files: usize) -> Result<BTreeMap<String, Stamp>, String> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // From the folder listing itself (no link is followed, and on
            // Windows no file is opened): `symlink_metadata` on each path
            // opened every file and took ~500 ms per 10,000 files, twice
            // per agent command (2026-11-26 review; now ~10 ms).
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if is_link(&meta) {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if meta.is_dir() {
                if !SKIPPED_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                    stack.push((entry.path(), rel));
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            match RelPath::parse(&rel) {
                Ok(p) if !matches!(classify(&p), PathClass::Secret(_)) => {}
                _ => continue,
            }
            if out.len() >= max_files {
                return Err(format!("more than {max_files} files"));
            }
            let modified_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.insert(
                rel,
                Stamp {
                    size: meta.len(),
                    modified_ns,
                },
            );
        }
    }
    Ok(out)
}

#[cfg(windows)]
fn is_link(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{CheckpointStore, Retention};

    /// The cost of `walk` (twice per agent command) against a bare listing,
    /// on 10,000 files. Run with
    /// `cargo test -p gardenwall-fs --release --lib walk_cost -- --ignored --nocapture`.
    /// 2026-11-26: 15 ms against 9 ms (it was 500 ms when each file's
    /// details came from opening it).
    #[test]
    #[ignore]
    fn walk_cost() {
        let dir = tempfile::tempdir().unwrap();
        for d in 0..100 {
            let sub = dir.path().join(format!("d{d}"));
            fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                fs::write(sub.join(format!("f{f}.rs")), "x").unwrap();
            }
        }
        let t = std::time::Instant::now();
        let a = walk(dir.path(), 20_000).unwrap();
        let slow = t.elapsed();
        let t = std::time::Instant::now();
        let mut n = 0;
        let mut stack = vec![dir.path().to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let m = e.metadata().unwrap();
                if m.is_dir() {
                    stack.push(e.path());
                } else {
                    n += 1;
                    let _ = (m.len(), m.modified());
                }
            }
        }
        let fast = t.elapsed();
        eprintln!(
            "{} files: walk {slow:?}, bare listing of {n} {fast:?}",
            a.len()
        );
    }

    fn setup() -> (tempfile::TempDir, Workspace, FileCache) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/x")).unwrap();
        fs::write(root.join("src/a.txt"), "alpha").unwrap();
        fs::write(root.join("src/b.txt"), "beta").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "dep").unwrap();
        fs::write(root.join(".env"), "KEY=secret").unwrap();
        let ws = Workspace::open(&root).unwrap();
        let cache =
            FileCache::create(&dir.path().join("cache"), SnapshotLimits::default()).unwrap();
        (dir, ws, cache)
    }

    fn tick() {
        // Make sure modified times move on filesystems with coarse clocks.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    #[test]
    fn a_commands_changes_are_found_with_their_earlier_contents() {
        let (_dir, ws, mut cache) = setup();
        let root = ws.root().to_path_buf();
        let mark = cache.prepare(&ws).unwrap();
        tick();
        // What a command might do.
        fs::write(root.join("src/a.txt"), "ALPHA, changed").unwrap();
        fs::remove_file(root.join("src/b.txt")).unwrap();
        fs::write(root.join("src/c.txt"), "new").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "changed dep").unwrap();
        fs::write(root.join(".env"), "KEY=other").unwrap();
        let changes = cache.changes_since(&ws, &mark);
        let shown: Vec<(String, Option<String>)> = changes
            .changed
            .iter()
            .map(|(p, b)| {
                (
                    p.as_display(),
                    b.as_ref().map(|b| String::from_utf8_lossy(b).into_owned()),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("src/a.txt".to_string(), Some("alpha".to_string())),
                ("src/b.txt".to_string(), Some("beta".to_string())),
                ("src/c.txt".to_string(), None),
            ],
            "skipped folders and secret files are left out"
        );
        assert!(changes.uncovered.is_empty());
    }

    #[test]
    fn recorded_changes_undo_like_tool_edits() {
        let (dir, ws, mut cache) = setup();
        let root = ws.root().to_path_buf();
        let store =
            CheckpointStore::open(&dir.path().join("checkpoints"), &ws, Retention::default())
                .unwrap();
        let mut turn = store.begin_turn("run the formatter").unwrap();
        let mark = cache.prepare(&ws).unwrap();
        tick();
        fs::write(root.join("src/a.txt"), "formatted").unwrap();
        fs::write(root.join("src/new.txt"), "created").unwrap();
        fs::remove_file(root.join("src/b.txt")).unwrap();
        for (path, before) in cache.changes_since(&ws, &mark).changed {
            turn.record_known_before(&path, before.as_deref()).unwrap();
        }
        drop(turn);
        store.undo_latest(&ws).unwrap().unwrap();
        assert_eq!(fs::read_to_string(root.join("src/a.txt")).unwrap(), "alpha");
        assert_eq!(fs::read_to_string(root.join("src/b.txt")).unwrap(), "beta");
        assert!(!root.join("src/new.txt").exists());
    }

    #[test]
    fn only_changed_files_are_copied_again_and_large_ones_are_reported() {
        let (dir, ws, _) = setup();
        let root = ws.root().to_path_buf();
        fs::write(root.join("big.bin"), vec![7u8; 2048]).unwrap();
        let mut cache = FileCache::create(
            &dir.path().join("small-cache"),
            SnapshotLimits {
                max_files: 100,
                max_file_bytes: 1024,
                max_total_bytes: 1 << 20,
            },
        )
        .unwrap();
        let mark = cache.prepare(&ws).unwrap();
        let copies = cache.total;
        // Nothing changed: nothing copied again.
        let again = cache.prepare(&ws).unwrap();
        assert_eq!(cache.total, copies);
        assert_eq!(again.files, mark.files);
        tick();
        fs::write(root.join("big.bin"), vec![8u8; 2048]).unwrap();
        let changes = cache.changes_since(&ws, &mark);
        assert!(changes.changed.is_empty());
        assert_eq!(changes.uncovered, ["big.bin"]);
        // Too many files: not covered at all.
        let mut tiny = FileCache::create(
            &dir.path().join("tiny"),
            SnapshotLimits {
                max_files: 1,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
        assert!(tiny.prepare(&ws).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_not_followed() {
        let (dir, ws, mut cache) = setup();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("private.txt"), "private").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(ws.root().join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(status.status.success());
        let mark = cache.prepare(&ws).unwrap();
        assert!(mark.files.keys().all(|p| !p.starts_with("link")));
    }
}
