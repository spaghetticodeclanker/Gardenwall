//! Back up and restore (release plan feature 3).
//!
//! A backup is one passphrase-locked file (`gardenwall_store::backup`)
//! holding the chats database (chats, agent sessions, projects, memories,
//! documents, drafts, visited-sites lists), the settings, the user's own
//! chat key and, only if the user ticks them, memories, the user's own
//! skills and the activity log
//! (memories and skills are opt-in). Models, runtimes, keys for web services, sandbox grants
//! and checkpoints are left out (F3.2).
//!
//! A restore never touches live data while Gardenwall runs. The backup is
//! checked whole and unpacked into `restore-staging` (the chat key wrapped
//! for this Windows account, the database opened and migrated there), then
//! Gardenwall restarts. Before anything opens, [`apply_pending_restore`]
//! sets the current files aside in `before-restore`, moves the restored ones
//! in, and opens them; if that fails, everything is moved back. The files
//! set aside are deleted only once the restored data has opened.

use std::fs;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gardenwall_store::backup::{
    self as format, BackupError, BackupReader, BackupWriter, DataCounts,
};
use gardenwall_store::conversations::ConversationStore;
use serde::{Deserialize, Serialize};
use tauri::State;
use zeroize::Zeroizing;

use crate::state::{AppState, ChatEncryption, Paths, now_ms};

type CmdResult<T> = Result<T, String>;

const EXT: &str = "gwbackup";
const WORK: &str = "backup-work";
const STAGING: &str = "restore-staging";
const BEFORE: &str = "before-restore";
const READY: &str = "READY";
const RECORD: &str = "backup.json";
const SKILLS: &str = "skills";
/// The user's skills are instructions and small resources; a folder larger
/// than this is refused rather than silently cut.
const MAX_SKILLS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SETTINGS: u64 = 64 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024;

/// What a backup holds, stored first inside it (readable only with the
/// passphrase). Counts and dates only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub app_version: String,
    pub created_ms: i64,
    pub data: DataCounts,
    /// Entries in the activity log, if it was included.
    pub activity_log: Option<u64>,
    /// Whether memories are in the backup (`data.memories` counts them).
    pub memories_included: bool,
    /// Skill files, if the skills folder was included.
    pub skills: Option<u64>,
    pub settings: bool,
    /// Whether the chats are encrypted (the key is inside).
    pub encrypted: bool,
}

/// What the user chose to add to a backup (the chats, sessions, documents
/// and settings are always in).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Include {
    pub activity_log: bool,
    pub memories: bool,
    pub skills: bool,
}

/// The backup chosen for a restore, kept in the core so the page never
/// passes a path.
#[derive(Default)]
pub struct BackupState {
    chosen: std::sync::Mutex<Option<PathBuf>>,
    busy: AtomicBool,
}

struct Busy<'a>(&'a AtomicBool);

