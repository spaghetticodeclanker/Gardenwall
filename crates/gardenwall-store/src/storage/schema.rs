use rusqlite::{Connection, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE authorization_runtime (
    runtime_id BLOB PRIMARY KEY
        CHECK(length(runtime_id) = 16),
    started_at_ms INTEGER NOT NULL
);

CREATE TABLE authorization_proposals (
    proposal_id BLOB PRIMARY KEY
        CHECK(typeof(proposal_id) = 'blob'
              AND length(proposal_id) = 16),

    owner_id BLOB NOT NULL
        CHECK(typeof(owner_id) = 'blob'
              AND length(owner_id) = 16),

    session_id BLOB NOT NULL
        CHECK(typeof(session_id) = 'blob'
              AND length(session_id) = 16),

    capability TEXT NOT NULL
        CHECK(capability IN ('WebSearch', 'WebpageRetrieval')),

    request_arguments BLOB
        CHECK(
            request_arguments IS NULL
            OR (
                typeof(request_arguments) = 'blob'
                AND length(request_arguments) > 0
            )
        ),

    state TEXT NOT NULL
        CHECK(state IN (
            'Pending',
            'Approved',
            'Rejected',
            'Cancelled',
            'Expired',
            'Claimed'
        )),

    created_at_ms INTEGER NOT NULL
        CHECK(typeof(created_at_ms) = 'integer'),

    original_deadline_ms INTEGER NOT NULL
        CHECK(typeof(original_deadline_ms) = 'integer'),

    approved_at_ms INTEGER
        CHECK(approved_at_ms IS NULL
              OR typeof(approved_at_ms) = 'integer'),

    terminal_at_ms INTEGER
    CHECK(
        terminal_at_ms IS NULL
        OR typeof(terminal_at_ms) = 'integer'
    ),

    runtime_id BLOB NOT NULL
        CHECK(typeof(runtime_id) = 'blob'
              AND length(runtime_id) = 16),

    FOREIGN KEY(runtime_id)
        REFERENCES authorization_runtime(runtime_id),


    CHECK(
        created_at_ms >= 0
        AND created_at_ms <= 253402300799999
        AND original_deadline_ms >= 300000
        AND original_deadline_ms <= 253402300799999
        AND original_deadline_ms - created_at_ms = 300000
    ),

    CHECK(
    approved_at_ms IS NULL
    OR (
        approved_at_ms >= 0
        AND approved_at_ms <= 253402300799999
    )
),

    CHECK(
        (state = 'Pending' AND approved_at_ms IS NULL)
        OR
        (
            state IN ('Approved', 'Claimed')
            AND approved_at_ms IS NOT NULL
            AND approved_at_ms >= created_at_ms
            AND approved_at_ms < original_deadline_ms
        )
        OR
        (
            state IN ('Rejected', 'Cancelled', 'Expired')
            AND (
                approved_at_ms IS NULL
                OR (
                    approved_at_ms >= created_at_ms
                    AND approved_at_ms < original_deadline_ms
                )
            )
        )
    ),

CHECK(
    (
        state IN ('Pending', 'Approved')
        AND request_arguments IS NOT NULL
        AND terminal_at_ms IS NULL
    )
    OR
    (
        state IN (
            'Rejected',
            'Cancelled',
            'Expired',
            'Claimed'
        )
        AND terminal_at_ms IS NOT NULL
    )
),

CHECK(
    terminal_at_ms IS NULL
    OR (
        terminal_at_ms >= created_at_ms
        AND (
            approved_at_ms IS NULL
            OR terminal_at_ms >= approved_at_ms
        )
    )
)
);

CREATE TABLE authorization_audit (
    audit_id INTEGER PRIMARY KEY,

    proposal_id BLOB NOT NULL
        CHECK(length(proposal_id) = 16),

    event_type TEXT NOT NULL,

    event_at_ms INTEGER NOT NULL,

    FOREIGN KEY(proposal_id)
        REFERENCES authorization_proposals(proposal_id)
);

CREATE INDEX idx_proposals_state_deadline
ON authorization_proposals(state, original_deadline_ms);

CREATE INDEX idx_proposals_session
ON authorization_proposals(session_id);

CREATE INDEX idx_audit_proposal
ON authorization_audit(proposal_id);
"#;

fn validate_database_integrity(connection: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;

    if foreign_keys_enabled != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let integrity_result: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;

    if integrity_result != "ok" {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let foreign_key_violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;

    if foreign_key_violations != 0 {
        return Err(rusqlite::Error::InvalidQuery);
    }

    Ok(())
}

pub(super) fn schema_fingerprint(connection: &Connection) -> Result<[u8; 32]> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql
         FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%'
           AND type IN ('table', 'index', 'trigger', 'view')
         ORDER BY type, name",
    )?;

    let mut rows = statement.query([])?;
    let mut hasher = Sha256::new();

    while let Some(row) = rows.next()? {
        let object_type: String = row.get(0)?;
        let object_name: String = row.get(1)?;
        let definition: Option<String> = row.get(2)?;

        let definition = definition.unwrap_or_default();

        for field in [
            object_type.as_bytes(),
            object_name.as_bytes(),
            definition.as_bytes(),
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field);
        }
    }

    Ok(hasher.finalize().into())
}

