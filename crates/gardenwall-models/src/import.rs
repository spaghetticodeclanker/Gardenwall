//! Local model import (ADR-013 §2.2a).
//!
//! Imports a GGUF file the user selected (e.g. downloaded manually from a hub
//! or already present from another tool) through the same intake pipeline as
//! hub downloads:
//!
//! 1. The file is copied into quarantine under a temporary name while its
//!    SHA-256 is computed in the same pass.
//! 2. If the user supplied an expected SHA-256 (for example, copied from the
//!    hub's file page), it must match; otherwise the copy is deleted.
//! 3. The **quarantine copy** — never the original — is GGUF-validated, so a
//!    source modified during import cannot bypass validation.
//! 4. A valid copy is renamed to `<sha256>.gguf`; an invalid one is kept as
//!    `<sha256>.rejected` and can never be loaded.
//!
//! The original file is never modified, moved, or deleted. Its name is used
//! for display only. File extensions are not trusted: format is decided by
//! content validation, so non-GGUF formats (including pickle) are rejected
//! whatever they are called.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::gguf::{self, GgufError, GgufLimits};
use crate::intake::{self, ModelSource, Provenance, QuarantinedModel};
use crate::integrity::Sha256Digest;
use crate::split::{self, QuarantinedSplit, SplitPart};

