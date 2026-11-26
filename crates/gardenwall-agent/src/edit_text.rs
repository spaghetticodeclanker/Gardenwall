//! Exact-then-fuzzy text replacement for the `edit` tool, ported from Pi
//! (`packages/coding-agent/src/core/tools/edit-diff.ts`, pi-mono commit
//! 682c7b9). Copyright (c) 2025 Mario Zechner, MIT License; see
//! THIRD_PARTY_NOTICES.md.
//!
//! All edits are matched against the same original (LF-normalized) text.
//! If any edit needs fuzzy matching (NFKC, smart quotes and dashes, special
//! spaces, trailing whitespace), matching runs on the normalized text and the
//! touched lines are rewritten from it, while untouched lines keep their
//! original bytes. Finally the replacements are mapped back onto the
//! original text so its line endings (CRLF, LF or CR) are preserved.
//!
//! Offsets are UTF-8 byte offsets (Pi uses UTF-16 units; results are equal).

use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    index: usize,
    len: usize,
    new_text: String,
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed = nfkc
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            spans.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

fn apply(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut result = content.to_string();
    for r in replacements.iter().rev() {
        let at = r.index - offset;
        result.replace_range(at..at + r.len, &r.new_text);
    }
    result
}

fn line_range(lines: &[(usize, usize)], r: &Replacement) -> Result<(usize, usize), String> {
    let start = lines
        .iter()
        .position(|&(s, e)| r.index >= s && r.index < e)
        .ok_or("Replacement range is outside the base content.")?;
    let end_offset = r.index + r.len;
    let mut end = start;
    while end < lines.len() && lines[end].1 < end_offset {
        end += 1;
    }
    if end >= lines.len() {
        return Err("Replacement range is outside the base content.".into());
    }
    Ok((start, end + 1))
}

fn widen_to_lines(
    original: &str,
    base: &str,
    replacements: &[Replacement],
) -> Result<Vec<Replacement>, String> {
    let orig_lines = line_spans(original);
    let base_lines = line_spans(base);
    if orig_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .into(),
        );
    }
    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.index);
    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for r in sorted {
        let (s, e) = line_range(&base_lines, &r)?;
        if let Some(last) = groups.last_mut()
            && s < last.1
        {
            last.1 = last.1.max(e);
            last.2.push(r);
            continue;
        }
        groups.push((s, e, vec![r]));
    }
    Ok(groups
        .into_iter()
        .map(|(s, e, reps)| {
            let g_start = base_lines[s].0;
            let g_end = base_lines[e - 1].1;
            let o_start = orig_lines[s].0;
            Replacement {
                index: o_start,
                len: orig_lines[e - 1].1 - o_start,
                new_text: apply(&base[g_start..g_end], &reps, g_start),
            }
        })
        .collect())
}

fn find(content: &str, old: &str) -> Option<(usize, usize, bool)> {
    if let Some(i) = content.find(old) {
        return Some((i, old.len(), false));
    }
    let fc = normalize_for_fuzzy_match(content);
    let fo = normalize_for_fuzzy_match(old);
    fc.find(&fo).map(|i| (i, fo.len(), true))
}

fn count(content: &str, old: &str) -> usize {
    let fc = normalize_for_fuzzy_match(content);
    let fo = normalize_for_fuzzy_match(old);
    if fo.is_empty() {
        return 0;
    }
    fc.matches(&fo).count()
}

/// Apply `edits` to LF-normalized `content`. Error messages are Pi's.
pub fn apply_edits_normalized(content: &str, edits: &[Edit], path: &str) -> Result<String, String> {
    let n = edits.len();
    let edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();
    for (i, e) in edits.iter().enumerate() {
        if e.old_text.is_empty() {
            return Err(if n == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{i}].oldText must not be empty in {path}.")
            });
        }
    }
    let fuzzy = edits
        .iter()
        .any(|e| matches!(find(content, &e.old_text), Some((_, _, true))));
    let base = if fuzzy {
        normalize_for_fuzzy_match(content)
    } else {
        content.to_string()
    };
    let mut matched: Vec<(usize, Replacement)> = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let Some((index, len, _)) = find(&base, &e.old_text) else {
            return Err(if n == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            });
        };
        let occurrences = count(&base, &e.old_text);
        if occurrences > 1 {
            return Err(if n == 1 {
                format!(
                    "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
                )
            } else {
                format!(
                    "Found {occurrences} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
                )
            });
        }
        matched.push((
            i,
            Replacement {
                index,
                len,
                new_text: e.new_text.clone(),
            },
        ));
    }
    matched.sort_by_key(|(_, r)| r.index);
    for w in matched.windows(2) {
        let (pi, p) = &w[0];
        let (ci, c) = &w[1];
        if p.index + p.len > c.index {
            return Err(format!(
                "edits[{pi}] and edits[{ci}] overlap in {path}. Merge them into one edit or target disjoint regions."
            ));
        }
    }
    let reps: Vec<Replacement> = matched.into_iter().map(|(_, r)| r).collect();
    let reps = if fuzzy {
        widen_to_lines(content, &base, &reps)?
    } else {
        reps
    };
    let new_content = apply(content, &reps, 0);
    if new_content == content {
        return Err(if n == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        });
    }
    Ok(new_content)
}

