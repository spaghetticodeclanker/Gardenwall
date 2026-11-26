//! Windows command-line and environment-block construction.
//!
//! Windows passes a single command-line string to a new process; the child's
//! C runtime splits it back into arguments. Incorrect quoting lets one
//! argument be interpreted as several, so quoting follows the documented
//! `CommandLineToArgvW` / MSVC rules exactly and is unit-tested.

use std::ffi::OsStr;

/// Quote one argument (not the program name) for the MSVC argument parser.
pub fn quote_arg(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty() || arg.contains([' ', '\t', '\n', '\u{0B}', '"']);
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Escape the preceding backslashes and the quote itself.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote must be doubled.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgvError {
    NonUnicode,
    /// The program path contains a quote, which cannot be represented in
    /// `argv[0]` (it has no escape mechanism).
    QuoteInProgram,
    Nul,
}

/// Build a full command line: quoted program path followed by arguments.
pub fn command_line(program: &OsStr, args: &[&OsStr]) -> Result<String, ArgvError> {
    let program = program.to_str().ok_or(ArgvError::NonUnicode)?;
    if program.contains('"') {
        return Err(ArgvError::QuoteInProgram);
    }
    if program.contains('\0') {
        return Err(ArgvError::Nul);
    }
    let mut line = String::new();
    line.push('"');
    line.push_str(program);
    line.push('"');
    for arg in args {
        let arg = arg.to_str().ok_or(ArgvError::NonUnicode)?;
        if arg.contains('\0') {
            return Err(ArgvError::Nul);
        }
        line.push(' ');
        quote_arg(arg, &mut line);
    }
    Ok(line)
}

/// Build a `CREATE_UNICODE_ENVIRONMENT` block: `NAME=VALUE\0` entries sorted
/// case-insensitively by name, terminated by an extra `\0`. Names and values
/// must already be validated (see [`crate::policy::valid_env`]).
pub fn environment_block(env: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = env.iter().collect();
    sorted.sort_by_key(|(name, _)| name.to_uppercase());
    let mut block = Vec::new();
    for (name, value) in sorted {
        block.extend(name.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(value.encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference splitter implementing the MSVC rules, used to check that
    /// quoting round-trips.
    fn split(line: &str) -> Vec<String> {
        let mut args = Vec::new();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        // argv[0]: quotes toggle, no escapes.
        let mut program = String::new();
        let mut in_quotes = false;
        while i < chars.len() && (in_quotes || !chars[i].is_whitespace()) {
            if chars[i] == '"' {
                in_quotes = !in_quotes;
            } else {
                program.push(chars[i]);
            }
            i += 1;
        }
        args.push(program);
        loop {
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let mut arg = String::new();
            let mut in_quotes = false;
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' {
                    let mut n = 0;
                    while i < chars.len() && chars[i] == '\\' {
                        n += 1;
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '"' {
                        arg.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                    } else {
                        arg.extend(std::iter::repeat_n('\\', n));
                    }
                    continue;
                }
                if c == '"' {
                    if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                        arg.push('"');
                        i += 2;
                        continue;
                    }
                    in_quotes = !in_quotes;
                    i += 1;
                    continue;
                }
                if !in_quotes && (c == ' ' || c == '\t') {
                    break;
                }
                arg.push(c);
                i += 1;
            }
            args.push(arg);
        }
        args
    }

    #[test]
    fn quoting_round_trips() {
        let cases = [
            "simple",
            "",
            "with space",
            "tab\there",
            "quote\"inside",
            "trailing\\",
            "trailing space\\",
            "\\\\server\\share\\",
            "back\\\\\"slash",
            "\"",
            "\\\"",
            "a\\\\\\b",
            "--flag=\"x y\"",
            "C:\\Program Files\\x\\",
            "unicode é ✓",
        ];
        let os: Vec<&OsStr> = cases.iter().map(OsStr::new).collect();
        let line = command_line(OsStr::new("C:\\Program Files\\app.exe"), &os).unwrap();
        let parsed = split(&line);
        assert_eq!(parsed[0], "C:\\Program Files\\app.exe");
        assert_eq!(&parsed[1..], &cases.map(String::from));
    }

    #[test]
    fn injection_attempts_stay_single_arguments() {
        let evil = "x\" --api-key leaked \"y";
        let line = command_line(OsStr::new("C:\\a.exe"), &[OsStr::new(evil)]).unwrap();
        assert_eq!(
            split(&line),
            vec!["C:\\a.exe".to_string(), evil.to_string()]
        );
    }

    #[test]
    fn rejects_unrepresentable_input() {
        assert_eq!(
            command_line(OsStr::new("C:\\a\"b.exe"), &[]),
            Err(ArgvError::QuoteInProgram)
        );
        assert_eq!(
            command_line(OsStr::new("C:\\a.exe"), &[OsStr::new("a\0b")]),
            Err(ArgvError::Nul)
        );
    }

    #[test]
    fn environment_block_is_sorted_and_terminated() {
        let block = environment_block(&[
            ("b".into(), "2".into()),
            ("A".into(), "1".into()),
            ("SystemRoot".into(), "C:\\Windows".into()),
        ]);
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "A=1\0b=2\0SystemRoot=C:\\Windows\0\0");
        assert_eq!(environment_block(&[]), vec![0, 0]);
    }
}
