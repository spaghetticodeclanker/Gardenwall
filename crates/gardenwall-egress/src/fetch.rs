//! DNS-pinned HTTP GET with bounded redirects, size and time
//! (ADR-007 §10.2–10.3).
//!
//! For each hop the gateway resolves the host itself, keeps only public
//! addresses (or the matching local exception), and makes the HTTP client
//! connect to exactly that address, so DNS can't change between check and
//! connection. No proxy, cookies, credentials or referrer are sent; only a
//! fixed set of headers is. A redirect is followed only within the original
//! origin, after full revalidation. A cross-origin redirect ends the attempt
//! and is reported, so the new destination can be approved separately.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header;
use tokio::time::{Instant, timeout_at};
use url::{Host, Url};

use crate::EgressError;
use crate::addr::is_public;
use crate::target::{LocalException, Target, check, origin_of};

/// A generic user agent, so requests don't identify the user's setup.
const USER_AGENT: &str = "Mozilla/5.0 (compatible)";

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_redirects: usize,
    pub connect_timeout: Duration,
    /// For the whole fetch, including redirects and the body.
    pub total_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 5 << 20,
            max_redirects: 5,
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fetched {
    /// The final URL (after same-origin redirects).
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// The body was longer than the limit and was cut.
    pub truncated: bool,
}

/// The address to connect to for `target`, or `None` for an IP literal.
async fn pin(target: &Target, deadline: Instant) -> Result<Option<SocketAddr>, EgressError> {
    let url = &target.url;
    let port = url
        .port_or_known_default()
        .ok_or(EgressError::InvalidUrl("it has no port"))?;
    let Some(Host::Domain(domain)) = url.host() else {
        return Ok(None);
    };
    let addrs: Vec<SocketAddr> = timeout_at(deadline, tokio::net::lookup_host((domain, port)))
        .await
        .map_err(|_| EgressError::Timeout)?
        .map_err(|e| EgressError::Dns(e.to_string()))?
        .collect();
    let chosen = if target.local {
        addrs.into_iter().find(|a| a.ip().is_loopback())
    } else {
        addrs.into_iter().find(|a| is_public(a.ip()))
    };
    chosen.map(Some).ok_or(EgressError::PrivateDestination)
}

fn http_error(e: reqwest::Error) -> EgressError {
    if e.is_timeout() {
        EgressError::Timeout
    } else {
        EgressError::Http(e.without_url().to_string())
    }
}

/// GET `target`, following same-origin redirects only.
pub async fn fetch(
    target: &Target,
    accept: &str,
    exceptions: &[LocalException],
    limits: &Limits,
) -> Result<Fetched, EgressError> {
    fetch_with_headers(target, accept, &[], exceptions, limits).await
}

