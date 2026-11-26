use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    WebSearch,
    WebpageRetrieval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityRequest {
    WebSearch { query: String },
    WebpageRetrieval { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProposalId(u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u128);

// Stable, platform-independent encoding for persisted identifiers.
// Decoding bytes does not establish authorization or ownership.
macro_rules! impl_identifier_encoding {
    ($identifier:ident) => {
        impl $identifier {
            pub fn to_bytes(self) -> [u8; 16] {
                self.0.to_be_bytes()
            }

            pub fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(u128::from_be_bytes(bytes))
            }
        }
    };
}

impl_identifier_encoding!(ProposalId);
impl_identifier_encoding!(UserId);
impl_identifier_encoding!(SessionId);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationState {
    Pending,
    Approved,
    Rejected,
    Cancelled,
    Expired,
    Claimed,
}

impl AuthorizationState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Approved)
                | (Self::Pending, Self::Rejected)
                | (Self::Pending, Self::Cancelled)
                | (Self::Pending, Self::Expired)
                | (Self::Approved, Self::Claimed)
                | (Self::Approved, Self::Cancelled)
                | (Self::Approved, Self::Expired)
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapabilityProposal {
    id: ProposalId,
    owner: UserId,
    session: SessionId,
    request: CapabilityRequest,
    state: AuthorizationState,
    created_at: SystemTime,
    original_deadline: SystemTime,
    approved_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidStateTransition {
    pub from: AuthorizationState,
    pub to: AuthorizationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalTransitionError {
    InvalidState(InvalidStateTransition),
    Expired,
    InvalidTimestamp,
}

impl CapabilityProposal {
    pub fn new(
        id: ProposalId,
        owner: UserId,
        session: SessionId,
        request: CapabilityRequest,
        created_at: SystemTime,
    ) -> Option<Self> {
        let original_deadline = created_at.checked_add(PENDING_LIFETIME)?;

        Some(Self {
            id,
            owner,
            session,
            request,
            state: AuthorizationState::Pending,
            created_at,
            original_deadline,
            approved_at: None,
        })
    }

    pub fn id(&self) -> ProposalId {
        self.id
    }

    pub fn owner(&self) -> UserId {
        self.owner
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn request(&self) -> &CapabilityRequest {
        &self.request
    }

    pub fn state(&self) -> AuthorizationState {
        self.state
    }

    pub fn transition_to(
        &mut self,
        next: AuthorizationState,
    ) -> Result<(), InvalidStateTransition> {
        if !self.state.can_transition_to(next) {
            return Err(InvalidStateTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        Ok(())
    }

    pub fn approve(&mut self, now: SystemTime) -> Result<(), ProposalTransitionError> {
        self.ensure_transition(AuthorizationState::Approved)?;

        if now < self.created_at {
            return Err(ProposalTransitionError::InvalidTimestamp);
        }

        if now >= self.original_deadline {
            return Err(ProposalTransitionError::Expired);
        }

        self.transition_to(AuthorizationState::Approved)
            .map_err(ProposalTransitionError::InvalidState)?;

        self.approved_at = Some(now);
        Ok(())
    }

    pub fn claim(&mut self, now: SystemTime) -> Result<(), ProposalTransitionError> {
        self.ensure_transition(AuthorizationState::Claimed)?;

        let approved_at = self
            .approved_at
            .ok_or(ProposalTransitionError::InvalidTimestamp)?;

        if now < self.created_at || now < approved_at {
            return Err(ProposalTransitionError::InvalidTimestamp);
        }

        let deadline = claim_deadline(self.original_deadline, approved_at)
            .ok_or(ProposalTransitionError::InvalidTimestamp)?;

        if now >= deadline {
            return Err(ProposalTransitionError::Expired);
        }

        self.transition_to(AuthorizationState::Claimed)
            .map_err(ProposalTransitionError::InvalidState)
    }

    pub fn reject(&mut self) -> Result<(), ProposalTransitionError> {
        self.transition_to(AuthorizationState::Rejected)
            .map_err(ProposalTransitionError::InvalidState)
    }

    pub fn cancel(&mut self) -> Result<(), ProposalTransitionError> {
        self.transition_to(AuthorizationState::Cancelled)
            .map_err(ProposalTransitionError::InvalidState)
    }

    pub fn expire(&mut self, now: SystemTime) -> Result<(), ProposalTransitionError> {
        self.ensure_transition(AuthorizationState::Expired)?;

        if now < self.created_at {
            return Err(ProposalTransitionError::InvalidTimestamp);
        }

        let deadline = match self.state {
            AuthorizationState::Pending => self.original_deadline,
            AuthorizationState::Approved => {
                let approved_at = self
                    .approved_at
                    .ok_or(ProposalTransitionError::InvalidTimestamp)?;

                if now < approved_at {
                    return Err(ProposalTransitionError::InvalidTimestamp);
                }

                claim_deadline(self.original_deadline, approved_at)
                    .ok_or(ProposalTransitionError::InvalidTimestamp)?
            }
            _ => unreachable!("state checked above"),
        };

        if now < deadline {
            return Err(ProposalTransitionError::InvalidTimestamp);
        }

        self.transition_to(AuthorizationState::Expired)
            .map_err(ProposalTransitionError::InvalidState)
    }

    fn ensure_transition(&self, next: AuthorizationState) -> Result<(), ProposalTransitionError> {
        if !self.state.can_transition_to(next) {
            return Err(ProposalTransitionError::InvalidState(
                InvalidStateTransition {
                    from: self.state,
                    to: next,
                },
            ));
        }

        Ok(())
    }

    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    pub fn original_deadline(&self) -> SystemTime {
        self.original_deadline
    }

    pub fn approved_at(&self) -> Option<SystemTime> {
        self.approved_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Completed,
    Failed,
    Unknown,
}

pub const PENDING_LIFETIME: Duration = Duration::from_secs(5 * 60);

pub const APPROVED_CLAIM_LIFETIME: Duration = Duration::from_secs(30);

pub fn claim_deadline(
    original_deadline: SystemTime,
    approved_at: SystemTime,
) -> Option<SystemTime> {
    let approval_deadline = approved_at.checked_add(APPROVED_CLAIM_LIFETIME)?;

    Some(original_deadline.min(approval_deadline))
}

impl CapabilityRequest {
    pub fn capability(&self) -> Capability {
        match self {
            Self::WebSearch { .. } => Capability::WebSearch,
            Self::WebpageRetrieval { .. } => Capability::WebpageRetrieval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_proposal() -> CapabilityProposal {
        CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            CapabilityRequest::WebSearch {
                query: "authorization test".to_string(),
            },
            SystemTime::UNIX_EPOCH,
        )
        .expect("test proposal should have a valid deadline")
    }

    #[test]
    fn web_search_request_has_correct_capability() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust security".to_string(),
        };

        assert_eq!(request.capability(), Capability::WebSearch);
    }

    #[test]
    fn webpage_retrieval_has_correct_capability() {
        let request = CapabilityRequest::WebpageRetrieval {
            url: "https://example.com".to_string(),
        };

        assert_eq!(request.capability(), Capability::WebpageRetrieval);
    }

    #[test]
    fn authorization_state_distinguishes_approval_from_claim() {
        assert_ne!(AuthorizationState::Approved, AuthorizationState::Claimed);
    }

    #[test]
    fn execution_outcome_is_separate_from_authorization() {
        let authorization = AuthorizationState::Claimed;
        let outcome = ExecutionOutcome::Unknown;

        assert_eq!(authorization, AuthorizationState::Claimed);
        assert_eq!(outcome, ExecutionOutcome::Unknown);
    }

    #[test]
    fn pending_has_only_expected_transitions() {
        use AuthorizationState::*;

        assert!(Pending.can_transition_to(Approved));
        assert!(Pending.can_transition_to(Rejected));
        assert!(Pending.can_transition_to(Cancelled));
        assert!(Pending.can_transition_to(Expired));

        assert!(!Pending.can_transition_to(Claimed));
        assert!(!Pending.can_transition_to(Pending));
    }

    #[test]
    fn approved_has_only_expected_transitions() {
        use AuthorizationState::*;

        assert!(Approved.can_transition_to(Claimed));
        assert!(Approved.can_transition_to(Cancelled));
        assert!(Approved.can_transition_to(Expired));

        assert!(!Approved.can_transition_to(Pending));
        assert!(!Approved.can_transition_to(Rejected));
        assert!(!Approved.can_transition_to(Approved));
    }

    #[test]
    fn claimed_cannot_be_reused() {
        use AuthorizationState::*;

        assert!(!Claimed.can_transition_to(Approved));
        assert!(!Claimed.can_transition_to(Claimed));
        assert!(!Claimed.can_transition_to(Pending));
    }

    #[test]
    fn terminal_states_cannot_transition() {
        use AuthorizationState::*;

        let all_states = [Pending, Approved, Rejected, Cancelled, Expired, Claimed];

        for terminal in [Rejected, Cancelled, Expired] {
            for next in all_states {
                assert!(
                    !terminal.can_transition_to(next),
                    "{terminal:?} unexpectedly allows transition to {next:?}"
                );
            }
        }
    }

    #[test]
    fn new_proposal_starts_pending() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust security".to_string(),
        };

        let created_at = SystemTime::UNIX_EPOCH;

        let proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request.clone(),
            created_at,
        )
        .expect("test proposal should have a valid deadline");

        assert_eq!(proposal.state(), AuthorizationState::Pending);
        assert_eq!(proposal.request(), &request);
        assert_eq!(proposal.id(), ProposalId(100));
        assert_eq!(proposal.owner(), UserId(200));
        assert_eq!(proposal.session(), SessionId(300));
        assert_eq!(proposal.created_at(), created_at);
        assert_eq!(proposal.original_deadline(), created_at + PENDING_LIFETIME);
        assert_eq!(proposal.approved_at(), None);
    }

    #[test]
    fn valid_proposal_transition_changes_state() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust security".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            SystemTime::UNIX_EPOCH,
        )
        .expect("test proposal should have a valid deadline");

        assert_eq!(proposal.transition_to(AuthorizationState::Approved), Ok(()));

        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }

    #[test]
    fn invalid_proposal_transition_preserves_state() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust security".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            SystemTime::UNIX_EPOCH,
        )
        .expect("test proposal should have a valid deadline");

        assert_eq!(
            proposal.transition_to(AuthorizationState::Claimed),
            Err(InvalidStateTransition {
                from: AuthorizationState::Pending,
                to: AuthorizationState::Claimed,
            })
        );

        assert_eq!(proposal.state(), AuthorizationState::Pending);
    }

    #[test]
    fn claimed_proposal_cannot_be_reapproved() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust security".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            SystemTime::UNIX_EPOCH,
        )
        .expect("test proposal should have a valid deadline");

        proposal
            .transition_to(AuthorizationState::Approved)
            .unwrap();

        proposal.transition_to(AuthorizationState::Claimed).unwrap();

        assert_eq!(
            proposal.transition_to(AuthorizationState::Approved),
            Err(InvalidStateTransition {
                from: AuthorizationState::Claimed,
                to: AuthorizationState::Approved,
            })
        );

        assert_eq!(proposal.state(), AuthorizationState::Claimed);
    }

    #[test]
    fn proposal_preserves_identity_and_ownership() {
        let request = CapabilityRequest::WebSearch {
            query: "Rust authorization".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request.clone(),
            SystemTime::UNIX_EPOCH,
        )
        .expect("test proposal should have a valid deadline");

        assert_eq!(proposal.id(), ProposalId(100));
        assert_eq!(proposal.owner(), UserId(200));
        assert_eq!(proposal.session(), SessionId(300));
        assert_eq!(proposal.request(), &request);

        proposal
            .transition_to(AuthorizationState::Approved)
            .unwrap();

        assert_eq!(proposal.id(), ProposalId(100));
        assert_eq!(proposal.owner(), UserId(200));
        assert_eq!(proposal.session(), SessionId(300));
        assert_eq!(proposal.request(), &request);
        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }

    #[test]
    fn authorization_transition_matrix_is_exact() {
        use AuthorizationState::*;

        let states = [Pending, Approved, Rejected, Cancelled, Expired, Claimed];

        let allowed = [
            (Pending, Approved),
            (Pending, Rejected),
            (Pending, Cancelled),
            (Pending, Expired),
            (Approved, Claimed),
            (Approved, Cancelled),
            (Approved, Expired),
        ];

        for from in states {
            for to in states {
                let expected = allowed.contains(&(from, to));

                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "Unexpected transition rule: {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn proposal_identifiers_have_value_equality() {
        let first = ProposalId(123);
        let same = ProposalId(123);
        let different = ProposalId(456);

        assert_eq!(first, same);
        assert_ne!(first, different);
    }

    #[test]
    fn user_and_session_identifiers_are_distinct_types() {
        let user = UserId(123);
        let session = SessionId(123);

        assert_eq!(user.0, session.0);

        // UserId and SessionId are different Rust types,
        // even when their underlying numeric values match.
    }

    #[test]
    fn proposal_id_round_trips_through_bytes() {
        let original = ProposalId(u128::MAX - 123);
        let encoded = original.to_bytes();
        let decoded = ProposalId::from_bytes(encoded);

        assert_eq!(encoded.len(), 16);
        assert_eq!(decoded, original);
    }

    #[test]
    fn user_id_round_trips_through_bytes() {
        let original = UserId(123456789);
        assert_eq!(UserId::from_bytes(original.to_bytes()), original);
    }

    #[test]
    fn session_id_round_trips_through_bytes() {
        let original = SessionId(987654321);
        assert_eq!(SessionId::from_bytes(original.to_bytes()), original);
    }

    #[test]
    fn identifier_encoding_uses_big_endian_order() {
        let original = ProposalId(0x0102030405060708090A0B0C0D0E0F10);

        assert_eq!(
            original.to_bytes(),
            [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
                0x0F, 0x10,
            ]
        );
    }

    #[test]
    fn different_identifier_types_remain_distinct() {
        let bytes = [42_u8; 16];
        let proposal = ProposalId::from_bytes(bytes);
        let user = UserId::from_bytes(bytes);
        let session = SessionId::from_bytes(bytes);

        assert_eq!(proposal.to_bytes(), user.to_bytes());
        assert_eq!(user.to_bytes(), session.to_bytes());
        // These remain separate Rust types despite identical encodings.
    }

    #[test]
    fn early_approval_gets_thirty_second_claim_window() {
        let created_at = SystemTime::UNIX_EPOCH;
        let original_deadline = created_at + PENDING_LIFETIME;
        let approved_at = created_at + Duration::from_secs(60);

        let deadline = claim_deadline(original_deadline, approved_at);

        assert_eq!(deadline, Some(approved_at + Duration::from_secs(30)));
    }

    #[test]
    fn late_approval_cannot_extend_original_deadline() {
        let created_at = SystemTime::UNIX_EPOCH;
        let original_deadline = created_at + PENDING_LIFETIME;
        let approved_at = created_at + Duration::from_secs(290);

        let deadline = claim_deadline(original_deadline, approved_at);

        assert_eq!(deadline, Some(original_deadline));
    }

    #[test]
    fn approval_at_original_deadline_grants_no_extra_time() {
        let created_at = SystemTime::UNIX_EPOCH;
        let original_deadline = created_at + PENDING_LIFETIME;

        let deadline = claim_deadline(original_deadline, original_deadline);

        assert_eq!(deadline, Some(original_deadline));
    }

    #[test]
    fn proposal_has_immutable_creation_deadline() {
        let created_at = SystemTime::UNIX_EPOCH;

        let request = CapabilityRequest::WebSearch {
            query: "Rust authorization".to_string(),
        };

        let proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            created_at,
        )
        .expect("valid test timestamp");

        assert_eq!(proposal.created_at(), created_at);
        assert_eq!(proposal.original_deadline(), created_at + PENDING_LIFETIME);
        assert_eq!(proposal.approved_at(), None);
    }

    #[test]
    fn state_transition_does_not_change_original_deadline() {
        let created_at = SystemTime::UNIX_EPOCH;

        let request = CapabilityRequest::WebSearch {
            query: "Rust authorization".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            created_at,
        )
        .expect("valid test timestamp");

        let original_deadline = proposal.original_deadline();

        proposal
            .transition_to(AuthorizationState::Approved)
            .unwrap();

        assert_eq!(proposal.original_deadline(), original_deadline);
        assert_eq!(proposal.created_at(), created_at);
    }

    #[test]
    fn approval_at_deadline_is_rejected() {
        let created_at = SystemTime::UNIX_EPOCH;
        let request = CapabilityRequest::WebSearch {
            query: "deadline test".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            created_at,
        )
        .unwrap();

        assert_eq!(
            proposal.approve(created_at + PENDING_LIFETIME),
            Err(ProposalTransitionError::Expired)
        );

        assert_eq!(proposal.state(), AuthorizationState::Pending);
        assert_eq!(proposal.approved_at(), None);
    }

    #[test]
    fn claim_at_thirty_second_deadline_is_rejected() {
        let created_at = SystemTime::UNIX_EPOCH;
        let request = CapabilityRequest::WebSearch {
            query: "claim deadline test".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            created_at,
        )
        .unwrap();

        let approved_at = created_at + Duration::from_secs(60);

        proposal.approve(approved_at).unwrap();

        assert_eq!(
            proposal.claim(approved_at + APPROVED_CLAIM_LIFETIME),
            Err(ProposalTransitionError::Expired)
        );

        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }

    #[test]
    fn late_approval_has_shortened_claim_window() {
        let created_at = SystemTime::UNIX_EPOCH;
        let request = CapabilityRequest::WebSearch {
            query: "late approval".to_string(),
        };

        let mut proposal = CapabilityProposal::new(
            ProposalId(100),
            UserId(200),
            SessionId(300),
            request,
            created_at,
        )
        .unwrap();

        proposal
            .approve(created_at + Duration::from_secs(290))
            .unwrap();

        assert_eq!(
            proposal.claim(created_at + Duration::from_secs(300)),
            Err(ProposalTransitionError::Expired)
        );

        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }

    #[test]
    fn valid_approval_records_timestamp() {
        let mut proposal = test_proposal();
        let approved_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);

        assert_eq!(proposal.approve(approved_at), Ok(()));
        assert_eq!(proposal.state(), AuthorizationState::Approved);
        assert_eq!(proposal.approved_at(), Some(approved_at));
    }

    #[test]
    fn valid_claim_consumes_authorization() {
        let mut proposal = test_proposal();
        let created_at = SystemTime::UNIX_EPOCH;

        proposal
            .approve(created_at + Duration::from_secs(60))
            .unwrap();

        assert_eq!(proposal.claim(created_at + Duration::from_secs(70)), Ok(()));

        assert_eq!(proposal.state(), AuthorizationState::Claimed);
    }

    #[test]
    fn duplicate_claim_is_rejected() {
        let mut proposal = test_proposal();
        let created_at = SystemTime::UNIX_EPOCH;

        proposal
            .approve(created_at + Duration::from_secs(60))
            .unwrap();

        proposal
            .claim(created_at + Duration::from_secs(70))
            .unwrap();

        assert_eq!(
            proposal.claim(created_at + Duration::from_secs(71)),
            Err(ProposalTransitionError::InvalidState(
                InvalidStateTransition {
                    from: AuthorizationState::Claimed,
                    to: AuthorizationState::Claimed,
                }
            ))
        );

        assert_eq!(proposal.state(), AuthorizationState::Claimed);
    }

    #[test]
    fn approval_before_creation_is_rejected() {
        let mut proposal = test_proposal();
        let before_creation = SystemTime::UNIX_EPOCH - Duration::from_secs(1);

        assert_eq!(
            proposal.approve(before_creation),
            Err(ProposalTransitionError::InvalidTimestamp)
        );

        assert_eq!(proposal.state(), AuthorizationState::Pending);
        assert_eq!(proposal.approved_at(), None);
    }

    #[test]
    fn claim_before_approval_is_rejected() {
        let mut proposal = test_proposal();
        let created_at = SystemTime::UNIX_EPOCH;

        proposal
            .approve(created_at + Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            proposal.claim(created_at + Duration::from_secs(59)),
            Err(ProposalTransitionError::InvalidTimestamp)
        );

        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }

    #[test]
    fn explicit_expiration_prevents_later_claim() {
        let mut proposal = test_proposal();
        let created_at = SystemTime::UNIX_EPOCH;

        proposal
            .approve(created_at + Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            proposal.expire(created_at + Duration::from_secs(90)),
            Ok(())
        );

        assert_eq!(proposal.state(), AuthorizationState::Expired);

        assert_eq!(
            proposal.claim(created_at + Duration::from_secs(91)),
            Err(ProposalTransitionError::InvalidState(
                InvalidStateTransition {
                    from: AuthorizationState::Expired,
                    to: AuthorizationState::Claimed,
                }
            ))
        );
    }

    #[test]
    fn original_deadline_limits_approved_claim() {
        let mut proposal = test_proposal();
        let created_at = SystemTime::UNIX_EPOCH;

        proposal
            .approve(created_at + Duration::from_secs(290))
            .unwrap();

        // This is only 15 seconds after approval,
        // but already 5 seconds beyond the original deadline.
        assert_eq!(
            proposal.claim(created_at + Duration::from_secs(305)),
            Err(ProposalTransitionError::Expired)
        );

        assert_eq!(proposal.state(), AuthorizationState::Approved);
    }
}
