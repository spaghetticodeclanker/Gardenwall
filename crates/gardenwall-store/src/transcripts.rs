//! Saved agent transcripts (ADR-014 §2.1 "Sessions").
//!
//! Kept in `conversations.db` beside chats and protected the same way:
//! every text field is sealed with the chat key, deleted rows are
//! overwritten, and the log is scrubbed after deletions. Values are bound to
//! their transcript and field with associated data distinct from chats', so
//! they can't be moved between transcripts or into a chat.
//!
//! The store keeps each entry as an opaque JSON payload written by the
//! trusted core: a context message, or a compaction record. Everything in a
//! payload may be untrusted model output and is never interpreted here.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use super::{ConversationStore, MAX_MESSAGE_BYTES, Result, StoreError, clean_title, now, parse_id};

/// Title of a transcript until its first request names it.
pub const DEFAULT_AGENT_TITLE: &str = "New agent session";
/// Largest entry payload (a message with reasoning and tool calls).
pub const MAX_ENTRY_BYTES: usize = 2 * MAX_MESSAGE_BYTES + 64 * 1024;
/// Most folders recorded for display.
const MAX_FOLDERS: usize = 64;
const MAX_FOLDER_BYTES: usize = 32 * 1024;

/// A transcript row before decryption.
type SummaryRow = (
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
    i64,
    Option<String>,
);

fn summary_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

pub(super) fn aad(transcript: &str, field: &str) -> String {
    format!("gardenwall-agent:{transcript}:{field}")
}

