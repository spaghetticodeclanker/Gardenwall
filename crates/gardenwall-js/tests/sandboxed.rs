//! Real programs in the real helper, inside its OS sandbox.

#![cfg(any(windows, target_os = "linux"))]

use std::time::Duration;

use gardenwall_agent::agent::{ScriptHost, ScriptRun, ScriptStep};
use gardenwall_agent::script_host::SandboxScriptHost;
use serde_json::json;

struct Fx {
    _tmp: tempfile::TempDir,
    host: SandboxScriptHost,
}

/// The helper alone in its own folder, as the app stages it.
fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let host = SandboxScriptHost::new(staged_helper().to_path_buf(), Some(journal))
        .with_script_time(Duration::from_secs(3));
    Fx { _tmp: tmp, host }
}

/// The helper is copied once, before any test starts a program: on Linux a
/// copy made while another test thread was starting its helper could not
/// be run ("executable file busy": the starting process briefly held the
/// copy's open handle). The folder is left in the temp folder.
fn staged_helper() -> &'static std::path::Path {
    static HELPER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HELPER.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep().join("js");
        std::fs::create_dir_all(&dir).unwrap();
        // Cross-built tests run from another system (WSL), where the path from
        // the build doesn't exist: the helper is then beside the tests' folder.
        let built = std::path::PathBuf::from(env!("CARGO_BIN_EXE_gardenwall-js"));
        let source = if built.is_file() {
            built
        } else {
            let exe = std::env::current_exe().unwrap();
            exe.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("gardenwall-js")
        };
        let helper = dir.join(source.file_name().unwrap());
        std::fs::copy(source, &helper).unwrap();
        helper
    })
}

fn tools() -> Vec<String> {
    ["read", "write"].map(String::from).to_vec()
}

async fn finish(mut run: Box<dyn ScriptRun>) -> ScriptStep {
    run.next(None).await
}

#[tokio::test]
async fn a_program_calls_tools_through_the_core_and_returns() {
    let f = fx();
    let mut run = f
        .host
        .start(
            r#"
            let total = 0;
            for (const p of ["a.txt", "b.txt"]) total += read({ path: p }).length;
            try { write({ path: "c.txt", content: "x" }); } catch (e) { console.log("write: " + e.message); }
            return { total };
            "#,
            &tools(),
        )
        .unwrap();
    let mut asked = Vec::new();
    let mut reply = None;
    let end = loop {
        match run.next(reply.take()).await {
            ScriptStep::Call { name, args } => {
                reply = Some(if name == "read" {
                    ("12345".to_string(), false)
                } else {
                    ("The user denied this action.".to_string(), true)
                });
                asked.push((name, args));
            }
            other => break other,
        }
    };
    assert_eq!(
        asked,
        [
            ("read".to_string(), json!({ "path": "a.txt" })),
            ("read".to_string(), json!({ "path": "b.txt" })),
            (
                "write".to_string(),
                json!({ "path": "c.txt", "content": "x" })
            ),
        ]
    );
    assert_eq!(
        end,
        ScriptStep::Done {
            value: "{\n  \"total\": 10\n}".into(),
            logs: "write: The user denied this action.\n".into()
        }
    );
    // The sandbox is kept: a second program starts in it.
    let again = finish(f.host.start("return 'again';", &tools()).unwrap()).await;
    assert_eq!(
        again,
        ScriptStep::Done {
            value: "again".into(),
            logs: String::new()
        }
    );
}

#[tokio::test]
async fn errors_come_back_with_their_line() {
    let f = fx();
    let end = finish(
        f.host
            .start("const a = 1;\nnull.missing;", &tools())
            .unwrap(),
    )
    .await;
    let ScriptStep::Failed { message, .. } = end else {
        panic!("{end:?}")
    };
    assert!(
        message.starts_with("TypeError") && message.ends_with("(line 2)"),
        "{message}"
    );
}

#[tokio::test]
async fn a_program_that_never_yields_is_ended() {
    let f = fx();
    // No loop for the engine's own limit to count: a regular expression
    // that backtracks for a very long time.
    let end = finish(
        f.host
            .start(r#"return /^(a+)+$/.test("a".repeat(60) + "b");"#, &tools())
            .unwrap(),
    )
    .await;
    let ScriptStep::Failed { message, .. } = end else {
        panic!("{end:?}")
    };
    assert!(message.contains("more than 3 seconds"), "{message}");
}

#[tokio::test]
async fn a_program_that_eats_memory_is_ended() {
    let f = fx();
    let end = finish(
        f.host
            .start(
                "const a = []; for (;;) a.push(new Array(1000000).fill(a.length));",
                &tools(),
            )
            .unwrap(),
    )
    .await;
    assert!(matches!(end, ScriptStep::Failed { .. }), "{end:?}");
}

#[tokio::test]
async fn dropping_a_run_ends_the_helper() {
    let f = fx();
    let mut run = f
        .host
        .start("read({ path: 'a' }); return 1;", &tools())
        .unwrap();
    assert!(matches!(run.next(None).await, ScriptStep::Call { .. }));
    drop(run);
    // Only one process may exist in the sandbox, so this start proves the
    // first helper is gone.
    let end = finish(f.host.start("return 2;", &tools()).unwrap()).await;
    assert_eq!(
        end,
        ScriptStep::Done {
            value: "2".into(),
            logs: String::new()
        }
    );
}
