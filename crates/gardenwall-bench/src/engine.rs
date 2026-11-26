//! Model-server suites: S1 (raw speed across launch settings), S6 (chat)
//! and the vision half of S5. Requests use temperature 0 and a fixed seed;
//! each prompt starts with a fresh nonce so the server's prompt cache
//! can't skip the work being measured (except where reuse is the point).

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use gardenwall_inference::client::{
    ChatMessage, ChatRequest, ChatResponse, InferenceClient, Role, Sampling, StreamEvent,
};
use serde::{Deserialize, Serialize};

use crate::common::{self, Health, Kv, ServerConfig, Spec, Spread};
use crate::fixtures::{self, Rng};

/// What one request measured.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub prompt_tokens: u64,
    /// Tokens the server reused from its prompt cache.
    pub cached_tokens: u64,
    pub prompt_per_second: f64,
    pub generated_tokens: u64,
    pub generation_per_second: f64,
    pub ttft_ms: f64,
    pub wall_ms: f64,
}

fn sample(r: &ChatResponse, wall: Duration) -> Sample {
    let t = r.timings.unwrap_or_default();
    Sample {
        prompt_tokens: t.prompt_n,
        cached_tokens: t.cache_n,
        prompt_per_second: t.prompt_per_second,
        generated_tokens: t.predicted_n,
        generation_per_second: t.predicted_per_second,
        ttft_ms: r.time_to_first_token.map_or(0.0, common::ms),
        wall_ms: common::ms(wall),
    }
}

/// Filler text of about `tokens` tokens (about 4 characters per token),
/// starting with a nonce.
pub fn filler(tokens: usize, rng: &mut Rng) -> String {
    let mut text = format!("Reference {:016x}.\n", rng.next());
    while text.len() < tokens * 4 {
        text.push_str(&fixtures::sentence(rng));
        text.push(' ');
    }
    text
}

fn request(messages: Vec<ChatMessage>, max_tokens: u32, seed: u64) -> ChatRequest {
    ChatRequest {
        messages,
        tools: vec![],
        sampling: Sampling {
            temperature: Some(0.0),
            seed: Some(seed),
            max_tokens: Some(max_tokens),
            ..Default::default()
        },
        // No thinking: the generated text is the measured text.
        template_kwargs: serde_json::json!({"enable_thinking": false})
            .as_object()
            .cloned(),
    }
}

async fn send(client: &InferenceClient, req: &ChatRequest) -> Result<Sample, String> {
    let t = Instant::now();
    let r = client
        .chat(req, |_| ControlFlow::Continue(()))
        .await
        .map_err(|e| e.to_string())?;
    Ok(sample(&r, t.elapsed()))
}

const STORY: &str =
    "Write a long, detailed story inspired by the text above. Keep writing until you are stopped.";

// ---------- S1: engine ----------

/// A named server configuration in the engine matrix. Parts combine with
/// `+` (`mtp+slots-2`, `mtp+mmproj`); `mmproj` loads the main model's image
/// projector from machine.json.
pub fn engine_config(
    name: &str,
    server: &std::path::Path,
    model: &std::path::Path,
    mmproj: Option<&std::path::Path>,
) -> Result<ServerConfig, String> {
    let mut c = ServerConfig::new(server, model);
    c.context = Some(32_768);
    for part in name.split('+') {
        apply_part(&mut c, part, mmproj)?;
    }
    Ok(c)
}

