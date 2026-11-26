//! Gardenwall's pre-commit secret check
//! (ADR-011 §2.5).
//!
//! For the agent's commands only, the core points git at a read-only hooks
//! folder (through `GIT_CONFIG_*` in the session's environment) whose
//! `pre-commit` is the `gardenwall-gitscan` helper. Git runs it inside the
//! session's sandbox before every commit (amend, `commit -a`, scripts):
//!
//! 1. It writes the staged tree (`git write-tree`) and scans the staged diff
//!    with the same rules as the after-commit check ([`scan_diff`]).
//! 2. A clean commit goes on to the project's own pre-commit hook, if it has
//!    one, run in the same sandbox.
//! 3. A commit with a secret is refused with one [`MARKER`] line naming the
//!    tree, unless the core left an allow mark for exactly that tree in the
//!    hooks folder's `allow` subfolder (the sandbox can't write there).
//!
//! The core never trusts the marker's claims: it re-scans the named tree
//! itself before showing the "Commit anyway?" card, and an allow mark is
//! removed after the next command. Git that can't run the hook refuses the
//! commit, so a missing helper fails closed. The after-commit scan stays as
//! a second line.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gardenwall_policy::paths::{PathClass, RelPath, classify};
use serde::{Deserialize, Serialize};

/// The line the hook prints when it refuses a commit.
pub const MARKER: &str = "GARDENWALL-BLOCKED-COMMIT tree=";
/// The line `gardenwall-gitscan --scan` prints: a JSON [`Findings`].
pub const SCAN_MARKER: &str = "GARDENWALL-SCAN ";
/// The helper's name (staged as `pre-commit` in the hooks folder).
pub const HELPER: &str = "gardenwall-gitscan";
/// Where allow marks live inside the hooks folder.
pub const ALLOW_DIR: &str = "allow";
/// The empty tree, the base of a first commit, in the hash `like` uses
/// (SHA-1 or SHA-256 repositories).
pub fn empty_tree(like: &str) -> &'static str {
    if like.len() == 64 {
        "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    }
}

/// What a diff adds that looks secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Findings {
    /// The kinds found, described ("AWS access key").
    pub kinds: Vec<String>,
    /// How many files carry one.
    pub files: usize,
    /// Those files per top-level folder ("" for files at the top), most
    /// first, so a false alarm in a dependency folder is easy to see.
    pub folders: Vec<(String, usize)>,
}

impl Findings {
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// "password assignment in 738 files: 736 in vendor/, 2 in src/", or
    /// "… in 3 files, all in vendor/".
    pub fn summary(&self) -> String {
        let plural = if self.files == 1 { "" } else { "s" };
        let mut text = format!("{} in {} file{plural}", self.kinds.join(", "), self.files);
        let place = |folder: &str| match folder {
            "" => "at the top of the project".to_string(),
            f => format!("in {f}/"),
        };
        match self.folders.as_slice() {
            [] => {}
            [(only, _)] if self.files == 1 => text.push_str(&format!(", {}", place(only))),
            [(only, _)] => text.push_str(&format!(", all {}", place(only))),
            many => {
                let shown: Vec<String> = many
                    .iter()
                    .take(3)
                    .map(|(f, n)| format!("{n} {}", place(f)))
                    .collect();
                let more = if many.len() > 3 { ", …" } else { "" };
                text.push_str(&format!(": {}{more}", shown.join(", ")));
            }
        }
        text
    }

    /// Output of the helper, read by the core: anything the sandbox could
    /// have shaped is bounded and made visible before it reaches a card.
    pub fn from_scan_output(output: &str) -> Option<Self> {
        let json = output
            .lines()
            .find_map(|l| l.trim().strip_prefix(SCAN_MARKER))?;
        let mut f: Findings = serde_json::from_str(json).ok()?;
        let clean = |s: &str| {
            let s = gardenwall_policy::protect::reveal_invisible(s).0;
            let s: String = s.chars().filter(|c| !c.is_control()).take(60).collect();
            s
        };
        f.kinds.truncate(12);
        f.kinds = f.kinds.iter().map(|k| clean(k)).collect();
        f.folders.truncate(4);
        f.folders = f.folders.iter().map(|(d, n)| (clean(d), *n)).collect();
        Some(f)
    }
}

