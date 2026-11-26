//! The chat Web switch (ADR-018): with it on, a chat's replies may search
//! and read pages, through the same harness, approvals and guardrails as a
//! research session.
//!
//! - Kept in memory only: after a restart every chat's switch is off.
//! - Each chat with the switch on has its own approval session, page cache
//!   and visited list; turning it off ends them (and any grant).
//! - A turn runs a research [`Agent`] seeded with the chat's messages; only
//!   the final answer is stored in the chat (web content stays out of the
//!   saved history), and the turn's visits are saved with the chat.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gardenwall_agent::agent::{Agent, AgentEvent, Restore, TurnEnd, WebLimits};
use gardenwall_agent::tools::Workspaces;
use gardenwall_inference::client::{ChatMessage, InferenceClient, Role};
use gardenwall_policy::tools::SessionPolicy;
use gardenwall_store::approvals::SessionId;
use gardenwall_store::conversations::{MessageStats, VisitOwner};
use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;

use crate::agent::{ClientModel, StoreApprover};
use crate::commands::ChatEvent;
use crate::state::{AppState, WebMode, now_ms};
use crate::web::AppWeb;
use crate::web::visits::VisitLog;

type CmdResult<T> = Result<T, String>;

pub struct ChatWeb {
    pub mode: std::sync::Mutex<WebMode>,
    pub approval_session: SessionId,
    web: Arc<AppWeb>,
    visits: Arc<VisitLog>,
}

/// Chats with the switch on, by conversation id.
pub type ChatWebs = std::sync::Mutex<HashMap<String, Arc<ChatWeb>>>;

pub fn active(state: &AppState, conversation: &str) -> Option<Arc<ChatWeb>> {
    state.chat_webs.lock().ok()?.get(conversation).cloned()
}

