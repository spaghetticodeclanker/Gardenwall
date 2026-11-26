//! Safe extraction of runtime archives.
//!
//! Archives come from pinned, hash-verified downloads, but extraction is
//! still written defensively, as if the archive were hostile:
//! - every entry path is validated component by component: no absolute or
//!   drive paths, no `.`/`..`, no backslashes, no `:` (alternate data
//!   streams), no characters or names Windows treats specially;
//! - symlinks, hard links, devices, FIFOs, and other special entries are
//!   rejected;
//! - files are created with `create_new`, so duplicate entries fail instead
//!   of overwriting;
//! - entry count, per-file size, and total decompressed size are capped
//!   (zip/gzip bombs);
//! - extraction targets a fresh staging directory that contains no links;
//! - Unix permissions are normalized to 0o755 (executable) or 0o644, which
//!   drops setuid/setgid/sticky bits.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_file_bytes: 4 << 30,
            max_total_bytes: 8 << 30,
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    Io(io::Error),
    Archive(String),
    UnsafePath(String),
    Link(String),
    SpecialEntry(String),
    Duplicate(String),
    TooManyEntries,
    TooLarge,
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "extraction I/O error: {e}"),
            Self::Archive(e) => write!(f, "invalid archive: {e}"),
            Self::UnsafePath(p) => write!(f, "archive entry has an unsafe path: {p:?}"),
            Self::Link(p) => write!(f, "archive entry is a link: {p:?}"),
            Self::SpecialEntry(p) => write!(f, "archive entry is not a regular file: {p:?}"),
            Self::Duplicate(p) => write!(f, "duplicate archive entry: {p:?}"),
            Self::TooManyEntries => f.write_str("archive has too many entries"),
            Self::TooLarge => f.write_str("archive contents exceed the size limit"),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

const RESERVED: &[&str] = &[
    "con",
    "prn",
    "aux",
    "nul",
    "com1",
    "com2",
    "com3",
    "com4",
    "com5",
    "com6",
    "com7",
    "com8",
    "com9",
    "lpt1",
    "lpt2",
    "lpt3",
    "lpt4",
    "lpt5",
    "lpt6",
    "lpt7",
    "lpt8",
    "lpt9",
    "conin$",
    "conout$",
    "com\u{b9}",
    "com\u{b2}",
    "com\u{b3}",
    "lpt\u{b9}",
    "lpt\u{b2}",
    "lpt\u{b3}",
];

/// Whether a file name is a Windows device name (`CON`, `nul.txt`,
/// `COM1 .log`, `COM¹`), which opens a device instead of a file.
pub fn is_device_name(component: &str) -> bool {
    let lower = component.to_lowercase();
    let stem = lower.split('.').next().unwrap_or("").trim_end();
    RESERVED.contains(&stem)
}
const MAX_DEPTH: usize = 32;
const MAX_COMPONENT: usize = 255;

/// Validate an archive entry name and convert it to a relative path.
/// Directory entries may end with a single `/`.
pub fn safe_relative(name: &str) -> Result<PathBuf, ExtractError> {
    let unsafe_path = || ExtractError::UnsafePath(name.chars().take(200).collect());
    let trimmed = name.strip_prefix("./").unwrap_or(name);
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed.contains(['\\', '\0']) || trimmed.starts_with('/') {
        return Err(unsafe_path());
    }
    let mut path = PathBuf::new();
    let mut depth = 0;
    for component in trimmed.split('/') {
        depth += 1;
        let bad = component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > MAX_COMPONENT
            || component.ends_with(['.', ' '])
            || component
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
            || is_device_name(component);
        if bad || depth > MAX_DEPTH {
            return Err(unsafe_path());
        }
        path.push(component);
    }
    Ok(path)
}

/// Running totals shared across all archives extracted into one directory.
#[derive(Debug, Default)]
pub struct Totals {
    entries: usize,
    bytes: u64,
}

impl Totals {
    fn add_entry(&mut self, limits: &ExtractLimits) -> Result<(), ExtractError> {
        self.entries += 1;
        if self.entries > limits.max_entries {
            return Err(ExtractError::TooManyEntries);
        }
        Ok(())
    }
}

