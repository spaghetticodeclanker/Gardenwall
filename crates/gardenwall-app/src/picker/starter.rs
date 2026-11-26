//! The starter list (`data/starter-models.json`): a few reviewed models per
//! GPU-memory tier, each pinned to a commit and SHA-256, and "Help me
//! choose", whose rule lives here so it is tested.
//!
//! Tiers are memory amounts, not GPU names, so a GPU released later needs
//! no change. The list goes out of date; after six months it says so and
//! points to the ranking and a research session.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::card::{TradeOff, fit_label, gib, trade_off};
use super::dates;
use crate::tuning::{Fit, FitView, Hardware, fit_for};

const LIST_JSON: &str = include_str!("../../data/starter-models.json");
const GIB: u64 = 1 << 30;
/// GPU memory a card of a tier may report below its nominal size.
const TIER_SLACK: u64 = 3 * GIB / 4;
/// After this the list says it is old.
const STALE_DAYS: i64 = 183;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub version: u32,
    pub reviewed: String,
    pub entries: Vec<Entry>,
    pub tiers: Vec<Tier>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub path: String,
    pub commit: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub projector: Option<Projector>,
    pub licence: String,
    pub publisher: String,
    pub kind: Kind,
    pub uses: Vec<Purpose>,
    pub summary: String,
    pub reviewed: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Projector {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    General,
    Coding,
    Vision,
    Small,
}

/// "Help me choose", question 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Purpose {
    Chat,
    Coding,
    Images,
}

/// "Help me choose", question 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    Quality,
    Balanced,
    Speed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tier {
    pub min_gib: u32,
    pub label: String,
    pub recommended: String,
    pub faster: String,
    pub vision: Option<String>,
}

pub fn list() -> &'static List {
    static LIST: OnceLock<List> = OnceLock::new();
    // Checked by tests; a broken list can only come from a bad build.
    LIST.get_or_init(|| serde_json::from_str(LIST_JSON).expect("starter-models.json parses"))
}

impl Entry {
    /// The model and its projector, as loaded together.
    pub fn total(&self) -> u64 {
        self.size + self.projector.as_ref().map_or(0, |p| p.size)
    }
}

impl List {
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The tier for this computer, or `None` when GPU memory is unknown.
    pub fn tier_index(&self, hw: &Hardware) -> Option<usize> {
        let vram = hw.vram_total?;
        self.tiers
            .iter()
            .rposition(|t| vram + TIER_SLACK >= u64::from(t.min_gib) * GIB)
            .or(Some(0))
    }
}

// ---------- what the UI receives ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub path: String,
    pub commit: String,
    /// The model file's SHA-256, to find it in the library once downloaded.
    pub sha256: String,
    pub size: u64,
    /// With the projector, for a vision entry.
    pub total: u64,
    pub projector: bool,
    pub licence: String,
    pub publisher: String,
    pub kind: Kind,
    pub uses: Vec<Purpose>,
    pub summary: String,
    /// When this entry was last checked.
    pub reviewed: String,
    pub fit: Fit,
    pub tight: bool,
    pub label: String,
    pub card: TradeOff,
    /// Already in the library (by SHA-256).
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterView {
    pub hardware: String,
    /// This computer's tier, `None` when GPU memory is unknown (then all
    /// tiers are shown and nothing is recommended).
    pub tier: Option<usize>,
    pub tiers: Vec<Tier>,
    pub entries: Vec<EntryView>,
    /// "Reviewed November 2026".
    pub reviewed: String,
    /// The list's version and review date.
    pub about: String,
    /// Set once the list is six months old.
    pub stale: Option<String>,
}

pub fn view(hw: &Hardware, hardware: String, installed: &dyn Fn(&str) -> bool) -> StarterView {
    let l = list();
    let reviewed_days = dates::days_from_iso(&l.reviewed).unwrap_or(0);
    StarterView {
        hardware,
        tier: l.tier_index(hw),
        tiers: l.tiers.clone(),
        entries: l
            .entries
            .iter()
            .map(|e| entry_view(e, hw, installed))
            .collect(),
        reviewed: format!("Reviewed {}", dates::month(reviewed_days)),
        about: dates::about(l.version, &l.reviewed),
        stale: (dates::today() - reviewed_days > STALE_DAYS).then(|| {
            "This list is old. The ranking and a research session can show newer models.".into()
        }),
    }
}