/// [`fetch`] with extra request headers, such as a search API's key. They
/// are marked sensitive (kept out of debug output) and only ever reach the
/// target's own origin: redirects are followed within it only.
pub async fn fetch_with_headers(
    target: &Target,
    accept: &str,
    headers: &[(&'static str, &str)],
    exceptions: &[LocalException],
    limits: &Limits,
) -> Result<Fetched, EgressError> {
    let deadline = Instant::now() + limits.total_timeout;
    let origin = target.origin();
    let mut current = target.clone();
    for _ in 0..=limits.max_redirects {
        let pinned = pin(&current, deadline).await?;
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .referer(false)
            .connect_timeout(limits.connect_timeout)
            .https_only(!current.local);
        if let (Some(addr), Some(host)) = (pinned, current.url.host_str()) {
            builder = builder.resolve(host, addr);
        }
        let client = builder.build().map_err(http_error)?;
        let mut request = client
            .get(current.url.clone())
            .header(header::ACCEPT, accept)
            .header(header::ACCEPT_LANGUAGE, "en");
        for (name, value) in headers {
            let mut value = header::HeaderValue::from_str(value)
                .map_err(|_| EgressError::InvalidUrl("a request header is not valid"))?;
            value.set_sensitive(true);
            request = request.header(*name, value);
        }
        let response = timeout_at(deadline, request.send())
            .await
            .map_err(|_| EgressError::Timeout)?
            .map_err(http_error)?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(EgressError::Status(status.as_u16()))?;
            let mut next = current
                .url
                .join(location)
                .map_err(|_| EgressError::InvalidUrl("the redirect address is not valid"))?;
            let next = check(&mut next, exceptions)?;
            if origin_of(&next.url) != origin {
                return Err(EgressError::CrossOriginRedirect(next.url.to_string()));
            }
            current = next;
            continue;
        }
        if !status.is_success() {
            return Err(EgressError::Status(status.as_u16()));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut body = Vec::new();
        let mut truncated = false;
        let mut stream = response.bytes_stream();
        loop {
            let next = timeout_at(deadline, stream.next())
                .await
                .map_err(|_| EgressError::Timeout)?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(http_error)?;
            let room = limits.max_bytes - body.len();
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(Fetched {
            url: current.url,
            status: status.as_u16(),
            content_type,
            body,
            truncated,
        });
    }
    Err(EgressError::TooManyRedirects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::validate;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;

    /// A loopback test server, reachable only through a local exception.
    async fn server() -> (String, Vec<LocalException>) {
        let app = Router::new()
            .route(
                "/page",
                get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<p>hello</p>") }),
            )
            .route(
                "/hop",
                get(|| async {
                    (StatusCode::FOUND, [(header::LOCATION, "/page")]).into_response()
                }),
            )
            .route(
                "/away",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, "https://example.com/x")],
                    )
                        .into_response()
                }),
            )
            .route(
                "/inside",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, "http://10.0.0.1/admin")],
                    )
                        .into_response()
                }),
            )
            .route(
                "/loop",
                get(|| async {
                    (StatusCode::FOUND, [(header::LOCATION, "/loop")]).into_response()
                }),
            )
            .route("/big", get(|| async { "x".repeat(10_000) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://127.0.0.1:{}", addr.port());
        let ex = vec![LocalException::for_base(&Url::parse(&base).unwrap()).unwrap()];
        (base, ex)
    }

    #[tokio::test]
    async fn same_origin_redirects_are_followed_and_others_stop() {
        let (base, ex) = server().await;
        let limits = Limits::default();
        let t = validate(&format!("{base}/hop"), &ex).unwrap();
        let f = fetch(&t, "text/html", &ex, &limits).await.unwrap();
        assert_eq!(f.url.path(), "/page");
        assert_eq!(f.body, b"<p>hello</p>");
        assert_eq!(f.content_type.as_deref(), Some("text/html"));

        let t = validate(&format!("{base}/away"), &ex).unwrap();
        assert!(matches!(
            fetch(&t, "*/*", &ex, &limits).await,
            Err(EgressError::CrossOriginRedirect(to)) if to == "https://example.com/x"
        ));
        // A redirect into a private network is refused before any request.
        let t = validate(&format!("{base}/inside"), &ex).unwrap();
        assert!(matches!(
            fetch(&t, "*/*", &ex, &limits).await,
            Err(EgressError::InvalidUrl(_))
        ));
        let t = validate(&format!("{base}/loop"), &ex).unwrap();
        assert_eq!(
            fetch(&t, "*/*", &ex, &limits).await.unwrap_err(),
            EgressError::TooManyRedirects
        );
    }

    #[tokio::test]
    async fn bodies_are_cut_at_the_limit() {
        let (base, ex) = server().await;
        let limits = Limits {
            max_bytes: 1000,
            ..Limits::default()
        };
        let t = validate(&format!("{base}/big"), &ex).unwrap();
        let f = fetch(&t, "*/*", &ex, &limits).await.unwrap();
        assert_eq!(f.body.len(), 1000);
        assert!(f.truncated);
    }

    #[tokio::test]
    async fn loopback_is_unreachable_without_the_exception() {
        let (base, _) = server().await;
        // The URL itself is refused (plain http, loopback address).
        assert!(validate(&format!("{base}/page"), &[]).is_err());
        // A name that resolves to loopback is refused at connection time.
        let t = Target {
            url: Url::parse("https://localhost.localdomain.test.invalid/").unwrap(),
            local: false,
        };
        assert!(fetch(&t, "*/*", &[], &Limits::default()).await.is_err());
    }
}
