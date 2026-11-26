//! Encrypted backup files (release plan feature 3).
//!
//! A backup moves chats, sessions and settings to a new Windows install or
//! another computer, where the DPAPI-wrapped chat key can't be unwrapped. It
//! is locked with a passphrase the user chooses instead.
//!
//! File layout:
//!
//! ```text
//! "GWBACKUP" | format (u16 LE) | header length (u32 LE) | header (JSON)
//! chunk*     : ciphertext length (u32 LE) | XChaCha20-Poly1305 ciphertext
//! ```
//!
//! - The key is Argon2id(passphrase, salt) with the parameters stored in the
//!   header, so they can be raised later.
//! - The header carries a sealed empty value, so a wrong passphrase is told
//!   apart from a damaged file.
//! - The body is a stream of entries (`1 | name length u16 | name | size u64
//!   | bytes`, ended by `0`), cut into 1 MiB chunks. Each chunk's nonce is a
//!   random per-file prefix, its counter and a "last chunk" flag, and its
//!   associated data is the whole header, so chunks can't be reordered,
//!   dropped, cut off or moved to another file, and the header can't be
//!   changed, without failing authentication.
//! - Nothing about the contents is readable without the passphrase.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 8] = b"GWBACKUP";
pub const FORMAT: u16 = 1;
/// The shortest passphrase accepted for a new backup.
pub const MIN_PASSPHRASE_CHARS: usize = 12;

const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
const PREFIX_LEN: usize = 19;
const SALT_LEN: usize = 16;
const MAX_HEADER: usize = 4096;
const MAX_NAME: usize = 1024;
const VERIFY_AAD: &[u8] = b"Gardenwall backup passphrase check v1";

/// Argon2id cost settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// 256 MiB, 3 passes, 4 lanes: about a second on a desktop, and
    /// expensive to guess on GPUs.
    pub const DEFAULT: Self = Self {
        m_kib: 256 * 1024,
        t: 3,
        p: 4,
    };
    /// Fast settings for tests only.
    pub const FAST_FOR_TESTS: Self = Self {
        m_kib: 64,
        t: 1,
        p: 1,
    };

    /// Settings read from a file are bounded, so a crafted file can't ask
    /// for gigabytes of memory or minutes of work.
    fn within_limits(&self) -> bool {
        (1..=16).contains(&self.p)
            && self.m_kib >= 8 * self.p
            && self.m_kib <= 2 * 1024 * 1024
            && (1..=10).contains(&self.t)
    }
}

