//! The visited-sites list (ADR-018): every search and page a session or
//! chat asked for, how it was allowed, and how it ended.
//!
//! Built only by the core, as requests are prepared, decided and run;
//! never from model text. The model chooses what to ask for, so targets are
//! data: they are shown as plain text and never opened by the UI.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::state::now_ms;

/// Most entries kept per session (later ones are counted, not listed).
const MAX_VISITS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisitKind {
    Search,
    Page,
}

/// How a request was allowed, or why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Allowed {
    /// Waiting for the user.
    Pending,
    /// The user approved it in the dialog.
    Approved,
    /// Covered by the research grant or a "this session" grant.
    Granted,
    /// Typed by the user (`/web`, `/read`, Test connection).
    Typed,
    Denied,
    Expired,
    /// Refused by the core before anyone was asked (private address,
    /// secret, limit...).
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Visit {
    pub at_ms: i64,
    pub kind: VisitKind,
    /// The query or the page address.
    pub target: String,
    /// The search provider, or the page's site.
    pub via: String,
    pub allowed: Allowed,
    /// `None` until it ran (or if it never did).
    pub ok: Option<bool>,
    /// Bytes received, for a page.
    pub bytes: Option<usize>,
    /// Why it was blocked or failed.
    pub note: Option<String>,
}

/// One session's or chat request's list.
#[derive(Default)]
pub struct VisitLog {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    visits: Vec<Visit>,
    dropped: usize,
    /// Entries already handed out by [`VisitLog::take_new`].
    taken: usize,
}

fn clip(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

impl VisitLog {
    fn push(&self, visit: Visit) {
        if let Ok(mut g) = self.inner.lock() {
            if g.visits.len() < MAX_VISITS {
                g.visits.push(visit);
            } else {
                g.dropped += 1;
            }
        }
    }

    /// A request about to be decided (or, with `allowed`, already decided).
    pub fn start(&self, kind: VisitKind, target: &str, via: &str, allowed: Allowed) {
        self.push(Visit {
            at_ms: now_ms(),
            kind,
            target: clip(target, 2000),
            via: clip(via, 200),
            allowed,
            ok: None,
            bytes: None,
            note: None,
        });
    }

    /// A request refused before anyone was asked.
    pub fn blocked(&self, kind: VisitKind, target: &str, via: &str, why: &str) {
        self.start(kind, target, via, Allowed::Blocked);
        self.update(|v| v.note = Some(clip(why, 300)));
    }

    /// Change the newest entry (requests in one session run one at a time).
    fn update(&self, f: impl FnOnce(&mut Visit)) {
        if let Ok(mut g) = self.inner.lock()
            && let Some(v) = g.visits.last_mut()
        {
            f(v);
        }
    }

    /// The decision on the newest pending request.
    pub fn decided(&self, allowed: Allowed) {
        self.update(|v| {
            if v.allowed == Allowed::Pending {
                v.allowed = allowed;
            }
        });
    }

    /// How the newest request ended.
    pub fn finished(&self, result: Result<usize, &str>) {
        self.update(|v| match result {
            Ok(bytes) => {
                v.ok = Some(true);
                if v.kind == VisitKind::Page {
                    v.bytes = Some(bytes);
                }
            }
            Err(e) => {
                v.ok = Some(false);
                v.note = Some(clip(e, 300));
            }
        });
    }

    /// Every entry, and how many more weren't kept.
    pub fn all(&self) -> (Vec<Visit>, usize) {
        self.inner
            .lock()
            .map(|g| (g.visits.clone(), g.dropped))
            .unwrap_or_default()
    }

    /// Entries added since the last call (for saving one turn's list).
    pub fn take_new(&self) -> Vec<Visit> {
        self.inner
            .lock()
            .map(|mut g| {
                let new = g.visits[g.taken.min(g.visits.len())..].to_vec();
                g.taken = g.visits.len();
                new
            })
            .unwrap_or_default()
    }

    /// Restore a saved session's list (already saved, so not "new").
    pub fn restore(&self, visits: Vec<Visit>) {
        if let Ok(mut g) = self.inner.lock() {
            g.visits = visits;
            g.visits.truncate(MAX_VISITS);
            g.taken = g.visits.len();
        }
    }
}

/// Links in `text` (a model's answer) to pages that weren't fetched in this
/// session: a citation the model never opened (ADR-018 source check).
pub fn unopened_links(text: &str, visits: &[Visit]) -> Vec<String> {
    let fetched: Vec<String> = visits
        .iter()
        .filter(|v| v.kind == VisitKind::Page && v.ok == Some(true))
        .map(|v| normalize(&v.target))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for link in links(text) {
        let n = normalize(&link);
        if !fetched.contains(&n) && !out.contains(&link) {
            out.push(link);
        }
        if out.len() >= 50 {
            break;
        }
    }
    out
}

/// http(s) links in text, with trailing punctuation removed.
fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let tail = &rest[i..];
        if tail.starts_with("https://") || tail.starts_with("http://") {
            let end = tail
                .find(|c: char| c.is_whitespace() || "<>\"'`()[]{}".contains(c))
                .unwrap_or(tail.len());
            let link = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*', '_']);
            if link.len() > "https://".len() {
                out.push(link.to_string());
            }
            rest = &tail[end.max(1)..];
        } else {
            rest = &tail[4..];
        }
    }
    out
}

