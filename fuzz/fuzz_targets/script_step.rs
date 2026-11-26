//! Lines from the code-mode helper are written by a program the model
//! wrote. The core's parser must reject or bound every one of them.
#![no_main]

use gardenwall_agent::agent::ScriptStep;
use gardenwall_agent::script_host::parse_step;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(ScriptStep::Call { name, args }) = parse_step(data) {
        assert!(!name.is_empty() && name.len() <= 64);
        assert!(args.is_object());
    }
});
