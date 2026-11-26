//! Web search and fetch (ROADMAP Phase 4, ADR-007 §10, ADR-018).
//!
//! - Every request is validated and secret-scanned here *before* the user
//!   is asked; the agent loop then gets the trusted approval (or a grant)
//!   and a single-use claim before [`AppWeb::run`] sends anything.
//! - Search goes to the provider chosen in Settings ([`provider`]).
//! - Fetched HTML is turned into text by the parse worker in its own OS
//!   sandbox (no network, no files), so the core never parses page content.
//!   Nothing fetched is ever rendered or run; links and redirects on a page
//!   are never followed by themselves.
//! - Fetched pages are cached per session, so reading another part costs no
//!   request; every request goes in the session's [`visits`] list.
//! - Offline mode turns all of this off: the tools aren't even offered.

pub mod provider;
pub mod visits;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gardenwall_agent::agent::{BoxFuture, Web, WebPlan, WebRequest};
use gardenwall_egress::EgressError;
use gardenwall_egress::fetch::{Limits, fetch};
use gardenwall_egress::scan::find_secrets;
use gardenwall_egress::target::{LocalException, origin_of, validate};
use url::Url;

use crate::state::{AppState, SearchProviderKind};
use provider::Provider;
use visits::{Allowed, VisitKind, VisitLog};

/// Time allowed for the parse worker.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
const PARSE_TIMEOUT: Duration = Duration::from_secs(20);
/// PDFs may need OCR (about a second a page, up to 300 pages).
const PDF_TIMEOUT: Duration = Duration::from_secs(600);
/// Pages kept per session, and their total size.
const CACHE_PAGES: usize = 40;
const CACHE_BYTES: usize = 16 << 20;
/// Longest search query.
const MAX_QUERY_CHARS: usize = 500;

/// Check a SearXNG base URL from Settings: http only on loopback, https
/// otherwise, no credentials, no query.
pub fn validate_searxng(raw: &str) -> Result<Url, String> {
    let mut url =
        Url::parse(raw.trim()).map_err(|_| "enter a full URL, such as http://127.0.0.1:8888")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL must not contain a user name or password".into());
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    let local = LocalException::for_base(&url).is_some();
    match url.scheme() {
        "https" => Ok(url),
        "http" if local => Ok(url),
        "http" => Err("use https:// for a SearXNG that is not on this computer".into()),
        _ => Err("the URL must start with http:// or https://".into()),
    }
}

/// The search provider chosen in Settings, if it is set up.
pub fn configured_provider(state: &AppState) -> Option<Provider> {
    let settings = state.settings();
    match settings.web.provider {
        SearchProviderKind::Searxng => settings
            .searxng_url
            .as_deref()
            .and_then(|u| validate_searxng(u).ok())
            .map(Provider::searxng),
        SearchProviderKind::Brave => {
            gardenwall_keychain::load_secret(&state.paths.brave_key, provider::BRAVE_PURPOSE)
                .ok()
                .flatten()
                .map(|key| Provider::Brave { key })
        }
    }
}

/// The visited lists saved for an agent session or chat, oldest first.
pub fn saved_visits(
    state: &AppState,
    owner: gardenwall_store::conversations::VisitOwner<'_>,
) -> Vec<visits::Visit> {
    let Ok(store) = state.conversations.lock() else {
        return Vec::new();
    };
    store
        .web_visits(owner)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, json)| serde_json::from_str::<Vec<visits::Visit>>(&json).ok())
        .flatten()
        .collect()
}

/// Save one turn's new visits for `owner` (nothing if there are none).
pub fn save_visits(
    state: &AppState,
    owner: gardenwall_store::conversations::VisitOwner<'_>,
    new: &[visits::Visit],
) -> Result<(), String> {
    if new.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(new).map_err(|e| e.to_string())?;
    let store = state
        .conversations
        .lock()
        .map_err(|_| "chat store unavailable".to_string())?;
    store
        .add_web_visits(owner, &json)
        .map_err(|e| e.to_string())
}

