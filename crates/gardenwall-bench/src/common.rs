//! What every suite shares: the machine configuration, starting the model
//! server exactly as the app does (optionally in its sandbox), the
//! environment fingerprint, GPU health checks, statistics and result files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gardenwall_inference::client::InferenceClient;
use gardenwall_inference::launch::{ApiKey, GpuOffload, KvCacheType, LaunchSpec, Speculative};
use gardenwall_inference::supervisor::{self, Isolation, RunningServer, Timeouts};
use serde::{Deserialize, Serialize};

/// Results format version (all suites).
pub const SCHEMA: u32 = 2;

/// The error a suite returns when the STOP file asked it to pause.
pub const PAUSED: &str = "paused by the STOP file";

static STOP_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where the pause request is looked for (`<out>/STOP`).
pub fn set_stop_file(path: PathBuf) {
    let _ = STOP_FILE.set(path);
}

/// Whether someone asked the run to pause: suites check between runs.
pub fn stop_requested() -> bool {
    STOP_FILE.get().is_some_and(|p| p.exists())
}

/// Paths on this machine (`bench/machine.json`, git-ignored; see
/// `bench/machine.example.json`). Paths never go into result files.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Machine {
    /// The pinned `llama-server`.
    pub server: PathBuf,
    /// M1: the daily model (primary target).
    pub main_model: PathBuf,
    /// M2: a small model for quick runs.
    pub small_model: Option<PathBuf>,
    /// M3: a Mixture-of-Experts model (first part of a split model).
    pub moe_model: Option<PathBuf>,
    /// The main model's image projector, for the vision suite.
    pub main_mmproj: Option<PathBuf>,
    /// An embedding model, for hybrid document search.
    pub embed_model: Option<PathBuf>,
    /// The stock Pi command template, for the `pi` profile.
    pub pi: Option<String>,
}

impl Machine {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "{}: {e} (copy bench/machine.example.json to bench/machine.json and fill in the paths)",
                path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn model(&self, which: &str) -> Result<PathBuf, String> {
        match which {
            "main" => Ok(self.main_model.clone()),
            "small" => self
                .small_model
                .clone()
                .ok_or("machine.json has no smallModel".into()),
            "moe" => self
                .moe_model
                .clone()
                .ok_or("machine.json has no moeModel".into()),
            other => Ok(PathBuf::from(other)),
        }
    }
}