#[derive(Debug)]
pub enum ImportError {
    Io(io::Error),
    /// The selected path is not a regular file.
    NotAFile,
    Cancelled,
    /// The file's size changed while it was being copied.
    SourceChanged,
    /// The user-supplied SHA-256 did not match. The copy has been deleted.
    HashMismatch,
    /// The copy failed GGUF validation and is kept as `.rejected`.
    InvalidGguf(GgufError),
    /// A split part whose file name doesn't follow the published pattern
    /// (`<name>-00001-of-0000N.gguf`), so its other parts can't be found.
    SplitModel,
    /// Parts of a split model are missing from the folder.
    IncompleteSplit(crate::split::SplitError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "file error: {e}"),
            Self::NotAFile => f.write_str("the selected path is not a regular file"),
            Self::Cancelled => f.write_str("import cancelled"),
            Self::SourceChanged => f.write_str("the file changed while it was being imported"),
            Self::HashMismatch => f.write_str("SHA-256 does not match the expected value"),
            Self::InvalidGguf(e) => write!(f, "model file rejected: {e}"),
            Self::SplitModel => f.write_str(
                "this file is one part of a split model, but its name was changed; give the parts their original names (name-00001-of-0000N.gguf) and import again",
            ),
            Self::IncompleteSplit(e) => write!(f, "{e}; put every part in the same folder"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Deletes the temporary quarantine file unless disarmed.
struct PartGuard {
    path: Option<PathBuf>,
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn create_part(dir: &Path) -> io::Result<(File, PathBuf)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    for attempt in 0..16u32 {
        let path = dir.join(format!(
            "import-{nanos:x}-{}-{attempt}.part",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique quarantine file",
    ))
}

/// Import `source` into `quarantine_dir`.
///
/// `progress(copied, total)` is called after each chunk; returning `false`
/// cancels and removes the partial copy.
pub fn import_local(
    source: &Path,
    quarantine_dir: &Path,
    expected_sha256: Option<&Sha256Digest>,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<QuarantinedModel, ImportError> {
    let mut input = File::open(source)?;
    let meta = input.metadata()?;
    if !meta.is_file() {
        return Err(ImportError::NotAFile);
    }
    let total = meta.len();

    std::fs::create_dir_all(quarantine_dir)?;
    let (mut output, part_path) = create_part(quarantine_dir)?;
    let mut guard = PartGuard {
        path: Some(part_path.clone()),
    };

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut copied: u64 = 0;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        copied = copied
            .checked_add(n as u64)
            .ok_or(ImportError::SourceChanged)?;
        if copied > total {
            return Err(ImportError::SourceChanged);
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        if !progress(copied, total) {
            return Err(ImportError::Cancelled);
        }
    }
    if copied != total {
        return Err(ImportError::SourceChanged);
    }
    output.sync_all()?;
    drop(output);

    let digest = Sha256Digest::from_bytes(hasher.finalize().into());
    if let Some(expected) = expected_sha256
        && !digest.ct_eq(expected)
    {
        return Err(ImportError::HashMismatch);
    }
    let digest_hex = digest.to_hex();

    let summary = match gguf::validate(File::open(&part_path)?, &GgufLimits::default()) {
        Ok(summary) => summary,
        Err(error) => {
            let rejected = quarantine_dir.join(format!("{digest_hex}.rejected"));
            if std::fs::rename(&part_path, &rejected).is_ok() {
                guard.path = None;
            }
            return Err(ImportError::InvalidGguf(error));
        }
    };
    if summary.split().is_some_and(|(_, count)| count > 1) {
        return Err(ImportError::SplitModel);
    }

    let final_path = quarantine_dir.join(format!("{digest_hex}.gguf"));
    if final_path.exists() {
        // Identical content was imported before; the guard removes our copy.
    } else {
        std::fs::rename(&part_path, &final_path)?;
        guard.path = None;
    }

    let original = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(QuarantinedModel {
        path: final_path,
        summary,
        provenance: Provenance {
            source: ModelSource::Local {
                original_name: intake::display_name(&original),
                hash_verified: expected_sha256.is_some(),
            },
            display_name: intake::display_name(&original),
            size: total,
            sha256: digest_hex,
            acquired_at_unix: intake::now_unix(),
        },
    })
}

/// Copy `source` to a new file in `dir`, hashing it in the same pass.
/// `progress(copied)` reports bytes of this file; returning `false` cancels.
fn copy_hashed(
    source: &Path,
    dir: &Path,
    mut progress: impl FnMut(u64) -> bool,
) -> Result<(PathBuf, Sha256Digest, u64), ImportError> {
    let mut input = File::open(source)?;
    let meta = input.metadata()?;
    if !meta.is_file() {
        return Err(ImportError::NotAFile);
    }
    let total = meta.len();
    let (mut output, part_path) = create_part(dir)?;
    let mut guard = PartGuard {
        path: Some(part_path.clone()),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut copied: u64 = 0;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        copied = copied
            .checked_add(n as u64)
            .ok_or(ImportError::SourceChanged)?;
        if copied > total {
            return Err(ImportError::SourceChanged);
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        if !progress(copied) {
            return Err(ImportError::Cancelled);
        }
    }
    if copied != total {
        return Err(ImportError::SourceChanged);
    }
    output.sync_all()?;
    guard.path = None;
    Ok((
        part_path,
        Sha256Digest::from_bytes(hasher.finalize().into()),
        total,
    ))
}

/// The parts of the split set `source` belongs to, in order, found by name
/// in the same folder; `None` when `source` isn't named like a split part.
pub fn split_siblings(source: &Path) -> Result<Option<Vec<PathBuf>>, ImportError> {
    let Some(name) = source.file_name().and_then(|n| n.to_str()) else {
        return Ok(None);
    };
    let Some((base, _, count)) = split::parse_split_name(name) else {
        return Ok(None);
    };
    let dir = source.parent().ok_or(ImportError::NotAFile)?;
    let paths: Vec<PathBuf> = (1..=count)
        .map(|i| dir.join(format!("{base}-{i:05}-of-{count:05}.gguf")))
        .collect();
    let present: Vec<u32> = (1..=count)
        .filter(|i| paths[(*i - 1) as usize].is_file())
        .collect();
    split::check_complete(&present, count).map_err(ImportError::IncompleteSplit)?;
    Ok(Some(paths))
}

/// Import every part of a split model (`parts` from [`split_siblings`])
/// into `<library_dir>/<set id>/` under their names: each part is copied and
/// hashed, the copies (never the originals) are validated and checked
/// against each other, as for a download. The originals are not changed.
///
/// `progress(copied, total)` covers the whole set.
pub fn import_split(
    parts: &[PathBuf],
    library_dir: &Path,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<QuarantinedSplit, ImportError> {
    std::fs::create_dir_all(library_dir)?;
    let mut total = 0u64;
    for p in parts {
        total += std::fs::metadata(p)?.len();
    }
    // Staging folder, removed on any failure.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = library_dir.join(format!("import-{nanos:x}-{}.split", std::process::id()));
    std::fs::create_dir(&staging)?;
    struct DirGuard(Option<PathBuf>);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            if let Some(d) = self.0.take() {
                let _ = std::fs::remove_dir_all(d);
            }
        }
    }
    let mut guard = DirGuard(Some(staging.clone()));

    let mut copies = Vec::with_capacity(parts.len());
    let mut offset = 0u64;
    for source in parts {
        let base = offset;
        let (path, digest, size) = copy_hashed(source, &staging, |n| progress(base + n, total))?;
        offset += size;
        let file_name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(ImportError::NotAFile)?
            .to_string();
        copies.push((file_name, path, digest.to_hex(), size));
    }

    let mut summaries = Vec::with_capacity(copies.len());
    for (_, path, _, _) in &copies {
        summaries.push(
            gguf::validate_part(File::open(path)?, &GgufLimits::default())
                .map_err(ImportError::InvalidGguf)?,
        );
    }
    if summaries[0].get("general.architecture").is_none() {
        return Err(ImportError::InvalidGguf(GgufError::MissingArchitecture));
    }
    let tensor_count = split::check_parts(&summaries).map_err(ImportError::IncompleteSplit)?;

    let id = split::set_id(copies.iter().map(|(_, _, d, _)| d.as_str()));
    let final_dir = library_dir.join(&id);
    std::fs::create_dir_all(&final_dir)?;
    for (name, path, _, _) in &copies {
        std::fs::rename(path, final_dir.join(name))?;
    }
    if let Some(dir) = guard.0.take() {
        let _ = std::fs::remove_dir_all(dir);
    }

    let first = &copies[0].0;
    let display = split::parse_split_name(first)
        .map(|(base, _, _)| format!("{base}.gguf"))
        .unwrap_or_else(|| first.clone());
    Ok(QuarantinedSplit {
        model: QuarantinedModel {
            path: final_dir.join(first),
            summary: summaries.into_iter().next().expect("at least two parts"),
            provenance: Provenance {
                source: ModelSource::Local {
                    original_name: intake::display_name(&display),
                    hash_verified: false,
                },
                display_name: intake::display_name(&display),
                size: total,
                sha256: id,
                acquired_at_unix: intake::now_unix(),
            },
        },
        parts: copies
            .into_iter()
            .map(|(file_name, _, sha256, size)| SplitPart {
                file_name,
                sha256,
                size,
            })
            .collect(),
        tensor_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal GGUF v3 with the given extra u32 metadata keys and no tensors.
    fn gguf(extra_u32: &[(&str, u32)]) -> Vec<u8> {
        let mut kv = Vec::new();
        let mut count = 0u64;
        let mut put = |key: &str, tag: u32, value: &[u8]| {
            kv.extend_from_slice(&(key.len() as u64).to_le_bytes());
            kv.extend_from_slice(key.as_bytes());
            kv.extend_from_slice(&tag.to_le_bytes());
            kv.extend_from_slice(value);
            count += 1;
        };
        let mut arch = Vec::new();
        arch.extend_from_slice(&5u64.to_le_bytes());
        arch.extend_from_slice(b"llama");
        put("general.architecture", 8, &arch);
        for (k, v) in extra_u32 {
            put(k, 4, &v.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&kv);
        out.resize(out.len().next_multiple_of(32) + 1000, 7);
        out
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest::from_bytes(Sha256::digest(bytes).into())
    }

    struct Dirs {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        quarantine: PathBuf,
    }

    fn dirs() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let quarantine = tmp.path().join("quarantine");
        std::fs::create_dir(&src).unwrap();
        Dirs {
            _tmp: tmp,
            src,
            quarantine,
        }
    }

    fn quarantine_files(d: &Dirs) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&d.quarantine)
            .map(|rd| {
                rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn imports_valid_file_and_leaves_original_untouched() {
        let d = dirs();
        let bytes = gguf(&[]);
        let source = d.src.join("My Model Q4_K_M.gguf");
        std::fs::write(&source, &bytes).unwrap();

        let model = import_local(&source, &d.quarantine, None, |_, _| true).unwrap();
        let hex = digest(&bytes).to_hex();
        assert_eq!(model.path, d.quarantine.join(format!("{hex}.gguf")));
        assert_eq!(std::fs::read(&model.path).unwrap(), bytes);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(model.summary.architecture(), "llama");
        assert_eq!(model.provenance.sha256, hex);
        assert_eq!(
            model.provenance.source,
            ModelSource::Local {
                original_name: "My Model Q4_K_M.gguf".into(),
                hash_verified: false
            }
        );
        assert_eq!(quarantine_files(&d), vec![format!("{hex}.gguf")]);
    }

    #[test]
    fn expected_hash_is_verified() {
        let d = dirs();
        let bytes = gguf(&[]);
        let source = d.src.join("m.gguf");
        std::fs::write(&source, &bytes).unwrap();

        let ok = import_local(&source, &d.quarantine, Some(&digest(&bytes)), |_, _| true).unwrap();
        assert!(matches!(
            ok.provenance.source,
            ModelSource::Local {
                hash_verified: true,
                ..
            }
        ));

        let d = dirs();
        std::fs::write(d.src.join("m.gguf"), &bytes).unwrap();
        let wrong = digest(b"something else");
        let err = import_local(
            &d.src.join("m.gguf"),
            &d.quarantine,
            Some(&wrong),
            |_, _| true,
        )
        .unwrap_err();
        assert!(matches!(err, ImportError::HashMismatch));
        assert!(
            quarantine_files(&d).is_empty(),
            "mismatched copy must be deleted"
        );
    }

    #[test]
    fn non_gguf_content_is_rejected_regardless_of_name() {
        let d = dirs();
        // A pickle-style payload disguised with a .gguf extension.
        let source = d.src.join("model.gguf");
        std::fs::write(&source, b"\x80\x04\x95 pickle payload").unwrap();
        let err = import_local(&source, &d.quarantine, None, |_, _| true).unwrap_err();
        assert!(matches!(err, ImportError::InvalidGguf(_)));
        let files = quarantine_files(&d);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with(".rejected"));
    }

    #[test]
    fn valid_gguf_without_extension_is_accepted() {
        // e.g. a content-addressed blob from another tool's model store.
        let d = dirs();
        let source = d.src.join("sha256-abcdef");
        std::fs::write(&source, gguf(&[])).unwrap();
        assert!(import_local(&source, &d.quarantine, None, |_, _| true).is_ok());
    }

    #[test]
    fn cancellation_removes_partial_copy() {
        let d = dirs();
        let source = d.src.join("m.gguf");
        std::fs::write(&source, gguf(&[])).unwrap();
        let err = import_local(&source, &d.quarantine, None, |_, _| false).unwrap_err();
        assert!(matches!(err, ImportError::Cancelled));
        assert!(quarantine_files(&d).is_empty());
    }

    #[test]
    fn directories_are_not_imported() {
        let d = dirs();
        let err = import_local(&d.src, &d.quarantine, None, |_, _| true).unwrap_err();
        assert!(matches!(err, ImportError::NotAFile | ImportError::Io(_)));
    }

    #[test]
    fn a_split_part_is_not_imported_as_a_single_model() {
        // A part imported on its own (e.g. renamed) is refused by content.
        let d = dirs();
        let source = d.src.join("renamed.gguf");
        std::fs::write(&source, gguf(&[("split.no", 0), ("split.count", 2)])).unwrap();
        let err = import_local(&source, &d.quarantine, None, |_, _| true).unwrap_err();
        assert!(matches!(err, ImportError::SplitModel));
        assert!(quarantine_files(&d).is_empty());
    }

    fn write_parts(d: &Dirs, second_no: u32) -> Vec<PathBuf> {
        let parts = [
            d.src.join("m-00001-of-00002.gguf"),
            d.src.join("m-00002-of-00002.gguf"),
        ];
        std::fs::write(&parts[0], gguf(&[("split.no", 0), ("split.count", 2)])).unwrap();
        std::fs::write(
            &parts[1],
            gguf(&[("split.no", second_no), ("split.count", 2)]),
        )
        .unwrap();
        parts.to_vec()
    }

    #[test]
    fn split_models_import_as_a_checked_set() {
        let d = dirs();
        let parts = write_parts(&d, 1);
        // Either part finds the whole set.
        assert_eq!(split_siblings(&parts[1]).unwrap().unwrap(), parts);
        assert!(split_siblings(&d.src.join("plain.gguf")).unwrap().is_none());
        let total: u64 = parts
            .iter()
            .map(|p| std::fs::metadata(p).unwrap().len())
            .sum();
        let mut last = 0;
        let model = import_split(&parts, &d.quarantine, |done, all| {
            assert_eq!(all, total);
            last = done;
            true
        })
        .unwrap();
        assert_eq!(last, total);
        let id = &model.model.provenance.sha256;
        assert_eq!(id.len(), 64);
        assert_eq!(model.model.provenance.display_name, "m.gguf");
        assert_eq!(model.parts.len(), 2);
        for p in &parts {
            // Copied under their names; the originals are untouched.
            assert!(d.quarantine.join(id).join(p.file_name().unwrap()).is_file());
            assert!(p.is_file());
        }
        // No staging folder is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&d.quarantine)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".split"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn incomplete_or_mismatched_split_sets_are_refused() {
        let d = dirs();
        let parts = write_parts(&d, 1);
        std::fs::remove_file(&parts[1]).unwrap();
        assert!(matches!(
            split_siblings(&parts[0]),
            Err(ImportError::IncompleteSplit(_))
        ));
        // Part 2 claims to be part 1: refused, nothing kept.
        let parts = write_parts(&d, 0);
        assert!(matches!(
            import_split(&parts, &d.quarantine, |_, _| true),
            Err(ImportError::IncompleteSplit(_))
        ));
        assert!(std::fs::read_dir(&d.quarantine).map_or(0, |r| r.count()) == 0);
    }

    #[test]
    fn reimporting_identical_content_is_idempotent() {
        let d = dirs();
        let bytes = gguf(&[]);
        std::fs::write(d.src.join("a.gguf"), &bytes).unwrap();
        std::fs::write(d.src.join("b.gguf"), &bytes).unwrap();
        let a = import_local(&d.src.join("a.gguf"), &d.quarantine, None, |_, _| true).unwrap();
        let b = import_local(&d.src.join("b.gguf"), &d.quarantine, None, |_, _| true).unwrap();
        assert_eq!(a.path, b.path);
        assert_eq!(quarantine_files(&d).len(), 1);
    }

    #[test]
    fn display_name_of_hostile_file_name_is_sanitized() {
        let d = dirs();
        let source = d.src.join("evil\u{202E}fdp.gguf");
        std::fs::write(&source, gguf(&[])).unwrap();
        let model = import_local(&source, &d.quarantine, None, |_, _| true).unwrap();
        assert_eq!(model.provenance.display_name, "evilfdp.gguf");
    }
}
