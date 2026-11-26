//! Context compaction, ported from Pi
//! (`packages/coding-agent/src/core/compaction/{compaction,utils}.ts` and the
//! summary wrapper in `core/messages.ts`, pi-mono commit 682c7b9).
//! Copyright (c) 2025 Mario Zechner, MIT License; see THIRD_PARTY_NOTICES.md.
//!
//! When the context nears the window (`tokens > window − reserve`), older
//! messages are replaced by a structured summary written by the model
//! itself. About `keep_recent_tokens` of the newest messages are kept as
//! they are. Cuts fall on user or assistant messages, never between a tool
//! call and its result. A cut inside a turn summarizes the turn's start
//! separately (Pi's "split turn").
//!
//! Differences from Pi:
//! - the summary body and the file lists are kept apart, so an update
//!   summary is not fed the file lists of the previous one;
//! - the summary is untrusted model output like any other and grants nothing.

use std::collections::BTreeSet;

use gardenwall_inference::client::{ChatMessage, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Pi's defaults.
pub const DEFAULT_RESERVE_TOKENS: u64 = 16384;
pub const DEFAULT_KEEP_RECENT_TOKENS: u64 = 20000;

/// Maximum characters of one tool result in the text given to the summarizer.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: DEFAULT_RESERVE_TOKENS,
            keep_recent_tokens: DEFAULT_KEEP_RECENT_TOKENS,
        }
    }
}

impl CompactionSettings {
    /// Pi's defaults assume windows of 128K tokens or more. On a small
    /// window they would compact on every step (or never fit at all), so
    /// both are capped to a share of the window.
    pub fn for_window(self, window: u64) -> Self {
        Self {
            reserve_tokens: self.reserve_tokens.min(window / 4),
            keep_recent_tokens: self.keep_recent_tokens.min(window * 3 / 10),
            ..self
        }
    }
}

pub fn should_compact(context_tokens: u64, context_window: u64, s: &CompactionSettings) -> bool {
    s.enabled && context_tokens > context_window.saturating_sub(s.reserve_tokens)
}

/// Characters of a string as JavaScript counts them (UTF-16 code units),
/// so estimates match Pi's.
fn js_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

fn ceil4(chars: u64) -> u64 {
    chars.div_ceil(4)
}

/// Characters counted for each attached image (Pi: about 1,200 tokens).
const IMAGE_CHARS: u64 = 4800;

/// Pi's conservative estimate: characters / 4, rounded up, plus about
/// 1,200 tokens per image.
pub fn estimate_tokens(m: &ChatMessage) -> u64 {
    let mut chars = js_len(&m.content) + IMAGE_CHARS * m.images.len() as u64;
    if m.role == Role::Assistant {
        chars += m.reasoning_content.as_deref().map_or(0, js_len);
        for c in &m.tool_calls {
            chars += js_len(&c.function.name) + js_len(&c.function.arguments);
        }
    }
    ceil4(chars)
}

/// Estimated tokens of the tool definitions sent with every request.
pub fn estimate_tools_tokens(tools: &[Value]) -> u64 {
    ceil4(tools.iter().map(|t| js_len(&t.to_string())).sum())
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let total = text.encode_utf16().count();
    if total <= max_chars {
        return text.to_string();
    }
    // Keep whole characters: stop before the code unit that would pass the limit.
    let mut units = 0;
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        if units + ch.len_utf16() > max_chars {
            break;
        }
        units += ch.len_utf16();
        end = i + ch.len_utf8();
    }
    format!(
        "{}\n\n[... {} more characters truncated]",
        &text[..end],
        total - units
    )
}

/// `key=JSON` pairs like Pi's `name(key="value", n=3)`.
fn format_args(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", "),
        // Arguments that are not a JSON object are shown as received.
        _ => arguments.to_string(),
    }
}

/// Text form of a conversation for the summarizer. Reasoning is left out:
/// replayed thinking can be larger than the context it summarizes.
pub fn serialize_conversation(messages: &[ChatMessage]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        match m.role {
            Role::System => {}
            Role::User => {
                if !m.content.is_empty() {
                    parts.push(format!("[User]: {}", m.content));
                }
            }
            Role::Assistant => {
                if !m.content.is_empty() {
                    parts.push(format!("[Assistant]: {}", m.content));
                }
                if !m.tool_calls.is_empty() {
                    let calls: Vec<String> = m
                        .tool_calls
                        .iter()
                        .map(|c| {
                            format!(
                                "{}({})",
                                c.function.name,
                                format_args(&c.function.arguments)
                            )
                        })
                        .collect();
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Role::Tool => {
                if !m.content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&m.content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }
    parts.join("\n\n")
}

const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.";

const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX: &str = "\n</summary>";

/// Messages asking for a (new or updated) history summary.
pub fn summary_request(conversation: &str, previous: Option<&str>) -> Vec<ChatMessage> {
    summary_request_focused(conversation, previous, None)
}

/// Like [`summary_request`], with Pi's "Additional focus" instructions
/// (from `/compact <focus>`).
pub fn summary_request_focused(
    conversation: &str,
    previous: Option<&str>,
    focus: Option<&str>,
) -> Vec<ChatMessage> {
    let mut text = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    if let Some(p) = previous {
        text.push_str(&format!("<previous-summary>\n{p}\n</previous-summary>\n\n"));
    }
    text.push_str(if previous.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    });
    if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        text.push_str(&format!("\n\nAdditional focus: {f}"));
    }
    vec![
        ChatMessage::new(Role::System, SUMMARIZATION_SYSTEM_PROMPT),
        ChatMessage::new(Role::User, text),
    ]
}

/// Messages asking for a summary of the start of a split turn.
pub fn turn_prefix_request(conversation: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::new(Role::System, SUMMARIZATION_SYSTEM_PROMPT),
        ChatMessage::new(
            Role::User,
            format!(
                "# Conversation\n{conversation}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
            ),
        ),
    ]
}

