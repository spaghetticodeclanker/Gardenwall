//! Web research commands (ADR-018): research sessions and their approval
//! mode, the visited-sites list, Settings → Web access, and chat's typed
//! `/web` and `/read`.
//!
//! Everything here re-checks what the webview asks for: the research grant
//! exists only in research sessions, keys never come back out, and typed
//! requests pass the same validation and secret scan as the model's.

use std::sync::Arc;

use gardenwall_store::conversations::VisitOwner;
use serde::Serialize;
use tauri::State;

use crate::agent::AgentView;
use crate::state::{AppState, SearchProviderKind, WebMode};
use crate::web::provider::{self, ConnectionTest, Provider};
use crate::web::visits::{Visit, VisitLog, unopened_links};

type CmdResult<T> = Result<T, String>;

/// The research grant's scopes: searches, and any public HTTPS page.
const RESEARCH_GRANT: [&str; 2] = ["search", "fetch:public"];

/// Start a research session (no folders, no shell, web tools only) in the
/// approval mode the user picked in the trusted start dialog.
#[tauri::command]
pub async fn agent_start_research(
    state: State<'_, AppState>,
    mode: WebMode,
) -> CmdResult<AgentView> {
    let view = crate::agent::begin(&state, Vec::new(), None, None, true, None).await?;
    set_mode(&state, &view.id, mode).await
}

/// Change a research session's approval mode. "Ask for each page" ends
/// every grant the session has; research mode is only for research
/// sessions, never for sessions with folders.
#[tauri::command]
pub async fn agent_set_web_mode(
    state: State<'_, AppState>,
    sid: String,
    mode: WebMode,
) -> CmdResult<AgentView> {
    set_mode(&state, &sid, mode).await
}

async fn set_mode(state: &AppState, sid: &str, mode: WebMode) -> CmdResult<AgentView> {
    let live = crate::agent::live(state, sid)?;
    let mut session = live.session.lock().await;
    if session.research.is_none() {
        return Err("research mode is only for research sessions".into());
    }
    let mut grants = state.egress_grants.lock().map_err(|e| e.to_string())?;
    let set = grants.entry(live.approval_session).or_default();
    match mode {
        WebMode::Ask => set.clear(),
        WebMode::Research => set.extend(RESEARCH_GRANT.map(String::from)),
    }
    drop(grants);
    session.research = Some(mode);
    Ok(crate::agent::view_of(&live, &session))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitsView {
    pub visits: Vec<Visit>,
    /// Entries beyond the list's limit, counted only.
    pub dropped: usize,
    /// Links in the latest answer to pages never fetched in this session.
    pub unopened: Vec<String>,
}

fn visits_view(log: &VisitLog, answer: Option<&str>) -> VisitsView {
    let (visits, dropped) = log.all();
    let unopened = answer.map_or_else(Vec::new, |a| unopened_links(a, &visits));
    VisitsView {
        visits,
        dropped,
        unopened,
    }
}

/// A session's visited list, with the source check of its latest answer.
#[tauri::command]
pub async fn agent_visits(state: State<'_, AppState>, sid: String) -> CmdResult<VisitsView> {
    let live = crate::agent::live(&state, &sid)?;
    let session = live.session.lock().await;
    let log = session.visits.clone().unwrap_or_default();
    let answer = session.agent.last_answer();
    Ok(visits_view(&log, answer.as_deref()))
}

/// A chat's saved visited list, with the source check of `answer` (the
/// reply shown, given by id so the core reads its own copy).
#[tauri::command]
pub fn chat_visits(
    state: State<'_, AppState>,
    conversation_id: String,
    answer_id: Option<i64>,
) -> CmdResult<VisitsView> {
    let id =
        gardenwall_store::conversations::parse_id(&conversation_id).map_err(|e| e.to_string())?;
    let log = VisitLog::default();
    log.restore(crate::web::saved_visits(&state, VisitOwner::Chat(&id)));
    let answer = match answer_id {
        Some(mid) => state
            .conversations
            .lock()
            .map_err(|e| e.to_string())?
            .get(&id)
            .map_err(|e| e.to_string())?
            .messages
            .into_iter()
            .find(|m| m.id == mid)
            .map(|m| m.content),
        None => None,
    };
    Ok(visits_view(&log, answer.as_deref()))
}

// ---------- Settings → Web access ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebStatus {
    pub settings: crate::state::WebSettings,
    pub searxng_url: Option<String>,
    /// A Brave key is saved (the key itself never leaves the core).
    pub brave_key_saved: bool,
    /// Keys can be kept wrapped on this system.
    pub keys_supported: bool,
    pub offline: bool,
}

#[tauri::command]
pub fn web_status(state: State<'_, AppState>) -> WebStatus {
    let s = state.settings();
    WebStatus {
        settings: s.web.clone(),
        searxng_url: s.searxng_url.clone(),
        brave_key_saved: state.paths.brave_key.is_file(),
        keys_supported: cfg!(windows),
        offline: s.offline,
    }
}

