//! A small start-up fix Gardenwall loads into Python inside the Windows
//! sandbox (ADR-024), like Node's preload
//! (`node.rs`). It answers one way the AppContainer differs from a normal
//! login and grants nothing: the code runs inside the same sandbox as the
//! command.
//!
//! The core writes it to a folder only it changes; the sandbox gets that
//! folder read and run only, so neither the model nor a command can change
//! it. Only that folder is put on `PYTHONPATH`: nothing from the user's
//! environment or the project. (Python's own `venv` clears `PYTHONPATH`
//! before it installs pip, so the agent is told to create a virtual
//! environment `--without-pip` and run `ensurepip` in it.)

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Python 3.12.4+ (CVE-2024-4030) gives a folder made with mode 0o700 a
/// private permission list (SYSTEM, Administrators, its owner) that leaves
/// out the sandbox's identity, so a sandboxed Python couldn't use the
/// temporary folders it makes (`tempfile`, `venv`, pip, pytest's
/// `tmp_path`). Such folders get the permissions they inherit instead: the
/// session's scratch folder is private to the session already, and a folder
/// in the project gets the project's own permissions.
pub const PYTHON_SITECUSTOMIZE: &str = r#"# Gardenwall, for this sandboxed session only: Python gives folders made
# with mode 0o700 a private permission list that leaves out the sandbox, so
# the sandbox couldn't use its own temporary folders. Such folders get the
# permissions they inherit instead (the session's scratch folder is private
# to the session already).
import os as _gw_os
import sys as _gw_sys

if _gw_sys.platform == "win32":
    _gw_mkdir = _gw_os.mkdir

    def mkdir(path, mode=0o777, *, dir_fd=None):
        return _gw_mkdir(path, 0o777 if mode == 0o700 else mode, dir_fd=dir_fd)

    mkdir.__doc__ = _gw_mkdir.__doc__
    _gw_os.mkdir = mkdir
"#;

/// Write the preload under `dir` (`python\sitecustomize.py`), removing
/// anything else there. Returns the folder to give the sandbox (read and
/// run only).
pub fn stage(dir: &Path) -> std::io::Result<PathBuf> {
    for (sub, name, text) in [("python", "sitecustomize.py", PYTHON_SITECUSTOMIZE)] {
        let folder = dir.join(sub);
        std::fs::create_dir_all(&folder)?;
        let path = folder.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            let tmp = folder.join(format!("{name}.tmp"));
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)?;
        }
        // Nothing else may sit beside it (an extra `.pth` or `.rb`).
        for entry in std::fs::read_dir(&folder)?.flatten() {
            if entry.file_name() != name {
                let p = entry.path();
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
    }
    std::fs::canonicalize(dir)
}

/// The environment that loads the preload from `staged` (as [`stage`]
/// returned it).
pub fn env(staged: &Path) -> Vec<(OsString, OsString)> {
    let plain = |p: PathBuf| -> OsString { crate::display::shown(&p).into() };
    vec![("PYTHONPATH".into(), plain(staged.join("python")))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_writes_only_the_preloads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        std::fs::write(dir.path().join("python").join("evil.pth"), "import x").unwrap();
        let staged = stage(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(staged.join("python").join("sitecustomize.py")).unwrap(),
            PYTHON_SITECUSTOMIZE
        );
        assert!(!staged.join("python").join("evil.pth").exists());
        let env = env(&staged);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "PYTHONPATH");
        assert!(!env[0].1.to_string_lossy().starts_with(r"\\?\"));
    }
}
