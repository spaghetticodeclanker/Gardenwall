//! Deterministic input mutation for robustness tests (Phase 6 groundwork:
//! "fuzzing the parsers", on stable Rust and in CI).
//!
//! Every parser of untrusted data (model files, documents, web pages,
//! executables, MCP and model-server streams, imported sessions) gets a
//! test that feeds it thousands of mutated valid inputs and requires that
//! it never panics, hangs or allocates without bound. The same seed always
//! produces the same inputs, so a failure is reproducible from the
//! iteration number it reports.

#![forbid(unsafe_code)]

/// A small, fast, deterministic generator (xorshift64*).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

/// Values that break length and offset arithmetic.
const EVIL_U64: &[u64] = &[
    0,
    1,
    0x7f,
    0x80,
    0xff,
    0x7fff,
    0xffff,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_ffff,
    0x1_0000_0000,
    0x7fff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
];

/// Bytes that matter to text formats.
const EVIL_BYTES: &[u8] = b"\0\xff\x7f\x80<>&\"'\\/\n\r{}[]%";

/// One mutation of `input`: flips, boundary bytes, evil integers written
/// over 2, 4 or 8 bytes, truncation, duplication, deletion and insertion.
pub fn mutate(input: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut out = input.to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        let len = out.len();
        match rng.below(8) {
            0 if len > 0 => {
                let i = rng.below(len);
                out[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                out[i] = EVIL_BYTES[rng.below(EVIL_BYTES.len())];
            }
            2 if len >= 8 => {
                let v = EVIL_U64[rng.below(EVIL_U64.len())];
                let width = [2usize, 4, 8][rng.below(3)];
                let i = rng.below(len - width + 1);
                out[i..i + width].copy_from_slice(&v.to_le_bytes()[..width]);
            }
            3 if len > 0 => out.truncate(rng.below(len)),
            4 if len > 1 => {
                let a = rng.below(len);
                let b = (a + 1 + rng.below(64)).min(len);
                let chunk = out[a..b].to_vec();
                let at = rng.below(out.len());
                for (k, byte) in chunk.into_iter().enumerate().take(4096) {
                    out.insert(at + k, byte);
                }
            }
            5 if len > 1 => {
                let a = rng.below(len);
                let b = (a + 1 + rng.below(64)).min(len);
                out.drain(a..b);
            }
            6 => {
                let at = rng.below(len + 1);
                for k in 0..1 + rng.below(16) {
                    out.insert(at + k, rng.next_u64() as u8);
                }
            }
            _ => {
                // Repeat a slice many times (deep nesting, huge lists).
                if len > 0 {
                    let a = rng.below(len);
                    let b = (a + 1 + rng.below(8)).min(len);
                    let chunk = out[a..b].to_vec();
                    let at = rng.below(out.len());
                    let reps = 1 + rng.below(200);
                    let tail = out.split_off(at);
                    for _ in 0..reps {
                        out.extend_from_slice(&chunk);
                    }
                    out.extend_from_slice(&tail);
                }
            }
        }
    }
    out
}

/// Run `check` on `iterations` mutations of each seed input. On a panic,
/// the message names the seed and iteration so the input can be rebuilt.
pub fn run(name: &str, seeds: &[Vec<u8>], iterations: usize, mut check: impl FnMut(&[u8])) {
    let mut rng = Rng::new(
        name.bytes()
            .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64)),
    );
    for seed in seeds {
        check(seed);
    }
    for i in 0..iterations {
        let seed = &seeds[i % seeds.len()];
        let input = mutate(seed, &mut rng);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&input)));
        if result.is_err() {
            panic!(
                "{name}: panicked on iteration {i} (seed input {}, {} bytes)",
                i % seeds.len(),
                input.len()
            );
        }
    }
}

/// [`run`] for parsers that take text: mutations are decoded lossily.
pub fn run_text(name: &str, seeds: &[&str], iterations: usize, mut check: impl FnMut(&str)) {
    let seeds: Vec<Vec<u8>> = seeds.iter().map(|s| s.as_bytes().to_vec()).collect();
    run(name, &seeds, iterations, |b| {
        check(&String::from_utf8_lossy(b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_is_deterministic_and_varied() {
        let input = b"GGUF\x03\0\0\0 some header bytes and more".to_vec();
        let a: Vec<Vec<u8>> = {
            let mut r = Rng::new(7);
            (0..50).map(|_| mutate(&input, &mut r)).collect()
        };
        let b: Vec<Vec<u8>> = {
            let mut r = Rng::new(7);
            (0..50).map(|_| mutate(&input, &mut r)).collect()
        };
        assert_eq!(a, b);
        let distinct: std::collections::HashSet<&Vec<u8>> = a.iter().collect();
        assert!(distinct.len() > 40);
        assert!(mutate(&[], &mut Rng::new(1)).len() < 10_000);
    }

    #[test]
    fn panics_are_reported_with_the_iteration() {
        // The seed itself passes; the first different mutation panics.
        let r =
            std::panic::catch_unwind(|| run("t", &[b"ab".to_vec()], 100, |b| assert_eq!(b, b"ab")));
        let message = r.unwrap_err();
        let text = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(text.contains("t: panicked on iteration"), "{text}");
    }
}
