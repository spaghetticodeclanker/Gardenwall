//! Construction of the `llama-server` command line and environment.
//!
//! The inference runtime is untrusted (ADR-002). Independently of the OS
//! sandbox (ADR-012), the launcher removes every runtime feature that can
//! reach the network or load extra code/data:
//! - the environment is cleared, so `LLAMA_ARG_*`, `HF_TOKEN`, proxy
//!   variables, and `PATH` lookups (e.g. ffmpeg) cannot influence it;
//! - the web UI and its MCP proxy are disabled, and `--offline` is set;
//! - model-download flags are never passed; only a local, validated model
//!   path is used;
//! - slot inspection endpoints are disabled;
//! - the server binds to loopback only, on a port it chooses itself.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::compat::{CONTEXT_WITHOUT_FIT, CompatError, RuntimeFeatures};

/// How many model layers to offload to the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuOffload {
    /// Let the runtime place layers to fit free device memory (`--fit`).
    Auto,
    None,
    All,
    Layers(u32),
}

/// KV-cache element type. `Q8_0` roughly halves cache memory (allowing about
/// twice the context) at a small quality cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KvCacheType {
    #[default]
    F16,
    Q8_0,
}

/// Smallest context the runtime may shrink to when fitting device memory,
/// when the caller asks for nothing else (embedding models, tests).
pub const MIN_FIT_CONTEXT: u32 = 4096;

/// The recommended smallest context for chat and the agent
/// . A thinking model can fill a smaller window with reasoning
/// before it answers, and the agent's fixed prompt alone is about 2,400
/// tokens: in the first baseline, 8K and 16K windows lost 10-13 % of agent
/// tasks. Automatic context never goes below it (up to the model's trained
/// length); users may still choose a smaller context by hand.
pub const RECOMMENDED_MIN_CONTEXT: u32 = 16384;

/// The same for Mixture-of-Experts models. llama.cpp's fitter shrinks the
/// context before moving expert weights to system RAM, so a MoE model larger
/// than VRAM got only 4,096 tokens, enough for one or two answers from a
/// thinking model. Moving a few more experts costs little: measured on a
/// 63 GiB MoE on a 32 GiB RTX 5090, 64K context instead of 4K moved one more
/// layer's experts and cost about 5-10 % of generation speed.
pub const MIN_FIT_CONTEXT_MOE: u32 = 32768;

/// Device memory (MiB) the fitter leaves free for the rest of the system.
///
/// llama.cpp's default (1024) is too tight on a desktop: the compositor,
/// browser and other apps grow their GPU use after load, and the Windows
/// driver then silently pages VRAM to system memory. Measured with a 27B Q6_K
/// model on a 32 GiB RTX 5090: a 1 GiB margin fitted 144K context but ran at
/// 12.5 tok/s generation and 330 tok/s prompt; 2 GiB or more ran at
/// 54 tok/s and 2,830 tok/s. 3 GiB leaves room for other apps to grow.
pub const FIT_MARGIN_MIB: u32 = 3072;

