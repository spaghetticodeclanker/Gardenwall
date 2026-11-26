//! Run an untrusted helper (page reader, binary analyzer, decompiler) in
//! its own OS sandbox (ADR-012): no network, only the folders granted,
//! bounded memory, processes and time. Input goes to stdin; stdout and a
//! short stderr come back as untrusted bytes.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// One sandboxed run.
pub struct WorkerSpec<'a> {
    /// AppContainer profile name.
    pub profile: &'a str,
    pub program: &'a Path,
    pub args: Vec<std::ffi::OsString>,
    /// Folders the helper may read and execute (its own folder at least).
    pub read_execute: Vec<PathBuf>,
    /// Folders the helper may write (scratch space only).
    pub read_write: Vec<PathBuf>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub working_dir: PathBuf,
    pub memory_bytes: u64,
    pub max_processes: u32,
    pub timeout: Duration,
    /// Most stdout bytes kept.
    pub stdout_limit: usize,
    pub journal: &'a Path,
}

/// How the helper ended.
pub struct WorkerOutput {
    /// `None` when it was stopped for taking too long.
    pub exit: Option<i32>,
    pub stdout: Vec<u8>,
    /// At most 4 KiB, for error messages.
    pub stderr: String,
}

#[cfg(any(windows, target_os = "linux"))]
pub fn run(spec: WorkerSpec<'_>, input: &[u8]) -> Result<WorkerOutput, String> {
    use std::io::{Read, Write};

    use gardenwall_sandbox::{
        Access, Limits as SandboxLimits, Network, PathGrant, Sandbox, SandboxPolicy,
    };

    let mut grants: Vec<PathGrant> = spec
        .read_execute
        .iter()
        .map(|p| PathGrant {
            path: p.clone(),
            access: Access::ReadExecute,
        })
        .collect();
    grants.extend(spec.read_write.iter().map(|p| PathGrant {
        path: p.clone(),
        access: Access::ReadWrite,
    }));
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut env = spec.env;
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot")
        && !env.iter().any(|(k, _)| k == "SystemRoot")
    {
        env.push(("SystemRoot".into(), root));
    }
    let policy = SandboxPolicy {
        profile: spec.profile.into(),
        grants,
        network: Network::Denied,
        limits: SandboxLimits {
            memory_bytes: Some(spec.memory_bytes),
            max_processes: spec.max_processes,
            wall_time: Some(spec.timeout),
        },
        env,
        working_dir: Some(spec.working_dir),
        grant_journal_dir: Some(spec.journal.to_path_buf()),
    };
    let sandbox = Sandbox::new(policy).map_err(|e| format!("the helper's sandbox failed: {e}"))?;
    let args: Vec<&std::ffi::OsStr> = spec.args.iter().map(|a| a.as_os_str()).collect();
    #[allow(unused_mut)]
    let mut child = sandbox
        .spawn(spec.program, &args)
        .map_err(|e| format!("the helper failed to start: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("helper input unavailable")?;
    let stdout = child.stdout.take().ok_or("helper output unavailable")?;
    let stderr = child.stderr.take();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let limit = spec.stdout_limit as u64;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(limit).read_to_end(&mut out);
        out
    });
    let err_reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(e) = stderr {
            let _ = e.take(4096).read_to_end(&mut out);
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    let exited = child
        .wait_timeout(Some(spec.timeout))
        .map_err(|e| e.to_string())?;
    if exited.is_none() {
        let _ = sandbox.terminate_all();
        let _ = child.wait_timeout(Some(Duration::from_secs(5)));
    }
    let _ = writer.join();
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    drop(sandbox);
    Ok(WorkerOutput {
        exit: exited.map(|c| c as i32),
        stdout,
        stderr,
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn run(_spec: WorkerSpec<'_>, _input: &[u8]) -> Result<WorkerOutput, String> {
    Err("this helper can't run on this platform (no sandbox for it yet)".into())
}
