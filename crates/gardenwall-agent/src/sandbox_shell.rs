//! The shell tool's runner: commands execute inside the OS sandbox
//! (ADR-012) with read/write access to the granted folders and a scratch
//! folder only, and no network.
//!
//! One sandbox is created per agent session, so folder grants are applied
//! once rather than for every command. On Windows the shell is PowerShell
//! (PowerShell 7 when it is installed in
//! Program Files and starts in the sandbox, otherwise Windows PowerShell
//! 5.1) or `cmd.exe`, chosen in Settings. Each granted folder gets a private drive letter (`W:`, `X:`…)
//! that only the sandbox sees, so the folder is the root of a drive:
//! PowerShell can enter it and `dir` can read its drive (both need what an
//! AppContainer may not open on `C:`). Commands start in the real folder
//! and switch to the letter. PowerShell gets each command encoded, so no
//! quoting is lost; cmd gets `cmd /d /s /c "<command>"` verbatim. Batch
//! files still can't run. On Linux the shell is `/bin/sh`, with the system
//! toolchain folders granted read-only.

#![cfg(any(windows, target_os = "linux"))]

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gardenwall_sandbox::{Access, Limits, Network, PathGrant, Sandbox, SandboxPolicy};

use crate::agent::{BoxFuture, Shell, ShellOutput};

/// Output kept per stream; the rest is discarded (the model sees the tail).
const MAX_CAPTURE_BYTES: usize = 16 << 20;
/// Commands without a timeout are still stopped after this long.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Which shell runs commands on Windows (Settings → Agent). Linux always
/// uses `/bin/sh`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellKind {
    /// PowerShell 7 when it is installed (and starts in the sandbox),
    /// otherwise Windows PowerShell 5.1 (the default).
    #[default]
    PowerShell,
    /// Windows PowerShell 5.1, even when PowerShell 7 is installed.
    WindowsPowerShell,
    /// `cmd.exe`.
    Cmd,
}

