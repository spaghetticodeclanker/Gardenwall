//! The command boundary in the real sandbox (ADR-011 §2.3, §2.5): commands
//! can't change protected locations that exist; ones they create (or
//! rebuild after moving a folder aside) are held back for the user;
//! programs left running wait for the user's decision; ordinary work is
//! unaffected.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use gardenwall_agent::agent::Shell;
use gardenwall_agent::sandbox_shell::{SandboxShell, ShellKind};

fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

struct Fx {
    _tmp: tempfile::TempDir,
    work: PathBuf,
    shell: SandboxShell,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(work.join(".git").join("hooks")).unwrap();
    std::fs::create_dir_all(work.join(".git").join("objects")).unwrap();
    std::fs::write(work.join(".git").join("config"), "[core]\n").unwrap();
    std::fs::write(
        work.join(".git").join("hooks").join("pre-commit.sample"),
        "x",
    )
    .unwrap();
    std::fs::write(work.join("package.json"), "{}").unwrap();
    std::fs::write(work.join("app.py"), "print(1)\n").unwrap();
    let granted = std::fs::canonicalize(&work).unwrap();
    let mut shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &tmp.path().join("scratch"),
        &[],
        None,
        ShellKind::PowerShell,
    )
    .unwrap();
    shell.set_quarantine(tmp.path().join("held"));
    assert!(shell.unprotected().is_empty(), "{:?}", shell.unprotected());
    Fx {
        _tmp: tmp,
        work,
        shell,
    }
}

fn run(f: &Fx, cmd: &str) -> String {
    let out = block(f.shell.run(cmd, Some(Duration::from_secs(60)))).unwrap();
    out.output
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[test]
fn existing_protected_locations_are_read_only_for_commands() {
    let f = fx();
    run(&f, "Set-Content .git\\hooks\\pre-commit 'evil'");
    run(&f, "Add-Content .git\\config 'hooksPath = x'");
    run(
        &f,
        "Set-Content package.json '{\"scripts\":{\"postinstall\":\"evil\"}}'",
    );
    run(&f, "Rename-Item .git\\hooks hooks-old");
    assert!(!f.work.join(".git/hooks/pre-commit").exists());
    assert_eq!(read(&f.work.join(".git/config")), "[core]\n");
    assert_eq!(read(&f.work.join("package.json")), "{}");
    assert!(f.work.join(".git/hooks").is_dir());
    // Nothing was changed, so nothing waits for the user.
    assert_eq!(f.shell.aftermath(), None);
    // Ordinary work goes on, programs included, and Git can keep its data.
    run(
        &f,
        "Set-Content app.py 'print(2)'; Set-Content .git\\objects\\ab 'x'",
    );
    assert_eq!(read(&f.work.join("app.py")).trim(), "print(2)");
    assert!(f.work.join(".git/objects/ab").exists());
}

#[test]
fn created_protected_locations_are_held_back() {
    let f = fx();
    let out = run(
        &f,
        "New-Item -ItemType Directory .vscode | Out-Null; Set-Content .vscode\\tasks.json '{\"runOn\":\"folderOpen\"}'; 'done'",
    );
    assert!(out.contains("done"), "{out}");
    assert!(!f.work.join(".vscode").exists(), "moved out of the project");
    let a = f.shell.aftermath().expect("held back");
    assert_eq!(a.changes, [".vscode (created)"]);
    assert!(a.details.contains("tasks.json"), "{}", a.details);
    // The user refuses: it is gone, and nothing else waits.
    f.shell.settle_changes(false);
    assert!(!f.work.join(".vscode").exists());
    assert_eq!(f.shell.aftermath(), None);

    // Kept: it comes back and is read-only for later commands.
    run(
        &f,
        "New-Item -ItemType Directory .vscode | Out-Null; Set-Content .vscode\\settings.json '{}'",
    );
    assert!(f.shell.aftermath().is_some());
    f.shell.settle_changes(true);
    assert_eq!(read(&f.work.join(".vscode/settings.json")).trim(), "{}");
    run(&f, "Set-Content .vscode\\tasks.json 'evil'");
    assert!(!f.work.join(".vscode/tasks.json").exists());
}

#[test]
fn a_git_folder_moved_aside_and_rebuilt_is_caught() {
    let f = fx();
    run(
        &f,
        "Rename-Item .git git-old; New-Item -ItemType Directory .git\\hooks -Force | Out-Null; Set-Content .git\\hooks\\post-checkout 'evil'; Set-Content .git\\config '[core]'",
    );
    let a = f.shell.aftermath().expect("caught");
    assert!(
        a.changes.iter().any(|c| c.contains(".git/hooks")),
        "{:?}",
        a.changes
    );
    f.shell.settle_changes(false);
    assert!(!f.work.join(".git/hooks/post-checkout").exists());
}

#[test]
fn programs_left_running_wait_for_the_user() {
    let f = fx();
    let start = "Start-Process -WindowStyle Hidden powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep 60'";
    let out = run(&f, start);
    let a = f
        .shell
        .aftermath()
        .unwrap_or_else(|| panic!("a program is left: {out}"));
    assert!(
        a.programs.iter().any(|p| p.starts_with("powershell.exe")),
        "{:?}",
        a.programs
    );
    // Refused: stopped.
    f.shell.settle_programs(false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(f.shell.background().is_empty());
    assert_eq!(f.shell.aftermath(), None);

    // Approved: it keeps running, is listed, and isn't asked about again.
    run(&f, start);
    assert!(f.shell.aftermath().is_some());
    f.shell.settle_programs(true);
    assert_eq!(f.shell.background().len(), 1, "{:?}", f.shell.background());
    run(&f, "'next command'");
    assert_eq!(f.shell.aftermath(), None);
    f.shell.stop_background();
    std::thread::sleep(Duration::from_millis(300));
    assert!(f.shell.background().is_empty());
}

/// A program left running can't plant a protected file after its command's
/// check (review GW-005): what it creates before the next command is held
/// back before that command runs, and asked about with it.
#[test]
fn changes_by_programs_left_running_are_caught_before_the_next_command() {
    let f = fx();
    let target = std::fs::canonicalize(&f.work).unwrap();
    let target = target.to_string_lossy();
    let target = target.strip_prefix(r"\\?\").unwrap_or(&target);
    let later = format!(
        r"Start-Process -WindowStyle Hidden powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep 3; New-Item -ItemType Directory ''{target}\.vscode'' | Out-Null; Set-Content ''{target}\.vscode\tasks.json'' evil'"
    );
    run(&f, &later);
    // Nothing protected changed yet; the program is let run.
    let a = f.shell.aftermath().expect("a program is left");
    assert!(a.changes.is_empty(), "{:?}", a.changes);
    f.shell.settle_programs(true);
    // The program waits 3 s, then writes. Windows PowerShell can take
    // several seconds to start in a sandbox on a busy machine (CI), so
    // wait for the file rather than a fixed time.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !f.work.join(".vscode/tasks.json").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
    // Let the write finish before the check reads the folder.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        f.work.join(".vscode/tasks.json").exists(),
        "the program ran"
    );
    run(&f, "'next command'");
    assert!(
        !f.work.join(".vscode").exists(),
        "held back before the next command"
    );
    let a = f.shell.aftermath().expect("caught");
    assert_eq!(a.changes, [".vscode (created)"]);
    f.shell.settle_changes(false);
    assert!(!f.work.join(".vscode").exists());
    f.shell.stop_background();
}