/// How the model server is started for a run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(skip)]
    pub server: PathBuf,
    #[serde(skip)]
    pub model: PathBuf,
    #[serde(skip)]
    pub mmproj: Option<PathBuf>,
    pub context: Option<u32>,
    pub kv: Kv,
    pub speculative: Spec,
    pub parallel: u32,
    pub moe_cache_mib: u32,
    pub embeddings: bool,
    /// In the app's AppContainer, reached through the relay (Windows).
    pub sandboxed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kv {
    F16,
    Q8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spec {
    Off,
    Ngram,
    Mtp,
}

impl ServerConfig {
    pub fn new(server: &Path, model: &Path) -> Self {
        Self {
            server: server.to_path_buf(),
            model: model.to_path_buf(),
            mmproj: None,
            context: None,
            kv: Kv::F16,
            speculative: Spec::Off,
            parallel: 1,
            moe_cache_mib: 0,
            embeddings: false,
            sandboxed: false,
        }
    }

    /// A short label for tables ("ctx32768 kv=f16 spec=off x1").
    pub fn label(&self) -> String {
        format!(
            "ctx={} kv={:?} spec={:?} slots={}{}{}",
            self.context.map_or("auto".into(), |c| c.to_string()),
            self.kv,
            self.speculative,
            self.parallel,
            if self.moe_cache_mib > 0 {
                format!(" cache={}MiB", self.moe_cache_mib)
            } else {
                String::new()
            },
            if self.sandboxed { " sandboxed" } else { "" }
        )
        .to_ascii_lowercase()
    }
}

/// A started server and what the suites need from it.
pub struct Started {
    pub server: RunningServer,
    pub client: InferenceClient,
    pub context_window: u64,
    pub build: Option<String>,
    /// `http://127.0.0.1:port` when unsandboxed (the Pi arm needs it).
    pub url: Option<String>,
    pub secret: String,
    pub load_ms: u64,
    pub model_name: String,
    /// Kept until the server stops (its work and relay folders).
    pub _dir: TempDir,
}

/// A folder removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Result<Self, String> {
        let mut raw = [0u8; 6];
        getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
        let tag: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("gardenwall-bench-{label}-{tag}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The SHA-256 a model is stored under in the app's library (its file
/// name, or its folder for a split model), needed for the sandboxed
/// launch's per-model identity.
fn model_sha(model: &Path) -> Option<String> {
    let is_hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let stem = model.file_stem()?.to_str()?;
    if is_hash(stem) {
        return Some(stem.to_lowercase());
    }
    let folder = model.parent()?.file_name()?.to_str()?;
    is_hash(folder).then(|| folder.to_lowercase())
}

/// Copy a helper built next to this program (`gardenwall-relay`,
/// `gardenwall-parse`) into `dir`, as the app stages its helpers.
pub fn stage_helper(dir: &Path, base: &str) -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    };
    let source = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(|p| p.join(&name))
        .ok_or("no program folder")?;
    if !source.is_file() {
        return Err(format!(
            "{name} isn't built next to the benchmark (cargo build --release -p {base})"
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let target = dir.join(&name);
    std::fs::copy(&source, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

pub async fn start_server(cfg: &ServerConfig) -> Result<Started, String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
    let secret: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let key = ApiKey::from_secret(secret.clone()).ok_or("key")?;
    let dir = TempDir::new("server")?;
    let work_dir = dir.0.join("work");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let isolation = if cfg.sandboxed {
        sandboxed_isolation(&dir.0)?
    } else {
        Isolation::Unsandboxed
    };
    let features = supervisor::probe_features(&cfg.server, &isolation)
        .await
        .map_err(|e| format!("couldn't read the server's options: {e}"))?;
    let parts = split_parts(&cfg.model);
    let spec = LaunchSpec {
        server_exe: cfg.server.clone(),
        model: cfg.model.clone(),
        model_parts: parts.clone(),
        model_sha256: model_sha(&cfg.model),
        chat_template_file: None,
        context_size: cfg.context,
        gpu: GpuOffload::Auto,
        kv_cache: match cfg.kv {
            Kv::F16 => KvCacheType::F16,
            Kv::Q8 => KvCacheType::Q8_0,
        },
        work_dir,
        extra_env: vec![],
        features,
        embeddings: cfg.embeddings,
        mmproj: cfg.mmproj.clone(),
        speculative: match cfg.speculative {
            Spec::Off => Speculative::Off,
            Spec::Ngram => Speculative::Ngram,
            Spec::Mtp => Speculative::Mtp,
        },
        moe_cache_mib: cfg.moe_cache_mib,
        // As the app does: at least 32K for Mixture-of-Experts models (split
        // models here) and the recommended 16K for others (embedding models
        // keep the launcher's default).
        min_fit_context: (!cfg.embeddings).then_some(if parts.is_empty() {
            gardenwall_inference::launch::RECOMMENDED_MIN_CONTEXT
        } else {
            gardenwall_inference::launch::MIN_FIT_CONTEXT_MOE
        }),
        parallel: cfg.parallel,
    };
    if cfg.sandboxed && spec.model_sha256.is_none() {
        return Err(
            "a sandboxed launch needs the model from the app's library (its file name is its SHA-256)"
                .into(),
        );
    }
    let started = Instant::now();
    let server = supervisor::launch_with_key(&spec, &isolation, Timeouts::default(), key)
        .await
        .map_err(|e| format!("the model server didn't start ({}): {e}", cfg.label()))?;
    let load_ms = started.elapsed().as_millis() as u64;
    let client = server.client().clone();
    let props = client.props().await.map_err(|e| e.to_string())?;
    let url = match client.connector() {
        gardenwall_inference::transport::Connector::Loopback(addr) => {
            Some(format!("http://{addr}"))
        }
        _ => None,
    };
    Ok(Started {
        server,
        client,
        context_window: props.context_window,
        build: props.build,
        url,
        secret,
        load_ms,
        model_name: model_name(&cfg.model),
        _dir: dir,
    })
}

#[cfg(windows)]
fn sandboxed_isolation(dir: &Path) -> Result<Isolation, String> {
    let journal = dir.join("grants");
    std::fs::create_dir_all(&journal).map_err(|e| e.to_string())?;
    Ok(Isolation::Sandboxed {
        relay_exe: stage_helper(&dir.join("relay"), "gardenwall-relay")?,
        grant_journal_dir: Some(journal),
    })
}

#[cfg(not(windows))]
fn sandboxed_isolation(_dir: &Path) -> Result<Isolation, String> {
    Err("the sandboxed model server is Windows-only for now".into())
}

/// The other parts of a split model (`…-00001-of-00003.gguf`).
fn split_parts(model: &Path) -> Vec<PathBuf> {
    let Some(name) = model.file_name().and_then(|n| n.to_str()) else {
        return vec![];
    };
    let Some(stem) = name.strip_suffix(".gguf") else {
        return vec![];
    };
    let bits: Vec<&str> = stem.rsplitn(4, '-').collect();
    // [total, "of", index, prefix]
    if bits.len() == 4 && bits[1] == "of" && bits[2] == "00001" {
        let total: usize = bits[0].parse().unwrap_or(1);
        return (2..=total)
            .map(|i| model.with_file_name(format!("{}-{i:05}-of-{}.gguf", bits[3], bits[0])))
            .collect();
    }
    vec![]
}

/// The model's file name (an app library file is named by its hash, so
/// no personal path goes into results).
pub fn model_name(model: &Path) -> String {
    model
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------- environment fingerprint ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    pub os: String,
    pub cpu: Option<String>,
    pub ram_gib: Option<f64>,
    pub gpu: Option<String>,
    pub gpu_driver: Option<String>,
    pub vram_gib: Option<f64>,
    pub vram_free_gib_at_start: Option<f64>,
    pub pwsh: Option<String>,
    pub bench_version: String,
    /// The protected-path rules in force (ADR-011 §2.3): a change here can
    /// change agent results without being a regression. Missing in older
    /// results.
    #[serde(default)]
    pub path_rules_version: Option<u32>,
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new(program);
    c.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn fingerprint() -> Fingerprint {
    let gib = |b: u64| (b as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0;
    let mut f = Fingerprint {
        git_commit: command_output("git", &["rev-parse", "HEAD"]),
        git_dirty: command_output("git", &["status", "--porcelain", "--untracked-files=no"])
            .map(|s| !s.is_empty())
            .or(Some(false))
            .filter(|_| command_output("git", &["rev-parse", "HEAD"]).is_some()),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        bench_version: env!("CARGO_PKG_VERSION").into(),
        path_rules_version: Some(gardenwall_policy::paths::PATH_RULES_VERSION),
        ..Default::default()
    };
    if let Ok(r) = gardenwall_gpu::report() {
        f.gpu = Some(r.adapter.clone());
        f.vram_gib = Some(gib(r.dedicated_total));
        f.vram_free_gib_at_start = r.free().map(gib);
    }
    if let Ok((total, _)) = gardenwall_gpu::system_memory() {
        f.ram_gib = Some(gib(total));
    }
    f.gpu_driver = command_output(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
    );
    if cfg!(windows) {
        if let Some(info) = command_output(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name; [Environment]::OSVersion.Version.ToString()",
            ],
        ) {
            let mut lines = info.lines();
            f.cpu = lines.next().map(|s| s.trim().to_string());
            if let Some(v) = lines.next() {
                f.os = format!("Windows {}", v.trim());
            }
        }
        f.pwsh = find_powershell7().and_then(|p| {
            command_output(
                &p.to_string_lossy(),
                &[
                    "-NoProfile",
                    "-Command",
                    "$PSVersionTable.PSVersion.ToString()",
                ],
            )
        });
    } else if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        f.cpu = text
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().to_string());
    }
    f
}

// ---------- GPU health ----------

/// The GPU's state around a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub vram_free_gib: Option<f64>,
    /// GPU memory was (nearly) full: driver paging slows everything down.
    pub pressure: bool,
    pub temperature_c: Option<u32>,
    pub sm_clock_mhz: Option<u32>,
    /// Non-idle throttle reasons reported by the driver (hex mask).
    pub throttle: Option<String>,
}

impl Health {
    /// The run's numbers aren't comparable (paging or thermal/power
    /// throttling).
    pub fn degraded(&self) -> bool {
        self.pressure
            || self
                .throttle
                .as_deref()
                .and_then(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok())
                // Hardware slowdown (0x8), software and hardware thermal
                // (0x20, 0x40) and power brake (0x80). The power cap (0x4)
                // is normal at full load and doesn't count.
                .is_some_and(|m| m & 0xE8 != 0)
    }
}

pub fn health() -> Health {
    let mut h = Health::default();
    if let Ok(r) = gardenwall_gpu::report() {
        h.vram_free_gib = r
            .free()
            .map(|b| (b as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0);
        h.pressure = r.under_pressure();
    }
    if let Some(line) = command_output(
        "nvidia-smi",
        &[
            "--query-gpu=temperature.gpu,clocks.sm,clocks_throttle_reasons.active",
            "--format=csv,noheader,nounits",
        ],
    ) {
        let v: Vec<&str> = line.split(',').map(str::trim).collect();
        h.temperature_c = v.first().and_then(|s| s.parse().ok());
        h.sm_clock_mhz = v.get(1).and_then(|s| s.parse().ok());
        h.throttle = v.get(2).map(|s| s.to_string());
    }
    h
}

/// Refuse to start while other programs hold GPU memory (Gardenwall's own
/// model, games…), unless forced. Returns a warning line when forced.
pub fn gpu_is_free(force: bool) -> Result<Option<String>, String> {
    let Ok(r) = gardenwall_gpu::report() else {
        return Ok(None);
    };
    let busy: Vec<String> = r
        .processes
        .iter()
        .filter(|p| !p.is_system() && p.dedicated > (1u64 << 30))
        .map(|p| {
            format!(
                "{} ({:.1} GiB)",
                p.name,
                p.dedicated as f64 / (1u64 << 30) as f64
            )
        })
        .collect();
    if busy.is_empty() {
        return Ok(None);
    }
    let msg = format!(
        "other programs are using GPU memory: {}. Unload Gardenwall's model and close them, or pass --force",
        busy.join(", ")
    );
    if force { Ok(Some(msg)) } else { Err(msg) }
}

/// GPU memory used by one process (the model server), in GiB.
pub fn process_vram_gib(pid: Option<u32>) -> Option<f64> {
    let r = gardenwall_gpu::report().ok()?;
    let p = r.process(pid?)?;
    Some((p.dedicated as f64 / (1u64 << 30) as f64 * 100.0).round() / 100.0)
}

// ---------- statistics ----------

/// Median, quartiles and p95 of a sample.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Spread {
    pub n: usize,
    pub median: f64,
    pub p25: f64,
    pub p75: f64,
    pub p95: f64,
}

pub fn spread(values: &[f64]) -> Option<Spread> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| {
        let pos = p * (v.len() - 1) as f64;
        let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
        v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
    };
    Some(Spread {
        n: v.len(),
        median: q(0.5),
        p25: q(0.25),
        p75: q(0.75),
        p95: q(0.95),
    })
}

/// Time a closure `n` times (after one warm-up), in milliseconds.
pub fn time_n<T>(n: usize, mut f: impl FnMut() -> T) -> Vec<f64> {
    let _ = f();
    (0..n)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The UTC date for result folders (YYYY-MM-DD).
pub fn today() -> String {
    let days = (now_unix() / 86_400) as i64;
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

/// The header of every result file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub schema: u32,
    pub suite: String,
    pub created_unix: u64,
    pub seed: u64,
    pub fingerprint: Fingerprint,
}

impl Header {
    pub fn new(suite: &str, seed: u64, fingerprint: &Fingerprint) -> Self {
        Self {
            schema: SCHEMA,
            suite: suite.into(),
            created_unix: now_unix(),
            seed,
            fingerprint: fingerprint.clone(),
        }
    }
}

/// Write a result file to `<out>/<date>/<suite>-<time>[-tag].json`.
pub fn write_result(
    out: &Path,
    header: &Header,
    tag: &str,
    body: &impl Serialize,
) -> Result<PathBuf, String> {
    let mut value = serde_json::to_value(body).map_err(|e| e.to_string())?;
    let head = serde_json::to_value(header).map_err(|e| e.to_string())?;
    if let (Some(obj), Some(h)) = (value.as_object_mut(), head.as_object()) {
        for (k, v) in h {
            obj.insert(k.clone(), v.clone());
        }
    }
    let dir = out.join(today());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = if tag.is_empty() {
        format!("{}-{}.json", header.suite, header.created_unix)
    } else {
        format!("{}-{}-{tag}.json", header.suite, header.created_unix)
    };
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(path)
}

/// A deterministic shuffle (xorshift), so run orders are reproducible.
pub fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in (1..items.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        items.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// PowerShell 7, where the agent's sandboxed shell exists.
#[cfg(any(windows, target_os = "linux"))]
fn find_powershell7() -> Option<std::path::PathBuf> {
    gardenwall_agent::sandbox_shell::find_powershell7()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn find_powershell7() -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_and_dates() {
        let s = spread(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!((s.median, s.p25, s.p75), (3.0, 2.0, 4.0));
        assert!(spread(&[]).is_none());
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }

    #[test]
    fn split_parts_and_hashes() {
        let p = Path::new("/m/Model-IQ2-00001-of-00003.gguf");
        let parts = split_parts(p);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].ends_with("Model-IQ2-00003-of-00003.gguf"));
        assert!(split_parts(Path::new("/m/one.gguf")).is_empty());
        let h = "a".repeat(64);
        assert_eq!(model_sha(Path::new(&format!("/m/{h}.gguf"))), Some(h));
        assert_eq!(model_sha(Path::new("/m/one.gguf")), None);
        assert_eq!(
            model_sha(Path::new(&format!(
                "/m/{}/Part-00001-of-00002.gguf",
                "B".repeat(64)
            ))),
            Some("b".repeat(64))
        );
    }

    #[test]
    fn shuffle_is_reproducible_and_complete() {
        let mut a: Vec<u32> = (0..50).collect();
        let mut b = a.clone();
        shuffle(&mut a, 9);
        shuffle(&mut b, 9);
        assert_eq!(a, b);
        assert_ne!(a, (0..50).collect::<Vec<_>>());
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }
}
