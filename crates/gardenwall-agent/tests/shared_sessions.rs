//! Two agent sessions on one project at once (found 2026-11-24). All agent
//! sandboxes share one container identity, so the first session to end used
//! to take the folder away from the other, and protecting a file twice
//! recorded read-only permissions as the ones to restore.
#![cfg(windows)]

use std::path::Path;

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};

/// Whether `path` inherits permissions from its folder (no `D:P` flag).
fn inherits(path: &Path) -> bool {
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(Get-Acl -LiteralPath '{}').AreAccessRulesProtected",
                path.display().to_string().trim_start_matches(r"\\?\")
            ),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim() == "False"
}

#[tokio::test]
async fn ending_one_session_leaves_the_other_working() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("proj");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("a.txt"), "hi").unwrap();
    std::fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    let config = granted.join(".git").join("config");
    assert!(inherits(&config));
    let shell = |n: &str| {
        SandboxShell::new(
            std::slice::from_ref(&granted),
            &outside.path().join(n),
            &[],
            None,
            ShellKind::Cmd,
        )
        .expect("the sandbox starts")
    };
    let one = shell("s1");
    let two = shell("s2");
    drop(one);

    // The other session still reads and writes the project...
    let out = two.run("type a.txt && echo x > b.txt", None).await.unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.output);
    // ...and its protected file is still read-only.
    let out = two.run("echo [x] >> .git\\config", None).await.unwrap();
    assert_ne!(out.exit_code, Some(0), "{}", out.output);
    let _ = two.settle_changes(false);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "[core]\n");

    // When the last session ends, the original permissions are back.
    drop(two);
    assert!(
        inherits(&config),
        "the file inherits its folder's permissions again"
    );
}
