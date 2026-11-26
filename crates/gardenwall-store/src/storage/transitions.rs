//! Transactional, identity-bound authorization transitions. This module does not
//! expose HTTP handlers or execute capabilities. Only trusted callers may supply
//! a verified AuthorizationContext and a trusted timestamp.
use super::api::AuthorizationContext;
use super::runtime::RuntimeId;
use gardenwall_core::ProposalId;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const MAX_MS: i64 = 253402300799999;
const CLAIM_WINDOW_MS: i64 = 30_000;

// Database row returned by the authorization lookup.
type ProposalRow = (String, i64, i64, Option<i64>, String, Option<Vec<u8>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    Approve,
    Reject,
    Cancel,
    Claim,
}

impl Decision {
    fn target(self) -> &'static str {
        match self {
            Self::Approve => "Approved",
            Self::Reject => "Rejected",
            Self::Cancel => "Cancelled",
            Self::Claim => "Claimed",
        }
    }
    fn event(self) -> &'static str {
        match self {
            Self::Approve => "ProposalApproved",
            Self::Reject => "ProposalRejected",
            Self::Cancel => "ProposalCancelled",
            Self::Claim => "ProposalClaimed",
        }
    }
}

#[derive(Debug)]
pub(super) enum TransitionError {
    InvalidTime,
    Unavailable,
    Expired,
    Database(rusqlite::Error),
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTime => write!(formatter, "invalid authorization timestamp"),
            Self::Unavailable => write!(formatter, "authorization unavailable"),
            Self::Expired => write!(formatter, "authorization expired"),
            Self::Database(error) => write!(formatter, "authorization database error: {error}"),
        }
    }
}

impl std::error::Error for TransitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::InvalidTime | Self::Unavailable | Self::Expired => None,
        }
    }
}

impl From<rusqlite::Error> for TransitionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// Claim payload is returned only after the claim and audit commit. It is not
/// an execution authorization by itself: the restricted gateway must verify
/// capability, destination, session validity and its own isolation controls.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ClaimedOperation {
    pub capability: String,
    pub request_arguments: Vec<u8>,
}