/// Fetched pages' text, newest last, bounded in count and size.
#[derive(Default)]
struct PageCache {
    pages: VecDeque<(String, Arc<String>)>,
    bytes: usize,
}

impl PageCache {
    fn get(&self, url: &str) -> Option<Arc<String>> {
        self.pages
            .iter()
            .find(|(u, _)| u == url)
            .map(|(_, t)| t.clone())
    }

    fn put(&mut self, url: String, text: Arc<String>) {
        if text.len() > CACHE_BYTES {
            return;
        }
        self.pages.retain(|(u, _)| *u != url);
        self.bytes = self.pages.iter().map(|(_, t)| t.len()).sum();
        while !self.pages.is_empty()
            && (self.pages.len() >= CACHE_PAGES || self.bytes + text.len() > CACHE_BYTES)
        {
            if let Some((_, old)) = self.pages.pop_front() {
                self.bytes -= old.len();
            }
        }
        self.bytes += text.len();
        self.pages.push_back((url, text));
    }
}

/// Ends the web access that is open when offline mode or lockdown turns on
/// (GW-019): requests in flight are cancelled,
/// and a gateway made before can't start another. Each gateway notes the
/// count when it is made; turning offline or lockdown on raises it.
#[derive(Default)]
pub struct NetStop {
    count: std::sync::atomic::AtomicU64,
    wake: tokio::sync::Notify,
}

/// What a request stopped this way returns.
pub const NET_STOPPED: &str =
    "offline mode or lockdown was turned on, so Gardenwall ended this session's web access";

