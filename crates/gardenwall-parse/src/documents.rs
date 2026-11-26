//! Text from DOCX and PDF files, for document chat (these
//! formats are read only inside the sandboxed worker).
//!
//! Both readers are deliberately small and bounded:
//! - **DOCX:** the `word/document.xml` part of the ZIP, with paragraphs,
//!   tabs, line breaks and table cells. Decompressed size is capped.
//! - **PDF:** text-showing operators in content streams (uncompressed or
//!   Flate-compressed), with `ToUnicode` maps applied to hex strings. It
//!   handles most PDFs made by office tools and TeX; scanned images and
//!   unusual font encodings yield little or no text, which is reported.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::decode_entities;

/// Largest decompressed DOCX document part.
const MAX_DOCX_XML: u64 = 64 << 20;
/// Largest single decompressed PDF stream, and all streams together.
const MAX_PDF_STREAM: u64 = 32 << 20;
const MAX_PDF_TOTAL: usize = 128 << 20;

/// Plain text of a DOCX file.
pub fn docx_to_text(bytes: &[u8], max_chars: usize) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "not a valid DOCX file".to_string())?;
    let mut part = archive
        .by_name("word/document.xml")
        .map_err(|_| "this DOCX has no document text".to_string())?;
    let mut xml = Vec::new();
    (&mut part)
        .take(MAX_DOCX_XML)
        .read_to_end(&mut xml)
        .map_err(|_| "the DOCX could not be decompressed".to_string())?;
    let xml = String::from_utf8_lossy(&xml);
    let mut out = String::new();
    let mut rest: &str = &xml;
    while let Some(lt) = rest.find('<') {
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match (name, tag.starts_with('/')) {
            ("w:t", false) if !tag.ends_with('/') => {
                let end = rest.find("</w:t>").unwrap_or(rest.len());
                out.push_str(&decode_entities(&rest[..end]));
                rest = &rest[end..];
            }
            ("w:tab", false) => out.push('\t'),
            ("w:br" | "w:cr", false) => out.push('\n'),
            ("w:p", true) => out.push('\n'),
            ("w:tc", true) => out.push_str(" | "),
            ("w:tr", true) => out.push('\n'),
            _ => {}
        }
        if out.len() >= max_chars {
            break;
        }
    }
    Ok(finish(out, max_chars))
}

