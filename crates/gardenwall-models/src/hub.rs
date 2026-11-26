//! Hugging Face hub client: search, commit resolution, and verified,
//! resumable downloads into quarantine (ADR-013 §2.2).
//!
//! Security properties:
//! - HTTPS only, to an allowlisted set of hosts, re-checked on every redirect.
//! - No ambient proxy, cookies, or credentials; a generic user agent.
//! - Every hub response body is size-capped before parsing.
//! - Repository IDs, revisions, and file paths are validated before they are
//!   placed in a URL; downloads are pinned to a resolved commit.
//! - Only `.gguf` files with a hub-declared SHA-256 are offered; files without
//!   a verifiable hash are never downloaded.
//! - Quarantine files are named by their expected digest, never by a
//!   hub-supplied name, and are never written past their declared size.
//! - Hub search terms are disclosed to the hub; callers must surface this.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::gguf::{self, GgufLimits};
use crate::intake::{self, ModelSource, Provenance, QuarantinedModel};
use crate::integrity::{self, Sha256Digest, Verification};
use crate::split;

mod segmented;
mod sparse;

const USER_AGENT: &str = "Gardenwall/0.1";
const MAX_API_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_SEARCH_RESULTS: u32 = 100;
const MAX_QUERY_CHARS: usize = 200;

/// Which hosts the client may contact.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    /// Exact host names, e.g. `huggingface.co`.
    exact: Vec<String>,
    /// Domain suffixes, e.g. `hf.co` permits `cdn.hf.co` (but not `hf.co.evil`).
    suffixes: Vec<String>,
    /// Test-only: permit plain HTTP to loopback addresses.
    allow_loopback_http: bool,
}

impl HostPolicy {
    pub fn hugging_face() -> Self {
        Self {
            exact: vec!["huggingface.co".into(), "hf.co".into()],
            suffixes: vec!["huggingface.co".into(), "hf.co".into()],
            allow_loopback_http: false,
        }
    }

    #[cfg(test)]
    fn loopback_for_tests() -> Self {
        Self {
            exact: vec![],
            suffixes: vec![],
            allow_loopback_http: true,
        }
    }

    pub fn permits(&self, url: &Url) -> bool {
        if url.username() != "" || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host() else {
            return false;
        };
        match url.scheme() {
            "https" => {}
            "http" if self.allow_loopback_http => {
                return match host {
                    url::Host::Ipv4(ip) => ip.is_loopback(),
                    url::Host::Ipv6(ip) => ip.is_loopback(),
                    url::Host::Domain(_) => false,
                };
            }
            _ => return false,
        }
        let url::Host::Domain(name) = host else {
            // IP-literal HTTPS endpoints are never part of the hub.
            return false;
        };
        let name = name.to_ascii_lowercase();
        self.exact.contains(&name)
            || self.suffixes.iter().any(|s| {
                name.len() > s.len() && name.ends_with(s.as_str()) && {
                    name.as_bytes()[name.len() - s.len() - 1] == b'.'
                }
            })
    }
}

#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Name recorded in provenance, e.g. `huggingface`.
    pub hub_name: String,
    /// The user's offline setting. When `true`, no client can be constructed,
    /// so no request of any kind can be sent. This is the single choke point
    /// the application wires to its "offline / no hub" setting (ADR-013 §2.7).
    pub offline: bool,
    pub base_url: Url,
    pub hosts: HostPolicy,
    pub connect_timeout: Duration,
    /// Timeout for API requests (not for whole downloads).
    pub api_timeout: Duration,
    /// Maximum time without receiving any download bytes.
    pub read_timeout: Duration,
    /// Concurrent connections per download.
    pub download_connections: usize,
    /// Bytes fetched per ranged request.
    pub download_chunk_bytes: u64,
}

impl HubConfig {
    pub fn hugging_face() -> Self {
        Self {
            hub_name: "huggingface".into(),
            offline: false,
            base_url: Url::parse("https://huggingface.co/").expect("static URL is valid"),
            hosts: HostPolicy::hugging_face(),
            connect_timeout: Duration::from_secs(15),
            api_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            // One CDN connection can be limited to ~5 MB/s for files not
            // cached nearby; eight together reach typical line rates.
            download_connections: 8,
            download_chunk_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum HubError {
    /// The user has enabled offline mode; no hub access is permitted.
    Offline,
    InvalidRepoId,
    InvalidRevision,
    InvalidQuery,
    InvalidFilePath,
    DisallowedUrl,
    Http(reqwest::Error),
    /// Any other refusal; 429 and 5xx are retried.
    Status(u16),
    /// The repository is gated: Hugging Face serves its files only to a
    /// signed-in account that has accepted the model's terms, and Gardenwall
    /// downloads without signing in.
    Gated,
    /// The repository, revision or file doesn't exist, or is private (the
    /// hub answers both the same way).
    NotFound,
    ResponseTooLarge,
    MalformedResponse,
    Io(std::io::Error),
    UnexpectedContentRange,
    RangesUnsupported,
    TooManyBytes,
    Incomplete {
        expected: u64,
        actual: u64,
    },
    Cancelled,
    /// The downloaded bytes do not match the hub-declared digest or size.
    /// The partial file has been deleted.
    IntegrityMismatch(Verification),
    /// The file passed integrity checks but failed GGUF validation; it has
    /// been left in quarantine under a `.rejected` name and cannot be loaded.
    InvalidGguf(gguf::GgufError),
    /// A split model's parts are missing or disagree with each other.
    Split(split::SplitError),
    /// The sandboxed download helper failed or couldn't start.
    Helper(String),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("offline mode is enabled; model hub access is disabled"),
            Self::InvalidRepoId => f.write_str("invalid repository id"),
            Self::InvalidRevision => f.write_str("invalid revision"),
            Self::InvalidQuery => f.write_str("invalid search query"),
            Self::InvalidFilePath => f.write_str("invalid file path"),
            Self::DisallowedUrl => f.write_str("destination is not an allowed hub host"),
            Self::Http(e) => write!(f, "network error: {e}"),
            Self::Gated => f.write_str(
                "this model is gated: its publisher lets Hugging Face give the files only to a signed-in account that has accepted the model's terms, and Gardenwall downloads without signing in. To use it, sign in on huggingface.co, accept the terms on the model's page, download the file in your browser and add it with Import (it is checked the same way)",
            ),
            Self::NotFound => f.write_str(
                "Hugging Face couldn't find this model or file: it may have been renamed, removed or made private. Search again to see the current files",
            ),
            Self::Status(429) => f.write_str(
                "Hugging Face is limiting requests from this computer (HTTP 429); wait a few minutes and try again",
            ),
            Self::Status(code @ 500..=599) => write!(
                f,
                "Hugging Face had a problem on its side (HTTP {code}); try again later"
            ),
            Self::Status(code @ (401 | 403)) => write!(
                f,
                "Hugging Face refused access to this file (HTTP {code}); it may need a signed-in account, which Gardenwall doesn't use"
            ),
            Self::Status(code) => write!(f, "Hugging Face answered with HTTP {code}"),
            Self::ResponseTooLarge => f.write_str("hub response exceeded the size limit"),
            Self::MalformedResponse => f.write_str("hub response was malformed"),
            Self::Io(e) => write!(f, "file error: {e}"),
            Self::UnexpectedContentRange => f.write_str("server returned an unexpected range"),
            Self::RangesUnsupported => f.write_str("server does not support ranged downloads"),
            Self::TooManyBytes => f.write_str("server sent more bytes than declared"),
            Self::Incomplete { expected, actual } => {
                write!(f, "download incomplete: {actual} of {expected} bytes")
            }
            Self::Cancelled => f.write_str("download cancelled"),
            Self::IntegrityMismatch(v) => write!(f, "integrity check failed: {v:?}"),
            Self::InvalidGguf(e) => write!(f, "model file rejected: {e}"),
            Self::Split(e) => write!(f, "{e}"),
            Self::Helper(e) => write!(f, "the download helper failed: {e}"),
        }
    }
}

impl std::error::Error for HubError {}

impl From<std::io::Error> for HubError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<reqwest::Error> for HubError {
    fn from(e: reqwest::Error) -> Self {
        // Strip URLs: signed CDN URLs carry short-lived credentials.
        Self::Http(e.without_url())
    }
}

/// A validated `owner/name` repository identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoId {
    owner: String,
    name: String,
}

