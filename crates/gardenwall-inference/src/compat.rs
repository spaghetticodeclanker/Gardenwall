//! llama.cpp compatibility layer (ADR-017 §2.1).
//!
//! The flags Gardenwall passes are declared as data ([`FLAGS`]), each with
//! why it is needed and what happens when a runtime lacks it. A runtime's
//! supported flags come from its `llama-server --help` output, which is
//! read once inside the sandbox and cached per build.
//!
//! The help text is untrusted output. It can only select among flags
//! Gardenwall already knows, and a runtime that claims a flag it ignores
//! gains nothing: the OS sandbox enforces the security properties these
//! flags request (ADR-012). A runtime missing a *security* flag is refused.

use std::collections::BTreeSet;
use std::fmt;

/// Why a flag is passed, which decides what happens without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Turns off a network or inspection feature. A runtime without it is
    /// refused, even though the sandbox would contain it anyway.
    Security,
    /// Needed for Gardenwall to work at all (model, port, templates).
    Required,
    /// Used when present; left out, or replaced by a fallback, otherwise.
    Optional,
}

/// Every flag Gardenwall may pass, with its purpose.
pub const FLAGS: &[(&str, Need, &str)] = &[
    ("--model", Need::Required, "load the validated model file"),
    (
        "--alias",
        Need::Required,
        "serve the model under a fixed name",
    ),
    ("--host", Need::Required, "listen on loopback only"),
    ("--port", Need::Required, "let the runtime pick a free port"),
    (
        "--jinja",
        Need::Required,
        "native tool calling with the model's chat template",
    ),
    ("--no-webui", Need::Security, "turn off the built-in web UI"),
    (
        "--offline",
        Need::Security,
        "turn off model downloads and network access",
    ),
    (
        "--no-slots",
        Need::Security,
        "turn off the slot inspection endpoint",
    ),
    (
        "--no-webui-mcp-proxy",
        Need::Optional,
        "turn off the web UI's MCP proxy (older runtimes have none)",
    ),
    (
        "--parallel",
        Need::Optional,
        "one slot, so one conversation gets the whole context",
    ),
    (
        "--kv-unified",
        Need::Optional,
        "parallel agent sessions share one context memory",
    ),
    (
        "--fit",
        Need::Optional,
        "fit context and GPU layers to free device memory",
    ),
    (
        "--fit-target",
        Need::Optional,
        "leave a GPU memory margin when fitting",
    ),
    (
        "--fit-ctx",
        Need::Optional,
        "smallest context the fitter may choose",
    ),
    (
        "--ctx-size",
        Need::Required,
        "context size, when chosen or when fitting is unavailable",
    ),
    (
        "--gpu-layers",
        Need::Optional,
        "GPU offload, when chosen or when fitting is unavailable",
    ),
    (
        "--cache-type-k",
        Need::Optional,
        "q8_0 KV cache, when chosen",
    ),
    (
        "--cache-type-v",
        Need::Optional,
        "q8_0 KV cache, when chosen",
    ),
    (
        "--chat-template-file",
        Need::Optional,
        "the replacement template chosen after the audit",
    ),
    (
        "--embedding",
        Need::Optional,
        "serve an embedding model for document search",
    ),
    (
        "--ubatch-size",
        Need::Optional,
        "let embedding inputs fit in one batch",
    ),
    (
        "--mmproj",
        Need::Optional,
        "the image projector of a vision model",
    ),
    (
        "--log-verbosity",
        Need::Optional,
        "the model-loading log lines that show where weights were placed",
    ),
    (
        "--moe-cache-mib",
        Need::Optional,
        "a GPU cache for MoE experts kept in system RAM, when chosen",
    ),
    (
        "--spec-type",
        Need::Optional,
        "speculative decoding (faster generation), when chosen",
    ),
    (
        "--spec-draft-model",
        Need::Optional,
        "a paired draft model for speculative decoding",
    ),
    (
        "--spec-draft-type-k",
        Need::Optional,
        "the draft model's q8_0 KV cache",
    ),
    (
        "--spec-draft-type-v",
        Need::Optional,
        "the draft model's q8_0 KV cache",
    ),
];

/// Context used when a runtime can't fit to device memory itself.
pub const CONTEXT_WITHOUT_FIT: u32 = 8192;

/// Help output with fewer flags than this is not `llama-server`'s.
const MIN_HELP_FLAGS: usize = 40;

/// The flags a runtime build accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFeatures {
    flags: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatError {
    /// The output did not look like `llama-server --help`.
    UnrecognizedHelp,
    /// The runtime lacks flags Gardenwall can't run without.
    Missing(Vec<&'static str>),
    /// The user chose something this runtime can't do.
    Unsupported(&'static str),
}

impl fmt::Display for CompatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrecognizedHelp => f.write_str("the runtime's option list could not be read"),
            Self::Missing(flags) => write!(
                f,
                "this runtime version is not compatible with Gardenwall: it lacks {}",
                flags.join(", ")
            ),
            Self::Unsupported(what) => write!(f, "this runtime version does not support {what}"),
        }
    }
}

impl std::error::Error for CompatError {}

