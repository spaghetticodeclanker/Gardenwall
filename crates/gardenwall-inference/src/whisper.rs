//! The voice-input sidecar (release plan feature 7): whisper.cpp's
//! `whisper-server` in its own AppContainer with no network, like
//! `llama-server` (ADR-012 §4.1): read and execute access to its runtime,
//! read access to the speech model, write access to a GPU kernel cache
//! folder only. The trusted core reaches it through `gardenwall-relay` over
//! a core-owned named pipe, sends the recorded audio (a WAV file held in
//! memory, never written to disk) and gets text back. The server and
//! everything it returns are untrusted: the text only ever lands in the
//! user's own message box.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, header};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;

use crate::transport::Connector;

/// Longest a server may take to start: the first start on a new GPU
/// compiles kernels (about 27 s on an RTX 5090) before loading the model.
#[cfg_attr(not(windows), allow(dead_code))]
const STARTUP: Duration = Duration::from_secs(180);
/// Longest one transcription may take.
const TRANSCRIBE: Duration = Duration::from_secs(120);
/// Largest audio sent (bytes): about 5 minutes of 16 kHz mono 16-bit.
pub const MAX_WAV_BYTES: usize = 10 << 20;
/// Largest reply read back.
const MAX_REPLY_BYTES: usize = 1 << 20;
/// Pre-connected relay streams kept ready.
#[cfg_attr(not(windows), allow(dead_code))]
const RELAY_CONNECTIONS: usize = 4;
/// The persistent sandbox identity: the GPU kernel cache it keeps belongs
/// to voice input only.
pub const PROFILE: &str = "Gardenwall.Speech.Input";

#[derive(Debug)]
pub enum WhisperError {
    /// A path, the language or an option isn't acceptable.
    Invalid(&'static str),
    Unsupported,
    Sandbox(String),
    /// The server stopped during startup; the last log lines.
    Exited(Vec<String>),
    Timeout,
    Http(String),
    Status(u16),
    Malformed,
}

impl std::fmt::Display for WhisperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::Unsupported => write!(f, "voice input is available on Windows only"),
            Self::Sandbox(e) => write!(f, "the voice-input sandbox couldn't start: {e}"),
            Self::Exited(lines) => write!(
                f,
                "the speech recogniser stopped while starting{}",
                lines.last().map(|l| format!(" ({l})")).unwrap_or_default()
            ),
            Self::Timeout => write!(f, "the speech recogniser took too long"),
            Self::Http(e) => write!(f, "couldn't reach the speech recogniser: {e}"),
            Self::Status(code) => write!(f, "the speech recogniser answered with error {code}"),
            Self::Malformed => write!(f, "the speech recogniser's answer couldn't be read"),
        }
    }
}

impl std::error::Error for WhisperError {}

/// What to launch.
#[derive(Debug, Clone)]
pub struct WhisperSpec {
    pub server_exe: PathBuf,
    pub model: PathBuf,
    /// A language code such as `en` (fixed, chosen in
    /// Settings).
    pub language: String,
    /// Use the GPU (`false` adds `-ng`).
    pub gpu: bool,
    pub threads: u32,
    /// Writable folder for the GPU driver's compiled kernels.
    pub cache_dir: PathBuf,
}

/// A language code whisper.cpp accepts: two or three lowercase letters
/// (`en`, `de`, `haw`).
pub fn valid_language(code: &str) -> bool {
    (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase())
}

impl WhisperSpec {
    /// The server's arguments, with `model` as the server will see it.
    pub fn args(&self, model: &str, port: u16) -> Result<Vec<String>, WhisperError> {
        if !valid_language(&self.language) {
            return Err(WhisperError::Invalid("language"));
        }
        let mut args = vec![
            "-m".to_string(),
            model.to_string(),
            "-l".into(),
            self.language.clone(),
            "-t".into(),
            self.threads.clamp(1, 32).to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "-nt".into(),
        ];
        if !self.gpu {
            args.push("-ng".into());
        }
        Ok(args)
    }
}

/// One multipart/form-data request body with the WAV as `file`.
pub fn multipart(boundary: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in [("response_format", "json"), ("temperature", "0")] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// The text in whisper-server's JSON answer (`{"text": "..."}`), trimmed.
pub fn parse_reply(bytes: &[u8]) -> Result<String, WhisperError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        text: String,
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| WhisperError::Malformed)?;
    Ok(reply.text.trim().to_string())
}

