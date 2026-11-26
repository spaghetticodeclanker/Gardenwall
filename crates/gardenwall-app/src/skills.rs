//! Skills for the agent.
//!
//! A skill is a folder with a `SKILL.md`: front matter with a `name` and a
//! `description`, then instructions. The agent sees every skill's name and
//! description in its system prompt and loads the instructions with the
//! `skill` tool when a task matches.
//!
//! - **Project skills:** `.gardenwall/skills/<name>/SKILL.md` or
//!   `.claude/skills/<name>/SKILL.md` in a granted folder, read through the
//!   workspace handle. Changing them asks first (persistence-risk rule).
//! - **Personal skills:** copied into Gardenwall's data folder from a
//!   folder the user picks in Settings, so they apply to every session.
//!
//! Skill text is instructions the user chose to give the agent, like
//! `AGENTS.md`. It never grants anything: every tool call still goes
//! through policy, approval and the sandbox.
//!
//! **Custom commands** (`.gardenwall/commands/<name>.md` or
//! `.claude/commands/<name>.md` in a granted folder) are prompt templates
//! the user runs by typing `/<name> [arguments]`. They are only text sent
//! as the user's request; like skills, they grant nothing, and changing
//! them asks first.

use std::path::{Path, PathBuf};

use gardenwall_agent::prompt::{Skill, SkillFile};
use gardenwall_agent::tools::Workspaces;
use gardenwall_policy::paths::RelPath;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

const MAX_SKILL_BYTES: u64 = 64 * 1024;
const MAX_SKILLS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_COMMAND_BYTES: u64 = 32 * 1024;
const MAX_COMMANDS: usize = 64;
/// Built-in agent commands; a custom command can't replace one.
const BUILT_IN_COMMANDS: &[&str] = &[
    "help", "plan", "init", "compact", "context", "clear", "resume", "undo", "review", "memory",
    "skills", "model",
];

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A custom command: `/<name>` sends `template`, with `$ARGUMENTS`
/// replaced by what follows the name.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserCommand {
    pub name: String,
    pub description: String,
    /// Shown after the name in suggestions, e.g. "[issue number]".
    pub argument_hint: Option<String>,
    pub template: String,
}

/// Parse a command file. Front matter is optional and may give a
/// `description` and an `argument-hint`; without a description the first
/// line of the prompt is used.
pub fn parse_command(name: &str, text: &str) -> Result<UserCommand, String> {
    if !valid_name(name) {
        return Err("a command name must be lowercase letters, digits and dashes".into());
    }
    if BUILT_IN_COMMANDS.contains(&name) {
        return Err(format!("/{name} is a built-in command"));
    }
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (front, body) = match text.strip_prefix("---\n").and_then(|rest| {
        let end = rest.find("\n---")?;
        Some((&rest[..end], rest[end + 4..].to_string()))
    }) {
        Some((front, body)) => (Some(front.to_string()), body),
        None => (None, text.clone()),
    };
    let field = |key: &str| {
        front.as_deref().and_then(|f| {
            f.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string())
            })
        })
    };
    let template = body.trim_start_matches(['\n', '-']).trim().to_string();
    if template.is_empty() {
        return Err("the command has no prompt".into());
    }
    let clean = |s: String| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .take(MAX_DESCRIPTION_CHARS)
            .collect()
    };
    let description = field("description")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            template
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .trim_start_matches('#')
                .trim()
                .to_string()
        });
    Ok(UserCommand {
        name: name.into(),
        description: clean(description),
        argument_hint: field("argument-hint").filter(|h| !h.is_empty()).map(clean),
        template,
    })
}

