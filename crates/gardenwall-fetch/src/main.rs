//! Gardenwall download helper (ADR-013 §2.6).
//!
//! Runs inside its own AppContainer with no network capability and write
//! access to one staging folder. It fetches one model file's bytes from
//! Hugging Face with the same segmented, resumable downloader the core used
//! to run itself, so the HTTP, TLS and HTTP/2 parsing of the hub's
//! responses happens here, not in the trusted core.
//!
//! Its only way out is the core: every connection reqwest makes goes to a
//! container-local loopback listener here, which passes the bytes over the
//! core's named pipe. The core reads the `CONNECT host:443` request and
//! connects only to a Hugging Face host at a public address. TLS runs end
//! to end here, so the core never sees or parses a response.
//!
//! Input: `--pipe \\.\pipe\gardenwall-<32 hex>`, and the job as JSON on
//! stdin. Output: progress as JSON lines on stdout, then `{"ok":true}` or
//! `{"error":"..."}`. The core checks the file's SHA-256 itself afterwards;
//! nothing this helper says is trusted.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        helper::main()
    }
    #[cfg(not(windows))]
    {
        eprintln!("the download helper runs on Windows only");
        ExitCode::from(2)
    }
}

#[cfg(windows)]
mod helper {
    use std::io::{Read, Write};
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use gardenwall_models::hub::{PartJob, fetch_part};
    use tokio::net::TcpListener;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

    const ERROR_PIPE_BUSY: i32 = 231;
    const PIPE_PREFIX: &str = r"\\.\pipe\gardenwall-";
    const MAX_JOB_BYTES: u64 = 64 * 1024;
    const PIPE_WAIT: Duration = Duration::from_secs(30);
    const PROGRESS_EVERY: Duration = Duration::from_millis(250);

    pub fn parse_pipe(args: &[String]) -> Result<String, String> {
        match args {
            [flag, pipe] if flag == "--pipe" => {
                let suffix = pipe.strip_prefix(PIPE_PREFIX).ok_or("invalid --pipe")?;
                if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalid --pipe".into());
                }
                Ok(pipe.clone())
            }
            _ => Err("usage: gardenwall-fetch --pipe <name> < job.json".into()),
        }
    }

    fn say(value: serde_json::Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let pipe = match parse_pipe(&args) {
            Ok(p) => p,
            Err(e) => {
                say(serde_json::json!({ "error": e }));
                return ExitCode::from(2);
            }
        };
        let mut input = String::new();
        if std::io::stdin()
            .take(MAX_JOB_BYTES)
            .read_to_string(&mut input)
            .is_err()
        {
            say(serde_json::json!({ "error": "couldn't read the job" }));
            return ExitCode::from(2);
        }
        let job: PartJob = match serde_json::from_str(&input) {
            Ok(j) => j,
            Err(_) => {
                say(serde_json::json!({ "error": "invalid job" }));
                return ExitCode::from(2);
            }
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                say(serde_json::json!({ "error": e.to_string() }));
                return ExitCode::FAILURE;
            }
        };
        match runtime.block_on(run(job, pipe)) {
            Ok(()) => {
                say(serde_json::json!({ "ok": true }));
                ExitCode::SUCCESS
            }
            Err(e) => {
                say(serde_json::json!({ "error": e }));
                ExitCode::FAILURE
            }
        }
    }

    async fn run(job: PartJob, pipe: String) -> Result<(), String> {
        // The bridge: container-local loopback in, the core's pipe out.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("loopback listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("loopback listener: {e}"))?
            .port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let pipe = pipe.clone();
                tokio::spawn(async move {
                    if let Some(mut core) = open_pipe(&pipe).await {
                        let _ = tcp.set_nodelay(true);
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut core).await;
                    }
                });
            }
        });
        let proxy = format!("http://127.0.0.1:{port}");
        let mut last = Instant::now() - PROGRESS_EVERY;
        let mut progress = |done: u64, total: u64| {
            if last.elapsed() >= PROGRESS_EVERY || done == total {
                last = Instant::now();
                say(serde_json::json!({ "done": done, "total": total }));
            }
            true
        };
        fetch_part(&job, Some(&proxy), &mut progress)
            .await
            .map_err(|e| e.to_string())
    }

    async fn open_pipe(name: &str) -> Option<NamedPipeClient> {
        let started = Instant::now();
        loop {
            match ClientOptions::new().open(name) {
                Ok(client) => return Some(client),
                // Every instance is taken; the core creates the next one.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    if started.elapsed() > PIPE_WAIT {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(_) => return None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_a_gardenwall_pipe_is_accepted() {
            let ok = format!("{PIPE_PREFIX}{}", "a".repeat(32));
            assert_eq!(parse_pipe(&["--pipe".into(), ok.clone()]).unwrap(), ok);
            for bad in [
                vec![],
                vec!["--pipe".to_string(), r"\\.\pipe\other".into()],
                vec!["--pipe".to_string(), format!("{PIPE_PREFIX}xyz")],
                vec!["--port".to_string(), "1".into()],
            ] {
                assert!(parse_pipe(&bad).is_err(), "{bad:?}");
            }
        }
    }
}
