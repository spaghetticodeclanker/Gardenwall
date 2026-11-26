//! Platform-neutral sandbox policy (ADR-012 §3).
//!
//! A policy states exactly what an untrusted process may touch. Everything not
//! listed is denied. Backends compile the policy to OS mechanisms; if a
//! backend cannot enforce a field, spawning fails (never silently weaker).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Filesystem access granted to one path (and, for directories, its subtree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read file contents and list directories.
    Read,
    /// Read and execute (binaries, DLLs).
    ReadExecute,
    /// Read, create, modify, and delete.
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathGrant {
    pub path: PathBuf,
    pub access: Access,
}

/// Network policy. There is deliberately no "allow all" variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// No network access of any kind (including loopback to other processes
    /// outside the sandbox).
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum committed memory for all processes in the sandbox.
    pub memory_bytes: Option<u64>,
    /// Maximum simultaneously running processes (1 = no children).
    pub max_processes: u32,
    /// Wall-clock limit enforced by [`crate::SandboxedChild::wait_timeout`]
    /// callers; recorded here so policies are self-describing.
    pub wall_time: Option<Duration>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: None,
            max_processes: 1,
            wall_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Stable identifier for the sandbox profile (e.g. `Gardenwall.Inference`).
    /// Processes sharing a profile share an OS identity and can reach each
    /// other's container-local loopback (ADR-012 §4.1).
    pub profile: String,
    pub grants: Vec<PathGrant>,
    pub network: Network,
    pub limits: Limits,
    /// The complete environment. Nothing is inherited from the parent.
    pub env: Vec<(OsString, OsString)>,
    /// Working directory; must be inside a granted path.
    pub working_dir: Option<PathBuf>,
    /// Where to record ACL grants before they are added, so grants left by
    /// a crash can be revoked at the next start (Windows). `None` keeps no
    /// record; production callers should always set it.
    pub grant_journal_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    InvalidProfileName,
    RelativePath(PathBuf),
    MissingPath(PathBuf),
    /// The path is or contains a symlink/junction/reparse point.
    ReparsePoint(PathBuf),
    /// A file under the path has more than one hard link, so granting access
    /// could expose another location.
    MultiplyLinked(PathBuf),
    TooManyEntries(PathBuf),
    InvalidEnv(String),
    WorkingDirNotGranted,
    ZeroProcesses,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfileName => f.write_str("invalid sandbox profile name"),
            Self::RelativePath(p) => write!(f, "path is not absolute: {}", p.display()),
            Self::MissingPath(p) => write!(f, "path does not exist: {}", p.display()),
            Self::ReparsePoint(p) => {
                write!(f, "path contains a link or reparse point: {}", p.display())
            }
            Self::MultiplyLinked(p) => write!(f, "file has multiple hard links: {}", p.display()),
            Self::TooManyEntries(p) => write!(f, "too many entries under {}", p.display()),
            Self::InvalidEnv(name) => write!(f, "invalid environment variable {name:?}"),
            Self::WorkingDirNotGranted => f.write_str("working directory is not granted"),
            Self::ZeroProcesses => f.write_str("max_processes must be at least 1"),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Profile names: 1–64 chars of ASCII letters, digits, `.`, `-`.
pub fn valid_profile_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !name.starts_with(['.', '-'])
}

/// Environment names must be non-empty and contain no `=` or NUL; values no
/// NUL. (A leading `=` is used by Windows for drive-cwd pseudo-variables.)
pub fn valid_env(name: &OsStr, value: &OsStr) -> bool {
    let (Some(n), Some(v)) = (name.to_str(), value.to_str()) else {
        return false;
    };
    !n.is_empty() && !n.contains(['=', '\0']) && !v.contains('\0')
}

impl SandboxPolicy {
    /// Structural validation that does not touch the filesystem.
    pub fn validate_shape(&self) -> Result<(), PolicyError> {
        if !valid_profile_name(&self.profile) {
            return Err(PolicyError::InvalidProfileName);
        }
        if self.limits.max_processes == 0 {
            return Err(PolicyError::ZeroProcesses);
        }
        for grant in &self.grants {
            if !grant.path.is_absolute() {
                return Err(PolicyError::RelativePath(grant.path.clone()));
            }
        }
        for (name, value) in &self.env {
            if !valid_env(name, value) {
                return Err(PolicyError::InvalidEnv(name.to_string_lossy().into_owned()));
            }
        }
        if let Some(dir) = &self.working_dir {
            if !dir.is_absolute() {
                return Err(PolicyError::RelativePath(dir.clone()));
            }
            if !self.grants.iter().any(|g| dir.starts_with(&g.path)) {
                return Err(PolicyError::WorkingDirNotGranted);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\{p}"))
        } else {
            PathBuf::from(format!("/{p}"))
        }
    }

    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            profile: "Gardenwall.Test".into(),
            grants: vec![PathGrant {
                path: abs("work"),
                access: Access::ReadWrite,
            }],
            network: Network::Denied,
            limits: Limits::default(),
            env: vec![("A".into(), "1".into())],
            working_dir: Some(abs("work")),
            grant_journal_dir: None,
        }
    }

    #[test]
    fn valid_policy_passes() {
        assert_eq!(policy().validate_shape(), Ok(()));
    }

    #[test]
    fn profile_names_are_restricted() {
        for bad in ["", ".x", "-x", "a b", "a/b", "a\\b", &"x".repeat(65)] {
            assert!(!valid_profile_name(bad), "{bad}");
        }
        assert!(valid_profile_name("Gardenwall.Inference-1"));
    }

    #[test]
    fn rejects_bad_env_paths_and_limits() {
        let mut p = policy();
        p.env = vec![("A=B".into(), "1".into())];
        assert!(matches!(
            p.validate_shape(),
            Err(PolicyError::InvalidEnv(_))
        ));
        p.env = vec![("".into(), "1".into())];
        assert!(matches!(
            p.validate_shape(),
            Err(PolicyError::InvalidEnv(_))
        ));
        p.env = vec![("A".into(), "x\0y".into())];
        assert!(matches!(
            p.validate_shape(),
            Err(PolicyError::InvalidEnv(_))
        ));

        let mut p = policy();
        p.grants[0].path = "relative".into();
        assert!(matches!(
            p.validate_shape(),
            Err(PolicyError::RelativePath(_))
        ));

        let mut p = policy();
        p.working_dir = Some(abs("elsewhere"));
        assert_eq!(p.validate_shape(), Err(PolicyError::WorkingDirNotGranted));

        let mut p = policy();
        p.limits.max_processes = 0;
        assert_eq!(p.validate_shape(), Err(PolicyError::ZeroProcesses));
    }
}
