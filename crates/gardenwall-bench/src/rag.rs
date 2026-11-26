//! S5 (documents): indexing speed through the sandboxed parse worker,
//! search latency, and retrieval quality on labelled questions, for BM25
//! alone and (with an embedding model) hybrid search. The corpus is
//! generated from a seed (nothing is downloaded).

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::common::{self, ServerConfig, Spread};
use crate::fixtures::{self, Rng, WORDS};

/// One generated document and the facts planted in it.
struct Doc {
    name: String,
    format: &'static str,
    paragraphs: Vec<String>,
}

/// A labelled question: the document that answers it.
struct Question {
    text: String,
    doc: usize,
}

fn title(rng: &mut Rng) -> String {
    let a = rng.pick(WORDS);
    let b = rng.pick(WORDS);
    format!(
        "{}{} {}{}",
        a[..1].to_ascii_uppercase(),
        &a[1..],
        b[..1].to_ascii_uppercase(),
        &b[1..]
    )
}

/// `docs` documents (a quarter PDF, a quarter DOCX, the rest text and
/// Markdown) and one question per fact for `questions` of them.
fn corpus(docs: usize, questions: usize, seed: u64) -> (Vec<Doc>, Vec<Question>) {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    let mut qs = Vec::new();
    // Earlier stations and years, mentioned again as distractors.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for i in 0..docs {
        let format = match i % 4 {
            0 => "pdf",
            1 => "docx",
            2 => "txt",
            _ => "md",
        };
        let station = format!("{} Station {}", title(&mut rng), 100 + i);
        let year = 2015 + rng.below(10);
        let budget = 1000 + rng.below(90_000);
        let crew = 3 + rng.below(40);
        let mut paragraphs: Vec<String> = (0..8)
            .map(|_| {
                (0..4)
                    .map(|_| fixtures::sentence(&mut rng))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        let at = rng.below(paragraphs.len());
        paragraphs[at].push_str(&format!(
            " The maintenance budget of {station} for {year} was {budget} credits, and its crew numbered {crew} people."
        ));
        // Distractors: other stations, their own year, and other credits,
        // so keywords alone don't single out the right report.
        for _ in 0..3 {
            if seen.is_empty() {
                break;
            }
            let (other, other_year) = seen[rng.below(seen.len())].clone();
            let p = rng.below(paragraphs.len());
            paragraphs[p].push_str(&format!(
                " In {other_year}, {other} received {} credits in emergency funding from this office.",
                500 + rng.below(9000)
            ));
        }
        seen.push((station.clone(), year));
        paragraphs.insert(0, format!("Report on {station}"));
        if qs.len() < questions && i % (docs / questions.max(1)).max(1) == 0 {
            // Paraphrased, so keyword overlap is partial.
            qs.push(Question {
                text: format!("How many credits were set aside for upkeep at {station} in {year}?"),
                doc: i,
            });
        }
        out.push(Doc {
            name: format!("doc-{i:03}.{format}"),
            format,
            paragraphs,
        });
    }
    (out, qs)
}

fn write_doc(dir: &Path, d: &Doc) -> std::io::Result<PathBuf> {
    let path = dir.join(&d.name);
    let bytes = match d.format {
        "pdf" => fixtures::pdf(&d.paragraphs),
        "docx" => fixtures::docx(&d.paragraphs),
        "md" => format!(
            "# {}\n\n{}\n",
            d.paragraphs[0],
            d.paragraphs[1..].join("\n\n")
        )
        .into_bytes(),
        _ => d.paragraphs.join("\n\n").into_bytes(),
    };
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Text from a file as the app reads it: PDF and DOCX through the parse
/// worker in its sandbox, text directly.
fn extract(worker: &Path, journal: &Path, path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let format = match path.extension().and_then(|e| e.to_str()) {
        Some("pdf") => "pdf",
        Some("docx") => "docx",
        _ => return Ok(String::from_utf8_lossy(&bytes).into_owned()),
    };
    parse_sandboxed(worker, journal, &bytes, format)
}

#[cfg(any(windows, target_os = "linux"))]
fn parse_sandboxed(
    worker: &Path,
    journal: &Path,
    input: &[u8],
    format: &str,
) -> Result<String, String> {
    use std::io::{Read, Write};

    use gardenwall_sandbox::{Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy};
    let dir = worker.parent().ok_or("no worker folder")?.to_path_buf();
    let mut env = vec![];
    if let Some(root) = std::env::var_os("SystemRoot") {
        env.push(("SystemRoot".into(), root));
    }
    // The app's page-reader limits, under the benchmark's own identity.
    let sandbox = Sandbox::new(SandboxPolicy {
        profile: "Gardenwall.Bench.PageReader".into(),
        grants: vec![PathGrant {
            path: dir.clone(),
            access: Access::ReadExecute,
        }],
        network: Network::Denied,
        limits: Limits {
            memory_bytes: Some(512 << 20),
            max_processes: 1,
            wall_time: Some(std::time::Duration::from_secs(60)),
        },
        env,
        working_dir: Some(dir),
        grant_journal_dir: Some(journal.to_path_buf()),
    })
    .map_err(|e| e.to_string())?;
    let args = [
        std::ffi::OsStr::new("--format"),
        std::ffi::OsStr::new(format),
    ];
    let mut child = sandbox.spawn(worker, &args).map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut text = Vec::new();
    let _ = stdout.read_to_end(&mut text);
    let _ = writer.join();
    let code = child
        .wait_timeout(Some(std::time::Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;
    match code {
        Some(0) => Ok(String::from_utf8_lossy(&text).into_owned()),
        other => Err(format!("the parse worker failed ({other:?})")),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn parse_sandboxed(_w: &Path, _j: &Path, _i: &[u8], _f: &str) -> Result<String, String> {
    Err("no sandbox on this platform".into())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RagResult {
    pub documents: usize,
    pub bytes: u64,
    pub passages: usize,
    pub index_total_ms: f64,
    pub files_per_second: f64,
    pub megabytes_per_second: f64,
    /// Per format: milliseconds per file.
    pub extract_ms: Vec<(String, Spread)>,
    pub bm25_build_ms: f64,
    pub reindex_one_ms: Option<Spread>,
    pub bm25_query_ms: Option<Spread>,
    pub questions: usize,
    pub bm25_hit_at_5: usize,
    pub bm25_mrr: f64,
    pub hybrid: Option<Hybrid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hybrid {
    pub embed_passages_per_second: f64,
    pub query_ms: Option<Spread>,
    pub hit_at_5: usize,
    pub mrr: f64,
}

/// Rank of the right document (1-based) among the passages returned.
fn doc_rank(passage_docs: &[usize], ranking: &[usize], doc: usize) -> Option<usize> {
    let mut seen = Vec::new();
    for &p in ranking {
        let d = passage_docs[p];
        if !seen.contains(&d) {
            seen.push(d);
        }
        if d == doc {
            return Some(seen.len());
        }
    }
    None
}

pub struct RagOptions {
    pub docs: usize,
    pub questions: usize,
    pub reps: usize,
    pub seed: u64,
    /// Server and embedding model for hybrid search.
    pub embed: Option<ServerConfig>,
}

pub async fn run_rag(o: &RagOptions) -> Result<RagResult, String> {
    let dir = common::TempDir::new("rag")?;
    let worker = common::stage_helper(&dir.0.join("worker"), "gardenwall-parse")?;
    let journal = dir.0.join("journal");
    std::fs::create_dir_all(&journal).map_err(|e| e.to_string())?;
    let docs_dir = dir.0.join("docs");
    std::fs::create_dir_all(&docs_dir).map_err(|e| e.to_string())?;
    let (docs, questions) = corpus(o.docs, o.questions, o.seed);
    let mut paths = Vec::new();
    for d in &docs {
        paths.push(write_doc(&docs_dir, d).map_err(|e| e.to_string())?);
    }
    let mut r = RagResult {
        documents: docs.len(),
        questions: questions.len(),
        ..Default::default()
    };
    // Index: extract and chunk every file.
    let mut per_format: Vec<(String, Vec<f64>)> = Vec::new();
    let mut passages: Vec<String> = Vec::new();
    let mut passage_docs: Vec<usize> = Vec::new();
    let t0 = Instant::now();
    for (i, p) in paths.iter().enumerate() {
        r.bytes += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let t = Instant::now();
        let text = extract(&worker, &journal, p).map_err(|e| format!("{}: {e}", docs[i].name))?;
        let took = common::ms(t.elapsed());
        match per_format.iter_mut().find(|(f, _)| f == docs[i].format) {
            Some((_, v)) => v.push(took),
            None => per_format.push((docs[i].format.into(), vec![took])),
        }
        for c in gardenwall_rag::chunk(&text) {
            passages.push(c);
            passage_docs.push(i);
        }
    }
    let t = Instant::now();
    let bm25 = gardenwall_rag::Bm25::new(&passages);
    r.bm25_build_ms = common::ms(t.elapsed());
    r.index_total_ms = common::ms(t0.elapsed());
    r.passages = passages.len();
    r.files_per_second = docs.len() as f64 / (r.index_total_ms / 1000.0);
    r.megabytes_per_second = r.bytes as f64 / 1e6 / (r.index_total_ms / 1000.0);
    r.extract_ms = per_format
        .into_iter()
        .filter_map(|(f, v)| common::spread(&v).map(|s| (f, s)))
        .collect();
    eprintln!(
        "[rag] indexed {} files ({:.1} MB, {} passages) in {:.1} s: {:.1} files/s",
        r.documents,
        r.bytes as f64 / 1e6,
        r.passages,
        r.index_total_ms / 1000.0,
        r.files_per_second
    );
    // One file changed: read it again and rebuild the index.
    let changed = paths
        .iter()
        .position(|p| p.extension().is_some_and(|e| e == "txt"));
    if let Some(ci) = changed {
        let mut times = Vec::new();
        for rep in 0..o.reps.min(5) {
            std::fs::write(
                &paths[ci],
                format!("Edited {rep}.\n\n{}", docs[ci].paragraphs.join("\n\n")),
            )
            .map_err(|e| e.to_string())?;
            let t = Instant::now();
            let text = extract(&worker, &journal, &paths[ci])?;
            let mut all: Vec<String> = passages
                .iter()
                .zip(&passage_docs)
                .filter(|(_, d)| **d != ci)
                .map(|(p, _)| p.clone())
                .collect();
            all.extend(gardenwall_rag::chunk(&text));
            std::hint::black_box(gardenwall_rag::Bm25::new(&all));
            times.push(common::ms(t.elapsed()));
        }
        r.reindex_one_ms = common::spread(&times);
    }
    // Search: latency and quality.
    let mut times = Vec::new();
    let (mut hits, mut rr) = (0, 0.0);
    for q in &questions {
        let mut ranking = Vec::new();
        for _ in 0..o.reps.max(1) {
            let t = Instant::now();
            ranking = bm25.search(&q.text, 20);
            times.push(common::ms(t.elapsed()));
        }
        let ids: Vec<usize> = ranking.iter().map(|(i, _)| *i).collect();
        if let Some(rank) = doc_rank(&passage_docs, &ids, q.doc) {
            if rank <= 5 {
                hits += 1;
            }
            rr += 1.0 / rank as f64;
        }
    }
    r.bm25_query_ms = common::spread(&times);
    r.bm25_hit_at_5 = hits;
    r.bm25_mrr = rr / questions.len().max(1) as f64;
    eprintln!(
        "[rag] BM25: hit@5 {}/{}, MRR {:.3}, query {:.3} ms",
        hits,
        questions.len(),
        r.bm25_mrr,
        r.bm25_query_ms.map_or(0.0, |s| s.median)
    );
    if let Some(cfg) = &o.embed {
        r.hybrid = Some(hybrid(cfg, &bm25, &passages, &passage_docs, &questions).await?);
    }
    Ok(r)
}

async fn hybrid(
    cfg: &ServerConfig,
    bm25: &gardenwall_rag::Bm25,
    passages: &[String],
    passage_docs: &[usize],
    questions: &[Question],
) -> Result<Hybrid, String> {
    let started = common::start_server(cfg).await?;
    let client = &started.client;
    let t = Instant::now();
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::new();
    for batch in passages.chunks(32) {
        let v = client.embeddings(batch).await.map_err(|e| e.to_string())?;
        vectors.extend(v.into_iter().map(Some));
    }
    let embed_rate = passages.len() as f64 / t.elapsed().as_secs_f64();
    let mut times = Vec::new();
    let (mut hits, mut rr) = (0, 0.0);
    for q in questions {
        let t = Instant::now();
        let qv = client
            .embeddings(std::slice::from_ref(&q.text))
            .await
            .map_err(|e| e.to_string())?;
        let by_vec = gardenwall_rag::rank_by_embedding(&qv[0], &vectors, 20);
        let by_kw = bm25.search(&q.text, 20);
        let fused = gardenwall_rag::fuse(&[by_kw, by_vec], 20);
        times.push(common::ms(t.elapsed()));
        if let Some(rank) = doc_rank(passage_docs, &fused, q.doc) {
            if rank <= 5 {
                hits += 1;
            }
            rr += 1.0 / rank as f64;
        }
    }
    started.server.shutdown().await;
    let h = Hybrid {
        embed_passages_per_second: embed_rate,
        query_ms: common::spread(&times),
        hit_at_5: hits,
        mrr: rr / questions.len().max(1) as f64,
    };
    eprintln!(
        "[rag] hybrid: hit@5 {}/{}, MRR {:.3}, {:.0} passages/s embedded",
        h.hit_at_5,
        questions.len(),
        h.mrr,
        h.embed_passages_per_second
    );
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_is_deterministic_and_labelled() {
        let (a, qa) = corpus(40, 10, 5);
        let (b, _) = corpus(40, 10, 5);
        assert_eq!(a.len(), 40);
        assert_eq!(qa.len(), 10);
        assert_eq!(a[3].paragraphs, b[3].paragraphs);
        for q in &qa {
            let station = q
                .text
                .split(" at ")
                .nth(1)
                .unwrap()
                .split(" in ")
                .next()
                .unwrap();
            assert!(a[q.doc].paragraphs.iter().any(|p| p.contains(station)));
        }
        assert_eq!(doc_rank(&[0, 0, 1, 2], &[0, 1, 2, 3], 1), Some(2));
        assert_eq!(doc_rank(&[0, 0, 1, 2], &[0, 1, 2, 3], 2), Some(3));
        assert_eq!(doc_rank(&[0, 1], &[0], 1), None);
    }
}
