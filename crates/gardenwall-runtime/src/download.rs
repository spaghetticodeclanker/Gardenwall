//! Verified download of pinned runtime archives.
//!
//! HTTPS only, to GitHub's release hosts (re-checked on every redirect), no
//! ambient proxy, never more bytes than the pinned size, SHA-256 checked
//! while streaming. A mismatching file is deleted.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;

const MAX_REDIRECTS: usize = 5;
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

#[derive(Debug)]
pub enum DownloadError {
    DisallowedUrl,
    Http(reqwest::Error),
    Status(u16),
    TooLarge,
    Incomplete { expected: u64, actual: u64 },
    DigestMismatch,
    Cancelled,
    Io(std::io::Error),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DisallowedUrl => f.write_str("download URL is not an allowed release host"),
            Self::Http(e) => write!(f, "network error: {e}"),
            Self::Status(c) => write!(f, "server returned HTTP {c}"),
            Self::TooLarge => f.write_str("server sent more data than the pinned size"),
            Self::Incomplete { expected, actual } => {
                write!(f, "download incomplete: {actual} of {expected} bytes")
            }
            Self::DigestMismatch => f.write_str("SHA-256 does not match the pinned value"),
            Self::Cancelled => f.write_str("download cancelled"),
            Self::Io(e) => write!(f, "file error: {e}"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<reqwest::Error> for DownloadError {
    fn from(e: reqwest::Error) -> Self {
        // Signed asset URLs carry short-lived credentials; never keep them.
        Self::Http(e.without_url())
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn host_allowed(url: &Url, allow_loopback_http: bool) -> bool {
    if url.username() != "" || url.password().is_some() {
        return false;
    }
    match (url.scheme(), url.host()) {
        ("https", Some(url::Host::Domain(d))) => {
            ALLOWED_HOSTS.contains(&d.to_ascii_lowercase().as_str())
        }
        ("http", Some(url::Host::Ipv4(ip))) if allow_loopback_http => ip.is_loopback(),
        _ => false,
    }
}

pub struct Downloader {
    http: reqwest::Client,
    allow_loopback_http: bool,
}

impl Downloader {
    pub fn new() -> Result<Self, DownloadError> {
        Self::build(false)
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::build(true).expect("client builds")
    }

    fn build(allow_loopback_http: bool) -> Result<Self, DownloadError> {
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if host_allowed(attempt.url(), allow_loopback_http) {
                attempt.follow()
            } else {
                attempt.error("redirect to a disallowed host")
            }
        });
        let http = reqwest::Client::builder()
            .user_agent("Gardenwall/0.1")
            .redirect(redirect)
            .no_proxy()
            .https_only(!allow_loopback_http)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            allow_loopback_http,
        })
    }

    /// Download `url` into memory, never more than `max` bytes, under the
    /// same host rules. For files with no pinned digest whose content is
    /// checked by the caller (Gardenwall's own release file and installer,
    /// which carry a minisign signature, ADR-017 §3.3).
    /// `progress(done, total)` gets the announced length, or `max` when the
    /// server sends none; returning `false` cancels.
    pub async fn fetch_capped(
        &self,
        url: &str,
        max: u64,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, DownloadError> {
        let url = Url::parse(url).map_err(|_| DownloadError::DisallowedUrl)?;
        if !host_allowed(&url, self.allow_loopback_http) {
            return Err(DownloadError::DisallowedUrl);
        }
        let response = self.http.get(url).send().await?;
        if !response.status().is_success() {
            return Err(DownloadError::Status(response.status().as_u16()));
        }
        let announced = response.content_length();
        if announced.is_some_and(|len| len > max) {
            return Err(DownloadError::TooLarge);
        }
        let total = announced.unwrap_or(max);
        let mut out = Vec::with_capacity(usize::try_from(total.min(64 << 20)).unwrap_or(0));
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if (out.len() + chunk.len()) as u64 > max {
                return Err(DownloadError::TooLarge);
            }
            out.extend_from_slice(&chunk);
            if !progress(out.len() as u64, total) {
                return Err(DownloadError::Cancelled);
            }
        }
        if let Some(expected) = announced
            && out.len() as u64 != expected
        {
            return Err(DownloadError::Incomplete {
                expected,
                actual: out.len() as u64,
            });
        }
        Ok(out)
    }

