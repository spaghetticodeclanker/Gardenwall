//! The binary analysis worker: a file's bytes on stdin, a text report on
//! stdout. `--op info|strings|functions|disasm|hex` plus `key=value`
//! arguments.
//!
//! It runs inside the OS sandbox with no network, no files and no child
//! processes (ADR-012). The file is never executed: it is only read as
//! bytes. The trusted core treats the report as untrusted text.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::process::ExitCode;

use gardenwall_binary::{MAX_HEX, Op, run};

/// Largest file analyzed.
pub const MAX_INPUT_BYTES: u64 = 256 << 20;

fn parse_args(args: &[String]) -> Option<Op> {
    let (flag, op) = (args.first()?, args.get(1)?);
    if flag != "--op" {
        return None;
    }
    let mut kv = std::collections::HashMap::new();
    for a in &args[2..] {
        let (k, v) = a.split_once('=')?;
        kv.insert(k.to_string(), v.to_string());
    }
    let num = |k: &str, default: u64| -> Option<u64> {
        match kv.get(k) {
            None => Some(default),
            Some(v) => match v.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).ok(),
                None => v.parse().ok(),
            },
        }
    };
    let filter = kv.get("filter").cloned().filter(|f| !f.is_empty());
    Some(match op.as_str() {
        "info" => Op::Info,
        "strings" => Op::Strings {
            min_len: num("min", 6)? as usize,
            offset: num("offset", 0)? as usize,
            limit: num("limit", 200)? as usize,
            filter,
        },
        "functions" => Op::Functions {
            offset: num("offset", 0)? as usize,
            limit: num("limit", 200)? as usize,
            filter,
        },
        "disasm" => Op::Disasm {
            target: kv.get("target").cloned().unwrap_or_else(|| "entry".into()),
            count: num("count", 120)? as usize,
        },
        "hex" => Op::Hex {
            address: match kv.get("address") {
                Some(_) => Some(num("address", 0)?),
                None => None,
            },
            offset: num("offset", 0)?,
            length: num("length", 256)?.min(MAX_HEX as u64) as usize,
        },
        _ => return None,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(op) = parse_args(&args) else {
        return ExitCode::from(2);
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_end(&mut input)
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let text = match run(&input, &op) {
        Ok(t) => t,
        Err(e) => {
            // The reason goes to stderr; exit code 3 marks "can't describe".
            eprintln!("{e}");
            return ExitCode::from(3);
        }
    };
    let mut out = std::io::stdout().lock();
    if out
        .write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_are_strict() {
        assert_eq!(parse_args(&args(&["--op", "info"])), Some(Op::Info));
        assert_eq!(
            parse_args(&args(&["--op", "disasm", "target=0x401000", "count=50"])),
            Some(Op::Disasm {
                target: "0x401000".into(),
                count: 50
            })
        );
        assert_eq!(
            parse_args(&args(&["--op", "hex", "address=0x10", "length=99999"])),
            Some(Op::Hex {
                address: Some(16),
                offset: 0,
                length: MAX_HEX
            })
        );
        assert_eq!(parse_args(&args(&["--op", "run"])), None);
        assert_eq!(parse_args(&args(&["--op", "strings", "min=abc"])), None);
        assert_eq!(parse_args(&args(&["--op", "info", "noequals"])), None);
        assert_eq!(parse_args(&args(&["info"])), None);
    }
}
