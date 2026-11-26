//! Gardenwall agent harness (ADR-014): a native Rust port of Pi's coding
//! agent, with every tool call routed through policy, approval and the OS
//! sandbox.
//!
//! - [`truncate`], [`edit_text`]: Pi's truncation and edit algorithms.
//! - [`tools`]: tool definitions and their execution on granted workspaces.

#![forbid(unsafe_code)]

pub mod agent;
pub mod commit_check;
pub mod compaction;
pub mod dependencies;
pub mod display;
pub mod edit_text;
pub mod git;
pub mod guard;
pub mod handoff;
#[cfg(windows)]
pub mod node;
pub mod preloads;
pub mod prompt;
#[cfg(any(windows, target_os = "linux"))]
pub mod python;
pub mod repo_init;
pub mod run_request;
#[cfg(any(windows, target_os = "linux"))]
pub mod sandbox_shell;
#[cfg(any(windows, target_os = "linux"))]
pub mod script_host;
pub mod secrets;
pub mod session_shell;
pub mod toolchain_request;
pub mod tools;
pub mod truncate;
pub mod web_text;