/// Join a history summary and a split-turn summary as Pi does.
pub fn merge_split_turn(history: Option<&str>, turn_prefix: &str) -> String {
    format!(
        "{}\n\n---\n\n**Turn Context (split turn):**\n\n{turn_prefix}",
        history.unwrap_or("No prior history.")
    )
}

/// Files the agent read and changed, carried across compactions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOps {
    pub read: BTreeSet<String>,
    pub modified: BTreeSet<String>,
}

impl FileOps {
    /// Record the paths of `read`, `write` and `edit` calls.
    pub fn collect(&mut self, messages: &[ChatMessage]) {
        for c in messages.iter().flat_map(|m| &m.tool_calls) {
            let name = c.function.name.as_str();
            if !matches!(name, "read" | "write" | "edit") {
                continue;
            }
            let Ok(args) = serde_json::from_str::<Value>(&c.function.arguments) else {
                continue;
            };
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                if name == "read" {
                    self.read.insert(path.to_string());
                } else {
                    self.modified.insert(path.to_string());
                }
            }
        }
    }

    /// Pi's `<read-files>` and `<modified-files>` sections; files that were
    /// changed are not listed as read.
    pub fn format(&self) -> String {
        let read: Vec<&str> = self
            .read
            .iter()
            .filter(|f| !self.modified.contains(*f))
            .map(String::as_str)
            .collect();
        let mut sections = Vec::new();
        if !read.is_empty() {
            sections.push(format!("<read-files>\n{}\n</read-files>", read.join("\n")));
        }
        if !self.modified.is_empty() {
            let m: Vec<&str> = self.modified.iter().map(String::as_str).collect();
            sections.push(format!(
                "<modified-files>\n{}\n</modified-files>",
                m.join("\n")
            ));
        }
        if sections.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", sections.join("\n\n"))
        }
    }
}

/// The current summary of compacted history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub text: String,
    pub files: FileOps,
}

impl Summary {
    /// The user message that stands in for the compacted history.
    pub fn message(&self) -> ChatMessage {
        ChatMessage::new(
            Role::User,
            format!(
                "{SUMMARY_PREFIX}{}{}{SUMMARY_SUFFIX}",
                self.text,
                self.files.format()
            ),
        )
    }
}

/// Where to cut `messages[start..end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPoint {
    /// First message kept as it is.
    pub first_kept: usize,
    /// When the cut falls inside a turn: where that turn starts.
    pub turn_start: Option<usize>,
}

fn is_cut_point(m: &ChatMessage) -> bool {
    matches!(m.role, Role::User | Role::Assistant)
}

