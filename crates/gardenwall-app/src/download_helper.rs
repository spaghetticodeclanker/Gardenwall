//! Model downloads in a sandboxed helper (ADR-013 §2.6
//! ). Windows only; elsewhere the core downloads as before.
//!
//! The core still resolves the repository and checks every file's SHA-256
//! and GGUF structure itself. Only fetching the bytes moves out: the
//! `gardenwall-fetch` helper runs in its own AppContainer, with no network
//! capability and write access to one staging folder, and does all of the
//! HTTP, TLS and HTTP/2 work. Its only way out is this module's gate: each
//! connection arrives on a pipe only the helper's sandbox can open, and the
//! gate reads one `CONNECT host:443` request and connects only to a Hugging
//! Face host ([`HostPolicy::hugging_face`]) at a public address. TLS is end
//! to end in the helper, so nothing from the hub is parsed here.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gardenwall_models::hub::{FetchFuture, HostPolicy, HubError, PartFetcher, PartJob};
use gardenwall_sandbox::{
    Access, Limits, Network, PathGrant, PipeListener, Sandbox, SandboxPolicy,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::windows::named_pipe::NamedPipeServer;

/// Longest `CONNECT` request accepted from the helper.
const MAX_REQUEST_BYTES: usize = 4096;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the helper has to send its `CONNECT` line (review GW-020).
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
/// A connection on which nothing moves either way for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest progress line read from the helper.
const MAX_LINE_BYTES: usize = 512;

/// Fetches download bytes with the sandboxed helper.
pub struct SandboxedFetcher {
    pub helper_dir: PathBuf,
    pub journal: PathBuf,
}

impl PartFetcher for SandboxedFetcher {
    fn fetch<'a>(
        &'a self,
        job: PartJob,
        progress: &'a mut (dyn FnMut(u64, u64) -> bool + Send),
    ) -> FetchFuture<'a> {
        Box::pin(async move {
            let exe = crate::state::stage_helper(&self.helper_dir, "gardenwall-fetch")
                .map_err(|e| HubError::Helper(format!("unavailable: {e}")))?;
            fetch_in_sandbox(&exe, &self.journal, job, progress).await
        })
    }
}

/// A private folder next to the partial file, holding only this download,
/// so the helper can write nothing else in the library.
pub(crate) fn staging_for(part: &Path) -> Result<PathBuf, HubError> {
    let parent = part.parent().ok_or(HubError::InvalidFilePath)?;
    let stem = part
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(HubError::InvalidFilePath)?;
    Ok(parent.join(format!(".fetch-{stem}")))
}

/// Move `from` to `to` when it exists (a partial file or resume state).
fn carry(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.exists() {
        std::fs::rename(from, to)?;
    }
    Ok(())
}

async fn fetch_in_sandbox(
    exe: &Path,
    journal: &Path,
    job: PartJob,
    progress: &mut (dyn FnMut(u64, u64) -> bool + Send),
) -> Result<(), HubError> {
    let staging = staging_for(&job.part_path)?;
    std::fs::create_dir_all(&staging)?;
    let inner = PartJob {
        part_path: staging.join("download.part"),
        state_path: staging.join("download.state"),
        ..job.clone()
    };
    // Resume: the earlier partial file and its state go in with it.
    carry(&job.part_path, &inner.part_path)?;
    carry(&job.state_path, &inner.state_path)?;
    let result = run_helper(exe, journal, &staging, &inner, progress).await;
    // Whatever happened, the bytes come back out of the helper's folder
    // (kept for resuming after a failure or pause). If they couldn't be
    // moved, the folder stays: the next attempt resumes from it.
    let back = carry(&inner.part_path, &job.part_path)
        .and_then(|()| carry(&inner.state_path, &job.state_path));
    if back.is_ok() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result?;
    back?;
    Ok(())
}