/// Files whose "password assignment" findings need a quoted value: source
/// code and documentation full of code samples, where `token: TokenStream`
/// is a type, not a secret. Shell scripts, `.env`, config and text files
/// keep the loose rule (`export API_KEY=…`).
pub fn is_code_path(path: &str) -> bool {
    const CODE: &[&str] = &[
        "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java",
        "kt", "kts", "scala", "groovy", "swift", "cs", "fs", "vb", "c", "h", "cc", "cpp", "cxx",
        "hpp", "hh", "m", "mm", "rb", "php", "dart", "lua", "ex", "exs", "erl", "hs", "ml", "clj",
        "zig", "nim", "vue", "svelte", "sol", "jl", "r", "md", "markdown", "mdx", "rst", "adoc",
        "html", "htm",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && CODE.iter().any(|c| c.eq_ignore_ascii_case(ext))
    })
}

/// [`scan_diff`] over a stream, a line at a time, so a diff of a whole
/// vendored dependency tree is never held in memory.
pub fn scan_diff_reader(mut diff: impl BufRead) -> Findings {
    let mut found = Findings::default();
    let mut folders: std::collections::HashMap<String, usize> = Default::default();
    let mut path = String::new();
    let mut code = false;
    let mut counted = false;
    let mut note = |kind: &str, counted: &mut bool, path: &str| {
        if !found.kinds.iter().any(|k| k == kind) {
            found.kinds.push(kind.to_string());
        }
        if !*counted {
            *counted = true;
            found.files += 1;
            let top = path.split_once('/').map_or("", |(top, _)| top);
            *folders.entry(top.to_string()).or_default() += 1;
        }
    };
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match diff.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some(p) = line.strip_prefix("+++ ") {
            counted = false;
            let p = p.trim().trim_matches('"');
            path = p.strip_prefix("b/").unwrap_or(p).to_string();
            code = is_code_path(&path);
            if let Ok(rel) = RelPath::parse(&path)
                && let PathClass::Secret(rule) = classify(&rel)
            {
                note(rule.describe(), &mut counted, &path);
            }
        } else if let Some(added) = line.strip_prefix('+') {
            let kinds = if code {
                gardenwall_egress::scan::find_secrets_in_code(added)
            } else {
                gardenwall_egress::scan::find_secrets(added)
            };
            for kind in kinds {
                note(kind, &mut counted, &path);
            }
        }
    }
    let mut folders: Vec<(String, usize)> = folders.into_iter().collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found.folders = folders;
    found
}

/// What a diff adds that looks secret: the kinds found (described) and how
/// many files carry one. A file whose name is a secret file counts too.
pub fn scan_diff(diff: &str) -> (Vec<String>, usize) {
    let f = scan_diff_reader(diff.as_bytes());
    (f.kinds, f.files)
}

/// Scan `git diff <flags> <args>` as it streams out of git. `None` when git
/// can't run or fails.
fn scan_git_diff(args: &[&str]) -> Option<Findings> {
    let mut child = Command::new("git")
        .arg("diff")
        .args(DIFF_FLAGS)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let found = scan_diff_reader(BufReader::with_capacity(1 << 16, child.stdout.take()?));
    child.wait().ok()?.success().then_some(found)
}