fn finish(text: String, max_chars: usize) -> String {
    let mut lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    let mut s = lines.join("\n").trim().to_string();
    if s.len() > max_chars {
        let mut end = max_chars;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Every stream in the file, decompressed when Flate-encoded. Streams
/// with other filters (images and the like) are skipped.
fn pdf_streams(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut total = 0usize;
    let mut from = 0;
    while let Some(pos) = find(bytes, b"stream", from) {
        from = pos + 6;
        // "endstream" also contains "stream".
        if pos >= 3 && &bytes[pos - 3..pos] == b"end" {
            continue;
        }
        let mut start = pos + 6;
        if bytes.get(start) == Some(&b'\r') {
            start += 1;
        }
        if bytes.get(start) == Some(&b'\n') {
            start += 1;
        }
        let Some(end) = find(bytes, b"endstream", start) else {
            break;
        };
        from = end + 9;
        // The stream's dictionary precedes the keyword.
        let dict_start = rfind(&bytes[..pos], b"<<").unwrap_or(0);
        let dict = &bytes[dict_start..pos];
        let data = &bytes[start..end];
        let flate = find(dict, b"/FlateDecode", 0).is_some();
        let other_filter = find(dict, b"/Filter", 0).is_some() && !flate;
        let decoded = if flate {
            let mut d = Vec::new();
            let _ = flate2::read::ZlibDecoder::new(data)
                .take(MAX_PDF_STREAM)
                .read_to_end(&mut d);
            d
        } else if other_filter {
            continue;
        } else {
            data.to_vec()
        };
        total += decoded.len();
        if total > MAX_PDF_TOTAL {
            break;
        }
        out.push(decoded);
    }
    out
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

fn hex_value(s: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(s).ok()?;
    u32::from_str_radix(text, 16).ok()
}

fn utf16_hex(s: &[u8]) -> String {
    let units: Vec<u16> = s
        .chunks(4)
        .filter_map(|c| hex_value(c).map(|v| v as u16))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Code → text from every `ToUnicode` CMap in the file (merged; a
/// heuristic, since codes are really per font).
fn cmap(streams: &[Vec<u8>]) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for s in streams
        .iter()
        .filter(|s| find(s, b"begincmap", 0).is_some())
    {
        let text = String::from_utf8_lossy(s);
        let hexes = |line: &str| -> Vec<String> {
            line.split('<')
                .skip(1)
                .filter_map(|p| p.split('>').next().map(str::to_string))
                .collect()
        };
        let mut mode = "";
        for line in text.lines() {
            let line = line.trim();
            if line.ends_with("beginbfchar") {
                mode = "char";
                continue;
            }
            if line.ends_with("beginbfrange") {
                mode = "range";
                continue;
            }
            if line.starts_with("endbf") {
                mode = "";
                continue;
            }
            let h = hexes(line);
            match (mode, h.as_slice()) {
                ("char", [src, dst, ..]) => {
                    if let Some(code) = hex_value(src.as_bytes()) {
                        map.insert(code, utf16_hex(dst.as_bytes()));
                    }
                }
                ("range", [lo, hi, dst, ..]) => {
                    if let (Some(lo), Some(hi), Some(base)) = (
                        hex_value(lo.as_bytes()),
                        hex_value(hi.as_bytes()),
                        hex_value(dst.as_bytes()),
                    ) && hi >= lo
                        && hi - lo < 65_536
                    {
                        for (i, code) in (lo..=hi).enumerate() {
                            if let Some(c) = char::from_u32(base + i as u32) {
                                map.insert(code, c.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    map
}

/// Text from one content stream.
fn content_text(s: &[u8], map: &HashMap<u32, String>, out: &mut String) {
    let mut i = 0;
    let mut pending: Vec<String> = Vec::new();
    let mut operands: Vec<f32> = Vec::new();
    while i < s.len() {
        match s[i] {
            b'(' => {
                // A literal string with escapes and balanced parentheses.
                let mut depth = 1;
                let mut buf = Vec::new();
                i += 1;
                while i < s.len() && depth > 0 {
                    match s[i] {
                        b'\\' if i + 1 < s.len() => {
                            i += 1;
                            match s[i] {
                                b'n' => buf.push(b'\n'),
                                b'r' => {}
                                b't' => buf.push(b'\t'),
                                b'0'..=b'7' => {
                                    let mut v = 0u32;
                                    let mut n = 0;
                                    while n < 3 && i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                                        v = v * 8 + u32::from(s[i] - b'0');
                                        i += 1;
                                        n += 1;
                                    }
                                    i -= 1;
                                    buf.push(v as u8);
                                }
                                c => buf.push(c),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            buf.push(b'(');
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                buf.push(b')');
                            }
                        }
                        c => buf.push(c),
                    }
                    i += 1;
                }
                // Single-byte codes: Latin-1, or the CMap if it covers them.
                let text: String = buf
                    .iter()
                    .map(|b| {
                        map.get(&u32::from(*b))
                            .cloned()
                            .unwrap_or_else(|| (*b as char).to_string())
                    })
                    .collect();
                pending.push(text);
                continue;
            }
            b'<' if s.get(i + 1) != Some(&b'<') => {
                let end = s[i..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map_or(s.len(), |e| i + e);
                let hex: Vec<u8> = s[i + 1..end]
                    .iter()
                    .copied()
                    .filter(u8::is_ascii_hexdigit)
                    .collect();
                let text = if !map.is_empty() && hex.len().is_multiple_of(4) {
                    hex.chunks(4)
                        .filter_map(hex_value)
                        .map(|c| map.get(&c).cloned().unwrap_or_default())
                        .collect()
                } else {
                    hex.chunks(2)
                        .filter_map(hex_value)
                        .map(|c| {
                            map.get(&c)
                                .cloned()
                                .unwrap_or_else(|| char::from(c as u8).to_string())
                        })
                        .collect()
                };
                pending.push(text);
                i = end + 1;
                continue;
            }
            b'[' | b']' => {
                i += 1;
                continue;
            }
            c if c.is_ascii_alphabetic() || c == b'\'' || c == b'"' || c == b'*' => {
                let start = i;
                while i < s.len()
                    && (s[i].is_ascii_alphabetic() || matches!(s[i], b'\'' | b'"' | b'*'))
                {
                    i += 1;
                }
                let op = &s[start..i];
                match op {
                    b"Tj" | b"'" | b"\"" | b"TJ" => {
                        if op != b"Tj" && op != b"TJ" {
                            out.push('\n');
                        }
                        out.push_str(&pending.join(""));
                    }
                    b"T*" | b"ET" => out.push('\n'),
                    b"Td" | b"TD" => {
                        // A vertical move starts a new line.
                        if operands.len() >= 2 && operands[operands.len() - 1].abs() > 0.1 {
                            out.push('\n');
                        } else {
                            out.push(' ');
                        }
                    }
                    b"Tm" => out.push('\n'),
                    _ => {}
                }
                pending.clear();
                operands.clear();
                continue;
            }
            c if c.is_ascii_digit() || c == b'-' || c == b'.' => {
                let start = i;
                i += 1;
                while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'.') {
                    i += 1;
                }
                if let Some(v) = std::str::from_utf8(&s[start..i])
                    .ok()
                    .and_then(|t| t.parse::<f32>().ok())
                {
                    // A large negative kerning value inside TJ is a space.
                    if v < -200.0 && !pending.is_empty() {
                        pending.push(" ".into());
                    }
                    operands.push(v);
                }
                continue;
            }
            _ => i += 1,
        }
    }
}

/// Plain text of a PDF file.
pub fn pdf_to_text(bytes: &[u8], max_chars: usize) -> Result<String, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("not a PDF file".into());
    }
    if find(bytes, b"/Encrypt", 0).is_some() {
        return Err("this PDF is encrypted, so its text can't be read".into());
    }
    let streams = pdf_streams(bytes);
    let map = cmap(&streams);
    let mut out = String::new();
    for s in streams.iter().filter(|s| {
        find(s, b"begincmap", 0).is_none()
            && (find(s, b"Tj", 0).is_some() || find(s, b"TJ", 0).is_some())
    }) {
        content_text(s, &map, &mut out);
        out.push('\n');
        if out.len() >= max_chars {
            break;
        }
    }
    let text = finish(
        out.chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect(),
        max_chars,
    );
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 10 {
        return Err("no readable text was found in this PDF (it may be scanned images)".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn docx(document_xml: &str) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("word/document.xml", opts).unwrap();
            w.write_all(document_xml.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn docx_paragraphs_tabs_and_tables_are_read() {
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>Quarterly report</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Revenue &amp; costs</w:t><w:tab/><w:t>rose</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#;
        let text = docx_to_text(&docx(xml), 10_000).unwrap();
        assert!(
            text.starts_with("Quarterly report\nRevenue & costs\trose"),
            "{text}"
        );
        assert!(text.contains('A') && text.contains('B'));
        assert!(docx_to_text(b"not a zip", 100).is_err());
    }

    fn pdf_with(content: &[u8], compress: bool) -> Vec<u8> {
        let (dict, data) = if compress {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(content).unwrap();
            ("<< /Length 0 /Filter /FlateDecode >>", e.finish().unwrap())
        } else {
            ("<< /Length 0 >>", content.to_vec())
        };
        let mut pdf = b"%PDF-1.4\n1 0 obj\n".to_vec();
        pdf.extend_from_slice(dict.as_bytes());
        pdf.extend_from_slice(b"\nstream\n");
        pdf.extend_from_slice(&data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        pdf
    }

    #[test]
    fn pdf_text_operators_are_read_compressed_or_not() {
        let content = b"BT /F1 12 Tf 72 712 Td (Gardenwall keeps local models contained.) Tj 0 -14 Td [(Second) -300 (line \\(with parens\\))] TJ ET";
        for compress in [false, true] {
            let text = pdf_to_text(&pdf_with(content, compress), 10_000).unwrap();
            assert!(
                text.contains("Gardenwall keeps local models contained."),
                "{text}"
            );
            assert!(text.contains("Second line (with parens)"), "{text}");
        }
        assert!(pdf_to_text(b"hello", 100).is_err());
        assert!(pdf_to_text(&pdf_with(b"BT ET", false), 100).is_err());
    }

    #[test]
    fn tounicode_maps_decode_hex_strings() {
        let cmap = b"/CIDInit begincmap\n2 beginbfchar\n<0001> <0048>\n<0002> <0069>\nendbfchar\n1 beginbfrange\n<0010> <0012> <0061>\nendbfrange\nendcmap";
        let page =
            b"BT <000100020010001100120010001100120010001100120010001100120010001100120010> Tj ET";
        let mut pdf = pdf_with(page, true);
        pdf.extend_from_slice(&pdf_with(cmap, false)[9..]);
        let text = pdf_to_text(&pdf, 10_000).unwrap();
        assert!(text.starts_with("Hiabcabc"), "{text}");
    }
}
