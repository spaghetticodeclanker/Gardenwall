//! The hub ranking (`data/hub-ranking.json`): what people are downloading
//! now, with plain warnings. No model is involved and nothing about the
//! computer is sent: only fixed parameters. Kept in memory for 15 minutes.
//!
//! Nothing here says a model is safe or good. "Known publisher" says who
//! published a repository, from `data/known-publishers.json`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gardenwall_models::hub::{HubClient, HubError, RankedListing};
use serde::{Deserialize, Serialize};

use super::dates;

const CONFIG_JSON: &str = include_str!("../../data/hub-ranking.json");
const PUBLISHERS_JSON: &str = include_str!("../../data/known-publishers.json");

pub const INTRO: &str = "This is what people are downloading, not a test of quality. Gardenwall hasn't tried these models. Every download is still checked.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub reviewed: String,
    pub kinds: Vec<Choice>,
    pub sorts: Vec<Choice>,
    pub fallback_sort: String,
    pub limit: u32,
    pub cache_minutes: u64,
    pub new_days: i64,
    pub few_downloads: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    #[serde(alias = "tag", alias = "key")]
    pub id: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct Publishers {
    pub version: u32,
    pub reviewed: String,
    makers: Vec<String>,
    quantizers: Vec<String>,
}

pub fn config() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| serde_json::from_str(CONFIG_JSON).expect("hub-ranking.json parses"))
}

pub fn publishers() -> &'static Publishers {
    static P: OnceLock<Publishers> = OnceLock::new();
    P.get_or_init(|| serde_json::from_str(PUBLISHERS_JSON).expect("known-publishers.json parses"))
}