async fn run_helper(
    exe: &Path,
    journal: &Path,
    staging: &Path,
    job: &PartJob,
    progress: &mut (dyn FnMut(u64, u64) -> bool + Send),
) -> Result<(), HubError> {
    let helper_dir = exe.parent().ok_or(HubError::InvalidFilePath)?;
    let mut env = Vec::new();
    for name in ["SystemRoot", "windir"] {
        if let Some(v) = std::env::var_os(name) {
            env.push((name.into(), v));
        }
    }
    let sandbox = Sandbox::new(SandboxPolicy {
        profile: "Gardenwall.Downloader".into(),
        grants: vec![
            PathGrant {
                path: helper_dir.to_path_buf(),
                access: Access::ReadExecute,
            },
            PathGrant {
                path: staging.to_path_buf(),
                access: Access::ReadWrite,
            },
        ],
        network: Network::Denied,
        limits: Limits {
            memory_bytes: Some(1 << 30),
            max_processes: 1,
            wall_time: None,
        },
        env,
        working_dir: Some(staging.to_path_buf()),
        grant_journal_dir: Some(journal.to_path_buf()),
    })
    .map_err(|e| HubError::Helper(format!("its sandbox failed: {e}")))?;
    let mut listener = PipeListener::for_sandbox(&sandbox)
        .map_err(|e| HubError::Helper(format!("its connection failed: {e}")))?;
    let pipe = listener.name().to_string();
    let mut child = sandbox
        .spawn(exe, &[OsStr::new("--pipe"), OsStr::new(&pipe)])
        .map_err(|e| HubError::Helper(format!("it failed to start: {e}")))?;
    let pid = child.pid();
    let input = serde_json::to_vec(job).map_err(|_| HubError::MalformedResponse)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| HubError::Helper("no input".into()))?;
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HubError::Helper("no output".into()))?;

    // The gate: only the helper's own connections, each checked.
    let gate = tokio::spawn(async move {
        while let Ok((conn, from)) = listener.accept().await {
            if from == pid {
                tokio::spawn(gate_connection(conn));
            }
        }
    });

    // Progress lines, read on a thread (the pipe is a blocking file).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match std::io::Read::take(&mut reader, MAX_LINE_BYTES as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .send(String::from_utf8_lossy(&line).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let mut outcome: Option<Result<(), String>> = None;
    while let Some(line) = rx.recv().await {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if let (Some(done), Some(total)) = (
            value.get("done").and_then(serde_json::Value::as_u64),
            value.get("total").and_then(serde_json::Value::as_u64),
        ) {
            // The helper's numbers only drive the progress bar.
            if !progress(done.min(job.size), job.size) {
                let _ = child.kill();
                // Wait until it has exited and closed the partial file, so
                // the file can be moved back out of its folder.
                let _ = tokio::task::spawn_blocking(move || {
                    child.wait_timeout(Some(Duration::from_secs(10)))
                })
                .await;
                gate.abort();
                return Err(HubError::Cancelled);
            }
            let _ = total;
        } else if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            outcome = Some(Ok(()));
        } else if let Some(e) = value.get("error").and_then(serde_json::Value::as_str) {
            outcome = Some(Err(e.chars().take(300).collect()));
        }
    }
    let code =
        tokio::task::spawn_blocking(move || child.wait_timeout(Some(Duration::from_secs(30))))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
    gate.abort();
    drop(sandbox);
    match (outcome, code) {
        (Some(Ok(())), Some(0)) => Ok(()),
        (Some(Err(e)), _) => Err(HubError::Helper(e)),
        (_, code) => Err(HubError::Helper(format!(
            "it stopped without finishing (exit code {code:?})"
        ))),
    }
}

/// The host and port of a `CONNECT host:port HTTP/1.1` request, when the
/// request is exactly that.
fn parse_connect(head: &str) -> Option<(String, u16)> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method != "CONNECT" || !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    let (host, port) = target.rsplit_once(':')?;
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some((host, port.parse().ok()?))
}

/// Whether the gate may connect to `host:port`: HTTPS to a Hugging Face
/// host only.
fn allowed(host: &str, port: u16) -> bool {
    port == 443
        && url::Url::parse(&format!("https://{host}/"))
            .is_ok_and(|u| HostPolicy::hugging_face().permits(&u))
}

async fn gate_connection(mut conn: NamedPipeServer) {
    let refuse = |status: &str| format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
    // Read the request head byte by byte, so nothing after it is consumed,
    // within a time limit (review GW-020).
    let mut head = Vec::new();
    let read_head = async {
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= MAX_REQUEST_BYTES {
                return false;
            }
            match conn.read_u8().await {
                Ok(b) => head.push(b),
                Err(_) => return false,
            }
        }
        true
    };
    if !matches!(
        tokio::time::timeout(HEAD_TIMEOUT, read_head).await,
        Ok(true)
    ) {
        return;
    }
    let Some((host, port)) = parse_connect(&String::from_utf8_lossy(&head)) else {
        let _ = conn.write_all(refuse("400 Bad Request").as_bytes()).await;
        return;
    };
    if !allowed(&host, port) {
        let _ = conn.write_all(refuse("403 Forbidden").as_bytes()).await;
        return;
    }
    let addresses = match tokio::time::timeout(
        RESOLVE_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(a)) => a
            .filter(|a| gardenwall_egress::addr::is_public(a.ip()))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut upstream = None;
    for address in addresses {
        if let Ok(Ok(tcp)) =
            tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await
        {
            upstream = Some(tcp);
            break;
        }
    }
    let Some(mut upstream) = upstream else {
        let _ = conn.write_all(refuse("502 Bad Gateway").as_bytes()).await;
        return;
    };
    let _ = upstream.set_nodelay(true);
    if conn
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }
    relay(&mut conn, &mut upstream, IDLE_TIMEOUT).await;
}

