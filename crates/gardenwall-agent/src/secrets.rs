//! Secret files in the granted folders, kept away from commands (ADR-011
//! §2.4).
//!
//! The file tools hide secret-pattern files (`.env`, keys, credential
//! stores) and ask before showing one. A command's output goes to the model
//! as well, so the session's sandbox loses all access to these files: a
//! command can't read, change, delete or rename them. The user's own access
//! is untouched, and the original permissions come back when the session
//! ends ([`gardenwall_sandbox::Sandbox::hide`]).
//!
//! The files are found by walking the granted folders when the shell
//! starts. Before every command after that:
//! - each watched folder's change time is compared with the one seen last;
//!   only a folder that changed (a file added, removed or renamed in it) is
//!   listed again, and a new folder in it is walked, so a secret file saved
//!   just before a command is hidden from it. Watched are the ordinary
//!   folders, shallowest first, as many as the check can look at within
//!   [`CHECK_BUDGET`] (10 ms): measured in release builds, about 2 ms on
//!   this repository (130 folders) and 7 ms on a project 2,500 folders
//!   wide (`secrets_cost`);
//! - build output and dependency caches ([`is_busy`]: `node_modules`,
//!   `target`, `build`, …), and ordinary folders past the budget, are
//!   walked again with everything else once enough time has passed that
//!   the walk costs little next to the work done since (every few seconds
//!   on a very large project), as every folder was before. They change on
//!   nearly every build, aren't where people keep secrets, and checking
//!   their thousands of folders before every command would add most of a
//!   second to each.
//!
//! Files already found are re-checked before every command, because an
//! editor that saves by replacing the file gives the new copy the folder's
//! permissions again.
//!
//! Enforced on Windows. On Linux, Landlock can't take a file out of a
//! granted folder, so nothing is hidden there.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use gardenwall_policy::paths::{PathClass, RelPath, classify};
#[cfg(any(windows, target_os = "linux"))]
use gardenwall_sandbox::Sandbox;

/// The full walk is repeated once this many times its own duration has
/// passed.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
const RESCAN_FACTOR: u32 = 20;

/// Build output and dependency folders besides
/// [`gardenwall_fs::snapshot::SKIPPED_DIRS`]: builds change them on nearly
/// every command and people don't keep secrets in them.
const BUSY_EXTRA: &[&str] = &[
    "obj", "bin", "build", "dist", "out", "coverage", "vendor", "wheels",
];

/// At most this many ordinary folders are watched (their change time looked
/// at before each command), the shallowest first; deeper ones keep the
/// timed walk. Most projects have a few hundred at most (this repository:
/// about 130, checked in 2 ms).
const MAX_WATCHED: usize = 2_000;

/// The most the check may take before a command. When it takes longer
/// (a very wide project, a slow disk), the deepest watched folders go back
/// to the timed walk until it fits, so a command is never held up by more
/// than about this.
const CHECK_BUDGET: Duration = Duration::from_millis(10);

/// The watch never shrinks below this many folders.
const MIN_WATCHED: usize = 64;

/// Build output and dependency caches, whose folders keep the timed walk.
fn is_busy(name: &str) -> bool {
    gardenwall_fs::snapshot::SKIPPED_DIRS
        .iter()
        .chain(BUSY_EXTRA)
        .any(|d| d.eq_ignore_ascii_case(name))
}

/// A folder's change time, `None` when it is gone (or can't be read).
fn changed_at(dir: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(dir)
        .ok()
        .filter(|m| m.is_dir())
        .and_then(|m| m.modified().ok())
}

/// A watched folder: its name relative to its granted folder (for the
/// secret files in it) and its change time when it was last listed.
#[derive(Debug, Clone)]
struct Stamp {
    shown: String,
    at: Option<SystemTime>,
    /// The change time was recent when it was read ([`is_recent`]): the
    /// folder is listed again at the next check even if it stays the same.
    recent: bool,
}

/// Windows takes a folder's change time from a clock that ticks every few
/// milliseconds (FAT drives keep only two seconds), so a second change in
/// the same tick as the one already seen leaves it unchanged. A folder
/// changed less than this long before it was looked at is listed again at
/// the next check; after that its time is settled.
const SETTLE: Duration = Duration::from_secs(2);

/// Whether `at` is within [`SETTLE`] of now (or in the future).
fn is_recent(at: Option<SystemTime>) -> bool {
    at.is_some_and(|t| {
        SystemTime::now()
            .duration_since(t)
            .is_ok_and(|d| d < SETTLE)
            || t > SystemTime::now()
    })
}

