//! Sandbox escape suite for Windows (ADR-012 §6).
//!
//! Each test runs `sandbox-probe` inside a fresh AppContainer and asserts that
//! the attempted operation is denied (or, for granted locations, allowed).

#![cfg(windows)]

use std::ffi::OsStr;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gardenwall_sandbox::windows::delete_profile;
use gardenwall_sandbox::{
    Access, Limits, Network, PathGrant, PolicyError, Sandbox, SandboxError, SandboxPolicy,
};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    runtime: PathBuf,
    workspace: PathBuf,
    outside: PathBuf,
    probe: PathBuf,
    profile: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let runtime = root.join("runtime");
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        for d in [&runtime, &workspace, &outside] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(outside.join("secret.txt"), b"top secret").unwrap();
        std::fs::write(workspace.join("hello.txt"), b"hello").unwrap();
        let probe = runtime.join("sandbox-probe.exe");
        std::fs::copy(env!("CARGO_BIN_EXE_sandbox-probe"), &probe).unwrap();
        Self {
            _dir: dir,
            root,
            runtime,
            workspace,
            outside,
            probe,
            profile: format!("Gardenwall.Test.{name}"),
        }
    }

    fn policy(&self) -> SandboxPolicy {
        SandboxPolicy {
            profile: self.profile.clone(),
            grants: vec![
                PathGrant {
                    path: self.runtime.clone(),
                    access: Access::ReadExecute,
                },
                PathGrant {
                    path: self.workspace.clone(),
                    access: Access::ReadWrite,
                },
            ],
            network: Network::Denied,
            limits: Limits::default(),
            env: vec![
                ("GW_TEST".into(), "visible".into()),
                (
                    "SystemRoot".into(),
                    std::env::var_os("SystemRoot").unwrap_or_default(),
                ),
            ],
            working_dir: Some(self.workspace.clone()),
            grant_journal_dir: None,
        }
    }

    fn sandbox(&self) -> Sandbox {
        Sandbox::new(self.policy()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = delete_profile(&self.profile);
    }
}

