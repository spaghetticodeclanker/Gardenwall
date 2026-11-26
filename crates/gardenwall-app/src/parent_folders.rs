//! "Folders above the project" (ADR-025).
//!
//! Some tools (Ruby, Gradle and Java's temporary files, Git creating a
//! repository) check every folder from the top of the drive down to the
//! project, and the sandbox can't see those folders. When the user turns
//! this on, for one project or for every project, each folder above the
//! project lets the agent's sandbox see the names inside it: that folder
//! only, nothing in it can be opened, read or changed, nothing deeper can
//! be listed. Folders this user owns are changed by Gardenwall while the
//! setting is on (journaled, removed when it is turned off or the app
//! closes); folders only an administrator can change (`C:\`, `C:\Users`)
//! need a one-time step the user runs: a script shown in full, to paste
//! into PowerShell run as administrator.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Whether the setting is on for sessions in `project`.
pub fn enabled(agent: &crate::state::AgentSettings, project: Option<&str>) -> bool {
    agent.parent_folders
        || project.is_some_and(|p| agent.project_parent_folders.iter().any(|x| x == p))
}

/// Every folder above `folders`, from each drive root down, without
/// repeats (the folders themselves are left out).
pub fn ancestors(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for f in folders {
        let plain = PathBuf::from(shown(f));
        let mut chain: Vec<PathBuf> = plain.ancestors().skip(1).map(Path::to_path_buf).collect();
        chain.reverse();
        for a in chain {
            if !out.iter().any(|o| same(o, &a)) {
                out.push(a);
            }
        }
    }
    out
}

fn same(a: &Path, b: &Path) -> bool {
    shown(a)
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(shown(b).trim_end_matches('\\'))
}

use gardenwall_agent::display::shown;

/// One folder above a project, as the panel shows it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub path: String,
    /// What of it the agent could see (names only).
    pub what: String,
    /// Only an administrator can change it (`C:\`, `C:\Users`).
    pub admin: bool,
    /// The sandbox can list it now.
    pub done: bool,
    /// Why Gardenwall won't do it, if so.
    pub problem: Option<String>,
}

/// What the agent could see in `path`, in plain words.
fn what(path: &Path) -> String {
    let p = shown(path);
    let trimmed = p.trim_end_matches('\\');
    let profile = std::env::var_os("USERPROFILE")
        .map(|h| shown(Path::new(&h)))
        .unwrap_or_default();
    if path.parent().is_none() || trimmed.len() <= 2 {
        "The names of the folders and files at the top of this drive (Program Files, Users, Windows and any of yours)".into()
    } else if !profile.is_empty() && trimmed.eq_ignore_ascii_case(profile.trim_end_matches('\\')) {
        "The names in your profile folder (Documents, Downloads, and whether folders like .ssh or .aws exist)".into()
    } else if path
        .parent()
        .is_some_and(|q| q.parent().is_none() || shown(q).trim_end_matches('\\').len() <= 2)
        && path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("Users"))
    {
        "The account names on this PC".into()
    } else {
        "The names of the files and folders in it (for example your other projects)".into()
    }
}

