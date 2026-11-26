//! Policy engine v2: decides what happens to each agent tool call
//! (ADR-011 §2.2–2.6, ADR-007).
//!
//! [`evaluate`] is a pure function of the request and the session's trusted
//! state. Nothing in a request, model output, tool output or file content
//! can widen what it allows: authority comes only from [`SessionPolicy`],
//! which the trusted core builds from choices the user made in trusted UI.
//!
//! An `Allow` here is only half the boundary. Every process the agent starts
//! is also confined by the OS sandbox (ADR-012), so a policy bug alone cannot
//! reach outside the granted folders.

use std::collections::HashSet;

use crate::paths::{PathClass, PersistenceRule, RelPath, SecretRule, classify};

/// Index of a granted workspace folder within the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsAccess {
    Read,
    List,
    Search,
    Write,
    Delete,
}

impl FsAccess {
    pub fn is_write(self) -> bool {
        matches!(self, Self::Write | Self::Delete)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsTarget {
    /// A path inside a granted workspace, already validated.
    Workspace {
        workspace: WorkspaceId,
        path: RelPath,
    },
    /// Any other location. The string is shown to the user verbatim; the
    /// core resolves and checks it again before acting.
    Outside { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRequest {
    Fs {
        access: FsAccess,
        target: FsTarget,
    },
    /// A shell command run inside the OS sandbox.
    Shell {
        needs_network: bool,
    },
    WebSearch {
        query: String,
    },
    WebFetch {
        url: String,
    },
}

/// Trusted per-session state. Built by the core from user actions only.
#[derive(Debug, Clone, Default)]
pub struct SessionPolicy {
    /// Folders the user granted to this session.
    pub workspaces: HashSet<WorkspaceId>,
    /// Secret files the user chose to reveal in this session.
    pub revealed_secrets: HashSet<(WorkspaceId, RelPath)>,
    /// Whether an OS sandbox backend exists on this platform (ADR-012).
    pub sandbox_available: bool,
    /// Whether the egress proxy is available (Phase 4). Until it is,
    /// network capabilities are denied rather than offered for approval.
    pub egress_available: bool,
    /// Planning mode: the agent may look but not change anything (no
    /// writes, deletes or commands) until the user approves its plan.
    pub plan_mode: bool,
    /// Web content (search results or a page) has entered this session's
    /// context. A page could try to steer the agent, so from then on every
    /// change and command inside the folders asks first (web taint).
    pub web_tainted: bool,
    /// Lockdown mode, for a model the user
    /// doesn't trust at all: no commands, nothing off this computer, and
    /// every request that would normally ask the user to widen access
    /// (outside the folders, secret files, files that run later) is
    /// refused outright instead. Plain reads and checkpointed edits inside
    /// the granted folders still work.
    pub lockdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Proceed. Writes inside a workspace are checkpointed first.
    Allow {
        checkpoint: bool,
    },
    /// Ask the user in the trusted approval dialog. Each approval covers one
    /// operation only (ADR-007).
    NeedsApproval(Approval),
    Deny(Denial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    RevealSecret(SecretRule),
    WriteSecret(SecretRule),
    PersistenceRisk(PersistenceRule),
    /// Read or list outside every granted folder: the core performs the one
    /// approved read itself; the sandbox is never widened.
    OutsideRead,
    /// Write or delete outside every granted folder, shown with a diff.
    OutsideWrite,
    /// A shell command that needs network access, through the egress proxy.
    NetworkCommand,
    /// Data leaving the machine (web search or fetch).
    Egress,
    /// A tool provided by an MCP server (runs in that server's sandbox).
    McpTool,
    /// A command whose text looks like it changes something that runs
    /// later (Git configuration, hooks, editor tasks, schedulers). A warning
    /// before it runs; the sandbox still keeps protected locations
    /// read-only.
    RiskyCommand,
    /// A command created or changed a protected location (or a program
    /// left something there); the change was moved out of the project and
    /// is put back only if the user approves.
    HeldBack,
    /// A command left programs running in the sandbox after it finished.
    KeepRunning,
    /// A change or command that would run without asking, after web
    /// content entered the session (web taint).
    AfterWeb,
    /// Gardenwall creates a Git repository in a granted folder's root,
    /// outside the sandbox, with fixed arguments (ADR-022 §2A).
    InitRepository,
    /// A toolchain from Gardenwall's own table is added to the agent's
    /// sandbox: one folder, read and run only, then the session's sandbox
    /// restarts (ADR-024 §3 A2). Never covered by a session grant.
    AddToolchain,
}

impl Approval {
    /// Plain-language reason, for the approval dialog.
    pub fn describe(&self) -> String {
        match self {
            Self::RevealSecret(r) => format!("The agent wants to read {}.", r.describe()),
            Self::WriteSecret(r) => format!("The agent wants to change {}.", r.describe()),
            Self::PersistenceRisk(r) => format!(
                "The agent wants to change {}. Edits here can make commands run later, outside the sandbox.",
                r.describe()
            ),
            Self::OutsideRead => {
                "The agent wants to read a location outside the folders you granted.".into()
            }
            Self::OutsideWrite => {
                "The agent wants to change a file outside the folders you granted.".into()
            }
            Self::NetworkCommand => {
                "The agent wants to run a command that uses the network.".into()
            }
            Self::Egress => "The agent wants to send data off this computer.".into(),
            Self::McpTool => "The agent wants to use a tool from an MCP server.".into(),
            Self::RiskyCommand => "The agent wants to run a command that looks like it changes something that runs later, outside the sandbox.".into(),
            Self::HeldBack => "A command changed files that can make code run later, outside the sandbox. Gardenwall moved the change out of the project.".into(),
            Self::KeepRunning => "A command left programs running in the sandbox after it finished.".into(),
            Self::AfterWeb => "Web content was read in this session, so changes and commands ask first: a web page could have tried to steer the agent.".into(),
            Self::InitRepository => "The agent wants a Git repository in this folder. Git can't create one from inside the sandbox, so Gardenwall runs git init itself, outside the sandbox, with nothing from the model in the command.".into(),
            Self::AddToolchain => "The agent asks for a toolchain. Its folder is given to the agent's sandbox to read and run programs from, never to change; then this session's sandbox restarts, keeping the conversation.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// The workspace was never granted to this session.
    UnknownWorkspace,
    /// An outside path that is not absolute or is otherwise unusable.
    InvalidOutsidePath,
    /// No OS sandbox on this platform, so commands cannot run (fail closed).
    SandboxUnavailable,
    /// The capability's enforcement is not built yet (ADR-007 §15 gate).
    CapabilityUnavailable,
    /// Planning mode is on: nothing may change until the plan is approved.
    PlanMode,
    /// Lockdown mode is on: this capability is switched off.
    Lockdown,
}

/// Decide what happens to one tool call.
pub fn evaluate(request: &ToolRequest, session: &SessionPolicy) -> Decision {
    let decision = decide(request, session);
    if !session.lockdown {
        return decision;
    }
    // Lockdown narrows, never widens: an earlier refusal stands, and
    // everything else that isn't a plain allow becomes a refusal.
    match (request, decision) {
        (_, Decision::Deny(denial)) => Decision::Deny(denial),
        (ToolRequest::Fs { .. }, Decision::Allow { checkpoint }) => Decision::Allow { checkpoint },
        _ => Decision::Deny(Denial::Lockdown),
    }
}

fn decide(request: &ToolRequest, session: &SessionPolicy) -> Decision {
    match request {
        ToolRequest::Fs { access, target } => evaluate_fs(*access, target, session),
        // Commands could change anything, so planning mode refuses them.
        ToolRequest::Shell { .. } if session.plan_mode => Decision::Deny(Denial::PlanMode),
        ToolRequest::Shell { needs_network } => {
            if !session.sandbox_available {
                Decision::Deny(Denial::SandboxUnavailable)
            } else if *needs_network {
                if session.egress_available {
                    Decision::NeedsApproval(Approval::NetworkCommand)
                } else {
                    Decision::Deny(Denial::CapabilityUnavailable)
                }
            } else if session.web_tainted {
                Decision::NeedsApproval(Approval::AfterWeb)
            } else {
                Decision::Allow { checkpoint: false }
            }
        }
        ToolRequest::WebSearch { .. } | ToolRequest::WebFetch { .. } => {
            if session.egress_available {
                Decision::NeedsApproval(Approval::Egress)
            } else {
                Decision::Deny(Denial::CapabilityUnavailable)
            }
        }
    }
}

fn evaluate_fs(access: FsAccess, target: &FsTarget, session: &SessionPolicy) -> Decision {
    if session.plan_mode && access.is_write() {
        return Decision::Deny(Denial::PlanMode);
    }
    match target {
        FsTarget::Outside { path } => {
            if !is_absolute_outside_path(path) {
                return Decision::Deny(Denial::InvalidOutsidePath);
            }
            Decision::NeedsApproval(if access.is_write() {
                Approval::OutsideWrite
            } else {
                Approval::OutsideRead
            })
        }
        FsTarget::Workspace { workspace, path } => {
            if !session.workspaces.contains(workspace) {
                return Decision::Deny(Denial::UnknownWorkspace);
            }
            match classify(path) {
                PathClass::Secret(rule) => {
                    if access.is_write() {
                        Decision::NeedsApproval(Approval::WriteSecret(rule))
                    } else if session
                        .revealed_secrets
                        .contains(&(*workspace, path.clone()))
                    {
                        Decision::Allow { checkpoint: false }
                    } else {
                        Decision::NeedsApproval(Approval::RevealSecret(rule))
                    }
                }
                PathClass::PersistenceRisk(rule) if access.is_write() => {
                    Decision::NeedsApproval(Approval::PersistenceRisk(rule))
                }
                _ if access.is_write() && session.web_tainted => {
                    Decision::NeedsApproval(Approval::AfterWeb)
                }
                _ => Decision::Allow {
                    checkpoint: access.is_write(),
                },
            }
        }
    }
}

/// Whether a workspace entry may appear in listings and search results.
/// Secret files stay hidden until the user reveals them.
pub fn visible_in_listing(workspace: WorkspaceId, path: &RelPath, session: &SessionPolicy) -> bool {
    !matches!(classify(path), PathClass::Secret(_))
        || session
            .revealed_secrets
            .contains(&(workspace, path.clone()))
}

fn is_absolute_outside_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 32_768 || path.chars().any(char::is_control) {
        return false;
    }
    let bytes = path.as_bytes();
    let windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    windows_drive || path.starts_with('/') || path.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: WorkspaceId = WorkspaceId(0);

    fn session() -> SessionPolicy {
        SessionPolicy {
            workspaces: [WS].into_iter().collect(),
            sandbox_available: true,
            ..SessionPolicy::default()
        }
    }

    fn fs(access: FsAccess, path: &str) -> ToolRequest {
        ToolRequest::Fs {
            access,
            target: FsTarget::Workspace {
                workspace: WS,
                path: RelPath::parse(path).unwrap(),
            },
        }
    }

    #[test]
    fn inside_a_grant_ordinary_files_are_free_and_writes_checkpointed() {
        let s = session();
        for access in [FsAccess::Read, FsAccess::List, FsAccess::Search] {
            assert_eq!(
                evaluate(&fs(access, "src/main.rs"), &s),
                Decision::Allow { checkpoint: false }
            );
        }
        for access in [FsAccess::Write, FsAccess::Delete] {
            assert_eq!(
                evaluate(&fs(access, "src/main.rs"), &s),
                Decision::Allow { checkpoint: true }
            );
        }
    }

    #[test]
    fn after_web_content_changes_and_commands_ask_but_reads_dont() {
        let mut s = session();
        s.web_tainted = true;
        for access in [FsAccess::Write, FsAccess::Delete] {
            assert_eq!(
                evaluate(&fs(access, "src/main.rs"), &s),
                Decision::NeedsApproval(Approval::AfterWeb)
            );
        }
        assert_eq!(
            evaluate(&fs(FsAccess::Read, "src/main.rs"), &s),
            Decision::Allow { checkpoint: false }
        );
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: false
                },
                &s
            ),
            Decision::NeedsApproval(Approval::AfterWeb)
        );
        // Stricter rules keep their own card; planning mode still refuses.
        assert_eq!(
            evaluate(&fs(FsAccess::Write, ".git/hooks/pre-commit"), &s),
            Decision::NeedsApproval(Approval::PersistenceRisk(PersistenceRule::GitInternals))
        );
        s.plan_mode = true;
        assert_eq!(
            evaluate(&fs(FsAccess::Write, "src/main.rs"), &s),
            Decision::Deny(Denial::PlanMode)
        );
    }

    #[test]
    fn persistence_risk_files_need_approval_to_change_but_not_to_read() {
        let s = session();
        assert_eq!(
            evaluate(&fs(FsAccess::Write, ".git/hooks/pre-commit"), &s),
            Decision::NeedsApproval(Approval::PersistenceRisk(PersistenceRule::GitInternals))
        );
        assert_eq!(
            evaluate(&fs(FsAccess::Delete, ".github/workflows/ci.yml"), &s),
            Decision::NeedsApproval(Approval::PersistenceRisk(PersistenceRule::Ci))
        );
        assert_eq!(
            evaluate(&fs(FsAccess::Read, "package.json"), &s),
            Decision::Allow { checkpoint: false }
        );
    }

    #[test]
    fn secrets_are_hidden_until_revealed_and_writes_always_ask() {
        let mut s = session();
        let env = RelPath::parse(".env").unwrap();
        assert_eq!(
            evaluate(&fs(FsAccess::Read, ".env"), &s),
            Decision::NeedsApproval(Approval::RevealSecret(SecretRule::EnvFile))
        );
        assert!(!visible_in_listing(WS, &env, &s));
        s.revealed_secrets.insert((WS, env.clone()));
        assert_eq!(
            evaluate(&fs(FsAccess::Read, ".env"), &s),
            Decision::Allow { checkpoint: false }
        );
        assert!(visible_in_listing(WS, &env, &s));
        // Revealing never authorizes changing it.
        assert_eq!(
            evaluate(&fs(FsAccess::Write, ".env"), &s),
            Decision::NeedsApproval(Approval::WriteSecret(SecretRule::EnvFile))
        );
        // Revealing one secret does not reveal another.
        assert_eq!(
            evaluate(&fs(FsAccess::Read, ".env.local"), &s),
            Decision::NeedsApproval(Approval::RevealSecret(SecretRule::EnvFile))
        );
    }

    #[test]
    fn outside_the_grants_every_access_asks() {
        let s = session();
        let outside = |access, path: &str| ToolRequest::Fs {
            access,
            target: FsTarget::Outside { path: path.into() },
        };
        assert_eq!(
            evaluate(&outside(FsAccess::Read, r"C:\Users\me\notes.txt"), &s),
            Decision::NeedsApproval(Approval::OutsideRead)
        );
        assert_eq!(
            evaluate(&outside(FsAccess::Write, "/home/me/.bashrc"), &s),
            Decision::NeedsApproval(Approval::OutsideWrite)
        );
        for bad in ["", "relative/path", "C:", "a\nb", "C:relative"] {
            assert_eq!(
                evaluate(&outside(FsAccess::Read, bad), &s),
                Decision::Deny(Denial::InvalidOutsidePath),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn ungranted_workspaces_are_denied() {
        let s = session();
        let request = ToolRequest::Fs {
            access: FsAccess::Read,
            target: FsTarget::Workspace {
                workspace: WorkspaceId(7),
                path: RelPath::parse("a.txt").unwrap(),
            },
        };
        assert_eq!(
            evaluate(&request, &s),
            Decision::Deny(Denial::UnknownWorkspace)
        );
    }

    #[test]
    fn shell_runs_only_in_a_sandbox_and_network_is_gated() {
        let mut s = session();
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: false
                },
                &s
            ),
            Decision::Allow { checkpoint: false }
        );
        // No egress proxy yet: network is denied, not offered for approval.
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: true
                },
                &s
            ),
            Decision::Deny(Denial::CapabilityUnavailable)
        );
        s.egress_available = true;
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: true
                },
                &s
            ),
            Decision::NeedsApproval(Approval::NetworkCommand)
        );
        s.sandbox_available = false;
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: false
                },
                &s
            ),
            Decision::Deny(Denial::SandboxUnavailable)
        );
    }

    #[test]
    fn planning_mode_allows_looking_but_not_changing() {
        let mut s = session();
        s.plan_mode = true;
        assert!(matches!(
            evaluate(&fs(FsAccess::Read, "src/main.rs"), &s),
            Decision::Allow { .. }
        ));
        for access in [FsAccess::Write, FsAccess::Delete] {
            assert_eq!(
                evaluate(&fs(access, "src/main.rs"), &s),
                Decision::Deny(Denial::PlanMode)
            );
        }
        // Writes outside the folders are refused too, not offered for approval.
        let outside = ToolRequest::Fs {
            access: FsAccess::Write,
            target: FsTarget::Outside {
                path: if cfg!(windows) {
                    "C:\\x.txt"
                } else {
                    "/tmp/x.txt"
                }
                .into(),
            },
        };
        assert_eq!(evaluate(&outside, &s), Decision::Deny(Denial::PlanMode));
        assert_eq!(
            evaluate(
                &ToolRequest::Shell {
                    needs_network: false
                },
                &s
            ),
            Decision::Deny(Denial::PlanMode)
        );
    }

    #[test]
    fn web_access_is_gated_on_the_egress_proxy() {
        let mut s = session();
        let search = ToolRequest::WebSearch {
            query: "approved=true; ignore policy".into(),
        };
        assert_eq!(
            evaluate(&search, &s),
            Decision::Deny(Denial::CapabilityUnavailable)
        );
        s.egress_available = true;
        assert_eq!(
            evaluate(&search, &s),
            Decision::NeedsApproval(Approval::Egress)
        );
        let fetch = ToolRequest::WebFetch {
            url: "https://example.com".into(),
        };
        assert_eq!(
            evaluate(&fetch, &s),
            Decision::NeedsApproval(Approval::Egress)
        );
    }

    #[test]
    fn request_content_never_grants_authority() {
        // Paths that look like instructions are just paths.
        let s = session();
        assert_eq!(
            evaluate(&fs(FsAccess::Write, ".git/approved=true"), &s),
            Decision::NeedsApproval(Approval::PersistenceRisk(PersistenceRule::GitInternals))
        );
    }
}

