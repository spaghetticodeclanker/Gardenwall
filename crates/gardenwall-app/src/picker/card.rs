//! Fit labels, the trade-off card and the best-fit file for a repository's
//! files, worked out in the core from file sizes and this computer's
//! memory (or a figure the user plans for). Nothing here is sent anywhere.

use serde::Serialize;

use super::quant::{self, Quant, Role};
use crate::tuning::{Fit, FitView, Hardware, fit_for};

const GIB: f64 = (1u64 << 30) as f64;

/// The note shown with every label before download.
pub const BEFORE_DOWNLOAD: &str = "Judged from the file size alone: before download Gardenwall can't tell whether a model is Mixture-of-Experts (which runs acceptably when partly in system RAM) or exactly how long a conversation will fit. After download, \"Suggest for this computer\" on the model's card gives the full answer.";

/// One file as the picker sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIn<'a> {
    pub path: &'a str,
    /// The whole model's size (all parts of a split set).
    pub size: u64,
}

/// What the picker says about one file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFacts {
    pub role: Role,
    pub quant: Quant,
    pub fit: Fit,
    pub tight: bool,
    /// "Fits on the GPU", or what a projector or draft file is for.
    pub label: String,
    /// The file the core suggests for this computer (at most one).
    pub best: bool,
    /// Why it is the best fit (set on the best file only).
    pub why_best: Option<String>,
    /// Models only.
    pub card: Option<TradeOff>,
}

/// What choosing this file means, in words.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeOff {
    pub runs: String,
    pub speed: String,
    pub room: String,
    pub larger: Option<String>,
    pub smaller: Option<String>,
}

pub fn fit_label(v: FitView) -> &'static str {
    match (v.fit, v.tight) {
        (Fit::Gpu, false) => "Fits on the GPU",
        (Fit::Gpu, true) => "Fits, with a short context",
        (Fit::Partly, _) => "Part runs from system RAM (slower)",
        (Fit::TooBig, _) => "Too large for this computer",
        (Fit::Unknown, _) => "GPU memory unknown",
    }
}

pub fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

/// Better is smaller: on the GPU with room, on the GPU but tight, partly,
/// too big. Unknown sorts with "on the GPU" (nothing can be said).
fn rank(v: FitView) -> u8 {
    match (v.fit, v.tight) {
        (Fit::Gpu, false) | (Fit::Unknown, _) => 0,
        (Fit::Gpu, true) => 1,
        (Fit::Partly, _) => 2,
        (Fit::TooBig, _) => 3,
    }
}

/// Where it runs, how fast, and how much room is left, for one fit.
pub fn trade_off(v: FitView) -> TradeOff {
    let (runs, speed, room) = match (v.fit, v.tight) {
        (Fit::Gpu, tight) => (
            "All on the graphics card (GPU)",
            "Fastest",
            if tight {
                "Short conversations only: little GPU memory is left after the model"
            } else {
                "Room for long conversations"
            },
        ),
        (Fit::Partly, _) => (
            "Part on the GPU, part in system RAM",
            "Several times slower than a file that fits on the GPU (a Mixture-of-Experts model loses less)",
            "The conversation competes with the model for GPU memory",
        ),
        (Fit::TooBig, _) => (
            "Larger than GPU memory and system RAM together",
            "May not load, or run very slowly",
            "No room left",
        ),
        (Fit::Unknown, _) => (
            "Unknown: Gardenwall couldn't read this computer's GPU memory",
            "Unknown",
            "Unknown",
        ),
    };
    TradeOff {
        runs: runs.into(),
        speed: speed.into(),
        room: room.into(),
        larger: None,
        smaller: None,
    }
}

fn name(q: &Quant, path: &str) -> String {
    q.label
        .clone()
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string())
}

fn larger_text(this: FitView, other: FitView, what: &str) -> String {
    if this.fit == Fit::Unknown {
        return format!(
            "The larger file ({what}) would be a little more accurate, and needs more memory."
        );
    }
    match (other.fit, rank(other) > rank(this)) {
        (Fit::Gpu, true) => format!(
            "The larger file ({what}) would be a little more accurate, but leaves less room for the conversation."
        ),
        (Fit::Gpu, false) => {
            format!(
                "The larger file ({what}) would be a little more accurate and still fits on the GPU."
            )
        }
        _ if this.fit == Fit::Gpu => format!(
            "The larger file ({what}) would be a little more accurate but would not fit on the GPU."
        ),
        _ => format!(
            "The larger file ({what}) would be a little more accurate and even slower here."
        ),
    }
}

