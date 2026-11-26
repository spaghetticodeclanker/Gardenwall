//! Compare a result file with a baseline (plan §6): every measure gets a
//! direction and a threshold; runs flagged as degraded are left out of the
//! agent comparison. Writes a Markdown report and reports regressions.

use std::path::Path;

use serde_json::Value;

/// How a measure is judged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// Higher is better; a regression is a drop of more than this share.
    Higher(f64),
    /// Lower is better; a regression is a rise of more than this share.
    Lower(f64),
    /// Lower is better: a rise of more than this share *and* more than
    /// this many units (tiny timings jitter by large shares).
    LowerMin(f64, f64),
    /// A count of correct answers; a regression is a drop of this many.
    Drop(f64),
    /// A pass rate: Fisher's exact test, p < 0.1, on passed/runs.
    PassRate,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub base: f64,
    pub new: f64,
    /// For pass rates: (passed, runs) of base and new.
    pub counts: Option<((u64, u64), (u64, u64))>,
    pub rule: Rule,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Better,
    Same,
    Worse,
}

impl Row {
    pub fn verdict(&self) -> Verdict {
        let change = if self.base.abs() > f64::EPSILON {
            (self.new - self.base) / self.base.abs()
        } else {
            0.0
        };
        match self.rule {
            Rule::Higher(t) if change < -t => Verdict::Worse,
            Rule::Higher(t) if change > t => Verdict::Better,
            Rule::Lower(t) if change > t => Verdict::Worse,
            Rule::Lower(t) if change < -t => Verdict::Better,
            Rule::LowerMin(t, min) if change > t && self.new - self.base > min => Verdict::Worse,
            Rule::LowerMin(t, min) if change < -t && self.base - self.new > min => Verdict::Better,
            Rule::Drop(n) if self.base - self.new >= n => Verdict::Worse,
            Rule::Drop(n) if self.new - self.base >= n => Verdict::Better,
            Rule::PassRate => match self.counts {
                Some(((bp, bn), (np, nn))) if fisher_lower(np, nn, bp, bn) < 0.1 => Verdict::Worse,
                Some(((bp, bn), (np, nn))) if fisher_lower(bp, bn, np, nn) < 0.1 => Verdict::Better,
                _ => Verdict::Same,
            },
            _ => Verdict::Same,
        }
    }
}

fn ln_fact(n: u64) -> f64 {
    (1..=n).map(|k| (k as f64).ln()).sum()
}

/// One-sided Fisher exact test: the probability that group A (a of n)
/// passes this rarely or more rarely than group B (b of m), if both had
/// the same pass rate.
pub fn fisher_lower(a: u64, n: u64, b: u64, m: u64) -> f64 {
    let total = n + m;
    let passes = a + b;
    let ln_c = |n: u64, k: u64| ln_fact(n) - ln_fact(k) - ln_fact(n - k);
    let denom = ln_c(total, passes);
    let lo = passes.saturating_sub(m);
    (lo..=a.min(passes))
        .filter(|&x| x <= n && passes - x <= m)
        .map(|x| (ln_c(n, x) + ln_c(m, passes - x) - denom).exp())
        .sum::<f64>()
        .min(1.0)
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_f64()
}

/// A measure: name, value, (passed, runs) for pass rates, and its rule.
type MeasureRow = (String, f64, Option<(u64, u64)>, Rule);