fn expected_schema_fingerprint() -> Result<[u8; 32]> {
    let connection = Connection::open_in_memory()?;

    connection.execute_batch(INITIAL_SCHEMA)?;

    schema_fingerprint(&connection)
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let expected_tables = [
        (
            "authorization_runtime",
            &["runtime_id", "started_at_ms"][..],
        ),
        (
            "authorization_proposals",
            &[
                "proposal_id",
                "owner_id",
                "session_id",
                "capability",
                "request_arguments",
                "state",
                "created_at_ms",
                "original_deadline_ms",
                "approved_at_ms",
                "terminal_at_ms",
                "runtime_id",
            ][..],
        ),
        (
            "authorization_audit",
            &["audit_id", "proposal_id", "event_type", "event_at_ms"][..],
        ),
    ];

    for (table, expected_columns) in expected_tables {
        let exists: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM sqlite_master
             WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )?;

        if exists != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }

        let pragma = format!("PRAGMA table_info({table})");
        let mut statement = connection.prepare(&pragma)?;

        let actual_columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>>>()?;

        for expected_column in expected_columns {
            if !actual_columns.iter().any(|name| name == expected_column) {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
    }

    let actual_fingerprint = schema_fingerprint(connection)?;
    let expected_fingerprint = expected_schema_fingerprint()?;

    if actual_fingerprint != expected_fingerprint {
        return Err(rusqlite::Error::InvalidQuery);
    }

    Ok(())
}

/// Validate the complete, canonical v2 schema including all v1 objects.
/// No unknown objects, modified constraints or missing indexes are tolerated.
pub(super) fn validate_v2(connection: &Connection) -> Result<()> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(INITIAL_SCHEMA)?;
    expected.execute_batch(super::sessions::SESSION_SCHEMA_V2)?;
    if schema_fingerprint(connection)? != schema_fingerprint(&expected)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    validate_database_integrity(connection)
}

