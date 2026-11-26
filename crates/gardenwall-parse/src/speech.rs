//! Read aloud with Windows' built-in voices (release plan feature 8,
//! decision F8.1). `Windows.Media.SpeechSynthesis` is a WinRT
//! component made for sandboxed apps, so it runs here, inside the worker's
//! AppContainer (no network, no files), like OCR does. Text comes in on
//! stdin, a WAV file goes out on stdout. Only Microsoft-signed voices
//! installed on the system can speak. Windows only; other systems report
//! that read aloud isn't available.

/// Longest text spoken in one call (characters); the core splits replies
/// into shorter pieces.
pub const MAX_SPEAK_CHARS: usize = 4_000;
/// Largest WAV written back (bytes).
pub const MAX_WAV_BYTES: usize = 48 << 20;
/// Speaking rates Windows accepts (1.0 is normal speed).
pub const RATE_RANGE: (f64, f64) = (0.5, 3.0);

/// One installed voice: an identifier for `--voice`, a name to show and a
/// language tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub language: String,
}

/// A voice identifier passed on the command line: Windows uses a registry
/// token path, so allow its characters and nothing that could be anything
/// else.
pub fn valid_voice_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 300
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "\\_-. ".contains(c))
}

/// The voices, one per line as `id<TAB>name<TAB>language`.
pub fn voices_text(voices: &[Voice]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    voices
        .iter()
        .map(|v| {
            format!(
                "{}\t{}\t{}\n",
                clean(&v.id),
                clean(&v.name),
                clean(&v.language)
            )
        })
        .collect()
}

#[cfg(windows)]
pub fn voices() -> Result<Vec<Voice>, String> {
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    let all = SpeechSynthesizer::AllVoices()
        .map_err(|e| format!("Windows voices aren't available: {}", e.message()))?;
    let mut out = Vec::new();
    for v in all {
        out.push(Voice {
            id: v.Id().map(|s| s.to_string()).unwrap_or_default(),
            name: v.DisplayName().map(|s| s.to_string()).unwrap_or_default(),
            language: v.Language().map(|s| s.to_string()).unwrap_or_default(),
        });
    }
    out.retain(|v| valid_voice_id(&v.id));
    Ok(out)
}

#[cfg(not(windows))]
pub fn voices() -> Result<Vec<Voice>, String> {
    Err("read aloud with system voices is available on Windows only".into())
}

/// Speak `text` with the voice `voice` (or Windows' default) at `rate`,
/// returning a WAV file.
#[cfg(windows)]
pub fn speak(text: &str, voice: Option<&str>, rate: f64) -> Result<Vec<u8>, String> {
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::DataReader;
    use windows::core::HSTRING;

    let fail = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
    let synth = SpeechSynthesizer::new().map_err(|e| fail("Windows voices aren't available", e))?;
    if let Some(id) = voice {
        let all = SpeechSynthesizer::AllVoices().map_err(|e| fail("voices", e))?;
        let chosen = all
            .into_iter()
            .find(|v| v.Id().is_ok_and(|s| s == id))
            .ok_or("that voice isn't installed any more")?;
        synth.SetVoice(&chosen).map_err(|e| fail("voice", e))?;
    }
    synth
        .Options()
        .and_then(|o| o.SetSpeakingRate(rate.clamp(RATE_RANGE.0, RATE_RANGE.1)))
        .map_err(|e| fail("speaking rate", e))?;
    let stream = synth
        .SynthesizeTextToStreamAsync(&HSTRING::from(text))
        .and_then(|op| op.join())
        .map_err(|e| fail("Windows couldn't speak this text", e))?;
    let size = stream.Size().map_err(|e| fail("speech", e))?;
    if size as usize > MAX_WAV_BYTES {
        return Err("the spoken audio is too long".into());
    }
    let input = stream.GetInputStreamAt(0).map_err(|e| fail("speech", e))?;
    let reader = DataReader::CreateDataReader(&input).map_err(|e| fail("speech", e))?;
    reader
        .LoadAsync(size as u32)
        .and_then(|op| op.join())
        .map_err(|e| fail("speech", e))?;
    let mut bytes = vec![0u8; size as usize];
    reader
        .ReadBytes(&mut bytes)
        .map_err(|e| fail("speech", e))?;
    if !bytes.starts_with(b"RIFF") {
        return Err("Windows returned audio in an unexpected format".into());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
pub fn speak(_text: &str, _voice: Option<&str>, _rate: f64) -> Result<Vec<u8>, String> {
    Err("read aloud with system voices is available on Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_ids_allow_windows_tokens_only() {
        assert!(valid_voice_id(
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices\Tokens\MSTTS_V110_enUS_DavidM"
        ));
        for bad in ["", "a\"b", "--format", "x;y", "a\nb", &"a".repeat(301)] {
            assert_eq!(valid_voice_id(bad), bad == "--format", "{bad}");
        }
    }

    #[test]
    fn voice_lines_cannot_be_split_by_names() {
        let text = voices_text(&[Voice {
            id: "a".into(),
            name: "Name\twith\ntabs".into(),
            language: "en-US".into(),
        }]);
        assert_eq!(text, "a\tName with tabs\ten-US\n");
    }

    /// Opt-in on Windows: `cargo test -p gardenwall-parse speaks -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "uses the installed Windows voices"]
    fn speaks_with_the_default_voice() {
        let wav = speak("Hello from Gardenwall.", None, 1.0).unwrap();
        assert!(wav.starts_with(b"RIFF") && wav.len() > 1000);
        assert!(!voices().unwrap().is_empty());
    }
}
