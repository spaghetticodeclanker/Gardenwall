use super::runtime::RuntimeId;
use rusqlite::{Connection, OptionalExtension, Result, TransactionBehavior, params};

const MAX_TIMESTAMP_MS: i64 = 253402300799999;
const RESTART_AUDIT_EVENT: &str = "RuntimeRestartInvalidation";

#[derive(Debug, PartialEq, Eq)]
pub struct RecoverySummary {
    pub cancelled_proposals: usize,
}

pub fn register_runtime_and_recover(
    connection: &mut Connection,
    runtime_id: RuntimeId,
    started_at_ms: i64,
) -> Result<RecoverySummary> {
    if !(0..=MAX_TIMESTAMP_MS).contains(&started_at_ms) {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // A runtime identifier must never be registered twice.
    let existing_runtime: Option<i64> = transaction
        .query_row(
            "SELECT 1
             FROM authorization_runtime
             WHERE runtime_id = ?1",
            [runtime_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()?;

    if existing_runtime.is_some() {
        return Err(rusqlite::Error::InvalidQuery);
    }

    // Reject inconsistent timestamps before modifying proposals.
    let invalid_timestamps: i64 = transaction.query_row(
        "SELECT COUNT(*)
         FROM authorization_proposals
         WHERE runtime_id != ?1
           AND state IN ('Pending', 'Approved')
           AND (
               created_at_ms > ?2
               OR (
                   approved_at_ms IS NOT NULL
                   AND approved_at_ms > ?2
               )
           )",
        params![runtime_id.as_bytes().as_slice(), started_at_ms],
        |row| row.get(0),
    )?;

    if invalid_timestamps != 0 {
        return Err(rusqlite::Error::InvalidQuery);
    }

    // Register the new runtime within the same transaction.
    transaction.execute(
        "INSERT INTO authorization_runtime
         (runtime_id, started_at_ms)
         VALUES (?1, ?2)",
        params![runtime_id.as_bytes().as_slice(), started_at_ms],
    )?;

    // Insert mandatory audit events before changing proposal states.
    let audit_count = transaction.execute(
        "INSERT INTO authorization_audit
         (proposal_id, event_type, event_at_ms)
         SELECT proposal_id, ?1, ?2
         FROM authorization_proposals
         WHERE runtime_id != ?3
           AND state IN ('Pending', 'Approved')",
        params![
            RESTART_AUDIT_EVENT,
            started_at_ms,
            runtime_id.as_bytes().as_slice()
        ],
    )?;

    // Cancel precisely the proposals covered by those audit events.
    let cancelled_count = transaction.execute(
        "UPDATE authorization_proposals
         SET state = 'Cancelled',
             terminal_at_ms = ?1
         WHERE runtime_id != ?2
           AND state IN ('Pending', 'Approved')",
        params![started_at_ms, runtime_id.as_bytes().as_slice()],
    )?;

    if audit_count != cancelled_count {
        return Err(rusqlite::Error::InvalidQuery);
    }

    transaction.commit()?;

    Ok(RecoverySummary {
        cancelled_proposals: cancelled_count,
    })
}

#[cfg(test)]
mod tests {
    use super::super::schema::initialize_database;
    use super::*;

    fn initialized_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();
        connection
    }

    fn insert_runtime(connection: &Connection, runtime_id: RuntimeId) {
        connection
            .execute(
                "INSERT INTO authorization_runtime
                 (runtime_id, started_at_ms)
                 VALUES (?1, 0)",
                [runtime_id.as_bytes().as_slice()],
            )
            .unwrap();
    }

    fn insert_proposal(
        connection: &Connection,
        runtime_id: RuntimeId,
        proposal_id: u8,
        state: &str,
    ) {
        let approved_at_ms: Option<i64> = if state == "Approved" {
            Some(1000)
        } else {
            None
        };

        connection
            .execute(
                "INSERT INTO authorization_proposals (
                    proposal_id,
                    owner_id,
                    session_id,
                    capability,
                    request_arguments,
                    state,
                    created_at_ms,
                    original_deadline_ms,
                    approved_at_ms,
                    terminal_at_ms,
                    runtime_id
                ) VALUES (
                    ?1, ?2, ?3,
                    'WebSearch', ?4, ?5,
                    0, 300000, ?6, NULL, ?7
                )",
                params![
                    vec![proposal_id; 16],
                    vec![10_u8; 16],
                    vec![11_u8; 16],
                    vec![1_u8],
                    state,
                    approved_at_ms,
                    runtime_id.as_bytes().as_slice(),
                ],
            )
            .unwrap();
    }

    #[test]
    fn first_startup_registers_runtime() {
        let mut connection = initialized_connection();
        let runtime_id = RuntimeId::generate();

        let summary = register_runtime_and_recover(&mut connection, runtime_id, 1000).unwrap();

        assert_eq!(summary.cancelled_proposals, 0);

        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM authorization_runtime
                 WHERE runtime_id = ?1",
                [runtime_id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn restart_cancels_pending_and_approved_proposals() {
        let mut connection = initialized_connection();

        let old_runtime = RuntimeId::generate();
        let new_runtime = RuntimeId::generate();

        insert_runtime(&connection, old_runtime);
        insert_proposal(&connection, old_runtime, 1, "Pending");
        insert_proposal(&connection, old_runtime, 2, "Approved");

        let summary = register_runtime_and_recover(&mut connection, new_runtime, 2000).unwrap();

        assert_eq!(summary.cancelled_proposals, 2);

        let cancelled_count: i64 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM authorization_proposals
                 WHERE state = 'Cancelled'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let audit_count: i64 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM authorization_audit
                 WHERE event_type = ?1",
                [RESTART_AUDIT_EVENT],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(cancelled_count, 2);
        assert_eq!(audit_count, 2);
    }

    #[test]
    fn duplicate_runtime_registration_is_rejected() {
        let mut connection = initialized_connection();
        let runtime_id = RuntimeId::generate();

        register_runtime_and_recover(&mut connection, runtime_id, 1000).unwrap();

        let second = register_runtime_and_recover(&mut connection, runtime_id, 2000);

        assert!(second.is_err());
    }

    #[test]
    fn invalid_startup_timestamp_is_rejected() {
        let mut connection = initialized_connection();
        let runtime_id = RuntimeId::generate();

        assert!(register_runtime_and_recover(&mut connection, runtime_id, -1).is_err());

        assert!(
            register_runtime_and_recover(&mut connection, runtime_id, MAX_TIMESTAMP_MS + 1)
                .is_err()
        );
    }

    #[test]
    fn restart_preserves_claimed_proposals() {
        let mut connection = initialized_connection();

        let old_runtime = RuntimeId::generate();
        let new_runtime = RuntimeId::generate();

        insert_runtime(&connection, old_runtime);

        connection
            .execute(
                "INSERT INTO authorization_proposals (
                proposal_id, owner_id, session_id,
                capability, request_arguments, state,
                created_at_ms, original_deadline_ms,
                approved_at_ms, terminal_at_ms, runtime_id
            ) VALUES (
                ?1, ?2, ?3,
                'WebSearch', NULL, 'Claimed',
                0, 300000,
                1000, 1500, ?4
            )",
                params![
                    vec![20_u8; 16],
                    vec![10_u8; 16],
                    vec![11_u8; 16],
                    old_runtime.as_bytes().as_slice(),
                ],
            )
            .unwrap();

        let summary = register_runtime_and_recover(&mut connection, new_runtime, 2000).unwrap();

        assert_eq!(summary.cancelled_proposals, 0);

        let state: String = connection
            .query_row(
                "SELECT state FROM authorization_proposals
             WHERE proposal_id = ?1",
                [vec![20_u8; 16]],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(state, "Claimed");
    }

    #[test]
    fn restart_preserves_terminal_proposals() {
        let mut connection = initialized_connection();

        let old_runtime = RuntimeId::generate();
        let new_runtime = RuntimeId::generate();

        insert_runtime(&connection, old_runtime);

        for (proposal_id, state) in [
            (30_u8, "Rejected"),
            (31_u8, "Cancelled"),
            (32_u8, "Expired"),
        ] {
            connection
                .execute(
                    "INSERT INTO authorization_proposals (
                    proposal_id, owner_id, session_id,
                    capability, request_arguments, state,
                    created_at_ms, original_deadline_ms,
                    approved_at_ms, terminal_at_ms, runtime_id
                ) VALUES (
                    ?1, ?2, ?3,
                    'WebSearch', NULL, ?4,
                    0, 300000,
                    NULL, 1000, ?5
                )",
                    params![
                        vec![proposal_id; 16],
                        vec![10_u8; 16],
                        vec![11_u8; 16],
                        state,
                        old_runtime.as_bytes().as_slice(),
                    ],
                )
                .unwrap();
        }

        let summary = register_runtime_and_recover(&mut connection, new_runtime, 2000).unwrap();

        assert_eq!(summary.cancelled_proposals, 0);

        let states: Vec<String> = {
            let mut statement = connection
                .prepare(
                    "SELECT state FROM authorization_proposals
                 ORDER BY proposal_id",
                )
                .unwrap();

            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };

        assert_eq!(states, vec!["Rejected", "Cancelled", "Expired"]);
    }

    #[test]
    fn restart_rejects_future_approval_timestamp() {
        let mut connection = initialized_connection();

        let old_runtime = RuntimeId::generate();
        let new_runtime = RuntimeId::generate();

        insert_runtime(&connection, old_runtime);
        insert_proposal(&connection, old_runtime, 40, "Approved");

        let result = register_runtime_and_recover(&mut connection, new_runtime, 500);

        assert!(result.is_err());

        let runtime_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM authorization_runtime
             WHERE runtime_id = ?1",
                [new_runtime.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(runtime_count, 0);

        let state: String = connection
            .query_row(
                "SELECT state FROM authorization_proposals
             WHERE proposal_id = ?1",
                [vec![40_u8; 16]],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(state, "Approved");
    }

    #[test]
    fn audit_failure_rolls_back_entire_recovery() {
        let mut connection = initialized_connection();

        let old_runtime = RuntimeId::generate();
        let new_runtime = RuntimeId::generate();

        insert_runtime(&connection, old_runtime);
        insert_proposal(&connection, old_runtime, 50, "Pending");

        connection
            .execute_batch(
                "CREATE TEMP TRIGGER reject_restart_audit
             BEFORE INSERT ON main.authorization_audit
             WHEN NEW.event_type = 'RuntimeRestartInvalidation'
             BEGIN
                 SELECT RAISE(ABORT, 'simulated audit failure');
             END;",
            )
            .unwrap();

        let result = register_runtime_and_recover(&mut connection, new_runtime, 2000);

        assert!(result.is_err());

        let runtime_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM authorization_runtime
             WHERE runtime_id = ?1",
                [new_runtime.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        let state: String = connection
            .query_row(
                "SELECT state FROM authorization_proposals
             WHERE proposal_id = ?1",
                [vec![50_u8; 16]],
                |row| row.get(0),
            )
            .unwrap();

        let audit_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM authorization_audit", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(runtime_count, 0);
        assert_eq!(state, "Pending");
        assert_eq!(audit_count, 0);
    }
}
