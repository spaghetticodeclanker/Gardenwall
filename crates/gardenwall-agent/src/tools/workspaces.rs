//! The folders granted to an agent session, with path resolution and
//! checkpointed writes.

use std::path::{Component, Path, PathBuf};

use gardenwall_fs::snapshot::{FileCache, Mark, SnapshotLimits};
use gardenwall_fs::{CheckpointStore, EntryKind, Retention, Turn, Workspace};
use gardenwall_policy::paths::RelPath;
use gardenwall_policy::tools::WorkspaceId;

/// Most files a single find/grep walk visits.
pub const MAX_WALK_FILES: usize = 100_000;

pub struct GrantedWorkspace {
    pub id: WorkspaceId,
    pub workspace: Workspace,
    store: CheckpointStore,
    turn: Option<Turn>,
    /// Copies of the folder's files for undoing commands' changes, made
    /// before the first command; `Err` once the folder turned out too large.
    commands: Option<Result<FileCache, String>>,
    /// The first copy, started in the background when the user sent a
    /// message ([`Workspaces::prepare_commands_in_background`]); the first
    /// command waits only for what's left of it.
    preparing: Option<std::thread::JoinHandle<Result<FileCache, String>>>,
}

/// The granted folders as they were before a command.
pub struct CommandMarks(Vec<(WorkspaceId, Mark)>);

/// What a command changed, as recorded for Undo.
#[derive(Debug, Default)]
pub struct CommandReport {
    /// Files recorded, so Undo and the Changes panel cover them.
    pub recorded: usize,
    /// Changed files that couldn't be recorded (too large, or the copies
    /// were full), as full paths.
    pub uncovered: Vec<String>,
    /// Folders whose command changes can't be undone at all, with why.
    pub not_covered: Vec<String>,
}

/// Where a model-supplied path points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Root(WorkspaceId),
    Inside(WorkspaceId, RelPath),
    /// Outside every granted folder (absolute, no `..`).
    Outside(PathBuf),
}

pub struct Workspaces {
    granted: Vec<GrantedWorkspace>,
    cwd: Option<WorkspaceId>,
    checkpoint_root: PathBuf,
    turn_label: String,
    /// The shell's private drive letters (`W:`…) and the folders they
    /// stand for, so a path the model copied from a command resolves to
    /// the granted folder instead of a host drive with the same letter.
    drives: Vec<(char, WorkspaceId)>,
    /// The saved transcript of the session, recorded with each checkpoint
    /// turn so a handoff can list this session's changes.
    session: Option<String>,
    /// A code-mode program is reading: `read` returns the whole file (see
    /// [`Workspaces::set_whole_reads`]).
    whole_reads: bool,
}

