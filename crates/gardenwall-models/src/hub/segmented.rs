//! Parallel, resumable, ranged downloads into a preallocated file.
//!
//! The file is split into fixed-size chunks that several connections fetch
//! concurrently. One connection to the hub's CDN is often limited to a few
//! MB/s when the file is not cached near the user, so parallel chunks are
//! what makes multi-gigabyte downloads finish at line rate.
//!
//! Some ranges come far slower than others, so slow requests are asked
//! again and the last unfinished chunks are split across idle connections
//! (see [`Tuning`]).
//!
//! A chunk is recorded as done in a small state file beside the partial
//! download only after its bytes are synced, so an interrupted download
//! resumes without refetching completed chunks. Nothing here is trusted for
//! integrity: the caller hashes the whole finished file against the
//! hub-declared digest before it is used.
//!
//! Every request goes through the caller's HTTP client, so the host
//! allowlist (re-checked on each redirect), timeouts and lack of ambient
//! proxy or credentials apply to each chunk exactly as to a single download.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use url::Url;

use super::HubError;

const STATE_MAGIC: &str = "gardenwall-download-v1";
const WRITE_BUFFER_BYTES: usize = 1 << 20;
/// Upper bound on chunks per file, which bounds the state file's size.
const MAX_CHUNKS: u64 = 1 << 16;
/// Failed requests in a row for one piece before the whole download fails
/// (and can be resumed).
const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);

pub(super) struct Plan<'a> {
    pub http: &'a reqwest::Client,
    pub url: Url,
    pub part_path: &'a Path,
    pub state_path: &'a Path,
    pub size: u64,
    pub chunk_bytes: u64,
    pub connections: usize,
}

/// Which fixed-size chunks of the file are complete.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunks {
    size: u64,
    chunk: u64,
    done: Vec<bool>,
}

impl Chunks {
    fn new(size: u64, chunk: u64) -> Self {
        Self {
            size,
            chunk,
            done: vec![false; size.div_ceil(chunk) as usize],
        }
    }

    /// Byte range `[start, end)` of chunk `i`.
    fn range(&self, i: usize) -> (u64, u64) {
        range(self.size, self.chunk, i)
    }

    fn done_bytes(&self) -> u64 {
        (0..self.done.len())
            .filter(|&i| self.done[i])
            .map(|i| {
                let (s, e) = self.range(i);
                e - s
            })
            .sum()
    }

    fn pending(&self) -> VecDeque<usize> {
        (0..self.done.len()).filter(|&i| !self.done[i]).collect()
    }

    fn encode(&self) -> String {
        let mut bytes = vec![0u8; self.done.len().div_ceil(8)];
        for (i, &d) in self.done.iter().enumerate() {
            if d {
                bytes[i / 8] |= 1 << (i % 8);
            }
        }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!("{STATE_MAGIC} {} {} {hex}\n", self.size, self.chunk)
    }

    /// Parse a state file written for this exact size and chunk size.
    fn decode(text: &str, size: u64, chunk: u64) -> Option<Self> {
        let mut parts = text.trim_end_matches('\n').split(' ');
        if parts.next()? != STATE_MAGIC
            || parts.next()?.parse::<u64>().ok()? != size
            || parts.next()?.parse::<u64>().ok()? != chunk
        {
            return None;
        }
        let hex = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let mut chunks = Self::new(size, chunk);
        let n = chunks.done.len();
        if hex.len() != n.div_ceil(8) * 2 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        for (byte_index, pair) in hex.as_bytes().chunks(2).enumerate() {
            let byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            for bit in 0..8 {
                let i = byte_index * 8 + bit;
                let set = byte & (1 << bit) != 0;
                match (i < n, set) {
                    (true, set) => chunks.done[i] = set,
                    (false, true) => return None,
                    (false, false) => {}
                }
            }
        }
        Some(chunks)
    }
}

fn range(size: u64, chunk: u64, i: usize) -> (u64, u64) {
    let start = i as u64 * chunk;
    (start, start.saturating_add(chunk).min(size))
}

/// Chunk size actually used: never so small that the file has more than
/// [`MAX_CHUNKS`] chunks.
fn effective_chunk(size: u64, requested: u64) -> u64 {
    requested.max(size.div_ceil(MAX_CHUNKS)).max(1)
}

async fn write_state(path: &Path, chunks: &Chunks) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, chunks.encode()).await?;
    tokio::fs::rename(&tmp, path).await
}

