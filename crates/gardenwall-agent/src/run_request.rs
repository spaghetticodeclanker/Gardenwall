//! "Run this yourself" cards.
//!
//! Agent commands have no network, and none is offered. When a task needs
//! one (installing a dependency, `git pull`), the agent can ask the user to
//! run a command themselves with `ask_user_to_run`. The core checks the
//! request and shows it on a card: the exact command, the reason, the real
//! folder to run it in, a warning when it looks risky, and a reminder that a
//! command the user runs has the user's full rights. The core never runs it.

use gardenwall_policy::protect::{reveal_invisible, risky_command};
use serde_json::{Value, json};

use crate::agent::BoxFuture;
use crate::tools::{Resolved, ToolDef, Workspaces};

/// Longest command shown (it is the model's text, and the user must be able
/// to read all of it before running it).
const MAX_COMMAND_CHARS: usize = 2_000;
const MAX_COMMAND_LINES: usize = 20;
const MAX_REASON_CHARS: usize = 300;
/// Longest output the user may paste back for the agent.
pub const MAX_PASTED_CHARS: usize = 8_000;

/// A checked request, exactly as the card shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    /// The command, with hidden characters made visible (`[U+XXXX]`).
    pub command: String,
    /// How many hidden characters were made visible in it.
    pub hidden: usize,
    pub reason: String,
    /// The real folder to run it in (a granted folder or one inside it).
    pub folder: String,
    /// Why the command looks risky, when it does.
    pub warning: Option<String>,
    /// The session had read web content before asking: the request may
    /// come from a page.
    pub after_web: bool,
    /// Gardenwall wrote the command (`fetch_dependencies`), not the model.
    pub by_gardenwall: bool,
}

/// What the user did with the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The user ran it; `pasted` is what they chose to paste back (may be
    /// empty).
    Done { pasted: String },
    /// The user chose not to run it.
    Skipped,
}

/// Shows the card and waits for the user.
pub trait RunAsker: Send + Sync {
    /// `None` when the card was dismissed or the turn was stopped.
    fn ask_to_run<'a>(&'a self, request: &'a RunRequest) -> BoxFuture<'a, Option<RunOutcome>>;
}

pub fn definition() -> ToolDef {
    ToolDef {
        name: "ask_user_to_run",
        description: "Ask the user to run one command themselves, outside your sandbox, and wait until they say it is done. Your own commands have no network, so use this when a step needs one (installing a dependency, git pull or clone, downloading a file) and nothing else works. Give the exact command, why it is needed and the folder to run it in. Don't use it to get around a refused approval.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The exact command to run"},
                "reason": {"type": "string", "description": "One sentence: why it is needed"},
                "folder": {"type": "string", "description": "Folder to run it in, inside the project (default: the project folder)"}
            },
            "required": ["command", "reason"]
        }),
        snippet: "Ask the user to run a command that needs the network",
        guidelines: &[],
    }
}

/// Check the model's request and build the card. Errors go back to the
/// model.
pub fn parse(args: &Value, workspaces: &Workspaces, after_web: bool) -> Result<RunRequest, String> {
    let raw = args
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if raw.is_empty() {
        return Err("ask_user_to_run requires the command.".into());
    }
    if raw.chars().count() > MAX_COMMAND_CHARS || raw.lines().count() > MAX_COMMAND_LINES {
        return Err(format!(
            "The command is too long for the user to check (at most {MAX_COMMAND_LINES} lines and {MAX_COMMAND_CHARS} characters). Ask for one short command."
        ));
    }
    let (command, hidden) = reveal_invisible(raw);
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reason = reveal_invisible(reason).0;
    let reason: String = reason
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_REASON_CHARS)
        .collect();
    if reason.is_empty() {
        return Err("ask_user_to_run requires a reason.".into());
    }
    let folder_arg = args
        .get("folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or(".");
    let folder = match workspaces.resolve(folder_arg)? {
        Resolved::Root(id) => workspaces.absolute(id, None),
        Resolved::Inside(id, rel) => workspaces.absolute(id, Some(&rel)),
        Resolved::Outside(_) => {
            return Err(format!(
                "{folder_arg} is outside the project's folders. Ask the user to run commands only inside them."
            ));
        }
    };
    if !folder.is_dir() {
        return Err(format!("{folder_arg} is not a folder in the project."));
    }
    // Files in the folder named like programs (git.exe, npm.cmd) would run
    // instead of the real ones in a Command Prompt, outside the sandbox.
    let planted = crate::dependencies::planted_programs(&folder);
    let planted = (!planted.is_empty()).then(|| {
        format!(
            "This folder has files named like programs ({}). In a Command Prompt they run instead of the real programs, outside the sandbox: run this from PowerShell, or delete them first.",
            planted.join(", ")
        )
    });
    let warning = match (planted, risky_command(&command)) {
        (Some(p), Some(r)) => Some(format!("{r} {p}")),
        (Some(p), None) => Some(p),
        (None, r) => r.map(str::to_string),
    };
    Ok(RunRequest {
        warning,
        command,
        hidden,
        reason,
        folder: folder.display().to_string(),
        after_web,
        by_gardenwall: false,
    })
}

