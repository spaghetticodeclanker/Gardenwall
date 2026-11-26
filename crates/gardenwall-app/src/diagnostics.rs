//! "Copy diagnostics" (release plan feature 2).
//!
//! A plain-text report a user can hand over when something fails. Built by
//! the core, shown to the user in full before they copy or save it, and
//! never sent anywhere by Gardenwall.
//!
//! - **What's in it:** versions, Windows, WebView2, GPU, driver and memory,
//!   the runtime and its test result, the sandbox state, settings that
//!   change behaviour (as values, never paths, addresses or keys), the
//!   loaded model (its name too, by design), where its weights
//!   went, the model server's recent log lines, recent errors, and whether
//!   the previous session closed normally.
//! - **Never in it:** prompts, replies, tool results, file contents, chat or
//!   session titles, MCP server names or arguments. Free text that the
//!   report does carry (errors, notices, log lines) passes [`redact`]
//!   first: file paths, web addresses, email addresses, the user's name and
//!   home folder are replaced, and a line that looks like it holds a secret
//!   is dropped.
//! - **On disk** (F2.3): only the session marker, `session.marker` in the
//!   data folder: written at start, removed at a normal exit, holding the
//!   version, the start time and whether the app panicked (never a
//!   message). Errors are kept in memory and gone at exit.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::{AppState, now_ms};

type CmdResult<T> = Result<T, String>;

const MAX_ERRORS: usize = 40;
const MAX_ERROR_CHARS: usize = 600;
const MAX_LOG_LINES: usize = 60;
const MARKER_FILE: &str = "session.marker";

/// Recent errors (memory only) and what the last start found.
#[derive(Default)]
pub struct Diagnostics {
    errors: Mutex<VecDeque<ErrorEntry>>,
    previous: Mutex<Option<Marker>>,
    /// The report last shown to the user: Save writes exactly this.
    last_report: Mutex<Option<String>>,
}

#[derive(Clone)]
struct ErrorEntry {
    at_ms: i64,
    area: &'static str,
    message: String,
}

/// The session marker. The previous session's marker still being there at
/// start means it didn't reach a normal exit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Marker {
    version: String,
    started_ms: i64,
    #[serde(default)]
    panicked: bool,
}

// ---------- redaction ----------

/// What to replace besides the generic patterns: the user's home folder,
/// Gardenwall's data folder and the user name.
#[derive(Default, Clone)]
pub struct Known {
    pub home: Option<String>,
    pub data: Option<String>,
    pub user: Option<String>,
}

impl Known {
    pub fn from_env(data: &Path) -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .ok();
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok();
        Self {
            home,
            data: Some(data.display().to_string()),
            user,
        }
    }
}