/// Bring `part_path` to exactly `plan.size` downloaded bytes.
///
/// A partial file without a state file is either complete (its length
/// equals the declared size, e.g. finished but not yet verified) or a
/// sequential partial download whose prefix is kept.
pub(super) async fn fetch(
    plan: Plan<'_>,
    progress: &mut (impl FnMut(u64, u64) -> bool + Send),
) -> Result<(), HubError> {
    let size = plan.size;
    let chunk = effective_chunk(size, plan.chunk_bytes);
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(plan.part_path)
        .await?;
    let have = file.metadata().await?.len();

    let mut chunks = if tokio::fs::try_exists(plan.state_path).await? {
        let saved = tokio::fs::read_to_string(plan.state_path)
            .await
            .ok()
            .and_then(|text| Chunks::decode(&text, size, chunk));
        match saved {
            Some(chunks) if have <= size => chunks,
            // Unreadable or mismatched state: start over.
            _ => {
                file.set_len(0).await?;
                Chunks::new(size, chunk)
            }
        }
    } else if have == size {
        return Ok(());
    } else {
        let have = if have > size {
            file.set_len(0).await?;
            0
        } else {
            have
        };
        let mut chunks = Chunks::new(size, chunk);
        for i in 0..chunks.done.len() {
            chunks.done[i] = chunks.range(i).1 <= have;
        }
        chunks
    };

    let pending = chunks.pending();
    if pending.is_empty() {
        drop(file);
        tokio::fs::remove_file(plan.state_path).await?;
        return Ok(());
    }
    // Record the plan before growing the file, so a crash in between is
    // still resumed from the recorded chunks.
    write_state(plan.state_path, &chunks).await?;
    let file = file.into_std().await;
    // Best effort: avoids NTFS zero-filling the gap before each chunk.
    super::sparse::mark(&file);
    file.set_len(size)?;
    drop(file);

    let workers = plan.connections.clamp(1, pending.len());
    // Bytes still missing per chunk: a chunk is done when this reaches zero,
    // which may take several pieces once a slow chunk has been split.
    let mut missing: Vec<u64> = (0..chunks.done.len())
        .map(|i| {
            let (s, e) = chunks.range(i);
            if chunks.done[i] { 0 } else { e - s }
        })
        .collect();
    let sched = Arc::new(Mutex::new(Sched::new(
        size,
        chunk,
        pending,
        workers,
        Tuning::DEFAULT,
    )));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut set = JoinSet::new();
    for id in 0..workers {
        let http = plan.http.clone();
        let url = plan.url.clone();
        let path = plan.part_path.to_path_buf();
        let sched = sched.clone();
        let tx = tx.clone();
        set.spawn(async move {
            let worker = Worker {
                http: &http,
                url: &url,
                path: &path,
                size,
                sched: &sched,
                tx: &tx,
                id,
            };
            if let Err(error) = worker.run().await {
                let _ = tx.send(Event::Failed(error));
            }
        });
    }
    drop(tx);

    let mut reported = chunks.done_bytes();
    let outcome = loop {
        match rx.recv().await {
            None => break Ok(()),
            Some(Event::Progress(n)) => {
                reported = reported.saturating_add(n).min(size);
                if !progress(reported, size) {
                    break Err(HubError::Cancelled);
                }
            }
            Some(Event::Done { chunk: i, bytes }) => {
                missing[i] = missing[i].saturating_sub(bytes);
                if missing[i] == 0 && !chunks.done[i] {
                    chunks.done[i] = true;
                    if let Err(error) = write_state(plan.state_path, &chunks).await {
                        break Err(error.into());
                    }
                }
            }
            Some(Event::Failed(error)) => break Err(error),
        }
    };
    set.abort_all();
    while set.join_next().await.is_some() {}
    outcome?;

    if chunks.done.iter().any(|d| !d) {
        return Err(HubError::Incomplete {
            expected: size,
            actual: chunks.done_bytes(),
        });
    }
    tokio::fs::remove_file(plan.state_path).await?;
    Ok(())
}

enum Event {
    /// Bytes written.
    Progress(u64),
    /// A piece of chunk `chunk` is written and synced.
    Done {
        chunk: usize,
        bytes: u64,
    },
    Failed(HubError),
}

