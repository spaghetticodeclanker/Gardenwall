//! The research sub-agent's trusted side (ADR-019
//! ): the question and findings cards, and the approver a
//! research child runs under.
//!
//! - The cards are built from what the core holds (the checked question, the
//!   child's bounded findings); the webview only answers them.
//! - A child gets its own approval session holding only the research grant
//!   (`search`, `fetch:public`), sharing the parent's visited list. It is
//!   listed for the approval dialog while it runs and ended, with its grant,
//!   when the research ends.
//! - Reviewing the findings is on by default (Settings → Web access); with it
//!   off they go straight back to the agent as unreviewed web content.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gardenwall_agent::agent::research::{Findings, MAX_QUESTION_CHARS, ResearchHost, Review};
use gardenwall_agent::agent::{ApprovalResult, Approver, BoxFuture};
use gardenwall_policy::tools::Approval;
use gardenwall_store::approvals::SessionId;
use serde::Serialize;
use tauri::State;

use crate::agent::StoreApprover;
use crate::state::{AppState, now_ms};

type CmdResult<T> = Result<T, String>;

/// Longest findings text the user may send back after editing.
const MAX_EDITED_FINDINGS: usize = 16 * 1024;

/// Research children's approval sessions, for the dialog (session → title).
pub type ResearchSessions = std::sync::Mutex<HashMap<SessionId, String>>;

/// What the session's research card shows.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ResearchCard {
    /// Approve (or edit) the question before anything leaves.
    Question { question: String },
    /// The child is working; `step` is the core's plain progress line.
    Working { question: String, step: String },
    /// Review (or edit) the findings before the agent gets them.
    Findings {
        question: String,
        findings: Findings,
    },
}

enum Answer {
    /// Approve the question / send the findings, with the (edited) text.
    Yes(String),
    /// Decline the question / discard the findings.
    No,
}

#[derive(Default)]
struct SlotState {
    /// Counts cards, so an answer can't land on a later card than the one
    /// the user saw.
    id: u64,
    card: Option<ResearchCard>,
    answer: Option<Answer>,
}

/// The session's research card and the user's answer.
#[derive(Default)]
pub struct ResearchSlot(std::sync::Mutex<SlotState>);

impl ResearchSlot {
    /// What the card waits on the user for (Home's "Waiting for you"):
    /// approving the question or reviewing the findings, not progress.
    pub fn waiting_for_user(&self) -> Option<&'static str> {
        let s = self.0.lock().ok()?;
        match (&s.card, &s.answer) {
            (Some(ResearchCard::Question { .. }), None) => Some("Approve a research question"),
            (Some(ResearchCard::Findings { .. }), None) => Some("Review the research findings"),
            _ => None,
        }
    }

    fn show(&self, card: ResearchCard) {
        if let Ok(mut s) = self.0.lock() {
            s.id += 1;
            s.card = Some(card);
            s.answer = None;
        }
    }

    fn clear(&self) {
        if let Ok(mut s) = self.0.lock() {
            s.card = None;
            s.answer = None;
        }
    }

    /// Wait for the answer to the card on show; `None` when stopped.
    async fn wait(&self, stop: &AtomicBool) -> Option<Answer> {
        loop {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut s = self.0.lock().ok()?;
            if stop.load(Ordering::SeqCst) {
                s.card = None;
                s.answer = None;
                return None;
            }
            if let Some(answer) = s.answer.take() {
                s.card = None;
                return Some(answer);
            }
        }
    }
}

/// The cards for one agent session.
pub struct AppResearch {
    pub slot: Arc<ResearchSlot>,
    pub stop: Arc<AtomicBool>,
    /// Reads "review findings" at the moment they arrive.
    pub review_on: Box<dyn Fn() -> bool + Send + Sync>,
    /// The question being researched, for the progress and findings cards.
    pub question: std::sync::Mutex<String>,
}

