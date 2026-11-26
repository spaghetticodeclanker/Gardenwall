//! The model server is sandboxed but untrusted: its event stream, fed in
//! uneven pieces as the network delivers it.
#![no_main]

use gardenwall_inference::client::SseParser;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut parser = SseParser::default();
    let mut out = Vec::new();
    let step = 1 + data.first().map_or(0, |b| usize::from(*b % 13));
    for piece in data.chunks(step) {
        if parser.push(piece, &mut out).is_err() {
            break;
        }
    }
});
