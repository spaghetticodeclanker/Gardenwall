//! Session handoff (ROADMAP
//! "Planned features" §1): continue a long agent session or chat in a
//! fresh one that starts from a reviewed handoff note.
//!
//! - Agent → agent: same project, folders, documents and memory choice.
//! - Chat → chat: same system prompt and memory choice.
//! - Chat → agent: a project the user picks (or new folders from the
//!   native picker).
//!
//! The model drafts the note ([`handoff_draft`]); the core adds the facts
//! (project, folders, changed files from checkpoints). The user edits the
//! note, then starts the new session ([`handoff_start`]), whose first
//! message the webview sends through the normal path, or saves it
//! ([`handoff_save_as`] through the native dialog, or
//! [`handoff_save_to_project`] as `HANDOFF.md`, which always asks in the
//! trusted approval dialog). Values from secret files the session read are
//! removed from drafts and refused in what the user starts or saves.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gardenwall_agent::agent::Model;
use gardenwall_agent::compaction::serialize_conversation;
use gardenwall_agent::handoff::{self, Facts, HandoffKind, Suggestion};
use gardenwall_inference::client::{ChatMessage, ChatRequest, Role, Sampling};
use gardenwall_store::conversations::{
    ConversationSummary, HandoffParent, MessageRole, title_from_message,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::agent::{
    AgentSession, AgentView, BusyGuard, ClientModel, StoreApprover, begin, live, with_conversations,
};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// The session a new one continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A saved agent transcript.
    Agent(String),
    /// A chat.
    Chat(String),
}

impl Origin {
    pub fn parent(&self) -> HandoffParent<'_> {
        match self {
            Self::Agent(id) => HandoffParent::Agent(id),
            Self::Chat(id) => HandoffParent::Chat(id),
        }
    }
}

/// Where a handoff comes from, as the webview names it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// An open agent session.
    Agent { sid: String },
    /// A chat; `request` lets Stop cancel the draft (`cancel_chat`).
    Chat {
        id: String,
        #[serde(default)]
        request: Option<u64>,
    },
}

/// Where the new session goes.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    /// An agent session: an agent source's own project; for a chat, the
    /// named project, or folders chosen in the native picker (`None`).
    Agent { project: Option<String> },
    /// A new chat (from a chat only).
    Chat,
}

/// Why to suggest a handoff after an agent turn (thresholds in
/// [`handoff::suggestion`]).
pub fn agent_suggestion(s: &AgentSession) -> Option<Suggestion> {
    handoff::suggestion(
        s.agent.context_tokens(),
        s.agent.context_window,
        s.compactions,
        s.agent.todos(),
    )
}

/// The facts the core adds to an agent session's handoff.
fn agent_facts(state: &AppState, s: &AgentSession) -> Facts {
    let project = s
        .project
        .as_deref()
        .and_then(|id| crate::projects::project_folders(state, id).ok())
        .map(|(p, _)| p.name);
    let shown = gardenwall_agent::display::shown;
    Facts {
        project,
        folders: s.folders.iter().map(|f| shown(f)).collect(),
        changed_files: Some(s.agent.changed_files()),
    }
}

/// A drafted handoff for the user to review.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffDraft {
    /// The model's note, hidden characters collapsed and secret values
    /// removed.
    text: String,
    /// Values from secret files removed from the draft.
    redacted: usize,
    /// The facts Gardenwall adds (agent sessions only).
    facts: Option<Facts>,
    /// The session's title.
    title: String,
}

fn clean_focus(focus: Option<String>) -> Option<String> {
    focus
        .map(|f| {
            f.chars()
                .filter(|c| !c.is_control())
                .take(500)
                .collect::<String>()
        })
        .filter(|f| !f.trim().is_empty())
}

