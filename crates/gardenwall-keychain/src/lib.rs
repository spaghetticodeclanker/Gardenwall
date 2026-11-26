//! OS-protected keys for Gardenwall.
//!
//! The chat encryption key is 32 random bytes, stored only in wrapped form:
//! on Windows it is wrapped with DPAPI (`CryptProtectData`, current-user
//! scope), so only this Windows account on this machine can unwrap it. A
//! copied disk, a backup, or another account sees only the wrapped bytes.
//!
//! An existing key file that cannot be unwrapped is never replaced: doing
//! so would make every saved chat permanently unreadable. The caller gets
//! [`KeyError::Unavailable`] and must keep the chats read-only.
//!
//! macOS (Keychain) and Linux (Secret Service) backends follow ADR-005; until
//! then those platforms report [`KeyError::Unsupported`].
//!
//! All `unsafe` code lives in the Windows module.

#![deny(unsafe_code)]

use std::fmt;
use std::path::Path;

use zeroize::Zeroizing;

#[cfg(windows)]
mod dpapi;

pub const KEY_LEN: usize = 32;

#[derive(Debug)]
pub enum KeyError {
    /// No OS key protection on this platform yet.
    Unsupported,
    /// A key exists but this account cannot unwrap it (e.g. the Windows
    /// profile was reset, or the file came from another machine).
    Unavailable(String),
    Io(std::io::Error),
    Random,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => {
                f.write_str("OS key protection is not available on this platform yet")
            }
            Self::Unavailable(why) => write!(
                f,
                "the saved key cannot be unwrapped by this account ({why})"
            ),
            Self::Io(e) => write!(f, "key file error: {e}"),
            Self::Random => f.write_str("the system random number generator failed"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Context mixed into the wrapping so the blob is useless for any other
/// purpose.
#[cfg_attr(not(windows), allow(dead_code))]
const ENTROPY: &[u8] = b"Gardenwall chat key v1";

/// Load the key wrapped in `path`, creating it on first use.
pub fn load_or_create_key(path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, KeyError> {
    #[cfg(windows)]
    {
        match std::fs::read(path) {
            Ok(wrapped) => {
                let plain = dpapi::unprotect(&wrapped, ENTROPY).map_err(KeyError::Unavailable)?;
                let key: [u8; KEY_LEN] = plain
                    .as_slice()
                    .try_into()
                    .map_err(|_| KeyError::Unavailable("unexpected key length".into()))?;
                Ok(Zeroizing::new(key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => create(path),
            Err(e) => Err(KeyError::Io(e)),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(KeyError::Unsupported)
    }
}

/// The key wrapped in `path`, without creating one: `Ok(None)` if there is
/// no key file (for a backup, which must not invent a key).
pub fn load_key(path: &Path) -> Result<Option<Zeroizing<[u8; KEY_LEN]>>, KeyError> {
    match std::fs::metadata(path) {
        Ok(_) => load_or_create_key(path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(KeyError::Io(e)),
    }
}

/// Wrap `key` (restored from a backup) for this account and write it to
/// `path`, which must not exist yet: the caller sets any earlier key aside
/// first, so a key is never overwritten.
pub fn store_key(path: &Path, key: &[u8; KEY_LEN]) -> Result<(), KeyError> {
    #[cfg(windows)]
    {
        use std::io::Write;
        let wrapped = dpapi::protect(key, ENTROPY).map_err(KeyError::Unavailable)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(KeyError::Io)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(KeyError::Io)?;
        file.write_all(&wrapped)
            .and_then(|()| file.sync_all())
            .map_err(KeyError::Io)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, key);
        Err(KeyError::Unsupported)
    }
}

/// Context for wrapped API keys (`purpose` is added, so a key saved for one
/// service can't be unwrapped as another's).
#[cfg_attr(not(windows), allow(dead_code))]
const SECRET_ENTROPY: &str = "Gardenwall API key v1:";

/// Save `secret` (an API key) wrapped for this account in `path`, replacing
/// any earlier one. The file is written whole, then renamed into place.
pub fn save_secret(path: &Path, purpose: &str, secret: &str) -> Result<(), KeyError> {
    #[cfg(windows)]
    {
        let entropy = format!("{SECRET_ENTROPY}{purpose}");
        let wrapped =
            dpapi::protect(secret.as_bytes(), entropy.as_bytes()).map_err(KeyError::Unavailable)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(KeyError::Io)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &wrapped)
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(KeyError::Io)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, purpose, secret);
        Err(KeyError::Unsupported)
    }
}

/// The secret saved in `path` for `purpose`, `Ok(None)` if there is none.
pub fn load_secret(path: &Path, purpose: &str) -> Result<Option<Zeroizing<String>>, KeyError> {
    #[cfg(windows)]
    {
        let wrapped = match std::fs::read(path) {
            Ok(w) => w,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(KeyError::Io(e)),
        };
        let entropy = format!("{SECRET_ENTROPY}{purpose}");
        let plain =
            dpapi::unprotect(&wrapped, entropy.as_bytes()).map_err(KeyError::Unavailable)?;
        let text = std::str::from_utf8(&plain)
            .map_err(|_| KeyError::Unavailable("the saved key is not text".into()))?;
        Ok(Some(Zeroizing::new(text.to_string())))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, purpose);
        Ok(None)
    }
}

/// Remove a saved secret (nothing to do if there is none).
pub fn delete_secret(path: &Path) -> Result<(), KeyError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(KeyError::Io(e)),
    }
}

#[cfg(windows)]
fn create(path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, KeyError> {
    use std::io::Write;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    getrandom::fill(key.as_mut()).map_err(|_| KeyError::Random)?;
    let wrapped = dpapi::protect(key.as_ref(), ENTROPY).map_err(KeyError::Unavailable)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(KeyError::Io)?;
    }
    // `create_new`: if another instance created the key meanwhile, use
    // theirs instead of overwriting it.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(&wrapped)
                .and_then(|()| file.sync_all())
                .map_err(KeyError::Io)?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_or_create_key(path),
        Err(e) => Err(KeyError::Io(e)),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn key_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.key");
        let first = load_or_create_key(&path).unwrap();
        let wrapped = std::fs::read(&path).unwrap();
        assert!(wrapped.len() > KEY_LEN, "stored form must be wrapped");
        assert!(
            !wrapped.windows(KEY_LEN).any(|w| w == first.as_ref()),
            "raw key on disk"
        );
        let again = load_or_create_key(&path).unwrap();
        assert_eq!(first.as_ref(), again.as_ref());
    }

    #[test]
    fn a_restored_key_is_wrapped_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.key");
        assert!(load_key(&path).unwrap().is_none());
        assert!(!path.exists(), "load_key never creates a key");
        store_key(&path, &[7u8; KEY_LEN]).unwrap();
        assert_eq!(load_key(&path).unwrap().unwrap().as_ref(), &[7u8; KEY_LEN]);
        assert!(store_key(&path, &[8u8; KEY_LEN]).is_err());
        assert_eq!(load_key(&path).unwrap().unwrap().as_ref(), &[7u8; KEY_LEN]);
    }

    #[test]
    fn a_damaged_key_is_reported_and_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.key");
        std::fs::write(&path, b"not a dpapi blob").unwrap();
        assert!(matches!(
            load_or_create_key(&path),
            Err(KeyError::Unavailable(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"not a dpapi blob");
    }

    #[test]
    fn secrets_are_wrapped_per_purpose_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brave.key");
        assert!(load_secret(&path, "brave").unwrap().is_none());
        save_secret(&path, "brave", "BSA-test-key-123").unwrap();
        let wrapped = std::fs::read(&path).unwrap();
        assert!(
            !wrapped.windows(8).any(|w| w == b"test-key"),
            "raw key on disk"
        );
        assert_eq!(
            load_secret(&path, "brave").unwrap().unwrap().as_str(),
            "BSA-test-key-123"
        );
        // Another purpose can't unwrap it.
        assert!(load_secret(&path, "other").is_err());
        delete_secret(&path).unwrap();
        assert!(load_secret(&path, "brave").unwrap().is_none());
    }
}
