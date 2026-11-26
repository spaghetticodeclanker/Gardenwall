//! Gardenwall policy (ADR-007, ADR-011).
//!
//! - [`paths`]: validation and classification of workspace-relative paths.
//! - [`tools`]: the policy engine for agent tool calls.
//! - [`protect`]: what commands may not change, which commands get a
//!   warning, and hidden characters made visible.
//!
//! Web search and fetch are decided by the core's ADR-018 gateway; network
//! commands stay denied in [`tools`] until the egress proxy exists.

#![forbid(unsafe_code)]

pub mod paths;
pub mod protect;
pub mod tools;