/// Ask the loaded model for a handoff note. Nothing is changed or saved.
#[tauri::command]
pub async fn handoff_draft(
    state: State<'_, AppState>,
    source: Source,
    focus: Option<String>,
) -> CmdResult<HandoffDraft> {
    let focus = clean_focus(focus);
    let (client, context_window, model_sha) = {
        let engine = state.engine.lock().await;
        let loaded = engine.as_ref().ok_or("no model is loaded")?;
        (
            loaded.server.client().clone(),
            loaded.props.context_window,
            loaded.sha256.clone(),
        )
    };
    match source {
        Source::Agent { sid } => {
            let live = live(&state, &sid)?;
            if live.busy.swap(true, Ordering::SeqCst) {
                return Err("The agent is still working in this session. Write the handoff when it stops, or stop it first.".into());
            }
            let _busy = BusyGuard(&live.busy);
            let mut guard = live.session.lock().await;
            let session = &mut *guard;
            if session.transcript.is_none() {
                return Err("there is nothing to hand off yet: send a request first".into());
            }
            live.stop.store(false, Ordering::SeqCst);
            session.agent.context_window = (context_window > 0).then_some(context_window);
            session.agent.sampling = state.settings().generation.sampling();
            session.agent.template_kwargs = state
                .library
                .get(&model_sha)
                .and_then(|r| r.thinking.template_kwargs());
            let model = ClientModel(client);
            let (text, redacted) = session
                .agent
                .draft_handoff(&model, focus.as_deref())
                .await
                .map_err(|e| format!("the handoff could not be written: {e}"))?;
            Ok(HandoffDraft {
                text,
                redacted,
                facts: Some(agent_facts(&state, session)),
                title: live.title(),
            })
        }
        Source::Chat {
            id,
            request: request_no,
        } => {
            let conversation = with_conversations(&state, |s| s.get(&id))?;
            if conversation.messages.is_empty() {
                return Err("there is nothing to hand off yet".into());
            }
            let messages: Vec<ChatMessage> = conversation
                .messages
                .iter()
                .map(|m| {
                    let role = match m.role {
                        MessageRole::User => Role::User,
                        MessageRole::Assistant => Role::Assistant,
                    };
                    ChatMessage::new(role, m.content.clone())
                })
                .collect();
            let budget = (context_window.max(4096) as usize / 2).saturating_mul(4);
            let text = handoff::fit_newest(&serialize_conversation(&messages), budget);
            let request = ChatRequest {
                messages: handoff::request(HandoffKind::Chat, &text, None, &[], focus.as_deref()),
                sampling: Sampling {
                    max_tokens: None,
                    ..state.settings().generation.sampling()
                },
                template_kwargs: state
                    .library
                    .get(&model_sha)
                    .and_then(|r| r.thinking.template_kwargs()),
                ..ChatRequest::default()
            };
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(r) = request_no {
                state
                    .chats
                    .lock()
                    .map_err(|_| "state unavailable")?
                    .insert(r, cancel.clone());
            }
            let result = ClientModel(client)
                .chat(&request, &mut |_| {
                    if cancel.load(Ordering::SeqCst) {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                })
                .await;
            if let (Some(r), Ok(mut chats)) = (request_no, state.chats.lock()) {
                chats.remove(&r);
            }
            let r = result.map_err(|e| format!("the handoff could not be written: {e}"))?;
            if r.finish_reason.as_deref() == Some("length") {
                return Err("the handoff could not be written: it hit the token cap".into());
            }
            let note = r.content.trim();
            if note.is_empty() {
                return Err("the handoff could not be written: the model returned nothing".into());
            }
            let (text, _) = gardenwall_policy::protect::hide_invisible(note);
            Ok(HandoffDraft {
                text,
                redacted: 0,
                facts: None,
                title: conversation.summary.title,
            })
        }
    }
}

/// A started session: open it and send `message` as its first request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffStarted {
    /// The new agent session.
    agent: Option<AgentView>,
    /// The new chat.
    chat: Option<ConversationSummary>,
    /// The first message: the reviewed note framed, with the core's facts.
    message: String,
}

fn continued_title(title: &str) -> String {
    let base = title.trim().trim_end_matches(" (continued)");
    let base: String = base.chars().take(160).collect();
    format!("{base} (continued)")
}

