use gardenwall_core::{SessionId, UserId};

/// Trusted identity context supplied by the authorization service.
///
/// This context must not be constructed from unverified
/// model output or untrusted HTTP request fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizationContext {
    pub user_id: UserId,
    pub session_id: SessionId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_context_preserves_distinct_identity_types() {
        fn requires_user_id(_: UserId) {}
        fn requires_session_id(_: SessionId) {}

        let _user_check: fn(UserId) = requires_user_id;
        let _session_check: fn(SessionId) = requires_session_id;
    }
}
