# Fuzzing Gardenwall's parsers

Coverage-guided fuzz targets (cargo-fuzz and libFuzzer) for every parser of untrusted data. They complement the deterministic robustness tests (`gardenwall-fuzzkit`), which run on stable Rust on every push; these run longer and find new paths by coverage. CI runs each target for a few minutes every Monday (`.github/workflows/hygiene.yml`).

| Target | What it reads | Where it runs in the app |
|---|---|---|
| `gguf` | GGUF model headers | core, at intake |
| `template_audit` | chat templates | core, at intake |
| `html` | web pages | sandboxed parse worker |
| `documents` | PDF and DOCX | sandboxed parse worker |
| `binary` | executables (every analyzer operation) | sandboxed binary worker |
| `sse` | the model server's event stream | core |
| `server_log` | the model server's log lines | core |
| `egress` | model-written URLs and outbound text | core |
| `rag` | document text and questions | core |
| `script_step` | messages from the code-mode program helper | core |

## Running

Needs nightly Rust and cargo-fuzz (Linux or macOS; Windows works with the MSVC toolchain but is less tested):

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run html corpus/html -- -max_total_time=300
```

Targets without a committed corpus (`gguf`, `binary`) start from nothing, or from any local files: `cargo +nightly fuzz run binary /path/to/some/exes`. A crash is saved under `artifacts/<target>/`; replay it with `cargo +nightly fuzz run <target> <file>`, then add the input to that crate's `tests/robustness.rs` seeds so the stable suite keeps it fixed.

Seeds must stay harmless: no real or realistic-looking credentials (GitHub's push protection blocks them), and nothing personal.
