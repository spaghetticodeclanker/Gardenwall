//! Agent tools (ADR-014 §2.1), with Pi's names, parameter schemas and
//! descriptions (pi-mono commit 682c7b9, `packages/coding-agent/src/core/tools`;
//! Copyright (c) 2025 Mario Zechner, MIT License; see THIRD_PARTY_NOTICES.md).
//!
//! Every call is resolved to a workspace path (or an outside path), checked
//! by the policy engine, and only then performed:
//! - inside a granted folder, through its capability handle; writes are
//!   checkpointed first;
//! - anything needing approval returns [`ToolOutcome::NeedsApproval`] with the
//!   exact operation; the loop asks the user and calls [`execute_approved`]
//!   after a successful single-use claim;
//! - outside reads and writes are carried out by the core itself after
//!   approval; the sandbox is never widened.
//!
//! Tool arguments and file contents are untrusted data.

mod workspaces;

use std::path::{Path, PathBuf};

use gardenwall_policy::paths::RelPath;
use gardenwall_policy::tools::{
    Approval, Decision, Denial, FsAccess, FsTarget, SessionPolicy, ToolRequest, WorkspaceId,
    evaluate, visible_in_listing,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use workspaces::{CommandMarks, CommandReport, GrantedWorkspace, Resolved, Workspaces};

use crate::edit_text::{Edit, apply_edits};
use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH, TruncatedBy, format_size,
    truncate_head, truncate_line,
};

/// Largest file the tools will read into memory.
pub const MAX_READ_BYTES: u64 = 64 << 20;

/// A tool as offered to the model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: String,
    pub parameters: Value,
    /// One-line summary for the system prompt.
    pub snippet: &'static str,
    /// Guideline bullets the tool contributes to the system prompt.
    pub guidelines: &'static [&'static str],
}

/// The file tools. `bash` is defined by the sandboxed shell runner.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read",
            description: format!(
                "Read the contents of a file. Supports text files. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to read (relative or absolute)"},
                    "offset": {"type": "number", "description": "Line number to start reading from (1-indexed)"},
                    "limit": {"type": "number", "description": "Maximum number of lines to read"}
                },
                "required": ["path"]
            }),
            snippet: "Read file contents",
            guidelines: &["Use read to examine files instead of cat or sed."],
        },
        ToolDef {
            name: "write",
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to write (relative or absolute)"},
                    "content": {"type": "string", "description": "Content to write to the file"}
                },
                "required": ["path", "content"]
            }),
            snippet: "Create or overwrite files",
            guidelines: &["Use write only for new files or complete rewrites."],
        },
        ToolDef {
            name: "edit",
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to edit (relative or absolute)"},
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {"type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."},
                                "newText": {"type": "string", "description": "Replacement text for this targeted edit."}
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            snippet: "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
            guidelines: &[
                "Use edit for targeted changes. Batch changes to one file in a single call.",
                "Each edits[].oldText must exactly and uniquely match the original file. Keep matches short and non-overlapping; merge nearby changes without including large unchanged regions.",
            ],
        },
        ToolDef {
            name: "ls",
            description: format!(
                "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {LS_LIMIT} entries or {}KB (whichever is hit first).",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory to list (default: current directory)"},
                    "limit": {"type": "number", "description": "Maximum number of entries to return (default: 500)"}
                }
            }),
            snippet: "List directory contents",
            guidelines: &[],
        },
        ToolDef {
            name: "find",
            description: format!(
                "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {FIND_LIMIT} results or {}KB (whichever is hit first).",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"},
                    "path": {"type": "string", "description": "Directory to search in (default: current directory)"},
                    "limit": {"type": "number", "description": "Maximum number of results (default: 1000)"}
                },
                "required": ["pattern"]
            }),
            snippet: "Find files by glob pattern (respects .gitignore)",
            guidelines: &[],
        },
        ToolDef {
            name: "grep",
            description: format!(
                "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {GREP_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Search pattern (regex or literal string)"},
                    "path": {"type": "string", "description": "Directory or file to search (default: current directory)"},
                    "glob": {"type": "string", "description": "Filter files by glob pattern, relative to path, e.g. '*.ts', 'src/**/*.ts' or '**/*.spec.ts'. A pattern without '/' matches file names at any depth"},
                    "ignoreCase": {"type": "boolean", "description": "Case-insensitive search (default: false)"},
                    "literal": {"type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)"},
                    "context": {"type": "number", "description": "Number of lines to show before and after each match (default: 0)"},
                    "limit": {"type": "number", "description": "Maximum number of matches to return (default: 100)"}
                },
                "required": ["pattern"]
            }),
            snippet: "Search file contents for patterns (respects .gitignore)",
            guidelines: &[],
        },
    ]
}

