//! Unsent message text, kept so it survives closing the app.
//!
//! One row per place a message can be typed (a chat, a new chat, an agent
//! session's transcript), named by a short key the app chooses. The text is
//! the user's own; it is sealed with the chat key and bound to its key like
//! every other stored text. Old drafts are dropped, so nothing typed and
//! abandoned stays on disk for long.

use rusqlite::params;

use super::{ConversationStore, Result, StoreError, now};

/// Largest draft kept (longer text is simply not saved).
pub const MAX_DRAFT_BYTES: usize = 64 * 1024;
/// Drafts older than this are removed.
const MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;
/// Most drafts kept; the oldest go first.
const MAX_DRAFTS: i64 = 200;

/// Version 12 (idempotent, like the visited-sites table).
pub(crate) const DRAFT_TABLES: &str = "CREATE TABLE IF NOT EXISTS drafts (
         owner TEXT PRIMARY KEY,
         body TEXT NOT NULL,
         updated_at INTEGER NOT NULL
     ) STRICT;";

fn aad(owner: &str) -> String {
    format!("gardenwall-draft:{owner}")
}

/// A draft's key: 1 to 128 characters of `a-z`, `0-9`, `:` and `-`.
fn checked(owner: &str) -> Result<&str> {
    let ok = (1..=128).contains(&owner.len())
        && owner
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'-'));
    if ok {
        Ok(owner)
    } else {
        Err(StoreError::InvalidId)
    }
}

impl ConversationStore {
    /// Keep `text` as the draft for `owner`; empty text removes it.
    pub fn set_draft(&self, owner: &str, text: &str) -> Result<()> {
        let owner = checked(owner)?;
        if text.trim().is_empty() {
            self.conn
                .execute("DELETE FROM drafts WHERE owner = ?1", [owner])?;
            return Ok(());
        }
        if text.len() > MAX_DRAFT_BYTES {
            return Err(StoreError::TooLarge("draft"));
        }
        let body = match &self.cipher {
            Some(c) => c.seal(&aad(owner), text).map_err(StoreError::Crypto)?,
            None if self.locked => return Err(StoreError::Locked),
            None => text.to_string(),
        };
        self.conn.execute(
            "INSERT INTO drafts (owner, body, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
            params![owner, body, now()],
        )?;
        self.conn.execute(
            "DELETE FROM drafts WHERE updated_at < ?1
                OR owner IN (SELECT owner FROM drafts ORDER BY updated_at DESC, owner LIMIT -1 OFFSET ?2)",
            params![now() - MAX_AGE_SECONDS, MAX_DRAFTS],
        )?;
        Ok(())
    }

    /// The draft for `owner`, or `None` (also when it can't be decrypted).
    pub fn draft(&self, owner: &str) -> Result<Option<String>> {
        let owner = checked(owner)?;
        let stored: Option<String> = self
            .conn
            .query_row("SELECT body FROM drafts WHERE owner = ?1", [owner], |r| {
                r.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(stored.and_then(|s| {
            if !crate::crypto::FieldCipher::is_sealed(&s) {
                return Some(s);
            }
            self.cipher.as_ref()?.open(&aad(owner), &s).ok()
        }))
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
    fn drafts_are_sealed_bound_to_their_key_and_removable() {
        let s = ConversationStore::open_in_memory_with(key(7)).unwrap();
        assert_eq!(s.draft("chat:new").unwrap(), None);
        s.set_draft("chat:new", "half a thought").unwrap();
        s.set_draft("chat:new", "a whole thought").unwrap();
        assert_eq!(
            s.draft("chat:new").unwrap().as_deref(),
            Some("a whole thought")
        );

        // Stored sealed, and a value moved to another key doesn't open.
        let stored: String = s
            .conn
            .query_row(
                "SELECT body FROM drafts WHERE owner = 'chat:new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(FieldCipher::is_sealed(&stored) && !stored.contains("thought"));
        s.conn
            .execute(
                "INSERT INTO drafts (owner, body, updated_at) VALUES ('chat:other', ?1, ?2)",
                params![stored, now()],
            )
            .unwrap();
        assert_eq!(s.draft("chat:other").unwrap(), None);

        // Empty text removes it; bad keys and oversized text are refused.
        s.set_draft("chat:new", "  ").unwrap();
        assert_eq!(s.draft("chat:new").unwrap(), None);
        for bad in ["", "Chat:new", "chat new", "../x", &"a".repeat(129)] {
            assert!(s.set_draft(bad, "x").is_err(), "{bad}");
        }
        assert!(
            s.set_draft("chat:new", &"x".repeat(MAX_DRAFT_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn old_and_surplus_drafts_are_dropped() {
        let s = ConversationStore::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO drafts (owner, body, updated_at) VALUES ('chat:stale', 'x', ?1)",
                [now() - MAX_AGE_SECONDS - 1],
            )
            .unwrap();
        for i in 0..MAX_DRAFTS + 5 {
            s.conn
                .execute(
                    "INSERT INTO drafts (owner, body, updated_at) VALUES (?1, 'x', ?2)",
                    params![format!("chat:{i}"), now() - 1000 + i],
                )
                .unwrap();
        }
        s.set_draft("chat:newest", "keep").unwrap();
        let count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM drafts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MAX_DRAFTS);
        assert_eq!(s.draft("chat:stale").unwrap(), None);
        assert_eq!(s.draft("chat:0").unwrap(), None);
        assert_eq!(s.draft("chat:newest").unwrap().as_deref(), Some("keep"));
    }
}