/// Custom commands in the granted folders. The first one found with a
/// name wins; files that don't parse are skipped.
pub fn project_commands(workspaces: &Workspaces) -> Vec<UserCommand> {
    let mut out: Vec<UserCommand> = Vec::new();
    for id in workspaces.ids() {
        for base in [".gardenwall/commands", ".claude/commands"] {
            let Ok(base) = RelPath::parse(base) else {
                continue;
            };
            let Ok(entries) = workspaces.list(id, Some(&base)) else {
                continue;
            };
            for (file, _) in entries.into_iter().filter(|(_, is_dir)| !*is_dir) {
                let Some(name) = file.strip_suffix(".md") else {
                    continue;
                };
                if out.iter().any(|c| c.name == name) || out.len() >= MAX_COMMANDS {
                    continue;
                }
                let Ok(path) = RelPath::parse(&format!("{base}/{file}")) else {
                    continue;
                };
                if let Ok(Some(bytes)) = workspaces.read_optional(id, &path, MAX_COMMAND_BYTES)
                    && let Ok(cmd) = parse_command(name, &String::from_utf8_lossy(&bytes))
                {
                    out.push(cmd);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse a `SKILL.md`: `---` front matter with `name:` and `description:`,
/// then the body. Names are lowercase letters, digits and dashes.
pub fn parse_skill(text: &str, source: &str) -> Result<Skill, String> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or("SKILL.md must start with front matter (---)")?;
    let end = rest
        .find("\n---")
        .ok_or("SKILL.md front matter is not closed (---)")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let field = |key: &str| {
        front.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string())
        })
    };
    let name = field("name").ok_or("SKILL.md has no name")?;
    if !valid_name(&name) {
        return Err("a skill name must be lowercase letters, digits and dashes".into());
    }
    let description = field("description").ok_or("SKILL.md has no description")?;
    if description.is_empty() {
        return Err("SKILL.md has an empty description".into());
    }
    Ok(Skill {
        name,
        description: description
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_DESCRIPTION_CHARS)
            .collect(),
        body: body.trim_start_matches(['\n', '-']).trim().to_string(),
        source: source.into(),
        files: vec![],
    })
}

/// Limits on the files bundled with one skill: count, size of each, total
/// size, and folder depth.
const MAX_SKILL_FILES: usize = 32;
const MAX_SKILL_FILE_BYTES: u64 = 64 * 1024;
const MAX_SKILL_FILES_TOTAL: usize = 256 * 1024;
const MAX_SKILL_DEPTH: usize = 3;