const LS_LIMIT: usize = 500;
const FIND_LIMIT: usize = 1000;
const GREP_LIMIT: usize = 100;

/// The result of running a tool.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// Text returned to the model. `is_error` results are failures the model
    /// should see and react to.
    Done { text: String, is_error: bool },
    /// The user must approve first. `details` is the JSON shown in the
    /// trusted dialog; `pending` is executed only after a successful claim.
    NeedsApproval {
        approval: Approval,
        details: String,
        pending: PendingOperation,
    },
}

impl ToolOutcome {
    fn ok(text: impl Into<String>) -> Self {
        Self::Done {
            text: text.into(),
            is_error: false,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Self::Done {
            text: text.into(),
            is_error: true,
        }
    }
}

/// An operation waiting for approval. Stored as the proposal's immutable
/// details and executed exactly as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOperation {
    pub tool: String,
    pub args: Value,
}

/// Details shown in the approval dialog (`ApprovalDetails` in the UI).
#[derive(Serialize)]
struct DialogDetails<'a> {
    reason: String,
    operation: &'a str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    /// The exact operation that will run (for audit before execution).
    pending: &'a PendingOperation,
}

fn approval(
    approval: Approval,
    operation: &str,
    target: String,
    diff: Option<String>,
    pending: PendingOperation,
) -> ToolOutcome {
    let details = serde_json::to_string(&DialogDetails {
        reason: approval.describe(),
        operation,
        target,
        diff,
        pending: &pending,
    })
    .unwrap_or_default();
    ToolOutcome::NeedsApproval {
        approval,
        details,
        pending,
    }
}

fn denial_text(denial: Denial) -> String {
    match denial {
        Denial::UnknownWorkspace => "That folder has not been granted to this session.".into(),
        Denial::PlanMode => "Planning mode is on: nothing can be changed and no commands can run until the user approves your plan. Finish exploring, then present a numbered plan.".into(),
        Denial::Lockdown => "Lockdown mode is on: commands, the web, files outside the granted folders, secret files and files that can run code later are all off. Work with the files in the granted folders only, and don't look for another way.".into(),
        Denial::InvalidOutsidePath => "Paths outside the granted folders must be absolute.".into(),
        Denial::SandboxUnavailable => {
            "Commands cannot run: no sandbox is available on this platform.".into()
        }
        Denial::CapabilityUnavailable => {
            "This capability is not available in Gardenwall yet.".into()
        }
    }
}

/// Everything a tool call may touch, owned by the trusted core.
pub struct ToolContext<'a> {
    pub workspaces: &'a mut Workspaces,
    pub policy: &'a SessionPolicy,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn num_arg(args: &Value, key: &str) -> Option<f64> {
    args.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Run a tool call proposed by the model.
pub fn execute(name: &str, args: &Value, ctx: &mut ToolContext<'_>) -> ToolOutcome {
    run(name, args, ctx, false)
}

/// Run an operation the user approved, after its claim succeeded. The
/// operation is exactly the one stored with the proposal.
pub fn execute_approved(pending: &PendingOperation, ctx: &mut ToolContext<'_>) -> ToolOutcome {
    run(&pending.tool, &pending.args, ctx, true)
}

fn run(name: &str, args: &Value, ctx: &mut ToolContext<'_>, approved: bool) -> ToolOutcome {
    if !args.is_object() {
        return ToolOutcome::err(format!(
            "Invalid arguments for {name}: expected a JSON object."
        ));
    }
    match name {
        "read" => read(args, ctx, approved),
        "write" => write(args, ctx, approved),
        "edit" => edit(args, ctx, approved),
        "ls" => ls(args, ctx, approved),
        "find" => find(args, ctx),
        "grep" => grep(args, ctx),
        other => ToolOutcome::err(format!("Unknown tool: {other}")),
    }
}