/// A running voice-input server.
pub struct WhisperServer {
    connector: Connector,
    #[cfg(windows)]
    _running: sandboxed::Running,
}

impl WhisperServer {
    async fn request(
        &self,
        method: Method,
        path: &'static str,
        content_type: Option<String>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), WhisperError> {
        let stream = self
            .connector
            .connect()
            .await
            .map_err(|e| WhisperError::Http(e.to_string()))?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| WhisperError::Http(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost");
        if let Some(t) = content_type {
            builder = builder.header(header::CONTENT_TYPE, t);
        }
        let request = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| WhisperError::Malformed)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| WhisperError::Http(e.to_string()))?;
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| WhisperError::Http(e.to_string()))?;
            if let Ok(data) = frame.into_data() {
                if out.len() + data.len() > MAX_REPLY_BYTES {
                    return Err(WhisperError::Malformed);
                }
                out.extend_from_slice(&data);
            }
        }
        Ok((status, out))
    }

    /// Transcribe a 16 kHz mono 16-bit WAV.
    pub async fn transcribe(&self, wav: &[u8]) -> Result<String, WhisperError> {
        if wav.len() > MAX_WAV_BYTES || !wav.starts_with(b"RIFF") {
            return Err(WhisperError::Invalid("audio"));
        }
        let mut raw = [0u8; 12];
        getrandom::fill(&mut raw).map_err(|_| WhisperError::Malformed)?;
        let boundary: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let body = multipart(&boundary, wav);
        let (status, reply) = tokio::time::timeout(
            TRANSCRIBE,
            self.request(
                Method::POST,
                "/inference",
                Some(format!("multipart/form-data; boundary={boundary}")),
                body,
            ),
        )
        .await
        .map_err(|_| WhisperError::Timeout)??;
        if status != 200 {
            return Err(WhisperError::Status(status));
        }
        parse_reply(&reply)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    async fn ready(&self) -> bool {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                self.request(Method::GET, "/", None, Vec::new())
            )
            .await,
            Ok(Ok((200, _)))
        )
    }
}

/// Start the server in its sandbox and wait until it answers.
#[cfg(windows)]
pub async fn launch(
    spec: &WhisperSpec,
    relay_exe: &std::path::Path,
    grant_journal_dir: Option<&std::path::Path>,
) -> Result<WhisperServer, WhisperError> {
    sandboxed::launch(spec, relay_exe, grant_journal_dir).await
}

#[cfg(not(windows))]
pub async fn launch(
    _spec: &WhisperSpec,
    _relay_exe: &std::path::Path,
    _grant_journal_dir: Option<&std::path::Path>,
) -> Result<WhisperServer, WhisperError> {
    Err(WhisperError::Unsupported)
}

#[cfg(windows)]
mod sandboxed {
    use std::collections::VecDeque;
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use gardenwall_sandbox::{
        Access, Limits, Network, PathGrant, PipeListener, Sandbox, SandboxPolicy, SandboxedProcess,
    };
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::sync::mpsc;

    use super::*;
    use crate::transport::BoxStream;

    /// One voice-input server at a time (one profile, one set of grants).
    static ACTIVE: Mutex<bool> = Mutex::new(false);

    struct ActiveGuard;

