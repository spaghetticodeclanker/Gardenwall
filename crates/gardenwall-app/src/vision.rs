//! "Add vision": find an image projector
//! (mmproj) made for a model, so screenshots can be shared with it.
//!
//! Only names and sizes come from the hub; the projector is then downloaded
//! like any model (pinned commit, verified SHA-256, validated GGUF) and the
//! pairing is refused when its output width doesn't match the model's.

use std::collections::BTreeSet;

use gardenwall_models::gguf::GgufSummary;
use gardenwall_models::hub::RepoId;
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Repositories looked at for one search.
const MAX_REPOS: usize = 6;

/// A projector file that may fit the model.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectorCandidate {
    repo: String,
    path: String,
    size: u64,
    /// Published in the same repository as the model.
    same_repo: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectorSearch {
    candidates: Vec<ProjectorCandidate>,
    /// Repositories that were looked at, for the user.
    searched: Vec<String>,
    /// What the search was based on (base model or name).
    based_on: String,
}

fn meta_str<'a>(s: &'a GgufSummary, key: &str) -> Option<&'a str> {
    s.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// `owner/name` from a Hugging Face URL, if it is one.
fn repo_from_url(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://huggingface.co/")?
        .trim_end_matches('/');
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let repo = format!("{owner}/{name}");
    RepoId::parse(&repo).ok().map(|_| repo)
}

/// A search phrase from a model name: without the quantization and file
/// suffixes (e.g. "Huihui-Qwen3.8-27B-abliterated-Q6_K.gguf" →
/// "Huihui-Qwen3.8-27B-abliterated").
fn search_phrase(name: &str) -> String {
    let stem = name.trim_end_matches(".gguf");
    let quant = regex_like_quant(stem);
    stem[..quant].trim_end_matches(['-', '_', '.']).to_string()
}

/// Where a trailing quantization tag (Q4_K_M, IQ2_XS, F16, BF16, …) starts.
fn regex_like_quant(stem: &str) -> usize {
    let bytes = stem.as_bytes();
    for (i, _) in stem.char_indices().rev() {
        if i == 0 || !matches!(bytes[i - 1], b'-' | b'_' | b'.') {
            continue;
        }
        let tail = &stem[i..].to_ascii_uppercase();
        let quant = tail.starts_with('Q') && tail[1..].starts_with(|c: char| c.is_ascii_digit())
            || tail.starts_with("IQ") && tail[2..].starts_with(|c: char| c.is_ascii_digit())
            || matches!(tail.as_str(), "F16" | "BF16" | "F32" | "FP16" | "FP8");
        if quant {
            return i - 1;
        }
    }
    stem.len()
}

/// Order: the model's own repository first, then full-precision files
/// (f16/bf16, what projectors are usually published as), then smaller.
fn rank(c: &ProjectorCandidate) -> (bool, bool, u64) {
    let p = c.path.to_ascii_lowercase();
    (
        !c.same_repo,
        !(p.contains("f16") || p.contains("bf16")),
        c.size,
    )
}

