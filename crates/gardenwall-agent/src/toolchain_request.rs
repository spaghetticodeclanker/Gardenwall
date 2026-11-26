//! "Ask for a toolchain" (ADR-024 §3 A2).
//!
//! The agent names a toolchain id from Gardenwall's own table, never a
//! folder; the core resolves the folder, the user decides on a card built
//! from the core's data, and on Add the core adds the toolchain, then
//! restarts the session's sandbox with it (the conversation stays). A
//! toolchain that isn't installed gets an install card with Gardenwall's own
//! download address instead: the agent never installs anything.

use serde_json::{Value, json};

use crate::agent::BoxFuture;
use crate::tools::ToolDef;

pub const NAME: &str = "request_toolchain";

/// The toolchain ids the agent may ask for (the core's table has the rest).
pub const IDS: &[&str] = &[
    "rust", "java", "dotnet", "go", "cpp", "python", "node", "ruby", "php",
];

/// Where a toolchain stands for this session, from the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Already in this session's sandbox ("Rust 1.98.1").
    Ready(String),
    /// Already added (in Settings or on the project page) but not in this
    /// session's sandbox yet: the sandbox restarts without a card, since the
    /// user added it themselves.
    Added(String),
    /// On this PC and not added: the user decides on a card.
    Installed {
        /// "Rust 1.98.1".
        label: String,
        /// The folders the sandbox would get, as shown to the user.
        folders: Vec<String>,
        /// The session has no project: it is added for every project.
        every_project: bool,
        /// Programs the user let keep running, which stop when the sandbox
        /// restarts.
        stops: Vec<String>,
    },
    /// On this PC, but the sandbox can't be given it (why, for the agent).
    Unusable(String),
    /// Not on this PC: the core shows the install card. The name, for the
    /// agent ("Ruby").
    Missing(String),
}

/// The app's side: the core's toolchain table, adding, and the sandbox
/// restart.
pub trait ToolchainHost: Send + Sync {
    fn status(&self, id: &str) -> Status;
    /// After the user's yes: add the toolchain (this project, or every
    /// project), keep its folder granted, and restart the session's sandbox
    /// with it. Returns the new tools note for the prompt.
    fn add<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String, String>>;
    /// Show the install card for a toolchain that isn't on this PC.
    fn show_install(&self, id: &str);
}

pub fn definition() -> ToolDef {
    ToolDef {
        name: NAME,
        description: format!(
            "Ask the user to add a toolchain to your sandbox when you need one you don't have (see # Tools). Ids: {}. If it is on this PC, the user decides on a card; on Add your sandbox restarts with it and you continue in this conversation. If it isn't installed, the user is shown where to get it. Never ask the user to change settings for this or to start a new session.",
            IDS.join(", ")
        ),
        parameters: json!({
            "type": "object",
            "properties": {
                "toolchain": {"type": "string", "enum": IDS, "description": "Which toolchain"},
                "reason": {"type": "string", "description": "Why you need it, one short sentence (shown to the user)"}
            },
            "required": ["toolchain", "reason"]
        }),
        snippet: "Ask the user to add a toolchain (Rust, Python, Node.js, …) to your sandbox",
        guidelines: &[
            "When a build tool you need isn't in your sandbox, use request_toolchain instead of telling the user to visit Settings",
        ],
    }
}

/// Longest reason shown on the card.
const MAX_REASON: usize = 300;

/// The toolchain id and the reason (one line, at most 300 characters).
/// Errors go back to the model.
pub fn parse(args: &Value) -> Result<(&'static str, String), String> {
    let wanted = args
        .get("toolchain")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(id) = IDS.iter().copied().find(|i| *i == wanted) else {
        return Err(format!(
            "Unknown toolchain \"{}\". Ask for one of: {}. For anything else, the user can add its folder in Settings → Agent tools.",
            wanted.chars().take(40).collect::<String>(),
            IDS.join(", ")
        ));
    };
    let reason: String = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_REASON)
        .collect();
    if reason.is_empty() {
        return Err("request_toolchain requires a reason.".into());
    }
    Ok((id, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_ids_and_a_reason_are_accepted() {
        assert_eq!(
            parse(&json!({"toolchain": "Rust", "reason": " build\n the  crate "})).unwrap(),
            ("rust", "build the crate".to_string())
        );
        assert!(
            parse(&json!({"toolchain": "C:\\evil", "reason": "x"}))
                .unwrap_err()
                .contains("Unknown toolchain")
        );
        assert!(
            parse(&json!({"toolchain": "node"}))
                .unwrap_err()
                .contains("reason")
        );
        let long = "a".repeat(1000);
        assert_eq!(
            parse(&json!({"toolchain": "go", "reason": long}))
                .unwrap()
                .1
                .len(),
            MAX_REASON
        );
    }
}