/// Run the probe; returns (exit code, first stdout line).
fn probe(sandbox: &Sandbox, fx: &Fixture, args: &[&str]) -> (u32, String) {
    let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let mut child = sandbox.spawn(&fx.probe, &os).unwrap();
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

fn assert_ok(result: (u32, String), what: &str) {
    assert!(
        result.0 == 0 && result.1.starts_with("OK"),
        "{what} should be allowed: {result:?}"
    );
}

fn assert_denied(result: (u32, String), what: &str) {
    assert!(
        result.0 != 0 && !result.1.starts_with("OK"),
        "{what} should be denied: {result:?}"
    );
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn granted_workspace_is_readable_and_writable() {
    let fx = Fixture::new("Workspace");
    let sb = fx.sandbox();
    assert_ok(
        probe(&sb, &fx, &["read", s(&fx.workspace.join("hello.txt"))]),
        "read workspace",
    );
    assert_ok(
        probe(&sb, &fx, &["write", s(&fx.workspace.join("new.txt"))]),
        "write workspace",
    );
    assert_ok(
        probe(&sb, &fx, &["write", "relative.txt"]),
        "write relative to cwd",
    );
    assert_eq!(
        std::fs::read(fx.workspace.join("new.txt")).unwrap(),
        b"probe"
    );
}

#[test]
fn locations_outside_grants_are_denied() {
    let fx = Fixture::new("Outside");
    let sb = fx.sandbox();
    let secret = fx.outside.join("secret.txt");
    assert_denied(
        probe(&sb, &fx, &["read", s(&secret)]),
        "read sibling of workspace",
    );
    assert_denied(
        probe(&sb, &fx, &["write", s(&fx.outside.join("x.txt"))]),
        "write sibling",
    );
    let dotdot = fx.workspace.join("..").join("outside").join("y.txt");
    assert_denied(probe(&sb, &fx, &["write", s(&dotdot)]), "write via ..");
    assert_denied(
        probe(&sb, &fx, &["list", s(&fx.root)]),
        "list parent of workspace",
    );
    assert_denied(
        probe(&sb, &fx, &["write", s(&fx.runtime.join("planted.dll"))]),
        "write read-only grant",
    );
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        assert_denied(
            probe(&sb, &fx, &["list", &profile.to_string_lossy()]),
            "list user profile",
        );
    }
    assert!(!fx.outside.join("x.txt").exists());
    assert!(!fx.runtime.join("planted.dll").exists());
}

#[test]
fn hard_links_cannot_reach_outside_files() {
    let fx = Fixture::new("Hardlink");
    let sb = fx.sandbox();
    let secret = fx.outside.join("secret.txt");
    let link = fx.workspace.join("link.txt");
    assert_denied(
        probe(&sb, &fx, &["hardlink", s(&secret), s(&link)]),
        "create hard link to outside file",
    );
    // Even a link planted by someone else after the grant must not be readable.
    std::fs::hard_link(&secret, fx.workspace.join("planted.txt")).unwrap();
    assert_denied(
        probe(&sb, &fx, &["read", s(&fx.workspace.join("planted.txt"))]),
        "read hard link planted after grant",
    );
}

#[test]
fn grants_refuse_trees_with_links() {
    let fx = Fixture::new("RefuseLinks");
    std::fs::hard_link(fx.outside.join("secret.txt"), fx.workspace.join("link.txt")).unwrap();
    assert!(matches!(
        Sandbox::new(fx.policy()),
        Err(SandboxError::Policy(PolicyError::MultiplyLinked(_)))
    ));
    std::fs::remove_file(fx.workspace.join("link.txt")).unwrap();

    let junction = fx.workspace.join("junction");
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&fx.outside)
        .output()
        .unwrap();
    assert!(status.status.success(), "mklink failed: {status:?}");
    assert!(matches!(
        Sandbox::new(fx.policy()),
        Err(SandboxError::Policy(PolicyError::ReparsePoint(_)))
    ));
    // The refusal must not have modified the outside directory's ACL.
    std::fs::remove_dir(&junction).unwrap();
}

