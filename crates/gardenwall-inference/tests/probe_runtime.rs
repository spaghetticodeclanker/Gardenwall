//! Reads a real runtime's option list in the sandbox (ADR-017 §2.1).
//!
//! Needs an installed llama.cpp build, so it only runs when
//! `GARDENWALL_TEST_SERVER` names a `llama-server` executable:
//!
//! ```text
//! GARDENWALL_TEST_SERVER=C:\path\to\llama-server.exe cargo test -p gardenwall-inference --test probe_runtime
//! ```

use std::path::PathBuf;

use gardenwall_inference::supervisor::{Isolation, probe_features};

fn server() -> Option<PathBuf> {
    std::env::var_os("GARDENWALL_TEST_SERVER").map(PathBuf::from)
}

#[tokio::test]
async fn a_real_runtime_reports_every_flag_gardenwall_needs() {
    let Some(exe) = server() else {
        eprintln!("skipped: GARDENWALL_TEST_SERVER is not set");
        return;
    };
    #[cfg(windows)]
    let isolation = Isolation::Sandboxed {
        relay_exe: PathBuf::new(),
        grant_journal_dir: None,
    };
    #[cfg(not(windows))]
    let isolation = Isolation::Unsandboxed;
    let started = std::time::Instant::now();
    let features = probe_features(&exe, &isolation).await.expect("probe");
    eprintln!("probed in {:?}", started.elapsed());
    assert!(features.missing_essentials().is_empty());
    assert!(features.can_fit());
}
