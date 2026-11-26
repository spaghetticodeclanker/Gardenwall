//! One-time move of app data from the roaming profile to the local one.
//!
//! Gardenwall first stored models, runtimes and chats under the roaming
//! profile (`%APPDATA%`). On managed Windows machines a roaming profile can be
//! copied to a server at sign-out, which would take chats and multi-gigabyte
//! models off this computer. Everything now lives under the local profile
//! (`%LOCALAPPDATA%`), which never roams.
//!
//! Only entries Gardenwall itself creates are moved, each with a same-volume
//! rename. If any rename fails, the ones already done are moved back and the
//! old location stays in use, so data is never split between the two.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Entries Gardenwall creates in its data directory.
pub const KNOWN_ENTRIES: &[&str] = &[
    "models",
    "runtime",
    "relay",
    "work",
    "sandbox-grants",
    "settings.json",
    "conversations.db",
    "conversations.db-wal",
    "conversations.db-shm",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to move: use the new location.
    NothingToMove,
    /// Data moved: use the new location.
    Moved(usize),
    /// Both locations hold Gardenwall data; the old one is left alone and
    /// stays in use so nothing is hidden. Needs manual resolution.
    Conflict(String),
    /// A move failed and was rolled back; the old location stays in use.
    Failed(String),
}

impl Outcome {
    /// Which directory the app should use.
    pub fn root<'a>(&self, old: &'a Path, new: &'a Path) -> &'a Path {
        match self {
            Self::NothingToMove | Self::Moved(_) => new,
            Self::Conflict(_) | Self::Failed(_) => old,
        }
    }
}

/// Move Gardenwall's entries from `old` to `new`.
pub fn move_to_local(old: &Path, new: &Path) -> Outcome {
    if old == new {
        return Outcome::NothingToMove;
    }
    // Empty folders hold no data: drop them instead of moving or counting
    // them as a conflict (they are recreated on demand).
    for name in KNOWN_ENTRIES {
        let _ = remove_if_empty_dir(&old.join(name));
    }
    let present: Vec<&str> = KNOWN_ENTRIES
        .iter()
        .copied()
        .filter(|name| fs::symlink_metadata(old.join(name)).is_ok())
        .collect();
    if present.is_empty() {
        let _ = fs::remove_dir(old);
        return Outcome::NothingToMove;
    }
    for name in &present {
        let _ = remove_if_empty_dir(&new.join(name));
    }
    if let Some(name) = present
        .iter()
        .find(|name| fs::symlink_metadata(new.join(name)).is_ok())
    {
        return Outcome::Conflict(format!(
            "{name} exists in both {} and {}",
            old.display(),
            new.display()
        ));
    }
    if let Err(e) = fs::create_dir_all(new) {
        return Outcome::Failed(format!("cannot create {}: {e}", new.display()));
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in &present {
        let (from, to) = (old.join(name), new.join(name));
        if let Err(e) = fs::rename(&from, &to) {
            let rollback = rollback(&moved);
            return Outcome::Failed(format!(
                "moving {name} failed ({e}); {}",
                match rollback {
                    Ok(()) => "earlier moves were undone".to_string(),
                    Err(r) => format!("undoing earlier moves also failed: {r}"),
                }
            ));
        }
        moved.push((from, to));
    }
    // Leave the old directory only if it is now empty.
    let _ = fs::remove_dir(old);
    Outcome::Moved(moved.len())
}

/// Remove `path` if it is a real (non-link) directory with nothing in it.
fn remove_if_empty_dir(path: &Path) -> io::Result<bool> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && fs::read_dir(path)?.next().is_none() {
        fs::remove_dir(path)?;
        return Ok(true);
    }
    Ok(false)
}

fn rollback(moved: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    for (from, to) in moved.iter().rev() {
        fs::rename(to, from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populate(dir: &Path) {
        fs::create_dir_all(dir.join("models")).unwrap();
        fs::write(dir.join("models").join("a.gguf"), b"model").unwrap();
        fs::write(dir.join("settings.json"), b"{}").unwrap();
    }

    #[test]
    fn moves_known_entries_and_leaves_others() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        populate(&old);
        fs::write(old.join("unrelated.txt"), b"keep").unwrap();
        // The webview's own folder already exists in the new location.
        fs::create_dir_all(new.join("EBWebView")).unwrap();

        let outcome = move_to_local(&old, &new);
        assert_eq!(outcome, Outcome::Moved(2));
        assert_eq!(outcome.root(&old, &new), new);
        assert_eq!(
            fs::read(new.join("models").join("a.gguf")).unwrap(),
            b"model"
        );
        assert!(new.join("settings.json").exists());
        assert!(old.join("unrelated.txt").exists());
        assert!(!old.join("models").exists());
    }

    #[test]
    fn empty_old_directory_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        populate(&old);
        assert_eq!(move_to_local(&old, &new), Outcome::Moved(2));
        assert!(!old.exists());
        // Running again finds nothing to do.
        assert_eq!(move_to_local(&old, &new), Outcome::NothingToMove);
    }

    #[test]
    fn conflict_leaves_both_untouched_and_keeps_old_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        populate(&old);
        fs::create_dir_all(new.join("models")).unwrap();
        fs::write(new.join("models").join("b.gguf"), b"other").unwrap();
        let outcome = move_to_local(&old, &new);
        assert!(matches!(outcome, Outcome::Conflict(_)), "{outcome:?}");
        assert_eq!(outcome.root(&old, &new), old);
        assert!(old.join("models").join("a.gguf").exists());
        assert!(old.join("settings.json").exists());
    }

    #[test]
    fn empty_leftover_folders_are_not_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        populate(&new);
        // Empty folders left in the old location after an earlier move.
        for d in ["models", "relay", "runtime", "work"] {
            fs::create_dir_all(old.join(d)).unwrap();
        }
        let outcome = move_to_local(&old, &new);
        assert_eq!(outcome, Outcome::NothingToMove);
        assert_eq!(outcome.root(&old, &new), new);
        assert!(!old.exists());
        assert!(new.join("models").join("a.gguf").exists());
    }

    #[test]
    fn an_empty_destination_folder_does_not_block_the_move() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        populate(&old);
        fs::create_dir_all(new.join("models")).unwrap();
        assert_eq!(move_to_local(&old, &new), Outcome::Moved(2));
        assert!(new.join("models").join("a.gguf").exists());
    }

    #[test]
    fn nothing_to_move_uses_new_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("roaming"), tmp.path().join("local"));
        let outcome = move_to_local(&old, &new);
        assert_eq!(outcome, Outcome::NothingToMove);
        assert_eq!(outcome.root(&old, &new), new);
    }
}