/// A random bearer key shared only between the trusted core and the sidecar.
/// Passed through the child's environment, never its command line.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// 256 bits from the OS CSPRNG, hex-encoded.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// A key the caller generated and shares with another trusted local
    /// client of the same server (the parity benchmark's Pi arm). Must be
    /// at least 32 characters of hex.
    pub fn from_secret(secret: String) -> Option<Self> {
        (secret.len() >= 32 && secret.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(Self(secret))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Verified `llama-server` executable.
    pub server_exe: PathBuf,
    /// Validated GGUF model (the first part of a split model).
    pub model: PathBuf,
    /// The other parts of a split model, in the same folder as `model`
    /// under their published names (llama.cpp finds them by name). Each is
    /// granted read-only like the model.
    pub model_parts: Vec<PathBuf>,
    /// Lowercase hex SHA-256 of the model, from intake. Required for
    /// sandboxed launch: it keys the model's persistent sandbox identity
    /// (ADR-012 §4.1, option 2).
    pub model_sha256: Option<String>,
    /// Replacement chat template chosen after the template audit (ADR-013).
    pub chat_template_file: Option<PathBuf>,
    /// Context size in tokens. `None` lets the runtime choose the largest
    /// context (up to the trained length, down to [`MIN_FIT_CONTEXT`]) that
    /// fits free device memory.
    pub context_size: Option<u32>,
    pub gpu: GpuOffload,
    pub kv_cache: KvCacheType,
    /// Dedicated, otherwise empty working directory for the child.
    pub work_dir: PathBuf,
    /// Additional environment variables required by GPU drivers on some
    /// platforms. Names must be on [`ALLOWED_EXTRA_ENV`].
    pub extra_env: Vec<(String, OsString)>,
    /// Flags this runtime build accepts (ADR-017 §2.1), detected from its
    /// `--help`. Missing security or required flags refuse the launch.
    pub features: RuntimeFeatures,
    /// Serve embeddings only (document search), not chat.
    pub embeddings: bool,
    /// The image projector of a vision model (validated GGUF), if any.
    pub mmproj: Option<PathBuf>,
    /// Speculative decoding (faster generation), off unless chosen.
    pub speculative: Speculative,
    /// GPU cache (MiB) for MoE experts kept in system RAM; 0 is off. The
    /// fitting margin grows by the same amount (see [`FIT_MARGIN_MIB`]).
    pub moe_cache_mib: u32,
    /// Smallest automatic context; `None` is [`MIN_FIT_CONTEXT`]. The app
    /// passes [`MIN_FIT_CONTEXT_MOE`] (capped at the trained length) for
    /// Mixture-of-Experts models.
    pub min_fit_context: Option<u32>,
    /// Requests served at the same time (slots), 1 to [`MAX_PARALLEL`].
    /// Above 1 the slots share one context memory (`--kv-unified`) when the
    /// runtime supports it, so each can still use the whole window while
    /// the total fits.
    pub parallel: u32,
}

/// Most parallel slots accepted.
pub const MAX_PARALLEL: u32 = 4;

/// Largest expert cache accepted (MiB).
pub const MAX_MOE_CACHE_MIB: u32 = 16 * 1024;

/// Speculative decoding: guessed tokens are checked by the main model in
/// one batch, so output is the same as without it, only faster when the
/// guesses are right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Speculative {
    #[default]
    Off,
    /// Draft-free: guesses come from text already in the context
    /// (`ngram-mod`). No extra model or memory.
    Ngram,
    /// A small model with the same tokenizer drafts (validated GGUF).
    Draft(PathBuf),
    /// The model's own multi-token-prediction layer drafts (for models
    /// that ship one). No extra file; a little extra memory.
    Mtp,
}

/// The draft-free method: n-gram matches against the whole context, best
/// when output repeats earlier text (code edits, quoting).
pub const NGRAM_SPEC_TYPE: &str = "ngram-mod";

