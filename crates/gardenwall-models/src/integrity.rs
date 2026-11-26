//! Integrity verification and safe naming for model artifacts (ADR-013 §2.2).
//!
//! Hub-supplied file names and hashes are untrusted input. Names are checked
//! against a strict allowlist before they are ever joined to a local path, and
//! hashes are compared in constant time against a locally computed digest.

use std::fmt;
use std::io::{self, Read};

use sha2::{Digest, Sha256};

/// A SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse exactly 64 hexadecimal characters (either case).
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, &[high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let hi = hex_value(high)?;
            let lo = hex_value(low)?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }

    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(DIGITS[usize::from(byte >> 4)] as char);
            s.push(DIGITS[usize::from(byte & 0x0F)] as char);
        }
        s
    }

    /// Constant-time comparison. Digests are not secret, but this avoids
    /// leaking match position through timing as a matter of hygiene.
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Compute the SHA-256 of a stream, reporting cumulative bytes to `progress`.
/// `progress` returning `false` cancels hashing with `ErrorKind::Interrupted`.
pub fn sha256_reader<R: Read>(
    mut reader: R,
    mut progress: impl FnMut(u64) -> bool,
) -> io::Result<(Sha256Digest, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buf[..n]);
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("stream length overflow"))?;
        if !progress(total) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "hashing cancelled",
            ));
        }
    }
    Ok((Sha256Digest(hasher.finalize().into()), total))
}

/// Outcome of verifying an artifact against its expected digest and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Match,
    SizeMismatch { expected: u64, actual: u64 },
    DigestMismatch,
}

/// Verify a stream against the hub-declared digest and size.
pub fn verify_reader<R: Read>(
    reader: R,
    expected_digest: &Sha256Digest,
    expected_size: u64,
    progress: impl FnMut(u64) -> bool,
) -> io::Result<Verification> {
    let (digest, size) = sha256_reader(reader, progress)?;
    if size != expected_size {
        return Ok(Verification::SizeMismatch {
            expected: expected_size,
            actual: size,
        });
    }
    if !digest.ct_eq(expected_digest) {
        return Ok(Verification::DigestMismatch);
    }
    Ok(Verification::Match)
}

/// Why a hub-supplied file name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafeFileName {
    Empty,
    TooLong,
    DisallowedCharacter,
    LeadingDotOrDash,
    NotGguf,
    ReservedDeviceName,
}

impl fmt::Display for UnsafeFileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "file name is empty",
            Self::TooLong => "file name is too long",
            Self::DisallowedCharacter => "file name contains a disallowed character",
            Self::LeadingDotOrDash => "file name starts with '.' or '-'",
            Self::NotGguf => "file is not a .gguf file",
            Self::ReservedDeviceName => "file name is a reserved device name",
        })
    }
}

impl std::error::Error for UnsafeFileName {}

const MAX_FILE_NAME_BYTES: usize = 200;

