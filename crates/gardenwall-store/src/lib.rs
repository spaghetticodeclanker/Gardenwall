//! Gardenwall persistent storage: authorization proposals, single-use claims,
//! sessions, schema migration, recovery, and metadata-only audit.
//!
//! Per ADR-008/ADR-009, the storage layer remains compiled for tests only until
//! its public service boundary enforces authentication, trusted timestamps,
//! session verification, and authorization transitions within a single
//! IMMEDIATE transaction. Nothing here is exposed to application code yet.

#![forbid(unsafe_code)]

/// Chat conversations (ADR-015). Unlike the authorization store below, this
/// holds only the user's own chat history and is used by the application.
pub mod conversations;

/// Desktop approval sessions and single-use approvals (ADR-016). Separate
/// from the browser-session authorization store below, which stays parked
/// for future remote access.
pub mod approvals;

/// Field-level encryption for stored chats.
pub mod crypto;

/// Passphrase-locked backup files (release plan feature 3).
pub mod backup;

#[cfg(test)]
mod storage;