const SECRET_REFUSAL: &str =
    "the handoff contains a value from a protected file this session read; remove it first";

/// Start a new session from a reviewed handoff. The old session stays as
/// it is, linked as the new one's parent once its first request is saved.
#[tauri::command]
pub async fn handoff_start(
    app: AppHandle,
    state: State<'_, AppState>,
    source: Source,
    target: Target,
    text: String,
) -> CmdResult<HandoffStarted> {
    let (note, _) = handoff::clean(&text)?;
    match (source, target) {
        (Source::Agent { sid }, Target::Agent { .. }) => {
            let live = live(&state, &sid)?;
            let guard = live
                .session
                .try_lock()
                .map_err(|_| "The agent is still working in this session. Write the handoff when it stops, or stop it first.")?;
            let parent = guard
                .transcript
                .clone()
                .ok_or("there is nothing to hand off yet: send a request first")?;
            if guard.agent.contains_revealed(&note) {
                return Err(SECRET_REFUSAL.into());
            }
            let facts = agent_facts(&state, &guard);
            // A project's current folders (checked again), as for any new
            // session in it.
            let folders = match &guard.project {
                Some(p) => crate::projects::project_folders(&state, p)?.1,
                None => guard.folders.clone(),
            };
            let (docs, memories_on, project) =
                (guard.docs.clone(), guard.memories_on, guard.project.clone());
            let title = continued_title(&live.title());
            drop(guard);
            let view = begin(&state, folders, None, docs, memories_on, project).await?;
            set_origin(&state, &view.id, Origin::Agent(parent), title).await?;
            Ok(HandoffStarted {
                agent: Some(view),
                chat: None,
                message: handoff::first_message(&note, Some(&facts)),
            })
        }
        (Source::Chat { id, .. }, Target::Chat) => {
            let parent = with_conversations(&state, |s| s.get(&id))?;
            let mut chat = with_conversations(&state, |s| s.create_continuation(&id))?;
            let title = continued_title(&parent.summary.title);
            with_conversations(&state, |s| s.rename(&chat.id, &title))?;
            chat.title = title_from_message(&title);
            Ok(HandoffStarted {
                agent: None,
                chat: Some(chat),
                message: handoff::first_message(&note, None),
            })
        }
        (Source::Chat { id, .. }, Target::Agent { project }) => {
            let parent = with_conversations(&state, |s| s.get(&id))?;
            let (folders, project) = match project {
                Some(p) => {
                    let (p, folders) = crate::projects::project_folders(&state, &p)?;
                    (folders, p)
                }
                None => {
                    let Some(picked) = crate::agent::pick_folders(app).await? else {
                        return Err("no folder was chosen".into());
                    };
                    let folders = picked
                        .iter()
                        .map(|f| crate::projects::checked(f))
                        .collect::<CmdResult<Vec<_>>>()?;
                    let p = crate::projects::create_for(&state, &folders)?;
                    (folders, p)
                }
            };
            let facts = Facts {
                project: Some(project.name.clone()),
                folders: folders
                    .iter()
                    .map(|f| gardenwall_agent::display::shown(f))
                    .collect(),
                changed_files: None,
            };
            let title = continued_title(&parent.summary.title);
            let view = begin(&state, folders, None, None, true, Some(project.id)).await?;
            set_origin(&state, &view.id, Origin::Chat(parent.summary.id), title).await?;
            Ok(HandoffStarted {
                agent: Some(view),
                chat: None,
                message: handoff::first_message(&note, Some(&facts)),
            })
        }
        (Source::Agent { .. }, Target::Chat) => {
            Err("an agent session hands off to a new agent session".into())
        }
    }
}

async fn set_origin(state: &AppState, sid: &str, origin: Origin, title: String) -> CmdResult<()> {
    let live = live(state, sid)?;
    let mut s = live.session.lock().await;
    s.handoff_from = Some((origin, title));
    Ok(())
}

