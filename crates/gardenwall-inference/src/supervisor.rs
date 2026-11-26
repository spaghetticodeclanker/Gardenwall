//! Lifecycle supervision for the `llama-server` sidecar.
//!
//! Two isolation modes:
//! - [`Isolation::Sandboxed`] (Windows): the runtime and the Gardenwall relay
//!   run in a per-launch AppContainer with a Job Object and no network
//!   (ADR-012 §4.1). The trusted core reaches the runtime only through a
//!   container-only named pipe whose client must be the spawned relay.
//! - [`Isolation::Unsandboxed`]: development only. The runtime runs as a
//!   normal child on loopback. [`RunningServer::isolation`] reports this so
//!   the UI can show that isolation is not in effect (ADR-002).
//!
//! In both modes the runtime gets the hardened arguments and cleared
//! environment from [`crate::launch`], discovers its own port, has its logs
//! drained and bounded, and is killed when the [`RunningServer`] is dropped.

use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::client::InferenceClient;
use crate::compat::{CompatError, RuntimeFeatures};
use crate::launch::{self, ApiKey, LaunchError, LaunchSpec, Speculative};
use crate::placement::Placement;
use crate::transport::Connector;

const LOG_LINES_KEPT: usize = 200;
const LOG_LINE_MAX_CHARS: usize = 512;
/// Upper bound on a single raw log line read from the child.
const LOG_LINE_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub enum SupervisorError {
    Launch(LaunchError),
    MissingExecutable,
    InvalidPath(PathBuf),
    /// Sandboxed launch needs the model's SHA-256 (64 hex), consistent with
    /// a digest-named model file.
    InvalidModelId,
    /// The same model is already running (it would share a sandbox identity).
    ModelAlreadyRunning,
    Spawn(std::io::Error),
    Random,
    /// The child exited before becoming ready.
    Exited(Option<i32>),
    /// The runtime doesn't know the model's architecture (named, from its
    /// log): the model is likely newer than the installed llama.cpp build.
    UnsupportedModel(String),
    /// The child did not report a listening port in time.
    StartupTimeout,
    /// The child listened but did not become healthy in time.
    LoadTimeout,
    Client(crate::client::ClientError),
    /// The runtime's option list could not be read or is incompatible.
    Compat(CompatError),
    #[cfg(windows)]
    Sandbox(gardenwall_sandbox::SandboxError),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch(e) => write!(f, "invalid launch configuration: {e}"),
            Self::MissingExecutable => f.write_str("inference runtime executable not found"),
            Self::InvalidPath(p) => write!(f, "path must be absolute and exist: {}", p.display()),
            Self::InvalidModelId => f.write_str("missing or inconsistent model SHA-256"),
            Self::ModelAlreadyRunning => f.write_str("this model is already running"),
            Self::Spawn(e) => write!(f, "failed to start inference runtime: {e}"),
            Self::Random => f.write_str("failed to generate API key"),
            Self::Exited(code) => write!(f, "inference runtime exited during startup ({code:?})"),
            Self::UnsupportedModel(arch) => write!(
                f,
                "this model may need a newer runtime: the installed llama.cpp build doesn't know its architecture ({arch}). Newer models often need a newer build; see the Runtime page"
            ),
            Self::StartupTimeout => f.write_str("inference runtime did not start in time"),
            Self::LoadTimeout => f.write_str("model did not finish loading in time"),
            Self::Client(e) => write!(f, "{e}"),
            Self::Compat(e) => write!(f, "{e}"),
            #[cfg(windows)]
            Self::Sandbox(e) => write!(f, "inference sandbox: {e}"),
        }
    }
}

impl std::error::Error for SupervisorError {}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Time allowed for the child to bind its port.
    pub startup: Duration,
    /// Time allowed for the model to load after binding.
    pub load: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(60),
            load: Duration::from_secs(600),
        }
    }
}