/// Apply one part of an engine config name.
pub fn apply_part(
    c: &mut ServerConfig,
    part: &str,
    mmproj: Option<&std::path::Path>,
) -> Result<(), String> {
    match part {
        "base" | "moe-nocache" => {}
        "kv-q8" => c.kv = Kv::Q8,
        "ctx-auto" => c.context = None,
        "ngram" => c.speculative = Spec::Ngram,
        "mtp" => c.speculative = Spec::Mtp,
        "slots-2" => c.parallel = 2,
        "slots-4" => c.parallel = 4,
        "sandboxed" => c.sandboxed = true,
        "moe-cache4g" => c.moe_cache_mib = 4096,
        "mmproj" => {
            c.mmproj = Some(
                mmproj
                    .ok_or("the mmproj config part needs mainMmproj in machine.json")?
                    .to_path_buf(),
            )
        }
        other => {
            return Err(format!(
                "unknown engine config {other} (base, kv-q8, ctx-auto, ngram, mtp, slots-2, slots-4, sandboxed, moe-nocache, moe-cache4g, mmproj; combine with +)"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineResult {
    pub config: String,
    pub label: String,
    pub model: String,
    pub context_window: u64,
    pub load_ms: u64,
    /// The first start of this model in the run (the file may not be in
    /// the OS cache yet).
    pub first_load: bool,
    pub vram_gib_loaded: Option<f64>,
    pub vram_gib_peak: Option<f64>,
    /// Per prompt size (tokens asked for).
    pub prompts: Vec<PromptResult>,
    /// Slots > 1: total and per-request generation speed with that many
    /// requests at once.
    pub concurrent: Option<Concurrent>,
    pub error: Option<String>,
    pub health: Health,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub size: usize,
    pub samples: Vec<Sample>,
    pub prompt_per_second: Option<Spread>,
    pub generation_per_second: Option<Spread>,
    pub ttft_ms: Option<Spread>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Concurrent {
    pub requests: usize,
    pub total_generation_per_second: Option<Spread>,
    pub per_request_generation_per_second: Option<Spread>,
}

pub struct EngineOptions {
    pub configs: Vec<(String, ServerConfig)>,
    pub sizes: Vec<usize>,
    pub generate: u32,
    pub reps: usize,
    pub seed: u64,
}

pub async fn run_engine(o: &EngineOptions) -> Result<Vec<EngineResult>, String> {
    let mut out = Vec::new();
    let mut loaded_models: Vec<std::path::PathBuf> = Vec::new();
    let mut rng = Rng::new(o.seed);
    for (name, cfg) in &o.configs {
        if common::stop_requested() {
            return Err(common::PAUSED.into());
        }
        let first_load = !loaded_models.contains(&cfg.model);
        loaded_models.push(cfg.model.clone());
        eprintln!("[engine] {name}: starting ({}) …", cfg.label());
        let mut result = EngineResult {
            config: name.clone(),
            label: cfg.label(),
            model: common::model_name(&cfg.model),
            context_window: 0,
            load_ms: 0,
            first_load,
            vram_gib_loaded: None,
            vram_gib_peak: None,
            prompts: vec![],
            concurrent: None,
            error: None,
            health: Health::default(),
        };
        let started = match common::start_server(cfg).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[engine] {name}: {e}");
                result.error = Some(e);
                out.push(result);
                continue;
            }
        };
        result.context_window = started.context_window;
        result.load_ms = started.load_ms;
        let pid = started.server.pid();
        result.vram_gib_loaded = common::process_vram_gib(pid);
        let client = &started.client;
        // Warm-up (discarded).
        let _ = send(
            client,
            &request(vec![ChatMessage::new(Role::User, "Say hello.")], 16, o.seed),
        )
        .await;
        let mut peak = result.vram_gib_loaded;
        for &size in &o.sizes {
            if (size + o.generate as usize + 256) as u64 > started.context_window {
                continue;
            }
            let mut samples = Vec::new();
            for rep in 0..o.reps {
                let text = filler(size, &mut rng);
                let req = request(
                    vec![ChatMessage::new(Role::User, format!("{text}\n\n{STORY}"))],
                    o.generate,
                    o.seed + rep as u64,
                );
                match send(client, &req).await {
                    Ok(s) => samples.push(s),
                    Err(e) => {
                        result.error = Some(format!("prompt {size}: {e}"));
                        break;
                    }
                }
                let now = common::process_vram_gib(pid);
                if now > peak {
                    peak = now;
                }
            }
            let f =
                |g: fn(&Sample) -> f64| common::spread(&samples.iter().map(g).collect::<Vec<_>>());
            eprintln!(
                "[engine] {name}: {size} tokens: prompt {:.0} tok/s, generation {:.1} tok/s",
                f(|s| s.prompt_per_second).map_or(0.0, |s| s.median),
                f(|s| s.generation_per_second).map_or(0.0, |s| s.median)
            );
            result.prompts.push(PromptResult {
                size,
                prompt_per_second: f(|s| s.prompt_per_second),
                generation_per_second: f(|s| s.generation_per_second),
                ttft_ms: f(|s| s.ttft_ms),
                samples,
            });
        }
        if cfg.parallel > 1 {
            let n = cfg.parallel as usize;
            let (mut totals, mut each) = (Vec::new(), Vec::new());
            for rep in 0..o.reps {
                let t = Instant::now();
                let handles: Vec<_> = (0..n)
                    .map(|i| {
                        let client = client.clone();
                        let text = filler(4096, &mut rng);
                        let req = request(
                            vec![ChatMessage::new(Role::User, format!("{text}\n\n{STORY}"))],
                            o.generate,
                            o.seed + (rep * n + i) as u64,
                        );
                        tokio::spawn(async move { send(&client, &req).await })
                    })
                    .collect();
                let mut generated = 0u64;
                for h in handles {
                    if let Ok(Ok(s)) = h.await {
                        generated += s.generated_tokens;
                        each.push(s.generation_per_second);
                    }
                }
                totals.push(generated as f64 / t.elapsed().as_secs_f64());
            }
            result.concurrent = Some(Concurrent {
                requests: n,
                total_generation_per_second: common::spread(&totals),
                per_request_generation_per_second: common::spread(&each),
            });
        }
        result.vram_gib_peak = peak;
        result.health = common::health();
        started.server.shutdown().await;
        out.push(result);
    }
    Ok(out)
}

// ---------- S6: chat ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatResult {
    pub short: Vec<Sample>,
    /// A conversation of about 20K tokens: the first request.
    pub long_first: Vec<Sample>,
    /// The next turn of that conversation (the prompt cache should cover
    /// the history).
    pub long_next: Vec<Sample>,
    /// Time from Stop until the request was released.
    pub stop_ms: Vec<f64>,
    /// First token of the request right after a Stop.
    pub after_stop_ttft_ms: Vec<f64>,
    pub summary: ChatSummary,
    pub health: Health,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub short_ttft_ms: Option<Spread>,
    pub short_generation_per_second: Option<Spread>,
    pub long_first_ttft_ms: Option<Spread>,
    pub long_next_ttft_ms: Option<Spread>,
    pub long_next_cached_share: Option<Spread>,
    pub stop_ms: Option<Spread>,
    /// First token of the request right after a Stop: shows whether the
    /// server really let go of the stopped one.
    pub after_stop_ttft_ms: Option<Spread>,
}

pub async fn run_chat(cfg: &ServerConfig, reps: usize, seed: u64) -> Result<ChatResult, String> {
    let started = common::start_server(cfg).await?;
    let client = &started.client;
    let mut rng = Rng::new(seed);
    let mut r = ChatResult::default();
    let _ = send(
        client,
        &request(vec![ChatMessage::new(Role::User, "Hi.")], 8, seed),
    )
    .await;
    let system = ChatMessage::new(Role::System, "You are a helpful assistant.");
    for rep in 0..reps {
        let s = seed + rep as u64;
        // Short.
        r.short.push(
            send(
                client,
                &request(
                    vec![
                        system.clone(),
                        ChatMessage::new(
                            Role::User,
                            format!(
                                "({:x}) Explain in a few sentences why the sky is blue.",
                                rng.next()
                            ),
                        ),
                    ],
                    256,
                    s,
                ),
            )
            .await?,
        );
        // Long: about 20K tokens of earlier conversation.
        let mut history = vec![system.clone()];
        for i in 0..20 {
            history.push(ChatMessage::new(Role::User, filler(500, &mut rng)));
            history.push(ChatMessage::new(
                Role::Assistant,
                format!("Noted, item {i}. {}", filler(450, &mut rng)),
            ));
        }
        let mut first = history.clone();
        first.push(ChatMessage::new(
            Role::User,
            "Summarize our conversation in three sentences.",
        ));
        let req = request(first.clone(), 128, s);
        let t = Instant::now();
        let resp = client
            .chat(&req, |_| ControlFlow::Continue(()))
            .await
            .map_err(|e| e.to_string())?;
        r.long_first.push(sample(&resp, t.elapsed()));
        let mut next = first;
        next.push(ChatMessage::new(Role::Assistant, resp.content.clone()));
        next.push(ChatMessage::new(
            Role::User,
            "Now list three themes from it.",
        ));
        r.long_next
            .push(send(client, &request(next, 128, s)).await?);
        // Stop: cancel a long generation after its first tokens.
        let req = request(
            vec![ChatMessage::new(
                Role::User,
                format!("{}\n\n{STORY}", filler(200, &mut rng)),
            )],
            4096,
            s,
        );
        let mut tokens = 0usize;
        let mut stop_at: Option<Instant> = None;
        let _ = client
            .chat(&req, |e| {
                if matches!(e, StreamEvent::Content(_) | StreamEvent::Reasoning(_)) {
                    tokens += 1;
                }
                if tokens >= 20 {
                    stop_at.get_or_insert_with(Instant::now);
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .await;
        if let Some(at) = stop_at {
            r.stop_ms.push(common::ms(at.elapsed()));
        }
        let after = send(
            client,
            &request(vec![ChatMessage::new(Role::User, "Say OK.")], 8, s),
        )
        .await?;
        r.after_stop_ttft_ms.push(after.ttft_ms);
        eprintln!(
            "[chat] rep {rep}: short first token {:.0} ms, long {:.0} ms then {:.0} ms ({} of {} cached), stop {:.1} ms, next first token {:.0} ms",
            r.short.last().map_or(0.0, |s| s.ttft_ms),
            r.long_first.last().map_or(0.0, |s| s.ttft_ms),
            r.long_next.last().map_or(0.0, |s| s.ttft_ms),
            r.long_next.last().map_or(0, |s| s.cached_tokens),
            r.long_next
                .last()
                .map_or(0, |s| s.cached_tokens + s.prompt_tokens),
            r.stop_ms.last().copied().unwrap_or(0.0),
            r.after_stop_ttft_ms.last().copied().unwrap_or(0.0)
        );
    }
    let f =
        |v: &[Sample], g: fn(&Sample) -> f64| common::spread(&v.iter().map(g).collect::<Vec<_>>());
    r.summary = ChatSummary {
        short_ttft_ms: f(&r.short, |s| s.ttft_ms),
        short_generation_per_second: f(&r.short, |s| s.generation_per_second),
        long_first_ttft_ms: f(&r.long_first, |s| s.ttft_ms),
        long_next_ttft_ms: f(&r.long_next, |s| s.ttft_ms),
        long_next_cached_share: f(&r.long_next, |s| {
            s.cached_tokens as f64 / (s.cached_tokens + s.prompt_tokens).max(1) as f64
        }),
        stop_ms: common::spread(&r.stop_ms),
        after_stop_ttft_ms: common::spread(&r.after_stop_ttft_ms),
    };
    r.health = common::health();
    started.server.shutdown().await;
    Ok(r)
}

// ---------- S5 (vision) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionResult {
    pub images: usize,
    pub size_px: u32,
    pub samples: Vec<Sample>,
    pub ttft_ms: Option<Spread>,
}

pub async fn run_vision(
    cfg: &ServerConfig,
    reps: usize,
    seed: u64,
) -> Result<Vec<VisionResult>, String> {
    if cfg.mmproj.is_none() {
        return Err("the vision suite needs mainMmproj in machine.json".into());
    }
    let started = common::start_server(cfg).await?;
    let client = &started.client;
    let mut out = Vec::new();
    for size in [1024u32, 2048] {
        for images in [1usize, 2, 4] {
            let mut samples = Vec::new();
            for rep in 0..reps {
                let mut msg = ChatMessage::new(
                    Role::User,
                    "Describe the shapes and colours in these images in two sentences.",
                );
                msg.images = (0..images)
                    .map(|i| {
                        let png = fixtures::png(size, size * 3 / 4, seed + (rep * 10 + i) as u64);
                        format!("data:image/png;base64,{}", fixtures::base64(&png))
                    })
                    .collect();
                match send(client, &request(vec![msg], 64, seed)).await {
                    Ok(s) => samples.push(s),
                    Err(e) => return Err(format!("vision ({images} × {size} px): {e}")),
                }
            }
            let ttft = common::spread(&samples.iter().map(|s| s.ttft_ms).collect::<Vec<_>>());
            eprintln!(
                "[vision] {images} × {size} px: first token {:.0} ms",
                ttft.map_or(0.0, |t| t.median)
            );
            out.push(VisionResult {
                images,
                size_px: size,
                samples,
                ttft_ms: ttft,
            });
        }
    }
    started.server.shutdown().await;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_has_about_the_asked_size_and_differs() {
        let mut rng = Rng::new(1);
        let a = filler(1000, &mut rng);
        let b = filler(1000, &mut rng);
        assert!(a.len() >= 4000 && a.len() < 4200, "{}", a.len());
        assert_ne!(a[..30], b[..30]);
    }

    #[test]
    fn engine_configs_are_known() {
        let p = std::path::Path::new("x");
        assert_eq!(engine_config("kv-q8", p, p, None).unwrap().kv, Kv::Q8);
        assert_eq!(engine_config("slots-4", p, p, None).unwrap().parallel, 4);
        assert!(
            engine_config("ctx-auto", p, p, None)
                .unwrap()
                .context
                .is_none()
        );
        assert!(engine_config("bogus", p, p, None).is_err());
        assert!(engine_config("mtp+bogus", p, p, None).is_err());
        let both = engine_config("mtp+slots-2", p, p, None).unwrap();
        assert_eq!((both.speculative, both.parallel), (Spec::Mtp, 2));
        assert!(engine_config("mmproj", p, p, None).is_err());
        let vision = engine_config("mtp+mmproj", p, p, Some(p)).unwrap();
        assert_eq!(vision.mmproj.as_deref(), Some(p));
    }
}
