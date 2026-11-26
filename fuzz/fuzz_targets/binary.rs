//! Executables are the most hostile input Gardenwall reads; every analyzer
//! operation must survive them and keep its output bound.
#![no_main]

use gardenwall_binary::{MAX_OUTPUT, Op, run};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let ops = [
        Op::Info,
        Op::Functions {
            offset: 0,
            limit: 50,
            filter: None,
        },
        Op::Strings {
            min_len: 4,
            offset: 0,
            limit: 50,
            filter: None,
        },
        Op::Disasm {
            target: "entry".into(),
            count: 40,
        },
        Op::Hex {
            address: Some(0x1000),
            offset: 0,
            length: 64,
        },
    ];
    for op in &ops {
        if let Ok(out) = run(data, op) {
            assert!(out.len() <= MAX_OUTPUT + 200);
        }
    }
});