/// Environment variables a caller may forward to the runtime. Anything else
/// is refused so that configuration cannot re-enable network features.
pub const ALLOWED_EXTRA_ENV: &[&str] = &[
    "CUDA_VISIBLE_DEVICES",
    "HIP_VISIBLE_DEVICES",
    "GGML_VK_VISIBLE_DEVICES",
    "LD_LIBRARY_PATH",
    "VK_ICD_FILENAMES",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    DisallowedEnv(String),
    InvalidContextSize(u32),
    InvalidMoeCache(u32),
    Compat(CompatError),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DisallowedEnv(name) => write!(f, "environment variable {name} is not allowed"),
            Self::InvalidContextSize(n) => write!(f, "invalid context size {n}"),
            Self::InvalidMoeCache(n) => write!(f, "invalid expert cache size {n} MiB"),
            Self::Compat(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

pub const LOOPBACK_HOST: &str = "127.0.0.1";
/// Name the model is served under, so local paths are not echoed in API
/// responses.
pub const MODEL_ALIAS: &str = "gardenwall-model";

impl LaunchSpec {
    pub fn args(&self) -> Result<Vec<OsString>, LaunchError> {
        let f = &self.features;
        let missing = f.missing_essentials();
        if !missing.is_empty() {
            return Err(LaunchError::Compat(CompatError::Missing(missing)));
        }
        let unsupported = |what| LaunchError::Compat(CompatError::Unsupported(what));
        let mut args: Vec<OsString> = Vec::new();
        let mut push = |a: &str| args.push(a.into());
        push("--model");
        args.push(self.model.as_os_str().to_owned());
        let mut push = |a: &str| args.push(a.into());
        push("--alias");
        push(MODEL_ALIAS);
        push("--host");
        push(LOOPBACK_HOST);
        push("--port");
        push("0");
        push("--no-webui");
        if f.supports("--no-webui-mcp-proxy") {
            push("--no-webui-mcp-proxy");
        }
        push("--offline");
        // "Trace" level adds the model-loading lines (where the weights
        // went, see `placement`). Checked on b12283: it logs no prompt or
        // output text, and the log is only kept in memory.
        if f.supports("--log-verbosity") {
            push("--log-verbosity");
            push("4");
        }
        push("--no-slots");
        push("--jinja");
        // A single slot gives the whole context window to one
        // conversation; several share one context memory when possible.
        let parallel = self.parallel.clamp(1, MAX_PARALLEL);
        if f.supports("--parallel") {
            push("--parallel");
            push(&parallel.to_string());
            if parallel > 1 && f.supports("--kv-unified") {
                push("--kv-unified");
            }
        } else if parallel > 1 {
            return Err(unsupported("serving several requests at once"));
        }
        let fit = f.can_fit();
        if fit {
            // Fitting only adjusts options left unset (context, GPU
            // layers), so explicit choices below are always honoured.
            push("--fit");
            push("on");
            // The expert cache is allocated next to the model. Whether or
            // not the fitter counts it, a margin grown by the cache's size
            // keeps it from pushing VRAM into driver paging.
            push("--fit-target");
            push(&(FIT_MARGIN_MIB + self.moe_cache_mib).to_string());
        }
        match self.context_size {
            Some(n) => {
                if !(512..=(1 << 24)).contains(&n) {
                    return Err(LaunchError::InvalidContextSize(n));
                }
                push("--ctx-size");
                push(&n.to_string());
            }
            None if fit => {
                push("--fit-ctx");
                let min = self
                    .min_fit_context
                    .unwrap_or(MIN_FIT_CONTEXT)
                    .clamp(512, 1 << 24);
                push(&min.to_string());
            }
            // Without fitting, a fixed context that fits most GPUs.
            None => {
                push("--ctx-size");
                push(&CONTEXT_WITHOUT_FIT.to_string());
            }
        }
        if !matches!(self.gpu, GpuOffload::Auto) && !f.supports("--gpu-layers") {
            return Err(unsupported("choosing the number of GPU layers"));
        }
        match self.gpu {
            GpuOffload::Auto if !fit && f.supports("--gpu-layers") => {
                push("--gpu-layers");
                push("all");
            }
            GpuOffload::Auto => {}
            GpuOffload::None => {
                push("--gpu-layers");
                push("0");
            }
            GpuOffload::All => {
                push("--gpu-layers");
                push("all");
            }
            GpuOffload::Layers(n) => {
                push("--gpu-layers");
                push(&n.to_string());
            }
        }
        if self.kv_cache == KvCacheType::Q8_0 {
            if !(f.supports("--cache-type-k") && f.supports("--cache-type-v")) {
                return Err(unsupported("the q8_0 KV cache"));
            }
            for flag in ["--cache-type-k", "--cache-type-v"] {
                push(flag);
                push("q8_0");
            }
        }
        if self.embeddings {
            if !f.supports("--embedding") {
                return Err(unsupported("embedding models"));
            }
            push("--embedding");
            if f.supports("--ubatch-size") {
                // Non-causal embedding models need a whole input in one batch.
                push("--ubatch-size");
                push("2048");
            }
        }
        if let Some(mmproj) = &self.mmproj {
            if !f.supports("--mmproj") {
                return Err(unsupported("vision models"));
            }
            args.push("--mmproj".into());
            args.push(mmproj.as_os_str().to_owned());
        }
        if self.moe_cache_mib > 0 && !self.embeddings {
            if self.moe_cache_mib > MAX_MOE_CACHE_MIB {
                return Err(LaunchError::InvalidMoeCache(self.moe_cache_mib));
            }
            // Without fitting nothing would make room for the cache.
            if !fit || !f.supports("--moe-cache-mib") {
                return Err(unsupported("the MoE expert cache"));
            }
            args.push("--moe-cache-mib".into());
            args.push(self.moe_cache_mib.to_string().into());
        }
        match &self.speculative {
            Speculative::Off => {}
            _ if self.embeddings => {}
            Speculative::Ngram => {
                if !f.supports("--spec-type") {
                    return Err(unsupported("draft-free faster generation"));
                }
                args.push("--spec-type".into());
                args.push(NGRAM_SPEC_TYPE.into());
            }
            Speculative::Mtp => {
                if !f.supports("--spec-type") {
                    return Err(unsupported("built-in multi-token prediction"));
                }
                args.push("--spec-type".into());
                args.push("draft-mtp".into());
            }
            Speculative::Draft(draft) => {
                if !(f.supports("--spec-type") && f.supports("--spec-draft-model")) {
                    return Err(unsupported("draft models"));
                }
                args.push("--spec-type".into());
                args.push("draft-simple".into());
                args.push("--spec-draft-model".into());
                args.push(draft.as_os_str().to_owned());
                // The draft's cache follows the main model's choice.
                if self.kv_cache == KvCacheType::Q8_0
                    && f.supports("--spec-draft-type-k")
                    && f.supports("--spec-draft-type-v")
                {
                    for flag in ["--spec-draft-type-k", "--spec-draft-type-v"] {
                        args.push(flag.into());
                        args.push("q8_0".into());
                    }
                }
            }
        }
        if let Some(template) = &self.chat_template_file {
            if !f.supports("--chat-template-file") {
                return Err(unsupported("a replacement chat template"));
            }
            args.push("--chat-template-file".into());
            args.push(template.as_os_str().to_owned());
        }
        Ok(args)
    }

    /// The complete environment for the child (the parent's is not inherited).
    pub fn env(&self, key: &ApiKey) -> Result<Vec<(OsString, OsString)>, LaunchError> {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        // Windows needs SystemRoot for Winsock and system DLL resolution.
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            env.push(("SystemRoot".into(), root));
        }
        for (name, value) in &self.extra_env {
            if !ALLOWED_EXTRA_ENV.contains(&name.as_str()) {
                return Err(LaunchError::DisallowedEnv(name.clone()));
            }
            env.push((name.into(), value.clone()));
        }
        env.push(("LLAMA_API_KEY".into(), key.expose().into()));
        Ok(env)
    }
}

/// Parse the port from llama-server's startup line, e.g.
/// `... srv  server_on_li: listening on http://127.0.0.1:51194`.
/// Only a loopback URL on our host with a valid non-zero port is accepted.
/// The line comes from an untrusted process and is parsed strictly.
pub fn parse_listening_port(line: &str) -> Option<u16> {
    let marker = format!("listening on http://{LOOPBACK_HOST}:");
    let rest = &line[line.find(&marker)? + marker.len()..];
    let digits: &str = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    let trailing = &rest[digits.len()..];
    if !(trailing.is_empty() || trailing.starts_with(char::is_whitespace)) {
        return None;
    }
    match digits.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(port) => Some(port),
    }
}

