//! HTML to plain text, for the sandboxed parse worker.
//!
//! A small, forgiving converter written for untrusted input. It never
//! fails, keeps no state beyond one pass, and bounds its output. It keeps
//! what a model needs to read a page: the title, headings, paragraphs,
//! list items, table cells, code blocks and link targets. Scripts, styles
//! and other non-content elements are dropped. It is not a full HTML5
//! parser and does not need to be: its output is only ever text for the
//! model, and it runs in a sandbox.

#![forbid(unsafe_code)]

pub mod documents;
pub mod ocr;
pub mod speech;

/// Most input read (bytes); the gateway's fetch limit is the same.
pub const MAX_INPUT_BYTES: usize = 6 << 20;
/// Most output written (characters).
pub const MAX_OUTPUT_CHARS: usize = 400_000;

/// Elements whose content is never text for the reader.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "math", "iframe", "object", "canvas",
    "select", "button",
];

/// Elements that start a new line.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "nav",
    "aside",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "table",
    "tr",
    "blockquote",
    "figure",
    "figcaption",
    "form",
    "fieldset",
    "details",
    "summary",
    "address",
    "hr",
    "br",
    "pre",
    "body",
    "html",
];

struct Out {
    text: String,
    max: usize,
    /// Pending whitespace: 0 none, 1 space, 2+ newlines.
    gap: u8,
    pre: usize,
}

impl Out {
    fn full(&self) -> bool {
        self.text.len() >= self.max
    }

    fn word(&mut self, s: &str) {
        if s.is_empty() || self.full() {
            return;
        }
        if !self.text.is_empty() {
            match self.gap {
                0 => {}
                1 => self.text.push(' '),
                n => {
                    let have = self.text.chars().rev().take_while(|c| *c == '\n').count();
                    for _ in have..usize::from(n.min(3)) - 1 {
                        self.text.push('\n');
                    }
                }
            }
        }
        self.gap = 0;
        let room = self.max - self.text.len();
        if s.len() <= room {
            self.text.push_str(s);
        } else {
            let mut end = room;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&s[..end]);
        }
    }

    fn text_run(&mut self, s: &str) {
        if self.pre > 0 {
            if self.gap > 1 {
                self.word("");
            }
            self.word(s);
            return;
        }
        let mut first = true;
        for part in s.split(char::is_whitespace) {
            if part.is_empty() {
                self.space();
                continue;
            }
            if !first {
                self.space();
            }
            self.word(part);
            first = false;
        }
        if s.ends_with(char::is_whitespace) {
            self.space();
        }
    }

    fn space(&mut self) {
        if self.gap == 0 {
            self.gap = 1;
        }
    }

    fn newline(&mut self, n: u8) {
        self.gap = self.gap.max(n + 1);
    }
}

/// Decode character references such as `&amp;`, `&#39;` and `&#x2014;`.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|e| *e <= 12);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = if let Some(num) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
            } else if let Some(num) = name.strip_prefix('#') {
                num.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                match name {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "mdash" => Some('—'),
                    "ndash" => Some('–'),
                    "hellip" => Some('…'),
                    "copy" => Some('©'),
                    "reg" => Some('®'),
                    "trade" => Some('™'),
                    "lsquo" => Some('‘'),
                    "rsquo" => Some('’'),
                    "ldquo" => Some('“'),
                    "rdquo" => Some('”'),
                    "middot" => Some('·'),
                    "bull" => Some('•'),
                    "times" => Some('×'),
                    "euro" => Some('€'),
                    "pound" => Some('£'),
                    _ => None,
                }
            };
            // Control characters other than whitespace are dropped.
            c.filter(|c| !c.is_control() || c.is_whitespace())
                .map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in a tag's attribute text.
fn attribute<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = attrs[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value = rest[1..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        });
    }
    None
}