/// How slow parts of a download are handled.
///
/// The hub's CDN serves some ranges of a file far slower than others (it
/// fetches them from storage first): measured 2026-11-21, one 64 MiB range
/// arrived at 1 MB/s while the next came at 45 MB/s on the same connection,
/// and asking again for the rest of a slow range was often many times
/// faster. So a request well below the best recent rate is restarted from
/// where it got to, and when no chunks are left to start, idle connections
/// split the largest unfinished piece instead of waiting for it.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// Pieces are never split smaller than this, and a piece with less
    /// than this left is not restarted.
    min_piece: u64,
    /// How long a request is measured before it can be judged slow.
    window: Duration,
    /// A request is slow below the best recent rate divided by this.
    slow_divisor: f64,
    /// Restarts of one piece for slowness (transfer errors count apart).
    max_restarts: u32,
}

impl Tuning {
    const DEFAULT: Self = Self {
        min_piece: 4 << 20,
        window: Duration::from_secs(4),
        slow_divisor: 6.0,
        max_restarts: 8,
    };
}

/// How long a best rate counts before a newer measurement replaces it.
const BEST_RATE_MAX_AGE: Duration = Duration::from_secs(30);
/// How often a waiting request is checked for slowness.
const TICK: Duration = Duration::from_secs(1);

/// A byte range `[pos, end)` still to write, within one chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    chunk: usize,
    start: u64,
    pos: u64,
    end: u64,
}

/// Which worker writes which bytes. A worker claims bytes before writing
/// them, and a split only ever takes bytes nobody has claimed, so no byte is
/// written twice.
struct Sched {
    size: u64,
    chunk: u64,
    queue: VecDeque<usize>,
    active: Vec<Option<Piece>>,
    best: Option<(f64, Instant)>,
    tuning: Tuning,
}

impl Sched {
    fn new(size: u64, chunk: u64, queue: VecDeque<usize>, workers: usize, tuning: Tuning) -> Self {
        Self {
            size,
            chunk,
            queue,
            active: vec![None; workers],
            best: None,
            tuning,
        }
    }

    /// The next piece for `worker`: a whole pending chunk or, when none is
    /// left, the back half of the unfinished piece with the most left.
    fn next(&mut self, worker: usize) -> Option<Piece> {
        let piece = match self.queue.pop_front() {
            Some(i) => {
                let (start, end) = range(self.size, self.chunk, i);
                Some(Piece {
                    chunk: i,
                    start,
                    pos: start,
                    end,
                })
            }
            None => self.split(),
        };
        self.active[worker] = piece;
        piece
    }

    fn split(&mut self) -> Option<Piece> {
        let min = self.tuning.min_piece;
        let victim = self
            .active
            .iter_mut()
            .flatten()
            .filter(|p| p.end - p.pos >= 2 * min)
            .max_by_key(|p| p.end - p.pos)?;
        let mid = victim.pos + (victim.end - victim.pos) / 2;
        let back = Piece {
            chunk: victim.chunk,
            start: mid,
            pos: mid,
            end: victim.end,
        };
        victim.end = mid;
        Some(back)
    }

    fn piece(&self, worker: usize) -> Option<Piece> {
        self.active[worker]
    }

    /// Claim up to `n` received bytes of `worker`'s piece: how many fit (fewer
    /// when the piece was split meanwhile) and how many are left after them.
    fn claim(&mut self, worker: usize, n: u64) -> (u64, u64) {
        match self.active[worker].as_mut() {
            Some(p) => {
                let take = n.min(p.end - p.pos);
                p.pos += take;
                (take, p.end - p.pos)
            }
            None => (0, 0),
        }
    }

    fn finish(&mut self, worker: usize) -> Option<Piece> {
        self.active[worker].take()
    }

    /// Record a request's recent rate; true when it is slow next to the best
    /// recent one.
    fn is_slow(&mut self, rate: f64, now: Instant) -> bool {
        let best = match self.best {
            Some((best, at)) if best >= rate && now.duration_since(at) < BEST_RATE_MAX_AGE => best,
            _ => {
                self.best = Some((rate, now));
                rate
            }
        };
        rate * self.tuning.slow_divisor < best
    }
}

enum Stop {
    /// The piece is complete (or was split, and its own part is complete).
    Finished,
    /// The request was slow; ask again for the rest.
    Slow,
}

struct Worker<'a> {
    http: &'a reqwest::Client,
    url: &'a Url,
    path: &'a Path,
    size: u64,
    sched: &'a Mutex<Sched>,
    tx: &'a mpsc::UnboundedSender<Event>,
    id: usize,
}