fn smaller_text(this: FitView, other: FitView, what: &str, quality: Option<&str>) -> String {
    let loss = quality.map_or_else(
        || "with some loss of quality".to_string(),
        |q| format!("with some loss of quality (its quality: {q})"),
    );
    if this.fit != Fit::Gpu && other.fit == Fit::Gpu {
        format!("The smaller file ({what}) would fit on the GPU and run much faster, {loss}.")
    } else {
        format!("The smaller file ({what}) is faster to download and leaves more room, {loss}.")
    }
}

/// The best fit: the largest model that fits on the GPU without being
/// tight, preferring 4- to 6-bit files over larger ones that add little;
/// else the largest that fits at all; else none. Unknown hardware: none.
fn best(models: &[(usize, FitView, Option<f64>, u64)]) -> Option<(usize, &'static str)> {
    let untight: Vec<_> = models
        .iter()
        .filter(|(_, v, _, _)| v.fit == Fit::Gpu && !v.tight)
        .collect();
    let largest = |set: &[&(usize, FitView, Option<f64>, u64)]| {
        set.iter().max_by_key(|(_, _, _, size)| *size).map(|m| m.0)
    };
    let mid: Vec<_> = untight
        .iter()
        .copied()
        .filter(|(_, _, bits, _)| bits.is_some_and(|b| (4.0..7.0).contains(&b)))
        .collect();
    if let Some(i) = largest(&mid) {
        return Some((
            i,
            "The largest 4- to 6-bit file that fits on the GPU with room left for the conversation. Larger files add little quality for their size.",
        ));
    }
    if let Some(i) = largest(&untight) {
        return Some((
            i,
            "The largest file that fits on the GPU with room left for the conversation.",
        ));
    }
    let fits: Vec<_> = models
        .iter()
        .filter(|(_, v, _, _)| v.fit == Fit::Gpu)
        .collect();
    largest(&fits).map(|i| {
        (
            i,
            "The largest file that fits on the GPU. It leaves little room, so conversations must stay short.",
        )
    })
}

