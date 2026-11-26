//! Chat conversation storage (ADR-015).
//!
//! One SQLite database, separate from the authorization store. Conversations
//! are private user data: deleted rows are overwritten (`secure_delete`) and
//! the write-ahead log is checkpointed and truncated after deletions, so
//! removed text does not linger in either file. Nothing here is sent
//! anywhere.
//!
//! All text stored here may contain untrusted model output; it is data only
//! and is never interpreted.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::FieldCipher;

/// Associated data binding a sealed value to its conversation and field.
fn aad(conversation: &str, field: &str) -> String {
    format!("gardenwall-chat:{conversation}:{field}")
}

/// Longest title kept (characters).
pub const MAX_TITLE_CHARS: usize = 200;
/// Largest system prompt accepted (bytes).
pub const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;
/// Largest single message accepted (bytes), content and reasoning each.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Title given to a conversation until its first message names it.
pub const DEFAULT_TITLE: &str = "New chat";

// A child module (it uses the store internals); kept beside this file
// because `conversations/` folders are git-ignored as user data.
#[path = "documents.rs"]
mod documents;
#[path = "drafts.rs"]
mod drafts;
#[path = "memories.rs"]
mod memories;
#[path = "projects.rs"]
mod projects;
#[path = "search.rs"]
mod search;
#[path = "transcripts.rs"]
mod transcripts;
#[path = "visits.rs"]
mod visits;
pub use documents::{CollectionSummary, FileSource, FileSummary, SourceRecord, StoredChunk};
pub use memories::{MAX_MEMORY_CHARS, Memory, MemoryScope, MemorySource};
pub use projects::{MAX_PROJECT_FOLDERS, ProjectSummary};
pub use search::SearchHit;
pub use transcripts::{
    DEFAULT_AGENT_TITLE, EntryKind, HandoffParent, MAX_ENTRIES, MAX_ENTRY_BYTES, Transcript,
    TranscriptEntry, TranscriptSummary,
};
pub use visits::{MAX_VISITS_BYTES, VisitOwner};

/// 1: chats. 2: agent transcripts. 3: document collections. 4: images.
/// 5: memories. 6: saved document sources. 7: agent images. 8: forks.
/// 9: agent projects. 10: session handoffs. 11: visited-sites lists.
const SCHEMA_VERSION: i64 = 12;

/// Chats (v1).
const CHAT_TABLES: &str = "CREATE TABLE conversations (
         id TEXT PRIMARY KEY NOT NULL,
         title TEXT NOT NULL,
         system_prompt TEXT NOT NULL DEFAULT '',
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE messages (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         conversation_id TEXT NOT NULL
             REFERENCES conversations(id) ON DELETE CASCADE,
         role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
         content TEXT NOT NULL,
         reasoning TEXT NOT NULL DEFAULT '',
         stats TEXT,
         created_at INTEGER NOT NULL
     ) STRICT;
     CREATE INDEX messages_by_conversation
         ON messages(conversation_id, id);
     CREATE INDEX conversations_by_update
         ON conversations(updated_at DESC);";

/// Session handoffs (v10): a session started
/// from a reviewed handoff links to the session it continues. An agent
/// session's agent parent is `parent_id` (as for forks) with `handoff = 1`;
/// a chat parent is `parent_chat_id`. A chat continued in a new chat links
/// with `conversations.parent_id`. The handoff text itself is the new
/// session's first message, sealed like every message.
const HANDOFF_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "agent_transcripts",
        "handoff",
        "INTEGER NOT NULL DEFAULT 0 CHECK (handoff IN (0, 1))",
    ),
    (
        "agent_transcripts",
        "parent_chat_id",
        "TEXT REFERENCES conversations(id) ON DELETE SET NULL",
    ),
    (
        "conversations",
        "parent_id",
        "TEXT REFERENCES conversations(id) ON DELETE SET NULL",
    ),
];

/// Version 9 to 10: add the handoff columns that are missing (a column
/// may already exist in a database rebuilt from parts of a newer one).
fn add_handoff_columns(conn: &Connection) -> rusqlite::Result<()> {
    let mut sql = String::from("BEGIN IMMEDIATE;");
    for (table, column, decl) in HANDOFF_COLUMNS {
        let exists: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"),
            [column],
            |r| r.get(0),
        )?;
        if !exists {
            sql.push_str(&format!(" ALTER TABLE {table} ADD COLUMN {column} {decl};"));
        }
    }
    sql.push_str(" PRAGMA user_version = 10; COMMIT;");
    conn.execute_batch(&sql)
}

/// Agent projects (v9): sealed name and folders; sessions link to one.
const PROJECT_TABLES: &str = "CREATE TABLE agent_projects (
         id TEXT PRIMARY KEY NOT NULL,
         name TEXT NOT NULL,
         folders TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;
     ALTER TABLE agent_transcripts ADD COLUMN project_id TEXT
         REFERENCES agent_projects(id) ON DELETE SET NULL;
     CREATE INDEX agent_transcripts_by_project ON agent_transcripts(project_id);";

/// Where a forked or cloned agent session came from (v8).
const FORK_COLUMNS: &str = "ALTER TABLE agent_transcripts ADD COLUMN parent_id TEXT
         REFERENCES agent_transcripts(id) ON DELETE SET NULL;
     ALTER TABLE agent_transcripts ADD COLUMN fork_seq INTEGER;";

/// Images attached to agent requests (v7), sealed and bound to their
/// transcript; `seq` is the user message's entry.
const AGENT_IMAGE_TABLES: &str = "CREATE TABLE agent_images (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         transcript_id TEXT NOT NULL REFERENCES agent_transcripts(id) ON DELETE CASCADE,
         seq INTEGER NOT NULL CHECK (seq >= 0),
         mime TEXT NOT NULL CHECK (mime IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
         data TEXT NOT NULL
     ) STRICT;
     CREATE INDEX agent_images_by_entry ON agent_images(transcript_id, seq, id);";