/// What one walk found.
#[derive(Debug, Default)]
struct Walked {
    /// Secret files in watched folders.
    watched: Vec<Secret>,
    /// Secret files in the other folders (build and dependency folders, and
    /// any beyond [`MAX_WATCHED`]).
    other: Vec<Secret>,
    /// The watched folders. Each change time is taken before the folder is
    /// listed, so a change during the listing shows up next time.
    stamps: Vec<(PathBuf, Stamp)>,
    /// How long looking at the watched folders' change times took: what
    /// the check before each command will cost.
    stamping: Duration,
}

/// Walk breadth-first from `starts` (folder, its relative name, watched).
/// Folders that aren't watched are listed only when `full`; at most `room`
/// new folders become watched. Links and junctions are not followed (a
/// grant refuses folders that contain them anyway).
fn walk_from(starts: Vec<(PathBuf, String, bool)>, full: bool, mut room: usize) -> Walked {
    let mut out = Walked::default();
    let mut queue: std::collections::VecDeque<(PathBuf, String, bool)> = starts.into();
    while let Some((dir, prefix, watched)) = queue.pop_front() {
        if watched {
            let started = Instant::now();
            let at = changed_at(&dir);
            out.stamping += started.elapsed();
            out.stamps.push((
                dir.clone(),
                Stamp {
                    shown: prefix.clone(),
                    at,
                    recent: is_recent(at),
                },
            ));
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            // A name that isn't valid Unicode is still checked (review
            // GW-003): the secret patterns are plain ASCII, so the
            // replacement characters never hide a match.
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let shown = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                let child_watched = watched && room > 0 && !is_busy(&name);
                if child_watched {
                    room -= 1;
                }
                if full || child_watched {
                    queue.push_back((entry.path(), shown, child_watched));
                }
            } else if is_secret(&shown) {
                let secret = Secret {
                    path: entry.path(),
                    shown,
                };
                if watched {
                    out.watched.push(secret);
                } else {
                    out.other.push(secret);
                }
            }
        }
    }
    out
}

/// A full walk of `roots`, watching at most `limit` folders.
fn walk_all(roots: &[PathBuf], limit: usize) -> Walked {
    let starts = roots
        .iter()
        .map(|r| (r.clone(), String::new(), true))
        .collect();
    walk_from(starts, true, limit.saturating_sub(roots.len()))
}

/// A secret file in a granted folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    pub path: PathBuf,
    /// Relative to its granted folder, with `/`, for messages.
    pub shown: String,
}

