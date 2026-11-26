//! Gardenwall's benchmark harness (performance plan,
//! `docs/internal/PERFORMANCE-BASELINE-PLAN.md`; agent parity, ADR-014 §2.3).
//!
//! ```text
//! gardenwall-bench run --tier quick|standard|full      the plan's tiers
//! gardenwall-bench agent    [--model main] [--profiles default,pi] [--tiers 1,2] [--reps 3]
//! gardenwall-bench sessions [--counts 2,3] [--reps 2]
//! gardenwall-bench engine   [--models main] [--configs base,kv-q8] [--sizes 512,4096] [--reps 3]
//! gardenwall-bench chat | vision | rag | micro   [--reps N]
//! gardenwall-bench compare <baseline.json> <new.json>
//! gardenwall-bench compare <agent.json> --profiles default,code-mode
//! ```
//!
//! Paths come from `bench/machine.json` (`--machine` to use another file);
//! results go to `bench/results/<date>/` (`--out`). Model servers run on
//! loopback with a random key; `--force` runs even if other programs hold
//! GPU memory. A development tool, never part of the app.

mod ab;
mod agent;
mod common;
mod compare;
mod engine;
mod fixtures;
mod micro;
mod rag;
mod tasks;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use common::shuffle;
use common::{Fingerprint, Header, Machine, ServerConfig};
use serde::Serialize;

/// Command-line flags: `--name value` pairs and bare `--switch`es.
struct Flags {
    values: HashMap<String, String>,
    switches: Vec<String>,
    positional: Vec<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        const SWITCHES: &[&str] = &["--force", "--hybrid", "--resume"];
        let mut f = Flags {
            values: HashMap::new(),
            switches: vec![],
            positional: vec![],
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if SWITCHES.contains(&a.as_str()) {
                f.switches.push(a.clone());
            } else if let Some(name) = a.strip_prefix("--") {
                let v = it.next().ok_or(format!("--{name} needs a value"))?;
                f.values.insert(name.to_string(), v.clone());
            } else {
                f.positional.push(a.clone());
            }
        }
        Ok(f)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn list(&self, name: &str, default: &str) -> Vec<String> {
        self.get(name)
            .unwrap_or(default)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("--{name} takes a number")),
        }
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn check(&self, known: &[&str]) -> Result<(), String> {
        for k in self.values.keys() {
            if !known.contains(&k.as_str()) {
                return Err(format!("unknown option --{k}"));
            }
        }
        Ok(())
    }
}

/// Everything a suite run needs.
struct Ctx {
    machine: Machine,
    out: PathBuf,
    fingerprint: Fingerprint,
    seed: u64,
    force: bool,
}

impl Ctx {
    fn new(f: &Flags) -> Result<Self, String> {
        let machine_path = f.get("machine").unwrap_or("bench/machine.json");
        let machine = match (f.get("server"), f.get("model")) {
            // The parity form: --server and --model as paths.
            (Some(server), Some(model)) if Path::new(model).is_file() => Machine {
                server: server.into(),
                main_model: model.into(),
                small_model: None,
                moe_model: None,
                main_mmproj: None,
                embed_model: None,
                pi: f.get("pi").map(str::to_string),
            },
            _ => {
                let mut m = Machine::load(Path::new(machine_path))?;
                if let Some(s) = f.get("server") {
                    m.server = s.into();
                }
                if let Some(p) = f.get("pi") {
                    m.pi = Some(p.into());
                }
                m
            }
        };
        if !machine.server.is_file() {
            return Err(format!(
                "the server {} isn't a file",
                machine.server.display()
            ));
        }
        Ok(Self {
            machine,
            out: f.get("out").unwrap_or("bench/results").into(),
            fingerprint: common::fingerprint(),
            seed: f.num("seed", common::now_unix())?,
            force: f.has("--force"),
        })
    }

    fn model(&self, which: &str) -> Result<PathBuf, String> {
        let p = self.machine.model(which)?;
        if !p.is_file() {
            return Err(format!("model {which} ({}) isn't a file", p.display()));
        }
        Ok(p)
    }

    fn gpu_check(&self) -> Result<(), String> {
        if let Some(w) = common::gpu_is_free(self.force)? {
            eprintln!("warning: {w}");
        }
        Ok(())
    }

    fn save(&self, suite: &str, tag: &str, body: &impl Serialize) -> Result<PathBuf, String> {
        let path = common::write_result(
            &self.out,
            &Header::new(suite, self.seed, &self.fingerprint),
            tag,
            body,
        )?;
        eprintln!("Results: {}", path.display());
        Ok(path)
    }
}

