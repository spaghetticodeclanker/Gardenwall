//! Desktop approval sessions and single-use approvals (ADR-016, ADR-007).
//!
//! Every state change runs in one `IMMEDIATE` transaction together with its
//! metadata-only audit event, so a change without its audit record can never
//! be committed. Opening the store cancels everything left pending or
//! approved by an earlier run and ends that run's sessions before any new
//! approval is possible.
//!
//! Time is passed in explicitly (milliseconds since the Unix epoch) so the
//! deadline rules are deterministic and testable.

use std::fmt;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

/// Pending proposals expire this long after creation (ADR-007 §7).
pub const PROPOSAL_TTL_MS: i64 = 5 * 60 * 1000;
/// An approval must be claimed within this long (ADR-007 §7).
pub const CLAIM_WINDOW_MS: i64 = 30 * 1000;
/// Sessions end after this long without interactive user activity.
pub const SESSION_IDLE_MS: i64 = 30 * 60 * 1000;
/// Sessions end this long after they start, whatever the activity.
pub const SESSION_ABSOLUTE_MS: i64 = 12 * 60 * 60 * 1000;
/// Audit metadata is kept this long.
pub const AUDIT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Largest stored proposal details (JSON).
pub const MAX_DETAILS_BYTES: usize = 1 << 20;

/// 1: initial. 2: the `McpTool` kind. 3: every later kind (`RiskyCommand`,
/// `HeldBack` and `KeepRunning` were refused by the version 2 table, so
/// those requests expired instead of reaching the dialog) and `AfterWeb`.
/// 4: `InitRepository` (ADR-022). 5: `AddToolchain` (ADR-024).
const SCHEMA_VERSION: i64 = 5;

const SCHEMA: &str = r#"
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value BLOB NOT NULL
) STRICT;

CREATE TABLE runtimes (
    runtime_id BLOB PRIMARY KEY CHECK(length(runtime_id) = 16),
    started_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    session_id BLOB PRIMARY KEY CHECK(length(session_id) = 16),
    runtime_id BLOB NOT NULL REFERENCES runtimes(runtime_id),
    created_at_ms INTEGER NOT NULL,
    last_interactive_at_ms INTEGER NOT NULL,
    absolute_deadline_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    end_reason TEXT CHECK(end_reason IS NULL OR end_reason IN ('User', 'Restart', 'Idle', 'Absolute')),
    CHECK((ended_at_ms IS NULL) = (end_reason IS NULL))
) STRICT;

CREATE TABLE proposals (
    proposal_id BLOB PRIMARY KEY CHECK(length(proposal_id) = 16),
    session_id BLOB NOT NULL REFERENCES sessions(session_id),
    kind TEXT NOT NULL CHECK(kind IN ('RevealSecret', 'WriteSecret', 'PersistenceRisk',
        'OutsideRead', 'OutsideWrite', 'NetworkCommand', 'Egress', 'McpTool',
        'RiskyCommand', 'HeldBack', 'KeepRunning', 'AfterWeb', 'InitRepository', 'AddToolchain')),
    details TEXT,
    state TEXT NOT NULL CHECK(state IN ('Pending', 'Approved', 'Rejected', 'Cancelled', 'Expired', 'Claimed')),
    created_at_ms INTEGER NOT NULL,
    deadline_ms INTEGER NOT NULL,
    approved_at_ms INTEGER,
    terminal_at_ms INTEGER,
    outcome TEXT CHECK(outcome IS NULL OR outcome IN ('NotStarted', 'Running', 'Completed', 'Failed', 'Unknown')),
    CHECK(deadline_ms = created_at_ms + 300000),
    CHECK((state IN ('Pending', 'Approved')) = (terminal_at_ms IS NULL)),
    CHECK((state = 'Claimed') = (outcome IS NOT NULL)),
    CHECK(state NOT IN ('Approved', 'Claimed') OR approved_at_ms IS NOT NULL),
    CHECK(details IS NOT NULL OR terminal_at_ms IS NOT NULL)
) STRICT;

CREATE TABLE audit (
    audit_id INTEGER PRIMARY KEY,
    at_ms INTEGER NOT NULL,
    event TEXT NOT NULL,
    session_id BLOB,
    proposal_id BLOB,
    kind TEXT,
    reason TEXT
) STRICT;

CREATE INDEX proposals_session ON proposals(session_id, state);
CREATE INDEX audit_time ON audit(at_ms);
"#;

/// What a proposal asks the user to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ApprovalKind {
    RevealSecret,
    WriteSecret,
    PersistenceRisk,
    OutsideRead,
    OutsideWrite,
    NetworkCommand,
    Egress,
    /// A tool provided by an MCP server.
    McpTool,
    /// A command that looks like it changes something that runs later.
    RiskyCommand,
    /// Changes a command made to protected files, held back.
    HeldBack,
    /// Programs a command left running in the sandbox.
    KeepRunning,
    /// A change or command after web content entered the session.
    AfterWeb,
    /// Gardenwall creates a Git repository in a granted folder (ADR-022).
    InitRepository,
    /// A toolchain is added to the agent's sandbox (ADR-024 §3 A2).
    AddToolchain,
}