/// Windows reserved device names (checked case-insensitively, with or
/// without an extension) so the same rule holds on every platform.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Validate the final path component of a hub file (e.g. `model-Q4_K_M.gguf`)
/// for use as a local file name. Directories in the hub path must be stripped
/// by the caller *and* this function refuses any separator, so a name like
/// `../../x.gguf` or `C:\x.gguf` can never reach the filesystem.
///
/// Allowed: ASCII letters, digits, `.`, `_`, `-`; must end in `.gguf`; must
/// not start with `.` or `-`; at most 200 bytes; no reserved device names.
pub fn safe_model_file_name(name: &str) -> Result<&str, UnsafeFileName> {
    if name.is_empty() {
        return Err(UnsafeFileName::Empty);
    }
    if name.len() > MAX_FILE_NAME_BYTES {
        return Err(UnsafeFileName::TooLong);
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(UnsafeFileName::DisallowedCharacter);
    }
    if name.starts_with('.') || name.starts_with('-') {
        return Err(UnsafeFileName::LeadingDotOrDash);
    }
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".gguf") else {
        return Err(UnsafeFileName::NotGguf);
    };
    if stem.is_empty() || stem.ends_with('.') {
        return Err(UnsafeFileName::NotGguf);
    }
    let base = stem.split('.').next().unwrap_or(stem);
    if RESERVED_NAMES.contains(&base) {
        return Err(UnsafeFileName::ReservedDeviceName);
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // SHA-256("abc") from FIPS 180-2.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn hashes_known_vector() {
        let (digest, size) = sha256_reader(Cursor::new(b"abc"), |_| true).unwrap();
        assert_eq!(digest.to_hex(), ABC);
        assert_eq!(size, 3);
    }

    #[test]
    fn hex_round_trip_and_rejection() {
        let d = Sha256Digest::from_hex(ABC).unwrap();
        assert_eq!(d.to_hex(), ABC);
        assert_eq!(Sha256Digest::from_hex(&ABC.to_uppercase()), Some(d));
        assert!(Sha256Digest::from_hex(&ABC[..63]).is_none());
        assert!(Sha256Digest::from_hex(&format!("{ABC}0")).is_none());
        assert!(Sha256Digest::from_hex(&ABC.replace('b', "g")).is_none());
        assert!(Sha256Digest::from_hex(&"é".repeat(32)).is_none());
    }

    #[test]
    fn verification_detects_size_and_digest_mismatch() {
        let expected = Sha256Digest::from_hex(ABC).unwrap();
        assert_eq!(
            verify_reader(Cursor::new(b"abc"), &expected, 3, |_| true).unwrap(),
            Verification::Match
        );
        assert_eq!(
            verify_reader(Cursor::new(b"abc"), &expected, 4, |_| true).unwrap(),
            Verification::SizeMismatch {
                expected: 4,
                actual: 3
            }
        );
        assert_eq!(
            verify_reader(Cursor::new(b"abd"), &expected, 3, |_| true).unwrap(),
            Verification::DigestMismatch
        );
    }

    #[test]
    fn hashing_can_be_cancelled() {
        let error = sha256_reader(Cursor::new(vec![0u8; 10]), |_| false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn accepts_typical_hub_file_names() {
        for name in [
            "Qwen3-8B-Q4_K_M.gguf",
            "model-00001-of-00003.gguf",
            "mmproj-model-f16.GGUF",
            "a.gguf",
        ] {
            assert_eq!(safe_model_file_name(name), Ok(name), "{name}");
        }
    }

    #[test]
    fn rejects_hostile_file_names() {
        let cases: &[(&str, UnsafeFileName)] = &[
            ("", UnsafeFileName::Empty),
            ("../evil.gguf", UnsafeFileName::DisallowedCharacter),
            ("..\\evil.gguf", UnsafeFileName::DisallowedCharacter),
            ("dir/evil.gguf", UnsafeFileName::DisallowedCharacter),
            ("C:evil.gguf", UnsafeFileName::DisallowedCharacter),
            ("evil.gguf:stream", UnsafeFileName::DisallowedCharacter),
            ("evil .gguf", UnsafeFileName::DisallowedCharacter),
            ("evil\0.gguf", UnsafeFileName::DisallowedCharacter),
            ("modèle.gguf", UnsafeFileName::DisallowedCharacter),
            (".hidden.gguf", UnsafeFileName::LeadingDotOrDash),
            ("-rf.gguf", UnsafeFileName::LeadingDotOrDash),
            ("model.bin", UnsafeFileName::NotGguf),
            ("model.gguf.exe", UnsafeFileName::NotGguf),
            ("model.pt", UnsafeFileName::NotGguf),
            (".gguf", UnsafeFileName::LeadingDotOrDash),
            ("x..gguf", UnsafeFileName::NotGguf),
            ("CON.gguf", UnsafeFileName::ReservedDeviceName),
            ("nul.q4.gguf", UnsafeFileName::ReservedDeviceName),
        ];
        for (name, expected) in cases {
            assert_eq!(safe_model_file_name(name), Err(*expected), "{name:?}");
        }
        let long = format!("{}.gguf", "a".repeat(MAX_FILE_NAME_BYTES));
        assert_eq!(safe_model_file_name(&long), Err(UnsafeFileName::TooLong));
    }
}
