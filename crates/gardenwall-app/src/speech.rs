//! Read aloud (release plan feature 8). The reply's Markdown becomes plain
//! spoken text here, in the core (prose only: code, tables and tool output
//! are announced, never read out), split into short
//! pieces so the first one plays quickly. Each piece is spoken by Windows'
//! own voices inside the sandboxed worker (`gardenwall-parse --speak`): no
//! network, no files. Nothing is sent anywhere, so lockdown mode leaves it
//! on.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Longest piece spoken in one call; the first piece is shorter so speech
/// starts sooner.
const PIECE_CHARS: usize = 600;
const FIRST_PIECE_CHARS: usize = 220;
/// Most of one reply that is read (characters of spoken text).
const MAX_SPOKEN_CHARS: usize = 60_000;
/// How long one piece may take to synthesise.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(60);
const SANDBOX_PROFILE: &str = "Gardenwall.Speech";

/// Which speech recogniser voice input uses (the user
/// chooses; Auto is the F7.2 rule).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InputEngine {
    /// The GPU when it has room beside the loaded model, else the processor.
    #[default]
    Auto,
    /// Always the GPU (large model).
    Gpu,
    /// Always the processor (small model).
    Cpu,
    /// Voice input off: no microphone button or shortcut.
    Off,
}

/// Settings → Voice (features 7 and 8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceSettings {
    /// The Windows voice (its identifier); `None` uses Windows' default.
    pub read_voice: Option<String>,
    /// Speaking rate, 0.5 to 3.0 (1.0 is normal).
    pub read_rate: f64,
    /// Read new replies aloud as they finish (off by
    /// default).
    pub read_auto: bool,
    /// Voice input's language, a whisper.cpp code
    /// (fixed, English by default).
    pub input_language: String,
    /// Tidy transcripts with the loaded chat model
    /// (on by default).
    pub input_cleanup: bool,
    pub input_engine: InputEngine,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            read_voice: None,
            read_rate: 1.0,
            read_auto: false,
            input_language: "en".into(),
            input_cleanup: true,
            input_engine: InputEngine::Auto,
        }
    }
}

impl VoiceSettings {
    pub fn validate(&self) -> Result<(), String> {
        let (lo, hi) = gardenwall_parse::speech::RATE_RANGE;
        if !(lo..=hi).contains(&self.read_rate) {
            return Err(format!("the speaking rate must be between {lo} and {hi}"));
        }
        if !gardenwall_inference::whisper::valid_language(&self.input_language) {
            return Err("that isn't a language voice input knows".into());
        }
        if let Some(v) = &self.read_voice
            && !gardenwall_parse::speech::valid_voice_id(v)
        {
            return Err("that isn't a Windows voice".into());
        }
        Ok(())
    }
}

/// Characters that are invisible or reorder text: never spoken.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF | 0xE0000..=0xE007F)
        || (c.is_control() && c != '\n' && c != '\t')
}