/// Every secret-pattern file under `roots`, in every folder. Links and
/// junctions are not followed.
pub fn find(roots: &[PathBuf]) -> Vec<Secret> {
    let walked = walk_all(roots, MAX_WATCHED);
    let mut out = walked.watched;
    out.extend(walked.other);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn is_secret(relative: &str) -> bool {
    RelPath::parse(relative).is_ok_and(|p| matches!(classify(&p), PathClass::Secret(_)))
}

/// The secret files of one shell session and when they were last looked for.
#[derive(Debug, Default)]
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub struct Watch {
    /// Every secret file found, sorted by path.
    found: Vec<Secret>,
    /// The ones in watched folders, kept up to date before each command.
    watched: Vec<Secret>,
    /// The ones in the other folders, from the last full walk.
    other: Vec<Secret>,
    /// The watched folders.
    stamps: std::collections::HashMap<PathBuf, Stamp>,
    /// When the last full walk ended and how long it took.
    scanned: Option<(Instant, Duration)>,
    /// How many folders may be watched ([`MAX_WATCHED`] until the check
    /// went over [`CHECK_BUDGET`]).
    limit: Option<usize>,
    /// Whether the sandbox really hides them (Windows).
    enforced: bool,
}

impl Watch {
    /// Look for new secret files: a full walk when it is due; otherwise
    /// only the watched folders whose change time moved are listed again
    /// (a new folder inside one is walked; a folder that is gone is
    /// forgotten with everything below it).
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn update(&mut self, roots: &[PathBuf]) {
        let full_due = self
            .scanned
            .is_none_or(|(at, took)| at.elapsed() >= took * RESCAN_FACTOR);
        if full_due {
            let start = Instant::now();
            let walked = walk_all(roots, self.limit());
            self.scanned = Some((Instant::now(), start.elapsed()));
            self.stamps = walked.stamps.into_iter().collect();
            self.watched = walked.watched;
            self.other = walked.other;
            // The walk timed the change-time lookups: fit the watch to the
            // budget before the first command waits on it.
            if walked.stamping > CHECK_BUDGET {
                self.fit_budget(walked.stamping);
            }
        } else {
            let checking = Instant::now();
            let mut changed: Vec<(PathBuf, Option<SystemTime>)> = self
                .stamps
                .iter()
                .filter_map(|(dir, stamp)| {
                    let now = changed_at(dir);
                    (now != stamp.at || stamp.recent).then(|| (dir.clone(), now))
                })
                .collect();
            let took = checking.elapsed();
            if took > CHECK_BUDGET {
                self.fit_budget(took);
                changed.retain(|(dir, _)| self.stamps.contains_key(dir));
            }
            if changed.is_empty() {
                return;
            }
            // Parents first, so a folder that is gone takes its children
            // with it before they are looked at.
            changed.sort_by_key(|(dir, _)| dir.components().count());
            for (dir, now) in changed {
                let Some(stamp) = self.stamps.get_mut(&dir) else {
                    continue;
                };
                if now.is_none() {
                    self.stamps.retain(|p, _| !p.starts_with(&dir));
                    self.watched.retain(|s| !s.path.starts_with(&dir));
                    continue;
                }
                stamp.at = now;
                stamp.recent = is_recent(now);
                let prefix = stamp.shown.clone();
                self.relist(&dir, &prefix);
            }
        }
        self.found = self.watched.clone();
        self.found.extend(self.other.iter().cloned());
        self.found.sort_by(|a, b| a.path.cmp(&b.path));
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(MAX_WATCHED)
    }

    /// The check took `took`, over [`CHECK_BUDGET`]: keep watching only as
    /// many folders (the shallowest) as fit in about three quarters of it.
    /// The others, and the secret files found in them, go back to the timed
    /// walk, as every folder was before.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn fit_budget(&mut self, took: Duration) {
        let count = self.stamps.len();
        let fits = (count as u128 * CHECK_BUDGET.as_nanos() * 3) / (took.as_nanos().max(1) * 4);
        let keep = usize::try_from(fits).unwrap_or(count).max(MIN_WATCHED);
        if keep >= count {
            return;
        }
        self.shrink(keep);
    }

    /// Watch only the `keep` shallowest folders from now on.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn shrink(&mut self, keep: usize) {
        let mut dirs: Vec<PathBuf> = self.stamps.keys().cloned().collect();
        dirs.sort_by_key(|d| d.components().count());
        for dir in dirs.into_iter().skip(keep) {
            self.stamps.remove(&dir);
        }
        let watched = std::mem::take(&mut self.watched);
        let (stay, moved): (Vec<Secret>, Vec<Secret>) = watched
            .into_iter()
            .partition(|s| s.path.parent().is_some_and(|p| self.stamps.contains_key(p)));
        self.watched = stay;
        self.other.extend(moved);
        self.limit = Some(keep);
    }

    /// List one watched folder again: its secret files, and a walk of any
    /// folder in it that isn't watched yet.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn relist(&mut self, dir: &Path, prefix: &str) {
        self.watched.retain(|s| s.path.parent() != Some(dir));
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut new_dirs = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let shown = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if kind.is_dir() {
                if !is_busy(&name) && !self.stamps.contains_key(&path) {
                    new_dirs.push((path, shown, true));
                }
            } else if is_secret(&shown) {
                self.watched.push(Secret { path, shown });
            }
        }
        if !new_dirs.is_empty() {
            let limit = self.limit();
            let room = limit.saturating_sub(self.stamps.len() + new_dirs.len());
            new_dirs.truncate(limit.saturating_sub(self.stamps.len()));
            let walked = walk_from(new_dirs, false, room);
            self.stamps.extend(walked.stamps);
            self.watched.extend(walked.watched);
        }
    }

    /// Hide every secret file from `sandbox`, looking for new ones first
    /// (see the module notes). An error names a file that couldn't be
    /// hidden: the caller must not run a command then.
    #[cfg(any(windows, target_os = "linux"))]
    pub fn refresh(&mut self, roots: &[PathBuf], sandbox: &mut Sandbox) -> Result<(), String> {
        self.update(roots);
        for secret in &self.found {
            if std::fs::symlink_metadata(&secret.path).is_err() {
                continue;
            }
            match sandbox.hide(&secret.path) {
                Ok(enforced) => self.enforced = enforced,
                Err(e) => {
                    return Err(format!(
                        "Commands are unavailable: `{}` is a secret file and couldn't be hidden from them ({e}).",
                        secret.shown
                    ));
                }
            }
        }
        Ok(())
    }

    /// The files being hidden, for the user.
    pub fn hidden(&self) -> &[Secret] {
        if self.enforced { &self.found } else { &[] }
    }

    /// A note for a failed command that names a hidden file, so the model
    /// doesn't try other ways to read it.
    pub fn hint(&self, command: &str) -> Option<String> {
        if !self.enforced {
            return None;
        }
        let command = command.to_lowercase();
        let secret = self.found.iter().find(|s| {
            Path::new(&s.shown)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| command.contains(&n.to_lowercase()))
        })?;
        Some(format!(
            "Note: `{}` is a secret file. Commands can't read, change or move secret files. Don't try another way; the read tool asks the user for approval.",
            secret.shown
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret file whose name isn't valid Unicode is still found (review
    /// GW-003).
    #[cfg(windows)]
    #[test]
    fn a_name_that_isnt_valid_unicode_is_still_found() {
        use std::os::windows::ffi::OsStringExt;
        let tmp = tempfile::tempdir().unwrap();
        // "x", an unpaired surrogate, then ".pem".
        let mut wide: Vec<u16> = "x".encode_utf16().collect();
        wide.push(0xD800);
        wide.extend(".pem".encode_utf16());
        let name = std::ffi::OsString::from_wide(&wide);
        std::fs::write(tmp.path().join(&name), "key").unwrap();
        let found = find(&[tmp.path().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, tmp.path().join(&name));
    }

    #[test]
    fn finds_secret_files_at_any_depth_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("api/deep/.ssh")).unwrap();
        for file in [
            ".env",
            ".env.example",
            "api/.env.production",
            "api/deep/server.pem",
            "api/deep/.ssh/work_key",
            "api/deep/.ssh/known_hosts",
            "README.md",
            "src.rs",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        let found: Vec<String> = find(&[root]).into_iter().map(|s| s.shown).collect();
        assert_eq!(
            found,
            [
                ".env",
                "api/.env.production",
                "api/deep/.ssh/work_key",
                "api/deep/server.pem",
            ]
        );
    }

    /// A secret file saved in an ordinary folder is found before the very
    /// next command, however long until the full walk is due; one in a
    /// build or dependency folder waits for the full walk
    /// .
    #[test]
    fn a_changed_folder_is_walked_again_before_the_next_command() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("api/deep")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/test.pem"), "x").unwrap();
        let roots = [root.clone()];
        let shown = |w: &Watch| w.found.iter().map(|s| s.shown.clone()).collect::<Vec<_>>();
        let mut watch = Watch::default();
        watch.update(&roots);
        assert_eq!(shown(&watch), ["node_modules/pkg/test.pem"]);
        assert!(
            watch
                .stamps
                .iter()
                .all(|(d, _)| !d.ends_with("node_modules") && !d.ends_with("pkg")),
            "build and dependency folders aren't checked every time"
        );
        // The full walk isn't due for an hour.
        watch.scanned = Some((Instant::now(), Duration::from_secs(180)));

        // Nothing changed: nothing is walked (the list stays as it was).
        watch.update(&roots);
        assert_eq!(shown(&watch), ["node_modules/pkg/test.pem"]);

        // A new secret file deep in an ordinary folder, and one in a new
        // folder: both found at once; the dependency folder's file is kept.
        std::fs::write(root.join("api/deep/.env"), "KEY=1").unwrap();
        std::fs::create_dir_all(root.join("keys")).unwrap();
        std::fs::write(root.join("keys/id_ed25519"), "k").unwrap();
        watch.update(&roots);
        assert_eq!(
            shown(&watch),
            [
                "api/deep/.env",
                "keys/id_ed25519",
                "node_modules/pkg/test.pem"
            ]
        );

        // A rename into place counts too (editors save that way).
        std::fs::write(root.join("api/settings.tmp"), "x").unwrap();
        watch.update(&roots);
        std::fs::rename(root.join("api/settings.tmp"), root.join("api/server.key")).unwrap();
        watch.update(&roots);
        assert!(shown(&watch).contains(&"api/server.key".to_string()));

        // A folder that is removed is forgotten with what was in it.
        std::fs::remove_dir_all(root.join("api/deep")).unwrap();
        watch.update(&roots);
        assert!(!shown(&watch).contains(&"api/deep/.env".to_string()));
        assert!(!watch.stamps.keys().any(|d| d.ends_with("deep")));

        // A new file in a dependency folder waits for the full walk.
        std::fs::write(root.join("node_modules/pkg/other.pem"), "x").unwrap();
        watch.update(&roots);
        assert!(!shown(&watch).contains(&"node_modules/pkg/other.pem".to_string()));
        watch.scanned = None;
        watch.update(&roots);
        assert!(shown(&watch).contains(&"node_modules/pkg/other.pem".to_string()));
    }

    /// Over the budget, the deepest folders go back to the timed walk, and
    /// the secret files already found in them stay hidden.
    #[test]
    fn a_slow_check_watches_fewer_folders_and_keeps_what_it_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut deep = root.clone();
        for i in 0..100 {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(".env"), "KEY=1").unwrap();
        let mut watch = Watch::default();
        watch.update(std::slice::from_ref(&root));
        assert_eq!(watch.stamps.len(), 101);
        let before = watch.found.clone();
        // A check that took ten times the budget.
        watch.fit_budget(CHECK_BUDGET * 10);
        assert_eq!(watch.stamps.len(), MIN_WATCHED);
        assert_eq!(watch.limit(), MIN_WATCHED);
        assert!(watch.stamps.contains_key(&root), "the shallowest stay");
        assert!(!watch.stamps.contains_key(&deep));
        assert!(watch.watched.is_empty() && watch.other.len() == 1);
        watch.scanned = Some((Instant::now(), Duration::from_secs(3600)));
        watch.update(std::slice::from_ref(&root));
        assert_eq!(watch.found, before, "still hidden");
        // The next full walk keeps to the smaller watch.
        watch.scanned = None;
        watch.update(&[root]);
        assert_eq!(watch.stamps.len(), MIN_WATCHED);
        assert_eq!(watch.found, before);
    }

    /// What the check costs per command (run on request:
    /// `cargo test -p gardenwall-agent --release --lib secrets_cost -- --ignored --nocapture`).
    /// It walks this repository (build output and `node_modules` included)
    /// and a made-up worst case past [`MAX_WATCHED`].
    #[test]
    #[ignore]
    fn secrets_cost() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let repo = std::fs::canonicalize(repo).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let wide = tmp.path().join("wide");
        for i in 0..50 {
            for j in 0..50 {
                std::fs::create_dir_all(wide.join(format!("m{i}/p{j}"))).unwrap();
            }
        }
        // A project's folders aren't all brand new: let the made-up ones
        // settle, or every one is listed again for the first two seconds.
        std::thread::sleep(SETTLE);
        for (label, root) in [("this repository", repo), ("2,550 folders", wide)] {
            let roots = [root.clone()];
            let mut watch = Watch::default();
            let t = Instant::now();
            watch.update(&roots);
            let full = t.elapsed();
            watch.scanned = Some((Instant::now(), Duration::from_secs(3600)));
            // The first check may go over the budget and shrink the watch.
            let t = Instant::now();
            watch.update(&roots);
            eprintln!("{label}: first check {:?}", t.elapsed());
            let runs = 20;
            let t = Instant::now();
            for _ in 0..runs {
                watch.update(&roots);
            }
            let unchanged = t.elapsed() / runs;
            let deep = watch
                .stamps
                .keys()
                .max_by_key(|d| d.components().count())
                .unwrap()
                .clone();
            let file = deep.join("gw-cost-check.env");
            std::fs::write(&file, "x").unwrap();
            let t = Instant::now();
            watch.update(&roots);
            let one_change = t.elapsed();
            assert!(watch.found.iter().any(|s| s.path == file));
            std::fs::remove_file(&file).unwrap();
            watch.update(&roots);
            eprintln!(
                "{label}: full walk {full:?} (still timed, as before); watched folders {}; per command, nothing changed {unchanged:?}; one new file {one_change:?}",
                watch.stamps.len()
            );
        }
    }

    #[test]
    fn the_hint_is_given_only_when_enforced_and_named() {
        let secret = Secret {
            path: PathBuf::from("x"),
            shown: "api/.env.production".into(),
        };
        let mut watch = Watch {
            found: vec![secret],
            ..Watch::default()
        };
        assert!(watch.hint("type api/.ENV.production").is_none());
        watch.enforced = true;
        assert!(
            watch
                .hint("type api/.ENV.production")
                .unwrap()
                .contains("`api/.env.production`")
        );
        assert!(watch.hint("dir").is_none());
    }
}