/// The comparable measures of one result file, by name.
pub fn measures(v: &Value) -> Vec<MeasureRow> {
    let suite = v.get("suite").and_then(Value::as_str).unwrap_or_default();
    let mut out = Vec::new();
    let mut push = |name: String, val: Option<f64>, rule: Rule| {
        if let Some(x) = val.filter(|x| x.is_finite()) {
            out.push((name, x, None, rule));
        }
    };
    match suite {
        "agent" => {
            // Recomputed from runs, leaving out degraded ones.
            let runs = v
                .get("runs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let ok: Vec<&Value> = runs.iter().filter(|r| !degraded(r.get("health"))).collect();
            let mut keys: Vec<(String, String, u64)> = ok
                .iter()
                .map(|r| {
                    (
                        r["profile"].as_str().unwrap_or("default").to_string(),
                        r["arm"].as_str().unwrap_or("gardenwall").to_string(),
                        r["tier"].as_u64().unwrap_or(1),
                    )
                })
                .collect();
            keys.sort();
            keys.dedup();
            for (profile, arm, tier) in keys {
                let mine: Vec<&&Value> = ok
                    .iter()
                    .filter(|r| {
                        r["profile"].as_str().unwrap_or("default") == profile
                            && r["arm"].as_str().unwrap_or("gardenwall") == arm
                            && r["tier"].as_u64().unwrap_or(1) == tier
                    })
                    .collect();
                let passed = mine
                    .iter()
                    .filter(|r| r["passed"].as_bool() == Some(true))
                    .count() as u64;
                let prefix = format!("agent.{profile}.{arm}.tier{tier}");
                out.push((
                    format!("{prefix}.pass-rate"),
                    passed as f64 / mine.len().max(1) as f64,
                    Some((passed, mine.len() as u64)),
                    Rule::PassRate,
                ));
                let walls: Vec<f64> = mine.iter().filter_map(|r| r["wallMs"].as_f64()).collect();
                let ttfts: Vec<f64> = mine.iter().filter_map(|r| r["ttftMs"].as_f64()).collect();
                if let Some(s) = crate::common::spread(&walls) {
                    out.push((
                        format!("{prefix}.wall-ms"),
                        s.median,
                        None,
                        Rule::Lower(0.10),
                    ));
                }
                if let Some(s) = crate::common::spread(&ttfts) {
                    out.push((
                        format!("{prefix}.ttft-ms"),
                        s.median,
                        None,
                        Rule::Lower(0.10),
                    ));
                }
            }
        }
        "engine" => {
            for r in v
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let c = r["config"].as_str().unwrap_or("?");
                let m = r["model"].as_str().unwrap_or("?");
                let m = &m[..m.len().min(12)];
                let p = format!("engine.{m}.{c}");
                push(
                    format!("{p}.load-ms"),
                    r["loadMs"].as_f64(),
                    Rule::Lower(0.15),
                );
                push(
                    format!("{p}.vram-peak-gib"),
                    r["vramGibPeak"].as_f64(),
                    Rule::Lower(0.03),
                );
                for pr in r["prompts"].as_array().into_iter().flatten() {
                    let size = pr["size"].as_u64().unwrap_or(0);
                    push(
                        format!("{p}.p{size}.generation-tps"),
                        num(pr, &["generationPerSecond", "median"]),
                        Rule::Higher(0.05),
                    );
                    push(
                        format!("{p}.p{size}.prompt-tps"),
                        num(pr, &["promptPerSecond", "median"]),
                        Rule::Higher(0.10),
                    );
                    push(
                        format!("{p}.p{size}.ttft-ms"),
                        num(pr, &["ttftMs", "median"]),
                        Rule::Lower(0.10),
                    );
                }
                if let Some(c) = r.get("concurrent").filter(|c| !c.is_null()) {
                    push(
                        format!("{p}.concurrent-total-tps"),
                        num(c, &["totalGenerationPerSecond", "median"]),
                        Rule::Higher(0.05),
                    );
                }
            }
        }
        "chat" => {
            let s = &v["result"]["summary"];
            push(
                "chat.short.ttft-ms".into(),
                num(s, &["shortTtftMs", "median"]),
                Rule::Lower(0.10),
            );
            push(
                "chat.short.generation-tps".into(),
                num(s, &["shortGenerationPerSecond", "median"]),
                Rule::Higher(0.05),
            );
            push(
                "chat.long.first-ttft-ms".into(),
                num(s, &["longFirstTtftMs", "median"]),
                Rule::Lower(0.10),
            );
            push(
                "chat.long.next-ttft-ms".into(),
                num(s, &["longNextTtftMs", "median"]),
                Rule::Lower(0.10),
            );
            push(
                "chat.stop-ms".into(),
                num(s, &["stopMs", "median"]),
                Rule::LowerMin(0.50, 5.0),
            );
            push(
                "chat.after-stop-ttft-ms".into(),
                num(s, &["afterStopTtftMs", "median"]),
                Rule::LowerMin(0.20, 20.0),
            );
        }
        "micro" => {
            for m in v
                .get("measures")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = m["name"].as_str().unwrap_or("?");
                // Disk-bound measures (database syncs, checkpoint copies)
                // vary much more between identical runs.
                let rule = if name.starts_with("store.") || name.starts_with("checkpoint.") {
                    Rule::LowerMin(0.50, 1.0)
                } else {
                    Rule::LowerMin(0.20, 0.5)
                };
                push(format!("micro.{name}"), num(m, &["spread", "median"]), rule);
            }
        }
        "rag" => {
            let r = &v["result"];
            push(
                "rag.files-per-second".into(),
                r["filesPerSecond"].as_f64(),
                Rule::Higher(0.10),
            );
            push(
                "rag.bm25-query-ms".into(),
                num(r, &["bm25QueryMs", "median"]),
                Rule::Lower(0.20),
            );
            push(
                "rag.reindex-one-ms".into(),
                num(r, &["reindexOneMs", "median"]),
                Rule::Lower(0.20),
            );
            push(
                "rag.bm25-hit-at-5".into(),
                r["bm25HitAt5"].as_f64(),
                Rule::Drop(2.0),
            );
            if let Some(h) = r.get("hybrid").filter(|h| !h.is_null()) {
                push(
                    "rag.hybrid-hit-at-5".into(),
                    h["hitAt5"].as_f64(),
                    Rule::Drop(2.0),
                );
                push(
                    "rag.hybrid-query-ms".into(),
                    num(h, &["queryMs", "median"]),
                    Rule::Lower(0.20),
                );
            }
        }
        "vision" => {
            for r in v
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                push(
                    format!(
                        "vision.{}x{}px.ttft-ms",
                        r["images"].as_u64().unwrap_or(0),
                        r["sizePx"].as_u64().unwrap_or(0)
                    ),
                    num(r, &["ttftMs", "median"]),
                    Rule::Lower(0.10),
                );
            }
        }
        "sessions" => {
            let runs = v
                .get("runs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut keys: Vec<(String, u64, u64)> = runs
                .iter()
                .map(|r| {
                    (
                        r["mode"].as_str().unwrap_or("?").to_string(),
                        r["sessions"].as_u64().unwrap_or(0),
                        r["slots"].as_u64().unwrap_or(1),
                    )
                })
                .collect();
            keys.sort();
            keys.dedup();
            for (mode, n, slots) in keys {
                let totals: Vec<f64> = runs
                    .iter()
                    .filter(|r| {
                        r["mode"].as_str() == Some(&mode)
                            && r["sessions"].as_u64() == Some(n)
                            && r["slots"].as_u64() == Some(slots)
                    })
                    .filter_map(|r| r["totalMs"].as_f64())
                    .collect();
                if let Some(s) = crate::common::spread(&totals) {
                    out.push((
                        format!("sessions.{mode}.{n}x{slots}.total-ms"),
                        s.median,
                        None,
                        Rule::Lower(0.10),
                    ));
                }
            }
        }
        _ => {}
    }
    out
}

