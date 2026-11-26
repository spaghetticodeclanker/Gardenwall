//! Visited-sites lists (web research, ADR-018): what each turn that used
//! the web searched and fetched, as recorded by the core.
//!
//! One row per turn, owned by an agent transcript or a chat and deleted with
//! it. The list is JSON the app defines; here it is sealed with the chat key
//! and bound to its owner, like messages.

use rusqlite::params;

use super::{ConversationStore, Result, StoreError, now};

/// Largest stored list for one turn (JSON).
pub const MAX_VISITS_BYTES: usize = 1 << 20;

/// Version 11 (idempotent: a database rebuilt from parts of a newer one may
/// already have it).
pub(crate) const VISIT_TABLES: &str = "CREATE TABLE IF NOT EXISTS web_visits (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         transcript_id TEXT REFERENCES agent_transcripts(id) ON DELETE CASCADE,
         conversation_id TEXT REFERENCES conversations(id) ON DELETE CASCADE,
         payload TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         CHECK ((transcript_id IS NULL) <> (conversation_id IS NULL))
     ) STRICT;
     CREATE INDEX IF NOT EXISTS web_visits_by_transcript ON web_visits(transcript_id, id);
     CREATE INDEX IF NOT EXISTS web_visits_by_conversation ON web_visits(conversation_id, id);";

/// Who a visited list belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitOwner<'a> {
    Agent(&'a str),
    Chat(&'a str),
}

impl VisitOwner<'_> {
    fn aad(&self) -> String {
        match self {
            Self::Agent(id) => format!("gardenwall-visits:agent:{id}"),
            Self::Chat(id) => format!("gardenwall-visits:chat:{id}"),
        }
    }

    fn column(&self) -> &'static str {
        match self {
            Self::Agent(_) => "transcript_id",
            Self::Chat(_) => "conversation_id",
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Agent(id) | Self::Chat(id) => id,
        }
    }
}

impl ConversationStore {
    /// Save one turn's visited list (JSON) for `owner`.
    pub fn add_web_visits(&self, owner: VisitOwner<'_>, json: &str) -> Result<()> {
        if json.len() > MAX_VISITS_BYTES {
            return Err(StoreError::TooLarge("visited list"));
        }
        let payload = match &self.cipher {
            Some(c) => c.seal(&owner.aad(), json).map_err(StoreError::Crypto)?,
            None if self.locked => return Err(StoreError::Locked),
            None => json.to_string(),
        };
        self.conn.execute(
            &format!(
                "INSERT INTO web_visits ({}, payload, created_at) VALUES (?1, ?2, ?3)",
                owner.column()
            ),
            params![owner.id(), payload, now()],
        )?;
        Ok(())
    }

    /// Every saved list for `owner`, oldest first, as `(saved at, JSON)`.
    /// Lists that can't be decrypted are left out.
    pub fn web_visits(&self, owner: VisitOwner<'_>) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT created_at, payload FROM web_visits WHERE {} = ?1 ORDER BY id",
            owner.column()
        ))?;
        let rows = stmt
            .query_map([owner.id()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let aad = owner.aad();
        Ok(rows
            .into_iter()
            .filter_map(|(at, stored)| {
                if !crate::crypto::FieldCipher::is_sealed(&stored) {
                    return Some((at, stored));
                }
                let open = self.cipher.as_ref()?.open(&aad, &stored).ok()?;
                Some((at, open))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::FieldCipher;

    fn key(byte: u8) -> FieldCipher {
        FieldCipher::new(&zeroize::Zeroizing::new([byte; 32]))
    }

    #[test]
    fn lists_are_sealed_bound_to_their_owner_and_deleted_with_it() {
        let s = ConversationStore::open_in_memory_with(key(21)).unwrap();
        let t = s.create_transcript(&["C:/p".into()]).unwrap();
        let chat = s.create("").unwrap();
        let json = r#"[{"kind":"page","target":"https://docs.rs/tokio"}]"#;
        s.add_web_visits(VisitOwner::Agent(&t.id), json).unwrap();
        s.add_web_visits(VisitOwner::Chat(&chat.id), "[]").unwrap();
        let stored: String = s
            .conn
            .query_row(
                "SELECT payload FROM web_visits WHERE transcript_id = ?1",
                [&t.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!stored.contains("docs.rs"), "stored in the clear");
        assert_eq!(s.web_visits(VisitOwner::Agent(&t.id)).unwrap()[0].1, json);
        // Moved to another owner, it no longer opens.
        s.conn
            .execute("UPDATE web_visits SET conversation_id = ?1, transcript_id = NULL WHERE transcript_id = ?2", [&chat.id, &t.id])
            .unwrap();
        assert_eq!(s.web_visits(VisitOwner::Chat(&chat.id)).unwrap().len(), 1);
        s.delete(&chat.id).unwrap();
        let left: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM web_visits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        assert!(
            s.add_web_visits(VisitOwner::Agent(&t.id), &"x".repeat(MAX_VISITS_BYTES + 1))
                .is_err()
        );
    }
}