fn acl_of(path: &Path) -> String {
    let out = Command::new("icacls").arg(path).output().unwrap();
    assert!(out.status.success(), "icacls failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A read-only folder every AppContainer can already read (like Program
/// Files, where Git for Windows keeps hard-linked programs) may hold hard
/// links, because the sandbox adds no permission to it: the linked file
/// stays exactly as unreadable as before, and no ACL changes. A folder that
/// does get a permission still refuses links (ADR-011 §2.5).
#[test]
fn already_readable_tool_folders_may_hold_links_but_add_nothing() {
    let fx = Fixture::new("ReadableLinks");
    let shared = fx.root.join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(shared.join("tool.txt"), b"tool").unwrap();
    // Make it readable by ALL APPLICATION PACKAGES, as Program Files is.
    let out = Command::new("icacls")
        .arg(&shared)
        .args(["/grant", "*S-1-15-2-1:(OI)(CI)(RX)"])
        .output()
        .unwrap();
    assert!(out.status.success(), "icacls grant failed: {out:?}");
    std::fs::hard_link(fx.outside.join("secret.txt"), shared.join("link.txt")).unwrap();
    let before = (
        acl_of(&shared),
        acl_of(&shared.join("link.txt")),
        acl_of(&fx.outside.join("secret.txt")),
    );

    let mut policy = fx.policy();
    policy.grants.push(PathGrant {
        path: shared.clone(),
        access: Access::ReadExecute,
    });
    let sb = Sandbox::new(policy).expect("an already-readable folder needs no link check");
    assert_ok(
        probe(&sb, &fx, &["read", s(&shared.join("tool.txt"))]),
        "read a file in the already-readable folder",
    );
    assert_denied(
        probe(&sb, &fx, &["read", s(&shared.join("link.txt"))]),
        "read the outside file through its link",
    );
    assert_denied(
        probe(&sb, &fx, &["read", s(&fx.outside.join("secret.txt"))]),
        "read the outside file by its own name",
    );
    let during = (
        acl_of(&shared),
        acl_of(&shared.join("link.txt")),
        acl_of(&fx.outside.join("secret.txt")),
    );
    assert_eq!(before, during, "no permission may be added");
    drop(sb);

    // The same link in a read-only folder that does need a permission.
    std::fs::hard_link(fx.outside.join("secret.txt"), fx.runtime.join("link.txt")).unwrap();
    assert!(matches!(
        Sandbox::new(fx.policy()),
        Err(SandboxError::Policy(PolicyError::MultiplyLinked(_)))
    ));
}

#[test]
fn junction_created_after_grant_does_not_open_target() {
    let fx = Fixture::new("LateJunction");
    let sb = fx.sandbox();
    let junction = fx.workspace.join("late");
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&fx.outside)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_denied(
        probe(&sb, &fx, &["read", s(&junction.join("secret.txt"))]),
        "read through junction",
    );
    assert_denied(
        probe(&sb, &fx, &["write", s(&junction.join("z.txt"))]),
        "write through junction",
    );
    std::fs::remove_dir(&junction).unwrap();
}

#[test]
fn network_is_denied() {
    let fx = Fixture::new("Network");
    let sb = fx.sandbox();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().to_string();
    assert_denied(
        probe(&sb, &fx, &["connect", &local]),
        "connect to host loopback service",
    );
    assert_denied(
        probe(&sb, &fx, &["connect", "1.1.1.1:443"]),
        "connect to internet IP",
    );
    assert_denied(
        probe(&sb, &fx, &["resolve", "example.com"]),
        "DNS resolution",
    );
}

#[test]
fn environment_is_not_inherited() {
    let fx = Fixture::new("Env");
    let sb = fx.sandbox();
    assert_ok(probe(&sb, &fx, &["env", "GW_TEST"]), "policy variable");
    for name in ["PATH", "USERPROFILE", "USERNAME", "APPDATA"] {
        assert_denied(probe(&sb, &fx, &["env", name]), name);
    }
    // Windows substitutes the container's private folder for LOCALAPPDATA.
    let (code, line) = probe(&sb, &fx, &["env", "LOCALAPPDATA"]);
    assert_eq!(code, 0, "{line}");
    let real = std::env::var("LOCALAPPDATA").unwrap();
    assert_ne!(
        line.trim_start_matches("OK ").trim(),
        real,
        "real LOCALAPPDATA leaked"
    );
    assert!(line.contains("Packages"), "unexpected LOCALAPPDATA: {line}");
}

#[test]
fn child_processes_follow_policy() {
    let fx = Fixture::new("Children");
    let sb = fx.sandbox();
    assert_denied(probe(&sb, &fx, &["spawn"]), "spawn with max_processes = 1");
    drop(sb);

    // The "children allowed" half cannot run on GitHub's Windows Server
    // runners: process creation from inside a capability-less AppContainer
    // is refused there (it works on Windows 11 desktops, where this check
    // runs in full). The "children denied" half above always runs.
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        eprintln!(
            "skipped: child processes in AppContainers are refused on GitHub's Windows runners"
        );
        return;
    }
    let mut policy = fx.policy();
    policy.limits.max_processes = 4;
    let mut sb = Sandbox::new(policy).unwrap();
    // As the agent's shell runs: children share a hidden console. (In a
    // session without a desktop, such as CI, a console-less sandboxed
    // process cannot start console children at all.)
    sb.use_hidden_console();
    assert_ok(probe(&sb, &fx, &["spawn"]), "spawn with max_processes = 4");
}

#[test]
fn memory_limit_is_enforced() {
    let fx = Fixture::new("Memory");
    let mut policy = fx.policy();
    policy.limits.memory_bytes = Some(128 << 20);
    let sb = Sandbox::new(policy).unwrap();
    assert_ok(probe(&sb, &fx, &["alloc", "16"]), "small allocation");
    assert_denied(
        probe(&sb, &fx, &["alloc", "512"]),
        "allocation over the job limit",
    );
}

