//! Linux sandbox escape suite (ADR-012 §6). Each test runs the probe inside
//! a sandbox and checks the operation is allowed or denied.
#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gardenwall_sandbox::{
    Access, Limits, Network, PathGrant, Sandbox, SandboxError, SandboxPolicy,
};

/// Create a sandbox, or skip the test (with a message) on a kernel that has
/// no Landlock at all. The sandbox itself still refuses to run there; only
/// the test is skipped.
macro_rules! sandbox {
    ($policy:expr) => {
        match Sandbox::new($policy) {
            Ok(sb) => sb,
            Err(SandboxError::Unsupported) => {
                eprintln!("skipped: this kernel does not provide Landlock");
                return;
            }
            Err(e) => panic!("sandbox setup failed: {e}"),
        }
    };
}

struct Fixture {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    bin: PathBuf,
    probe: PathBuf,
}

fn probe_source() -> PathBuf {
    // Test binaries live in target/<triple>/debug/deps; the probe one level up.
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    dir.join("sandbox-probe")
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        let bin = tmp.path().join("bin");
        for d in [&workspace, &outside, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(outside.join("secret.txt"), b"outside secret").unwrap();
        std::fs::write(workspace.join("inside.txt"), b"inside").unwrap();
        let probe = bin.join("sandbox-probe");
        std::fs::copy(probe_source(), &probe).unwrap();
        Self {
            _tmp: tmp,
            workspace,
            outside,
            bin,
            probe,
        }
    }

    fn policy(&self) -> SandboxPolicy {
        SandboxPolicy {
            profile: "Gardenwall.Test".into(),
            grants: vec![
                PathGrant {
                    path: self.workspace.clone(),
                    access: Access::ReadWrite,
                },
                PathGrant {
                    path: self.bin.clone(),
                    access: Access::ReadExecute,
                },
            ]
            .into_iter()
            .chain(system_libraries())
            .collect(),
            network: Network::Denied,
            limits: Limits::default(),
            env: vec![("GW_TEST".into(), "1".into())],
            working_dir: Some(self.workspace.clone()),
            grant_journal_dir: None,
        }
    }
}

/// Read-only access to the dynamic loader and shared libraries, so a
/// dynamically linked probe can start (CI builds one; musl builds are static).
fn system_libraries() -> Vec<PathGrant> {
    [
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/lib64",
        "/etc/ld.so.cache",
    ]
    .iter()
    .filter(|p| Path::new(p).exists())
    .map(|p| PathGrant {
        path: PathBuf::from(p),
        access: Access::ReadExecute,
    })
    .collect()
}

/// Retry while Linux reports "Text file busy": a process another test
/// thread is starting can briefly hold an inherited write handle to a
/// freshly copied probe.
fn retry_busy<T, E: std::fmt::Debug>(
    mut f: impl FnMut() -> Result<T, E>,
    is_busy: impl Fn(&E) -> bool,
) -> T {
    for _ in 0..50 {
        match f() {
            Err(e) if is_busy(&e) => std::thread::sleep(Duration::from_millis(20)),
            other => return other.unwrap(),
        }
    }
    f().unwrap()
}

fn probe(sandbox: &Sandbox, fx: &Fixture, args: &[&str]) -> (u32, String) {
    let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let mut child = retry_busy(
        || sandbox.spawn(&fx.probe, &os),
        |e| {
            matches!(
                e,
                SandboxError::Io(_, std::io::ErrorKind::ExecutableFileBusy)
            )
        },
    );
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let code = child
        .wait_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .expect("probe timed out");
    (code, out.lines().next().unwrap_or_default().to_string())
}

fn assert_ok(r: (u32, String), what: &str) {
    assert!(
        r.0 == 0 && r.1.starts_with("OK"),
        "{what} should be allowed: {r:?}"
    );
}

fn assert_denied(r: (u32, String), what: &str) {
    assert!(
        r.0 != 0 && !r.1.starts_with("OK"),
        "{what} should be denied: {r:?}"
    );
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn granted_workspace_is_readable_and_writable() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    assert_ok(
        probe(&sb, &fx, &["read", s(&fx.workspace.join("inside.txt"))]),
        "read inside",
    );
    assert_ok(
        probe(&sb, &fx, &["write", s(&fx.workspace.join("new.txt"))]),
        "write inside",
    );
    assert_ok(probe(&sb, &fx, &["list", s(&fx.workspace)]), "list inside");
    assert_eq!(
        std::fs::read(fx.workspace.join("new.txt")).unwrap(),
        b"probe"
    );
}