/// Approval sessions of chats with the switch on, for the dialog.
pub fn approval_sessions(state: &AppState) -> Vec<(SessionId, String)> {
    state
        .chat_webs
        .lock()
        .map(|m| {
            m.values()
                .map(|c| (c.approval_session, "Chat with web access".to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatWebView {
    pub on: bool,
    pub mode: Option<WebMode>,
}

#[tauri::command]
pub fn chat_web_state(state: State<'_, AppState>, conversation_id: String) -> ChatWebView {
    match active(&state, &conversation_id) {
        Some(c) => ChatWebView {
            on: true,
            mode: c.mode.lock().ok().map(|m| *m),
        },
        None => ChatWebView {
            on: false,
            mode: None,
        },
    }
}

/// Turn a chat's Web switch on (in `mode`) or off, or change its mode.
#[tauri::command]
pub fn chat_set_web(
    state: State<'_, AppState>,
    conversation_id: String,
    mode: Option<WebMode>,
) -> CmdResult<ChatWebView> {
    let id =
        gardenwall_store::conversations::parse_id(&conversation_id).map_err(|e| e.to_string())?;
    match mode {
        None => end(&state, &id),
        Some(mode) => {
            let chat = match active(&state, &id) {
                Some(c) => c,
                None => start(&state, &id)?,
            };
            if let Ok(mut m) = chat.mode.lock() {
                *m = mode;
            }
            let mut grants = state.egress_grants.lock().map_err(|e| e.to_string())?;
            let set = grants.entry(chat.approval_session).or_default();
            match mode {
                WebMode::Ask => set.clear(),
                WebMode::Research => {
                    set.insert("search".into());
                    set.insert("fetch:public".into());
                }
            }
        }
    }
    Ok(chat_web_state(state, id))
}

fn start(state: &AppState, id: &str) -> CmdResult<Arc<ChatWeb>> {
    let visits = Arc::new(VisitLog::default());
    let web = AppWeb::new(state, visits.clone())
        .ok_or("offline mode is on: turn it off to use the web")?;
    let approval_session = state
        .approvals
        .lock()
        .map_err(|_| "approval store unavailable")?
        .start_session(now_ms())
        .map_err(|e| e.to_string())?;
    let chat = Arc::new(ChatWeb {
        mode: std::sync::Mutex::new(WebMode::Ask),
        approval_session,
        web,
        visits: visits.clone(),
    });
    if let Ok(mut v) = state.web_visits.lock() {
        v.insert(approval_session, visits);
    }
    state
        .chat_webs
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.to_string(), chat.clone());
    Ok(chat)
}

/// Switch off: end the approval session, its grants and its visit log.
pub fn end(state: &AppState, id: &str) {
    let Some(chat) = state.chat_webs.lock().ok().and_then(|mut m| m.remove(id)) else {
        return;
    };
    if let Ok(mut store) = state.approvals.lock() {
        let _ = store.end_session(chat.approval_session, now_ms());
    }
    if let Ok(mut g) = state.egress_grants.lock() {
        g.remove(&chat.approval_session);
    }
    if let Ok(mut v) = state.web_visits.lock() {
        v.remove(&chat.approval_session);
    }
}

/// The only tools a chat's web turn offers (an allow list: nothing added
/// to Gardenwall later reaches a chat by accident).
const WEB_TOOLS: &[&str] = &["web_search", "web_fetch"];

/// What a web turn produced, for storing as the chat's reply.
pub struct Reply {
    pub content: String,
    pub reasoning: String,
    pub stats: MessageStats,
    pub cancelled: bool,
}

/// Answer the chat's last message with web tools. `messages` is the
/// request as built for an ordinary reply (system first, the new user
/// message last); `cancel` is the chat's stop flag.
#[allow(clippy::too_many_arguments)]
pub async fn answer(
    state: &AppState,
    chat: &ChatWeb,
    conversation_id: &str,
    client: InferenceClient,
    mut messages: Vec<ChatMessage>,
    sampling: gardenwall_inference::client::Sampling,
    template_kwargs: Option<serde_json::Map<String, serde_json::Value>>,
    cancel: Arc<AtomicBool>,
    on_event: &Channel<ChatEvent>,
) -> CmdResult<Reply> {
    let last = messages
        .pop()
        .filter(|m| m.role == Role::User)
        .ok_or("there is no message to respond to")?;
    let system = match messages.first() {
        Some(m) if m.role == Role::System => Some(messages.remove(0).content),
        _ => None,
    };
    let policy = SessionPolicy {
        egress_available: true,
        ..SessionPolicy::default()
    };
    let web: Arc<dyn gardenwall_agent::agent::Web> = chat.web.clone();
    let mut agent = Agent::new(
        Workspaces::new(state.paths.checkpoints.clone()),
        policy,
        None,
        Some(web),
        None,
        None,
        &[],
    );
    agent.set_allowed_tools(WEB_TOOLS.iter().map(|t| t.to_string()).collect());
    agent.set_research(true);
    // The chat's own prompt and memories, as the user's standing
    // instructions.
    agent.set_append_prompt(system);
    let settings = state.settings();
    agent.web_limits = WebLimits {
        searches: settings.web.max_searches as usize,
        pages: settings.web.max_pages as usize,
    };
    agent.sampling = sampling;
    agent.template_kwargs = template_kwargs;
    let next_seq = messages.len() as u64 + 1;
    agent.restore(Restore {
        summary: None,
        messages: messages
            .into_iter()
            .enumerate()
            .map(|(i, m)| (i as u64 + 1, m))
            .collect(),
        next_seq,
    });
    let stop = agent.stop_handle();
    // The chat's own Stop flag: it also cancels a request waiting in the
    // approval dialog (the agent's flag is set from the next event).
    let approver = StoreApprover {
        state,
        session: chat.approval_session,
        stop: cancel.clone(),
        interrupt: Arc::default(),
        claimed: std::sync::Mutex::new(None),
    };
    let model = ClientModel(client);
    let mut sink = |e: AgentEvent| {
        if cancel.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
        }
        let ui = match e {
            AgentEvent::Content(text) => Some(ChatEvent::Content { text }),
            AgentEvent::Reasoning(text) => Some(ChatEvent::Reasoning { text }),
            AgentEvent::ToolStart {
                name, arguments, ..
            } => Some(ChatEvent::Step {
                text: step_label(&name, &arguments),
            }),
            AgentEvent::ApprovalWaiting { .. } => Some(ChatEvent::Step {
                text: "Waiting for your approval in the Gardenwall dialog…".into(),
            }),
            _ => None,
        };
        if let Some(ui) = ui {
            let _ = on_event.send(ui);
        }
    };
    let images = last.images.clone();
    let result = agent
        .run_turn_with_images(&last.content, images, &model, &approver, None, &mut sink)
        .await;
    // Listed and saved however the turn ended.
    if let Err(e) = crate::web::save_visits(
        state,
        VisitOwner::Chat(conversation_id),
        &chat.visits.take_new(),
    ) {
        state.add_notice(format!("The visited-sites list couldn't be saved: {e}"));
    }
    let end = result?;
    let content = agent.last_answer().unwrap_or_default();
    let reasoning = String::new();
    let stats = MessageStats {
        finish_reason: Some(
            match end {
                TurnEnd::Answered => "stop",
                TurnEnd::Stopped => "cancelled",
                TurnEnd::StepLimit => "step_limit",
                TurnEnd::Repeating => "repetition",
            }
            .into(),
        ),
        ..MessageStats::default()
    };
    Ok(Reply {
        content,
        reasoning,
        stats,
        cancelled: end == TurnEnd::Stopped,
    })
}

/// "Searching: …" or "Reading: …" for the chat's progress line. The query
/// or address is the model's; it is shown as plain text only.
fn step_label(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let pick = |k: &str| -> String {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect()
    };
    match name {
        "web_search" => format!("Searching: {}", pick("query")),
        "web_fetch" => format!("Reading: {}", pick("url")),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_plain_and_bounded() {
        assert_eq!(
            step_label("web_search", r#"{"query":"tokio\nspawn"}"#),
            "Searching: tokiospawn"
        );
        let long = format!(r#"{{"url":"https://a.io/{}"}}"#, "x".repeat(500));
        assert!(step_label("web_fetch", &long).chars().count() < 220);
    }
}
