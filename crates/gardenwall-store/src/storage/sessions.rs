//! Persistent session lifecycle primitives (not yet connected to HTTP).
//!
//! This module deliberately does not install its schema in the v1 database.
//! Session schema installation requires an explicit, fingerprinted v2 migration.
//! Callers must obtain user identity from a trusted authentication mechanism;
//! these functions do not authenticate users or issue browser cookies.
use gardenwall_core::{SessionId, UserId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const IDLE_MS: i64 = 30 * 60 * 1000;
const ABSOLUTE_MS: i64 = 12 * 60 * 60 * 1000;
const MAX_MS: i64 = 253_402_300_799_999;

/// Proposed v2 schema. Do not execute against a v1 production database.
/// All state changes and their metadata-only audit records must be atomic.
pub(super) const SESSION_SCHEMA_V2: &str = r#"
CREATE TABLE authenticated_sessions (
    session_id BLOB PRIMARY KEY CHECK(typeof(session_id)='blob' AND length(session_id)=16),
    user_id BLOB NOT NULL CHECK(typeof(user_id)='blob' AND length(user_id)=16),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    last_interactive_at_ms INTEGER NOT NULL,
    absolute_deadline_ms INTEGER NOT NULL,
    revoked_at_ms INTEGER,
    CHECK(last_interactive_at_ms >= created_at_ms),
    CHECK(last_interactive_at_ms < absolute_deadline_ms),
    CHECK(absolute_deadline_ms - created_at_ms = 43200000),
    CHECK(revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms)
);
CREATE TABLE session_audit (
    audit_id INTEGER PRIMARY KEY,
    session_id BLOB NOT NULL REFERENCES authenticated_sessions(session_id),
    event_type TEXT NOT NULL CHECK(event_type IN ('Created','Interactive','Revoked','Expired')),
    event_at_ms INTEGER NOT NULL CHECK(event_at_ms >= 0)
);
CREATE INDEX idx_sessions_user ON authenticated_sessions(user_id);
CREATE INDEX idx_sessions_expiry ON authenticated_sessions(absolute_deadline_ms);
CREATE INDEX idx_session_audit_session ON session_audit(session_id);
"#;

#[derive(Debug)]
pub(super) enum SessionError {
    InvalidTime,
    Unavailable,
    Database(rusqlite::Error),
}
impl From<rusqlite::Error> for SessionError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTime => write!(f, "invalid session time"),
            Self::Unavailable => write!(f, "session unavailable"),
            Self::Database(e) => write!(f, "session database error: {e}"),
        }
    }
}
impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(e) => Some(e),
            _ => None,
        }
    }
}

fn checked_time(now: i64) -> Result<(), SessionError> {
    if (0..=MAX_MS).contains(&now) {
        Ok(())
    } else {
        Err(SessionError::InvalidTime)
    }
}

fn valid(created: i64, last: i64, deadline: i64, revoked: Option<i64>, now: i64) -> bool {
    revoked.is_none() && now >= created && now >= last && now < deadline && now - last < IDLE_MS
}

