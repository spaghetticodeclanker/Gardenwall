//! Gardenwall inference: supervision of the bundled `llama-server` sidecar and
//! a streaming OpenAI-compatible chat client (ADR-011 §2.1, ADR-012, ADR-013).
//!
//! The runtime and everything it returns are untrusted.

#![forbid(unsafe_code)]

pub mod client;
pub mod compat;
pub mod launch;
pub mod placement;
pub mod repetition;
pub mod supervisor;
pub mod transport;
pub mod whisper;