/// Line ending of the line containing `offset` (Pi's `getLineEndingAt`).
fn line_ending_at(content: &str, offset: usize) -> &'static str {
    let lf = content[offset..]
        .find('\n')
        .map(|i| i + offset)
        .or_else(|| content[..offset].rfind('\n'));
    match lf {
        None if content.contains('\r') => "\r",
        None => "\n",
        Some(i) if i > 0 && content.as_bytes()[i - 1] == b'\r' => "\r\n",
        Some(_) => "\n",
    }
}

/// Apply edits to the original text, preserving its line endings and a BOM.
pub fn apply_edits(original: &str, edits: &[Edit], path: &str) -> Result<String, String> {
    let (bom, body) = match original.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", original),
    };
    let normalized = normalize_to_lf(body);
    let new_normalized = apply_edits_normalized(&normalized, edits, path)?;
    if !body.contains('\r') {
        return Ok(format!("{bom}{new_normalized}"));
    }
    // Recover the replacements as one diff hunk between the normalized texts
    // and map them onto the original, like Pi's applyReplacementsToOriginal.
    let prefix = normalized
        .bytes()
        .zip(new_normalized.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut prefix = prefix;
    while !normalized.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = normalized.len().min(new_normalized.len()) - prefix;
    let mut suffix = normalized
        .bytes()
        .rev()
        .zip(new_normalized.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while !normalized.is_char_boundary(normalized.len() - suffix) {
        suffix -= 1;
    }
    let r = Replacement {
        index: prefix,
        len: normalized.len() - suffix - prefix,
        new_text: new_normalized[prefix..new_normalized.len() - suffix].to_string(),
    };
    Ok(format!("{bom}{}", replace_in_original(body, &r)))
}

fn replace_in_original(original: &str, r: &Replacement) -> String {
    // The n-th CRLF's LF sits at original offset p and normalized offset p - n.
    let crlfs: Vec<usize> = original.match_indices("\r\n").map(|(i, _)| i).collect();
    let to_original = |offset: usize| -> usize {
        let before = crlfs
            .iter()
            .enumerate()
            .take_while(|(n, p)| **p - *n < offset)
            .count();
        offset + before
    };
    let start = to_original(r.index);
    let end = to_original(r.index + r.len);
    let ending = line_ending_at(original, start);
    let new_text = if ending == "\n" {
        r.new_text.clone()
    } else {
        r.new_text.replace('\n', ending)
    };
    let mut out = String::with_capacity(original.len() + new_text.len());
    let push = |part: &str, out: &mut String| {
        if part.is_empty() {
            return;
        }
        if out.ends_with('\r') && part.starts_with('\n') {
            out.push('\n');
        }
        out.push_str(part);
    };
    push(&original[..start], &mut out);
    push(&new_text, &mut out);
    push(&original[end..], &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.into(),
            new_text: new.into(),
        }
    }

    #[test]
    fn exact_multi_edit_matches_against_the_original() {
        let out = apply_edits(
            "a = 1\nb = 2\nc = 3\n",
            &[e("a = 1", "a = 10"), e("c = 3", "c = 30")],
            "f",
        )
        .unwrap();
        assert_eq!(out, "a = 10\nb = 2\nc = 30\n");
    }

    #[test]
    fn errors_match_pi() {
        let content = "x\nx\ny\n";
        assert!(
            apply_edits(content, &[e("x", "z")], "f")
                .unwrap_err()
                .starts_with("Found 2 occurrences")
        );
        assert!(
            apply_edits(content, &[e("nope", "z")], "f")
                .unwrap_err()
                .starts_with("Could not find the exact text")
        );
        assert!(
            apply_edits(content, &[e("", "z")], "f")
                .unwrap_err()
                .contains("must not be empty")
        );
        assert!(
            apply_edits(content, &[e("y", "y")], "f")
                .unwrap_err()
                .starts_with("No changes made")
        );
        let err = apply_edits("abcdef", &[e("abc", "1"), e("bcd", "2")], "f").unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn fuzzy_matching_handles_quotes_dashes_and_trailing_spaces() {
        let content = "say(\u{201C}hi\u{201D})   \nnext \u{2014} line\nkeep   \n";
        let out = apply_edits(content, &[e("say(\"hi\")", "say(\"bye\")")], "f").unwrap();
        // The touched line is rewritten from the normalized text; untouched
        // lines keep their original bytes.
        assert_eq!(out, "say(\"bye\")\nnext \u{2014} line\nkeep   \n");
    }

    #[test]
    fn crlf_and_bom_are_preserved() {
        let content = "\u{feff}one\r\ntwo\r\nthree\r\n";
        let out = apply_edits(content, &[e("two", "2\nand a half")], "f").unwrap();
        assert_eq!(out, "\u{feff}one\r\n2\r\nand a half\r\nthree\r\n");
        let out = apply_edits("a\r\nb\r\n", &[e("a\nb", "c")], "f").unwrap();
        assert_eq!(out, "c\r\n");
    }
}