/// Whether Gardenwall may change `path` itself: it is this user's (owned, or
/// their profile folder or inside it) and they can
/// change its permissions (opening it for WRITE_DAC only, which changes
/// nothing). Windows' own folders (`C:\`, `C:\Users`) are owned by SYSTEM
/// or TrustedInstaller and always go through the administrator step, even
/// on a PC where this user could change them.
#[cfg(windows)]
fn can_change(path: &Path) -> bool {
    // This user's: they own it, or it is their profile folder or inside it
    // (Windows makes SYSTEM the owner of profile folders).
    let in_profile = std::env::var_os("USERPROFILE").is_some_and(|home| {
        let home = shown(Path::new(&home))
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        let p = shown(path).trim_end_matches('\\').to_ascii_lowercase();
        !home.is_empty() && (p == home || p.starts_with(&format!("{home}\\")))
    });
    if !in_profile && !gardenwall_sandbox::owned_by_current_user(path) {
        return false;
    }
    use std::os::windows::fs::OpenOptionsExt;
    const WRITE_DAC: u32 = 0x0004_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .access_mode(WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .is_ok()
}

#[cfg(not(windows))]
fn can_change(_path: &Path) -> bool {
    false
}

#[cfg(windows)]
fn listable(path: &Path) -> bool {
    gardenwall_sandbox::parent_listable(gardenwall_agent::sandbox_shell::AGENT_PROFILE, path)
}

#[cfg(not(windows))]
fn listable(_path: &Path) -> bool {
    false
}

/// The folders above `folders` and where each stands.
pub fn plan(folders: &[PathBuf]) -> Vec<Folder> {
    let problem_in_project = folders
        .iter()
        .find_map(|f| crate::toolchains::credential_folder(f));
    ancestors(folders)
        .into_iter()
        .map(|a| Folder {
            path: shown(&a),
            what: what(&a),
            admin: !can_change(&a),
            done: listable(&a),
            problem: crate::toolchains::credential_folder(&a)
                .or_else(|| problem_in_project.clone())
                .map(|_| {
                    "This project is inside a folder that holds credentials or private settings, so Gardenwall doesn't open the folders above it.".to_string()
                }),
        })
        .collect()
}

/// Whether a session in `project` on `folders` gets what the setting gives:
/// it is on, and every folder above is ready.
pub fn session_ready(state: &AppState, project: Option<&str>, folders: &[PathBuf]) -> bool {
    enabled(&state.settings().agent, project) && ready(folders)
}

/// Whether every folder above `folders` lets the sandbox list it now (the
/// tools that need it will work in a session started now).
pub fn ready(folders: &[PathBuf]) -> bool {
    let p = plan(folders);
    !p.is_empty() && p.iter().all(|f| f.done && f.problem.is_none())
}

/// Hold the folders above `folders` that this user owns, for a session
/// starting now (the setting is on for its project).
#[cfg(windows)]
pub fn hold_for(state: &AppState, folders: &[PathBuf]) {
    let Ok(mut holds) = state.parent_holds.lock() else {
        return;
    };
    for f in plan(folders) {
        if f.admin || f.problem.is_some() {
            continue;
        }
        let path = PathBuf::from(&f.path);
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if holds.iter().any(|h| h.path() == canonical) {
            continue;
        }
        if let Ok(hold) = gardenwall_sandbox::ParentListHold::new(
            gardenwall_agent::sandbox_shell::AGENT_PROFILE,
            &path,
            Some(&state.paths.grants),
        ) {
            holds.push(hold);
        }
    }
}

#[cfg(not(windows))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hold_for(_state: &AppState, _folders: &[PathBuf]) {}

/// Every project's folders, checked loosely (paths only).
fn project_folders(state: &AppState) -> Vec<(String, Vec<PathBuf>)> {
    state
        .conversations
        .lock()
        .ok()
        .and_then(|s| s.list_projects().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p.folders.iter().map(PathBuf::from).collect()))
        .collect()
}

