//! Read-only locations inside a read/write grant (ADR-012 §3.3): the
//! sandbox can't write, add, delete, rename or re-permission them, can't
//! move a protected folder aside to rebuild it, and everything else in the
//! grant stays writable. The original permissions come back with the
//! sandbox. (Moving a folder above a protected one aside is caught by the
//! agent's after-command check, which compares what is there.)

#![cfg(windows)]

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gardenwall_sandbox::windows::delete_profile;
use gardenwall_sandbox::{Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy};

struct Fx {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    probe: PathBuf,
    profile: String,
}

impl Fx {
    fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let runtime = root.join("runtime");
        let ws = root.join("ws");
        for d in [
            &runtime,
            &ws.join(".git").join("hooks"),
            &ws.join(".git").join("objects"),
            &ws.join(".vscode"),
            &ws.join("venv")
                .join("Lib")
                .join("site-packages")
                .join("pkg"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        for (p, c) in [
            (".git/hooks/pre-commit.sample", "sample"),
            (".git/config", "[core]"),
            (".vscode/settings.json", "{}"),
            ("venv/Lib/site-packages/pkg/__init__.py", ""),
            ("hello.txt", "hello"),
        ] {
            std::fs::write(ws.join(p), c).unwrap();
        }
        let probe = runtime.join("sandbox-probe.exe");
        std::fs::copy(env!("CARGO_BIN_EXE_sandbox-probe"), &probe).unwrap();
        Self {
            _dir: dir,
            ws,
            probe,
            profile: format!("Gardenwall.Test.{name}"),
        }
    }

    fn sandbox(&self) -> Sandbox {
        let runtime = self.probe.parent().unwrap().to_path_buf();
        let mut sb = Sandbox::new(SandboxPolicy {
            profile: self.profile.clone(),
            grants: vec![
                PathGrant {
                    path: runtime,
                    access: Access::ReadExecute,
                },
                PathGrant {
                    path: self.ws.clone(),
                    access: Access::ReadWrite,
                },
            ],
            network: Network::Denied,
            limits: Limits::default(),
            env: vec![(
                "SystemRoot".into(),
                std::env::var_os("SystemRoot").unwrap_or_default(),
            )],
            working_dir: Some(self.ws.clone()),
            grant_journal_dir: None,
        })
        .unwrap();
        for p in [".git/hooks", ".git/config", ".vscode", "venv/Scripts"] {
            std::fs::create_dir_all(self.ws.join("venv/Scripts")).unwrap();
            assert!(sb.protect(&self.ws.join(p)).unwrap(), "{p}");
        }
        sb
    }

    fn p(&self, rel: &str) -> String {
        self.ws.join(rel).display().to_string()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = delete_profile(&self.profile);
    }
}

fn probe(sb: &Sandbox, fx: &Fx, args: &[&str]) -> String {
    let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let mut child = sb.spawn(&fx.probe, &os).unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .wait_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .expect("probe timed out");
    out.lines().next().unwrap_or_default().to_string()
}

fn denied(sb: &Sandbox, fx: &Fx, args: &[&str]) {
    let out = probe(sb, fx, args);
    assert!(out.starts_with("DENIED"), "{args:?} was allowed: {out}");
}

fn allowed(sb: &Sandbox, fx: &Fx, args: &[&str]) {
    let out = probe(sb, fx, args);
    assert!(out.starts_with("OK"), "{args:?} was refused: {out}");
}

#[test]
fn protected_locations_are_read_only() {
    let fx = Fx::new("ProtectRo");
    let sb = fx.sandbox();
    // Git hooks: no new hook, no change, no delete, still readable.
    denied(&sb, &fx, &["write", &fx.p(".git/hooks/pre-commit")]);
    denied(&sb, &fx, &["write", &fx.p(".git/hooks/pre-commit.sample")]);
    denied(&sb, &fx, &["remove", &fx.p(".git/hooks/pre-commit.sample")]);
    denied(
        &sb,
        &fx,
        &["readonly", &fx.p(".git/hooks/pre-commit.sample")],
    );
    allowed(&sb, &fx, &["read", &fx.p(".git/hooks/pre-commit.sample")]);
    // Git config: no change, no replacement by renaming another file onto it.
    denied(&sb, &fx, &["write", &fx.p(".git/config")]);
    allowed(&sb, &fx, &["write", &fx.p(".git/evil")]);
    denied(
        &sb,
        &fx,
        &["rename", &fx.p(".git/evil"), &fx.p(".git/config")],
    );
    denied(&sb, &fx, &["remove", &fx.p(".git/config")]);
    // Editor settings: nothing new (tasks.json), nothing changed.
    denied(&sb, &fx, &["write", &fx.p(".vscode/tasks.json")]);
    denied(&sb, &fx, &["write", &fx.p(".vscode/settings.json")]);
    denied(&sb, &fx, &["mkdir", &fx.p(".vscode/sub")]);
}

#[test]
fn protected_locations_cant_be_moved_aside_and_rebuilt() {
    let fx = Fx::new("ProtectMove");
    let sb = fx.sandbox();
    denied(&sb, &fx, &["rename", &fx.p(".vscode"), &fx.p("vscode-old")]);
    denied(&sb, &fx, &["remove", &fx.p(".vscode")]);
    denied(
        &sb,
        &fx,
        &["rename", &fx.p(".git/hooks"), &fx.p(".git/hooks-old")],
    );
    denied(&sb, &fx, &["remove", &fx.p(".git/hooks")]);
    denied(
        &sb,
        &fx,
        &["rename", &fx.p("venv/Scripts"), &fx.p("venv/Scripts-old")],
    );
}

#[test]
fn the_rest_of_the_project_stays_writable() {
    let fx = Fx::new("ProtectRest");
    let sb = fx.sandbox();
    allowed(&sb, &fx, &["write", &fx.p("hello.txt")]);
    allowed(&sb, &fx, &["write", &fx.p("new.txt")]);
    allowed(
        &sb,
        &fx,
        &["rename", &fx.p("new.txt"), &fx.p("renamed.txt")],
    );
    allowed(&sb, &fx, &["remove", &fx.p("renamed.txt")]);
    allowed(&sb, &fx, &["mkdir", &fx.p("src")]);
    allowed(&sb, &fx, &["remove", &fx.p("src")]);
    // Git can still keep its own data.
    allowed(&sb, &fx, &["write", &fx.p(".git/objects/ab")]);
    allowed(&sb, &fx, &["write", &fx.p(".git/index.lock")]);
    allowed(
        &sb,
        &fx,
        &["rename", &fx.p(".git/index.lock"), &fx.p(".git/index")],
    );
    allowed(&sb, &fx, &["remove", &fx.p(".git/index")]);
}

#[test]
fn venv_scripts_are_read_only() {
    let fx = Fx::new("ProtectVenv");
    let sb = fx.sandbox();
    denied(&sb, &fx, &["write", &fx.p("venv/Scripts/activate.ps1")]);
    allowed(
        &sb,
        &fx,
        &["write", &fx.p("venv/Lib/site-packages/pkg/__init__.py")],
    );
}

#[test]
fn protections_are_removed_with_the_sandbox() {
    let fx = Fx::new("ProtectGone");
    let sb = fx.sandbox();
    let sid = sb.sid_string().unwrap();
    drop(sb);
    for p in [".git/hooks", ".git/config", ".vscode", "venv/Scripts"] {
        let path: &Path = &fx.ws.join(p);
        let out = std::process::Command::new("icacls")
            .arg(path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains(&sid),
            "{} still has an entry: {text}",
            path.display()
        );
        // Inheritance is back on (entries are inherited again).
        assert!(
            text.contains("(I)"),
            "{} doesn't inherit: {text}",
            path.display()
        );
    }
}

/// A hidden file (a secret, ADR-011 §2.4) can't be read, changed, removed
/// or renamed, in a read/write grant and in a read-only one. Hiding it
/// again changes nothing, a replaced file is hidden anew, and the original
/// permissions come back with the sandbox.
#[test]
fn hidden_files_are_out_of_reach() {
    let fx = Fx::new("HideSecret");
    let runtime = fx.probe.parent().unwrap().to_path_buf();
    std::fs::write(fx.ws.join(".env"), "KEY=1").unwrap();
    std::fs::write(runtime.join("server.pem"), "KEY=2").unwrap();
    let mut sb = fx.sandbox();
    let sid = sb.sid_string().unwrap();
    allowed(&sb, &fx, &["read", &fx.p(".env")]);
    for path in [fx.ws.join(".env"), runtime.join("server.pem")] {
        assert!(sb.hide(&path).unwrap());
        assert!(sb.hide(&path).unwrap());
        let shown = path.display().to_string();
        denied(&sb, &fx, &["read", &shown]);
        denied(&sb, &fx, &["write", &shown]);
        denied(&sb, &fx, &["remove", &shown]);
        denied(&sb, &fx, &["rename", &shown, &fx.p("moved.txt")]);
        // The user's own access is untouched.
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("KEY="));
    }
    allowed(&sb, &fx, &["read", &fx.p("hello.txt")]);
    // An editor replaces the file: readable again until hidden again.
    std::fs::remove_file(fx.ws.join(".env")).unwrap();
    std::fs::write(fx.ws.join(".env"), "KEY=3").unwrap();
    assert!(sb.hide(&fx.ws.join(".env")).unwrap());
    denied(&sb, &fx, &["read", &fx.p(".env")]);
    // Outside every grant is refused.
    assert!(sb.hide(fx._dir.path()).is_err());
    drop(sb);
    for path in [fx.ws.join(".env"), runtime.join("server.pem")] {
        let out = std::process::Command::new("icacls")
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains(&sid),
            "{} still has an entry: {text}",
            path.display()
        );
        assert!(
            text.contains("(I)"),
            "{} doesn't inherit: {text}",
            path.display()
        );
    }
}
