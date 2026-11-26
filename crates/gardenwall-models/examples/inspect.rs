//! Validate a GGUF file and print a structural summary.
//!
//! Usage: cargo run -p gardenwall-models --example inspect -- <path-to-model.gguf>

use std::fs::File;
use std::process::ExitCode;

use gardenwall_models::gguf::{GgufLimits, validate};
use gardenwall_models::template;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: inspect <model.gguf>");
        return ExitCode::FAILURE;
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("cannot open file: {error}");
            return ExitCode::FAILURE;
        }
    };
    let started = std::time::Instant::now();
    match validate(file, &GgufLimits::default()) {
        Ok(summary) => {
            println!("valid GGUF v{} ({:?})", summary.version, started.elapsed());
            println!("  architecture:   {}", summary.architecture());
            println!("  name:           {}", summary.name().unwrap_or("-"));
            println!("  context length: {:?}", summary.context_length());
            println!("  block count:    {:?}", summary.block_count());
            println!("  embedding:      {:?}", summary.embedding_length());
            println!("  metadata keys:  {}", summary.metadata.len());
            println!("  tensors:        {}", summary.tensors.len());
            println!("  tensor bytes:   {}", summary.total_tensor_bytes);
            println!("  file bytes:     {}", summary.file_size);
            println!(
                "  chat template:  {}",
                summary
                    .chat_template()
                    .map_or("none".to_string(), |t| format!("{} bytes", t.len()))
            );
            if let Some(t) = summary.chat_template() {
                let audit = template::audit(t);
                println!("  template sha256: {}", audit.sha256);
                println!(
                    "  template audit: {} finding(s), acknowledgement required: {}",
                    audit.findings.len(),
                    audit.requires_acknowledgement()
                );
                for finding in &audit.findings {
                    // Excerpts are untrusted; print them escaped.
                    println!(
                        "    [{:?}] {:?} {}",
                        finding.severity,
                        finding.kind,
                        finding
                            .excerpt
                            .as_deref()
                            .map(|e| format!("{e:?}"))
                            .unwrap_or_default()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("REJECTED: {error}");
            ExitCode::from(2)
        }
    }
}