impl NetStop {
    pub fn current(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// End every gateway's web access now.
    pub fn stop_all(&self) {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    pub fn stopped_since(&self, count: u64) -> bool {
        self.current() != count
    }

    /// Resolves once [`stop_all`](Self::stop_all) runs after `count`.
    pub async fn stopped(&self, count: u64) {
        loop {
            // Registered before the check, so a stop in between isn't lost.
            let woken = self.wake.notified();
            if self.stopped_since(count) {
                return;
            }
            woken.await;
        }
    }
}

pub struct AppWeb {
    /// Offline mode or lockdown ends this gateway ([`NetStop`]).
    net: Arc<NetStop>,
    /// The stop count when it was made.
    net_count: u64,
    provider: Option<Provider>,
    /// The staged parse worker; without it, only plain-text pages work.
    parser: Option<PathBuf>,
    grants_dir: PathBuf,
    cache: Mutex<PageCache>,
    visits: Arc<VisitLog>,
}

impl AppWeb {
    /// Web access for a new session or chat request, or `None` in offline
    /// mode. Requests are recorded in `visits`.
    pub fn new(state: &AppState, visits: Arc<VisitLog>) -> Option<Arc<Self>> {
        if state.settings().offline {
            return None;
        }
        let parser = if cfg!(any(windows, target_os = "linux")) {
            match crate::state::stage_helper(&state.paths.parse_dir, "gardenwall-parse") {
                Ok(p) => Some(p),
                Err(e) => {
                    state.add_notice(format!("Web pages can't be read in this session: {e}"));
                    None
                }
            }
        } else {
            None
        };
        Some(Arc::new(Self {
            net: state.net_stop.clone(),
            net_count: state.net_stop.current(),
            provider: configured_provider(state),
            parser,
            grants_dir: state.paths.grants.clone(),
            cache: Mutex::default(),
            visits,
        }))
    }

    /// For the agent header.
    pub fn describe(&self) -> String {
        match &self.provider {
            Some(p) => format!("web search ({}) and pages", p.name()),
            None => "web pages (set up search in Settings → Web access)".into(),
        }
    }

    /// A search the user typed (`/web`): no approval (the user asked for
    /// exactly this), but the same checks, and it is listed.
    pub async fn typed_search(&self, query: &str) -> Result<String, String> {
        let plan = self.prepare(WebRequest::Search(query.to_string()))?;
        self.visits.decided(Allowed::Typed);
        self.run(&plan).await
    }

    /// A page the user typed (`/read`), the same way.
    pub async fn typed_fetch(&self, url: &str) -> Result<(String, String), String> {
        let plan = self.prepare(WebRequest::Fetch(url.to_string()))?;
        if !plan.cached {
            self.visits.decided(Allowed::Typed);
        }
        let text = self.run(&plan).await?;
        Ok((plan.target, text))
    }

    async fn fetch_page(&self, raw: &str) -> Result<String, String> {
        // Page fetches never use the SearXNG exception.
        let target = validate(raw, &[]).map_err(|e| e.to_string())?;
        let accept = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1";
        let fetched = fetch(&target, accept, &[], &Limits::default())
            .await
            .map_err(|e| match e {
                EgressError::CrossOriginRedirect(to) => format!(
                    "the page redirects to another site ({to}); fetch that address in a new call if you still need it"
                ),
                other => other.to_string(),
            })?;
        let kind = fetched
            .content_type
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let looks_html = kind.contains("html")
            || (kind.is_empty()
                && fetched.body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<'));
        let text = if looks_html {
            let parser = self.parser.clone().ok_or(
                "web pages can't be read on this platform (no sandbox for the page reader)",
            )?;
            let journal = self.grants_dir.clone();
            let body = fetched.body;
            tokio::task::spawn_blocking(move || parse_sandboxed(&parser, &body, &journal, "html"))
                .await
                .map_err(|e| e.to_string())??
        } else if kind.starts_with("text/")
            || kind.contains("json")
            || kind.contains("xml")
            || kind.is_empty()
        {
            String::from_utf8_lossy(&fetched.body).into_owned()
        } else {
            // Programs, archives, images and the like are never saved or
            // opened; the model only hears what the address is.
            return Err(format!(
                "this address is a {} file, which web_fetch doesn't read",
                kind.split(';').next().unwrap_or("binary")
            ));
        };
        let mut out = text.trim().to_string();
        if fetched.url.as_str() != target.url.as_str() {
            out = format!("[Redirected within the site to {}]\n\n{out}", fetched.url);
        }
        if fetched.truncated {
            out.push_str("\n\n[The page was larger than the 5 MB limit and was cut.]");
        }
        Ok(out)
    }
}

/// Run the parse worker in its own sandbox: no network, read and execute
/// access to its own folder only, one process, bounded memory and time.
pub(crate) fn parse_sandboxed(
    exe: &Path,
    input: &[u8],
    journal: &Path,
    format: &str,
) -> Result<String, String> {
    let dir = exe
        .parent()
        .ok_or("invalid page reader path")?
        .to_path_buf();
    let out = crate::worker::run(
        crate::worker::WorkerSpec {
            profile: "Gardenwall.PageReader",
            program: exe,
            args: vec!["--format".into(), format.into()],
            read_execute: vec![dir.clone()],
            read_write: vec![],
            env: vec![],
            working_dir: dir,
            memory_bytes: 512 << 20,
            max_processes: 1,
            timeout: if format == "pdf" {
                PDF_TIMEOUT
            } else {
                PARSE_TIMEOUT
            },
            stdout_limit: gardenwall_parse_limit(),
            journal,
        },
        input,
    )?;
    match out.exit {
        None => Err("the page took too long to read".into()),
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Some(3) => {
            Err("the file could not be read (it may be damaged or encrypted, or a scan whose text Windows' OCR couldn't find)".into())
        }
        Some(code) => Err(format!("the page reader failed (exit code {code})")),
    }
}

/// Bytes accepted back from the worker (its own cap, plus room for UTF-8).
fn gardenwall_parse_limit() -> usize {
    4 * 400_000
}

impl Web for AppWeb {
    fn can_search(&self) -> bool {
        self.provider.is_some()
    }

    fn prepare(&self, request: WebRequest) -> Result<WebPlan, String> {
        let plan = self.plan(&request);
        let (kind, target, via) = match &request {
            WebRequest::Search(q) => (
                VisitKind::Search,
                q.clone(),
                self.provider.as_ref().map_or("", |p| p.name()).to_string(),
            ),
            WebRequest::Fetch(u) => (VisitKind::Page, u.clone(), site(u)),
        };
        match &plan {
            Ok(p) if p.cached => {}
            Ok(_) => self.visits.start(kind, &target, &via, Allowed::Pending),
            Err(why) => self.visits.blocked(kind, &target, &via, why),
        }
        plan
    }

    fn run<'a>(&'a self, plan: &'a WebPlan) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            // Pages already in this session's cache never reach the network
            // and are not listed again.
            if let WebRequest::Fetch(u) = &plan.request {
                let cached = self.cache.lock().ok().and_then(|c| c.get(u));
                if let Some(text) = cached {
                    return Ok(text.as_ref().clone());
                }
                // A plan prepared as cached skipped the approval, so it
                // must never reach the network: if the page has left the
                // cache since, fail closed and let the model ask again
                // (that request is approved like any other).
                if plan.cached {
                    return Err(
                        "that page is no longer in this session's cache; fetch it again".into(),
                    );
                }
            }
            // Offline mode or lockdown since this gateway was made: nothing
            // starts, and a request in flight is cancelled.
            let result = if self.net.stopped_since(self.net_count) {
                Err(NET_STOPPED.to_string())
            } else {
                tokio::select! {
                    r = self.run_network(&plan.request) => r,
                    () = self.net.stopped(self.net_count) => Err(NET_STOPPED.to_string()),
                }
            };
            self.visits
                .finished(result.as_ref().map(String::len).map_err(String::as_str));
            result
        })
    }

