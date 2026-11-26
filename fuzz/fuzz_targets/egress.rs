//! URLs and outbound text come from the model: validation and the secret
//! scanner must hold up against anything.
#![no_main]

use gardenwall_egress::scan::find_secrets;
use gardenwall_egress::target::validate;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = validate(&text, &[]);
    let _ = find_secrets(&text);
});