/// Lossy, bounded, control-character-free rendering of an untrusted log line
/// for diagnostics.
pub fn sanitize_log_line(line: &str, max_chars: usize) -> String {
    line.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            server_exe: "llama-server".into(),
            model: "model.gguf".into(),
            model_sha256: None,
            chat_template_file: None,
            context_size: Some(8192),
            gpu: GpuOffload::All,
            kv_cache: KvCacheType::F16,
            work_dir: "work".into(),
            extra_env: vec![],
            features: RuntimeFeatures::all_known(),
            embeddings: false,
            mmproj: None,
            speculative: Speculative::Off,
            moe_cache_mib: 0,
            model_parts: vec![],
            min_fit_context: None,
            parallel: 1,
        }
    }

    #[test]
    fn embedding_and_vision_modes_add_their_flags() {
        let mut s = spec();
        s.embeddings = true;
        s.mmproj = Some("proj.gguf".into());
        let args = strings(&s.args().unwrap());
        assert!(args.contains(&"--embedding".to_string()));
        let ub = args.iter().position(|a| a == "--ubatch-size").unwrap();
        assert_eq!(args[ub + 1], "2048");
        let mm = args.iter().position(|a| a == "--mmproj").unwrap();
        assert_eq!(args[mm + 1], "proj.gguf");
        s.features = features_without(&["--mmproj"]);
        assert!(s.args().is_err());
    }

    fn features_without(drop: &[&str]) -> RuntimeFeatures {
        RuntimeFeatures::from_known_flags(
            RuntimeFeatures::all_known()
                .known_flags()
                .into_iter()
                .filter(|f| !drop.contains(&f.as_str())),
        )
    }

    #[test]
    fn runtimes_without_security_flags_are_refused() {
        let mut s = spec();
        s.features = features_without(&["--offline"]);
        assert_eq!(
            s.args(),
            Err(LaunchError::Compat(CompatError::Missing(vec!["--offline"])))
        );
    }

    #[test]
    fn parallel_slots_share_one_context_memory() {
        let mut s = spec();
        let one = strings(&s.args().unwrap());
        let at = one.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(one[at + 1], "1");
        assert!(!one.contains(&"--kv-unified".to_string()));
        s.parallel = 3;
        let three = strings(&s.args().unwrap());
        let at = three.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(three[at + 1], "3");
        assert!(three.contains(&"--kv-unified".to_string()));
        s.parallel = 99;
        let capped = strings(&s.args().unwrap());
        let at = capped.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(capped[at + 1], MAX_PARALLEL.to_string());
        // A runtime that can't serve several requests refuses rather than
        // silently serving one.
        s.features = features_without(&["--parallel"]);
        assert!(s.args().is_err());
    }

    #[test]
    fn older_runtimes_get_fallbacks_for_optional_flags() {
        let mut s = spec();
        s.context_size = None;
        s.gpu = GpuOffload::Auto;
        s.features = features_without(&[
            "--fit",
            "--fit-target",
            "--fit-ctx",
            "--no-webui-mcp-proxy",
            "--parallel",
        ]);
        let args = strings(&s.args().unwrap());
        assert!(!args.iter().any(|a| a.starts_with("--fit")));
        assert!(!args.contains(&"--no-webui-mcp-proxy".to_string()));
        let ctx = args.iter().position(|a| a == "--ctx-size").unwrap();
        assert_eq!(args[ctx + 1], CONTEXT_WITHOUT_FIT.to_string());
        let ngl = args.iter().position(|a| a == "--gpu-layers").unwrap();
        assert_eq!(args[ngl + 1], "all");
        // Explicit choices the runtime can't honour are errors, not ignored.
        s.kv_cache = KvCacheType::Q8_0;
        s.features = features_without(&["--cache-type-v"]);
        assert!(matches!(
            s.args(),
            Err(LaunchError::Compat(CompatError::Unsupported(_)))
        ));
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn moe_models_keep_a_larger_automatic_context() {
        let mut s = spec();
        s.context_size = None;
        s.min_fit_context = Some(MIN_FIT_CONTEXT_MOE);
        let args = strings(&s.args().unwrap());
        let min = args.iter().position(|a| a == "--fit-ctx").unwrap();
        assert_eq!(args[min + 1], MIN_FIT_CONTEXT_MOE.to_string());
        // A fixed context is passed as is, without a minimum.
        s.context_size = Some(8192);
        let args = strings(&s.args().unwrap());
        assert!(!args.contains(&"--fit-ctx".to_string()));
    }

    #[test]
    fn expert_cache_grows_the_fit_margin() {
        let mut s = spec();
        let args = strings(&s.args().unwrap());
        let t = args.iter().position(|a| a == "--fit-target").unwrap();
        assert_eq!(args[t + 1], FIT_MARGIN_MIB.to_string());
        assert!(!args.contains(&"--moe-cache-mib".to_string()));

        s.moe_cache_mib = 4096;
        let args = strings(&s.args().unwrap());
        let t = args.iter().position(|a| a == "--fit-target").unwrap();
        assert_eq!(args[t + 1], (FIT_MARGIN_MIB + 4096).to_string());
        let c = args.iter().position(|a| a == "--moe-cache-mib").unwrap();
        assert_eq!(args[c + 1], "4096");

        s.moe_cache_mib = MAX_MOE_CACHE_MIB + 1;
        assert!(s.args().is_err());
        // No cache without fitting, or on a runtime without the flag.
        s.moe_cache_mib = 1024;
        s.features = features_without(&["--fit"]);
        assert!(s.args().is_err());
        s.features = features_without(&["--moe-cache-mib"]);
        assert!(s.args().is_err());
    }

    #[test]
    fn speculative_decoding_flags() {
        let mut s = spec();
        assert!(!strings(&s.args().unwrap()).contains(&"--spec-type".to_string()));

        s.speculative = Speculative::Ngram;
        let args = strings(&s.args().unwrap());
        let t = args.iter().position(|a| a == "--spec-type").unwrap();
        assert_eq!(args[t + 1], NGRAM_SPEC_TYPE);
        assert!(!args.contains(&"--spec-draft-model".to_string()));

        s.speculative = Speculative::Draft("draft.gguf".into());
        s.kv_cache = KvCacheType::Q8_0;
        let args = strings(&s.args().unwrap());
        let t = args.iter().position(|a| a == "--spec-type").unwrap();
        assert_eq!(args[t + 1], "draft-simple");
        let d = args.iter().position(|a| a == "--spec-draft-model").unwrap();
        assert_eq!(args[d + 1], "draft.gguf");
        assert!(args.contains(&"--spec-draft-type-k".to_string()));

        s.speculative = Speculative::Mtp;
        let args = strings(&s.args().unwrap());
        let t = args.iter().position(|a| a == "--spec-type").unwrap();
        assert_eq!(args[t + 1], "draft-mtp");
        s.speculative = Speculative::Draft("draft.gguf".into());

        // An embedding server never speculates.
        s.embeddings = true;
        assert!(!strings(&s.args().unwrap()).contains(&"--spec-type".to_string()));
        s.embeddings = false;

        // A runtime without the flags refuses rather than ignoring the choice.
        s.features = features_without(&["--spec-draft-model"]);
        assert!(s.args().is_err());
        s.speculative = Speculative::Ngram;
        s.features = features_without(&["--spec-type"]);
        assert!(s.args().is_err());
    }

    #[test]
    fn hardening_flags_are_always_present() {
        let args = strings(&spec().args().unwrap());
        for flag in [
            "--no-webui",
            "--no-webui-mcp-proxy",
            "--offline",
            "--no-slots",
            "--jinja",
        ] {
            assert!(args.contains(&flag.to_string()), "missing {flag}");
        }
        let host = args.iter().position(|a| a == "--host").unwrap();
        assert_eq!(args[host + 1], "127.0.0.1");
        let port = args.iter().position(|a| a == "--port").unwrap();
        assert_eq!(args[port + 1], "0");
        for forbidden in [
            "-hf",
            "--hf-repo",
            "--model-url",
            "-mu",
            "--mmproj-url",
            "--spec-draft-hf",
            "-hfd",
            "--hf-repo-draft",
            "--path",
            "--api-key",
        ] {
            assert!(
                !args.contains(&forbidden.to_string()),
                "{forbidden} present"
            );
        }
    }

    #[test]
    fn api_key_is_in_env_not_args_and_redacted() {
        let key = ApiKey::generate().unwrap();
        assert_eq!(key.expose().len(), 64);
        let args = strings(&spec().args().unwrap());
        assert!(!args.iter().any(|a| a.contains(key.expose())));
        let env = spec().env(&key).unwrap();
        assert!(
            env.iter()
                .any(|(k, v)| k == "LLAMA_API_KEY" && v == key.expose())
        );
        assert!(!format!("{key:?}").contains(key.expose()));
        assert_ne!(ApiKey::generate().unwrap(), key);
    }

    #[test]
    fn environment_is_allowlisted() {
        let mut s = spec();
        s.extra_env = vec![("CUDA_VISIBLE_DEVICES".into(), "0".into())];
        assert!(s.env(&ApiKey::generate().unwrap()).is_ok());
        for bad in [
            "LLAMA_ARG_MODEL_URL",
            "HF_TOKEN",
            "HTTPS_PROXY",
            "PATH",
            "LLAMA_API_KEY",
        ] {
            s.extra_env = vec![(bad.into(), "x".into())];
            assert_eq!(
                s.env(&ApiKey::generate().unwrap()),
                Err(LaunchError::DisallowedEnv(bad.into()))
            );
        }
    }

    #[test]
    fn context_size_and_gpu_options() {
        let mut s = spec();
        s.context_size = Some(100);
        assert_eq!(s.args(), Err(LaunchError::InvalidContextSize(100)));
        s.context_size = None;
        s.gpu = GpuOffload::Layers(12);
        let args = strings(&s.args().unwrap());
        assert!(!args.contains(&"--ctx-size".to_string()));
        let i = args.iter().position(|a| a == "--gpu-layers").unwrap();
        assert_eq!(args[i + 1], "12");
        s.chat_template_file = Some("t.jinja".into());
        assert!(strings(&s.args().unwrap()).contains(&"--chat-template-file".to_string()));
    }

    #[test]
    fn auto_fit_leaves_context_and_layers_to_the_runtime() {
        let mut s = spec();
        s.context_size = None;
        s.gpu = GpuOffload::Auto;
        let args = strings(&s.args().unwrap());
        let fit = args.iter().position(|a| a == "--fit").unwrap();
        assert_eq!(args[fit + 1], "on");
        let margin = args.iter().position(|a| a == "--fit-target").unwrap();
        assert_eq!(args[margin + 1], FIT_MARGIN_MIB.to_string());
        let min = args.iter().position(|a| a == "--fit-ctx").unwrap();
        assert_eq!(args[min + 1], MIN_FIT_CONTEXT.to_string());
        assert!(!args.contains(&"--ctx-size".to_string()));
        assert!(!args.contains(&"--gpu-layers".to_string()));
        assert!(!args.contains(&"--cache-type-k".to_string()));

        s.kv_cache = KvCacheType::Q8_0;
        let args = strings(&s.args().unwrap());
        for flag in ["--cache-type-k", "--cache-type-v"] {
            let i = args.iter().position(|a| a == flag).unwrap();
            assert_eq!(args[i + 1], "q8_0");
        }
    }

    #[test]
    fn listening_port_is_parsed_strictly() {
        let ok = "0.03.314.543 I srv  server_on_li: listening on http://127.0.0.1:51194";
        assert_eq!(parse_listening_port(ok), Some(51194));
        assert_eq!(parse_listening_port(&format!("{ok} \r")), Some(51194));
        for bad in [
            "listening on http://0.0.0.0:8080",
            "listening on http://127.0.0.1:0",
            "listening on http://127.0.0.1:70000",
            "listening on http://127.0.0.1:",
            "listening on http://127.0.0.1:80abc",
            "listening on http://127.0.0.10:80",
            "listening on http://127.0.0.1.evil:80",
            "nothing here",
        ] {
            assert_eq!(parse_listening_port(bad), None, "{bad}");
        }
    }

    #[test]
    fn log_lines_are_sanitized() {
        assert_eq!(sanitize_log_line("a\x1b[31mb\u{7}c", 100), "a[31mbc");
        assert_eq!(sanitize_log_line("abcdef", 3), "abc");
    }
}