fn degraded(h: Option<&Value>) -> bool {
    h.and_then(|h| serde_json::from_value::<crate::common::Health>(h.clone()).ok())
        .is_some_and(|h| h.degraded())
}

/// Tier-1 tasks that passed in some baseline run and now fail every run.
fn newly_failing(base: &Value, new: &Value) -> Vec<String> {
    let tally = |v: &Value| {
        let mut m: std::collections::BTreeMap<(String, String), (u32, u32)> = Default::default();
        for r in v
            .get("runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if r["tier"].as_u64().unwrap_or(1) != 1 || r["arm"].as_str() == Some("pi") {
                continue;
            }
            let e = m
                .entry((
                    r["task"].as_str().unwrap_or("?").to_string(),
                    r["profile"].as_str().unwrap_or("default").to_string(),
                ))
                .or_default();
            e.1 += 1;
            if r["passed"].as_bool() == Some(true) {
                e.0 += 1;
            }
        }
        m
    };
    let (b, n) = (tally(base), tally(new));
    n.iter()
        .filter(|(k, (p, runs))| *p == 0 && *runs > 0 && b.get(*k).is_some_and(|(bp, _)| *bp > 0))
        .map(|((t, p), _)| format!("{t} ({p})"))
        .collect()
}

pub fn compare(base: &Value, new: &Value) -> Result<(Vec<Row>, Vec<String>), String> {
    let (bs, ns) = (base["suite"].as_str(), new["suite"].as_str());
    if bs != ns || bs.is_none() {
        return Err(format!("different suites: {bs:?} and {ns:?}"));
    }
    let b = measures(base);
    let mut rows = Vec::new();
    for (name, val, counts, rule) in measures(new) {
        if let Some((_, bv, bc, _)) = b.iter().find(|(n, ..)| *n == name) {
            rows.push(Row {
                name,
                base: *bv,
                new: val,
                counts: bc.zip(counts),
                rule,
            });
        }
    }
    let mut notes = Vec::new();
    if bs == Some("agent") {
        for t in newly_failing(base, new) {
            notes.push(format!(
                "REGRESSION: tier-1 task {t} now fails every repetition"
            ));
        }
        if base["suiteVersion"] != new["suiteVersion"] {
            notes.push(
                "Note: the task suites differ in version; only tasks in both are comparable."
                    .into(),
            );
        }
    }
    let fp = |v: &Value, k: &str| v["fingerprint"][k].clone();
    for k in ["gpu", "gpuDriver", "cpu", "os", "pwsh", "pathRulesVersion"] {
        if fp(base, k) != fp(new, k) {
            notes.push(format!(
                "Note: {k} differs: {} → {}",
                fp(base, k),
                fp(new, k)
            ));
        }
    }
    Ok((rows, notes))
}

