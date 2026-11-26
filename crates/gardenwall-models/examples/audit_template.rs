//! Audit the chat template of a GGUF file (the first part of a split model),
//! or a template saved as text, and print each finding with its risk.
//!
//! Usage: cargo run -p gardenwall-models --example audit_template -- <model.gguf>
//!        cargo run -p gardenwall-models --example audit_template -- --text <template.jinja>
//!
//! The text form checks a template before its model is downloaded (the
//! starter list's entries: the hub returns a repository's template with
//! `expand[]=gguf`). The download still audits the file's own template.

use std::process::ExitCode;

use gardenwall_models::gguf::{self, GgufLimits};
use gardenwall_models::template;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = match args.as_slice() {
        [flag, path] if flag == "--text" => match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        },
        [path] => {
            let summary = match std::fs::File::open(path)
                .map_err(gguf::GgufError::Io)
                .and_then(|f| gguf::validate(f, &GgufLimits::default()))
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let Some(text) = summary.chat_template() else {
                println!("no embedded chat template");
                return ExitCode::SUCCESS;
            };
            text.to_string()
        }
        _ => {
            eprintln!("usage: audit_template <model.gguf> | --text <template>");
            return ExitCode::FAILURE;
        }
    };
    let audit = template::audit(&text);
    println!("sha256: {}", audit.sha256);
    println!(
        "template: {} bytes, acknowledgement required: {}",
        text.len(),
        audit.requires_acknowledgement()
    );
    for f in &audit.findings {
        println!(
            "[{:?}] {}{}",
            f.kind.risk(),
            f.kind.code(),
            f.kind
                .detail()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        if let Some(e) = &f.excerpt {
            println!("    {}", e.replace('\n', " / "));
        }
    }
    ExitCode::SUCCESS
}
