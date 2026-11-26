//! Compare two profiles measured in the same agent run, task by task
//! (`compare <file> --profiles default,code-mode`).
//!
//! `compare` proper matches rows by profile name across two files, so it
//! can't judge an experiment against its control. This does, with the gate
//! used for code mode (ADR-014 §2.1): the pass rate must hold, tasks that
//! take many steps must get clearly cheaper, and one-step tasks must not get
//! slower.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::common::{Health, spread};
use crate::compare::fisher_lower;

/// A task counts as multi-step from this many tool calls in the control.
const MULTI_STEP_CALLS: f64 = 6.0;
/// A task counts as a one-step control up to this many.
const ONE_STEP_CALLS: f64 = 2.0;
/// Multi-step tasks must use at least this much less.
const WANTED_SAVING: f64 = 0.20;
/// Nothing gated may get worse by more than this.
const ALLOWED_LOSS: f64 = 0.10;

#[derive(Default, Clone)]
struct Cell {
    runs: u64,
    passed: u64,
    calls: Vec<f64>,
    prompt: Vec<f64>,
    peak: Vec<f64>,
    generated: Vec<f64>,
    wall: Vec<f64>,
    programs: f64,
}

fn median(v: &[f64]) -> Option<f64> {
    spread(v).map(|s| s.median)
}

/// The change from `a` to `b` as a fraction of `a` (negative: less).
fn change(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) if a > 0.0 => Some((b - a) / a),
        _ => None,
    }
}

fn percent(c: Option<f64>) -> String {
    c.map_or("-".into(), |c| format!("{:+.0} %", c * 100.0))
}

fn number(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("{v:.0}"))
}

fn cells(report: &Value, profile: &str) -> BTreeMap<(u64, String), Cell> {
    let mut out: BTreeMap<(u64, String), Cell> = BTreeMap::new();
    let runs = report.get("runs").and_then(Value::as_array);
    for r in runs.into_iter().flatten() {
        let degraded = r
            .get("health")
            .and_then(|h| serde_json::from_value::<Health>(h.clone()).ok())
            .is_some_and(|h| h.degraded());
        if degraded
            || r["profile"].as_str() != Some(profile)
            || r["arm"].as_str() != Some("gardenwall")
        {
            continue;
        }
        let key = (
            r["tier"].as_u64().unwrap_or(1),
            r["task"].as_str().unwrap_or_default().to_string(),
        );
        let c = out.entry(key).or_default();
        c.runs += 1;
        c.passed += u64::from(r["passed"].as_bool() == Some(true));
        let add = |list: &mut Vec<f64>, field: &str| {
            if let Some(x) = r[field].as_f64() {
                list.push(x);
            }
        };
        add(&mut c.calls, "toolCalls");
        add(&mut c.prompt, "promptTokens");
        add(&mut c.peak, "peakContextTokens");
        add(&mut c.generated, "completionTokens");
        add(&mut c.wall, "wallMs");
        c.programs += r["programs"].as_f64().unwrap_or(0.0);
    }
    out
}

/// The median over tasks of each task's change in one measure.
fn typical(pairs: &[(&Cell, &Cell)], measure: fn(&Cell) -> &Vec<f64>) -> Option<f64> {
    let changes: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| change(median(measure(a)), median(measure(b))))
        .collect();
    median(&changes)
}