fn fmt(x: f64) -> String {
    if x.abs() >= 100.0 {
        format!("{x:.0}")
    } else if x.abs() >= 1.0 {
        format!("{x:.2}")
    } else {
        format!("{x:.3}")
    }
}

pub fn markdown(base_name: &str, new_name: &str, rows: &[Row], notes: &[String]) -> String {
    let worse = rows
        .iter()
        .filter(|r| r.verdict() == Verdict::Worse)
        .count()
        + notes.iter().filter(|n| n.starts_with("REGRESSION")).count();
    let better = rows
        .iter()
        .filter(|r| r.verdict() == Verdict::Better)
        .count();
    let mut s = format!(
        "# Benchmark comparison\n\n- Baseline: `{base_name}`\n- New: `{new_name}`\n- **{worse} regression(s)**, {better} improvement(s), {} unchanged within thresholds.\n\n",
        rows.len()
            - better
            - rows
                .iter()
                .filter(|r| r.verdict() == Verdict::Worse)
                .count()
    );
    for n in notes {
        s.push_str(&format!("- {n}\n"));
    }
    if !notes.is_empty() {
        s.push('\n');
    }
    s.push_str(
        "| Measure | Baseline | New | Change | Threshold | Verdict |\n|---|---|---|---|---|---|\n",
    );
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by_key(|r| match r.verdict() {
        Verdict::Worse => 0,
        Verdict::Better => 1,
        Verdict::Same => 2,
    });
    for r in sorted {
        let change = if r.base.abs() > f64::EPSILON {
            format!("{:+.1} %", (r.new - r.base) / r.base.abs() * 100.0)
        } else {
            "–".into()
        };
        let threshold = match r.rule {
            Rule::Higher(t) => format!("−{:.0} %", t * 100.0),
            Rule::Lower(t) => format!("+{:.0} %", t * 100.0),
            Rule::LowerMin(t, min) => format!("+{:.0} % and +{min}", t * 100.0),
            Rule::Drop(n) => format!("−{n:.0}"),
            Rule::PassRate => "Fisher p < 0.1".into(),
        };
        let (b, n) = match r.counts {
            Some(((bp, bn), (np, nn))) => (format!("{bp}/{bn}"), format!("{np}/{nn}")),
            None => (fmt(r.base), fmt(r.new)),
        };
        let verdict = match r.verdict() {
            Verdict::Worse => "**worse**",
            Verdict::Better => "better",
            Verdict::Same => "same",
        };
        s.push_str(&format!(
            "| {} | {b} | {n} | {change} | {threshold} | {verdict} |\n",
            r.name
        ));
    }
    s
}

