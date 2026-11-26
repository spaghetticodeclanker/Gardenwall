//! GGUF model files come from the internet: the validator must refuse or
//! describe any input, never panic or allocate without bound.
#![no_main]

use std::io::Cursor;

use gardenwall_models::gguf::{GgufLimits, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = validate(Cursor::new(data), &GgufLimits::default());
});
