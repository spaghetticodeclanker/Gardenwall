//! Executables are the most hostile input Gardenwall reads: every
//! operation must survive mutated headers and tables.

use gardenwall_binary::{Op, run};

fn ops() -> Vec<Op> {
    vec![
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
            filter: Some("a".into()),
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
    ]
}

#[test]
fn executables_survive_mutation() {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    // Headers and tables live at the start; mutating a small prefix keeps
    // each run fast while still exercising the parsers.
    let head = exe[..exe.len().min(16 * 1024)].to_vec();
    let ops = ops();
    gardenwall_fuzzkit::run("binary-head", &[head], 3000, |bytes| {
        for op in &ops {
            if let Ok(out) = run(bytes, op) {
                assert!(out.len() <= gardenwall_binary::MAX_OUTPUT + 200);
            }
        }
    });
    // A few whole-file mutations reach section data and import tables.
    gardenwall_fuzzkit::run("binary-full", &[exe], 60, |bytes| {
        let _ = run(bytes, &Op::Info);
        let _ = run(
            bytes,
            &Op::Disasm {
                target: "entry".into(),
                count: 200,
            },
        );
    });
}