/// Pi's `findCutPoint`: walk back from the newest message until about
/// `keep_recent` tokens are kept, then cut at the next user or assistant
/// message.
pub fn find_cut_point(
    messages: &[ChatMessage],
    start: usize,
    end: usize,
    keep_recent: u64,
) -> CutPoint {
    let cut_points: Vec<usize> = (start..end)
        .filter(|&i| is_cut_point(&messages[i]))
        .collect();
    let Some(&first) = cut_points.first() else {
        return CutPoint {
            first_kept: start,
            turn_start: None,
        };
    };
    let mut cut = first;
    let mut kept = 0;
    for i in (start..end).rev() {
        let t = estimate_tokens(&messages[i]);
        if t == 0 {
            continue;
        }
        kept += t;
        if kept >= keep_recent {
            cut = cut_points
                .iter()
                .copied()
                .find(|&c| c >= i)
                .unwrap_or(cut_points[cut_points.len() - 1]);
            break;
        }
    }
    let turn_start = if messages[cut].role == Role::User {
        None
    } else {
        (start..cut).rev().find(|&i| messages[i].role == Role::User)
    };
    CutPoint {
        first_kept: cut,
        turn_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gardenwall_inference::client::{FunctionCall, ToolCall};

    fn msg(role: Role, chars: usize) -> ChatMessage {
        ChatMessage::new(role, "x".repeat(chars))
    }

    fn with_call(name: &str, args: &str) -> ChatMessage {
        let mut m = ChatMessage::new(Role::Assistant, "");
        m.tool_calls.push(ToolCall {
            id: "c".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        });
        m
    }

    #[test]
    fn estimates_round_up_and_count_tool_calls() {
        assert_eq!(estimate_tokens(&msg(Role::User, 5)), 2);
        let m = with_call("read", r#"{"path":"a"}"#);
        assert_eq!(estimate_tokens(&m), (4 + 12_u64).div_ceil(4));
        // UTF-16 counting: one astral character is two units.
        assert_eq!(estimate_tokens(&ChatMessage::new(Role::User, "😀😀")), 1);
        let mut with_images = ChatMessage::new(Role::User, "abcd");
        with_images.images = vec!["data:image/png;base64,AAAA".into(); 2];
        assert_eq!(estimate_tokens(&with_images), 1 + 2 * 1200);
    }

    #[test]
    fn compaction_triggers_inside_the_reserve() {
        let s = CompactionSettings::default();
        assert!(!should_compact(100_000, 131_072, &s));
        assert!(should_compact(115_000, 131_072, &s));
        let off = CompactionSettings {
            enabled: false,
            ..s
        };
        assert!(!should_compact(131_000, 131_072, &off));
        let small = s.for_window(8192);
        assert_eq!(small.reserve_tokens, 2048);
        assert_eq!(small.keep_recent_tokens, 2457);
    }

    #[test]
    fn cuts_never_split_a_tool_call_from_its_result() {
        // sys, user, assistant(call), tool, assistant, user, assistant
        let mut call = with_call("ls", "{}");
        call.content = "x".repeat(400);
        let m = vec![
            msg(Role::System, 10),
            msg(Role::User, 400),
            call,
            msg(Role::Tool, 400),
            msg(Role::Assistant, 400),
            msg(Role::User, 400),
            msg(Role::Assistant, 400),
        ];
        // Keep ~250 tokens: walking back reaches index 5 (200 tokens kept
        // after index 6 and 5 → 200), index 4 → 300 >= 250: cut at 4.
        let c = find_cut_point(&m, 1, m.len(), 250);
        assert_eq!(c.first_kept, 4);
        // Index 4 is an assistant message inside the turn started at 1.
        assert_eq!(c.turn_start, Some(1));
        // A budget that lands on the tool result moves forward to index 4 too.
        let c = find_cut_point(&m, 1, m.len(), 350);
        assert_eq!(c.first_kept, 4);
        // A budget that lands on a user message cuts cleanly.
        let c = find_cut_point(&m, 1, m.len(), 150);
        assert_eq!(c.first_kept, 5);
        assert_eq!(c.turn_start, None);
        // Everything fits: nothing to cut.
        let c = find_cut_point(&m, 1, m.len(), 1_000_000);
        assert_eq!(c.first_kept, 1);
    }

    #[test]
    fn serialization_matches_pi() {
        let mut call = with_call("edit", r#"{"path":"a.rs","n":3}"#);
        call.content = "Fixing.".into();
        call.reasoning_content = Some("secret thoughts".into());
        let m = vec![
            ChatMessage::new(Role::User, "Fix it"),
            call,
            ChatMessage::new(Role::Tool, "y".repeat(2005)),
        ];
        let text = serialize_conversation(&m);
        assert!(text.starts_with("[User]: Fix it\n\n[Assistant]: Fixing.\n\n"));
        assert!(text.contains(r#"[Assistant tool calls]: edit(n=3, path="a.rs")"#));
        assert!(text.ends_with("\n\n[... 5 more characters truncated]"));
        assert!(!text.contains("secret thoughts"));
    }

    #[test]
    fn file_lists_exclude_changed_files_from_reads() {
        let mut ops = FileOps::default();
        ops.collect(&[
            with_call("read", r#"{"path":"a"}"#),
            with_call("read", r#"{"path":"b"}"#),
            with_call("edit", r#"{"path":"b"}"#),
            with_call("bash", r#"{"command":"cat c"}"#),
        ]);
        assert_eq!(
            ops.format(),
            "\n\n<read-files>\na\n</read-files>\n\n<modified-files>\nb\n</modified-files>"
        );
        let s = Summary {
            text: "## Goal".into(),
            files: FileOps::default(),
        };
        assert_eq!(
            s.message().content,
            "The conversation history before this point was compacted into the following summary:\n\n<summary>\n## Goal\n</summary>"
        );
    }

    #[test]
    fn prompts_pick_initial_or_update() {
        let first = summary_request("[User]: hi", None);
        assert!(
            first[1]
                .content
                .contains("Create a structured context checkpoint")
        );
        assert!(!first[1].content.contains("<previous-summary>"));
        let next = summary_request("[User]: hi", Some("old"));
        assert!(
            next[1]
                .content
                .contains("<previous-summary>\nold\n</previous-summary>")
        );
        assert!(next[1].content.contains("NEW conversation messages"));
        assert!(merge_split_turn(None, "p").starts_with("No prior history.\n\n---"));
    }
}
