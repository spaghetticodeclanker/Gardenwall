//! Lockdown mode as an emergency stop (GW-019
//! ADR-011 §2.6a).
//!
//! Turning it on takes one confirmation in the window, then, in an order
//! that fails closed:
//! 1. Lockdown is put in force (and saved) first, so nothing new starts even
//!    if a later step fails: no agent or research session (chat only), the
//!    settings in force are the narrow ones ([`crate::state::Settings::locked_down`]).
//! 2. Web requests in flight and model downloads are stopped
//!    ([`crate::web::NetStop`]; downloads see offline mode).
//! 3. Every agent session ends: its turn is stopped, the reply being written
//!    cut, the running command and every program in its sandbox killed, its
//!    cards withdrawn, its approvals and grants ended.
//! 4. Chats' Web switches end, research assistants' approvals end, and chat
//!    replies being written stop.
//! 5. The toolchain and "folders above" permissions the app kept for the
//!    agent's sandbox are taken back.
//! 6. The model is unloaded.
//! 7. Gardenwall waits (up to [`SETTLE`]) until every session's turn has
//!    really ended, then reports what it did and anything that wouldn't
//!    stop. Lockdown stays on whatever the report says.
//!
//! Turning it off needs the word [`UNLOCK_WORD`] typed in the window, and
//! nothing restarts by itself: sessions, servers and the model stay off
//! until the user starts them.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// What the user types to turn lockdown off.
pub const UNLOCK_WORD: &str = "UNLOCK";

/// How long to wait for running turns to end before reporting.
const SETTLE: Duration = Duration::from_secs(10);

/// What turning lockdown on did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockdownReport {
    /// What was stopped ("2 agent sessions", "the loaded model").
    pub stopped: Vec<String>,
    /// What couldn't be stopped or saved. Lockdown is on regardless.
    pub problems: Vec<String>,
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Turn lockdown on and stop everything (see the module notes).
pub async fn engage(state: &AppState) -> LockdownReport {
    let mut report = LockdownReport::default();

    // 1. In force first. (The settings change in memory before they are
    // written, so lockdown holds even if saving fails.)
    if let Err(e) = state.update_settings(|s| s.lockdown = true) {
        report.problems.push(format!(
            "Lockdown is on, but it couldn't be saved ({e}): it won't hold after Gardenwall restarts."
        ));
    }

    // 2. The network: requests in flight are cancelled, and no web access
    // made earlier can start another. Downloads stop at their next progress
    // report (offline mode is now in force) and are kept for resuming.
    state.net_stop.stop_all();

    // 3. Agent sessions.
    let sessions = crate::agent::end_all_sessions(state);
    if !sessions.is_empty() {
        report.stopped.push(count(
            sessions.len(),
            "agent session (its commands and programs killed, its cards withdrawn)",
            "agent sessions (their commands and programs killed, their cards withdrawn)",
        ));
    }

    // 4. Chats' Web switches, research assistants, chat replies.
    let webs: Vec<String> = state
        .chat_webs
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for id in &webs {
        crate::chat_web::end(state, id);
    }
    if !webs.is_empty() {
        report.stopped.push(count(
            webs.len(),
            "chat's Web switch",
            "chats' Web switches",
        ));
    }
    let research: Vec<gardenwall_store::approvals::SessionId> = state
        .research_sessions
        .lock()
        .map(|mut m| m.drain().map(|(id, _)| id).collect())
        .unwrap_or_default();
    if !research.is_empty() {
        if let Ok(mut store) = state.approvals.lock() {
            for id in &research {
                let _ = store.end_session(*id, crate::state::now_ms());
            }
        }
        if let Ok(mut g) = state.egress_grants.lock() {
            for id in &research {
                g.remove(id);
            }
        }
    }
    let replies = state
        .chats
        .lock()
        .map(|chats| {
            chats
                .values()
                .filter(|stop| !stop.swap(true, Ordering::SeqCst))
                .count()
        })
        .unwrap_or(0);
    if replies > 0 {
        report.stopped.push(count(
            replies,
            "chat reply being written",
            "chat replies being written",
        ));
    }

    // 5. Permissions the app kept for the agent's sandbox.
    #[cfg(windows)]
    {
        let tools = state
            .tool_holds
            .lock()
            .map(|mut h| std::mem::take(&mut *h))
            .unwrap_or_default();
        let parents = state
            .parent_holds
            .lock()
            .map(|mut h| std::mem::take(&mut *h))
            .unwrap_or_default();
        if !tools.is_empty() || !parents.is_empty() {
            report.stopped.push(
                "the toolchain and \"folders above the project\" permissions kept for the agent's sandbox"
                    .into(),
            );
        }
        drop(parents);
        // Revoking a toolchain folder's permission is a pass over all its
        // files (tens of seconds for Rust): on its own thread, so the rest
        // of the stop doesn't wait. No session can use it meanwhile (they
        // were all ended above, and none starts in lockdown).
        if !tools.is_empty() {
            std::thread::spawn(move || drop(tools));
        }
    }

    // 6. The model. A generation holding the engine is cut by the stops
    // above; if it doesn't let go in time, say so. Steps 6 and 7 share one
    // deadline, so the whole stop waits at most SETTLE.
    let deadline = tokio::time::Instant::now() + SETTLE;
    match tokio::time::timeout_at(deadline, state.engine.lock()).await {
        Ok(mut engine) => {
            if let Some(loaded) = engine.take() {
                loaded.server.shutdown().await;
                report.stopped.push("the loaded model (unloaded)".into());
            }
        }
        Err(_) => report.problems.push(
            "The model couldn't be unloaded yet (it was busy). Unload it on the Models page."
                .into(),
        ),
    }

    // 7. Wait until every session's turn has ended, so its sandbox goes.
    while sessions.iter().any(|s| s.busy.load(Ordering::SeqCst))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let still = sessions
        .iter()
        .filter(|s| s.busy.load(Ordering::SeqCst))
        .count();
    if still > 0 {
        report.problems.push(format!(
            "{} hadn't finished stopping after {} seconds. {} commands were killed and can't run again; it ends as soon as its current step returns.",
            count(still, "agent session", "agent sessions"),
            SETTLE.as_secs(),
            if still == 1 { "Its" } else { "Their" },
        ));
    }
    drop(sessions);
    report
}

