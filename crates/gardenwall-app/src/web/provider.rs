//! Search providers (ADR-018): SearXNG (the user's own, usually local) and
//! the Brave Search API (an independent index, with the user's own key).
//!
//! Both go through the egress gateway (DNS-pinned, public addresses only,
//! size and time limits). The Brave key goes only to its exact HTTPS host,
//! in a sensitive header, never in a URL, a log, the model or the webview.

use std::time::{Duration, Instant};

use gardenwall_egress::EgressError;
use gardenwall_egress::fetch::{Limits, fetch_with_headers};
use gardenwall_egress::target::{LocalException, validate};
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

/// Results shown per search.
const MAX_RESULTS: usize = 8;
/// Longest snippet kept per result (characters).
const MAX_SNIPPET_CHARS: usize = 400;
const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
/// The key file's purpose, mixed into its wrapping.
pub const BRAVE_PURPOSE: &str = "brave-search";
/// Longest key accepted (Brave keys are about 30 characters).
const MAX_KEY_CHARS: usize = 200;

pub enum Provider {
    Searxng {
        base: Url,
        exceptions: Vec<LocalException>,
    },
    Brave {
        key: Zeroizing<String>,
    },
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl Provider {
    pub fn searxng(base: Url) -> Self {
        let exceptions = LocalException::for_base(&base).into_iter().collect();
        Self::Searxng { base, exceptions }
    }

    /// Short name, for the dialog and the visited list.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Searxng { .. } => "SearXNG",
            Self::Brave { .. } => "Brave Search",
        }
    }

    /// Where queries go, for the approval dialog.
    pub fn destination(&self) -> String {
        match self {
            Self::Searxng { base, .. } => base.to_string(),
            Self::Brave { .. } => "https://api.search.brave.com/".into(),
        }
    }

    /// The dialog's plain-language reason.
    pub fn reason(&self) -> String {
        match self {
            Self::Searxng { base, .. } => format!(
                "The query goes to your SearXNG ({base}), which forwards it to search engines."
            ),
            Self::Brave { .. } => {
                "The query goes to Brave Search with your API key. Brave sees the query and your IP address.".into()
            }
        }
    }

    pub fn exceptions(&self) -> &[LocalException] {
        match self {
            Self::Searxng { exceptions, .. } => exceptions,
            Self::Brave { .. } => &[],
        }
    }

    fn url(&self, query: &str) -> Result<Url, String> {
        let mut url = match self {
            Self::Searxng { base, .. } => base.join("search").map_err(|_| "invalid SearXNG URL")?,
            Self::Brave { .. } => Url::parse(BRAVE_ENDPOINT).map_err(|_| "invalid endpoint")?,
        };
        match self {
            Self::Searxng { .. } => url
                .query_pairs_mut()
                .append_pair("q", query)
                .append_pair("format", "json"),
            Self::Brave { .. } => url
                .query_pairs_mut()
                .append_pair("q", query)
                .append_pair("count", &MAX_RESULTS.to_string())
                .append_pair("safesearch", "moderate"),
        };
        Ok(url)
    }

    /// Validate the request for `query` without sending it.
    pub fn check(&self, query: &str) -> Result<(), String> {
        let url = self.url(query)?;
        validate(url.as_str(), self.exceptions())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Run a search; results as a numbered list for the model.
    pub async fn search(&self, query: &str) -> Result<String, String> {
        Ok(format_hits(&self.hits(query).await?))
    }

    pub async fn hits(&self, query: &str) -> Result<Vec<Hit>, String> {
        let url = self.url(query)?;
        let target = validate(url.as_str(), self.exceptions()).map_err(|e| e.to_string())?;
        let limits = Limits {
            max_bytes: 2 << 20,
            total_timeout: Duration::from_secs(20),
            ..Limits::default()
        };
        let headers: Vec<(&'static str, &str)> = match self {
            Self::Searxng { .. } => vec![],
            Self::Brave { key } => vec![("X-Subscription-Token", key.as_str())],
        };
        let fetched = fetch_with_headers(
            &target,
            "application/json",
            &headers,
            self.exceptions(),
            &limits,
        )
        .await
        .map_err(|e| self.explain(e))?;
        if fetched.truncated {
            return Err("the search response was too large".into());
        }
        match self {
            Self::Searxng { .. } => parse_searxng(&fetched.body),
            Self::Brave { .. } => parse_brave(&fetched.body),
        }
    }

    /// An error with the exact fix where there is one.
    fn explain(&self, e: EgressError) -> String {
        match (self, &e) {
            (Self::Searxng { .. }, EgressError::Status(403)) => "SearXNG refused JSON results. Add `json` under `search: formats:` in its settings.yml and restart it.".into(),
            (Self::Brave { .. }, EgressError::Status(401 | 403)) => "Brave Search refused the API key. Check it in Settings → Web access (it starts with BSA).".into(),
            (Self::Brave { .. }, EgressError::Status(422)) => "Brave Search refused the request (422): the key may not include the Web Search API.".into(),
            (Self::Brave { .. }, EgressError::Status(429)) => "Brave Search's rate limit was reached: wait a moment, or check your plan's monthly quota.".into(),
            (Self::Searxng { .. }, EgressError::Http(_) | EgressError::Timeout) => format!("SearXNG didn't answer ({e}). Is it running? See Settings → Web access → Set up SearXNG."),
            _ => e.to_string(),
        }
    }
}

/// Check and clean an API key typed in Settings.
pub fn clean_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("enter the API key".into());
    }
    if key.chars().count() > MAX_KEY_CHARS
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err("that doesn't look like an API key (letters, digits, - and _ only)".into());
    }
    Ok(key.to_string())
}

