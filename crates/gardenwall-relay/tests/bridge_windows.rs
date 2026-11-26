//! Behavioural tests for the relay binary (run unsandboxed; sandbox
//! containment is covered by gardenwall-sandbox's escape suite).

#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

fn pipe_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        r"\\.\pipe\gardenwall-{:032x}",
        nanos ^ (u128::from(std::process::id()) << 64)
    )
}

/// Mock runtime: answers one HTTP request per connection and closes.
async fn mock_runtime() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    (port, accepted)
}

fn start_relay(pipe: &str, port: u16, max: usize) -> Child {
    Command::new(env!("CARGO_BIN_EXE_gardenwall-relay"))
        .args([
            "--pipe",
            pipe,
            "--port",
            &port.to_string(),
            "--max-connections",
            &max.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn instance(name: &str, first: bool) -> NamedPipeServer {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(name)
        .unwrap()
}

async fn read_all(pipe: &mut NamedPipeServer) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), pipe.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return out,
            Ok(Ok(n)) => out.extend_from_slice(&chunk[..n]),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bridges_lazily_bounds_connections_and_exits_with_core() {
    let (port, accepted) = mock_runtime().await;
    let name = pipe_name();
    let mut pending = instance(&name, true);
    let mut relay = start_relay(&name, port, 2);

    // 1. The relay connects to the pipe, but not to the runtime until a
    //    request arrives.
    tokio::time::timeout(Duration::from_secs(10), pending.connect())
        .await
        .expect("relay did not connect")
        .unwrap();
    let mut conn = std::mem::replace(&mut pending, instance(&name, false));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "runtime contacted before any request"
    );

    // 2. A request is bridged; the response arrives and the connection ends
    //    when the runtime closes (no half-close on pipes).
    conn.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let response = read_all(&mut conn).await;
    assert!(
        response.ends_with(b"ok"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    drop(conn);

    // 3. At most two connections exist at once: two instances get clients,
    //    a third does not while those two are held open.
    let mut held = Vec::new();
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), pending.connect())
            .await
            .expect("relay should open up to max connections")
            .unwrap();
        held.push(std::mem::replace(&mut pending, instance(&name, false)));
    }
    let third = tokio::time::timeout(Duration::from_millis(700), pending.connect()).await;
    assert!(third.is_err(), "relay exceeded --max-connections");

    // 4. When the core's pipe disappears, the relay exits.
    drop(held);
    drop(pending);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = relay.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "relay did not exit"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_invalid_arguments() {
    let out = Command::new(env!("CARGO_BIN_EXE_gardenwall-relay"))
        .args(["--pipe", r"\\.\pipe\evil", "--port", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64));
}