/// `gardenwall-gitscan --scan <from> <to>`, run by the core inside the
/// session's sandbox: scans what `<to>` adds over `<from>` (object ids
/// only) and prints one [`SCAN_MARKER`] line. Only that short line crosses
/// the shell's output pipe, however large the diff.
pub fn run_scan(from: &str, to: &str) -> i32 {
    if !is_object_id(from) || !is_object_id(to) {
        eprintln!("Gardenwall: --scan takes two object ids.");
        return 2;
    }
    match scan_git_diff(&[from, to]) {
        Some(found) => {
            let json = serde_json::to_string(&found).unwrap_or_default();
            println!("{SCAN_MARKER}{json}");
            0
        }
        None => {
            eprintln!("Gardenwall: git couldn't compare {from} and {to}.");
            1
        }
    }
}

/// A git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
pub fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The tree a refused commit names, from command output. Only the id is
/// taken; anything else on the line is the hook's (untrusted) text.
pub fn blocked_tree(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let id = l.trim().strip_prefix(MARKER)?.split_whitespace().next()?;
        is_object_id(id).then(|| id.to_string())
    })
}

/// Let exactly `tree` be committed once (until [`clear_allows`]).
pub fn allow(hooks: &Path, tree: &str) -> std::io::Result<()> {
    if !is_object_id(tree) {
        return Err(std::io::Error::other("not an object id"));
    }
    let dir = hooks.join(ALLOW_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(tree), b"")
}

/// Remove every allow mark.
pub fn clear_allows(hooks: &Path) {
    if let Ok(entries) = std::fs::read_dir(hooks.join(ALLOW_DIR)) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The git diff flags used for scanning (no external diff or text
/// conversion programs, no colour, added lines only matter).
pub const DIFF_FLAGS: &[&str] = &["--no-ext-diff", "--no-textconv", "--no-color", "-U0"];

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The hook itself (`gardenwall-gitscan`'s `main`). Returns the exit code.
pub fn run_hook() -> i32 {
    let Some(tree) = git(&["write-tree"]).map(|t| t.trim().to_string()) else {
        eprintln!("Gardenwall: couldn't read the staged changes, so the commit was stopped.");
        return 1;
    };
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let allowed = here
        .as_ref()
        .is_some_and(|h| is_object_id(&tree) && h.join(ALLOW_DIR).join(&tree).is_file());
    if !allowed {
        let Some(found) = scan_git_diff(&["--cached"]) else {
            eprintln!("Gardenwall: couldn't read the staged changes, so the commit was stopped.");
            return 1;
        };
        if !found.is_empty() {
            eprintln!("{MARKER}{tree}");
            eprintln!(
                "Gardenwall stopped this commit: it contains what looks like a secret ({}). Remove it (keep secrets in an ignored file such as .env), or wait for the user to allow it.",
                found.summary()
            );
            return 1;
        }
    }
    run_project_hook(here.as_deref())
}

/// The project's own pre-commit hook, run after the check
/// , in the same sandbox. A script can't run there (Git's shell
/// can't start inside the sandbox), so it is reported instead.
fn run_project_hook(here: Option<&Path>) -> i32 {
    let Some(hook) = project_hook() else {
        return 0;
    };
    let same = |a: &Path, b: &Path| {
        std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
    };
    if here.is_some_and(|h| hook.parent().is_some_and(|p| same(p, h))) {
        return 0;
    }
    let script = std::fs::read(&hook).is_ok_and(|b| b.starts_with(b"#!"));
    if script {
        eprintln!(
            "This project's pre-commit hook ({}) is a script, which can't run inside Gardenwall's sandbox, so the commit was stopped. Ask the user to commit from their own terminal.",
            hook.display()
        );
        return 1;
    }
    match Command::new(&hook)
        .args(std::env::args_os().skip(1))
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "This project's pre-commit hook couldn't start ({e}), so the commit was stopped."
            );
            1
        }
    }
}