#[derive(Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearxResult>,
}

#[derive(Deserialize)]
struct SearxResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

fn parse_searxng(body: &[u8]) -> Result<Vec<Hit>, String> {
    let parsed: SearxResponse = serde_json::from_slice(body)
        .map_err(|_| "SearXNG returned something other than JSON results")?;
    Ok(hits(
        parsed
            .results
            .into_iter()
            .map(|r| (r.title, r.url, r.content)),
    ))
}

fn parse_brave(body: &[u8]) -> Result<Vec<Hit>, String> {
    let parsed: BraveResponse = serde_json::from_slice(body)
        .map_err(|_| "Brave Search returned something other than JSON results")?;
    let results = parsed.web.map(|w| w.results).unwrap_or_default();
    // Brave marks matches with <strong>; the model gets plain text.
    Ok(hits(results.into_iter().map(|r| {
        (strip_tags(&r.title), r.url, strip_tags(&r.description))
    })))
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Web results only (no `javascript:` or other schemes), bounded.
fn hits(results: impl Iterator<Item = (String, String, String)>) -> Vec<Hit> {
    results
        .filter(|(_, url, _)| url.starts_with("http://") || url.starts_with("https://"))
        .take(MAX_RESULTS)
        .map(|(title, url, snippet)| Hit {
            title: clip(&title, 200),
            url: clip(&url, 500),
            snippet: clip(&snippet, MAX_SNIPPET_CHARS),
        })
        .collect()
}

fn clip(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

pub fn format_hits(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "No results.".into();
    }
    hits.iter()
        .enumerate()
        .map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What Settings → Test connection reports.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTest {
    pub ok: bool,
    pub provider: String,
    pub results: usize,
    pub millis: u64,
    /// The first result's title, so the user sees real results came back.
    pub sample: Option<String>,
    /// What went wrong, with the fix where known.
    pub error: Option<String>,
}

/// Run a fixed, harmless query.
pub async fn test_connection(provider: &Provider) -> ConnectionTest {
    let started = Instant::now();
    let result = provider.hits("open source software").await;
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(hits) if hits.is_empty() => ConnectionTest {
            ok: false,
            provider: provider.name().into(),
            results: 0,
            millis,
            sample: None,
            error: Some(match provider {
                Provider::Searxng { .. } => {
                    "It answered with no results: check that SearXNG has search engines enabled."
                        .into()
                }
                Provider::Brave { .. } => "It answered with no results.".into(),
            }),
        },
        Ok(hits) => ConnectionTest {
            ok: true,
            provider: provider.name().into(),
            results: hits.len(),
            millis,
            sample: hits.first().map(|h| h.title.clone()),
            error: None,
        },
        Err(e) => ConnectionTest {
            ok: false,
            provider: provider.name().into(),
            results: 0,
            millis,
            sample: None,
            error: Some(e),
        },
    }
}

/// Loopback ports a local SearXNG commonly uses.
pub const LOCAL_PORTS: [u16; 4] = [8888, 8080, 8081, 4000];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searxng_results_are_listed_and_bounded() {
        let body = serde_json::json!({
            "results": [
                {"title": "Tokio  docs", "url": "https://docs.rs/tokio", "content": "An async runtime"},
                {"title": "bad", "url": "javascript:alert(1)", "content": ""},
                {"title": "Long", "url": "https://x.io", "content": "y".repeat(1000)}
            ]
        });
        let out = format_hits(&parse_searxng(body.to_string().as_bytes()).unwrap());
        assert!(out.starts_with("1. Tokio docs\n   https://docs.rs/tokio\n   An async runtime"));
        assert!(!out.contains("javascript"));
        assert!(out.contains("2. Long"));
        assert!(out.len() < 1200);
        assert_eq!(
            format_hits(&parse_searxng(b"{\"results\":[]}").unwrap()),
            "No results."
        );
        assert!(parse_searxng(b"<html>").is_err());
    }

    #[test]
    fn brave_results_are_plain_text() {
        let body = serde_json::json!({
            "web": {"results": [
                {"title": "<strong>Tokio</strong> &amp; you", "url": "https://tokio.rs", "description": "An <strong>async</strong> runtime"},
                {"title": "x", "url": "data:text/html,hi", "description": ""}
            ]}
        });
        let hits = parse_brave(body.to_string().as_bytes()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Tokio & you");
        assert_eq!(hits[0].snippet, "An async runtime");
        assert!(parse_brave(b"{}").unwrap().is_empty());
    }

    #[test]
    fn queries_go_only_where_they_should() {
        let brave = Provider::Brave {
            key: Zeroizing::new("BSAkey".into()),
        };
        let url = brave.url("a b").unwrap();
        assert_eq!(url.host_str(), Some("api.search.brave.com"));
        assert_eq!(url.scheme(), "https");
        // The key is a header, never part of the address.
        assert!(!url.as_str().contains("BSAkey"));
        brave.check("rust").unwrap();
        let local = Provider::searxng(Url::parse("http://127.0.0.1:8888/").unwrap());
        local.check("rust").unwrap();
        assert!(local.url("q").unwrap().as_str().contains("format=json"));
    }

    #[test]
    fn keys_are_checked() {
        assert_eq!(clean_key("  BSA-abc_123 ").unwrap(), "BSA-abc_123");
        assert!(clean_key("").is_err());
        assert!(clean_key("key with spaces").is_err());
        assert!(clean_key(&"k".repeat(201)).is_err());
    }
}