    fn note_blocked(&self, request: &WebRequest, why: &str) {
        match request {
            WebRequest::Search(q) => self.visits.blocked(
                VisitKind::Search,
                q,
                self.provider.as_ref().map_or("", |p| p.name()),
                why,
            ),
            WebRequest::Fetch(u) => self.visits.blocked(VisitKind::Page, u, &site(u), why),
        }
    }
}

impl AppWeb {
    /// The network part of a request (a search, or a page not cached).
    async fn run_network(&self, request: &WebRequest) -> Result<String, String> {
        match request {
            WebRequest::Search(q) => match &self.provider {
                Some(p) => p.search(q).await,
                None => Err("web search is not set up".into()),
            },
            WebRequest::Fetch(u) => {
                let page = self.fetch_page(u).await;
                if let (Ok(text), Ok(mut cache)) = (&page, self.cache.lock()) {
                    cache.put(u.clone(), Arc::new(text.clone()));
                }
                page
            }
        }
    }
}

impl AppWeb {
    fn plan(&self, request: &WebRequest) -> Result<WebPlan, String> {
        if self.net.stopped_since(self.net_count) {
            return Err(NET_STOPPED.into());
        }
        match request {
            WebRequest::Search(query) => {
                let provider = self.provider.as_ref().ok_or("web search is not set up")?;
                if query.chars().count() > MAX_QUERY_CHARS {
                    return Err(format!(
                        "the search query is too long ({MAX_QUERY_CHARS} characters at most)"
                    ));
                }
                let found = find_secrets(query);
                if !found.is_empty() {
                    return Err(EgressError::SecretDetected(found).to_string());
                }
                provider.check(query)?;
                Ok(WebPlan {
                    reason: format!("The agent wants to search the web. {}", provider.reason()),
                    operation: "Search the web".into(),
                    target: provider.destination(),
                    payload: Some(query.clone()),
                    grant: "search".into(),
                    request: request.clone(),
                    cached: false,
                })
            }
            WebRequest::Fetch(raw) => {
                let found = find_secrets(raw);
                if !found.is_empty() {
                    return Err(EgressError::SecretDetected(found).to_string());
                }
                let target = validate(raw, &[]).map_err(|e| e.to_string())?;
                let url = target.url.to_string();
                let cached = self.cache.lock().is_ok_and(|c| c.get(&url).is_some());
                Ok(WebPlan {
                    reason: "The agent wants to read a web page. The site will see your IP address and the page address. Nothing on the page is run or followed.".into(),
                    operation: "Fetch web page".into(),
                    target: url.clone(),
                    payload: None,
                    grant: format!("fetch:{}", origin_of(&target.url)),
                    request: WebRequest::Fetch(url),
                    cached,
                })
            }
        }
    }
}