#[derive(Serialize)]
struct Wrapped<T: Serialize> {
    result: T,
}

#[derive(Serialize)]
struct Results<T: Serialize> {
    results: T,
}

// ---------- suites ----------

async fn agent_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    ctx.gpu_check()?;
    let profiles = f
        .list("profiles", "default")
        .iter()
        .map(|p| agent::Profile::named(p))
        .collect::<Result<Vec<_>, _>>()?;
    let tiers: Vec<u8> = f
        .list("tiers", "1,2,3")
        .iter()
        .map(|t| t.parse().map_err(|_| "--tiers takes numbers".to_string()))
        .collect::<Result<_, _>>()?;
    let o = agent::AgentOptions {
        server: ctx.machine.server.clone(),
        model: ctx.model(f.get("model").unwrap_or("main"))?,
        tasks: f.get("tasks").unwrap_or("bench/tasks.json").into(),
        profiles,
        tiers,
        only: f
            .get("only")
            .map(|o| o.split(',').map(str::to_string).collect()),
        reps: f.num("reps", 3)?,
        seed: ctx.seed,
        pi: ctx.machine.pi.clone(),
        partial: f.get("partial").map(PathBuf::from),
    };
    let report = agent::run_suite(&o).await?;
    ctx.save("agent", &f.list("profiles", "default").join("-"), &report)
}

async fn sessions_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    ctx.gpu_check()?;
    let counts: Vec<usize> = f
        .list("counts", "2,3")
        .iter()
        .map(|c| c.parse().map_err(|_| "--counts takes numbers".to_string()))
        .collect::<Result<_, _>>()?;
    let runs = agent::run_sessions(&agent::SessionsOptions {
        server: ctx.machine.server.clone(),
        model: ctx.model(f.get("model").unwrap_or("main"))?,
        tasks: f.get("tasks").unwrap_or("bench/tasks.json").into(),
        counts,
        reps: f.num("reps", 2)?,
    })
    .await?;
    #[derive(Serialize)]
    struct S {
        runs: Vec<agent::SessionsRun>,
    }
    ctx.save("sessions", "", &S { runs })
}

async fn engine_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    ctx.gpu_check()?;
    let mut configs = Vec::new();
    for m in f.list("models", "main") {
        let model = ctx.model(&m)?;
        let default = if m == "moe" {
            "moe-nocache,moe-cache4g"
        } else {
            "base,kv-q8,ngram,mtp,sandboxed"
        };
        for c in f.list("configs", default) {
            configs.push((
                c.clone(),
                engine::engine_config(
                    &c,
                    &ctx.machine.server,
                    &model,
                    ctx.machine.main_mmproj.as_deref(),
                )?,
            ));
        }
    }
    let sizes: Vec<usize> = f
        .list("sizes", "512,4096,16384,30000")
        .iter()
        .map(|s| s.parse().map_err(|_| "--sizes takes numbers".to_string()))
        .collect::<Result<_, _>>()?;
    let results = engine::run_engine(&engine::EngineOptions {
        configs,
        sizes,
        generate: f.num("gen", 512)?,
        reps: f.num("reps", 3)?,
        seed: ctx.seed,
    })
    .await?;
    ctx.save(
        "engine",
        &f.list("models", "main").join("-"),
        &Results { results },
    )
}

fn base_config(ctx: &Ctx, f: &Flags) -> Result<ServerConfig, String> {
    let mut c = ServerConfig::new(
        &ctx.machine.server,
        &ctx.model(f.get("model").unwrap_or("main"))?,
    );
    c.context = Some(32_768);
    Ok(c)
}

async fn chat_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    ctx.gpu_check()?;
    let result = engine::run_chat(&base_config(ctx, f)?, f.num("reps", 3)?, ctx.seed).await?;
    ctx.save("chat", "", &Wrapped { result })
}

async fn vision_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    ctx.gpu_check()?;
    let mut cfg = base_config(ctx, f)?;
    cfg.mmproj = ctx.machine.main_mmproj.clone();
    // `--config mtp` (or any engine config parts joined by +) measures
    // vision under that server setting.
    let config = f.get("config").unwrap_or("base");
    for part in config.split('+') {
        engine::apply_part(&mut cfg, part, ctx.machine.main_mmproj.as_deref())?;
    }
    let results = engine::run_vision(&cfg, f.num("reps", 3)?, ctx.seed).await?;
    let label = if config == "base" { "" } else { config };
    ctx.save("vision", label, &Results { results })
}