/// Keeps the text files among `(relative path, bytes)` within the limits:
/// SKILL.md itself, binaries (a NUL byte or invalid UTF-8), odd names and
/// anything past the budget are left out. Sorted by path.
fn bundle(mut found: Vec<(String, Vec<u8>)>) -> Vec<SkillFile> {
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    let mut total = 0usize;
    for (path, bytes) in found {
        if out.len() >= MAX_SKILL_FILES {
            break;
        }
        let depth = path.split('/').count();
        if path.eq_ignore_ascii_case("SKILL.md")
            || depth > MAX_SKILL_DEPTH
            || path.chars().any(char::is_control)
            || bytes.len() as u64 > MAX_SKILL_FILE_BYTES
            || bytes.contains(&0)
            || total + bytes.len() > MAX_SKILL_FILES_TOTAL
        {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        total += content.len();
        out.push(SkillFile { path, content });
    }
    out
}

/// Files under a folder on disk as `(relative path, bytes)`: regular files
/// only (links and junctions are never followed), within the depth limit.
fn read_folder(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.filter_map(Result::ok) {
            if out.len() >= MAX_SKILL_FILES * 4 {
                return;
            }
            let Some(name) = e.file_name().to_str().map(String::from) else {
                continue;
            };
            let path = e.path();
            // symlink_metadata: a link or junction is seen as itself.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if meta.file_type().is_symlink() {
                continue;
            } else if meta.is_dir() {
                if depth < MAX_SKILL_DEPTH && name != ".git" {
                    walk(&path, &rel, depth + 1, out);
                }
            } else if meta.is_file()
                && meta.len() <= MAX_SKILL_FILE_BYTES
                && let Ok(bytes) = std::fs::read(&path)
            {
                out.push((rel, bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", 1, &mut out);
    out
}

/// Files bundled with a project skill, read through the workspace handle.
fn project_skill_files(
    workspaces: &Workspaces,
    id: gardenwall_policy::tools::WorkspaceId,
    dir: &RelPath,
) -> Vec<SkillFile> {
    let prefix = format!("{dir}/");
    let found = workspaces
        .walk_files(id, Some(dir))
        .into_iter()
        .filter_map(|p| {
            let rel = p.to_string().strip_prefix(&prefix)?.to_string();
            let bytes = workspaces.read(id, &p, MAX_SKILL_FILE_BYTES).ok()?;
            Some((rel, bytes))
        })
        .take(MAX_SKILL_FILES * 4)
        .collect();
    bundle(found)
}

/// Skills in the granted folders.
pub fn project_skills(workspaces: &Workspaces) -> Vec<Skill> {
    let mut out = Vec::new();
    for id in workspaces.ids() {
        for base in [".gardenwall/skills", ".claude/skills"] {
            let Ok(base) = RelPath::parse(base) else {
                continue;
            };
            let Ok(entries) = workspaces.list(id, Some(&base)) else {
                continue;
            };
            for (name, _) in entries.into_iter().filter(|(_, is_dir)| *is_dir) {
                let Ok(path) = RelPath::parse(&format!("{base}/{name}/SKILL.md")) else {
                    continue;
                };
                if let Ok(Some(bytes)) = workspaces.read_optional(id, &path, MAX_SKILL_BYTES)
                    && let Ok(mut skill) = parse_skill(&String::from_utf8_lossy(&bytes), "project")
                    && let Ok(dir) = RelPath::parse(&format!("{base}/{name}"))
                {
                    skill.files = project_skill_files(workspaces, id, &dir);
                    out.push(skill);
                }
            }
        }
    }
    out
}

fn personal_dir(state: &AppState) -> PathBuf {
    state.paths.root.join("skills")
}

/// The user's personal skills (from Gardenwall's data folder).
pub fn personal_skills(state: &AppState) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(personal_dir(state)) else {
        return vec![];
    };
    let mut out = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let file = e.path().join("SKILL.md");
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        if meta.len() > MAX_SKILL_BYTES {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&file)
            && let Ok(mut skill) = parse_skill(&text, "personal")
        {
            skill.files = bundle(read_folder(&e.path()));
            out.push(skill);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every skill for a session: project skills win over personal ones with
/// the same name.
pub fn session_skills(state: &AppState, workspaces: &Workspaces) -> Vec<Skill> {
    let mut skills = project_skills(workspaces);
    for p in personal_skills(state) {
        if !skills.iter().any(|s| s.name == p.name) {
            skills.push(p);
        }
    }
    skills.truncate(MAX_SKILLS);
    skills
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    name: String,
    description: String,
    source: String,
    /// Text files bundled with it.
    files: usize,
}

fn view(skills: Vec<Skill>) -> Vec<SkillView> {
    skills
        .into_iter()
        .map(|s| SkillView {
            name: s.name,
            description: s.description,
            source: s.source,
            files: s.files.len(),
        })
        .collect()
}

#[tauri::command]
pub fn list_skills(state: State<'_, AppState>) -> CmdResult<Vec<SkillView>> {
    Ok(view(personal_skills(&state)))
}

/// Pick a skill folder (with a SKILL.md) and copy its SKILL.md into
/// Gardenwall's personal skills.
#[tauri::command]
pub async fn add_skill(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<SkillView>> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    let Some(dir) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(view(personal_skills(&state)));
    };
    let file = dir.join("SKILL.md");
    let meta = std::fs::metadata(&file).map_err(|_| "that folder has no SKILL.md".to_string())?;
    if meta.len() > MAX_SKILL_BYTES {
        return Err("SKILL.md is larger than 64 KB".into());
    }
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let skill = parse_skill(&text, "personal")?;
    // Bundled text files are copied with it, within the same limits used
    // when skills are read (links are never followed).
    let files = bundle(read_folder(&dir));
    install(&personal_dir(&state), &skill.name, &text, &files)?;
    Ok(view(personal_skills(&state)))
}

fn install(root: &Path, name: &str, text: &str, files: &[SkillFile]) -> CmdResult<()> {
    let dir = root.join(name);
    // A skill added again replaces the old copy, files included.
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("SKILL.md"), text).map_err(|e| e.to_string())?;
    for f in files {
        // Paths came from our own walk; parse them again so nothing can
        // land outside the skill's folder.
        let rel = RelPath::parse(&f.path).map_err(|e| e.to_string())?;
        let target = rel
            .components()
            .iter()
            .fold(dir.clone(), |p, part| p.join(part));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&target, &f.content).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn remove_skill(state: State<'_, AppState>, name: String) -> CmdResult<Vec<SkillView>> {
    if !valid_name(&name) {
        return Err("invalid skill name".into());
    }
    let dir = personal_dir(&state).join(&name);
    if dir.is_dir() {
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    Ok(view(personal_skills(&state)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_files_are_parsed_strictly() {
        let s = parse_skill(
            "---\nname: release-notes\ndescription: \"Write release notes from the git log\"\n---\n\n# Steps\n1. Read the log.\n",
            "project",
        )
        .unwrap();
        assert_eq!(s.name, "release-notes");
        assert_eq!(s.description, "Write release notes from the git log");
        assert_eq!(s.body, "# Steps\n1. Read the log.");
        for bad in [
            "no front matter",
            "---\nname: x\n",
            "---\ndescription: d\n---\nbody",
            "---\nname: Bad Name\ndescription: d\n---\n",
            "---\nname: ok\ndescription:\n---\n",
        ] {
            assert!(parse_skill(bad, "p").is_err(), "{bad}");
        }
        // Windows line endings and a byte-order mark are fine.
        assert!(
            parse_skill(
                "\u{feff}---\r\nname: a\r\ndescription: b\r\n---\r\nbody",
                "p"
            )
            .is_ok()
        );
    }

    #[test]
    fn command_files_are_parsed() {
        let c = parse_command(
            "fix-issue",
            "---\ndescription: Fix a GitHub issue\nargument-hint: [issue number]\n---\nFix issue $ARGUMENTS.\n",
        )
        .unwrap();
        assert_eq!(c.description, "Fix a GitHub issue");
        assert_eq!(c.argument_hint.as_deref(), Some("[issue number]"));
        assert_eq!(c.template, "Fix issue $ARGUMENTS.");
        // No front matter: the first line describes it.
        let c = parse_command("tidy", "# Tidy imports\r\nSort and group the imports.").unwrap();
        assert_eq!(c.description, "Tidy imports");
        assert_eq!(c.argument_hint, None);
        for (name, text) in [
            ("Bad", "x"),
            ("init", "shadow a built-in"),
            ("empty", "---\ndescription: d\n---\n"),
            ("blank", "  \n"),
        ] {
            assert!(parse_command(name, text).is_err(), "{name}");
        }
    }

    #[test]
    fn project_commands_are_found_in_granted_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let a = root.join(".gardenwall/commands");
        let b = root.join(".claude/commands");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("release.md"), "Draft release notes.").unwrap();
        // The same name in .claude is ignored; .gardenwall comes first.
        std::fs::write(b.join("release.md"), "Other release notes.").unwrap();
        std::fs::write(b.join("lint.md"), "Run the linter.").unwrap();
        std::fs::write(b.join("help.md"), "Not allowed.").unwrap();
        std::fs::write(b.join("notes.txt"), "Not a command.").unwrap();
        std::fs::create_dir_all(tmp.path().join("cp")).unwrap();
        let mut ws = Workspaces::new(tmp.path().join("cp"));
        ws.grant(&root).unwrap();
        let cmds = project_commands(&ws);
        let names: Vec<_> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["lint", "release"]);
        assert_eq!(cmds[1].template, "Draft release notes.");
    }

    #[test]
    fn project_skills_are_found_in_granted_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let skill = root.join(".gardenwall/skills/deploy");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: deploy\ndescription: Deploy the site\n---\nRun the script.",
        )
        .unwrap();
        let broken = root.join(".claude/skills/broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("SKILL.md"), "nothing").unwrap();
        let mut ws = Workspaces::new(tmp.path().join("cp"));
        std::fs::create_dir_all(tmp.path().join("cp")).unwrap();
        ws.grant(&root).unwrap();
        let skills = project_skills(&ws);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body, "Run the script.");
        assert!(skills[0].files.is_empty());
    }

    #[test]
    fn bundled_files_are_text_only_and_bounded() {
        let big = vec![b'a'; (MAX_SKILL_FILE_BYTES + 1) as usize];
        let files = bundle(vec![
            ("z.md".into(), b"last".to_vec()),
            ("SKILL.md".into(), b"itself".to_vec()),
            ("bin.dat".into(), vec![1, 0, 2]),
            ("bad.txt".into(), vec![0xff, 0xfe]),
            ("big.txt".into(), big),
            ("a/b/c/deep.txt".into(), b"too deep".to_vec()),
            ("odd\nname.txt".into(), b"x".to_vec()),
            ("a/ok.py".into(), b"print(1)".to_vec()),
        ]);
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a/ok.py", "z.md"]);
        let many: Vec<_> = (0..100)
            .map(|i| (format!("f{i:03}.txt"), b"x".to_vec()))
            .collect();
        assert_eq!(bundle(many).len(), MAX_SKILL_FILES);
        let heavy: Vec<_> = (0..10)
            .map(|i| (format!("h{i}.txt"), vec![b'x'; 60 * 1024]))
            .collect();
        let kept: usize = bundle(heavy).iter().map(|f| f.content.len()).sum();
        assert!(kept <= MAX_SKILL_FILES_TOTAL);
    }

    #[test]
    fn project_skill_files_come_through_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let skill = root.join(".claude/skills/deploy");
        std::fs::create_dir_all(skill.join("scripts")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: deploy\ndescription: Deploy\n---\nSee scripts/run.py.",
        )
        .unwrap();
        std::fs::write(skill.join("scripts/run.py"), "print('deploy')").unwrap();
        std::fs::create_dir_all(tmp.path().join("cp")).unwrap();
        let mut ws = Workspaces::new(tmp.path().join("cp"));
        ws.grant(&root).unwrap();
        let skills = project_skills(&ws);
        assert_eq!(skills[0].files.len(), 1);
        assert_eq!(skills[0].files[0].path, "scripts/run.py");
        assert_eq!(skills[0].files[0].content, "print('deploy')");
    }

    #[test]
    fn personal_skills_copy_their_files_and_never_follow_links() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("refs")).unwrap();
        let text = "---\nname: notes\ndescription: Notes\n---\nRead refs/a.md.";
        std::fs::write(src.join("SKILL.md"), text).unwrap();
        std::fs::write(src.join("refs/a.md"), "alpha").unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        // A link to a folder outside (a junction on Windows needs no
        // privilege; skip quietly where links can't be made).
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&outside, src.join("linked"));
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(src.join("linked"))
            .arg(&outside)
            .output();
        let files = bundle(read_folder(&src));
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["refs/a.md"]);
        let root = tmp.path().join("installed");
        install(&root, "notes", text, &files).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("notes/refs/a.md")).unwrap(),
            "alpha"
        );
        // A path that tries to leave the folder is refused.
        let evil = [SkillFile {
            path: "../escape.txt".into(),
            content: "x".into(),
        }];
        assert!(install(&root, "evil", text, &evil).is_err());
        assert!(!root.join("escape.txt").exists());
    }
}