/// Where each document came from (v6): the path (sealed), its modified
/// time and its SHA-256 (sealed), so changed files can be found and read
/// again. `NULL` for files added before v6.
const DOC_SOURCE_COLUMNS: &str = "ALTER TABLE doc_files ADD COLUMN source TEXT;
     ALTER TABLE doc_files ADD COLUMN source_mtime INTEGER;
     ALTER TABLE doc_files ADD COLUMN source_hash TEXT;";

/// Memories (v5) and per-chat options.
const MEMORY_TABLES: &str = "CREATE TABLE memories (
         id TEXT PRIMARY KEY NOT NULL,
         scope TEXT NOT NULL CHECK (scope IN ('personal', 'project')),
         project TEXT NOT NULL DEFAULT '',
         text TEXT NOT NULL,
         status TEXT NOT NULL CHECK (status IN ('active', 'pending')),
         source TEXT NOT NULL CHECK (source IN ('user', 'model')),
         in_chat INTEGER NOT NULL DEFAULT 1,
         in_agent INTEGER NOT NULL DEFAULT 1,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE chat_options (
         conversation_id TEXT PRIMARY KEY NOT NULL
             REFERENCES conversations(id) ON DELETE CASCADE,
         use_memories INTEGER NOT NULL DEFAULT 1
     ) STRICT;";

const IMAGE_TABLES: &str = "CREATE TABLE message_images (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
         conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
         mime TEXT NOT NULL CHECK (mime IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
         data TEXT NOT NULL
     ) STRICT;
     CREATE INDEX message_images_by_message ON message_images(message_id, id);";

/// Largest image kept with a message (bytes).
pub const MAX_IMAGE_BYTES: usize = 10 << 20;
/// Most images per message.
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;

const DOC_TABLES: &str = "CREATE TABLE doc_collections (
         id TEXT PRIMARY KEY NOT NULL,
         name TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE doc_files (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         collection_id TEXT NOT NULL REFERENCES doc_collections(id) ON DELETE CASCADE,
         name TEXT NOT NULL,
         size INTEGER NOT NULL,
         chunks INTEGER NOT NULL,
         added_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE doc_chunks (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         file_id INTEGER NOT NULL REFERENCES doc_files(id) ON DELETE CASCADE,
         collection_id TEXT NOT NULL REFERENCES doc_collections(id) ON DELETE CASCADE,
         seq INTEGER NOT NULL,
         text TEXT NOT NULL,
         embedding TEXT,
         embed_model TEXT
     ) STRICT;
     CREATE INDEX doc_chunks_by_collection ON doc_chunks(collection_id, id);
     CREATE TABLE doc_links (
         conversation_id TEXT PRIMARY KEY NOT NULL
             REFERENCES conversations(id) ON DELETE CASCADE,
         collection_id TEXT NOT NULL REFERENCES doc_collections(id) ON DELETE CASCADE
     ) STRICT;";

const AGENT_TABLES: &str = "CREATE TABLE agent_transcripts (
         id TEXT PRIMARY KEY NOT NULL,
         title TEXT NOT NULL,
         folders TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE agent_entries (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         transcript_id TEXT NOT NULL
             REFERENCES agent_transcripts(id) ON DELETE CASCADE,
         seq INTEGER NOT NULL CHECK (seq >= 0),
         kind TEXT NOT NULL CHECK (kind IN ('message', 'compaction')),
         payload TEXT NOT NULL,
         created_at INTEGER NOT NULL
     ) STRICT;
     CREATE INDEX agent_entries_by_transcript ON agent_entries(transcript_id, id);
     CREATE INDEX agent_transcripts_by_update ON agent_transcripts(updated_at DESC);";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    NotFound,
    InvalidId,
    TooLarge(&'static str),
    Empty(&'static str),
    UnsupportedSchema(i64),
    /// Chats are encrypted but the key is unavailable, so nothing can be
    /// written (writing plaintext beside encrypted chats would downgrade them).
    Locked,
    Crypto(crate::crypto::CryptoError),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "conversation storage error: {e}"),
            Self::NotFound => f.write_str("conversation not found"),
            Self::InvalidId => f.write_str("invalid conversation id"),
            Self::TooLarge(what) => write!(f, "{what} is too large"),
            Self::Empty(what) => write!(f, "{what} is empty"),
            Self::UnsupportedSchema(v) => write!(
                f,
                "conversation database version {v} is newer than this version of Gardenwall"
            ),
            Self::Locked => f.write_str(
                "saved chats are encrypted and the key is unavailable on this account, so changes are disabled"
            ),
            Self::Crypto(e) => write!(f, "chat encryption error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// Generation statistics kept with an assistant message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageStats {
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub prompt_per_second: Option<f64>,
    pub generation_per_second: Option<f64>,
    pub ttft_ms: Option<u64>,
    /// Document passages quoted for this answer (file name and part).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRef>,
    /// Speculative decoding: tokens guessed, and how many the model kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_accepted: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub file: String,
    pub part: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: i64,
    pub role: MessageRole,
    pub content: String,
    pub reasoning: String,
    pub stats: Option<MessageStats>,
    pub created_at: i64,
    /// Images the user attached, decrypted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
}

/// An image kept with a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageData {
    pub mime: String,
    /// Base64 of the image bytes.
    pub base64: String,
}

impl ImageData {
    /// The image as a `data:` URL.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.base64)
    }
}

