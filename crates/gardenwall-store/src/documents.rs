//! Document collections for document chat (ROADMAP Phase 5).
//!
//! Kept in `conversations.db` and protected like chats: collection names,
//! file names, passage text and embeddings are sealed with the chat key and
//! bound to their collection, deletions are overwritten, and the log is
//! scrubbed afterwards. Only sizes, counts and times are stored in the clear.
//! Search happens in memory over decrypted passages, so no plaintext index
//! is ever written to disk.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use super::{ConversationStore, Result, StoreError, clean_title, now, parse_id};

/// Most passages stored per collection.
pub const MAX_CHUNKS_PER_COLLECTION: usize = 200_000;
/// Largest passage stored (bytes).
pub const MAX_CHUNK_BYTES: usize = 16 * 1024;
/// Most dimensions accepted for an embedding vector.
pub const MAX_EMBEDDING_DIMS: usize = 8192;

pub(super) fn aad(collection: &str, field: &str) -> String {
    format!("gardenwall-docs:{collection}:{field}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionSummary {
    pub id: String,
    pub name: String,
    pub files: u64,
    pub chunks: u64,
    /// Passages with an embedding from the current embedding model.
    pub embedded: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    pub id: i64,
    pub name: String,
    pub size: u64,
    pub chunks: u64,
    pub added_at: i64,
    /// The file's location was saved (v6), so it can be checked for changes
    /// and read again. Files added earlier must be removed and added again.
    pub has_source: bool,
}

/// Where a document came from: the path the user picked, and the file's
/// modified time (milliseconds since the epoch) and SHA-256 (hex) when it
/// was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSource {
    pub path: String,
    pub mtime: i64,
    pub sha256: String,
}

/// A stored file's saved source, for change checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub file_id: i64,
    pub size: u64,
    pub source: FileSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredChunk {
    pub id: i64,
    pub file_name: String,
    pub seq: u64,
    pub text: String,
    /// Present when embedded with `embed_model`.
    pub embedding: Option<Vec<f32>>,
}

/// Rows as read from SQLite, before decryption.
type ChunkRow = (i64, String, i64, String, Option<String>, Option<String>);
type CollectionRow = (String, String, i64, i64, i64, i64, i64);

fn encode_vector(v: &[f32]) -> String {
    v.iter().map(|x| format!("{:08x}", x.to_bits())).collect()
}

fn decode_vector(s: &str) -> Option<Vec<f32>> {
    if !s.len().is_multiple_of(8) || s.len() / 8 > MAX_EMBEDDING_DIMS {
        return None;
    }
    (0..s.len() / 8)
        .map(|i| {
            u32::from_str_radix(s.get(i * 8..i * 8 + 8)?, 16)
                .ok()
                .map(f32::from_bits)
        })
        .collect()
}

fn insert_chunks(
    tx: &rusqlite::Transaction<'_>,
    file_id: i64,
    collection: &str,
    sealed: &[String],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO doc_chunks (file_id, collection_id, seq, text) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (seq, text) in sealed.iter().enumerate() {
        stmt.execute(params![file_id, collection, seq as i64, text])?;
    }
    Ok(())
}

impl ConversationStore {
    fn seal_doc(&self, collection: &str, field: &str, text: &str) -> Result<String> {
        match &self.cipher {
            Some(c) => c
                .seal(&aad(collection, field), text)
                .map_err(StoreError::Crypto),
            None if self.locked => Err(StoreError::Locked),
            None => Ok(text.to_string()),
        }
    }

    fn unseal_doc(&self, collection: &str, field: &str, stored: String) -> Option<String> {
        if !crate::crypto::FieldCipher::is_sealed(&stored) {
            return Some(stored);
        }
        self.cipher
            .as_ref()
            .and_then(|c| c.open(&aad(collection, field), &stored).ok())
    }

    pub fn create_collection(&self, name: &str) -> Result<CollectionSummary> {
        let id = Uuid::new_v4().hyphenated().to_string();
        let name = clean_title(name);
        let t = now();
        let sealed = self.seal_doc(&id, "name", &name)?;
        self.conn.execute(
            "INSERT INTO doc_collections (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![id, sealed, t],
        )?;
        Ok(CollectionSummary {
            id,
            name,
            files: 0,
            chunks: 0,
            embedded: 0,
            created_at: t,
            updated_at: t,
        })
    }

