//! Launch the llama-server sidecar through the supervisor and stream one chat.
//!
//! Usage: cargo run -p gardenwall-inference --example chat -- <llama-server> <model.gguf> <work-dir> <prompt> [relay-exe]
//!
//! With `relay-exe` (Windows), the runtime runs sandboxed behind the relay.
//! `GW_SPEC=ngram` or `GW_SPEC=<draft.gguf>` turns on speculative decoding;
//! `GW_GRANT_JOURNAL=<dir>` records sandbox grants there (crash-safe).
//! `GW_CTX=<tokens>` fixes the context; `GW_KV=q8` uses the 8-bit cache.
//! A split model (`...-00001-of-0000N.gguf`) gets its other parts granted.

use std::io::Write;
use std::ops::ControlFlow;
use std::process::ExitCode;

use gardenwall_inference::client::{ChatMessage, ChatRequest, Role, StreamEvent};
use gardenwall_inference::launch::{GpuOffload, KvCacheType, LaunchSpec, Speculative};
use gardenwall_inference::supervisor::{self, Isolation, Timeouts};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (exe, model, work, prompt, relay) = match args.as_slice() {
        [e, m, w, p] => (e, m, w, p, None),
        [e, m, w, p, r] => (e, m, w, p, Some(r)),
        _ => {
            eprintln!("usage: chat <llama-server> <model.gguf> <work-dir> <prompt> [relay-exe]");
            return ExitCode::FAILURE;
        }
    };
    let isolation = match relay {
        #[cfg(windows)]
        Some(r) => Isolation::Sandboxed {
            relay_exe: std::fs::canonicalize(r).expect("relay path"),
            grant_journal_dir: std::env::var_os("GW_GRANT_JOURNAL").map(Into::into),
        },
        #[cfg(not(windows))]
        Some(_) => {
            eprintln!("sandboxed launch is only implemented on Windows");
            return ExitCode::FAILURE;
        }
        None => Isolation::Unsandboxed,
    };
    let spec = LaunchSpec {
        server_exe: exe.into(),
        model: std::fs::canonicalize(model).expect("model path"),
        model_parts: split_siblings(&std::fs::canonicalize(model).expect("model path")),
        parallel: 1,
        min_fit_context: std::env::var("GW_MIN_CTX")
            .ok()
            .and_then(|v| v.parse().ok()),
        // Intake names quarantined models `<sha256>.gguf`.
        // A split model's id is its folder's name.
        model_sha256: {
            let p = std::path::Path::new(model);
            let hex = |s: Option<&str>| {
                s.filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase)
            };
            hex(p.file_stem().and_then(|s| s.to_str())).or_else(|| {
                hex(std::fs::canonicalize(p)
                    .ok()
                    .as_deref()
                    .and_then(|c| c.parent())
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str()))
            })
        },
        chat_template_file: None,
        context_size: std::env::var("GW_CTX").ok().and_then(|v| v.parse().ok()),
        gpu: GpuOffload::Auto,
        kv_cache: if std::env::var("GW_KV").as_deref() == Ok("q8") {
            KvCacheType::Q8_0
        } else {
            KvCacheType::F16
        },
        work_dir: work.into(),
        extra_env: vec![],
        // The example assumes the pinned build; the app detects features.
        features: gardenwall_inference::compat::RuntimeFeatures::all_known(),
        embeddings: false,
        mmproj: None,
        speculative: match std::env::var("GW_SPEC").ok().as_deref() {
            None | Some("") | Some("off") => Speculative::Off,
            Some("ngram") => Speculative::Ngram,
            Some("mtp") => Speculative::Mtp,
            Some(draft) => Speculative::Draft(std::fs::canonicalize(draft).expect("draft path")),
        },
        moe_cache_mib: std::env::var("GW_MOE_CACHE_MIB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };
    let result = async {
        let started = std::time::Instant::now();
        let server = supervisor::launch(&spec, &isolation, Timeouts::default()).await?;
        println!(
            "[ready in {:?}, pid {:?}, isolation {:?}]",
            started.elapsed(),
            server.pid(),
            server.isolation()
        );
        if std::env::var_os("GW_DUMP_LOG").is_some() {
            for line in server.logs().lines() {
                println!("log| {line}");
            }
        }
        let p = server.logs().placement();
        println!(
            "[weights: {:.1} GiB on GPU, {:.1} GiB in RAM, layers {:?}, overflowing {:?}]",
            p.gpu_mib / 1024.0,
            p.ram_mib / 1024.0,
            p.layers,
            p.overflowing_layers
        );
        let props = server.client().props().await?;
        println!(
            "[context window {} tokens, tools: {}, build {:?}]",
            props.context_window, props.supports_tools, props.build
        );

        let template_kwargs = match std::env::var("GW_THINK").ok().as_deref() {
            Some("off") => Some(serde_json::json!({"enable_thinking": false})),
            Some(level @ ("low" | "medium")) => Some(serde_json::json!({"reasoning_effort": level})),
            _ => None,
        }
        .and_then(|v| v.as_object().cloned());
        let request = ChatRequest {
            messages: vec![ChatMessage::new(Role::User, prompt.as_str())],
            template_kwargs,
            ..ChatRequest::default()
        };
        let quiet = std::env::var_os("GW_QUIET").is_some();
        let response = server
            .client()
            .chat(&request, |event| {
                if !quiet {
                    match event {
                        StreamEvent::Content(t) => print!("{t}"),
                        StreamEvent::Reasoning(t) => print!("\x1b[2m{t}\x1b[0m"),
                        _ => {}
                    }
                    let _ = std::io::stdout().flush();
                }
                ControlFlow::Continue(())
            })
            .await?;
        println!();
        // Repetition: how often the most common line of the thinking recurs.
        let mut counts = std::collections::HashMap::new();
        for line in response.reasoning.lines().map(str::trim).filter(|l| l.len() > 8) {
            *counts.entry(line).or_insert(0u32) += 1;
        }
        let top = counts.values().max().copied().unwrap_or(0);
        println!(
            "[finish {:?}, answer {} chars, thinking {} chars, completion {:?} tokens, most repeated thinking line x{top}]",
            response.finish_reason,
            response.content.len(),
            response.reasoning.len(),
            response.usage.map(|u| u.completion_tokens),
        );
        if !response.content.is_empty() {
            println!("[answer starts: {:?}]", response.content.chars().take(160).collect::<String>());
        }
        if let Some(u) = response.usage {
            println!(
                "[context used {} / {} tokens ({:.1}%)]",
                u.total_tokens,
                props.context_window,
                u.total_tokens as f64 * 100.0 / props.context_window as f64
            );
        }
        if let Some(t) = response.timings {
            println!(
                "[prompt {:.0} tok/s, generation {:.0} tok/s, ttft {:?}]",
                t.prompt_per_second, t.predicted_per_second, response.time_to_first_token
            );
            if t.draft_n > 0 {
                println!(
                    "[draft: {} of {} guessed tokens kept]",
                    t.draft_n_accepted, t.draft_n
                );
            }
        }
        // A second, different request shows warm (post-first-request) speed.
        let warm = ChatRequest {
            messages: vec![ChatMessage::new(
                Role::User,
                "Name three primary colors. /no_think",
            )],
            ..ChatRequest::default()
        };
        let response = server
            .client()
            .chat(&warm, |_| ControlFlow::Continue(()))
            .await?;
        if let Some(t) = response.timings {
            println!(
                "[warm request: prompt {:.0} tok/s, generation {:.0} tok/s, ttft {:?}]",
                t.prompt_per_second, t.predicted_per_second, response.time_to_first_token
            );
        }
        if std::env::var_os("GW_DUMP_LOG").is_some() {
            for line in server.logs().lines().iter().rev().take(40).rev() {
                println!("endlog| {line}");
            }
        }
        server.shutdown().await;
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

/// The other parts of a split model next to `first` (`<base>-00001-of-0000N.gguf`).
fn split_siblings(first: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(name) = first.file_name().and_then(|n| n.to_str()) else {
        return vec![];
    };
    let Some(stem) = name.strip_suffix(".gguf") else {
        return vec![];
    };
    if stem.len() < 15 || !stem.is_char_boundary(stem.len() - 15) {
        return vec![];
    }
    let (base, tail) = stem.split_at(stem.len() - 15);
    let Some(count) = tail
        .strip_prefix("-00001-of-")
        .and_then(|c| c.parse::<u32>().ok())
    else {
        return vec![];
    };
    (2..=count)
        .map(|i| first.with_file_name(format!("{base}-{i:05}-of-{count:05}.gguf")))
        .collect()
}