#[derive(Debug)]
pub enum BackupError {
    NotABackup,
    NewerFormat(u16),
    WrongPassphrase,
    Damaged(&'static str),
    WeakPassphrase,
    Io(io::Error),
    Random,
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotABackup => f.write_str("this file is not a Gardenwall backup"),
            Self::NewerFormat(v) => write!(
                f,
                "this backup was made by a newer version of Gardenwall (format {v}); update Gardenwall first"
            ),
            Self::WrongPassphrase => f.write_str("the passphrase is not right for this backup"),
            Self::Damaged(why) => write!(f, "the backup file is damaged ({why})"),
            Self::WeakPassphrase => write!(
                f,
                "the passphrase must be at least {MIN_PASSPHRASE_CHARS} characters and not one repeated character"
            ),
            Self::Io(e) => write!(f, "file error: {e}"),
            Self::Random => f.write_str("the system random number generator failed"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<io::Error> for BackupError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, BackupError>;

/// The rule a new passphrase must meet (the UI also shows a strength hint).
pub fn passphrase_ok(passphrase: &str) -> bool {
    let mut chars = passphrase.chars();
    let first = chars.next();
    passphrase.chars().count() >= MIN_PASSPHRASE_CHARS && !chars.all(|c| Some(c) == first)
}

#[derive(Serialize, Deserialize)]
struct Header {
    kdf: String,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
    nonce_prefix: String,
    verify: String,
}

fn derive(passphrase: &str, salt: &[u8], kdf: KdfParams) -> Result<XChaCha20Poly1305> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32))
        .map_err(|_| BackupError::Damaged("its key settings are invalid"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| BackupError::Damaged("its key settings are invalid"))?;
    Ok(XChaCha20Poly1305::new(&chacha20poly1305::Key::from(*key)))
}

fn nonce(prefix: &[u8; PREFIX_LEN], counter: u32, flag: u8) -> XNonce {
    let mut n = [0u8; 24];
    n[..PREFIX_LEN].copy_from_slice(prefix);
    n[PREFIX_LEN..PREFIX_LEN + 4].copy_from_slice(&counter.to_be_bytes());
    n[23] = flag;
    XNonce::from(n)
}

/// Writes a backup: entries are added in order, then [`finish`](Self::finish).
pub struct BackupWriter<W: Write> {
    out: W,
    aead: XChaCha20Poly1305,
    prefix: [u8; PREFIX_LEN],
    aad: Vec<u8>,
    buf: Vec<u8>,
    counter: u32,
}

impl<W: Write> BackupWriter<W> {
    pub fn create(mut out: W, passphrase: &str, kdf: KdfParams) -> Result<Self> {
        if !passphrase_ok(passphrase) {
            return Err(BackupError::WeakPassphrase);
        }
        let mut salt = [0u8; SALT_LEN];
        let mut prefix = [0u8; PREFIX_LEN];
        getrandom::fill(&mut salt).map_err(|_| BackupError::Random)?;
        getrandom::fill(&mut prefix).map_err(|_| BackupError::Random)?;
        let aead = derive(passphrase, &salt, kdf)?;
        let verify = aead
            .encrypt(
                &nonce(&prefix, u32::MAX, 2),
                Payload {
                    msg: &[],
                    aad: VERIFY_AAD,
                },
            )
            .map_err(|_| BackupError::Random)?;
        let header = serde_json::to_vec(&Header {
            kdf: "argon2id".into(),
            m_kib: kdf.m_kib,
            t: kdf.t,
            p: kdf.p,
            salt: B64.encode(salt),
            nonce_prefix: B64.encode(prefix),
            verify: B64.encode(verify),
        })
        .map_err(|e| BackupError::Io(io::Error::other(e)))?;
        let mut aad = Vec::with_capacity(14 + header.len());
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(&FORMAT.to_le_bytes());
        aad.extend_from_slice(&(header.len() as u32).to_le_bytes());
        aad.extend_from_slice(&header);
        out.write_all(&aad)?;
        Ok(Self {
            out,
            aead,
            prefix,
            aad,
            buf: Vec::with_capacity(CHUNK),
            counter: 0,
        })
    }

    pub fn add_bytes(&mut self, name: &str, data: &[u8]) -> Result<()> {
        self.add(name, data.len() as u64, &mut &data[..])
    }

