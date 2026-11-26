use super::recovery::{RecoverySummary, register_runtime_and_recover};
use super::runtime::{RuntimeId, RuntimeOwnership, canonical_database_path};
use super::schema::open_persistent_database;

use rusqlite::Connection;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum StorageOpenError {
    Io(io::Error),
    Database(rusqlite::Error),
    Clock(std::time::SystemTimeError),
}

impl std::fmt::Display for StorageOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => {
                write!(formatter, "storage filesystem error: {error}")
            }
            Self::Database(error) => {
                write!(formatter, "storage database error: {error}")
            }
            Self::Clock(error) => {
                write!(formatter, "storage clock error: {error}")
            }
        }
    }
}

impl From<std::time::SystemTimeError> for StorageOpenError {
    fn from(error: std::time::SystemTimeError) -> Self {
        Self::Clock(error)
    }
}

impl std::error::Error for StorageOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Clock(error) => Some(error),
        }
    }
}

impl From<io::Error> for StorageOpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for StorageOpenError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Debug)]
pub(super) struct PersistentStorageOwner {
    // The connection must close before ownership is released.
    connection: Connection,
    ownership: RuntimeOwnership,
    database_path: PathBuf,
    recovery_summary: RecoverySummary,
}

impl PersistentStorageOwner {
    pub(super) fn open(database_path: &Path) -> Result<Self, StorageOpenError> {
        let duration = SystemTime::now().duration_since(UNIX_EPOCH)?;

        let started_at_ms =
            i64::try_from(duration.as_millis()).map_err(|_| rusqlite::Error::InvalidQuery)?;

        Self::open_at(database_path, started_at_ms)
    }

    fn open_at(database_path: &Path, started_at_ms: i64) -> Result<Self, StorageOpenError> {
        let canonical_path = canonical_database_path(database_path)?;

        // Acquire ownership before opening SQLite.
        let ownership = RuntimeOwnership::acquire(&canonical_path)?;

        // If any later step fails, ownership is released automatically.
        let mut connection = open_persistent_database(&canonical_path)?;

        // Register this owner's runtime ID and invalidate old approvals.
        // This transaction must commit before the owner is returned.
        let recovery_summary =
            register_runtime_and_recover(&mut connection, ownership.runtime_id(), started_at_ms)?;

        Ok(Self {
            connection,
            ownership,
            database_path: canonical_path,
            recovery_summary,
        })
    }

    pub fn runtime_id(&self) -> RuntimeId {
        self.ownership.runtime_id()
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn recovery_summary(&self) -> &RecoverySummary {
        &self.recovery_summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_owner_initializes_persistent_database() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        let owner = PersistentStorageOwner::open(&database_path).unwrap();

        let version: i64 = owner
            .connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();

        assert_eq!(version, 1);
        assert!(owner.database_path().is_absolute());
        assert_eq!(owner.runtime_id().as_bytes().len(), 16);
    }

    #[test]
    fn second_storage_owner_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        let first = PersistentStorageOwner::open(&database_path).unwrap();

        let second = PersistentStorageOwner::open(&database_path);

        assert!(matches!(second, Err(StorageOpenError::Io(_))));

        drop(first);

        let third = PersistentStorageOwner::open(&database_path);

        assert!(third.is_ok());
    }

    #[test]
    fn failed_database_initialization_releases_lock() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        // Create an incompatible SQLite schema.
        {
            let connection = Connection::open(&database_path).unwrap();

            connection
                .execute_batch("CREATE TABLE unexpected_table (id INTEGER PRIMARY KEY);")
                .unwrap();
        }

        let result = PersistentStorageOwner::open(&database_path);

        assert!(matches!(result, Err(StorageOpenError::Database(_))));

        // A failed initialization must not leave ownership held.
        let ownership = RuntimeOwnership::acquire(&database_path);