/// Convert HTML to readable plain text of at most `max_chars` bytes.
pub fn html_to_text(html: &str, max_chars: usize) -> String {
    let mut out = Out {
        text: String::new(),
        max: max_chars,
        gap: 0,
        pre: 0,
    };
    let mut title: Option<String> = None;
    let mut in_title = false;
    // The href of the open <a>, and where its text started.
    let mut link: Option<(String, usize)> = None;
    let mut rest = html;
    while !rest.is_empty() && !out.full() {
        let Some(lt) = rest.find('<') else {
            out.text_run(&decode_entities(rest));
            break;
        };
        let (text, tail) = rest.split_at(lt);
        if !text.is_empty() {
            if in_title {
                title.get_or_insert_with(String::new).push_str(text);
            } else {
                out.text_run(&decode_entities(text));
            }
        }
        rest = tail;
        // Comments, doctype and processing instructions.
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            continue;
        }
        // A tag: find its end, outside quoted attribute values.
        let bytes = rest.as_bytes();
        let mut end = None;
        let mut quote = 0u8;
        for (i, &b) in bytes.iter().enumerate().skip(1) {
            match (quote, b) {
                (0, b'"' | b'\'') => quote = b,
                (q, _) if q != 0 && b == q => quote = 0,
                (0, b'>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            // An unterminated tag: treat the rest as text.
            out.text_run(&decode_entities(&rest[1..]));
            break;
        };
        let inner = &rest[1..end];
        rest = &rest[end + 1..];
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let name_len = inner
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(inner.len());
        if name_len == 0 {
            // "<" followed by something that is not a tag name: literal text.
            out.text_run("<");
            out.text_run(&decode_entities(inner));
            out.text_run(">");
            continue;
        }
        let name = inner[..name_len].to_ascii_lowercase();
        let attrs = &inner[name_len..];

        if name == "title" {
            in_title = !closing;
            continue;
        }
        if !closing && SKIP.contains(&name.as_str()) && !attrs.trim_end().ends_with('/') {
            // Jump to the closing tag without tokenizing the content: script
            // and style bodies are not HTML ("x<y" is not a tag).
            let needle = format!("</{name}");
            let lower = rest.to_ascii_lowercase();
            rest = match lower.find(&needle) {
                Some(i) => rest[i..].find('>').map_or("", |e| &rest[i + e + 1..]),
                None => "",
            };
            continue;
        }
        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                out.newline(2);
                if !closing {
                    let level = name.as_bytes()[1] - b'0';
                    out.word(&"#".repeat(usize::from(level)));
                    out.space();
                }
            }
            "li" if !closing => {
                out.newline(1);
                out.word("-");
                out.space();
            }
            "td" | "th" if !closing => out.word(" | "),
            "pre" => {
                out.newline(2);
                if closing {
                    out.pre = out.pre.saturating_sub(1);
                } else {
                    out.pre += 1;
                }
            }
            "a" if !closing => {
                link = attribute(attrs, "href")
                    .map(decode_entities)
                    .filter(|h| h.starts_with("http://") || h.starts_with("https://"))
                    .filter(|h| h.len() <= 2048)
                    .map(|h| (h, out.text.len()));
            }
            "a" => {
                if let Some((href, start)) = link.take() {
                    let shown = out.text.get(start..).unwrap_or("");
                    if !shown.trim().is_empty() && !shown.contains(&href) {
                        out.word(&format!(" ({href})"));
                    }
                }
            }
            "img" if !closing => {
                if let Some(alt) = attribute(attrs, "alt").filter(|a| !a.trim().is_empty()) {
                    out.space();
                    out.word(&format!("[image: {}]", decode_entities(alt).trim()));
                    out.space();
                }
            }
            n if BLOCK.contains(&n) => out.newline(if matches!(n, "p" | "pre" | "table") {
                2
            } else {
                1
            }),
            _ => {}
        }
    }
    let body = out.text.trim().to_string();
    match title
        .map(|t| decode_entities(t.trim()))
        .filter(|t| !t.is_empty())
    {
        Some(t) => {
            let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut s = format!("# {t}\n\n{body}");
            if s.len() > max_chars {
                let mut end = max_chars;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                s.truncate(end);
            }
            s
        }
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(html: &str) -> String {
        html_to_text(html, MAX_OUTPUT_CHARS)
    }

    #[test]
    fn pages_become_readable_text() {
        let html = r#"<!DOCTYPE html><html><head><title>Serde &amp; you</title>
            <style>body{color:red}</style><script>alert("x<y")</script></head>
            <body><nav><a href="/">Home</a></nav>
            <h1>Getting   started</h1>
            <p>Add <code>serde</code> to   <b>Cargo.toml</b>.</p>
            <ul><li>fast</li><li>flexible &mdash; very</li></ul>
            <p>See <a href="https://docs.rs/serde">the docs</a>.</p>
            <pre>fn main() {
    println!("hi");
}</pre>
            <img src="x.png" alt="A diagram"><!-- hidden comment -->
            </body></html>"#;
        let out = t(html);
        assert!(out.starts_with("# Serde & you\n\n"), "{out}");
        assert!(out.contains("# Getting started"), "{out}");
        assert!(out.contains("Add serde to Cargo.toml."), "{out}");
        assert!(out.contains("- fast\n- flexible — very"), "{out}");
        assert!(out.contains("the docs (https://docs.rs/serde)"), "{out}");
        assert!(
            out.contains("fn main() {\n    println!(\"hi\");\n}"),
            "{out}"
        );
        assert!(out.contains("[image: A diagram]"), "{out}");
        for gone in ["color:red", "alert", "hidden comment", "<", "DOCTYPE"] {
            assert!(!out.contains(gone), "{gone} in {out}");
        }
    }

    #[test]
    fn hostile_input_is_harmless() {
        // Unterminated tags and comments, huge attributes, odd entities.
        for html in [
            "<p>a <b",
            "<!-- never closed",
            "<script>never closed",
            "a < b and c > d",
            "&#0; &#xFFFFFFFF; &bogus; &#1114112; &",
            &format!("<a href=\"{}\">x</a>", "h".repeat(100_000)),
            "<div title='>'>quoted bracket</div>",
            "\u{0}\u{1}<p>\u{7f}</p>",
        ] {
            let out = t(html);
            assert!(out.len() <= MAX_OUTPUT_CHARS);
        }
        assert_eq!(t("a < b and c > d"), "a < b and c > d");
        assert_eq!(t("<div title='>'>quoted bracket</div>"), "quoted bracket");
        assert_eq!(t("<script>never closed"), "");
    }

    #[test]
    fn output_is_bounded() {
        let html = "<p>word</p>".repeat(100_000);
        let out = html_to_text(&html, 1000);
        assert!(out.len() <= 1000);
        let out = html_to_text("<title>éééé</title>ééééééééé", 7);
        assert!(out.len() <= 7);
    }
}
