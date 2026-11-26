//! URL validation (ADR-007 §10.2–10.3).

use std::net::IpAddr;

use url::{Host, Url};

use crate::EgressError;
use crate::addr::is_public;

/// Longest URL accepted.
pub const MAX_URL_CHARS: usize = 4096;

/// A local service web tools may reach despite the private-address ban:
/// the user's configured SearXNG. It is
/// matched by exact scheme, host and port, never as a blanket exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalException {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl LocalException {
    /// The exception for a configured base URL, if it is a loopback one.
    /// Public instances need no exception (and must use HTTPS).
    pub fn for_base(base: &Url) -> Option<Self> {
        let host = match base.host()? {
            Host::Ipv4(ip) => IpAddr::V4(ip),
            Host::Ipv6(ip) => IpAddr::V6(ip),
            Host::Domain(d) if d.eq_ignore_ascii_case("localhost") => {
                return Some(Self {
                    scheme: base.scheme().into(),
                    host: "localhost".into(),
                    port: base.port_or_known_default()?,
                });
            }
            Host::Domain(_) => return None,
        };
        if !host.is_loopback() {
            return None;
        }
        Some(Self {
            scheme: base.scheme().into(),
            host: base.host_str()?.to_string(),
            port: base.port_or_known_default()?,
        })
    }

    pub fn matches(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.host_str() == Some(self.host.as_str())
            && url.port_or_known_default() == Some(self.port)
    }
}

/// A validated destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: Url,
    /// Matches a configured local exception (skips the address check).
    pub local: bool,
}

impl Target {
    /// Scheme, host and effective port: what "same origin" compares.
    pub fn origin(&self) -> String {
        origin_of(&self.url)
    }
}

pub fn origin_of(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

/// Parse and validate a URL proposed by the model (or a redirect target).
pub fn validate(raw: &str, exceptions: &[LocalException]) -> Result<Target, EgressError> {
    if raw.len() > MAX_URL_CHARS {
        return Err(EgressError::InvalidUrl("it is too long"));
    }
    let mut url =
        Url::parse(raw.trim()).map_err(|_| EgressError::InvalidUrl("it is not a valid URL"))?;
    check(&mut url, exceptions)
}

/// Validate an already parsed URL (used for redirects).
pub fn check(url: &mut Url, exceptions: &[LocalException]) -> Result<Target, EgressError> {
    url.set_fragment(None);
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EgressError::InvalidUrl(
            "it contains a user name or password",
        ));
    }
    if url.host().is_none() {
        return Err(EgressError::InvalidUrl("it has no host"));
    }
    if exceptions.iter().any(|e| e.matches(url)) {
        return Ok(Target {
            url: url.clone(),
            local: true,
        });
    }
    if url.scheme() != "https" {
        return Err(EgressError::InvalidUrl(
            "only https:// addresses are allowed",
        ));
    }
    match url.host() {
        Some(Host::Ipv4(ip)) if !is_public(IpAddr::V4(ip)) => {
            return Err(EgressError::PrivateDestination);
        }
        Some(Host::Ipv6(ip)) if !is_public(IpAddr::V6(ip)) => {
            return Err(EgressError::PrivateDestination);
        }
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") || !d.contains('.') {
                return Err(EgressError::PrivateDestination);
            }
        }
        _ => {}
    }
    Ok(Target {
        url: url.clone(),
        local: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searx() -> Vec<LocalException> {
        vec![LocalException::for_base(&Url::parse("http://127.0.0.1:8888/").unwrap()).unwrap()]
    }

    #[test]
    fn only_public_https_urls_pass() {
        let t = validate("https://docs.rs/serde#top", &[]).unwrap();
        assert_eq!(t.url.as_str(), "https://docs.rs/serde");
        assert_eq!(t.origin(), "https://docs.rs:443");
        for (bad, why) in [
            ("http://docs.rs", "scheme"),
            ("ftp://docs.rs", "scheme"),
            ("file:///etc/passwd", "file"),
            ("https://user:pw@docs.rs", "credentials"),
            ("https://127.0.0.1/", "loopback"),
            ("https://[::1]/", "loopback v6"),
            ("https://10.0.0.5/", "private"),
            ("https://169.254.169.254/latest/meta-data", "metadata"),
            ("https://localhost/", "localhost"),
            ("https://printer/", "single label"),
            ("https://a.localhost/", "localhost suffix"),
            ("not a url", "garbage"),
        ] {
            assert!(validate(bad, &[]).is_err(), "{why}: {bad}");
        }
        assert!(validate(&format!("https://a.io/{}", "x".repeat(MAX_URL_CHARS)), &[]).is_err());
    }

    #[test]
    fn the_local_exception_matches_exactly() {
        let ex = searx();
        let t = validate("http://127.0.0.1:8888/search?q=x", &ex).unwrap();
        assert!(t.local);
        assert!(validate("http://127.0.0.1:8889/", &ex).is_err());
        assert!(validate("https://127.0.0.1:8888/", &ex).is_err());
        assert!(validate("http://127.0.0.2:8888/", &ex).is_err());
        // A public base needs no exception.
        assert!(
            LocalException::for_base(&Url::parse("https://searx.example.org").unwrap()).is_none()
        );
        assert!(LocalException::for_base(&Url::parse("http://localhost:8080").unwrap()).is_some());
        assert!(
            LocalException::for_base(&Url::parse("http://192.168.1.5:8080").unwrap()).is_none()
        );
    }
}
