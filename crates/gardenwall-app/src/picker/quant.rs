//! The quantization table (`data/quantizations.json`): what a label in a
//! file name means in bits per weight and in quality words, and which files
//! in a repository are not models to chat with (projectors, draft layers).
//!
//! Data, not code, so a new format needs no release of logic. A label the
//! table doesn't know is still shown as written, judged by size alone.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const TABLE_JSON: &str = include_str!("../../data/quantizations.json");

#[derive(Debug, Deserialize)]
pub struct Table {
    pub version: u32,
    pub reviewed: String,
    quality: std::collections::BTreeMap<String, String>,
    formats: Vec<Format>,
    roles: Roles,
}

#[derive(Debug, Deserialize)]
struct Format {
    label: String,
    bits: Option<f64>,
    quality: String,
}

#[derive(Debug, Deserialize)]
struct Roles {
    projector: Vec<String>,
    draft: Vec<String>,
}

/// What a file is, from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Model,
    /// A vision model's image projector (mmproj).
    Projector,
    /// A draft or prediction layer that speeds up a larger model.
    Draft,
}

/// The quantization of one file, as far as the table knows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quant {
    /// The label as written in the file name, if one was found.
    pub label: Option<String>,
    pub bits: Option<f64>,
    /// Quality words ("small loss"), or `None` for an unknown label.
    pub quality: Option<String>,
    pub known: bool,
}

pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    // Checked by a test; a broken table can only come from a bad build.
    TABLE.get_or_init(|| serde_json::from_str(TABLE_JSON).expect("quantizations.json parses"))
}

fn stem(file_name: &str) -> String {
    let name = file_name.rsplit('/').next().unwrap_or(file_name);
    let name = name
        .strip_suffix(".gguf")
        .or_else(|| name.strip_suffix(".GGUF"))
        .unwrap_or(name);
    name.to_ascii_uppercase()
}

/// Pieces of a name that can hold a label: split on anything but letters,
/// digits and `_`, then every tail that starts a piece or follows a `_`
/// ("26B_Q4_0" offers "26B_Q4_0", "Q4_0" and "0").
fn candidates(stem: &str) -> impl Iterator<Item = &str> {
    stem.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .flat_map(|piece| {
            std::iter::once(piece)
                .chain(piece.match_indices('_').map(move |(i, _)| &piece[i + 1..]))
        })
        .filter(|c| !c.is_empty())
}

/// `Q4…` or `IQ4…`: what an unknown quantization label looks like.
fn looks_like_label(c: &str) -> bool {
    let b = c.as_bytes();
    let rest = if b.first() == Some(&b'I') { &b[1..] } else { b };
    rest.len() >= 2 && rest[0] == b'Q' && rest[1].is_ascii_digit()
}

impl Table {
    pub fn quant(&self, file_name: &str) -> Quant {
        let stem = stem(file_name);
        for c in candidates(&stem) {
            if let Some(f) = self.formats.iter().find(|f| f.label == c) {
                return Quant {
                    label: Some(f.label.clone()),
                    bits: f.bits,
                    quality: self.quality.get(&f.quality).cloned(),
                    known: true,
                };
            }
        }
        Quant {
            label: candidates(&stem)
                .find(|c| looks_like_label(c))
                .map(|c| c.chars().take(24).collect()),
            bits: None,
            quality: None,
            known: false,
        }
    }

    pub fn role(&self, file_name: &str) -> Role {
        let stem = stem(file_name);
        let marked = |words: &[String]| {
            words.iter().any(|w| {
                let w = w.to_ascii_uppercase();
                stem.starts_with(&w) || stem.contains(&format!("-{w}"))
            })
        };
        if marked(&self.roles.projector) {
            Role::Projector
        } else if marked(&self.roles.draft) {
            Role::Draft
        } else {
            Role::Model
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_parses_and_every_format_has_quality_words() {
        let t = table();
        assert!(t.version >= 1);
        assert!(super::super::dates::days_from_iso(&t.reviewed).is_some());
        let mut seen = std::collections::HashSet::new();
        for f in &t.formats {
            assert!(t.quality.contains_key(&f.quality), "{}", f.label);
            assert!(seen.insert(&f.label), "duplicate {}", f.label);
            assert_eq!(f.label, f.label.to_ascii_uppercase());
            assert!(f.bits.is_none_or(|b| b > 1.0 && b <= 32.0));
        }
    }

    #[test]
    fn labels_are_found_where_publishers_put_them() {
        let t = table();
        let label = |n: &str| t.quant(n).label;
        assert_eq!(label("Qwen3.5-9B-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(
            label("Qwen3.5-9B-UD-Q4_K_XL.gguf").as_deref(),
            Some("Q4_K_XL")
        );
        assert_eq!(label("gemma-4-26B_q4_0-it.gguf").as_deref(), Some("Q4_0"));
        assert_eq!(
            label("gemma-4-12b-it-qat-q4_0.gguf").as_deref(),
            Some("Q4_0")
        );
        assert_eq!(label("Qwen3-0.6B-Q8_0.gguf").as_deref(), Some("Q8_0"));
        assert_eq!(label("model-IQ4_XS.gguf").as_deref(), Some("IQ4_XS"));
        assert_eq!(
            label("sub/dir/Model-BF16-00001-of-00002.gguf").as_deref(),
            Some("BF16")
        );
        let q = t.quant("Qwen3.5-9B-Q4_K_M.gguf");
        assert!(q.known);
        assert_eq!(q.quality.as_deref(), Some("small loss"));
    }

    #[test]
    fn unknown_labels_are_shown_as_written() {
        let t = table();
        let q = t.quant("Model-Q4_K_ZZ.gguf");
        assert!(!q.known);
        assert_eq!(q.label.as_deref(), Some("Q4_K_ZZ"));
        assert_eq!(q.bits, None);
        let q = t.quant("model.gguf");
        assert!(!q.known);
        assert_eq!(q.label, None);
        // A model name that merely starts with Q is not a label.
        assert_eq!(t.quant("Qwen3-Base.gguf").label, None);
    }

    #[test]
    fn projectors_and_draft_layers_are_told_apart() {
        let t = table();
        assert_eq!(t.role("mmproj-F16.gguf"), Role::Projector);
        assert_eq!(t.role("gemma-4-E4B-it-mmproj.gguf"), Role::Projector);
        assert_eq!(t.role("mtp-Qwen3.8-27B-Q4_0.gguf"), Role::Draft);
        assert_eq!(t.role("dflash-Qwen3.8-27B-Q8_0.gguf"), Role::Draft);
        assert_eq!(t.role("Qwen3.8-27B-Q4_K_M.gguf"), Role::Model);
    }
}
