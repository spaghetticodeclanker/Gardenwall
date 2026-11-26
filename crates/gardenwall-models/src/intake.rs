//! Shared results of model intake (ADR-013 §2.2–§2.3).
//!
//! Every model, whether downloaded from a hub or imported from a local file,
//! ends up as a [`QuarantinedModel`]: a file in quarantine, named by its
//! SHA-256, that has passed GGUF validation, together with a [`Provenance`]
//! record. Provenance is informational; it never grants trust (ADR-002).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::gguf::GgufSummary;

/// How the model reached Gardenwall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelSource {
    /// Downloaded by Gardenwall, pinned to a commit and verified against the
    /// hub-declared SHA-256.
    Hub {
        hub: String,
        repo: String,
        commit: String,
        path: String,
        license: Option<String>,
    },
    /// Imported from a file the user selected.
    Local {
        /// Display-only rendering of the original file name (never used as
        /// a path).
        original_name: String,
        /// Whether the user supplied an expected SHA-256 and it matched.
        hash_verified: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: ModelSource,
    /// Display-only name (sanitized, bounded).
    pub display_name: String,
    pub size: u64,
    pub sha256: String,
    pub acquired_at_unix: u64,
}

/// A file that passed integrity checks and GGUF validation.
#[derive(Debug)]
pub struct QuarantinedModel {
    pub path: PathBuf,
    pub summary: GgufSummary,
    pub provenance: Provenance,
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const MAX_DISPLAY_CHARS: usize = 200;

/// Render an untrusted name for display: control and bidirectional-override
/// characters removed, length bounded.
pub fn display_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
                )
        })
        .take(MAX_DISPLAY_CHARS)
        .collect();
    if cleaned.trim().is_empty() {
        "unnamed model".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_sanitized() {
        assert_eq!(display_name("Qwen3 8B.gguf"), "Qwen3 8B.gguf");
        assert_eq!(display_name("evil\u{202E}fdp.gguf"), "evilfdp.gguf");
        assert_eq!(display_name("a\nb\u{7}c"), "abc");
        assert_eq!(display_name(""), "unnamed model");
        assert_eq!(
            display_name(&"x".repeat(500)).chars().count(),
            MAX_DISPLAY_CHARS
        );
    }

    #[test]
    fn provenance_serializes_with_tagged_source() {
        let p = Provenance {
            source: ModelSource::Local {
                original_name: "m.gguf".into(),
                hash_verified: true,
            },
            display_name: "m.gguf".into(),
            size: 1,
            sha256: "00".repeat(32),
            acquired_at_unix: 0,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"]["kind"], "local");
        let back: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }
}
