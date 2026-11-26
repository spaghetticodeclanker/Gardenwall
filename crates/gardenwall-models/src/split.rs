//! Split (multi-file) GGUF models: `<base>-00001-of-00003.gguf` and so on.
//!
//! Hugging Face caps a file at 50 GB, so larger models ship as a set of
//! parts written by llama.cpp's `gguf-split`. The first part carries all the
//! metadata (architecture, tokenizer, chat template); every part records its
//! index and the part count. llama.cpp loads the set from the first part and
//! finds the others by name in the same folder.
//!
//! A set is admitted only when every part is present once, each part's
//! digest matches the published one, and the parts agree with each other.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gguf::{GgufSummary, MetadataValue};
use crate::intake::QuarantinedModel;

/// One part of a split model, as stored in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitPart {
    pub file_name: String,
    pub sha256: String,
    pub size: u64,
}

/// A verified split model in its library folder.
#[derive(Debug)]
pub struct QuarantinedSplit {
    /// The first part (the one llama.cpp is given) plus the set's
    /// provenance; `provenance.sha256` is the set id ([`set_id`]).
    pub model: QuarantinedModel,
    pub parts: Vec<SplitPart>,
    pub tensor_count: usize,
}

/// Most parts accepted in one set.
pub const MAX_PARTS: u32 = 256;

/// The parts of `<base>-NNNNN-of-MMMMM.gguf`: `(base, index, count)` with a
/// 1-based index. Exactly five digits each, as `gguf-split` writes them.
pub fn parse_split_name(file_name: &str) -> Option<(&str, u32, u32)> {
    let stem = file_name
        .strip_suffix(".gguf")
        .or_else(|| file_name.strip_suffix(".GGUF"))?;
    // "-00001-of-00003" is 15 characters.
    if stem.len() <= 15 || !stem.is_char_boundary(stem.len() - 15) {
        return None;
    }
    let (base, tail) = stem.split_at(stem.len() - 15);
    let tail = tail.strip_prefix('-')?;
    let (index, count) = tail.split_once("-of-")?;
    let digits = |s: &str| s.len() == 5 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(index) || !digits(count) || base.is_empty() {
        return None;
    }
    let (index, count): (u32, u32) = (index.parse().ok()?, count.parse().ok()?);
    ((2..=MAX_PARTS).contains(&count) && (1..=count).contains(&index))
        .then_some((base, index, count))
}

/// The library id of a set: SHA-256 over the parts' digests in order, with
/// a domain prefix so it can never equal a single file's digest.
pub fn set_id<'a>(part_digests: impl IntoIterator<Item = &'a str>) -> String {
    let mut h = Sha256::new();
    h.update(b"gardenwall-split-v1\n");
    for d in part_digests {
        h.update(d.as_bytes());
        h.update(b"\n");
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// Not every part 1..=count is present exactly once.
    Incomplete { count: u32, missing: Vec<u32> },
    /// A part's own metadata disagrees with its place in the set.
    Inconsistent(String),
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete { count, missing } => write!(
                f,
                "the model's {count} parts are not all available (missing part {})",
                missing
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Inconsistent(why) => write!(f, "the model's parts don't match: {why}"),
        }
    }
}

impl std::error::Error for SplitError {}

/// Check that parts `1..=count` are each present once (`indices` as parsed
/// from the names).
pub fn check_complete(indices: &[u32], count: u32) -> Result<(), SplitError> {
    let mut seen = vec![0u32; count as usize];
    for &i in indices {
        if i == 0 || i > count {
            return Err(SplitError::Inconsistent(format!("part {i} of {count}")));
        }
        seen[(i - 1) as usize] += 1;
    }
    if seen.iter().any(|&n| n > 1) {
        return Err(SplitError::Inconsistent("a part appears twice".into()));
    }
    let missing: Vec<u32> = (1..=count)
        .filter(|i| seen[(*i - 1) as usize] == 0)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SplitError::Incomplete { count, missing })
    }
}

fn u64_of(s: &GgufSummary, key: &str) -> Option<u64> {
    s.get(key).and_then(MetadataValue::as_u64)
}

/// Check validated parts, in order, against each other: each records its
/// own index and the same count, and the first carries the architecture.
/// Returns the total tensor count.
pub fn check_parts(parts: &[GgufSummary]) -> Result<usize, SplitError> {
    let count = parts.len() as u64;
    for (i, s) in parts.iter().enumerate() {
        let no = u64_of(s, "split.no");
        let n = u64_of(s, "split.count");
        if no != Some(i as u64) || n != Some(count) {
            return Err(SplitError::Inconsistent(format!(
                "part {} says it is part {} of {}",
                i + 1,
                no.map_or("?".into(), |v| (v + 1).to_string()),
                n.map_or("?".into(), |v| v.to_string())
            )));
        }
    }
    let first = parts
        .first()
        .ok_or_else(|| SplitError::Inconsistent("no parts".into()))?;
    if first.get("general.architecture").is_none() {
        return Err(SplitError::Inconsistent(
            "the first part has no model description".into(),
        ));
    }
    let tensors: usize = parts.iter().map(|s| s.tensors.len()).sum();
    if let Some(expected) = u64_of(first, "split.tensors.count")
        && expected != tensors as u64
    {
        return Err(SplitError::Inconsistent(format!(
            "the parts hold {tensors} tensors, but the first part says {expected}"
        )));
    }
    Ok(tensors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_names_are_parsed_strictly() {
        assert_eq!(
            parse_split_name("Swift-Qwen3.8-IQ2_XS-00001-of-00002.gguf"),
            Some(("Swift-Qwen3.8-IQ2_XS", 1, 2))
        );
        assert_eq!(parse_split_name("m-00006-of-00006.gguf"), Some(("m", 6, 6)));
        for bad in [
            "model.gguf",
            "m-00000-of-00002.gguf",
            "m-00003-of-00002.gguf",
            "m-00001-of-00001.gguf",
            "m-0001-of-0002.gguf",
            "m-00001-of-00002.bin",
            "-00001-of-00002.gguf",
            "m-0000a-of-00002.gguf",
            "m-00001-of-99999.gguf",
        ] {
            assert_eq!(parse_split_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn set_ids_depend_on_every_part_and_order() {
        let a = set_id(["aa", "bb"]);
        assert_eq!(a.len(), 64);
        assert_ne!(a, set_id(["bb", "aa"]));
        assert_ne!(a, set_id(["aa"]));
        assert_eq!(a, set_id(["aa", "bb"]));
    }

    #[test]
    fn completeness() {
        assert!(check_complete(&[2, 1, 3], 3).is_ok());
        assert_eq!(
            check_complete(&[1, 3], 3),
            Err(SplitError::Incomplete {
                count: 3,
                missing: vec![2]
            })
        );
        assert!(check_complete(&[1, 1, 2], 2).is_err());
        assert!(check_complete(&[1, 4], 2).is_err());
    }
}
