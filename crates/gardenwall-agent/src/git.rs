//! Git for the agent's commands.
//!
//! Git for Windows in Program Files can be a read-only tool folder: every
//! AppContainer can already read it, so the sandbox adds no permission and
//! its hard-linked programs are fine. This module finds it, and works out
//! the name and email the agent's commits carry, passed to commands as
//! `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables. The sandbox has its own
//! home folder, so nothing else from the user's git configuration (aliases,
//! credential helpers, hooks) reaches commands.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The name and email agent commits carry when the user picks "Gardenwall
/// Agent". `.invalid` is a reserved domain, so it can never reach anyone.
pub const AGENT_NAME: &str = "Gardenwall Agent";
pub const AGENT_EMAIL: &str = "agent@gardenwall.invalid";

/// Longest name or email accepted (git has no limit; this keeps a pasted
/// paragraph out of every commit).
const MAX_FIELD: usize = 200;

/// Git for Windows installed for all users (`<Program Files>\Git`), when
/// its `cmd\git.exe` is there.
pub fn find_git_for_windows() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        ["ProgramW6432", "ProgramFiles"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|p| PathBuf::from(p).join("Git"))
            .find(|g| g.join("cmd").join("git.exe").is_file())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether a sandbox can use the Git folder without any permission change.
pub fn git_folder_ready(folder: &Path) -> bool {
    #[cfg(windows)]
    {
        gardenwall_sandbox::appcontainers_can_read(folder)
    }
    #[cfg(not(windows))]
    {
        let _ = folder;
        false
    }
}

/// A commit name and email, checked to be one plain line each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    /// `None` when either is empty, too long or holds control characters
    /// or the `<>` git uses around the email.
    pub fn new(name: &str, email: &str) -> Option<Self> {
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars().count() <= MAX_FIELD
                && !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
        };
        let (name, email) = (name.trim(), email.trim());
        (ok(name) && ok(email)).then(|| Self {
            name: name.to_string(),
            email: email.to_string(),
        })
    }

    pub fn agent() -> Self {
        Self {
            name: AGENT_NAME.into(),
            email: AGENT_EMAIL.into(),
        }
    }

    /// The variables that make git use this identity for authoring and
    /// committing, whatever the repository's configuration says.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let mut env = Vec::new();
        for role in ["AUTHOR", "COMMITTER"] {
            env.push((format!("GIT_{role}_NAME").into(), self.name.clone().into()));
            env.push((
                format!("GIT_{role}_EMAIL").into(),
                self.email.clone().into(),
            ));
        }
        env
    }
}

/// The user's own git identity, as git outside the sandbox would use it in
/// `folder`: the repository's `.git/config` when it sets one, else the
/// global configuration. Read by running the installed git with `config
/// --get` only (no hooks, nothing in the repository runs); the repository
/// file is read with `--file`, so no include in it is followed into other
/// repositories' settings.
pub fn user_identity(git: &Path, folder: Option<&Path>) -> Option<Identity> {
    let get = |args: &[&str]| -> Option<String> {
        let mut command = Command::new(git);
        command.args(args).env("GIT_CONFIG_NOSYSTEM", "1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flash from the app.
            command.creation_flags(0x0800_0000);
        }
        let out = command.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let local = folder
        .map(|f| f.join(".git").join("config"))
        .filter(|c| c.is_file())
        .and_then(|config| {
            let file = config.to_string_lossy().into_owned();
            Identity::new(
                &get(&["config", "--file", &file, "--get", "user.name"])?,
                &get(&["config", "--file", &file, "--get", "user.email"])?,
            )
        });
    local.or_else(|| {
        Identity::new(
            &get(&["config", "--global", "--get", "user.name"])?,
            &get(&["config", "--global", "--get", "user.email"])?,
        )
    })
}

/// The `git.exe` to read the user's settings with: Git for Windows when
/// installed, else `git.exe` in a tool folder.
pub fn host_git(tool_folders: &[PathBuf]) -> Option<PathBuf> {
    find_git_for_windows()
        .map(|g| g.join("cmd").join("git.exe"))
        .or_else(|| {
            tool_folders.iter().find_map(|t| {
                ["cmd", "bin", ""]
                    .iter()
                    .map(|sub| {
                        t.join(sub)
                            .join(if cfg!(windows) { "git.exe" } else { "git" })
                    })
                    .find(|p| p.is_file())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_one_plain_line_each() {
        assert!(Identity::new("Ada", "ada@example.com").is_some());
        assert_eq!(
            Identity::new("  Ada ", " ada@example.com ").unwrap().name,
            "Ada"
        );
        for (n, e) in [
            ("", "a@b"),
            ("Ada", ""),
            ("Ada\nEvil: x", "a@b"),
            ("Ada", "a@b>\n"),
            ("Ada <x@y>", "a@b"),
        ] {
            assert!(Identity::new(n, e).is_none(), "{n:?} {e:?}");
        }
        assert!(Identity::new(&"x".repeat(201), "a@b").is_none());
    }

    #[test]
    fn the_identity_covers_author_and_committer() {
        let env = Identity::agent().env();
        let names: Vec<_> = env
            .iter()
            .map(|(n, _)| n.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL"
            ]
        );
        assert!(env.iter().any(|(_, v)| v == "Gardenwall Agent"));
    }

    #[cfg(windows)]
    #[test]
    fn a_repository_identity_wins_over_the_global_one() {
        let Some(git) = host_git(&[]) else {
            eprintln!("skipped: Git for Windows is not installed");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new(&git)
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Repo Person"]);
        run(&["config", "user.email", "repo@example.invalid"]);
        let id = user_identity(&git, Some(dir.path())).unwrap();
        assert_eq!(id.name, "Repo Person");
        assert_eq!(id.email, "repo@example.invalid");
    }
}
