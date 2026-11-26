//! PDF and DOCX files are read by the sandboxed parse worker.
#![no_main]

use gardenwall_parse::documents::{docx_to_text, pdf_to_text};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for text in [docx_to_text(data, 10_000), pdf_to_text(data, 10_000)]
        .into_iter()
        .flatten()
    {
        assert!(text.chars().count() <= 10_100);
    }
});