#[test]
fn locations_outside_grants_are_denied() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    assert_denied(
        probe(&sb, &fx, &["read", s(&fx.outside.join("secret.txt"))]),
        "read outside",
    );
    assert_denied(
        probe(&sb, &fx, &["write", s(&fx.outside.join("planted.txt"))]),
        "write outside",
    );
    assert_denied(probe(&sb, &fx, &["list", s(&fx.outside)]), "list outside");
    assert_denied(
        probe(&sb, &fx, &["read", "/etc/passwd"]),
        "read /etc/passwd",
    );
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    assert_denied(probe(&sb, &fx, &["list", &home]), "list home");
    assert_denied(
        probe(&sb, &fx, &["write", "/tmp/gardenwall-escape-probe"]),
        "write /tmp",
    );
    assert!(!fx.outside.join("planted.txt").exists());
    assert!(!Path::new("/tmp/gardenwall-escape-probe").exists());
}

#[test]
fn links_cannot_reach_outside_files() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    // A hard link to an outside file, made from inside, is refused.
    assert_denied(
        probe(
            &sb,
            &fx,
            &[
                "hardlink",
                s(&fx.outside.join("secret.txt")),
                s(&fx.workspace.join("h.txt")),
            ],
        ),
        "hard link to outside file",
    );
    // A symlink placed in the workspace does not open its outside target.
    std::os::unix::fs::symlink(fx.outside.join("secret.txt"), fx.workspace.join("link.txt"))
        .unwrap();
    assert_denied(
        probe(&sb, &fx, &["read", s(&fx.workspace.join("link.txt"))]),
        "read through symlink",
    );
}

#[test]
fn network_is_denied() {
    let fx = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().to_string();
    let sb = sandbox!(fx.policy());
    assert_denied(probe(&sb, &fx, &["connect", &local]), "loopback connect");
    assert_denied(
        probe(&sb, &fx, &["connect", "1.1.1.1:443"]),
        "internet connect",
    );
    assert_denied(probe(&sb, &fx, &["resolve", "example.com"]), "DNS");
}

#[test]
fn environment_is_not_inherited() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    assert_ok(probe(&sb, &fx, &["env", "GW_TEST"]), "explicit variable");
    for name in ["PATH", "HOME", "USER"] {
        assert_denied(probe(&sb, &fx, &["env", name]), name);
    }
}

#[test]
fn process_creation_follows_policy_and_children_stay_confined() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    assert_denied(probe(&sb, &fx, &["spawn"]), "spawn with max_processes = 1");

    let mut policy = fx.policy();
    policy.limits.max_processes = 4;
    let sb = sandbox!(policy);
    assert_ok(probe(&sb, &fx, &["spawn"]), "spawn with max_processes = 4");
}

#[test]
fn namespaces_and_privilege_tricks_are_denied() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    assert_denied(probe(&sb, &fx, &["unshare"]), "unshare(CLONE_NEWUSER)");
}

#[test]
fn memory_limit_is_enforced() {
    let fx = Fixture::new();
    let mut policy = fx.policy();
    policy.limits.memory_bytes = Some(256 << 20);
    let sb = sandbox!(policy);
    assert_ok(probe(&sb, &fx, &["alloc", "16"]), "small allocation");
    assert_denied(
        probe(&sb, &fx, &["alloc", "1024"]),
        "allocation over the limit",
    );
}

#[test]
fn dropping_the_sandbox_kills_its_processes() {
    let fx = Fixture::new();
    let sb = sandbox!(fx.policy());
    let mut child = sb
        .spawn(&fx.probe, &[OsStr::new("sleep"), OsStr::new("30")])
        .unwrap();
    assert_eq!(
        child
            .wait_timeout(Some(Duration::from_millis(200)))
            .unwrap(),
        None
    );
    drop(sb);
    let code = child.wait_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(code, Some(128 + libc_sigkill()), "process should be killed");
}

fn libc_sigkill() -> u32 {
    9
}

#[test]
fn controls_succeed_without_sandbox() {
    // The same probe operations work unsandboxed, so denials above come
    // from the sandbox, not from a broken probe.
    let fx = Fixture::new();
    let run = |args: &[&str]| {
        let out = retry_busy(
            || std::process::Command::new(&fx.probe).args(args).output(),
            |e| e.kind() == std::io::ErrorKind::ExecutableFileBusy,
        );
        (
            out.status.code().unwrap_or(-1) as u32,
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        )
    };
    assert_ok(
        run(&["read", s(&fx.outside.join("secret.txt"))]),
        "control read",
    );
    assert_ok(run(&["list", s(&fx.outside)]), "control list");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert_ok(
        run(&["connect", &listener.local_addr().unwrap().to_string()]),
        "control connect",
    );
    assert_ok(run(&["spawn"]), "control spawn");
}