    /// Add an entry of exactly `len` bytes read from `src`.
    pub fn add(&mut self, name: &str, len: u64, src: &mut impl Read) -> Result<()> {
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(BackupError::Io(io::Error::other("bad entry name")));
        }
        self.put(&[1])?;
        self.put(&(name.len() as u16).to_le_bytes())?;
        self.put(name.as_bytes())?;
        self.put(&len.to_le_bytes())?;
        let mut left = len;
        let mut block = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = block.len().min(left as usize);
            let got = src.read(&mut block[..want])?;
            if got == 0 {
                return Err(BackupError::Io(io::ErrorKind::UnexpectedEof.into()));
            }
            self.put(&block[..got])?;
            left -= got as u64;
        }
        Ok(())
    }

    fn put(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            let take = (CHUNK - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == CHUNK {
                self.seal(false)?;
            }
        }
        Ok(())
    }

    fn seal(&mut self, last: bool) -> Result<()> {
        let sealed = self
            .aead
            .encrypt(
                &nonce(&self.prefix, self.counter, u8::from(last)),
                Payload {
                    msg: &self.buf,
                    aad: &self.aad,
                },
            )
            .map_err(|_| BackupError::Random)?;
        self.out.write_all(&(sealed.len() as u32).to_le_bytes())?;
        self.out.write_all(&sealed)?;
        self.buf.clear();
        self.counter = self
            .counter
            .checked_add(1)
            .filter(|c| *c < u32::MAX)
            .ok_or(BackupError::Io(io::Error::other("backup too large")))?;
        Ok(())
    }

    /// Write the end marker and the last chunk.
    pub fn finish(mut self) -> Result<W> {
        self.put(&[0])?;
        self.seal(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// One entry's name and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
}

/// Reads a backup. Every byte handed out has passed authentication; the
/// end of the file is checked when the end marker is reached.
pub struct BackupReader<R: Read> {
    input: R,
    aead: XChaCha20Poly1305,
    prefix: [u8; PREFIX_LEN],
    aad: Vec<u8>,
    buf: Vec<u8>,
    pos: usize,
    counter: u32,
    last_seen: bool,
    left_in_entry: u64,
    done: bool,
}

fn read_header(input: &mut impl Read) -> Result<(Vec<u8>, Header)> {
    let mut fixed = [0u8; 14];
    match input.read_exact(&mut fixed) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(BackupError::NotABackup),
        Err(e) => return Err(e.into()),
    }
    if &fixed[..8] != MAGIC {
        return Err(BackupError::NotABackup);
    }
    let format = u16::from_le_bytes([fixed[8], fixed[9]]);
    if format == 0 {
        return Err(BackupError::NotABackup);
    }
    if format > FORMAT {
        return Err(BackupError::NewerFormat(format));
    }
    let len = u32::from_le_bytes([fixed[10], fixed[11], fixed[12], fixed[13]]) as usize;
    if len > MAX_HEADER {
        return Err(BackupError::Damaged("its header is too long"));
    }
    let mut json = vec![0u8; len];
    input
        .read_exact(&mut json)
        .map_err(|_| BackupError::Damaged("the file ends early"))?;
    let header: Header = serde_json::from_slice(&json)
        .map_err(|_| BackupError::Damaged("its header is unreadable"))?;
    let mut aad = fixed.to_vec();
    aad.extend_from_slice(&json);
    Ok((aad, header))
}

/// Check only that `input` starts like a Gardenwall backup this version can
/// read (no passphrase needed).
pub fn check_header(mut input: impl Read) -> Result<()> {
    read_header(&mut input).map(|_| ())
}

impl<R: Read> BackupReader<R> {
    pub fn open(mut input: R, passphrase: &str) -> Result<Self> {
        let (aad, header) = read_header(&mut input)?;
        let kdf = KdfParams {
            m_kib: header.m_kib,
            t: header.t,
            p: header.p,
        };
        if header.kdf != "argon2id" || !kdf.within_limits() {
            return Err(BackupError::Damaged("its key settings are invalid"));
        }
        let bad = |_| BackupError::Damaged("its header is unreadable");
        let salt = B64.decode(&header.salt).map_err(bad)?;
        let prefix: [u8; PREFIX_LEN] = B64
            .decode(&header.nonce_prefix)
            .map_err(bad)?
            .try_into()
            .map_err(|_| BackupError::Damaged("its header is unreadable"))?;
        let verify = B64.decode(&header.verify).map_err(bad)?;
        if salt.len() != SALT_LEN {
            return Err(BackupError::Damaged("its header is unreadable"));
        }
        let aead = derive(passphrase, &salt, kdf)?;
        aead.decrypt(
            &nonce(&prefix, u32::MAX, 2),
            Payload {
                msg: &verify,
                aad: VERIFY_AAD,
            },
        )
        .map_err(|_| BackupError::WrongPassphrase)?;
        Ok(Self {
            input,
            aead,
            prefix,
            aad,
            buf: Vec::new(),
            pos: 0,
            counter: 0,
            last_seen: false,
            left_in_entry: 0,
            done: false,
        })
    }

