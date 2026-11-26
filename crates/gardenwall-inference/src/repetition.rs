//! Detect a model stuck repeating itself while it streams.
//!
//! Heavily compressed models sometimes fall into a loop ("Now, the poem.
//! I'll do that." over and over) and never finish their thinking. Without a
//! guard that runs until the context is full and the user gets no answer.
//! The thresholds are deliberately high, so text the user asked to be
//! repeated (a paragraph four times, code with repeated closing braces) is
//! never cut off.

/// Characters of recent output kept for checking.
const TAIL_CHARS: usize = 6000;
/// Check again after this many new characters.
const CHECK_EVERY: usize = 200;
/// A block repeated back to back at least this many times is a loop…
const MIN_REPEATS: usize = 8;
/// …when the repeated stretch is at least this long.
const MIN_LOOP_CHARS: usize = 400;
/// Shortest and longest repeating block considered (characters).
const MIN_PERIOD: usize = 12;
const MAX_PERIOD: usize = 400;
/// Line test: among the last `RECENT_LINES` lines of at least
/// `MIN_LINE_CHARS`, no more than `MAX_DISTINCT_LINES` different ones.
const RECENT_LINES: usize = 40;
const MIN_LINE_CHARS: usize = 12;
const MAX_DISTINCT_LINES: usize = 4;

#[derive(Debug, Default)]
pub struct RepetitionGuard {
    tail: Vec<char>,
    since_check: usize,
}

impl RepetitionGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add streamed text; `true` once the output is looping.
    pub fn push(&mut self, text: &str) -> bool {
        self.tail.extend(text.chars());
        if self.tail.len() > TAIL_CHARS {
            let drop = self.tail.len() - TAIL_CHARS;
            self.tail.drain(..drop);
        }
        self.since_check += text.chars().count();
        if self.since_check < CHECK_EVERY {
            return false;
        }
        self.since_check = 0;
        periodic(&self.tail) || few_distinct_lines(&self.tail)
    }
}

/// The end of `tail` is one block repeated back to back.
fn periodic(tail: &[char]) -> bool {
    (MIN_PERIOD..=MAX_PERIOD).any(|p| {
        let span = (p * MIN_REPEATS).max(MIN_LOOP_CHARS.div_ceil(p) * p);
        if span > tail.len() {
            return false;
        }
        let end = &tail[tail.len() - span..];
        end.iter()
            .enumerate()
            .skip(p)
            .all(|(i, c)| *c == end[i - p])
    })
}

/// The recent lines are a handful of long lines over and over.
fn few_distinct_lines(tail: &[char]) -> bool {
    let text: String = tail.iter().collect();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().count() >= MIN_LINE_CHARS)
        .collect();
    if lines.len() < RECENT_LINES {
        return false;
    }
    let recent = &lines[lines.len() - RECENT_LINES..];
    let mut distinct: Vec<&str> = Vec::new();
    for l in recent {
        if !distinct.contains(l) {
            distinct.push(l);
            if distinct.len() > MAX_DISTINCT_LINES {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(text: &str) -> bool {
        let mut g = RepetitionGuard::new();
        // Stream in small pieces, as tokens arrive.
        text.chars()
            .collect::<Vec<_>>()
            .chunks(7)
            .any(|c| g.push(&c.iter().collect::<String>()))
    }

    #[test]
    fn the_loop_seen_in_testing_is_caught() {
        let start = "Second, a short poem about ducks. I want it to be cute and playful.\n";
        let block = "Now, the structure. I'll do that.\nNow, the explanation. I'll do that.\nNow, the poem. I'll do that.\nSo I'll do that.\n";
        assert!(feed(&format!("{start}{}", block.repeat(40))));
    }

    #[test]
    fn a_loop_without_line_breaks_is_caught() {
        assert!(feed(&"and then we wait, ".repeat(60)));
    }

    #[test]
    fn ordinary_and_requested_repetition_is_left_alone() {
        // A paragraph the user asked for four times.
        let para = "The sandbox gives each model its own identity with no network access, and every file it may read is granted explicitly.\n\n";
        assert!(!feed(&para.repeat(4)));
        // Code with many short repeated lines.
        let code = (0..80)
            .map(|i| format!("    if x == {i} {{\n        return {i};\n    }}\n"))
            .collect::<String>();
        assert!(!feed(&code));
        // A numbered list.
        let list = (1..200)
            .map(|i| format!("{i}. Item number {i} in the list\n"))
            .collect::<String>();
        assert!(!feed(&list));
        // Normal prose.
        assert!(!feed(
            &"Chickens can flap and hop, but domestic breeds are heavy. ".repeat(2)
        ));
    }
}
