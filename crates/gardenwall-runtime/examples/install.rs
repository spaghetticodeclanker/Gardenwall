//! Install (if needed) and verify a pinned runtime build.
//!
//! Usage: cargo run -p gardenwall-runtime --example install -- <store-root> [build-id]
//! Without a build id, lists the builds for this platform.

use std::process::ExitCode;
use std::time::Instant;

use gardenwall_runtime::download::Downloader;
use gardenwall_runtime::pins;
use gardenwall_runtime::store::RuntimeStore;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(root) = args.first() else {
        eprintln!("usage: install <store-root> [build-id]");
        return ExitCode::FAILURE;
    };
    let Some(id) = args.get(1) else {
        if let Some((os, arch)) = pins::current_platform() {
            for b in pins::builds_for(os, arch) {
                println!(
                    "{}  ({:?}, {:.0} MB download)",
                    b.id,
                    b.backend,
                    b.download_size() as f64 / 1e6
                );
            }
        }
        return ExitCode::SUCCESS;
    };
    let Some(build) = pins::find(id) else {
        eprintln!("unknown build id");
        return ExitCode::FAILURE;
    };
    let store = RuntimeStore::new(root);
    let result = async {
        if !store.is_installed(build) {
            let started = Instant::now();
            let mut last = 0u64;
            store
                .install(build, &Downloader::new()?, |done, total| {
                    if done - last > 16 << 20 || done == total {
                        last = done;
                        println!("  {:>5.1}%", done as f64 * 100.0 / total as f64);
                    }
                    true
                })
                .await?;
            println!("installed {} in {:?}", build.id, started.elapsed());
        }
        let started = Instant::now();
        let installed = store.verify(build)?;
        println!(
            "verified in {:?}: {}",
            started.elapsed(),
            installed.server_exe.display()
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
