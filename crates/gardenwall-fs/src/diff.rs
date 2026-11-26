//! Diffs for review and approval dialogs.

use serde::Serialize;
use similar::TextDiff;

/// Text larger than this is summarized instead of diffed.
pub const MAX_DIFF_INPUT_BYTES: usize = 2 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub change: Change,
    /// Unified diff text, or `None` for binary or very large files.
    pub unified: Option<String>,
    pub before_bytes: Option<u64>,
    pub after_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Created,
    Modified,
    Deleted,
    Unchanged,
}

fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.len() > MAX_DIFF_INPUT_BYTES || bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Describe the change from `before` to `after` (`None` = file absent).
pub fn diff_file(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> FileDiff {
    let change = match (before, after) {
        (None, None) => Change::Unchanged,
        (None, Some(_)) => Change::Created,
        (Some(_), None) => Change::Deleted,
        (Some(a), Some(b)) if a == b => Change::Unchanged,
        _ => Change::Modified,
    };
    let unified = match (before.map(as_text), after.map(as_text)) {
        (Some(None), _) | (_, Some(None)) => None,
        (b, a) => {
            let old = b.flatten().unwrap_or("");
            let new = a.flatten().unwrap_or("");
            if change == Change::Unchanged {
                None
            } else {
                Some(
                    TextDiff::from_lines(old, new)
                        .unified_diff()
                        .context_radius(3)
                        .header(&format!("a/{path}"), &format!("b/{path}"))
                        .to_string(),
                )
            }
        }
    };
    FileDiff {
        path: path.to_string(),
        change,
        unified,
        before_bytes: before.map(|b| b.len() as u64),
        after_bytes: after.map(|a| a.len() as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_changes_produce_unified_diffs() {
        let d = diff_file("a.txt", Some(b"one\ntwo\n"), Some(b"one\nthree\n"));
        assert_eq!(d.change, Change::Modified);
        let u = d.unified.unwrap();
        assert!(
            u.contains("-two") && u.contains("+three") && u.contains("a/a.txt"),
            "{u}"
        );
        assert_eq!(
            diff_file("n.txt", None, Some(b"x\n")).change,
            Change::Created
        );
        assert_eq!(
            diff_file("n.txt", Some(b"x\n"), None).change,
            Change::Deleted
        );
        assert_eq!(diff_file("n.txt", Some(b"x"), Some(b"x")).unified, None);
    }

    #[test]
    fn binary_and_huge_files_are_summarized() {
        let d = diff_file("img.png", Some(&[0, 1, 2]), Some(&[0, 1, 3]));
        assert_eq!(d.change, Change::Modified);
        assert_eq!(d.unified, None);
        assert_eq!(d.before_bytes, Some(3));
        let big = vec![b'a'; MAX_DIFF_INPUT_BYTES + 1];
        assert_eq!(diff_file("big.txt", Some(&big), Some(b"a")).unified, None);
    }
}
