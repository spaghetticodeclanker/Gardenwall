//! Gardenwall's egress gateway (ADR-007 §10, ADR-011 §2.6, ADR-012 §5).
//!
//! Everything that leaves the machine on a model's behalf goes through
//! here, after the trusted approval flow has authorized it:
//! - [`addr`]: which IP addresses are public. Loopback, private, link-local,
//!   CGNAT, multicast, reserved and documentation ranges are never contacted,
//!   including through IPv4-mapped and NAT64 IPv6 forms.
//! - [`target`]: strict URL validation. HTTPS only, no embedded credentials,
//!   no fragments. The one configured local exception (a loopback SearXNG)
//!   is matched by exact scheme, host and port.
//! - [`fetch`]: resolves DNS itself, checks every address, and connects to
//!   the checked address (no rebinding window). Same-origin redirects are
//!   revalidated and followed; a cross-origin redirect stops and needs a new
//!   approval. Headers, sizes and times are bounded.
//! - [`scan`]: outbound secret detection, as defence in depth. A request
//!   that looks like it carries a credential is refused, and no approval or
//!   grant overrides that.
//!
//! Approval never overrides any of these checks.

#![forbid(unsafe_code)]

pub mod addr;
#[cfg(feature = "fetch")]
pub mod fetch;
pub mod scan;
pub mod target;

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressError {
    InvalidUrl(&'static str),
    /// The destination resolves only to addresses that must not be contacted.
    PrivateDestination,
    Dns(String),
    /// A redirect to another origin: contacting it needs a new approval.
    CrossOriginRedirect(String),
    TooManyRedirects,
    Status(u16),
    Http(String),
    Timeout,
    /// The request looks like it contains a credential (scanner finding).
    SecretDetected(Vec<&'static str>),
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(why) => write!(f, "the address is not allowed: {why}"),
            Self::PrivateDestination => f.write_str(
                "the address points to this computer or a private network, which web tools may not contact",
            ),
            Self::Dns(e) => write!(f, "the address could not be resolved: {e}"),
            Self::CrossOriginRedirect(to) => write!(
                f,
                "the site redirected to a different site ({to}); fetching that needs a new approval"
            ),
            Self::TooManyRedirects => f.write_str("too many redirects"),
            Self::Status(code) => write!(f, "the site answered with HTTP {code}"),
            Self::Http(e) => write!(f, "the request failed: {e}"),
            Self::Timeout => f.write_str("the site did not answer in time"),
            Self::SecretDetected(kinds) => write!(
                f,
                "blocked: the request appears to contain a secret ({})",
                kinds.join(", ")
            ),
        }
    }
}

impl std::error::Error for EgressError {}