/// The image type from its first bytes, if it is one Gardenwall accepts.
/// The bytes are never decoded here; the sandboxed runtime does that.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The chat this one came from (v10): continued from a handoff, or
    /// branched from it ([`ConversationStore::branch`]), if it still exists.
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    #[serde(flatten)]
    pub summary: ConversationSummary,
    pub system_prompt: String,
    pub messages: Vec<StoredMessage>,
}

/// A message to append.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub role: MessageRole,
    pub content: String,
    pub reasoning: String,
    pub stats: Option<MessageStats>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Canonical (lowercase, hyphenated) form of a conversation id, or an error.
pub fn parse_id(id: &str) -> Result<String> {
    Uuid::parse_str(id)
        .ok()
        .filter(|u| u.hyphenated().to_string() == id)
        .map(|u| u.hyphenated().to_string())
        .ok_or(StoreError::InvalidId)
}

/// Clean a title: no control characters, collapsed whitespace, bounded.
pub fn clean_title(title: &str) -> String {
    let cleaned: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    if cleaned.is_empty() {
        DEFAULT_TITLE.into()
    } else {
        cleaned
    }
}

/// A title derived from the first user message.
pub fn title_from_message(text: &str) -> String {
    const LEN: usize = 60;
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let cleaned = clean_title(first_line);
    if cleaned.chars().count() > LEN {
        let mut t: String = cleaned.chars().take(LEN).collect();
        t.push('…');
        t
    } else {
        cleaned
    }
}

/// Shown in place of an encrypted field that cannot be opened.
pub const UNREADABLE: &str = "[This text is encrypted and cannot be read on this account]";

pub struct ConversationStore {
    conn: Connection,
    cipher: Option<FieldCipher>,
    /// Encrypted rows exist but no key was supplied: read-only.
    locked: bool,
}