fn patterns() -> &'static [(Regex, &'static str)] {
    static RE: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RE.get_or_init(|| {
        [
            // URLs first, so a path inside one isn't half-replaced.
            (
                r"(?i)\b(?:https?|ftp|file|wss?)://[^\s<>]+",
                "<web address>",
            ),
            (r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", "<email>"),
            // \\?\C:\…, \\server\share\…
            (r#"\\\\[^\s"'<>|]+"#, "<path>"),
            // C:\… and C:/…
            (r#"(?i)\b[a-z]:[\\/][^\s"'<>|]*"#, "<path>"),
            // Unix home and mount folders.
            (
                r#"(?:/home|/Users|/root|/mnt|/media|/tmp|/var/folders)/[^\s"'<>|]*"#,
                "<path>",
            ),
        ]
        .into_iter()
        .map(|(re, with)| (Regex::new(re).expect("pattern compiles"), with))
        .collect()
    })
}

fn replace_case_insensitive(text: &str, needle: &str, with: &str) -> String {
    if needle.chars().count() < 3 {
        return text.to_string();
    }
    let re = Regex::new(&format!("(?i){}", regex::escape(needle))).expect("escaped");
    re.replace_all(text, with).into_owned()
}

/// Free text made safe to share: hidden characters collapsed, a line that
/// looks like it holds a secret dropped, the known folders and user name
/// replaced, then any remaining path, web address or email address.
pub fn redact(text: &str, known: &Known) -> String {
    let (text, _) = gardenwall_policy::protect::hide_invisible(text);
    text.lines()
        .map(|line| {
            let kinds = gardenwall_egress::scan::find_secrets(line);
            if !kinds.is_empty() {
                return format!(
                    "[line removed: it looks like it holds a secret ({})]",
                    kinds.join(", ")
                );
            }
            let mut out = line.to_string();
            for (value, with) in [(&known.data, "<data folder>"), (&known.home, "<home>")] {
                if let Some(v) = value {
                    out = replace_case_insensitive(&out, v, with);
                }
            }
            for (re, with) in patterns() {
                out = re.replace_all(&out, *with).into_owned();
            }
            if let Some(user) = &known.user {
                out = replace_case_insensitive(&out, user, "<user>");
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut t: String = text.chars().take(max).collect();
    t.push('…');
    t
}

// ---------- recording ----------

/// Remember an error for the report (redacted now, memory only).
pub fn record(state: &AppState, area: &'static str, message: &str) {
    let known = Known::from_env(&state.paths.root);
    let message = truncate(&redact(message, &known), MAX_ERROR_CHARS);
    if let Ok(mut q) = state.diagnostics.errors.lock() {
        if q.len() == MAX_ERRORS {
            q.pop_front();
        }
        q.push_back(ErrorEntry {
            at_ms: now_ms(),
            area,
            message,
        });
    }
}

/// Pass a command's result through, remembering its error.
pub fn noted<T>(state: &AppState, area: &'static str, result: CmdResult<T>) -> CmdResult<T> {
    if let Err(e) = &result {
        record(state, area, e);
    }
    result
}

// ---------- session marker ----------

fn marker_path(data: &Path) -> PathBuf {
    data.join(MARKER_FILE)
}

/// At start: read the previous session's marker (still there = it didn't
/// close normally), then write this session's.
pub fn start_session(state: &AppState) {
    let path = marker_path(&state.paths.root);
    let previous = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Marker>(&b).ok());
    if let Ok(mut p) = state.diagnostics.previous.lock() {
        *p = previous;
    }
    let marker = Marker {
        version: env!("CARGO_PKG_VERSION").into(),
        started_ms: now_ms(),
        panicked: false,
    };
    if let Ok(bytes) = serde_json::to_vec(&marker) {
        let _ = std::fs::write(&path, bytes);
    }
    install_panic_hook(path, marker);
}

/// A panic marks the session (no message, no location) and then runs the
/// default hook as before.
fn install_panic_hook(path: PathBuf, marker: Marker) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let marked = Marker {
            panicked: true,
            ..marker.clone()
        };
        if let Ok(bytes) = serde_json::to_vec(&marked) {
            let _ = std::fs::write(&path, bytes);
        }
        previous(info);
    }));
}

/// At a normal exit (and before an update's installer closes the app).
pub fn end_session(data: &Path) {
    let _ = std::fs::remove_file(marker_path(data));
}

fn previous_line(previous: Option<&Marker>) -> String {
    match previous {
        None => "Previous session: closed normally (or this is the first start).".into(),
        Some(m) => format!(
            "Previous session: did not close normally{} (version {}, started {}).",
            if m.panicked {
                " - Gardenwall stopped on an internal error"
            } else {
                " - it was ended without closing (for example by Task Manager, a crash or a power loss)"
            },
            m.version,
            utc(m.started_ms)
        ),
    }
}

// ---------- the report ----------

/// A UTC time as YYYY-MM-DD HH:MM UTC (no time-zone database needed).
pub(crate) fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// Windows' version string (`ver`), run without a window.
#[cfg(windows)]
fn windows_version() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // By its full path: a bare name is looked up in Gardenwall's own
    // folder first.
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let cmd = std::path::Path::new(&system)
        .join("System32")
        .join("cmd.exe");
    let out = std::process::Command::new(cmd)
        .args(["/d", "/c", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then(|| truncate(&text, 120))
}

#[cfg(not(windows))]
fn windows_version() -> Option<String> {
    None
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

/// Everything the report shows about the loaded model, gathered under the
/// engine lock.
struct ModelFacts {
    name: String,
    source: String,
    size: u64,
    architecture: String,
    trained_context: Option<u64>,
    context: u64,
    tools: bool,
    runtime: String,
    build: Option<String>,
    sandboxed: bool,
    vision: bool,
    auto_mtp: Option<String>,
    placement: String,
    log: Vec<String>,
}

async fn model_facts(state: &AppState) -> Option<ModelFacts> {
    let facts = crate::commands::engine_now(state, |e| {
        e.map(|l| {
            let p = l.server.logs().placement();
            (
                l.sha256.clone(),
                l.display_name.clone(),
                l.props.context_window,
                l.props.supports_tools,
                l.props.build.clone(),
                l.runtime_id.clone(),
                matches!(
                    l.server.isolation(),
                    gardenwall_inference::supervisor::IsolationStatus::Sandboxed
                ),
                l.vision,
                l.auto_mtp.map(|m| format!("{m:?}")),
                format!(
                    "{:.1} GiB on the GPU, {:.1} GiB in system RAM{}{}",
                    p.gpu_mib / 1024.0,
                    p.ram_mib / 1024.0,
                    p.layers
                        .map(|(a, b)| format!(", {a}/{b} layers offloaded"))
                        .unwrap_or_default(),
                    p.overflowing_layers
                        .map(|n| format!(", {n} layers overflowing"))
                        .unwrap_or_default(),
                ),
                l.server.logs().lines(),
            )
        })
    })
    .await
    .flatten()?;
    let (sha, name, context, tools, build, runtime, sandboxed, vision, auto_mtp, placement, log) =
        facts;
    let record = state.library.get(&sha);
    let source = match record.as_ref().map(|r| &r.source) {
        Some(gardenwall_models::intake::ModelSource::Hub {
            repo, path, commit, ..
        }) => format!(
            "{repo} / {path} (commit {})",
            commit.chars().take(12).collect::<String>()
        ),
        Some(gardenwall_models::intake::ModelSource::Local { original_name, .. }) => {
            format!("imported file {original_name}")
        }
        None => "unknown".into(),
    };
    Some(ModelFacts {
        name,
        source,
        size: record.as_ref().map_or(0, |r| r.size),
        architecture: record
            .as_ref()
            .map_or_else(|| "unknown".into(), |r| r.architecture.clone()),
        trained_context: record.as_ref().and_then(|r| r.context_length),
        context,
        tools,
        runtime,
        build,
        sandboxed,
        vision,
        auto_mtp,
        placement,
        log,
    })
}

/// Build the report: plain text, every free-text part redacted.
pub async fn build_report(state: &AppState) -> String {
    let known = Known::from_env(&state.paths.root);
    let settings = state.settings();
    let saved = state.saved_settings();
    let mut r = String::new();
    let mut line = |s: String| {
        r.push_str(&s);
        r.push('\n');
    };

    line("Gardenwall diagnostics".into());
    line(format!("Made: {}", utc(now_ms())));
    line(
        "Review it before sharing; nothing here was sent anywhere. A loaded model is named; remove the name if you prefer."
            .into(),
    );
    line(String::new());

    line("## Versions".into());
    line(format!(
        "Gardenwall: {} ({} build)",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "development"
        } else {
            "release"
        }
    ));
    let (os, webview, gpu, ram) = tauri::async_runtime::spawn_blocking(|| {
        (
            windows_version(),
            tauri::webview_version().ok(),
            gardenwall_gpu::report().ok(),
            gardenwall_gpu::system_memory().ok(),
        )
    })
    .await
    .unwrap_or((None, None, None, None));
    line(format!(
        "System: {} {}{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        os.map(|v| format!(", {v}")).unwrap_or_default()
    ));
    line(format!(
        "WebView2: {}",
        webview.unwrap_or_else(|| "not found".into())
    ));
    line(String::new());

    line("## Hardware".into());
    match gpu.as_ref() {
        Some(g) => {
            let v = crate::speed::gpu_view(g, None);
            line(format!(
                "GPU: {}, {} dedicated{}",
                v.adapter,
                gib(v.total_bytes),
                v.free_bytes
                    .map(|f| format!(", {} free", gib(f)))
                    .unwrap_or_default()
            ));
        }
        None => line("GPU: not read".into()),
    }
    let (driver_notice, driver_current) =
        tauri::async_runtime::spawn_blocking(crate::gpu_driver::status)
            .await
            .unwrap_or((None, None));
    match (driver_notice, driver_current) {
        (Some(n), _) => line(format!(
            "Graphics driver: {} (older than the security fix in {})",
            n.installed, n.fixed
        )),
        (None, Some(c)) => line(format!(
            "Graphics driver: {} (has the latest security fix)",
            c.installed
        )),
        _ => line("Graphics driver: not checked (NVIDIA only)".into()),
    }
    if let Some((total, available)) = ram {
        line(format!(
            "System RAM: {} total, {} available",
            gib(total),
            gib(available)
        ));
    }
    line(String::new());

    line("## Runtime".into());
    let report = state.update_report.lock().ok().and_then(|r| r.clone());
    match report
        .as_ref()
        .and_then(|r| r.runtimes.iter().find(|x| x.active))
    {
        Some(rt) => {
            line(format!("Active runtime: {} ({})", rt.id, rt.backend));
            match &rt.smoke {
                Some(s) => line(format!(
                    "Runtime test: answer {}, tool call {}{}",
                    if s.completion { "ok" } else { "failed" },
                    if s.tool_call { "ok" } else { "failed" },
                    s.error
                        .as_deref()
                        .map(|e| format!(", error: {}", truncate(&redact(e, &known), 200)))
                        .unwrap_or_default()
                )),
                None => line("Runtime test: not run".into()),
            }
            for a in &rt.advisories {
                line(format!("Security notice: {}", a.id));
            }
        }
        None => line(format!(
            "Active runtime: {}",
            settings
                .active_runtime
                .as_deref()
                .unwrap_or("not chosen (the newest installed build is used)")
        )),
    }
    line(String::new());

    line("## Settings that change behaviour".into());
    line(format!(
        "Offline mode: {}; lockdown: {}",
        on_off(saved.offline),
        on_off(saved.lockdown)
    ));
    line(format!(
        "Agent shell: {:?}; Python: {}; code mode: {}; question cards: {}; Run this yourself cards: {}",
        settings.agent.shell,
        on_off(settings.agent.python),
        on_off(settings.agent.code_mode),
        on_off(settings.agent.ask_user),
        on_off(settings.agent.run_requests)
    ));
    line(format!(
        "Sessions: {:?}; parallel slots: {}; compaction: {}",
        settings.agent.sessions,
        settings.agent.parallel_slots,
        on_off(settings.agent.compaction)
    ));
    line(format!(
        "Web search provider: {:?}; default mode: {:?}",
        settings.web.provider, settings.web.default_mode
    ));
    line(format!(
        "MCP servers: {}; agent tool folders: {}",
        settings.mcp_servers.len(),
        settings.agent_tool_folders.len()
    ));
    line(format!("Update checks: {:?}", settings.update_schedule));
    line(String::new());

    line("## Model".into());
    let model = model_facts(state).await;
    match &model {
        Some(m) => {
            line(format!("Loaded: {}", m.name));
            line(format!("Source: {}", redact(&m.source, &known)));
            line(format!(
                "File: {}, architecture {}, trained context {}",
                gib(m.size),
                m.architecture,
                m.trained_context
                    .map_or_else(|| "unknown".into(), |c| c.to_string())
            ));
            line(format!(
                "Context: {} tokens; tool calling: {}; vision: {}; faster generation (MTP): {}",
                m.context,
                if m.tools { "yes" } else { "no" },
                if m.vision { "yes" } else { "no" },
                m.auto_mtp.as_deref().unwrap_or("not automatic")
            ));
            line(format!(
                "Runtime: {}{}; sandboxed: {}",
                m.runtime,
                m.build
                    .as_deref()
                    .map(|b| format!(" (build {b})"))
                    .unwrap_or_default(),
                if m.sandboxed { "yes" } else { "NO" }
            ));
            line(format!("Weights: {}", m.placement));
        }
        None => line("No model loaded.".into()),
    }
    line(String::new());

    let (log_title, log) = match model {
        Some(m) => ("## Model server log (last lines)", m.log),
        None => (
            "## Model server log of the last start that failed",
            gardenwall_inference::supervisor::last_failed_start_log(),
        ),
    };
    line(log_title.into());
    if log.is_empty() {
        line("(none)".into());
    } else {
        let start = log.len().saturating_sub(MAX_LOG_LINES);
        for l in &log[start..] {
            line(redact(l, &known));
        }
    }
    line(String::new());

    line("## Recent errors".into());
    let errors: Vec<ErrorEntry> = state
        .diagnostics
        .errors
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default();
    if errors.is_empty() {
        line("(none since Gardenwall started)".into());
    }
    for e in errors {
        line(format!("{}  {}: {}", utc(e.at_ms), e.area, e.message));
    }
    let notices: Vec<String> = state.notices.lock().map(|n| n.clone()).unwrap_or_default();
    if !notices.is_empty() {
        line(String::new());
        line("## Notices shown".into());
        for n in notices {
            line(truncate(&redact(&n, &known), MAX_ERROR_CHARS));
        }
    }
    line(String::new());
    let previous = state
        .diagnostics
        .previous
        .lock()
        .ok()
        .and_then(|p| p.clone());
    line(previous_line(previous.as_ref()));
    r
}

/// Build the report for the preview. The text shown is kept, so Save
/// writes exactly what the user saw.
#[tauri::command]
pub async fn diagnostics_report(state: State<'_, AppState>) -> CmdResult<String> {
    let report = build_report(&state).await;
    if let Ok(mut last) = state.diagnostics.last_report.lock() {
        *last = Some(report.clone());
    }
    Ok(report)
}

/// Save the report last shown through the native save dialog. The text
/// comes from the core, never from the webview.
#[tauri::command]
pub async fn save_diagnostics(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<bool> {
    use tauri_plugin_dialog::DialogExt;
    let report = state
        .diagnostics
        .last_report
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .ok_or("Open the report first.")?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name("gardenwall-diagnostics.txt")
        .add_filter("Text", &["txt"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(false);
    };
    let path = path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, report).map_err(|e| format!("Couldn't save the report: {e}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Known {
        Known {
            home: Some(r"C:\Users\Avery Example".into()),
            data: Some(r"C:\Users\Avery Example\AppData\Local\ai.gardenwall.desktop".into()),
            user: Some("Avery Example".into()),
        }
    }

    #[test]
    fn paths_names_addresses_and_emails_are_replaced() {
        let k = known();
        let out = redact(
            r"llama_model_load: loading model from C:\Users\Avery Example\AppData\Local\ai.gardenwall.desktop\models\abc.gguf",
            &k,
        );
        assert!(!out.contains("Avery"), "{out}");
        assert!(out.contains("<data folder>"), "{out}");

        let out = redact(
            r"failed to open D:\Projects\secret-plan\notes.md: denied",
            &k,
        );
        assert_eq!(out, "failed to open <path> denied");

        let out = redact(
            r"error at \\?\C:\work\x and \\server\share\y and /home/avery/z",
            &k,
        );
        assert!(
            !out.contains("work") && !out.contains("server") && !out.contains("avery"),
            "{out}"
        );

        let out = redact(
            "couldn't reach https://searx.example.lan:8888/search?q=private",
            &k,
        );
        assert_eq!(out, "couldn't reach <web address>");

        let out = redact("mail avery@example.com about it", &k);
        assert_eq!(out, "mail <email> about it");

        let out = redact("hello Avery Example, the session for avery example", &k);
        assert!(!out.to_lowercase().contains("avery"), "{out}");
    }

    #[test]
    fn a_line_with_a_secret_is_dropped_whole() {
        let out = redact(
            "ok line\nAuthorization: Bearer ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\nlast",
            &Known::default(),
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "ok line");
        assert!(lines[1].starts_with("[line removed"), "{out}");
        assert!(!out.contains("ghp_"));
        assert_eq!(lines[2], "last");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_report_carries_errors_and_notices_only_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        record(
            &state,
            "model load",
            r"couldn't open D:\Clients\Zebra-7731\plan.gguf",
        );
        record(
            &state,
            "update check",
            "token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ leaked",
        );
        state.add_notice("Searched https://intranet.example/zebra-7731 for you".into());
        let report = build_report(&state).await;
        if std::env::var_os("GARDENWALL_PRINT_REPORT").is_some() {
            println!("{report}");
        }
        for section in [
            "## Versions",
            "## Hardware",
            "## Runtime",
            "## Settings that change behaviour",
            "## Model",
            "## Recent errors",
            "## Notices shown",
        ] {
            assert!(report.contains(section), "missing {section}:\n{report}");
        }
        assert!(
            report.contains("model load: couldn't open <path>"),
            "{report}"
        );
        assert!(!report.contains("Zebra"), "{report}");
        assert!(!report.contains("zebra"), "{report}");
        assert!(!report.contains("sk-ant-"), "{report}");
        assert!(report.contains("No model loaded."));
    }

    #[test]
    fn hidden_characters_are_collapsed() {
        let out = redact("a\u{200B}\u{200B}\u{200B}b", &Known::default());
        assert!(!out.contains('\u{200B}'));
    }

    #[test]
    fn short_names_are_not_replaced_everywhere() {
        let k = Known {
            user: Some("al".into()),
            ..Known::default()
        };
        assert_eq!(redact("total all", &k), "total all");
    }

    #[test]
    fn utc_dates_are_right() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_795_363_200_000), "2026-11-22 16:00 UTC");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn the_session_marker_reports_an_unclean_exit_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = marker_path(dir.path());
        assert!(previous_line(None).contains("closed normally"));
        let m = Marker {
            version: "1.0.0".into(),
            started_ms: 0,
            panicked: true,
        };
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let read: Marker = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, m);
        let text = previous_line(Some(&read));
        assert!(text.contains("did not close normally") && text.contains("internal error"));
        end_session(dir.path());
        assert!(!path.exists());
        // The marker holds no message: only these three fields.
        let fields = serde_json::to_value(&m).unwrap();
        assert_eq!(fields.as_object().unwrap().len(), 3);
    }
}
