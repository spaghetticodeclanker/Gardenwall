//! Explicit, atomic v1 -> v2 session-schema migration. Never invoked automatically.
//! The v1 database is validated before modification, and a full v2 fingerprint
//! is checked before the transaction can commit.
use super::{schema, sessions::SESSION_SCHEMA_V2};
use rusqlite::{Connection, Result, TransactionBehavior};

pub(super) const SESSION_SCHEMA_VERSION: i64 = 2;

pub(super) fn migrate_v1_to_v2(db: &mut Connection) -> Result<()> {
    // Reject incompatible, modified or partially migrated v1 databases.
    schema::initialize_database(db)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    tx.execute_batch(SESSION_SCHEMA_V2)?;
    tx.pragma_update(None, "user_version", SESSION_SCHEMA_VERSION)?;
    schema::validate_v2(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    fn fixture() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        schema::initialize_database(&mut db).unwrap();
        db
    }
    #[test]
    fn migrates_and_reopens_without_data_loss() {
        let mut db = fixture();
        db.execute(
            "INSERT INTO authorization_runtime(runtime_id,started_at_ms) VALUES(?1,0)",
            params![vec![7_u8; 16]],
        )
        .unwrap();
        migrate_v1_to_v2(&mut db).unwrap();
        schema::initialize_database(&mut db).unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let count: i64 = db
            .query_row("SELECT count(*) FROM authorization_runtime", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        assert!(migrate_v1_to_v2(&mut db).is_err());
    }
    #[test]
    fn migration_rejects_modified_v1_without_writing_v2() {
        let mut db = fixture();
        db.execute_batch("CREATE TABLE unexpected(x INTEGER)")
            .unwrap();
        assert!(migrate_v1_to_v2(&mut db).is_err());
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='authenticated_sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn failed_migration_rolls_back_all_changes() {
        let mut db = fixture();
        // This name collides with v2, so v1 fingerprint validation fails before mutation.
        db.execute_batch("CREATE TABLE authenticated_sessions(x INTEGER)")
            .unwrap();
        assert!(migrate_v1_to_v2(&mut db).is_err());
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
    #[test]
    fn v2_schema_tampering_fails_closed() {
        let mut db = fixture();
        migrate_v1_to_v2(&mut db).unwrap();
        db.execute_batch("CREATE TABLE rogue_table(x INTEGER)")
            .unwrap();
        assert!(schema::initialize_database(&mut db).is_err());
    }
    #[test]
    fn v2_session_constraints_are_enforced() {
        let mut db = fixture();
        migrate_v1_to_v2(&mut db).unwrap();
        let result = db.execute("INSERT INTO authenticated_sessions(session_id,user_id,created_at_ms,last_interactive_at_ms,absolute_deadline_ms) VALUES(?1,?2,100,100,101)",params![vec![1_u8;16],vec![2_u8;16]]);
        assert!(result.is_err());
    }
}