/// Resolve and evaluate one path access. Returns the decision and target.
fn check(
    raw: &str,
    access: FsAccess,
    ctx: &ToolContext<'_>,
) -> Result<(Decision, Resolved), ToolOutcome> {
    let resolved = ctx.workspaces.resolve(raw).map_err(ToolOutcome::err)?;
    let target = match &resolved {
        Resolved::Inside(ws, path) => FsTarget::Workspace {
            workspace: *ws,
            path: path.clone(),
        },
        // A workspace root is always ordinary.
        Resolved::Root(_) => return Ok((Decision::Allow { checkpoint: false }, resolved)),
        Resolved::Outside(p) => FsTarget::Outside {
            path: p.display().to_string(),
        },
    };
    Ok((
        evaluate(&ToolRequest::Fs { access, target }, ctx.policy),
        resolved,
    ))
}

fn display(resolved: &Resolved, ws: &Workspaces) -> String {
    match resolved {
        Resolved::Inside(id, p) => ws.absolute(*id, Some(p)).display().to_string(),
        Resolved::Root(id) => ws.absolute(*id, None).display().to_string(),
        Resolved::Outside(p) => p.display().to_string(),
    }
}

fn read_text(bytes: Vec<u8>, path: &str) -> Result<String, ToolOutcome> {
    String::from_utf8(bytes).map_err(|e| {
        ToolOutcome::err(format!(
            "{path} is not a UTF-8 text file ({} bytes); it cannot be read as text.",
            e.as_bytes().len()
        ))
    })
}

fn read_outside(path: &Path) -> Result<Vec<u8>, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(format!(
            "{} is too large to read ({})",
            path.display(),
            format_size(meta.len() as usize)
        ));
    }
    std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

fn read(args: &Value, ctx: &mut ToolContext<'_>, approved: bool) -> ToolOutcome {
    let Some(raw) = str_arg(args, "path") else {
        return ToolOutcome::err("read requires a path.");
    };
    let (decision, resolved) = match check(raw, FsAccess::Read, ctx) {
        Ok(v) => v,
        Err(e) => return e,
    };
    match decision {
        Decision::Deny(d) => return ToolOutcome::err(denial_text(d)),
        Decision::NeedsApproval(a) if !approved => {
            let target = display(&resolved, ctx.workspaces);
            let pending = PendingOperation {
                tool: "read".into(),
                args: args.clone(),
            };
            return approval(a, "Read file", target, None, pending);
        }
        _ => {}
    }
    let bytes = match &resolved {
        Resolved::Inside(ws, p) => ctx.workspaces.read(*ws, p, MAX_READ_BYTES),
        Resolved::Root(_) => Err(format!("{raw} is a directory; use ls to list it.")),
        Resolved::Outside(p) => read_outside(p),
    };
    let bytes = match bytes {
        Ok(b) => b,
        Err(e) => return ToolOutcome::err(e),
    };
    let text = match read_text(bytes, raw) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (offset, limit) = (num_arg(args, "offset"), num_arg(args, "limit"));
    if ctx.workspaces.whole_reads() && offset.is_none() && limit.is_none() {
        return if text.len() <= PROGRAM_READ_BYTES {
            ToolOutcome::Done {
                text,
                is_error: false,
            }
        } else {
            ToolOutcome::err(format!(
                "{raw} is larger than {}: read it in parts with offset and limit.",
                format_size(PROGRAM_READ_BYTES)
            ))
        };
    }
    select_lines(&text, raw, offset, limit)
}

/// Largest file a code-mode program reads whole.
pub const PROGRAM_READ_BYTES: usize = 1024 * 1024;

/// Largest file the binary analyzer is given.
pub const MAX_BINARY_BYTES: u64 = 256 << 20;

/// The first step of `binary` and `decompile`: the file's bytes, read like
/// `read` (same path rules, policy and approval). The bytes then go to the
/// sandboxed analyzer; the core never parses them.
pub enum BinaryRead {
    Bytes(Vec<u8>),
    Outcome(ToolOutcome),
}