/// The session a handoff continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffParent<'a> {
    /// A saved agent transcript.
    Agent(&'a str),
    /// A chat.
    Chat(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A message added to the agent's context.
    Message,
    /// A compaction: messages before the recorded sequence number were
    /// replaced by the recorded summary.
    Compaction,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Compaction => "compaction",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "message" => Some(Self::Message),
            "compaction" => Some(Self::Compaction),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSummary {
    pub id: String,
    pub title: String,
    /// The folders the session was granted, for display only. Resuming
    /// always grants folders again through the native picker.
    pub folders: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// The session this one was forked or cloned from (v8), if it still
    /// exists.
    pub parent_id: Option<String>,
    /// Entries before this sequence number were copied from the parent; the
    /// end of the parent for a clone.
    pub fork_seq: Option<u64>,
    /// The project this session belongs to (v9), if any.
    pub project_id: Option<String>,
    /// Started from a reviewed handoff (v10); its parent is `parent_id`
    /// (an agent session) or `parent_chat_id` (a chat).
    pub handoff: bool,
    /// The chat this session continues from a handoff (v10), if it still
    /// exists.
    pub parent_chat_id: Option<String>,
}

/// Most entries copied or imported into one transcript.
pub const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    pub seq: u64,
    pub kind: EntryKind,
    /// JSON written by the trusted core.
    pub payload: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    #[serde(flatten)]
    pub summary: TranscriptSummary,
    pub entries: Vec<TranscriptEntry>,
}

impl ConversationStore {
    fn seal_agent(&self, id: &str, field: &str, text: &str) -> Result<String> {
        match &self.cipher {
            Some(c) => c.seal(&aad(id, field), text).map_err(StoreError::Crypto),
            None if self.locked => Err(StoreError::Locked),
            None => Ok(text.to_string()),
        }
    }

    fn unseal_agent(&self, id: &str, field: &str, stored: String) -> Option<String> {
        if !crate::crypto::FieldCipher::is_sealed(&stored) {
            return Some(stored);
        }
        self.cipher
            .as_ref()
            .and_then(|c| c.open(&aad(id, field), &stored).ok())
    }

    fn summary_from_row(
        &self,
        (
            id,
            title,
            folders,
            created,
            updated,
            parent_id,
            fork_seq,
            project_id,
            handoff,
            parent_chat_id,
        ): SummaryRow,
    ) -> TranscriptSummary {
        let title = self
            .unseal_agent(&id, "title", title)
            .unwrap_or_else(|| super::UNREADABLE.to_string());
        let folders = self
            .unseal_agent(&id, "folders", folders)
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default();
        TranscriptSummary {
            id,
            title,
            folders,
            created_at: created,
            updated_at: updated,
            parent_id,
            fork_seq: fork_seq.map(|s| s as u64),
            project_id,
            handoff: handoff != 0,
            parent_chat_id,
        }
    }

    pub fn create_transcript(&self, folders: &[String]) -> Result<TranscriptSummary> {
        let folders: Vec<String> = folders.iter().take(MAX_FOLDERS).cloned().collect();
        let folders_json = serde_json::to_string(&folders).unwrap_or_else(|_| "[]".into());
        if folders_json.len() > MAX_FOLDER_BYTES {
            return Err(StoreError::TooLarge("folder list"));
        }
        let id = Uuid::new_v4().hyphenated().to_string();
        let t = now();
        let title = self.seal_agent(&id, "title", DEFAULT_AGENT_TITLE)?;
        let sealed_folders = self.seal_agent(&id, "folders", &folders_json)?;
        self.conn.execute(
            "INSERT INTO agent_transcripts (id, title, folders, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, sealed_folders, t],
        )?;
        Ok(TranscriptSummary {
            id,
            title: DEFAULT_AGENT_TITLE.into(),
            folders,
            created_at: t,
            updated_at: t,
            parent_id: None,
            fork_seq: None,
            project_id: None,
            handoff: false,
            parent_chat_id: None,
        })
    }

    /// Most recently updated first.
    pub fn list_transcripts(&self) -> Result<Vec<TranscriptSummary>> {
        let rows: Vec<SummaryRow> = self
            .conn
            .prepare(
                "SELECT id, title, folders, created_at, updated_at, parent_id, fork_seq, project_id, handoff, parent_chat_id
                 FROM agent_transcripts ORDER BY updated_at DESC, rowid DESC",
            )?
            .query_map([], summary_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|row| self.summary_from_row(row))
            .collect())
    }

    /// Put a session in a project, or take it out (`None`).
    pub fn set_transcript_project(&self, id: &str, project: Option<&str>) -> Result<()> {
        let id = parse_id(id)?;
        let project = project.map(parse_id).transpose()?;
        let changed = self.conn.execute(
            "UPDATE agent_transcripts SET project_id = ?1 WHERE id = ?2",
            params![project, id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Mark a new transcript as started from a handoff of `parent`: an
    /// agent session or a chat. A parent that
    /// no longer exists is an error, so the link is never dangling.
    pub fn set_transcript_handoff(&self, id: &str, parent: HandoffParent<'_>) -> Result<()> {
        let id = parse_id(id)?;
        let (agent, chat) = match parent {
            HandoffParent::Agent(p) => (Some(parse_id(p)?), None),
            HandoffParent::Chat(p) => (None, Some(parse_id(p)?)),
        };
        let changed = self.conn.execute(
            "UPDATE agent_transcripts SET handoff = 1, parent_id = ?1, fork_seq = NULL,
                 parent_chat_id = ?2 WHERE id = ?3",
            params![agent, chat, id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn rename_transcript(&self, id: &str, title: &str) -> Result<()> {
        let id = parse_id(id)?;
        let title = self.seal_agent(&id, "title", &clean_title(title))?;
        let changed = self.conn.execute(
            "UPDATE agent_transcripts SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Append an entry. `seq` comes from the agent and orders the transcript.
    /// Remove the entries (and their images) from `from_seq` on: a request
    /// the user stopped before anything ran is withdrawn.
    pub fn remove_entries_from(&mut self, id: &str, from_seq: u64) -> Result<()> {
        let id = parse_id(id)?;
        let seq = i64::try_from(from_seq).map_err(|_| StoreError::TooLarge("sequence number"))?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM agent_entries WHERE transcript_id = ?1 AND seq >= ?2",
            params![id, seq],
        )?;
        tx.execute(
            "DELETE FROM agent_images WHERE transcript_id = ?1 AND seq >= ?2",
            params![id, seq],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_entry(&self, id: &str, seq: u64, kind: EntryKind, payload: &str) -> Result<()> {
        let id = parse_id(id)?;
        if payload.len() > MAX_ENTRY_BYTES {
            return Err(StoreError::TooLarge("transcript entry"));
        }
        let seq = i64::try_from(seq).map_err(|_| StoreError::TooLarge("sequence number"))?;
        let sealed = self.seal_agent(&id, "entry", payload)?;
        let t = now();
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM agent_transcripts WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        self.conn.execute(
            "INSERT INTO agent_entries (transcript_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, seq, kind.as_str(), sealed, t],
        )?;
        self.conn.execute(
            "UPDATE agent_transcripts SET updated_at = ?1 WHERE id = ?2",
            params![t, id],
        )?;
        Ok(())
    }

    /// The transcript with its entries in the order they were written.
    /// Entries that can't be opened are left out.
    pub fn get_transcript(&self, id: &str) -> Result<Transcript> {
        let id = parse_id(id)?;
        let row: SummaryRow = self
            .conn
            .query_row(
                "SELECT id, title, folders, created_at, updated_at, parent_id, fork_seq, project_id, handoff, parent_chat_id
                 FROM agent_transcripts WHERE id = ?1",
                [&id],
                summary_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let summary = self.summary_from_row(row);
        let rows: Vec<(i64, String, String, i64)> = self
            .conn
            .prepare(
                "SELECT seq, kind, payload, created_at FROM agent_entries
                 WHERE transcript_id = ?1 ORDER BY id",
            )?
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let entries = rows
            .into_iter()
            .filter_map(|(seq, kind, payload, created_at)| {
                Some(TranscriptEntry {
                    seq: u64::try_from(seq).ok()?,
                    kind: EntryKind::parse(&kind)?,
                    payload: self.unseal_agent(&id, "entry", payload)?,
                    created_at,
                })
            })
            .collect();
        Ok(Transcript { summary, entries })
    }

    /// A new transcript with the entries of `source` before `before_seq`
    /// (all of them for a clone), their images, and a link to the source
    /// (Pi's `/fork` and `/clone`). Values are re-sealed for the new
    /// transcript.
    pub fn copy_transcript(
        &mut self,
        source: &str,
        before_seq: Option<u64>,
        title: &str,
    ) -> Result<TranscriptSummary> {
        let src = self.get_transcript(source)?;
        let images = self.agent_images(&src.summary.id)?;
        let keep = |seq: u64| before_seq.is_none_or(|b| seq < b);
        let entries: Vec<(u64, EntryKind, String)> = src
            .entries
            .into_iter()
            .filter(|e| keep(e.seq))
            .map(|e| (e.seq, e.kind, e.payload))
            .collect();
        let fork_seq = before_seq.or_else(|| entries.iter().map(|e| e.0 + 1).max());
        let images: Vec<(u64, Vec<super::ImageData>)> =
            images.into_iter().filter(|(seq, _)| keep(*seq)).collect();
        let mut copy = self.write_transcript(
            title,
            &src.summary.folders,
            Some((&src.summary.id, fork_seq.unwrap_or(0))),
            &entries,
            &images,
        )?;
        // Forks and clones stay in the original's project.
        if let Some(project) = &src.summary.project_id {
            self.set_transcript_project(&copy.id, Some(project))?;
            copy.project_id = Some(project.clone());
        }
        Ok(copy)
    }

    /// A new transcript from imported entries (Pi's `/import`). The caller
    /// validates the payloads; the store checks sizes and counts.
    pub fn import_transcript(
        &mut self,
        title: &str,
        folders: &[String],
        entries: &[(u64, EntryKind, String)],
        images: &[(u64, Vec<super::ImageData>)],
    ) -> Result<TranscriptSummary> {
        self.write_transcript(title, folders, None, entries, images)
    }

    fn write_transcript(
        &mut self,
        title: &str,
        folders: &[String],
        parent: Option<(&str, u64)>,
        entries: &[(u64, EntryKind, String)],
        images: &[(u64, Vec<super::ImageData>)],
    ) -> Result<TranscriptSummary> {
        if entries.len() > MAX_ENTRIES {
            return Err(StoreError::TooLarge("transcript"));
        }
        if entries.iter().any(|(_, _, p)| p.len() > MAX_ENTRY_BYTES) {
            return Err(StoreError::TooLarge("transcript entry"));
        }
        let mut summary = self.create_transcript(folders)?;
        let id = summary.id.clone();
        let title = clean_title(title);
        let tx = self.conn.unchecked_transaction()?;
        let result = (|| -> Result<()> {
            tx.execute(
                "UPDATE agent_transcripts SET title = ?1, parent_id = ?2, fork_seq = ?3 WHERE id = ?4",
                params![
                    self.seal_agent(&id, "title", &title)?,
                    parent.map(|p| p.0),
                    parent.map(|p| p.1 as i64),
                    id
                ],
            )?;
            let t = now();
            for (seq, kind, payload) in entries {
                let seq =
                    i64::try_from(*seq).map_err(|_| StoreError::TooLarge("sequence number"))?;
                tx.execute(
                    "INSERT INTO agent_entries (transcript_id, seq, kind, payload, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id,
                        seq,
                        kind.as_str(),
                        self.seal_agent(&id, "entry", payload)?,
                        t
                    ],
                )?;
            }
            for (seq, list) in images {
                if list.len() > super::MAX_IMAGES_PER_MESSAGE {
                    return Err(StoreError::TooLarge("number of images"));
                }
                for img in list {
                    if !matches!(
                        img.mime.as_str(),
                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                    ) || img.base64.len() > super::MAX_IMAGE_BYTES / 3 * 4 + 4
                    {
                        return Err(StoreError::TooLarge("image"));
                    }
                    tx.execute(
                        "INSERT INTO agent_images (transcript_id, seq, mime, data) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            id,
                            *seq as i64,
                            img.mime,
                            self.seal_agent(&id, &format!("image:{seq}"), &img.base64)?
                        ],
                    )?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => tx.commit()?,
            Err(e) => {
                drop(tx);
                let _ = self.delete_transcript(&id);
                return Err(e);
            }
        }
        summary.title = title;
        summary.parent_id = parent.map(|p| p.0.to_string());
        summary.fork_seq = parent.map(|p| p.1);
        Ok(summary)
    }

    /// Keep the images of the user message saved as entry `seq`. Images are
    /// stored apart from the entry so a message can carry up to four large
    /// images without passing the entry size limit.
    pub fn attach_agent_images(&mut self, id: &str, seq: u64, images: &[Vec<u8>]) -> Result<()> {
        use base64::Engine;
        let id = parse_id(id)?;
        if images.len() > super::MAX_IMAGES_PER_MESSAGE {
            return Err(StoreError::TooLarge("number of images"));
        }
        let seq = i64::try_from(seq).map_err(|_| StoreError::TooLarge("sequence number"))?;
        let mut sealed = Vec::new();
        for bytes in images {
            if bytes.len() > super::MAX_IMAGE_BYTES {
                return Err(StoreError::TooLarge("image"));
            }
            let mime = super::sniff_image(bytes)
                .ok_or(StoreError::TooLarge("image (unsupported type)"))?;
            let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            sealed.push((
                mime,
                self.seal_agent(&id, &format!("image:{seq}"), &base64)?,
            ));
        }
        let tx = self.conn.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM agent_transcripts WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        for (mime, data) in sealed {
            tx.execute(
                "INSERT INTO agent_images (transcript_id, seq, mime, data) VALUES (?1, ?2, ?3, ?4)",
                params![id, seq, mime, data],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A transcript's images by entry, in the order attached. Images that
    /// can't be decrypted are left out.
    pub fn agent_images(
        &self,
        id: &str,
    ) -> Result<std::collections::BTreeMap<u64, Vec<super::ImageData>>> {
        let id = parse_id(id)?;
        let rows: Vec<(i64, String, String)> = self
            .conn
            .prepare(
                "SELECT seq, mime, data FROM agent_images WHERE transcript_id = ?1 ORDER BY seq, id",
            )?
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out: std::collections::BTreeMap<u64, Vec<super::ImageData>> = Default::default();
        for (seq, mime, data) in rows {
            if let Some(base64) = self.unseal_agent(&id, &format!("image:{seq}"), data) {
                out.entry(seq as u64)
                    .or_default()
                    .push(super::ImageData { mime, base64 });
            }
        }
        Ok(out)
    }

    pub fn delete_transcript(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let changed = self
            .conn
            .execute("DELETE FROM agent_transcripts WHERE id = ?1", [&id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.scrub_log()
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
    fn transcripts_can_be_forked_cloned_and_imported() {
        let mut s = ConversationStore::open_in_memory_with(key(11)).unwrap();
        let t = s.create_transcript(&["C:/p".into()]).unwrap();
        s.rename_transcript(&t.id, "Fix the bug").unwrap();
        for seq in 1..=6 {
            s.append_entry(&t.id, seq, EntryKind::Message, &format!("{{\"n\":{seq}}}"))
                .unwrap();
        }
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(b"x");
        s.attach_agent_images(&t.id, 2, std::slice::from_ref(&png))
            .unwrap();
        s.attach_agent_images(&t.id, 5, std::slice::from_ref(&png))
            .unwrap();

        let fork = s
            .copy_transcript(&t.id, Some(4), "Fix the bug (fork)")
            .unwrap();
        assert_eq!(
            (fork.parent_id.as_deref(), fork.fork_seq),
            (Some(t.id.as_str()), Some(4))
        );
        let got = s.get_transcript(&fork.id).unwrap();
        assert_eq!(
            got.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(got.summary.title, "Fix the bug (fork)");
        assert_eq!(got.summary.folders, ["C:/p"]);
        assert_eq!(
            s.agent_images(&fork.id)
                .unwrap()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [2]
        );

        let clone = s
            .copy_transcript(&t.id, None, "Fix the bug (copy)")
            .unwrap();
        assert_eq!(clone.fork_seq, Some(7));
        assert_eq!(s.get_transcript(&clone.id).unwrap().entries.len(), 6);
        assert_eq!(s.agent_images(&clone.id).unwrap().len(), 2);
        let listed = s.list_transcripts().unwrap();
        assert!(
            listed
                .iter()
                .any(|x| x.id == fork.id && x.parent_id.as_deref() == Some(t.id.as_str()))
        );

        // Deleting the parent keeps the forks, without the link.
        s.delete_transcript(&t.id).unwrap();
        assert_eq!(s.get_transcript(&fork.id).unwrap().summary.parent_id, None);

        let imported = s
            .import_transcript(
                "Imported",
                &[],
                &[
                    (1, EntryKind::Message, "{}".into()),
                    (2, EntryKind::Compaction, "{}".into()),
                ],
                &[(
                    1,
                    vec![crate::conversations::ImageData {
                        mime: "image/png".into(),
                        base64: "iVBORw0KGgp4".into(),
                    }],
                )],
            )
            .unwrap();
        let got = s.get_transcript(&imported.id).unwrap();
        assert_eq!(got.entries[1].kind, EntryKind::Compaction);
        assert_eq!(s.agent_images(&imported.id).unwrap()[&1].len(), 1);
        // A bad image type or an oversized entry leaves nothing behind.
        let before = s.list_transcripts().unwrap().len();
        assert!(
            s.import_transcript(
                "Bad",
                &[],
                &[(1, EntryKind::Message, "{}".into())],
                &[(
                    1,
                    vec![crate::conversations::ImageData {
                        mime: "text/html".into(),
                        base64: "PGI+".into()
                    }]
                )],
            )
            .is_err()
        );
        assert!(
            s.import_transcript(
                "Big",
                &[],
                &[(1, EntryKind::Message, "x".repeat(MAX_ENTRY_BYTES + 1))],
                &[]
            )
            .is_err()
        );
        assert_eq!(s.list_transcripts().unwrap().len(), before);
    }

    #[test]
    fn agent_images_are_sealed_per_entry_and_deleted_with_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let mut s = ConversationStore::open_with(&path, Some(key(9))).unwrap();
        let t = s.create_transcript(&["C:/p".into()]).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(b"IMG-CANARY-42");
        s.attach_agent_images(&t.id, 3, &[png.clone(), png.clone()])
            .unwrap();
        s.attach_agent_images(&t.id, 7, &[png.clone()]).unwrap();
        let got = s.agent_images(&t.id).unwrap();
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), [3, 7]);
        assert_eq!(got[&3].len(), 2);
        assert!(got[&7][0].data_url().starts_with("data:image/png;base64,"));
        // Not an image, too many, or an unknown transcript: refused.
        assert!(
            s.attach_agent_images(&t.id, 8, &[b"text".to_vec()])
                .is_err()
        );
        assert!(
            s.attach_agent_images(&t.id, 8, &vec![png.clone(); 5])
                .is_err()
        );
        let other = s.create_transcript(&[]).unwrap();
        s.delete_transcript(&other.id).unwrap();
        assert!(matches!(
            s.attach_agent_images(&other.id, 1, std::slice::from_ref(&png)),
            Err(StoreError::NotFound)
        ));
        // The base64 of the canary never reaches the disk in plaintext.
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let on_disk = |m: &str| {
            std::fs::read_dir(dir.path()).unwrap().any(|e| {
                let b = std::fs::read(e.unwrap().path()).unwrap_or_default();
                b.windows(m.len()).any(|w| w == m.as_bytes())
            })
        };
        assert!(!on_disk(&b64[4..24]));
        s.delete_transcript(&t.id).unwrap();
        assert!(s.agent_images(&t.id).unwrap().is_empty());
    }

    #[test]
    fn transcripts_round_trip_in_order() {
        let store = ConversationStore::open_in_memory_with(key(1)).unwrap();
        let t = store.create_transcript(&["C:\\work".into()]).unwrap();
        assert_eq!(t.title, DEFAULT_AGENT_TITLE);
        store
            .append_entry(&t.id, 1, EntryKind::Message, r#"{"role":"user"}"#)
            .unwrap();
        store
            .append_entry(&t.id, 2, EntryKind::Message, r#"{"role":"assistant"}"#)
            .unwrap();
        store
            .append_entry(&t.id, 2, EntryKind::Compaction, r#"{"summary":"s"}"#)
            .unwrap();
        store.rename_transcript(&t.id, "Fix the\nbuild").unwrap();
        let got = store.get_transcript(&t.id).unwrap();
        assert_eq!(got.summary.title, "Fix the build");
        assert_eq!(got.summary.folders, vec!["C:\\work".to_string()]);
        let kinds: Vec<(u64, EntryKind)> = got.entries.iter().map(|e| (e.seq, e.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (1, EntryKind::Message),
                (2, EntryKind::Message),
                (2, EntryKind::Compaction)
            ]
        );
        assert_eq!(store.list_transcripts().unwrap().len(), 1);
        // A withdrawn request removes its entries, and only from its seq on.
        let mut store = store;
        store.remove_entries_from(&t.id, 2).unwrap();
        let left: Vec<u64> = store
            .get_transcript(&t.id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(left, vec![1]);
        store.delete_transcript(&t.id).unwrap();
        assert!(matches!(
            store.get_transcript(&t.id),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.append_entry(&t.id, 3, EntryKind::Message, "{}"),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.get_transcript("nope"),
            Err(StoreError::InvalidId)
        ));
    }

    #[test]
    fn transcripts_are_encrypted_and_erased_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let marker = "AGENT-CANARY-8f2e";
        let id = {
            let store = ConversationStore::open_with(&path, Some(key(2))).unwrap();
            let t = store.create_transcript(&[format!("C:\\{marker}")]).unwrap();
            store.rename_transcript(&t.id, marker).unwrap();
            store
                .append_entry(
                    &t.id,
                    1,
                    EntryKind::Message,
                    &format!("{{\"c\":\"{marker}\"}}"),
                )
                .unwrap();
            t.id
        };
        let on_disk = |m: &str| {
            std::fs::read_dir(dir.path()).unwrap().any(|e| {
                let bytes = std::fs::read(e.unwrap().path()).unwrap_or_default();
                bytes.windows(m.len()).any(|w| w == m.as_bytes())
            })
        };
        assert!(!on_disk(marker));
        // Without the key the transcript is unreadable and read-only.
        {
            let store = ConversationStore::open(&path).unwrap();
            assert!(store.is_locked());
            let got = store.get_transcript(&id).unwrap();
            assert!(got.entries.is_empty());
            assert!(matches!(
                store.create_transcript(&[]),
                Err(StoreError::Locked)
            ));
        }
        // A different key opens nothing either.
        {
            let store = ConversationStore::open_with(&path, Some(key(3))).unwrap();
            assert!(store.get_transcript(&id).unwrap().entries.is_empty());
        }
        let store = ConversationStore::open_with(&path, Some(key(2))).unwrap();
        assert!(
            store.get_transcript(&id).unwrap().entries[0]
                .payload
                .contains(marker)
        );
        store.delete_transcript(&id).unwrap();
        drop(store);
        assert!(!on_disk("AGENT-CANARY"));
    }

    #[test]
    fn entries_cannot_be_moved_between_transcripts() {
        let store = ConversationStore::open_in_memory_with(key(4)).unwrap();
        let a = store.create_transcript(&[]).unwrap();
        let b = store.create_transcript(&[]).unwrap();
        store
            .append_entry(&a.id, 1, EntryKind::Message, "{\"x\":1}")
            .unwrap();
        store
            .conn
            .execute("UPDATE agent_entries SET transcript_id = ?1", [&b.id])
            .unwrap();
        assert!(store.get_transcript(&b.id).unwrap().entries.is_empty());
    }

    #[test]
    fn oversized_entries_are_refused() {
        let store = ConversationStore::open_in_memory().unwrap();
        let t = store.create_transcript(&[]).unwrap();
        let big = "x".repeat(MAX_ENTRY_BYTES + 1);
        assert!(matches!(
            store.append_entry(&t.id, 1, EntryKind::Message, &big),
            Err(StoreError::TooLarge(_))
        ));
    }
}

impl ConversationStore {
    /// Agent sessions whose title or user and assistant messages contain
    /// `query` (case ignored), newest first, one hit per session, at most
    /// `limit`. Decrypted in memory only, like [`Self::search_chats`]. Tool
    /// results are not searched (they are mostly file contents).
    pub fn search_transcripts(&self, query: &str, limit: usize) -> Result<Vec<super::SearchHit>> {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for t in self.list_transcripts()? {
            if hits.len() >= limit {
                break;
            }
            let mut found = super::search::snippet(&t.title, &needle);
            if found.is_none() {
                let mut stmt = self.conn.prepare(
                    "SELECT payload FROM agent_entries WHERE transcript_id = ?1 AND kind = 'message' ORDER BY id",
                )?;
                let rows = stmt.query_map([&t.id], |r| r.get::<_, String>(0))?;
                for stored in rows {
                    let Some(payload) = self.unseal_agent(&t.id, "entry", stored?) else {
                        continue;
                    };
                    let Ok(message) = serde_json::from_str::<serde_json::Value>(&payload) else {
                        continue;
                    };
                    let role = message.get("role").and_then(|r| r.as_str());
                    if !matches!(role, Some("user" | "assistant")) {
                        continue;
                    }
                    let Some(text) = message.get("content").and_then(|c| c.as_str()) else {
                        continue;
                    };
                    found = super::search::snippet(text, &needle);
                    if found.is_some() {
                        break;
                    }
                }
            }
            if let Some(snippet) = found {
                hits.push(super::SearchHit {
                    id: t.id,
                    title: t.title,
                    snippet,
                    updated_at: t.updated_at,
                });
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn sessions_are_found_by_what_was_said_not_by_tool_results() {
        let s = ConversationStore::open_in_memory().unwrap();
        let t = s.create_transcript(&["C:/work".into()]).unwrap();
        for (seq, payload) in [
            r#"{"role":"user","content":"Rename the Greeting struct"}"#,
            r#"{"role":"tool","content":"struct Greeting; secret_marker"}"#,
            r#"{"role":"assistant","content":"Done: it is now Salutation."}"#,
        ]
        .iter()
        .enumerate()
        {
            s.append_entry(&t.id, seq as u64, EntryKind::Message, payload)
                .unwrap();
        }
        assert_eq!(s.search_transcripts("salutation", 5).unwrap()[0].id, t.id);
        assert!(
            s.search_transcripts("greeting struct", 5).unwrap()[0]
                .snippet
                .contains("Rename")
        );
        assert!(s.search_transcripts("secret_marker", 5).unwrap().is_empty());
    }
}
