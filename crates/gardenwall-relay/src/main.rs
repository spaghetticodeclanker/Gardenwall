//! Gardenwall relay (ADR-012 §4.1, SPIKE-002).
//!
//! Runs **inside** the inference AppContainer, alongside `llama-server`. It
//! connects out to the trusted core's named pipe and bridges each pipe
//! connection to the runtime's container-local loopback port. It forwards
//! bytes unmodified, holds no secrets, and is treated as untrusted.
//!
//! Behaviour:
//! - Strict arguments: `--pipe \\.\pipe\gardenwall-<32 hex> --port <1-65535>
//!   [--max-connections <1-64>]`.
//! - At most `max-connections` pipe connections exist at once.
//! - The TCP connection to the runtime is opened lazily, when the first
//!   bytes of a request arrive, so idle pre-opened pipe connections cannot
//!   go stale on the runtime's keep-alive timeout.
//! - Unanswered loopback connects time out (rather than being refused) inside
//!   an AppContainer, so connects use short timeouts and bounded retries.
//! - Named pipes have no half-close: a connection ends entirely as soon as
//!   either direction finishes.
//! - The relay exits when the core's pipe disappears.
//!
//! Nothing about traffic content is logged.

#[cfg(windows)]
mod relay {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use tokio::sync::Semaphore;

    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    const PIPE_PREFIX: &str = r"\\.\pipe\gardenwall-";
    /// How long to wait for the core's pipe to appear at startup.
    const STARTUP_WAIT: Duration = Duration::from_secs(60);
    /// How long the pipe may be missing after it existed before exiting.
    const GONE_WAIT: Duration = Duration::from_secs(5);
    const CONNECT_ATTEMPT: Duration = Duration::from_millis(500);
    const CONNECT_DEADLINE: Duration = Duration::from_secs(120);
    const FIRST_CHUNK: usize = 64 * 1024;

    #[derive(Debug, PartialEq, Eq)]
    pub struct Args {
        pub pipe: String,
        pub port: u16,
        pub max_connections: usize,
    }

    pub fn parse_args(args: &[String]) -> Result<Args, String> {
        let mut pipe = None;
        let mut port = None;
        let mut max_connections = 8usize;
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--pipe" => pipe = Some(value.clone()),
                "--port" => port = Some(value.parse::<u16>().map_err(|_| "invalid --port")?),
                "--max-connections" => {
                    max_connections = value.parse().map_err(|_| "invalid --max-connections")?;
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let pipe = pipe.ok_or("missing --pipe")?;
        let suffix = pipe.strip_prefix(PIPE_PREFIX).ok_or("invalid --pipe")?;
        if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid --pipe".into());
        }
        let port = port.ok_or("missing --port")?;
        if port == 0 {
            return Err("invalid --port".into());
        }
        if !(1..=64).contains(&max_connections) {
            return Err("invalid --max-connections".into());
        }
        Ok(Args {
            pipe,
            port,
            max_connections,
        })
    }

    enum Open {
        Connected(NamedPipeClient),
        Gone,
        Fatal(std::io::Error),
    }

    async fn open_pipe(name: &str, ever_connected: bool) -> Open {
        let started = Instant::now();
        let limit = if ever_connected {
            GONE_WAIT
        } else {
            STARTUP_WAIT
        };
        loop {
            match ClientOptions::new().open(name) {
                Ok(client) => return Open::Connected(client),
                // Busy: every instance is taken; the core will create more.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                    if started.elapsed() > limit {
                        return Open::Gone;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Open::Fatal(e),
            }
        }
    }

    async fn connect_runtime(port: u16) -> Option<TcpStream> {
        let started = Instant::now();
        while started.elapsed() < CONNECT_DEADLINE {
            if let Ok(Ok(stream)) =
                tokio::time::timeout(CONNECT_ATTEMPT, TcpStream::connect(("127.0.0.1", port))).await
            {
                let _ = stream.set_nodelay(true);
                return Some(stream);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None
    }

    async fn bridge(mut pipe: NamedPipeClient, port: u16) {
        // Wait for the core to send a request before touching the runtime.
        let mut first = vec![0u8; FIRST_CHUNK];
        let n = match pipe.read(&mut first).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let Some(mut tcp) = connect_runtime(port).await else {
            return;
        };
        if tcp.write_all(&first[..n]).await.is_err() {
            return;
        }
        let (mut pipe_r, mut pipe_w) = tokio::io::split(pipe);
        let (mut tcp_r, mut tcp_w) = tcp.split();
        tokio::select! {
            _ = tokio::io::copy(&mut pipe_r, &mut tcp_w) => {}
            _ = tokio::io::copy(&mut tcp_r, &mut pipe_w) => {}
        }
    }

    /// Returns the process exit code.
    pub async fn run(args: Args) -> u8 {
        let permits = Arc::new(Semaphore::new(args.max_connections));
        let mut ever_connected = false;
        loop {
            let Ok(permit) = permits.clone().acquire_owned().await else {
                return 1;
            };
            match open_pipe(&args.pipe, ever_connected).await {
                Open::Connected(client) => {
                    ever_connected = true;
                    let port = args.port;
                    tokio::spawn(async move {
                        bridge(client, port).await;
                        drop(permit);
                    });
                }
                Open::Gone => return if ever_connected { 0 } else { 3 },
                Open::Fatal(e) => {
                    eprintln!("gardenwall-relay: cannot open pipe ({:?})", e.kind());
                    return 2;
                }
            }
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match relay::parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gardenwall-relay: {e}");
            return std::process::ExitCode::from(64);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return std::process::ExitCode::from(1),
    };
    std::process::ExitCode::from(runtime.block_on(relay::run(args)))
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("gardenwall-relay is only used on Windows");
    std::process::ExitCode::from(1)
}

#[cfg(all(test, windows))]
mod tests {
    use super::relay::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    const PIPE: &str = r"\\.\pipe\gardenwall-0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_valid_arguments() {
        assert_eq!(
            parse_args(&args(&["--pipe", PIPE, "--port", "8080"])),
            Ok(Args {
                pipe: PIPE.into(),
                port: 8080,
                max_connections: 8
            })
        );
        assert_eq!(
            parse_args(&args(&[
                "--port",
                "1",
                "--pipe",
                PIPE,
                "--max-connections",
                "2"
            ]))
            .unwrap()
            .max_connections,
            2
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            vec!["--pipe", PIPE],
            vec!["--port", "80"],
            vec!["--pipe", PIPE, "--port", "0"],
            vec!["--pipe", PIPE, "--port", "70000"],
            vec!["--pipe", r"\\.\pipe\other", "--port", "80"],
            vec!["--pipe", r"\\.\pipe\gardenwall-xyz", "--port", "80"],
            vec![
                "--pipe",
                r"\\server\pipe\gardenwall-0123456789abcdef0123456789abcdef",
                "--port",
                "80",
            ],
            vec!["--pipe", PIPE, "--port", "80", "--max-connections", "0"],
            vec!["--pipe", PIPE, "--port", "80", "--extra", "x"],
            vec!["--pipe"],
        ] {
            assert!(parse_args(&args(&bad)).is_err(), "{bad:?}");
        }
    }
}