    /// Download `url` to `dest`, verifying exact size and SHA-256.
    /// `progress(done, total)` returning `false` cancels.
    pub async fn fetch(
        &self,
        url: &str,
        size: u64,
        sha256_hex: &str,
        dest: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<(), DownloadError> {
        let url = Url::parse(url).map_err(|_| DownloadError::DisallowedUrl)?;
        if !host_allowed(&url, self.allow_loopback_http) {
            return Err(DownloadError::DisallowedUrl);
        }
        let result = async {
            let response = self.http.get(url).send().await?;
            if !response.status().is_success() {
                return Err(DownloadError::Status(response.status().as_u16()));
            }
            if response.content_length().is_some_and(|len| len > size) {
                return Err(DownloadError::TooLarge);
            }
            let mut out = tokio::fs::File::create(dest).await?;
            let mut hasher = Sha256::new();
            let mut done: u64 = 0;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                done += chunk.len() as u64;
                if done > size {
                    return Err(DownloadError::TooLarge);
                }
                hasher.update(&chunk);
                out.write_all(&chunk).await?;
                if !progress(done, size) {
                    return Err(DownloadError::Cancelled);
                }
            }
            out.flush().await?;
            out.sync_all().await?;
            if done != size {
                return Err(DownloadError::Incomplete {
                    expected: size,
                    actual: done,
                });
            }
            let actual: [u8; 32] = hasher.finalize().into();
            let actual_hex: String = actual.iter().map(|b| format!("{b:02x}")).collect();
            if !actual_hex.eq_ignore_ascii_case(sha256_hex) {
                return Err(DownloadError::DigestMismatch);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(dest).await;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    #[test]
    fn host_policy() {
        let ok = |s: &str| host_allowed(&Url::parse(s).unwrap(), false);
        assert!(ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b1/x.zip"
        ));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("https://evilgithub.com/x"));
        assert!(!ok("https://user:pw@github.com/x"));
        assert!(!ok("https://140.82.112.3/x"));
        assert!(!ok("http://127.0.0.1/x"));
    }

    async fn serve(body: Vec<u8>) -> String {
        let app = axum::Router::new().route("/a.zip", get(move || async move { body.clone() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/a.zip")
    }

    fn hex(data: &[u8]) -> String {
        let d: [u8; 32] = Sha256::digest(data).into();
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn verifies_size_and_digest() {
        let body = b"runtime archive bytes".to_vec();
        let url = serve(body.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.part");
        let d = Downloader::for_tests();

        d.fetch(&url, body.len() as u64, &hex(&body), &dest, |_, _| true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);

        let r = d
            .fetch(&url, body.len() as u64, &"0".repeat(64), &dest, |_, _| true)
            .await;
        assert!(matches!(r, Err(DownloadError::DigestMismatch)));
        assert!(!dest.exists(), "mismatching file must be deleted");

        let r = d.fetch(&url, 5, &hex(&body), &dest, |_, _| true).await;
        assert!(matches!(r, Err(DownloadError::TooLarge)));
        assert!(!dest.exists());

        let r = d
            .fetch(&url, body.len() as u64 + 10, &hex(&body), &dest, |_, _| {
                true
            })
            .await;
        assert!(matches!(r, Err(DownloadError::Incomplete { .. })));

        let r = d
            .fetch(&url, body.len() as u64, &hex(&body), &dest, |_, _| false)
            .await;
        assert!(matches!(r, Err(DownloadError::Cancelled)));
        assert!(!dest.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capped_fetch_keeps_to_its_limit() {
        let body = b"release file bytes".to_vec();
        let url = serve(body.clone()).await;
        let d = Downloader::for_tests();

        let got = d.fetch_capped(&url, 1024, |_, _| true).await.unwrap();
        assert_eq!(got, body);

        let r = d.fetch_capped(&url, 5, |_, _| true).await;
        assert!(matches!(r, Err(DownloadError::TooLarge)));

        let r = d.fetch_capped(&url, 1024, |_, _| false).await;
        assert!(matches!(r, Err(DownloadError::Cancelled)));

        let prod = Downloader::new().unwrap();
        let r = prod
            .fetch_capped("https://example.com/latest.json", 1024, |_, _| true)
            .await;
        assert!(matches!(r, Err(DownloadError::DisallowedUrl)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn production_client_refuses_non_allowlisted_urls() {
        let d = Downloader::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let r = d
            .fetch(
                "https://example.com/x.zip",
                1,
                &"0".repeat(64),
                &dir.path().join("x"),
                |_, _| true,
            )
            .await;
        assert!(matches!(r, Err(DownloadError::DisallowedUrl)));
    }
}
