//! Python for the agent's sandboxed shell.
//!
//! Python gets no capability of its own: it is one more program inside the
//! shell's sandbox, with the same boundary as every command (the granted
//! folders read/write, the scratch folder, no network). This module only
//! decides which interpreter, which environment, and what the model is told.
//!
//! - **Which interpreter (Windows):** an all-users python.org install, in
//!   either of its standard folders: `%ProgramFiles%\Python3NN` (the
//!   current default) or `%SystemDrive%\Python3NN` (the older one). Both are
//!   normally changeable only by administrators. Inside the sandbox the
//!   folder is read-only wherever it is, so nothing the agent runs can
//!   change the interpreter or its packages. The Store build (`WindowsApps`)
//!   can't start inside an AppContainer and is never used. Any other Python
//!   (a per-user install, a virtual environment, another distribution) can
//!   be added by hand as a read-only tool folder.
//! - **Which interpreter (Linux):** the system's `/usr/bin/python3`, already
//!   readable through the sandbox's read-only `/usr` grant.
//! - **Environment:** no user site-packages, bytecode written to the scratch
//!   folder (never into the project, where a stale or forged `.pyc` could
//!   later run on the host instead of its source), UTF-8, and pip told not to
//!   prompt or check for updates. `PYTHONHOME` and `PYTHONSTARTUP` are
//!   never set, and nothing from the user's environment or the project
//!   reaches `PYTHONPATH`: the sandbox's environment is rebuilt from an
//!   allowlist. On Windows the app puts one folder of its own on
//!   `PYTHONPATH` (`preloads`, Python's private
//!   temporary folders leave out the sandbox's identity).
//! - **Packages:** there is no network, so `pip install` fails. Packages
//!   come from the interpreter's own folder, installed by the user outside
//!   the agent.

#![cfg(any(windows, target_os = "linux"))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What [`python_status`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonStatus {
    /// No all-users install.
    Missing,
    /// `python.exe` in a folder every AppContainer can read: offered as is.
    Ready(PathBuf),
    /// Installed, but sandboxes can't read its folder (common for
    /// `C:\Python3NN`). Gardenwall never changes a system folder's
    /// permissions, so the user grants read access once as administrator.
    NotReadable(PathBuf),
}

/// The interpreter to offer the agent, if it needs no permission change.
pub fn find_python() -> Option<PathBuf> {
    match python_status() {
        PythonStatus::Ready(exe) => Some(exe),
        _ => None,
    }
}

/// The newest all-users python.org install (Windows) and whether sandboxes
/// can read it. Folders are matched exactly as `Python3NN` in Program Files
/// or at the root of the system drive; anything else is left to the user to
/// add as a tool folder.
pub fn python_status() -> PythonStatus {
    let Some(exe) = newest_install() else {
        return PythonStatus::Missing;
    };
    if readable_by_sandboxes(&exe) {
        PythonStatus::Ready(exe)
    } else {
        PythonStatus::NotReadable(exe)
    }
}

#[cfg(windows)]
fn readable_by_sandboxes(exe: &Path) -> bool {
    exe.parent()
        .is_some_and(gardenwall_sandbox::appcontainers_can_read)
}

#[cfg(not(windows))]
fn readable_by_sandboxes(_exe: &Path) -> bool {
    false
}

fn newest_install() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.is_absolute() && !bases.contains(&p) {
                bases.push(p);
            }
        }
    }
    if let Some(drive) = std::env::var_os("SystemDrive") {
        // "C:" → "C:\".
        let mut root = drive;
        root.push("\\");
        let p = PathBuf::from(root);
        if p.is_absolute() && !bases.contains(&p) {
            bases.push(p);
        }
    }
    let mut best: Option<(u32, PathBuf)> = None;
    for base in bases {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(minor) = entry.file_name().to_str().and_then(python_minor) else {
                continue;
            };
            let exe = entry.path().join("python.exe");
            if exe.is_file() && !in_store(&exe) && best.as_ref().is_none_or(|(m, _)| minor > *m) {
                best = Some((minor, exe));
            }
        }
    }
    best.map(|(_, exe)| exe)
}