/// Look for projectors made for a library model.
#[tauri::command]
pub async fn find_projectors(
    state: State<'_, AppState>,
    sha256: String,
) -> CmdResult<ProjectorSearch> {
    let record = state.library.get(&sha256).ok_or("model not found")?;
    if record.architecture == "clip" {
        return Err("this is a projector; choose the model to add vision to".into());
    }
    let header = state.library.header(&sha256).ok();
    let client = crate::commands::hub(&state)?;

    let mut repos: Vec<String> = Vec::new();
    let own = match &record.source {
        gardenwall_models::intake::ModelSource::Hub { repo, .. } => Some(repo.clone()),
        _ => None,
    };
    repos.extend(own.clone());
    let base_url = header
        .as_ref()
        .and_then(|h| meta_str(h, "general.base_model.0.repo_url"))
        .and_then(repo_from_url);
    repos.extend(base_url);
    let based_on = header
        .as_ref()
        .and_then(|h| {
            meta_str(h, "general.base_model.0.name")
                .or_else(|| meta_str(h, "general.basename"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| search_phrase(&record.display_name));
    // GGUF conversions of the base model or of this model usually carry
    // the projector.
    for query in [based_on.clone(), search_phrase(&record.display_name)] {
        if repos.len() >= MAX_REPOS {
            break;
        }
        if let Ok(found) = client.search(&query, 8).await {
            repos.extend(
                found
                    .into_iter()
                    .map(|l| l.repo.to_string())
                    .filter(|r| r.to_ascii_lowercase().contains("gguf"))
                    .take(3),
            );
        }
    }
    let mut seen = BTreeSet::new();
    repos.retain(|r| seen.insert(r.to_ascii_lowercase()));
    repos.truncate(MAX_REPOS);

    let mut candidates = Vec::new();
    for repo in &repos {
        let Ok(id) = RepoId::parse(repo) else {
            continue;
        };
        let Ok(resolved) = client.resolve(&id, "main").await else {
            continue;
        };
        candidates.extend(
            resolved
                .files
                .iter()
                .filter(|f| f.file_name.to_ascii_lowercase().contains("mmproj"))
                .map(|f| ProjectorCandidate {
                    repo: resolved.repo.to_string(),
                    path: f.path.clone(),
                    size: f.size,
                    same_repo: own.as_deref() == Some(repo.as_str()),
                }),
        );
    }
    candidates.sort_by_key(rank);
    candidates.truncate(12);
    Ok(ProjectorSearch {
        candidates,
        searched: repos,
        based_on,
    })
}

/// Why a projector can't serve a model, when their headers say so: the
/// projector's output must be as wide as the model's embeddings.
pub fn mismatch(model: &GgufSummary, projector: &GgufSummary) -> Option<String> {
    let want = model.embedding_length()?;
    let got = projector
        .get("clip.vision.projection_dim")
        .and_then(|v| v.as_u64())?;
    (want != got).then(|| {
        format!(
            "this projector was made for a different model (it produces {got}-wide embeddings; the model needs {want})"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_search_phrases() {
        assert_eq!(
            search_phrase("Huihui-Qwen3.8-27B-abliterated-Q6_K.gguf"),
            "Huihui-Qwen3.8-27B-abliterated"
        );
        assert_eq!(search_phrase("Qwen3-0.6B-Q8_0.gguf"), "Qwen3-0.6B");
        assert_eq!(search_phrase("model-IQ2_XS.gguf"), "model");
        assert_eq!(search_phrase("gemma-3-4b-it-BF16.gguf"), "gemma-3-4b-it");
        assert_eq!(search_phrase("plain"), "plain");
    }

    #[test]
    fn only_hugging_face_repo_urls_are_used() {
        assert_eq!(
            repo_from_url("https://huggingface.co/Qwen/Qwen3.8-27B"),
            Some("Qwen/Qwen3.8-27B".into())
        );
        assert_eq!(
            repo_from_url("https://huggingface.co/Qwen/Qwen3.8-27B/"),
            Some("Qwen/Qwen3.8-27B".into())
        );
        assert_eq!(repo_from_url("https://evil.example/Qwen/x"), None);
        assert_eq!(repo_from_url("https://huggingface.co/onlyowner"), None);
    }

    #[test]
    fn the_models_own_full_precision_projector_comes_first() {
        let c = |repo: &str, path: &str, size, same_repo| ProjectorCandidate {
            repo: repo.into(),
            path: path.into(),
            size,
            same_repo,
        };
        let mut v = [
            c("b/x-GGUF", "mmproj-Q8_0.gguf", 600, false),
            c("a/model-GGUF", "mmproj-Q8_0.gguf", 600, true),
            c("b/x-GGUF", "mmproj-F16.gguf", 900, false),
            c("a/model-GGUF", "mmproj-BF16.gguf", 900, true),
        ];
        v.sort_by_key(rank);
        let order: Vec<(&str, &str)> = v
            .iter()
            .map(|c| (c.repo.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("a/model-GGUF", "mmproj-BF16.gguf"),
                ("a/model-GGUF", "mmproj-Q8_0.gguf"),
                ("b/x-GGUF", "mmproj-F16.gguf"),
                ("b/x-GGUF", "mmproj-Q8_0.gguf"),
            ]
        );
    }
}