/// Compare two files; write `<new>.compare.md`; Ok(true) when anything
/// regressed.
pub fn run_compare(base: &Path, new: &Path) -> Result<bool, String> {
    let read = |p: &Path| -> Result<Value, String> {
        serde_json::from_str(
            &std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        )
        .map_err(|e| format!("{}: {e}", p.display()))
    };
    let (b, n) = (read(base)?, read(new)?);
    let (rows, notes) = compare(&b, &n)?;
    let name = |p: &Path| {
        p.file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let md = markdown(&name(base), &name(new), &rows, &notes);
    let out = new.with_extension("compare.md");
    std::fs::write(&out, &md).map_err(|e| e.to_string())?;
    println!("{md}");
    eprintln!("Report: {}", out.display());
    Ok(rows.iter().any(|r| r.verdict() == Verdict::Worse)
        || notes.iter().any(|n| n.starts_with("REGRESSION")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fisher_matches_known_values() {
        // 0/10 against 10/10 is very unlikely by chance.
        assert!(fisher_lower(0, 10, 10, 10) < 0.001);
        // Equal results are not a regression.
        assert!(fisher_lower(9, 10, 9, 10) > 0.5);
        // 34/36 against 36/36: not significant at 0.1.
        assert!(fisher_lower(34, 36, 36, 36) > 0.1);
    }

    #[test]
    fn thresholds_decide_verdicts() {
        let row = |base, new, rule| Row {
            name: "x".into(),
            base,
            new,
            counts: None,
            rule,
        };
        assert_eq!(
            row(100.0, 111.0, Rule::Lower(0.10)).verdict(),
            Verdict::Worse
        );
        assert_eq!(
            row(100.0, 109.0, Rule::Lower(0.10)).verdict(),
            Verdict::Same
        );
        assert_eq!(
            row(60.0, 56.0, Rule::Higher(0.05)).verdict(),
            Verdict::Worse
        );
        assert_eq!(row(40.0, 38.0, Rule::Drop(2.0)).verdict(), Verdict::Worse);
        assert_eq!(row(40.0, 39.0, Rule::Drop(2.0)).verdict(), Verdict::Same);
        // 0.07 → 0.11 ms is +60 % but only 0.04 ms: not a regression.
        assert_eq!(
            row(0.07, 0.11, Rule::LowerMin(0.2, 0.5)).verdict(),
            Verdict::Same
        );
        assert_eq!(
            row(10.0, 13.0, Rule::LowerMin(0.2, 0.5)).verdict(),
            Verdict::Worse
        );
    }

    #[test]
    fn agent_results_compare_and_flag_new_failures() {
        let run = |task: &str, passed: bool, wall: u64| json!({"task": task, "tier": 1, "profile": "default", "arm": "gardenwall", "passed": passed, "wallMs": wall, "ttftMs": 300});
        let base = json!({"suite": "agent", "suiteVersion": 2, "fingerprint": {"gpu": "G"},
            "runs": [run("a", true, 1000), run("a", true, 1000), run("b", true, 1000)]});
        let new = json!({"suite": "agent", "suiteVersion": 2, "fingerprint": {"gpu": "G"},
            "runs": [run("a", false, 1500), run("a", false, 1500), run("b", true, 1500)]});
        let (rows, notes) = compare(&base, &new).unwrap();
        let wall = rows.iter().find(|r| r.name.ends_with("wall-ms")).unwrap();
        assert_eq!(wall.verdict(), Verdict::Worse);
        assert!(notes.iter().any(|n| n.contains("task a")), "{notes:?}");
        let md = markdown("b.json", "n.json", &rows, &notes);
        assert!(md.contains("| agent.default.gardenwall.tier1.wall-ms |"));
        assert!(compare(&base, &json!({"suite": "micro"})).is_err());
    }
}