impl ApprovalKind {
    /// Every kind (the table's CHECK list must name each one).
    pub const ALL: [Self; 14] = [
        Self::RevealSecret,
        Self::WriteSecret,
        Self::PersistenceRisk,
        Self::OutsideRead,
        Self::OutsideWrite,
        Self::NetworkCommand,
        Self::Egress,
        Self::McpTool,
        Self::RiskyCommand,
        Self::HeldBack,
        Self::KeepRunning,
        Self::AfterWeb,
        Self::InitRepository,
        Self::AddToolchain,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RevealSecret => "RevealSecret",
            Self::WriteSecret => "WriteSecret",
            Self::PersistenceRisk => "PersistenceRisk",
            Self::OutsideRead => "OutsideRead",
            Self::OutsideWrite => "OutsideWrite",
            Self::NetworkCommand => "NetworkCommand",
            Self::Egress => "Egress",
            Self::McpTool => "McpTool",
            Self::RiskyCommand => "RiskyCommand",
            Self::HeldBack => "HeldBack",
            Self::KeepRunning => "KeepRunning",
            Self::AfterWeb => "AfterWeb",
            Self::InitRepository => "InitRepository",
            Self::AddToolchain => "AddToolchain",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "RevealSecret" => Self::RevealSecret,
            "WriteSecret" => Self::WriteSecret,
            "PersistenceRisk" => Self::PersistenceRisk,
            "OutsideRead" => Self::OutsideRead,
            "OutsideWrite" => Self::OutsideWrite,
            "NetworkCommand" => Self::NetworkCommand,
            "Egress" => Self::Egress,
            "McpTool" => Self::McpTool,
            "RiskyCommand" => Self::RiskyCommand,
            "HeldBack" => Self::HeldBack,
            "KeepRunning" => Self::KeepRunning,
            "AfterWeb" => Self::AfterWeb,
            "InitRepository" => Self::InitRepository,
            "AddToolchain" => Self::AddToolchain,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProposalState {
    Pending,
    Approved,
    Rejected,
    Cancelled,
    Expired,
    Claimed,
}

impl ProposalState {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Pending" => Self::Pending,
            "Approved" => Self::Approved,
            "Rejected" => Self::Rejected,
            "Cancelled" => Self::Cancelled,
            "Expired" => Self::Expired,
            "Claimed" => Self::Claimed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Outcome {
    Running,
    Completed,
    Failed,
    Unknown,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Unknown => "Unknown",
        }
    }
}

/// An opaque 128-bit identifier, shown as 32 hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id([u8; 16]);

impl Id {
    fn generate() -> Self {
        Self(*uuid::Uuid::new_v4().as_bytes())
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        }
        Some(Self(out))
    }
}

pub type SessionId = Id;
pub type ProposalId = Id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalView {
    pub id: String,
    pub kind: ApprovalKind,
    pub state: ProposalState,
    /// The exact operation, as stored at creation. `None` once purged.
    pub details: Option<String>,
    pub created_at_ms: i64,
    pub deadline_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub at_ms: i64,
    pub event: String,
    pub session: Option<String>,
    pub proposal: Option<String>,
    pub kind: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySummary {
    pub cancelled_proposals: usize,
    pub ended_sessions: usize,
    pub purged_details: usize,
}

#[derive(Debug)]
pub enum ApprovalError {
    NotFound,
    /// The proposal belongs to a different session.
    WrongSession,
    SessionEnded,
    /// The proposal is not in a state that allows this action.
    InvalidState(ProposalState),
    Expired,
    DetailsTooLarge,
    Corrupt,
    Database(rusqlite::Error),
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("approval request not found"),
            Self::WrongSession => f.write_str("approval request belongs to another session"),
            Self::SessionEnded => f.write_str("the agent session has ended"),
            Self::InvalidState(s) => write!(f, "approval request is {s:?}"),
            Self::Expired => f.write_str("approval request expired"),
            Self::DetailsTooLarge => f.write_str("approval details are too large"),
            Self::Corrupt => f.write_str("approval database is inconsistent"),
            Self::Database(e) => write!(f, "approval database error: {e}"),
        }
    }
}

impl std::error::Error for ApprovalError {}

impl From<rusqlite::Error> for ApprovalError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}

type Tx<'a> = rusqlite::Transaction<'a>;

fn audit(
    tx: &Tx<'_>,
    now: i64,
    event: &str,
    session: Option<Id>,
    proposal: Option<Id>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO audit (at_ms, event, session_id, proposal_id, kind, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, event, session.map(|s| s.0.to_vec()), proposal.map(|p| p.0.to_vec()), kind, reason],
    )?;
    Ok(())
}

fn blob_id(v: Vec<u8>) -> Result<Id, ApprovalError> {
    <[u8; 16]>::try_from(v.as_slice())
        .map(Id)
        .map_err(|_| ApprovalError::Corrupt)
}

pub struct ApprovalStore {
    db: Connection,
    runtime: Id,
    recovery: RecoverySummary,
}

impl fmt::Debug for ApprovalStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalStore")
            .field("runtime", &self.runtime.to_hex())
            .finish()
    }
}

impl ApprovalStore {
    pub fn open(path: &Path, now: i64) -> Result<Self, ApprovalError> {
        Self::init(Connection::open(path)?, now)
    }

    pub fn open_in_memory(now: i64) -> Result<Self, ApprovalError> {
        Self::init(Connection::open_in_memory()?, now)
    }

