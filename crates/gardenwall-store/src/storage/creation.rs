use super::request_codec::{RequestCodecError, encode_request};
use super::runtime::RuntimeId;
use gardenwall_core::{AuthorizationState, Capability, CapabilityProposal};
use rusqlite::{Connection, TransactionBehavior, params};
use std::time::UNIX_EPOCH;

const MAX_TIMESTAMP_MS: i64 = 253402300799999;
const PENDING_LIFETIME_MS: i64 = 300000;
const CREATION_AUDIT_EVENT: &str = "ProposalCreated";

#[derive(Debug)]
pub(super) enum ProposalCreationError {
    InvalidState,
    InvalidTimestamp,
    InvalidRequest(RequestCodecError),
    Database(rusqlite::Error),
}

impl std::fmt::Display for ProposalCreationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidState => {
                write!(formatter, "proposal is not pending")
            }
            Self::InvalidTimestamp => {
                write!(formatter, "invalid proposal timestamp")
            }
            Self::InvalidRequest(error) => {
                write!(formatter, "invalid proposal request: {error:?}")
            }
            Self::Database(error) => {
                write!(formatter, "proposal database error: {error}")
            }
        }
    }
}

impl std::error::Error for ProposalCreationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for ProposalCreationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

fn timestamp_ms(timestamp: std::time::SystemTime) -> Result<i64, ProposalCreationError> {
    let duration = timestamp
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ProposalCreationError::InvalidTimestamp)?;

    let milliseconds =
        i64::try_from(duration.as_millis()).map_err(|_| ProposalCreationError::InvalidTimestamp)?;

    if !(0..=MAX_TIMESTAMP_MS).contains(&milliseconds) {
        return Err(ProposalCreationError::InvalidTimestamp);
    }

    Ok(milliseconds)
}