    /// Make plaintext available; `false` when the stream has ended.
    fn fill(&mut self) -> Result<bool> {
        if self.pos < self.buf.len() {
            return Ok(true);
        }
        if self.last_seen {
            return Ok(false);
        }
        let mut len = [0u8; 4];
        self.input
            .read_exact(&mut len)
            .map_err(|_| BackupError::Damaged("the file ends early"))?;
        let len = u32::from_le_bytes(len) as usize;
        if !(TAG..=CHUNK + TAG).contains(&len) {
            return Err(BackupError::Damaged("a part has an impossible size"));
        }
        let mut sealed = vec![0u8; len];
        self.input
            .read_exact(&mut sealed)
            .map_err(|_| BackupError::Damaged("the file ends early"))?;
        let open = |flag: u8| {
            self.aead.decrypt(
                &nonce(&self.prefix, self.counter, flag),
                Payload {
                    msg: &sealed,
                    aad: &self.aad,
                },
            )
        };
        let (plain, last) = match open(0) {
            Ok(p) => (p, false),
            Err(_) => (
                open(1).map_err(|_| BackupError::Damaged("it failed its integrity check"))?,
                true,
            ),
        };
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or(BackupError::Damaged("it has too many parts"))?;
        self.last_seen = last;
        self.buf = plain;
        self.pos = 0;
        if last {
            let mut extra = [0u8; 1];
            if self.input.read(&mut extra)? != 0 {
                return Err(BackupError::Damaged("it has extra data at the end"));
            }
        }
        Ok(self.pos < self.buf.len())
    }

    fn take(&mut self, out: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while filled < out.len() {
            if !self.fill()? {
                return Err(BackupError::Damaged("the file ends early"));
            }
            let n = (self.buf.len() - self.pos).min(out.len() - filled);
            out[filled..filled + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            filled += n;
        }
        Ok(())
    }

    /// The next entry, skipping what's left of the current one; `None` at
    /// the end, after checking that nothing follows the end marker.
    pub fn next_entry(&mut self) -> Result<Option<Entry>> {
        if self.done {
            return Ok(None);
        }
        if self.left_in_entry > 0 {
            self.copy_entry(&mut io::sink())?;
        }
        let mut kind = [0u8; 1];
        self.take(&mut kind)?;
        match kind[0] {
            0 => {
                if self.pos != self.buf.len() || !self.last_seen {
                    return Err(BackupError::Damaged("it has data after its end"));
                }
                self.done = true;
                Ok(None)
            }
            1 => {
                let mut n = [0u8; 2];
                self.take(&mut n)?;
                let n = u16::from_le_bytes(n) as usize;
                if n == 0 || n > MAX_NAME {
                    return Err(BackupError::Damaged("an entry has a bad name"));
                }
                let mut name = vec![0u8; n];
                self.take(&mut name)?;
                let name = String::from_utf8(name)
                    .map_err(|_| BackupError::Damaged("an entry has a bad name"))?;
                let mut size = [0u8; 8];
                self.take(&mut size)?;
                let size = u64::from_le_bytes(size);
                self.left_in_entry = size;
                Ok(Some(Entry { name, size }))
            }
            _ => Err(BackupError::Damaged("an entry is malformed")),
        }
    }

    /// Copy the rest of the current entry to `w`.
    pub fn copy_entry(&mut self, w: &mut impl Write) -> Result<u64> {
        let mut copied = 0;
        while self.left_in_entry > 0 {
            if !self.fill()? {
                return Err(BackupError::Damaged("the file ends early"));
            }
            let n = ((self.buf.len() - self.pos) as u64).min(self.left_in_entry) as usize;
            w.write_all(&self.buf[self.pos..self.pos + n])?;
            self.pos += n;
            self.left_in_entry -= n as u64;
            copied += n as u64;
        }
        Ok(copied)
    }

    /// The current entry in memory, refused if it is larger than `max`.
    pub fn read_entry(&mut self, max: u64) -> Result<Zeroizing<Vec<u8>>> {
        if self.left_in_entry > max {
            return Err(BackupError::Damaged("an entry is larger than expected"));
        }
        let mut out = Zeroizing::new(Vec::with_capacity(self.left_in_entry as usize));
        self.copy_entry(&mut *out)?;
        Ok(out)
    }
}

/// Read a whole backup, checking every part; returns its entries.
pub fn verify(input: impl Read, passphrase: &str) -> Result<Vec<Entry>> {
    let mut reader = BackupReader::open(input, passphrase)?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry()? {
        reader.copy_entry(&mut io::sink())?;
        entries.push(entry);
    }
    Ok(entries)
}

/// What a conversations database holds, for the summary shown before a
/// restore (counts and dates only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataCounts {
    pub chats: u64,
    pub agent_sessions: u64,
    pub projects: u64,
    pub memories: u64,
    pub document_collections: u64,
    /// The newest change to a chat or session (ms since the epoch).
    pub newest_ms: Option<i64>,
}