impl RuntimeFeatures {
    /// Parse `llama-server --help`. Flags are the `--name` tokens in the
    /// option column of each line, including every alias and `--no-` form.
    pub fn from_help(text: &str) -> Result<Self, CompatError> {
        let mut flags = BTreeSet::new();
        for line in text.lines() {
            let line = line.trim_start();
            if !line.starts_with('-') {
                continue; // a description continuation line
            }
            // Options and their aliases come first, in segments separated by
            // runs of spaces (`-m,    --model FNAME`); the description is the
            // first segment that does not start with a dash.
            let column = line
                .split("  ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take_while(|s| s.starts_with('-'))
                .collect::<Vec<_>>()
                .join(" ");
            for token in column.split([',', ' ']) {
                let token = token.trim();
                if let Some(name) = token.strip_prefix("--")
                    && !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                {
                    flags.insert(format!("--{name}"));
                }
            }
        }
        if flags.len() < MIN_HELP_FLAGS {
            return Err(CompatError::UnrecognizedHelp);
        }
        Ok(Self { flags })
    }

    /// Every flag in [`FLAGS`]: what the pinned build (`b12283`) supports.
    /// Used by tests and examples; the app detects features per runtime.
    pub fn all_known() -> Self {
        Self {
            flags: FLAGS.iter().map(|(f, _, _)| (*f).to_string()).collect(),
        }
    }

    pub fn supports(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    /// Flags from [`FLAGS`] that must be present, but are not.
    pub fn missing_essentials(&self) -> Vec<&'static str> {
        FLAGS
            .iter()
            .filter(|(f, need, _)| *need != Need::Optional && !self.supports(f))
            .map(|(f, _, _)| *f)
            .collect()
    }

    /// The flags Gardenwall uses that this runtime has, for caching.
    pub fn known_flags(&self) -> Vec<String> {
        FLAGS
            .iter()
            .filter(|(f, _, _)| self.supports(f))
            .map(|(f, _, _)| (*f).to_string())
            .collect()
    }

    /// A short fingerprint of the flag table. A cache of
    /// [`Self::known_flags`] is only valid for the table that wrote it:
    /// flags added later would otherwise look unsupported.
    pub fn table_fingerprint() -> String {
        // FNV-1a over the flag names; stable across builds and platforms.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (flag, _, _) in FLAGS {
            for b in flag.bytes().chain([0]) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{h:016x}")
    }

    /// Rebuild from [`Self::known_flags`] (a cache written by the core).
    pub fn from_known_flags<I: IntoIterator<Item = String>>(flags: I) -> Self {
        let known: BTreeSet<&str> = FLAGS.iter().map(|(f, _, _)| *f).collect();
        Self {
            flags: flags
                .into_iter()
                .filter(|f| known.contains(f.as_str()))
                .collect(),
        }
    }

    /// Whether the runtime can fit context and layers to device memory.
    pub fn can_fit(&self) -> bool {
        ["--fit", "--fit-target", "--fit-ctx"]
            .iter()
            .all(|f| self.supports(f))
    }
}

impl Default for RuntimeFeatures {
    fn default() -> Self {
        Self::all_known()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = include_str!("../tests/fixtures/llama-server-b12283-help.txt");

    #[test]
    fn table_fingerprint_is_stable_and_hex() {
        let f = RuntimeFeatures::table_fingerprint();
        assert_eq!(f.len(), 16);
        assert!(f.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(f, RuntimeFeatures::table_fingerprint());
    }

    #[test]
    fn the_pinned_runtime_supports_every_flag_gardenwall_uses() {
        let f = RuntimeFeatures::from_help(HELP).unwrap();
        for (flag, _, _) in FLAGS {
            assert!(f.supports(flag), "b12283 should support {flag}");
        }
        assert!(f.missing_essentials().is_empty());
        assert!(f.can_fit());
        // Aliases and negated forms on one line are all picked up.
        assert!(f.supports("--webui") && f.supports("--no-ui"));
        assert!(f.supports("--n-gpu-layers"));
        // Description text is not mistaken for flags.
        assert!(!f.supports("--usage-of-text"));
    }

    #[test]
    fn the_newest_pinned_runtime_supports_every_flag_too() {
        let help = include_str!("../tests/fixtures/llama-server-b12556-help.txt");
        let f = RuntimeFeatures::from_help(help).unwrap();
        for (flag, _, _) in FLAGS {
            assert!(f.supports(flag), "b12556 should support {flag}");
        }
        assert!(f.missing_essentials().is_empty());
        assert!(f.can_fit());
    }

    #[test]
    fn missing_security_flags_are_reported() {
        let old = HELP
            .lines()
            .filter(|l| !l.contains("--no-slots") && !l.contains("--fit"))
            .collect::<Vec<_>>()
            .join("\n");
        let f = RuntimeFeatures::from_help(&old).unwrap();
        assert_eq!(f.missing_essentials(), vec!["--no-slots"]);
        assert!(!f.can_fit());
    }

    #[test]
    fn other_output_is_not_taken_for_a_runtime() {
        assert_eq!(
            RuntimeFeatures::from_help("--help  print help\n--version  print version"),
            Err(CompatError::UnrecognizedHelp)
        );
    }

    #[test]
    fn cached_flags_round_trip_and_ignore_unknown_names() {
        let f = RuntimeFeatures::from_help(HELP).unwrap();
        let cached = f.known_flags();
        let mut with_junk = cached.clone();
        with_junk.push("--evil".into());
        let back = RuntimeFeatures::from_known_flags(with_junk);
        assert_eq!(back.known_flags(), cached);
        assert!(!back.supports("--evil"));
    }
}
