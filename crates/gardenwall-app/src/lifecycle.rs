//! Starting and closing Gardenwall.
//!
//! Start: the window opens at once. Leftover sandbox permissions from the
//! last run are cleaned up on a thread of their own ([`startup_cleanup`]);
//! sandboxes wait for it inside the sandbox crate, so the cleanup never
//! takes away what a new session just added. Toolchain grants are kept
//! between runs (ADR-022 §8a), so the cleanup usually has little to do; it
//! used to revoke a Rust toolchain's 72,000 files on the main thread at
//! every start ("Not responding"), and then grant them again.
//!
//! Close: the X button closes straight away unless something is running
//! (an agent turn, a chat reply, a download). Then a native prompt (shown
//! by the core, not the page) lists what closing ends, with "End and close"
//! and "Cancel" ([`on_close_requested`]). Closing ends every session the
//! way lockdown does (commands and programs killed, cards withdrawn,
//! approvals ended) and lets their sandboxes go, within [`CLOSE_BUDGET`];
//! whatever is left is cleaned up at the next start, in the background.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// How long closing waits for sessions to end and their sandboxes to go.
/// The window is already gone; what isn't done by then is cleaned up at
/// the next start.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// Clean up what the last run left, then keep the toolchain and "folders
/// above" permissions sessions use, all off the main thread.
pub fn startup_cleanup(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        #[cfg(windows)]
        cleanup_leftovers(&state);
        state.cleaning_up.store(false, Ordering::SeqCst);
        crate::toolchains::warm(&state);
        crate::parent_folders::warm(&state);
    });
}

#[cfg(windows)]
fn cleanup_leftovers(state: &AppState) {
    use std::path::{Path, PathBuf};
    // Toolchain folders the app keeps granted anyway: a leftover grant on
    // one is kept (recorded as kept) instead of revoked and granted again.
    // Found only when a leftover agent grant needs the answer (finding the
    // toolchains runs Visual Studio's locator), so a start after a clean
    // close, with nothing left over, doesn't wait for it.
    let wanted: std::cell::OnceCell<Vec<PathBuf>> = std::cell::OnceCell::new();
    let profile = gardenwall_agent::sandbox_shell::AGENT_PROFILE;
    let keep = |p: &str, path: &Path| {
        if p != profile {
            return None;
        }
        wanted
            .get_or_init(|| {
                crate::toolchains::wanted_folders(state)
                    .iter()
                    .filter_map(|f| std::fs::canonicalize(f).ok())
                    .collect()
            })
            .iter()
            .any(|w| w == path)
            .then_some(gardenwall_sandbox::Access::ReadExecute)
    };
    match gardenwall_sandbox::reconcile_grant_journals_keeping(
        &state.paths.grants,
        Some(&state.paths.kept_grants),
        &keep,
    ) {
        Ok(report) => {
            if report.revoked > 0 {
                state.add_notice(format!(
                    "Removed {} sandbox file permission(s) left behind when Gardenwall last closed.",
                    report.revoked
                ));
            }
            if report.kept > 0 {
                state.add_notice(format!(
                    "{} leftover sandbox permission record(s) could not be cleaned up and will be retried next start.",
                    report.kept
                ));
            }
        }
        Err(e) => state.add_notice(format!(
            "Could not check for leftover sandbox permissions: {e}"
        )),
    }
    // Scratch folders moved aside at start (`AppState::new`).
    if let Ok(entries) = std::fs::read_dir(&state.paths.root) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("agent-scratch.old-")
            {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// What closing now would cut off, in words for the prompt. Empty when
/// nothing is running.
pub fn running_work(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    let working: Vec<String> = crate::agent::live_sessions(state)
        .iter()
        .filter(|s| s.busy.load(Ordering::SeqCst))
        .map(|s| {
            let title = s.card.lock().map(|c| c.1.clone()).unwrap_or_default();
            if title.trim().is_empty() {
                "an unnamed session".to_string()
            } else {
                format!("\"{}\"", title.trim())
            }
        })
        .collect();
    match working.len() {
        0 => {}
        1 => out.push(format!(
            "The agent's work in {} (its command is stopped, and a card waiting for you is withdrawn)",
            working[0]
        )),
        n => out.push(format!(
            "The agent's work in {n} sessions: {} (their commands are stopped, and cards waiting for you are withdrawn)",
            working.join(", ")
        )),
    }
    let replies = state.chats.lock().map(|c| c.len()).unwrap_or(0);
    match replies {
        0 => {}
        1 => out.push("A chat reply being written".into()),
        n => out.push(format!("{n} chat replies being written")),
    }
    let downloads = state.active_downloads.lock().map(|d| d.len()).unwrap_or(0);
    match downloads {
        0 => {}
        1 => out.push("A model download (kept, so you can resume it)".into()),
        n => out.push(format!(
            "{n} model downloads (kept, so you can resume them)"
        )),
    }
    out
}

/// One prompt at a time.
static PROMPT_OPEN: AtomicBool = AtomicBool::new(false);

/// The X button (or Alt+F4): close straight away when nothing runs;
/// otherwise ask first, in a native prompt.
pub fn on_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let state = window.state::<AppState>();
    if state.closing.load(Ordering::SeqCst) {
        return;
    }
    let work = running_work(&state);
    if work.is_empty() {
        state.closing.store(true, Ordering::SeqCst);
        return;
    }
    api.prevent_close();
    if PROMPT_OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = window.app_handle().clone();
    std::thread::spawn(move || {
        const END: &str = "End and close";
        let description = format!(
            "Closing Gardenwall now ends:\n\n• {}\n\nEverything else is saved.",
            work.join("\n• ")
        );
        let end_and_close = confirm_close(&app, description, END);
        PROMPT_OPEN.store(false, Ordering::SeqCst);
        if end_and_close {
            app.state::<AppState>()
                .closing
                .store(true, Ordering::SeqCst);
            app.exit(0);
        }
    });
}

/// Ask whether to end what runs and close; true for "End and close".
/// Windows uses rfd directly, as before.
#[cfg(windows)]
fn confirm_close(_app: &AppHandle, description: String, end: &str) -> bool {
    let choice = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("Close Gardenwall?")
        .set_description(description)
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            end.into(),
            "Cancel".into(),
        ))
        .show();
    choice == rfd::MessageDialogResult::Custom(end.into())
}