/// Facts for each of a repository's files, in the order given.
pub fn facts(files: &[FileIn<'_>], hw: &Hardware) -> Vec<FileFacts> {
    let table = quant::table();
    let mut out: Vec<FileFacts> = files
        .iter()
        .map(|f| {
            let role = table.role(f.path);
            let quant = table.quant(f.path);
            let v = fit_for(f.size, hw);
            let label = match role {
                Role::Model => fit_label(v).to_string(),
                Role::Projector => {
                    "Image projector: download it with a vision model so the model can see images"
                        .to_string()
                }
                Role::Draft => {
                    "Speeds up a larger model; not a model to chat with on its own".to_string()
                }
            };
            FileFacts {
                role,
                quant,
                fit: v.fit,
                tight: v.tight,
                label,
                best: false,
                why_best: None,
                card: (role == Role::Model).then(|| trade_off(v)),
            }
        })
        .collect();

    // Models by size, for the next size up and down and the best fit.
    let mut models: Vec<(usize, FitView, Option<f64>, u64)> = out
        .iter()
        .enumerate()
        .filter(|(_, f)| f.role == Role::Model)
        .map(|(i, f)| {
            (
                i,
                FitView {
                    fit: f.fit,
                    tight: f.tight,
                },
                f.quant.bits,
                files[i].size,
            )
        })
        .collect();
    models.sort_by_key(|m| m.3);
    for (k, &(i, v, _, _)) in models.iter().enumerate() {
        let describe = |j: usize| {
            let f = &out[j];
            format!("{}, {}", name(&f.quant, files[j].path), gib(files[j].size))
        };
        let larger = models
            .get(k + 1)
            .map(|&(j, w, _, _)| larger_text(v, w, &describe(j)));
        let smaller = k.checked_sub(1).map(|p| {
            let (j, w, _, _) = models[p];
            smaller_text(v, w, &describe(j), out[j].quant.quality.as_deref())
        });
        if let Some(card) = out[i].card.as_mut() {
            card.larger = larger;
            card.smaller = smaller;
        }
    }
    if hw.vram_total.is_some()
        && let Some((i, why)) = best(&models)
    {
        out[i].best = true;
        out[i].why_best = Some(why.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn hw(vram_gib: u64) -> Hardware {
        Hardware {
            gpu_name: Some("GPU".into()),
            vram_total: Some(vram_gib * G),
            ram_total: Some(32 * G),
        }
    }

    fn sized(list: &[(&'static str, f64)]) -> Vec<(String, u64)> {
        list.iter()
            .map(|(p, s)| (p.to_string(), (*s * G as f64) as u64))
            .collect()
    }

    fn run(list: &[(&'static str, f64)], h: &Hardware) -> Vec<FileFacts> {
        let owned = sized(list);
        let files: Vec<FileIn<'_>> = owned
            .iter()
            .map(|(p, s)| FileIn { path: p, size: *s })
            .collect();
        facts(&files, h)
    }

    fn best_of(f: &[FileFacts], list: &[(&'static str, f64)]) -> Option<&'static str> {
        f.iter().position(|x| x.best).map(|i| list[i].0)
    }

    const REPO: &[(&str, f64)] = &[
        ("M-Q3_K_M.gguf", 4.35),
        ("M-Q4_K_M.gguf", 5.29),
        ("M-Q6_K.gguf", 6.95),
        ("M-Q8_0.gguf", 8.87),
        ("M-BF16.gguf", 16.7),
        ("mmproj-F16.gguf", 0.86),
    ];

    #[test]
    fn the_best_fit_prefers_four_to_six_bits_with_room() {
        // 16 GiB: 12 usable, 8.4 untight. Q6_K is the largest mid file.
        let f = run(REPO, &hw(16));
        assert_eq!(best_of(&f, REPO), Some("M-Q6_K.gguf"));
        assert!(f[2].why_best.as_deref().unwrap().contains("4- to 6-bit"));
        // Q8_0 (8.87) fits but is tight on 16 GiB.
        assert_eq!(f[3].label, "Fits, with a short context");
        // 32 GiB: Q8_0 and BF16 fit with room, but Q6_K still wins.
        assert_eq!(best_of(&run(REPO, &hw(32)), REPO), Some("M-Q6_K.gguf"));
        // The projector is never the best, and has no card.
        assert_eq!(f[5].role, Role::Projector);
        assert!(f[5].card.is_none());
    }

    #[test]
    fn the_best_fit_falls_back_then_gives_up() {
        // 10 GiB: 6 usable, 4.2 untight: nothing fits with room, so the
        // largest that fits at all (Q3_K_M: 4.35 + 1.5 <= 6).
        let f = run(REPO, &hw(10));
        assert_eq!(best_of(&f, REPO), Some("M-Q3_K_M.gguf"));
        assert!(f[0].why_best.as_deref().unwrap().contains("short"));
        // 6 GiB: nothing fits on the GPU: none is marked.
        assert_eq!(best_of(&run(REPO, &hw(6)), REPO), None);
        // Unknown hardware: none is marked, every label says so.
        let f = run(REPO, &Hardware::default());
        assert_eq!(best_of(&f, REPO), None);
        assert!(
            f.iter()
                .filter(|x| x.role == Role::Model)
                .all(|x| x.label == "GPU memory unknown")
        );
    }

    #[test]
    fn unknown_labels_are_judged_by_size_alone() {
        let list: &[(&str, f64)] = &[("M-Q4_K_ZZ.gguf", 5.0), ("M-weird.gguf", 7.0)];
        let f = run(list, &hw(16));
        // No 4-6 bit file is known, so the largest with room wins.
        assert_eq!(best_of(&f, list), Some("M-weird.gguf"));
        assert_eq!(f[0].quant.label.as_deref(), Some("Q4_K_ZZ"));
        assert!(!f[0].quant.known);
    }

    #[test]
    fn the_card_names_the_next_size_up_and_down() {
        let f = run(REPO, &hw(16));
        let q6 = f[2].card.as_ref().unwrap();
        assert_eq!(q6.speed, "Fastest");
        assert_eq!(q6.room, "Room for long conversations");
        let larger = q6.larger.as_deref().unwrap();
        assert!(
            larger.contains("Q8_0, 8.9 GiB") && larger.contains("less room"),
            "{larger}"
        );
        assert!(q6.smaller.as_deref().unwrap().contains("Q4_K_M"));
        // The largest has no larger file; the smallest no smaller one.
        assert!(f[4].card.as_ref().unwrap().larger.is_none());
        assert!(f[0].card.as_ref().unwrap().smaller.is_none());
        // BF16 on 16 GiB runs partly from RAM, and the smaller would fit.
        let bf16 = f[4].card.as_ref().unwrap();
        assert!(bf16.speed.starts_with("Several times slower"));
        assert!(
            bf16.smaller
                .as_deref()
                .unwrap()
                .contains("would fit on the GPU")
        );
        let q8 = f[3].card.as_ref().unwrap();
        assert!(q8.larger.as_deref().unwrap().contains("would not fit"));
    }
}