/// Keep exactly the folders the setting needs now held: those above every
/// project it is on for (this user's folders only), and let the rest go.
#[cfg(windows)]
pub fn warm(state: &AppState) {
    let agent = state.settings().agent;
    let mut wanted: Vec<PathBuf> = Vec::new();
    for (id, folders) in project_folders(state) {
        if enabled(&agent, Some(&id)) {
            for f in plan(&folders) {
                if !f.admin && f.problem.is_none() {
                    let p = PathBuf::from(&f.path);
                    wanted.push(std::fs::canonicalize(&p).unwrap_or(p));
                }
            }
        }
    }
    if let Ok(mut holds) = state.parent_holds.lock() {
        holds.retain(|h| wanted.iter().any(|w| w == h.path()));
    }
    let paths: Vec<PathBuf> = wanted;
    // Adding uses the same path as a session start.
    let mut missing: Vec<PathBuf> = Vec::new();
    if let Ok(holds) = state.parent_holds.lock() {
        for p in paths {
            if !holds.iter().any(|h| h.path() == p) && !missing.contains(&p) {
                missing.push(p);
            }
        }
    }
    if let Ok(mut holds) = state.parent_holds.lock() {
        for p in missing {
            if let Ok(hold) = gardenwall_sandbox::ParentListHold::new(
                gardenwall_agent::sandbox_shell::AGENT_PROFILE,
                &p,
                Some(&state.paths.grants),
            ) {
                holds.push(hold);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn warm(_state: &AppState) {}

/// The administrator step, as one block the user pastes into PowerShell run
/// as administrator. It is shown, never saved
/// as a script file: a file in Gardenwall's data folder could be changed by
/// any program running as the user between being shown and being run with
/// administrator rights. `& { ... }` makes it one unit: the first error
/// stops it, and it leaves nothing behind in the PowerShell session.
const SCRIPT: &str = r#"& {
# Gardenwall, "Folders above the project" (ADR-025). Lets Gardenwall's agent
# sandbox, and no other app, see the names inside each folder below, for that
# folder only: nothing inside can be opened, read or changed, and nothing
# deeper can be listed. SetFileSecurity changes only the folder named.
$ErrorActionPreference = 'Stop'
$paths = @(@PATHS@)
$remove = $@REMOVE@
$sid = New-Object System.Security.Principal.SecurityIdentifier '@SID@'
# List the folder (0x1), read its attributes (0x80), synchronize (0x100000).
$rights = 0x100081
if (-not ('GardenwallAcl.Native' -as [type])) {
Add-Type -Namespace GardenwallAcl -Name Native -MemberDefinition @'
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool SetFileSecurityW(string path, int info, byte[] sd);
'@
}
foreach ($p in $paths) {
    $item = Get-Item -LiteralPath $p -Force
    $sddl = (Get-Acl -LiteralPath $p).GetSecurityDescriptorSddlForm('Access')
    $sd = New-Object System.Security.AccessControl.CommonSecurityDescriptor($true, $false, $sddl)
    if ($remove) {
        [void]$sd.DiscretionaryAcl.RemoveAccess('Allow', $sid, 0x1F01FF, 'None', 'None')
    } else {
        $sd.DiscretionaryAcl.AddAccess('Allow', $sid, $rights, 'None', 'None')
    }
    $bytes = New-Object byte[] $sd.BinaryLength
    $sd.GetBinaryForm($bytes, 0)
    # Keep the folder's "auto-inherited" flag (SetFileSecurity drops it
    # unless the matching request flag is set too).
    if ($sddl -match '^D:[A-Z]*AI') {
        $raw = New-Object System.Security.AccessControl.RawSecurityDescriptor($bytes, 0)
        $raw.SetFlags($raw.ControlFlags -bor [System.Security.AccessControl.ControlFlags]::DiscretionaryAclAutoInherited -bor [System.Security.AccessControl.ControlFlags]::DiscretionaryAclAutoInheritRequired)
        $bytes = New-Object byte[] $raw.BinaryLength
        $raw.GetBinaryForm($bytes, 0)
    }
    if (-not [GardenwallAcl.Native]::SetFileSecurityW($item.FullName, 4, $bytes)) {
        throw "Changing $p failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())). Run PowerShell as administrator."
    }
    Write-Output ("{0}: {1}" -f $p, $(if ($remove) { 'removed' } else { 'done (names only, this folder only)' }))
}
}"#;

/// The script file earlier versions saved, removed so no stale copy is left
/// to be run.
const OLD_SCRIPT: &str = "parent-folders";

/// The agent sandbox's container SID, checked to be one (`S-1-15-2-…`).
fn sandbox_sid() -> Option<String> {
    #[cfg(windows)]
    let sid = gardenwall_sandbox::profile_sid(gardenwall_agent::sandbox_shell::AGENT_PROFILE)?;
    #[cfg(not(windows))]
    let sid = String::new();
    let ok =
        sid.starts_with("S-1-15-2-") && sid[9..].chars().all(|c| c.is_ascii_digit() || c == '-');
    ok.then_some(sid)
}

/// `text` inside PowerShell single quotes. PowerShell treats the curly
/// single quotes as quotes too, so each kind is doubled (as its own
/// `EscapeSingleQuotedStringContent` does).
fn ps_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The block to paste for `paths` (taking the step back when `remove`).
fn pasted(sid: &str, paths: &[&str], remove: bool) -> String {
    let list: Vec<String> = paths.iter().map(|p| ps_quoted(p)).collect();
    // The paths go in last, so nothing in them is taken for a placeholder.
    SCRIPT
        .replace("@SID@", sid)
        .replace("@REMOVE@", if remove { "true" } else { "false" })
        .replace("@PATHS@", &list.join(","))
        .replace('\n', "\r\n")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// On for every project (Settings → Agent).
    pub global: bool,
    /// On for this project by its own switch.
    pub project_on: bool,
    /// On for this project either way (or, without a project, globally).
    pub on: bool,
    /// The folders above this project (or above every project, for the
    /// global panel), and where each stands.
    pub folders: Vec<Folder>,
    /// The administrator step for folders still missing it, if any: the
    /// whole script, to paste into PowerShell run as administrator.
    pub admin_command: Option<String>,
    /// How to take the administrator step back, for folders that have it.
    pub remove_command: Option<String>,
    /// Every folder above lets the sandbox list it now.
    pub ready: bool,
}

/// The panel's data, for one project or (without one) for every project.
#[tauri::command]
pub fn parent_folders_status(
    state: State<'_, AppState>,
    project: Option<String>,
) -> CmdResult<Status> {
    let agent = state.saved_settings().agent;
    let projects = project_folders(&state);
    let folders: Vec<PathBuf> = match &project {
        Some(id) => projects
            .iter()
            .find(|(p, _)| p == id)
            .map(|(_, f)| f.clone())
            .ok_or("that project no longer exists")?,
        None => projects.iter().flat_map(|(_, f)| f.clone()).collect(),
    };
    let list = plan(&folders);
    let _ = std::fs::remove_dir_all(state.paths.root.join(OLD_SCRIPT));
    let sid = sandbox_sid();
    let need: Vec<&str> = list
        .iter()
        .filter(|f| f.admin && !f.done && f.problem.is_none())
        .map(|f| f.path.as_str())
        .collect();
    let have: Vec<&str> = list
        .iter()
        .filter(|f| f.admin && f.done)
        .map(|f| f.path.as_str())
        .collect();
    let project_on = project
        .as_ref()
        .is_some_and(|p| agent.project_parent_folders.iter().any(|x| x == p));
    Ok(Status {
        global: agent.parent_folders,
        project_on,
        on: agent.parent_folders || project_on,
        admin_command: sid
            .as_deref()
            .filter(|_| !need.is_empty())
            .map(|s| pasted(s, &need, false)),
        remove_command: sid
            .as_deref()
            .filter(|_| !have.is_empty())
            .map(|s| pasted(s, &have, true)),
        ready: !list.is_empty() && list.iter().all(|f| f.done && f.problem.is_none()),
        folders: list,
    })
}

/// Turn the setting on or off for one project, or (without one) for every
/// project. Open sessions are offered a sandbox restart.
#[tauri::command]
pub fn set_parent_folders(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    on: bool,
) -> CmdResult<()> {
    if state.saved_settings().lockdown && on {
        return Err(
            "Lockdown mode is on: the agent's sandbox stays as narrow as it can be.".into(),
        );
    }
    let mut agent = state.saved_settings().agent;
    match &project {
        Some(id) => {
            if !project_folders(&state).iter().any(|(p, _)| p == id) {
                return Err("that project no longer exists".into());
            }
            agent.project_parent_folders.retain(|p| p != id);
            if on {
                agent.project_parent_folders.push(id.clone());
            }
        }
        None => agent.parent_folders = on,
    }
    agent.validate()?;
    state
        .update_settings(|s| s.agent = agent)
        .map_err(|e| e.to_string())?;
    warm_in_background(&app);
    let text = if on {
        "Folders above the project turned on. Restart this session's sandbox to use it."
    } else {
        "Folders above the project turned off. Restart this session's sandbox to take it away."
    };
    for live in crate::agent::live_sessions(&state) {
        if live.folders.is_empty() {
            continue;
        }
        if (project.is_none() || live.project.as_deref() == project.as_deref())
            && let Ok(mut o) = live.restart_offer.lock()
        {
            *o = Some(text.into());
        }
    }
    Ok(())
}

/// Run [`warm`] on its own thread.
pub fn warm_in_background(app: &AppHandle) {
    use tauri::Manager;
    let app = app.clone();
    std::thread::spawn(move || warm(&app.state::<AppState>()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)] // Windows paths: on other systems `C:\...` isn't a path
    #[test]
    fn the_folders_above_are_listed_once_from_the_root() {
        let a = ancestors(&[
            PathBuf::from(r"C:\Users\you\Projects\app"),
            PathBuf::from(r"\\?\C:\Users\you\Projects\lib"),
        ]);
        let shown: Vec<String> = a.iter().map(|p| shown(p)).collect();
        assert_eq!(
            shown,
            vec![
                r"C:\",
                r"C:\Users",
                r"C:\Users\you",
                r"C:\Users\you\Projects"
            ]
        );
        assert!(ancestors(&[PathBuf::from(r"C:\app")]).len() == 1);
    }

    #[cfg(windows)] // Windows paths: on other systems `C:\...` isn't a path
    #[test]
    fn each_folder_says_what_becomes_visible() {
        assert!(what(Path::new(r"C:\")).contains("top of this drive"));
        assert!(what(Path::new(r"C:\Users")).contains("account names"));
        assert!(what(Path::new(r"C:\Users\you\Projects")).contains("other projects"));
        if let Some(home) = std::env::var_os("USERPROFILE") {
            assert!(what(Path::new(&home)).contains(".ssh"));
        }
    }

    #[test]
    fn the_setting_is_per_project_or_global() {
        let mut agent = crate::state::AgentSettings::default();
        assert!(!enabled(&agent, Some("p1")));
        agent.project_parent_folders.push("p1".into());
        assert!(enabled(&agent, Some("p1")));
        assert!(!enabled(&agent, Some("p2")));
        agent.parent_folders = true;
        assert!(enabled(&agent, Some("p2")) && enabled(&agent, None));
    }

    #[test]
    fn the_pasted_script_quotes_its_paths() {
        let sid = "S-1-15-2-1-2-3";
        let c = pasted(sid, &[r"C:\", r"C:\Users"], false);
        assert!(c.starts_with("& {\r\n"), "{c}");
        assert!(c.trim_end().ends_with('}'));
        assert!(c.contains(r"$paths = @('C:\','C:\Users')"), "{c}");
        assert!(c.contains("$remove = $false"));
        assert!(c.contains("SecurityIdentifier 'S-1-15-2-1-2-3'"));
        assert!(!c.contains("@PATHS@") && !c.contains("@SID@") && !c.contains("@REMOVE@"));
        // The here-string's end stays at the start of its line.
        assert!(c.contains("\r\n'@\r\n"));
        assert!(pasted(sid, &["D:\\"], true).contains("$remove = $true"));
        // Every kind of single quote is doubled, so a name can't end the
        // string early; a placeholder in a name is left as it is.
        assert_eq!(ps_quoted("a'b\u{2019}c"), "'a''b\u{2019}\u{2019}c'");
        assert!(pasted(sid, &[r"C:\@SID@"], false).contains(r"'C:\@SID@'"));
    }

    #[cfg(windows)] // Windows paths: on other systems `C:\...` isn't a path
    #[test]
    fn projects_inside_credential_folders_are_refused() {
        let p = plan(&[PathBuf::from(r"C:\Users\you\.config\tool\proj")]);
        assert!(p.iter().all(|f| f.problem.is_some()));
    }
}