/// `Python313` → 13 (Python 3 only).
fn python_minor(folder: &str) -> Option<u32> {
    let digits = folder.strip_prefix("Python3")?;
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn in_store(exe: &Path) -> bool {
    exe.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

/// The interpreter the shell can run: on Windows the first tool folder
/// holding `python.exe`, on Linux the system's `python3`.
pub(crate) fn interpreter(tool_folders: &[PathBuf]) -> Option<PathBuf> {
    if cfg!(windows) {
        tool_folders
            .iter()
            .map(|t| t.join("python.exe"))
            .find(|exe| exe.is_file() && !in_store(exe))
    } else {
        let p = PathBuf::from("/usr/bin/python3");
        p.is_file().then_some(p)
    }
}

/// Arguments that print the interpreter's version, ignoring the
/// environment and user site (`-I`).
pub(crate) const VERSION_ARGS: [&str; 3] = [
    "-I",
    "-c",
    "import sys; print('%d.%d.%d' % sys.version_info[:3])",
];

/// A version line of Python 3 ("3.13.1"), or `None`.
pub(crate) fn parse_version(output: &str) -> Option<String> {
    let v = output.trim();
    let parts: Vec<&str> = v.split('.').collect();
    (parts.len() == 3
        && parts[0] == "3"
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit())))
    .then(|| v.to_string())
}

/// The environment that keeps Python inside the session.
pub(crate) fn env(scratch: &Path) -> Vec<(OsString, OsString)> {
    vec![
        ("PYTHONNOUSERSITE".into(), "1".into()),
        (
            "PYTHONPYCACHEPREFIX".into(),
            scratch.join("pycache").into_os_string(),
        ),
        ("PYTHONUTF8".into(), "1".into()),
        ("PYTHONIOENCODING".into(), "utf-8".into()),
        ("PIP_NO_INPUT".into(), "1".into()),
        ("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into()),
    ]
}

/// What the model is told, so it neither assumes Python nor searches the
/// disk for one.
pub(crate) fn guideline(version: Option<&str>) -> String {
    match version {
        Some(v) => format!(
            "Python {v} is available as python (python -m pytest when pytest is installed). There is no network, so pip install fails: use the packages already installed, or wheels the user fetched (pip install --no-index --find-links <folder>). Create a virtual environment with python -m venv --without-pip <dir>, then <dir>\\Scripts\\python -m ensurepip (venv's own pip step fails in the sandbox)"
        ),
        None => "Python is not available in this sandbox. Don't search the disk for it; if a task needs to run Python, say so".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_python3_folders_match() {
        assert_eq!(python_minor("Python313"), Some(13));
        assert_eq!(python_minor("Python39"), Some(9));
        for no in [
            "Python",
            "Python3",
            "Python27",
            "Python313-32",
            "PythonX",
            "python313x",
            "Python31415",
        ] {
            assert_eq!(python_minor(no), None, "{no}");
        }
    }

    #[test]
    fn versions_are_strict() {
        assert_eq!(parse_version("3.13.1\r\n").as_deref(), Some("3.13.1"));
        for no in [
            "2.7.18",
            "3.13",
            "3.13.1rc1",
            "",
            "3.13.1 extra",
            "Python 3.13.1",
        ] {
            assert_eq!(parse_version(no), None, "{no}");
        }
    }

    #[test]
    fn the_environment_keeps_python_in_the_session() {
        let env = env(Path::new("S"));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("PYTHONNOUSERSITE").as_deref(), Some("1".as_ref()));
        assert_eq!(
            get("PYTHONPYCACHEPREFIX"),
            Some(Path::new("S").join("pycache").into_os_string())
        );
        for unset in ["PYTHONPATH", "PYTHONHOME", "PYTHONSTARTUP"] {
            assert!(get(unset).is_none(), "{unset}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn store_builds_are_never_used() {
        assert!(in_store(Path::new(
            r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\python.exe"
        )));
        assert!(!in_store(Path::new(
            r"C:\Program Files\Python313\python.exe"
        )));
    }
}
