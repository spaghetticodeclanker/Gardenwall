//! Chat templates are model-supplied text shown to the user after audit.
#![no_main]

use gardenwall_models::template::audit;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = audit(text);
    }
});
