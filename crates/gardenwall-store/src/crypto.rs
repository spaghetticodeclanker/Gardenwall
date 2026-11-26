//! Field-level encryption for stored chats.
//!
//! Each sensitive text field is sealed separately with XChaCha20-Poly1305
//! under a 256-bit key that the caller obtains from the OS (DPAPI on
//! Windows). The associated data binds a ciphertext to its conversation and
//! field, so a sealed value cannot be moved to another conversation or
//! column without failing authentication.
//!
//! Stored form: `gw1:` followed by base64 of `nonce (24 bytes) || ciphertext`.
//! Values without the prefix are legacy plaintext, read as-is and encrypted
//! by the store's migration.
//!
//! This protects copies of the database (backups, a stolen disk, other
//! accounts). It does not protect against software running as the same
//! user, which can ask the OS to unwrap the key just as Gardenwall does.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

pub const PREFIX: &str = "gw1:";
const NONCE_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    Random,
    /// The value is damaged, was sealed under another key, or was moved.
    Unauthentic,
    Malformed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Random => "the system random number generator failed",
            Self::Unauthentic => "stored text failed its integrity check",
            Self::Malformed => "stored text is not in the expected format",
        })
    }
}

impl std::error::Error for CryptoError {}

/// Seals and opens text fields under one key.
pub struct FieldCipher {
    aead: XChaCha20Poly1305,
}

impl fmt::Debug for FieldCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FieldCipher(<key hidden>)")
    }
}

impl FieldCipher {
    pub fn new(key: &Zeroizing<[u8; 32]>) -> Self {
        Self {
            aead: XChaCha20Poly1305::new(&chacha20poly1305::Key::from(**key)),
        }
    }

    pub fn is_sealed(value: &str) -> bool {
        value.starts_with(PREFIX)
    }

    pub fn seal(&self, aad: &str, plaintext: &str) -> Result<String, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
        let sealed = self
            .aead
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Malformed)?;
        let mut raw = Vec::with_capacity(NONCE_LEN + sealed.len());
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&sealed);
        Ok(format!("{PREFIX}{}", B64.encode(raw)))
    }

    pub fn open(&self, aad: &str, value: &str) -> Result<String, CryptoError> {
        let encoded = value.strip_prefix(PREFIX).ok_or(CryptoError::Malformed)?;
        let raw = B64.decode(encoded).map_err(|_| CryptoError::Malformed)?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(CryptoError::Malformed);
        }
        let (nonce, sealed) = raw.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Malformed)?;
        let plain = self
            .aead
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Unauthentic)?;
        String::from_utf8(plain).map_err(|_| CryptoError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher(byte: u8) -> FieldCipher {
        FieldCipher::new(&Zeroizing::new([byte; 32]))
    }

    #[test]
    fn round_trip_with_fresh_nonces() {
        let c = cipher(1);
        let a = c.seal("conv:content", "hello").unwrap();
        let b = c.seal("conv:content", "hello").unwrap();
        assert!(FieldCipher::is_sealed(&a));
        assert_ne!(a, b, "nonces must differ");
        assert!(!a.contains("hello"));
        assert_eq!(c.open("conv:content", &a).unwrap(), "hello");
        assert_eq!(
            c.open("conv:content", &c.seal("conv:content", "").unwrap())
                .unwrap(),
            ""
        );
    }

    #[test]
    fn wrong_key_place_or_tampering_fails() {
        let c = cipher(1);
        let sealed = c.seal("a:content", "secret").unwrap();
        assert_eq!(
            cipher(2).open("a:content", &sealed),
            Err(CryptoError::Unauthentic)
        );
        assert_eq!(c.open("b:content", &sealed), Err(CryptoError::Unauthentic));
        assert_eq!(
            c.open("a:reasoning", &sealed),
            Err(CryptoError::Unauthentic)
        );
        let mut bytes = sealed.into_bytes();
        let last = bytes.len() - 3;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(c.open("a:content", &tampered).is_err());
        assert_eq!(c.open("a:content", "gw1:%%%"), Err(CryptoError::Malformed));
        assert_eq!(c.open("a:content", "plain"), Err(CryptoError::Malformed));
    }
}