/// The handoff as a Markdown document, refusing secret values.
async fn document(state: &AppState, source: &Source, note: &str) -> CmdResult<String> {
    match source {
        Source::Agent { sid } => {
            let live = live(state, sid)?;
            let guard = live
                .session
                .try_lock()
                .map_err(|_| "The agent is still working in this session. Write the handoff when it stops, or stop it first.")?;
            if guard.agent.contains_revealed(note) {
                return Err(SECRET_REFUSAL.into());
            }
            Ok(handoff::document(note, Some(&agent_facts(state, &guard))))
        }
        Source::Chat { .. } => Ok(handoff::document(note, None)),
    }
}

/// Save the handoff where the user chooses in the native dialog. Returns
/// the path, or `None` when cancelled.
#[tauri::command]
pub async fn handoff_save_as(
    app: AppHandle,
    state: State<'_, AppState>,
    source: Source,
    text: String,
) -> CmdResult<Option<String>> {
    let (note, _) = handoff::clean(&text)?;
    let doc = document(&state, &source, &note).await?;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Markdown", &["md"])
            .set_file_name("HANDOFF.md")
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    std::fs::write(&path, doc).map_err(|e| format!("could not save: {e}"))?;
    Ok(Some(path.display().to_string()))
}

/// Save the handoff as `HANDOFF.md` in the session's working folder. It is
/// a persistence-risk path (it steers later sessions), so the trusted
/// approval dialog always asks first; the
/// write is checkpointed like the agent's own.
#[tauri::command]
pub async fn handoff_save_to_project(
    state: State<'_, AppState>,
    sid: String,
    text: String,
) -> CmdResult<String> {
    let (note, _) = handoff::clean(&text)?;
    let live = live(&state, &sid)?;
    if live.busy.swap(true, Ordering::SeqCst) {
        return Err("The agent is still working in this session. Write the handoff when it stops, or stop it first.".into());
    }
    let _busy = BusyGuard(&live.busy);
    let mut guard = live.session.lock().await;
    let session = &mut *guard;
    if session.agent.contains_revealed(&note) {
        return Err(SECRET_REFUSAL.into());
    }
    let doc = handoff::document(&note, Some(&agent_facts(&state, session)));
    live.stop.store(false, Ordering::SeqCst);
    let approver = StoreApprover {
        state: &state,
        session: session.approval_session,
        stop: session.stop.clone(),
        interrupt: Arc::default(),
        claimed: std::sync::Mutex::new(None),
    };
    session
        .agent
        .write_for_user("HANDOFF.md", &doc, "Save the session handoff", &approver)
        .await?;
    let folder = session
        .folders
        .first()
        .map(|f| gardenwall_agent::display::shown(f))
        .unwrap_or_default();
    Ok(format!("Saved HANDOFF.md in {folder}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_say_continued_once() {
        assert_eq!(continued_title("Fix the bug"), "Fix the bug (continued)");
        assert_eq!(
            continued_title("Fix the bug (continued)"),
            "Fix the bug (continued)"
        );
    }

    #[test]
    fn sources_and_targets_parse_from_the_webview() {
        let s: Source = serde_json::from_str(r#"{"kind":"agent","sid":"x"}"#).unwrap();
        assert!(matches!(s, Source::Agent { sid } if sid == "x"));
        let s: Source = serde_json::from_str(r#"{"kind":"chat","id":"y","request":4}"#).unwrap();
        assert!(matches!(s, Source::Chat { id, request: Some(4) } if id == "y"));
        let t: Target = serde_json::from_str(r#"{"kind":"agent","project":null}"#).unwrap();
        assert!(matches!(t, Target::Agent { project: None }));
        let t: Target = serde_json::from_str(r#"{"kind":"chat"}"#).unwrap();
        assert!(matches!(t, Target::Chat));
        // Unknown kinds are refused, never guessed.
        assert!(serde_json::from_str::<Target>(r#"{"kind":"web"}"#).is_err());
    }
}
