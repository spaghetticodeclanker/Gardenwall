//! The agent's shell really runs confined (Windows AppContainer with cmd /
//! Linux Landlock + seccomp with sh).
#![cfg(any(windows, target_os = "linux"))]

use std::path::PathBuf;
use std::time::Duration;

use gardenwall_agent::agent::{Shell, ShellOutput};
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
    outside: PathBuf,
    tools: PathBuf,
    shell: SandboxShell,
}

/// The fixture, or `None` (the test is skipped with a message) on a Linux
/// kernel without Landlock, where the sandbox refuses to run.
fn fx() -> Option<Fx> {
    fx_with(ShellKind::Cmd)
}

fn fx_with(kind: ShellKind) -> Option<Fx> {
    fx_full(kind, false)
}

/// With `short_name`, the work folder is granted by its 8.3 short path
/// (as `%TEMP%` often is), when the volume has short names.
fn fx_full(kind: ShellKind, short_name: bool) -> Option<Fx> {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join(if short_name {
        "work folder with a long name"
    } else {
        "work"
    });
    let outside = tmp.path().join("outside");
    let tools = tmp.path().join("tools");
    for d in [&work, &outside, &tools.join("bin")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();
    // A program in a read-only tool folder.
    #[cfg(windows)]
    {
        // A real .exe (copied, so the tool folder belongs to this user).
        let system32 = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        std::fs::copy(
            system32.join("hostname.exe"),
            tools.join("bin").join("gwtool.exe"),
        )
        .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = tools.join("bin").join("gwtool");
        std::fs::write(&script, "#!/bin/sh\necho tool-ran\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The app grants canonical folders (`\?\C:\...` on Windows).
    let granted = if short_name {
        short_path(&work)?
    } else {
        std::fs::canonicalize(&work).unwrap()
    };
    let shell = match SandboxShell::new(
        std::slice::from_ref(&granted),
        &tmp.path().join("scratch"),
        std::slice::from_ref(&tools),
        None,
        kind,
    ) {
        Ok(shell) => shell,
        Err(e) if cfg!(target_os = "linux") && e.contains("not available") => {
            eprintln!("skipped: {e}");
            return None;
        }
        Err(e) => panic!("{e}"),
    };
    Some(Fx {
        _tmp: tmp,
        work,
        outside,
        tools,
        shell,
    })
}

/// The 8.3 short form of `path` (cmd's `%~s`), or `None` (with a message)
/// when it has none.
fn short_path(path: &std::path::Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/d /c for %I in (\"{}\") do @echo %~sI",
                path.display()
            ))
            .output()
            .unwrap();
        let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !short.contains('~') {
            eprintln!("skipped: no 8.3 short names here ({short})");
            return None;
        }
        Some(PathBuf::from(short))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

fn run(f: &Fx, cmd: &str, secs: u64) -> ShellOutput {
    block(f.shell.run(cmd, Some(Duration::from_secs(secs)))).unwrap()
}

#[test]
fn commands_work_in_the_workspace_and_are_confined() {
    let Some(f) = fx() else { return };
    let secret = f.outside.join("secret.txt").display().to_string();
    let (write_cmd, read_outside, net) = if cfg!(windows) {
        (
            "echo hello> made.txt && type \"made.txt\"".to_string(),
            format!("type \"{secret}\""),
            "curl.exe -s -m 5 http://1.1.1.1/ >nul 2>&1 && echo NET-OK || echo NET-BLOCKED",
        )
    } else {
        (
            "echo hello > made.txt && cat made.txt".to_string(),
            format!("cat '{secret}'"),
            "(exec 3<>/dev/tcp/1.1.1.1/80 && echo NET-OK) 2>/dev/null || echo NET-BLOCKED",
        )
    };
    let out = run(&f, &write_cmd, 60);
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.output.lines().any(|l| l.trim() == "hello"), "{out:?}");
    // An ordinary command leaves nothing for the user to decide on (no
    // protected file changed, no program left running).
    assert_eq!(f.shell.aftermath(), None);
    // GitHub's Windows runners (a non-interactive service session) add a
    // stray "Access is denied." line to cmd's output that a desktop session
    // does not; the write itself succeeds, which is what is checked below.
    assert_eq!(
        std::fs::read_to_string(f.work.join("made.txt"))
            .unwrap()
            .trim(),
        "hello"
    );

    let out = run(&f, &read_outside, 60);
    assert!(
        !out.output.contains("outside secret"),
        "read outside the workspace: {out:?}"
    );
    assert_ne!(out.exit_code, Some(0));

    let out = run(&f, net, 60);
    assert!(
        out.output.contains("NET-BLOCKED") && !out.output.contains("NET-OK"),
        "{out:?}"
    );

    assert_eq!(run(&f, "exit 3", 60).exit_code, Some(3));

    // Commands start in the granted folder, not the Windows folder.
    let out = run(&f, if cfg!(windows) { "cd" } else { "pwd" }, 60);
    let here = out
        .output
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if cfg!(windows) {
        // cmd stays in the real folder, so the programs it runs can use it.
        let want = f.work.display().to_string().to_lowercase();
        assert!(
            here.ends_with(&want[want.len() - 20..]),
            "{out:?} vs {want}"
        );
        // Programs (not just cmd's own commands) can open project files.
        let out = run(&f, "findstr hello made.txt", 60);
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert!(out.output.contains("hello"), "{out:?}");
        // dir works through the private drive (it can't read the drive of
        // C:\ in the sandbox, so a bare dir is refused).
        let out = run(&f, r"dir /b W:\", 60);
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert!(out.output.contains("made.txt"), "{out:?}");
    } else {
        let want = std::fs::canonicalize(&f.work)
            .unwrap()
            .display()
            .to_string()
            .to_lowercase();
        assert!(
            want.ends_with(&here) && !here.is_empty(),
            "{out:?} vs {want}"
        );
    }
}

/// Windows PowerShell 5.1 (the fallback when PowerShell 7 isn't
/// installed): it works in the project (its private drive), stays
/// confined, reports failures, and points out bash habits.
#[cfg(windows)]
#[test]
fn powershell_works_in_the_project_and_is_confined() {
    let Some(f) = fx_with(ShellKind::WindowsPowerShell) else {
        return;
    };
    assert_eq!(f.shell.name(), "Windows PowerShell 5.1");
    powershell_checks(&f);
    let out = run(&f, "echo a && echo b", 60);
    assert!(
        out.output.contains("[Gardenwall:") && out.output.contains("has no &&"),
        "{out:?}"
    );
}

/// A folder granted by its 8.3 short path works too. Windows PowerShell
/// 5.1 expands short names when it adds the private drive, which failed
/// ("Access to the path is denied") when the drive pointed at the short
/// path, and every command then started with CLIXML errors.
#[cfg(windows)]
#[test]
fn powershell_works_in_a_folder_given_by_its_short_name() {
    for kind in [ShellKind::WindowsPowerShell, ShellKind::Cmd] {
        let Some(f) = fx_full(kind, true) else {
            return;
        };
        let out = run(&f, "echo ok", 60);
        assert_eq!(out.exit_code, Some(0), "{kind:?}: {out:?}");
        assert_eq!(out.output.trim(), "ok", "{kind:?}: {out:?}");
    }
}

/// PowerShell 7, the preferred shell, passes the same checks and has `&&`
/// and `||`. Skipped when it isn't installed in Program Files.
#[cfg(windows)]
#[test]
fn powershell7_works_in_the_project_and_is_confined() {
    let Some(exe) = gardenwall_agent::sandbox_shell::find_powershell7() else {
        eprintln!("skipped: PowerShell 7 is not installed in Program Files");
        return;
    };
    assert!(!exe.display().to_string().contains("WindowsApps"));
    let Some(f) = fx_with(ShellKind::PowerShell) else {
        return;
    };
    assert!(
        f.shell.name().starts_with("PowerShell 7."),
        "PowerShell 7 should start in the sandbox: {}",
        f.shell.name()
    );
    powershell_checks(&f);
    let out = run(&f, "$PSVersionTable.PSEdition", 60);
    assert!(out.output.contains("Core"), "{out:?}");
    let out = run(&f, "echo a && echo b; cmd /c exit 2 || echo recovered", 60);
    assert!(
        out.output.contains("a\nb\n") && out.output.contains("recovered"),
        "{out:?}"
    );
    assert!(!out.output.contains("[Gardenwall:"), "{out:?}");
    // No colour codes in errors (PowerShell 7 adds them by default).
    let out = run(&f, "Get-Item missing.txt", 60);
    assert!(!out.output.contains('\u{1b}'), "{out:?}");
    // Its own modules load (from its folder, not Windows PowerShell's).
    let out = run(
        &f,
        "(Get-FileHash -Algorithm SHA256 -InputStream ([IO.MemoryStream]::new())).Hash; (Get-Command Get-FileHash).Module.Path",
        60,
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(
        out.output.contains("E3B0C442")
            && out
                .output
                .to_ascii_lowercase()
                .contains(r"powershell\7\modules"),
        "{out:?}"
    );
    // The model is told it has && (and not that it hasn't).
    let g = f.shell.guidelines().join("\n");
    assert!(
        g.contains("PowerShell 7.") && g.contains("&&") && !g.contains("no &&"),
        "{g}"
    );
}

/// What every PowerShell must do in the sandbox.
#[cfg(windows)]
fn powershell_checks(f: &Fx) {
    assert_eq!(f.shell.tool_name(), "powershell");
    let out = run(
        f,
        "Set-Content -Path made.txt -Value hello\n(Get-Location).Path\nGet-ChildItem -Name\nGet-Content made.txt",
        60,
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.output.contains(r"W:\"), "{out:?}");
    assert!(
        out.output.contains("made.txt") && out.output.contains("hello"),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("made.txt"))
            .unwrap()
            .trim(),
        "hello"
    );
    // Programs PowerShell starts (not just its own commands) can open
    // project files, also from a subfolder: they can't see the private
    // letter, so they must get the real folder.
    let out = run(
        f,
        "findstr hello made.txt; New-Item -ItemType Directory sub | Out-Null; Set-Location sub; findstr hello ..\\made.txt",
        60,
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.output.matches("hello").count(), 2, "{out:?}");
    // PowerShell's own move, rename and delete work too (they check the
    // drive's volume, which only the private letter lets the sandbox read).
    let out = run(
        f,
        "Set-Content r1.txt x; Rename-Item r1.txt r2.txt; Move-Item r2.txt sub; Remove-Item sub\\r2.txt; Test-Path sub\\r2.txt",
        60,
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.output.trim(), "False", "{out:?}");
    // A program's exit code and piped input pass through.
    let out = run(f, "'piped' | findstr piped", 60);
    assert!(out.output.contains("piped"), "{out:?}");
    assert_eq!(run(f, "cmd /c exit 4", 60).exit_code, Some(4));
    // ls and cat work as PowerShell aliases.
    let out = run(f, "ls; cat made.txt", 60);
    assert!(out.output.contains("hello"), "{out:?}");
    // Quotes and special characters reach PowerShell as written.
    let out = run(f, r#"Write-Output "a `"quoted`" & 'single' | 100%""#, 60);
    assert!(
        out.output.contains(r#"a "quoted" & 'single' | 100%"#),
        "{out:?}"
    );
    // Outside stays denied; going up stays on the drive.
    let secret = f.outside.join("secret.txt").display().to_string();
    let out = run(f, &format!("Get-Content -LiteralPath '{secret}'"), 60);
    assert!(!out.output.contains("outside secret"), "{out:?}");
    assert_ne!(out.exit_code, Some(0));
    let out = run(f, "Set-Location ..; (Get-Location).Path", 60);
    assert!(out.output.contains(r"W:\"), "{out:?}");
    // No network.
    let out = run(
        f,
        "try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 http://1.1.1.1/ | Out-Null; 'NET-OK' } catch { 'NET-BLOCKED' }",
        60,
    );
    assert!(
        out.output.contains("NET-BLOCKED") && !out.output.contains("NET-OK"),
        "{out:?}"
    );
    // Exit codes.
    assert_eq!(run(f, "exit 3", 60).exit_code, Some(3));
    let out = run(f, "Get-Item missing.txt", 60);
    assert_eq!(out.exit_code, Some(1), "{out:?}");
    assert!(
        out.output.contains("missing.txt") && !out.output.contains("CLIXML"),
        "errors are plain text: {out:?}"
    );
    // A program's exit code is the command's.
    assert_eq!(run(f, "cmd /c exit 4", 60).exit_code, Some(4));
    // A program's error lines and the output around them keep their order
    // (cargo's "Compiling…" came after the program's output before).
    let out = run(
        f,
        "Write-Output first; cmd /c \"echo second 1>&2\"; Write-Output third; Get-Item missing-order.txt; Write-Output fifth",
        60,
    );
    let at = |s: &str| out.output.find(s).unwrap_or_else(|| panic!("{s}: {out:?}"));
    assert!(
        at("first") < at("second")
            && at("second") < at("third")
            && at("third") < at("missing-order.txt")
            && at("missing-order.txt") < at("fifth"),
        "{out:?}"
    );
    // A syntax error is reported as text and fails.
    let out = run(f, "if (", 60);
    assert_eq!(out.exit_code, Some(1), "{out:?}");
    assert!(!out.output.contains("CLIXML"), "{out:?}");
}

#[cfg(windows)]
#[test]
fn cmd_receives_quotes_verbatim_and_rejects_multiline() {
    let Some(f) = fx() else { return };
    let out = run(&f, "echo \"a b\" ^& 'c' && echo done", 60);
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(
        out.output.contains("\"a b\" & 'c'") && out.output.contains("done"),
        "{out:?}"
    );
    assert!(block(f.shell.run("echo a\necho b", None)).is_err());
}

#[test]
fn tool_folders_are_on_path_and_read_only() {
    let Some(f) = fx() else { return };
    let out = run(&f, "gwtool", 60);
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(!out.output.trim().is_empty(), "{out:?}");
    let target = f.tools.join("planted.txt").display().to_string();
    let write = if cfg!(windows) {
        format!("echo x> \"{target}\"")
    } else {
        format!("echo x > '{target}'")
    };
    run(&f, &write, 60);
    assert!(
        !f.tools.join("planted.txt").exists(),
        "tool folder must be read-only"
    );
}

/// A timed-out command ends its children too, and the call returns, even if
/// a child would otherwise wait forever and hold the output pipes open.
#[test]
fn timeouts_end_the_whole_command_tree() {
    let Some(f) = fx() else { return };
    let cmd = if cfg!(windows) {
        "cmd /d /c \"for /l %i in (1,0,2) do @rem\""
    } else {
        "sh -c 'while :; do :; done'"
    };
    let started = std::time::Instant::now();
    let out = run(&f, cmd, 3);
    assert!(out.timed_out, "{out:?}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        run(&f, "echo still-alive", 60)
            .output
            .contains("still-alive")
    );
}

/// A console program that reads stdin (here `findstr` with no file) must
/// see end-of-input and finish, not open a visible window and wait for the
/// keyboard. Regression test for a hang found in manual testing.
#[test]
fn child_programs_reading_stdin_finish_instead_of_hanging() {
    let Some(f) = fx() else { return };
    let cmd = if cfg!(windows) {
        "findstr /i \"3a 0d\" & echo after"
    } else {
        "grep x; echo after"
    };
    let started = std::time::Instant::now();
    let out = run(&f, cmd, 20);
    assert!(!out.timed_out, "the child waited for input: {out:?}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert!(out.output.contains("after"), "{out:?}");
}

/// A PowerShell 7 that doesn't start in the sandbox (or isn't one) is not
/// used: the session falls back to Windows PowerShell 5.1. Also prints how
/// long each shell takes to set up (PowerShell 7 is checked by starting it).
#[cfg(windows)]
#[test]
fn powershell7_falls_back_to_windows_powershell() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let fake = tmp.path().join("fake7");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&fake).unwrap();
    // Starts in the sandbox, but prints a host name, not a version.
    let system32 = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    std::fs::copy(system32.join("hostname.exe"), fake.join("pwsh.exe")).unwrap();
    let granted = std::fs::canonicalize(&work).unwrap();
    let make = |kind, pwsh: Option<PathBuf>| {
        let started = std::time::Instant::now();
        let shell = SandboxShell::with_powershell7(
            std::slice::from_ref(&granted),
            &tmp.path().join("scratch"),
            &[],
            None,
            kind,
            pwsh,
        )
        .unwrap();
        (shell, started.elapsed())
    };
    let mut candidates = vec![
        fake.join("pwsh.exe"),
        tmp.path().join("missing").join("pwsh.exe"),
    ];
    // The Microsoft Store build, when installed, can't start in another
    // AppContainer.
    let store = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("Microsoft")
        .join("WindowsApps")
        .join("pwsh.exe");
    if store.exists() {
        candidates.push(store);
    }
    for pwsh in candidates {
        let (shell, took) = make(ShellKind::PowerShell, Some(pwsh.clone()));
        assert_eq!(shell.name(), "Windows PowerShell 5.1", "{}", pwsh.display());
        assert_eq!(shell.tool_name(), "powershell");
        let out = block(shell.run(
            "$PSVersionTable.PSVersion.Major",
            Some(Duration::from_secs(60)),
        ))
        .unwrap();
        assert!(out.output.trim() == "5", "{out:?}");
        eprintln!("fallback from {}: {took:?}", pwsh.display());
    }
    // A chosen shell never tries PowerShell 7.
    let real = gardenwall_agent::sandbox_shell::find_powershell7();
    let (shell, took) = make(ShellKind::WindowsPowerShell, real.clone());
    assert_eq!(shell.name(), "Windows PowerShell 5.1");
    eprintln!("Windows PowerShell 5.1 setup: {took:?}");
    let (shell, took) = make(ShellKind::Cmd, real.clone());
    assert_eq!(shell.name(), "cmd");
    eprintln!("cmd setup: {took:?}");
    if real.is_some() {
        let (shell, took) = make(ShellKind::PowerShell, real);
        assert!(
            shell.name().starts_with("PowerShell 7."),
            "{}",
            shell.name()
        );
        eprintln!("{} setup: {took:?}", shell.name());
    }
}

/// A shell on `work` with one canonical (`\\?\C:\...`) tool folder, as the
/// app passes them.
#[cfg(windows)]
fn shell_with_tools(
    tmp: &std::path::Path,
    tools: &std::path::Path,
    kind: ShellKind,
) -> SandboxShell {
    let work = std::fs::canonicalize(tmp.join("work")).unwrap();
    SandboxShell::new(
        std::slice::from_ref(&work),
        &tmp.join("scratch"),
        &[std::fs::canonicalize(tools).unwrap()],
        None,
        kind,
    )
    .unwrap()
}

/// Batch files run in every shell, including launchers in a tool folder
/// that find their program through `%~dp0` (as npm.cmd does). They failed
/// while tool folders were on PATH in the `\\?\` form.
#[cfg(windows)]
#[test]
fn batch_files_and_launchers_run_in_every_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(tmp.path().join("work")).unwrap();
    std::fs::create_dir_all(&tools).unwrap();
    let system32 = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    std::fs::copy(system32.join("whoami.exe"), tools.join("gwprog.exe")).unwrap();
    std::fs::write(
        tools.join("gwlaunch.cmd"),
        "@echo off\r\nif not exist \"%~dp0gwprog.exe\" (echo missing & exit /b 3)\r\nfor /f \"delims=\" %%F in ('call \"%~dp0gwprog.exe\"') do echo launched-%1\r\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("work").join("local.cmd"),
        "@echo off\r\necho local-%1\r\n",
    )
    .unwrap();
    for kind in [
        ShellKind::PowerShell,
        ShellKind::WindowsPowerShell,
        ShellKind::Cmd,
    ] {
        let shell = shell_with_tools(tmp.path(), &tools, kind);
        let run = |c: &str| block(shell.run(c, Some(Duration::from_secs(60)))).unwrap();
        let out = run("gwlaunch a");
        assert_eq!(out.exit_code, Some(0), "{kind:?}: {out:?}");
        assert_eq!(out.output.trim(), "launched-a", "{kind:?}");
        let out = run("gwlaunch.cmd b");
        assert_eq!(out.output.trim(), "launched-b", "{kind:?}: {out:?}");
        let out = run(r".\local.cmd c");
        assert_eq!(out.output.trim(), "local-c", "{kind:?}: {out:?}");
    }
}

/// Node.js scripts, ES modules and npm run in the sandbox. Node's
/// JavaScript realpath needs to open the drive root, which an AppContainer
/// can't; the session's preload makes it use the native call. Runs with a
/// Node install the sandbox can use: `GW_NODE`, or Program Files when its
/// permissions allow it (skipped otherwise).
#[cfg(windows)]
#[test]
fn node_and_npm_run_in_the_sandbox() {
    let node = std::env::var_os("GW_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\nodejs"));
    if !node.join("node.exe").is_file() || !gardenwall_sandbox::sandbox_can_use(&node) {
        eprintln!("skipped: no Node.js the sandbox can use (set GW_NODE to a copy)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("lib.cjs"), "module.exports = 'cjs-ok';").unwrap();
    std::fs::write(work.join("main.cjs"), "console.log(require('./lib.cjs'));").unwrap();
    std::fs::write(work.join("lib.mjs"), "export default 'esm-ok';").unwrap();
    std::fs::write(
        work.join("main.mjs"),
        "import v from './lib.mjs'; console.log(v);",
    )
    .unwrap();
    std::fs::write(
        work.join("real.cjs"),
        "const fs = require('fs'); fs.realpath('lib.cjs', (e, p) => console.log(e ? 'err' : 'async-ok', fs.realpathSync('lib.cjs').endsWith('lib.cjs')));",
    )
    .unwrap();
    std::fs::write(
        work.join("package.json"),
        r#"{"name":"p","version":"1.0.0","scripts":{"test":"node main.cjs"}}"#,
    )
    .unwrap();
    // A pnpm-style layout: `node_modules/a` is a junction into the store,
    // and `a` finds its dependency `b` next to its real location. That only
    // works when links are resolved (it failed with --preserve-symlinks).
    let store = ["node_modules", ".pnpm", "a@1.0.0", "node_modules"]
        .iter()
        .fold(work.clone(), |p, c| p.join(c));
    for (name, body) in [
        ("a", "module.exports = 'a+' + require('b');"),
        ("b", "module.exports = 'b';"),
    ] {
        std::fs::create_dir_all(store.join(name)).unwrap();
        std::fs::write(store.join(name).join("index.js"), body).unwrap();
        std::fs::write(
            store.join(name).join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0","main":"index.js"}}"#),
        )
        .unwrap();
    }
    // Grants refuse a tree with links in it, so the junction is made after
    // the sandbox is set up (as a link made during a session would be).
    let junction = work.join("node_modules").join("a");
    let link = |on: bool| {
        let _ = std::fs::remove_dir(&junction);
        if on {
            let made = std::process::Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&junction)
                .arg(store.join("a"))
                .output()
                .unwrap();
            assert!(made.status.success(), "{made:?}");
        }
    };
    std::fs::write(work.join("pnpm.cjs"), "console.log(require('a'));").unwrap();
    std::fs::write(
        work.join("pnpm.mjs"),
        "import a from 'a'; console.log('esm-' + a);",
    )
    .unwrap();
    // User code still gets the real error for a folder it can't open.
    std::fs::write(
        work.join("root.cjs"),
        "try { require('fs').lstatSync('C:/'); console.log('opened'); } catch (e) { console.log(e.code); }",
    )
    .unwrap();
    let npm = node.join("npm.cmd").is_file();
    for kind in [
        ShellKind::PowerShell,
        ShellKind::WindowsPowerShell,
        ShellKind::Cmd,
    ] {
        link(false);
        let shell = shell_with_tools(tmp.path(), &node, kind);
        link(true);
        let run = |c: &str| block(shell.run(c, Some(Duration::from_secs(120)))).unwrap();
        for (cmd, want) in [
            ("node main.cjs", "cjs-ok"),
            ("node main.mjs", "esm-ok"),
            ("node real.cjs", "async-ok true"),
            ("node pnpm.cjs", "a+b"),
            ("node pnpm.mjs", "esm-a+b"),
            ("node root.cjs", "EPERM"),
        ] {
            let out = run(cmd);
            assert_eq!(out.exit_code, Some(0), "{kind:?} {cmd}: {out:?}");
            assert_eq!(out.output.trim(), want, "{kind:?} {cmd}");
        }
        if npm {
            let out = run("npm test");
            assert_eq!(out.exit_code, Some(0), "{kind:?} npm test: {out:?}");
            assert!(out.output.contains("cjs-ok"), "{kind:?}: {out:?}");
            let out = run("npx --version");
            assert_eq!(out.exit_code, Some(0), "{kind:?} npx: {out:?}");
        }
        // Still confined: Node can't read outside the granted folders.
        let out = run("node -e \"require('fs').readFileSync('C:/Windows/System32/config/SAM')\"");
        assert_ne!(out.exit_code, Some(0), "{kind:?}: {out:?}");
    }
}

/// A file's permissions as `icacls` prints them (the path line removed).
#[cfg(windows)]
fn permissions(path: &std::path::Path) -> String {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).replace(&path.display().to_string(), "");
    text.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

/// Secret-pattern files in a granted folder (ADR-011 §2.4) are hidden from
/// the file tools. A command's output goes to the model as well, so the
/// sandbox loses all access to them: a command can't read, change, delete,
/// rename or copy one, also not a file that appeared or was replaced after
/// the session started. The user's own access and, afterwards, the original
/// permissions are untouched. (Not enforceable on Linux: Landlock can't
/// take a file out of a granted folder.)
#[cfg(windows)]
#[test]
fn commands_cannot_touch_secret_files_in_the_workspace() {
    const CANARY: &str = "sk-live-CANARY-51f0";
    let body = format!("API_KEY={CANARY}\n");
    for kind in [ShellKind::Cmd, ShellKind::PowerShell] {
        // The files exist before the session starts.
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(work.join("api")).unwrap();
        let files = [".env", "api/.env.production", "server.pem"];
        for file in files {
            std::fs::write(work.join(file), &body).unwrap();
        }
        std::fs::write(work.join("notes.txt"), "plain").unwrap();
        let before: Vec<String> = files.iter().map(|f| permissions(&work.join(f))).collect();
        let shell = SandboxShell::new(
            &[std::fs::canonicalize(&work).unwrap()],
            &tmp.path().join("scratch"),
            &[],
            None,
            kind,
        )
        .unwrap();
        assert!(shell.unprotected().is_empty(), "{:?}", shell.unprotected());
        assert_eq!(
            shell.hidden_secrets(),
            [".env", "api/.env.production", "server.pem"]
        );
        let run = |c: &str| block(shell.run(c, Some(Duration::from_secs(60)))).unwrap();
        let commands: &[&str] = if kind == ShellKind::Cmd {
            &[
                "type .env",
                "type api\\.env.production",
                "type server.pem",
                "more < .env",
                "findstr API .env",
                "copy .env leaked.txt && type leaked.txt",
                "ren .env plain.txt && type plain.txt",
                "echo API_KEY=changed> .env",
                "del /f /q .env",
            ]
        } else {
            &[
                "Get-Content .env",
                "Get-Content api/.env.production",
                "Get-Content server.pem",
                "[IO.File]::ReadAllText((Join-Path (Get-Location) '.env'))",
                "Select-String -Path .env -Pattern API",
                "Copy-Item .env leaked.txt; Get-Content leaked.txt",
                "Rename-Item .env plain.txt; Get-Content plain.txt",
                "Set-Content .env 'API_KEY=changed'",
                "Remove-Item .env -Force",
            ]
        };
        for cmd in commands {
            let out = run(cmd);
            assert!(
                !out.output.contains(CANARY),
                "{kind:?} `{cmd}` printed the secret: {out:?}"
            );
        }
        // A failed command naming the file is told why.
        let out = run(commands[0]);
        assert_ne!(out.exit_code, Some(0), "{kind:?}: {out:?}");
        assert!(out.output.contains("is a secret file"), "{kind:?}: {out:?}");
        // Nothing was changed, moved or removed, and the user still reads them.
        for file in files {
            assert_eq!(
                std::fs::read_to_string(work.join(file)).unwrap(),
                body,
                "{kind:?} {file}"
            );
        }
        assert!(
            !work.join("leaked.txt").exists() || {
                !std::fs::read_to_string(work.join("leaked.txt"))
                    .unwrap_or_default()
                    .contains(CANARY)
            }
        );
        // Ordinary files are unaffected.
        let out = run(if kind == ShellKind::Cmd {
            "type notes.txt"
        } else {
            "Get-Content notes.txt"
        });
        assert!(out.output.contains("plain"), "{kind:?}: {out:?}");

        // A secret file that appears during the session is hidden before
        // the next command, and so is one an editor replaced.
        std::fs::write(work.join("id_ed25519"), &body).unwrap();
        std::fs::remove_file(work.join(".env")).unwrap();
        std::fs::write(work.join(".env"), &body).unwrap();
        let (new_key, replaced) = if kind == ShellKind::Cmd {
            ("type id_ed25519", "type .env")
        } else {
            ("Get-Content id_ed25519", "Get-Content .env")
        };
        for cmd in [new_key, replaced] {
            let out = run(cmd);
            assert!(
                !out.output.contains(CANARY),
                "{kind:?} `{cmd}` printed the secret: {out:?}"
            );
        }

        // Ending the session puts the original permissions back.
        drop(shell);
        std::fs::remove_file(work.join(".env")).unwrap();
        std::fs::write(work.join(".env"), &body).unwrap();
        for (file, was) in files.iter().zip(&before) {
            assert_eq!(&permissions(&work.join(file)), was, "{kind:?} {file}");
        }
    }
}

/// Variables the core adds (the agent's git identity) reach commands, and
/// none can replace one the sandbox sets itself (PATH, TEMP, HOME...).
#[test]
fn extra_variables_reach_commands_but_cannot_replace_the_sandboxs_own() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let scratch = tmp.path().join("scratch");
    let id = gardenwall_agent::git::Identity::agent();
    let shell = match SandboxShell::new_with_env(
        std::slice::from_ref(&work),
        &scratch,
        &[],
        None,
        ShellKind::Cmd,
        &id.env(),
    ) {
        Ok(s) => s,
        Err(e) if cfg!(target_os = "linux") => {
            eprintln!("skipped: {e}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let cmd = if cfg!(windows) {
        "echo %GIT_AUTHOR_NAME%"
    } else {
        "echo $GIT_AUTHOR_NAME"
    };
    let out = block(shell.run(cmd, Some(Duration::from_secs(30)))).unwrap();
    assert!(out.output.contains("Gardenwall Agent"), "{}", out.output);
    drop(shell);
    for name in ["PATH", "Path", "TEMP", "HOME"] {
        let refused = SandboxShell::new_with_env(
            std::slice::from_ref(&work),
            &scratch,
            &[],
            None,
            ShellKind::Cmd,
            &[(name.into(), "C:/elsewhere".into())],
        );
        assert!(refused.is_err(), "{name} must be refused");
    }
}

/// Windows' folder names are set, so a program that asks the shell for
/// ProgramData gets the real path, not a literal "%SystemDrive%" folder in
/// the project.
#[cfg(windows)]
#[test]
fn system_locations_are_named_so_nothing_lands_in_the_project() {
    let Some(f) = fx() else { return };
    let out = run(&f, "echo [%SystemDrive%] [%ProgramData%]", 20);
    let drive = std::env::var("SystemDrive").unwrap();
    assert!(out.output.contains(&format!("[{drive}]")), "{}", out.output);
    assert!(!out.output.contains("%SystemDrive%"), "{}", out.output);
    assert!(!f.work.join("%SystemDrive%").exists());
}

/// Prints what differs on a machine where the shell tests fail (GitHub's
/// hosted Windows runners): Windows PowerShell 5.1's modules, programs that
/// get "Access is denied", Node's realpath, and folder permissions before,
/// during and after a session. Never fails; run on request:
/// `cargo test -p gardenwall-agent --test sandbox_shell runner_diagnostics -- --ignored --nocapture`
#[cfg(windows)]
#[test]
#[ignore]
fn runner_diagnostics() {
    fn section(name: &str) {
        println!("\n===== {name} =====");
    }
    fn host(program: &str, args: &[&str]) -> String {
        match std::process::Command::new(program).args(args).output() {
            Ok(o) => format!(
                "{}{}(exit {:?})",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
                o.status.code()
            ),
            Err(e) => format!("could not start {program}: {e}"),
        }
    }
    let acl = |p: &std::path::Path| host("icacls", &[&p.display().to_string()]);
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&tools).unwrap();
    let system32 = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    std::fs::copy(system32.join("whoami.exe"), tools.join("gwprog.exe")).unwrap();
    std::fs::copy(system32.join("hostname.exe"), tools.join("gwhost.exe")).unwrap();
    std::fs::write(work.join("before.txt"), "x").unwrap();
    std::fs::write(work.join(".env"), "KEY=1").unwrap();

    section("host");
    println!("TEMP = {:?}", std::env::var_os("TEMP"));
    println!("{}", host("cmd", &["/d", "/c", "ver"]));
    println!(
        "{}",
        host(
            "powershell.exe",
            &[
                "-NoProfile",
                "-Command",
                "$PSVersionTable.PSVersion.ToString(); $env:PSModulePath; \
                 [Reflection.Assembly]::Load('Microsoft.PowerShell.Commands.Utility, Version=3.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35').Location; \
                 (Get-Module -ListAvailable Microsoft.PowerShell.Utility | Select-Object -First 1).Path"
            ]
        )
    );

    section("permissions before the session");
    let temp_dir = PathBuf::from(std::env::var_os("TEMP").unwrap());
    for p in [
        temp_dir.as_path(),
        tmp.path(),
        &work,
        &work.join("before.txt"),
        &work.join(".env"),
    ] {
        println!("{}", acl(p));
    }

    let ps = shell_with_tools(tmp.path(), &tools, ShellKind::WindowsPowerShell);
    let run =
        |shell: &SandboxShell, c: &str| match block(shell.run(c, Some(Duration::from_secs(60)))) {
            Ok(o) => format!("{}(exit {:?})", o.output, o.exit_code),
            Err(e) => format!("error: {e}"),
        };
    section("permissions during the session");
    std::fs::write(work.join("during.txt"), "x").unwrap();
    for p in [&work, &work.join("before.txt"), &work.join("during.txt")] {
        println!("{}", acl(p));
    }

    section("Windows PowerShell 5.1 in the sandbox");
    for c in [
        "[Console]::Out.WriteLine('version ' + $PSVersionTable.PSVersion + ' mode ' + $ExecutionContext.SessionState.LanguageMode)",
        "[Console]::Out.WriteLine('PSModulePath ' + $env:PSModulePath)",
        "[Console]::Out.WriteLine('psd1 exists ' + [IO.File]::Exists(\"$env:windir\\System32\\WindowsPowerShell\\v1.0\\Modules\\Microsoft.PowerShell.Utility\\Microsoft.PowerShell.Utility.psd1\"))",
        "try { [Console]::Out.WriteLine('modules dir: ' + ([IO.Directory]::GetDirectories(\"$env:windir\\System32\\WindowsPowerShell\\v1.0\\Modules\") -join ', ')) } catch { [Console]::Out.WriteLine('list modules: ' + $_.Exception.ToString()) }",
        "try { [Console]::Out.WriteLine('assembly ' + [Reflection.Assembly]::Load('Microsoft.PowerShell.Commands.Utility, Version=3.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35').Location) } catch { [Console]::Out.WriteLine('assembly load: ' + $_.Exception.ToString()) }",
        "try { Import-Module Microsoft.PowerShell.Utility -ErrorAction Stop; [Console]::Out.WriteLine('import ok') } catch { [Console]::Out.WriteLine('import: ' + $_.Exception.ToString()) }",
        "try { Import-Module \"$env:windir\\System32\\WindowsPowerShell\\v1.0\\Modules\\Microsoft.PowerShell.Utility\\Microsoft.PowerShell.Utility.psd1\" -ErrorAction Stop; [Console]::Out.WriteLine('import by path ok') } catch { [Console]::Out.WriteLine('import by path: ' + $_.Exception.ToString()) }",
        "foreach ($m in Get-Module -ListAvailable) { [Console]::Out.WriteLine('available ' + $m.Name + ' ' + $m.Path) }",
    ] {
        println!("> {c}\n{}", run(&ps, c));
    }
    drop(ps);

    section("cmd in the sandbox");
    let cmd = shell_with_tools(tmp.path(), &tools, ShellKind::Cmd);
    for c in [
        "whoami & echo rc=%errorlevel%",
        "gwprog & echo rc=%errorlevel%",
        "gwhost & echo rc=%errorlevel%",
        "for /f \"delims=\" %F in ('gwprog') do @echo got %F",
        "for /f \"delims=\" %F in ('gwhost') do @echo got %F",
        "for /f \"delims=\" %F in ('echo plain') do @echo got %F",
    ] {
        println!("> {c}\n{}", run(&cmd, c));
    }

    let node = PathBuf::from(r"C:\Program Files\nodejs");
    if node.join("node.exe").is_file() && gardenwall_sandbox::sandbox_can_use(&node) {
        section("Node.js in the sandbox");
        std::fs::write(work.join("lib.cjs"), "module.exports = 1;").unwrap();
        let js = shell_with_tools(tmp.path(), &node, ShellKind::Cmd);
        for c in [
            "node --version",
            "node -e \"require('fs').realpath('lib.cjs', (e, p) => console.log(e ? 'realpath: ' + e.code + ' ' + e.message : 'realpath ok ' + p))\"",
            "node -e \"require('fs').realpath.native('lib.cjs', (e, p) => console.log(e ? 'native: ' + e.code + ' ' + e.message : 'native ok ' + p))\"",
            "node -e \"try { console.log('sync ok ' + require('fs').realpathSync('lib.cjs')) } catch (e) { console.log('sync: ' + e.code + ' ' + e.message) }\"",
            "node -e \"require('fs').realpath('.', (e, p) => console.log(e ? 'cwd: ' + e.code + ' ' + e.message : 'cwd ok ' + p))\"",
        ] {
            println!("> {c}\n{}", run(&js, c));
        }
        drop(js);
    } else {
        println!("\n(no Node.js the sandbox can use at {})", node.display());
    }
    drop(cmd);

    section("permissions after the session");
    std::fs::write(work.join("after.txt"), "x").unwrap();
    std::fs::remove_file(work.join(".env")).unwrap();
    std::fs::write(work.join(".env"), "KEY=2").unwrap();
    for p in [
        &work,
        &work.join("before.txt"),
        &work.join("after.txt"),
        &work.join(".env"),
    ] {
        println!("{}", acl(p));
    }
}
