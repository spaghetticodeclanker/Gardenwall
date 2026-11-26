//! Gardenwall process sandbox (ADR-012).
//!
//! Untrusted processes — the inference runtime and every command the agent
//! runs — are started through this crate so that filesystem and network
//! restrictions are enforced by the operating system, independently of the
//! trusted core's policy checks.
//!
//! Backends:
//! - **Windows:** AppContainer + Job Objects ([`windows`]).
//! - **Linux:** Landlock + seccomp-BPF ([`linux`]); fails closed when the
//!   kernel has no Landlock.
//! - **macOS:** not yet implemented. [`Sandbox::new`] fails closed with
//!   [`SandboxError::Unsupported`]; callers must never fall back to running
//!   unsandboxed (ADR-012 §6).
//!
//! `unsafe` code is confined to the Windows backend modules.

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod argv;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod policy;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod windows_pipe;

use std::fmt;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use linux::{Sandbox, SandboxedProcess};
pub use policy::{Access, Limits, Network, PathGrant, PolicyError, SandboxPolicy};
#[cfg(windows)]
pub use windows::{
    GrantHold, KeptGrant, KillOnCloseJob, PARENT_LIST_MASK, ParentListHold, ReconcileReport,
    Sandbox, SandboxedProcess, appcontainers_can_read, kept_grants, owned_by_current_user,
    parent_listable, profile_sid, reconcile_grant_journals, reconcile_grant_journals_keeping,
    revoke_all_kept, revoke_kept, running_elevated, sandbox_can_use,
};
#[cfg(windows)]
pub use windows_pipe::PipeListener;

#[derive(Debug)]
pub enum SandboxError {
    Policy(PolicyError),
    Argv(argv::ArgvError),
    Io(PathBuf, std::io::ErrorKind),
    Os {
        op: &'static str,
        code: u32,
    },
    /// No sandbox backend exists for this platform; the operation fails closed.
    Unsupported,
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(e) => write!(f, "sandbox policy rejected: {e}"),
            Self::Argv(e) => write!(f, "cannot build command line: {e:?}"),
            Self::Io(path, kind) => write!(f, "cannot access {}: {kind}", path.display()),
            Self::Os { op, code } => write!(f, "{op} failed (0x{code:08x})"),
            Self::Unsupported => {
                f.write_str("process sandboxing is not available on this platform")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<PolicyError> for SandboxError {
    fn from(e: PolicyError) -> Self {
        Self::Policy(e)
    }
}

impl From<argv::ArgvError> for SandboxError {
    fn from(e: argv::ArgvError) -> Self {
        Self::Argv(e)
    }
}

/// Fail-closed stand-in on platforms without a backend.
#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Debug)]
pub struct Sandbox(());

#[cfg(not(any(windows, target_os = "linux")))]
impl Sandbox {
    pub fn new(_policy: SandboxPolicy) -> Result<Self, SandboxError> {
        Err(SandboxError::Unsupported)
    }
}