/// Pass bytes both ways until either side closes, or until nothing has
/// moved for `idle`.
async fn relay<A, B>(helper: A, host: B, idle: Duration)
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let (mut from_helper, mut to_helper) = tokio::io::split(helper);
    let (mut from_host, mut to_host) = tokio::io::split(host);
    let mut up = vec![0u8; 64 * 1024];
    let mut down = vec![0u8; 64 * 1024];
    loop {
        let step = tokio::time::timeout(idle, async {
            tokio::select! {
                r = from_helper.read(&mut up) => match r {
                    Ok(n) if n > 0 => to_host.write_all(&up[..n]).await.is_ok(),
                    _ => false,
                },
                r = from_host.read(&mut down) => match r {
                    Ok(n) if n > 0 => to_helper.write_all(&down[..n]).await.is_ok(),
                    _ => false,
                },
            }
        })
        .await;
        if !matches!(step, Ok(true)) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_to_hugging_face_passes_the_gate() {
        let parse = |s: &str| parse_connect(s);
        assert_eq!(
            parse("CONNECT huggingface.co:443 HTTP/1.1\r\nHost: huggingface.co:443\r\n\r\n"),
            Some(("huggingface.co".into(), 443))
        );
        assert_eq!(
            parse("CONNECT CDN-LFS.HF.CO:443 HTTP/1.1\r\n\r\n"),
            Some(("cdn-lfs.hf.co".into(), 443))
        );
        for bad in [
            "GET http://huggingface.co/ HTTP/1.1\r\n\r\n",
            "CONNECT huggingface.co HTTP/1.1\r\n\r\n",
            "CONNECT [::1]:443 HTTP/1.1\r\n\r\n",
            "CONNECT hugging face.co:443 HTTP/1.1\r\n\r\n",
            "CONNECT huggingface.co:443 HTTP/2 extra\r\n\r\n",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert!(allowed("huggingface.co", 443));
        assert!(allowed("cas-bridge.xethub.hf.co", 443));
        for (host, port) in [
            ("huggingface.co", 80),
            ("huggingface.co.evil.com", 443),
            ("evilhf.co", 443),
            ("example.com", 443),
            ("127.0.0.1", 443),
            ("localhost", 443),
        ] {
            assert!(!allowed(host, port), "{host}:{port}");
        }
    }

    /// A real download from Hugging Face through the sandboxed helper and
    /// the gate, verified and validated by the core. Needs the network and
    /// the helper built (`cargo build -p gardenwall-fetch`), so it is run
    /// by hand: `cargo test -p gardenwall-app real_download -- --ignored`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the network"]
    async fn real_download_through_the_sandboxed_helper() {
        use gardenwall_models::hub::{HubClient, HubConfig, RepoId};
        let built = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(Path::parent)
            .map(|d| d.join("gardenwall-fetch.exe"))
            .filter(|p| p.is_file())
            .expect("build gardenwall-fetch first");
        // stage_helper copies it from next to the running program.
        let beside = std::env::current_exe()
            .unwrap()
            .with_file_name("gardenwall-fetch.exe");
        std::fs::copy(&built, &beside).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let helper_dir = dir.path().join("fetch-helper");
        let journal = dir.path().join("grants");
        std::fs::create_dir(&journal).unwrap();
        let library = dir.path().join("library");
        std::fs::create_dir(&library).unwrap();

        let fetcher = std::sync::Arc::new(SandboxedFetcher {
            helper_dir: helper_dir.clone(),
            journal,
        });
        let client = HubClient::new(HubConfig::hugging_face())
            .unwrap()
            .with_fetcher(fetcher);
        let resolved = client
            .resolve(&RepoId::parse("ggml-org/models").unwrap(), "main")
            .await
            .unwrap();
        let file = resolved
            .files
            .iter()
            // Not the big-endian samples the repository also keeps.
            .filter(|f| f.size < 32 << 20 && !f.file_name.to_ascii_lowercase().contains("-be"))
            .min_by_key(|f| f.size)
            .expect("a small GGUF in ggml-org/models")
            .clone();
        let mut last = 0;
        let model = client
            .download(&file, &library, |done, _| {
                last = done;
                true
            })
            .await
            .expect("downloaded, verified and validated");
        assert_eq!(last, file.size);
        assert!(model.path.is_file());
        assert_eq!(std::fs::metadata(&model.path).unwrap().len(), file.size);
        // The helper's staging folder is gone; nothing else was written.
        let left: Vec<_> = std::fs::read_dir(&library)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    /// The gate passes bytes both ways and closes a connection on which
    /// nothing moves (review GW-020).
    #[tokio::test]
    async fn the_relay_passes_bytes_and_closes_idle_connections() {
        let (helper, mut helper_end) = tokio::io::duplex(1024);
        let (host, mut host_end) = tokio::io::duplex(1024);
        let started = std::time::Instant::now();
        let relay = tokio::spawn(relay(helper, host, Duration::from_millis(300)));
        helper_end.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        host_end.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        host_end.write_all(b"pong").await.unwrap();
        helper_end.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
        // Nothing more: the relay ends by itself.
        tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .expect("an idle connection is closed")
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn staging_sits_beside_the_partial_file() {
        let s = staging_for(Path::new("C:/lib/abc.part")).unwrap();
        assert_eq!(s, Path::new("C:/lib/.fetch-abc"));
    }
}