pub(super) fn initialize_database(connection: &mut Connection) -> Result<()> {
    connection.pragma_update(None, "foreign_keys", "ON")?;

    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let current_version: i64 =
        transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;

    match current_version {
        0 => {
            let existing_objects: i64 = transaction.query_row(
                "SELECT COUNT(*)
                 FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%'
                   AND type IN ('table', 'index', 'trigger', 'view')",
                [],
                |row| row.get(0),
            )?;

            if existing_objects != 0 {
                return Err(rusqlite::Error::InvalidQuery);
            }

            transaction.execute_batch(INITIAL_SCHEMA)?;

            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }

        SCHEMA_VERSION => {
            // Existing version-1 databases must already match the canonical schema.
        }
        super::migration::SESSION_SCHEMA_VERSION => {
            validate_v2(&transaction)?;
            transaction.commit()?;
            return Ok(());
        }

        _ => {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }

    validate_schema(&transaction)?;
    validate_database_integrity(&transaction)?;

    transaction.commit()?;

    Ok(())
}

pub(super) fn open_persistent_database(path: &Path) -> Result<Connection> {
    let mut connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    connection.busy_timeout(std::time::Duration::from_secs(5))?;

    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;

    initialize_database(&mut connection)?;

    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_creates_expected_tables() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        for table in [
            "authorization_runtime",
            "authorization_proposals",
            "authorization_audit",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();

            assert_eq!(count, 1, "missing table: {table}");
        }
    }

    #[test]
    fn initialization_sets_schema_version() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn initialization_is_idempotent() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();
        initialize_database(&mut connection).unwrap();

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        connection
            .pragma_update(None, "user_version", 999_i64)
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());
    }

    #[test]
    fn foreign_key_enforcement_is_enabled() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        let enabled: i64 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();

        assert_eq!(enabled, 1);
    }

    #[test]
    fn incomplete_versioned_schema_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());
    }

    #[test]
    fn proposal_rejects_invalid_deadline() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', ?4, 'Pending',
                0, 600000,
                NULL, ?5
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![5_u8],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn pending_proposal_rejects_approval_timestamp() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', ?4, 'Pending',
                0, 300000,
                1000, ?5
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![5_u8],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn approved_proposal_rejects_invalid_approval_time() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', ?4, 'Approved',
                1000, 301000,
                500, ?5
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![5_u8],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn pending_proposal_cannot_have_purged_arguments() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', NULL, 'Pending',
                0, 300000,
                NULL, ?4
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn terminal_proposal_allows_purged_arguments() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, terminal_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', NULL, 'Rejected',
                0, 300000,
                NULL, 1000, ?4
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_ok());
    }

    #[test]
    fn terminal_timestamp_before_creation_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [vec![1_u8; 16]],
            )
            .unwrap();

        let result = connection.execute(
            "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, terminal_at_ms, runtime_id
             ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', NULL, 'Rejected',
                1000, 301000,
                NULL, 500, ?4
             )",
            rusqlite::params![
                vec![2_u8; 16],
                vec![3_u8; 16],
                vec![4_u8; 16],
                vec![1_u8; 16],
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn versioned_schema_missing_columns_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        connection
            .execute_batch(
                "CREATE TABLE authorization_runtime (
                    runtime_id BLOB PRIMARY KEY,
                    started_at_ms INTEGER NOT NULL
                );

                CREATE TABLE authorization_proposals (
                    proposal_id BLOB PRIMARY KEY
                );

                CREATE TABLE authorization_audit (
                    audit_id INTEGER PRIMARY KEY,
                    proposal_id BLOB NOT NULL,
                    event_type TEXT NOT NULL,
                    event_at_ms INTEGER NOT NULL
                );",
            )
            .unwrap();

        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());
    }

    #[test]
    fn database_integrity_validation_succeeds() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        assert!(validate_database_integrity(&connection).is_ok());
    }

    #[test]
    fn canonical_schema_fingerprint_matches() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        let actual = schema_fingerprint(&connection).unwrap();
        let expected = expected_schema_fingerprint().unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn modified_constraint_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        let weakened_schema = INITIAL_SCHEMA.replace(
            "original_deadline_ms - created_at_ms = 300000",
            "original_deadline_ms - created_at_ms = 600000",
        );

        assert_ne!(
            weakened_schema, INITIAL_SCHEMA,
            "expected constraint was not found"
        );

        connection.execute_batch(&weakened_schema).unwrap();

        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());
    }

    #[test]
    fn unexpected_schema_object_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        connection
            .execute_batch(
                "CREATE INDEX unexpected_index
                 ON authorization_proposals(owner_id);",
            )
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());
    }

    #[test]
    fn version_zero_database_with_existing_objects_is_rejected() {
        let mut connection = Connection::open_in_memory().unwrap();

        connection
            .execute_batch(
                "CREATE TABLE unexpected_table (
                    id INTEGER PRIMARY KEY
                );",
            )
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();

        assert_eq!(version, 0);
    }

    #[test]
    fn failed_initialization_preserves_existing_schema() {
        let mut connection = Connection::open_in_memory().unwrap();

        connection
            .execute_batch(
                "CREATE TABLE authorization_runtime (
                    runtime_id BLOB PRIMARY KEY
                );",
            )
            .unwrap();

        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();

        assert!(initialize_database(&mut connection).is_err());

        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'authorization_proposals'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn successful_initialization_commits_schema_version() {
        let mut connection = Connection::open_in_memory().unwrap();

        initialize_database(&mut connection).unwrap();

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);

        assert!(validate_schema(&connection).is_ok());
        assert!(validate_database_integrity(&connection).is_ok());
    }

    #[test]
    fn persistent_database_survives_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall-test.db");

        {
            let connection = open_persistent_database(&database_path).unwrap();

            let version: i64 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();

            assert_eq!(version, SCHEMA_VERSION);
        }

        {
            let connection = open_persistent_database(&database_path).unwrap();

            assert!(validate_schema(&connection).is_ok());
            assert!(validate_database_integrity(&connection).is_ok());
        }
    }

    #[test]
    fn persistent_database_uses_wal_and_full_synchronous() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall-test.db");

        let connection = open_persistent_database(&database_path).unwrap();

        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();

        let synchronous: i64 = connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode.to_lowercase(), "wal");
        assert_eq!(synchronous, 2);
    }

    #[test]
    fn persistent_database_rejects_invalid_path() {
        let directory = tempfile::tempdir().unwrap();

        let invalid_path = directory
            .path()
            .join("missing-directory")
            .join("gardenwall.db");

        assert!(open_persistent_database(&invalid_path).is_err());
    }

    #[test]
    fn persistent_database_rejects_incompatible_schema() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall-test.db");

        {
            let connection = Connection::open(&database_path).unwrap();

            connection
                .execute_batch(
                    "CREATE TABLE unexpected_table (
                        id INTEGER PRIMARY KEY
                    );",
                )
                .unwrap();
        }

        assert!(open_persistent_database(&database_path).is_err());
    }
}