fn entry_view(e: &Entry, hw: &Hardware, installed: &dyn Fn(&str) -> bool) -> EntryView {
    let v = fit_for(e.total(), hw);
    EntryView {
        id: e.id.clone(),
        name: e.name.clone(),
        repo: e.repo.clone(),
        path: e.path.clone(),
        commit: e.commit.clone(),
        sha256: e.sha256.clone(),
        size: e.size,
        total: e.total(),
        projector: e.projector.is_some(),
        licence: e.licence.clone(),
        publisher: e.publisher.clone(),
        kind: e.kind,
        uses: e.uses.clone(),
        summary: e.summary.clone(),
        reviewed: e.reviewed.clone(),
        fit: v.fit,
        tight: v.tight,
        label: fit_label(v).into(),
        card: trade_off(v),
        installed: installed(&e.sha256)
            && e.projector.as_ref().is_none_or(|p| installed(&p.sha256)),
    }
}

// ---------- "Help me choose" ----------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answers {
    pub purpose: Purpose,
    pub priority: Priority,
    pub long_documents: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    /// "Why this one", in one or two sentences.
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chosen {
    /// One or two entries, the first is the answer.
    pub choices: Vec<Choice>,
    /// Set when nothing can be chosen (GPU memory unknown).
    pub note: Option<String>,
}