impl<'a> Busy<'a> {
    fn claim(flag: &'a AtomicBool) -> CmdResult<Self> {
        if flag.swap(true, Ordering::SeqCst) {
            return Err("A backup or restore is already running.".into());
        }
        Ok(Self(flag))
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Record {
    last_ms: Option<i64>,
    last_file: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    pub last_ms: Option<i64>,
    pub last_file: Option<String>,
    /// A restore is unpacked and waits for a restart.
    pub restore_pending: bool,
    /// Why a backup can't be made now, if it can't.
    pub cannot_back_up: Option<String>,
    pub min_passphrase: usize,
}

fn read_record(root: &Path) -> Record {
    fs::read(root.join(RECORD))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn cannot_back_up(state: &AppState) -> Option<String> {
    match state.encryption {
        ChatEncryption::Locked => Some(
            "Your saved chats are locked (their key can't be unwrapped on this account), so a backup couldn't be read either."
                .into(),
        ),
        _ => None,
    }
}

#[tauri::command]
pub fn backup_status(state: State<'_, AppState>) -> BackupStatus {
    let record = read_record(&state.paths.root);
    BackupStatus {
        last_ms: record.last_ms,
        last_file: record.last_file,
        restore_pending: state.paths.root.join(STAGING).join(READY).exists(),
        cannot_back_up: cannot_back_up(&state),
        min_passphrase: format::MIN_PASSPHRASE_CHARS,
    }
}

fn err(e: impl std::fmt::Display) -> String {
    let text = e.to_string();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>() + ".",
        None => text,
    }
}

/// A file's name for display (never the whole path).
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupDone {
    pub file_name: String,
    pub bytes: u64,
}

/// Back up to a file the user chooses. `None` if the save dialog was
/// cancelled.
#[tauri::command]
pub async fn create_backup(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
    include: Include,
) -> CmdResult<Option<BackupDone>> {
    use tauri_plugin_dialog::DialogExt;
    let passphrase = Zeroizing::new(passphrase);
    if let Some(why) = cannot_back_up(&state) {
        return Err(why);
    }
    if !format::passphrase_ok(&passphrase) {
        return Err(err(BackupError::WeakPassphrase));
    }
    let _busy = Busy::claim(&state.backup.busy)?;
    let date = crate::diagnostics::utc(now_ms());
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(format!(
            "gardenwall-backup-{}.{EXT}",
            date.get(..10).unwrap_or("now")
        ))
        .add_filter("Gardenwall backup", &[EXT])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(dest) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let dest = dest.into_path().map_err(err)?;
    let result = {
        let state = state.inner();
        let dest = dest.clone();
        // The work is blocking (SQLite, Argon2, file I/O); `block_in_place`
        // keeps the borrowed state usable.
        tokio::task::block_in_place(|| {
            write_backup(
                state,
                &dest,
                &passphrase,
                include,
                format::KdfParams::DEFAULT,
            )
        })
    };
    match result {
        Ok(bytes) => {
            let record = Record {
                last_ms: Some(now_ms()),
                last_file: Some(file_name(&dest)),
            };
            if let Ok(json) = serde_json::to_vec(&record) {
                let _ = fs::write(state.paths.root.join(RECORD), json);
            }
            Ok(Some(BackupDone {
                file_name: file_name(&dest),
                bytes,
            }))
        }
        Err(e) => {
            crate::diagnostics::record(&state, "backup", &e);
            Err(e)
        }
    }
}

/// Remove a folder when dropped (the plain copies made for a backup).
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The user's skill files as (name inside the backup, path), regular files
/// only (links are skipped, never followed).
fn skill_files(root: &Path) -> CmdResult<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![(root.join(SKILLS), String::from(SKILLS))];
    while let Some((dir, name)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let Some(part) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let inner = format!("{name}/{part}");
            if kind.is_dir() {
                stack.push((entry.path(), inner));
            } else if kind.is_file() && safe_name(&inner) {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((inner, entry.path()));
            }
        }
    }
    if total > MAX_SKILLS_BYTES {
        return Err(format!(
            "Your skills folder is larger than {} MB, which is more than a backup takes. Remove large files from it first.",
            MAX_SKILLS_BYTES / (1024 * 1024)
        ));
    }
    out.sort();
    Ok(out)
}