/// Comparable form of a path: no `\\?\` prefix, `/` separators, and
/// lowercase on Windows.
fn comparable(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// `path` with its deepest existing ancestor canonicalized and the rest
/// appended unchanged.
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The part of `real` below `root`, with `/` separators.
fn real_tail(real: &Path, root: &Path) -> String {
    let skip = comparable(root).split('/').count();
    let full = real.to_string_lossy();
    let full = full
        .strip_prefix(r"\\?\")
        .unwrap_or(&full)
        .replace('\\', "/");
    full.split('/').skip(skip).collect::<Vec<_>>().join("/")
}

impl Workspaces {
    /// `checkpoint_root` must be outside every workspace (the app data folder).
    pub fn new(checkpoint_root: PathBuf) -> Self {
        Self {
            granted: Vec::new(),
            cwd: None,
            checkpoint_root,
            turn_label: String::new(),
            drives: Vec::new(),
            session: None,
            whole_reads: false,
        }
    }

    /// Where checkpoints are kept (outside every workspace).
    pub fn checkpoint_root(&self) -> &Path {
        &self.checkpoint_root
    }

    /// Whether any folder is granted.
    pub fn has_grants(&self) -> bool {
        !self.granted.is_empty()
    }

    /// While on, `read` without `offset` or `limit` returns the whole file
    /// with no truncation note. For code-mode programs only: what they read
    /// stays in the program, and a cut file written back would lose its end.
    pub fn set_whole_reads(&mut self, on: bool) {
        self.whole_reads = on;
    }

    pub fn whole_reads(&self) -> bool {
        self.whole_reads
    }

    /// Grant a folder chosen by the user. The first one becomes the
    /// working directory.
    pub fn grant(&mut self, root: &Path) -> Result<WorkspaceId, String> {
        let workspace = Workspace::open(root).map_err(|e| e.to_string())?;
        if let Some(existing) = self
            .granted
            .iter()
            .find(|g| comparable(g.workspace.root()) == comparable(workspace.root()))
        {
            return Ok(existing.id);
        }
        let store = CheckpointStore::open(&self.checkpoint_root, &workspace, Retention::default())
            .map_err(|e| e.to_string())?;
        let id = WorkspaceId(self.granted.len() as u32);
        self.granted.push(GrantedWorkspace {
            id,
            workspace,
            store,
            turn: None,
            commands: None,
            preparing: None,
        });
        self.cwd.get_or_insert(id);
        Ok(id)
    }

    pub fn ids(&self) -> Vec<WorkspaceId> {
        self.granted.iter().map(|g| g.id).collect()
    }

    pub fn cwd(&self) -> Option<WorkspaceId> {
        self.cwd
    }

    fn get(&self, id: WorkspaceId) -> Result<&GrantedWorkspace, String> {
        self.granted
            .iter()
            .find(|g| g.id == id)
            .ok_or_else(|| "That folder has not been granted to this session.".to_string())
    }

    pub fn root(&self, id: WorkspaceId) -> Option<&Path> {
        self.get(id).ok().map(|g| g.workspace.root())
    }

    pub fn absolute(&self, id: WorkspaceId, rel: Option<&RelPath>) -> PathBuf {
        let root = self.root(id).map(Path::to_path_buf).unwrap_or_default();
        let root = PathBuf::from(
            root.to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&root.to_string_lossy())
                .to_string(),
        );
        super::join_display(&root, rel)
    }

    /// Take the shell's drive letters. A letter whose folder isn't exactly
    /// a granted folder is ignored.
    pub fn set_drives(&mut self, drives: &[(char, PathBuf)]) {
        let mut mapped = Vec::new();
        for (letter, folder) in drives {
            if let Ok(Resolved::Root(id)) = self.resolve_path(&folder.to_string_lossy()) {
                mapped.push((letter.to_ascii_uppercase(), id));
            }
        }
        self.drives = mapped;
    }

    /// `W:\src\x` → the granted folder behind `W:`, and `src\x`.
    fn on_drive<'a>(&self, raw: &'a str) -> Option<(WorkspaceId, &'a str)> {
        let mut chars = raw.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        if chars.next()? != ':' {
            return None;
        }
        let rest = &raw[2..];
        if !(rest.is_empty() || rest.starts_with(['\\', '/'])) {
            return None;
        }
        let id = self.drives.iter().find(|(l, _)| *l == letter)?.1;
        Some((id, rest.trim_start_matches(['\\', '/'])))
    }

    /// Resolve a path the model gave (relative to the working folder, or
    /// absolute).
    pub fn resolve(&self, raw: &str) -> Result<Resolved, String> {
        let raw = raw.trim();
        let raw = raw.strip_prefix('@').unwrap_or(raw);
        if let Some((id, rest)) = self.on_drive(raw) {
            if rest.is_empty() {
                return Ok(Resolved::Root(id));
            }
            let rel = RelPath::parse(rest).map_err(|e| format!("Invalid path {raw}: {e}"))?;
            return self.real(id, &rel);
        }
        self.resolve_path(raw)
    }

    /// `rel` inside folder `id`, with short (8.3) names, case and links
    /// resolved to where they really point, so the policy classifies the
    /// real file: `ENV~1` is `.env`, and a link to `.github` is `.github`.
    /// A link that leads out of every granted folder resolves outside.
    fn real(&self, id: WorkspaceId, rel: &RelPath) -> Result<Resolved, String> {
        let root = self.get(id)?.workspace.root().to_path_buf();
        let joined = rel.components().iter().fold(root, |p, c| p.join(c));
        self.locate(&joined, &rel.as_display())
    }

    /// Where an absolute `path` really is: a granted folder, a path inside
    /// one (by its real names), or outside.
    fn locate(&self, path: &Path, raw: &str) -> Result<Resolved, String> {
        // Canonicalize the deepest existing ancestor, so short (8.3)
        // names, case and links resolve to where they really point.
        let real = canonical_prefix(path);
        let target = comparable(&real);
        for g in &self.granted {
            let root = comparable(g.workspace.root());
            if target == root {
                return Ok(Resolved::Root(g.id));
            }
            if target.starts_with(&format!("{root}/")) {
                let tail = real_tail(&real, g.workspace.root());
                return RelPath::parse(&tail)
                    .map(|p| Resolved::Inside(g.id, p))
                    .map_err(|e| format!("Invalid path {raw}: {e}"));
            }
        }
        Ok(Resolved::Outside(path.to_path_buf()))
    }

    fn resolve_path(&self, raw: &str) -> Result<Resolved, String> {
        let path = Path::new(raw);
        let absolute = path.is_absolute() || raw.starts_with('/') || raw.starts_with('\\');
        if absolute {
            if path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            {
                return Err(format!(
                    "Path may not contain '.' or '..' components: {raw}"
                ));
            }
            return self.locate(path, raw);
        }
        let cwd = self
            .cwd
            .ok_or("No folder has been granted to this session.")?;
        let trimmed = raw.trim_start_matches("./").trim_start_matches(".\\");
        if trimmed.is_empty() || trimmed == "." {
            return Ok(Resolved::Root(cwd));
        }
        let rel = RelPath::parse(trimmed).map_err(|e| {
            format!("Invalid path {raw}: {e}. Use an absolute path for locations outside the granted folders.")
        })?;
        self.real(cwd, &rel)
    }

    pub fn read(&self, id: WorkspaceId, path: &RelPath, max: u64) -> Result<Vec<u8>, String> {
        self.get(id)?
            .workspace
            .read(path, max)
            .map_err(|e| e.to_string())
    }

    pub fn read_optional(
        &self,
        id: WorkspaceId,
        path: &RelPath,
        max: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        self.get(id)?
            .workspace
            .read_optional(path, max)
            .map_err(|e| e.to_string())
    }

    pub fn is_file(&self, id: WorkspaceId, path: &RelPath) -> bool {
        self.get(id)
            .ok()
            .and_then(|g| g.workspace.kind(path).ok().flatten())
            == Some(EntryKind::File)
    }

    /// Entries of a folder as `(name, is_dir)`; links are never followed.
    pub fn list(
        &self,
        id: WorkspaceId,
        path: Option<&RelPath>,
    ) -> Result<Vec<(String, bool)>, String> {
        let entries = self
            .get(id)?
            .workspace
            .list(path)
            .map_err(|e| e.to_string())?;
        Ok(entries
            .into_iter()
            .map(|e| (e.name, e.kind == EntryKind::Dir))
            .collect())
    }

    /// Files under `base`, honouring `.gitignore` and skipping `.git`.
    /// Links are not followed.
    pub fn walk_files(&self, id: WorkspaceId, base: Option<&RelPath>) -> Vec<RelPath> {
        let Ok(g) = self.get(id) else {
            return Vec::new();
        };
        let root = g.workspace.root();
        let start = super::join_display(root, base);
        let mut out = Vec::new();
        let walker = ignore::WalkBuilder::new(&start)
            .follow_links(false)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let joined: Vec<String> = rel
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => s.to_str().map(String::from),
                    _ => None,
                })
                .collect();
            if let Ok(p) = RelPath::parse(&joined.join("/")) {
                out.push(p);
            }
            if out.len() >= MAX_WALK_FILES {
                break;
            }
        }
        out.sort();
        out
    }

    /// Label used for the next checkpoint turn (e.g. the user's request).
    pub fn begin_turn(&mut self, label: &str) {
        self.turn_label = label.to_string();
        for g in &mut self.granted {
            g.turn = None;
        }
    }

    /// Close the current turn in every workspace and prune old turns.
    pub fn end_turn(&mut self) {
        for g in &mut self.granted {
            if g.turn.take().is_some() {
                let _ = g.store.prune();
            }
        }
    }

    /// Checkpoint `path`, then write it.
    pub fn write_checkpointed(
        &mut self,
        id: WorkspaceId,
        path: &RelPath,
        bytes: &[u8],
    ) -> Result<(), String> {
        let label = self.turn_label.clone();
        let g = self
            .granted
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or("That folder has not been granted to this session.")?;
        if g.turn.is_none() {
            g.turn = Some(
                g.store
                    .begin_session_turn(&label, self.session.as_deref())
                    .map_err(|e| e.to_string())?,
            );
        }
        let turn = g.turn.as_mut().ok_or("checkpoint unavailable")?;
        turn.write(&g.workspace, path, bytes)
            .map_err(|e| e.to_string())
    }

    /// Start the first copy of each folder's files for undoing commands'
    /// changes, in the background: called when
    /// the user sends a message, so the copy (up to 256 MB in a big
    /// project) runs while the model reads and writes, and the first
    /// command waits only for what's left. Does nothing for a folder whose
    /// copies exist or are being made, so calling it every turn is free.
    /// What it covers is unchanged: the next
    /// [`before_command`](Self::before_command) still copies any file
    /// changed meanwhile.
    pub fn prepare_commands_in_background(&mut self) {
        for g in &mut self.granted {
            if g.commands.is_some() || g.preparing.is_some() {
                continue;
            }
            let dir = command_copies(&self.checkpoint_root, g.id);
            let root = g.workspace.root().to_path_buf();
            let spawned = std::thread::Builder::new()
                .name("undo-copies".into())
                .spawn(move || {
                    // Its own handle on the folder: the same rules as the
                    // session's (links never followed).
                    let workspace = Workspace::open(&root).map_err(|e| e.to_string())?;
                    let mut cache = FileCache::create(&dir, SnapshotLimits::default())
                        .map_err(|e| e.to_string())?;
                    cache.prepare(&workspace)?;
                    Ok(cache)
                });
            // If no thread could start, the first command copies as before.
            g.preparing = spawned.ok();
        }
    }

    /// Before a command: bring each folder's copies up to date (only files
    /// changed since the last command are copied) and note the folders'
    /// state. A folder too large to cover is reported once by
    /// [`after_command`](Self::after_command).
    pub fn before_command(&mut self) -> CommandMarks {
        let mut marks = Vec::new();
        for g in &mut self.granted {
            // The first copy, if it was started when the user sent a message:
            // wait for whatever is left of it.
            if let Some(copying) = g.preparing.take() {
                g.commands = Some(
                    copying
                        .join()
                        .unwrap_or_else(|_| Err("copying the folder stopped".into())),
                );
            }
            if g.commands.is_none() {
                g.commands = Some(
                    FileCache::create(
                        &command_copies(&self.checkpoint_root, g.id),
                        SnapshotLimits::default(),
                    )
                    .map_err(|e| e.to_string()),
                );
            }
            let prepared = match g.commands.as_mut() {
                Some(Ok(cache)) => cache.prepare(&g.workspace),
                _ => continue,
            };
            match prepared {
                Ok(mark) => marks.push((g.id, mark)),
                Err(why) => g.commands = Some(Err(why)),
            }
        }
        CommandMarks(marks)
    }

    /// After a command: record every file it created, changed or deleted in
    /// the turn's checkpoint, with its earlier contents, so Undo and the
    /// Changes panel cover it like a tool edit.
    pub fn after_command(&mut self, marks: CommandMarks) -> CommandReport {
        let label = self.turn_label.clone();
        let session = self.session.clone();
        let mut report = CommandReport::default();
        for g in &mut self.granted {
            let root = g.workspace.root().to_string_lossy();
            let root = root.strip_prefix(r"\\?\").unwrap_or(&root).to_string();
            let cache = match &mut g.commands {
                Some(Ok(cache)) => cache,
                Some(Err(why)) => {
                    // Reported once: the reason is emptied afterwards.
                    if !why.is_empty() {
                        report.not_covered.push(format!("{root} ({why})"));
                        why.clear();
                    }
                    continue;
                }
                None => continue,
            };
            let Some((_, mark)) = marks.0.iter().find(|(id, _)| *id == g.id) else {
                continue;
            };
            let changes = cache.changes_since(&g.workspace, mark);
            for path in changes.uncovered {
                report.uncovered.push(format!("{root}/{path}"));
            }
            if changes.changed.is_empty() {
                continue;
            }
            if g.turn.is_none() {
                match g.store.begin_session_turn(&label, session.as_deref()) {
                    Ok(t) => g.turn = Some(t),
                    Err(_) => continue,
                }
            }
            let Some(turn) = g.turn.as_mut() else {
                continue;
            };
            for (path, before) in changes.changed {
                if turn.record_known_before(&path, before.as_deref()).is_ok() {
                    report.recorded += 1;
                } else {
                    report
                        .uncovered
                        .push(format!("{root}/{}", path.as_display()));
                }
            }
        }
        report
    }

    /// Name the session whose checkpoint turns these are (its saved
    /// transcript). Turns already open keep their earlier name.
    pub fn set_session(&mut self, session: Option<String>) {
        self.session = session;
    }

    /// Files this session changed through checkpointed writes (`write`,
    /// `edit`, approved operations), in turns not undone and still kept,
    /// as full paths, sorted, including what commands changed (recorded
    /// after each command). Trusted: read from the core's own checkpoints.
    pub fn changed_files(&self) -> Vec<String> {
        let Some(session) = self.session.as_deref() else {
            return Vec::new();
        };
        let mut out = std::collections::BTreeSet::new();
        for g in &self.granted {
            let Ok(history) = g.store.history() else {
                continue;
            };
            for turn in history {
                if turn.undone || turn.session.as_deref() != Some(session) {
                    continue;
                }
                for file in turn.files {
                    let full = file
                        .split('/')
                        .fold(g.workspace.root().to_path_buf(), |p, c| p.join(c));
                    let shown = full.to_string_lossy();
                    out.insert(shown.strip_prefix(r"\\?\").unwrap_or(&shown).to_string());
                }
            }
        }
        out.into_iter().collect()
    }

    /// What this session changed through checkpointed writes, per granted
    /// folder: the folder and one diff per file, from before the session
    /// first touched it to now ([`changed_files`](Self::changed_files) says
    /// what is and isn't covered).
    pub fn session_changes(&self) -> Vec<(String, Vec<gardenwall_fs::FileDiff>)> {
        let Some(session) = self.session.as_deref() else {
            return Vec::new();
        };
        self.granted
            .iter()
            .filter_map(|g| {
                let diffs = g.store.session_diff(&g.workspace, session).ok()?;
                let root = g.workspace.root().to_string_lossy();
                let root = root.strip_prefix(r"\\?\").unwrap_or(&root).to_string();
                (!diffs.is_empty()).then_some((root, diffs))
            })
            .collect()
    }

    /// Put one file back to how it was before this session first changed
    /// it (a trusted UI action, never the model). `folder` and `path` are
    /// as [`session_changes`](Self::session_changes) gave them. The revert
    /// is checkpointed as a turn of its own.
    pub fn revert_file(&self, folder: &str, path: &str) -> Result<bool, String> {
        let session = self
            .session
            .as_deref()
            .ok_or("this session has no saved changes")?;
        let g = self
            .granted
            .iter()
            .find(|g| {
                let root = g.workspace.root().to_string_lossy();
                root.strip_prefix(r"\\?\").unwrap_or(&root) == folder
            })
            .ok_or("that folder isn't part of this session")?;
        if g.turn.is_some() {
            return Err("the agent is working".into());
        }
        let path = RelPath::parse(path).map_err(|_| "that isn't a file in the folder")?;
        g.store
            .revert_session_file(&g.workspace, session, &path)
            .map_err(|e| e.to_string())
    }

    /// Undo the newest turn in a workspace.
    pub fn undo_latest(&mut self, id: WorkspaceId) -> Result<Option<Vec<String>>, String> {
        let g = self.get(id)?;
        g.store
            .undo_latest(&g.workspace)
            .map(|t| t.map(|t| t.files))
            .map_err(|e| e.to_string())
    }
}

/// A new folder for one granted folder's command copies (outside every
/// workspace; deleted with the session's cache).
fn command_copies(checkpoint_root: &Path, id: WorkspaceId) -> PathBuf {
    checkpoint_root.join("command-copies").join(format!(
        "{}-{}-{}",
        std::process::id(),
        id.0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first copy for undoing commands
    /// starts in the background when the user sends a message; the first
    /// command waits only for what's left, and covers exactly as before,
    /// including a file changed while the copy ran.
    #[test]
    fn the_first_copy_runs_in_the_background_and_covers_commands() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("a.txt"), "before").unwrap();
        let mut w = Workspaces::new(dir.path().join("cp"));
        let id = w.grant(&root).unwrap();
        w.begin_turn("change a");
        w.prepare_commands_in_background();
        assert!(w.granted[0].preparing.is_some());
        // Already started: nothing new.
        w.prepare_commands_in_background();
        // Made while the copy may still be running, before the command.
        std::fs::write(root.join("src").join("b.txt"), "made before").unwrap();
        let marks = w.before_command();
        assert!(w.granted[0].preparing.is_none());
        assert!(matches!(w.granted[0].commands, Some(Ok(_))));
        // The command changes one file and deletes the other.
        std::fs::write(root.join("src").join("a.txt"), "after").unwrap();
        std::fs::remove_file(root.join("src").join("b.txt")).unwrap();
        let report = w.after_command(marks);
        assert_eq!(report.recorded, 2, "{report:?}");
        w.end_turn();
        w.undo_latest(id).unwrap().unwrap();
        let read = |n: &str| std::fs::read_to_string(root.join("src").join(n)).unwrap();
        assert_eq!(read("a.txt"), "before");
        assert_eq!(read("b.txt"), "made before");
        // Later turns start nothing new: the copies exist.
        w.begin_turn("again");
        w.prepare_commands_in_background();
        assert!(w.granted[0].preparing.is_none());
    }
}
