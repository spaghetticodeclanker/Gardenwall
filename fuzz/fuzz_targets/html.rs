//! Fetched pages are read by the sandboxed parse worker.
#![no_main]

use gardenwall_parse::html_to_text;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let out = html_to_text(&text, 10_000);
    assert!(out.chars().count() <= 10_100);
});
