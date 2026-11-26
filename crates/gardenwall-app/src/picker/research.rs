//! "Research current models": the prepared question for a research session
//! (shown, editable and sent only by the user), and the hub repositories a
//! session really opened, taken from the core's visited list, never from
//! the model's answer.
//!
//! The question names the exact hardware and
//! nothing else about the computer: no user name, paths, serial numbers or
//! installed software. It names no model, so it doesn't date.

use serde::Serialize;

use super::dates;
use super::starter::{Answers, Priority, Purpose};
use crate::tuning::Hardware;
use crate::web::visits::{Visit, VisitKind};

const GIB: f64 = (1u64 << 30) as f64;

/// Paths on the hub's site that are not model repositories.
const NOT_MODELS: &[&str] = &[
    "api",
    "blog",
    "chat",
    "collections",
    "datasets",
    "docs",
    "enterprise",
    "join",
    "learn",
    "login",
    "models",
    "organizations",
    "papers",
    "posts",
    "pricing",
    "settings",
    "spaces",
    "tasks",
];

/// The operating system's name only. Its version isn't read: that would
/// need a new `unsafe` system call or a helper process.
pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

/// "NVIDIA GeForce RTX 5090 with 32 GB of graphics memory, 62 GB of system
/// RAM, Windows". The GPU's name as the system reports it, cleaned of
/// control characters.
pub fn hardware_line(hw: &Hardware, os: &str) -> String {
    let gb = |b: u64| format!("{:.0} GB", b as f64 / GIB);
    let mut parts = Vec::new();
    match (hw.gpu_name.as_deref(), hw.vram_total) {
        (Some(name), Some(v)) => {
            let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
            parts.push(format!("{} with {} of graphics memory", name.trim(), gb(v)));
        }
        (None, Some(v)) => parts.push(format!("a graphics card with {} of graphics memory", gb(v))),
        _ => parts.push("graphics memory that couldn't be read".into()),
    }
    if let Some(r) = hw.ram_total {
        parts.push(format!("{} of system RAM", gb(r)));
    }
    parts.push(os.to_string());
    parts.join(", ")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchQuestion {
    /// The question with the hardware sentence.
    pub with_hardware: String,
    /// The same question without it.
    pub without_hardware: String,
    /// The exact sentence the tick box adds or removes.
    pub hardware_sentence: String,
}

fn purpose_words(p: Purpose) -> &'static str {
    match p {
        Purpose::Chat => "chatting and writing",
        Purpose::Coding => "coding with an agent that calls tools",
        Purpose::Images => "looking at images and screenshots",
    }
}

fn priority_words(p: Priority) -> &'static str {
    match p {
        Priority::Quality => "the best answers matter most",
        Priority::Balanced => "a balance of answer quality and speed matters most",
        Priority::Speed => "the fastest replies matter most",
    }
}

/// The prepared question for `today` (days since 1970).
pub fn question(answers: Option<Answers>, hw: &Hardware, os: &str, today: i64) -> ResearchQuestion {
    let hardware_sentence = format!("My computer: {}.", hardware_line(hw, os));
    let head = vec![format!("Today is {}.", dates::long(today))];
    let wants = answers.map(|a| {
        let mut s = format!(
            "I want a model for {}, and {}.",
            purpose_words(a.purpose),
            priority_words(a.priority)
        );
        if a.long_documents {
            s.push_str(" I will use long documents, so a long context matters.");
        }
        s
    });
    let body = [
        "Which open-weight models released or updated recently are the best choice for this computer, running as GGUF files with llama.cpp? Give three to five. For each, give its Hugging Face repository, the exact file that suits this hardware and why that size, its licence, what it is good at, its drawbacks, and how it compares with the others.",
        "Prefer the publishers' own pages and model cards, and check each claim in a second source. Cite every source as [n]. Say what you couldn't confirm, and don't name a model whose page you didn't open. End with a short table of the trade-offs.",
    ];
    let compose = |hardware: bool| {
        let mut lines = head.clone();
        if hardware {
            lines.push(hardware_sentence.clone());
        }
        lines.extend(wants.clone());
        lines.extend(body.iter().map(|s| s.to_string()));
        lines.join("\n")
    };
    ResearchQuestion {
        with_hardware: compose(true),
        without_hardware: compose(false),
        hardware_sentence: hardware_sentence.clone(),
    }
}