/// The shell a session actually uses, after fallbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Flavor {
    /// PowerShell 7 (`pwsh.exe`): has `&&` and `||`.
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`): no `&&`.
    WindowsPowerShell,
    Cmd,
    /// `/bin/sh` (Linux).
    Sh,
}

/// PowerShell 7's `pwsh.exe` in its standard install folder
/// (`%ProgramFiles%\PowerShell\<major>`, where the official MSI and winget
/// put it), newest major version first; previews are skipped. The Store
/// build lives under `WindowsApps` and can't start inside another
/// AppContainer, so execution aliases and that folder are never used.
/// Program Files is readable by every AppContainer, so the sandbox needs no
/// grant for it (and none is added).
pub fn find_powershell7() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.is_absolute() && !bases.contains(&p) {
                bases.push(p);
            }
        }
    }
    let mut best: Option<(u32, PathBuf)> = None;
    for base in bases {
        let Ok(entries) = std::fs::read_dir(base.join("PowerShell")) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(major) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|m| *m >= 7)
            else {
                continue;
            };
            let exe = entry.path().join("pwsh.exe");
            let in_store = exe
                .components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps"));
            if !in_store && exe.is_file() && best.as_ref().is_none_or(|(m, _)| major > *m) {
                best = Some((major, exe));
            }
        }
    }
    best.map(|(_, exe)| exe)
}

/// Drive letters given to granted folders, in order.
#[cfg_attr(not(windows), allow(dead_code))]
const DRIVE_LETTERS: [char; 8] = ['W', 'X', 'Y', 'Z', 'V', 'U', 'T', 'S'];

/// The agent sandbox's profile (its AppContainer name).
pub const AGENT_PROFILE: &str = "Gardenwall.Agent";

/// System facts and folder names a Windows command expects to find in its
/// environment (copied from Gardenwall's own). Names only: the sandbox's
/// grants decide what can be read or written there.
#[cfg(windows)]
const SYSTEM_LOCATIONS: [&str; 13] = [
    "SystemDrive",
    "ProgramData",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

pub struct SandboxShell {
    sandbox: Arc<Mutex<Sandbox>>,
    program: PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    base_args: Vec<OsString>,
    /// The shell in use, after fallbacks (PowerShell 7 → Windows
    /// PowerShell 5.1 → cmd).
    flavor: Flavor,
    /// Its name for the model and the user, e.g. "PowerShell 7.6.10".
    label: &'static str,
    /// Private drive letters and the folders they stand for; empty when the
    /// mapping couldn't be set up (commands then run in the real folders).
    #[cfg_attr(not(windows), allow(dead_code))]
    drives: Vec<(char, PathBuf)>,
    /// What the model is told about this shell.
    snippet: &'static str,
    guidelines: &'static [&'static str],
    /// The granted folders, for the protected-location check.
    roots: Vec<PathBuf>,
    /// Where held-back changes wait, outside every grant.
    quarantine: PathBuf,
    /// Held-back changes and left-over programs waiting for a decision.
    pending: Arc<Mutex<Pending>>,
    /// Protected locations the sandbox couldn't make read-only.
    unprotected: Vec<String>,
    /// Protected locations already made read-only (each only once: a second
    /// time would record the read-only permissions as the ones to restore).
    protected: Mutex<std::collections::HashSet<PathBuf>>,
    /// Build and toolchain files below the top of the project, found when
    /// the session started (`guard::nested_locations`, ADR-022 §8).
    nested: Arc<Mutex<Vec<crate::guard::Location>>>,
    /// Secret files in the granted folders, hidden from commands.
    secrets: Arc<Mutex<crate::secrets::Watch>>,
    /// Why a secret file couldn't be hidden when the session started.
    secret_problem: Option<String>,
    /// The programs left running at the last look, shown while a command
    /// holds the sandbox (so a status check never waits for a command).
    background_seen: Mutex<Vec<String>>,
    /// Set by [`Shell::kill_now`] (lockdown):
    /// the running command and everything in the sandbox end at once, and
    /// no command runs after.
    killed: Arc<std::sync::atomic::AtomicBool>,
}

impl SandboxShell {
    /// - `folders`: granted workspaces (first is the working directory),
    ///   read/write.
    /// - `scratch`: a private temp folder for the session, read/write.
    /// - `tool_folders`: folders with programs the agent may run (e.g. a
    ///   Python or Rust install), read-only; each folder and its `bin` and
    ///   `Scripts` subfolders are put on PATH. A tool folder holding
    ///   `python.exe` makes Python available (see [`crate::python`]).
    pub fn new(
        folders: &[PathBuf],
        scratch: &Path,
        tool_folders: &[PathBuf],
        grant_journal_dir: Option<PathBuf>,
        kind: ShellKind,
    ) -> Result<Self, String> {
        Self::new_with_env(folders, scratch, tool_folders, grant_journal_dir, kind, &[])
    }

    /// [`SandboxShell::new`] with extra environment variables for every
    /// command, set by the core (e.g. the agent's git identity). They are
    /// added after the shell's own, so they can't be used to replace PATH
    /// or the sandbox's folders: those names are refused.
    pub fn new_with_env(
        folders: &[PathBuf],
        scratch: &Path,
        tool_folders: &[PathBuf],
        grant_journal_dir: Option<PathBuf>,
        kind: ShellKind,
        extra_env: &[(OsString, OsString)],
    ) -> Result<Self, String> {
        let pwsh = match kind {
            ShellKind::PowerShell => find_powershell7(),
            _ => None,
        };
        Self::build(
            folders,
            scratch,
            tool_folders,
            grant_journal_dir,
            kind,
            pwsh,
            extra_env,
        )
    }

    /// [`SandboxShell::new`] with a given `pwsh.exe` (tried only for
    /// [`ShellKind::PowerShell`]), so tests can check the fallback when it
    /// doesn't start in the sandbox.
    #[doc(hidden)]
    pub fn with_powershell7(
        folders: &[PathBuf],
        scratch: &Path,
        tool_folders: &[PathBuf],
        grant_journal_dir: Option<PathBuf>,
        kind: ShellKind,
        pwsh: Option<PathBuf>,
    ) -> Result<Self, String> {
        Self::build(
            folders,
            scratch,
            tool_folders,
            grant_journal_dir,
            kind,
            pwsh,
            &[],
        )
    }

    fn build(
        folders: &[PathBuf],
        scratch: &Path,
        tool_folders: &[PathBuf],
        grant_journal_dir: Option<PathBuf>,
        kind: ShellKind,
        pwsh: Option<PathBuf>,
        extra_env: &[(OsString, OsString)],
    ) -> Result<Self, String> {
        let pwsh = pwsh.filter(|_| kind == ShellKind::PowerShell);
        #[cfg(not(windows))]
        let _ = &pwsh;
        let cwd = plain_dir(folders.first().ok_or("no folder granted")?);
        std::fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
        // Inside an AppContainer, Windows gives programs
        // %LOCALAPPDATA%\Packages\<container>\AC\Temp as their temp folder,
        // whatever TEMP says; LOCALAPPDATA is the scratch folder here, so it
        // must exist there (the MSVC linker failed with LNK1104 without it;
        // ADR-022).
        #[cfg(windows)]
        std::fs::create_dir_all(
            scratch
                .join("Packages")
                .join(AGENT_PROFILE.to_ascii_lowercase())
                .join("AC")
                .join("Temp"),
        )
        .map_err(|e| e.to_string())?;
        let mut grants: Vec<PathGrant> = folders
            .iter()
            .chain(std::iter::once(&scratch.to_path_buf()))
            .map(|p| PathGrant {
                path: plain_dir(p),
                access: Access::ReadWrite,
            })
            .collect();
        // PATH entries and TEMP use the drive form: a verbatim `\\?\C:\...`
        // entry becomes a batch file's `%~dp0`, from which cmd can't start
        // programs (npm.cmd and similar launchers failed with "The system
        // cannot find the path specified").
        let mut tool_path: Vec<PathBuf> = Vec::new();
        for t in tool_folders {
            grants.push(PathGrant {
                path: t.clone(),
                access: Access::ReadExecute,
            });
            let t = plain_dir(t);
            for sub in ["bin", "Scripts"] {
                if t.join(sub).is_dir() {
                    tool_path.push(t.join(sub));
                }
            }
            tool_path.push(t);
        }
        let scratch_s: OsString = plain_dir(scratch).into_os_string();
        let mut env: Vec<(OsString, OsString)> = vec![
            ("TEMP".into(), scratch_s.clone()),
            ("TMP".into(), scratch_s.clone()),
            ("TMPDIR".into(), scratch_s.clone()),
            ("HOME".into(), scratch_s.clone()),
        ];
        env.extend(crate::python::env(scratch));
        // Node scripts need their realpath fixed to start in the sandbox.
        #[cfg(windows)]
        env.extend(crate::node::env());
        let (program, base_args);
        #[cfg_attr(windows, allow(unused_assignments))]
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut flavor = Flavor::Sh;
        #[cfg(windows)]
        let (cmd_exe, powershell);
        #[cfg(windows)]
        {
            // AppContainer processes can read the Windows folder by default.
            let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            let root_path = PathBuf::from(&root);
            let cmd = root_path.join("System32").join("cmd.exe");
            let ps_dir = root_path
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0");
            powershell = ps_dir.join("powershell.exe");
            // PowerShell 7 is tried first (it must still start in the
            // sandbox, checked below); then 5.1, then cmd.
            flavor = match kind {
                ShellKind::Cmd => Flavor::Cmd,
                _ if powershell.is_file() => Flavor::WindowsPowerShell,
                _ => Flavor::Cmd,
            };
            if let Some(dir) = pwsh.as_ref().and_then(|p| p.parent()) {
                // Its own folder first, so `pwsh` inside finds this one.
                tool_path.push(dir.to_path_buf());
            }
            tool_path.extend([
                root_path.join("System32"),
                root_path.join("System32").join("Wbem"),
                ps_dir.clone(),
            ]);
            let path = std::env::join_paths(&tool_path).map_err(|e| e.to_string())?;
            env.extend([
                ("SystemRoot".into(), root.clone()),
                ("windir".into(), root),
                ("PATH".into(), path),
                ("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into()),
                ("ComSpec".into(), cmd.clone().into_os_string()),
                ("USERPROFILE".into(), scratch_s.clone()),
                // PowerShell's caches and settings stay in the scratch folder.
                ("APPDATA".into(), scratch_s.clone()),
                ("LOCALAPPDATA".into(), scratch_s.clone()),
                (
                    "PSModulePath".into(),
                    ps_dir.join("Modules").into_os_string(),
                ),
                // Project scripts (.ps1) may run; the sandbox is the boundary.
                ("PSExecutionPolicyPreference".into(), "Bypass".into()),
                // PowerShell 7: no telemetry or update checks (there is no
                // network anyway; this avoids the attempts).
                ("POWERSHELL_TELEMETRY_OPTOUT".into(), "1".into()),
                ("POWERSHELL_UPDATECHECK".into(), "Off".into()),
                ("NO_COLOR".into(), "1".into()),
            ]);
            // Where Windows' own folders are. Only names, no access: without
            // them Windows expands paths such as "%SystemDrive%\ProgramData"
            // literally, and the shell cache then lands in a
            // "%SystemDrive%" folder inside the project.
            for name in SYSTEM_LOCATIONS {
                if let Some(v) = std::env::var_os(name) {
                    env.push((name.into(), v));
                }
            }
            cmd_exe = cmd;
            base_args = vec![OsString::from("/d"), OsString::from("/c")];
        }
        #[cfg(target_os = "linux")]
        {
            for sys in ["/bin", "/usr", "/lib", "/lib64", "/etc"] {
                if Path::new(sys).exists() {
                    grants.push(PathGrant {
                        path: sys.into(),
                        access: Access::ReadExecute,
                    });
                }
            }
            tool_path.extend(["/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
            env.push((
                "PATH".into(),
                std::env::join_paths(&tool_path).map_err(|e| e.to_string())?,
            ));
            let _ = kind;
            program = PathBuf::from("/bin/sh");
            base_args = vec![OsString::from("-c")];
        }
        for (name, value) in extra_env {
            let taken = env.iter().any(|(n, _)| {
                n.to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
            });
            if taken {
                return Err(format!(
                    "the variable {} is set by the sandbox itself",
                    name.to_string_lossy()
                ));
            }
            env.push((name.clone(), value.clone()));
        }
        let policy = SandboxPolicy {
            profile: AGENT_PROFILE.into(),
            grants: std::mem::take(&mut grants),
            network: Network::Denied,
            limits: Limits {
                memory_bytes: Some(8 << 30),
                max_processes: 64,
                wall_time: None,
            },
            env,
            working_dir: Some(cwd),
            grant_journal_dir,
        };
        let mut sandbox = Sandbox::new(policy).map_err(|e| e.to_string())?;
        sandbox.use_hidden_console();
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut drives: Vec<(char, PathBuf)> = Vec::new();
        #[cfg(windows)]
        {
            let wanted: Vec<(char, PathBuf)> = DRIVE_LETTERS
                .iter()
                .copied()
                .zip(folders.iter().map(|f| plain_dir(f)))
                .collect();
            if sandbox.map_drives(&wanted).is_ok() && drive_works(&sandbox, &cmd_exe, wanted[0].0) {
                drives = wanted;
            } else {
                // Not available here: commands run in the real folders,
                // exactly as without the mapping.
                let _ = sandbox.map_drives(&[]);
            }
        }
        #[cfg_attr(windows, allow(unused_assignments))]
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut label: &'static str = "bash";
        #[cfg(windows)]
        {
            label = match flavor {
                Flavor::Cmd => "cmd",
                _ => "Windows PowerShell 5.1",
            };
            // PowerShell 7 is used only if it really starts in this
            // sandbox and reports a version of 7 or later.
            let version = pwsh
                .as_deref()
                .and_then(|exe| pwsh_version(&sandbox, exe, &drives));
            program = match (version, flavor) {
                (Some(v), _) => {
                    flavor = Flavor::Pwsh;
                    label = Box::leak(format!("PowerShell {v}").into_boxed_str());
                    pwsh.clone().unwrap_or_default()
                }
                (None, Flavor::WindowsPowerShell) => powershell,
                _ => cmd_exe.clone(),
            };
        }
        // Python is offered only if it really starts in this sandbox.
        let python =
            crate::python::interpreter(tool_folders).and_then(|exe| python_version(&sandbox, &exe));
        let (snippet, guidelines) = describe(flavor, label, &drives, python.as_deref());
        // Protected locations that exist now become read-only for commands
        // (Windows; on Linux the after-command check covers them).
        let roots: Vec<PathBuf> = folders.iter().map(|f| plain_dir(f)).collect();
        let mut unprotected = Vec::new();
        let mut protected = std::collections::HashSet::new();
        let nested = crate::guard::nested_locations(&roots);
        for loc in crate::guard::locations(&roots)
            .into_iter()
            .chain(nested.clone())
        {
            if loc.read_only && std::fs::symlink_metadata(&loc.path).is_ok() {
                match sandbox.protect(&loc.path) {
                    Ok(_) => {
                        protected.insert(loc.path.clone());
                    }
                    Err(e) => unprotected.push(format!("{}: {e}", loc.shown)),
                }
            }
        }
        // Secret files lose all access for commands (ADR-011 §2.4). One
        // that can't be hidden is reported here, and every command is
        // refused until it can (`run` tries again).
        let mut secrets = crate::secrets::Watch::default();
        let secret_problem = secrets.refresh(&roots, &mut sandbox).err();
        Ok(Self {
            sandbox: Arc::new(Mutex::new(sandbox)),
            program,
            base_args,
            flavor,
            label,
            drives,
            snippet,
            guidelines,
            roots,
            quarantine: default_quarantine(),
            pending: Arc::new(Mutex::new(Pending::default())),
            unprotected,
            protected: Mutex::new(protected),
            nested: Arc::new(Mutex::new(nested)),
            secrets: Arc::new(Mutex::new(secrets)),
            secret_problem,
            background_seen: Mutex::new(Vec::new()),
            killed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Where held-back changes wait: a folder outside every grant (the app
    /// uses its data folder; the default is a fresh folder in the temp
    /// folder, which the sandbox can't reach).
    pub fn set_quarantine(&mut self, dir: PathBuf) {
        self.quarantine = dir;
    }

    /// Protected locations the sandbox couldn't make read-only (each with
    /// the reason). The after-command check still covers them.
    pub fn unprotected(&self) -> &[String] {
        &self.unprotected
    }

    /// A secret file that couldn't be hidden from commands when the session
    /// started. Commands are refused until it can be.
    pub fn secret_problem(&self) -> Option<&str> {
        self.secret_problem.as_deref()
    }

    /// Secret files in the granted folders that commands can't touch, as
    /// paths relative to their folder (empty where the OS can't enforce it).
    pub fn hidden_secrets(&self) -> Vec<String> {
        self.secrets
            .lock()
            .map(|w| w.hidden().iter().map(|s| s.shown.clone()).collect())
            .unwrap_or_default()
    }
}

fn default_quarantine() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("gardenwall-held-{}-{nanos}", std::process::id()))
}

/// What the last command left for the user to decide on.
#[derive(Default)]
struct Pending {
    held: Option<crate::guard::HeldBack>,
    programs: Vec<(u32, String)>,
    /// Programs the user let keep running.
    allowed: std::collections::HashSet<u32>,
    /// Commands run, for unique quarantine folders.
    count: u64,
    /// The protected locations as the last command left them, kept while
    /// programs from it still run: what they change before the next
    /// command is held back too (review GW-005).
    after: Option<crate::guard::Snapshot>,
}

fn drain(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if out.len() < MAX_CAPTURE_BYTES {
                let room = MAX_CAPTURE_BYTES - out.len();
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
        out
    })
}

/// How the shell is started for one command.
enum Launch {
    #[cfg_attr(windows, allow(dead_code))]
    Args(Vec<OsString>),
    /// A command-line tail passed verbatim (Windows `cmd /s /c "..."`).
    #[cfg_attr(not(windows), allow(dead_code))]
    Raw(String),
}

impl SandboxShell {
    fn launch(&self, command: &str) -> Result<Launch, String> {
        #[cfg(windows)]
        if matches!(self.flavor, Flavor::Pwsh | Flavor::WindowsPowerShell) {
            return Ok(Launch::Args(powershell_args(command, &self.drives)));
        }
        #[cfg(windows)]
        {
            if command.contains(['\r', '\n']) {
                return Err(
                    "cmd runs one line at a time. Join multiple commands with && (or & to continue after a failure)."
                        .into(),
                );
            }
            // `/s` makes cmd strip only the outer quotes and run the rest as
            // written; `chcp 65001` gives UTF-8 output where Windows allows
            // it (it is refused in non-interactive service sessions, so its
            // errors are silenced too and output keeps the default code page).
            // Its output goes to a file in the sandbox's temp folder, not
            // `nul`: where the sandbox can't open the NUL device (GitHub's
            // runners), `>nul` printed "Access is denied." after every command.
            // cmd stays in the real folder it starts in: programs it runs
            // inherit that working folder, while the private letters exist
            // only for cmd itself (Windows gives child processes the logon
            // session's drives). `dir` needs a path there (`dir W:\`).
            Ok(Launch::Raw(format!(
                "/d /s /c \"chcp 65001>\"%TEMP%\\gardenwall-chcp.txt\" 2>&1 & {command}\""
            )))
        }
        #[cfg(not(windows))]
        {
            let mut args = self.base_args.clone();
            args.push(command.into());
            Ok(Launch::Args(args))
        }
    }
}

/// How often a running command looks at the kill switch.
const KILL_CHECK: Duration = Duration::from_millis(100);

/// What a command killed by lockdown reports.
pub const KILLED: &str =
    "Gardenwall's lockdown stopped this command and every program in the sandbox.";

fn run_blocking(
    sandbox: &Mutex<Sandbox>,
    program: &Path,
    launch: &Launch,
    timeout: Duration,
    killed: &std::sync::atomic::AtomicBool,
) -> Result<ShellOutput, String> {
    use std::sync::atomic::Ordering;
    if killed.load(Ordering::SeqCst) {
        return Err(KILLED.into());
    }
    let sandbox = sandbox
        .lock()
        .map_err(|_| "sandbox unavailable".to_string())?;
    #[allow(unused_mut)]
    let mut child = match launch {
        Launch::Args(args) => {
            let refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
            sandbox.spawn(program, &refs)
        }
        #[cfg(windows)]
        Launch::Raw(tail) => sandbox.spawn_raw(program, tail),
        #[cfg(not(windows))]
        Launch::Raw(_) => return Err("raw command lines are Windows-only".into()),
    }
    .map_err(|e| e.to_string())?;
    drop(child.stdin.take());
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    // Waited for in short slices, so the kill switch is seen within
    // KILL_CHECK while this command holds the sandbox lock.
    let started = std::time::Instant::now();
    let mut was_killed = false;
    let code = loop {
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            break None;
        }
        if let Some(code) = child
            .wait_timeout(Some(left.min(KILL_CHECK)))
            .map_err(|e| e.to_string())?
        {
            break Some(code);
        }
        if killed.load(Ordering::SeqCst) {
            was_killed = true;
            break None;
        }
    };
    let timed_out = code.is_none() && !was_killed;
    if code.is_none() {
        // End every process in the sandbox, not just the shell: a child it
        // started could otherwise keep the output pipes open forever.
        let _ = sandbox.terminate_all();
        let _ = child.kill();
        let _ = child.wait_timeout(Some(Duration::from_secs(5)));
    }
    let mut output = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = err.and_then(|h| h.join().ok()).unwrap_or_default();
    if !err.is_empty() {
        if !output.is_empty() && !output.ends_with(b"\n") {
            output.push(b'\n');
        }
        output.extend_from_slice(&err);
    }
    if was_killed {
        return Err(KILLED.into());
    }
    Ok(ShellOutput {
        output: String::from_utf8_lossy(&output).replace("\r\n", "\n"),
        exit_code: code.map(|c| c as i32),
        timed_out,
    })
}

impl Shell for SandboxShell {
    fn name(&self) -> &'static str {
        self.label
    }

    fn tool_name(&self) -> &'static str {
        match self.flavor {
            Flavor::Pwsh | Flavor::WindowsPowerShell => "powershell",
            Flavor::Cmd => "cmd",
            Flavor::Sh => "bash",
        }
    }

    fn snippet(&self) -> &'static str {
        self.snippet
    }

    fn guidelines(&self) -> &'static [&'static str] {
        self.guidelines
    }

    fn drives(&self) -> Vec<(char, PathBuf)> {
        self.drives.clone()
    }

    fn program_command(&self, program: &Path, args: &[&str]) -> Option<String> {
        program_command(self.flavor, program, args)
    }

    fn run<'a>(
        &'a self,
        command: &'a str,
        timeout: Option<Duration>,
    ) -> BoxFuture<'a, Result<ShellOutput, String>> {
        let sandbox = self.sandbox.clone();
        let program = self.program.clone();
        let launch = match self.launch(command) {
            Ok(l) => l,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
        let flavor = self.flavor;
        let roots = self.roots.clone();
        let pending = self.pending.clone();
        let quarantine = self.quarantine.clone();
        let secrets = self.secrets.clone();
        let watch = self.secrets.clone();
        let nested = self.nested.clone();
        let killed = self.killed.clone();
        Box::pin(async move {
            // The command runs on its own thread, which wakes this task the
            // moment it finishes (no polling delay). Around it, the
            // protected locations are compared (ADR-011 §2.5): a few dozen
            // file lookups, never the whole project.
            let mut result = run_on_thread(move || {
                // Secret files are hidden first; if one can't be, the
                // command doesn't run.
                {
                    let mut watch = watch.lock().map_err(|_| "secret watch unavailable")?;
                    let mut sandbox = sandbox.lock().map_err(|_| "sandbox unavailable")?;
                    watch.refresh(&roots, &mut sandbox)?;
                }
                let mut locations = crate::guard::locations(&roots);
                if let Ok(n) = nested.lock() {
                    locations.extend(n.iter().cloned());
                }
                let mut before = crate::guard::Snapshot::take(&locations);
                // Programs left running by an earlier command may have
                // changed protected locations since: hold those changes
                // back before this command can use them (a planted Git
                // hook, say), and ask about them with this command's.
                let mut early = None;
                if let Ok(mut p) = pending.lock()
                    && let Some(last) = p.after.take()
                {
                    let between = last.changes(&before);
                    if !between.is_empty() {
                        p.count += 1;
                        let dir = quarantine.join(format!("{}-between", p.count));
                        early = Some(crate::guard::HeldBack::hold(between, &dir));
                        before = crate::guard::Snapshot::take(&locations);
                    }
                }
                let result = run_blocking(&sandbox, &program, &launch, timeout, &killed);
                let after = crate::guard::Snapshot::take(&locations);
                let changes = before.changes(&after);
                let running = sandbox.lock().map(|s| s.processes()).unwrap_or_default();
                if let Ok(mut p) = pending.lock() {
                    p.count += 1;
                    let mut held = early;
                    if !changes.is_empty() {
                        let dir = quarantine.join(p.count.to_string());
                        let now = crate::guard::HeldBack::hold(changes, &dir);
                        held = Some(match held {
                            Some(mut h) => {
                                h.extend(now);
                                h
                            }
                            None => now,
                        });
                    }
                    if let Some(held) = held
                        && let Some(old) = p.held.replace(held)
                    {
                        old.discard();
                    }
                    p.after = (!running.is_empty()).then_some(after);
                    let allowed = p.allowed.clone();
                    p.programs = running
                        .into_iter()
                        .filter(|(pid, _)| !allowed.contains(pid))
                        .collect();
                }
                result
            })
            .await
            .ok_or_else(|| "command runner stopped unexpectedly".to_string())??;
            // A failed command written for another shell gets a pointer to
            // this shell's way of doing it.
            if cfg!(windows)
                && result.exit_code != Some(0)
                && let Some(hint) = habit_hint(flavor, command, &result.output)
            {
                if !result.output.ends_with('\n') && !result.output.is_empty() {
                    result.output.push('\n');
                }
                result.output.push_str(&hint);
            }
            if cfg!(windows)
                && let Some(hint) = browser_hint(&result.output)
            {
                if !result.output.ends_with('\n') && !result.output.is_empty() {
                    result.output.push('\n');
                }
                result.output.push_str(hint);
            }
            if result.exit_code != Some(0)
                && let Some(hint) = secrets.lock().ok().and_then(|w| w.hint(command))
            {
                if !result.output.ends_with('\n') && !result.output.is_empty() {
                    result.output.push('\n');
                }
                result.output.push_str(&hint);
            }
            Ok(result)
        })
    }

    fn aftermath(&self) -> Option<crate::agent::Aftermath> {
        let p = self.pending.lock().ok()?;
        let held = p.held.as_ref().filter(|h| !h.is_empty());
        if held.is_none() && p.programs.is_empty() {
            return None;
        }
        Some(crate::agent::Aftermath {
            changes: held.map(|h| h.summary()).unwrap_or_default(),
            details: held.map(|h| h.details()).unwrap_or_default(),
            programs: p
                .programs
                .iter()
                .map(|(pid, name)| format!("{name} (process {pid})"))
                .collect(),
            not_held: held.map(|h| h.not_held()).unwrap_or_default(),
        })
    }

    fn settle_changes(&self, keep: bool) -> Vec<String> {
        let Some(held) = self.pending.lock().ok().and_then(|mut p| p.held.take()) else {
            return Vec::new();
        };
        if !keep {
            return held.discard();
        }
        let created = held.created();
        held.keep();
        // A protected location the user let a command create becomes
        // read-only for the rest of the session.
        let nested = self.nested.lock().map(|n| n.clone()).unwrap_or_default();
        let read_only: Vec<PathBuf> = crate::guard::locations(&self.roots)
            .into_iter()
            .chain(nested)
            .filter(|l| l.read_only)
            .map(|l| l.path)
            .collect();
        if let Ok(mut sandbox) = self.sandbox.lock() {
            for path in created.iter().filter(|p| read_only.contains(p)) {
                let _ = sandbox.protect(path);
            }
        }
        Vec::new()
    }

    fn protect_new(&self) {
        let found = crate::guard::nested_locations(&self.roots);
        if let Ok(mut n) = self.nested.lock() {
            n.clone_from(&found);
        }
        let (Ok(mut sandbox), Ok(mut protected)) = (self.sandbox.lock(), self.protected.lock())
        else {
            return;
        };
        for loc in crate::guard::locations(&self.roots)
            .into_iter()
            .chain(found)
        {
            if loc.read_only
                && !protected.contains(&loc.path)
                && std::fs::symlink_metadata(&loc.path).is_ok()
                && sandbox.protect(&loc.path).is_ok()
            {
                protected.insert(loc.path);
            }
        }
    }

    fn hidden_files(&self) -> Vec<String> {
        self.hidden_secrets()
    }

    fn background(&self) -> Vec<String> {
        let allowed = match self.pending.lock() {
            Ok(p) => p.allowed.clone(),
            Err(_) => return Vec::new(),
        };
        if allowed.is_empty() {
            if let Ok(mut seen) = self.background_seen.lock() {
                seen.clear();
            }
            return Vec::new();
        }
        // A running command holds the sandbox until it ends: show what was
        // seen last instead of waiting.
        let sandbox = match self.sandbox.try_lock() {
            Ok(s) => s,
            Err(std::sync::TryLockError::WouldBlock) => {
                return self
                    .background_seen
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return Vec::new(),
        };
        let now: Vec<String> = sandbox
            .processes()
            .into_iter()
            .filter(|(pid, _)| allowed.contains(pid))
            .map(|(pid, name)| format!("{name} (process {pid})"))
            .collect();
        if let Ok(mut seen) = self.background_seen.lock() {
            seen.clone_from(&now);
        }
        now
    }

    fn kill_now(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Between commands nothing holds the sandbox: end everything now.
        // During one, the command's wait ends it within KILL_CHECK.
        if let Ok(sandbox) = self.sandbox.try_lock() {
            let _ = sandbox.terminate_all();
        }
    }

    fn stop_background(&self) {
        let Ok(mut p) = self.pending.lock() else {
            return;
        };
        if let Ok(sandbox) = self.sandbox.lock() {
            for pid in p.allowed.drain() {
                sandbox.terminate_process(pid);
            }
        }
    }

    fn settle_programs(&self, keep: bool) {
        let Ok(mut p) = self.pending.lock() else {
            return;
        };
        let programs = std::mem::take(&mut p.programs);
        if keep {
            p.allowed.extend(programs.iter().map(|(pid, _)| *pid));
        } else if let Ok(sandbox) = self.sandbox.lock() {
            for (pid, _) in programs {
                sandbox.terminate_process(pid);
            }
        }
    }
}

/// Run `f` on a new thread and wait for its result without blocking the
/// executor, so this crate does not depend on one. `None` if the thread
/// panicked.
pub(crate) async fn run_on_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    use std::task::{Poll, Waker};

    struct Slot<T> {
        value: Option<T>,
        done: bool,
        waker: Option<Waker>,
    }
    /// Marks the slot done and wakes the waiter, also when `f` panics.
    struct Finish<T>(Arc<Mutex<Slot<T>>>, Option<T>);
    impl<T> Drop for Finish<T> {
        fn drop(&mut self) {
            if let Ok(mut s) = self.0.lock() {
                s.value = self.1.take();
                s.done = true;
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }
        }
    }

    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        done: false,
        waker: None,
    }));
    let theirs = slot.clone();
    std::thread::spawn(move || {
        let mut finish = Finish(theirs, None);
        finish.1 = Some(f());
    });
    std::future::poll_fn(move |cx| {
        let Ok(mut s) = slot.lock() else {
            return Poll::Ready(None);
        };
        if s.done {
            Poll::Ready(s.value.take())
        } else {
            s.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    })
    .await
}

/// Whether the private drive works: `cd` onto it succeeds in the sandbox.
#[cfg(windows)]
fn drive_works(sandbox: &Sandbox, cmd: &Path, letter: char) -> bool {
    let Ok(mut child) = sandbox.spawn_raw(cmd, &format!("/d /s /c \"cd /d {letter}:\\\"")) else {
        return false;
    };
    drop(child.stdin.take());
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let ok = matches!(
        child.wait_timeout(Some(Duration::from_secs(20))),
        Ok(Some(0))
    );
    if !ok {
        let _ = child.kill();
    }
    let _ = (out.map(|h| h.join()), err.map(|h| h.join()));
    ok
}

/// PowerShell 7's version as it reports it from inside the sandbox (run as
/// a real command, on the private drive), or `None` if it doesn't start
/// there, fails, or isn't version 7 or later.
#[cfg(windows)]
fn pwsh_version(sandbox: &Sandbox, exe: &Path, drives: &[(char, PathBuf)]) -> Option<String> {
    let args = powershell_args("$PSVersionTable.PSVersion.ToString()", drives);
    let refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    let mut child = sandbox.spawn(exe, &refs).ok()?;
    drop(child.stdin.take());
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let ok = matches!(
        child.wait_timeout(Some(Duration::from_secs(30))),
        Ok(Some(0))
    );
    if !ok {
        let _ = child.kill();
    }
    let out = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let _ = err.map(|h| h.join());
    parse_pwsh_version(&String::from_utf8_lossy(&out)).filter(|_| ok)
}

/// The version of the Python at `exe`, if it starts in this sandbox.
fn python_version(sandbox: &Sandbox, exe: &Path) -> Option<String> {
    let args: Vec<&OsStr> = crate::python::VERSION_ARGS.iter().map(OsStr::new).collect();
    let mut child = sandbox.spawn(exe, &args).ok()?;
    drop(child.stdin.take());
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let ok = matches!(
        child.wait_timeout(Some(Duration::from_secs(30))),
        Ok(Some(0))
    );
    if !ok {
        let _ = child.kill();
    }
    let out = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let _ = err.map(|h| h.join());
    crate::python::parse_version(&String::from_utf8_lossy(&out)).filter(|_| ok)
}

/// PowerShell's command-lookup hook that gives programs the real working
/// folder without wrapping them (so exit codes, `$?`, `&&`/`||`, pipes and
/// `Start-Process` behave exactly as usual). When a program (python, git,
/// findstr) is looked up, the location moves from `W:\src` to the same
/// folder on `GwRealW:`, whose path is the real one; when a command that
/// needs the private letter is looked up (moving, renaming or deleting,
/// which check the drive's volume, and showing the location), it moves
/// back. Other commands work on either drive. It uses the engine's own
/// API (not `Get-Command`, `Set-Location`), which loads no module: each
/// module load costs 100-200 ms per command.
#[cfg(windows)]
const PROGRAMS_IN_REAL_FOLDER: &str = r#"$global:gwLookup = $false
$global:gwOnLetter = @('Remove-Item','Move-Item','Rename-Item','Get-Location','rm','rmdir','rd','del','erase','ri','mv','move','mi','ren','rni','pwd','gl')
function global:GwMove([bool]$real) {
  $ss = $ExecutionContext.SessionState
  $here = $ss.Path.CurrentLocation.Path
  if ($real -and $here -match '^([A-Z]):(.*)$') {
    $to = 'GwReal' + $Matches[1] + ':' + $Matches[2]
  } elseif (-not $real -and $here -match '^GwReal([A-Z]):(.*)$') {
    $to = $Matches[1] + ':' + $Matches[2]
  } else { return }
  try { $null = $ss.Path.SetLocation([System.Management.Automation.WildcardPattern]::Escape($to)) } catch { }
}
$ExecutionContext.InvokeCommand.PreCommandLookupAction = {
  param($name, $e)
  if ($global:gwLookup) { return }
  $global:gwLookup = $true
  try {
    if ($global:gwOnLetter -contains $name) { GwMove $false; return }
    if ($name -match '[\\/:]') { GwMove $true; return }
    $ic = $ExecutionContext.InvokeCommand
    # Cheap checks first: a cmdlet already loaded, then an alias (through
    # the alias provider). A broader command search can start PowerShell's
    # module analysis, which costs hundreds of milliseconds.
    if ($ic.GetCmdlet($name)) { return }
    $escaped = [System.Management.Automation.WildcardPattern]::Escape($name)
    if ($ExecutionContext.InvokeProvider.Item.Exists('Alias:\' + $escaped)) { return }
    if ($ic.GetCommand($name, 'Application')) { GwMove $true }
  } finally { $global:gwLookup = $false }
}
"#;

/// A PowerShell version line of 7 or later ("7.6.10", "7.7.0-rc.1").
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_pwsh_version(output: &str) -> Option<String> {
    let v = output.trim();
    let major: u32 = v.split('.').next()?.parse().ok()?;
    let plain = v.len() <= 32
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    (major >= 7 && plain).then(|| v.to_string())
}

/// PowerShell's arguments for one command: a prologue (UTF-8 output, the
/// private drives, start in the first one), the command as written, and an
/// exit code that reflects failure. Sent encoded, so nothing is re-quoted.
/// The same for PowerShell 7 and 5.1: both answer an encoded command in
/// CLIXML unless told `-OutputFormat Text`, and 7's colour codes are
/// turned off (`$PSStyle` doesn't exist in 5.1).
///
/// The prologue loads no module (drives and the location go through the
/// engine's API) and text output is written straight through; the
/// formatter (`Out-String`, another module) is started only when a command
/// outputs objects. Measured in the sandbox, each module load added 140-190
/// ms to every command, so commands that run programs (python, git, tests)
/// now cost about what PowerShell's own start does.
#[cfg(windows)]
fn powershell_args(command: &str, drives: &[(char, PathBuf)]) -> Vec<OsString> {
    // Windows PowerShell 5.1 puts `Program Files\WindowsPowerShell\Modules`
    // back in front of the module path the sandbox sets; a module there
    // that it can't load (seen on GitHub's runners) stops it finding even
    // its own commands (`Write-Output`), so the prologue sets it again.
    let mut script = String::from(
        "$ErrorActionPreference = 'Continue'\n$ProgressPreference = 'SilentlyContinue'\n[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\n$OutputEncoding = [Console]::OutputEncoding\nif ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }\nif ($PSVersionTable.PSVersion.Major -lt 6) { $env:PSModulePath = $env:windir + '\\System32\\WindowsPowerShell\\v1.0\\Modules' }\n$gwFs = $ExecutionContext.SessionState.Provider.GetOne('FileSystem')\n",
    );
    // Each letter is two PowerShell drives. `W:` is rooted at the sandbox's
    // private letter, so PowerShell's own commands work there: moving,
    // renaming and deleting check the drive's volume, which only the
    // private letter lets the sandbox read (`C:\` is out of reach). A hidden
    // `GwRealW:` is rooted at the folder's real path, for programs: Windows
    // gives child processes the logon session's drives, not this process's,
    // so a program started in `W:\...` couldn't open any project file. A
    // lookup hook runs each program (python, git, findstr) from the
    // matching `GwRealW:` folder, so it gets the real working folder.
    // Errors here are written as plain text: Windows PowerShell 5.1 sends
    // uncaught ones as CLIXML even with `-OutputFormat Text`.
    for (letter, folder) in drives {
        let root = folder.display().to_string().replace('\'', "''");
        script.push_str(&format!(
            "try {{ $null = $ExecutionContext.SessionState.Drive.New([System.Management.Automation.PSDriveInfo]::new('{letter}', $gwFs, '{letter}:\\', '', $null), 'Global'); $null = $ExecutionContext.SessionState.Drive.New([System.Management.Automation.PSDriveInfo]::new('GwReal{letter}', $gwFs, '{root}', '', $null), 'Global') }} catch {{ [Console]::Error.WriteLine(\"[Gardenwall: drive {letter}: is unavailable: $($_.Exception.Message)]\") }}\n"
        ));
    }
    if let Some((letter, _)) = drives.first() {
        script.push_str(&format!(
            "try {{ $null = $ExecutionContext.SessionState.Path.SetLocation('{letter}:\\') }} catch {{ [Console]::Error.WriteLine(\"[Gardenwall: couldn't start in {letter}:\\: $($_.Exception.Message)]\") }}\n"
        ));
        script.push_str(PROGRAMS_IN_REAL_FOLDER);
    }
    // The command is compiled here, so a syntax error is reported like any
    // other error. Errors are written as plain text (PowerShell would send
    // CLIXML for an encoded command); text goes out as it comes, and
    // objects are formatted as text. Error lines from programs (their
    // stderr) are passed on without counting as a failure; their exit code
    // decides. `2>&1` already puts errors and output in one stream in the
    // order they came, so both are written to stdout: written to stderr,
    // they would be read from a second pipe and land after all the output
    // (cargo's "Compiling…" after the program's own output).
    script.push_str(&format!(
        "$gwCode = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))\n",
        crate::agent::base64_encode(command.as_bytes())
    ));
    script.push_str(
        r#"$global:gwFailed = $false
try { $gwBlock = [ScriptBlock]::Create($gwCode) } catch { [Console]::Error.WriteLine($_.Exception.InnerException.Message); if (-not $_.Exception.InnerException) { [Console]::Error.WriteLine($_.ToString()) }; exit 1 }
$LASTEXITCODE = 0
$gwOut = $null
. $gwBlock 2>&1 | ForEach-Object {
  if ($_ -is [System.Management.Automation.ErrorRecord]) {
    if ($_.FullyQualifiedErrorId -like 'NativeCommandError*') { [Console]::Out.WriteLine($_.ToString()) }
    else {
      $global:gwFailed = $true
      $name = $_.InvocationInfo.MyCommand.Name
      if ($name) { [Console]::Out.WriteLine("${name}: $($_.ToString())") } else { [Console]::Out.WriteLine($_.ToString()) }
    }
  } elseif ($null -eq $gwOut -and $_ -is [string]) { [Console]::Out.WriteLine($_) }
  else {
    if ($null -eq $gwOut) { $gwOut = { Out-String -Stream -Width 300 }.GetSteppablePipeline(); $gwOut.Begin($true) }
    foreach ($gwLine in $gwOut.Process($_)) { [Console]::Out.WriteLine($gwLine) }
  }
}
if ($gwOut) { foreach ($gwLine in $gwOut.End()) { [Console]::Out.WriteLine($gwLine) } }
if ($LASTEXITCODE -is [int] -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($global:gwFailed) { exit 1 }
exit 0
"#,
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        // Errors as text, not CLIXML (the default for encoded commands).
        "-OutputFormat".into(),
        "Text".into(),
        "-EncodedCommand".into(),
        crate::agent::base64_encode(&utf16).into(),
    ]
}

/// A command line that starts exactly `program` (absolute, never looked
/// up: cmd would otherwise try the project folder first) with plain-word
/// `args`, quoted for `flavor`. `None` for anything that can't be quoted
/// safely, so callers fall back rather than run something else.
fn program_command(flavor: Flavor, program: &Path, args: &[&str]) -> Option<String> {
    let plain = |a: &&str| {
        !a.is_empty()
            && a.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_=.".contains(&b))
    };
    if !program.is_absolute() || !args.iter().all(plain) {
        return None;
    }
    let path = crate::display::drive_form(program);
    let path = path.to_str()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    let quoted = match flavor {
        // PowerShell also reads the typographic single quotes as quotes.
        Flavor::Pwsh | Flavor::WindowsPowerShell => {
            if path.contains(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}']) {
                return None;
            }
            format!("& '{}'", path.replace('\'', "''"))
        }
        // Inside quotes cmd still expands %VAR%; there is no escape for it.
        Flavor::Cmd => {
            if path.contains(['"', '%']) {
                return None;
            }
            format!("\"{path}\"")
        }
        Flavor::Sh => format!("'{}'", path.replace('\'', "'\\''")),
    };
    Some(
        std::iter::once(quoted)
            .chain(args.iter().map(|a| a.to_string()))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// What the model is told about the shell: a summary and guidelines,
/// including which drive letter stands for which folder.
#[cfg_attr(not(windows), allow(unused_variables))]
fn describe(
    flavor: Flavor,
    label: &str,
    drives: &[(char, PathBuf)],
    python: Option<&str>,
) -> (&'static str, &'static [&'static str]) {
    let mut lines: Vec<String> = Vec::new();
    if flavor == Flavor::Sh {
        lines.push(crate::python::guideline(python));
        return (
            "Execute bash commands (ls, grep, find, etc.)",
            leak_lines(lines),
        );
    }
    let places: Vec<String> = drives
        .iter()
        .map(|(l, f)| format!("{l}:\\ is {}", f.display()))
        .collect();
    let commands = "use PowerShell commands (Get-ChildItem or ls, Get-Content or cat, Select-String instead of grep, Copy-Item, Move-Item, Remove-Item -Recurse, New-Item -ItemType Directory, Get-Command instead of which) and Windows paths";
    let snippet = match flavor {
        Flavor::Pwsh => {
            lines.push(format!(
                "The powershell tool runs {label}, not bash or cmd: {commands}"
            ));
            lines.push(
                "Separate commands with ; or new lines; cmd1 && cmd2 runs cmd2 only after success, cmd1 || cmd2 only after a failure".into(),
            );
            "Execute PowerShell 7 commands (Get-ChildItem, Get-Content, Select-String, etc.)"
        }
        Flavor::WindowsPowerShell | Flavor::Sh => {
            lines.push(format!(
                "The powershell tool runs Windows PowerShell 5.1, not bash, cmd or PowerShell 7: {commands}"
            ));
            lines.push(
                "Separate commands with ; or new lines (Windows PowerShell 5.1 has no && or ||); to continue only after success use: cmd1; if ($?) { cmd2 }".into(),
            );
            "Execute Windows PowerShell commands (Get-ChildItem, Get-Content, Select-String, etc.)"
        }
        Flavor::Cmd => {
            lines.push(
                "The cmd tool runs Windows cmd.exe, not bash or PowerShell: use dir, type, findstr, where, copy, move, del and mkdir, and Windows paths".into(),
            );
            lines.push(
                "Join several commands on one line with && (or & to continue after a failure); multi-line commands are not accepted".into(),
            );
            "Execute Windows cmd.exe commands (dir, type, findstr, where, etc.)"
        }
    };
    if !places.is_empty() {
        let letter = drives[0].0;
        if flavor == Flavor::Cmd {
            lines.push(format!(
                "Commands start in the project folder, {}. For cmd's own commands each granted folder is also a drive ({}); dir needs a path here, e.g. dir {letter}:\\ or dir {letter}:\\src",
                drives[0].1.display(),
                places.join(", ")
            ));
        } else {
            lines.push(format!(
                "Commands start in {letter}:\\. Inside the sandbox each granted folder is its own drive ({}); both names refer to the same files, and the file tools use the full paths",
                places.join(", ")
            ));
        }
        lines.push(
            "Programs you run (python, git, findstr, compilers) can't see these drive letters: give them paths relative to the current folder, or the full paths".into(),
        );
    }
    lines.push(
        "There is no network, so installing packages (pip install, npm install) fails; use what is already installed".into(),
    );
    lines.push(BROWSER_GUIDELINE.into());
    lines.push(WINDOW_GUIDELINE.into());
    lines.push(COMMIT_GUIDELINE.into());
    lines.push(crate::python::guideline(python));
    (snippet, leak_lines(lines))
}

/// Starting a GUI app from the sandbox showed no
/// window and held the command until its time limit.
const WINDOW_GUIDELINE: &str = "Programs with a window (GUI apps) don't show one from this sandbox and can hang the command until it times out: build them here, then ask the user to run them";

/// A first commit took in a 23,500-file vendored
/// dependency tree, flooding the output and tripping the secret check.
const COMMIT_GUIDELINE: &str = "Before committing, check git status: package and build folders (vendor, node_modules, target, wheels, packages, bin, obj, dist) belong in .gitignore, not in the commit, unless Git already tracks them";

/// Web browsers can't start in the sandbox: Chrome and
/// Edge stop themselves at once because their crash reporter needs a named
/// pipe an AppContainer may not create, whatever flags they are given.
const BROWSER_GUIDELINE: &str = "Web browsers (Chrome, Edge, Firefox) can't start in this sandbox, so headless screenshots or page rendering won't work here; to have a page looked at, ask the user to open it or to run the command themselves";

/// After a browser stopped itself in the sandbox: say why, so the model
/// doesn't keep retrying with other flags.
#[cfg_attr(not(windows), allow(dead_code))]
fn browser_hint(output: &str) -> Option<&'static str> {
    let lower = output.to_ascii_lowercase();
    (lower.contains("crashpad")
        && (lower.contains("createnamedpipe") || lower.contains("access is denied")))
    .then_some("Note from Gardenwall: this browser can't start in the sandbox (its crash reporter needs a named pipe the sandbox doesn't allow), and no flag changes that. Don't retry it; ask the user to open the page or to run the command themselves.")
}

/// One small, bounded allocation per session (the tool definitions need
/// 'static text).
fn leak_lines(lines: Vec<String>) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = lines
        .into_iter()
        .map(|l| &*Box::leak(l.into_boxed_str()))
        .collect();
    Box::leak(leaked.into_boxed_slice())
}

/// A pointer to this shell's way of doing something, when a failed command
/// looks written for another shell.
#[cfg_attr(not(windows), allow(dead_code))]
fn habit_hint(flavor: Flavor, command: &str, output: &str) -> Option<String> {
    let first = command
        .split(|c: char| c.is_whitespace() || c == '|' || c == ';' || c == '&')
        .find(|w| !w.is_empty())?
        .to_ascii_lowercase();
    let tip: &str = match flavor {
        Flavor::Sh => return None,
        Flavor::Pwsh | Flavor::WindowsPowerShell => {
            if flavor == Flavor::WindowsPowerShell
                && (command.contains("&&") || command.contains("||"))
                && (output.contains("&&") || output.contains("||"))
            {
                "Windows PowerShell 5.1 has no && or ||: separate commands with ; or use: cmd1; if ($?) { cmd2 }"
            } else {
                match first.as_str() {
                    "grep" | "egrep" => {
                        "Use Select-String -Pattern 'text' -Path files (or the grep tool)"
                    }
                    "head" => "Use Get-Content file -TotalCount N, or ... | Select-Object -First N",
                    "tail" => "Use Get-Content file -Tail N",
                    "touch" => "Use New-Item -ItemType File name (or Set-Content name '')",
                    "which" => "Use Get-Command name",
                    "export" => "Use $env:NAME = 'value'",
                    "sed" | "awk" => {
                        "Use -replace, Select-String or ForEach-Object in PowerShell, or the edit tool"
                    }
                    "dir" if command.contains("/") => {
                        "In PowerShell use Get-ChildItem (options like -Recurse, -Force, -Name)"
                    }
                    "find" if command.contains("-name") => {
                        "Use Get-ChildItem -Recurse -Filter '*.ext' (or the find tool)"
                    }
                    _ => return None,
                }
            }
        }
        Flavor::Cmd => match first.as_str() {
            "ls" => "In cmd use dir (or the ls tool)",
            "cat" => "In cmd use type",
            "grep" | "egrep" => "In cmd use findstr (or the grep tool)",
            "rm" => "In cmd use del (files) or rmdir /s /q (folders)",
            "cp" => "In cmd use copy (or xcopy for folders)",
            "mv" => "In cmd use move",
            "which" => "In cmd use where",
            "pwd" => "In cmd use cd",
            "touch" => "In cmd use: type nul > name",
            "head" | "tail" | "sed" | "awk" => {
                "cmd has no such command; use the read, grep or edit tools"
            }
            _ => return None,
        },
    };
    Some(format!("[Gardenwall: {tip}]"))
}

/// The folder in a form the shell can start in: `cmd.exe` refuses a
/// verbatim `\\?\C:\...` current folder ("UNC paths are not supported")
/// and falls back to the Windows folder, so the drive form is used.
/// Network (`\\?\UNC\...`) and other paths are kept as they are.
fn plain_dir(path: &Path) -> PathBuf {
    if cfg!(windows) {
        crate::display::drive_form(path)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod plain_dir_tests {
    use super::*;

    #[test]
    fn verbatim_drive_paths_become_plain() {
        if cfg!(windows) {
            assert_eq!(
                plain_dir(Path::new(r"\\?\C:\Users\x y\p")),
                PathBuf::from(r"C:\Users\x y\p")
            );
            assert_eq!(
                plain_dir(Path::new(r"\\?\UNC\srv\share")),
                PathBuf::from(r"\\?\UNC\srv\share")
            );
        }
        assert_eq!(plain_dir(Path::new("relative")), PathBuf::from("relative"));
    }

    #[test]
    fn bash_habits_get_a_pointer_to_this_shells_way() {
        let ps = |c: &str, o: &str| habit_hint(Flavor::WindowsPowerShell, c, o);
        assert!(ps("grep foo *.rs", "").unwrap().contains("Select-String"));
        assert!(
            ps("a && b", "token '&&' is not valid")
                .unwrap()
                .contains("has no &&")
        );
        assert!(ps("head -n 5 x", "").unwrap().contains("-TotalCount"));
        assert_eq!(ps("Get-Item x", "not found"), None);
        // PowerShell 7 has && and ||: no hint for them, but for bash tools.
        let ps7 = |c: &str, o: &str| habit_hint(Flavor::Pwsh, c, o);
        assert_eq!(ps7("a && b", "a: not recognized &&"), None);
        assert!(ps7("grep foo x", "").unwrap().contains("Select-String"));
        let cmd = |c: &str| habit_hint(Flavor::Cmd, c, "");
        assert!(cmd("ls -la").unwrap().contains("dir"));
        assert!(cmd("cat a.txt").unwrap().contains("type"));
        assert_eq!(cmd("dir"), None);
        assert_eq!(habit_hint(Flavor::Sh, "grep x", ""), None);
    }

    #[test]
    fn powershell_versions_are_checked() {
        assert_eq!(parse_pwsh_version("7.6.10\r\n").as_deref(), Some("7.6.10"));
        assert_eq!(
            parse_pwsh_version("7.7.0-rc.1").as_deref(),
            Some("7.7.0-rc.1")
        );
        assert_eq!(parse_pwsh_version("5.1.26100.1"), None);
        assert_eq!(parse_pwsh_version(""), None);
        assert_eq!(parse_pwsh_version("7.6.10 <script>"), None);
    }

    #[test]
    fn the_model_is_told_which_powershell_it_has() {
        let (_, g) = describe(Flavor::Pwsh, "PowerShell 7.6.10", &[], None);
        assert!(g[0].contains("PowerShell 7.6.10") && g[1].contains("&&"));
        assert!(!g[1].contains("no &&"));
        let (_, g) = describe(
            Flavor::WindowsPowerShell,
            "Windows PowerShell 5.1",
            &[],
            None,
        );
        assert!(g[0].contains("5.1") && g[1].contains("no &&"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn programs_are_started_by_their_quoted_path() {
        let p = Path::new(r"\\?\C:\Users\O'Neil Smith\gw\gardenwall-gitscan.exe");
        let args = ["--scan", "abc123"];
        assert_eq!(
            program_command(Flavor::Pwsh, p, &args).unwrap(),
            r"& 'C:\Users\O''Neil Smith\gw\gardenwall-gitscan.exe' --scan abc123"
        );
        assert_eq!(
            program_command(Flavor::Cmd, p, &args).unwrap(),
            r#""C:\Users\O'Neil Smith\gw\gardenwall-gitscan.exe" --scan abc123"#
        );
        // Unsafe to quote, relative, or arguments that aren't plain words.
        let pct = Path::new(r"C:\Users\a%PATH%b\x.exe");
        assert_eq!(program_command(Flavor::Cmd, pct, &args), None);
        assert!(program_command(Flavor::Pwsh, pct, &args).is_some());
        let curly = Path::new("C:\\Users\\a\u{2019}b\\x.exe");
        assert_eq!(
            program_command(Flavor::WindowsPowerShell, curly, &args),
            None
        );
        assert_eq!(
            program_command(Flavor::Pwsh, Path::new("x.exe"), &args),
            None
        );
        assert_eq!(program_command(Flavor::Pwsh, p, &["a;rm"]), None);
        assert_eq!(program_command(Flavor::Pwsh, p, &[""]), None);
    }

    #[test]
    fn a_browser_stopping_in_the_sandbox_is_explained() {
        let out = "[1121/221839:FATAL:crashpad_client_win.cc:331] Check failed: . CreateNamedPipe: Access is denied. (0x5)";
        assert!(browser_hint(out).is_some());
        assert!(browser_hint("Access is denied.").is_none());
    }
}