/// `pre-commit` in the hooks folder the project itself configured
/// (`core.hooksPath` in its own config), or in `.git/hooks`.
fn project_hook() -> Option<PathBuf> {
    let configured = git(&["config", "--local", "--get", "core.hooksPath"])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let dir = match configured {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                PathBuf::from(git(&["rev-parse", "--show-toplevel"])?.trim()).join(p)
            }
        }
        None => PathBuf::from(git(&["rev-parse", "--git-common-dir"])?.trim()).join("hooks"),
    };
    let hook = dir.join("pre-commit");
    hook.is_file().then_some(hook)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_only_a_checked_tree() {
        let tree = "a".repeat(40);
        let out = format!("noise\n{MARKER}{tree} kinds=whatever\nmore");
        assert_eq!(blocked_tree(&out), Some(tree));
        assert_eq!(blocked_tree(&format!("{MARKER}../../etc")), None);
        assert_eq!(blocked_tree(&format!("{MARKER}{}", "A".repeat(40))), None);
        assert_eq!(blocked_tree("nothing here"), None);
    }

    #[test]
    fn allow_marks_are_exact_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let tree = "b".repeat(64);
        allow(dir.path(), &tree).unwrap();
        assert!(dir.path().join(ALLOW_DIR).join(&tree).is_file());
        assert!(allow(dir.path(), "../x").is_err());
        clear_allows(dir.path());
        assert!(!dir.path().join(ALLOW_DIR).join(&tree).exists());
    }

    #[test]
    fn types_in_code_are_not_secrets_but_literals_are() {
        let diff = concat!(
            "+++ b/vendor/syn/src/lib.rs\n+    token: TokenStream,\n",
            "+++ b/vendor/quote/README.md\n+fn f(secret: &str) {}\n",
            "+++ b/src/config.py\n+password = \"hunter2hunter2\"\n",
            "+++ b/deploy.sh\n+export API_KEY=abcdefgh123\n",
        );
        let found = scan_diff_reader(diff.as_bytes());
        assert_eq!(found.kinds, ["password assignment"]);
        assert_eq!(found.files, 2);
        assert_eq!(found.folders, [("".to_string(), 1), ("src".to_string(), 1)]);
        assert_eq!(
            found.summary(),
            "password assignment in 2 files: 1 at the top of the project, 1 in src/"
        );
        assert!(is_code_path("a/b.RS") && is_code_path("x.tsx") && !is_code_path(".env"));
        assert!(!is_code_path("run.sh") && !is_code_path("notes.txt") && !is_code_path(".rs"));
    }

    #[test]
    fn many_findings_in_one_folder_are_summed_up() {
        let diff: String = (0..5)
            .map(|i| format!("+++ b/vendor/c{i}/x.txt\n+token = abcdefgh123\n"))
            .collect();
        let found = scan_diff_reader(diff.as_bytes());
        assert_eq!(
            found.summary(),
            "password assignment in 5 files, all in vendor/"
        );
    }

    #[test]
    fn the_helpers_line_is_bounded_before_use() {
        let f = Findings {
            kinds: vec!["AWS access key".into(); 20],
            files: 3,
            folders: vec![("a\u{202E}b\nc".into(), 2), ("x".repeat(200), 1)],
        };
        let out = format!(
            "noise\n{SCAN_MARKER}{}\n",
            serde_json::to_string(&f).unwrap()
        );
        let read = Findings::from_scan_output(&out).unwrap();
        assert_eq!(read.kinds.len(), 12);
        assert!(
            !read.folders[0].0.contains(['\u{202E}', '\n']),
            "{:?}",
            read.folders
        );
        assert_eq!(read.folders[1].0.chars().count(), 60);
        assert_eq!(Findings::from_scan_output("no marker"), None);
    }

    #[test]
    fn a_staged_secret_and_a_secret_file_are_found() {
        let diff = "+++ b/settings.py\n+KEY = \"AKIAIOSFODNN7EXAMPLE\"\n+++ b/prod.env\n+X=1\n";
        let (kinds, files) = scan_diff(diff);
        assert_eq!(files, 2);
        assert!(kinds.iter().any(|k| k == "AWS access key"));
        assert_eq!(scan_diff("+++ b/a.py\n+TIMEOUT = 30\n"), (vec![], 0));
    }
}