/// The rule behind "Help me choose". Uses the real fit on `hw`, not the
/// tier's nominal size.
pub fn choose(answers: Answers, hw: &Hardware) -> Chosen {
    let l = list();
    let Some(t) = l.tier_index(hw) else {
        return Chosen {
            choices: Vec::new(),
            note: Some("Gardenwall couldn't read this computer's GPU memory, so it can't choose for it. Enter a figure under \"Plan for a different amount of GPU memory\", or look through every tier.".into()),
        };
    };
    let tier = &l.tiers[t];
    let fit = |id: &str| -> FitView {
        l.entry(id)
            .map_or_else(|| fit_for(u64::MAX, hw), |e| fit_for(e.total(), hw))
    };
    let suits = |id: &str, p: Purpose| l.entry(id).is_some_and(|e| e.uses.contains(&p));
    let name = |id: &str| l.entry(id).map_or_else(String::new, |e| e.name.clone());
    let roomy = |id: &str| {
        let v = fit(id);
        v.fit == Fit::Gpu && !v.tight
    };

    if answers.purpose == Purpose::Images {
        // The vision slot of this tier, or the nearest smaller tier's.
        let Some(vision) = l.tiers[..=t].iter().rev().find_map(|x| x.vision.clone()) else {
            return Chosen {
                choices: Vec::new(),
                note: Some("No model that sees images is listed for this computer.".into()),
            };
        };
        let mut why = format!(
            "It can look at pictures, screenshots and scanned pages; it comes with its image projector ({}).",
            l.entry(&vision)
                .and_then(|e| e.projector.as_ref())
                .map_or_else(String::new, |p| gib(p.size))
        );
        if fit(&vision).fit != Fit::Gpu {
            why.push_str(" It is the smallest listed, and still runs partly from system RAM here, so expect slow replies.");
        }
        return Chosen {
            choices: vec![
                Choice { id: vision, why },
                Choice {
                    id: tier.recommended.clone(),
                    why: format!(
                        "For text only, {} gives better answers.",
                        name(&tier.recommended)
                    ),
                },
            ],
            note: None,
        };
    }

    let (rec, fast) = (tier.recommended.as_str(), tier.faster.as_str());
    let mut primary = match answers.priority {
        Priority::Quality => rec,
        Priority::Balanced if roomy(rec) => rec,
        Priority::Balanced => fast,
        Priority::Speed => fast,
    };
    let mut reasons: Vec<String> = Vec::new();
    match (answers.priority, primary == rec) {
        (Priority::Quality, _) => reasons.push("The strongest model listed for this computer's GPU memory.".into()),
        (Priority::Balanced, true) => reasons.push("The strongest model that still leaves room for the conversation on this computer.".into()),
        (Priority::Balanced, false) => reasons.push(format!("{} would leave little room for the conversation here, so this smaller model is the better balance.", name(rec))),
        (Priority::Speed, _) => reasons.push("Smaller, so it replies faster and leaves more room, with simpler answers.".into()),
    }
    if answers.long_documents && !roomy(primary) && roomy(fast) && primary != fast {
        primary = fast;
        reasons = vec![format!(
            "Long documents need room for a long context, and {} would leave little.",
            name(rec)
        )];
    }
    if answers.purpose == Purpose::Coding && !suits(primary, Purpose::Coding) {
        let other = if primary == rec { fast } else { rec };
        if suits(other, Purpose::Coding) {
            primary = other;
            reasons = vec!["Coding with the agent needs a model that calls tools reliably; this is the listed one that does.".into()];
        }
    }
    if answers.purpose == Purpose::Coding && suits(primary, Purpose::Coding) && primary == rec {
        reasons.push("Larger models follow the agent's tools more reliably.".into());
    }
    if answers.long_documents && roomy(primary) {
        reasons.push("It leaves room for long documents.".into());
    }
    let mut choices = vec![Choice {
        id: primary.to_string(),
        why: reasons.join(" "),
    }];
    let (other, why) = if primary == rec {
        (
            fast,
            format!(
                "If you prefer quicker replies: {} is smaller and faster, with simpler answers.",
                name(fast)
            ),
        )
    } else {
        (
            rec,
            format!(
                "If you prefer better answers: {} is stronger, and slower.",
                name(rec)
            ),
        )
    };
    if other != primary {
        choices.push(Choice {
            id: other.to_string(),
            why,
        });
    }
    Chosen {
        choices,
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn hw(gib: f64) -> Hardware {
        Hardware {
            gpu_name: Some("GPU".into()),
            vram_total: Some((gib * GIB as f64) as u64),
            ram_total: Some(32 * GIB),
        }
    }

    #[test]
    fn the_list_is_well_formed() {
        let l = list();
        assert!(l.version >= 1);
        assert!(dates::days_from_iso(&l.reviewed).is_some());
        let hex = |s: &str, n: usize| {
            s.len() == n
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        };
        let mut ids = HashSet::new();
        let mut files = HashSet::new();
        for e in &l.entries {
            assert!(ids.insert(&e.id), "duplicate id {}", e.id);
            assert!(
                files.insert((&e.repo, &e.path, e.projector.as_ref().map(|p| &p.path))),
                "duplicate {}",
                e.id
            );
            assert!(hex(&e.commit, 40), "{} commit", e.id);
            assert!(hex(&e.sha256, 64), "{} sha256", e.id);
            assert!(
                gardenwall_models::hub::RepoId::parse(&e.repo).is_ok(),
                "{}",
                e.id
            );
            assert!(
                gardenwall_models::split::parse_split_name(&e.path).is_none(),
                "{}: split files aren't supported here",
                e.id
            );
            assert!(e.size > 0 && !e.uses.is_empty() && !e.summary.is_empty());
            assert!(dates::days_from_iso(&e.reviewed).is_some());
            assert_eq!(
                e.kind == Kind::Vision,
                e.projector.is_some(),
                "{}: vision entries carry their projector",
                e.id
            );
            if let Some(p) = &e.projector {
                assert!(hex(&p.sha256, 64) && p.size > 0);
                assert_eq!(
                    super::super::quant::table().role(&p.path),
                    super::super::quant::Role::Projector
                );
            }
            // Same file, same pin, wherever it is listed.
            for o in l
                .entries
                .iter()
                .filter(|o| o.repo == e.repo && o.path == e.path)
            {
                assert_eq!(
                    (&o.commit, &o.sha256, o.size),
                    (&e.commit, &e.sha256, e.size)
                );
            }
        }
    }

    #[test]
    fn every_tier_has_a_general_entry_that_fits_it() {
        let l = list();
        assert_eq!(l.tiers[0].min_gib, 0);
        assert!(l.tiers.windows(2).all(|w| w[0].min_gib < w[1].min_gib));
        for t in &l.tiers {
            let rec = l.entry(&t.recommended).expect("recommended exists");
            assert_eq!(rec.kind, Kind::General, "{}", t.label);
            let fast = l.entry(&t.faster).expect("faster exists");
            assert!(fast.total() <= rec.total());
            if let Some(v) = &t.vision {
                assert_eq!(l.entry(v).expect("vision exists").kind, Kind::Vision);
            }
            // By estimate on the tier's own size; measured fit is the
            // a check on real hardware.
            if t.min_gib > 0 {
                let h = hw(f64::from(t.min_gib));
                for id in [Some(&t.recommended), Some(&t.faster), t.vision.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    let e = l.entry(id).unwrap();
                    assert_eq!(fit_for(e.total(), &h).fit, Fit::Gpu, "{id} on {}", t.label);
                }
                assert!(
                    !fit_for(rec.total(), &h).tight,
                    "{} is tight on {}",
                    rec.id,
                    t.label
                );
            }
        }
    }

    #[test]
    fn tiers_come_from_memory_amounts() {
        let l = list();
        let tier = |g: f64| l.tier_index(&hw(g)).map(|i| l.tiers[i].min_gib);
        assert_eq!(tier(0.5), Some(0));
        assert_eq!(tier(6.0), Some(0));
        // Cards report a little under their nominal size.
        assert_eq!(tier(7.6), Some(8));
        assert_eq!(tier(11.8), Some(12));
        assert_eq!(tier(31.5), Some(32));
        assert_eq!(tier(96.0), Some(32));
        assert_eq!(l.tier_index(&Hardware::default()), None);
    }

    #[test]
    fn unknown_hardware_recommends_nothing() {
        let v = view(&Hardware::default(), String::new(), &|_| false);
        assert_eq!(v.tier, None);
        assert!(v.entries.iter().all(|e| e.label == "GPU memory unknown"));
        let c = choose(
            Answers {
                purpose: Purpose::Chat,
                priority: Priority::Balanced,
                long_documents: false,
            },
            &Hardware::default(),
        );
        assert!(c.choices.is_empty() && c.note.is_some());
    }

    #[test]
    fn help_me_choose_for_every_answer() {
        let l = list();
        for g in [4.0, 8.0, 12.0, 16.0, 24.0, 32.0] {
            let h = hw(g);
            let tier = &l.tiers[l.tier_index(&h).unwrap()];
            for purpose in [Purpose::Chat, Purpose::Coding, Purpose::Images] {
                for priority in [Priority::Quality, Priority::Balanced, Priority::Speed] {
                    for long_documents in [false, true] {
                        let a = Answers {
                            purpose,
                            priority,
                            long_documents,
                        };
                        let c = choose(a, &h);
                        assert!((1..=2).contains(&c.choices.len()), "{a:?} on {g}");
                        let first = l.entry(&c.choices[0].id).expect("listed");
                        assert!(c.choices.iter().all(|x| !x.why.is_empty()));
                        if purpose == Purpose::Images {
                            assert_eq!(first.kind, Kind::Vision);
                        } else {
                            assert!([&tier.recommended, &tier.faster].contains(&&first.id));
                            assert!(first.uses.contains(&purpose) || purpose == Purpose::Chat);
                        }
                        let fast_suits = l.entry(&tier.faster).unwrap().uses.contains(&purpose);
                        if priority == Priority::Speed && purpose != Purpose::Images && fast_suits {
                            assert_eq!(first.id, tier.faster, "{a:?} on {g}");
                        }
                        if priority == Priority::Quality
                            && purpose == Purpose::Chat
                            && !long_documents
                        {
                            assert_eq!(first.id, tier.recommended);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn long_documents_move_off_a_tight_recommendation() {
        let a = |p, long_documents| Answers {
            purpose: Purpose::Chat,
            priority: p,
            long_documents,
        };
        // 11.3 GiB is the 12 GiB tier with 7.3 GiB usable: the 9B (5.3)
        // fits but is over 70 %, the 4B is roomy.
        let h = hw(11.3);
        assert_eq!(
            choose(a(Priority::Quality, false), &h).choices[0].id,
            "qwen3.5-9b"
        );
        let c = choose(a(Priority::Quality, true), &h);
        assert_eq!(c.choices[0].id, "qwen3.5-4b");
        assert!(c.choices[0].why.contains("Long documents"));
        assert_eq!(c.choices[1].id, "qwen3.5-9b");
        let c = choose(a(Priority::Balanced, false), &h);
        assert_eq!(c.choices[0].id, "qwen3.5-4b");
        // With room, the recommendation stays and says so.
        let c = choose(a(Priority::Quality, true), &hw(16.0));
        assert_eq!(c.choices[0].id, "gemma-4-12b");
        assert!(c.choices[0].why.contains("long documents"));
    }

    #[test]
    fn the_list_says_when_it_is_old() {
        let v = view(&hw(32.0), String::new(), &|_| false);
        assert!(v.reviewed.starts_with("Reviewed "));
        let reviewed = dates::days_from_iso(&list().reviewed).unwrap();
        assert_eq!(v.stale.is_some(), dates::today() - reviewed > STALE_DAYS);
    }

    #[test]
    fn installed_needs_the_projector_too() {
        let v = view(&hw(32.0), String::new(), &|sha| sha.starts_with("d15d"));
        let plain = v.entries.iter().find(|e| e.id == "qwen3.8-27b").unwrap();
        let vision = v
            .entries
            .iter()
            .find(|e| e.id == "qwen3.8-27b-vision")
            .unwrap();
        assert!(plain.installed);
        assert!(!vision.installed);
    }
}