/// Hub repositories among the pages a session really fetched, in the order
/// first opened. Only `https://huggingface.co/<owner>/<name>…` pages that
/// were fetched successfully count.
pub fn opened_repositories(visits: &[Visit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in visits {
        if v.kind != VisitKind::Page || v.ok != Some(true) {
            continue;
        }
        let Ok(url) = url::Url::parse(&v.target) else {
            continue;
        };
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("huggingface.co" | "www.huggingface.co")
            )
        {
            continue;
        }
        let mut segments = url.path_segments().into_iter().flatten();
        let (Some(owner), Some(name)) = (segments.next(), segments.next()) else {
            continue;
        };
        if NOT_MODELS.contains(&owner.to_ascii_lowercase().as_str()) {
            continue;
        }
        if let Ok(repo) = gardenwall_models::hub::RepoId::parse(&format!("{owner}/{name}")) {
            let id = repo.to_string();
            if !out.iter().any(|r| r.eq_ignore_ascii_case(&id)) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::visits::Allowed;

    const G: u64 = 1 << 30;

    fn rtx5090() -> Hardware {
        Hardware {
            gpu_name: Some("NVIDIA GeForce RTX 5090".into()),
            vram_total: Some(32 * G - 300 * (1 << 20)),
            ram_total: Some(62 * G),
        }
    }

    #[test]
    fn the_hardware_line_names_the_exact_hardware() {
        assert_eq!(
            hardware_line(&rtx5090(), "Windows"),
            "NVIDIA GeForce RTX 5090 with 32 GB of graphics memory, 62 GB of system RAM, Windows"
        );
        let unknown = Hardware {
            ram_total: Some(16 * G),
            ..Hardware::default()
        };
        assert_eq!(
            hardware_line(&unknown, "Linux"),
            "graphics memory that couldn't be read, 16 GB of system RAM, Linux"
        );
        let odd = Hardware {
            gpu_name: Some("GPU\u{7}\nName".into()),
            vram_total: Some(8 * G),
            ram_total: None,
        };
        assert_eq!(
            hardware_line(&odd, "Windows"),
            "GPUName with 8 GB of graphics memory, Windows"
        );
    }

    #[test]
    fn the_question_holds_only_the_listed_facts() {
        let today = dates::days_from_iso("2026-11-20").unwrap();
        let q = question(None, &rtx5090(), "Windows", today);
        let expected_tail = "Which open-weight models released or updated recently are the best choice for this computer, running as GGUF files with llama.cpp? Give three to five. For each, give its Hugging Face repository, the exact file that suits this hardware and why that size, its licence, what it is good at, its drawbacks, and how it compares with the others.\nPrefer the publishers' own pages and model cards, and check each claim in a second source. Cite every source as [n]. Say what you couldn't confirm, and don't name a model whose page you didn't open. End with a short table of the trade-offs.";
        assert_eq!(
            q.with_hardware,
            format!(
                "Today is 20 November 2026.\nMy computer: NVIDIA GeForce RTX 5090 with 32 GB of graphics memory, 62 GB of system RAM, Windows.\n{expected_tail}"
            )
        );
        assert_eq!(
            q.without_hardware,
            format!("Today is 20 November 2026.\n{expected_tail}")
        );
        assert_eq!(
            q.with_hardware
                .replace(&format!("{}\n", q.hardware_sentence), ""),
            q.without_hardware
        );
    }

    #[test]
    fn the_answers_are_added_in_plain_words() {
        let today = dates::days_from_iso("2026-11-20").unwrap();
        for purpose in [Purpose::Chat, Purpose::Coding, Purpose::Images] {
            for priority in [Priority::Quality, Priority::Balanced, Priority::Speed] {
                for long_documents in [false, true] {
                    let a = Answers {
                        purpose,
                        priority,
                        long_documents,
                    };
                    let q = question(Some(a), &rtx5090(), "Windows", today);
                    let line = q.with_hardware.lines().nth(2).unwrap();
                    assert!(
                        line.starts_with("I want a model for ")
                            && line.contains(purpose_words(purpose))
                    );
                    assert!(line.contains(priority_words(priority)));
                    assert_eq!(line.contains("long documents"), long_documents);
                    assert!(!q.without_hardware.contains("RTX"));
                }
            }
        }
    }

    fn page(target: &str, ok: Option<bool>) -> Visit {
        Visit {
            at_ms: 0,
            kind: VisitKind::Page,
            target: target.into(),
            via: String::new(),
            allowed: Allowed::Approved,
            ok,
            bytes: None,
            note: None,
        }
    }

    #[test]
    fn repositories_come_only_from_fetched_hub_pages() {
        let visits = vec![
            page("https://huggingface.co/Qwen/Qwen3-8B-GGUF", Some(true)),
            page(
                "https://huggingface.co/qwen/qwen3-8b-gguf/blob/main/README.md",
                Some(true),
            ),
            page(
                "https://huggingface.co/unsloth/Model-GGUF/tree/main",
                Some(true),
            ),
            page("https://huggingface.co/blocked/never-fetched", Some(false)),
            page("https://huggingface.co/pending/not-run", None),
            page("https://huggingface.co/docs/hub", Some(true)),
            page("https://huggingface.co/datasets/a/b", Some(true)),
            page("https://huggingface.co/papers", Some(true)),
            page("http://huggingface.co/plain/http", Some(true)),
            page("https://huggingface.co.evil.example/fake/repo", Some(true)),
            page("https://example.com/Qwen/Qwen3-8B-GGUF", Some(true)),
            Visit {
                kind: VisitKind::Search,
                ..page("https://huggingface.co/search/result", Some(true))
            },
        ];
        assert_eq!(
            opened_repositories(&visits),
            vec![
                "Qwen/Qwen3-8B-GGUF".to_string(),
                "unsloth/Model-GGUF".to_string()
            ]
        );
        // An answer can name any repository; only the visited list counts.
        assert!(opened_repositories(&[]).is_empty());
    }
}
