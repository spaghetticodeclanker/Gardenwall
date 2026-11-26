//! Output truncation, ported from Pi (`packages/coding-agent/src/core/tools/truncate.ts`,
//! pi-mono commit 682c7b9). Copyright (c) 2025 Mario Zechner, MIT License;
//! see THIRD_PARTY_NOTICES.md.
//!
//! Lines are counted like Pi: a trailing newline does not start a new line.
//! Sizes are UTF-8 bytes. Head truncation never returns a partial line; tail
//! truncation may return the end of an over-long last line.

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncation {
    pub content: String,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Truncation {
    pub fn truncated(&self) -> bool {
        self.truncated_by.is_some()
    }
}

fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Human-readable size, as Pi formats it.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn untruncated(
    content: &str,
    total_lines: usize,
    max_lines: usize,
    max_bytes: usize,
) -> Truncation {
    Truncation {
        content: content.to_string(),
        truncated_by: None,
        total_lines,
        total_bytes: content.len(),
        output_lines: total_lines,
        output_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Keep the first lines that fit (for file reads).
pub fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let lines = split_lines(content);
    let total_lines = lines.len();
    if total_lines <= max_lines && content.len() <= max_bytes {
        return untruncated(content, total_lines, max_lines, max_bytes);
    }
    if lines.first().is_some_and(|l| l.len() > max_bytes) {
        return Truncation {
            content: String::new(),
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes: content.len(),
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }
    let mut out: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    let mut by = TruncatedBy::Lines;
    for (i, line) in lines.iter().enumerate().take(max_lines) {
        let cost = line.len() + usize::from(i > 0);
        if bytes + cost > max_bytes {
            by = TruncatedBy::Bytes;
            break;
        }
        out.push(line);
        bytes += cost;
    }
    if out.len() >= max_lines && bytes <= max_bytes {
        by = TruncatedBy::Lines;
    }
    let content_out = out.join("\n");
    Truncation {
        output_bytes: content_out.len(),
        output_lines: out.len(),
        content: content_out,
        truncated_by: Some(by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// The last `max_bytes` of `s`, starting on a character boundary.
fn tail_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Keep the last lines that fit (for command output).
pub fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let lines = split_lines(content);
    let total_lines = lines.len();
    if total_lines <= max_lines && content.len() <= max_bytes {
        return untruncated(content, total_lines, max_lines, max_bytes);
    }
    let mut out: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    let mut by = TruncatedBy::Lines;
    let mut partial = false;
    for line in lines.iter().rev() {
        if out.len() >= max_lines {
            break;
        }
        let cost = line.len() + usize::from(!out.is_empty());
        if bytes + cost > max_bytes {
            by = TruncatedBy::Bytes;
            if out.is_empty() {
                let piece = tail_bytes(line, max_bytes);
                out.push(piece);
                bytes = piece.len();
                partial = true;
            }
            break;
        }
        out.push(line);
        bytes += cost;
    }
    if out.len() >= max_lines && bytes <= max_bytes {
        by = TruncatedBy::Lines;
    }
    out.reverse();
    let content_out = out.join("\n");
    Truncation {
        output_bytes: content_out.len(),
        output_lines: out.len(),
        content: content_out,
        truncated_by: Some(by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial: partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Cap one line at `max_chars` characters (grep results).
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    match line.char_indices().nth(max_chars) {
        None => (line.to_string(), false),
        Some((cut, _)) => (format!("{}... [truncated]", &line[..cut]), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_keeps_whole_lines_within_limits() {
        let t = truncate_head("a\nb\nc\n", 2, 100);
        assert_eq!(t.content, "a\nb");
        assert_eq!(t.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(t.total_lines, 3);
        let t = truncate_head("aaaa\nbbbb\ncccc", 10, 9);
        assert_eq!(t.content, "aaaa\nbbbb");
        assert_eq!(t.truncated_by, Some(TruncatedBy::Bytes));
        let t = truncate_head("x".repeat(20).as_str(), 10, 5);
        assert!(t.first_line_exceeds_limit && t.content.is_empty());
        let t = truncate_head("short\n", 10, 100);
        assert!(!t.truncated() && t.content == "short\n" && t.total_lines == 1);
    }

    #[test]
    fn tail_keeps_the_end_and_may_split_a_long_last_line() {
        let t = truncate_tail("1\n2\n3\n4", 2, 100);
        assert_eq!(t.content, "3\n4");
        let t = truncate_tail("é".repeat(10).as_str(), 10, 5);
        assert!(t.last_line_partial);
        assert_eq!(t.content, "éé");
    }

    #[test]
    fn lines_are_capped_on_character_boundaries() {
        assert_eq!(truncate_line("héllo", 10), ("héllo".into(), false));
        assert_eq!(
            truncate_line("héllo", 2),
            ("hé... [truncated]".into(), true)
        );
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(51200), "50.0KB");
    }
}