    fn init(db: Connection, now: i64) -> Result<Self, ApprovalError> {
        db.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            db.execute_batch(&format!(
                "BEGIN IMMEDIATE; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        } else if (1..=4).contains(&version) {
            // SQLite can't change a CHECK constraint: rebuild the proposals
            // table with the new kinds, keeping every row.
            let create = SCHEMA
                .split("CREATE TABLE proposals (")
                .nth(1)
                .and_then(|rest| rest.split(") STRICT;").next())
                .ok_or(ApprovalError::Corrupt)?;
            db.execute_batch(&format!(
                "PRAGMA foreign_keys = OFF;
                 BEGIN IMMEDIATE;
                 CREATE TABLE proposals_new ({create}) STRICT;
                 INSERT INTO proposals_new SELECT * FROM proposals;
                 DROP TABLE proposals;
                 ALTER TABLE proposals_new RENAME TO proposals;
                 CREATE INDEX proposals_session ON proposals(session_id, state);
                 PRAGMA user_version = {SCHEMA_VERSION};
                 COMMIT;
                 PRAGMA foreign_keys = ON;"
            ))?;
        } else if version != SCHEMA_VERSION {
            return Err(ApprovalError::Corrupt);
        }
        let mut store = Self {
            db,
            runtime: Id::generate(),
            recovery: RecoverySummary {
                cancelled_proposals: 0,
                ended_sessions: 0,
                purged_details: 0,
            },
        };
        store.recover(now)?;
        Ok(store)
    }

    /// What startup recovery did.
    pub fn recovery(&self) -> &RecoverySummary {
        &self.recovery
    }

    /// A consistent copy of the database at `dest` for a backup (the
    /// activity log). `dest` must not exist.
    pub fn copy_to(&self, dest: &Path) -> Result<(), crate::backup::BackupError> {
        crate::backup::copy_database(&self.db, dest)
    }

    /// ADR-007 §12: invalidate everything from earlier runs, then purge.
    fn recover(&mut self, now: i64) -> Result<(), ApprovalError> {
        let runtime = self.runtime;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO runtimes (runtime_id, started_at_ms) VALUES (?1, ?2)",
            params![runtime.0.to_vec(), now],
        )?;
        let stale: Vec<(Vec<u8>, Vec<u8>, String)> = tx
            .prepare("SELECT proposal_id, session_id, kind FROM proposals WHERE state IN ('Pending', 'Approved')")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (p, s, kind) in &stale {
            tx.execute(
                "UPDATE proposals SET state = 'Cancelled', terminal_at_ms = ?2, details = NULL WHERE proposal_id = ?1",
                params![p, now],
            )?;
            audit(
                &tx,
                now,
                "Cancelled",
                Some(blob_id(s.clone())?),
                Some(blob_id(p.clone())?),
                Some(kind),
                Some("Restart"),
            )?;
        }
        let open: Vec<Vec<u8>> = tx
            .prepare(
                "SELECT session_id FROM sessions WHERE ended_at_ms IS NULL AND runtime_id != ?1",
            )?
            .query_map([runtime.0.to_vec()], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for s in &open {
            tx.execute(
                "UPDATE sessions SET ended_at_ms = ?2, end_reason = 'Restart' WHERE session_id = ?1",
                params![s, now],
            )?;
            audit(
                &tx,
                now,
                "SessionEnded",
                Some(blob_id(s.clone())?),
                None,
                None,
                Some("Restart"),
            )?;
        }
        tx.commit()?;
        self.recovery.cancelled_proposals = stale.len();
        self.recovery.ended_sessions = open.len();
        self.recovery.purged_details = self.purge(now)?;
        Ok(())
    }

    /// Purge old audit metadata, and details left on decided proposals by
    /// earlier versions (ADR-016: details are cleared when a proposal is
    /// decided, so normally there are none).
    pub fn purge(&mut self, now: i64) -> Result<usize, ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let purged = tx.execute(
            "UPDATE proposals SET details = NULL WHERE details IS NOT NULL AND terminal_at_ms IS NOT NULL",
            [],
        )?;
        tx.execute(
            "DELETE FROM audit WHERE at_ms < ?1",
            params![now - AUDIT_RETENTION_MS],
        )?;
        tx.commit()?;
        self.scrub_log()?;
        Ok(purged)
    }

    /// Move committed pages into the database (where `secure_delete` has
    /// zeroed cleared details) and truncate the write-ahead log, so cleared
    /// details do not survive in it. Run after every terminal transition.
    fn scrub_log(&self) -> Result<(), ApprovalError> {
        let _: (i64, i64, i64) = self
            .db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        Ok(())
    }

