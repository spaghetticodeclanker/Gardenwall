//! Agent projects: a project is a set of
//! folders, and agent sessions are grouped under it.
//!
//! Folders reach a project only from the native folder picker opened by the
//! trusted core; the store keeps them sealed with the chat key like every
//! other text field, bound to the project and field. New sessions in a
//! project reuse its folders (the webview names a project by id, never a
//! path). Deleting a project keeps its sessions, unlinked.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use super::{ConversationStore, Result, StoreError, clean_title, now, parse_id};

/// Most folders in one project.
pub const MAX_PROJECT_FOLDERS: usize = 16;
const MAX_FOLDER_BYTES: usize = 32 * 1024;
/// Most projects kept.
pub const MAX_PROJECTS: usize = 500;

fn aad(project: &str, field: &str) -> String {
    format!("gardenwall-project:{project}:{field}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    /// Granted to the project's sessions. Chosen in the native picker only.
    pub folders: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ConversationStore {
    fn seal_project(&self, id: &str, field: &str, text: &str) -> Result<String> {
        match &self.cipher {
            Some(c) => c.seal(&aad(id, field), text).map_err(StoreError::Crypto),
            None if self.locked => Err(StoreError::Locked),
            None => Ok(text.to_string()),
        }
    }

    /// A field's value; `None` when it can't be opened (a project whose
    /// folders can't be read grants nothing).
    fn unseal_project(&self, id: &str, field: &str, stored: String) -> Option<String> {
        if !crate::crypto::FieldCipher::is_sealed(&stored) {
            return (self.cipher.is_none() && !self.locked).then_some(stored);
        }
        self.cipher
            .as_ref()
            .and_then(|c| c.open(&aad(id, field), &stored).ok())
    }

    fn folders_json(folders: &[String]) -> Result<String> {
        if folders.is_empty() {
            return Err(StoreError::Empty("project folder list"));
        }
        if folders.len() > MAX_PROJECT_FOLDERS {
            return Err(StoreError::TooLarge("project folders"));
        }
        let json = serde_json::to_string(folders).unwrap_or_else(|_| "[]".into());
        if json.len() > MAX_FOLDER_BYTES {
            return Err(StoreError::TooLarge("project folders"));
        }
        Ok(json)
    }

    pub fn create_project(&self, name: &str, folders: &[String]) -> Result<ProjectSummary> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM agent_projects", [], |r| r.get(0))?;
        if count as usize >= MAX_PROJECTS {
            return Err(StoreError::TooLarge("project list"));
        }
        let json = Self::folders_json(folders)?;
        let id = Uuid::new_v4().hyphenated().to_string();
        let name = clean_title(name);
        let t = now();
        self.conn.execute(
            "INSERT INTO agent_projects (id, name, folders, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![
                id,
                self.seal_project(&id, "name", &name)?,
                self.seal_project(&id, "folders", &json)?,
                t
            ],
        )?;
        Ok(ProjectSummary {
            id,
            name,
            folders: folders.to_vec(),
            created_at: t,
            updated_at: t,
        })
    }

    fn project_from_row(
        &self,
        (id, name, folders, created, updated): (String, String, String, i64, i64),
    ) -> ProjectSummary {
        let name = self
            .unseal_project(&id, "name", name)
            .unwrap_or_else(|| super::UNREADABLE.to_string());
        let folders = self
            .unseal_project(&id, "folders", folders)
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default();
        ProjectSummary {
            id,
            name,
            folders,
            created_at: created,
            updated_at: updated,
        }
    }

    /// Most recently used first.
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        let rows: Vec<(String, String, String, i64, i64)> = self
            .conn
            .prepare(
                "SELECT id, name, folders, created_at, updated_at FROM agent_projects
                 ORDER BY updated_at DESC, rowid DESC",
            )?
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows.into_iter().map(|r| self.project_from_row(r)).collect())
    }

    pub fn get_project(&self, id: &str) -> Result<ProjectSummary> {
        let id = parse_id(id)?;
        let row = self
            .conn
            .query_row(
                "SELECT id, name, folders, created_at, updated_at FROM agent_projects WHERE id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(self.project_from_row(row))
    }

    pub fn rename_project(&self, id: &str, name: &str) -> Result<()> {
        let id = parse_id(id)?;
        let name = self.seal_project(&id, "name", &clean_title(name))?;
        let changed = self.conn.execute(
            "UPDATE agent_projects SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, now(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Replace a project's folders (the caller got them from the picker).
    pub fn set_project_folders(&self, id: &str, folders: &[String]) -> Result<()> {
        let id = parse_id(id)?;
        let json = self.seal_project(&id, "folders", &Self::folders_json(folders)?)?;
        let changed = self.conn.execute(
            "UPDATE agent_projects SET folders = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Mark a project as just used (it moves to the top of the list).
    pub fn touch_project(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        self.conn.execute(
            "UPDATE agent_projects SET updated_at = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    /// Delete a project; its sessions are kept, no longer in a project.
    pub fn delete_project(&mut self, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let changed = self
            .conn
            .execute("DELETE FROM agent_projects WHERE id = ?1", [&id])?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
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
    fn projects_keep_folders_sealed_and_sessions_survive_deletion() {
        let mut s = ConversationStore::open_in_memory_with(key(3)).unwrap();
        assert!(matches!(
            s.create_project("Empty", &[]),
            Err(StoreError::Empty(_))
        ));
        let p = s
            .create_project("Game\nmod", &["C:\\work\\game".into()])
            .unwrap();
        assert_eq!(p.name, "Game mod");
        // Sealed: the raw column doesn't hold the path.
        let raw: String = s
            .conn
            .query_row("SELECT folders FROM agent_projects", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("work"));
        s.set_project_folders(&p.id, &["C:\\a".into(), "C:\\b".into()])
            .unwrap();
        s.rename_project(&p.id, "Renamed").unwrap();
        let got = s.get_project(&p.id).unwrap();
        assert_eq!((got.name.as_str(), got.folders.len()), ("Renamed", 2));

        let t = s.create_transcript(&got.folders).unwrap();
        s.set_transcript_project(&t.id, Some(&p.id)).unwrap();
        assert_eq!(
            s.get_transcript(&t.id)
                .unwrap()
                .summary
                .project_id
                .as_deref(),
            Some(p.id.as_str())
        );
        // A fork stays in the project.
        let fork = s.copy_transcript(&t.id, None, "copy").unwrap();
        assert_eq!(fork.project_id.as_deref(), Some(p.id.as_str()));
        s.delete_project(&p.id).unwrap();
        assert!(s.list_projects().unwrap().is_empty());
        let after = s.get_transcript(&t.id).unwrap();
        assert_eq!(after.summary.project_id, None);
        // A project id can't name another table's row or be malformed.
        assert!(s.set_transcript_project(&t.id, Some("not-an-id")).is_err());
    }

    #[test]
    fn version_8_databases_gain_projects_and_keep_their_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let old = {
            let s = ConversationStore::open_with(&path, Some(key(6))).unwrap();
            let t = s.create_transcript(&["C:\\w".into()]).unwrap();
            s.conn
                .execute_batch(
                    "DROP INDEX agent_transcripts_by_project;
                     ALTER TABLE agent_transcripts DROP COLUMN project_id;
                     DROP TABLE agent_projects;
                     PRAGMA user_version = 8;",
                )
                .unwrap();
            t
        };
        let s = ConversationStore::open_with(&path, Some(key(6))).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, super::super::SCHEMA_VERSION);
        let kept = s.get_transcript(&old.id).unwrap().summary;
        assert_eq!((kept.folders.len(), kept.project_id), (1, None));
        let p = s.create_project("P", &["C:\\w".into()]).unwrap();
        s.set_transcript_project(&old.id, Some(&p.id)).unwrap();
    }

    #[test]
    fn a_projects_values_cannot_be_swapped_with_another_projects() {
        let s = ConversationStore::open_in_memory_with(key(4)).unwrap();
        let a = s.create_project("A", &["C:\\a".into()]).unwrap();
        let b = s.create_project("B", &["C:\\b".into()]).unwrap();
        let folders_a: String = s
            .conn
            .query_row(
                "SELECT folders FROM agent_projects WHERE id = ?1",
                [&a.id],
                |r| r.get(0),
            )
            .unwrap();
        s.conn
            .execute(
                "UPDATE agent_projects SET folders = ?1 WHERE id = ?2",
                params![folders_a, b.id],
            )
            .unwrap();
        // B's folders no longer open: it grants nothing rather than A's.
        assert!(s.get_project(&b.id).unwrap().folders.is_empty());
    }
}