pub fn binary_bytes(
    tool: &str,
    args: &Value,
    ctx: &mut ToolContext<'_>,
    approved: bool,
) -> BinaryRead {
    let Some(raw) = str_arg(args, "path") else {
        return BinaryRead::Outcome(ToolOutcome::err(format!("{tool} requires a path.")));
    };
    let (decision, resolved) = match check(raw, FsAccess::Read, ctx) {
        Ok(v) => v,
        Err(e) => return BinaryRead::Outcome(e),
    };
    match decision {
        Decision::Deny(d) => return BinaryRead::Outcome(ToolOutcome::err(denial_text(d))),
        // Protected files hold secrets, not programs; their values are
        // tracked only when shown through `read`.
        Decision::NeedsApproval(Approval::RevealSecret(_)) => {
            return BinaryRead::Outcome(ToolOutcome::err(format!(
                "{raw} is a protected file; it can't be analyzed as a binary. Use read if you need it."
            )));
        }
        Decision::NeedsApproval(a) if !approved => {
            let target = display(&resolved, ctx.workspaces);
            let pending = PendingOperation {
                tool: tool.into(),
                args: args.clone(),
            };
            let title = if tool == "read" {
                "Read file"
            } else {
                "Analyze file"
            };
            return BinaryRead::Outcome(approval(a, title, target, None, pending));
        }
        _ => {}
    }
    let bytes = match &resolved {
        Resolved::Inside(ws, p) => ctx.workspaces.read(*ws, p, MAX_BINARY_BYTES),
        Resolved::Root(_) => Err(format!("{raw} is a directory; use ls to list it.")),
        Resolved::Outside(p) => read_outside_max(p, MAX_BINARY_BYTES),
    };
    match bytes {
        Ok(b) => BinaryRead::Bytes(b),
        Err(e) => BinaryRead::Outcome(ToolOutcome::err(e)),
    }
}