fn write_file(
    dest: &Path,
    rel: &Path,
    name: &str,
    reader: &mut dyn Read,
    executable: bool,
    limits: &ExtractLimits,
    totals: &mut Totals,
) -> Result<(), ExtractError> {
    let target = dest.join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExtractError::Duplicate(name.chars().take(200).collect()));
        }
        Err(e) => return Err(e.into()),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        totals.bytes += n as u64;
        if written > limits.max_file_bytes || totals.bytes > limits.max_total_bytes {
            drop(out);
            let _ = fs::remove_file(&target);
            return Err(ExtractError::TooLarge);
        }
        out.write_all(&buf[..n])?;
    }
    out.sync_all()?;
    set_mode(&target, executable)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) -> io::Result<()> {
    Ok(())
}

/// Extract a zip archive into `dest`.
pub fn extract_zip(
    archive: &Path,
    dest: &Path,
    limits: &ExtractLimits,
    totals: &mut Totals,
) -> Result<(), ExtractError> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)
        .map_err(|e| ExtractError::Archive(e.to_string()))?;
    if zip.len() > limits.max_entries {
        return Err(ExtractError::TooManyEntries);
    }
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ExtractError::Archive(e.to_string()))?;
        totals.add_entry(limits)?;
        let name = entry
            .name()
            .map_err(|e| ExtractError::Archive(e.to_string()))?
            .into_owned();
        if entry.is_symlink() {
            return Err(ExtractError::Link(name));
        }
        let rel = safe_relative(&name)?;
        if entry.is_dir() {
            fs::create_dir_all(dest.join(&rel))?;
            continue;
        }
        if entry.size() > limits.max_file_bytes {
            return Err(ExtractError::TooLarge);
        }
        let executable = entry.unix_mode().is_some_and(|m| m & 0o111 != 0);
        write_file(dest, &rel, &name, &mut entry, executable, limits, totals)?;
    }
    Ok(())
}

/// Extract a gzip-compressed tar archive into `dest`.
pub fn extract_tar_gz(
    archive: &Path,
    dest: &Path,
    limits: &ExtractLimits,
    totals: &mut Totals,
) -> Result<(), ExtractError> {
    let gz = flate2::read::GzDecoder::new(File::open(archive)?);
    extract_tar(gz, dest, limits, totals)
}