fn write_backup(
    state: &AppState,
    dest: &Path,
    passphrase: &str,
    include: Include,
    kdf: format::KdfParams,
) -> CmdResult<u64> {
    let root = &state.paths.root;
    let work = root.join(WORK);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(err)?;
    let _cleanup = Cleanup(work.clone());

    // Consistent copies of the open databases.
    let db_copy = work.join("conversations.db");
    state
        .conversations
        .lock()
        .map_err(|_| "The chats are busy.".to_string())?
        .copy_to(&db_copy)
        .map_err(err)?;
    if !include.memories {
        format::remove_memories(&db_copy).map_err(err)?;
    }
    let activity_copy = work.join("approvals.db");
    if include.activity_log {
        state
            .approvals
            .lock()
            .map_err(|_| "The activity log is busy.".to_string())?
            .copy_to(&activity_copy)
            .map_err(err)?;
    }
    let key = match state.encryption {
        ChatEncryption::Encrypted => Some(
            gardenwall_keychain::load_key(&root.join("chat.key"))
                .map_err(err)?
                .ok_or("The chat key is missing.")?,
        ),
        _ => None,
    };
    let settings = serde_json::to_vec_pretty(&state.saved_settings()).map_err(err)?;
    let skills = if include.skills {
        skill_files(root)?
    } else {
        Vec::new()
    };
    let manifest = Manifest {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created_ms: now_ms(),
        data: format::count_data(&db_copy).map_err(err)?,
        activity_log: include
            .activity_log
            .then(|| format::count_activity(&activity_copy)),
        memories_included: include.memories,
        skills: include.skills.then_some(skills.len() as u64),
        settings: true,
        encrypted: key.is_some(),
    };

    // Written beside the destination, checked whole, then moved into place.
    let partial = dest.with_extension(format!("{EXT}.partial"));
    let write = || -> Result<(), BackupError> {
        let file = fs::File::create(&partial)?;
        let mut w = BackupWriter::create(BufWriter::new(file), passphrase, kdf)?;
        w.add_bytes(
            "manifest.json",
            &serde_json::to_vec(&manifest)
                .map_err(|e| BackupError::Io(std::io::Error::other(e)))?,
        )?;
        if let Some(key) = &key {
            w.add_bytes("chat.key", key.as_ref())?;
        }
        w.add_bytes("settings.json", &settings)?;
        let add_file = |w: &mut BackupWriter<BufWriter<fs::File>>, name: &str, path: &Path| {
            let mut f = fs::File::open(path)?;
            let len = f.metadata()?.len();
            w.add(name, len, &mut f)
        };
        add_file(&mut w, "conversations.db", &db_copy)?;
        if include.activity_log {
            add_file(&mut w, "approvals.db", &activity_copy)?;
        }
        for (name, path) in &skills {
            add_file(&mut w, name, path)?;
        }
        let out = w.finish()?;
        out.into_inner()
            .map_err(|e| BackupError::Io(e.into_error()))?
            .sync_all()?;
        format::verify(BufReader::new(fs::File::open(&partial)?), passphrase)?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&partial);
        return Err(format!("The backup couldn't be made: {e}."));
    }
    fs::rename(&partial, dest).map_err(|e| {
        let _ = fs::remove_file(&partial);
        format!("The backup couldn't be saved: {e}.")
    })?;
    Ok(fs::metadata(dest).map(|m| m.len()).unwrap_or(0))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChosenBackup {
    pub file_name: String,
    pub bytes: u64,
}

/// Choose a backup file to restore. `None` if the dialog was cancelled.
#[tauri::command]
pub async fn choose_backup(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ChosenBackup>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Gardenwall backup", &[EXT])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = path.into_path().map_err(err)?;
    let file = fs::File::open(&path).map_err(err)?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    format::check_header(BufReader::new(file)).map_err(err)?;
    *state.backup.chosen.lock().map_err(err)? = Some(path.clone());
    Ok(Some(ChosenBackup {
        file_name: file_name(&path),
        bytes,
    }))
}

