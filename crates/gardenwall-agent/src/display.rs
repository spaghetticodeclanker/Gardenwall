//! Paths as people and ordinary programs write them. On Windows,
//! `canonicalize` returns verbatim paths (`\\?\C:\x`, `\\?\UNC\server\x`),
//! which cmd.exe and batch files refuse and which look odd on a card.

use std::path::{Path, PathBuf};

/// The drive form of a verbatim drive path (`\\?\C:\x` → `C:\x`), which
/// cmd.exe and batch files can start in. Network and other paths are
/// returned as they are.
pub fn drive_form(path: &Path) -> PathBuf {
    match path.to_str().and_then(drive_rest) {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// A path as the user reads it: `C:\x` for `\\?\C:\x`, `\\server\x` for
/// `\\?\UNC\server\x`, anything else as it is.
pub fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = drive_rest(&text) {
        return rest.to_string();
    }
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text
}

/// `C:\x` from `\\?\C:\x`, or `None` when it isn't a verbatim drive path.
fn drive_rest(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_paths_are_shown_plainly() {
        assert_eq!(shown(Path::new(r"\\?\C:\Users\you")), r"C:\Users\you");
        assert_eq!(
            shown(Path::new(r"\\?\UNC\server\share\p")),
            r"\\server\share\p"
        );
        assert_eq!(shown(Path::new(r"C:\plain")), r"C:\plain");
        // A volume path keeps its prefix: without it, it names nothing.
        let volume = r"\\?\Volume{1234}\x";
        assert_eq!(shown(Path::new(volume)), volume);
        assert_eq!(drive_form(Path::new(r"\\?\D:\p")), PathBuf::from(r"D:\p"));
        assert_eq!(
            drive_form(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
        assert_eq!(drive_form(Path::new("/home/x")), PathBuf::from("/home/x"));
    }
}
