//! Text search over saved chats.
//!
//! Chats are encrypted field by field, so there is no index: a search
//! decrypts each message in memory and looks for the text there. Nothing
//! readable is written to disk for it. (Agent sessions are searched the
//! same way, in `transcripts.rs`.)

use serde::Serialize;

use super::{ConversationStore, Result};

/// Characters shown around a match.
const SNIPPET_CONTEXT: usize = 60;

/// One place the text was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// The chat or agent session.
    pub id: String,
    pub title: String,
    /// The text around the first match, on one line.
    pub snippet: String,
    pub updated_at: i64,
}

/// The text around the first place `needle` (already lowercase) occurs in
/// `text`, ignoring case, with whitespace collapsed; `None` if it doesn't.
pub fn snippet(text: &str, needle: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase().next()).collect();
    let needle: Vec<char> = needle.chars().collect();
    if lower.len() != chars.len() || needle.len() > lower.len() {
        return None;
    }
    let at = lower
        .windows(needle.len())
        .position(|w| w == needle.as_slice())?;
    let start = at.saturating_sub(SNIPPET_CONTEXT);
    let end = (at + needle.len() + SNIPPET_CONTEXT).min(chars.len());
    let body: String = chars[start..end].iter().collect();
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!(
        "{}{body}{}",
        if start > 0 { "…" } else { "" },
        if end < chars.len() { "…" } else { "" }
    ))
}

impl ConversationStore {
    /// Chats whose title or messages contain `query` (case ignored), newest
    /// first, one hit per chat, at most `limit`.
    pub fn search_chats(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for chat in self.list()? {
            if hits.len() >= limit {
                break;
            }
            let mut found = snippet(&chat.title, &needle);
            if found.is_none() {
                let mut stmt = self.conn.prepare(
                    "SELECT content FROM messages WHERE conversation_id = ?1 ORDER BY id",
                )?;
                let rows = stmt.query_map([&chat.id], |r| r.get::<_, String>(0))?;
                for stored in rows {
                    let text = self.unseal(&chat.id, "content", stored?);
                    found = snippet(&text, &needle);
                    if found.is_some() {
                        break;
                    }
                }
            }
            if let Some(snippet) = found {
                hits.push(SearchHit {
                    id: chat.id,
                    title: chat.title,
                    snippet,
                    updated_at: chat.updated_at,
                });
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::{MessageRole, NewMessage};
    use crate::crypto::FieldCipher;

    #[test]
    fn snippets_ignore_case_and_stay_short() {
        assert_eq!(
            snippet("Hello World", "world").as_deref(),
            Some("Hello World")
        );
        assert_eq!(
            snippet("line one\n\n  line   two", "two").as_deref(),
            Some("line one line two")
        );
        let long = format!("{} needle {}", "a".repeat(200), "b".repeat(200));
        let s = snippet(&long, "needle").unwrap();
        assert!(
            s.starts_with('…') && s.ends_with('…') && s.chars().count() < 140,
            "{s}"
        );
        assert_eq!(snippet("nothing here", "absent"), None);
        assert_eq!(snippet("x", ""), None);
    }

    #[test]
    fn encrypted_chats_are_found_by_title_and_message() {
        let key = FieldCipher::new(&zeroize::Zeroizing::new([3; 32]));
        let mut s = ConversationStore::open_in_memory_with(key).unwrap();
        let a = s.create("").unwrap();
        s.append(
            &a.id,
            NewMessage {
                role: MessageRole::User,
                content: "How do I rotate the Postgres password?".into(),
                reasoning: String::new(),
                stats: None,
            },
        )
        .unwrap();
        let b = s.create("").unwrap();
        s.rename(&b.id, "Holiday plans").unwrap();

        let hits = s.search_chats("POSTGRES", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.id);
        assert!(hits[0].snippet.contains("Postgres password"));
        assert_eq!(s.search_chats("holiday", 10).unwrap()[0].id, b.id);
        assert!(s.search_chats("nowhere", 10).unwrap().is_empty());
        assert!(s.search_chats("o", 1).unwrap().len() <= 1);
    }
}
