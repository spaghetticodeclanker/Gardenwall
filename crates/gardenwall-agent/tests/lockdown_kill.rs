//! Lockdown's kill switch (GW-019): a command
//! running in the sandbox ends within moments, with everything it started,
//! and the shell runs no command after.
#![cfg(windows)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{KILLED, SandboxShell, ShellKind};

#[tokio::test]
async fn kill_now_ends_a_running_command_and_every_later_one() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    let shell = Arc::new(
        SandboxShell::new(
            std::slice::from_ref(&granted),
            &outside.path().join("scratch"),
            &[],
            None,
            ShellKind::Cmd,
        )
        .expect("the sandbox starts"),
    );

    // A command that would run for 30 seconds (a busy loop: no network,
    // no programs outside the sandbox).
    let running = {
        let shell = shell.clone();
        tokio::spawn(async move {
            shell
                .run(
                    "for /l %i in (1,1,300000000) do @rem",
                    Some(Duration::from_secs(60)),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let killed_at = Instant::now();
    shell.kill_now();
    let result = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the command ends soon after the kill")
        .unwrap();
    assert!(
        killed_at.elapsed() < Duration::from_secs(5),
        "took {:?}",
        killed_at.elapsed()
    );
    assert_eq!(result.unwrap_err(), KILLED);

    // Nothing runs after.
    let after = shell.run("echo hello", None).await;
    assert_eq!(after.unwrap_err(), KILLED);
}