/// The host of an address, for the visited list.
fn site(raw: &str) -> String {
    Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Whether `scope` is a grant the dialog may create.
pub fn valid_grant(scope: &str) -> bool {
    scope == "search"
        || scope
            .strip_prefix("fetch:https://")
            .is_some_and(|rest| !rest.is_empty() && rest.len() < 300 && !rest.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scanned PDF (one page that is only an image) is read by Windows'
    /// OCR inside the parse worker's real AppContainer. Needs the worker
    /// built (`cargo build -p gardenwall-parse`); skipped otherwise.
    #[cfg(windows)]
    #[test]
    fn scanned_pdfs_are_read_by_ocr_inside_the_sandbox() {
        let built = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(Path::parent)
            .map(|d| d.join("gardenwall-parse.exe"))
            .filter(|p| p.is_file());
        let Some(built) = built else {
            eprintln!("skipped: build gardenwall-parse first");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let worker_dir = dir.path().join("parse");
        std::fs::create_dir(&worker_dir).unwrap();
        let exe = worker_dir.join("gardenwall-parse.exe");
        std::fs::copy(&built, &exe).unwrap();
        let journal = dir.path().join("grants");
        std::fs::create_dir(&journal).unwrap();
        let pdf = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../gardenwall-parse/tests/fixtures/scanned.pdf"),
        )
        .unwrap();
        let text = parse_sandboxed(&exe, &pdf, &journal, "pdf").expect("OCR in the sandbox");
        assert!(text.contains("[Page 1, read by OCR]"), "{text}");
        assert!(text.contains("Harbor Supplies"), "{text}");
        assert!(text.contains("warehouse"), "{text}");
    }

    #[test]
    fn searxng_urls_are_checked() {
        assert_eq!(
            validate_searxng("http://127.0.0.1:8888").unwrap().as_str(),
            "http://127.0.0.1:8888/"
        );
        assert!(validate_searxng("https://searx.example.org/sub").is_ok());
        assert!(validate_searxng("http://searx.example.org").is_err());
        assert!(validate_searxng("http://192.168.1.4:8080").is_err());
        assert!(validate_searxng("https://u:p@searx.example.org").is_err());
        assert!(validate_searxng("ftp://x").is_err());
    }

    #[test]
    fn the_page_cache_is_bounded() {
        let mut c = PageCache::default();
        for i in 0..CACHE_PAGES + 5 {
            c.put(format!("https://a.io/{i}"), Arc::new("x".repeat(10)));
        }
        assert_eq!(c.pages.len(), CACHE_PAGES);
        assert!(c.get("https://a.io/0").is_none());
        assert!(
            c.get(&format!("https://a.io/{}", CACHE_PAGES + 4))
                .is_some()
        );
        c.put("big".into(), Arc::new("y".repeat(CACHE_BYTES - 5)));
        assert!(c.bytes <= CACHE_BYTES);
        assert_eq!(c.pages.len(), 1);
    }

    /// Runs the real worker in its sandbox, and fetches one public page.
    /// Opt-in: `GARDENWALL_TEST_PARSER=<path to gardenwall-parse>` and,
    /// for the network part, `GARDENWALL_TEST_NET=1`.
    #[tokio::test]
    async fn the_page_reader_runs_sandboxed_and_reads_a_real_page() {
        let Some(exe) = std::env::var_os("GARDENWALL_TEST_PARSER").map(PathBuf::from) else {
            eprintln!("skipped: GARDENWALL_TEST_PARSER is not set");
            return;
        };
        let journal = std::env::temp_dir().join("gw-test-grants");
        let _ = std::fs::create_dir_all(&journal);
        let text = parse_sandboxed(
            &exe,
            b"<title>T</title><p>Hello <b>world</b></p><script>x()</script>",
            &journal,
            "html",
        )
        .unwrap();
        assert_eq!(
            text,
            "# T

Hello world"
        );
        if std::env::var_os("GARDENWALL_TEST_NET").is_none() {
            return;
        }
        let web = AppWeb {
            net: Arc::default(),
            net_count: 0,
            provider: None,
            parser: Some(exe),
            grants_dir: journal,
            cache: Mutex::default(),
            visits: Arc::default(),
        };
        let plan = web
            .prepare(WebRequest::Fetch("https://example.com/".into()))
            .unwrap();
        assert_eq!(plan.grant, "fetch:https://example.com:443");
        let page = web.run(&plan).await.unwrap();
        eprintln!("{page}");
        assert!(page.contains("Example Domain"));
        // Read again from the cache: no new request is listed.
        assert!(
            web.prepare(WebRequest::Fetch("https://example.com/".into()))
                .unwrap()
                .cached
        );
        assert_eq!(web.visits.all().0.len(), 1);
    }

    /// Offline mode or lockdown (GW-019): a gateway made before can't plan
    /// another request, and waiting on the stop wakes at once.
    #[tokio::test]
    async fn a_stopped_gateway_starts_nothing() {
        let net: Arc<NetStop> = Arc::default();
        let web = AppWeb {
            net: net.clone(),
            net_count: net.current(),
            provider: None,
            parser: None,
            grants_dir: std::env::temp_dir(),
            cache: Mutex::default(),
            visits: Arc::default(),
        };
        let waiting = {
            let net = net.clone();
            let count = net.current();
            tokio::spawn(async move { net.stopped(count).await })
        };
        net.stop_all();
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("a request in flight is woken by the stop")
            .unwrap();
        let err = web
            .prepare(WebRequest::Fetch("https://example.com/".into()))
            .unwrap_err();
        assert_eq!(err, NET_STOPPED);
    }

    /// A plan prepared as cached skipped the approval; if the page has left
    /// the cache since, running it must fail without any request.
    #[tokio::test]
    async fn a_cached_plan_never_reaches_the_network() {
        let web = AppWeb {
            net: Arc::default(),
            net_count: 0,
            provider: None,
            parser: None,
            grants_dir: std::env::temp_dir(),
            cache: Mutex::default(),
            visits: Arc::default(),
        };
        web.cache
            .lock()
            .unwrap()
            .put("https://a.io/p".into(), Arc::new("page".into()));
        let plan = web
            .prepare(WebRequest::Fetch("https://a.io/p".into()))
            .unwrap();
        assert!(plan.cached);
        assert_eq!(web.run(&plan).await.unwrap(), "page");
        // Evicted between prepare and run: refused at once, nothing listed.
        *web.cache.lock().unwrap() = PageCache::default();
        let err = web.run(&plan).await.unwrap_err();
        assert!(err.contains("no longer in this session's cache"), "{err}");
        assert!(web.visits.all().0.is_empty());
    }

    #[test]
    fn only_well_formed_grants_are_accepted() {
        assert!(valid_grant("search"));
        assert!(valid_grant("fetch:https://docs.rs:443"));
        assert!(!valid_grant("fetch:http://docs.rs:80"));
        assert!(!valid_grant("fetch:https://"));
        assert!(!valid_grant("fetch:https://a.io/path"));
        assert!(!valid_grant("everything"));
    }
}
