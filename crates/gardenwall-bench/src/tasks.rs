//! Benchmark tasks: a fixture folder, a request, and checks evaluated by
//! the harness itself (file contents and the final answer). No task code is
//! ever run outside the agent's sandbox.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Suite {
    /// Bump when tasks change, so results stay comparable.
    pub version: u32,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub category: String,
    /// 1: single-step, 2: multi-step, 3: long-context and features.
    #[serde(default = "tier_one")]
    pub tier: u8,
    pub prompt: String,
    /// Later requests in the same session (a resumed conversation).
    #[serde(default)]
    pub followups: Vec<String>,
    /// The first request runs in planning mode (no changes); planning mode
    /// is turned off for the follow-ups.
    #[serde(default)]
    pub plan_first: bool,
    /// Skills offered to the agent (Gardenwall only).
    #[serde(default)]
    pub skills: Vec<TaskSkill>,
    /// Whether the stock Pi arm runs this task (false for Gardenwall-only
    /// features: follow-ups, planning mode, skills).
    #[serde(default = "yes")]
    pub pi: bool,
    /// The stand-in user refuses every approval card in this task (security
    /// tasks: an injected change must not get through on an approval).
    #[serde(default)]
    pub refuse_approvals: bool,
    /// Generated files written before `files` (which may override them).
    #[serde(default)]
    pub generate: Vec<crate::fixtures::Generator>,
    /// Relative path → contents.
    pub files: BTreeMap<String, String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskSkill {
    pub name: String,
    pub description: String,
    pub body: String,
}

fn tier_one() -> u8 {
    1
}

fn yes() -> bool {
    true
}

/// One pass condition. Exactly one field is set per check.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// The file the check is about (for the file checks).
    pub file: Option<String>,
    /// The file's text matches this regular expression.
    pub matches: Option<String>,
    /// The file's text doesn't match it.
    pub not_matches: Option<String>,
    /// The file exists / doesn't.
    pub exists: Option<bool>,
    /// The file is unchanged from the fixture.
    pub unchanged: Option<bool>,
    /// The final answer matches this regular expression (case-insensitive).
    pub answer_matches: Option<String>,
    /// The final answer doesn't match it (case-insensitive).
    pub answer_not_matches: Option<String>,
}

impl Suite {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let suite: Suite =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for t in &suite.tasks {
            if t.files
                .keys()
                .any(|f| f.contains("..") || Path::new(f).is_absolute())
            {
                return Err(format!("task {}: fixture paths must be relative", t.id));
            }
            if !(1..=3).contains(&t.tier) {
                return Err(format!("task {}: tier must be 1, 2 or 3", t.id));
            }
            if t.pi && (!t.followups.is_empty() || t.plan_first || !t.skills.is_empty()) {
                return Err(format!(
                    "task {}: follow-ups, planning mode and skills are Gardenwall-only (set \"pi\": false)",
                    t.id
                ));
            }
            for g in &t.generate {
                g.validate().map_err(|e| format!("task {}: {e}", t.id))?;
            }
            for c in &t.checks {
                c.validate().map_err(|e| format!("task {}: {e}", t.id))?;
            }
        }
        Ok(suite)
    }
}

impl Task {
    /// Write the fixture into an empty folder.
    pub fn materialize(&self, dir: &Path) -> std::io::Result<()> {
        for g in &self.generate {
            g.write(dir)?;
        }
        for (rel, text) in &self.files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, text)?;
        }
        Ok(())
    }

    /// The checks that failed, with reasons (empty: passed).
    pub fn failures(&self, dir: &Path, answer: &str) -> Vec<String> {
        self.checks
            .iter()
            .filter_map(|c| c.evaluate(self, dir, answer).err())
            .collect()
    }
}

fn regex(pattern: &str, case_insensitive: bool) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| e.to_string())
}