/// Count what the database at `path` holds (tables it lacks count as 0).
pub fn count_data(path: &Path) -> Result<DataCounts> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| BackupError::Io(io::Error::other(e)))?;
    let count = |table: &str| -> u64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
    };
    let newest = |table: &str| -> Option<i64> {
        conn.query_row(&format!("SELECT MAX(updated_at) FROM {table}"), [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .ok()
        .flatten()
    };
    Ok(DataCounts {
        chats: count("conversations"),
        agent_sessions: count("agent_transcripts"),
        projects: count("agent_projects"),
        memories: count("memories"),
        document_collections: count("doc_collections"),
        // Chats and sessions store `updated_at` in seconds.
        newest_ms: newest("conversations")
            .max(newest("agent_transcripts"))
            .map(|s| s.saturating_mul(1000)),
    })
}

/// Remove every memory from a database copy made for a backup (the user
/// chose to leave them out), leaving no trace of them in the file.
pub fn remove_memories(path: &Path) -> Result<()> {
    let conn =
        rusqlite::Connection::open(path).map_err(|e| BackupError::Io(io::Error::other(e)))?;
    conn.execute_batch("PRAGMA secure_delete = ON; DELETE FROM memories; VACUUM;")
        .map_err(|e| BackupError::Io(io::Error::other(e)))
}

/// Number of activity-log entries in an approvals database.
pub fn count_activity(path: &Path) -> u64 {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get::<_, i64>(0)))
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
}