#[test]
fn dropping_the_sandbox_kills_processes_and_revokes_grants() {
    let fx = Fixture::new("Teardown");
    let sb = fx.sandbox();
    let sid = sb.sid_string().unwrap();
    let child = sb
        .spawn(&fx.probe, &[OsStr::new("sleep"), OsStr::new("60")])
        .unwrap();
    assert_eq!(
        child
            .wait_timeout(Some(Duration::from_millis(200)))
            .unwrap(),
        None
    );

    let acl = |p: &Path| {
        String::from_utf8_lossy(&Command::new("icacls").arg(p).output().unwrap().stdout)
            .into_owned()
    };
    assert!(acl(&fx.workspace).contains(&sid), "grant should be present");

    drop(sb);
    let code = child.wait_timeout(Some(Duration::from_secs(5))).unwrap();
    assert!(code.is_some(), "process should be killed with the job");
    assert!(
        !acl(&fx.workspace).contains(&sid),
        "grant should be revoked"
    );
    assert!(
        !acl(&fx.workspace.join("hello.txt")).contains(&sid),
        "inherited ACE should be revoked"
    );
}

/// Controls: the same probe operations succeed without the sandbox, so the
/// denials above are caused by the sandbox and not by the probe or fixture.
#[test]
fn controls_succeed_without_sandbox() {
    let fx = Fixture::new("Controls");
    let run = |args: &[&str]| {
        let out = Command::new(&fx.probe).args(args).output().unwrap();
        let line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        (out.status.code().unwrap_or(-1) as u32, line)
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().to_string();
    assert_ok(run(&["connect", &local]), "unsandboxed loopback connect");
    assert_ok(
        run(&["read", s(&fx.outside.join("secret.txt"))]),
        "unsandboxed read",
    );
    assert_ok(run(&["list", s(&fx.root)]), "unsandboxed list");
    assert_ok(run(&["spawn"]), "unsandboxed spawn");
    assert_ok(run(&["alloc", "512"]), "unsandboxed allocation");
}

#[test]
fn pipe_listener_admits_only_its_own_sandbox() {
    use gardenwall_sandbox::PipeListener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let fx = Fixture::new("Pipe");
        let sb = fx.sandbox();
        let mut listener = PipeListener::for_sandbox(&sb).unwrap();
        let name = listener.name().to_string();

        // Same sandbox: connects, exchanges data, and reports its own PID.
        let mut child = sb
            .spawn(&fx.probe, &[OsStr::new("pipe"), OsStr::new(&name)])
            .unwrap();
        let (mut conn, pid) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("probe did not connect")
            .unwrap();
        assert_eq!(
            pid,
            child.pid(),
            "client PID must identify the spawned process"
        );
        let mut ping = [0u8; 4];
        conn.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"ping");
        conn.write_all(b"pong").await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let out = tokio::task::spawn_blocking(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).unwrap();
            s
        })
        .await
        .unwrap();
        assert!(out.starts_with("OK pong"), "{out}");

        // Different sandbox: the OS denies opening the pipe.
        let fx2 = Fixture::new("PipeOther");
        let other = fx2.sandbox();
        let result = tokio::task::spawn_blocking({
            let probe = fx2.probe.clone();
            let name = name.clone();
            move || {
                let mut c = other
                    .spawn(&probe, &[OsStr::new("pipe"), OsStr::new(&name)])
                    .unwrap();
                let mut s = String::new();
                c.stdout.take().unwrap().read_to_string(&mut s).unwrap();
                s
            }
        })
        .await
        .unwrap();
        assert!(
            result.starts_with("DENIED"),
            "other sandbox must be denied: {result}"
        );
        assert!(
            result.contains("denied") || result.contains("os error 5"),
            "{result}"
        );
    });
}

