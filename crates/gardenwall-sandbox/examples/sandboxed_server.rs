//! Start llama-server inside the Windows sandbox and report whether it loads.
//!
//! Usage: cargo run -p gardenwall-sandbox --example sandboxed_server -- <runtime-dir> <model.gguf>

#[cfg(windows)]
fn main() {
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;

    use gardenwall_sandbox::{Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = PathBuf::from(&args[0]);
    let model = PathBuf::from(&args[1]);
    let policy = SandboxPolicy {
        profile: "Gardenwall.Example.Inference".into(),
        grants: vec![
            PathGrant {
                path: runtime.clone(),
                access: Access::ReadExecute,
            },
            PathGrant {
                path: model.clone(),
                access: Access::Read,
            },
        ],
        network: Network::Denied,
        limits: Limits {
            max_processes: 1,
            ..Limits::default()
        },
        env: vec![(
            "SystemRoot".into(),
            std::env::var_os("SystemRoot").unwrap_or_default(),
        )],
        working_dir: Some(runtime.clone()),
        grant_journal_dir: None,
    };
    let sandbox = Sandbox::new(policy).expect("sandbox");
    let exe = std::fs::canonicalize(runtime.join("llama-server.exe")).unwrap();
    let model_arg = std::fs::canonicalize(&model).unwrap();
    let model_arg = model_arg.to_str().unwrap().trim_start_matches(r"\?\");
    let mut child = sandbox
        .spawn(
            &exe,
            &[
                "-m",
                model_arg,
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--no-webui",
                "--no-webui-mcp-proxy",
                "--offline",
                "--no-slots",
                "-c",
                "4096",
                "-ngl",
                "99",
            ]
            .map(OsStr::new),
        )
        .expect("spawn");
    let stderr = child.stderr.take().unwrap();
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        if line.contains("Vulkan0")
            || line.contains("model loaded")
            || line.contains("listening")
            || line.contains("error")
        {
            println!("{line}");
        }
        if line.contains("listening") || line.contains("exiting") {
            break;
        }
    }
    drop(sandbox);
    println!(
        "exit after sandbox drop: {:?}",
        child.wait_timeout(Some(std::time::Duration::from_secs(5)))
    );
    let _ = gardenwall_sandbox::windows::delete_profile("Gardenwall.Example.Inference");
}

#[cfg(not(windows))]
fn main() {}