async fn rag_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    let embed = match (&ctx.machine.embed_model, f.has("--hybrid")) {
        (Some(m), true) => {
            ctx.gpu_check()?;
            let mut c = ServerConfig::new(&ctx.machine.server, m);
            c.embeddings = true;
            Some(c)
        }
        (None, true) => return Err("--hybrid needs embedModel in machine.json".into()),
        _ => None,
    };
    let result = rag::run_rag(&rag::RagOptions {
        docs: f.num("docs", 300)?,
        questions: f.num("questions", 40)?,
        reps: f.num("reps", 5)?,
        seed: f.num("corpus-seed", 1118)?,
        embed,
    })
    .await?;
    ctx.save("rag", "", &Wrapped { result })
}

fn micro_cmd(ctx: &Ctx, f: &Flags) -> Result<PathBuf, String> {
    let o = micro::MicroOptions {
        reps: f.num("reps", 20)?,
        repo_files: f.num("repo-files", 10_000)?,
        big_files: f.num("big-files", 20)?,
    };
    // Synchronous, with its own runtime for the shell: off the main one.
    let measures = std::thread::spawn(move || micro::run_micro(&o))
        .join()
        .map_err(|_| "the micro-benchmarks panicked")??;
    #[derive(Serialize)]
    struct M {
        measures: Vec<micro::Measure>,
    }
    ctx.save("micro", "", &M { measures })
}

// ---------- the plan's tiers ----------

/// One step of a tier: a suite and its flags.
fn tier_steps(tier: &str, machine: &Machine) -> Result<Vec<(&'static str, Vec<String>)>, String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut steps: Vec<(&'static str, Vec<String>)> = Vec::new();
    match tier {
        "quick" => {
            steps.push((
                "micro",
                s(&["--reps", "10", "--repo-files", "2000", "--big-files", "2"]),
            ));
            if machine.small_model.is_some() {
                steps.push((
                    "engine",
                    s(&[
                        "--models",
                        "small",
                        "--configs",
                        "base",
                        "--sizes",
                        "512,4096",
                        "--reps",
                        "2",
                    ]),
                ));
            }
            steps.push(("agent", s(&["--tiers", "1", "--reps", "1"])));
        }
        "standard" => {
            steps.push(("micro", s(&[])));
            steps.push((
                "engine",
                s(&[
                    "--models",
                    "main",
                    "--configs",
                    "base,kv-q8,ngram,mtp,sandboxed",
                ]),
            ));
            steps.push(("chat", s(&[])));
            steps.push(("agent", s(&["--tiers", "1,2", "--reps", "3"])));
        }
        "full" => {
            steps.push(("micro", s(&[])));
            steps.push((
                "engine",
                s(&[
                    "--models",
                    "main",
                    "--configs",
                    "base,kv-q8,ctx-auto,ngram,mtp,slots-2,slots-4,sandboxed",
                    "--reps",
                    "5",
                ]),
            ));
            if machine.moe_model.is_some() {
                steps.push(("engine", s(&["--models", "moe", "--reps", "5"])));
            }
            if machine.small_model.is_some() {
                steps.push((
                    "engine",
                    s(&["--models", "small", "--configs", "base", "--reps", "5"]),
                ));
            }
            steps.push(("chat", s(&["--reps", "5"])));
            if machine.main_mmproj.is_some() {
                steps.push(("vision", s(&["--reps", "5"])));
            }
            steps.push((
                "rag",
                if machine.embed_model.is_some() {
                    s(&["--hybrid"])
                } else {
                    s(&[])
                },
            ));
            steps.push(("sessions", s(&["--counts", "2,3", "--reps", "3"])));
            let mut main = vec!["default"];
            if machine.pi.is_some() {
                main.push("pi");
            }
            steps.push((
                "agent",
                s(&[
                    "--tiers",
                    "1,2,3",
                    "--reps",
                    "5",
                    "--profiles",
                    &main.join(","),
                ]),
            ));
            steps.push((
                "agent",
                s(&[
                    "--tiers",
                    "1,2,3",
                    "--reps",
                    "3",
                    "--profiles",
                    "shell-ps51,shell-cmd,thinking-off,thinking-low,ctx-8k,ctx-16k,kv-q8,mtp",
                ]),
            ));
        }
        other => return Err(format!("unknown tier {other} (quick, standard, full)")),
    }
    Ok(steps)
}

/// A tier run's progress, saved after every step so a paused run can be
/// resumed (`<out>/run-<tier>.progress.json`).
#[derive(Debug, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    tier: String,
    seed: u64,
    /// Steps finished (or failed), by index.
    done: Vec<usize>,
    files: Vec<String>,
    failures: Vec<String>,
    minutes: f64,
}