impl Publishers {
    pub fn known(&self, owner: &str) -> bool {
        self.makers
            .iter()
            .chain(&self.quantizers)
            .any(|p| p.eq_ignore_ascii_case(owner))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub repo: String,
    pub publisher: String,
    pub known_publisher: bool,
    pub created: Option<String>,
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    pub licence: Option<String>,
    pub gated: Option<bool>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankingView {
    pub kinds: Vec<Choice>,
    pub sorts: Vec<Choice>,
    pub kind: String,
    /// The sort used (the fallback when the hub refused the one asked for).
    pub sort: String,
    /// Says when the hub refused the sort and the fallback was used.
    pub note: Option<String>,
    pub intro: &'static str,
    /// Where the kinds, orders and publisher names come from.
    pub about: String,
    pub rows: Vec<Row>,
}

/// The warnings for one row, from the approved thresholds.
pub fn warnings(l: &RankedListing, today: i64, c: &Config) -> Vec<String> {
    let mut w = Vec::new();
    if let Some(created) = l.created.as_deref().and_then(dates::days_from_iso)
        && today - created < c.new_days
    {
        w.push(format!("New: created under {} days ago", c.new_days));
    }
    if l.downloads.is_some_and(|d| d < c.few_downloads) {
        w.push("Few downloads".into());
    }
    if l.gated == Some(true) {
        w.push("Gated: needs a hub account".into());
    }
    w
}

pub fn row(l: &RankedListing, today: i64) -> Row {
    let owner = l.repo.owner().to_string();
    Row {
        repo: l.repo.to_string(),
        known_publisher: publishers().known(&owner),
        publisher: owner,
        created: l.created.clone(),
        downloads: l.downloads,
        likes: l.likes,
        licence: l.license.clone(),
        gated: l.gated,
        warnings: warnings(l, today, config()),
    }
}

/// The same warnings and label for a repository opened directly (from
/// search, a typed name or a research session), from its resolved facts.
pub fn repo_row(r: &gardenwall_models::hub::ResolvedRepo) -> Row {
    row(
        &RankedListing {
            repo: r.repo.clone(),
            downloads: r.downloads,
            likes: None,
            gated: Some(r.gated),
            created: r.created.clone(),
            trending_score: None,
            license: r.license.clone(),
        },
        dates::today(),
    )
}

type Cache = Mutex<HashMap<(String, String), (Instant, Vec<RankedListing>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The ranking for one kind and sort, from the cache when fresh. A sort
/// the hub refuses falls back to the configured one, with a note.
pub async fn ranking(client: &HubClient, kind: &str, sort: &str) -> Result<RankingView, String> {
    let c = config();
    if !c.kinds.iter().any(|k| k.id == kind) {
        return Err("unknown kind of model".into());
    }
    if !c.sorts.iter().any(|s| s.id == sort) {
        return Err("unknown order".into());
    }
    let (rows, used, note) = match fetch(client, kind, sort).await {
        Ok(rows) => (rows, sort.to_string(), None),
        Err(HubError::Status(400)) if sort != c.fallback_sort => {
            let rows = fetch(client, kind, &c.fallback_sort)
                .await
                .map_err(|e| e.to_string())?;
            let label = |id: &str| {
                c.sorts
                    .iter()
                    .find(|s| s.id == id)
                    .map_or(id.to_string(), |s| s.label.clone())
            };
            (
                rows,
                c.fallback_sort.clone(),
                Some(format!(
                    "The hub no longer accepts \"{}\", so this list is \"{}\" instead.",
                    label(sort),
                    label(&c.fallback_sort)
                )),
            )
        }
        Err(e) => return Err(e.to_string()),
    };
    let today = dates::today();
    Ok(RankingView {
        kinds: c.kinds.clone(),
        sorts: c.sorts.clone(),
        kind: kind.to_string(),
        sort: used,
        note,
        intro: INTRO,
        about: format!(
            "Kinds and orders: {}. Known publishers: {}.",
            dates::about(c.version, &c.reviewed),
            dates::about(publishers().version, &publishers().reviewed)
        ),
        rows: rows.iter().map(|l| row(l, today)).collect(),
    })
}

async fn fetch(client: &HubClient, kind: &str, sort: &str) -> Result<Vec<RankedListing>, HubError> {
    let key = (kind.to_string(), sort.to_string());
    let fresh = Duration::from_secs(config().cache_minutes * 60);
    if let Ok(map) = cache().lock()
        && let Some((at, rows)) = map.get(&key)
        && at.elapsed() < fresh
    {
        return Ok(rows.clone());
    }
    let rows = client.ranking(kind, sort, config().limit).await?;
    if let Ok(mut map) = cache().lock() {
        map.retain(|_, (at, _)| at.elapsed() < fresh);
        map.insert(key, (Instant::now(), rows.clone()));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use gardenwall_models::hub::RepoId;

    use super::*;

    fn listing(id: &str) -> RankedListing {
        RankedListing {
            repo: RepoId::parse(id).unwrap(),
            downloads: None,
            likes: None,
            gated: None,
            created: None,
            trending_score: None,
            license: None,
        }
    }

    #[test]
    fn the_data_files_parse() {
        let c = config();
        assert!(c.version >= 1 && dates::days_from_iso(&c.reviewed).is_some());
        assert!(c.sorts.iter().any(|s| s.id == c.fallback_sort));
        assert!(!c.kinds.is_empty() && c.limit > 0 && c.limit <= 100);
        for x in c.kinds.iter().chain(&c.sorts) {
            assert!(
                x.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "{}",
                x.id
            );
        }
        let p = publishers();
        assert!(p.version >= 1 && dates::days_from_iso(&p.reviewed).is_some());
        for name in p.makers.iter().chain(&p.quantizers) {
            assert!(RepoId::parse(&format!("{name}/x")).is_ok(), "{name}");
        }
    }

    #[test]
    fn warnings_follow_the_thresholds() {
        let c = config();
        let today = dates::days_from_iso("2026-11-20").unwrap();
        let mut l = listing("someone/model-GGUF");
        assert!(
            warnings(&l, today, c).is_empty(),
            "missing fields warn about nothing"
        );
        l.created = Some("2026-11-01".into());
        l.downloads = Some(999);
        l.gated = Some(true);
        let w = warnings(&l, today, c);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].starts_with("New") && w[1] == "Few downloads" && w[2].starts_with("Gated"));
        l.created = Some("2026-10-21".into());
        l.downloads = Some(1000);
        l.gated = Some(false);
        assert!(warnings(&l, today, c).is_empty());
    }

    #[test]
    fn known_publishers_are_named_never_vouched_for() {
        let today = dates::days_from_iso("2026-11-20").unwrap();
        let r = row(&listing("BARTOWSKI/model-GGUF"), today);
        assert!(r.known_publisher);
        assert_eq!(r.publisher, "BARTOWSKI");
        assert!(!row(&listing("bartowsk1/model-GGUF"), today).known_publisher);
        assert!(!INTRO.to_lowercase().contains("safe"));
    }
}