fn read_outside_max(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > max {
        return Err(format!(
            "{} is too large to analyze ({})",
            path.display(),
            format_size(meta.len() as usize)
        ));
    }
    std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

/// Pi's offset/limit handling and continuation notices for `read`.
fn select_lines(text: &str, path: &str, offset: Option<f64>, limit: Option<f64>) -> ToolOutcome {
    let all: Vec<&str> = {
        let mut v: Vec<&str> = text.split('\n').collect();
        if text.ends_with('\n') {
            v.pop();
        }
        v
    };
    let total = all.len();
    let start = offset.map_or(0, |o| (o as i64 - 1).max(0) as usize);
    if start >= total && !(total == 0 && start == 0) {
        return ToolOutcome::err(format!(
            "Offset {} is beyond end of file ({total} lines total)",
            offset.map_or(0, |o| o as i64)
        ));
    }
    let limit = limit.map(|l| l.max(0.0) as usize);
    let end = limit.map_or(total, |l| (start + l).min(total));
    let selected = all[start..end].join("\n");
    let t = truncate_head(&selected, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let first = start + 1;
    if t.first_line_exceeds_limit {
        let size = all.get(start).map_or(0, |l| l.len());
        return ToolOutcome::ok(format!(
            "[Line {first} is {}, exceeds {} limit. Use bash: sed -n '{first}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(size),
            format_size(DEFAULT_MAX_BYTES)
        ));
    }
    if t.truncated() {
        let last = first + t.output_lines - 1;
        let next = last + 1;
        let note = match t.truncated_by {
            Some(TruncatedBy::Bytes) => format!(
                "[Showing lines {first}-{last} of {total} ({} limit). Use offset={next} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            ),
            _ => {
                format!("[Showing lines {first}-{last} of {total}. Use offset={next} to continue.]")
            }
        };
        return ToolOutcome::ok(format!("{}\n\n{note}", t.content));
    }
    if limit.is_some() && end < total {
        return ToolOutcome::ok(format!(
            "{}\n\n[{} more lines in file. Use offset={} to continue.]",
            t.content,
            total - end,
            end + 1
        ));
    }
    ToolOutcome::ok(t.content)
}

fn write(args: &Value, ctx: &mut ToolContext<'_>, approved: bool) -> ToolOutcome {
    let (Some(raw), Some(content)) = (str_arg(args, "path"), str_arg(args, "content")) else {
        return ToolOutcome::err("write requires a path and content.");
    };
    mutate(raw, args, ctx, approved, "write", "Write file", |_old| {
        Ok(content.to_string())
    })
}

fn edit(args: &Value, ctx: &mut ToolContext<'_>, approved: bool) -> ToolOutcome {
    let Some(raw) = str_arg(args, "path") else {
        return ToolOutcome::err("edit requires a path.");
    };
    let edits: Vec<Edit> = match args.get("edits").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => {
            let mut out = Vec::new();
            for item in list {
                match (str_arg(item, "oldText"), str_arg(item, "newText")) {
                    (Some(o), Some(n)) => out.push(Edit {
                        old_text: o.into(),
                        new_text: n.into(),
                    }),
                    _ => return ToolOutcome::err("Each edit needs oldText and newText strings."),
                }
            }
            out
        }
        _ => {
            return ToolOutcome::err(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            );
        }
    };
    let count = edits.len();
    let outcome = mutate(raw, args, ctx, approved, "edit", "Edit file", |old| {
        let old = old.ok_or_else(|| format!("Could not edit file: {raw}. File not found."))?;
        apply_edits(old, &edits, raw)
    });
    match outcome {
        ToolOutcome::Done {
            is_error: false, ..
        } => ToolOutcome::ok(format!("Successfully replaced {count} block(s) in {raw}.")),
        other => other,
    }
}

/// Shared write/edit path: compute the new text, ask if needed (with a
/// diff), checkpoint, then write.
fn mutate(
    raw: &str,
    args: &Value,
    ctx: &mut ToolContext<'_>,
    approved: bool,
    tool: &str,
    operation: &str,
    compute: impl FnOnce(Option<&str>) -> Result<String, String>,
) -> ToolOutcome {
    let (decision, resolved) = match check(raw, FsAccess::Write, ctx) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Decision::Deny(d) = decision {
        return ToolOutcome::err(denial_text(d));
    }
    let old = match &resolved {
        Resolved::Inside(ws, p) => ctx.workspaces.read_optional(*ws, p, MAX_READ_BYTES),
        Resolved::Root(_) => Err(format!("{raw} is a directory.")),
        Resolved::Outside(p) if p.exists() => read_outside(p).map(Some),
        Resolved::Outside(_) => Ok(None),
    };
    let old = match old {
        Ok(o) => o,
        Err(e) => return ToolOutcome::err(e),
    };
    let old_text = match old.map(|b| read_text(b, raw)).transpose() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let new_text = match compute(old_text.as_deref()) {
        Ok(t) => t,
        Err(e) => return ToolOutcome::err(e),
    };
    if let Decision::NeedsApproval(a) = decision
        && !approved
    {
        let diff = gardenwall_fs::diff::diff_file(
            raw,
            old_text.as_deref().map(str::as_bytes),
            Some(new_text.as_bytes()),
        )
        .unified;
        let pending = PendingOperation {
            tool: tool.into(),
            args: args.clone(),
        };
        return approval(
            a,
            operation,
            display(&resolved, ctx.workspaces),
            diff,
            pending,
        );
    }
    let result = match &resolved {
        Resolved::Inside(ws, p) => ctx
            .workspaces
            .write_checkpointed(*ws, p, new_text.as_bytes()),
        Resolved::Outside(p) => write_outside(p, new_text.as_bytes()),
        Resolved::Root(_) => Err(format!("{raw} is a directory.")),
    };
    match result {
        Ok(()) => ToolOutcome::ok(format!(
            "Successfully wrote {} bytes to {raw}",
            new_text.len()
        )),
        Err(e) => ToolOutcome::err(e),
    }
}

fn write_outside(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

fn limit_arg(args: &Value, default: usize) -> usize {
    num_arg(args, "limit").map_or(default, |l| (l.floor() as i64).max(1) as usize)
}

fn finish_with_notices(lines: Vec<String>, mut notices: Vec<String>) -> String {
    let body = lines.join("\n");
    let t = truncate_head(&body, usize::MAX, DEFAULT_MAX_BYTES);
    if t.truncated() {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if notices.is_empty() {
        t.content
    } else {
        format!("{}\n\n[{}]", t.content, notices.join(". "))
    }
}

fn ls(args: &Value, ctx: &mut ToolContext<'_>, approved: bool) -> ToolOutcome {
    let raw = str_arg(args, "path").unwrap_or(".");
    let (decision, resolved) = match check(raw, FsAccess::List, ctx) {
        Ok(v) => v,
        Err(e) => return e,
    };
    match decision {
        Decision::Deny(d) => return ToolOutcome::err(denial_text(d)),
        Decision::NeedsApproval(a) if !approved => {
            let pending = PendingOperation {
                tool: "ls".into(),
                args: args.clone(),
            };
            return approval(
                a,
                "List folder",
                display(&resolved, ctx.workspaces),
                None,
                pending,
            );
        }
        _ => {}
    }
    let limit = limit_arg(args, LS_LIMIT);
    let entries: Vec<(String, bool)> = match &resolved {
        Resolved::Inside(ws, p) => match ctx.workspaces.list(*ws, Some(p)) {
            Ok(e) => e
                .into_iter()
                .filter(|(name, _)| {
                    RelPath::parse(&format!("{p}/{name}"))
                        .is_ok_and(|child| visible_in_listing(*ws, &child, ctx.policy))
                })
                .collect(),
            Err(e) => return ToolOutcome::err(e),
        },
        Resolved::Root(ws) => match ctx.workspaces.list(*ws, None) {
            Ok(e) => e
                .into_iter()
                .filter(|(name, _)| {
                    RelPath::parse(name)
                        .is_ok_and(|child| visible_in_listing(*ws, &child, ctx.policy))
                })
                .collect(),
            Err(e) => return ToolOutcome::err(e),
        },
        Resolved::Outside(p) => match std::fs::read_dir(p) {
            Ok(rd) => {
                let mut v: Vec<(String, bool)> = rd
                    .flatten()
                    .filter_map(|e| {
                        Some((
                            e.file_name().into_string().ok()?,
                            e.file_type().ok()?.is_dir(),
                        ))
                    })
                    .collect();
                v.sort();
                v
            }
            Err(e) => return ToolOutcome::err(format!("Cannot read directory: {e}")),
        },
    };
    if entries.is_empty() {
        return ToolOutcome::ok("(empty directory)");
    }
    let mut notices = Vec::new();
    if entries.len() > limit {
        notices.push(format!(
            "{limit} entries limit reached. Use limit={} for more",
            limit * 2
        ));
    }
    let lines = entries
        .into_iter()
        .take(limit)
        .map(|(n, dir)| if dir { format!("{n}/") } else { n })
        .collect();
    ToolOutcome::ok(finish_with_notices(lines, notices))
}

/// Resolve a search root, which must be inside a granted folder.
fn search_root(
    raw: &str,
    ctx: &ToolContext<'_>,
) -> Result<(WorkspaceId, Option<RelPath>), ToolOutcome> {
    match ctx.workspaces.resolve(raw).map_err(ToolOutcome::err)? {
        Resolved::Root(ws) => Ok((ws, None)),
        Resolved::Inside(ws, p) => Ok((ws, Some(p))),
        Resolved::Outside(_) => Err(ToolOutcome::err(
            "Searching outside the granted folders is not supported. Ask the user to grant that folder.",
        )),
    }
}

fn find(args: &Value, ctx: &mut ToolContext<'_>) -> ToolOutcome {
    let Some(pattern) = str_arg(args, "pattern") else {
        return ToolOutcome::err("find requires a pattern.");
    };
    let (ws, base) = match search_root(str_arg(args, "path").unwrap_or("."), ctx) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let matcher = match globset::GlobBuilder::new(pattern)
        .literal_separator(pattern.contains('/'))
        .build()
    {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolOutcome::err(format!("Invalid glob pattern: {e}")),
    };
    let limit = limit_arg(args, FIND_LIMIT);
    let files = ctx.workspaces.walk_files(ws, base.as_ref());
    let mut found = Vec::new();
    let mut hit_limit = false;
    for rel in files {
        if !visible_in_listing(ws, &rel, ctx.policy) {
            continue;
        }
        let shown = relative_to(&rel, base.as_ref());
        let name_only = !pattern.contains('/');
        let candidate = if name_only {
            rel.file_name().to_string()
        } else {
            shown.clone()
        };
        if matcher.is_match(&candidate) {
            if found.len() == limit {
                hit_limit = true;
                break;
            }
            found.push(shown);
        }
    }
    if found.is_empty() {
        return ToolOutcome::ok("No files found matching pattern");
    }
    let mut notices = Vec::new();
    if hit_limit {
        notices.push(format!(
            "{limit} results limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
    }
    ToolOutcome::ok(finish_with_notices(found, notices))
}

fn relative_to(rel: &RelPath, base: Option<&RelPath>) -> String {
    match base {
        Some(b) => rel.components()[b.components().len()..].join("/"),
        None => rel.as_display(),
    }
}

fn grep(args: &Value, ctx: &mut ToolContext<'_>) -> ToolOutcome {
    let Some(pattern) = str_arg(args, "pattern") else {
        return ToolOutcome::err("grep requires a pattern.");
    };
    let (ws, base) = match search_root(str_arg(args, "path").unwrap_or("."), ctx) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let source = if bool_arg(args, "literal") {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let re = match regex::RegexBuilder::new(&source)
        .case_insensitive(bool_arg(args, "ignoreCase"))
        .size_limit(1 << 22)
        .build()
    {
        Ok(r) => r,
        Err(e) => return ToolOutcome::err(format!("Invalid regex: {e}")),
    };
    let glob = match str_arg(args, "glob") {
        Some(g) => match globset::GlobBuilder::new(g)
            .literal_separator(g.contains('/'))
            .build()
        {
            Ok(m) => Some((m.compile_matcher(), g.contains('/'))),
            Err(e) => return ToolOutcome::err(format!("Invalid glob pattern: {e}")),
        },
        None => None,
    };
    let context = num_arg(args, "context").map_or(0, |c| c.clamp(0.0, 20.0) as usize);
    let limit = limit_arg(args, GREP_LIMIT);
    // A single file as the search root.
    let files: Vec<RelPath> = match &base {
        Some(p) if ctx.workspaces.is_file(ws, p) => vec![p.clone()],
        _ => ctx.workspaces.walk_files(ws, base.as_ref()),
    };
    let mut out = Vec::new();
    let mut matches = 0usize;
    let mut hit_limit = false;
    let mut long_lines = false;
    'files: for rel in files {
        if !visible_in_listing(ws, &rel, ctx.policy) {
            continue;
        }
        let shown = relative_to(&rel, base.as_ref());
        if let Some((m, with_dirs)) = &glob {
            let candidate = if *with_dirs {
                shown.clone()
            } else {
                rel.file_name().to_string()
            };
            if !m.is_match(&candidate) {
                continue;
            }
        }
        let shown = if shown.is_empty() {
            rel.as_display()
        } else {
            shown
        };
        let Ok(bytes) = ctx.workspaces.read(ws, &rel, 8 << 20) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            if matches == limit {
                hit_limit = true;
                break 'files;
            }
            matches += 1;
            let from = i.saturating_sub(context);
            let to = (i + context + 1).min(lines.len());
            for (j, l) in lines.iter().enumerate().take(to).skip(from) {
                let (t, cut) = truncate_line(l, GREP_MAX_LINE_LENGTH);
                long_lines |= cut;
                let sep = if j == i { ':' } else { '-' };
                out.push(format!("{shown}{sep}{}{sep} {t}", j + 1));
            }
        }
    }
    if matches == 0 {
        return ToolOutcome::ok("No matches found");
    }
    let mut notices = Vec::new();
    if hit_limit {
        notices.push(format!(
            "{limit} matches limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
    }
    if long_lines {
        notices.push(format!(
            "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars"
        ));
    }
    ToolOutcome::ok(finish_with_notices(out, notices))
}

/// Absolute path for display, without touching the filesystem.
pub fn join_display(root: &Path, rel: Option<&RelPath>) -> PathBuf {
    match rel {
        Some(r) => r
            .components()
            .iter()
            .fold(root.to_path_buf(), |p, c| p.join(c)),
        None => root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests;