/// Private drive letters: `dir` and PowerShell work on the mapped project,
/// nothing outside it becomes reachable, and the letter isn't visible
/// outside the sandbox.
#[test]
fn private_drive_letters_work_and_grant_nothing() {
    let fx = Fixture::new("DriveMap");
    let mut policy = fx.policy();
    policy.limits.max_processes = 8;
    let mut sb = Sandbox::new(policy).unwrap();
    sb.use_hidden_console();
    sb.map_drives(&[('W', fx.workspace.clone())]).unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let run_cmd = |line: &str| -> (u32, String) {
        let mut c = sb
            .spawn_raw(&system.join("cmd.exe"), &format!(" /d /s /c \"{line}\""))
            .unwrap();
        let mut out = String::new();
        c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        let code = c
            .wait_timeout(Some(Duration::from_secs(30)))
            .unwrap()
            .unwrap();
        (code, out)
    };
    let (code, out) = run_cmd(r"cd /d W:\ && cd && dir /b && type hello.txt");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(r"W:\") && out.contains("hello.txt") && out.contains("hello"),
        "{out}"
    );
    // Up from the root stays at the root; outside stays denied.
    let (_, out) = run_cmd(r"cd /d W:\.. && cd");
    assert!(out.trim().starts_with("W:\\"), "{out}");
    let secret = fx.outside.join("secret.txt");
    let (code, out) = run_cmd(&format!("type \"{}\"", secret.display()));
    assert!(code != 0 && !out.contains("top secret"), "{out}");
    // PowerShell can work in the project.
    let ps = system
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if ps.is_file() {
        let mut c = sb
            .spawn(
                &ps,
                &[
                    OsStr::new("-NoProfile"),
                    OsStr::new("-NonInteractive"),
                    OsStr::new("-Command"),
                    OsStr::new(r"$null = New-PSDrive -Name W -PSProvider FileSystem -Root W:\ -Scope Global; Set-Location W:\; (Get-Location).Path; (Get-ChildItem -Name) -join ','; Set-Content new.txt hi; Get-Content new.txt"),
                ],
            )
            .unwrap();
        let mut out = String::new();
        c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        c.wait_timeout(Some(Duration::from_secs(60))).unwrap();
        assert!(
            out.contains(r"W:\") && out.contains("hello.txt") && out.contains("hi"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(fx.workspace.join("new.txt"))
                .unwrap()
                .trim(),
            "hi"
        );
    }
    // Outside the sandbox the letter doesn't exist (unless the machine has
    // a real W:, which would then simply be shadowed inside the sandbox).
    if !Path::new("W:\\").exists() {
        let o = Command::new("cmd")
            .args(["/c", r"if exist W:\ (echo SEEN) else (echo HIDDEN)"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&o.stdout).contains("HIDDEN"));
    }
    // A folder that isn't granted can't be mapped.
    assert!(sb.map_drives(&[('V', fx.outside.clone())]).is_err());
}

/// "Folders above this project" (ADR-025): a folder held with
/// `ParentListHold` can be listed by the sandbox, nothing in it can be read
/// or written, nothing below it can be listed, nothing below it is changed,
/// and its own permissions come back exactly when the hold ends.
#[test]
fn a_parent_folder_hold_shows_names_only() {
    use gardenwall_sandbox::ParentListHold;
    let fx = Fixture::new("ParentList");
    std::fs::write(fx.root.join("top.txt"), b"top").unwrap();
    let icacls = |p: &Path| {
        String::from_utf8_lossy(&Command::new("icacls").arg(p).output().unwrap().stdout)
            .replace(&*p.to_string_lossy(), "")
    };
    let before_root = icacls(&fx.root);
    let before_outside = icacls(&fx.outside);
    let before_secret = icacls(&fx.outside.join("secret.txt"));
    let sandbox = fx.sandbox();
    assert_denied(
        probe(&sandbox, &fx, &["list", s(&fx.root)]),
        "listing the parent without the hold",
    );
    drop(sandbox);

    let hold = ParentListHold::new(&fx.profile, &fx.root, None).unwrap();
    let again = ParentListHold::new(&fx.profile, &fx.root, None).unwrap();
    assert!(gardenwall_sandbox::parent_listable(&fx.profile, &fx.root));
    let sandbox = fx.sandbox();
    let listed = probe(&sandbox, &fx, &["list", s(&fx.root)]);
    assert_ok(listed, "listing the parent's names");
    assert_denied(
        probe(&sandbox, &fx, &["read", s(&fx.root.join("top.txt"))]),
        "reading a file in the parent",
    );
    assert_denied(
        probe(&sandbox, &fx, &["write", s(&fx.root.join("new.txt"))]),
        "writing in the parent",
    );
    assert_denied(
        probe(&sandbox, &fx, &["list", s(&fx.outside)]),
        "listing a folder below the parent",
    );
    assert_denied(
        probe(&sandbox, &fx, &["read", s(&fx.outside.join("secret.txt"))]),
        "reading a file below the parent",
    );
    assert_ok(
        probe(&sandbox, &fx, &["read", s(&fx.workspace.join("hello.txt"))]),
        "the workspace",
    );
    drop(sandbox);
    // Nothing below the held folder changed.
    assert_eq!(icacls(&fx.outside), before_outside);
    assert_eq!(icacls(&fx.outside.join("secret.txt")), before_secret);

    drop(hold);
    assert!(
        gardenwall_sandbox::parent_listable(&fx.profile, &fx.root),
        "a second holder keeps it"
    );
    drop(again);
    assert!(!gardenwall_sandbox::parent_listable(&fx.profile, &fx.root));
    assert_eq!(
        icacls(&fx.root),
        before_root,
        "the folder's own permissions are back"
    );
    let sandbox = fx.sandbox();
    assert_denied(
        probe(&sandbox, &fx, &["list", s(&fx.root)]),
        "listing after the hold ended",
    );
}

/// Files with several hard links are accepted only when every name is
/// inside the granted folder (Rust's incremental build output links files
/// inside `target`): their permissions are shared by all their names.
/// Runs in Cargo's temporary folder: some file systems (a virtualized
/// %TEMP%) can't list a file's names, and the file is refused there.
#[test]
fn hard_links_are_accepted_only_when_every_name_is_inside() {
    use gardenwall_sandbox::policy::Access;
    use gardenwall_sandbox::windows::check_grant_tree;
    let base = tempfile::Builder::new()
        .prefix("links")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let root = base.path().join("work");
    let sibling = base.path().join("work-other");
    std::fs::create_dir_all(root.join("deep")).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let refused = |what: &str| {
        assert!(
            matches!(
                check_grant_tree(&root, Access::ReadWrite),
                Err(SandboxError::Policy(PolicyError::MultiplyLinked(_)))
            ),
            "{what}"
        );
    };

    // Every name inside, at different depths: accepted.
    std::fs::write(root.join("deep/obj.o"), "x").unwrap();
    std::fs::hard_link(root.join("deep/obj.o"), root.join("deep/obj-copy.o")).unwrap();
    std::fs::hard_link(root.join("deep/obj.o"), root.join("obj-top.o")).unwrap();
    check_grant_tree(&root, Access::ReadWrite).expect("links inside the folder are fine");

    // One name in a sibling folder whose name starts the same way.
    std::fs::write(sibling.join("s.txt"), "x").unwrap();
    std::fs::hard_link(sibling.join("s.txt"), root.join("s.txt")).unwrap();
    refused("a name in work-other is outside work");

    // A second name inside doesn't make the outside one safe.
    std::fs::hard_link(root.join("s.txt"), root.join("deep/s-again.txt")).unwrap();
    refused("one outside name among inside ones");
    for f in ["s.txt", "deep/s-again.txt"] {
        std::fs::remove_file(root.join(f)).unwrap();
    }
    check_grant_tree(&root, Access::ReadWrite).expect("back to inside links only");

    // A link to a file elsewhere on the drive, with a different case of
    // the folder's name in the path, is still outside.
    std::fs::write(base.path().join("secret.txt"), "x").unwrap();
    std::fs::hard_link(base.path().join("secret.txt"), root.join("deep/SECRET.txt")).unwrap();
    refused("a name directly above the folder");
}