/// `[text](url)` → `text`, `![alt](url)` → `Image: alt.`, bare web
/// addresses → "a link", inline code kept when short.
fn inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let image = c == '!' && chars.get(i + 1) == Some(&'[');
        if c == '[' || image {
            let start = if image { i + 2 } else { i + 1 };
            if let Some(close) = (start..chars.len()).find(|&j| chars[j] == ']')
                && chars.get(close + 1) == Some(&'(')
                && let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == ')')
            {
                let label: String = chars[start..close].iter().collect();
                if image {
                    let label = label.trim();
                    if label.is_empty() {
                        out.push_str("Image.");
                    } else {
                        out.push_str(&format!("Image: {label}."));
                    }
                } else {
                    out.push_str(&label);
                }
                i = end + 1;
                continue;
            }
        }
        if c == '`'
            && let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`')
        {
            let code: String = chars[i + 1..end].iter().collect();
            if code.chars().count() <= 40 {
                out.push_str(&code);
            } else {
                out.push_str("some code");
            }
            i = end + 1;
            continue;
        }
        if c == '<'
            && let Some(end) = (i + 1..chars.len().min(i + 200)).find(|&j| chars[j] == '>')
        {
            let tag: String = chars[i + 1..end].iter().collect();
            if tag.starts_with("http://") || tag.starts_with("https://") {
                out.push_str("a link");
                i = end + 1;
                continue;
            }
            if tag
                .trim_start_matches('/')
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            {
                i = end + 1;
                continue;
            }
        }
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
        if rest.starts_with("http://") || rest.starts_with("https://") || rest.starts_with("www.") {
            out.push_str("a link");
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Emphasis markers.
    out.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('*', "")
}

/// A reply's Markdown as text to speak: prose kept; code blocks, tables
/// and rules announced or dropped.
pub fn speech_text(markdown: &str) -> String {
    let clean: String = markdown.chars().filter(|&c| !invisible(c)).collect();
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<String> = None;
    let mut in_table = false;
    for raw in clean.lines() {
        let line = raw.trim();
        if let Some(marker) = &fence {
            if line.starts_with(marker.as_str()) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(line[..3].to_string());
            out.push("Code block.".into());
            continue;
        }
        if line.starts_with('|') {
            if !in_table {
                out.push("Table.".into());
            }
            in_table = true;
            continue;
        }
        in_table = false;
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        if line.chars().all(|c| "-*_= ".contains(c)) && line.len() >= 3 {
            continue;
        }
        let mut text = line.trim_start_matches('>').trim_start();
        let heading = text.starts_with('#');
        text = text.trim_start_matches('#').trim_start();
        for marker in ["- [ ] ", "- [x] ", "- ", "* ", "+ "] {
            if let Some(rest) = text.strip_prefix(marker) {
                text = rest;
                break;
            }
        }
        if let Some(dot) = text.find(". ")
            && dot > 0
            && dot <= 3
            && text[..dot].chars().all(|c| c.is_ascii_digit())
        {
            text = &text[dot + 2..];
        }
        let mut spoken = inline(text).trim().to_string();
        if spoken.is_empty() {
            continue;
        }
        if heading && !spoken.ends_with(['.', '!', '?', ':']) {
            spoken.push('.');
        }
        out.push(spoken);
    }
    // Paragraphs: lines joined with spaces, blank lines kept as breaks.
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in out {
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    // Repeated announcements ("Code block. Code block.") collapse.
    paragraphs.dedup();
    paragraphs.join("\n\n")
}

/// Split spoken text into pieces at paragraph and sentence ends: the first
/// at most [`FIRST_PIECE_CHARS`], the rest at most [`PIECE_CHARS`].
pub fn pieces(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut current = String::new();
        for c in paragraph.chars() {
            current.push(c);
            if matches!(c, '.' | '!' | '?' | ';' | ':') && current.chars().count() > 1 {
                sentences.push(std::mem::take(&mut current));
            }
        }
        if !current.trim().is_empty() {
            sentences.push(current);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut piece = String::new();
    let mut total = 0usize;
    for sentence in sentences {
        let sentence = sentence.trim().to_string();
        if sentence.is_empty() {
            continue;
        }
        total += sentence.chars().count();
        if total > MAX_SPOKEN_CHARS {
            if !piece.is_empty() {
                out.push(std::mem::take(&mut piece));
            }
            out.push("The rest of this reply isn't read aloud.".into());
            return out;
        }
        let limit = if out.is_empty() {
            FIRST_PIECE_CHARS
        } else {
            PIECE_CHARS
        };
        if !piece.is_empty() && piece.chars().count() + 1 + sentence.chars().count() > limit {
            out.push(std::mem::take(&mut piece));
        }
        // A single sentence longer than a piece is cut at a space.
        let mut sentence = sentence;
        while sentence.chars().count() > PIECE_CHARS {
            let cut: String = sentence.chars().take(PIECE_CHARS).collect();
            let at = cut.rfind(' ').filter(|&i| i > 0).unwrap_or(cut.len());
            if !piece.is_empty() {
                out.push(std::mem::take(&mut piece));
            }
            out.push(sentence[..at].trim().to_string());
            sentence = sentence[at..].trim().to_string();
        }
        if !piece.is_empty() {
            piece.push(' ');
        }
        piece.push_str(&sentence);
    }
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

/// The pieces to speak for one reply (shown text, Markdown).
#[tauri::command]
pub fn speech_prepare(text: String) -> Vec<String> {
    pieces(&speech_text(&text))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceView {
    pub id: String,
    pub name: String,
    pub language: String,
}

fn run_helper(state: &AppState, args: Vec<String>, input: &[u8]) -> CmdResult<Vec<u8>> {
    let worker = crate::state::stage_helper(&state.paths.parse_dir, "gardenwall-parse")
        .map_err(|e| format!("read aloud is unavailable: {e}"))?;
    run_sandboxed(&worker, &state.paths.grants, args, input)
}

/// Run the worker in its own sandbox (profile [`SANDBOX_PROFILE`]): no
/// network, read and execute access to its own folder only.
fn run_sandboxed(
    worker: &std::path::Path,
    journal: &std::path::Path,
    args: Vec<String>,
    input: &[u8],
) -> CmdResult<Vec<u8>> {
    let dir = worker.parent().ok_or("invalid helper path")?.to_path_buf();
    let out = crate::worker::run(
        crate::worker::WorkerSpec {
            profile: SANDBOX_PROFILE,
            program: worker,
            args: args.into_iter().map(Into::into).collect(),
            read_execute: vec![dir.clone()],
            read_write: vec![],
            // Windows' voice files are registered as `%windir%\...` paths,
            // so the engine needs these two (system folder names only).
            env: ["SystemRoot", "windir"]
                .into_iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k.into(), v)))
                .collect(),
            working_dir: dir,
            memory_bytes: 512 << 20,
            max_processes: 1,
            timeout: SPEAK_TIMEOUT,
            stdout_limit: gardenwall_parse::speech::MAX_WAV_BYTES,
            journal,
        },
        input,
    )?;
    match out.exit {
        None => Err("speaking took too long".into()),
        Some(0) => Ok(out.stdout),
        Some(3) => Err(out.stderr.trim().to_string()),
        Some(code) => Err(format!("read aloud failed (exit code {code})")),
    }
}

/// The Windows voices installed on this computer.
#[tauri::command]
pub async fn speech_voices(state: State<'_, AppState>) -> CmdResult<Vec<VoiceView>> {
    let bytes = tokio::task::block_in_place(|| run_helper(&state, vec!["--voices".into()], &[]))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| {
            let mut parts = l.split('\t');
            Some(VoiceView {
                id: parts.next()?.to_string(),
                name: parts.next()?.to_string(),
                language: parts.next().unwrap_or("").to_string(),
            })
        })
        .filter(|v| gardenwall_parse::speech::valid_voice_id(&v.id))
        .collect())
}

/// Speak one piece with the chosen voice and rate; a WAV file, base64.
#[tauri::command]
pub async fn speak_piece(state: State<'_, AppState>, text: String) -> CmdResult<String> {
    if text.chars().count() > gardenwall_parse::speech::MAX_SPEAK_CHARS {
        return Err("that piece is too long to speak".into());
    }
    if text.trim().is_empty() {
        return Err("nothing to read".into());
    }
    let voice = state.settings().voice;
    let mut args = vec![
        "--speak".to_string(),
        "--rate".into(),
        voice.read_rate.to_string(),
    ];
    if let Some(id) = voice.read_voice {
        args.push("--voice".into());
        args.push(id);
    }
    let wav = tokio::task::block_in_place(|| run_helper(&state, args, text.as_bytes()))?;
    if !wav.starts_with(b"RIFF") {
        return Err("read aloud returned audio in an unexpected format".into());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(wav))
}

/// Save Settings → Voice.
#[tauri::command]
pub fn set_voice_settings(state: State<'_, AppState>, settings: VoiceSettings) -> CmdResult<()> {
    settings.validate()?;
    state
        .update_settings(|s| s.voice = settings)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_tables_and_links_are_announced_not_read() {
        let md = "# Result\n\nRun `cargo test` first.\n\n```rust\nfn secret() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nSee [the docs](https://example.com/x) or https://example.com/y.\n\n- **one**\n- two\n\n1. first\n---\n> quoted <b>bold</b>";
        let text = speech_text(md);
        assert_eq!(
            text,
            "Result.\n\nRun cargo test first.\n\nCode block.\n\nTable.\n\nSee the docs or a link\n\none two\n\nfirst quoted bold"
        );
        assert!(!text.contains("secret"));
        assert!(!text.contains("example.com"));
    }

    #[test]
    fn long_inline_code_and_images_are_summarised() {
        let long = "x".repeat(50);
        assert_eq!(
            speech_text(&format!("Use `{long}` now.")),
            "Use some code now."
        );
        assert_eq!(speech_text("![a chart](c.png)"), "Image: a chart.");
        assert_eq!(speech_text("![](c.png)"), "Image.");
    }

    #[test]
    fn invisible_characters_are_never_spoken() {
        let hidden = "Hi\u{200B}there\u{E0041}\u{E0042}.";
        assert_eq!(speech_text(hidden), "Hithere.");
    }

    #[test]
    fn an_unclosed_code_block_hides_everything_after_it() {
        assert_eq!(
            speech_text("Look:\n```\nrm -rf /\nmore"),
            "Look: Code block."
        );
    }

    #[test]
    fn pieces_start_short_and_stay_bounded() {
        let sentence = "This is a sentence of moderate length for testing. ";
        let text = sentence.repeat(60);
        let p = pieces(&text);
        assert!(p[0].chars().count() <= FIRST_PIECE_CHARS);
        assert!(p.iter().all(|x| x.chars().count() <= PIECE_CHARS));
        assert_eq!(
            p.join(" ").split_whitespace().count(),
            text.split_whitespace().count()
        );
        // One huge sentence is cut at spaces.
        let words = "word ".repeat(400);
        assert!(
            pieces(&words)
                .iter()
                .all(|x| x.chars().count() <= PIECE_CHARS)
        );
        // Very long replies stop with a note.
        let huge = sentence.repeat(2000);
        assert_eq!(
            pieces(&huge).last().map(String::as_str),
            Some("The rest of this reply isn't read aloud.")
        );
    }

    /// Windows' voices speak inside the worker's real AppContainer. Needs
    /// the worker built (`cargo build -p gardenwall-parse`); skipped
    /// otherwise.
    #[cfg(windows)]
    #[test]
    fn windows_voices_speak_inside_the_sandbox() {
        use std::path::Path;
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
        let voices = run_sandboxed(&exe, &journal, vec!["--voices".into()], &[]).expect("voices");
        assert!(!voices.is_empty());
        let wav = run_sandboxed(
            &exe,
            &journal,
            vec!["--speak".into(), "--rate".into(), "1.2".into()],
            b"Hello from the sandbox.",
        )
        .expect("speech in the sandbox");
        assert!(wav.starts_with(b"RIFF") && wav.len() > 1000);
    }

    #[test]
    fn the_recogniser_choice_is_saved_by_name() {
        for (engine, name) in [
            (InputEngine::Auto, "\"auto\""),
            (InputEngine::Gpu, "\"gpu\""),
            (InputEngine::Cpu, "\"cpu\""),
            (InputEngine::Off, "\"off\""),
        ] {
            assert_eq!(serde_json::to_string(&engine).unwrap(), name);
        }
        // Settings saved before the choice existed read as Auto.
        let old: VoiceSettings = serde_json::from_str(r#"{"readRate":1.0}"#).unwrap();
        assert_eq!(old.input_engine, InputEngine::Auto);
        assert!(old.input_cleanup);
    }

    #[test]
    fn voice_settings_are_checked() {
        assert!(VoiceSettings::default().validate().is_ok());
        let fast = VoiceSettings {
            read_rate: 5.0,
            ..Default::default()
        };
        assert!(fast.validate().is_err());
        let odd = VoiceSettings {
            read_voice: Some("a\"; rm".into()),
            ..Default::default()
        };
        assert!(odd.validate().is_err());
    }
}
