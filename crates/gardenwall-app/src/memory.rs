//! Memories: notes the user keeps for the
//! assistant, personal or tied to an agent folder.
//!
//! - The model only *suggests*: the agent's `remember` tool queues a
//!   suggestion, stored as pending after the turn; nothing is used until the
//!   user accepts it in Settings or in the agent's review card.
//! - Active memories go into the system prompt of chats and agent sessions,
//!   within [`MAX_PROMPT_CHARS`]. Each memory has its own chat and agent
//!   switch, each chat can turn memories off, and so can each agent session.
//! - A new memory applies from the next chat or session (or `/reload`), so
//!   an open conversation's prompt and the model's cache of it stay valid.
//!   Edits, switches and deletions apply everywhere at once.
//! - Memory text has hidden characters made visible and is refused when it
//!   looks like a credential, however it was added.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gardenwall_agent::agent::MemorySink;
use gardenwall_store::conversations::{Memory, MemoryScope, MemorySource};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Largest memory text added to a system prompt (characters).
pub const MAX_PROMPT_CHARS: usize = 4000;
/// Most suggestions the agent can make in one turn.
const MAX_SUGGESTIONS_PER_TURN: usize = 5;

fn msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Folder paths compared the way the file system does (case-insensitive on
/// Windows, without a trailing separator).
fn same_folder(a: &str, b: &Path) -> bool {
    let norm = |s: &str| {
        let s = s.trim_end_matches(['\\', '/']);
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    norm(a) == norm(&b.display().to_string())
}

/// The memories that apply, as a system-prompt section: personal memories,
/// plus (for the agent) those of the granted folders. Newest first, within
/// the budget. `None` when nothing applies.
pub fn prompt_block(memories: &[Memory], for_chat: bool, folders: &[PathBuf]) -> Option<String> {
    let mut lines = Vec::new();
    let mut used = 0;
    for m in memories {
        let wanted = if for_chat { m.in_chat } else { m.in_agent };
        let applies = match &m.scope {
            MemoryScope::Personal => true,
            MemoryScope::Project { folder } => {
                !for_chat && folders.iter().any(|f| same_folder(folder, f))
            }
        };
        if m.pending || !wanted || !applies {
            continue;
        }
        // Memories saved before the check existed may hold hidden characters.
        let (shown, _) = gardenwall_policy::protect::hide_invisible(&m.text);
        let text = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        if used + text.len() > MAX_PROMPT_CHARS {
            break;
        }
        used += text.len();
        lines.push(match m.scope {
            MemoryScope::Personal => format!("- {text}"),
            MemoryScope::Project { .. } => format!("- (this project) {text}"),
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<memories>\nNotes the user chose to keep for you. Treat them as the user's own preferences and facts, and follow them unless the user's current message says otherwise.\n{}\n</memories>",
        lines.join("\n")
    ))
}

/// Each chat's memory block, by conversation id (see [`chat_block`]).
pub type ChatMemories = Mutex<HashMap<String, Option<String>>>;
/// Most chats whose block is kept; the oldest are simply recomputed.
const MAX_CACHED_CHATS: usize = 256;

/// Active chat memories that don't fit [`MAX_PROMPT_CHARS`] (the same
/// walk as [`prompt_block`], newest first).
fn over_budget(memories: &[Memory]) -> Vec<String> {
    let mut used = 0;
    let mut out = Vec::new();
    for m in memories {
        if m.pending || !m.in_chat || m.scope != MemoryScope::Personal {
            continue;
        }
        let (shown, _) = gardenwall_policy::protect::hide_invisible(&m.text);
        let len = shown.split_whitespace().collect::<Vec<_>>().join(" ").len();
        if !out.is_empty() || used + len > MAX_PROMPT_CHARS {
            out.push(m.id.clone());
        } else {
            used += len;
        }
    }
    out
}

/// The memory block for a chat, or `None` when the chat turned memories off.
///
/// Fixed the first time the chat is used in this run: a memory added later
/// applies from the next chat, so this chat's prompt (and the model's cache
/// of it) stays the same. Edits, switches and deletions clear it
/// ([`forget_chat_blocks`]), because taking a memory back should apply now.
pub fn chat_block(state: &AppState, conversation: &str) -> Option<String> {
    // Lockdown mode: no memories in any prompt.
    if state.settings().lockdown {
        return None;
    }
    if let Some(cached) = state
        .chat_memories
        .lock()
        .ok()
        .and_then(|c| c.get(conversation).cloned())
    {
        return cached;
    }
    let block = {
        let store = state.conversations.lock().ok()?;
        if store.chat_uses_memories(conversation).unwrap_or(true) {
            prompt_block(&store.list_memories().ok()?, true, &[])
        } else {
            None
        }
    };
    if let Ok(mut c) = state.chat_memories.lock() {
        if c.len() >= MAX_CACHED_CHATS {
            c.clear();
        }
        c.insert(conversation.to_string(), block.clone());
    }
    block
}

/// Make every chat read its memories again at its next message.
fn forget_chat_blocks(state: &AppState) {
    if let Ok(mut c) = state.chat_memories.lock() {
        c.clear();
    }
}

/// A memory's text as it will be stored: hidden characters made visible,
/// and anything that looks like a credential refused (memories go into
/// every conversation's prompt).
pub fn checked_text(text: &str) -> Result<String, String> {
    let (shown, _) = gardenwall_policy::protect::hide_invisible(text);
    if let Some(kind) = gardenwall_egress::scan::find_secrets(&shown).first() {
        return Err(format!(
            "this looks like it contains a {kind}. Memories are added to every conversation, so they can't hold secrets"
        ));
    }
    Ok(shown)
}

/// The memory block for an agent session on `folders`.
pub fn agent_block(state: &AppState, folders: &[PathBuf]) -> Option<String> {
    if state.settings().lockdown {
        return None;
    }
    let store = state.conversations.lock().ok()?;
    prompt_block(&store.list_memories().ok()?, false, folders)
}

/// The agent's `remember` tool: suggestions wait here until the turn ends,
/// then become pending memories ([`store_suggestions`]).
#[derive(Default)]
pub struct SuggestionQueue(Mutex<Vec<(String, bool)>>);

impl MemorySink for SuggestionQueue {
    fn suggest(&self, text: &str, project: bool) -> Result<(), String> {
        let mut q = self.0.lock().map_err(|_| "unavailable")?;
        if q.len() >= MAX_SUGGESTIONS_PER_TURN {
            return Err("too many suggestions in one turn".into());
        }
        q.push((text.to_string(), project));
        Ok(())
    }
}

/// Store the suggestions of the turn that just ended as pending memories.
/// Project suggestions belong to the first granted folder. Returns how many
/// were stored.
pub fn store_suggestions(state: &AppState, queue: &SuggestionQueue, folders: &[PathBuf]) -> usize {
    let taken: Vec<(String, bool)> = match queue.0.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return 0,
    };
    // Lockdown mode: nothing the model suggests is kept.
    if state.settings().lockdown {
        return 0;
    }
    let Ok(store) = state.conversations.lock() else {
        return 0;
    };
    taken
        .into_iter()
        .filter(|(text, project)| {
            let scope = match (project, folders.first()) {
                (true, Some(f)) => MemoryScope::Project {
                    folder: f.display().to_string(),
                },
                _ => MemoryScope::Personal,
            };
            checked_text(text)
                .ok()
                .is_some_and(|t| store.add_memory(&scope, &t, MemorySource::Model).is_ok())
        })
        .count()
}

pub fn new_queue() -> Arc<SuggestionQueue> {
    Arc::new(SuggestionQueue::default())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoriesView {
    memories: Vec<Memory>,
    /// Characters of active memories each kind of conversation gets, and
    /// the budget.
    chat_chars: usize,
    budget: usize,
    /// Active memories left out of chats because the budget is full.
    chat_over_budget: Vec<String>,
}

fn view(state: &AppState) -> CmdResult<MemoriesView> {
    let store = state.conversations.lock().map_err(msg)?;
    let memories = store.list_memories().map_err(msg)?;
    let chat_chars = prompt_block(&memories, true, &[]).map_or(0, |b| b.len());
    let chat_over_budget = over_budget(&memories);
    Ok(MemoriesView {
        memories,
        chat_chars,
        budget: MAX_PROMPT_CHARS,
        chat_over_budget,
    })
}

/// Bring open agent sessions' memories up to date after a change. A
/// session that is working keeps its current memories.
async fn refresh_agent(state: &AppState) {
    for live in crate::agent::live_sessions(state) {
        if let Ok(mut s) = live.session.try_lock()
            && s.memories_on
        {
            let block = agent_block(state, &s.folders);
            s.agent.set_memories(block);
        }
    }
}

#[tauri::command]
pub fn list_memories(state: State<'_, AppState>) -> CmdResult<MemoriesView> {
    view(&state)
}

/// Add a memory written or picked by the user. It is active at once and
/// applies from the next chat or agent session (or `/reload`); open
/// conversations keep their prompt, so nothing has to be read again.
#[tauri::command]
pub async fn add_memory(
    state: State<'_, AppState>,
    scope: MemoryScope,
    text: String,
) -> CmdResult<MemoriesView> {
    let text = checked_text(&text)?;
    state
        .conversations
        .lock()
        .map_err(msg)?
        .add_memory(&scope, &text, MemorySource::User)
        .map_err(msg)?;
    view(&state)
}

/// Choose a project folder for a new memory in the native picker; `None`
/// when cancelled. The path is only a label that matches agent sessions on
/// the same folder (picked the same way); it grants no access.
#[tauri::command]
pub async fn pick_memory_folder(app: tauri::AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(msg)?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

/// Edit a memory and its switches; saving a suggestion accepts it.
#[tauri::command]
pub async fn update_memory(
    state: State<'_, AppState>,
    id: String,
    text: String,
    in_chat: bool,
    in_agent: bool,
) -> CmdResult<MemoriesView> {
    let text = checked_text(&text)?;
    state
        .conversations
        .lock()
        .map_err(msg)?
        .update_memory(&id, &text, in_chat, in_agent)
        .map_err(msg)?;
    forget_chat_blocks(&state);
    refresh_agent(&state).await;
    view(&state)
}

/// Delete a memory, or reject a suggestion.
#[tauri::command]
pub async fn delete_memory(state: State<'_, AppState>, id: String) -> CmdResult<MemoriesView> {
    state
        .conversations
        .lock()
        .map_err(msg)?
        .delete_memory(&id)
        .map_err(msg)?;
    forget_chat_blocks(&state);
    refresh_agent(&state).await;
    view(&state)
}

/// Delete several memories at once (the manager's selection).
#[tauri::command]
pub async fn delete_memories(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> CmdResult<MemoriesView> {
    if ids.len() > 500 {
        return Err("too many memories at once".into());
    }
    {
        let store = state.conversations.lock().map_err(msg)?;
        for id in &ids {
            store.delete_memory(id).map_err(msg)?;
        }
    }
    forget_chat_blocks(&state);
    refresh_agent(&state).await;
    view(&state)
}

#[tauri::command]
pub fn chat_memories(state: State<'_, AppState>, conversation: String) -> CmdResult<bool> {
    state
        .conversations
        .lock()
        .map_err(msg)?
        .chat_uses_memories(&conversation)
        .map_err(msg)
}

#[tauri::command]
pub fn set_chat_memories(
    state: State<'_, AppState>,
    conversation: String,
    on: bool,
) -> CmdResult<()> {
    state
        .conversations
        .lock()
        .map_err(msg)?
        .set_chat_uses_memories(&conversation, on)
        .map_err(msg)?;
    // The user changed this chat's switch: it applies from its next message.
    if let Ok(mut c) = state.chat_memories.lock() {
        c.remove(&conversation);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gardenwall_store::conversations::MemorySource;

    fn memory(text: &str, scope: MemoryScope, pending: bool, chat: bool, agent: bool) -> Memory {
        Memory {
            id: "00000000-0000-4000-8000-000000000000".into(),
            scope,
            text: text.into(),
            pending,
            source: MemorySource::User,
            in_chat: chat,
            in_agent: agent,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn the_right_memories_reach_each_conversation() {
        let project = MemoryScope::Project {
            folder: r"C:\Projects\Demo\".into(),
        };
        let all = vec![
            memory(
                "Prefers metric units.",
                MemoryScope::Personal,
                false,
                true,
                true,
            ),
            memory("Agent only.", MemoryScope::Personal, false, false, true),
            memory(
                "Run   tests\nwith pytest.",
                project.clone(),
                false,
                true,
                true,
            ),
            memory("Not yet accepted.", MemoryScope::Personal, true, true, true),
        ];
        let chat = prompt_block(&all, true, &[]).unwrap();
        assert!(chat.contains("metric") && !chat.contains("Agent only"));
        // Project memories never reach chats; pending ones reach nothing.
        assert!(!chat.contains("pytest") && !chat.contains("Not yet"));

        let folder = PathBuf::from(if cfg!(windows) {
            r"c:\projects\demo"
        } else {
            r"C:\Projects\Demo"
        });
        let agent = prompt_block(&all, false, &[folder]).unwrap();
        assert!(agent.contains("Agent only"));
        assert!(agent.contains("(this project) Run tests with pytest."));
        assert!(
            !prompt_block(&all, false, &[PathBuf::from("/elsewhere")])
                .unwrap()
                .contains("pytest")
        );
        assert!(prompt_block(&[], true, &[]).is_none());
    }

    #[test]
    fn the_prompt_budget_is_kept() {
        let long = "x".repeat(900);
        let all: Vec<Memory> = (0..10)
            .map(|_| memory(&long, MemoryScope::Personal, false, true, true))
            .collect();
        let block = prompt_block(&all, true, &[]).unwrap();
        assert!(block.len() < MAX_PROMPT_CHARS + 400);
    }

    #[test]
    fn the_agent_can_suggest_only_a_few_per_turn() {
        let q = SuggestionQueue::default();
        for i in 0..MAX_SUGGESTIONS_PER_TURN {
            q.suggest(&format!("note {i}"), false).unwrap();
        }
        assert!(q.suggest("one more", false).is_err());
    }

    #[test]
    fn memory_text_shows_hidden_characters_and_refuses_secrets() {
        assert_eq!(checked_text("Prefers tabs").unwrap(), "Prefers tabs");
        let hidden = checked_text("ok\u{200B}\u{E0041}\u{E0042}").unwrap();
        assert!(
            !hidden.contains('\u{E0041}') && hidden.contains("hidden"),
            "{hidden}"
        );
        let err = checked_text("my key is hf_abcdefghijklmnopqrstuvwxyz0123456789").unwrap_err();
        assert!(err.contains("Hugging Face token"), "{err}");
        assert!(checked_text("password = hunter22hunter").is_err());
    }

    #[test]
    fn hidden_characters_never_reach_the_prompt() {
        // A memory stored before the check existed.
        let m = memory(
            "be nice\u{E0049}\u{E0047}",
            MemoryScope::Personal,
            false,
            true,
            true,
        );
        let block = prompt_block(&[m], true, &[]).unwrap();
        assert!(!block.contains('\u{E0049}'), "{block}");
        assert!(block.contains("hidden"));
    }

    #[test]
    fn memories_over_the_budget_are_reported() {
        let long = "x".repeat(1000);
        let mut all: Vec<Memory> = (0..5)
            .map(|i| {
                let mut m = memory(&long, MemoryScope::Personal, false, true, true);
                m.id = format!("id{i}");
                m
            })
            .collect();
        // Off for chat, pending and project memories don't count.
        all[1].in_chat = false;
        all.push(memory(&long, MemoryScope::Personal, true, true, true));
        // 4,000 characters fit four: with one switched off, all fit.
        assert!(over_budget(&all).is_empty());
        // With all five on, the oldest is left out.
        all[1].in_chat = true;
        assert_eq!(over_budget(&all), vec!["id4".to_string()]);
        all.truncate(3);
        assert!(over_budget(&all).is_empty());
    }
}