    impl ActiveGuard {
        fn acquire() -> Result<Self, WhisperError> {
            let mut active = ACTIVE
                .lock()
                .map_err(|_| WhisperError::Sandbox("busy".into()))?;
            if *active {
                return Err(WhisperError::Sandbox(
                    "voice input is already running".into(),
                ));
            }
            *active = true;
            Ok(Self)
        }
    }

    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            if let Ok(mut a) = ACTIVE.lock() {
                *a = false;
            }
        }
    }

    pub(super) struct Running {
        accept_task: tokio::task::JoinHandle<()>,
        server: SandboxedProcess,
        relay: Option<SandboxedProcess>,
        sandbox: Option<Sandbox>,
        _active: ActiveGuard,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.accept_task.abort();
            if let Some(sandbox) = &self.sandbox {
                let _ = sandbox.terminate_all();
            }
            let _ = self.server.wait_timeout(Some(Duration::from_secs(5)));
            if let Some(relay) = &self.relay {
                let _ = relay.wait_timeout(Some(Duration::from_secs(5)));
            }
            drop(self.sandbox.take());
        }
    }

    fn plain(path: &Path) -> Result<String, WhisperError> {
        let canonical = std::fs::canonicalize(path).map_err(|_| WhisperError::Invalid("path"))?;
        let text = canonical.to_str().ok_or(WhisperError::Invalid("path"))?;
        Ok(text.strip_prefix(r"\\?\").unwrap_or(text).to_string())
    }

    type Logs = Arc<Mutex<VecDeque<String>>>;

    fn read_lines(file: std::fs::File, logs: Logs) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::fs::File::from_std(file)).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut l) = logs.lock() {
                    if l.len() >= 40 {
                        l.pop_front();
                    }
                    l.push_back(crate::launch::sanitize_log_line(&line, 300));
                }
            }
        });
    }

    fn random_port() -> u16 {
        let mut b = [0u8; 2];
        let _ = getrandom::fill(&mut b);
        20_000 + (u16::from_le_bytes(b) % 40_000)
    }

    pub(super) async fn launch(
        spec: &WhisperSpec,
        relay_exe: &Path,
        grant_journal_dir: Option<&Path>,
    ) -> Result<WhisperServer, WhisperError> {
        let active = ActiveGuard::acquire()?;
        let server_exe = std::fs::canonicalize(&spec.server_exe)
            .map_err(|_| WhisperError::Invalid("runtime"))?;
        let relay_exe =
            std::fs::canonicalize(relay_exe).map_err(|_| WhisperError::Invalid("relay"))?;
        let runtime_dir = server_exe
            .parent()
            .ok_or(WhisperError::Invalid("runtime"))?
            .to_path_buf();
        let relay_dir = relay_exe
            .parent()
            .ok_or(WhisperError::Invalid("relay"))?
            .to_path_buf();
        if !spec.model.is_file() {
            return Err(WhisperError::Invalid("speech model"));
        }
        std::fs::create_dir_all(&spec.cache_dir).map_err(|_| WhisperError::Invalid("cache"))?;
        let model = plain(&spec.model)?;
        let cache = plain(&spec.cache_dir)?;
        let port = random_port();
        let args = spec.args(&model, port)?;

        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
        for key in ["SystemRoot", "windir"] {
            if let Some(v) = std::env::var_os(key) {
                env.push((key.into(), v));
            }
        }
        // The GPU driver keeps compiled kernels here (folder names only).
        env.push(("CUDA_CACHE_PATH".into(), cache.into()));
        env.push(("CUDA_CACHE_MAXSIZE".into(), "1073741824".into()));

        let mut grants = vec![
            PathGrant {
                path: runtime_dir.clone(),
                access: Access::ReadExecute,
            },
            PathGrant {
                path: spec.model.clone(),
                access: Access::Read,
            },
            PathGrant {
                path: spec.cache_dir.clone(),
                access: Access::ReadWrite,
            },
        ];
        if relay_dir != runtime_dir {
            grants.push(PathGrant {
                path: relay_dir,
                access: Access::ReadExecute,
            });
        }
        let policy = SandboxPolicy {
            profile: PROFILE.into(),
            grants,
            network: Network::Denied,
            limits: Limits {
                max_processes: 2,
                ..Limits::default()
            },
            env,
            working_dir: Some(runtime_dir),
            grant_journal_dir: grant_journal_dir.map(Path::to_path_buf),
        };
        let sandbox = Sandbox::new(policy).map_err(|e| WhisperError::Sandbox(e.to_string()))?;
        let arg_refs: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut server = sandbox
            .spawn(&server_exe, &arg_refs)
            .map_err(|e| WhisperError::Sandbox(e.to_string()))?;
        let logs: Logs = Arc::default();
        if let Some(out) = server.stdout.take() {
            read_lines(out, logs.clone());
        }
        if let Some(err) = server.stderr.take() {
            read_lines(err, logs.clone());
        }

        let mut listener = PipeListener::for_sandbox(&sandbox)
            .map_err(|e| WhisperError::Sandbox(e.to_string()))?;
        let pipe_name = listener.name().to_string();
        let port_arg = port.to_string();
        let max_arg = RELAY_CONNECTIONS.to_string();
        let relay = sandbox
            .spawn(
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
            )
            .map_err(|e| WhisperError::Sandbox(e.to_string()))?;
        let relay_pid = relay.pid();
        let (stream_tx, stream_rx) = mpsc::channel::<BoxStream>(RELAY_CONNECTIONS);
        let accept_task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((conn, pid)) if pid == relay_pid => {
                        if stream_tx.send(Box::new(conn)).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let running = Running {
            accept_task,
            server,
            relay: Some(relay),
            sandbox: Some(sandbox),
            _active: active,
        };
        let server = WhisperServer {
            connector: Connector::pipe(stream_rx),
            _running: running,
        };
        let started = std::time::Instant::now();
        loop {
            if server
                ._running
                .server
                .wait_timeout(Some(Duration::from_millis(0)))
                .ok()
                .flatten()
                .is_some()
            {
                let lines = logs
                    .lock()
                    .map(|l| l.iter().cloned().collect())
                    .unwrap_or_default();
                return Err(WhisperError::Exited(lines));
            }
            if server.ready().await {
                return Ok(server);
            }
            if started.elapsed() > STARTUP {
                return Err(WhisperError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_fixed_and_checked() {
        let spec = WhisperSpec {
            server_exe: "w.exe".into(),
            model: "m.bin".into(),
            language: "en".into(),
            gpu: false,
            threads: 99,
            cache_dir: "c".into(),
        };
        let args = spec.args(r"C:\data\m.bin", 30000).unwrap();
        assert_eq!(
            args,
            [
                "-m",
                r"C:\data\m.bin",
                "-l",
                "en",
                "-t",
                "32",
                "--host",
                "127.0.0.1",
                "--port",
                "30000",
                "-nt",
                "-ng"
            ]
        );
        for bad in ["", "e", "EN", "english", "en;", "../"] {
            let spec = WhisperSpec {
                language: bad.into(),
                ..spec.clone()
            };
            assert!(spec.args("m", 1).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_audio_goes_as_one_file_field() {
        let body = multipart("XYZ", b"RIFFdata");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"response_format\"\r\n\r\njson"));
        assert!(text.contains("filename=\"speech.wav\""));
        assert!(text.contains("RIFFdata\r\n--XYZ--\r\n"));
    }

    /// Opt-in, Windows: whisper-server in its real sandbox, reached through
    /// the relay. `GARDENWALL_TEST_WHISPER=<whisper-server.exe>;<model>;<16
    /// kHz WAV>;<gpu: 0 or 1>` and `cargo build -p gardenwall-relay` first.
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn transcribes_inside_the_sandbox() {
        let Ok(setting) = std::env::var("GARDENWALL_TEST_WHISPER") else {
            eprintln!("skipped: set GARDENWALL_TEST_WHISPER");
            return;
        };
        let parts: Vec<&str> = setting.split(';').collect();
        let base = std::env::temp_dir().join(format!("gw-whisper-test-{}", std::process::id()));
        std::fs::create_dir_all(base.join("journal")).unwrap();
        // The relay in a folder of its own, as the app stages it.
        std::fs::create_dir_all(base.join("relay")).unwrap();
        let relay = base.join("relay").join("gardenwall-relay.exe");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/gardenwall-relay.exe"),
            &relay,
        )
        .unwrap();
        let spec = WhisperSpec {
            server_exe: parts[0].into(),
            model: parts[1].into(),
            language: "en".into(),
            gpu: parts[3] == "1",
            threads: 4,
            // An optional fifth part keeps the kernel cache between runs.
            cache_dir: parts
                .get(4)
                .map_or_else(|| base.join("cuda"), PathBuf::from),
        };
        let server = launch(&spec, &relay, Some(&base.join("journal")))
            .await
            .expect("launch");
        let wav = std::fs::read(parts[2]).unwrap();
        let started = std::time::Instant::now();
        let text = server.transcribe(&wav).await.expect("transcribe");
        eprintln!("{text:?} in {:?}", started.elapsed());
        drop(server);
        let _ = std::fs::remove_dir_all(&base);
        assert!(text.to_lowercase().contains("quick brown fox"), "{text}");
    }

    #[test]
    fn replies_are_parsed_and_trimmed() {
        assert_eq!(
            parse_reply(br#"{"text":"  Hello there.\n"}"#).unwrap(),
            "Hello there."
        );
        assert!(parse_reply(b"<html>").is_err());
    }
}