/// The same checks for every tar: no links or special entries, safe
/// relative names, size limits.
fn extract_tar(
    reader: impl Read,
    dest: &Path,
    limits: &ExtractLimits,
    totals: &mut Totals,
) -> Result<(), ExtractError> {
    let mut tar = tar::Archive::new(reader);
    for entry in tar.entries()? {
        let mut entry = entry?;
        totals.add_entry(limits)?;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| ExtractError::UnsafePath("<non-UTF-8 name>".into()))?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(ExtractError::Link(name));
        }
        let rel = safe_relative(&name)?;
        if kind.is_dir() {
            fs::create_dir_all(dest.join(&rel))?;
            continue;
        }
        if !(kind.is_file() || kind.is_contiguous()) {
            return Err(ExtractError::SpecialEntry(name));
        }
        if entry.header().size().unwrap_or(u64::MAX) > limits.max_file_bytes {
            return Err(ExtractError::TooLarge);
        }
        let executable = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
        write_file(dest, &rel, &name, &mut entry, executable, limits, totals)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    /// Build a tar.gz from raw headers so hostile names can be expressed.
    fn tar_gz_with(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (name, kind, data) in entries {
            let mut header = tar::Header::new_old();
            {
                let old = header.as_old_mut();
                old.name[..name.len()].copy_from_slice(name.as_bytes());
            }
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o4755);
            header.set_cksum();
            raw.extend_from_slice(header.as_bytes());
            raw.extend_from_slice(data);
            raw.resize(raw.len().next_multiple_of(512), 0);
        }
        raw.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        gz.finish().unwrap()
    }

    fn run_zip(
        bytes: &[u8],
        limits: &ExtractLimits,
    ) -> (tempfile::TempDir, Result<(), ExtractError>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(&archive, bytes).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let r = extract_zip(&archive, &dest, limits, &mut Totals::default());
        (dir, r)
    }

    fn run_tar(bytes: &[u8]) -> (tempfile::TempDir, Result<(), ExtractError>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        fs::write(&archive, bytes).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let r = extract_tar_gz(
            &archive,
            &dest,
            &ExtractLimits::default(),
            &mut Totals::default(),
        );
        (dir, r)
    }

    #[test]
    fn path_validation() {
        for ok in [
            "a",
            "a/b.dll",
            "dir/",
            "./a/b",
            "llama-b1-bin/llama-server.exe",
        ] {
            assert!(safe_relative(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "a\\b",
            "..\\x",
            "C:/x",
            "C:x",
            "a:stream",
            "x/NUL",
            "nul.txt",
            "CON",
            "COM1.dll",
            "a/b.",
            "a/b ",
            "a\u{0}b",
            "a\nb",
            "a/<b>",
            "a/?",
            "a/*",
        ] {
            assert!(safe_relative(bad).is_err(), "{bad:?}");
        }
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        assert!(safe_relative(&deep).is_err());
    }

    #[test]
    fn extracts_a_normal_zip() {
        let bytes = zip_with(&[
            ("bin/", b""),
            ("bin/llama-server.exe", b"MZ..."),
            ("bin/ggml.dll", b"dll"),
        ]);
        let (dir, r) = run_zip(&bytes, &ExtractLimits::default());
        r.unwrap();
        assert_eq!(
            fs::read(dir.path().join("out/bin/llama-server.exe")).unwrap(),
            b"MZ..."
        );
    }

    #[test]
    fn zip_slip_and_hostile_names_are_rejected() {
        for name in [
            "../evil.dll",
            "/abs.dll",
            "a\\..\\evil.dll",
            "C:/evil.dll",
            "x.exe:ads",
        ] {
            let bytes = zip_with(&[(name, b"payload")]);
            let (dir, r) = run_zip(&bytes, &ExtractLimits::default());
            assert!(
                matches!(r, Err(ExtractError::UnsafePath(_))),
                "{name}: {r:?}"
            );
            assert!(!dir.path().join("evil.dll").exists());
        }
    }

    #[test]
    fn zip_symlinks_and_duplicates_are_rejected() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.add_symlink(
            "link",
            "../../etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let (_d, r) = run_zip(&bytes, &ExtractLimits::default());
        assert!(matches!(r, Err(ExtractError::Link(_))), "{r:?}");

        // Same file twice (case variants collide on Windows/macOS too).
        let bytes = zip_with(&[("a.dll", b"1"), ("a/../a.dll", b"2")]);
        let (_d, r) = run_zip(&bytes, &ExtractLimits::default());
        assert!(matches!(r, Err(ExtractError::UnsafePath(_))));
    }

    #[test]
    fn size_and_entry_limits_are_enforced() {
        let big = vec![0u8; 1 << 20];
        let bytes = zip_with(&[("big.bin", &big)]);
        let limits = ExtractLimits {
            max_file_bytes: 1000,
            ..ExtractLimits::default()
        };
        let (dir, r) = run_zip(&bytes, &limits);
        assert!(matches!(r, Err(ExtractError::TooLarge)));
        assert!(!dir.path().join("out/big.bin").exists());

        let bytes = zip_with(&[("a", b"1"), ("b", b"2"), ("c", b"3")]);
        let limits = ExtractLimits {
            max_entries: 2,
            ..ExtractLimits::default()
        };
        let (_d, r) = run_zip(&bytes, &limits);
        assert!(matches!(r, Err(ExtractError::TooManyEntries)));
    }

    #[test]
    fn extracts_a_normal_tar_gz_and_normalizes_modes() {
        let bytes = tar_gz_with(&[
            ("build/", tar::EntryType::Directory, b""),
            ("build/llama-server", tar::EntryType::Regular, b"\x7fELF"),
        ]);
        let (dir, r) = run_tar(&bytes);
        r.unwrap();
        let path = dir.path().join("out/build/llama-server");
        assert_eq!(fs::read(&path).unwrap(), b"\x7fELF");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // setuid from the header (0o4755) is dropped.
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o755
            );
        }
    }

    #[test]
    fn tar_links_specials_and_traversal_are_rejected() {
        for (name, kind) in [
            ("link", tar::EntryType::Symlink),
            ("hard", tar::EntryType::Link),
            ("dev", tar::EntryType::Char),
            ("fifo", tar::EntryType::Fifo),
            ("../evil", tar::EntryType::Regular),
            ("/abs", tar::EntryType::Regular),
        ] {
            let bytes = tar_gz_with(&[(name, kind, b"")]);
            let (_d, r) = run_tar(&bytes);
            assert!(
                matches!(
                    r,
                    Err(ExtractError::Link(_)
                        | ExtractError::SpecialEntry(_)
                        | ExtractError::UnsafePath(_))
                ),
                "{name}: {r:?}"
            );
        }
    }

    #[test]
    fn corrupt_archives_fail_cleanly() {
        let (_d, r) = run_zip(b"not a zip", &ExtractLimits::default());
        assert!(matches!(r, Err(ExtractError::Archive(_))));
        let (_d, r) = run_tar(b"not gzip");
        assert!(r.is_err());
    }
}
