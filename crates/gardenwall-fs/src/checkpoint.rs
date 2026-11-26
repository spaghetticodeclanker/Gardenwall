//! Per-turn checkpoints and undo (ADR-011 §2.2).
//!
//! Before the agent first changes a file during a turn, the file's previous
//! contents (or the fact that it did not exist) are saved outside the
//! workspace, under the app's data folder. A turn can then be reviewed as
//! diffs and undone as a whole, or one file at a time.
//!
//! Undo always goes newest-first: only the most recent turn that has not
//! been undone can be reverted, so undoing never silently discards a later
//! turn's changes to the same file.
//!
//! Layout: `<store>/<workspace-key>/<turn-number>/manifest.json` plus one
//! blob file per saved original. Blob names are generated here, never taken
//! from the workspace.

use std::fs;
use std::path::{Path, PathBuf};

use gardenwall_policy::paths::RelPath;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::FsError;
use crate::diff::{FileDiff, diff_file};
use crate::workspace::Workspace;

/// Default: keep this many turns per workspace.
pub const DEFAULT_MAX_TURNS: usize = 100;
/// Default: keep at most this many bytes of saved originals per workspace.
pub const DEFAULT_MAX_BYTES: u64 = 4 << 30;
/// Largest single file that is checkpointed. Changing a larger file is
/// refused so it can never be modified without a way back.
pub const MAX_CHECKPOINT_FILE_BYTES: u64 = 512 << 20;