    /// Most recently updated first. `embed_model` counts passages embedded
    /// with that model.
    pub fn list_collections(&self, embed_model: Option<&str>) -> Result<Vec<CollectionSummary>> {
        let rows: Vec<CollectionRow> = self
            .conn
            .prepare(
                "SELECT c.id, c.name, c.created_at, c.updated_at,
                        (SELECT COUNT(*) FROM doc_files f WHERE f.collection_id = c.id),
                        (SELECT COUNT(*) FROM doc_chunks k WHERE k.collection_id = c.id),
                        (SELECT COUNT(*) FROM doc_chunks k WHERE k.collection_id = c.id
                             AND k.embed_model IS NOT NULL AND k.embed_model = ?1)
                 FROM doc_collections c ORDER BY c.updated_at DESC, c.rowid DESC",
            )?
            .query_map([embed_model], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(
                |(id, name, created, updated, files, chunks, embedded)| CollectionSummary {
                    name: self
                        .unseal_doc(&id, "name", name)
                        .unwrap_or_else(|| super::UNREADABLE.to_string()),
                    id,
                    files: files as u64,
                    chunks: chunks as u64,
                    embedded: embedded as u64,
                    created_at: created,
                    updated_at: updated,
                },
            )
            .collect())
    }

    fn require_collection(&self, id: &str) -> Result<String> {
        let id = parse_id(id)?;
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM doc_collections WHERE id = ?1", [&id], |r| {
                r.get(0)
            })
            .optional()?;
        exists.map(|_| id).ok_or(StoreError::NotFound)
    }

    pub fn rename_collection(&self, id: &str, name: &str) -> Result<()> {
        let id = self.require_collection(id)?;
        let sealed = self.seal_doc(&id, "name", &clean_title(name))?;
        self.conn.execute(
            "UPDATE doc_collections SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![sealed, now(), id],
        )?;
        Ok(())
    }

    pub fn delete_collection(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let changed = self
            .conn
            .execute("DELETE FROM doc_collections WHERE id = ?1", [&id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.scrub_log()
    }

    /// Check passage sizes and the collection's limit (`replacing` passages
    /// are about to be removed), and seal the passages.
    fn prepare_chunks(
        &self,
        collection: &str,
        chunks: &[String],
        replacing: i64,
    ) -> Result<Vec<String>> {
        if chunks.iter().any(|c| c.len() > MAX_CHUNK_BYTES) {
            return Err(StoreError::TooLarge("document passage"));
        }
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE collection_id = ?1",
            [collection],
            |r| r.get(0),
        )?;
        if (existing - replacing).max(0) as usize + chunks.len() > MAX_CHUNKS_PER_COLLECTION {
            return Err(StoreError::TooLarge("collection"));
        }
        chunks
            .iter()
            .map(|c| self.seal_doc(collection, "chunk", c))
            .collect()
    }

    /// The sealed path and hash of a source, for storing.
    fn seal_source(
        &self,
        collection: &str,
        source: Option<&FileSource>,
    ) -> Result<(Option<String>, Option<i64>, Option<String>)> {
        Ok(match source {
            None => (None, None, None),
            Some(s) => (
                Some(self.seal_doc(collection, "source", &s.path)?),
                Some(s.mtime),
                Some(self.seal_doc(collection, "source-hash", &s.sha256)?),
            ),
        })
    }

    /// Add one file's passages, with where it came from when known. Returns
    /// the file's id.
    pub fn add_file(
        &mut self,
        collection: &str,
        name: &str,
        size: u64,
        chunks: &[String],
        source: Option<&FileSource>,
    ) -> Result<i64> {
        let collection = self.require_collection(collection)?;
        let sealed = self.prepare_chunks(&collection, chunks, 0)?;
        let name = clean_title(name);
        let sealed_name = self.seal_doc(&collection, "file", &name)?;
        let (src, mtime, hash) = self.seal_source(&collection, source)?;
        let t = now();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO doc_files (collection_id, name, size, chunks, added_at, source, source_mtime, source_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![collection, sealed_name, size as i64, chunks.len() as i64, t, src, mtime, hash],
        )?;
        let file_id = tx.last_insert_rowid();
        insert_chunks(&tx, file_id, &collection, &sealed)?;
        tx.execute(
            "UPDATE doc_collections SET updated_at = ?1 WHERE id = ?2",
            params![t, collection],
        )?;
        tx.commit()?;
        Ok(file_id)
    }

    /// Replace a file's passages with a new reading of its source, keeping
    /// its id and name. The old passages and their embeddings are removed in
    /// the same transaction.
    pub fn replace_file(
        &mut self,
        collection: &str,
        file_id: i64,
        size: u64,
        chunks: &[String],
        source: &FileSource,
    ) -> Result<()> {
        let collection = self.require_collection(collection)?;
        let old: i64 = self
            .conn
            .query_row(
                "SELECT chunks FROM doc_files WHERE id = ?1 AND collection_id = ?2",
                params![file_id, collection],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let sealed = self.prepare_chunks(&collection, chunks, old)?;
        let (src, mtime, hash) = self.seal_source(&collection, Some(source))?;
        let t = now();
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM doc_chunks WHERE file_id = ?1 AND collection_id = ?2",
            params![file_id, collection],
        )?;
        tx.execute(
            "UPDATE doc_files SET size = ?1, chunks = ?2, source = ?3, source_mtime = ?4, source_hash = ?5
             WHERE id = ?6 AND collection_id = ?7",
            params![size as i64, chunks.len() as i64, src, mtime, hash, file_id, collection],
        )?;
        insert_chunks(&tx, file_id, &collection, &sealed)?;
        tx.execute(
            "UPDATE doc_collections SET updated_at = ?1 WHERE id = ?2",
            params![t, collection],
        )?;
        tx.commit()?;
        self.scrub_log()
    }

    /// The saved sources of a collection's files (files added before v6, or
    /// whose source can't be decrypted, are left out).
    pub fn sources(&self, collection: &str) -> Result<Vec<SourceRecord>> {
        let collection = self.require_collection(collection)?;
        let rows: Vec<(i64, i64, String, i64, String)> = self
            .conn
            .prepare(
                "SELECT id, size, source, source_mtime, source_hash FROM doc_files
                 WHERE collection_id = ?1 AND source IS NOT NULL
                   AND source_mtime IS NOT NULL AND source_hash IS NOT NULL
                 ORDER BY id",
            )?
            .query_map([&collection], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(file_id, size, path, mtime, hash)| {
                Some(SourceRecord {
                    file_id,
                    size: size as u64,
                    source: FileSource {
                        path: self.unseal_doc(&collection, "source", path)?,
                        mtime,
                        sha256: self.unseal_doc(&collection, "source-hash", hash)?,
                    },
                })
            })
            .collect())
    }

    /// Record a new modified time for a file whose content was found
    /// unchanged, so the next check doesn't hash it again.
    pub fn touch_source(&self, collection: &str, file_id: i64, mtime: i64) -> Result<()> {
        let collection = parse_id(collection)?;
        self.conn.execute(
            "UPDATE doc_files SET source_mtime = ?1 WHERE id = ?2 AND collection_id = ?3 AND source IS NOT NULL",
            params![mtime, file_id, collection],
        )?;
        Ok(())
    }

    pub fn remove_file(&self, collection: &str, file_id: i64) -> Result<()> {
        let collection = parse_id(collection)?;
        let changed = self.conn.execute(
            "DELETE FROM doc_files WHERE id = ?1 AND collection_id = ?2",
            params![file_id, collection],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.conn.execute(
            "UPDATE doc_collections SET updated_at = ?1 WHERE id = ?2",
            params![now(), collection],
        )?;
        self.scrub_log()
    }

    pub fn files(&self, collection: &str) -> Result<Vec<FileSummary>> {
        let collection = self.require_collection(collection)?;
        let rows: Vec<(i64, String, i64, i64, i64, bool)> = self
            .conn
            .prepare(
                "SELECT id, name, size, chunks, added_at, source IS NOT NULL FROM doc_files
                 WHERE collection_id = ?1 ORDER BY id",
            )?
            .query_map([&collection], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(
                |(id, name, size, chunks, added_at, has_source)| FileSummary {
                    has_source,
                    id,
                    name: self
                        .unseal_doc(&collection, "file", name)
                        .unwrap_or_else(|| super::UNREADABLE.to_string()),
                    size: size as u64,
                    chunks: chunks as u64,
                    added_at,
                },
            )
            .collect())
    }

    /// Every passage of a collection, decrypted, with embeddings made by
    /// `embed_model` (others are ignored). Unreadable passages are skipped.
    pub fn load_chunks(
        &self,
        collection: &str,
        embed_model: Option<&str>,
    ) -> Result<Vec<StoredChunk>> {
        let collection = self.require_collection(collection)?;
        let rows: Vec<ChunkRow> = self
            .conn
            .prepare(
                "SELECT k.id, f.name, k.seq, k.text, k.embedding, k.embed_model
                 FROM doc_chunks k JOIN doc_files f ON f.id = k.file_id
                 WHERE k.collection_id = ?1 ORDER BY k.id",
            )?
            .query_map([&collection], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut names: std::collections::HashMap<String, String> = Default::default();
        Ok(rows
            .into_iter()
            .filter_map(|(id, file, seq, text, emb, model)| {
                let file_name = names
                    .entry(file.clone())
                    .or_insert_with(|| {
                        self.unseal_doc(&collection, "file", file)
                            .unwrap_or_default()
                    })
                    .clone();
                let embedding = match (emb, model.as_deref(), embed_model) {
                    (Some(e), Some(m), Some(want)) if m == want => self
                        .unseal_doc(&collection, "embedding", e)
                        .and_then(|e| decode_vector(&e)),
                    _ => None,
                };
                Some(StoredChunk {
                    id,
                    file_name,
                    seq: seq as u64,
                    text: self.unseal_doc(&collection, "chunk", text)?,
                    embedding,
                })
            })
            .collect())
    }

    /// The passages of one file, in order and decrypted: the text Gardenwall
    /// extracted from it and searches (for "view the extracted text").
    /// Unreadable passages are skipped.
    pub fn file_passages(&self, collection: &str, file_id: i64) -> Result<Vec<String>> {
        let collection = self.require_collection(collection)?;
        let rows: Vec<String> = self
            .conn
            .prepare(
                "SELECT text FROM doc_chunks
                 WHERE collection_id = ?1 AND file_id = ?2 ORDER BY seq, id",
            )?
            .query_map(rusqlite::params![&collection, file_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|t| self.unseal_doc(&collection, "chunk", t))
            .collect())
    }

    /// Store embeddings made by `model` for passages of `collection`.
    pub fn set_embeddings(
        &mut self,
        collection: &str,
        model: &str,
        vectors: &[(i64, Vec<f32>)],
    ) -> Result<()> {
        let collection = self.require_collection(collection)?;
        if vectors
            .iter()
            .any(|(_, v)| v.len() > MAX_EMBEDDING_DIMS || v.iter().any(|x| !x.is_finite()))
        {
            return Err(StoreError::TooLarge("embedding"));
        }
        let sealed: Vec<(i64, String)> = vectors
            .iter()
            .map(|(id, v)| {
                Ok((
                    *id,
                    self.seal_doc(&collection, "embedding", &encode_vector(v))?,
                ))
            })
            .collect::<Result<_>>()?;
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE doc_chunks SET embedding = ?1, embed_model = ?2 WHERE id = ?3 AND collection_id = ?4",
            )?;
            for (id, e) in &sealed {
                stmt.execute(params![e, model, id, collection])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Attach a collection to a conversation (or detach with `None`).
    pub fn link_collection(&self, conversation: &str, collection: Option<&str>) -> Result<()> {
        let conversation = parse_id(conversation)?;
        match collection {
            None => {
                self.conn.execute(
                    "DELETE FROM doc_links WHERE conversation_id = ?1",
                    [&conversation],
                )?;
            }
            Some(c) => {
                let c = self.require_collection(c)?;
                self.conn.execute(
                    "INSERT INTO doc_links (conversation_id, collection_id) VALUES (?1, ?2)
                     ON CONFLICT(conversation_id) DO UPDATE SET collection_id = excluded.collection_id",
                    params![conversation, c],
                )?;
            }
        }
        Ok(())
    }

    pub fn linked_collection(&self, conversation: &str) -> Result<Option<String>> {
        let conversation = parse_id(conversation)?;
        Ok(self
            .conn
            .query_row(
                "SELECT collection_id FROM doc_links WHERE conversation_id = ?1",
                [&conversation],
                |r| r.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::FieldCipher;

    fn key(b: u8) -> FieldCipher {
        FieldCipher::new(&zeroize::Zeroizing::new([b; 32]))
    }

    #[test]
    fn collections_hold_files_passages_and_embeddings() {
        let mut s = ConversationStore::open_in_memory_with(key(1)).unwrap();
        let c = s.create_collection("Tax  papers").unwrap();
        assert_eq!(c.name, "Tax papers");
        let f = s
            .add_file(
                &c.id,
                "2025.pdf",
                1234,
                &["first passage".into(), "second passage".into()],
                None,
            )
            .unwrap();
        s.add_file(&c.id, "notes.md", 10, &["third".into()], None)
            .unwrap();
        let chunks = s.load_chunks(&c.id, Some("m1")).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(
            s.file_passages(&c.id, f).unwrap(),
            vec!["first passage".to_string(), "second passage".to_string()]
        );
        assert_eq!(chunks[0].file_name, "2025.pdf");
        assert_eq!(chunks[1].text, "second passage");
        s.set_embeddings(&c.id, "m1", &[(chunks[0].id, vec![0.5, -1.25])])
            .unwrap();
        let chunks = s.load_chunks(&c.id, Some("m1")).unwrap();
        assert_eq!(chunks[0].embedding, Some(vec![0.5, -1.25]));
        // Embeddings from another model are not used.
        assert_eq!(s.load_chunks(&c.id, Some("m2")).unwrap()[0].embedding, None);
        let list = s.list_collections(Some("m1")).unwrap();
        assert_eq!((list[0].files, list[0].chunks, list[0].embedded), (2, 3, 1));
        s.remove_file(&c.id, f).unwrap();
        assert_eq!(s.load_chunks(&c.id, None).unwrap().len(), 1);
        assert_eq!(s.files(&c.id).unwrap()[0].name, "notes.md");
    }

    #[test]
    fn conversations_link_to_one_collection() {
        let s = ConversationStore::open_in_memory_with(key(2)).unwrap();
        let conv = s.create("").unwrap();
        let a = s.create_collection("A").unwrap();
        let b = s.create_collection("B").unwrap();
        s.link_collection(&conv.id, Some(&a.id)).unwrap();
        s.link_collection(&conv.id, Some(&b.id)).unwrap();
        assert_eq!(s.linked_collection(&conv.id).unwrap(), Some(b.id.clone()));
        s.delete_collection(&b.id).unwrap();
        assert_eq!(s.linked_collection(&conv.id).unwrap(), None);
        s.link_collection(&conv.id, Some(&a.id)).unwrap();
        s.link_collection(&conv.id, None).unwrap();
        assert_eq!(s.linked_collection(&conv.id).unwrap(), None);
    }

    #[test]
    fn document_text_never_reaches_the_disk_in_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let marker = "DOC-CANARY-77aa";
        let mut s = ConversationStore::open_with(&path, Some(key(3))).unwrap();
        let c = s.create_collection(marker).unwrap();
        s.add_file(&c.id, marker, 1, &[format!("the {marker} passage")], None)
            .unwrap();
        let on_disk = |m: &str| {
            std::fs::read_dir(dir.path()).unwrap().any(|e| {
                let b = std::fs::read(e.unwrap().path()).unwrap_or_default();
                b.windows(m.len()).any(|w| w == m.as_bytes())
            })
        };
        assert!(!on_disk(marker));
        s.delete_collection(&c.id).unwrap();
        drop(s);
        assert!(!on_disk("DOC-CANARY"));
    }

    #[test]
    fn version_2_databases_gain_the_document_and_image_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let conv = {
            let s = ConversationStore::open_with(&path, Some(key(5))).unwrap();
            let conv = s.create("").unwrap();
            s.conn
                .execute_batch(&format!(
                    "DROP TABLE agent_images; DROP TABLE chat_options; DROP TABLE memories;
                     DROP TABLE message_images; DROP TABLE doc_links; DROP TABLE doc_chunks;
                     DROP TABLE doc_files; DROP TABLE doc_collections;
                     DROP TABLE agent_entries; DROP TABLE agent_transcripts; DROP TABLE agent_projects; {}
                     PRAGMA user_version = 2;",
                    super::super::AGENT_TABLES
                ))
                .unwrap();
            conv
        };
        let s = ConversationStore::open_with(&path, Some(key(5))).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, super::super::SCHEMA_VERSION);
        let c = s.create_collection("after upgrade").unwrap();
        s.link_collection(&conv.id, Some(&c.id)).unwrap();
        assert_eq!(s.get(&conv.id).unwrap().summary.id, conv.id);
    }

    #[test]
    fn sources_are_sealed_and_files_can_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let mut s = ConversationStore::open_with(&path, Some(key(6))).unwrap();
        let c = s.create_collection("Docs").unwrap();
        let src = FileSource {
            path: r"C:\Users\me\SRC-CANARY-91\plan.md".into(),
            mtime: 100,
            sha256: "HASH-CANARY-91".into(),
        };
        let f = s
            .add_file(
                &c.id,
                "plan.md",
                5,
                &["old one".into(), "old two".into()],
                Some(&src),
            )
            .unwrap();
        s.add_file(&c.id, "legacy.md", 1, &["legacy".into()], None)
            .unwrap();
        let ids: Vec<i64> = s
            .load_chunks(&c.id, None)
            .unwrap()
            .iter()
            .map(|k| k.id)
            .collect();
        s.set_embeddings(&c.id, "m", &[(ids[0], vec![1.0])])
            .unwrap();

        let files = s.files(&c.id).unwrap();
        assert!(files[0].has_source && !files[1].has_source);
        let recs = s.sources(&c.id).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(
            (recs[0].file_id, recs[0].size, &recs[0].source),
            (f, 5, &src)
        );

        s.touch_source(&c.id, f, 200).unwrap();
        assert_eq!(s.sources(&c.id).unwrap()[0].source.mtime, 200);

        let newer = FileSource {
            mtime: 300,
            sha256: "new-hash".into(),
            ..src.clone()
        };
        s.replace_file(&c.id, f, 9, &["new passage".into()], &newer)
            .unwrap();
        let chunks = s.load_chunks(&c.id, Some("m")).unwrap();
        let texts: Vec<&str> = chunks.iter().map(|k| k.text.as_str()).collect();
        assert_eq!(texts, ["legacy", "new passage"]);
        // The old passage's embedding went with it; the new one needs one.
        assert!(chunks.iter().all(|k| k.embedding.is_none()));
        let files = s.files(&c.id).unwrap();
        assert_eq!(
            (
                files[0].id,
                files[0].name.as_str(),
                files[0].size,
                files[0].chunks
            ),
            (f, "plan.md", 9, 1)
        );
        assert_eq!(s.sources(&c.id).unwrap()[0].source, newer);
        assert!(matches!(
            s.replace_file(&c.id, 9999, 1, &["x".into()], &newer),
            Err(StoreError::NotFound)
        ));

        drop(s);
        let on_disk = |m: &str| {
            std::fs::read_dir(dir.path()).unwrap().any(|e| {
                let b = std::fs::read(e.unwrap().path()).unwrap_or_default();
                b.windows(m.len()).any(|w| w == m.as_bytes())
            })
        };
        assert!(!on_disk("SRC-CANARY-91") && !on_disk("HASH-CANARY-91"));
    }

    #[test]
    fn version_5_databases_gain_document_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let (c, f) = {
            let s = ConversationStore::open_with(&path, Some(key(7))).unwrap();
            s.conn
                .execute_batch(&format!(
                    "DROP TABLE agent_images; DROP TABLE doc_links; DROP TABLE doc_chunks; DROP TABLE doc_files;
                     DROP TABLE doc_collections; DROP TABLE agent_entries; DROP TABLE agent_transcripts; DROP TABLE agent_projects;
                     {} {} PRAGMA user_version = 5;",
                    super::super::DOC_TABLES,
                    super::super::AGENT_TABLES
                ))
                .unwrap();
            let c = s.create_collection("old").unwrap();
            s.conn
                .execute(
                    "INSERT INTO doc_files (collection_id, name, size, chunks, added_at)
                     VALUES (?1, 'a.md', 1, 0, 0)",
                    [&c.id],
                )
                .unwrap();
            let f = s.conn.last_insert_rowid();
            (c, f)
        };
        let s = ConversationStore::open_with(&path, Some(key(7))).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, super::super::SCHEMA_VERSION);
        let files = s.files(&c.id).unwrap();
        assert_eq!((files[0].id, files[0].has_source), (f, false));
        assert!(s.sources(&c.id).unwrap().is_empty());
    }

    #[test]
    fn vectors_round_trip_and_bad_input_is_refused() {
        let v = vec![1.0f32, -0.0, 3.5e-8, f32::MAX];
        assert_eq!(decode_vector(&encode_vector(&v)), Some(v));
        assert_eq!(decode_vector("abc"), None);
        assert_eq!(decode_vector("zzzzzzzz"), None);
        let mut s = ConversationStore::open_in_memory().unwrap();
        let c = s.create_collection("x").unwrap();
        assert!(
            s.add_file(&c.id, "big", 0, &["x".repeat(MAX_CHUNK_BYTES + 1)], None)
                .is_err()
        );
        assert!(
            s.set_embeddings(&c.id, "m", &[(1, vec![f32::NAN])])
                .is_err()
        );
        assert!(matches!(
            s.create_collection("y").and_then(|_| s.files("nope")),
            Err(StoreError::InvalidId)
        ));
    }
}
