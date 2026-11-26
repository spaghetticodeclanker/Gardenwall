//! Build files below the top of a project are read-only for commands
//! (ADR-022 §8): a sub-crate's or vendored crate's `build.rs` runs on the
//! user's next build outside the sandbox.
#![cfg(windows)]

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};

#[tokio::test]
async fn a_sub_crates_build_script_is_read_only_for_commands() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("proj");
    std::fs::create_dir_all(root.join("crates").join("a").join("src")).unwrap();
    std::fs::write(
        root.join("crates").join("a").join("build.rs"),
        "fn main() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("crates").join("a").join("src").join("lib.rs"), "").unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    let shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &outside.path().join("scratch"),
        &[],
        None,
        ShellKind::Cmd,
    )
    .expect("the sandbox starts");
    let out = shell
        .run("echo // planted >> crates\\a\\build.rs", None)
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "{}", out.output);
    let _ = shell.settle_changes(false);
    assert_eq!(
        std::fs::read_to_string(granted.join("crates").join("a").join("build.rs")).unwrap(),
        "fn main() {}\n"
    );
    // Ordinary source files stay writable.
    let out = shell
        .run("echo // ok >> crates\\a\\src\\lib.rs", None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.output);
}

/// MSBuild project files: `dotnet restore`,
/// run by the user outside the sandbox, evaluates them.
#[tokio::test]
async fn msbuild_project_files_are_read_only_for_commands() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("proj");
    let lib = root.join("src").join("Lib");
    std::fs::create_dir_all(&lib).unwrap();
    let project = "<Project Sdk=\"Microsoft.NET.Sdk\" />\n";
    std::fs::write(root.join("App.csproj"), project).unwrap();
    std::fs::write(lib.join("Lib.csproj"), project).unwrap();
    std::fs::write(lib.join("Program.cs"), "").unwrap();
    let granted = std::fs::canonicalize(&root).unwrap();
    let shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &outside.path().join("scratch"),
        &[],
        None,
        ShellKind::Cmd,
    )
    .expect("the sandbox starts");
    for target in ["App.csproj", r"src\Lib\Lib.csproj"] {
        let out = shell
            .run(&format!("echo planted >> {target}"), None)
            .await
            .unwrap();
        assert_ne!(out.exit_code, Some(0), "{target}: {}", out.output);
        let _ = shell.settle_changes(false);
    }
    assert_eq!(
        std::fs::read_to_string(granted.join("App.csproj")).unwrap(),
        project
    );
    assert_eq!(
        std::fs::read_to_string(lib.join("Lib.csproj")).unwrap(),
        project
    );
    // Source files stay writable.
    let out = shell
        .run(r"echo // ok >> src\Lib\Program.cs", None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.output);
}