#[tauri::command]
pub fn set_web_settings(
    state: State<'_, AppState>,
    provider: SearchProviderKind,
    default_mode: WebMode,
    max_searches: u32,
    max_pages: u32,
    review_findings: Option<bool>,
) -> CmdResult<WebStatus> {
    if !(1..=500).contains(&max_searches) || !(1..=1000).contains(&max_pages) {
        return Err("limits must be 1–500 searches and 1–1000 pages".into());
    }
    state
        .update_settings(|s| {
            s.web.provider = provider;
            s.web.default_mode = default_mode;
            s.web.max_searches = max_searches;
            s.web.max_pages = max_pages;
            if let Some(review) = review_findings {
                s.web.review_findings = review;
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(web_status(state))
}

/// Save the Brave Search key (wrapped by the OS), or remove it with `None`.
#[tauri::command]
pub fn set_brave_key(state: State<'_, AppState>, key: Option<String>) -> CmdResult<WebStatus> {
    match key {
        Some(raw) => {
            let key = zeroize::Zeroizing::new(provider::clean_key(&raw)?);
            gardenwall_keychain::save_secret(&state.paths.brave_key, provider::BRAVE_PURPOSE, &key)
                .map_err(|e| e.to_string())?;
        }
        None => {
            gardenwall_keychain::delete_secret(&state.paths.brave_key).map_err(|e| e.to_string())?
        }
    }
    Ok(web_status(state))
}

/// Run a fixed, harmless query with the provider as configured now.
#[tauri::command]
pub async fn web_test_connection(state: State<'_, AppState>) -> CmdResult<ConnectionTest> {
    if state.settings().offline {
        return Err("offline mode is on: turn it off to use the web".into());
    }
    let provider =
        crate::web::configured_provider(&state).ok_or(match state.settings().web.provider {
            SearchProviderKind::Searxng => "enter your SearXNG address first",
            SearchProviderKind::Brave => "save your Brave Search API key first",
        })?;
    Ok(provider::test_connection(&provider).await)
}

/// Look for a SearXNG on this computer's usual ports (loopback only).
/// Returns the first address that answers like SearXNG.
#[tauri::command]
pub async fn web_find_searxng(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    if state.settings().offline {
        return Err("offline mode is on: turn it off to use the web".into());
    }
    for port in provider::LOCAL_PORTS {
        let Ok(base) = url::Url::parse(&format!("http://127.0.0.1:{port}/")) else {
            continue;
        };
        let p = Provider::searxng(base.clone());
        let test = provider::test_connection(&p).await;
        let looks_like_searxng = test.ok
            || test
                .error
                .as_deref()
                .is_some_and(|e| e.contains("refused JSON results") || e.contains("no results"));
        if looks_like_searxng {
            return Ok(Some(base.to_string()));
        }
    }
    Ok(None)
}

// ---------- chat: /web and /read ----------

/// Run a request the user typed in a chat and hold the result as an
/// attachment for their next message. The request is checked like the
/// model's (offline mode, private addresses, secrets) and listed in the
/// chat's visited list; it needs no approval, since the user asked for
/// exactly this.
#[tauri::command]
pub async fn chat_web(
    state: State<'_, AppState>,
    conversation_id: String,
    command: String,
    argument: String,
) -> CmdResult<crate::attach::Attachment> {
    let id =
        gardenwall_store::conversations::parse_id(&conversation_id).map_err(|e| e.to_string())?;
    let argument = argument.trim();
    if argument.is_empty() {
        return Err(format!(
            "/{command} needs {}",
            if command == "web" {
                "a search"
            } else {
                "an address"
            }
        ));
    }
    let log = Arc::new(VisitLog::default());
    let web = crate::web::AppWeb::new(&state, log.clone())
        .ok_or("offline mode is on: turn it off to use the web")?;
    let result = match command.as_str() {
        "web" => web.typed_search(argument).await.map(|text| {
            (
                "search results",
                provider_name(&state),
                format!("Search: {argument}"),
                text,
            )
        }),
        "read" => web.typed_fetch(argument).await.map(|(url, text)| {
            (
                "page",
                url.clone(),
                url,
                gardenwall_agent::web_text::window(&text, 0),
            )
        }),
        _ => return Err("unknown web command".into()),
    };
    // Listed whether it worked or not.
    crate::web::save_visits(&state, VisitOwner::Chat(&id), &log.take_new())?;
    let (kind, source, name, body) = result?;
    let (block, flagged) = gardenwall_agent::web_text::untrusted_block(kind, &source, &body);
    let mut summary = format!("{} characters from the web", body.chars().count());
    if flagged > 0 {
        summary.push_str(&format!(
            " · {flagged} line(s) flagged as instructions to an AI"
        ));
    }
    crate::attach::hold_block(&state, &name, "web", format!("\n\n{block}"), summary)
}

fn provider_name(state: &AppState) -> String {
    crate::web::configured_provider(state)
        .map(|p| p.name().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::agent::grant_covers;

    #[test]
    fn the_research_grant_covers_public_https_pages_and_searches_only() {
        let set: HashSet<String> = super::RESEARCH_GRANT.map(String::from).into();
        assert!(grant_covers(&set, "search"));
        assert!(grant_covers(&set, "fetch:https://docs.rs:443"));
        // Never MCP tools or anything else.
        assert!(!grant_covers(&set, "mcp:mcp__fs__write"));
        assert!(!grant_covers(&set, "fetch:http://docs.rs:80"));
        let site: HashSet<String> = ["fetch:https://docs.rs:443".to_string()].into();
        assert!(grant_covers(&site, "fetch:https://docs.rs:443"));
        assert!(!grant_covers(&site, "fetch:https://evil.example:443"));
    }
}
