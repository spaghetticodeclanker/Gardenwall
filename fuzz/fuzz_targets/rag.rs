//! Document text and questions are untrusted: chunking and ranking.
#![no_main]

use gardenwall_rag::{Bm25, chunk};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let passages = chunk(&text);
    let refs: Vec<&str> = passages.iter().map(String::as_str).collect();
    let _ = Bm25::new(&refs).search(&text, 5);
});