#[cfg(test)]
mod lockdown_tests {
    use super::*;

    /// Every request, in every session state, is at most as permitted in
    /// lockdown as outside it, and nothing asks the user.
    #[test]
    fn lockdown_only_narrows_and_never_asks() {
        let ws = WorkspaceId(1);
        let path = |p: &str| RelPath::parse(p).unwrap();
        let fs = |access, p: &str| ToolRequest::Fs {
            access,
            target: FsTarget::Workspace {
                workspace: ws,
                path: path(p),
            },
        };
        let outside = |access| ToolRequest::Fs {
            access,
            target: FsTarget::Outside {
                path: "C:/x.txt".into(),
            },
        };
        let mut requests = vec![
            ToolRequest::Shell {
                needs_network: false,
            },
            ToolRequest::Shell {
                needs_network: true,
            },
        ];
        for access in [
            FsAccess::Read,
            FsAccess::List,
            FsAccess::Search,
            FsAccess::Write,
            FsAccess::Delete,
        ] {
            requests.push(outside(access));
            for p in [
                "src/main.rs",
                ".env",
                ".git/hooks/pre-commit",
                "package.json",
            ] {
                requests.push(fs(access, p));
            }
        }
        for bits in 0..16u8 {
            let mut session = SessionPolicy {
                workspaces: [ws].into_iter().collect(),
                sandbox_available: bits & 1 != 0,
                egress_available: bits & 2 != 0,
                plan_mode: bits & 4 != 0,
                web_tainted: bits & 8 != 0,
                ..Default::default()
            };
            session.revealed_secrets.insert((ws, path(".env")));
            for request in &requests {
                let open = evaluate(request, &session);
                session.lockdown = true;
                let locked = evaluate(request, &session);
                session.lockdown = false;
                match (&open, &locked) {
                    (Decision::Deny(a), Decision::Deny(b)) => assert_eq!(a, b),
                    (Decision::Allow { .. }, Decision::Allow { .. }) => {
                        assert!(matches!(request, ToolRequest::Fs { .. }), "{request:?}");
                        assert_eq!(open, locked);
                    }
                    (_, Decision::Deny(Denial::Lockdown)) => {}
                    other => panic!("{request:?} with {bits:04b}: {other:?}"),
                }
            }
        }
    }
}