impl ResearchHost for AppResearch {
    fn approve<'a>(&'a self, question: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            self.slot.show(ResearchCard::Question {
                question: question.to_string(),
            });
            match self.slot.wait(&self.stop).await? {
                Answer::Yes(q) => {
                    if let Ok(mut current) = self.question.lock() {
                        current.clone_from(&q);
                    }
                    Some(q)
                }
                Answer::No => None,
            }
        })
    }

    fn progress(&self, step: &str) {
        let question = self.question.lock().map(|q| q.clone()).unwrap_or_default();
        if let Ok(mut s) = self.slot.0.lock() {
            // Progress isn't a decision: same card number, no answer reset.
            s.card = Some(ResearchCard::Working {
                question,
                step: step.to_string(),
            });
        }
    }

    fn review<'a>(&'a self, question: &'a str, findings: &'a Findings) -> BoxFuture<'a, Review> {
        Box::pin(async move {
            if !(self.review_on)() {
                self.slot.clear();
                return Review::Send {
                    text: findings.text.clone(),
                    reviewed: false,
                };
            }
            self.slot.show(ResearchCard::Findings {
                question: question.to_string(),
                findings: findings.clone(),
            });
            match self.slot.wait(&self.stop).await {
                Some(Answer::Yes(text)) => Review::Send {
                    text,
                    reviewed: true,
                },
                _ => Review::Discard,
            }
        })
    }
}

/// The open card, for the window.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchCardView {
    id: u64,
    card: ResearchCard,
}

#[tauri::command]
pub fn agent_research_card(
    state: State<'_, AppState>,
    sid: String,
) -> CmdResult<Option<ResearchCardView>> {
    let live = crate::agent::live(&state, &sid)?;
    let s = live.research.0.lock().map_err(|_| "state unavailable")?;
    Ok(s.card
        .clone()
        .filter(|_| s.answer.is_none())
        .map(|card| ResearchCardView { id: s.id, card }))
}

/// Answer the open question or findings card. `text` is the question or
/// findings as the user left them (edits allowed, bounded).
#[tauri::command]
pub fn agent_research_answer(
    state: State<'_, AppState>,
    sid: String,
    id: u64,
    approve: bool,
    text: Option<String>,
) -> CmdResult<()> {
    let live = crate::agent::live(&state, &sid)?;
    let mut s = live.research.0.lock().map_err(|_| "state unavailable")?;
    let limit = match &s.card {
        Some(ResearchCard::Question { .. }) => MAX_QUESTION_CHARS,
        Some(ResearchCard::Findings { .. }) => MAX_EDITED_FINDINGS,
        _ => return Err("that card is no longer open".into()),
    };
    if s.id != id {
        return Err("that card is no longer open".into());
    }
    s.answer = Some(if approve {
        let text = text.unwrap_or_default().trim().to_string();
        if text.is_empty() {
            return Err("the text is empty".into());
        }
        if text.chars().count() > limit {
            return Err(format!("at most {limit} characters"));
        }
        Answer::Yes(text)
    } else {
        Answer::No
    });
    Ok(())
}

/// The approver a research child runs under: its own approval session with
/// only the research grant, ended (with the grant) when dropped.
pub(crate) struct ChildApprover<'a> {
    inner: StoreApprover<'a>,
}

impl<'a> ChildApprover<'a> {
    pub(crate) fn start(parent: &StoreApprover<'a>) -> Option<Self> {
        let state = parent.state;
        let session = state.approvals.lock().ok()?.start_session(now_ms()).ok()?;
        state
            .egress_grants
            .lock()
            .ok()?
            .insert(session, ["search", "fetch:public"].map(String::from).into());
        // Its requests are listed with the parent's visits.
        if let Some(log) = crate::state::visit_log(state, parent.session)
            && let Ok(mut v) = state.web_visits.lock()
        {
            v.insert(session, log);
        }
        let title = crate::agent::approval_sessions(state)
            .into_iter()
            .find(|(s, _)| *s == parent.session)
            .map(|(_, t)| format!("Research for {t}"))
            .unwrap_or_else(|| "Research".into());
        if let Ok(mut r) = state.research_sessions.lock() {
            r.insert(session, title);
        }
        Some(Self {
            inner: StoreApprover {
                state,
                session,
                stop: parent.stop.clone(),
                interrupt: parent.interrupt.clone(),
                claimed: std::sync::Mutex::new(None),
            },
        })
    }
}

