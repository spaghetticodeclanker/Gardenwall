//! The model server's log lines drive the placement display and the port.
#![no_main]

use gardenwall_inference::launch::parse_listening_port;
use gardenwall_inference::placement::Placement;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut placement = Placement::default();
    for line in text.lines() {
        placement.observe(line);
        let _ = parse_listening_port(line);
    }
});