impl Check {
    fn validate(&self) -> Result<(), String> {
        let set = [
            self.matches.is_some(),
            self.not_matches.is_some(),
            self.exists.is_some(),
            self.unchanged.is_some(),
            self.answer_matches.is_some(),
            self.answer_not_matches.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if set != 1 {
            return Err("each check needs exactly one condition".into());
        }
        if self.answer_matches.is_none() && self.answer_not_matches.is_none() && self.file.is_none()
        {
            return Err("file checks need a file".into());
        }
        for p in [
            &self.matches,
            &self.not_matches,
            &self.answer_matches,
            &self.answer_not_matches,
        ]
        .into_iter()
        .flatten()
        {
            regex(p, true)?;
        }
        Ok(())
    }

    fn evaluate(&self, task: &Task, dir: &Path, answer: &str) -> Result<(), String> {
        if let Some(p) = &self.answer_matches {
            return if regex(p, true)?.is_match(answer) {
                Ok(())
            } else {
                Err(format!("answer doesn't match /{p}/"))
            };
        }
        if let Some(p) = &self.answer_not_matches {
            return if regex(p, true)?.is_match(answer) {
                Err(format!("answer matches /{p}/"))
            } else {
                Ok(())
            };
        }
        let file = self.file.as_deref().unwrap_or_default();
        let text = std::fs::read_to_string(dir.join(file)).ok();
        if let Some(want) = self.exists {
            return if text.is_some() == want {
                Ok(())
            } else {
                Err(format!(
                    "{file} {}",
                    if want { "is missing" } else { "still exists" }
                ))
            };
        }
        let Some(text) = text else {
            return Err(format!("{file} is missing"));
        };
        // Line endings don't matter (the agent may write CRLF on Windows).
        let text = text.replace("\r\n", "\n");
        if let Some(p) = &self.matches {
            return if regex(p, false)?.is_match(&text) {
                Ok(())
            } else {
                Err(format!("{file} doesn't match /{p}/"))
            };
        }
        if let Some(p) = &self.not_matches {
            return if regex(p, false)?.is_match(&text) {
                Err(format!("{file} still matches /{p}/"))
            } else {
                Ok(())
            };
        }
        if self.unchanged == Some(true) {
            let original = task.files.get(file).map(|s| s.replace("\r\n", "\n"));
            return if original.as_deref() == Some(text.as_str()) {
                Ok(())
            } else {
                Err(format!("{file} was changed"))
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(checks: Vec<Check>) -> Task {
        Task {
            id: "t".into(),
            category: "c".into(),
            tier: 1,
            prompt: "p".into(),
            followups: vec![],
            plan_first: false,
            skills: vec![],
            pi: true,
            refuse_approvals: false,
            generate: vec![],
            files: [("a.py".to_string(), "x = 1\n".to_string())].into(),
            checks,
        }
    }

    #[test]
    fn checks_pass_and_fail_as_described() {
        let dir = tempfile::tempdir().unwrap();
        let t = task(vec![
            Check {
                file: Some("a.py".into()),
                matches: Some(r"x\s*=\s*2".into()),
                ..Default::default()
            },
            Check {
                file: Some("a.py".into()),
                not_matches: Some("x = 1".into()),
                ..Default::default()
            },
            Check {
                file: Some("b.py".into()),
                exists: Some(false),
                ..Default::default()
            },
            Check {
                answer_matches: Some(r"\b42\b".into()),
                ..Default::default()
            },
        ]);
        t.materialize(dir.path()).unwrap();
        assert_eq!(t.failures(dir.path(), "it's 41").len(), 3);
        std::fs::write(dir.path().join("a.py"), "x = 2\r\n").unwrap();
        assert!(t.failures(dir.path(), "The answer is 42.").is_empty());
        let u = task(vec![Check {
            file: Some("a.py".into()),
            unchanged: Some(true),
            ..Default::default()
        }]);
        assert_eq!(u.failures(dir.path(), ""), ["a.py was changed"]);
    }

    #[test]
    fn the_shipped_suite_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/tasks.json");
        let suite = Suite::load(&path).unwrap();
        assert!(suite.tasks.len() >= 25);
        for tier in 1..=3 {
            assert!(
                suite.tasks.iter().filter(|t| t.tier == tier).count() >= 5,
                "tier {tier} has at least 5 tasks"
            );
        }
        let mut ids: Vec<&str> = suite.tasks.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), suite.tasks.len(), "task ids are unique");
        // Each fixture fails its own checks before the agent runs.
        for t in &suite.tasks {
            let dir = tempfile::tempdir().unwrap();
            t.materialize(dir.path()).unwrap();
            assert!(
                !t.failures(dir.path(), "").is_empty(),
                "{} passes untouched",
                t.id
            );
        }
    }
}
