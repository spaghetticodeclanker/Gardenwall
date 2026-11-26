//! Document text and questions are untrusted: chunking and ranking must
//! not crash on odd Unicode or sizes.

use gardenwall_rag::{Bm25, chunk};

#[test]
fn chunking_and_search_survive_mutation() {
    gardenwall_fuzzkit::run_text(
        "rag",
        &[
            "# Title\n\nFirst paragraph about invoices.\n\n- item one\n- item two\n\n```rust\nfn main() {}\n```\n\nÜnïcödé 漢字 😀 text.",
        ],
        3000,
        |t| {
            let passages = chunk(t);
            let refs: Vec<&str> = passages.iter().map(String::as_str).collect();
            let _ = Bm25::new(&refs).search(t, 5);
        },
    );
}