/// First line of the block Gardenwall adds to a `.gitignore`. Once it is
/// there the file is the user's to edit: nothing is suggested again.
pub const GITIGNORE_MARK: &str = "# Gardenwall: secret files.";

/// For a git project folder, the secret patterns its ignore rules
/// (`.gitignore` and `.git/info/exclude`) don't cover yet, as `.gitignore`
/// lines. `None` when the folder isn't a git project or already has
/// Gardenwall's block.
pub fn gitignore_missing(folder: &Path) -> Option<Vec<String>> {
    if std::fs::symlink_metadata(folder.join(".git")).is_err() {
        return None;
    }
    let file = folder.join(".gitignore");
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    if current.contains(GITIGNORE_MARK) {
        return None;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(folder);
    let _ = builder.add(&file);
    let _ = builder.add(folder.join(".git").join("info").join("exclude"));
    let rules = builder.build().ok()?;
    let lines = gardenwall_policy::paths::GITIGNORE_SECRETS;
    let mut missing: Vec<String> = lines
        .iter()
        .filter(|(_, example)| {
            example.is_some_and(|path| {
                !rules
                    .matched_path_or_any_parents(folder.join(path), false)
                    .is_ignore()
            })
        })
        .map(|(line, _)| line.to_string())
        .collect();
    // The lines that keep example files and public keys visible go with
    // the pattern they are exceptions to.
    for (line, _) in lines.iter().filter(|(_, example)| example.is_none()) {
        let base = if line.starts_with("!.env") {
            ".env.*"
        } else {
            "id_"
        };
        if let Some(at) = missing.iter().rposition(|m| m.starts_with(base)) {
            missing.insert(at + 1, line.to_string());
        }
    }
    Some(missing)
}

/// The block to append to a `.gitignore` for `lines`.
pub fn gitignore_block(lines: &[String]) -> String {
    format!(
        "{GITIGNORE_MARK} Remove a line only if you know that file is safe to publish.\n{}\n",
        lines.join("\n")
    )
}

#[cfg(test)]
mod gitignore_tests {
    use super::*;

    #[test]
    fn only_git_projects_get_the_lines_they_lack_once() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path();
        assert_eq!(gitignore_missing(folder), None);
        std::fs::create_dir_all(folder.join(".git/info")).unwrap();
        let all = gitignore_missing(folder).unwrap();
        assert_eq!(all.len(), gardenwall_policy::paths::GITIGNORE_SECRETS.len());

        // Lines already covered, here or in the private exclude file, are left out.
        std::fs::write(folder.join(".gitignore"), ".env*\n*.pem\n").unwrap();
        std::fs::write(folder.join(".git/info/exclude"), "*.key\n").unwrap();
        let some = gitignore_missing(folder).unwrap();
        for covered in [".env", ".env.*", "!.env.example", "*.pem", "*.key"] {
            assert!(!some.contains(&covered.to_string()), "{covered}");
        }
        assert!(some.contains(&"id_rsa*".to_string()) && some.contains(&"!id_*.pub".to_string()));

        // With the block added everything is covered, example files stay
        // visible, and nothing is suggested again.
        let mut text = std::fs::read_to_string(folder.join(".gitignore")).unwrap();
        text.push_str(&gitignore_block(&some));
        std::fs::write(folder.join(".gitignore"), &text).unwrap();
        assert_eq!(gitignore_missing(folder), None);
        let rules = ignore::gitignore::Gitignore::new(folder.join(".gitignore")).0;
        for (_, example) in gardenwall_policy::paths::GITIGNORE_SECRETS {
            if let Some(path) = example {
                let covered = rules
                    .matched_path_or_any_parents(folder.join(path), false)
                    .is_ignore()
                    // (`*.key` is covered by the private exclude file above.)
                    || *path == "tls/private.key";
                assert!(covered, "{path}");
            }
        }
        assert!(!rules.matched(folder.join("id_rsa.pub"), false).is_ignore());
        assert!(rules.matched(folder.join("id_rsa"), false).is_ignore());
    }
}
