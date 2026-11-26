//! Import a local GGUF file into quarantine through the intake pipeline.
//!
//! Usage: cargo run -p gardenwall-models --example import -- <file> <quarantine-dir> [expected-sha256]

use std::path::Path;
use std::process::ExitCode;

use gardenwall_models::import::import_local;
use gardenwall_models::integrity::Sha256Digest;
use gardenwall_models::template;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: import <file> <quarantine-dir> [expected-sha256]");
        return ExitCode::FAILURE;
    }
    let expected = match args.get(2).map(|h| Sha256Digest::from_hex(h)) {
        Some(Some(d)) => Some(d),
        Some(None) => {
            eprintln!("expected SHA-256 must be 64 hex characters");
            return ExitCode::FAILURE;
        }
        None => None,
    };
    let started = std::time::Instant::now();
    match import_local(
        Path::new(&args[0]),
        Path::new(&args[1]),
        expected.as_ref(),
        |_, _| true,
    ) {
        Ok(model) => {
            println!("imported in {:?}", started.elapsed());
            println!("  path:       {}", model.path.display());
            println!(
                "  provenance: {}",
                serde_json::to_string(&model.provenance).unwrap_or_default()
            );
            println!(
                "  model:      {} ({} tensors, context {:?})",
                model.summary.architecture(),
                model.summary.tensors.len(),
                model.summary.context_length()
            );
            if let Some(t) = model.summary.chat_template() {
                let audit = template::audit(t);
                println!(
                    "  template:   {} finding(s), acknowledgement required: {}",
                    audit.findings.len(),
                    audit.requires_acknowledgement()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("REJECTED: {e}");
            ExitCode::from(2)
        }
    }
}
