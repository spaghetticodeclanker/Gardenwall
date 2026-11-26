//! The parse worker: a page or document on stdin, plain text on stdout.
//! `--format html` (default), `--format pdf` or `--format docx`.
//! Also read aloud (release plan feature 8): `--voices` lists Windows'
//! voices, and `--speak [--voice <id>] [--rate <0.5-3.0>]` speaks the text
//! on stdin, writing a WAV file to stdout.
//!
//! It runs inside the OS sandbox with no network, no files and no child
//! processes (ADR-012). The trusted core passes it the bytes the egress
//! gateway fetched and treats whatever comes back as untrusted text.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::process::ExitCode;

use gardenwall_parse::documents::{docx_to_text, pdf_to_text};
use gardenwall_parse::ocr::pdf_ocr;
use gardenwall_parse::speech;
use gardenwall_parse::{MAX_INPUT_BYTES, MAX_OUTPUT_CHARS, html_to_text};

/// Most text kept from one document.
const MAX_DOCUMENT_CHARS: usize = 20 << 20;
/// Largest document read (documents can be larger than web pages).
const MAX_DOCUMENT_BYTES: usize = 64 << 20;

/// `--speak` arguments: the voice and the rate, checked.
fn speak_args(args: &[String]) -> Option<(Option<String>, f64)> {
    let mut voice = None;
    let mut rate = 1.0;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--voice" if speech::valid_voice_id(value) => voice = Some(value.clone()),
            "--rate" => {
                rate = value
                    .parse::<f64>()
                    .ok()
                    .filter(|r| (speech::RATE_RANGE.0..=speech::RATE_RANGE.1).contains(r))?
            }
            _ => return None,
        }
    }
    Some((voice, rate))
}

fn write_out(bytes: &[u8]) -> ExitCode {
    let mut out = std::io::stdout().lock();
    if out.write_all(bytes).and_then(|()| out.flush()).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn speech_main(args: &[String]) -> ExitCode {
    if args.len() == 1 && args[0] == "--voices" {
        return match speech::voices() {
            Ok(v) => write_out(speech::voices_text(&v).as_bytes()),
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(3)
            }
        };
    }
    let Some((voice, rate)) = speak_args(&args[1..]) else {
        return ExitCode::from(2);
    };
    let mut text = String::new();
    if std::io::stdin()
        .take((speech::MAX_SPEAK_CHARS * 4) as u64)
        .read_to_string(&mut text)
        .is_err()
        || text.chars().count() > speech::MAX_SPEAK_CHARS
    {
        return ExitCode::from(2);
    }
    match speech::speak(&text, voice.as_deref(), rate) {
        Ok(wav) => write_out(&wav),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(3)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| a == "--voices" || a == "--speak")
    {
        return speech_main(&args);
    }
    let format = match args.as_slice() {
        [] => "html",
        [flag, f] if flag == "--format" && matches!(f.as_str(), "html" | "pdf" | "docx") => {
            f.as_str()
        }
        _ => return ExitCode::from(2),
    };
    let limit = if format == "html" {
        MAX_INPUT_BYTES
    } else {
        MAX_DOCUMENT_BYTES
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(limit as u64)
        .read_to_end(&mut input)
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let text = match format {
        // A PDF without a text layer (a scan) is read by OCR instead.
        "pdf" => pdf_to_text(&input, MAX_DOCUMENT_CHARS).or_else(|e| {
            if e.starts_with("no readable text") {
                pdf_ocr(&input, MAX_DOCUMENT_CHARS)
            } else {
                Err(e)
            }
        }),
        "docx" => docx_to_text(&input, MAX_DOCUMENT_CHARS),
        _ => Ok(html_to_text(
            &String::from_utf8_lossy(&input),
            MAX_OUTPUT_CHARS,
        )),
    };
    let text = match text {
        Ok(t) => t,
        Err(e) => {
            // The reason goes to stderr; exit code 3 marks "unreadable".
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
