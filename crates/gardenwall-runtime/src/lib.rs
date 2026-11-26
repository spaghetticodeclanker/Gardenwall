//! Gardenwall runtime management (ADR-013 §2.5).
//!
//! Installs pinned llama.cpp builds from hash-verified archives, extracts
//! them defensively, records a manifest of every file outside the runtime
//! directory, and verifies the installation against that manifest before
//! every launch.

#![forbid(unsafe_code)]

pub mod download;
pub mod extract;
pub mod pins;
pub mod store;
pub mod vcredist;