/// Compare addresses loosely: no scheme, no `www.`, no fragment, no
/// trailing slash, lowercase host.
fn normalize(url: &str) -> String {
    let u = url.split('#').next().unwrap_or(url);
    let u = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .unwrap_or(u);
    let u = u.strip_prefix("www.").unwrap_or(u);
    let (host, path) = u.split_once('/').unwrap_or((u, ""));
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        host.to_lowercase()
    } else {
        format!("{}/{path}", host.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_follows_each_request_from_start_to_end() {
        let log = VisitLog::default();
        log.start(VisitKind::Search, "tokio", "SearXNG", Allowed::Pending);
        log.decided(Allowed::Granted);
        log.finished(Ok(900));
        log.start(
            VisitKind::Page,
            "https://docs.rs/",
            "docs.rs",
            Allowed::Pending,
        );
        log.decided(Allowed::Approved);
        log.finished(Ok(1234));
        log.blocked(
            VisitKind::Page,
            "http://10.0.0.1/",
            "10.0.0.1",
            "private address",
        );
        let (all, dropped) = log.all();
        assert_eq!(dropped, 0);
        assert_eq!(all[0].allowed, Allowed::Granted);
        assert_eq!(all[0].bytes, None);
        assert_eq!(all[1].bytes, Some(1234));
        assert_eq!(all[2].allowed, Allowed::Blocked);
        assert_eq!(all[2].note.as_deref(), Some("private address"));
        assert_eq!(log.take_new().len(), 3);
        assert!(log.take_new().is_empty());
        // A decision never rewrites a request that was already decided.
        log.decided(Allowed::Denied);
        assert_eq!(log.all().0[2].allowed, Allowed::Blocked);
    }

    #[test]
    fn citations_to_pages_never_opened_are_found() {
        let log = VisitLog::default();
        log.start(
            VisitKind::Page,
            "https://docs.rs/tokio/latest/",
            "docs.rs",
            Allowed::Granted,
        );
        log.finished(Ok(10));
        log.start(
            VisitKind::Page,
            "https://failed.example/",
            "failed.example",
            Allowed::Granted,
        );
        log.finished(Err("timeout"));
        let answer = "See [1](https://www.docs.rs/tokio/latest) and https://made-up.example/paper.pdf. Also (https://failed.example/).";
        let missing = unopened_links(answer, &log.all().0);
        assert_eq!(
            missing,
            [
                "https://made-up.example/paper.pdf",
                "https://failed.example/"
            ]
        );
    }
}
