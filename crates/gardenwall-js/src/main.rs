//! The sandboxed helper that runs one code-mode program.
//!
//! Lines of JSON on stdin and stdout (see `gardenwall_js::FromHelper`):
//! the core sends the program, the helper sends tool calls and reads each
//! answer, then sends how the program ended and exits. It takes no
//! arguments and opens no files.

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read, Write};

use gardenwall_js::{FromHelper, MAX_LINE_BYTES, Outcome, Reply, Start, Transport, run};

/// Read one line, refusing one that is too long. `None` at end of input.
fn read_line(input: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let read = input
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .ok()?;
    if read == 0 || line.len() > MAX_LINE_BYTES {
        return None;
    }
    Some(line)
}

fn send(message: &FromHelper) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    let mut out = std::io::stdout().lock();
    out.write_all(&line)
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())
}

struct Stdio {
    input: BufReader<std::io::Stdin>,
}

impl Transport for Stdio {
    fn call(&mut self, name: &str, args: serde_json::Value) -> Result<(String, bool), String> {
        const GONE: &str = "The app stopped answering.";
        send(&FromHelper::Call {
            name: name.to_string(),
            args,
        })
        .map_err(|_| GONE.to_string())?;
        let line = read_line(&mut self.input).ok_or(GONE)?;
        let reply: Reply = serde_json::from_slice(&line).map_err(|_| GONE.to_string())?;
        Ok((reply.result, reply.error))
    }
}

fn main() -> std::process::ExitCode {
    if std::env::args_os().len() > 1 {
        return std::process::ExitCode::from(2);
    }
    let mut input = BufReader::new(std::io::stdin());
    let Some(start) = read_line(&mut input).and_then(|l| serde_json::from_slice::<Start>(&l).ok())
    else {
        return std::process::ExitCode::from(2);
    };
    let message = match run(&start.run, &start.tools, Box::new(Stdio { input })) {
        Outcome::Done { value, logs } => FromHelper::Done { value, logs },
        Outcome::Failed { message, logs } => FromHelper::Failed { message, logs },
    };
    match send(&message) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::from(1),
    }
}