impl Drop for ChildApprover<'_> {
    fn drop(&mut self) {
        let (state, session) = (self.inner.state, self.inner.session);
        if let Ok(mut r) = state.research_sessions.lock() {
            r.remove(&session);
        }
        if let Ok(mut g) = state.egress_grants.lock() {
            g.remove(&session);
        }
        if let Ok(mut v) = state.web_visits.lock() {
            v.remove(&session);
        }
        if let Ok(mut store) = state.approvals.lock() {
            let _ = store.end_session(session, now_ms());
        }
    }
}

impl Approver for ChildApprover<'_> {
    fn request<'a>(
        &'a self,
        approval: &'a Approval,
        details: &'a str,
        tool: &'a str,
    ) -> BoxFuture<'a, ApprovalResult> {
        self.inner.request(approval, details, tool)
    }

    fn finished<'a>(&'a self, success: bool) -> BoxFuture<'a, ()> {
        self.inner.finished(success)
    }

    fn decides_alone(&self, approval: &Approval, details: &str) -> bool {
        self.inner.decides_alone(approval, details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_holds_only_the_research_grant_and_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf()).unwrap();
        let parent_session = state
            .approvals
            .lock()
            .unwrap()
            .start_session(now_ms())
            .unwrap();
        let log = Arc::new(crate::web::visits::VisitLog::default());
        state
            .web_visits
            .lock()
            .unwrap()
            .insert(parent_session, log.clone());
        let parent = StoreApprover {
            state: &state,
            session: parent_session,
            stop: Arc::default(),
            interrupt: Arc::default(),
            claimed: std::sync::Mutex::new(None),
        };
        let child = ChildApprover::start(&parent).unwrap();
        let session = child.inner.session;
        assert_ne!(session, parent_session);
        let grants = state
            .egress_grants
            .lock()
            .unwrap()
            .get(&session)
            .cloned()
            .unwrap();
        assert_eq!(grants.len(), 2);
        assert!(crate::agent::grant_covers(
            &grants,
            "fetch:https://docs.rs/tokio"
        ));
        assert!(crate::agent::grant_covers(&grants, "search"));
        // The parent gains nothing.
        assert!(
            !state
                .egress_grants
                .lock()
                .unwrap()
                .contains_key(&parent_session)
        );
        // Its requests go to the parent's visited list, and the dialog can see it.
        assert!(Arc::ptr_eq(
            &crate::state::visit_log(&state, session).unwrap(),
            &log
        ));
        assert!(
            crate::agent::approval_sessions(&state)
                .iter()
                .any(|(s, _)| *s == session)
        );
        drop(child);
        assert!(!state.egress_grants.lock().unwrap().contains_key(&session));
        assert!(crate::state::visit_log(&state, session).is_none());
        assert!(state.research_sessions.lock().unwrap().is_empty());
        assert!(crate::state::visit_log(&state, parent_session).is_some());
    }

    #[tokio::test]
    async fn an_answer_lands_only_on_the_card_shown_and_stop_ends_the_wait() {
        let slot = ResearchSlot::default();
        slot.show(ResearchCard::Question {
            question: "q".into(),
        });
        let stop = AtomicBool::new(false);
        {
            let mut s = slot.0.lock().unwrap();
            assert_eq!(s.id, 1);
            s.answer = Some(Answer::Yes("edited q".into()));
        }
        assert!(matches!(slot.wait(&stop).await, Some(Answer::Yes(t)) if t == "edited q"));
        assert!(slot.0.lock().unwrap().card.is_none());
        slot.show(ResearchCard::Question {
            question: "q2".into(),
        });
        stop.store(true, Ordering::SeqCst);
        assert!(slot.wait(&stop).await.is_none());
        assert!(slot.0.lock().unwrap().card.is_none());
    }
}