fn valid_repo_segment(s: &str) -> bool {
    (1..=96).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !s.starts_with(['.', '-'])
        && !s.ends_with('.')
        && !s.contains("..")
}

impl RepoId {
    pub fn parse(id: &str) -> Result<Self, HubError> {
        let (owner, name) = id.split_once('/').ok_or(HubError::InvalidRepoId)?;
        if !valid_repo_segment(owner) || !valid_repo_segment(name) {
            return Err(HubError::InvalidRepoId);
        }
        Ok(Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A full 40-hex-digit commit identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitId(String);

impl CommitId {
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self(s.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Branch/tag names or commit IDs accepted for resolution.
fn valid_revision(rev: &str) -> bool {
    (1..=128).contains(&rev.len())
        && rev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !rev.starts_with(['.', '-'])
        && !rev.contains("..")
}

/// Repository-relative file path: segments use the same charset as repo IDs.
fn valid_remote_path(path: &str) -> bool {
    path.len() <= 512 && !path.is_empty() && path.split('/').all(valid_repo_segment)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListing {
    pub repo: RepoId,
    pub downloads: u64,
    pub likes: u64,
    pub gated: bool,
    pub pipeline_tag: Option<String>,
}

/// One row of the hub's ranking. Every field but the repository may be
/// missing: the hub's list format isn't documented, so nothing is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedListing {
    pub repo: RepoId,
    /// Downloads in the last 30 days, as the hub counts them.
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    pub gated: Option<bool>,
    /// The date part of the creation time (`YYYY-MM-DD`).
    pub created: Option<String>,
    pub trending_score: Option<i64>,
    /// From the `license:` tag, if any.
    pub license: Option<String>,
}

/// A short hub parameter value: letters, digits and `-` only.
fn valid_parameter(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The date part of a hub timestamp, if it looks like one.
fn date_part(s: &str) -> Option<String> {
    s.get(..10)
        .filter(|d| d.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
        .map(str::to_string)
}

fn ranked_row(v: &serde_json::Value) -> Option<RankedListing> {
    let repo = RepoId::parse(v.get("id")?.as_str()?).ok()?;
    let uint = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
    let created = v
        .get("createdAt")
        .and_then(|c| c.as_str())
        .and_then(date_part);
    let license = v
        .get("tags")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str()?.strip_prefix("license:"))
        .find(|l| l.len() <= 64 && l.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_string);
    Some(RankedListing {
        repo,
        downloads: uint("downloads"),
        likes: uint("likes"),
        gated: v.get("gated").map(is_gated),
        created,
        trending_score: v
            .get("trendingScore")
            .and_then(|s| s.as_i64().or_else(|| s.as_f64().map(|f| f as i64))),
        license,
    })
}

/// A verifiable GGUF file at a pinned commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub repo: RepoId,
    pub commit: CommitId,
    /// Repository-relative path (validated).
    pub path: String,
    /// Validated local display name (final path component).
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    /// License declared on the model card, if any (informational).
    pub license: Option<String>,
}

impl RemoteFile {
    pub fn digest(&self) -> Sha256Digest {
        // Validated at construction.
        Sha256Digest::from_hex(&self.sha256).expect("digest validated at construction")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepo {
    pub repo: RepoId,
    pub commit: CommitId,
    pub gated: bool,
    pub license: Option<String>,
    pub files: Vec<RemoteFile>,
    /// GGUF files skipped because they had no verifiable hash or an unsafe name.
    pub skipped: usize,
    /// Downloads in the last 30 days, if the hub said.
    pub downloads: Option<u64>,
    /// The date part of the creation time (`YYYY-MM-DD`), if the hub said.
    pub created: Option<String>,
}

pub use crate::split::{QuarantinedSplit, SplitPart};

impl ResolvedRepo {
    /// All parts of the split set `path` belongs to, in order, or `None`
    /// when `path` is a single-file model. Every part must be listed (with
    /// a verifiable digest) in the same folder.
    pub fn split_set(&self, path: &str) -> Result<Option<Vec<RemoteFile>>, HubError> {
        let file = self
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or(HubError::InvalidFilePath)?;
        let Some((base, _, count)) = split::parse_split_name(&file.file_name) else {
            return Ok(None);
        };
        let folder = |p: &str| p.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let dir = folder(&file.path);
        let mut parts: Vec<(u32, RemoteFile)> = self
            .files
            .iter()
            .filter(|f| folder(&f.path) == dir)
            .filter_map(|f| match split::parse_split_name(&f.file_name) {
                Some((b, i, n)) if b == base && n == count => Some((i, f.clone())),
                _ => None,
            })
            .collect();
        let indices: Vec<u32> = parts.iter().map(|(i, _)| *i).collect();
        split::check_complete(&indices, count).map_err(HubError::Split)?;
        parts.sort_by_key(|(i, _)| *i);
        Ok(Some(parts.into_iter().map(|(_, f)| f).collect()))
    }
}

// --- Wire formats (only the fields Gardenwall uses) ---

#[derive(Deserialize)]
struct WireListing {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    gated: serde_json::Value,
    pipeline_tag: Option<String>,
}

#[derive(Deserialize)]
struct WireRevision {
    sha: String,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default, rename = "cardData")]
    card_data: Option<serde_json::Value>,
    #[serde(default)]
    siblings: Vec<WireSibling>,
}

#[derive(Deserialize)]
struct WireSibling {
    rfilename: String,
    lfs: Option<WireLfs>,
}

#[derive(Deserialize)]
struct WireLfs {
    sha256: String,
    size: u64,
}

/// The error for a refused hub request, from the status and the hub's own
/// `X-Error-Code` header (`GatedRepo`, `RepoNotFound`, `RevisionNotFound`,
/// `EntryNotFound`). The header is only used to choose a message.
pub(crate) fn refusal(response: &reqwest::Response) -> HubError {
    let code = response
        .headers()
        .get("x-error-code")
        .and_then(|v| v.to_str().ok());
    refusal_for(response.status().as_u16(), code)
}

fn refusal_for(status: u16, code: Option<&str>) -> HubError {
    match code {
        Some("GatedRepo") => HubError::Gated,
        Some("RepoNotFound" | "RevisionNotFound" | "EntryNotFound") => HubError::NotFound,
        _ if status == 404 => HubError::NotFound,
        _ => HubError::Status(status),
    }
}

/// The hub marks gated repos with `false`, `"auto"`, or `"manual"`.
fn is_gated(v: &serde_json::Value) -> bool {
    !matches!(v, serde_json::Value::Null | serde_json::Value::Bool(false))
}

pub struct HubClient {
    http: reqwest::Client,
    /// Chunk downloads in this process: one connection per chunk.
    download_http: reqwest::Client,
    config: HubConfig,
    /// Fetches the bytes of downloads somewhere else (the sandboxed
    /// download helper, ADR-013 §2.6); `None` fetches them here.
    fetcher: Option<std::sync::Arc<dyn PartFetcher>>,
}

/// One file's bytes to fetch: from where, and where its partial file and
/// resume state go. Passed to a [`PartFetcher`] (as JSON, to the download
/// helper). The digest is checked by the caller afterwards, never trusted
/// from the fetcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartJob {
    pub url: String,
    pub part_path: std::path::PathBuf,
    pub state_path: std::path::PathBuf,
    pub size: u64,
    pub chunk_bytes: u64,
    pub connections: usize,
}

pub type FetchFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HubError>> + Send + 'a>>;

/// Fetches a download's bytes into `job.part_path` (resuming from
/// `job.state_path`). `progress(done, total)` returning `false` cancels.
pub trait PartFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        job: PartJob,
        progress: &'a mut (dyn FnMut(u64, u64) -> bool + Send),
    ) -> FetchFuture<'a>;
}

/// The HTTP client every hub request uses: the host allowlist re-checked
/// on each redirect, HTTPS only, timeouts, a generic user agent, and no
/// ambient proxy. `proxy` is the download helper's own bridge to the core
/// (container-local loopback), the only proxy ever set.
///
/// `downloads` makes an HTTP/1.1-only client, so each parallel chunk gets
/// its own TCP connection. Over HTTP/2 every chunk request to the CDN is
/// multiplexed on one connection, and the hub's CDN limits each connection
/// to a few MB/s for files it hasn't cached (measured 2026-11-21: 2 MB/s
/// over HTTP/2 against 16-60 MB/s with eight HTTP/1.1 connections).
fn build_http(
    config: &HubConfig,
    proxy: Option<&str>,
    downloads: bool,
) -> Result<reqwest::Client, HubError> {
    let hosts = config.hosts.clone();
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if hosts.permits(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect to a disallowed host")
        }
    });
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(redirect)
        .https_only(!config.hosts.allow_loopback_http)
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.read_timeout);
    let builder = if downloads {
        builder.http1_only()
    } else {
        builder
    };
    let builder = match proxy {
        None => builder.no_proxy(),
        Some(p) => builder.no_proxy().proxy(reqwest::Proxy::all(p)?),
    };
    Ok(builder.build()?)
}

