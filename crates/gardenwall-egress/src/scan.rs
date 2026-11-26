//! Outbound secret detection (ADR-006, ADR-012 §5), as defence in depth.
//!
//! A web search query or URL proposed by the model is checked before the
//! approval dialog is shown. Anything that looks like a credential is
//! refused outright: a model (or a prompt injection) could otherwise try to
//! smuggle a key out inside an innocent-looking query. The patterns are
//! deliberately specific, so ordinary text is not blocked. Detection can
//! never be complete, which is why every request is also shown to the user.

use std::sync::OnceLock;

use regex::RegexSet;

/// The loose "password assignment" rule, last in [`PATTERNS`]. In source
/// code it matches type annotations and lookups (`token: TokenStream`,
/// `api_key = os.getenv(...)`), so [`find_secrets_in_code`] uses
/// [`CODE_ASSIGNMENT`] in its place.
const ASSIGNMENT: &str = "password assignment";

/// The assignment rule for source code: the value must be a quoted literal
/// of eight or more characters without spaces (`password = "hunter2hunter2"`,
/// `"api_key": "…"`, `token: 'abcdefgh'`, `if secret == "…"`). `=>` counts
/// only after a quoted or symbol key (`'password' => '…'`, `:token => "…"`),
/// not after a bare name or a path, which is a match arm
/// (`Self::Password => "password"`).
const CODE_ASSIGNMENT: &str = r#"(?i)(?:\b(?:password|passwd|secret|api[_-]?key|token)["']?\s*[:=]=?|(?:["']|(?:^|[\s,({\[]):)(?:password|passwd|secret|api[_-]?key|token)["']?\s*=>)\s*["'][^"'\s]{8,}["']"#;

const PATTERNS: &[(&str, &str)] = &[
    ("private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ("AWS access key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    (
        "GitHub token",
        r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b",
    ),
    ("GitHub token", r"\bgithub_pat_[A-Za-z0-9_]{60,}\b"),
    (
        "OpenAI-style API key",
        r"\bsk-(?:proj-|ant-)?[A-Za-z0-9_\-]{20,}\b",
    ),
    ("Hugging Face token", r"\bhf_[A-Za-z0-9]{30,}\b"),
    ("Slack token", r"\bxox[abprs]-[A-Za-z0-9\-]{10,}\b"),
    ("Google API key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
    ("Stripe key", r"\b(?:sk|rk)_live_[0-9A-Za-z]{20,}\b"),
    (
        "JSON web token",
        r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\b",
    ),
    (
        ASSIGNMENT,
        r"(?i)\b(?:password|passwd|secret|api[_-]?key|token)\s*[:=]\s*\S{8,}",
    ),
];

/// One compiled rule list: every pattern is tried in a single pass.
struct Rules {
    names: Vec<&'static str>,
    set: RegexSet,
}

fn rules(code: bool) -> &'static Rules {
    static PLAIN: OnceLock<Rules> = OnceLock::new();
    static CODE: OnceLock<Rules> = OnceLock::new();
    let build = move || {
        let (names, res): (Vec<&'static str>, Vec<&'static str>) = PATTERNS
            .iter()
            .map(|&(name, re)| match name {
                ASSIGNMENT if code => (name, CODE_ASSIGNMENT),
                _ => (name, re),
            })
            .unzip();
        Rules {
            names,
            set: RegexSet::new(res).expect("patterns compile"),
        }
    };
    if code {
        CODE.get_or_init(build)
    } else {
        PLAIN.get_or_init(build)
    }
}

fn find_with(text: &str, code: bool) -> Vec<&'static str> {
    let rules = rules(code);
    let plain = rules.set.matches(text);
    // URL-encoded text is checked in its decoded form too (only text that
    // has something to decode: this runs on every line of a commit).
    let decoded = text
        .contains(['%', '+'])
        .then(|| rules.set.matches(&percent_decode(text)));
    let mut found: Vec<&'static str> = Vec::new();
    for (i, name) in rules.names.iter().enumerate() {
        let hit = plain.matched(i) || decoded.as_ref().is_some_and(|d| d.matched(i));
        if hit && !found.contains(name) {
            found.push(name);
        }
    }
    found
}

/// The kinds of secret found in `text` (empty when none).
pub fn find_secrets(text: &str) -> Vec<&'static str> {
    find_with(text, false)
}

/// [`find_secrets`] for a line of source code: the same rules, except that
/// a "password assignment" needs a quoted literal value, so type
/// annotations and lookups (`token: TokenStream`) aren't reported.
pub fn find_secrets_in_code(text: &str) -> Vec<&'static str> {
    find_with(text, true)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(if b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_found() {
        for (text, kind) in [
            ("key AKIAIOSFODNN7EXAMPLE leaked", "AWS access key"),
            ("ghp_0123456789abcdefghijABCDEFGHIJ012345", "GitHub token"),
            (
                "use sk-proj-abcdefghijklmnopqrstuvwxyz123456",
                "OpenAI-style API key",
            ),
            ("hf_abcdefghijklmnopqrstuvwxyzABCDEF", "Hugging Face token"),
            ("-----BEGIN OPENSSH PRIVATE KEY-----", "private key"),
            ("password=hunter2hunter2", "password assignment"),
            (
                "https://x.io/?q=api_key%3Dabcdefgh12345678",
                "password assignment",
            ),
        ] {
            assert!(find_secrets(text).contains(&kind), "{kind}: {text}");
        }
    }

    #[test]
    fn code_needs_a_quoted_value() {
        for line in [
            "    token: TokenStream,",
            "fn new(secret: &'static str) -> Self {",
            "let token = parser.next_token()?;",
            "api_key = os.environ[\"API_KEY\"]",
            "password: String,",
            "const token = req.headers.authorization;",
            "token = \"${{ secrets.DEPLOY }}\"",
            "            Self::Password => \"Password\",",
        ] {
            assert!(find_secrets_in_code(line).is_empty(), "{line}");
        }
        for line in [
            "password = \"hunter2hunter2\"",
            "    \"api_key\": \"abcdefgh12345678\",",
            "  { token: 'abcdefgh1234' },",
            "if secret == \"abcdefgh1234\" {",
            "    'password' => 'hunter2hunter2',",
            "  :api_key => \"abcdefgh1234\",",
            "AWS = \"AKIAIOSFODNN7EXAMPLE\"",
        ] {
            assert!(!find_secrets_in_code(line).is_empty(), "{line}");
        }
        // Outside code the loose rule stays (shell, .env, plain text).
        assert_eq!(find_secrets("    token: TokenStream,"), [ASSIGNMENT]);
        assert_eq!(find_secrets("export API_KEY=abcdefgh123"), [ASSIGNMENT]);
    }

    #[test]
    fn ordinary_queries_pass() {
        for text in [
            "rust tokio spawn_blocking example",
            "how to reset a forgotten password on windows 11",
            "https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html",
            "sk-learn random forest tutorial",
            "what is an API key",
            "100%25 cotton",
        ] {
            assert!(
                find_secrets(text).is_empty(),
                "{text}: {:?}",
                find_secrets(text)
            );
        }
    }
}