/// What the model is told about the user's decision.
pub fn result_text(outcome: Option<RunOutcome>) -> (String, bool) {
    match outcome {
        Some(RunOutcome::Done { pasted }) => {
            let pasted = pasted.trim();
            if pasted.is_empty() {
                (
                    "The user says they ran the command. Check the result yourself (for example look at the files it should have changed) before you rely on it.".into(),
                    false,
                )
            } else {
                let pasted: String = pasted.chars().take(MAX_PASTED_CHARS).collect();
                (
                    format!("The user says they ran the command and pasted this output:\n{pasted}"),
                    false,
                )
            }
        }
        Some(RunOutcome::Skipped) => (
            "The user chose not to run the command. Don't ask again for the same command; continue without it, or stop and say what is blocked.".into(),
            true,
        ),
        None => (
            "The user closed the request without running the command. Continue without it, or stop and say what is blocked.".into(),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspaces() -> (tempfile::TempDir, Workspaces) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("web")).unwrap();
        let mut w = Workspaces::new(dir.path().join("checkpoints"));
        w.grant(&project).unwrap();
        (dir, w)
    }

    #[test]
    fn a_request_names_the_real_folder_and_warns_when_risky() {
        let (dir, w) = workspaces();
        let req = parse(
            &json!({"command": "npm install left-pad", "reason": "the test needs it", "folder": "web"}),
            &w,
            false,
        )
        .unwrap();
        let real = std::fs::canonicalize(dir.path().join("project").join("web")).unwrap();
        let shown = std::path::PathBuf::from(&req.folder);
        assert_eq!(std::fs::canonicalize(shown).unwrap(), real);
        assert_eq!(req.warning, None);
        let risky = parse(
            &json!({"command": "git reset --hard origin/main", "reason": "sync"}),
            &w,
            true,
        )
        .unwrap();
        assert!(risky.warning.is_some());
        assert!(risky.after_web);
        // A program planted in the folder runs instead in a Command Prompt.
        std::fs::write(dir.path().join("project").join("web").join("npm.cmd"), "").unwrap();
        let planted = parse(
            &json!({"command": "npm install left-pad", "reason": "x", "folder": "web"}),
            &w,
            false,
        )
        .unwrap();
        assert!(planted.warning.is_some_and(|w| w.contains("npm.cmd")));
    }

    #[test]
    fn hidden_characters_are_shown_and_bad_requests_refused() {
        let (_dir, w) = workspaces();
        let req = parse(
            &json!({"command": "pip install req\u{200B}uests", "reason": "needed"}),
            &w,
            false,
        )
        .unwrap();
        assert_eq!(req.hidden, 1);
        assert!(req.command.contains("[U+200B]"));
        for bad in [
            json!({"command": "", "reason": "x"}),
            json!({"command": "ls", "reason": " "}),
            json!({"command": "ls", "reason": "x", "folder": "missing"}),
            json!({"command": "x\n".repeat(30), "reason": "x"}),
        ] {
            assert!(parse(&bad, &w, false).is_err(), "{bad}");
        }
        let outside = if cfg!(windows) { "C:/Windows" } else { "/etc" };
        assert!(
            parse(
                &json!({"command": "ls", "reason": "x", "folder": outside}),
                &w,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn the_model_learns_what_the_user_did() {
        assert!(
            !result_text(Some(RunOutcome::Done {
                pasted: String::new()
            }))
            .1
        );
        let (text, err) = result_text(Some(RunOutcome::Done {
            pasted: "added 1 package".into(),
        }));
        assert!(!err && text.contains("added 1 package"));
        assert!(result_text(Some(RunOutcome::Skipped)).1);
        assert!(result_text(None).1);
    }
}