/// What a stopped download of `files` keeps in `library_dir` for resuming,
/// to delete when the user cancels it for good: a single file's partial
/// file and resume state, or a split set's whole staging folder.
pub fn partial_paths(
    files: &[RemoteFile],
    library_dir: &Path,
    split_set: bool,
) -> Vec<std::path::PathBuf> {
    if split_set {
        let id = split::set_id(files.iter().map(|f| f.sha256.as_str()));
        return vec![library_dir.join(format!("{id}.download"))];
    }
    files
        .iter()
        .filter(|f| Sha256Digest::from_hex(&f.sha256).is_some())
        .flat_map(|f| {
            ["part", "state", "state.tmp"]
                .map(|ext| library_dir.join(format!("{}.{ext}", f.sha256)))
        })
        .collect()
}

/// Fetch one download's bytes, as [`HubClient`] would, for the download
/// helper: Hugging Face hosts only (the URL and every redirect), through
/// `proxy` when given. The caller verifies the digest afterwards.
pub async fn fetch_part(
    job: &PartJob,
    proxy: Option<&str>,
    progress: &mut (impl FnMut(u64, u64) -> bool + Send),
) -> Result<(), HubError> {
    let config = HubConfig::hugging_face();
    let url = Url::parse(&job.url).map_err(|_| HubError::DisallowedUrl)?;
    if !config.hosts.permits(&url) {
        return Err(HubError::DisallowedUrl);
    }
    let http = build_http(&config, proxy, true)?;
    segmented::fetch(
        segmented::Plan {
            http: &http,
            url,
            part_path: &job.part_path,
            state_path: &job.state_path,
            size: job.size,
            chunk_bytes: job.chunk_bytes.max(1 << 20),
            connections: job.connections.clamp(1, 16),
        },
        progress,
    )
    .await
}

impl HubClient {
    pub fn new(config: HubConfig) -> Result<Self, HubError> {
        if config.offline {
            return Err(HubError::Offline);
        }
        if !config.hosts.permits(&config.base_url) {
            return Err(HubError::DisallowedUrl);
        }
        let http = build_http(&config, None, false)?;
        let download_http = build_http(&config, None, true)?;
        Ok(Self {
            http,
            download_http,
            config,
            fetcher: None,
        })
    }

