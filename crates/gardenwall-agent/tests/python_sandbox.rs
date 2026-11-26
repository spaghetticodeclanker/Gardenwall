//! Python inside the agent's sandbox keeps the sandbox's boundary: it can
//! change files only in the granted folder, can't read or write elsewhere
//! (its own install included), has no network, leaves no bytecode in the
//! project and sees no user site-packages.
//!
//! Uses the Python the app would offer (an all-users install sandboxes can
//! read), or the folder in `GARDENWALL_TEST_PYTHON_DIR`; skipped when there
//! is neither.
#![cfg(windows)]

use std::path::PathBuf;
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

fn python_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GARDENWALL_TEST_PYTHON_DIR") {
        return Some(PathBuf::from(dir));
    }
    gardenwall_agent::python::find_python().and_then(|p| p.parent().map(PathBuf::from))
}

const CHECK: &str = r#"
import os, socket, sys, site, importlib
def attempt(name, f):
    try:
        f()
        print(name + "=ALLOWED")
    except Exception as e:
        print(name + "=DENIED " + type(e).__name__)
outside = sys.argv[1]
attempt("write_project", lambda: open("made.txt", "w").write("hi"))
attempt("read_outside", lambda: open(os.path.join(outside, "secret.txt")).read())
attempt("write_outside", lambda: open(os.path.join(outside, "planted.txt"), "w").write("x"))
attempt("write_python", lambda: open(os.path.join(sys.base_prefix, "planted.txt"), "w").write("x"))
attempt("network", lambda: socket.create_connection(("1.1.1.1", 80), timeout=3))
sys.path.insert(0, os.getcwd())
importlib.import_module("helper")
print("pycache_prefix=" + str(sys.pycache_prefix))
print("user_site=" + str(site.ENABLE_USER_SITE))
print("pythonpath=" + str(os.environ.get("PYTHONPATH")))
"#;

#[test]
fn python_keeps_the_sandbox_boundary() {
    let Some(py) = python_dir() else {
        eprintln!("skipped: no Python that sandboxes can read (see Settings → Agent)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let outside = tmp.path().join("outside");
    let scratch = tmp.path().join("scratch");
    for d in [&work, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();
    std::fs::write(work.join("check.py"), CHECK).unwrap();
    std::fs::write(work.join("helper.py"), "VALUE = 1\n").unwrap();
    let granted = std::fs::canonicalize(&work).unwrap();
    let shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &scratch,
        std::slice::from_ref(&py),
        None,
        ShellKind::PowerShell,
    )
    .unwrap();

    // The model is told which Python it has.
    let g = shell.guidelines().join("\n");
    assert!(
        g.contains("Python 3.") && g.contains("pip install fails"),
        "{g}"
    );

    let cmd = format!("python check.py '{}'", outside.display());
    let out = block(shell.run(&cmd, Some(Duration::from_secs(120)))).unwrap();
    let o = &out.output;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(o.contains("write_project=ALLOWED"), "{o}");
    for denied in ["read_outside", "write_outside", "write_python", "network"] {
        assert!(o.contains(&format!("{denied}=DENIED")), "{denied}: {o}");
    }
    assert!(!outside.join("planted.txt").exists());
    assert!(!py.join("planted.txt").exists());
    assert_eq!(
        std::fs::read_to_string(work.join("made.txt")).unwrap(),
        "hi"
    );
    // Bytecode goes to the session's scratch folder, not the project.
    assert!(
        o.contains("pycache_prefix=") && o.contains("pycache"),
        "{o}"
    );
    assert!(
        !work.join("__pycache__").exists(),
        "bytecode was left in the project"
    );
    assert!(o.contains("user_site=False"), "{o}");
    assert!(o.contains("pythonpath=None"), "{o}");
}

#[test]
fn without_python_the_model_is_told_so() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let granted = std::fs::canonicalize(&work).unwrap();
    let shell = SandboxShell::new(
        std::slice::from_ref(&granted),
        &tmp.path().join("scratch"),
        &[],
        None,
        ShellKind::Cmd,
    )
    .unwrap();
    let g = shell.guidelines().join("\n");
    assert!(g.contains("Python is not available in this sandbox"), "{g}");
}