/// Insert an authenticated session and mandatory audit event together.
/// Authentication and generation of a cryptographically random ID are caller responsibilities.
pub(super) fn create_session(
    db: &mut Connection,
    user: UserId,
    session: SessionId,
    now: i64,
) -> Result<(), SessionError> {
    checked_time(now)?;
    let deadline = now
        .checked_add(ABSOLUTE_MS)
        .ok_or(SessionError::InvalidTime)?;
    if deadline > MAX_MS {
        return Err(SessionError::InvalidTime);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO authenticated_sessions
        (session_id,user_id,created_at_ms,last_interactive_at_ms,absolute_deadline_ms)
        VALUES (?1,?2,?3,?3,?4)",
        params![
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice(),
            now,
            deadline
        ],
    )?;
    tx.execute(
        "INSERT INTO session_audit(session_id,event_type,event_at_ms)
        VALUES (?1,'Created',?2)",
        params![session.to_bytes().as_slice(), now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Read-only validity check: background activity must never extend idle lifetime.
/// An absent or mismatched session is indistinguishable from a revoked session.
pub(super) fn verify_session(
    db: &Connection,
    user: UserId,
    session: SessionId,
    now: i64,
) -> Result<(), SessionError> {
    checked_time(now)?;
    let row: Option<(i64, i64, i64, Option<i64>)> = db
        .query_row(
            "SELECT created_at_ms,last_interactive_at_ms,absolute_deadline_ms,revoked_at_ms
         FROM authenticated_sessions WHERE session_id=?1 AND user_id=?2",
            params![session.to_bytes().as_slice(), user.to_bytes().as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (created, last, deadline, revoked) = row.ok_or(SessionError::Unavailable)?;
    if valid(created, last, deadline, revoked, now) {
        Ok(())
    } else {
        Err(SessionError::Unavailable)
    }
}

/// Only a trusted, authenticated foreground user interaction may refresh idle time.
pub(super) fn record_interaction(
    db: &mut Connection,
    user: UserId,
    session: SessionId,
    now: i64,
) -> Result<(), SessionError> {
    checked_time(now)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_session(&tx, user, session, now)?;
    let changed = tx.execute(
        "UPDATE authenticated_sessions SET last_interactive_at_ms=?1
        WHERE session_id=?2 AND user_id=?3 AND revoked_at_ms IS NULL",
        params![
            now,
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice()
        ],
    )?;
    if changed != 1 {
        return Err(SessionError::Unavailable);
    }
    tx.execute(
        "INSERT INTO session_audit(session_id,event_type,event_at_ms)
        VALUES (?1,'Interactive',?2)",
        params![session.to_bytes().as_slice(), now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Revoke a session and all its unclaimed proposals in one IMMEDIATE transaction.
/// A session from another user cannot be revoked through this API.
pub(super) fn revoke_session_and_proposals(
    db: &mut Connection,
    user: UserId,
    session: SessionId,
    now: i64,
) -> Result<usize, SessionError> {
    checked_time(now)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(i64, Option<i64>)> = tx
        .query_row(
            "SELECT created_at_ms,revoked_at_ms FROM authenticated_sessions
         WHERE session_id=?1 AND user_id=?2",
            params![session.to_bytes().as_slice(), user.to_bytes().as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (created, revoked) = row.ok_or(SessionError::Unavailable)?;
    if now < created {
        return Err(SessionError::InvalidTime);
    }
    if revoked.is_some() {
        return Err(SessionError::Unavailable);
    }
    let invalid: i64 = tx.query_row(
        "SELECT COUNT(*) FROM authorization_proposals WHERE session_id=?1 AND owner_id=?2
         AND state IN ('Pending','Approved')
         AND (created_at_ms>?3 OR (approved_at_ms IS NOT NULL AND approved_at_ms>?3))",
        params![
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice(),
            now
        ],
        |r| r.get(0),
    )?;
    if invalid != 0 {
        return Err(SessionError::InvalidTime);
    }
    let audited = tx.execute(
        "INSERT INTO authorization_audit(proposal_id,event_type,event_at_ms)
        SELECT proposal_id,'SessionRevoked',?1 FROM authorization_proposals
        WHERE session_id=?2 AND owner_id=?3 AND state IN ('Pending','Approved')",
        params![
            now,
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice()
        ],
    )?;
    let changed = tx.execute(
        "UPDATE authorization_proposals SET state='Cancelled',terminal_at_ms=?1
        WHERE session_id=?2 AND owner_id=?3 AND state IN ('Pending','Approved')",
        params![
            now,
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice()
        ],
    )?;
    if audited != changed {
        return Err(SessionError::Unavailable);
    }
    tx.execute(
        "UPDATE authenticated_sessions SET revoked_at_ms=?1
        WHERE session_id=?2 AND user_id=?3 AND revoked_at_ms IS NULL",
        params![
            now,
            session.to_bytes().as_slice(),
            user.to_bytes().as_slice()
        ],
    )?;
    tx.execute(
        "INSERT INTO session_audit(session_id,event_type,event_at_ms)
        VALUES (?1,'Revoked',?2)",
        params![session.to_bytes().as_slice(), now],
    )?;
    tx.commit()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::initialize_database;
    fn fixture() -> (Connection, UserId, SessionId) {
        let mut db = Connection::open_in_memory().unwrap();
        initialize_database(&mut db).unwrap();
        db.execute_batch(SESSION_SCHEMA_V2).unwrap();
        let user = UserId::from_bytes([1; 16]);
        let session = SessionId::from_bytes([2; 16]);
        create_session(&mut db, user, session, 1_000).unwrap();
        (db, user, session)
    }
    #[test]
    fn session_creation_and_verification() {
        let (db, u, s) = fixture();
        verify_session(&db, u, s, 1_000).unwrap();
        assert!(verify_session(&db, u, s, 1_000 + IDLE_MS).is_err());
    }
    #[test]
    fn wrong_identity_fails_closed() {
        let (db, _, s) = fixture();
        assert!(verify_session(&db, UserId::from_bytes([9; 16]), s, 1_001).is_err());
    }
    #[test]
    fn interaction_extends_idle_but_not_absolute() {
        let (mut db, u, s) = fixture();
        record_interaction(&mut db, u, s, 1_000 + IDLE_MS - 1).unwrap();
        assert!(verify_session(&db, u, s, 1_000 + IDLE_MS + 10).is_ok());
        let deadline: i64 = db
            .query_row(
                "SELECT absolute_deadline_ms FROM authenticated_sessions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deadline, 1_000 + ABSOLUTE_MS);
    }
    #[test]
    fn expired_interaction_does_not_resurrect_session() {
        let (mut db, u, s) = fixture();
        assert!(record_interaction(&mut db, u, s, 1_000 + IDLE_MS).is_err());
        assert!(verify_session(&db, u, s, 1_000 + IDLE_MS + 1).is_err());
    }
    #[test]
    fn clock_rollback_is_denied() {
        let (mut db, u, s) = fixture();
        assert!(verify_session(&db, u, s, 999).is_err());
        record_interaction(&mut db, u, s, 1_500).unwrap();
        assert!(verify_session(&db, u, s, 1_499).is_err());
    }
    #[test]
    fn revocation_is_durable_and_audited() {
        let (mut db, u, s) = fixture();
        assert_eq!(
            revoke_session_and_proposals(&mut db, u, s, 2_000).unwrap(),
            0
        );
        assert!(verify_session(&db, u, s, 2_001).is_err());
        assert!(revoke_session_and_proposals(&mut db, u, s, 2_002).is_err());
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM session_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
    #[test]
    fn failed_audit_rolls_back_revocation() {
        let (mut db, u, s) = fixture();
        db.execute_batch(
            "CREATE TEMP TRIGGER deny_revoke BEFORE INSERT ON main.session_audit
            WHEN NEW.event_type='Revoked' BEGIN SELECT RAISE(ABORT,'denied'); END;",
        )
        .unwrap();
        assert!(revoke_session_and_proposals(&mut db, u, s, 2_000).is_err());
        verify_session(&db, u, s, 2_001).unwrap();
    }
    #[test]
    fn background_verification_does_not_extend_idle() {
        let (db, u, s) = fixture();
        for t in [2_000, 5_000, 10_000] {
            verify_session(&db, u, s, t).unwrap();
        }
        assert!(verify_session(&db, u, s, 1_000 + IDLE_MS).is_err());
    }
    #[test]
    fn session_revocation_cancels_pending_proposal_atomically() {
        use crate::storage::{
            creation::create_proposal, recovery::register_runtime_and_recover, runtime::RuntimeId,
        };
        use gardenwall_core::{CapabilityProposal, CapabilityRequest, ProposalId};
        use std::time::{Duration, SystemTime};
        let (mut db, u, s) = fixture();
        let runtime = RuntimeId::generate();
        register_runtime_and_recover(&mut db, runtime, 0).unwrap();
        let id = ProposalId::from_bytes([3; 16]);
        let proposal = CapabilityProposal::new(
            id,
            u,
            s,
            CapabilityRequest::WebSearch {
                query: "test".into(),
            },
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_100),
        )
        .unwrap();
        create_proposal(&mut db, runtime, &proposal).unwrap();
        assert_eq!(
            revoke_session_and_proposals(&mut db, u, s, 2_000).unwrap(),
            1
        );
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Cancelled");
        assert!(verify_session(&db, u, s, 2_001).is_err());
    }
    #[test]
    fn duplicate_session_creation_is_rejected() {
        let (mut db, u, s) = fixture();
        assert!(create_session(&mut db, u, s, 2_000).is_err());
        verify_session(&db, u, s, 2_001).unwrap();
    }
}
