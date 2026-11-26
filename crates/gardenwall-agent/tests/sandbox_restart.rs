//! Restarting a session's sandbox (ADR-024 §3 A1): the new sandbox has the
//! tool folder added since, the project folder keeps working, and the old
//! sandbox's scratch folder is deleted once it is gone.
#![cfg(windows)]

use std::path::PathBuf;
use std::sync::Arc;

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};
use gardenwall_agent::session_shell::SessionShell;

/// Deletes its folder when dropped, like the app's per-session scratch.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn a_restarted_sandbox_has_the_new_tool_folder() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "kept").unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    // A stand-in toolchain: one folder with a program on PATH.
    let tools = outside.path().join("mytool");
    std::fs::create_dir_all(tools.join("bin")).unwrap();
    std::fs::write(
        tools.join("bin").join("hello-tool.cmd"),
        "@echo hello from the tool\r\n",
    )
    .unwrap();
    let tools = std::fs::canonicalize(&tools).unwrap();
    let start = |n: &str, tool_folders: &[PathBuf]| {
        let scratch = outside.path().join(n);
        let shell = SandboxShell::new(
            std::slice::from_ref(&granted),
            &scratch,
            tool_folders,
            None,
            ShellKind::Cmd,
        )
        .expect("the sandbox starts");
        (Arc::new(shell) as Arc<dyn Shell>, Scratch(scratch))
    };

    let (first, scratch) = start("s1", &[]);
    let old_scratch = scratch.0.clone();
    let session = SessionShell::new(first, Some(Box::new(scratch)));
    let out = session.run("hello-tool", None).await.unwrap();
    assert_ne!(out.exit_code, Some(0), "{}", out.output);
    assert!(old_scratch.is_dir());

    let (second, scratch) = start("s2", std::slice::from_ref(&tools));
    session.replace(second, Some(Box::new(scratch))).unwrap();
    let out = session.run("hello-tool && type a.txt", None).await.unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.output);
    assert!(out.output.contains("hello from the tool"), "{}", out.output);
    assert!(out.output.contains("kept"), "{}", out.output);
    assert!(!old_scratch.exists(), "the old scratch folder is gone");
}
