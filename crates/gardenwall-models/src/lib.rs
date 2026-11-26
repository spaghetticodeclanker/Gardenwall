//! Gardenwall model intake (ADR-013).
//!
//! Model files are attacker-controllable input. This crate validates them
//! before any inference runtime opens them. Passing validation never makes a
//! model trusted: every model runs under the same sandbox (ADR-002, ADR-012).
//!
//! Models arrive by one of two paths, both ending in a
//! [`intake::QuarantinedModel`]:
//! - [`import`]: a local file the user selected (no network);
//! - [`hub`]: a verified download (only with the default `hub` feature, and
//!   only when the user's offline setting is off).

// `unsafe` is denied everywhere except one audited Windows call that
// marks downloads sparse (`hub/sparse.rs`).
#![deny(unsafe_code)]

pub mod gguf;
#[cfg(feature = "hub")]
pub mod hub;
pub mod import;
pub mod intake;
pub mod integrity;
mod known_templates;
pub mod split;
pub mod template;