/// How the runtime is isolated.
#[derive(Debug, Clone)]
pub enum Isolation {
    /// Development only: no OS sandbox. Reported as not isolated.
    Unsandboxed,
    /// AppContainer + Job Object, reached through the relay (Windows).
    #[cfg(windows)]
    Sandboxed {
        /// Verified `gardenwall-relay.exe`.
        relay_exe: PathBuf,
        /// Directory for the sandbox's grant journal, reconciled at startup
        /// with `gardenwall_sandbox::reconcile_grant_journals`.
        grant_journal_dir: Option<PathBuf>,
    },
}

/// What isolation a running server actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationStatus {
    Sandboxed,
    Unsandboxed,
}

/// Bounded buffer of sanitized child log lines, for diagnostics only, and
/// where the model's weights were placed (read as lines arrive, since the
/// load lines scroll out of the buffer).
#[derive(Clone, Default)]
pub struct LogTail {
    lines: Arc<Mutex<VecDeque<String>>>,
    placement: Arc<Mutex<Placement>>,
}

/// The log lines of the last runtime that stopped during startup (already
/// sanitized and bounded like every tail), kept because the server and its
/// tail are gone by then. For the user's diagnostics report only.
static LAST_FAILED_START: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The log tail of the last runtime that stopped during startup, if any.
pub fn last_failed_start_log() -> Vec<String> {
    LAST_FAILED_START
        .lock()
        .map(|l| l.clone())
        .unwrap_or_default()
}

impl LogTail {
    /// Why the runtime stopped during startup: an unknown architecture if
    /// its log says so, else the plain exit.
    fn exited(&self, code: Option<i32>) -> SupervisorError {
        let lines = self.lines();
        if let Ok(mut last) = LAST_FAILED_START.lock() {
            last.clone_from(&lines);
        }
        lines.iter().find_map(|l| unknown_architecture(l)).map_or(
            SupervisorError::Exited(code),
            SupervisorError::UnsupportedModel,
        )
    }

    fn push(&self, line: &str) {
        let line = launch::sanitize_log_line(line, LOG_LINE_MAX_CHARS);
        if let Ok(mut p) = self.placement.lock() {
            p.observe(&line);
        }
        if let Ok(mut q) = self.lines.lock() {
            if q.len() == LOG_LINES_KEPT {
                q.pop_front();
            }
            q.push_back(line);
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Where the weights went, as reported by the runtime (display only).
    pub fn placement(&self) -> Placement {
        self.placement.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

enum Inner {
    Process(Box<Plain>),
    #[cfg(windows)]
    Sandbox(Box<sandboxed::Running>),
}

/// A server started without a sandbox (development tools and tests only:
/// the app always sandboxes it).
struct Plain {
    child: Child,
    /// Kills the server if this process dies without dropping it.
    #[cfg(windows)]
    _job: Option<gardenwall_sandbox::KillOnCloseJob>,
}

/// A ready inference server. Dropping it kills the runtime (and, when
/// sandboxed, the relay) and removes the sandbox identity.
pub struct RunningServer {
    client: InferenceClient,
    logs: LogTail,
    inner: Inner,
}

impl fmt::Debug for RunningServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunningServer")
            .field("isolation", &self.isolation())
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

impl RunningServer {
    pub fn client(&self) -> &InferenceClient {
        &self.client
    }

    pub fn logs(&self) -> &LogTail {
        &self.logs
    }

    pub fn isolation(&self) -> IsolationStatus {
        match self.inner {
            Inner::Process(_) => IsolationStatus::Unsandboxed,
            #[cfg(windows)]
            Inner::Sandbox(_) => IsolationStatus::Sandboxed,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match &self.inner {
            Inner::Process(p) => p.child.id(),
            #[cfg(windows)]
            Inner::Sandbox(running) => Some(running.server.pid()),
        }
    }

    /// Stop the runtime and wait for it to exit.
    pub async fn shutdown(self) {
        match self.inner {
            Inner::Process(mut p) => {
                let _ = p.child.kill().await;
            }
            #[cfg(windows)]
            Inner::Sandbox(running) => drop(running),
        }
    }
}

/// Read lines from a child stream, bounding each line's size, forwarding
/// them to the log tail and the startup channel.
fn spawn_line_reader<R: AsyncRead + Unpin + Send + 'static>(
    stream: R,
    logs: LogTail,
    tx: mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut limited = (&mut reader).take(LOG_LINE_MAX_BYTES as u64);
            match limited.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end();
                    logs.push(line);
                    let _ = tx.send(line.to_string());
                }
            }
        }
    });
}

