//! "Create a Git repository" (ADR-022 §2A).
//!
//! `git init` can't work inside the sandbox: Git for Windows resolves the
//! project's real path by checking each parent folder, and the sandbox can't
//! look into the user's profile folder (by design). So the agent asks, the
//! user approves a card built by the core, and Gardenwall runs `git init`
//! itself, outside the sandbox, with fixed arguments in the root of a
//! granted folder. Nothing from the model reaches the command line: the
//! model only picks which granted folder.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent::BoxFuture;
use crate::display::shown;
use crate::tools::{Resolved, ToolDef, Workspaces};

pub const NAME: &str = "init_repository";

/// Runs `git init` in a granted folder's root, outside the sandbox.
pub trait RepoInit: Send + Sync {
    /// The text to tell the model on success, or why it failed.
    fn init<'a>(&'a self, root: &'a Path) -> BoxFuture<'a, Result<String, String>>;
}

pub fn definition() -> ToolDef {
    ToolDef {
        name: NAME,
        description: "Create a Git repository in the project folder. Git can't create one from inside your sandbox (git init fails there), so Gardenwall creates it for you after the user approves (branch main). Use this instead of git init, and never build the .git folder by hand. Once it exists, git status, add, commit and log work in your commands as usual.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "folder": {"type": "string", "description": "Which project folder (its drive letter such as W:\\ or its path; default: the first project folder). Only the top of a project folder can get a repository."}
            }
        }),
        snippet: "Create a Git repository in the project folder (Gardenwall runs git init on the user's yes)",
        guidelines: &[],
    }
}

/// The granted folder's root the model asked for. Errors go back to the
/// model.
pub fn parse(args: &Value, workspaces: &Workspaces) -> Result<PathBuf, String> {
    let arg = args
        .get("folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or(".");
    let root = match workspaces.resolve(arg)? {
        Resolved::Root(id) => workspaces.absolute(id, None),
        Resolved::Inside(..) => {
            return Err(format!(
                "{arg} is inside a project folder. A repository can be created only at the top of a project folder."
            ));
        }
        Resolved::Outside(_) => {
            return Err(format!(
                "{arg} is outside the project's folders. A repository can be created only in one of them."
            ));
        }
    };
    if root.join(".git").exists() {
        return Err(format!(
            "{} already has a .git folder. If git commands fail with \"not a git repository\", tell the user: the folder holds an unfinished repository, which they can remove or repair themselves.",
            shown(&root)
        ));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_granted_root_without_git_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut ws = Workspaces::new(dir.path().join("checkpoints"));
        ws.grant(&root).unwrap();
        let got = parse(&json!({}), &ws).unwrap();
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
        assert!(
            parse(&json!({"folder": "src"}), &ws)
                .unwrap_err()
                .contains("top of a project folder")
        );
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(
            parse(&json!({"folder": outside.display().to_string()}), &ws)
                .unwrap_err()
                .contains("outside")
        );
        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert!(
            parse(&json!({}), &ws)
                .unwrap_err()
                .contains("already has a .git folder")
        );
    }
}