impl ConversationStore {
    /// Open without encryption (plaintext storage).
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, None)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    /// Open with an optional field cipher. With a cipher, any plaintext left
    /// from earlier versions is encrypted in place and the database is
    /// compacted so the plaintext does not survive in free pages or the log.
    pub fn open_with(path: &Path, cipher: Option<FieldCipher>) -> Result<Self> {
        Self::init(Connection::open(path)?, cipher)
    }

    pub fn open_in_memory_with(cipher: FieldCipher) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, Some(cipher))
    }

    /// Whether new text is stored encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    /// Whether encrypted chats exist that this store cannot open.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// A consistent copy of the database at `dest` for a backup (taken while
    /// open). `dest` must not exist.
    pub fn copy_to(&self, dest: &Path) -> std::result::Result<(), crate::backup::BackupError> {
        crate::backup::copy_database(&self.conn, dest)
    }

    fn init(conn: Connection, cipher: Option<FieldCipher>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA secure_delete = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(&format!(
                    "BEGIN IMMEDIATE; {CHAT_TABLES} PRAGMA user_version = 1; COMMIT;"
                ))?;
            }
            1..=11 | SCHEMA_VERSION => {}
            newer => return Err(StoreError::UnsupportedSchema(newer)),
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            1 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {AGENT_TABLES} {FORK_COLUMNS} {DOC_TABLES} {DOC_SOURCE_COLUMNS} {IMAGE_TABLES} {MEMORY_TABLES} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            2 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {DOC_TABLES} {DOC_SOURCE_COLUMNS} {IMAGE_TABLES} {MEMORY_TABLES} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            3 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {DOC_SOURCE_COLUMNS} {IMAGE_TABLES} {MEMORY_TABLES} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            4 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {DOC_SOURCE_COLUMNS} {MEMORY_TABLES} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            5 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {DOC_SOURCE_COLUMNS} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            6 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            7 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {FORK_COLUMNS} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            8 => conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))?,
            9..=11 | SCHEMA_VERSION => {}
            newer => return Err(StoreError::UnsupportedSchema(newer)),
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 9 {
            add_handoff_columns(&conn)?;
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 10 {
            conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {} PRAGMA user_version = 11; COMMIT;",
                visits::VISIT_TABLES
            ))?;
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 11 {
            conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {} PRAGMA user_version = 12; COMMIT;",
                drafts::DRAFT_TABLES
            ))?;
        }
        let mut store = Self {
            conn,
            cipher,
            locked: false,
        };
        if store.cipher.is_some() {
            store.encrypt_legacy_rows()?;
        } else {
            let sealed: i64 = store.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE title LIKE 'gw1:%')
                     OR EXISTS(SELECT 1 FROM messages WHERE content LIKE 'gw1:%')
                     OR EXISTS(SELECT 1 FROM agent_transcripts WHERE title LIKE 'gw1:%')
                     OR EXISTS(SELECT 1 FROM doc_collections WHERE name LIKE 'gw1:%')",
                [],
                |r| r.get(0),
            )?;
            store.locked = sealed != 0;
        }
        Ok(store)
    }

    /// Encrypt plaintext rows written before encryption was enabled.
    /// Returns how many fields were sealed.
    fn encrypt_legacy_rows(&mut self) -> Result<usize> {
        let Some(cipher) = &self.cipher else {
            return Ok(0);
        };
        let tx = self.conn.transaction()?;
        let mut sealed = 0usize;
        let convs: Vec<(String, String, String)> = tx
            .prepare("SELECT id, title, system_prompt FROM conversations")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, title, prompt) in convs {
            for (field, column, value) in [
                ("title", "title", title),
                ("prompt", "system_prompt", prompt),
            ] {
                if !FieldCipher::is_sealed(&value) {
                    let value = cipher
                        .seal(&aad(&id, field), &value)
                        .map_err(StoreError::Crypto)?;
                    tx.execute(
                        &format!("UPDATE conversations SET {column} = ?1 WHERE id = ?2"),
                        params![value, id],
                    )?;
                    sealed += 1;
                }
            }
        }
        let msgs: Vec<(i64, String, String, String)> = tx
            .prepare("SELECT id, conversation_id, content, reasoning FROM messages")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (mid, conv, content, reasoning) in msgs {
            for (field, value) in [("content", content), ("reasoning", reasoning)] {
                if !FieldCipher::is_sealed(&value) {
                    let value = cipher
                        .seal(&aad(&conv, field), &value)
                        .map_err(StoreError::Crypto)?;
                    tx.execute(
                        &format!("UPDATE messages SET {field} = ?1 WHERE id = ?2"),
                        params![value, mid],
                    )?;
                    sealed += 1;
                }
            }
        }
        let transcripts: Vec<(String, String, String)> = tx
            .prepare("SELECT id, title, folders FROM agent_transcripts")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, title, folders) in transcripts {
            for (field, value) in [("title", title), ("folders", folders)] {
                if !FieldCipher::is_sealed(&value) {
                    let value = cipher
                        .seal(&transcripts::aad(&id, field), &value)
                        .map_err(StoreError::Crypto)?;
                    tx.execute(
                        &format!("UPDATE agent_transcripts SET {field} = ?1 WHERE id = ?2"),
                        params![value, id],
                    )?;
                    sealed += 1;
                }
            }
        }
        let entries: Vec<(i64, String, String)> = tx
            .prepare("SELECT id, transcript_id, payload FROM agent_entries")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (eid, tid, payload) in entries {
            if !FieldCipher::is_sealed(&payload) {
                let value = cipher
                    .seal(&transcripts::aad(&tid, "entry"), &payload)
                    .map_err(StoreError::Crypto)?;
                tx.execute(
                    "UPDATE agent_entries SET payload = ?1 WHERE id = ?2",
                    params![value, eid],
                )?;
                sealed += 1;
            }
        }
        tx.commit()?;
        if sealed > 0 {
            // Rebuild the file so no page still holds the old plaintext, then
            // empty the write-ahead log.
            self.conn.execute_batch("VACUUM;")?;
            self.scrub_log()?;
        }
        Ok(sealed)
    }

    /// Prepare text for storage under this conversation and field.
    fn seal(&self, conversation: &str, field: &str, text: &str) -> Result<String> {
        match &self.cipher {
            Some(c) => c
                .seal(&aad(conversation, field), text)
                .map_err(StoreError::Crypto),
            None if self.locked => Err(StoreError::Locked),
            None => Ok(text.to_string()),
        }
    }

    /// Recover stored text; unreadable sealed values become [`UNREADABLE`].
    fn unseal(&self, conversation: &str, field: &str, stored: String) -> String {
        if !FieldCipher::is_sealed(&stored) {
            return stored;
        }
        self.cipher
            .as_ref()
            .and_then(|c| c.open(&aad(conversation, field), &stored).ok())
            .unwrap_or_else(|| UNREADABLE.to_string())
    }

    pub fn create(&self, system_prompt: &str) -> Result<ConversationSummary> {
        if system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(StoreError::TooLarge("system prompt"));
        }
        let id = Uuid::new_v4().hyphenated().to_string();
        let t = now();
        let title = self.seal(&id, "title", DEFAULT_TITLE)?;
        let prompt = self.seal(&id, "prompt", system_prompt)?;
        self.conn.execute(
            "INSERT INTO conversations (id, title, system_prompt, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, prompt, t],
        )?;
        Ok(ConversationSummary {
            id,
            title: DEFAULT_TITLE.into(),
            created_at: t,
            updated_at: t,
            parent_id: None,
        })
    }

    /// A new chat continuing `parent` from a handoff: the parent's system
    /// prompt and memory choice, linked to it.
    pub fn create_continuation(&self, parent: &str) -> Result<ConversationSummary> {
        let parent = parse_id(parent)?;
        let prompt = self.get(&parent)?.system_prompt;
        let uses_memories = self.chat_uses_memories(&parent)?;
        let mut created = self.create(&prompt)?;
        self.conn.execute(
            "UPDATE conversations SET parent_id = ?1 WHERE id = ?2",
            params![parent, created.id],
        )?;
        if !uses_memories {
            self.set_chat_uses_memories(&created.id, false)?;
        }
        created.parent_id = Some(parent);
        Ok(created)
    }

    /// A new chat that starts as a copy of `parent`'s messages before
    /// `before` (all of them when `None`), with their images, the parent's
    /// system prompt and memory choice, linked to it and titled
    /// "<title> (branch)". The parent is not changed. Each copied field is
    /// sealed again for the new chat (the chat id is part of the seal).
    pub fn branch(&mut self, parent: &str, before: Option<i64>) -> Result<ConversationSummary> {
        use base64::Engine;
        let parent = parse_id(parent)?;
        let source = self.get(&parent)?;
        let mut created = self.create_continuation(&parent)?;
        let title = clean_title(&format!("{} (branch)", source.summary.title));
        self.rename(&created.id, &title)?;
        created.title = title;
        for m in source
            .messages
            .iter()
            .filter(|m| before.is_none_or(|b| m.id < b))
        {
            let copied = self.append(
                &created.id,
                NewMessage {
                    role: m.role,
                    content: m.content.clone(),
                    reasoning: m.reasoning.clone(),
                    stats: m.stats.clone(),
                },
            )?;
            if !m.images.is_empty() {
                let images: Vec<Vec<u8>> = m
                    .images
                    .iter()
                    .filter_map(|i| {
                        base64::engine::general_purpose::STANDARD
                            .decode(&i.base64)
                            .ok()
                    })
                    .collect();
                self.attach_images(&created.id, copied.id, &images)?;
            }
        }
        Ok(created)
    }

    /// Most recently updated first.
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at, parent_id FROM conversations
             ORDER BY updated_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ConversationSummary {
                id: r.get(0)?,
                title: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
                parent_id: r.get(4)?,
            })
        })?;
        let rows: Vec<ConversationSummary> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|mut c| {
                c.title = self.unseal(&c.id, "title", std::mem::take(&mut c.title));
                c
            })
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<Conversation> {
        let id = parse_id(id)?;
        let (summary, system_prompt) = self
            .conn
            .query_row(
                "SELECT id, title, created_at, updated_at, system_prompt, parent_id
                 FROM conversations WHERE id = ?1",
                [&id],
                |r| {
                    Ok((
                        ConversationSummary {
                            id: r.get(0)?,
                            title: r.get(1)?,
                            created_at: r.get(2)?,
                            updated_at: r.get(3)?,
                            parent_id: r.get(5)?,
                        },
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, reasoning, stats, created_at FROM messages
             WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let messages = stmt
            .query_map([&id], |r| {
                let role: String = r.get(1)?;
                let stats: Option<String> = r.get(4)?;
                Ok(StoredMessage {
                    id: r.get(0)?,
                    // The CHECK constraint guarantees a known role.
                    role: MessageRole::parse(&role).unwrap_or(MessageRole::User),
                    content: r.get(2)?,
                    reasoning: r.get(3)?,
                    // Unreadable statistics are dropped, never fatal.
                    stats: stats.and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: r.get(5)?,
                    images: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<StoredMessage>>>()?;
        let image_rows: Vec<(i64, String, String)> = self
            .conn
            .prepare(
                "SELECT message_id, mime, data FROM message_images
                 WHERE conversation_id = ?1 ORDER BY id",
            )?
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut summary = summary;
        summary.title = self.unseal(&id, "title", std::mem::take(&mut summary.title));
        let system_prompt = self.unseal(&id, "prompt", system_prompt);
        let messages = messages
            .into_iter()
            .map(|mut m| {
                m.content = self.unseal(&id, "content", std::mem::take(&mut m.content));
                m.reasoning = self.unseal(&id, "reasoning", std::mem::take(&mut m.reasoning));
                // Images that can't be opened are left out.
                m.images = image_rows
                    .iter()
                    .filter(|(mid, _, _)| *mid == m.id)
                    .filter_map(|(_, mime, data)| {
                        let base64 = self.unseal(&id, "image", data.clone());
                        (base64 != UNREADABLE).then(|| ImageData {
                            mime: mime.clone(),
                            base64,
                        })
                    })
                    .collect();
                m
            })
            .collect();
        Ok(Conversation {
            summary,
            system_prompt,
            messages,
        })
    }

    fn update(&self, id: &str, field: &str, sql: &str, value: &str) -> Result<()> {
        let id = parse_id(id)?;
        let value = self.seal(&id, field, value)?;
        let changed = self.conn.execute(sql, params![value, now(), id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        self.update(
            id,
            "title",
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            &clean_title(title),
        )
    }

    pub fn set_system_prompt(&self, id: &str, prompt: &str) -> Result<()> {
        if prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(StoreError::TooLarge("system prompt"));
        }
        self.update(
            id,
            "prompt",
            "UPDATE conversations SET system_prompt = ?1, updated_at = ?2 WHERE id = ?3",
            prompt,
        )
    }

    /// Append a message. The first user message also names an untitled
    /// conversation.
    pub fn append(&mut self, id: &str, message: NewMessage) -> Result<StoredMessage> {
        let id = parse_id(id)?;
        if message.content.len() > MAX_MESSAGE_BYTES || message.reasoning.len() > MAX_MESSAGE_BYTES
        {
            return Err(StoreError::TooLarge("message"));
        }
        let stats = message
            .stats
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default());
        let t = now();
        let content = self.seal(&id, "content", &message.content)?;
        let reasoning = self.seal(&id, "reasoning", &message.reasoning)?;
        let stored_title: Option<String> = self
            .conn
            .query_row(
                "SELECT title FROM conversations WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        let current_title = self.unseal(&id, "title", stored_title.ok_or(StoreError::NotFound)?);
        let renamed = (message.role == MessageRole::User && current_title == DEFAULT_TITLE)
            .then(|| self.seal(&id, "title", &title_from_message(&message.content)))
            .transpose()?;
        let tx = self.conn.transaction()?;
        let title: String = tx
            .query_row(
                "SELECT title FROM conversations WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        tx.execute(
            "INSERT INTO messages (conversation_id, role, content, reasoning, stats, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, message.role.as_str(), content, reasoning, stats, t],
        )?;
        let message_id = tx.last_insert_rowid();
        let new_title = renamed.unwrap_or(title);
        tx.execute(
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_title, t, id],
        )?;
        tx.commit()?;
        Ok(StoredMessage {
            id: message_id,
            role: message.role,
            content: message.content,
            reasoning: message.reasoning,
            stats: message.stats,
            created_at: t,
            images: Vec::new(),
        })
    }

    /// Keep images with a message (sealed like its text). Each must be a
    /// PNG, JPEG, WebP or GIF of at most [`MAX_IMAGE_BYTES`].
    pub fn attach_images(
        &mut self,
        id: &str,
        message_id: i64,
        images: &[Vec<u8>],
    ) -> Result<Vec<ImageData>> {
        use base64::Engine;
        let id = parse_id(id)?;
        if images.len() > MAX_IMAGES_PER_MESSAGE {
            return Err(StoreError::TooLarge("number of images"));
        }
        let mut out = Vec::new();
        let mut sealed = Vec::new();
        for bytes in images {
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StoreError::TooLarge("image"));
            }
            let mime =
                sniff_image(bytes).ok_or(StoreError::TooLarge("image (unsupported type)"))?;
            let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            sealed.push((mime, self.seal(&id, "image", &base64)?));
            out.push(ImageData {
                mime: mime.into(),
                base64,
            });
        }
        let tx = self.conn.transaction()?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT conversation_id FROM messages WHERE id = ?1",
                [message_id],
                |r| r.get(0),
            )
            .optional()?;
        if owner.as_deref() != Some(id.as_str()) {
            return Err(StoreError::NotFound);
        }
        for (mime, data) in sealed {
            tx.execute(
                "INSERT INTO message_images (message_id, conversation_id, mime, data) VALUES (?1, ?2, ?3, ?4)",
                params![message_id, id, mime, data],
            )?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// Delete `message_id` and every later message of the conversation (used
    /// to regenerate or edit). Returns how many messages were removed.
    pub fn truncate_from(&self, id: &str, message_id: i64) -> Result<usize> {
        let id = parse_id(id)?;
        let removed = self.conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?1 AND id >= ?2",
            params![id, message_id],
        )?;
        self.conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        self.scrub_log()?;
        Ok(removed)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let changed = self
            .conn
            .execute("DELETE FROM conversations WHERE id = ?1", [&id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.scrub_log()
    }

    /// Move committed pages into the database (where `secure_delete` has
    /// zeroed freed content) and truncate the write-ahead log, so deleted
    /// text does not survive in it.
    fn scrub_log(&self) -> Result<()> {
        // A busy checkpoint is not an error; the next one completes it.
        let _: (i64, i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> NewMessage {
        NewMessage {
            role: MessageRole::User,
            content: text.into(),
            reasoning: String::new(),
            stats: None,
        }
    }

    fn assistant(text: &str) -> NewMessage {
        NewMessage {
            role: MessageRole::Assistant,
            content: text.into(),
            reasoning: "thinking".into(),
            stats: Some(MessageStats {
                total_tokens: Some(42),
                ..MessageStats::default()
            }),
        }
    }

    #[test]
    fn conversations_round_trip_and_are_titled_by_the_first_message() {
        let mut store = ConversationStore::open_in_memory().unwrap();
        let c = store.create("Be brief.").unwrap();
        assert_eq!(c.title, DEFAULT_TITLE);
        store
            .append(&c.id, user("  How do\tsandboxes work?\nmore"))
            .unwrap();
        store.append(&c.id, assistant("They isolate.")).unwrap();
        store.append(&c.id, user("Second question")).unwrap();

        let got = store.get(&c.id).unwrap();
        assert_eq!(got.summary.title, "How do sandboxes work?");
        assert_eq!(got.system_prompt, "Be brief.");
        assert_eq!(got.messages.len(), 3);
        assert_eq!(got.messages[1].role, MessageRole::Assistant);
        assert_eq!(got.messages[1].reasoning, "thinking");
        assert_eq!(
            got.messages[1].stats.as_ref().unwrap().total_tokens,
            Some(42)
        );

        store.rename(&c.id, "Renamed\u{7}  title").unwrap();
        store.set_system_prompt(&c.id, "").unwrap();
        let got = store.get(&c.id).unwrap();
        assert_eq!(got.summary.title, "Renamed title");
        assert_eq!(got.system_prompt, "");
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn images_are_kept_encrypted_with_their_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let mut s = ConversationStore::open_with(&path, Some(key(9))).unwrap();
        let c = s.create("").unwrap();
        let m = s.append(&c.id, user("what is in this picture?")).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(b"IMG-CANARY-5e1f");
        let stored = s.attach_images(&c.id, m.id, &[png.clone()]).unwrap();
        assert_eq!(stored[0].mime, "image/png");
        let got = s.get(&c.id).unwrap();
        assert_eq!(got.messages[0].images.len(), 1);
        assert!(
            got.messages[0].images[0]
                .data_url()
                .starts_with("data:image/png;base64,")
        );
        // Not an image, or too many: refused.
        assert!(
            s.attach_images(&c.id, m.id, &[b"plain text".to_vec()])
                .is_err()
        );
        assert!(
            s.attach_images(&c.id, m.id, &vec![png.clone(); MAX_IMAGES_PER_MESSAGE + 1])
                .is_err()
        );
        // The base64 of the bytes never reaches the disk unsealed.
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        drop(s);
        assert!(!files_contain(dir.path(), &b64[8..]));
        assert!(!files_contain(dir.path(), "IMG-CANARY"));
    }

    #[test]
    fn image_types_are_recognised_by_their_first_bytes() {
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg>"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn truncate_removes_the_message_and_everything_after_it() {
        let mut store = ConversationStore::open_in_memory().unwrap();
        let a = store.create("").unwrap();
        let b = store.create("").unwrap();
        store.append(&a.id, user("1")).unwrap();
        let second = store.append(&a.id, assistant("2")).unwrap();
        store.append(&a.id, user("3")).unwrap();
        let other = store.append(&b.id, user("other")).unwrap();
        assert_eq!(store.truncate_from(&a.id, second.id).unwrap(), 2);
        assert_eq!(store.get(&a.id).unwrap().messages.len(), 1);
        // Another conversation's messages are untouched even with a smaller id.
        assert_eq!(store.truncate_from(&a.id, 0).unwrap(), 1);
        assert_eq!(store.get(&b.id).unwrap().messages[0].id, other.id);
    }

    #[test]
    fn deleting_cascades_and_unknown_or_malformed_ids_are_rejected() {
        let mut store = ConversationStore::open_in_memory().unwrap();
        let c = store.create("").unwrap();
        store.append(&c.id, user("secret")).unwrap();
        store.delete(&c.id).unwrap();
        assert!(matches!(store.get(&c.id), Err(StoreError::NotFound)));
        let orphans: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orphans, 0);

        let missing = Uuid::new_v4().hyphenated().to_string();
        assert!(matches!(store.delete(&missing), Err(StoreError::NotFound)));
        assert!(matches!(
            store.append(&missing, user("x")),
            Err(StoreError::NotFound)
        ));
        for bad in [
            "",
            "x",
            "' OR 1=1 --",
            &c.id.to_uppercase(),
            &c.id.replace('-', ""),
        ] {
            assert!(
                matches!(store.get(bad), Err(StoreError::InvalidId)),
                "{bad}"
            );
        }
    }

    #[test]
    fn oversized_input_is_refused() {
        let mut store = ConversationStore::open_in_memory().unwrap();
        let big = "x".repeat(MAX_SYSTEM_PROMPT_BYTES + 1);
        assert!(matches!(store.create(&big), Err(StoreError::TooLarge(_))));
        let c = store.create("").unwrap();
        assert!(matches!(
            store.set_system_prompt(&c.id, &big),
            Err(StoreError::TooLarge(_))
        ));
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(matches!(
            store.append(&c.id, user(&huge)),
            Err(StoreError::TooLarge(_))
        ));
        assert_eq!(
            clean_title(&"t".repeat(500)).chars().count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(clean_title(" \n\t"), DEFAULT_TITLE);
        assert!(title_from_message(&"word ".repeat(40)).ends_with('…'));
    }

    fn key(byte: u8) -> FieldCipher {
        FieldCipher::new(&zeroize::Zeroizing::new([byte; 32]))
    }

    fn files_contain(dir: &std::path::Path, marker: &str) -> bool {
        std::fs::read_dir(dir).unwrap().any(|entry| {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            bytes.windows(marker.len()).any(|w| w == marker.as_bytes())
        })
    }

    #[test]
    fn encrypted_chats_never_reach_the_disk_in_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        let marker = "CANARY-enc-4b1d-plaintext";
        let id = {
            let mut store = ConversationStore::open_with(&path, Some(key(7))).unwrap();
            assert!(store.is_encrypted());
            let c = store.create(&format!("prompt {marker}")).unwrap();
            store
                .append(&c.id, user(&format!("question {marker}")))
                .unwrap();
            store
                .append(&c.id, assistant(&format!("answer {marker}")))
                .unwrap();
            let got = store.get(&c.id).unwrap();
            assert_eq!(got.summary.title, format!("question {marker}"));
            assert_eq!(got.messages[1].content, format!("answer {marker}"));
            assert_eq!(store.list().unwrap()[0].title, format!("question {marker}"));
            c.id
        };
        assert!(
            !files_contain(dir.path(), marker),
            "plaintext found on disk"
        );
        // The same key reads everything back after reopening.
        let store = ConversationStore::open_with(&path, Some(key(7))).unwrap();
        assert_eq!(
            store.get(&id).unwrap().system_prompt,
            format!("prompt {marker}")
        );
    }

    #[test]
    fn plaintext_from_earlier_versions_is_encrypted_and_erased() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        let marker = "CANARY-legacy-91c2-plaintext";
        let id = {
            let mut store = ConversationStore::open(&path).unwrap();
            let c = store.create("").unwrap();
            store.append(&c.id, user(&marker.repeat(50))).unwrap();
            c.id
        };
        assert!(files_contain(dir.path(), marker));
        let store = ConversationStore::open_with(&path, Some(key(3))).unwrap();
        assert!(
            !files_contain(dir.path(), marker),
            "legacy plaintext survived migration"
        );
        assert_eq!(
            store.get(&id).unwrap().messages[0].content,
            marker.repeat(50)
        );
    }

    #[test]
    fn without_the_key_chats_are_unreadable_and_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        let id = {
            let mut store = ConversationStore::open_with(&path, Some(key(1))).unwrap();
            let c = store.create("").unwrap();
            store.append(&c.id, user("private")).unwrap();
            c.id
        };
        let mut locked = ConversationStore::open(&path).unwrap();
        assert!(locked.is_locked());
        assert_eq!(locked.get(&id).unwrap().messages[0].content, UNREADABLE);
        assert!(matches!(
            locked.append(&id, user("x")),
            Err(StoreError::Locked)
        ));
        assert!(matches!(locked.create(""), Err(StoreError::Locked)));
        // A different key cannot read them either.
        let wrong = ConversationStore::open_with(&path, Some(key(2))).unwrap();
        assert_eq!(wrong.get(&id).unwrap().summary.title, UNREADABLE);
    }

    #[test]
    fn sealed_values_cannot_be_moved_between_conversations() {
        let mut store = ConversationStore::open_in_memory_with(key(5)).unwrap();
        let a = store.create("").unwrap();
        let b = store.create("").unwrap();
        store.append(&a.id, user("from a")).unwrap();
        store.append(&b.id, user("from b")).unwrap();
        store
            .conn
            .execute(
                "UPDATE messages SET content = (SELECT content FROM messages WHERE conversation_id = ?1)
                 WHERE conversation_id = ?2",
                params![a.id, b.id],
            )
            .unwrap();
        assert_eq!(store.get(&b.id).unwrap().messages[0].content, UNREADABLE);
    }

    #[test]
    fn deleted_text_does_not_remain_in_the_database_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        let marker = "CANARY-7f3a9c-do-not-keep";
        {
            let mut store = ConversationStore::open(&path).unwrap();
            let c = store.create("").unwrap();
            store.append(&c.id, user(&marker.repeat(200))).unwrap();
            store.delete(&c.id).unwrap();
            for entry in std::fs::read_dir(dir.path()).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                let found = bytes.windows(marker.len()).any(|w| w == marker.as_bytes());
                assert!(!found, "deleted text found on disk");
            }
        }
        // Reopening an existing database works and keeps the schema.
        let store = ConversationStore::open(&path).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn a_version_9_database_gains_handoff_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN; {CHAT_TABLES} {AGENT_TABLES} {FORK_COLUMNS} {DOC_TABLES} {DOC_SOURCE_COLUMNS} {IMAGE_TABLES} {MEMORY_TABLES} {AGENT_IMAGE_TABLES} {PROJECT_TABLES} PRAGMA user_version = 9; COMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES ('11111111-1111-4111-8111-111111111111', 'Old chat', 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agent_transcripts (id, title, folders, created_at, updated_at)
                 VALUES ('22222222-2222-4222-8222-222222222222', 'Old session', '[]', 1, 1)",
                [],
            )
            .unwrap();
        }
        let store = ConversationStore::open_with(&path, Some(key(5))).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let chats = store.list().unwrap();
        assert_eq!(chats[0].title, "Old chat");
        assert_eq!(chats[0].parent_id, None);
        let t = &store.list_transcripts().unwrap()[0];
        assert!(!t.handoff);
        assert_eq!(t.parent_chat_id, None);
    }

    #[test]
    fn a_branch_copies_the_chat_up_to_a_message_and_leaves_it_unchanged() {
        let mut s = ConversationStore::open_in_memory_with(key(7)).unwrap();
        let chat = s.create("Be brief.").unwrap();
        let q1 = s.append(&chat.id, user("first question")).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(b"image body");
        s.attach_images(&chat.id, q1.id, &[png]).unwrap();
        s.append(&chat.id, assistant("first answer")).unwrap();
        let q2 = s.append(&chat.id, user("second question")).unwrap();
        s.append(&chat.id, assistant("second answer")).unwrap();

        // Edit the second question: branch before it.
        let branch = s.branch(&chat.id, Some(q2.id)).unwrap();
        assert_eq!(branch.parent_id.as_deref(), Some(chat.id.as_str()));
        assert_eq!(branch.title, "first question (branch)");
        let got = s.get(&branch.id).unwrap();
        assert_eq!(got.system_prompt, "Be brief.");
        let texts: Vec<&str> = got.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, ["first question", "first answer"]);
        assert_eq!(got.messages[0].images.len(), 1, "images are copied");
        assert_eq!(got.messages[1].reasoning, "thinking");
        assert_eq!(
            got.messages[1].stats.as_ref().unwrap().total_tokens,
            Some(42)
        );
        // The original keeps everything.
        assert_eq!(s.get(&chat.id).unwrap().messages.len(), 4);
        // A whole copy.
        let all = s.branch(&chat.id, None).unwrap();
        assert_eq!(s.get(&all.id).unwrap().messages.len(), 4);
        // Branch of a missing chat.
        assert!(
            s.branch("00000000-0000-0000-0000-000000000000", None)
                .is_err()
        );
    }

    #[test]
    fn handoffs_link_sessions_and_chats_to_what_they_continue() {
        let mut s = ConversationStore::open_in_memory_with(key(6)).unwrap();
        // Chat to chat: the prompt and memory choice carry over.
        let chat = s.create("Be brief.").unwrap();
        s.set_chat_uses_memories(&chat.id, false).unwrap();
        s.append(&chat.id, user("first")).unwrap();
        let next = s.create_continuation(&chat.id).unwrap();
        assert_eq!(next.parent_id.as_deref(), Some(chat.id.as_str()));
        let got = s.get(&next.id).unwrap();
        assert_eq!(got.system_prompt, "Be brief.");
        assert!(got.messages.is_empty());
        assert_eq!(got.summary.parent_id.as_deref(), Some(chat.id.as_str()));
        assert!(!s.chat_uses_memories(&next.id).unwrap());

        // Agent to agent, and chat to agent.
        let a = s.create_transcript(&["C:/p".into()]).unwrap();
        let b = s.create_transcript(&["C:/p".into()]).unwrap();
        s.set_transcript_handoff(&b.id, HandoffParent::Agent(&a.id))
            .unwrap();
        let got = s.get_transcript(&b.id).unwrap().summary;
        assert!(got.handoff);
        assert_eq!(got.parent_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(got.fork_seq, None);
        let c = s.create_transcript(&["C:/p".into()]).unwrap();
        s.set_transcript_handoff(&c.id, HandoffParent::Chat(&chat.id))
            .unwrap();
        let got = s.get_transcript(&c.id).unwrap().summary;
        assert!(got.handoff);
        assert_eq!(got.parent_chat_id.as_deref(), Some(chat.id.as_str()));
        assert_eq!(got.parent_id, None);

        // A parent that doesn't exist is refused; deleting one keeps the child.
        assert!(
            s.set_transcript_handoff(
                &c.id,
                HandoffParent::Agent("33333333-3333-4333-8333-333333333333")
            )
            .is_err()
        );
        s.delete(&chat.id).unwrap();
        assert_eq!(s.get(&next.id).unwrap().summary.parent_id, None);
        assert_eq!(
            s.get_transcript(&c.id).unwrap().summary.parent_chat_id,
            None
        );
        s.delete_transcript(&a.id).unwrap();
        assert_eq!(s.get_transcript(&b.id).unwrap().summary.parent_id, None);
    }
}
