//! Git in the agent's sandbox (ADR-022 §1, §2A), with Git for Windows.
//!
//! Git can't create a repository inside the sandbox: it resolves the
//! project's real path through parent folders the sandbox can't read. Once
//! Gardenwall has created the repository outside the sandbox (as
//! `init_repository` does), Git works there.
#![cfg(windows)]

use std::path::PathBuf;

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};

fn git_home() -> Option<PathBuf> {
    let g = PathBuf::from(r"C:\Program Files\Git");
    g.join("cmd").join("git.exe").is_file().then_some(g)
}

#[tokio::test]
async fn git_works_in_the_sandbox_once_the_repository_exists() {
    // GitHub's hosted Windows runners run tests in a service session without
    // a console: Git there opens /dev/null to fill its missing standard
    // handles, which the sandbox can't open, so the test can't show anything
    // about Gardenwall. It runs on developer machines.
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        eprintln!("skipped: hosted CI runner (no console session)");
        return;
    }
    let Some(git) = git_home() else {
        eprintln!("skipped: Git for Windows is not installed in Program Files");
        return;
    };
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("my project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    let shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &outside.path().join("scratch"),
        std::slice::from_ref(&git),
        None,
        ShellKind::Cmd,
    )
    .expect("the sandbox starts");

    // Inside the sandbox, an empty folder can't become a repository.
    let out = shell.run("git init", None).await.unwrap();
    assert_ne!(
        out.exit_code,
        Some(0),
        "git init can't work in the sandbox: {}",
        out.output
    );
    let _ = shell.settle_changes(false);
    let _ = std::fs::remove_dir_all(granted.join(".git"));

    // Gardenwall creates it outside the sandbox (fixed arguments)...
    let made = std::process::Command::new(git.join("cmd").join("git.exe"))
        .args(["init", "-b", "main"])
        .current_dir(&granted)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );

    // Its config and hooks become read-only for commands right away.
    shell.protect_new();
    let config = std::fs::read_to_string(granted.join(".git").join("config")).unwrap();
    let out = shell
        .run("echo [core] >> .git\\config", None)
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "{}", out.output);
    let _ = shell.settle_changes(false);
    assert_eq!(
        std::fs::read_to_string(granted.join(".git").join("config")).unwrap(),
        config,
        "a command can't change the new repository's config"
    );
    shell.protect_new(); // twice changes nothing

    // ...and then Git works in the sandbox.
    let out = shell
        .run(
            "git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m first && git log --oneline",
            None,
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.output);
    assert!(out.output.contains("first"), "{}", out.output);
}