pub(super) fn create_proposal(
    connection: &mut Connection,
    runtime_id: RuntimeId,
    proposal: &CapabilityProposal,
) -> Result<(), ProposalCreationError> {
    if proposal.state() != AuthorizationState::Pending || proposal.approved_at().is_some() {
        return Err(ProposalCreationError::InvalidState);
    }

    let created_at_ms = timestamp_ms(proposal.created_at())?;
    let original_deadline_ms = timestamp_ms(proposal.original_deadline())?;

    if original_deadline_ms.checked_sub(created_at_ms) != Some(PENDING_LIFETIME_MS) {
        return Err(ProposalCreationError::InvalidTimestamp);
    }

    let request_arguments =
        encode_request(proposal.request()).map_err(ProposalCreationError::InvalidRequest)?;

    let capability = match proposal.request().capability() {
        Capability::WebSearch => "WebSearch",
        Capability::WebpageRetrieval => "WebpageRetrieval",
    };

    let proposal_id = proposal.id().to_bytes();
    let owner_id = proposal.owner().to_bytes();
    let session_id = proposal.session().to_bytes();
    let runtime_bytes = runtime_id.as_bytes();

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    transaction.execute(
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
            ?4, ?5, 'Pending',
            ?6, ?7,
            NULL, NULL, ?8
        )",
        params![
            proposal_id.as_slice(),
            owner_id.as_slice(),
            session_id.as_slice(),
            capability,
            request_arguments,
            created_at_ms,
            original_deadline_ms,
            runtime_bytes.as_slice(),
        ],
    )?;

    transaction.execute(
        "INSERT INTO authorization_audit (
            proposal_id,
            event_type,
            event_at_ms
        ) VALUES (?1, ?2, ?3)",
        params![proposal_id.as_slice(), CREATION_AUDIT_EVENT, created_at_ms,],
    )?;

    transaction.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{recovery::register_runtime_and_recover, schema::initialize_database};
    use gardenwall_core::{CapabilityRequest, ProposalId, SessionId, UserId};
    use std::time::{Duration, SystemTime};

    fn setup() -> (Connection, RuntimeId) {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&mut connection).unwrap();

        let runtime_id = RuntimeId::generate();

        register_runtime_and_recover(&mut connection, runtime_id, 0).unwrap();

        (connection, runtime_id)
    }

    fn proposal(id: u128) -> CapabilityProposal {
        CapabilityProposal::new(
            ProposalId::from_bytes(id.to_be_bytes()),
            UserId::from_bytes(200_u128.to_be_bytes()),
            SessionId::from_bytes(300_u128.to_be_bytes()),
            CapabilityRequest::WebSearch {
                query: "Rust security".to_owned(),
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap()
    }

    fn row_count(connection: &Connection, table: &str) -> i64 {
        let query = match table {
            "proposals" => "SELECT COUNT(*) FROM authorization_proposals",
            "audit" => "SELECT COUNT(*) FROM authorization_audit",
            _ => panic!("unexpected table"),
        };

        connection.query_row(query, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn creation_inserts_proposal_and_audit() {
        let (mut connection, runtime_id) = setup();

        create_proposal(&mut connection, runtime_id, &proposal(1)).unwrap();

        assert_eq!(row_count(&connection, "proposals"), 1);
        assert_eq!(row_count(&connection, "audit"), 1);

        let (state, deadline, event): (String, i64, String) = connection
            .query_row(
                "SELECT p.state,
                            p.original_deadline_ms,
                            a.event_type
                     FROM authorization_proposals p
                     JOIN authorization_audit a
                       ON p.proposal_id = a.proposal_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(state, "Pending");
        assert_eq!(deadline, 301000);
        assert_eq!(event, "ProposalCreated");
    }

    #[test]
    fn duplicate_proposal_id_is_rejected() {
        let (mut connection, runtime_id) = setup();
        let first = proposal(1);

        create_proposal(&mut connection, runtime_id, &first).unwrap();

        assert!(create_proposal(&mut connection, runtime_id, &first).is_err());

        assert_eq!(row_count(&connection, "proposals"), 1);
        assert_eq!(row_count(&connection, "audit"), 1);
    }

    #[test]
    fn audit_failure_rolls_back_creation() {
        let (mut connection, runtime_id) = setup();

        connection
            .execute_batch(
                "CREATE TEMP TRIGGER reject_creation_audit
                 BEFORE INSERT ON main.authorization_audit
                 WHEN NEW.event_type = 'ProposalCreated'
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'simulated creation audit failure'
                     );
                 END;",
            )
            .unwrap();

        assert!(create_proposal(&mut connection, runtime_id, &proposal(1),).is_err());

        assert_eq!(row_count(&connection, "proposals"), 0);
        assert_eq!(row_count(&connection, "audit"), 0);
    }

    #[test]
    fn unknown_runtime_cannot_create_proposal() {
        let (mut connection, _) = setup();
        let unknown_runtime = RuntimeId::generate();

        assert!(create_proposal(&mut connection, unknown_runtime, &proposal(1),).is_err());

        assert_eq!(row_count(&connection, "proposals"), 0);
        assert_eq!(row_count(&connection, "audit"), 0);
    }

    #[test]
    fn non_pending_proposal_is_rejected() {
        let (mut connection, runtime_id) = setup();
        let mut candidate = proposal(1);

        candidate.reject().unwrap();

        assert!(create_proposal(&mut connection, runtime_id, &candidate,).is_err());

        assert_eq!(row_count(&connection, "proposals"), 0);
    }

    #[test]
    fn pre_epoch_timestamp_is_rejected() {
        let (mut connection, runtime_id) = setup();

        let candidate = CapabilityProposal::new(
            ProposalId::from_bytes(1_u128.to_be_bytes()),
            UserId::from_bytes(2_u128.to_be_bytes()),
            SessionId::from_bytes(3_u128.to_be_bytes()),
            CapabilityRequest::WebSearch {
                query: "test".to_owned(),
            },
            SystemTime::UNIX_EPOCH - Duration::from_secs(1),
        )
        .unwrap();

        assert!(create_proposal(&mut connection, runtime_id, &candidate,).is_err());

        assert_eq!(row_count(&connection, "proposals"), 0);
    }
}