fn progress_path(out: &Path, tier: &str) -> PathBuf {
    out.join(format!("run-{tier}.progress.json"))
}

fn load_progress(out: &Path, tier: &str) -> Option<Progress> {
    serde_json::from_str(&std::fs::read_to_string(progress_path(out, tier)).ok()?).ok()
}

async fn run_tier(ctx: &Ctx, f: &Flags) -> Result<(), String> {
    let tier = f
        .get("tier")
        .ok_or("run needs --tier quick, standard or full")?;
    let steps = tier_steps(tier, &ctx.machine)?;
    let resume = f.has("--resume");
    let mut progress = match (load_progress(&ctx.out, tier), resume) {
        (Some(p), true) => {
            eprintln!(
                "Resuming the {tier} tier: {} of {} steps done, seed {}.",
                p.done.len(),
                steps.len(),
                p.seed
            );
            p
        }
        (None, true) => return Err(format!("there's no paused {tier} run to resume")),
        (Some(_), false) => {
            return Err(format!(
                "a paused {tier} run exists ({}): resume it with --resume, or delete that file to start over",
                progress_path(&ctx.out, tier).display()
            ));
        }
        (None, false) => Progress {
            tier: tier.into(),
            seed: ctx.seed,
            ..Default::default()
        },
    };
    let stop = ctx.out.join("STOP");
    if stop.exists() {
        eprintln!("Removing the STOP file left by the pause.");
        // Left from an earlier pause: this start is the go-ahead.
        let _ = std::fs::remove_file(&stop);
    }
    ctx.gpu_check()?;
    std::fs::create_dir_all(&ctx.out).map_err(|e| e.to_string())?;
    let save = |p: &Progress| {
        if let Ok(text) = serde_json::to_string_pretty(p) {
            let _ = std::fs::write(progress_path(&ctx.out, tier), text);
        }
    };
    let started = std::time::Instant::now();
    let base_minutes = progress.minutes;
    for (n, (suite, args)) in steps.iter().enumerate() {
        if progress.done.contains(&n) {
            continue;
        }
        let paused = |p: &mut Progress| {
            p.minutes = base_minutes + started.elapsed().as_secs_f64() / 60.0;
            save(p);
            format!(
                "paused after {} of {} steps (in {suite}); everything finished so far is kept. Continue with: gardenwall-bench run --tier {tier} --resume",
                n,
                steps.len()
            )
        };
        if common::stop_requested() {
            return Err(paused(&mut progress));
        }
        eprintln!(
            "
=== {tier} tier, step {}/{}: {suite} {} ===",
            n + 1,
            steps.len(),
            args.join(" ")
        );
        let mut sub = Flags::parse(args)?;
        sub.switches.extend(f.switches.iter().cloned());
        let partial = ctx
            .out
            .join(format!("run-{tier}-step{}.partial.jsonl", n + 1));
        if *suite == "agent" {
            sub.values
                .insert("partial".into(), partial.to_string_lossy().into_owned());
        }
        let r = match *suite {
            "micro" => micro_cmd(ctx, &sub),
            "engine" => engine_cmd(ctx, &sub).await,
            "chat" => chat_cmd(ctx, &sub).await,
            "vision" => vision_cmd(ctx, &sub).await,
            "rag" => rag_cmd(ctx, &sub).await,
            "sessions" => sessions_cmd(ctx, &sub).await,
            _ => agent_cmd(ctx, &sub).await,
        };
        match r {
            Ok(p) => {
                progress.files.push(
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
                let _ = std::fs::remove_file(&partial);
            }
            Err(e) if e == common::PAUSED => return Err(paused(&mut progress)),
            Err(e) => {
                eprintln!("step {suite} failed: {e}");
                progress.failures.push(format!("{suite}: {e}"));
            }
        }
        progress.done.push(n);
        progress.minutes = base_minutes + started.elapsed().as_secs_f64() / 60.0;
        save(&progress);
    }
    ctx.save("run", tier, &progress)?;
    let _ = std::fs::remove_file(progress_path(&ctx.out, tier));
    if progress.failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} step(s) failed: {}",
            progress.failures.len(),
            progress.failures.join("; ")
        ))
    }
}

// ---------- entry ----------

const KNOWN: &[&str] = &[
    "machine",
    "out",
    "seed",
    "server",
    "model",
    "models",
    "pi",
    "tasks",
    "profiles",
    "tiers",
    "only",
    "reps",
    "counts",
    "configs",
    "config",
    "sizes",
    "gen",
    "docs",
    "questions",
    "corpus-seed",
    "repo-files",
    "big-files",
    "tier",
    "partial",
    "arms",
    "context",
];

async fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if let [cmd, journal, dir, rep] = args.as_slice()
        && cmd == "internal-leak-grants"
    {
        micro::leak_grants(Path::new(journal), Path::new(dir), rep)?;
    }
    let (cmd, rest) = match args.first().map(String::as_str) {
        // The parity form from before subcommands: flags only.
        Some(a) if a.starts_with("--") => ("agent", &args[..]),
        Some(a) => (a, &args[1..]),
        None => {
            return Err(
                "usage: gardenwall-bench run --tier quick|standard|full (see bench/README.md)"
                    .into(),
            );
        }
    };
    let mut f = Flags::parse(rest)?;
    f.check(KNOWN)?;
    if cmd == "compare" {
        // Two profiles measured in one agent run, e.g. an experiment and
        // its control.
        if let ([file], Some(profiles)) = (f.positional.as_slice(), f.get("profiles")) {
            let Some((base, new)) = profiles.split_once(',') else {
                return Err("usage: gardenwall-bench compare <agent.json> --profiles <control>,<experiment>".into());
            };
            return ab::run_ab(Path::new(file), base, new);
        }
        let [base, new] = f.positional.as_slice() else {
            return Err("usage: gardenwall-bench compare <baseline.json> <new.json>, or compare <agent.json> --profiles <control>,<experiment>".into());
        };
        return compare::run_compare(Path::new(base), Path::new(new));
    }
    // The parity form's --arms gardenwall,pi means the default and pi profiles.
    if let Some(arms) = f.values.remove("arms") {
        let profiles: Vec<&str> = arms
            .split(',')
            .map(|a| if a == "pi" { "pi" } else { "default" })
            .collect();
        f.values.insert("profiles".into(), profiles.join(","));
        f.values.entry("tiers".into()).or_insert("1".into());
    }
    if cmd == "run"
        && f.has("--resume")
        && let Some(tier) = f.get("tier")
    {
        let out = PathBuf::from(f.get("out").unwrap_or("bench/results"));
        if let Some(p) = load_progress(&out, tier) {
            // The same seed, so the remaining runs keep their planned order.
            f.values.insert("seed".into(), p.seed.to_string());
        }
    }
    let ctx = Ctx::new(&f)?;
    common::set_stop_file(ctx.out.join("STOP"));
    match cmd {
        "run" => run_tier(&ctx, &f).await?,
        "agent" => drop(agent_cmd(&ctx, &f).await?),
        "sessions" => drop(sessions_cmd(&ctx, &f).await?),
        "engine" => drop(engine_cmd(&ctx, &f).await?),
        "chat" => drop(chat_cmd(&ctx, &f).await?),
        "vision" => drop(vision_cmd(&ctx, &f).await?),
        "rag" => drop(rag_cmd(&ctx, &f).await?),
        "micro" => drop(micro_cmd(&ctx, &f)?),
        other => return Err(format!("unknown command {other}")),
    }
    Ok(false)
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(false) => std::process::ExitCode::SUCCESS,
        // compare found a regression.
        Ok(true) => std::process::ExitCode::from(2),
        Err(e) => {
            eprintln!("gardenwall-bench: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_values_switches_and_positionals() {
        let args: Vec<String> = ["--reps", "3", "--force", "a.json", "b.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let f = Flags::parse(&args).unwrap();
        assert_eq!(f.num::<usize>("reps", 1).unwrap(), 3);
        assert!(f.has("--force"));
        assert_eq!(f.positional, ["a.json", "b.json"]);
        assert!(f.check(&["reps"]).is_ok());
        assert!(Flags::parse(&["--reps".to_string()]).is_err());
    }

    #[test]
    fn tiers_cover_the_plan() {
        let m = Machine {
            server: "s".into(),
            main_model: "m".into(),
            small_model: Some("s".into()),
            moe_model: Some("x".into()),
            main_mmproj: Some("p".into()),
            embed_model: None,
            pi: Some("node w {prompt}".into()),
        };
        let full: Vec<&str> = tier_steps("full", &m)
            .unwrap()
            .iter()
            .map(|s| s.0)
            .collect();
        for suite in [
            "micro", "engine", "chat", "vision", "rag", "sessions", "agent",
        ] {
            assert!(full.contains(&suite), "full tier runs {suite}");
        }
        let quick = tier_steps("quick", &m).unwrap();
        assert_eq!(quick.len(), 3);
        assert!(tier_steps("nope", &m).is_err());
    }
}
