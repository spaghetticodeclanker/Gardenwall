//! Test helper for the sandbox escape suite (ADR-012 §6). Not shipped.
//!
//! Performs one operation and reports `OK <detail>` (exit 0) or
//! `DENIED <error>` (exit 10).
//!
//! Operations:
//!   read <path> | write <path> | list <dir> | hardlink <existing> <new>
//!   rename <from> <to> | remove <path> | mkdir <path> | readonly <path>
//!   connect <addr:port> | resolve <host> | spawn | alloc <MiB>
//!   env <NAME> | sleep <secs> | noop | pipe <name> | unshare

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;

fn ok(detail: impl std::fmt::Display) -> ExitCode {
    println!("OK {detail}");
    ExitCode::SUCCESS
}

fn denied(error: impl std::fmt::Display) -> ExitCode {
    println!("DENIED {error}");
    ExitCode::from(10)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    let code = match arg(0) {
        "noop" => ok("noop"),
        "read" => match std::fs::read(arg(1)) {
            Ok(bytes) => ok(bytes.len()),
            Err(e) => denied(e),
        },
        "write" => match std::fs::File::create(arg(1)).and_then(|mut f| f.write_all(b"probe")) {
            Ok(()) => ok("written"),
            Err(e) => denied(e),
        },
        "rename" => match std::fs::rename(arg(1), arg(2)) {
            Ok(()) => ok("renamed"),
            Err(e) => denied(e),
        },
        "remove" => {
            let p = std::path::Path::new(arg(1));
            let r = if p.is_dir() {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
            match r {
                Ok(()) => ok("removed"),
                Err(e) => denied(e),
            }
        }
        "mkdir" => match std::fs::create_dir(arg(1)) {
            Ok(()) => ok("created"),
            Err(e) => denied(e),
        },
        "readonly" => match std::fs::metadata(arg(1)).and_then(|m| {
            let mut p = m.permissions();
            p.set_readonly(!p.readonly());
            std::fs::set_permissions(arg(1), p)
        }) {
            Ok(()) => ok("attributes changed"),
            Err(e) => denied(e),
        },
        "list" => match std::fs::read_dir(arg(1)) {
            Ok(entries) => ok(entries.count()),
            Err(e) => denied(e),
        },
        "hardlink" => match std::fs::hard_link(arg(1), arg(2)) {
            Ok(()) => ok("linked"),
            Err(e) => denied(e),
        },
        "connect" => match arg(1).parse() {
            Ok(addr) => match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                Ok(_) => ok("connected"),
                Err(e) => denied(e),
            },
            Err(e) => denied(e),
        },
        "resolve" => match (arg(1), 443).to_socket_addrs() {
            Ok(mut addrs) => match addrs.next() {
                Some(a) => ok(a),
                None => denied("no addresses"),
            },
            Err(e) => denied(e),
        },
        // Starts a child without capturing its output: the standard library
        // captures output through named pipes, which some Windows versions
        // refuse to create inside an AppContainer. Only process creation is
        // being tested.
        "spawn" => match std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg("noop")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        }) {
            Ok(status) if status.success() => ok("child ran"),
            Ok(status) => denied(format!("child exited {:?}", status.code())),
            Err(e) => denied(e),
        },
        "alloc" => {
            let mib: usize = arg(1).parse().unwrap_or(0);
            let mut v: Vec<u8> = Vec::new();
            match v.try_reserve_exact(mib << 20) {
                Ok(()) => {
                    v.resize(mib << 20, 1);
                    ok(v.len())
                }
                Err(e) => denied(e),
            }
        }
        "env" => match std::env::var_os(arg(1)) {
            Some(v) => ok(v.to_string_lossy()),
            None => denied("unset"),
        },
        "pipe" => {
            use std::io::Read;
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(arg(1))
            {
                Ok(mut p) => {
                    let mut reply = [0u8; 4];
                    match p.write_all(b"ping").and_then(|()| p.read_exact(&mut reply)) {
                        Ok(()) => ok(String::from_utf8_lossy(&reply)),
                        Err(e) => denied(e),
                    }
                }
                Err(e) => denied(e),
            }
        }
        "unshare" => {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: unshare takes only flags.
                if unsafe { libc::unshare(libc::CLONE_NEWUSER) } == 0 {
                    ok("new user namespace")
                } else {
                    denied(std::io::Error::last_os_error())
                }
            }
            #[cfg(not(target_os = "linux"))]
            denied("not applicable")
        }
        "sleep" => {
            std::thread::sleep(Duration::from_secs(arg(1).parse().unwrap_or(1)));
            ok("slept")
        }
        other => denied(format!("unknown operation {other:?}")),
    };
    let _ = std::io::stdout().flush();
    code
}