#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_turns: usize,
    pub max_bytes: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Before {
    Missing,
    File { blob: String, size: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: String,
    before: Before,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    turn: u64,
    label: String,
    started_unix: u64,
    undone: bool,
    entries: Vec<Entry>,
    /// The agent session that made the turn (its saved transcript), so a
    /// session handoff lists only its own changes. Absent in older turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

/// A summary of one turn for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub turn: u64,
    pub label: String,
    pub started_unix: u64,
    pub undone: bool,
    pub files: Vec<String>,
    /// The agent session that made the turn, when it was recorded.
    pub session: Option<String>,
}

/// Checkpoints for one workspace.
#[derive(Debug)]
pub struct CheckpointStore {
    dir: PathBuf,
    retention: Retention,
}

/// An open turn: records originals before each first change.
#[derive(Debug)]
pub struct Turn {
    dir: PathBuf,
    manifest: Manifest,
}

impl CheckpointStore {
    /// Open the store for `workspace` under `store_root` (which must be
    /// outside every workspace, e.g. the app's data folder).
    pub fn open(
        store_root: &Path,
        workspace: &Workspace,
        retention: Retention,
    ) -> Result<Self, FsError> {
        fs::create_dir_all(store_root).map_err(|e| FsError::io(store_root, e))?;
        let root = fs::canonicalize(store_root).map_err(|e| FsError::io(store_root, e))?;
        if root.starts_with(workspace.root()) {
            return Err(FsError::CheckpointsInsideWorkspace);
        }
        let key: String =
            Sha256::digest(workspace.root().to_string_lossy().to_lowercase().as_bytes())[..12]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
        let dir = root.join(key);
        fs::create_dir_all(&dir).map_err(|e| FsError::io(&dir, e))?;
        Ok(Self { dir, retention })
    }

    fn turn_numbers(&self) -> Result<Vec<u64>, FsError> {
        let mut turns: Vec<u64> = fs::read_dir(&self.dir)
            .map_err(|e| FsError::io(&self.dir, e))?
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        turns.sort_unstable();
        Ok(turns)
    }

    fn load(&self, turn: u64) -> Result<Manifest, FsError> {
        let path = self.dir.join(turn.to_string()).join("manifest.json");
        let bytes = fs::read(&path).map_err(|e| FsError::io(&path, e))?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| FsError::CorruptCheckpoint(turn))?;
        if manifest.turn != turn || manifest.version != 1 {
            return Err(FsError::CorruptCheckpoint(turn));
        }
        Ok(manifest)
    }

    /// Start a new turn. `label` is shown in the history (e.g. the request).
    pub fn begin_turn(&self, label: &str) -> Result<Turn, FsError> {
        self.begin_session_turn(label, None)
    }

    /// Start a new turn made by the agent session `session` (its saved
    /// transcript's id, at most 64 characters).
    pub fn begin_session_turn(&self, label: &str, session: Option<&str>) -> Result<Turn, FsError> {
        let next = self.turn_numbers()?.last().map_or(1, |n| n + 1);
        let dir = self.dir.join(next.to_string());
        fs::create_dir(&dir).map_err(|e| FsError::io(&dir, e))?;
        let label: String = label
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let turn = Turn {
            dir,
            manifest: Manifest {
                version: 1,
                turn: next,
                label,
                started_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                undone: false,
                entries: Vec::new(),
                session: session.map(|s| s.chars().take(64).collect()),
            },
        };
        turn.save()?;
        Ok(turn)
    }

    /// Turns, newest first.
    pub fn history(&self) -> Result<Vec<TurnSummary>, FsError> {
        let mut out = Vec::new();
        for n in self.turn_numbers()?.into_iter().rev() {
            if let Ok(m) = self.load(n) {
                out.push(TurnSummary {
                    turn: m.turn,
                    label: m.label,
                    started_unix: m.started_unix,
                    undone: m.undone,
                    files: m.entries.iter().map(|e| e.path.clone()).collect(),
                    session: m.session,
                });
            }
        }
        Ok(out)
    }

    /// Diffs between each file's saved original and its current contents.
    pub fn diff(&self, workspace: &Workspace, turn: u64) -> Result<Vec<FileDiff>, FsError> {
        let m = self.load(turn)?;
        let dir = self.dir.join(turn.to_string());
        let mut out = Vec::new();
        for e in &m.entries {
            let path = RelPath::parse(&e.path).map_err(|_| FsError::CorruptCheckpoint(turn))?;
            let before = read_before(&dir, &e.before, turn)?;
            let after = workspace.read_optional(&path, MAX_CHECKPOINT_FILE_BYTES)?;
            out.push(diff_file(&e.path, before.as_deref(), after.as_deref()));
        }
        Ok(out)
    }

    /// What `session` changed in this workspace, as one diff per file:
    /// from how the file was before the session first touched it (its
    /// oldest kept turn that isn't undone) to how it is now. Files that
    /// ended up unchanged are left out. Sorted by path.
    pub fn session_diff(
        &self,
        workspace: &Workspace,
        session: &str,
    ) -> Result<Vec<FileDiff>, FsError> {
        let mut seen = std::collections::BTreeMap::new();
        for n in self.turn_numbers()? {
            let Ok(m) = self.load(n) else { continue };
            if m.undone || m.session.as_deref() != Some(session) {
                continue;
            }
            let dir = self.dir.join(n.to_string());
            for e in m.entries {
                if seen.contains_key(&e.path) {
                    continue;
                }
                let path = RelPath::parse(&e.path).map_err(|_| FsError::CorruptCheckpoint(n))?;
                let before = read_before(&dir, &e.before, n)?;
                let after = workspace.read_optional(&path, MAX_CHECKPOINT_FILE_BYTES)?;
                let diff = diff_file(&e.path, before.as_deref(), after.as_deref());
                seen.insert(e.path, diff);
            }
        }
        Ok(seen
            .into_values()
            .filter(|d| d.change != crate::diff::Change::Unchanged)
            .collect())
    }

    /// Put one file back to how it was before `session` first changed it
    /// (its oldest kept turn that isn't undone). The revert is a turn of
    /// that session itself, so nothing is lost: undoing it brings the
    /// replaced contents back. `false` when the session has no kept change
    /// to the file, or the file is already as it was.
    pub fn revert_session_file(
        &self,
        workspace: &Workspace,
        session: &str,
        path: &RelPath,
    ) -> Result<bool, FsError> {
        let display = path.as_display();
        let mut original = None;
        for n in self.turn_numbers()? {
            let Ok(m) = self.load(n) else { continue };
            if m.undone || m.session.as_deref() != Some(session) {
                continue;
            }
            if let Some(e) = m.entries.iter().find(|e| e.path == display) {
                original = Some(read_before(&self.dir.join(n.to_string()), &e.before, n)?);
                break;
            }
        }
        let Some(before) = original else {
            return Ok(false);
        };
        if before == workspace.read_optional(path, MAX_CHECKPOINT_FILE_BYTES)? {
            return Ok(false);
        }
        let mut turn = self.begin_session_turn(&format!("Put back {display}"), Some(session))?;
        match before {
            Some(bytes) => turn.write(workspace, path, &bytes)?,
            None => turn.delete(workspace, path)?,
        }
        Ok(true)
    }

    /// The newest turn that has not been undone.
    pub fn undoable_turn(&self) -> Result<Option<u64>, FsError> {
        for n in self.turn_numbers()?.into_iter().rev() {
            match self.load(n) {
                Ok(m) if m.undone => continue,
                Ok(m) => return Ok(Some(m.turn)),
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Undo the newest turn that has not been undone: every file it changed
    /// goes back to how it was before the turn.
    pub fn undo_latest(&self, workspace: &Workspace) -> Result<Option<TurnSummary>, FsError> {
        let Some(turn) = self.undoable_turn()? else {
            return Ok(None);
        };
        let mut m = self.load(turn)?;
        let dir = self.dir.join(turn.to_string());
        for e in m.entries.iter().rev() {
            restore(workspace, &dir, e, turn)?;
        }
        m.undone = true;
        save_manifest(&dir, &m)?;
        Ok(Some(TurnSummary {
            turn: m.turn,
            label: m.label,
            started_unix: m.started_unix,
            undone: true,
            files: m.entries.iter().map(|e| e.path.clone()).collect(),
            session: m.session,
        }))
    }

    /// Undo one file's changes from the newest undoable turn.
    pub fn undo_file(&self, workspace: &Workspace, path: &RelPath) -> Result<bool, FsError> {
        let Some(turn) = self.undoable_turn()? else {
            return Ok(false);
        };
        let mut m = self.load(turn)?;
        let dir = self.dir.join(turn.to_string());
        let display = path.as_display();
        let Some(index) = m.entries.iter().position(|e| e.path == display) else {
            return Ok(false);
        };
        restore(workspace, &dir, &m.entries[index], turn)?;
        let entry = m.entries.remove(index);
        if let Before::File { blob, .. } = entry.before {
            let _ = fs::remove_file(dir.join(blob));
        }
        save_manifest(&dir, &m)?;
        Ok(true)
    }

    /// Drop the oldest turns beyond the retention limits.
    pub fn prune(&self) -> Result<usize, FsError> {
        let turns = self.turn_numbers()?;
        let mut sizes: Vec<(u64, u64)> = turns
            .iter()
            .map(|&n| (n, dir_size(&self.dir.join(n.to_string()))))
            .collect();
        let mut total: u64 = sizes.iter().map(|(_, s)| s).sum();
        let mut removed = 0;
        while sizes.len() > 1
            && (sizes.len() > self.retention.max_turns || total > self.retention.max_bytes)
        {
            let (n, size) = sizes.remove(0);
            fs::remove_dir_all(self.dir.join(n.to_string()))
                .map_err(|e| FsError::io(&self.dir, e))?;
            total = total.saturating_sub(size);
            removed += 1;
        }
        Ok(removed)
    }
}

impl Turn {
    pub fn number(&self) -> u64 {
        self.manifest.turn
    }

    fn save(&self) -> Result<(), FsError> {
        save_manifest(&self.dir, &self.manifest)
    }

    /// Save `path`'s current state, once per turn, before it is changed.
    /// Must be called before every write or delete.
    pub fn record_before(&mut self, workspace: &Workspace, path: &RelPath) -> Result<(), FsError> {
        let display = path.as_display();
        if self.manifest.entries.iter().any(|e| e.path == display) {
            return Ok(());
        }
        let before = match workspace.read_optional(path, MAX_CHECKPOINT_FILE_BYTES)? {
            None => Before::Missing,
            Some(bytes) => {
                let blob = format!("{}.blob", self.manifest.entries.len());
                let target = self.dir.join(&blob);
                fs::write(&target, &bytes).map_err(|e| FsError::io(&target, e))?;
                Before::File {
                    blob,
                    size: bytes.len() as u64,
                }
            }
        };
        self.manifest.entries.push(Entry {
            path: display,
            before,
        });
        self.save()
    }

    /// Record `path`'s state from before a change that already happened (a
    /// command's, found by [`crate::snapshot::FileCache`]): `before` is its
    /// earlier contents, or `None` when it didn't exist. Once per turn, like
    /// [`Turn::record_before`]: an earlier record in the turn is older and
    /// is kept.
    pub fn record_known_before(
        &mut self,
        path: &RelPath,
        before: Option<&[u8]>,
    ) -> Result<(), FsError> {
        let display = path.as_display();
        if self.manifest.entries.iter().any(|e| e.path == display) {
            return Ok(());
        }
        let before = match before {
            None => Before::Missing,
            Some(bytes) => {
                let blob = format!("{}.blob", self.manifest.entries.len());
                let target = self.dir.join(&blob);
                fs::write(&target, bytes).map_err(|e| FsError::io(&target, e))?;
                Before::File {
                    blob,
                    size: bytes.len() as u64,
                }
            }
        };
        self.manifest.entries.push(Entry {
            path: display,
            before,
        });
        self.save()
    }

    /// Checkpoint, then write.
    pub fn write(
        &mut self,
        workspace: &Workspace,
        path: &RelPath,
        contents: &[u8],
    ) -> Result<(), FsError> {
        self.record_before(workspace, path)?;
        workspace.write(path, contents)
    }

    /// Checkpoint, then delete.
    pub fn delete(&mut self, workspace: &Workspace, path: &RelPath) -> Result<(), FsError> {
        self.record_before(workspace, path)?;
        workspace.delete(path)
    }

    pub fn files(&self) -> Vec<String> {
        self.manifest
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }
}

fn save_manifest(dir: &Path, m: &Manifest) -> Result<(), FsError> {
    let path = dir.join("manifest.json");
    let tmp = dir.join("manifest.json.tmp");
    let bytes = serde_json::to_vec_pretty(m).map_err(|_| FsError::CorruptCheckpoint(m.turn))?;
    fs::write(&tmp, bytes).map_err(|e| FsError::io(&tmp, e))?;
    fs::rename(&tmp, &path).map_err(|e| FsError::io(&path, e))
}

fn read_before(dir: &Path, before: &Before, turn: u64) -> Result<Option<Vec<u8>>, FsError> {
    match before {
        Before::Missing => Ok(None),
        Before::File { blob, size } => {
            // Blob names are generated by this module; refuse anything else.
            if !blob.ends_with(".blob") || blob.contains(['/', '\\']) || blob.contains("..") {
                return Err(FsError::CorruptCheckpoint(turn));
            }
            let bytes = fs::read(dir.join(blob)).map_err(|e| FsError::io(dir, e))?;
            if bytes.len() as u64 != *size {
                return Err(FsError::CorruptCheckpoint(turn));
            }
            Ok(Some(bytes))
        }
    }
}

fn restore(workspace: &Workspace, dir: &Path, e: &Entry, turn: u64) -> Result<(), FsError> {
    let path = RelPath::parse(&e.path).map_err(|_| FsError::CorruptCheckpoint(turn))?;
    match read_before(dir, &e.before, turn)? {
        None => workspace.delete(&path),
        Some(bytes) => workspace.write(&path, &bytes),
    }
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}