        assert!(ownership.is_ok());
    }

    #[test]
    fn storage_errors_preserve_underlying_causes() {
        use std::error::Error;

        let error = StorageOpenError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "access denied",
        ));

        assert!(error.to_string().contains("storage filesystem error"));
        assert!(error.source().is_some());

        let database_error = StorageOpenError::Database(rusqlite::Error::InvalidQuery);

        assert!(
            database_error
                .to_string()
                .contains("storage database error")
        );
        assert!(database_error.source().is_some());
    }

    #[test]
    fn storage_owner_uses_canonical_database_path() {
        let directory = tempfile::tempdir().unwrap();

        let subdirectory = directory.path().join("subdirectory");
        std::fs::create_dir(&subdirectory).unwrap();

        let database_path = directory.path().join("gardenwall.db");

        let alternate_path = subdirectory.join("..").join("gardenwall.db");

        let expected_path = canonical_database_path(&database_path).unwrap();

        let owner = PersistentStorageOwner::open(&alternate_path).unwrap();

        assert_eq!(owner.database_path(), expected_path);
    }

    #[test]
    fn storage_owner_registers_runtime_automatically() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        let owner = PersistentStorageOwner::open_at(&database_path, 1000).unwrap();

        let registered_count: i64 = owner
            .connection()
            .query_row(
                "SELECT COUNT(*)
             FROM authorization_runtime
             WHERE runtime_id = ?1",
                [owner.runtime_id().as_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(registered_count, 1);
        assert_eq!(owner.recovery_summary().cancelled_proposals, 0);
    }

    #[test]
    fn storage_owner_recovers_previous_runtime_proposals() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        {
            let owner = PersistentStorageOwner::open_at(&database_path, 1000).unwrap();

            owner
                .connection()
                .execute(
                    "INSERT INTO authorization_proposals (
                    proposal_id, owner_id, session_id,
                    capability, request_arguments, state,
                    created_at_ms, original_deadline_ms,
                    approved_at_ms, terminal_at_ms, runtime_id
                ) VALUES (
                    ?1, ?2, ?3,
                    'WebSearch', ?4, 'Approved',
                    1000, 301000,
                    1500, NULL, ?5
                )",
                    rusqlite::params![
                        vec![1_u8; 16],
                        vec![2_u8; 16],
                        vec![3_u8; 16],
                        vec![4_u8],
                        owner.runtime_id().as_bytes().as_slice(),
                    ],
                )
                .unwrap();
        }

        let restarted_owner = PersistentStorageOwner::open_at(&database_path, 2000).unwrap();

        assert_eq!(restarted_owner.recovery_summary().cancelled_proposals, 1);

        let (state, terminal_at_ms): (String, i64) = restarted_owner
            .connection()
            .query_row(
                "SELECT state, terminal_at_ms
             FROM authorization_proposals
             WHERE proposal_id = ?1",
                [vec![1_u8; 16]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(state, "Cancelled");
        assert_eq!(terminal_at_ms, 2000);

        let audit_count: i64 = restarted_owner
            .connection()
            .query_row(
                "SELECT COUNT(*)
             FROM authorization_audit
             WHERE proposal_id = ?1
               AND event_type = 'RuntimeRestartInvalidation'",
                [vec![1_u8; 16]],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(audit_count, 1);
    }

    #[test]
    fn failed_recovery_releases_storage_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        {
            let owner = PersistentStorageOwner::open_at(&database_path, 2000).unwrap();

            owner
                .connection()
                .execute(
                    "INSERT INTO authorization_proposals (
                    proposal_id, owner_id, session_id,
                    capability, request_arguments, state,
                    created_at_ms, original_deadline_ms,
                    approved_at_ms, terminal_at_ms, runtime_id
                ) VALUES (
                    ?1, ?2, ?3,
                    'WebSearch', ?4, 'Approved',
                    2000, 302000,
                    2500, NULL, ?5
                )",
                    rusqlite::params![
                        vec![5_u8; 16],
                        vec![6_u8; 16],
                        vec![7_u8; 16],
                        vec![8_u8],
                        owner.runtime_id().as_bytes().as_slice(),
                    ],
                )
                .unwrap();
        }

        // Earlier startup time is inconsistent with approval time.
        let result = PersistentStorageOwner::open_at(&database_path, 1000);

        assert!(matches!(result, Err(StorageOpenError::Database(_))));

        // Ownership must have been released after recovery failed.
        let ownership = RuntimeOwnership::acquire(&database_path);
        assert!(ownership.is_ok());
    }
}