/// Wait for the listening line; keep draining afterwards.
async fn wait_for_port(
    mut rx: mpsc::UnboundedReceiver<String>,
    timeout: Duration,
) -> Result<Option<u16>, SupervisorError> {
    let port = tokio::time::timeout(timeout, async {
        while let Some(line) = rx.recv().await {
            if let Some(port) = launch::parse_listening_port(&line) {
                return Some(port);
            }
        }
        None
    })
    .await
    .map_err(|_| SupervisorError::StartupTimeout)?;
    // Keep draining so the readers never block.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    Ok(port)
}

/// The architecture named in llama.cpp's "unknown model architecture:
/// 'name'" log line, cleaned to a short identifier.
fn unknown_architecture(line: &str) -> Option<String> {
    let rest = line.split("unknown model architecture").nth(1)?;
    let name: String = rest
        .trim_start_matches([':', ' ', '\'', '"'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(64)
        .collect();
    Some(if name.is_empty() {
        "unknown".into()
    } else {
        name
    })
}

async fn wait_healthy(
    client: &InferenceClient,
    timeout: Duration,
    mut exited: impl FnMut() -> Option<Option<i32>>,
) -> Result<(), SupervisorError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(code) = exited() {
            return Err(SupervisorError::Exited(code));
        }
        if client.health().await.unwrap_or(false) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(SupervisorError::LoadTimeout);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn require_file(path: &Path) -> Result<(), SupervisorError> {
    if path.is_absolute() && path.is_file() {
        Ok(())
    } else {
        Err(SupervisorError::InvalidPath(path.to_path_buf()))
    }
}

/// Most bytes of `--help` output read.
const MAX_HELP_BYTES: u64 = 1 << 20;
/// Time allowed for `--help` (a CUDA build loads its libraries first).
const HELP_TIMEOUT: Duration = Duration::from_secs(30);

/// Detect which flags a runtime build accepts by running `llama-server
/// --help` with the same isolation a launch would use (ADR-017 §2.1). No
/// model is loaded. Callers cache the result per build.
pub async fn probe_features(
    server_exe: &Path,
    isolation: &Isolation,
) -> Result<RuntimeFeatures, SupervisorError> {
    if !server_exe.is_file() {
        return Err(SupervisorError::MissingExecutable);
    }
    let help = match isolation {
        Isolation::Unsandboxed => help_unsandboxed(server_exe).await?,
        #[cfg(windows)]
        Isolation::Sandboxed {
            grant_journal_dir, ..
        } => {
            let exe = server_exe.to_path_buf();
            let journal = grant_journal_dir.clone();
            tokio::task::spawn_blocking(move || sandboxed::help(&exe, journal.as_deref()))
                .await
                .map_err(|e| SupervisorError::Spawn(std::io::Error::other(e)))??
        }
    };
    RuntimeFeatures::from_help(&help).map_err(SupervisorError::Compat)
}

async fn help_unsandboxed(server_exe: &Path) -> Result<String, SupervisorError> {
    let mut command = Command::new(server_exe);
    command
        .arg("--help")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(SupervisorError::Spawn)?;
    let mut out = Vec::new();
    let stdout = child.stdout.take().ok_or(SupervisorError::Exited(None))?;
    let mut limited = stdout.take(MAX_HELP_BYTES);
    tokio::time::timeout(HELP_TIMEOUT, limited.read_to_end(&mut out))
        .await
        .map_err(|_| SupervisorError::StartupTimeout)?
        .map_err(SupervisorError::Spawn)?;
    let _ = child.kill().await;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Launch `llama-server` with the requested isolation and wait until healthy.
pub async fn launch(
    spec: &LaunchSpec,
    isolation: &Isolation,
    timeouts: Timeouts,
) -> Result<RunningServer, SupervisorError> {
    let key = ApiKey::generate().map_err(|_| SupervisorError::Random)?;
    launch_with_key(spec, isolation, timeouts, key).await
}

/// [`launch`] with a key the caller generated, for a second trusted local
/// client of the same server (the parity benchmark's Pi arm).
pub async fn launch_with_key(
    spec: &LaunchSpec,
    isolation: &Isolation,
    timeouts: Timeouts,
    key: ApiKey,
) -> Result<RunningServer, SupervisorError> {
    if !spec.server_exe.is_file() {
        return Err(SupervisorError::MissingExecutable);
    }
    require_file(&spec.model)?;
    for part in &spec.model_parts {
        require_file(part)?;
    }
    if let Some(t) = &spec.chat_template_file {
        require_file(t)?;
    }
    if let Some(m) = &spec.mmproj {
        require_file(m)?;
    }
    if let Speculative::Draft(d) = &spec.speculative {
        require_file(d)?;
    }
    match isolation {
        Isolation::Unsandboxed => launch_unsandboxed(spec, key, timeouts).await,
        #[cfg(windows)]
        Isolation::Sandboxed {
            relay_exe,
            grant_journal_dir,
        } => sandboxed::launch(spec, relay_exe, grant_journal_dir.as_deref(), key, timeouts).await,
    }
}

async fn launch_unsandboxed(
    spec: &LaunchSpec,
    key: ApiKey,
    timeouts: Timeouts,
) -> Result<RunningServer, SupervisorError> {
    let args = spec.args().map_err(SupervisorError::Launch)?;
    let env = spec.env(&key).map_err(SupervisorError::Launch)?;
    std::fs::create_dir_all(&spec.work_dir).map_err(SupervisorError::Spawn)?;

    let mut command = Command::new(&spec.server_exe);
    command
        .args(&args)
        .env_clear()
        .envs(env)
        .current_dir(&spec.work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window for the sidecar.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(SupervisorError::Spawn)?;
    // The server dies with this process even when it is killed outright
    // (drop handlers don't run then), so it never keeps holding the GPU.
    #[cfg(windows)]
    let job = match (gardenwall_sandbox::KillOnCloseJob::new(), child.id()) {
        (Ok(job), Some(pid)) => job.assign(pid).ok().map(|()| job),
        _ => None,
    };

    let logs = LogTail::default();
    let (tx, rx) = mpsc::unbounded_channel();
    if let Some(out) = child.stdout.take() {
        spawn_line_reader(out, logs.clone(), tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_line_reader(err, logs.clone(), tx.clone());
    }
    drop(tx);

    let Some(port) = wait_for_port(rx, timeouts.startup).await? else {
        let code = child.wait().await.ok().and_then(|s| s.code());
        return Err(logs.exited(code));
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let connector = Connector::loopback(addr).map_err(SupervisorError::Spawn)?;
    let client = InferenceClient::new(connector, key);
    wait_healthy(&client, timeouts.load, || match child.try_wait() {
        Ok(Some(status)) => Some(status.code()),
        _ => None,
    })
    .await
    .map_err(|e| match e {
        SupervisorError::Exited(code) => logs.exited(code),
        e => e,
    })?;

    Ok(RunningServer {
        client,
        logs,
        inner: Inner::Process(Box::new(Plain {
            child,
            #[cfg(windows)]
            _job: job,
        })),
    })
}

#[cfg(windows)]
pub use sandboxed::forget_model_sandbox;

#[cfg(windows)]
mod sandboxed {
    use std::ffi::OsStr;

    use gardenwall_sandbox::windows::delete_profile;
    use gardenwall_sandbox::{
        Access, Limits, Network, PathGrant, PipeListener, Sandbox, SandboxPolicy, SandboxedProcess,
    };

    use super::*;
    use crate::transport::BoxStream;

    /// Pre-connected relay streams kept ready for requests.
    const RELAY_CONNECTIONS: usize = 8;

    /// Profiles of models currently running. Two sessions of one model would
    /// share an identity, and revoking one session's grants would break the
    /// other, so concurrent launches of the same model are refused.
    static ACTIVE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// Removes the profile from [`ACTIVE`] when dropped.
    struct ActiveGuard(String);

    impl ActiveGuard {
        fn acquire(profile: &str) -> Result<Self, SupervisorError> {
            let mut active = ACTIVE
                .lock()
                .map_err(|_| SupervisorError::ModelAlreadyRunning)?;
            if active.iter().any(|p| p == profile) {
                return Err(SupervisorError::ModelAlreadyRunning);
            }
            active.push(profile.to_string());
            Ok(Self(profile.to_string()))
        }
    }

    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            if let Ok(mut active) = ACTIVE.lock() {
                active.retain(|p| p != &self.0);
            }
        }
    }

    /// The persistent sandbox identity for a model (ADR-012 §4.1, option 2):
    /// keyed by the model's SHA-256, so only the same model file ever sees
    /// state (e.g. the GPU shader cache) left by its previous sessions.
    pub(super) fn model_profile(sha256: &str) -> Result<String, SupervisorError> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SupervisorError::InvalidModelId);
        }
        Ok(format!(
            "Gardenwall.Inference.{}",
            sha256[..32].to_ascii_lowercase()
        ))
    }

    /// Delete a model's sandbox identity and its persisted state (call when
    /// the model is removed). Refused while that model is running.
    pub fn forget_model_sandbox(sha256: &str) -> Result<(), SupervisorError> {
        let profile = model_profile(sha256)?;
        let _guard = ActiveGuard::acquire(&profile)?;
        match delete_profile(&profile) {
            Ok(()) => Ok(()),
            // Already absent is fine.
            Err(gardenwall_sandbox::SandboxError::Os { code, .. })
                if code == 0x8007_0002 || code == 0x8007_0490 =>
            {
                Ok(())
            }
            Err(e) => Err(SupervisorError::Sandbox(e)),
        }
    }

    pub(super) struct Running {
        // Drop order: accept loop, then processes, then the sandbox (job +
        // grants), then the active-model guard.
        accept_task: tokio::task::JoinHandle<()>,
        pub(super) server: SandboxedProcess,
        _relay: SandboxedProcess,
        sandbox: Option<Sandbox>,
        _active: ActiveGuard,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.accept_task.abort();
            // Job termination is asynchronous: wait for both processes to
            // exit before revoking grants, so no process outlives them.
            if let Some(sandbox) = &self.sandbox {
                let _ = sandbox.terminate_all();
            }
            let _ = self.server.wait_timeout(Some(Duration::from_secs(5)));
            let _ = self._relay.wait_timeout(Some(Duration::from_secs(5)));
            // Revokes grants. The profile persists for this model's next run.
            drop(self.sandbox.take());
        }
    }

    /// Strip a verbatim prefix for use in command-line arguments.
    fn plain(path: &Path) -> Result<String, SupervisorError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| SupervisorError::InvalidPath(path.to_path_buf()))?;
        let text = canonical
            .to_str()
            .ok_or_else(|| SupervisorError::InvalidPath(path.to_path_buf()))?;
        Ok(text.strip_prefix(r"\\?\").unwrap_or(text).to_string())
    }

    fn parent(path: &Path) -> Result<PathBuf, SupervisorError> {
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| SupervisorError::InvalidPath(path.to_path_buf()))
    }

    fn file_reader(file: std::fs::File) -> tokio::fs::File {
        tokio::fs::File::from_std(file)
    }

    /// The sandbox identity used only to read runtimes' option lists.
    const PROBE_PROFILE: &str = "Gardenwall.RuntimeProbe";

    /// `llama-server --help` in its own AppContainer: no network, one
    /// process, read and execute access to the runtime folder only.
    pub(super) fn help(
        server_exe: &Path,
        grant_journal_dir: Option<&Path>,
    ) -> Result<String, SupervisorError> {
        use std::io::Read;

        let _active = ActiveGuard::acquire(PROBE_PROFILE)?;
        let server_exe =
            std::fs::canonicalize(server_exe).map_err(|_| SupervisorError::MissingExecutable)?;
        let runtime_dir = parent(&server_exe)?;
        let mut env = Vec::new();
        if let Some(root) = std::env::var_os("SystemRoot") {
            env.push(("SystemRoot".into(), root));
        }
        let policy = SandboxPolicy {
            profile: PROBE_PROFILE.into(),
            grants: vec![PathGrant {
                path: runtime_dir.clone(),
                access: Access::ReadExecute,
            }],
            network: Network::Denied,
            limits: Limits {
                max_processes: 1,
                ..Limits::default()
            },
            env,
            working_dir: Some(runtime_dir),
            grant_journal_dir: grant_journal_dir.map(Path::to_path_buf),
        };
        let sandbox = Sandbox::new(policy).map_err(SupervisorError::Sandbox)?;
        let mut child = sandbox
            .spawn(&server_exe, &[OsStr::new("--help")])
            .map_err(SupervisorError::Sandbox)?;
        drop(child.stdin.take());
        drop(child.stderr.take());
        let stdout = child.stdout.take().ok_or(SupervisorError::Exited(None))?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(MAX_HELP_BYTES).read_to_end(&mut out);
            out
        });
        let exited = child
            .wait_timeout(Some(HELP_TIMEOUT))
            .map_err(SupervisorError::Sandbox)?;
        if exited.is_none() {
            let _ = sandbox.terminate_all();
            let _ = child.wait_timeout(Some(Duration::from_secs(5)));
        }
        let out = reader.join().unwrap_or_default();
        // Dropping the sandbox ends its job and revokes the grant.
        drop(sandbox);
        if exited.is_none() {
            return Err(SupervisorError::StartupTimeout);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub(super) async fn launch(
        spec: &LaunchSpec,
        relay_exe: &Path,
        grant_journal_dir: Option<&Path>,
        key: ApiKey,
        timeouts: Timeouts,
    ) -> Result<RunningServer, SupervisorError> {
        require_file(relay_exe)?;
        let sha256 = spec
            .model_sha256
            .as_deref()
            .ok_or(SupervisorError::InvalidModelId)?;
        let profile = model_profile(sha256)?;
        // Quarantined models are named `<sha256>.gguf`; if the file follows
        // that convention, its name must agree with the claimed digest.
        if let Some(stem) = spec.model.file_stem().and_then(|s| s.to_str())
            && stem.len() == 64
            && stem.bytes().all(|b| b.is_ascii_hexdigit())
            && !stem.eq_ignore_ascii_case(sha256)
        {
            return Err(SupervisorError::InvalidModelId);
        }
        let active = ActiveGuard::acquire(&profile)?;
        let server_exe = std::fs::canonicalize(&spec.server_exe)
            .map_err(|_| SupervisorError::MissingExecutable)?;
        let relay_exe = std::fs::canonicalize(relay_exe)
            .map_err(|_| SupervisorError::InvalidPath(relay_exe.to_path_buf()))?;
        let runtime_dir = parent(&server_exe)?;

        // Model and template paths as the runtime will see them.
        let mut runtime_spec = spec.clone();
        runtime_spec.model = PathBuf::from(plain(&spec.model)?);
        if let Some(t) = &spec.chat_template_file {
            runtime_spec.chat_template_file = Some(PathBuf::from(plain(t)?));
        }
        if let Some(m) = &spec.mmproj {
            runtime_spec.mmproj = Some(PathBuf::from(plain(m)?));
        }
        if let Speculative::Draft(d) = &spec.speculative {
            runtime_spec.speculative = Speculative::Draft(PathBuf::from(plain(d)?));
        }
        let args = runtime_spec.args().map_err(SupervisorError::Launch)?;
        let env = runtime_spec.env(&key).map_err(SupervisorError::Launch)?;

        let mut grants = vec![
            PathGrant {
                path: runtime_dir.clone(),
                access: Access::ReadExecute,
            },
            PathGrant {
                path: spec.model.clone(),
                access: Access::Read,
            },
        ];
        // A split model's other parts, read only, file by file.
        for part in &spec.model_parts {
            grants.push(PathGrant {
                path: part.clone(),
                access: Access::Read,
            });
        }
        let relay_dir = parent(&relay_exe)?;
        if relay_dir != runtime_dir {
            grants.push(PathGrant {
                path: relay_dir,
                access: Access::ReadExecute,
            });
        }
        if let Some(t) = &spec.chat_template_file {
            grants.push(PathGrant {
                path: t.clone(),
                access: Access::Read,
            });
        }
        if let Some(m) = &spec.mmproj {
            grants.push(PathGrant {
                path: m.clone(),
                access: Access::Read,
            });
        }
        // The draft model is read only, like the main model.
        if let Speculative::Draft(d) = &spec.speculative {
            grants.push(PathGrant {
                path: d.clone(),
                access: Access::Read,
            });
        }

        let policy = SandboxPolicy {
            profile,
            grants,
            network: Network::Denied,
            limits: Limits {
                // The runtime and the relay; neither may start others.
                max_processes: 2,
                ..Limits::default()
            },
            env,
            working_dir: Some(runtime_dir),
            grant_journal_dir: grant_journal_dir.map(Path::to_path_buf),
        };
        let sandbox = Sandbox::new(policy).map_err(SupervisorError::Sandbox)?;
        // On failure, dropping the sandbox terminates its job and revokes
        // grants; the persistent profile is kept for the model's next run.
        let cleanup = |sandbox: Sandbox| drop(sandbox);

        let arg_refs: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        let mut server = match sandbox.spawn(&server_exe, &arg_refs) {
            Ok(p) => p,
            Err(e) => {
                cleanup(sandbox);
                return Err(SupervisorError::Sandbox(e));
            }
        };

        let logs = LogTail::default();
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(out) = server.stdout.take() {
            spawn_line_reader(file_reader(out), logs.clone(), tx.clone());
        }
        if let Some(err) = server.stderr.take() {
            spawn_line_reader(file_reader(err), logs.clone(), tx.clone());
        }
        drop(tx);

        let port = match wait_for_port(rx, timeouts.startup).await {
            Ok(Some(port)) => port,
            Ok(None) => {
                let code = server
                    .wait_timeout(Some(Duration::from_secs(2)))
                    .ok()
                    .flatten();
                cleanup(sandbox);
                return Err(logs.exited(code.map(|c| c as i32)));
            }
            Err(e) => {
                cleanup(sandbox);
                return Err(e);
            }
        };

        let mut listener = match PipeListener::for_sandbox(&sandbox) {
            Ok(l) => l,
            Err(e) => {
                cleanup(sandbox);
                return Err(SupervisorError::Sandbox(e));
            }
        };
        let pipe_name = listener.name().to_string();
        let port_arg = port.to_string();
        let max_arg = RELAY_CONNECTIONS.to_string();
        let relay = match sandbox.spawn(
            &relay_exe,
            &[
                "--pipe",
                pipe_name.as_str(),
                "--port",
                port_arg.as_str(),
                "--max-connections",
                max_arg.as_str(),
            ]
            .map(OsStr::new),
        ) {
            Ok(r) => r,
            Err(e) => {
                cleanup(sandbox);
                return Err(SupervisorError::Sandbox(e));
            }
        };
        let relay_pid = relay.pid();

        // Accept loop: only the spawned relay's connections are admitted.
        let (stream_tx, stream_rx) = mpsc::channel::<BoxStream>(RELAY_CONNECTIONS);
        let accept_logs = logs.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((conn, pid)) if pid == relay_pid => {
                        if stream_tx.send(Box::new(conn)).await.is_err() {
                            return;
                        }
                    }
                    Ok((_conn, pid)) => {
                        accept_logs.push(&format!(
                            "gardenwall: rejected pipe client with unexpected pid {pid}"
                        ));
                    }
                    Err(_) => return,
                }
            }
        });

        let running = Running {
            accept_task,
            server,
            _relay: relay,
            sandbox: Some(sandbox),
            _active: active,
        };
        let client = InferenceClient::new(Connector::pipe(stream_rx), key);
        wait_healthy(&client, timeouts.load, || {
            match running.server.wait_timeout(Some(Duration::ZERO)) {
                Ok(Some(code)) => Some(Some(code as i32)),
                _ => None,
            }
        })
        .await
        .map_err(|e| match e {
            SupervisorError::Exited(code) => logs.exited(code),
            e => e,
        })?;

        Ok(RunningServer {
            client,
            logs,
            inner: Inner::Sandbox(Box::new(running)),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const SHA: &str = "9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031";

        #[test]
        fn profile_is_derived_from_the_model_digest() {
            assert_eq!(
                model_profile(SHA).unwrap(),
                "Gardenwall.Inference.9465e63a22add5354d9bb4b99e901170"
            );
            assert_eq!(
                model_profile(&SHA.to_uppercase()).unwrap(),
                model_profile(SHA).unwrap()
            );
            for bad in [
                "",
                "abc",
                &SHA[..63],
                &format!("{SHA}0"),
                &SHA.replace('9', "g"),
            ] {
                assert!(
                    matches!(model_profile(bad), Err(SupervisorError::InvalidModelId)),
                    "{bad}"
                );
            }
        }

        #[test]
        fn one_session_per_model() {
            let profile = model_profile(&"ab".repeat(32)).unwrap();
            let first = ActiveGuard::acquire(&profile).unwrap();
            assert!(matches!(
                ActiveGuard::acquire(&profile),
                Err(SupervisorError::ModelAlreadyRunning)
            ));
            drop(first);
            assert!(ActiveGuard::acquire(&profile).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_architecture_is_named_from_the_log() {
        let line = "llama_model_load: error loading model: error loading model architecture: unknown model architecture: 'qwen9next'";
        assert_eq!(unknown_architecture(line).as_deref(), Some("qwen9next"));
        assert_eq!(
            unknown_architecture("load_tensors: offloaded 65/65 layers"),
            None
        );
        // Odd text after the marker can't leak into the message.
        let odd = "unknown model architecture: 'a<b>c' and more";
        assert_eq!(unknown_architecture(odd).as_deref(), Some("a"));
        assert_eq!(
            unknown_architecture("unknown model architecture: ''").as_deref(),
            Some("unknown")
        );

        let logs = LogTail::default();
        logs.push("main: loading model");
        assert!(matches!(
            logs.exited(Some(1)),
            SupervisorError::Exited(Some(1))
        ));
        logs.push(line);
        let e = logs.exited(Some(1));
        assert!(matches!(&e, SupervisorError::UnsupportedModel(a) if a == "qwen9next"));
        assert!(e.to_string().contains("newer runtime") && e.to_string().contains("Runtime page"));
    }
}