    /// Start an agent session (a trusted UI action).
    pub fn start_session(&mut self, now: i64) -> Result<SessionId, ApprovalError> {
        let id = Id::generate();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO sessions (session_id, runtime_id, created_at_ms, last_interactive_at_ms, absolute_deadline_ms)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![id.0.to_vec(), self.runtime.0.to_vec(), now, now + SESSION_ABSOLUTE_MS],
        )?;
        audit(&tx, now, "SessionStarted", Some(id), None, None, None)?;
        tx.commit()?;
        Ok(id)
    }

    /// End a session at the user's request; cancels its open proposals.
    pub fn end_session(&mut self, session: SessionId, now: i64) -> Result<(), ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ended = end_session_tx(&tx, session, now, "User")?;
        tx.commit()?;
        self.scrub_log()?;
        if ended {
            Ok(())
        } else {
            Err(ApprovalError::SessionEnded)
        }
    }

    /// Record a security flag in the audit log: something the core noticed
    /// about a session (for example a secret read before an outbound
    /// call). Metadata only: `kind` names the flag and `detail` holds tool
    /// names, kinds of secret and counts, never contents or paths. Both
    /// are bounded here.
    pub fn note_flag(
        &mut self,
        session: Option<SessionId>,
        kind: &str,
        detail: &str,
        now: i64,
    ) -> Result<(), ApprovalError> {
        let bounded = |s: &str, max: usize| -> String {
            s.chars().filter(|c| !c.is_control()).take(max).collect()
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        audit(
            &tx,
            now,
            "SecurityFlag",
            session,
            None,
            Some(&bounded(kind, 64)),
            Some(&bounded(detail, 200)),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record interactive user activity, which keeps the session alive.
    pub fn touch(&mut self, session: SessionId, now: i64) -> Result<(), ApprovalError> {
        self.ensure_active(session, now)?;
        self.db.execute(
            "UPDATE sessions SET last_interactive_at_ms = ?2 WHERE session_id = ?1 AND ended_at_ms IS NULL",
            params![session.0.to_vec(), now],
        )?;
        Ok(())
    }

    /// Fail if the session is over; end it (atomically, with audit) if it
    /// just timed out.
    fn ensure_active(&mut self, session: SessionId, now: i64) -> Result<(), ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(Vec<u8>, i64, i64, Option<i64>)> = tx
            .query_row(
                "SELECT runtime_id, last_interactive_at_ms, absolute_deadline_ms, ended_at_ms FROM sessions WHERE session_id = ?1",
                [session.0.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((runtime, last, absolute, ended)) = row else {
            return Err(ApprovalError::SessionEnded);
        };
        if ended.is_some() || runtime != self.runtime.0 {
            return Err(ApprovalError::SessionEnded);
        }
        let reason = if now >= absolute {
            Some("Absolute")
        } else if now - last >= SESSION_IDLE_MS {
            Some("Idle")
        } else {
            None
        };
        if let Some(reason) = reason {
            end_session_tx(&tx, session, now, reason)?;
            tx.commit()?;
            self.scrub_log()?;
            return Err(ApprovalError::SessionEnded);
        }
        Ok(())
    }

    /// Create a proposal. `details` is the exact operation as JSON; it is
    /// immutable from here on.
    pub fn propose(
        &mut self,
        session: SessionId,
        kind: ApprovalKind,
        details: &str,
        now: i64,
    ) -> Result<ProposalId, ApprovalError> {
        self.propose_labeled(session, kind, details, None, now)
    }

    /// Like [`ApprovalStore::propose`], also recording which tool asked
    /// (e.g. `write`) in the audit log. Only a short lowercase name is
    /// accepted, so the audit stays metadata-only.
    pub fn propose_from_tool(
        &mut self,
        session: SessionId,
        kind: ApprovalKind,
        details: &str,
        tool: &str,
        now: i64,
    ) -> Result<ProposalId, ApprovalError> {
        let valid = (1..=16).contains(&tool.len())
            && tool.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
        self.propose_labeled(session, kind, details, valid.then_some(tool), now)
    }

    fn propose_labeled(
        &mut self,
        session: SessionId,
        kind: ApprovalKind,
        details: &str,
        tool: Option<&str>,
        now: i64,
    ) -> Result<ProposalId, ApprovalError> {
        if details.len() > MAX_DETAILS_BYTES {
            return Err(ApprovalError::DetailsTooLarge);
        }
        self.ensure_active(session, now)?;
        let id = Id::generate();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO proposals (proposal_id, session_id, kind, details, state, created_at_ms, deadline_ms)
             VALUES (?1, ?2, ?3, ?4, 'Pending', ?5, ?6)",
            params![id.0.to_vec(), session.0.to_vec(), kind.as_str(), details, now, now + PROPOSAL_TTL_MS],
        )?;
        audit(
            &tx,
            now,
            "Proposed",
            Some(session),
            Some(id),
            Some(kind.as_str()),
            tool,
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// The user's decision in the trusted dialog. Approving is interactive
    /// activity and keeps the session alive.
    pub fn decide(
        &mut self,
        proposal: ProposalId,
        session: SessionId,
        approve: bool,
        now: i64,
    ) -> Result<(), ApprovalError> {
        self.decide_inner(proposal, session, approve, true, now)
    }

    /// Approve under a session grant the user created in the trusted dialog
    /// (ADR-011 §2.6). Same lifecycle as an interactive approval, audited
    /// with reason `SessionGrant`; it is not user activity, so it never
    /// keeps the session alive.
    pub fn approve_by_grant(
        &mut self,
        proposal: ProposalId,
        session: SessionId,
        now: i64,
    ) -> Result<(), ApprovalError> {
        self.decide_inner(proposal, session, true, false, now)
    }

    fn decide_inner(
        &mut self,
        proposal: ProposalId,
        session: SessionId,
        approve: bool,
        interactive: bool,
        now: i64,
    ) -> Result<(), ApprovalError> {
        self.ensure_active(session, now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (owner, kind, state, deadline, _) = load(&tx, proposal)?;
        if owner != session {
            return Err(ApprovalError::WrongSession);
        }
        if state != ProposalState::Pending {
            return Err(ApprovalError::InvalidState(state));
        }
        if now >= deadline {
            set_terminal(&tx, proposal, "Expired", now)?;
            audit(
                &tx,
                now,
                "Expired",
                Some(session),
                Some(proposal),
                Some(&kind),
                Some("ProposalDeadline"),
            )?;
            tx.commit()?;
            self.scrub_log()?;
            return Err(ApprovalError::Expired);
        }
        if approve {
            tx.execute(
                "UPDATE proposals SET state = 'Approved', approved_at_ms = ?2 WHERE proposal_id = ?1 AND state = 'Pending'",
                params![proposal.0.to_vec(), now],
            )?;
            audit(
                &tx,
                now,
                "Approved",
                Some(session),
                Some(proposal),
                Some(&kind),
                (!interactive).then_some("SessionGrant"),
            )?;
        } else {
            set_terminal(&tx, proposal, "Rejected", now)?;
            audit(
                &tx,
                now,
                "Rejected",
                Some(session),
                Some(proposal),
                Some(&kind),
                None,
            )?;
        }
        if interactive {
            tx.execute(
                "UPDATE sessions SET last_interactive_at_ms = ?2 WHERE session_id = ?1",
                params![session.0.to_vec(), now],
            )?;
        }
        tx.commit()?;
        if !approve {
            self.scrub_log()?;
        }
        Ok(())
    }

    /// Cancel a pending or approved proposal (first committed change wins).
    pub fn cancel(
        &mut self,
        proposal: ProposalId,
        session: SessionId,
        now: i64,
    ) -> Result<(), ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (owner, kind, state, _, _) = load(&tx, proposal)?;
        if owner != session {
            return Err(ApprovalError::WrongSession);
        }
        if !matches!(state, ProposalState::Pending | ProposalState::Approved) {
            return Err(ApprovalError::InvalidState(state));
        }
        set_terminal(&tx, proposal, "Cancelled", now)?;
        audit(
            &tx,
            now,
            "Cancelled",
            Some(session),
            Some(proposal),
            Some(&kind),
            Some("Requested"),
        )?;
        tx.commit()?;
        self.scrub_log()?;
        Ok(())
    }

    /// Consume an approval exactly once and return the approved operation.
    /// The caller must execute exactly these details, nothing else.
    pub fn claim(
        &mut self,
        proposal: ProposalId,
        session: SessionId,
        now: i64,
    ) -> Result<String, ApprovalError> {
        self.ensure_active(session, now)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (owner, kind, state, deadline, approved_at) = load(&tx, proposal)?;
        if owner != session {
            return Err(ApprovalError::WrongSession);
        }
        if state != ProposalState::Approved {
            return Err(ApprovalError::InvalidState(state));
        }
        let approved_at = approved_at.ok_or(ApprovalError::Corrupt)?;
        if now >= deadline.min(approved_at + CLAIM_WINDOW_MS) {
            set_terminal(&tx, proposal, "Expired", now)?;
            audit(
                &tx,
                now,
                "Expired",
                Some(session),
                Some(proposal),
                Some(&kind),
                Some("ClaimDeadline"),
            )?;
            tx.commit()?;
            self.scrub_log()?;
            return Err(ApprovalError::Expired);
        }
        let details: Option<String> = tx.query_row(
            "SELECT details FROM proposals WHERE proposal_id = ?1",
            [proposal.0.to_vec()],
            |r| r.get(0),
        )?;
        let details = details.ok_or(ApprovalError::Corrupt)?;
        let changed = tx.execute(
            "UPDATE proposals SET state = 'Claimed', terminal_at_ms = ?2, outcome = 'NotStarted', details = NULL
             WHERE proposal_id = ?1 AND state = 'Approved'",
            params![proposal.0.to_vec(), now],
        )?;
        if changed != 1 {
            return Err(ApprovalError::Corrupt);
        }
        audit(
            &tx,
            now,
            "Claimed",
            Some(session),
            Some(proposal),
            Some(&kind),
            None,
        )?;
        tx.commit()?;
        self.scrub_log()?;
        Ok(details)
    }

    /// Record what happened after a claim. Never restores the approval.
    pub fn set_outcome(
        &mut self,
        proposal: ProposalId,
        outcome: Outcome,
        now: i64,
    ) -> Result<(), ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (session, kind, state, _, _) = load(&tx, proposal)?;
        if state != ProposalState::Claimed {
            return Err(ApprovalError::InvalidState(state));
        }
        tx.execute(
            "UPDATE proposals SET outcome = ?2 WHERE proposal_id = ?1",
            params![proposal.0.to_vec(), outcome.as_str()],
        )?;
        audit(
            &tx,
            now,
            "Outcome",
            Some(session),
            Some(proposal),
            Some(&kind),
            Some(outcome.as_str()),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark everything past its deadline as expired.
    pub fn sweep(&mut self, now: i64) -> Result<usize, ApprovalError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let due: Vec<(Vec<u8>, Vec<u8>, String, &str)> = {
            let mut stmt = tx.prepare(
                "SELECT proposal_id, session_id, kind, state FROM proposals
                 WHERE (state = 'Pending' AND deadline_ms <= ?1)
                    OR (state = 'Approved' AND MIN(deadline_ms, approved_at_ms + ?2) <= ?1)",
            )?;
            stmt.query_map(params![now, CLAIM_WINDOW_MS], |r| {
                let state: String = r.get(3)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    if state == "Pending" {
                        "ProposalDeadline"
                    } else {
                        "ClaimDeadline"
                    },
                ))
            })?
            .collect::<Result<_, _>>()?
        };
        for (p, s, kind, reason) in &due {
            let p = blob_id(p.clone())?;
            set_terminal(&tx, p, "Expired", now)?;
            audit(
                &tx,
                now,
                "Expired",
                Some(blob_id(s.clone())?),
                Some(p),
                Some(kind),
                Some(reason),
            )?;
        }
        tx.commit()?;
        if !due.is_empty() {
            self.scrub_log()?;
        }
        Ok(due.len())
    }

    /// A proposal's current state, for the agent waiting on a decision.
    pub fn state(&self, proposal: ProposalId) -> Result<ProposalState, ApprovalError> {
        let state: Option<String> = self
            .db
            .query_row(
                "SELECT state FROM proposals WHERE proposal_id = ?1",
                [proposal.0.to_vec()],
                |r| r.get(0),
            )
            .optional()?;
        let state = state.ok_or(ApprovalError::NotFound)?;
        ProposalState::parse(&state).ok_or(ApprovalError::Corrupt)
    }

    /// Pending proposals of a session, oldest first, for the dialog.
    pub fn pending(
        &self,
        session: SessionId,
        now: i64,
    ) -> Result<Vec<ProposalView>, ApprovalError> {
        let mut stmt = self.db.prepare(
            "SELECT proposal_id, kind, state, details, created_at_ms, deadline_ms FROM proposals
             WHERE session_id = ?1 AND state = 'Pending' AND deadline_ms > ?2 ORDER BY created_at_ms",
        )?;
        let rows = stmt
            .query_map(params![session.0.to_vec(), now], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, kind, state, details, created, deadline)| {
                Ok(ProposalView {
                    id: blob_id(id)?.to_hex(),
                    kind: ApprovalKind::parse(&kind).ok_or(ApprovalError::Corrupt)?,
                    state: ProposalState::parse(&state).ok_or(ApprovalError::Corrupt)?,
                    details,
                    created_at_ms: created,
                    deadline_ms: deadline,
                })
            })
            .collect()
    }

    /// The newest audit entries (metadata only), newest first.
    pub fn audit_log(&self, limit: usize) -> Result<Vec<AuditEntry>, ApprovalError> {
        let mut stmt = self.db.prepare(
            "SELECT at_ms, event, session_id, proposal_id, kind, reason FROM audit ORDER BY audit_id DESC LIMIT ?1",
        )?;
        let hex = |v: Option<Vec<u8>>| {
            v.and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok())
                .map(|b| Id(b).to_hex())
        };
        let rows = stmt
            .query_map([limit.min(10_000) as i64], |r| {
                Ok(AuditEntry {
                    at_ms: r.get(0)?,
                    event: r.get(1)?,
                    session: hex(r.get(2)?),
                    proposal: hex(r.get(3)?),
                    kind: r.get(4)?,
                    reason: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
}

/// A proposal's owner session, kind, state, deadline and approval time.
type ProposalRow = (Id, String, ProposalState, i64, Option<i64>);
type RawProposalRow = (Vec<u8>, String, String, i64, Option<i64>);

fn load(tx: &Tx<'_>, proposal: ProposalId) -> Result<ProposalRow, ApprovalError> {
    let row: Option<RawProposalRow> = tx
        .query_row(
            "SELECT session_id, kind, state, deadline_ms, approved_at_ms FROM proposals WHERE proposal_id = ?1",
            [proposal.0.to_vec()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let (session, kind, state, deadline, approved) = row.ok_or(ApprovalError::NotFound)?;
    Ok((
        blob_id(session)?,
        kind,
        ProposalState::parse(&state).ok_or(ApprovalError::Corrupt)?,
        deadline,
        approved,
    ))
}

fn set_terminal(tx: &Tx<'_>, proposal: ProposalId, state: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE proposals SET state = ?2, terminal_at_ms = ?3, details = NULL WHERE proposal_id = ?1",
        params![proposal.0.to_vec(), state, now],
    )?;
    Ok(())
}

/// End a session and cancel its open proposals. Returns false if it had
/// already ended.
fn end_session_tx(
    tx: &Tx<'_>,
    session: SessionId,
    now: i64,
    reason: &str,
) -> Result<bool, ApprovalError> {
    let changed = tx.execute(
        "UPDATE sessions SET ended_at_ms = ?2, end_reason = ?3 WHERE session_id = ?1 AND ended_at_ms IS NULL",
        params![session.0.to_vec(), now, reason],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    let open: Vec<(Vec<u8>, String)> = tx
        .prepare("SELECT proposal_id, kind FROM proposals WHERE session_id = ?1 AND state IN ('Pending', 'Approved')")?
        .query_map([session.0.to_vec()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (p, kind) in open {
        let p = blob_id(p)?;
        set_terminal(tx, p, "Cancelled", now)?;
        audit(
            tx,
            now,
            "Cancelled",
            Some(session),
            Some(p),
            Some(&kind),
            Some("SessionEnded"),
        )?;
    }
    audit(
        tx,
        now,
        "SessionEnded",
        Some(session),
        None,
        None,
        Some(reason),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;
    const DETAILS: &str = r#"{"path":"C:\\Users\\me\\notes.txt"}"#;

    fn store() -> (ApprovalStore, SessionId) {
        let mut s = ApprovalStore::open_in_memory(T0).unwrap();
        let session = s.start_session(T0).unwrap();
        (s, session)
    }

    #[test]
    fn every_kind_can_be_proposed() {
        let (mut s, session) = store();
        for kind in ApprovalKind::ALL {
            assert!(
                s.propose(session, kind, DETAILS, T0).is_ok(),
                "{}",
                kind.as_str()
            );
            assert_eq!(ApprovalKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn approve_then_claim_exactly_once() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        assert_eq!(s.pending(session, T0).unwrap().len(), 1);
        // Claiming before approval fails.
        assert!(matches!(
            s.claim(p, session, T0),
            Err(ApprovalError::InvalidState(ProposalState::Pending))
        ));
        s.decide(p, session, true, T0 + 1000).unwrap();
        assert_eq!(s.claim(p, session, T0 + 2000).unwrap(), DETAILS);
        assert!(matches!(
            s.claim(p, session, T0 + 3000),
            Err(ApprovalError::InvalidState(ProposalState::Claimed))
        ));
        s.set_outcome(p, Outcome::Completed, T0 + 4000).unwrap();
        assert!(s.pending(session, T0 + 4000).unwrap().is_empty());
    }

    #[test]
    fn rejection_is_final() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::OutsideWrite, DETAILS, T0)
            .unwrap();
        s.decide(p, session, false, T0).unwrap();
        assert!(matches!(
            s.decide(p, session, true, T0),
            Err(ApprovalError::InvalidState(ProposalState::Rejected))
        ));
        assert!(matches!(
            s.claim(p, session, T0),
            Err(ApprovalError::InvalidState(ProposalState::Rejected))
        ));
    }

    #[test]
    fn deadlines_are_enforced_at_decision_and_claim() {
        let (mut s, session) = store();
        let late = s
            .propose(session, ApprovalKind::RevealSecret, DETAILS, T0)
            .unwrap();
        assert!(matches!(
            s.decide(late, session, true, T0 + PROPOSAL_TTL_MS),
            Err(ApprovalError::Expired)
        ));

        let slow = s
            .propose(session, ApprovalKind::RevealSecret, DETAILS, T0)
            .unwrap();
        s.decide(slow, session, true, T0 + 1000).unwrap();
        assert!(matches!(
            s.claim(slow, session, T0 + 1000 + CLAIM_WINDOW_MS),
            Err(ApprovalError::Expired)
        ));

        // The claim deadline never extends past the proposal's own deadline.
        let edge = s
            .propose(session, ApprovalKind::RevealSecret, DETAILS, T0)
            .unwrap();
        s.decide(edge, session, true, T0 + PROPOSAL_TTL_MS - 10)
            .unwrap();
        assert!(matches!(
            s.claim(edge, session, T0 + PROPOSAL_TTL_MS),
            Err(ApprovalError::Expired)
        ));
    }

    #[test]
    fn sweep_expires_stale_proposals() {
        let (mut s, session) = store();
        s.propose(session, ApprovalKind::Egress, DETAILS, T0)
            .unwrap();
        let a = s
            .propose(session, ApprovalKind::Egress, DETAILS, T0)
            .unwrap();
        s.decide(a, session, true, T0).unwrap();
        assert_eq!(s.sweep(T0 + CLAIM_WINDOW_MS).unwrap(), 1);
        assert_eq!(s.sweep(T0 + PROPOSAL_TTL_MS).unwrap(), 1);
        assert_eq!(s.sweep(T0 + PROPOSAL_TTL_MS).unwrap(), 0);
    }

    #[test]
    fn approvals_are_bound_to_their_session() {
        let (mut s, session) = store();
        let other = s.start_session(T0).unwrap();
        let p = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        assert!(matches!(
            s.decide(p, other, true, T0),
            Err(ApprovalError::WrongSession)
        ));
        s.decide(p, session, true, T0).unwrap();
        assert!(matches!(
            s.claim(p, other, T0),
            Err(ApprovalError::WrongSession)
        ));
        assert!(s.pending(other, T0).unwrap().is_empty());
    }

    #[test]
    fn ending_a_session_cancels_its_approvals() {
        let (mut s, session) = store();
        let pending = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        let approved = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        s.decide(approved, session, true, T0).unwrap();
        s.end_session(session, T0).unwrap();
        assert!(matches!(
            s.claim(approved, session, T0),
            Err(ApprovalError::SessionEnded)
        ));
        assert!(matches!(
            s.decide(pending, session, true, T0),
            Err(ApprovalError::SessionEnded)
        ));
        assert!(matches!(
            s.propose(session, ApprovalKind::OutsideRead, DETAILS, T0),
            Err(ApprovalError::SessionEnded)
        ));
    }

    #[test]
    fn idle_and_absolute_timeouts_end_sessions() {
        let (mut s, session) = store();
        s.touch(session, T0 + SESSION_IDLE_MS - 1).unwrap();
        assert!(matches!(
            s.propose(
                session,
                ApprovalKind::Egress,
                DETAILS,
                T0 + 2 * SESSION_IDLE_MS
            ),
            Err(ApprovalError::SessionEnded)
        ));
        let long = s.start_session(T0).unwrap();
        let mut t = T0;
        while t < T0 + SESSION_ABSOLUTE_MS - SESSION_IDLE_MS {
            t += SESSION_IDLE_MS / 2;
            s.touch(long, t).unwrap();
        }
        assert!(matches!(
            s.touch(long, T0 + SESSION_ABSOLUTE_MS),
            Err(ApprovalError::SessionEnded)
        ));
        let reasons: Vec<_> = s
            .audit_log(100)
            .unwrap()
            .into_iter()
            .filter(|e| e.event == "SessionEnded")
            .filter_map(|e| e.reason)
            .collect();
        assert_eq!(reasons, ["Absolute", "Idle"]);
    }

    #[test]
    fn restart_cancels_everything_from_the_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.db");
        let (session, p1, p2) = {
            let mut s = ApprovalStore::open(&path, T0).unwrap();
            let session = s.start_session(T0).unwrap();
            let p1 = s
                .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
                .unwrap();
            let p2 = s
                .propose(session, ApprovalKind::OutsideWrite, DETAILS, T0)
                .unwrap();
            s.decide(p2, session, true, T0).unwrap();
            (session, p1, p2)
        };
        let mut s = ApprovalStore::open(&path, T0 + 1000).unwrap();
        assert_eq!(s.recovery().cancelled_proposals, 2);
        assert_eq!(s.recovery().ended_sessions, 1);
        assert!(matches!(
            s.claim(p2, session, T0 + 1000),
            Err(ApprovalError::SessionEnded)
        ));
        assert!(matches!(
            s.decide(p1, session, true, T0 + 1000),
            Err(ApprovalError::SessionEnded)
        ));
    }

    #[test]
    fn audit_is_metadata_only_and_details_are_purged() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        s.decide(p, session, false, T0).unwrap();
        let log = s.audit_log(10).unwrap();
        assert!(log.iter().all(|e| {
            let text = format!("{e:?}");
            !text.contains("notes.txt") && !text.contains("Users")
        }));
        let events: Vec<_> = log.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(events, ["Rejected", "Proposed", "SessionStarted"]);
        // Rejecting cleared the details already; nothing is left to purge.
        assert_eq!(details_of(&s, p), None);
        assert_eq!(s.purge(T0 + 1).unwrap(), 0);
        s.purge(T0 + AUDIT_RETENTION_MS + 1).unwrap();
        assert!(s.audit_log(10).unwrap().is_empty());
    }

    fn details_of(s: &ApprovalStore, p: ProposalId) -> Option<String> {
        s.db.query_row(
            "SELECT details FROM proposals WHERE proposal_id = ?1",
            [p.0.to_vec()],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Rewrite the proposals table as an older version had it (`kinds`
    /// replaces the CHECK list's later kinds).
    fn downgrade(s: &ApprovalStore, kinds: &str, version: i64) {
        let old = SCHEMA.replace(
            ", 'McpTool',
        'RiskyCommand', 'HeldBack', 'KeepRunning', 'AfterWeb', 'InitRepository', 'AddToolchain'",
            kinds,
        );
        assert_ne!(old, SCHEMA, "the CHECK list changed: update this test");
        let create = old
            .split("CREATE TABLE proposals (")
            .nth(1)
            .and_then(|r| r.split(") STRICT;").next())
            .unwrap();
        s.db.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF; CREATE TABLE p1 ({create}) STRICT;
             INSERT INTO p1 SELECT * FROM proposals; DROP TABLE proposals;
             ALTER TABLE p1 RENAME TO proposals; PRAGMA user_version = {version}; PRAGMA foreign_keys = ON;"
        ))
        .unwrap();
    }

    #[test]
    fn older_stores_gain_every_kind_and_keep_their_rows() {
        for (kinds, version) in [
            ("", 1),
            (", 'McpTool'", 2),
            (
                ", 'McpTool',
        'RiskyCommand', 'HeldBack', 'KeepRunning', 'AfterWeb'",
                3,
            ),
            (
                ", 'McpTool',
        'RiskyCommand', 'HeldBack', 'KeepRunning', 'AfterWeb', 'InitRepository'",
                4,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("approvals.db");
            {
                let mut s = ApprovalStore::open(&path, T0).unwrap();
                let session = s.start_session(T0).unwrap();
                s.propose(session, ApprovalKind::Egress, DETAILS, T0)
                    .unwrap();
                downgrade(&s, kinds, version);
            }
            let mut s = ApprovalStore::open(&path, T0 + 1000).unwrap();
            let rows: i64 =
                s.db.query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get(0))
                    .unwrap();
            assert_eq!(rows, 1, "version {version}");
            let session = s.start_session(T0 + 1000).unwrap();
            for kind in ApprovalKind::ALL {
                s.propose(session, kind, DETAILS, T0 + 1000).unwrap();
            }
        }
    }

    #[test]
    fn grant_approvals_are_audited_and_are_not_user_activity() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::Egress, DETAILS, T0)
            .unwrap();
        s.approve_by_grant(p, session, T0 + 1000).unwrap();
        assert_eq!(s.claim(p, session, T0 + 1000).unwrap(), DETAILS);
        let log = s.audit_log(10).unwrap();
        assert!(
            log.iter()
                .any(|e| e.event == "Approved" && e.reason.as_deref() == Some("SessionGrant"))
        );
        // Grant approvals alone don't keep an idle session alive.
        let p2 = s
            .propose(
                session,
                ApprovalKind::Egress,
                DETAILS,
                T0 + SESSION_IDLE_MS - 1,
            )
            .unwrap();
        s.approve_by_grant(p2, session, T0 + SESSION_IDLE_MS - 1)
            .unwrap();
        assert!(matches!(
            s.approve_by_grant(p2, session, T0 + SESSION_IDLE_MS + 1),
            Err(ApprovalError::SessionEnded)
        ));
    }

    #[test]
    fn details_are_cleared_as_soon_as_a_proposal_is_decided() {
        let (mut s, session) = store();
        let propose = |s: &mut ApprovalStore| {
            s.propose(session, ApprovalKind::OutsideWrite, DETAILS, T0)
                .unwrap()
        };
        // Pending and approved proposals keep them: the dialog and the
        // claim need them.
        let claimed = propose(&mut s);
        assert_eq!(details_of(&s, claimed).as_deref(), Some(DETAILS));
        s.decide(claimed, session, true, T0).unwrap();
        assert_eq!(details_of(&s, claimed).as_deref(), Some(DETAILS));
        // The claim hands them over once, then they are gone.
        assert_eq!(s.claim(claimed, session, T0).unwrap(), DETAILS);
        assert_eq!(details_of(&s, claimed), None);

        let rejected = propose(&mut s);
        s.decide(rejected, session, false, T0).unwrap();
        assert_eq!(details_of(&s, rejected), None);

        let cancelled = propose(&mut s);
        s.cancel(cancelled, session, T0).unwrap();
        assert_eq!(details_of(&s, cancelled), None);

        let late = propose(&mut s);
        assert!(matches!(
            s.decide(late, session, true, T0 + PROPOSAL_TTL_MS),
            Err(ApprovalError::Expired)
        ));
        assert_eq!(details_of(&s, late), None);

        let unclaimed = propose(&mut s);
        s.decide(unclaimed, session, true, T0).unwrap();
        assert!(matches!(
            s.claim(unclaimed, session, T0 + CLAIM_WINDOW_MS),
            Err(ApprovalError::Expired)
        ));
        assert_eq!(details_of(&s, unclaimed), None);

        let swept = propose(&mut s);
        s.sweep(T0 + PROPOSAL_TTL_MS).unwrap();
        assert_eq!(details_of(&s, swept), None);

        let session2 = s.start_session(T0).unwrap();
        let open = s
            .propose(session2, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        s.end_session(session2, T0).unwrap();
        assert_eq!(details_of(&s, open), None);

        // Nothing decided holds details any more.
        let left: i64 =
            s.db.query_row(
                "SELECT COUNT(*) FROM proposals WHERE details IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    fn files_contain(dir: &Path, marker: &str) -> bool {
        std::fs::read_dir(dir).unwrap().any(|entry| {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap_or_default();
            bytes.windows(marker.len()).any(|w| w == marker.as_bytes())
        })
    }

    #[test]
    fn decided_details_do_not_remain_in_the_database_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.db");
        let marker = "APPROVAL-CANARY-41c9";
        let details = format!(r#"{{"diff":"+API_KEY={marker}"}}"#);
        let mut s = ApprovalStore::open(&path, T0).unwrap();
        let session = s.start_session(T0).unwrap();
        let p = s
            .propose(session, ApprovalKind::WriteSecret, &details, T0)
            .unwrap();
        assert!(files_contain(dir.path(), marker), "the canary was written");
        s.decide(p, session, true, T0).unwrap();
        s.claim(p, session, T0).unwrap();
        assert!(!files_contain(dir.path(), marker));
        drop(s);
        assert!(!files_contain(dir.path(), marker));
    }

    #[test]
    fn details_left_by_a_crash_are_cleared_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.db");
        let marker = "CRASH-CANARY-0b7e";
        {
            let mut s = ApprovalStore::open(&path, T0).unwrap();
            let session = s.start_session(T0).unwrap();
            s.propose(
                session,
                ApprovalKind::OutsideWrite,
                &format!(r#"{{"content":"{marker}"}}"#),
                T0,
            )
            .unwrap();
            // Dropped without a decision, as when the app crashes.
        }
        let s = ApprovalStore::open(&path, T0 + 1000).unwrap();
        assert_eq!(s.recovery().cancelled_proposals, 1);
        drop(s);
        assert!(!files_contain(dir.path(), marker));
    }

    #[test]
    fn details_kept_by_earlier_versions_are_purged() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::OutsideRead, DETAILS, T0)
            .unwrap();
        // An earlier version set the terminal state but kept the details.
        s.db.execute(
            "UPDATE proposals SET state = 'Rejected', terminal_at_ms = ?2 WHERE proposal_id = ?1",
            params![p.0.to_vec(), T0],
        )
        .unwrap();
        assert_eq!(s.purge(T0).unwrap(), 1);
        assert_eq!(details_of(&s, p), None);
    }

    #[test]
    fn cancel_and_claim_first_commit_wins() {
        let (mut s, session) = store();
        let p = s
            .propose(session, ApprovalKind::Egress, DETAILS, T0)
            .unwrap();
        s.decide(p, session, true, T0).unwrap();
        s.cancel(p, session, T0).unwrap();
        assert!(matches!(
            s.claim(p, session, T0),
            Err(ApprovalError::InvalidState(ProposalState::Cancelled))
        ));
        let q = s
            .propose(session, ApprovalKind::Egress, DETAILS, T0)
            .unwrap();
        s.decide(q, session, true, T0).unwrap();
        s.claim(q, session, T0).unwrap();
        assert!(matches!(
            s.cancel(q, session, T0),
            Err(ApprovalError::InvalidState(ProposalState::Claimed))
        ));
    }

    #[test]
    fn security_flags_are_logged_bounded_and_on_one_line() {
        let (mut s, session) = store();
        let long = format!("AWS access key\n{}", "x".repeat(500));
        s.note_flag(Some(session), "SecretInCommit", &long, T0 + 5)
            .unwrap();
        let log = s.audit_log(10).unwrap();
        let entry = &log[0];
        assert_eq!(entry.event, "SecurityFlag");
        assert_eq!(entry.kind.as_deref(), Some("SecretInCommit"));
        assert_eq!(entry.session, Some(session.to_hex()));
        let reason = entry.reason.as_deref().unwrap();
        assert_eq!(reason.chars().count(), 200);
        assert!(!reason.contains('\n'));
    }

    #[test]
    fn ids_round_trip_and_reject_garbage() {
        let id = Id::generate();
        assert_eq!(Id::from_hex(&id.to_hex()), Some(id));
        assert_eq!(Id::from_hex("xyz"), None);
        assert_eq!(Id::from_hex(&"g".repeat(32)), None);
    }
}
