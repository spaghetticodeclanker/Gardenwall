//! Retrieval for document chat (ROADMAP Phase 5; Hybrid).
//!
//! - [`chunk`]: splits extracted text into overlapping passages on
//!   paragraph and sentence boundaries.
//! - [`Bm25`]: keyword search that needs no model.
//! - [`cosine`]: similarity of embedding vectors, when the user has picked
//!   an embedding model.
//! - [`fuse`]: reciprocal-rank fusion of the two rankings.
//!
//! Everything here is pure computation over text the trusted core has
//! already decrypted. Document text is untrusted content: it is retrieved
//! and quoted, never interpreted.

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// Target passage size (characters) and the overlap between neighbours.
pub const CHUNK_CHARS: usize = 1200;
pub const CHUNK_OVERLAP: usize = 150;
/// Most passages made from one document.
pub const MAX_CHUNKS_PER_DOCUMENT: usize = 20_000;

/// Split `text` into passages of about [`CHUNK_CHARS`] characters,
/// preferring paragraph, then line, then sentence, then word boundaries,
/// with a small overlap so a fact split across a boundary is still found.
pub fn chunk(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n");
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() && out.len() < MAX_CHUNKS_PER_DOCUMENT {
        let hard_end = (start + CHUNK_CHARS).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            let window: String = chars[start..hard_end].iter().collect();
            let min = CHUNK_CHARS / 2;
            let cut = ["\n\n", "\n", ". ", "? ", "! ", "; ", " "]
                .iter()
                .find_map(|sep| {
                    window
                        .rfind(sep)
                        .map(|byte| window[..byte].chars().count() + sep.chars().count())
                        .filter(|c| *c >= min)
                });
            if let Some(c) = cut {
                end = start + c;
            }
        }
        let piece: String = chars[start..end].iter().collect();
        let piece = piece.trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        if end >= chars.len() {
            break;
        }
        // Step back by the overlap, but always move forward.
        start = (end.saturating_sub(CHUNK_OVERLAP)).max(start + 1);
    }
    out
}

/// Lower-cased word tokens; very short and very common words are dropped.
pub fn tokens(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "her", "was", "one",
        "our", "out", "has", "had", "his", "how", "its", "who", "did", "get", "may", "him", "she",
        "use", "that", "with", "this", "from", "they", "have", "what", "when", "your", "will",
        "into", "than", "then", "them", "were", "been", "which", "their", "there", "would",
        "about", "could", "should", "these", "those", "where", "while",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect()
}

/// An in-memory BM25 index over passages (k1 = 1.2, b = 0.75).
pub struct Bm25 {
    docs: Vec<HashMap<String, u32>>,
    lengths: Vec<f32>,
    df: HashMap<String, u32>,
    avg_len: f32,
}

impl Bm25 {
    pub fn new<S: AsRef<str>>(passages: &[S]) -> Self {
        let mut docs = Vec::with_capacity(passages.len());
        let mut lengths = Vec::with_capacity(passages.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        for p in passages {
            let toks = tokens(p.as_ref());
            lengths.push(toks.len() as f32);
            let mut tf: HashMap<String, u32> = HashMap::new();
            for t in toks {
                *tf.entry(t).or_default() += 1;
            }
            for t in tf.keys() {
                *df.entry(t.clone()).or_default() += 1;
            }
            docs.push(tf);
        }
        let avg_len = if lengths.is_empty() {
            0.0
        } else {
            lengths.iter().sum::<f32>() / lengths.len() as f32
        };
        Self {
            docs,
            lengths,
            df,
            avg_len,
        }
    }

    /// Passage indexes and scores, best first (only passages that match).
    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, f32)> {
        const K1: f32 = 1.2;
        const B: f32 = 0.75;
        let n = self.docs.len() as f32;
        let mut q = tokens(query);
        q.sort();
        q.dedup();
        let mut scores: Vec<(usize, f32)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(i, tf)| {
                let len = self.lengths[i];
                let s: f32 = q
                    .iter()
                    .filter_map(|t| {
                        let f = *tf.get(t)? as f32;
                        let df = *self.df.get(t)? as f32;
                        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                        Some(
                            idf * f * (K1 + 1.0)
                                / (f + K1 * (1.0 - B + B * len / self.avg_len.max(1.0))),
                        )
                    })
                    .sum();
                (s > 0.0).then_some((i, s))
            })
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scores.truncate(limit);
        scores
    }
}

/// Cosine similarity of two vectors (0 when either is empty or zero).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Passage indexes ranked by similarity to `query`, best first.
pub fn rank_by_embedding(
    query: &[f32],
    passages: &[Option<Vec<f32>>],
    limit: usize,
) -> Vec<(usize, f32)> {
    let mut scores: Vec<(usize, f32)> = passages
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.as_ref().map(|e| (i, cosine(query, e))))
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scores.truncate(limit);
    scores
}

/// Reciprocal-rank fusion (k = 60) of several rankings, best first.
pub fn fuse(rankings: &[Vec<(usize, f32)>], limit: usize) -> Vec<usize> {
    const K: f32 = 60.0;
    let mut score: HashMap<usize, f32> = HashMap::new();
    for ranking in rankings {
        for (rank, (i, _)) in ranking.iter().enumerate() {
            *score.entry(*i).or_default() += 1.0 / (K + rank as f32 + 1.0);
        }
    }
    let mut all: Vec<(usize, f32)> = score.into_iter().collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    all.into_iter().take(limit).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_boundaries_overlap_and_cover_everything() {
        let para = "Sentence one is here. ".repeat(40);
        let text = format!("{para}\n\n{para}\n\n{para}");
        let chunks = chunk(&text);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.chars().count() <= CHUNK_CHARS);
        }
        // Every sentence ends up whole somewhere.
        assert!(
            chunks
                .iter()
                .all(|c| c.ends_with('.') || c.ends_with("here"))
        );
        assert!(chunk("").is_empty());
        assert_eq!(chunk("short note"), vec!["short note".to_string()]);
        // A long run without spaces is still split and terminates.
        let blob = "x".repeat(5000);
        let parts = chunk(&blob);
        assert!(parts.len() >= 4 && parts.len() < 10);
    }

    #[test]
    fn bm25_finds_the_relevant_passage() {
        let passages = [
            "The invoice total for March was 4,200 euros, paid by bank transfer.",
            "Our office moved to Utrecht in 2021; the old Amsterdam address is closed.",
            "Contact support by email for password resets and account problems.",
        ];
        let idx = Bm25::new(&passages);
        assert_eq!(idx.search("where is the office now", 3)[0].0, 1);
        assert_eq!(idx.search("March invoice amount", 3)[0].0, 0);
        assert!(idx.search("quantum chromodynamics", 3).is_empty());
        assert!(Bm25::new::<&str>(&[]).search("x", 3).is_empty());
    }

    #[test]
    fn embeddings_and_fusion_rank_sensibly() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        let ranked = rank_by_embedding(
            &[1.0, 0.0],
            &[Some(vec![0.0, 1.0]), None, Some(vec![0.9, 0.1])],
            5,
        );
        assert_eq!(ranked[0].0, 2);
        assert_eq!(ranked.len(), 2);
        // A passage ranked well by both wins.
        let fused = fuse(&[vec![(1, 9.0), (0, 5.0)], vec![(1, 0.9), (2, 0.8)]], 3);
        assert_eq!(fused[0], 1);
        assert_eq!(fused.len(), 3);
    }
}