/// A consistent single-file copy of an open database (`VACUUM INTO`), taken
/// without closing it. `dest` must not exist.
pub(crate) fn copy_database(conn: &rusqlite::Connection, dest: &Path) -> Result<()> {
    let dest = dest
        .to_str()
        .ok_or_else(|| BackupError::Io(io::Error::other("the copy's path is not text")))?;
    conn.execute("VACUUM INTO ?1", [dest])
        .map_err(|e| BackupError::Io(io::Error::other(e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "correct horse battery";

    fn sample(passphrase: &str) -> Vec<u8> {
        let mut w =
            BackupWriter::create(Vec::new(), passphrase, KdfParams::FAST_FOR_TESTS).unwrap();
        w.add_bytes("manifest.json", b"{\"v\":1}").unwrap();
        // Larger than one chunk, so it spans parts.
        let big: Vec<u8> = (0..(CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
        w.add("conversations.db", big.len() as u64, &mut &big[..])
            .unwrap();
        w.add_bytes("settings.json", b"{}").unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn round_trip_across_chunks() {
        let file = sample(PASS);
        assert!(
            !file.windows(13).any(|w| w == b"manifest.json"),
            "names are hidden"
        );
        let mut r = BackupReader::open(&file[..], PASS).unwrap();
        let e = r.next_entry().unwrap().unwrap();
        assert_eq!(e.name, "manifest.json");
        assert_eq!(&*r.read_entry(100).unwrap(), b"{\"v\":1}");
        let e = r.next_entry().unwrap().unwrap();
        assert_eq!(e.size, (CHUNK * 2 + 123) as u64);
        let mut db = Vec::new();
        r.copy_entry(&mut db).unwrap();
        assert!(db.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
        assert_eq!(r.next_entry().unwrap().unwrap().name, "settings.json");
        // Skipped without reading, then the end is checked.
        assert!(r.next_entry().unwrap().is_none());
        assert_eq!(verify(&file[..], PASS).unwrap().len(), 3);
    }

    #[test]
    fn a_wrong_passphrase_is_told_apart_from_damage() {
        let file = sample(PASS);
        assert!(matches!(
            BackupReader::open(&file[..], "another passphrase"),
            Err(BackupError::WrongPassphrase)
        ));
        // A flipped byte in the body.
        let mut bad = file.clone();
        let at = bad.len() - 100;
        bad[at] ^= 1;
        assert!(matches!(
            verify(&bad[..], PASS),
            Err(BackupError::Damaged(_))
        ));
        // A changed header (the cost settings) fails too.
        let text = String::from_utf8_lossy(&file[14..200]).to_string();
        assert!(text.contains("\"t\":1"));
        let mut changed = file.clone();
        let pos = changed.windows(5).position(|w| w == b"\"t\":1").unwrap();
        changed[pos + 4] = b'2';
        assert!(BackupReader::open(&changed[..], PASS).is_err());
    }

    #[test]
    fn a_cut_or_extended_file_is_damaged() {
        let file = sample(PASS);
        // Cut at the end of a whole chunk: no last chunk.
        let first_chunk_end = {
            let header_len = u32::from_le_bytes(file[10..14].try_into().unwrap()) as usize;
            let start = 14 + header_len;
            let len = u32::from_le_bytes(file[start..start + 4].try_into().unwrap()) as usize;
            start + 4 + len
        };
        assert!(matches!(
            verify(&file[..first_chunk_end], PASS),
            Err(BackupError::Damaged(_))
        ));
        assert!(matches!(
            verify(&file[..file.len() - 1], PASS),
            Err(BackupError::Damaged(_))
        ));
        let mut longer = file.clone();
        longer.push(0);
        assert!(matches!(
            verify(&longer[..], PASS),
            Err(BackupError::Damaged(_))
        ));
    }

    #[test]
    fn headers_are_checked_before_any_work() {
        assert!(matches!(
            check_header(&b"PK\x03\x04 zip"[..]),
            Err(BackupError::NotABackup)
        ));
        assert!(matches!(
            check_header(&b""[..]),
            Err(BackupError::NotABackup)
        ));
        let mut newer = sample(PASS);
        newer[8] = 9;
        assert!(matches!(
            check_header(&newer[..]),
            Err(BackupError::NewerFormat(9))
        ));
        assert!(check_header(&sample(PASS)[..]).is_ok());
        // Cost settings beyond the limits are refused, not run.
        assert!(
            !KdfParams {
                m_kib: 64 * 1024 * 1024,
                t: 3,
                p: 4
            }
            .within_limits()
        );
        assert!(KdfParams::DEFAULT.within_limits());
    }

    /// How long the real key settings take (run with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn time_the_default_key_settings() {
        let start = std::time::Instant::now();
        derive(PASS, &[0u8; SALT_LEN], KdfParams::DEFAULT).unwrap();
        println!("Argon2id default: {:?}", start.elapsed());
    }

    #[test]
    fn weak_passphrases_are_refused() {
        assert!(!passphrase_ok("short"));
        assert!(!passphrase_ok("aaaaaaaaaaaaaaaa"));
        assert!(passphrase_ok("twelve chars"));
        assert!(matches!(
            BackupWriter::create(Vec::new(), "short", KdfParams::FAST_FOR_TESTS),
            Err(BackupError::WeakPassphrase)
        ));
    }

    #[test]
    fn databases_are_copied_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let store = crate::conversations::ConversationStore::open(&path).unwrap();
        store.create("").unwrap();
        let copy = dir.path().join("copy.db");
        store.copy_to(&copy).unwrap();
        let counts = count_data(&copy).unwrap();
        assert_eq!(counts.chats, 1);
        assert_eq!(counts.agent_sessions, 0);
        // In milliseconds, close to now (not a date in 1970).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let newest = counts.newest_ms.unwrap();
        assert!(
            (now_ms - newest).abs() < 60_000,
            "newest_ms {newest} vs now {now_ms}"
        );
        assert!(store.copy_to(&copy).is_err(), "never overwrites");

        // Memories left out leave nothing behind in the copy.
        store
            .add_memory(
                &crate::conversations::MemoryScope::Personal,
                "prefers tabs over spaces",
                crate::conversations::MemorySource::User,
            )
            .unwrap();
        let with = dir.path().join("with.db");
        store.copy_to(&with).unwrap();
        assert_eq!(count_data(&with).unwrap().memories, 1);
        remove_memories(&with).unwrap();
        assert_eq!(count_data(&with).unwrap().memories, 0);
        let bytes = std::fs::read(&with).unwrap();
        assert!(!bytes.windows(10).any(|w| w == b"tabs over "));
    }
}
