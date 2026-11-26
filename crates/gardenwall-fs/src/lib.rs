//! Workspace file access for the Gardenwall agent (ADR-011 §2.2).
//!
//! - [`workspace`]: a granted folder reached only through a capability
//!   handle, so paths cannot resolve outside it.
//! - [`checkpoint`]: per-turn snapshots of changed files, stored outside
//!   the workspace, with review diffs and newest-first undo.
//! - [`diff`]: unified diffs for review and approval dialogs.
//!
//! Policy (what needs approval) lives in `gardenwall-policy`; this crate is
//! the mechanism. Pre-existing hard links inside a granted folder are not
//! detected here: creating one needs write access to the target, which the
//! sandboxed agent never has outside its grants.

#![forbid(unsafe_code)]

pub mod checkpoint;
pub mod diff;
pub mod snapshot;
pub mod workspace;

use std::fmt;
use std::path::{Path, PathBuf};

pub use checkpoint::{CheckpointStore, Retention, Turn, TurnSummary};
pub use diff::{Change, FileDiff};
pub use workspace::{DirEntry, EntryKind, Workspace, is_secret_store, secrets_home};

#[derive(Debug)]
pub enum FsError {
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
        message: String,
    },
    NotADirectory(PathBuf),
    NotAFile(String),
    GrantTooBroad(PathBuf),
    /// A folder that holds secrets (the secrets folder, `.ssh`, …).
    SecretStore(PathBuf),
    TooLarge {
        path: String,
        size: u64,
        limit: u64,
    },
    CheckpointsInsideWorkspace,
    CorruptCheckpoint(u64),
}

impl FsError {
    pub(crate) fn io(path: &Path, e: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message, .. } => write!(f, "{}: {message}", path.display()),
            Self::NotADirectory(p) => write!(f, "{} is not a folder", p.display()),
            Self::NotAFile(p) => write!(f, "{p} is not a regular file"),
            Self::GrantTooBroad(p) => write!(
                f,
                "{} is too broad to grant (a drive, home or system folder); choose a project folder",
                p.display()
            ),
            Self::SecretStore(p) => write!(
                f,
                "{} holds secrets (keys or credentials) and can't be granted; choose a project folder",
                p.display()
            ),
            Self::TooLarge { path, size, limit } => {
                write!(f, "{path} is {size} bytes, over the {limit}-byte limit")
            }
            Self::CheckpointsInsideWorkspace => {
                f.write_str("checkpoints must be stored outside the workspace")
            }
            Self::CorruptCheckpoint(n) => write!(f, "checkpoint for turn {n} is damaged"),
        }
    }
}

impl std::error::Error for FsError {}