    /// Fetch download bytes with `fetcher` (the sandboxed helper) instead
    /// of in this process. Digests and GGUF validation stay here.
    pub fn with_fetcher(mut self, fetcher: std::sync::Arc<dyn PartFetcher>) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    fn url(&self, path_segments: &[&str]) -> Result<Url, HubError> {
        let mut url = self.config.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| HubError::DisallowedUrl)?
            .pop_if_empty()
            .extend(path_segments);
        if !self.config.hosts.permits(&url) {
            return Err(HubError::DisallowedUrl);
        }
        Ok(url)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: Url) -> Result<T, HubError> {
        let response = self
            .http
            .get(url)
            .timeout(self.config.api_timeout)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(refusal(&response));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_API_RESPONSE_BYTES as u64)
        {
            return Err(HubError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > MAX_API_RESPONSE_BYTES {
                return Err(HubError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| HubError::MalformedResponse)
    }

    /// Search GGUF model repositories. The query is sent to the hub.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<ModelListing>, HubError> {
        let query = query.trim();
        if query.is_empty()
            || query.chars().count() > MAX_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(HubError::InvalidQuery);
        }
        let mut url = self.url(&["api", "models"])?;
        url.query_pairs_mut()
            .append_pair("search", query)
            .append_pair("filter", "gguf")
            .append_pair("sort", "downloads")
            .append_pair("limit", &limit.clamp(1, MAX_SEARCH_RESULTS).to_string());
        let listings: Vec<WireListing> = self.get_json(url).await?;
        Ok(listings
            .into_iter()
            .filter_map(|w| {
                Some(ModelListing {
                    repo: RepoId::parse(&w.id).ok()?,
                    downloads: w.downloads,
                    likes: w.likes,
                    gated: is_gated(&w.gated),
                    pipeline_tag: w.pipeline_tag.filter(|t| t.len() <= 64),
                })
            })
            .collect())
    }

    /// GGUF repositories of one kind (`pipeline_tag`), ranked by `sort`.
    /// Only these fixed parameters are sent: no search text and nothing
    /// about the computer. A sort the hub refuses comes back as
    /// `HubError::Status(400)`, for the caller to fall back.
    pub async fn ranking(
        &self,
        kind: &str,
        sort: &str,
        limit: u32,
    ) -> Result<Vec<RankedListing>, HubError> {
        if !valid_parameter(kind) || !valid_parameter(sort) {
            return Err(HubError::InvalidQuery);
        }
        let mut url = self.url(&["api", "models"])?;
        url.query_pairs_mut()
            .append_pair("filter", "gguf")
            .append_pair("pipeline_tag", kind)
            .append_pair("sort", sort)
            .append_pair("direction", "-1")
            .append_pair("limit", &limit.clamp(1, MAX_SEARCH_RESULTS).to_string());
        for field in [
            "downloads",
            "likes",
            "gated",
            "createdAt",
            "trendingScore",
            "tags",
        ] {
            url.query_pairs_mut().append_pair("expand[]", field);
        }
        let rows: Vec<serde_json::Value> = self.get_json(url).await?;
        Ok(rows.iter().filter_map(ranked_row).collect())
    }

    /// Resolve a revision to a commit and list its verifiable GGUF files.
    pub async fn resolve(&self, repo: &RepoId, revision: &str) -> Result<ResolvedRepo, HubError> {
        if !valid_revision(revision) {
            return Err(HubError::InvalidRevision);
        }
        let mut url = self.url(&[
            "api",
            "models",
            repo.owner(),
            repo.name(),
            "revision",
            revision,
        ])?;
        url.query_pairs_mut().append_pair("blobs", "true");
        let wire: WireRevision = self.get_json(url).await?;
        let commit = CommitId::parse(&wire.sha).ok_or(HubError::MalformedResponse)?;
        let license = wire
            .card_data
            .as_ref()
            .and_then(|c| c.get("license"))
            .and_then(|l| l.as_str())
            .filter(|l| l.len() <= 64 && l.chars().all(|c| c.is_ascii_graphic()))
            .map(str::to_string);

        let mut files = Vec::new();
        let mut skipped = 0;
        for sibling in wire.siblings {
            if !sibling.rfilename.to_ascii_lowercase().ends_with(".gguf") {
                continue;
            }
            let file_name = sibling.rfilename.rsplit('/').next().unwrap_or_default();
            let usable = valid_remote_path(&sibling.rfilename)
                && integrity::safe_model_file_name(file_name).is_ok();
            match sibling.lfs {
                Some(lfs) if usable && Sha256Digest::from_hex(&lfs.sha256).is_some() => {
                    files.push(RemoteFile {
                        repo: repo.clone(),
                        commit: commit.clone(),
                        file_name: file_name.to_string(),
                        path: sibling.rfilename,
                        size: lfs.size,
                        sha256: lfs.sha256.to_ascii_lowercase(),
                        license: license.clone(),
                    });
                }
                _ => skipped += 1,
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(ResolvedRepo {
            repo: repo.clone(),
            commit,
            gated: is_gated(&wire.gated),
            license,
            files,
            skipped,
            downloads: wire.downloads,
            created: wire.created_at.as_deref().and_then(date_part),
        })
    }

    /// Fetch `file` into `dir` as `<digest>.part` (resuming a previous
    /// partial download) and verify its size and digest. A mismatch deletes
    /// the partial file.
    async fn fetch_verified(
        &self,
        file: &RemoteFile,
        dir: &Path,
        progress: &mut (impl FnMut(u64, u64) -> bool + Send),
    ) -> Result<std::path::PathBuf, HubError> {
        if !valid_remote_path(&file.path)
            || integrity::safe_model_file_name(&file.file_name).is_err()
        {
            return Err(HubError::InvalidFilePath);
        }
        let expected_digest =
            Sha256Digest::from_hex(&file.sha256).ok_or(HubError::MalformedResponse)?;
        let digest_hex = expected_digest.to_hex();
        let part_path = dir.join(format!("{digest_hex}.part"));
        let state_path = dir.join(format!("{digest_hex}.state"));
        tokio::fs::create_dir_all(dir).await?;

        let mut segments = vec![
            file.repo.owner(),
            file.repo.name(),
            "resolve",
            file.commit.as_str(),
        ];
        segments.extend(file.path.split('/'));
        let url = self.url(&segments)?;

        match &self.fetcher {
            Some(fetcher) => {
                let job = PartJob {
                    url: url.to_string(),
                    part_path: part_path.clone(),
                    state_path: state_path.clone(),
                    size: file.size,
                    chunk_bytes: self.config.download_chunk_bytes,
                    connections: self.config.download_connections,
                };
                fetcher.fetch(job, progress).await?;
            }
            None => {
                segmented::fetch(
                    segmented::Plan {
                        http: &self.download_http,
                        url,
                        part_path: &part_path,
                        state_path: &state_path,
                        size: file.size,
                        chunk_bytes: self.config.download_chunk_bytes,
                        connections: self.config.download_connections,
                    },
                    progress,
                )
                .await?;
            }
        }

        let verify_path = part_path.clone();
        let size = file.size;
        let verification = tokio::task::spawn_blocking(move || {
            let reader = std::fs::File::open(&verify_path)?;
            integrity::verify_reader(reader, &expected_digest, size, |_| true)
        })
        .await
        .map_err(|_| HubError::Io(std::io::Error::other("verification task failed")))??;
        if verification != Verification::Match {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(HubError::IntegrityMismatch(verification));
        }
        Ok(part_path)
    }

    fn provenance(
        &self,
        file: &RemoteFile,
        display: &str,
        size: u64,
        sha256: String,
    ) -> Provenance {
        Provenance {
            source: ModelSource::Hub {
                hub: self.config.hub_name.clone(),
                repo: file.repo.to_string(),
                commit: file.commit.as_str().to_string(),
                path: file.path.clone(),
                license: file.license.clone(),
            },
            display_name: intake::display_name(display),
            size,
            sha256,
            acquired_at_unix: intake::now_unix(),
        }
    }

    /// Download `file` into `quarantine_dir`, resuming a previous partial
    /// download, then verify its digest and validate it as GGUF.
    ///
    /// `progress(done, total)` is called as bytes arrive; returning `false`
    /// cancels (the partial file is kept for resumption).
    pub async fn download(
        &self,
        file: &RemoteFile,
        quarantine_dir: &Path,
        mut progress: impl FnMut(u64, u64) -> bool + Send,
    ) -> Result<QuarantinedModel, HubError> {
        let part_path = self
            .fetch_verified(file, quarantine_dir, &mut progress)
            .await?;
        let digest_hex = file.sha256.clone();
        let done_path = quarantine_dir.join(format!("{digest_hex}.gguf"));
        let rejected_path = quarantine_dir.join(format!("{digest_hex}.rejected"));

        let validate_path = part_path.clone();
        let validated = tokio::task::spawn_blocking(move || {
            let reader = std::fs::File::open(&validate_path).map_err(gguf::GgufError::Io)?;
            gguf::validate(reader, &GgufLimits::default())
        })
        .await
        .map_err(|_| HubError::Io(std::io::Error::other("validation task failed")))?;
        match validated {
            Ok(summary) => {
                tokio::fs::rename(&part_path, &done_path).await?;
                Ok(QuarantinedModel {
                    path: done_path,
                    summary,
                    provenance: self.provenance(file, &file.file_name, file.size, digest_hex),
                })
            }
            Err(error) => {
                let _ = tokio::fs::rename(&part_path, &rejected_path).await;
                Err(HubError::InvalidGguf(error))
            }
        }
    }

    /// Download every part of a split model (from [`ResolvedRepo::split_set`])
    /// into `<library_dir>/<set id>/` under their published names, each
    /// verified against its digest, then check the parts agree with each
    /// other. Partial parts are kept in `<set id>.download/` for resuming.
    ///
    /// `progress(done, total)` covers the whole set.
    pub async fn download_split(
        &self,
        parts: &[RemoteFile],
        library_dir: &Path,
        mut progress: impl FnMut(u64, u64) -> bool + Send,
    ) -> Result<QuarantinedSplit, HubError> {
        let first = parts.first().ok_or(HubError::InvalidFilePath)?;
        let id = split::set_id(parts.iter().map(|p| p.sha256.as_str()));
        let staging = library_dir.join(format!("{id}.download"));
        let total: u64 = parts.iter().map(|p| p.size).sum();

        let mut verified = Vec::with_capacity(parts.len());
        let mut offset = 0u64;
        for part in parts {
            let base = offset;
            let mut overall = |done: u64, _: u64| progress(base + done, total);
            verified.push(self.fetch_verified(part, &staging, &mut overall).await?);
            offset += part.size;
        }

        // Validate every part, then the set as a whole.
        let paths = verified.clone();
        let summaries = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|p| {
                    let reader = std::fs::File::open(p).map_err(gguf::GgufError::Io)?;
                    gguf::validate_part(reader, &GgufLimits::default())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|_| HubError::Io(std::io::Error::other("validation task failed")))?
        .map_err(HubError::InvalidGguf)?;
        // The first part must also pass the full single-file checks.
        if summaries[0].get("general.architecture").is_none() {
            return Err(HubError::InvalidGguf(gguf::GgufError::MissingArchitecture));
        }
        let tensor_count = split::check_parts(&summaries).map_err(HubError::Split)?;

        // Move the parts into place under their published names, which is
        // how llama.cpp finds parts 2..n next to the first.
        let final_dir = library_dir.join(&id);
        tokio::fs::create_dir_all(&final_dir).await?;
        for (part, path) in parts.iter().zip(&verified) {
            tokio::fs::rename(path, final_dir.join(&part.file_name)).await?;
            let _ = tokio::fs::remove_file(staging.join(format!("{}.state", part.sha256))).await;
        }
        let _ = tokio::fs::remove_dir(&staging).await;

        let display = split::parse_split_name(&first.file_name)
            .map(|(base, _, _)| format!("{base}.gguf"))
            .unwrap_or_else(|| first.file_name.clone());
        let summary = summaries.into_iter().next().expect("at least one part");
        Ok(QuarantinedSplit {
            model: QuarantinedModel {
                path: final_dir.join(&first.file_name),
                summary,
                provenance: self.provenance(first, &display, total, id),
            },
            parts: parts
                .iter()
                .map(|p| SplitPart {
                    file_name: p.file_name.clone(),
                    sha256: p.sha256.clone(),
                    size: p.size,
                })
                .collect(),
            tensor_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_refusals_are_explained() {
        // Hugging Face answers an anonymous download of a gated model with
        // 401 and `X-Error-Code: GatedRepo` (403 while a manual request is
        // pending).
        assert!(matches!(
            refusal_for(401, Some("GatedRepo")),
            HubError::Gated
        ));
        assert!(matches!(
            refusal_for(403, Some("GatedRepo")),
            HubError::Gated
        ));
        let gated = HubError::Gated.to_string();
        assert!(
            gated.contains("gated") && gated.contains("Import"),
            "{gated}"
        );
        // Missing and private repos both answer 401 RepoNotFound.
        for code in ["RepoNotFound", "RevisionNotFound", "EntryNotFound"] {
            assert!(matches!(refusal_for(401, Some(code)), HubError::NotFound));
        }
        assert!(matches!(refusal_for(404, None), HubError::NotFound));
        // Others keep their status, so 429 and 5xx are still retried.
        assert!(matches!(refusal_for(429, None), HubError::Status(429)));
        assert!(matches!(
            refusal_for(503, Some("Other")),
            HubError::Status(503)
        ));
        assert!(
            HubError::Status(429)
                .to_string()
                .contains("wait a few minutes")
        );
        assert!(
            HubError::Status(502)
                .to_string()
                .contains("try again later")
        );
        assert!(HubError::Status(401).to_string().contains("signed-in"));
    }
    use axum::extract::{Path as AxPath, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Redirect, Response};
    use axum::routing::get;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const COMMIT: &str = "23749fefcc72300e3a2ad315e1317431b06b590a";

    /// Smallest valid GGUF: header plus architecture key, no tensors.
    fn tiny_gguf() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"GGUF");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        v.extend_from_slice(&1u64.to_le_bytes());
        let key = b"general.architecture";
        v.extend_from_slice(&(key.len() as u64).to_le_bytes());
        v.extend_from_slice(key);
        v.extend_from_slice(&8u32.to_le_bytes());
        v.extend_from_slice(&5u64.to_le_bytes());
        v.extend_from_slice(b"llama");
        v.resize(v.len().next_multiple_of(32) + 3000, 0x5A);
        v
    }

    struct Fixture {
        body: Vec<u8>,
        sha256: String,
        /// When set, the first download request is cut off after half the body.
        truncate_first: AtomicBool,
        ignore_range: bool,
        /// Send extra bytes beyond the requested range (with a valid header).
        overlong: bool,
        redirect_to: Option<String>,
        /// Refuse downloads like Hugging Face does for a gated model.
        gated: bool,
        /// Total body bytes served, to check what resumption refetches.
        served: AtomicUsize,
    }

    async fn revision(State(f): State<Arc<Fixture>>) -> Response {
        axum::Json(serde_json::json!({
            "sha": COMMIT,
            "gated": false,
            "downloads": 42,
            "createdAt": "2026-11-20T17:59:59.000Z",
            "cardData": {"license": "apache-2.0"},
            "siblings": [
                {"rfilename": "README.md"},
                {"rfilename": "model-Q8_0.gguf", "lfs": {"sha256": f.sha256, "size": f.body.len()}},
                {"rfilename": "nohash.gguf"},
                {"rfilename": "../evil.gguf", "lfs": {"sha256": f.sha256, "size": 1}},
                {"rfilename": "sub/split-00001-of-00002.gguf", "lfs": {"sha256": f.sha256, "size": f.body.len()}},
            ]
        }))
        .into_response()
    }

    async fn resolve_file(
        State(f): State<Arc<Fixture>>,
        AxPath((_o, _n, _c, _file)): AxPath<(String, String, String, String)>,
        headers: HeaderMap,
    ) -> Response {
        if let Some(target) = &f.redirect_to {
            return Redirect::temporary(target).into_response();
        }
        if f.gated {
            return (
                StatusCode::UNAUTHORIZED,
                [("x-error-code", "GatedRepo")],
                "Access to model o/n is restricted.",
            )
                .into_response();
        }
        serve_body(&f, &headers)
    }

    fn serve_body(f: &Fixture, headers: &HeaderMap) -> Response {
        let total = f.body.len();
        let range = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(s, e)| {
                let s = s.parse::<usize>().ok()?;
                let e = if e.is_empty() {
                    total - 1
                } else {
                    e.parse::<usize>().ok()?
                };
                Some((s, e.min(total - 1)))
            });
        match range {
            Some((s, e)) if !f.ignore_range => {
                let mut body = f.body[s..=e].to_vec();
                if f.truncate_first.swap(false, Ordering::SeqCst) {
                    // A dropped connection: valid headers, half the bytes.
                    body.truncate(body.len() / 2);
                }
                if f.overlong {
                    body.extend_from_slice(&[0; 100]);
                }
                f.served.fetch_add(body.len(), Ordering::SeqCst);
                (
                    StatusCode::PARTIAL_CONTENT,
                    [(header::CONTENT_RANGE, format!("bytes {s}-{e}/{total}"))],
                    body,
                )
                    .into_response()
            }
            _ => {
                f.served.fetch_add(total, Ordering::SeqCst);
                (StatusCode::OK, f.body.clone()).into_response()
            }
        }
    }

    async fn start(fixture: Fixture) -> (HubClient, Url) {
        let (client, base, _) = start_with(fixture, 3).await;
        (client, base)
    }

    /// Serve `fixture` to a client using 1 KiB chunks over `connections`.
    async fn start_with(fixture: Fixture, connections: usize) -> (HubClient, Url, Arc<Fixture>) {
        let fixture = Arc::new(fixture);
        let shared = fixture.clone();
        let app = axum::Router::new()
            .route("/api/models/{o}/{n}/revision/{r}", get(revision))
            .route("/{o}/{n}/resolve/{c}/{file}", get(resolve_file))
            .with_state(fixture);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = Url::parse(&format!("http://{addr}/")).unwrap();
        let config = HubConfig {
            base_url: base.clone(),
            hosts: HostPolicy::loopback_for_tests(),
            download_connections: connections,
            download_chunk_bytes: 1024,
            ..HubConfig::hugging_face()
        };
        (HubClient::new(config).unwrap(), base, shared)
    }

    /// A GGUF with no tensors and the given string and u16 metadata.
    fn gguf_with(strings: &[(&str, &str)], u16s: &[(&str, u16)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"GGUF");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        v.extend_from_slice(&((strings.len() + u16s.len()) as u64).to_le_bytes());
        for (k, val) in strings {
            v.extend_from_slice(&(k.len() as u64).to_le_bytes());
            v.extend_from_slice(k.as_bytes());
            v.extend_from_slice(&8u32.to_le_bytes());
            v.extend_from_slice(&(val.len() as u64).to_le_bytes());
            v.extend_from_slice(val.as_bytes());
        }
        for (k, val) in u16s {
            v.extend_from_slice(&(k.len() as u64).to_le_bytes());
            v.extend_from_slice(k.as_bytes());
            v.extend_from_slice(&2u32.to_le_bytes());
            v.extend_from_slice(&val.to_le_bytes());
        }
        v.resize(v.len().next_multiple_of(32) + 2500, 0x5A);
        v
    }

    /// A hub serving a two-part split model (`m-0000N-of-00002.gguf` in
    /// `q/`) plus an incomplete three-part set. `second` is part 2's body.
    async fn split_hub(second: Vec<u8>) -> HubClient {
        let first = gguf_with(
            &[("general.architecture", "llama")],
            &[("split.no", 0), ("split.count", 2)],
        );
        let parts: Arc<Vec<(String, Fixture)>> = Arc::new(vec![
            ("m-00001-of-00002.gguf".into(), fixture(first)),
            ("m-00002-of-00002.gguf".into(), fixture(second)),
            ("x-00001-of-00003.gguf".into(), fixture(tiny_gguf())),
        ]);
        async fn rev(State(p): State<Arc<Vec<(String, Fixture)>>>) -> Response {
            let siblings: Vec<_> = p
                .iter()
                .map(|(name, f)| {
                    serde_json::json!({"rfilename": format!("q/{name}"), "lfs": {"sha256": f.sha256, "size": f.body.len()}})
                })
                .collect();
            axum::Json(serde_json::json!({"sha": COMMIT, "gated": false, "siblings": siblings}))
                .into_response()
        }
        async fn file(
            State(p): State<Arc<Vec<(String, Fixture)>>>,
            AxPath((_o, _n, _c, _dir, name)): AxPath<(String, String, String, String, String)>,
            headers: HeaderMap,
        ) -> Response {
            match p.iter().find(|(n, _)| *n == name) {
                Some((_, f)) => serve_body(f, &headers),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        let app = axum::Router::new()
            .route("/api/models/{o}/{n}/revision/{r}", get(rev))
            .route("/{o}/{n}/resolve/{c}/{dir}/{name}", get(file))
            .with_state(parts);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        HubClient::new(HubConfig {
            base_url: Url::parse(&format!("http://{addr}/")).unwrap(),
            hosts: HostPolicy::loopback_for_tests(),
            download_connections: 2,
            download_chunk_bytes: 1024,
            ..HubConfig::hugging_face()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn split_models_download_as_a_verified_set() {
        let client = split_hub(gguf_with(&[], &[("split.no", 1), ("split.count", 2)])).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        // Either part finds the whole set, in order; a single file is not a set.
        let set = resolved
            .split_set("q/m-00002-of-00002.gguf")
            .unwrap()
            .unwrap();
        let names: Vec<_> = set.iter().map(|f| f.file_name.as_str()).collect();
        assert_eq!(names, ["m-00001-of-00002.gguf", "m-00002-of-00002.gguf"]);
        // Parts 2 and 3 of the x set aren't published.
        assert!(matches!(
            resolved.split_set("q/x-00001-of-00003.gguf"),
            Err(HubError::Split(split::SplitError::Incomplete { .. }))
        ));

        let dir = tempfile::tempdir().unwrap();
        let total: u64 = set.iter().map(|f| f.size).sum();
        let mut last = 0;
        let model = client
            .download_split(&set, dir.path(), |done, all| {
                assert!(done <= all && all == total);
                last = done;
                true
            })
            .await
            .unwrap();
        assert_eq!(last, total);
        let id = split::set_id(set.iter().map(|f| f.sha256.as_str()));
        assert_eq!(model.model.provenance.sha256, id);
        assert_eq!(model.model.provenance.size, total);
        assert_eq!(model.model.provenance.display_name, "m.gguf");
        assert_eq!(model.model.summary.architecture(), "llama");
        assert_eq!(model.parts.len(), 2);
        // Published names, side by side, and nothing left in staging.
        for f in &set {
            assert!(dir.path().join(&id).join(&f.file_name).is_file());
        }
        assert_eq!(
            model.model.path,
            dir.path().join(&id).join("m-00001-of-00002.gguf")
        );
        assert!(!dir.path().join(format!("{id}.download")).exists());
    }

    #[tokio::test]
    async fn inconsistent_split_parts_are_refused() {
        // Part 2 claims to be part 1.
        let client = split_hub(gguf_with(&[], &[("split.no", 0), ("split.count", 2)])).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let set = resolved
            .split_set("q/m-00001-of-00002.gguf")
            .unwrap()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = client
            .download_split(&set, dir.path(), |_, _| true)
            .await
            .unwrap_err();
        assert!(
            matches!(err, HubError::Split(split::SplitError::Inconsistent(_))),
            "{err}"
        );
        // Nothing was admitted.
        let id = split::set_id(set.iter().map(|f| f.sha256.as_str()));
        assert!(!dir.path().join(&id).exists());
    }

    fn fixture(body: Vec<u8>) -> Fixture {
        let (digest, _) = integrity::sha256_reader(&body[..], |_| true).unwrap();
        Fixture {
            sha256: digest.to_hex(),
            body,
            truncate_first: AtomicBool::new(false),
            ignore_range: false,
            overlong: false,
            redirect_to: None,
            gated: false,
            served: AtomicUsize::new(0),
        }
    }

    #[test]
    fn host_policy_is_strict() {
        let p = HostPolicy::hugging_face();
        let ok = |s: &str| p.permits(&Url::parse(s).unwrap());
        assert!(ok("https://huggingface.co/api"));
        assert!(ok("https://us.aws.cdn.hf.co/xet-bridge-us/x"));
        assert!(ok("https://cdn-lfs.huggingface.co/x"));
        assert!(!ok("http://huggingface.co/api"));
        assert!(!ok("https://huggingface.co.evil.example/"));
        assert!(!ok("https://evilhf.co/"));
        assert!(!ok("https://user:pw@huggingface.co/"));
        assert!(!ok("https://127.0.0.1/"));
        assert!(!ok("https://[::1]/"));
        assert!(!ok("ftp://huggingface.co/"));
        assert!(!ok("file:///etc/passwd"));
    }

    #[test]
    fn offline_mode_prevents_any_client() {
        let config = HubConfig {
            offline: true,
            ..HubConfig::hugging_face()
        };
        assert!(matches!(HubClient::new(config), Err(HubError::Offline)));
    }

    #[test]
    fn identifiers_are_validated() {
        assert!(RepoId::parse("Qwen/Qwen3-0.6B-GGUF").is_ok());
        for bad in [
            "", "a", "a/", "/b", "a/b/c", "../b", "a/..", "a/b c", "a/b%2f", "a/.b", "-a/b",
        ] {
            assert!(RepoId::parse(bad).is_err(), "{bad}");
        }
        assert!(valid_revision("main"));
        assert!(valid_revision(COMMIT));
        for bad in ["", "..", "a/b", "a b", "-x", "refs%2Fpr"] {
            assert!(!valid_revision(bad), "{bad}");
        }
        assert!(CommitId::parse(COMMIT).is_some());
        assert!(CommitId::parse("main").is_none());
        assert!(valid_remote_path("sub/x.gguf"));
        assert!(!valid_remote_path("sub/../x.gguf"));
        assert!(!valid_remote_path("/x.gguf"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_lists_only_verifiable_safe_gguf_files() {
        let (client, _) = start(fixture(tiny_gguf())).await;
        let repo = RepoId::parse("Qwen/Qwen3-0.6B-GGUF").unwrap();
        let resolved = client.resolve(&repo, "main").await.unwrap();
        assert_eq!(resolved.commit.as_str(), COMMIT);
        assert_eq!(resolved.license.as_deref(), Some("apache-2.0"));
        let paths: Vec<_> = resolved.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["model-Q8_0.gguf", "sub/split-00001-of-00002.gguf"]);
        assert_eq!(resolved.files[1].file_name, "split-00001-of-00002.gguf");
        assert_eq!(resolved.skipped, 2);
        assert_eq!(resolved.downloads, Some(42));
        assert_eq!(resolved.created.as_deref(), Some("2026-11-20"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn download_retries_verifies_and_validates() {
        let f = fixture(tiny_gguf());
        // The first chunk response is cut short; the chunk is retried.
        f.truncate_first.store(true, Ordering::SeqCst);
        let (client, _) = start(f).await;
        let repo = RepoId::parse("o/n").unwrap();
        let resolved = client.resolve(&repo, "main").await.unwrap();
        let file = &resolved.files[0];
        assert!(file.size > 3 * 1024, "fixture must span several chunks");
        let dir = tempfile::tempdir().unwrap();

        let mut last = 0;
        let model = client
            .download(file, dir.path(), |done, total| {
                assert!(done <= total);
                last = done;
                true
            })
            .await
            .unwrap();
        assert_eq!(last, file.size);
        assert_eq!(model.summary.architecture(), "llama");
        match &model.provenance.source {
            ModelSource::Hub {
                repo,
                commit,
                path,
                license,
                ..
            } => {
                assert_eq!(repo, "o/n");
                assert_eq!(commit, COMMIT);
                assert_eq!(path, "model-Q8_0.gguf");
                assert_eq!(license.as_deref(), Some("apache-2.0"));
            }
            other => panic!("unexpected source {other:?}"),
        }
        assert_eq!(model.provenance.sha256, file.sha256);
        assert_eq!(
            model.path.file_name().unwrap().to_str().unwrap(),
            format!("{}.gguf", file.sha256)
        );
        // Only the admitted model remains: no partial or state file.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_download_resumes_without_refetching_done_chunks() {
        let (client, _, f) = start_with(fixture(tiny_gguf()), 1).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let file = &resolved.files[0];
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(format!("{}.state", file.sha256));

        // Cancel once two chunks' worth of bytes have arrived.
        let result = client
            .download(file, dir.path(), |done, _| done < 2048)
            .await;
        assert!(matches!(result, Err(HubError::Cancelled)), "{result:?}");
        assert!(state.exists());
        let part = dir.path().join(format!("{}.part", file.sha256));
        assert_eq!(std::fs::metadata(&part).unwrap().len(), file.size);

        let before = f.served.load(Ordering::SeqCst);
        client
            .download(file, dir.path(), |_, _| true)
            .await
            .unwrap();
        let refetched = f.served.load(Ordering::SeqCst) - before;
        assert!(
            refetched as u64 <= file.size - 1024,
            "refetched {refetched} of {}",
            file.size
        );
        assert!(!state.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sequential_partial_prefix_is_kept() {
        let body = tiny_gguf();
        let (client, _, f) = start_with(fixture(body.clone()), 2).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let file = &resolved.files[0];
        let dir = tempfile::tempdir().unwrap();
        // A partial file from the earlier sequential downloader.
        std::fs::write(
            dir.path().join(format!("{}.part", file.sha256)),
            &body[..2100],
        )
        .unwrap();
        client
            .download(file, dir.path(), |_, _| true)
            .await
            .unwrap();
        // Chunks 0 and 1 (bytes 0..2048) were already present.
        assert_eq!(f.served.load(Ordering::SeqCst), body.len() - 2048);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn complete_partial_file_is_verified_without_network() {
        let body = tiny_gguf();
        let mut f = fixture(body.clone());
        // Any download request would fail.
        f.redirect_to = Some("http://example.com/steal".into());
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let file = &resolved.files[0];
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{}.part", file.sha256)), &body).unwrap();
        let model = client
            .download(file, dir.path(), |_, _| true)
            .await
            .unwrap();
        assert_eq!(model.provenance.sha256, file.sha256);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn corrupt_state_file_restarts_cleanly() {
        let body = tiny_gguf();
        let (client, _, f) = start_with(fixture(body.clone()), 2).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let file = &resolved.files[0];
        let dir = tempfile::tempdir().unwrap();
        // Claims chunks are done, but for a different chunk size.
        std::fs::write(
            dir.path().join(format!("{}.state", file.sha256)),
            format!("gardenwall-download-v1 {} 999 ff", file.size),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(format!("{}.part", file.sha256)),
            vec![0u8; body.len()],
        )
        .unwrap();
        client
            .download(file, dir.path(), |_, _| true)
            .await
            .unwrap();
        assert_eq!(f.served.load(Ordering::SeqCst), body.len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn server_ignoring_ranges_is_refused() {
        let mut f = fixture(tiny_gguf());
        f.ignore_range = true;
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = &resolved.files[0];
        let result = client.download(file, dir.path(), |_, _| true).await;
        assert!(
            matches!(result, Err(HubError::RangesUnsupported)),
            "{result:?}"
        );
        assert!(!dir.path().join(format!("{}.gguf", file.sha256)).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn digest_mismatch_deletes_partial_file() {
        let mut f = fixture(tiny_gguf());
        f.sha256 = "0".repeat(64);
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = client
            .download(&resolved.files[0], dir.path(), |_, _| true)
            .await;
        assert!(matches!(
            result,
            Err(HubError::IntegrityMismatch(Verification::DigestMismatch))
        ));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_gguf_stays_quarantined_as_rejected() {
        let (client, _) = start(fixture(vec![7u8; 4096])).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = &resolved.files[0];
        let result = client.download(file, dir.path(), |_, _| true).await;
        assert!(matches!(result, Err(HubError::InvalidGguf(_))));
        assert!(
            dir.path()
                .join(format!("{}.rejected", file.sha256))
                .exists()
        );
        assert!(!dir.path().join(format!("{}.gguf", file.sha256)).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_response_is_rejected() {
        let mut f = fixture(tiny_gguf());
        // The server sends more than the requested range.
        f.overlong = true;
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = client
            .download(&resolved.files[0], dir.path(), |_, _| true)
            .await;
        assert!(matches!(result, Err(HubError::TooManyBytes)), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gated_downloads_say_why() {
        let mut f = fixture(tiny_gguf());
        f.gated = true;
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = client
            .download(&resolved.files[0], dir.path(), |_, _| true)
            .await;
        assert!(matches!(result, Err(HubError::Gated)), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn partial_paths_name_only_this_downloads_leftovers() {
        let lib = Path::new("lib");
        let file = |sha: &str| RemoteFile {
            repo: RepoId::parse("a/b").unwrap(),
            commit: CommitId::parse(&"0".repeat(40)).unwrap(),
            file_name: "m.gguf".into(),
            path: "m.gguf".into(),
            size: 1,
            sha256: sha.into(),
            license: None,
        };
        let sha = "ab".repeat(32);
        assert_eq!(
            partial_paths(&[file(&sha)], lib, false),
            vec![
                lib.join(format!("{sha}.part")),
                lib.join(format!("{sha}.state")),
                lib.join(format!("{sha}.state.tmp")),
            ]
        );
        // Never a path built from something that isn't a digest.
        assert!(partial_paths(&[file("../x")], lib, false).is_empty());
        let parts = [file(&sha), file(&"cd".repeat(32))];
        let id = split::set_id(parts.iter().map(|f| f.sha256.as_str()));
        assert_eq!(
            partial_paths(&parts, lib, true),
            vec![lib.join(format!("{id}.download"))]
        );
    }

    #[tokio::test]
    async fn cancellation_keeps_partial_for_resume() {
        let (client, _) = start(fixture(tiny_gguf())).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = &resolved.files[0];
        let result = client.download(file, dir.path(), |_, _| false).await;
        assert!(matches!(result, Err(HubError::Cancelled)));
        assert!(dir.path().join(format!("{}.part", file.sha256)).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redirects_to_disallowed_hosts_are_refused() {
        let mut f = fixture(tiny_gguf());
        f.redirect_to = Some("http://example.com/steal".into());
        let (client, _) = start(f).await;
        let resolved = client
            .resolve(&RepoId::parse("o/n").unwrap(), "main")
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = client
            .download(&resolved.files[0], dir.path(), |_, _| true)
            .await;
        assert!(matches!(result, Err(HubError::Http(_))), "{result:?}");
    }

    /// A hub that answers `/api/models` with `body`, refuses `sort=bogus`
    /// like the live hub (400), and records each query string.
    async fn ranking_hub(body: String) -> (HubClient, Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::extract::RawQuery;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = (seen.clone(), Arc::new(body));
        type St = (Arc<std::sync::Mutex<Vec<String>>>, Arc<String>);
        async fn models(State((seen, body)): State<St>, RawQuery(q): RawQuery) -> Response {
            let q = q.unwrap_or_default();
            seen.lock().unwrap().push(q.clone());
            if q.contains("sort=bogus") {
                return (StatusCode::BAD_REQUEST, "Invalid sort parameter").into_response();
            }
            (
                [(header::CONTENT_TYPE, "application/json")],
                body.as_str().to_owned(),
            )
                .into_response()
        }
        let app = axum::Router::new()
            .route("/api/models", get(models))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = HubConfig {
            base_url: Url::parse(&format!("http://{addr}/")).unwrap(),
            hosts: HostPolicy::loopback_for_tests(),
            ..HubConfig::hugging_face()
        };
        (HubClient::new(config).unwrap(), seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ranking_sends_fixed_parameters_and_tolerates_missing_fields() {
        let body = serde_json::json!([
            {"id": "org/full-GGUF", "downloads": 1234, "likes": 5, "gated": "manual",
             "createdAt": "2026-11-20T17:59:59.000Z", "trendingScore": 12.7,
             "tags": ["gguf", "license:apache-2.0"]},
            {"id": "org/bare"},
            {"id": "../bad"},
            {"nothing": true},
            "not an object"
        ])
        .to_string();
        let (client, seen) = ranking_hub(body).await;
        let rows = client
            .ranking("text-generation", "trendingScore", 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].repo.to_string(), "org/full-GGUF");
        assert_eq!(rows[0].downloads, Some(1234));
        assert_eq!(rows[0].gated, Some(true));
        assert_eq!(rows[0].created.as_deref(), Some("2026-11-20"));
        assert_eq!(rows[0].trending_score, Some(12));
        assert_eq!(rows[0].license.as_deref(), Some("apache-2.0"));
        let bare = &rows[1];
        assert_eq!(
            (bare.downloads, bare.gated, &bare.created, &bare.license),
            (None, None, &None, &None)
        );
        let q = seen.lock().unwrap()[0].clone();
        for part in [
            "filter=gguf",
            "pipeline_tag=text-generation",
            "sort=trendingScore",
            "direction=-1",
            "limit=50",
        ] {
            assert!(q.contains(part), "{q}");
        }
        assert!(!q.contains("search="), "{q}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ranking_reports_a_refused_sort_and_checks_parameters_locally() {
        let (client, seen) = ranking_hub("[]".into()).await;
        assert!(matches!(
            client.ranking("text-generation", "bogus", 50).await,
            Err(HubError::Status(400))
        ));
        for (kind, sort) in [
            ("text generation", "downloads"),
            ("x", "a&b=c"),
            ("", "downloads"),
        ] {
            assert!(matches!(
                client.ranking(kind, sort, 50).await,
                Err(HubError::InvalidQuery)
            ));
        }
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "invalid parameters never reach the hub"
        );
        // Not a list at all.
        let (client, _) = ranking_hub("{\"error\": 1}".into()).await;
        assert!(matches!(
            client.ranking("text-generation", "downloads", 50).await,
            Err(HubError::MalformedResponse)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ranking_refuses_an_oversized_response() {
        let row = serde_json::json!({"id": "org/model", "tags": ["x".repeat(1000)]}).to_string();
        let body = format!("[{}]", vec![row; 17_000].join(","));
        assert!(body.len() > MAX_API_RESPONSE_BYTES);
        let (client, _) = ranking_hub(body).await;
        assert!(matches!(
            client.ranking("text-generation", "downloads", 50).await,
            Err(HubError::ResponseTooLarge)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_rejects_bad_queries_locally() {
        let (client, _) = start(fixture(tiny_gguf())).await;
        for q in ["", "   ", "a\nb", &"x".repeat(201)] {
            assert!(matches!(
                client.search(q, 10).await,
                Err(HubError::InvalidQuery)
            ));
        }
    }
}