/// Turn lockdown on (one confirmation in the window), or off (with
/// [`UNLOCK_WORD`] typed). Turning it on returns what it stopped.
#[tauri::command]
pub async fn set_lockdown(
    state: State<'_, AppState>,
    lockdown: bool,
    confirm: Option<String>,
) -> CmdResult<Option<LockdownReport>> {
    if lockdown {
        return Ok(Some(engage(&state).await));
    }
    unlock(&state, confirm.as_deref())?;
    Ok(None)
}

/// Turn lockdown off, only with [`UNLOCK_WORD`] (exactly, around spaces
/// aside). Nothing restarts.
fn unlock(state: &AppState, confirm: Option<&str>) -> CmdResult<()> {
    if confirm.map(str::trim) != Some(UNLOCK_WORD) {
        return Err(format!("Type {UNLOCK_WORD} to turn lockdown off."));
    }
    state
        .update_settings(|s| s.lockdown = false)
        .map_err(|e| e.to_string())?;
    // Lockdown took back the toolchain and "folders above" permissions kept
    // for the agent's sandbox: keep them again, so sessions start without
    // the grant check and Ruby and Gradle work as set up.
    if let Some(app) = state.app.get() {
        crate::toolchains::warm_in_background(app);
        crate::parent_folders::warm_in_background(app);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count(1, "session", "sessions"), "1 session");
        assert_eq!(count(3, "session", "sessions"), "3 sessions");
    }

    /// Lockdown ends an open session (its approval session too), stops the
    /// network, refuses new agent and research sessions, and turns off only
    /// with the exact word (GW-019).
    #[tokio::test]
    async fn lockdown_ends_sessions_refuses_new_ones_and_needs_the_word() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().join("data")).unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "x").unwrap();
        let project = std::fs::canonicalize(&project).unwrap();

        let view = crate::agent::begin(&state, vec![project.clone()], None, None, false, None)
            .await
            .expect("a session starts before lockdown");
        let live = crate::agent::live_sessions(&state);
        assert_eq!(live.len(), 1);
        let approval = live[0].approval_session;
        drop(live);
        let net = state.net_stop.current();

        let report = engage(&state).await;
        assert!(state.settings().lockdown && state.settings().offline);
        assert!(
            state.saved_settings().lockdown,
            "saved, so it holds after a restart"
        );
        assert!(crate::agent::live_sessions(&state).is_empty());
        assert!(
            report
                .stopped
                .iter()
                .any(|s| s.starts_with("1 agent session")),
            "{report:?}"
        );
        assert!(report.problems.is_empty(), "{report:?}");
        assert!(state.net_stop.stopped_since(net));
        // The session's approval session ended: nothing more can be
        // proposed in it.
        assert!(
            state
                .approvals
                .lock()
                .unwrap()
                .touch(approval, crate::state::now_ms())
                .is_err(),
            "{}",
            view.id
        );

        // Chat only: no agent or research session starts.
        for folders in [vec![project.clone()], Vec::new()] {
            let Err(err) = crate::agent::begin(&state, folders, None, None, false, None).await
            else {
                panic!("a session started in lockdown");
            };
            assert!(err.contains("Lockdown is on"), "{err}");
        }

        // Off only with the exact word; nothing restarts.
        for wrong in [None, Some(""), Some("unlock"), Some("UNLOCKED")] {
            assert!(unlock(&state, wrong).is_err(), "{wrong:?}");
            assert!(state.settings().lockdown);
        }
        unlock(&state, Some(" UNLOCK ")).unwrap();
        assert!(!state.settings().lockdown);
        assert!(crate::agent::live_sessions(&state).is_empty());
    }
}