/// Other systems: the same prompt through the dialog plugin (rfd is a
/// Windows-only dependency here).
#[cfg(not(windows))]
fn confirm_close(app: &AppHandle, description: String, end: &str) -> bool {
    confirm_close_with_plugin(app, description, end)
}

/// Compiled on Windows too, so it is type-checked where the app is built.
#[cfg_attr(windows, allow(dead_code))]
fn confirm_close_with_plugin(app: &AppHandle, description: String, end: &str) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    app.dialog()
        .message(description)
        .title("Close Gardenwall?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            end.into(),
            "Cancel".into(),
        ))
        .blocking_show()
}

/// The app is exiting (the window is already gone): end every session,
/// stop chat replies, let the sandboxes go (within [`CLOSE_BUDGET`]), and
/// stop the model's server.
pub fn shut_down(state: &AppState) {
    state.closing.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let sessions = crate::agent::end_all_sessions(state);
    if let Ok(chats) = state.chats.lock() {
        for stop in chats.values() {
            stop.store(true, Ordering::SeqCst);
        }
    }
    state.net_stop.stop_all();
    while sessions.iter().any(|s| s.busy.load(Ordering::SeqCst)) && started.elapsed() < CLOSE_BUDGET
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    // Each session's sandbox goes with its last holder; revoking a big
    // project folder's permission can take a while, so it runs on its own
    // thread and closing waits only for what's left of the budget. What
    // isn't finished is cleaned up at the next start.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(sessions);
        let _ = done_tx.send(());
    });
    let _ = done_rx.recv_timeout(CLOSE_BUDGET.saturating_sub(started.elapsed()));
    tauri::async_runtime::block_on(crate::state::shutdown_servers(state));
    crate::diagnostics::end_session(&state.paths.root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_running_means_no_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        assert!(running_work(&state).is_empty());
        state
            .chats
            .lock()
            .unwrap()
            .insert(1, std::sync::Arc::new(AtomicBool::new(false)));
        state
            .active_downloads
            .lock()
            .unwrap()
            .extend(["a".to_string(), "b".to_string()]);
        let work = running_work(&state);
        assert_eq!(
            work,
            vec![
                "A chat reply being written".to_string(),
                "2 model downloads (kept, so you can resume them)".to_string()
            ]
        );
    }
}