fn chosen(state: &AppState) -> CmdResult<PathBuf> {
    state
        .backup
        .chosen
        .lock()
        .ok()
        .and_then(|c| c.clone())
        .ok_or_else(|| "Choose a backup file first.".to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupContents {
    pub backup: Manifest,
    /// What is here now (replaced by a restore).
    pub current: DataCounts,
    pub current_activity: u64,
}

fn read_manifest<R: Read>(reader: &mut BackupReader<R>) -> CmdResult<Manifest> {
    let first = reader.next_entry().map_err(err)?;
    if first.as_ref().map(|e| e.name.as_str()) != Some("manifest.json") {
        return Err(err(BackupError::Damaged("its summary is missing")));
    }
    let bytes = reader.read_entry(MAX_MANIFEST).map_err(err)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err(BackupError::Damaged("its summary is unreadable")))
}

/// Check the passphrase and show what the chosen backup holds.
#[tauri::command]
pub async fn inspect_backup(
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<BackupContents> {
    let passphrase = Zeroizing::new(passphrase);
    let path = chosen(&state)?;
    let _busy = Busy::claim(&state.backup.busy)?;
    tokio::task::block_in_place(|| {
        let file = fs::File::open(&path).map_err(err)?;
        let mut reader = BackupReader::open(BufReader::new(file), &passphrase).map_err(err)?;
        let backup = read_manifest(&mut reader)?;
        let root = &state.paths.root;
        Ok(BackupContents {
            backup,
            current: format::count_data(&root.join("conversations.db")).unwrap_or_default(),
            current_activity: format::count_activity(&root.join("approvals.db")),
        })
    })
}

/// Whether `name` is a safe relative path inside the backup: `/`-separated
/// plain components, nothing that could leave the folder it is unpacked to.
fn safe_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    parts.len() <= 16
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 255
                && *p != "."
                && *p != ".."
                && !p.ends_with('.')
                && !p.ends_with(' ')
                && !gardenwall_runtime::extract::is_device_name(p)
                && !p.chars().any(|c| {
                    c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
}

/// What the startup step replaces, written into `READY`.
#[derive(Serialize, Deserialize)]
struct Pending {
    manifest: Manifest,
    /// Names (in the data folder) replaced by staged ones.
    replace: Vec<String>,
    /// Names only set aside (e.g. a database's old journal files).
    set_aside: Vec<String>,
}

/// Check the whole backup and unpack it for the next start.
#[tauri::command]
pub async fn restore_backup(state: State<'_, AppState>, passphrase: String) -> CmdResult<()> {
    let passphrase = Zeroizing::new(passphrase);
    let path = chosen(&state)?;
    let _busy = Busy::claim(&state.backup.busy)?;
    // This computer's memories, kept if the backup has none (they are
    // sealed again with the backup's key).
    let current_memories = state
        .conversations
        .lock()
        .ok()
        .and_then(|c| c.list_memories().ok())
        .unwrap_or_default();
    let result =
        tokio::task::block_in_place(|| stage(&state.paths, &path, &passphrase, &current_memories));
    if let Err(e) = &result {
        let _ = fs::remove_dir_all(state.paths.root.join(STAGING));
        crate::diagnostics::record(&state, "restore", e);
    }
    result
}

fn stage(
    paths: &Paths,
    backup: &Path,
    passphrase: &str,
    current_memories: &[gardenwall_store::conversations::Memory],
) -> CmdResult<()> {
    let root = &paths.root;
    let staging = root.join(STAGING);
    let _ = fs::remove_dir_all(&staging);
    // Left over from a restore whose data opened but couldn't be deleted.
    let _ = fs::remove_dir_all(root.join(BEFORE));
    fs::create_dir_all(&staging).map_err(err)?;

    let file = fs::File::open(backup).map_err(err)?;
    let mut reader = BackupReader::open(BufReader::new(file), passphrase).map_err(err)?;
    let manifest = read_manifest(&mut reader)?;
    let mut key: Option<Zeroizing<[u8; 32]>> = None;
    let mut have = std::collections::BTreeSet::new();
    while let Some(entry) = reader.next_entry().map_err(err)? {
        let name = entry.name.clone();
        if !have.insert(name.clone()) {
            return Err(err(BackupError::Damaged("an entry appears twice")));
        }
        match name.as_str() {
            "chat.key" => {
                let bytes = reader.read_entry(32).map_err(err)?;
                let k: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| err(BackupError::Damaged("its key has the wrong length")))?;
                key = Some(Zeroizing::new(k));
            }
            "settings.json" => {
                let bytes = reader.read_entry(MAX_SETTINGS).map_err(err)?;
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|_| err(BackupError::Damaged("its settings are unreadable")))?;
                fs::write(staging.join("settings.json"), &*bytes).map_err(err)?;
            }
            "conversations.db" | "approvals.db" => {
                let mut out = BufWriter::new(fs::File::create(staging.join(&name)).map_err(err)?);
                reader.copy_entry(&mut out).map_err(err)?;
                out.into_inner()
                    .map_err(|e| err(e.into_error()))?
                    .sync_all()
                    .map_err(err)?;
            }
            other if other.starts_with("skills/") && safe_name(other) => {
                let dest = staging.join(other.replace('/', std::path::MAIN_SEPARATOR_STR));
                if let Some(dir) = dest.parent() {
                    fs::create_dir_all(dir).map_err(err)?;
                }
                // New files only: two names for one file (they differ only
                // in case) make the backup damaged, not a silent overwrite.
                let file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dest)
                    .map_err(|e| {
                        if e.kind() == std::io::ErrorKind::AlreadyExists {
                            err(BackupError::Damaged("it names one skills file twice"))
                        } else {
                            err(e)
                        }
                    })?;
                let mut out = BufWriter::new(file);
                reader.copy_entry(&mut out).map_err(err)?;
            }
            // Entries a later version may add are skipped.
            _ => {}
        }
    }
    if !have.contains("conversations.db") {
        return Err(err(BackupError::Damaged("it holds no chats database")));
    }
    if manifest.encrypted && key.is_none() {
        return Err(err(BackupError::Damaged("its chat key is missing")));
    }

    // The key, wrapped for this account, then a trial open (which also
    // brings an older database up to date, and refuses a newer one).
    let cipher = match &key {
        Some(k) => {
            gardenwall_keychain::store_key(&staging.join("chat.key"), k).map_err(|e| {
                format!("The restored chat key couldn't be protected on this computer ({e}).")
            })?;
            Some(gardenwall_store::crypto::FieldCipher::new(k))
        }
        None => None,
    };
    {
        let store = ConversationStore::open_with(&staging.join("conversations.db"), cipher)
            .map_err(|e| format!("The restored chats couldn't be opened ({e})."))?;
        if store.is_locked() {
            return Err(
                "The restored chats couldn't be unlocked with the key in the backup.".into(),
            );
        }
        if !manifest.memories_included {
            store
                .keep_memories(current_memories)
                .map_err(|e| format!("Your memories couldn't be kept ({e})."))?;
        }
    }
    if have.contains("approvals.db") {
        gardenwall_store::approvals::ApprovalStore::open(&staging.join("approvals.db"), now_ms())
            .map_err(|e| format!("The restored activity log couldn't be opened ({e})."))?;
    }

    let mut replace = vec!["conversations.db".to_string()];
    let mut set_aside = vec![
        "conversations.db-wal".to_string(),
        "conversations.db-shm".to_string(),
    ];
    if key.is_some() {
        replace.push("chat.key".into());
    }
    if have.contains("settings.json") {
        replace.push("settings.json".into());
    }
    if have.contains("approvals.db") {
        replace.push("approvals.db".into());
        set_aside.extend(["approvals.db-wal".into(), "approvals.db-shm".into()]);
    }
    // Skills left out of the backup (or none in it): the ones here stay.
    if manifest.skills.is_some() && staging.join(SKILLS).exists() {
        replace.push(SKILLS.into());
    }
    // Journal files the trial opens left in staging are part of the copy.
    for name in [
        "conversations.db-wal",
        "conversations.db-shm",
        "approvals.db-wal",
        "approvals.db-shm",
    ] {
        if staging.join(name).exists() && !replace.iter().any(|r| r == name) {
            set_aside.retain(|s| s != name);
            replace.push(name.into());
        }
    }
    let pending = Pending {
        manifest,
        replace,
        set_aside,
    };
    fs::write(
        staging.join(READY),
        serde_json::to_vec(&pending).map_err(err)?,
    )
    .map_err(err)?;
    Ok(())
}

