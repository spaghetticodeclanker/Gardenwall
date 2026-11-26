//! Memories (ROADMAP Phase 5): short notes the
//! user keeps for the assistant, personal or tied to an agent folder.
//!
//! Kept in `conversations.db` and protected like chats: the text and the
//! project folder are sealed with the chat key and bound to the memory's id;
//! deletions are overwritten and the log is scrubbed. Memories the model
//! proposes are stored as *pending* and are never used until the user
//! accepts them.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ConversationStore, Result, StoreError, now, parse_id};

/// Longest memory (characters).
pub const MAX_MEMORY_CHARS: usize = 1000;
/// Most memories kept.
pub const MAX_MEMORIES: usize = 500;
/// Most pending suggestions waiting for review, so a model can't flood the
/// list.
pub const MAX_PENDING: usize = 30;

fn aad(id: &str, field: &str) -> String {
    format!("gardenwall-memory:{id}:{field}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MemoryScope {
    /// Applies everywhere.
    Personal,
    /// Applies when this agent folder is granted.
    Project { folder: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemorySource {
    User,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub scope: MemoryScope,
    pub text: String,
    /// Waiting for the user's review (proposed by the model).
    pub pending: bool,
    pub source: MemorySource,
    pub in_chat: bool,
    pub in_agent: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

fn clean(text: &str) -> Result<String> {
    let t: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    if t.is_empty() {
        return Err(StoreError::Empty("memory"));
    }
    if t.chars().count() > MAX_MEMORY_CHARS {
        return Err(StoreError::TooLarge("memory"));
    }
    Ok(t)
}

impl ConversationStore {
    fn seal_memory(&self, id: &str, field: &str, text: &str) -> Result<String> {
        match &self.cipher {
            Some(c) => c.seal(&aad(id, field), text).map_err(StoreError::Crypto),
            None if self.locked => Err(StoreError::Locked),
            None => Ok(text.to_string()),
        }
    }

    fn unseal_memory(&self, id: &str, field: &str, stored: String) -> Option<String> {
        if !crate::crypto::FieldCipher::is_sealed(&stored) {
            return Some(stored);
        }
        self.cipher
            .as_ref()
            .and_then(|c| c.open(&aad(id, field), &stored).ok())
    }

    /// Every memory, newest first. Memories that can't be decrypted (for
    /// example after a key change) are left out.
    pub fn list_memories(&self) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, scope, project, text, status, source, in_chat, in_agent, created_at, updated_at
             FROM memories ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(
                |(id, scope, project, text, status, source, chat, agent, c, u)| {
                    let text = self.unseal_memory(&id, "text", text)?;
                    let scope = if scope == "project" {
                        MemoryScope::Project {
                            folder: self.unseal_memory(&id, "project", project)?,
                        }
                    } else {
                        MemoryScope::Personal
                    };
                    Some(Memory {
                        id,
                        scope,
                        text,
                        pending: status == "pending",
                        source: if source == "model" {
                            MemorySource::Model
                        } else {
                            MemorySource::User
                        },
                        in_chat: chat != 0,
                        in_agent: agent != 0,
                        created_at: c,
                        updated_at: u,
                    })
                },
            )
            .collect())
    }

    /// Put memories read from another store into this one as they were
    /// (same ids, review state and dates), sealed with this store's key. A
    /// restore from a backup without memories uses it to keep the ones on
    /// this computer. Memories with an id already here are left alone.
    pub fn keep_memories(&self, memories: &[Memory]) -> Result<usize> {
        let mut kept = 0;
        for m in memories.iter().take(MAX_MEMORIES) {
            let (scope_name, folder) = match &m.scope {
                MemoryScope::Personal => ("personal", String::new()),
                MemoryScope::Project { folder } => {
                    ("project", self.seal_memory(&m.id, "project", folder)?)
                }
            };
            let sealed = self.seal_memory(&m.id, "text", &m.text)?;
            kept += self.conn.execute(
                "INSERT OR IGNORE INTO memories (id, scope, project, text, status, source, in_chat, in_agent, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    m.id,
                    scope_name,
                    folder,
                    sealed,
                    if m.pending { "pending" } else { "active" },
                    if m.source == MemorySource::Model { "model" } else { "user" },
                    m.in_chat as i64,
                    m.in_agent as i64,
                    m.created_at,
                    m.updated_at
                ],
            )?;
        }
        Ok(kept)
    }

    /// Add a memory. The user's are active at once; the model's are pending
    /// until the user accepts them.
    pub fn add_memory(
        &self,
        scope: &MemoryScope,
        text: &str,
        source: MemorySource,
    ) -> Result<Memory> {
        let text = clean(text)?;
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        if total as usize >= MAX_MEMORIES {
            return Err(StoreError::TooLarge("memory list"));
        }
        let pending = source == MemorySource::Model;
        if pending {
            let waiting: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )?;
            if waiting as usize >= MAX_PENDING {
                return Err(StoreError::TooLarge("pending memory suggestions"));
            }
        }
        let id = Uuid::new_v4().hyphenated().to_string();
        let (scope_name, folder) = match scope {
            MemoryScope::Personal => ("personal", String::new()),
            MemoryScope::Project { folder } => {
                ("project", self.seal_memory(&id, "project", folder)?)
            }
        };
        let sealed = self.seal_memory(&id, "text", &text)?;
        let t = now();
        self.conn.execute(
            "INSERT INTO memories (id, scope, project, text, status, source, in_chat, in_agent, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, ?7, ?7)",
            params![
                id,
                scope_name,
                folder,
                sealed,
                if pending { "pending" } else { "active" },
                if pending { "model" } else { "user" },
                t
            ],
        )?;
        Ok(Memory {
            id,
            scope: scope.clone(),
            text,
            pending,
            source,
            in_chat: true,
            in_agent: true,
            created_at: t,
            updated_at: t,
        })
    }

    /// Change a memory's text and switches. Saving a pending memory accepts
    /// it: the user has now reviewed it.
    pub fn update_memory(&self, id: &str, text: &str, in_chat: bool, in_agent: bool) -> Result<()> {
        let id = parse_id(id)?;
        let text = clean(text)?;
        let sealed = self.seal_memory(&id, "text", &text)?;
        let changed = self.conn.execute(
            "UPDATE memories SET text = ?2, in_chat = ?3, in_agent = ?4, status = 'active', updated_at = ?5
             WHERE id = ?1",
            params![id, sealed, in_chat as i64, in_agent as i64, now()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn delete_memory(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let changed = self
            .conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.scrub_log()
    }

    /// Whether a chat uses memories (on unless turned off).
    pub fn chat_uses_memories(&self, conversation: &str) -> Result<bool> {
        let id = parse_id(conversation)?;
        Ok(self
            .conn
            .query_row(
                "SELECT use_memories FROM chat_options WHERE conversation_id = ?1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_none_or(|v| v != 0))
    }

    pub fn set_chat_uses_memories(&self, conversation: &str, on: bool) -> Result<()> {
        let id = parse_id(conversation)?;
        self.conn.execute(
            "INSERT INTO chat_options (conversation_id, use_memories) VALUES (?1, ?2)
             ON CONFLICT(conversation_id) DO UPDATE SET use_memories = excluded.use_memories",
            params![id, on as i64],
        )?;
        Ok(())
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
    fn memories_round_trip_and_the_model_only_suggests() {
        let s = ConversationStore::open_in_memory_with(key(1)).unwrap();
        let mine = s
            .add_memory(
                &MemoryScope::Personal,
                "  I prefer metric units. ",
                MemorySource::User,
            )
            .unwrap();
        assert_eq!(mine.text, "I prefer metric units.");
        assert!(!mine.pending);
        let project = MemoryScope::Project {
            folder: r"C:\Projects\demo".into(),
        };
        let suggested = s
            .add_memory(&project, "Tests run with cargo test.", MemorySource::Model)
            .unwrap();
        assert!(suggested.pending);
        let all = s.list_memories().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|m| m.scope == project && m.pending));

        // Saving a suggestion accepts it; switches are kept per memory.
        s.update_memory(&suggested.id, "Tests: cargo test --workspace", true, false)
            .unwrap();
        let m = s
            .list_memories()
            .unwrap()
            .into_iter()
            .find(|m| m.id == suggested.id)
            .unwrap();
        assert!(!m.pending && m.in_chat && !m.in_agent);
        assert_eq!(m.text, "Tests: cargo test --workspace");

        s.delete_memory(&mine.id).unwrap();
        assert_eq!(s.list_memories().unwrap().len(), 1);
        assert!(matches!(
            s.delete_memory(&mine.id),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn memory_text_is_bounded_and_suggestions_are_capped() {
        let s = ConversationStore::open_in_memory_with(key(2)).unwrap();
        assert!(
            s.add_memory(&MemoryScope::Personal, "   ", MemorySource::User)
                .is_err()
        );
        let long = "x".repeat(MAX_MEMORY_CHARS + 1);
        assert!(
            s.add_memory(&MemoryScope::Personal, &long, MemorySource::User)
                .is_err()
        );
        for i in 0..MAX_PENDING {
            s.add_memory(
                &MemoryScope::Personal,
                &format!("note {i}"),
                MemorySource::Model,
            )
            .unwrap();
        }
        // A model can't flood the review list; the user still can add.
        assert!(
            s.add_memory(&MemoryScope::Personal, "one more", MemorySource::Model)
                .is_err()
        );
        assert!(
            s.add_memory(&MemoryScope::Personal, "mine", MemorySource::User)
                .is_ok()
        );
    }

    #[test]
    fn memories_are_encrypted_on_disk_and_chats_can_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let s = ConversationStore::open_with(&path, Some(key(3))).unwrap();
        let c = s.create("").unwrap();
        assert!(s.chat_uses_memories(&c.id).unwrap());
        s.set_chat_uses_memories(&c.id, false).unwrap();
        assert!(!s.chat_uses_memories(&c.id).unwrap());
        s.add_memory(
            &MemoryScope::Project {
                folder: r"C:\FOLDER-CANARY".into(),
            },
            "MEMORY-CANARY likes tea",
            MemorySource::User,
        )
        .unwrap();
        drop(s);
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let b = std::fs::read(e.unwrap().path()).unwrap_or_default();
            for marker in ["MEMORY-CANARY", "FOLDER-CANARY"] {
                assert!(
                    !b.windows(marker.len()).any(|w| w == marker.as_bytes()),
                    "{marker}"
                );
            }
        }
        // Another key can't read them; they're left out, not shown garbled.
        let other = ConversationStore::open_with(&path, Some(key(4)));
        if let Ok(other) = other {
            assert!(other.list_memories().unwrap_or_default().is_empty());
        }
    }
}
