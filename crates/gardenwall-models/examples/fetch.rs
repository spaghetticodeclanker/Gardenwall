//! Resolve a hub repository, download one GGUF file into quarantine, verify
//! its SHA-256 and validate it.
//!
//! Usage: cargo run -p gardenwall-models --example fetch -- <owner/repo> <file.gguf> <quarantine-dir>

use std::path::PathBuf;
use std::process::ExitCode;

use gardenwall_models::hub::{HubClient, HubConfig, RepoId};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, file, dir] = args.as_slice() else {
        eprintln!("usage: fetch <owner/repo> <file.gguf> <quarantine-dir>");
        return ExitCode::FAILURE;
    };
    let result = async {
        let client = HubClient::new(HubConfig::hugging_face())?;
        let repo = RepoId::parse(repo)?;
        let resolved = client.resolve(&repo, "main").await?;
        println!("resolved {} @ {}", resolved.repo, resolved.commit.as_str());
        for f in &resolved.files {
            println!("  {} ({} bytes) sha256={}", f.path, f.size, f.sha256);
        }
        let remote = resolved
            .files
            .iter()
            .find(|f| f.path == *file)
            .ok_or("file not found or not verifiable")?;
        let mut last = 0u64;
        let started = std::time::Instant::now();
        let model = client
            .download(remote, &PathBuf::from(dir), |done, total| {
                if done - last >= 64 << 20 || done == total {
                    last = done;
                    println!("  {:>5.1}%", done as f64 * 100.0 / total as f64);
                }
                true
            })
            .await?;
        println!(
            "verified and validated in {:?}: {} ({} tensors, arch {})",
            started.elapsed(),
            model.path.display(),
            model.summary.tensors.len(),
            model.summary.architecture()
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