/// The report and whether the experiment failed its gate.
pub fn ab(report: &Value, base: &str, new: &str) -> Result<(String, bool), String> {
    let (a, b) = (cells(report, base), cells(report, new));
    if a.is_empty() || b.is_empty() {
        return Err(format!(
            "the file has no runs for both {base} and {new} (run them together: --profiles {base},{new})"
        ));
    }
    let mut md = format!("# {new} against {base}\n\n");
    md.push_str("Medians per task; the change is from the first to the second.\n\n");
    md.push_str("| Task | Tier | Passed | Tool calls | Prompt tokens | Peak context | Generated | Time (s) | Programs |\n|---|---|---|---|---|---|---|---|---|\n");
    let mut pairs: Vec<(&Cell, &Cell)> = Vec::new();
    let (mut pa, mut na, mut pb, mut nb) = (0, 0, 0, 0);
    for ((tier, task), ca) in &a {
        let Some(cb) = b.get(&(*tier, task.clone())) else {
            continue;
        };
        pairs.push((ca, cb));
        pa += ca.passed;
        na += ca.runs;
        pb += cb.passed;
        nb += cb.runs;
        let col = |m: fn(&Cell) -> &Vec<f64>, scale: f64| {
            let (x, y) = (median(m(ca)), median(m(cb)));
            format!(
                "{} → {} ({})",
                number(x.map(|v| v / scale)),
                number(y.map(|v| v / scale)),
                percent(change(x, y))
            )
        };
        md.push_str(&format!(
            "| {task} | {tier} | {}/{} → {}/{} | {} | {} | {} | {} | {} | {:.1} |\n",
            ca.passed,
            ca.runs,
            cb.passed,
            cb.runs,
            col(|c| &c.calls, 1.0),
            col(|c| &c.prompt, 1.0),
            col(|c| &c.peak, 1.0),
            col(|c| &c.generated, 1.0),
            col(|c| &c.wall, 1000.0),
            cb.programs / cb.runs.max(1) as f64,
        ));
    }
    if pairs.is_empty() {
        return Err("the two profiles share no tasks".into());
    }

    let group = |keep: &dyn Fn(f64) -> bool| -> Vec<(&Cell, &Cell)> {
        pairs
            .iter()
            .filter(|(ca, _)| median(&ca.calls).is_some_and(keep))
            .copied()
            .collect()
    };
    let multi = group(&|c| c >= MULTI_STEP_CALLS);
    let single = group(&|c| c <= ONE_STEP_CALLS);
    let p = fisher_lower(pb, nb, pa, na);
    let mut failed = false;
    let mut check = |label: String, ok: bool| {
        failed |= !ok;
        format!("- {} {label}\n", if ok { "PASS" } else { "FAIL" })
    };
    md.push_str("\n## Gate\n\n");
    md.push_str(&check(
        format!("pass rate {pa}/{na} → {pb}/{nb} (not lower: p = {p:.2}, fails under 0.10)"),
        p >= 0.1,
    ));
    if multi.is_empty() {
        md.push_str(&check(
            format!("no task took {MULTI_STEP_CALLS:.0} or more tool calls, so there is nothing to gain from"),
            false,
        ));
    } else {
        for (name, m) in [
            ("prompt tokens", (|c| &c.prompt) as fn(&Cell) -> &Vec<f64>),
            ("peak context", |c| &c.peak),
        ] {
            let c = typical(&multi, m);
            md.push_str(&check(
                format!(
                    "multi-step tasks ({}): {name} {} (wanted {:.0} % less)",
                    multi.len(),
                    percent(c),
                    WANTED_SAVING * 100.0
                ),
                c.is_some_and(|c| c <= -WANTED_SAVING),
            ));
        }
        let c = typical(&multi, |c| &c.wall);
        md.push_str(&check(
            format!(
                "multi-step tasks: time {} (at most {:.0} % more)",
                percent(c),
                ALLOWED_LOSS * 100.0
            ),
            c.is_some_and(|c| c <= ALLOWED_LOSS),
        ));
    }
    for (name, m) in [
        ("time", (|c| &c.wall) as fn(&Cell) -> &Vec<f64>),
        ("prompt tokens", |c| &c.prompt),
    ] {
        let c = typical(&single, m);
        md.push_str(&check(
            format!(
                "one-step tasks ({}): {name} {} (at most {:.0} % more)",
                single.len(),
                percent(c),
                ALLOWED_LOSS * 100.0
            ),
            c.is_none_or(|c| c <= ALLOWED_LOSS),
        ));
    }
    md.push_str(&format!(
        "\nAlso: generated tokens {} on multi-step tasks, {} on one-step tasks.\n",
        percent(typical(&multi, |c| &c.generated)),
        percent(typical(&single, |c| &c.generated)),
    ));
    md.push_str(&format!(
        "\n**{}**\n",
        if failed {
            "The experiment does not pass its gate."
        } else {
            "The experiment passes its gate."
        }
    ));
    Ok((md, failed))
}

pub fn run_ab(file: &Path, base: &str, new: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let report: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let (md, failed) = ab(&report, base, new)?;
    let out = file.with_extension(format!("{new}-vs-{base}.md"));
    std::fs::write(&out, &md).map_err(|e| e.to_string())?;
    println!("{md}");
    eprintln!("Report: {}", out.display());
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(profile: &str, task: &str, calls: u32, prompt: u32, wall: u32, passed: bool) -> Value {
        json!({
            "profile": profile, "arm": "gardenwall", "tier": 2, "task": task,
            "passed": passed, "toolCalls": calls, "promptTokens": prompt,
            "peakContextTokens": prompt / 2, "completionTokens": 300, "wallMs": wall,
            "programs": u32::from(profile == "code-mode"),
        })
    }

    fn report(new_prompt: u32, new_single_wall: u32, new_passes: bool) -> Value {
        let mut runs = Vec::new();
        for _ in 0..5 {
            runs.push(run("default", "rename", 14, 20_000, 20_000, true));
            runs.push(run("default", "count", 1, 4_000, 5_000, true));
            runs.push(run(
                "code-mode",
                "rename",
                2,
                new_prompt,
                15_000,
                new_passes,
            ));
            runs.push(run("code-mode", "count", 1, 4_100, new_single_wall, true));
        }
        json!({ "suite": "agent", "runs": runs })
    }

    #[test]
    fn a_clear_saving_passes() {
        let (md, failed) = ab(&report(9_000, 5_200, true), "default", "code-mode").unwrap();
        assert!(!failed, "{md}");
        assert!(
            md.contains("| rename | 2 | 5/5 → 5/5 | 14 → 2 (-86 %) | 20000 → 9000 (-55 %)"),
            "{md}"
        );
    }

    #[test]
    fn a_small_saving_slower_controls_or_lost_passes_fail() {
        for (prompt, wall, passes) in [
            (18_000, 5_200, true),
            (9_000, 6_000, true),
            (9_000, 5_200, false),
        ] {
            let (md, failed) = ab(&report(prompt, wall, passes), "default", "code-mode").unwrap();
            assert!(failed, "{md}");
        }
    }

    #[test]
    fn both_profiles_must_be_in_the_file() {
        assert!(ab(&report(9_000, 5_200, true), "default", "mtp").is_err());
    }
}