/// Throw away an unpacked restore (the user chose not to restart).
#[tauri::command]
pub fn cancel_restore(state: State<'_, AppState>) -> CmdResult<()> {
    let staging = state.paths.root.join(STAGING);
    match fs::remove_dir_all(&staging) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err(e)),
    }
}

/// Restart Gardenwall so an unpacked restore takes effect.
#[tauri::command]
pub async fn restart_to_restore(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    if !state.paths.root.join(STAGING).join(READY).exists() {
        return Err("There is no restore waiting.".into());
    }
    crate::state::shutdown_servers(&state).await;
    crate::diagnostics::end_session(&state.paths.root);
    app.restart();
}

/// Rename, retrying for a few seconds: just after a restart the previous
/// process may still hold the database open.
fn rename_patiently(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 30 && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(150));
                let _ = e;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Finish an unpacked restore at startup, before any store opens. Safe to
/// run again after a crash part-way: each step checks what is already done.
pub fn apply_pending_restore(root: &Path, notices: &mut Vec<String>) {
    let staging = root.join(STAGING);
    let before = root.join(BEFORE);
    let pending: Pending = match fs::read(staging.join(READY))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(p) => p,
        None => {
            // An unpacking that never finished: nothing was replaced.
            if staging.exists() {
                let _ = fs::remove_dir_all(&staging);
            }
            return;
        }
    };
    if let Err(e) = fs::create_dir_all(&before) {
        notices.push(format!(
            "The restore couldn't start ({e}). It will be tried again at the next start."
        ));
        return;
    }
    // Each step done, so it can be undone: (live name, whether a restored
    // file was moved in).
    let mut done: Vec<(String, bool)> = Vec::new();
    let mut failure: Option<String> = None;
    let steps = pending
        .set_aside
        .iter()
        .map(|n| (n, false))
        .chain(pending.replace.iter().map(|n| (n, true)));
    for (name, from_staging) in steps {
        let live = root.join(name);
        let aside = before.join(name);
        let staged = staging.join(name);
        if from_staging && !staged.exists() {
            continue; // Moved in by an earlier, interrupted start.
        }
        if live.exists() {
            if aside.exists() {
                failure = Some(format!("both a current and a set-aside {name} exist"));
                break;
            }
            if let Err(e) = rename_patiently(&live, &aside) {
                failure = Some(format!("{name} couldn't be set aside: {e}"));
                break;
            }
        }
        if from_staging && let Err(e) = rename_patiently(&staged, &live) {
            failure = Some(format!("{name} couldn't be moved in: {e}"));
            done.push((name.clone(), false));
            break;
        }
        done.push((name.clone(), from_staging));
    }
    if failure.is_none() {
        failure = trial_open(root).err();
    }
    match failure {
        None => {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_dir_all(&before);
            let m = &pending.manifest;
            let memories = if m.memories_included {
                count(m.data.memories, "memory", "memories")
            } else {
                "your memories on this computer kept".to_string()
            };
            notices.push(format!(
                "Restored the backup made {}: {}, {} and {}.",
                crate::diagnostics::utc(m.created_ms),
                count(m.data.chats, "chat", "chats"),
                count(m.data.agent_sessions, "agent session", "agent sessions"),
                memories,
            ));
        }
        Some(why) => {
            // Put everything back as it was, checking each step: "unchanged"
            // is said only when it is true (review GW-016).
            let mut not_back: Vec<String> = Vec::new();
            for (name, moved_in) in done.iter().rev() {
                let live = root.join(name);
                let aside = before.join(name);
                if *moved_in && live.exists() {
                    let removed = if live.is_dir() {
                        fs::remove_dir_all(&live)
                    } else {
                        fs::remove_file(&live)
                    };
                    if removed.is_err() {
                        not_back.push(name.clone());
                        continue;
                    }
                }
                if aside.exists() && rename_patiently(&aside, &live).is_err() {
                    not_back.push(name.clone());
                }
            }
            if not_back.is_empty() {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_dir(&before);
                notices.push(format!(
                    "The backup couldn't be restored ({why}). Your data is unchanged; the backup file is untouched."
                ));
            } else {
                // Keep the set-aside originals under a name no later restore
                // uses, and stop this restore from being tried again.
                let _ = fs::remove_file(staging.join(READY));
                let kept = root.join(format!("{BEFORE}-kept-{}", now_ms()));
                let kept = if fs::rename(&before, &kept).is_ok() {
                    kept
                } else {
                    before.clone()
                };
                notices.push(format!(
                    "The backup couldn't be restored ({why}), and putting your data back didn't fully work: {}. Your original files are in {}. Close Gardenwall and move them back into {} (or ask for help with Copy diagnostics) before using it; don't delete that folder.",
                    not_back.join(", "),
                    kept.display(),
                    root.display(),
                ));
            }
        }
    }
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Open the restored chats once with their key.
fn trial_open(root: &Path) -> Result<(), String> {
    let cipher = match gardenwall_keychain::load_key(&root.join("chat.key")) {
        Ok(Some(k)) => Some(gardenwall_store::crypto::FieldCipher::new(&k)),
        Ok(None) | Err(gardenwall_keychain::KeyError::Unsupported) => None,
        Err(e) => return Err(format!("the restored key couldn't be read: {e}")),
    };
    let store = ConversationStore::open_with(&root.join("conversations.db"), cipher)
        .map_err(|e| format!("the restored chats couldn't be opened: {e}"))?;
    if store.is_locked() {
        return Err("the restored chats couldn't be unlocked".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_inside_a_backup_cannot_leave_their_folder() {
        assert!(safe_name("skills/deploy/SKILL.md"));
        for bad in [
            "skills/../chat.key",
            "skills//x",
            "skills/./x",
            "skills/C:/x",
            "skills/a\\..\\b",
            "/skills/x",
            "skills/con.",
            "skills/x ",
            "skills/a\u{0}b",
            // Windows device names (review GW-018).
            "skills/CON",
            "skills/nul.txt",
            "skills/x/Com1.md",
            "skills/LPT9 .log",
            "skills/com\u{b9}",
            "skills/conout$",
        ] {
            assert!(!safe_name(bad), "{bad}");
        }
        assert!(safe_name("skills/console/SKILL.md"));
        assert!(safe_name("skills/connect.md"));
    }

    fn state_with_chat(dir: &Path) -> AppState {
        use gardenwall_store::conversations::{MemoryScope, MemorySource};
        let state = AppState::new(dir.to_path_buf()).unwrap();
        state.conversations.lock().unwrap().create("").unwrap();
        state
            .conversations
            .lock()
            .unwrap()
            .add_memory(
                &MemoryScope::Personal,
                "likes short answers",
                MemorySource::User,
            )
            .unwrap();
        fs::create_dir_all(dir.join("skills").join("notes")).unwrap();
        fs::write(dir.join("skills").join("notes").join("SKILL.md"), "# Notes").unwrap();
        state
    }

    /// A backup made here, unpacked and applied in a second data folder,
    /// opens every chat; the second folder's own data is replaced and its
    /// set-aside copy removed.
    #[test]
    fn a_backup_restores_into_another_data_folder() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let state = state_with_chat(a.path());
        let file = a.path().join("out.gwbackup");
        let fast = format::KdfParams::FAST_FOR_TESTS;
        let all = Include {
            activity_log: true,
            memories: true,
            skills: true,
        };
        write_backup(&state, &file, "a long passphrase", all, fast).unwrap();
        assert!(!a.path().join(WORK).exists(), "plain copies are removed");

        let other = AppState::new(b.path().to_path_buf()).unwrap();
        other.conversations.lock().unwrap().create("").unwrap();
        other.conversations.lock().unwrap().create("").unwrap();
        let paths = Paths::new(b.path().to_path_buf()).unwrap();
        drop(other);

        assert!(stage(&paths, &file, "the wrong passphrase", &[]).is_err());
        assert!(!b.path().join(STAGING).join(READY).exists());
        stage(&paths, &file, "a long passphrase", &[]).unwrap();
        assert!(b.path().join(STAGING).join(READY).exists());

        let mut notices = Vec::new();
        apply_pending_restore(b.path(), &mut notices);
        assert!(notices[0].starts_with("Restored the backup"), "{notices:?}");
        assert!(!b.path().join(STAGING).exists());
        assert!(!b.path().join(BEFORE).exists());
        let restored = AppState::new(b.path().to_path_buf()).unwrap();
        assert_eq!(
            restored.conversations.lock().unwrap().list().unwrap().len(),
            1
        );
        assert!(b.path().join("skills/notes/SKILL.md").exists());
        assert_eq!(
            restored
                .conversations
                .lock()
                .unwrap()
                .list_memories()
                .unwrap()
                .len(),
            1
        );
    }

    /// Memories and skills left out of a backup: the ones on the computer
    /// restored to stay (memories sealed again with the backup's key), and
    /// none from the backed-up computer arrive.
    #[test]
    fn memories_and_skills_left_out_are_kept_here() {
        use gardenwall_store::conversations::{MemoryScope, MemorySource};
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let state = state_with_chat(a.path());
        let file = a.path().join("out.gwbackup");
        let none = Include {
            activity_log: false,
            memories: false,
            skills: false,
        };
        write_backup(
            &state,
            &file,
            "a long passphrase",
            none,
            format::KdfParams::FAST_FOR_TESTS,
        )
        .unwrap();

        let other = AppState::new(b.path().to_path_buf()).unwrap();
        let mine = other
            .conversations
            .lock()
            .unwrap()
            .add_memory(
                &MemoryScope::Project {
                    folder: "C:/work".into(),
                },
                "uses pnpm",
                MemorySource::User,
            )
            .unwrap();
        let current = other.conversations.lock().unwrap().list_memories().unwrap();
        fs::create_dir_all(b.path().join("skills").join("mine")).unwrap();
        fs::write(
            b.path().join("skills").join("mine").join("SKILL.md"),
            "# Mine",
        )
        .unwrap();
        let paths = Paths::new(b.path().to_path_buf()).unwrap();
        drop(other);

        stage(&paths, &file, "a long passphrase", &current).unwrap();
        let mut notices = Vec::new();
        apply_pending_restore(b.path(), &mut notices);
        assert!(
            notices[0].contains("memories on this computer kept"),
            "{notices:?}"
        );
        let restored = AppState::new(b.path().to_path_buf()).unwrap();
        let memories = restored
            .conversations
            .lock()
            .unwrap()
            .list_memories()
            .unwrap();
        assert_eq!(memories.len(), 1, "only this computer's memory");
        assert_eq!(memories[0].id, mine.id);
        assert_eq!(memories[0].text, "uses pnpm");
        assert_eq!(
            memories[0].scope,
            MemoryScope::Project {
                folder: "C:/work".into()
            }
        );
        assert!(b.path().join("skills/mine/SKILL.md").exists());
        assert!(!b.path().join("skills/notes").exists());
        assert_eq!(
            restored.conversations.lock().unwrap().list().unwrap().len(),
            1
        );
    }

    #[test]
    fn a_failed_restore_puts_everything_back() {
        let b = tempfile::tempdir().unwrap();
        let other = AppState::new(b.path().to_path_buf()).unwrap();
        other.conversations.lock().unwrap().create("").unwrap();
        drop(other);
        // A staged "database" that isn't one.
        let staging = b.path().join(STAGING);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("conversations.db"), b"not a database").unwrap();
        let pending = Pending {
            manifest: Manifest {
                app_version: "0".into(),
                created_ms: 0,
                data: DataCounts::default(),
                activity_log: None,
                memories_included: true,
                skills: None,
                settings: false,
                encrypted: false,
            },
            replace: vec!["conversations.db".into()],
            set_aside: vec!["conversations.db-wal".into(), "conversations.db-shm".into()],
        };
        fs::write(staging.join(READY), serde_json::to_vec(&pending).unwrap()).unwrap();
        let mut notices = Vec::new();
        apply_pending_restore(b.path(), &mut notices);
        assert!(notices[0].contains("Your data is unchanged"), "{notices:?}");
        let again = AppState::new(b.path().to_path_buf()).unwrap();
        assert_eq!(again.conversations.lock().unwrap().list().unwrap().len(), 1);
        assert!(!staging.exists());
    }
}