impl Worker<'_> {
    fn sched(&self) -> std::sync::MutexGuard<'_, Sched> {
        self.sched.lock().expect("download scheduler lock")
    }

    async fn run(&self) -> Result<(), HubError> {
        loop {
            let next = self.sched().next(self.id);
            if next.is_none() {
                return Ok(());
            }
            let max_restarts = self.sched().tuning.max_restarts;
            let (mut failures, mut restarts) = (0, 0);
            loop {
                let before = self.sched().piece(self.id).map(|p| p.pos);
                let outcome = self.request(restarts < max_restarts).await;
                let after = self.sched().piece(self.id).map(|p| p.pos);
                if after != before {
                    // Bytes arrived, so a later failure is a new one.
                    failures = 0;
                }
                match outcome {
                    Ok(Stop::Finished) => break,
                    Ok(Stop::Slow) => restarts += 1,
                    Err(error) if failures + 1 < MAX_ATTEMPTS && retryable(&error) => {
                        tokio::time::sleep(RETRY_BASE_DELAY * 2u32.pow(failures)).await;
                        failures += 1;
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(done) = self.sched().finish(self.id) {
                let _ = self.tx.send(Event::Done {
                    chunk: done.chunk,
                    bytes: done.pos - done.start,
                });
            }
        }
    }

    /// One request for the rest of this worker's piece. What arrived is
    /// written and synced even when the transfer fails, so the next request
    /// continues after it.
    async fn request(&self, may_restart: bool) -> Result<Stop, HubError> {
        let Some(piece) = self.sched().piece(self.id) else {
            return Ok(Stop::Finished);
        };
        if piece.pos >= piece.end {
            return Ok(Stop::Finished);
        }
        let (first, last) = (piece.pos, piece.end - 1);
        let begun = Instant::now();
        let response = self
            .http
            .get(self.url.clone())
            .header(reqwest::header::RANGE, format!("bytes={first}-{last}"))
            .send()
            .await?;
        match response.status().as_u16() {
            206 => {
                let range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range);
                match range {
                    Some((s, e, total))
                        if s == first && e == last && total.is_none_or(|t| t == self.size) => {}
                    _ => return Err(HubError::UnexpectedContentRange),
                }
            }
            // A full response is only acceptable when the piece is the whole file.
            200 if first == 0 && piece.end == self.size => {}
            200 => return Err(HubError::RangesUnsupported),
            _ => return Err(super::refusal(&response)),
        }

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(self.path)
            .await?;
        file.seek(std::io::SeekFrom::Start(first)).await?;
        let mut out = tokio::io::BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
        let result = self
            .copy(response, &mut out, last + 1 - first, begun, may_restart)
            .await;
        out.flush().await?;
        out.get_ref().sync_data().await?;
        result
    }

    async fn copy(
        &self,
        response: reqwest::Response,
        out: &mut tokio::io::BufWriter<tokio::fs::File>,
        asked: u64,
        begun: Instant,
        may_restart: bool,
    ) -> Result<Stop, HubError> {
        let tuning = self.sched().tuning;
        let mut stream = response.bytes_stream();
        let mut received = 0u64;
        let mut left = asked;
        let (mut window_start, mut window_bytes) = (begun, 0u64);
        loop {
            match tokio::time::timeout(TICK, stream.next()).await {
                Ok(Some(bytes)) => {
                    let bytes = bytes?;
                    let n = bytes.len() as u64;
                    received += n;
                    if received > asked {
                        return Err(HubError::TooManyBytes);
                    }
                    let (take, rest) = self.sched().claim(self.id, n);
                    out.write_all(&bytes[..take as usize]).await?;
                    window_bytes += take;
                    left = rest;
                    if take > 0 {
                        let _ = self.tx.send(Event::Progress(take));
                    }
                    if take < n {
                        // The piece was split and its own part is complete.
                        return Ok(Stop::Finished);
                    }
                }
                Ok(None) if left == 0 => return Ok(Stop::Finished),
                Ok(None) => {
                    return Err(HubError::Incomplete {
                        expected: asked,
                        actual: received,
                    });
                }
                // Nothing arrived for a tick: still judge the speed below.
                Err(_) => {}
            }
            let now = Instant::now();
            let span = now.duration_since(window_start);
            if span >= tuning.window {
                let rate = window_bytes as f64 / span.as_secs_f64();
                let slow = self.sched().is_slow(rate, now);
                if slow && may_restart && left >= tuning.min_piece {
                    return Ok(Stop::Slow);
                }
                (window_start, window_bytes) = (now, 0);
            }
        }
    }
}

/// Transient failures worth retrying. Policy refusals (such as a redirect to
/// a host outside the allowlist) and integrity violations are not.
fn retryable(error: &HubError) -> bool {
    match error {
        HubError::Http(e) => !e.is_redirect() && !e.is_builder(),
        HubError::Incomplete { .. } => true,
        HubError::Status(code) => *code == 429 || (500..=599).contains(code),
        _ => false,
    }
}

/// Parse `bytes <start>-<end>/<total|*>`.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let total = match total {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    let (start, end): (u64, u64) = (start.parse().ok()?, end.parse().ok()?);
    (start <= end).then_some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_is_parsed_strictly() {
        assert_eq!(
            parse_content_range("bytes 100-199/200"),
            Some((100, 199, Some(200)))
        );
        assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, 0, None)));
        assert_eq!(parse_content_range("bytes */200"), None);
        assert_eq!(parse_content_range("bytes 5-4/200"), None);
        assert_eq!(parse_content_range("bytes 1-2/x"), None);
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }

    #[test]
    fn state_round_trips_and_rejects_mismatches() {
        let mut c = Chunks::new(10_000, 1024);
        assert_eq!(c.done.len(), 10);
        c.done[0] = true;
        c.done[9] = true;
        let text = c.encode();
        assert_eq!(Chunks::decode(&text, 10_000, 1024), Some(c.clone()));
        assert_eq!(c.done_bytes(), 1024 + (10_000 - 9 * 1024));
        // Different file or chunk size.
        assert_eq!(Chunks::decode(&text, 10_001, 1024), None);
        assert_eq!(Chunks::decode(&text, 10_000, 2048), None);
        // Bits beyond the last chunk, bad hex, wrong length, trailing junk.
        let bad = [
            format!("{STATE_MAGIC} 10000 1024 01ff"),
            format!("{STATE_MAGIC} 10000 1024 01zz"),
            format!("{STATE_MAGIC} 10000 1024 01"),
            format!("{STATE_MAGIC} 10000 1024 0102 x"),
            "other 10000 1024 0102".to_string(),
        ];
        for text in bad {
            assert_eq!(Chunks::decode(&text, 10_000, 1024), None, "{text}");
        }
    }

    #[test]
    fn chunk_count_is_bounded() {
        let size = 1 << 40;
        let chunk = effective_chunk(size, 1);
        assert!(size.div_ceil(chunk) <= MAX_CHUNKS);
        assert_eq!(effective_chunk(1000, 64), 64);
        assert_eq!(range(1000, 64, 15), (960, 1000));
    }

    fn sched(size: u64, chunk: u64, workers: usize) -> Sched {
        let queue = (0..size.div_ceil(chunk) as usize).collect();
        let tuning = Tuning {
            min_piece: 10,
            ..Tuning::DEFAULT
        };
        Sched::new(size, chunk, queue, workers, tuning)
    }

    #[test]
    fn idle_workers_split_the_largest_unfinished_piece() {
        let mut s = sched(200, 100, 3);
        assert_eq!(s.next(0).map(|p| (p.pos, p.end)), Some((0, 100)));
        assert_eq!(s.next(1).map(|p| (p.pos, p.end)), Some((100, 200)));
        // Worker 0 is 30 bytes in; worker 1 only 4.
        assert_eq!(s.claim(0, 30), (30, 70));
        assert_eq!(s.claim(1, 4), (4, 96));
        // No chunks left: worker 2 takes the back half of worker 1's piece.
        let back = s.next(2).unwrap();
        assert_eq!((back.chunk, back.pos, back.end), (1, 152, 200));
        // Worker 1 now stops at the split: a 60-byte buffer only half fits.
        assert_eq!(s.claim(1, 60), (48, 0));
        assert_eq!(s.claim(1, 10), (0, 0));
        let front = s.finish(1).unwrap();
        assert_eq!(front.pos - front.start, 52);
        // Pieces under twice the minimum are left alone.
        assert_eq!(s.claim(0, 55), (55, 15));
        assert_eq!(s.claim(2, 30), (30, 18));
        assert_eq!(s.next(1), None);
    }

    #[test]
    fn slow_means_well_below_the_best_recent_rate() {
        let mut s = sched(100, 10, 1);
        let t = Instant::now();
        assert!(!s.is_slow(60.0, t));
        assert!(!s.is_slow(20.0, t));
        assert!(s.is_slow(5.0, t));
        // A uniformly slow line is not slow next to itself.
        let later = t + BEST_RATE_MAX_AGE;
        assert!(!s.is_slow(5.0, later));
        assert!(!s.is_slow(4.0, later));
    }
}