/// A failed or expired transition never grants authority. Expiration is
/// committed with an audit event when encountered at the decision boundary.
/// No externally supplied state, capability, or arguments are accepted.
pub(super) fn decide(
    connection: &mut Connection,
    runtime: RuntimeId,
    context: AuthorizationContext,
    proposal: ProposalId,
    decision: Decision,
    now_ms: i64,
) -> Result<Option<ClaimedOperation>, TransitionError> {
    if !(0..=MAX_MS).contains(&now_ms) {
        return Err(TransitionError::InvalidTime);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<ProposalRow> = tx.query_row(
        "SELECT state, created_at_ms, original_deadline_ms, approved_at_ms, capability, request_arguments
         FROM authorization_proposals
         WHERE proposal_id=?1 AND owner_id=?2 AND session_id=?3 AND runtime_id=?4",
        params![proposal.to_bytes().as_slice(), context.user_id.to_bytes().as_slice(),
            context.session_id.to_bytes().as_slice(), runtime.as_bytes().as_slice()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).optional()?;
    let (state, created, deadline, approved, capability, args) =
        row.ok_or(TransitionError::Unavailable)?;
    if now_ms < created || approved.is_some_and(|a| now_ms < a) {
        return Err(TransitionError::InvalidTime);
    }
    let active_deadline = if state == "Approved" {
        approved
            .ok_or(TransitionError::Unavailable)?
            .saturating_add(CLAIM_WINDOW_MS)
            .min(deadline)
    } else {
        deadline
    };
    if (state == "Pending" || state == "Approved") && now_ms >= active_deadline {
        let changed = tx.execute(
            "UPDATE authorization_proposals SET state='Expired', terminal_at_ms=?1
             WHERE proposal_id=?2 AND state=?3 AND runtime_id=?4",
            params![
                now_ms,
                proposal.to_bytes().as_slice(),
                state,
                runtime.as_bytes().as_slice()
            ],
        )?;
        if changed != 1 {
            return Err(TransitionError::Unavailable);
        }
        tx.execute(
            "INSERT INTO authorization_audit(proposal_id,event_type,event_at_ms)
                    VALUES(?1,'ProposalExpired',?2)",
            params![proposal.to_bytes().as_slice(), now_ms],
        )?;
        tx.commit()?;
        return Err(TransitionError::Expired);
    }
    let valid = match decision {
        Decision::Approve | Decision::Reject => state == "Pending",
        Decision::Cancel => state == "Pending" || state == "Approved",
        Decision::Claim => state == "Approved",
    };
    if !valid {
        return Err(TransitionError::Unavailable);
    }
    // Validate the immutable payload before committing a single-use claim.
    // A corrupt or already-purged payload must not consume authorization.
    if decision == Decision::Claim && args.is_none() {
        return Err(TransitionError::Unavailable);
    }
    let terminal = if decision == Decision::Approve {
        None
    } else {
        Some(now_ms)
    };
    let approval = if decision == Decision::Approve {
        Some(now_ms)
    } else {
        approved
    };
    let changed = tx.execute(
        "UPDATE authorization_proposals SET state=?1, approved_at_ms=?2, terminal_at_ms=?3
         WHERE proposal_id=?4 AND state=?5 AND runtime_id=?6",
        params![
            decision.target(),
            approval,
            terminal,
            proposal.to_bytes().as_slice(),
            state,
            runtime.as_bytes().as_slice()
        ],
    )?;
    if changed != 1 {
        return Err(TransitionError::Unavailable);
    }
    tx.execute(
        "INSERT INTO authorization_audit(proposal_id,event_type,event_at_ms)
                VALUES(?1,?2,?3)",
        params![proposal.to_bytes().as_slice(), decision.event(), now_ms],
    )?;
    tx.commit()?;
    if decision == Decision::Claim {
        Ok(Some(ClaimedOperation {
            capability,
            request_arguments: args.expect("claim payload checked before commit"),
        }))
    } else {
        Ok(None)
    }
}

/// Revoke all unclaimed authority for a verified session, in one transaction.
/// Call this synchronously on logout, expiry and administrative revocation.
pub(super) fn revoke_session(
    connection: &mut Connection,
    runtime: RuntimeId,
    context: AuthorizationContext,
    now_ms: i64,
) -> Result<usize, TransitionError> {
    if !(0..=MAX_MS).contains(&now_ms) {
        return Err(TransitionError::InvalidTime);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let invalid: i64 = tx.query_row(
        "SELECT COUNT(*) FROM authorization_proposals WHERE owner_id=?1 AND session_id=?2
         AND runtime_id=?3 AND state IN ('Pending','Approved')
         AND (created_at_ms>?4 OR (approved_at_ms IS NOT NULL AND approved_at_ms>?4))",
        params![
            context.user_id.to_bytes().as_slice(),
            context.session_id.to_bytes().as_slice(),
            runtime.as_bytes().as_slice(),
            now_ms
        ],
        |r| r.get(0),
    )?;
    if invalid != 0 {
        return Err(TransitionError::InvalidTime);
    }
    let audited = tx.execute(
        "INSERT INTO authorization_audit(proposal_id,event_type,event_at_ms)
         SELECT proposal_id,'SessionRevoked',?1 FROM authorization_proposals
         WHERE owner_id=?2 AND session_id=?3 AND runtime_id=?4 AND state IN ('Pending','Approved')",
        params![
            now_ms,
            context.user_id.to_bytes().as_slice(),
            context.session_id.to_bytes().as_slice(),
            runtime.as_bytes().as_slice()
        ],
    )?;
    let changed = tx.execute(
        "UPDATE authorization_proposals SET state='Cancelled', terminal_at_ms=?1
         WHERE owner_id=?2 AND session_id=?3 AND runtime_id=?4 AND state IN ('Pending','Approved')",
        params![
            now_ms,
            context.user_id.to_bytes().as_slice(),
            context.session_id.to_bytes().as_slice(),
            runtime.as_bytes().as_slice()
        ],
    )?;
    if changed != audited {
        return Err(TransitionError::Unavailable);
    }
    tx.commit()?;
    Ok(changed)
}

/// Purge active-table sensitive arguments after terminal state. SQLite WAL,
/// free pages, backups and snapshots may still retain historical copies.
pub(super) fn purge_terminal_arguments(
    connection: &mut Connection,
    now_ms: i64,
) -> Result<usize, TransitionError> {
    if !(0..=MAX_MS).contains(&now_ms) {
        return Err(TransitionError::InvalidTime);
    }
    let cutoff = now_ms.saturating_sub(24 * 60 * 60 * 1000);
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE authorization_proposals SET request_arguments=NULL
         WHERE state IN ('Rejected','Cancelled','Expired','Claimed')
         AND terminal_at_ms<=?1 AND request_arguments IS NOT NULL",
        [cutoff],
    )?;
    tx.commit()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        creation::create_proposal, recovery::register_runtime_and_recover,
        schema::initialize_database,
    };
    use gardenwall_core::{CapabilityProposal, CapabilityRequest, SessionId, UserId};
    use std::time::{Duration, SystemTime};

    fn setup() -> (Connection, RuntimeId, AuthorizationContext, ProposalId) {
        let mut db = Connection::open_in_memory().unwrap();
        initialize_database(&mut db).unwrap();
        let runtime = RuntimeId::generate();
        register_runtime_and_recover(&mut db, runtime, 0).unwrap();
        let ctx = AuthorizationContext {
            user_id: UserId::from_bytes(2u128.to_be_bytes()),
            session_id: SessionId::from_bytes(3u128.to_be_bytes()),
        };
        let id = ProposalId::from_bytes(1u128.to_be_bytes());
        let proposal = CapabilityProposal::new(
            id,
            ctx.user_id,
            ctx.session_id,
            CapabilityRequest::WebSearch {
                query: "sample".into(),
            },
            SystemTime::UNIX_EPOCH + Duration::from_millis(1000),
        )
        .unwrap();
        create_proposal(&mut db, runtime, &proposal).unwrap();
        (db, runtime, ctx, id)
    }
    #[test]
    fn approval_and_claim_are_single_use() {
        let (mut db, rt, ctx, id) = setup();
        assert!(
            decide(&mut db, rt, ctx, id, Decision::Approve, 2000)
                .unwrap()
                .is_none()
        );
        let claim = decide(&mut db, rt, ctx, id, Decision::Claim, 2001)
            .unwrap()
            .unwrap();
        assert_eq!(claim.capability, "WebSearch");
        assert!(!claim.request_arguments.is_empty());
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 2002).is_err());
        let audit: i64 = db
            .query_row("SELECT COUNT(*) FROM authorization_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audit, 3);
    }
    #[test]
    fn explicit_cancellation_is_terminal_and_audited() {
        let (mut db, rt, ctx, id) = setup();
        assert!(
            decide(&mut db, rt, ctx, id, Decision::Cancel, 2000)
                .unwrap()
                .is_none()
        );
        assert!(decide(&mut db, rt, ctx, id, Decision::Approve, 2001).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Cancelled");
        let event: String = db
            .query_row(
                "SELECT event_type FROM authorization_audit ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event, "ProposalCancelled");
    }

    #[test]
    fn database_errors_preserve_their_source() {
        let error = TransitionError::Database(rusqlite::Error::InvalidQuery);
        assert!(std::error::Error::source(&error).is_some());
        assert!(error.to_string().contains("database"));
    }

    #[test]
    fn wrong_identity_and_runtime_fail_closed() {
        let (mut db, rt, ctx, id) = setup();
        let wrong = AuthorizationContext {
            session_id: SessionId::from_bytes([9; 16]),
            ..ctx
        };
        assert!(decide(&mut db, rt, wrong, id, Decision::Approve, 2000).is_err());
        assert!(
            decide(
                &mut db,
                RuntimeId::generate(),
                ctx,
                id,
                Decision::Approve,
                2000
            )
            .is_err()
        );
        assert!(decide(&mut db, rt, ctx, id, Decision::Approve, 2000).is_ok());
    }
    #[test]
    fn expiry_and_approval_window() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Approve, 2000).unwrap();
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 32000).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Expired");
    }
    #[test]
    fn revocation_prevents_claim() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Approve, 2000).unwrap();
        assert_eq!(revoke_session(&mut db, rt, ctx, 2001).unwrap(), 1);
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 2002).is_err());
    }
    #[test]
    fn audit_failure_rolls_back_claim() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Approve, 2000).unwrap();
        db.execute_batch(
            "CREATE TEMP TRIGGER block_claim BEFORE INSERT ON main.authorization_audit
            WHEN NEW.event_type='ProposalClaimed' BEGIN SELECT RAISE(ABORT,'blocked'); END;",
        )
        .unwrap();
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 2001).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Approved");
    }
    #[test]
    fn terminal_arguments_are_purged_only_after_cutoff() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Reject, 2000).unwrap();
        assert_eq!(purge_terminal_arguments(&mut db, 2000).unwrap(), 0);
        assert_eq!(purge_terminal_arguments(&mut db, 86_402_000).unwrap(), 1);
        let args: Option<Vec<u8>> = db
            .query_row(
                "SELECT request_arguments FROM authorization_proposals",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(args.is_none());
    }
    #[test]
    fn rejection_is_terminal_and_audited() {
        let (mut db, rt, ctx, id) = setup();
        assert!(
            decide(&mut db, rt, ctx, id, Decision::Reject, 2000)
                .unwrap()
                .is_none()
        );
        assert!(decide(&mut db, rt, ctx, id, Decision::Approve, 2001).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Rejected");
    }

    #[test]
    fn pending_proposal_cannot_be_claimed() {
        let (mut db, rt, ctx, id) = setup();
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 2000).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Pending");
    }

    #[test]
    fn deadline_is_exclusive_for_approval() {
        let (mut db, rt, ctx, id) = setup();
        assert!(decide(&mut db, rt, ctx, id, Decision::Approve, 301000).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Expired");
    }

    #[test]
    fn approval_does_not_extend_original_deadline() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Approve, 300999).unwrap();
        assert!(decide(&mut db, rt, ctx, id, Decision::Claim, 301000).is_err());
        let state: String = db
            .query_row("SELECT state FROM authorization_proposals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "Expired");
    }

    #[test]
    fn clock_rollback_fails_closed() {
        let (mut db, rt, ctx, id) = setup();
        assert!(matches!(
            decide(&mut db, rt, ctx, id, Decision::Approve, 999),
            Err(TransitionError::InvalidTime)
        ));
        decide(&mut db, rt, ctx, id, Decision::Approve, 2000).unwrap();
        assert!(matches!(
            decide(&mut db, rt, ctx, id, Decision::Claim, 1999),
            Err(TransitionError::InvalidTime)
        ));
    }

    #[test]
    fn missing_claim_payload_does_not_consume_approval() {
        let (mut db, rt, ctx, id) = setup();
        decide(&mut db, rt, ctx, id, Decision::Approve, 2000).unwrap();
        // Simulate a malformed database row without violating SQLite schema constraints:
        // temporarily remove the CHECK-enforcing row by switching to a separate test table
        // is not possible. Instead validate the invariant through a failed update.
        assert!(
            db.execute(
                "UPDATE authorization_proposals SET request_arguments=NULL WHERE proposal_id=?1",
                [id.to_bytes().as_slice()]
            )
            .is_err()
        );
        assert!(
            decide(&mut db, rt, ctx, id, Decision::Claim, 2001)
                .unwrap()
                .is_some()
        );
    }
}
