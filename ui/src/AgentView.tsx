// Agent mode: folders chosen in the trusted core's folder picker, a
// transcript of the agent's replies and tool activity, and controls to stop,
// undo the last turn and end the session. Approval requests appear in the
// trusted approval card docked above the composer (ADR-016 §4), outside the
// transcript, never in it. Session details (changes, context, tasks, skills,
// folders) are in the right-hand panel.
//
// Sessions are saved (encrypted) as they happen. A saved session can be
// read, continued (folders are chosen again in the picker) or deleted.
import { usePoll } from "./usePoll";
import { InstallCardNotice, RestartOffer } from "./ToolchainCards";
import { RememberForm, SelectionRemember } from "./Remember";
import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  AgentUiEvent,
  CollectionSummary,
  AgentUsage,
  AgentView as AgentViewState,
  api,
  type AppView,
  draftText,
  errorText,
  type Memory,
  LoadedView,
  GpuWarning,
  TranscriptEntry,
  TranscriptSummary,
  TranscriptView,
  ForkResult,
  SessionCard,
  Project,
  Thinking,
  Todo,
  type HandoffStarted,
  type HandoffSuggestion,
  type WebMode,
} from "./api";
import { HandoffCard, HandoffDialog } from "./HandoffDialog";
import { QuickSwitch } from "./QuickSwitch";
import { AgentDashboard } from "./AgentDashboard";
import { ProjectGitignore, ProjectSecrets } from "./ProjectSecrets";
import { ProjectSetup } from "./ProjectSetup";
import { AskCard } from "./AskCard";
import { RunRequestCard } from "./RunRequestCard";
import { ResearchCard } from "./ResearchCard";
import { useDraft } from "./useDraft";
import { type Insert, useInsert } from "./useInsert";
import { SessionChanges } from "./SessionChanges";
import { MessageTime } from "./MessageTime";
import { useAutoGrow } from "./useAutoGrow";
import { NARROW_WINDOW, useMedia } from "./useMedia";
import { useStickyScroll } from "./useStickyScroll";
import { Markdown } from "./Markdown";
import {
  type AgentCommand,
  allCommands,
  APPROVE_PLAN_PROMPT,
  CONTINUE_PROMPT,
  expandTemplate,
  INIT_PROMPT,
  parseCommand,
  REVIEW_PROMPT,
  skillRequest,
  suggestions,
} from "./agentCommands";
import { SlowdownBanner } from "./SlowdownBanner";
import { AttachmentStrip, useAttachments } from "./useAttachments";
import { UserText } from "./ChatView";
import { ModeBadge, ModeChoice, VisitedSites } from "./WebResearch";
import { OpenedRepositories } from "./ModelPicker";
import { AtSign, BookOpen, Check, ChevronRight, Download, Eye, FileDown, Lightbulb, Loader2, MoreHorizontal, PanelRightClose, PanelRightOpen, Paperclip, Pencil, Play, Plus, SquareSlash, Trash2, Undo2, X } from "lucide-react";
import { DockedApproval } from "./ApprovalDialog";
import { Growth } from "./art/Growth";
import { Sprig } from "./art/Sprig";
import { Button, CopyButton, EmptyState, Fold, IconButton, Inspector, Menu, NoticeFold, Pill, RenameField, SegmentedBar, Toggle, useContextMenu, useCornerFloor } from "./ui";
import "./agent.css";
import { shownPath, shownPaths } from "./paths";
import { ReadAloudButton } from "./ReadAloud";
import { MicButton } from "./MicButton";
import { readIfAuto } from "./speech";
import { NewProjectDialog } from "./NewProjectDialog";

type Entry =
  | { kind: "user"; text: string; images?: string[]; files?: string[]; at?: number }
  | { kind: "shell"; command: string; output: string | null; failed: boolean; shared: boolean }
  | { kind: "assistant"; text: string; reasoning: string; at?: number }
  | ToolEntry
  | { kind: "summary"; tokensBefore: number; text: string }
  | { kind: "note"; text: string }
  | { kind: "memories" };

/** A tool call. `calls` are the calls a `run_js` program made (code mode);
 * they are shown while the session is open and aren't saved with it. */
type ToolEntry = {
  kind: "tool";
  id: string;
  name: string;
  args: string;
  output: string | null;
  isError: boolean;
  calls?: ToolEntry[];
};

/** The agent's memory suggestions, to accept or reject right here. */
function MemorySuggestions() {
  const [pending, setPending] = useState<Memory[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api
      .listMemories()
      .then((v) => setPending(v.memories.filter((m) => m.pending)))
      .catch((e) => setError(errorText(e)));
  }, []);
  const decide = async (m: Memory, accept: boolean) => {
    try {
      const v = accept ? await api.updateMemory(m.id, m.text, true, true) : await api.deleteMemory(m.id);
      setPending(v.memories.filter((x) => x.pending));
    } catch (e) {
      setError(errorText(e));
    }
  };
  if (pending === null) return null;
  return (
    <div className="tool-card memory-card">
      <div className="small">
        <strong>The agent suggested {pending.length === 1 ? "a memory" : "memories"}</strong>
        <span className="muted"> · used only if you accept · also in the Memory page</span>
      </div>
      {error && <div className="small error-text">{error}</div>}
      {pending.length === 0 && <div className="small muted">All reviewed.</div>}
      <ul className="memories">
        {pending.map((m) => (
          <li key={m.id} className="memory pending">
            <div className="memory-text">{m.text}</div>
            <div className="memory-actions small">
              <span className="muted">{m.scope.kind === "project" ? `Project: ${shownPath(m.scope.folder)}` : "Personal"}</span>
              <button className="primary small" onClick={() => void decide(m, true)}>
                Accept
              </button>
              <button className="small danger" onClick={() => void decide(m, false)}>
                Reject
              </button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** A saved request as the user typed it: fixed command prompts show as
 * their command, and the planning-mode note added by the core is hidden. */
/** Folder names for a sentence ("project", or "a and b"). */
function folderNames(folders: string[]): string {
  const names = folders.map((f) => f.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? f);
  if (names.length === 0) return "the folder";
  return names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0]!;
}

function shownRequest(text: string): string {
  // Output of the user's `!` commands sent with the request.
  if (text.startsWith("<user-shell>\n")) {
    const end = text.indexOf("\n</user-shell>\n\n");
    if (end >= 0) text = text.slice(end + "\n</user-shell>\n\n".length);
  }
  if (text === INIT_PROMPT) return "/init";
  if (text === REVIEW_PROMPT) return "/review";
  if (text === APPROVE_PLAN_PROMPT) return "Plan approved. Go ahead.";
  const note = text.indexOf("\n\n[Planning mode is on.");
  return note >= 0 ? text.slice(0, note) : text;
}

/** Rebuild the view of a saved transcript. Saved results don't record
 * whether a tool failed, so they show as finished. */
function entriesFrom(saved: TranscriptEntry[]): Entry[] {
  const out: Entry[] = [];
  for (const e of saved) {
    if (e.kind === "compaction") {
      out.push({ kind: "summary", tokensBefore: e.tokensBefore, text: e.summary });
      continue;
    }
    const m = e.message;
    // When it was saved, for the time on hover (release plan feature 5).
    const at = e.createdAt ? e.createdAt * 1000 : undefined;
    if (m.role === "user") out.push({ kind: "user", text: shownRequest(m.content), images: m.images, at });
    else if (m.role === "assistant") {
      if (m.content || m.reasoning_content)
        out.push({ kind: "assistant", text: m.content, reasoning: m.reasoning_content ?? "", at });
      for (const c of m.tool_calls ?? [])
        out.push({ kind: "tool", id: c.id, name: c.function.name, args: c.function.arguments, output: null, isError: false });
    } else if (m.role === "tool") {
      const card = out.find((x) => x.kind === "tool" && x.id === m.tool_call_id && x.output === null);
      if (card && card.kind === "tool") card.output = m.content;
    }
  }
  for (const x of out) if (x.kind === "tool" && x.output === null) x.output = "(no result was saved)";
  return out;
}

function summarizeArgs(name: string, raw: string): string {
  try {
    const a = JSON.parse(raw) as Record<string, unknown>;
    const pick = name === "bash" || name === "powershell" || name === "cmd" ? a.command : (a.path ?? a.pattern);
    return typeof pick === "string" ? pick : "";
  } catch {
    return "";
  }
}

/** One string field of a call's arguments (the program, the question). */
function argText(raw: string, field: string): string {
  try {
    const value = (JSON.parse(raw) as Record<string, unknown>)[field];
    return typeof value === "string" ? value : raw;
  } catch {
    return raw;
  }
}

function ToolState({ state }: { state: "running" | "failed" | "done" }) {
  if (state === "running") return <Loader2 className="gw-tool-icon running" aria-label="Running" size={14} />;
  if (state === "failed") return <X className="gw-tool-icon failed" aria-label="Failed" size={14} />;
  return <Check className="gw-tool-icon done" aria-label="Done" size={14} />;
}

/** One tool call as a single line; click to see its arguments and result. */
function ToolCard({ entry }: { entry: ToolEntry }) {
  const summary = summarizeArgs(entry.name, entry.args);
  const state = entry.output === null ? "running" : entry.isError ? "failed" : "done";
  if (entry.name === "run_js" || entry.name === "research") {
    const calls = entry.calls ?? [];
    const research = entry.name === "research";
    return (
      <details className={`gw-tool ${state}`}>
        <summary>
          <ToolState state={state} />
          <span className="gw-tool-name">{research ? "Research" : "Program"}</span>
          <span className="gw-tool-arg">
            {calls.length} {research ? "web" : "tool"} {calls.length === 1 ? "request" : "requests"}
          </span>
          <ChevronRight className="gw-tool-chevron" aria-hidden="true" size={14} />
        </summary>
        <div className="gw-tool-body">
          <div className="small muted">
            {research
              ? "The question the agent asked (a research assistant with no access to the project answered it)"
              : "The program the model wrote (it runs in its own sandbox and can only ask for tool calls)"}
          </div>
          <pre className="tool-pre">{argText(entry.args, research ? "question" : "code")}</pre>
          {calls.length > 0 && <div className="small muted">{research ? "What the research assistant did" : "Tool calls it made"}</div>}
          <div className="tool-calls">
            {calls.map((c) => (
              <ToolCard key={c.id} entry={c} />
            ))}
          </div>
          {entry.output !== null && (
            <>
              <div className="small muted">What the model got back (treated as untrusted)</div>
              <pre className="tool-pre">{entry.output}</pre>
            </>
          )}
        </div>
      </details>
    );
  }
  return (
    <details className={`gw-tool ${state}`}>
      <summary>
        <ToolState state={state} />
        <span className="gw-tool-name">{entry.name}</span>
        <span className="gw-tool-arg mono">{summary}</span>
        <ChevronRight className="gw-tool-chevron" aria-hidden="true" size={14} />
      </summary>
      <div className="gw-tool-body">
        <div className="small muted">Arguments</div>
        <pre className="tool-pre">{entry.args}</pre>
        {entry.output !== null && (
          <>
            <div className="small muted">Result (from the tool, treated as untrusted)</div>
            <pre className="tool-pre">{entry.output}</pre>
          </>
        )}
      </div>
    </details>
  );
}

/** Three or more tool calls in a row fold into one line ("Ran 6 tools"). */
const GROUP_FROM = 3;

function ToolGroup({ tools }: { tools: ToolEntry[] }) {
  const running = tools.some((t) => t.output === null);
  const failed = tools.filter((t) => t.isError).length;
  const names = [...new Set(tools.map((t) => (t.name === "run_js" ? "program" : t.name)))];
  return (
    <details className="gw-tool gw-tool-group" open={running}>
      <summary>
        <ToolState state={running ? "running" : failed ? "failed" : "done"} />
        <span className="gw-tool-name">
          {running ? "Running" : "Ran"} {tools.length} tools
        </span>
        <span className="gw-tool-arg">
          {names.slice(0, 4).join(", ")}
          {names.length > 4 ? "…" : ""}
          {failed > 0 && ` · ${failed} failed`}
        </span>
        <ChevronRight className="gw-tool-chevron" aria-hidden="true" size={14} />
      </summary>
      <div className="gw-tool-group-body">
        {tools.map((t) => (
          <ToolCard key={t.id} entry={t} />
        ))}
      </div>
    </details>
  );
}

function Entries({ entries }: { entries: Entry[] }) {
  let lastReply = -1;
  entries.forEach((e, i) => {
    if (e.kind === "assistant" && e.text) lastReply = i;
  });
  const out: React.ReactNode[] = [];
  for (let i = 0; i < entries.length; i++) {
    const e = entries[i]!;
    if (e.kind === "tool") {
      let j = i;
      while (j < entries.length && entries[j]!.kind === "tool") j++;
      const run = entries.slice(i, j) as ToolEntry[];
      if (run.length >= GROUP_FROM) out.push(<ToolGroup key={`g${i}`} tools={run} />);
      else run.forEach((t, k) => out.push(<ToolCard key={i + k} entry={t} />));
      i = j - 1;
      continue;
    }
    switch (e.kind) {
      case "shell":
        out.push(
          <details key={i} className={`gw-tool ${e.output === null ? "running" : e.failed ? "failed" : "done"}`} open={e.output === null}>
            <summary>
              <ToolState state={e.output === null ? "running" : e.failed ? "failed" : "done"} />
              <span className="gw-tool-arg mono">
                {e.shared ? "!" : "!!"} {e.command}
              </span>
              <span className="small muted">{e.shared ? "output goes with your next message" : "not shown to the agent"}</span>
              <ChevronRight className="gw-tool-chevron" aria-hidden="true" size={14} />
            </summary>
            {e.output !== null && <pre className="tool-output">{e.output}</pre>}
          </details>,
        );
        break;
      case "user":
        out.push(
          <div key={i} className="message user">
            {e.images && e.images.length > 0 && (
              <div className="attachments">
                {e.images.map((src, j) => (
                  <img key={j} className="thumb" alt="Attached image" src={src} />
                ))}
              </div>
            )}
            {e.files && e.files.length > 0 && (
              <div className="attachments">
                {e.files.map((name, j) => (
                  <span key={j} className="attachment file-chip">
                    <span className="file-chip-name">{name}</span>
                  </span>
                ))}
              </div>
            )}
            <UserText content={e.text} />
            {e.at && (
              <div className="user-foot gw-msg-foot">
                <MessageTime at={e.at} />
              </div>
            )}
          </div>,
        );
        break;
      case "assistant":
        out.push(
          <div key={i} className={`message assistant${i === lastReply ? " latest" : ""}`}>
            {e.reasoning && (
              <details className="reasoning">
                <summary>Reasoning</summary>
                <div className="text">{e.reasoning}</div>
              </details>
            )}
            {e.text && <Markdown text={e.text} />}
            {(e.text || e.at) && (
              <div className="gw-msg-foot">
                {e.text && (
                  <div className="gw-msg-actions">
                    <CopyButton text={e.text} label="Copy the reply" />
                    <ReadAloudButton text={e.text} />
                  </div>
                )}
                <MessageTime at={e.at} />
              </div>
            )}
          </div>,
        );
        break;
      case "summary":
        out.push(
          <details key={i} className="tool-card summary-card">
            <summary className="small">
              Earlier messages were summarized to free up context
              {e.tokensBefore > 0 && ` (about ${e.tokensBefore.toLocaleString()} tokens before)`}. Show summary
            </summary>
            <div className="small muted">Written by the model; the agent continues from this summary.</div>
            <Markdown text={e.text} />
          </details>,
        );
        break;
      case "note":
        out.push(
          <div key={i} className="small muted center">
            {e.text}
          </div>,
        );
        break;
      case "memories":
        out.push(<MemorySuggestions key={i} />);
        break;
    }
  }
  return <>{out}</>;
}

const PARTS: [keyof AgentUsage["breakdown"], string, string][] = [
  ["system", "System prompt", "#2f6b4c"],
  ["tools", "Tool definitions", "#3e8c63"],
  ["summary", "Summary of earlier messages", "#6b7a72"],
  ["conversation", "Conversation", "--accent"],
  ["toolResults", "Tool results", "#8fd4ac"],
];

/** The parts of the context, estimated and scaled to the size the model reported. */
function contextParts(usage: AgentUsage) {
  const est = PARTS.reduce((n, [k]) => n + usage.breakdown[k], 0);
  const scale = est > 0 && usage.promptTokens ? usage.promptTokens / est : 1;
  return PARTS.filter(([k]) => usage.breakdown[k] > 0).map(([k, label, color]) => ({
    label,
    value: Math.round(usage.breakdown[k] * scale),
    color,
  }));
}

/**
 * The always-visible context bar: a hairline split
 * by what fills the context, with the summarise-at point marked. Hover for
 * the numbers; the Context tab has the full breakdown.
 */
function ContextBar({ usage, compactions, onOpen }: { usage: AgentUsage; compactions: number; onOpen: () => void }) {
  const window = usage.contextWindow;
  const used = usage.totalTokens;
  if (!window || used === null) return null;
  const pct = Math.min(100, (used / window) * 100);
  const level = pct >= 95 ? "danger" : pct >= 80 ? "warn" : "ok";
  const untilCompact = usage.compactAt !== null ? usage.compactAt - used : null;
  return (
    <div className="gw-context">
      <button type="button" className="gw-context-bar" onClick={onOpen} title="Show the context breakdown">
        <SegmentedBar
          label="Context"
          total={window}
          height={4}
          segments={contextParts(usage)}
          marker={usage.compactAt ? { at: Math.min(1, usage.compactAt / window), label: "Summarizes older messages here" } : undefined}
        />
        <span className={`gw-context-text ${level}`}>
          {used.toLocaleString()} / {window.toLocaleString()} ({pct.toFixed(0)}%)
          {untilCompact !== null && untilCompact <= 0 && " · will summarize before the next step"}
        </span>
        {usage.generationPerSecond != null && <span className="gw-context-speed">{usage.generationPerSecond.toFixed(0)} tok/s</span>}
      </button>
      {(compactions >= 3 || (compactions > 0 && level !== "ok")) && (
        <div className="meter-note warn">
          This session has been summarized {compactions === 1 ? "once" : `${compactions} times`} and details get lost
          each time. For a new task, end this session and start a fresh one.
        </div>
      )}
    </div>
  );
}

/** The full context breakdown (Context tab of the session details). */
function ContextDetails({ usage, compactions }: { usage: AgentUsage; compactions: number }) {
  const window = usage.contextWindow;
  const used = usage.totalTokens;
  if (!window || used === null) return <p className="muted small">Shows after the first reply.</p>;
  const pct = Math.min(100, (used / window) * 100);
  const untilCompact = usage.compactAt !== null ? usage.compactAt - used : null;
  const parts = contextParts(usage);
  return (
    <div className="gw-inspector-list">
      <SegmentedBar
        label="Context"
        total={window}
        height={8}
        legend
        segments={parts}
        marker={usage.compactAt ? { at: Math.min(1, usage.compactAt / window), label: "Summarizes here" } : undefined}
      />
      <p className="small">
        {used.toLocaleString()} of {window.toLocaleString()} tokens ({pct.toFixed(1)}%)
        {untilCompact !== null &&
          (untilCompact > 0
            ? `. Summarizes older messages in ${untilCompact.toLocaleString()} tokens.`
            : ". Will summarize older messages before the next step.")}
      </p>
      {usage.generationPerSecond != null && (
        <p className="small muted">
          {usage.generationPerSecond.toFixed(0)} tok/s
          {usage.ttftMs != null && ` · first token ${usage.ttftMs} ms`}
          {draftText(usage.draftTokens, usage.draftAccepted) && ` · ${draftText(usage.draftTokens, usage.draftAccepted)}`}
        </p>
      )}
      <p className="small muted">
        Parts are estimated from text length and scaled to the size the model reported.
        {compactions > 0 && ` Summarized ${compactions} time${compactions === 1 ? "" : "s"} this session.`}
      </p>
    </div>
  );
}


function SavedSessions({
  items,
  heading,
  note,
  onOpen,
  onContinue,
  onChanged,
  isOpen,
}: {
  items: TranscriptSummary[];
  heading: string;
  note?: string;
  onOpen: (id: string) => void;
  onContinue: (id: string) => void;
  /** Something was imported or deleted. */
  onChanged: () => void;
  /** Whether a saved session is open now. */
  isOpen: (id: string) => boolean;
}) {
  const [error, setError] = useState<string | null>(null);
  const [exported, setExported] = useState<string | null>(null);
  const load = onChanged;
  // Forks and clones under the session they came from (Pi's tree).
  const ids = new Set(items.map((t) => t.id));
  const children = new Map<string, TranscriptSummary[]>();
  for (const t of items) {
    if (t.parentId && ids.has(t.parentId)) children.set(t.parentId, [...(children.get(t.parentId) ?? []), t]);
  }
  const ordered: { t: TranscriptSummary; depth: number }[] = [];
  const walk = (t: TranscriptSummary, depth: number) => {
    ordered.push({ t, depth });
    for (const c of children.get(t.id) ?? []) walk(c, Math.min(depth + 1, 6));
  };
  for (const t of items) if (!t.parentId || !ids.has(t.parentId)) walk(t, 0);
  const exportAs = async (id: string, format: "html" | "md" | "jsonl") => {
    try {
      const path = await api.exportTranscript(id, format);
      if (path) setExported(`Saved to ${path}. The exported file isn't encrypted.`);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const remove = async (t: TranscriptSummary) => {
    if (!window.confirm(`Delete "${t.title}"? This cannot be undone.`)) return;
    try {
      await api.agentDeleteTranscript(t.id);
      load();
    } catch (e) {
      setError(errorText(e));
    }
  };
  const menuFor = (t: TranscriptSummary) => [
    { label: "Read", icon: BookOpen, onSelect: () => onOpen(t.id) },
    { label: isOpen(t.id) ? "Go to session" : "Continue", icon: Play, onSelect: () => onContinue(t.id) },
    "separator" as const,
    { label: "Export", hint: "web page", icon: Download, title: "Save as a web page you can open in a browser", onSelect: () => void exportAs(t.id, "html") },
    { label: "Markdown", hint: "text", icon: FileDown, title: "Save as a Markdown text file", onSelect: () => void exportAs(t.id, "md") },
    { label: "JSONL", hint: "for Import", icon: FileDown, title: "Save as JSONL, which Import can read back", onSelect: () => void exportAs(t.id, "jsonl") },
    "separator" as const,
    { label: "Delete…", icon: Trash2, danger: true, onSelect: () => void remove(t) },
  ];
  const context = useContextMenu();
  const today = new Date().toDateString();
  const lastUsed = (secs: number) => {
    const d = new Date(secs * 1000);
    return d.toDateString() === today
      ? `Today ${d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`
      : d.toLocaleDateString();
  };
  return (
    <section className="gw-group gw-sessions">
      <div className="gw-section-head">
        <h3 className="gw-group-title">{heading}</h3>
        <Button
          size="sm"
          variant="ghost"
          title="Import a session saved with Export → JSONL"
          onClick={async () => {
            try {
              const t = await api.importTranscript();
              if (t) {
                setExported(`Imported "${t.title}".`);
                load();
              }
            } catch (e) {
              setError(errorText(e));
            }
          }}
        >
          Import…
        </Button>
      </div>
      <p className="gw-row-help gw-sessions-note">{note ?? "Saved encrypted on this computer."}</p>
      {error && <div className="banner error">{error}</div>}
      {exported && <p className="small ok-text">{exported}</p>}
      {items.length === 0 ? (
        <div className="gw-group-body gw-sessions-empty small muted">No saved sessions yet.</div>
      ) : (
        <div className="gw-group-body">
          <table className="gw-utable gw-sessions-table">
            <thead>
              <tr>
                <th scope="col">Session</th>
                <th scope="col">Last used</th>
                <th scope="col">
                  <span className="gw-sr">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {ordered.map(({ t, depth }) => (
                <tr
                  key={t.id}
                  onContextMenu={(e) => context.open(e, `Session ${t.title}`, menuFor(t))}
                >
                  <td style={depth ? { paddingLeft: `calc(var(--sp-4) + ${depth * 1.25}rem)` } : undefined}>
                    <button
                      type="button"
                      className="gw-sessions-title"
                      title={`Read "${t.title}"\nRight-click for more`}
                      onClick={() => onOpen(t.id)}
                      onKeyDown={(e) => context.open(e, `Session ${t.title}`, menuFor(t))}
                    >
                      {depth > 0 && <span className="muted">↳ </span>}
                      {t.title}
                    </button>
                    {isOpen(t.id) && (
                      <>
                        {" "}
                        <Pill tone="accent">Open</Pill>
                      </>
                    )}
                    <div className="small muted mono gw-sessions-folders">{shownPaths(t.folders, " · ")}</div>
                  </td>
                  <td className="small muted gw-nowrap">{lastUsed(t.updatedAt)}</td>
                  <td className="gw-utable-actions">
                    <Menu
                      label={`Session ${t.title}`}
                      items={menuFor(t)}
                      trigger={(p) => (
                        <button type="button" className="gw-icon-btn" aria-label="More actions" title="More actions" {...p}>
                          <MoreHorizontal aria-hidden="true" size={16} strokeWidth={1.75} />
                        </button>
                      )}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {context.node}
    </section>
  );
}

/** One open agent session. It stays mounted while another session or page
 * is shown, so its work keeps streaming. */
/** The "this session uses Windows PowerShell 5.1" note was dismissed for good. */
const PWSH_NOTE_KEY = "gw.note.pwsh51";

function AgentSessionPane({
  sid,
  initial,
  loaded,
  goToModels,
  state,
  refresh,
  active,
  onEnded,
  onReplaced,
  onHome,
  onChanged,
  onOpened,
  onAsk,
  insert,
  onLookAtRepo,
}: {
  /** Show a hub repository's files on the Models page. */
  onLookAtRepo: (repo: string) => void;
  /** Ask about selected text in a side chat. */
  onAsk: (text: string) => void;
  /** A side chat's answer to add to the message box. */
  insert: Insert | null;
  sid: string;
  initial: Opening;
  loaded: LoadedView | null;
  goToModels: () => void;
  state: AppView;
  refresh: () => Promise<void>;
  /** This pane is on screen (drops go to it). */
  active: boolean;
  /** The session was ended. */
  onEnded: () => void;
  /** `/clear` replaced this session with a fresh one. */
  onReplaced: (opening: Opening) => void;
  /** Show the session list's home (new session, saved sessions). */
  onHome: () => void;
  /** The session's name or the saved sessions changed. */
  onChanged: () => void;
  /** Another session was started from this one (a handoff): show it. */
  onOpened: (opening: Opening) => void;
}) {
  const [session, setSession] = useState<AgentViewState | null>(initial.view);
  const [entries, setEntries] = useState<Entry[]>(initial.entries);
  const [pwshNoteHidden, setPwshNoteHidden] = useState(() => {
    try {
      return localStorage.getItem(PWSH_NOTE_KEY) === "1";
    } catch {
      return false;
    }
  });
  // The latest entries, for reading a finished reply aloud.
  const entriesRef = useRef(entries);
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);
  const [input, setInput] = useState(initial.input ?? "");
  const composerRef = useRef<HTMLTextAreaElement>(null);
  // Small windows (final review item 8): the header keeps to one line with a
  // ⋯ menu below 1100 px. (The message box starts at one line at any
  // height since 2026-11-23.)
  const compactHead = useMedia(NARROW_WINDOW);
  // Corner cards and notices sit above the message box, never over it.
  const composerBox = useRef<HTMLDivElement>(null);
  useCornerFloor(composerBox);
  // One line to start; it grows as you type.
  useAutoGrow(composerRef, input, 1);
  /** Start a mention or command from the + menu: adds the character and puts the cursor after it. */
  const typeIntoComposer = (ch: string) => {
    const box = composerRef.current;
    const at = box ? box.selectionStart ?? input.length : input.length;
    const before = input.slice(0, at);
    const lead = ch === "/" || before === "" || /\s$/.test(before) ? "" : " ";
    const next = ch === "/" && input.trim() === "" ? "/" : `${before}${lead}${ch}${input.slice(at)}`;
    setInput(next);
    setSlashAt(0);
    window.requestAnimationFrame(() => {
      box?.focus();
      const pos = ch === "/" && input.trim() === "" ? 1 : before.length + lead.length + 1;
      box?.setSelectionRange(pos, pos);
      if (ch === "@") lookupPaths(next, pos);
    });
  };
  useDraft(session?.transcript ? `agent:${session.transcript}` : null, input, setInput);
  useInsert(insert, active, setInput);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Programs left running end on their own, and a toolchain added in
  // Settings offers a sandbox restart: refresh those while no turn runs (a
  // turn refreshes the session when it ends).
  usePoll(
    () => {
      if (!session) return;
      const watched = (v: AgentViewState) => JSON.stringify([v.background, v.restartOffer, v.installCard]);
      void api.agentState(session.id).then(
        (v) => setSession((prev) => (prev && watched(prev) === watched(v) ? prev : v)),
        () => {},
      );
    },
    3000,
    active && !busy && !!session && session.research === null && session.folders.length > 0,
  );
  const attach = useAttachments({ canSee: loaded?.vision === true, active, onError: setError });
  /** Messages sent while the agent works: steering (delivered after the
   * current step) and follow-ups (sent when the task is done), as in Pi. */
  const [queued, setQueued] = useState<{ text: string; followUp: boolean }[]>([]);
  const followUps = useRef<string[]>([]);
  /** Prompt history (Up/Down), this session only, kept in memory. */
  const history = useRef<string[]>([]);
  const historyAt = useRef<number | null>(null);
  const [paths, setPaths] = useState<string[]>([]);
  const [forkPoints, setForkPoints] = useState<{ seq: number; text: string }[] | null>(null);
  const pathQuery = useRef(0);
  const [usage, setUsage] = useState<AgentUsage | null>(null);
  const [compactions, setCompactions] = useState(0);
  const [slowdown, setSlowdown] = useState<GpuWarning | null>(null);
  const [todos, setTodos] = useState<Todo[]>(initial.view.todos);
  const [memory, setMemory] = useState<string | null>(null);
  /** Text the user chose to save as a memory (from a selection). */
  const [remember, setRemember] = useState<string | null>(null);
  /** A planning-mode turn finished: its plan awaits approval. */
  const [planReady, setPlanReady] = useState(false);
  /** The last turn paused at the step checkpoint (Settings → Agent). */
  const [stepPause, setStepPause] = useState(false);
  /** Why a handoff is suggested now, and what the user last dismissed. */
  const [handoffReason, setHandoffReason] = useState<HandoffSuggestion | null>(null);
  /** The open tab of the session details panel (null: closed). */
  const [inspector, setInspector] = useState<string | null>(null);
  /** The highlighted entry of the / menu. */
  const [slashAt, setSlashAt] = useState(0);
  const dismissedHandoff = useRef<{ reason: HandoffSuggestion; compactions: number } | null>(null);
  /** Summaries of this session, as the core counts them (across restarts). */
  const coreCompactions = useRef(0);
  /** The handoff review dialog is open (with an optional focus). */
  const [handoff, setHandoff] = useState<{ focus: string } | null>(null);
  /** The visited-sites list is open. */
  const [showVisits, setShowVisits] = useState(false);
  const [showRepos, setShowRepos] = useState(false);
  const loadVisits = useCallback(() => api.agentVisits(sid), [sid]);
  const scroll = useStickyScroll(entries);

  // Streamed events arrive one per token. They are queued and applied once
  // per frame, in order, so a fast model redraws the transcript about 60
  // times a second instead of once per token. When a request ends, the queue
  // is flushed before anything else is added.
  const eventQueue = useRef<AgentUiEvent[]>([]);
  const eventFrame = useRef<number | null>(null);
  const flushEvents = () => {
    if (eventFrame.current !== null) window.cancelAnimationFrame(eventFrame.current);
    eventFrame.current = null;
    const queued = eventQueue.current;
    eventQueue.current = [];
    for (const e of queued) handleNow(e);
  };
  const handle = (e: AgentUiEvent) => {
    eventQueue.current.push(e);
    if (eventFrame.current === null) eventFrame.current = window.requestAnimationFrame(flushEvents);
  };
  useEffect(() => () => {
    if (eventFrame.current !== null) window.cancelAnimationFrame(eventFrame.current);
  }, []);

  const handleNow = (e: AgentUiEvent) => {
    if (e.type === "todos") {
      setTodos(e.todos);
      return;
    }
    if (e.type === "usage") {
      if (typeof e.totalTokens === "number") setUsage(e);
      return;
    }
    if (e.type === "gpuWarning") {
      setSlowdown({ kind: e.kind, baseline: e.baseline, current: e.current, gpu: e.gpu });
      return;
    }
    if (e.type === "compacted") setCompactions((n) => n + 1);
    if (e.type === "waiting") {
      setEntries((list) => [
        ...list,
        { kind: "note", text: "Waiting for another session to finish (Settings → Agent: one session at a time)…" },
      ]);
      return;
    }
    if (e.type === "contextChanged") {
      setSession((s) => (s ? { ...s, contextFiles: e.files } : s));
      setEntries((list) => [
        ...list,
        {
          kind: "note",
          text: `Project instructions changed: now using ${e.files.length ? e.files.map((f) => f.split(/[\\/]/).pop()).join(", ") : "none"}.`,
        },
      ]);
      return;
    }
    if (e.type === "steered") {
      setQueued((q) => {
        const i = q.findIndex((x) => !x.followUp && x.text === e.text);
        return i < 0 ? q : [...q.slice(0, i), ...q.slice(i + 1)];
      });
    }
    setEntries((list) => {
      const next = [...list];
      const last = next[next.length - 1];
      switch (e.type) {
        case "content":
        case "reasoning": {
          if (last?.kind === "assistant") {
            next[next.length - 1] =
              e.type === "content"
                ? { ...last, text: last.text + e.text }
                : { ...last, reasoning: last.reasoning + e.text };
          } else {
            next.push({ kind: "assistant", text: e.type === "content" ? e.text : "", reasoning: e.type === "reasoning" ? e.text : "", at: Date.now() });
          }
          return next;
        }
        case "toolStart": {
          const card: ToolEntry = { kind: "tool", id: e.id, name: e.name, args: e.arguments, output: null, isError: false };
          // A call made by a program goes inside the program's card.
          if (e.parent && next.some((x) => x.kind === "tool" && x.id === e.parent))
            return next.map((x) => (x.kind === "tool" && x.id === e.parent ? { ...x, calls: [...(x.calls ?? []), card] } : x));
          next.push(card);
          return next;
        }
        case "toolEnd": {
          const end = (x: ToolEntry): ToolEntry =>
            x.id === e.id ? { ...x, output: e.output, isError: e.isError } : x.calls ? { ...x, calls: x.calls.map(end) } : x;
          return next.map((x) => (x.kind === "tool" ? end(x) : x));
        }
        case "approvalWaiting": {
          const text = `Waiting for your approval (${e.tool}): the request is in the card above the message box.`;
          // One line for a run of requests, not one per request.
          const last = next[next.length - 1];
          if (!(last && last.kind === "note" && last.text === text)) next.push({ kind: "note", text });
          return next;
        }
        case "compacting":
          next.push({ kind: "note", text: "The context is nearly full. Summarizing older messages…" });
          return next;
        case "compacted":
          next.push({ kind: "summary", tokensBefore: e.tokensBefore, text: e.summary });
          return next;
        case "retrying":
          next.push({
            kind: "note",
            text: `The model server didn't answer (${e.error}). Trying again in ${Math.round(e.delayMs / 1000)} s (${e.attempt} of ${e.max}).`,
          });
          return next;
        case "steered":
          next.push({ kind: "user", text: e.text, at: Date.now() });
          return next;
        case "interrupted":
          next.push({ kind: "note", text: "Interrupted by your message." });
          return next;
        case "compactionFailed":
          next.push({ kind: "note", text: `Older messages could not be summarized (${e.message}). Continuing with the full context.` });
          return next;
        case "notSaved":
          next.push({ kind: "note", text: `This session is not being saved: ${e.message}` });
          return next;
        default:
          // Unknown events are ignored rather than breaking the view.
          return list;
      }
    });
  };

  const note = (text: string) => setEntries((l) => [...l, { kind: "note", text }]);

  // A session started from a reviewed handoff sends it as its first request.
  const autoSent = useRef(false);
  useEffect(() => {
    if (initial.autoSend && !autoSent.current) {
      autoSent.current = true;
      void sendText(initial.autoSend);
    }
    // Once, when the pane opens.
  }, []);

  const commands = allCommands(session?.commands, session?.mcpPrompts, session?.skills);

  /** Run a slash command (a trusted UI action, never the model). */
  const runCommand = async (command: AgentCommand, arg: string) => {
    setError(null);
    if (command.template !== undefined) {
      // A custom command from the folder: its prompt goes as the user's request.
      await sendText(expandTemplate(command.template, arg), arg ? `${command.name} ${arg}` : command.name);
      return;
    }
    if (command.skill !== undefined) {
      // A skill by name: a fixed request on the user's behalf; loading the
      // skill and everything after it goes through the agent as usual.
      await sendText(skillRequest(command.skill, arg), arg ? `${command.name} ${arg}` : command.name);
      return;
    }
    if (command.mcpServer !== undefined) {
      // An MCP prompt is untrusted text: it goes into the message box for
      // the user to read and edit, never straight to the model.
      try {
        const text = await api.agentMcpPrompt(sid, command.name, arg);
        setInput(text);
        note(`The prompt from the MCP server "${command.mcpServer}" is in the message box. Read it, edit it if you like, then send it.`);
      } catch (e) {
        setInput(arg ? `${command.name} ${arg}` : command.name);
        setError(errorText(e));
      }
      return;
    }
    try {
      switch (command.name) {
        case "/help":
          note(
            commands.map((c) => `${c.name}${c.args ? " " + c.args : ""}: ${c.summary}`).join("\n") +
              (session?.commands.length
                ? ""
                : "\n\nAdd your own: a .gardenwall/commands/<name>.md file in a granted folder becomes /<name>.") +
              "\n\nThe Help page in the sidebar explains every feature.",
          );
          break;
        case "/plan": {
          const s = await api.agentSetPlanMode(sid, !session?.planMode);
          setSession(s);
          note(s.planMode ? "Planning mode is on: the agent can look but not change anything until you approve its plan." : "Planning mode is off.");
          break;
        }
        case "/init":
          await sendText(INIT_PROMPT, "/init");
          break;
        case "/review":
          await sendText(REVIEW_PROMPT, "/review");
          break;
        case "/compact": {
          setBusy(true);
          const done = await api.agentCompact(sid, arg || null, handle).finally(() => {
            flushEvents();
            setBusy(false);
          });
          if (!done) note("Nothing to compact yet: the conversation is still short.");
          break;
        }
        case "/context": {
          const c = await api.agentContext(sid);
          const b = c.breakdown;
          note(
            `Context: about ${c.tokens.toLocaleString()} tokens` +
              (c.contextWindow ? ` of ${c.contextWindow.toLocaleString()}` : "") +
              `\nSystem prompt ~${b.system.toLocaleString()} · tools ~${b.tools.toLocaleString()} · summary ~${b.summary.toLocaleString()} · conversation ~${b.conversation.toLocaleString()} · tool results ~${b.toolResults.toLocaleString()}` +
              (c.compacted ? "\nOlder messages have been summarized." : ""),
          );
          break;
        }
        case "/clear": {
          const s = await api.agentClear(sid);
          onReplaced({ view: s, entries: [{ kind: "note", text: "Started a fresh session with the same folders." }] });
          break;
        }
        case "/resume":
          // This session stays open in the list; saved ones are on the home panel.
          onHome();
          break;
        case "/undo":
          await undo();
          break;
        case "/memory":
          setMemory(await api.agentInstructions(sid));
          break;
        case "/remember": {
          const project = /^project:\s*/i.test(arg);
          const text = arg.replace(/^project:\s*/i, "").trim();
          if (!text) {
            note("Usage: /remember <text>, or /remember project: <text> for these folders.");
            break;
          }
          const folder = session?.folders[0];
          await api.addMemory(project && folder ? { kind: "project", folder } : { kind: "personal" }, text);
          note(
            project && folder
              ? `Saved as a memory for ${folder}. It applies from the next session on this folder (or /reload).`
              : "Saved as a personal memory. It applies from the next chat or session (or /reload here); see the Memory page.",
          );
          break;
        }
        case "/skills":
          note(
            session && session.skills.length > 0
              ? "Skills: " + session.skills.map(([n, src]) => `${n} (${src})`).join(", ")
              : "No skills. Add .gardenwall/skills/<name>/SKILL.md to a granted folder, or add personal skills in Settings.",
          );
          break;
        case "/model":
          goToModels();
          break;
        case "/thinking": {
          if (!loaded) {
            note("Load a model first.");
            break;
          }
          const model = state.models.find((m) => m.sha256 === loaded.sha256);
          const level = arg.trim().toLowerCase();
          if (!level) {
            note(`Thinking for ${loaded.displayName}: ${model?.thinking ?? "default"}. Use /thinking default, medium, low or off.`);
            break;
          }
          if (!["default", "medium", "low", "off"].includes(level)) {
            note("Use /thinking default, medium, low or off.");
            break;
          }
          await api.setModelThinking(loaded.sha256, level as Thinking);
          await refresh();
          note(`Thinking for ${loaded.displayName} is now ${level}. It applies from the next request.`);
          break;
        }
        case "/name": {
          const name = arg.trim();
          if (!name) {
            const info = await api.agentSessionInfo(sid);
            note(info.title ? `This session is named "${info.title}". Use /name <new name> to change it.` : "The session gets a name with its first request.");
            break;
          }
          setSession(await api.agentRename(sid, name));
          onChanged();
          note(`Renamed to "${name}".`);
          break;
        }
        case "/session": {
          const i = await api.agentSessionInfo(sid);
          note(
            [
              i.title ? `Session: ${i.title}` : "Session: not saved yet (it is saved with the first request)",
              i.parentTitle ? `Forked from: ${i.parentTitle}` : null,
              `Folders: ${shownPaths(i.folders, ", ")}`,
              i.createdAt ? `Started: ${new Date(i.createdAt * 1000).toLocaleString()}` : null,
              `Messages: ${i.userMessages} from you, ${i.assistantMessages} from the agent, ${i.toolResults} tool results`,
              `Summarized: ${i.compactions} time${i.compactions === 1 ? "" : "s"}`,
              `Context: ~${i.contextTokens.toLocaleString()} tokens${i.contextWindow ? ` of ${i.contextWindow.toLocaleString()}` : ""}`,
              i.id ? `ID: ${i.id}` : null,
            ]
              .filter(Boolean)
              .join("\n"),
          );
          break;
        }
        case "/fork": {
          const points = await api.agentForkPoints(sid);
          if (points.length === 0) {
            note("Nothing to fork yet: send a request first.");
            break;
          }
          setForkPoints(points);
          break;
        }
        case "/handoff": {
          if (!session?.transcript) {
            note("There's nothing to hand off yet: send a request first.");
            break;
          }
          setHandoff({ focus: arg });
          break;
        }
        case "/clone": {
          const r = await api.agentFork(sid, null);
          applyFork(r, "Now continuing in a copy of the session. The original is unchanged under Saved sessions.");
          break;
        }
        case "/copy": {
          const last = [...entries].reverse().find((x) => x.kind === "assistant" && x.text.trim());
          if (!last || last.kind !== "assistant") {
            note("There's no reply to copy yet.");
            break;
          }
          await navigator.clipboard.writeText(last.text);
          note("Copied the agent's last reply.");
          break;
        }
        case "/export": {
          if (!session?.transcript) {
            note("Nothing to export yet: the session is saved with its first request.");
            break;
          }
          const wanted = arg.trim().toLowerCase();
          const format = wanted === "jsonl" ? "jsonl" : wanted === "md" || wanted === "markdown" ? "md" : "html";
          const path = await api.exportTranscript(session.transcript, format);
          if (path) note(`Saved to ${path}. The exported file isn't encrypted.`);
          break;
        }
        case "/import": {
          const t = await api.importTranscript();
          if (t) {
            onChanged();
            note(`Imported "${t.title}". Find it under Saved sessions after ending this session.`);
          }
          break;
        }
        case "/reload": {
          const v = await api.agentReload(sid);
          setSession(v);
          note(
            `Reloaded: ${v.contextFiles.length ? v.contextFiles.map((f) => f.split(/[\\/]/).pop()).join(", ") : `no AGENTS.md or CLAUDE.md at the top of ${folderNames(v.folders)}`}, ${v.skills.length} skill(s), ${v.commands.length} command(s), and Settings → Agent.`,
          );
          break;
        }
      }
    } catch (e) {
      setError(errorText(e));
    }
  };

  const applyFork = (r: ForkResult, message: string) => {
    setSession(r.view);
    setEntries([...entriesFrom(r.transcript.entries), { kind: "note", text: message }]);
    setTodos([]);
    setUsage(null);
    onChanged();
    if (r.text) setInput(r.text);
  };

  const runShell = async (command: string, share: boolean) => {
    const id = entries.length;
    setEntries((l) => [...l, { kind: "shell", command, output: null, failed: false, shared: share }]);
    let result: { output: string; failed: boolean };
    try {
      result = await api.agentUserShell(sid, command, share);
    } catch (e) {
      result = { output: errorText(e), failed: true };
    }
    setEntries((l) =>
      l.map((x, i) =>
        i >= id && x.kind === "shell" && x.command === command && x.output === null ? { ...x, ...result } : x,
      ),
    );
  };

  const send = async (followUp = false) => {
    const text = input.trim();
    if (!text) return;
    if (busy) {
      // Enter is Send now: the reply being written stops and this goes in
      // next (a running tool finishes first). Alt+Enter queues it for after
      // the task.
      if (text.startsWith("/") || text.startsWith("!")) {
        setError("Commands can't be queued. Wait for the agent, or press Stop.");
        return;
      }
      setInput("");
      if (followUp) {
        followUps.current.push(text);
        setQueued((q) => [...q, { text, followUp: true }]);
        return;
      }
      try {
        await api.agentSteer(sid, text);
        setQueued((q) => [...q, { text, followUp: false }]);
      } catch (e) {
        setError(errorText(e));
        setInput(text);
      }
      return;
    }
    if (history.current[history.current.length - 1] !== text) history.current.push(text);
    historyAt.current = null;
    if (text.startsWith("!")) {
      const share = !text.startsWith("!!");
      const command = text.slice(share ? 1 : 2).trim();
      if (!command) return;
      setInput("");
      await runShell(command, share);
      return;
    }
    const cmd = parseCommand(text, commands);
    if (cmd) {
      setInput("");
      await runCommand(cmd.command, cmd.arg);
      return;
    }
    if (text.startsWith("/") && !text.includes(" ")) {
      setError(`Unknown command ${text}. Type /help for the list.`);
      return;
    }
    setInput("");
    const names = attach.items.filter((a) => a.kind === "file").map((a) => a.name);
    const taken = attach.take();
    await sendText(text, undefined, taken.images, taken.previews, taken.files, names);
  };

  const sendText = async (
    text: string,
    shown?: string,
    imageTokens: string[] = [],
    previews: string[] = [],
    fileTokens: string[] = [],
    fileNames: string[] = [],
  ) => {
    scroll.follow();
    setBusy(true);
    setError(null);
    setPlanReady(false);
    setStepPause(false);
    const planning = session?.planMode === true;
    let userAt = -1;
    setEntries((l) => {
      userAt = l.length;
      return [...l, { kind: "user", text: shown ?? text, images: previews, files: fileNames, at: Date.now() }];
    });
    let end = "";
    let undelivered: string[] = [];
    let withdrawn: string | null = null;
    try {
      // Something else may still be finishing in this session (a handoff
      // draft that was just closed): wait a little for it instead of failing.
      const r = await (async () => {
        for (let tries = 0; ; tries++) {
          try {
            return await api.agentSend(sid, text, handle, imageTokens, fileTokens).finally(flushEvents);
          } catch (e) {
            if (tries < 30 && /still busy|already working/i.test(errorText(e))) {
              await new Promise((done) => window.setTimeout(done, 500));
              continue;
            }
            throw e;
          }
        }
      })();
      end = r.end;
      undelivered = r.undelivered;
      withdrawn = r.withdrawn;
      if (planning && r.end === "answered") setPlanReady(true);
      if (r.end === "answered")
        window.setTimeout(() => {
          const reply = [...entriesRef.current].reverse().find((x) => x.kind === "assistant");
          if (reply && reply.kind === "assistant") readIfAuto(reply.text);
        }, 0);
      if (r.suggested > 0) setEntries((l) => [...l, { kind: "memories" }]);
      // A dismissed suggestion comes back only for a new reason or after
      // another summary.
      coreCompactions.current = r.compactions;
      const seen = dismissedHandoff.current;
      setHandoffReason(
        r.handoff && !(seen && seen.reason === r.handoff && r.compactions <= seen.compactions) ? r.handoff : null,
      );
      if (r.withdrawn !== null) {
        // Stopped before anything ran: the request was taken back out of the
        // session (the model won't see it), so it leaves the view too.
        setEntries((l) => [
          ...(userAt >= 0 ? l.slice(0, userAt) : l),
          { kind: "note", text: "Stopped before anything ran. Your request is back in the message box." },
        ]);
      } else if (r.end === "stopped") {
        setEntries((l) => [...l, { kind: "note", text: "Stopped. The work done so far stays; the agent knows you stopped it." }]);
      }
      if (r.end === "step_limit") setStepPause(true);
      if (r.end === "repeating")
        setEntries((l) => [
          ...l,
          {
            kind: "note",
            text: "The model started repeating itself, so Gardenwall stopped this step. Ask again, or set Thinking to Medium or Low for this model on the Models page.",
          },
        ]);
    } catch (e) {
      const message = errorText(e);
      if (/still busy|already working/i.test(message)) {
        // It never started: take the request back out of the view, and keep
        // the pause card (or the typed text) so nothing has to be redone.
        setEntries((l) => (userAt >= 0 ? l.slice(0, userAt) : l));
        if (text === CONTINUE_PROMPT) setStepPause(true);
        else withdrawn = text;
        setError("This session is still finishing something else (such as a handoff draft). Try again in a moment.");
      } else {
        setError(message);
      }
    } finally {
      setBusy(false);
      // The first request saves the session; later commands (/handoff,
      // /export) need its transcript.
      api
        .agentState(sid)
        .then(setSession)
        .catch(() => {});
      setQueued((q) => q.filter((x) => x.followUp));
      // Messages the agent never got go back to the composer; after Stop,
      // the follow-ups too (Pi returns queued messages to the editor).
      const back = [
        ...(withdrawn !== null ? [shown ?? withdrawn] : []),
        ...(end === "answered" ? undelivered : [...undelivered, ...followUps.current.splice(0)]),
      ];
      if (end !== "answered") setQueued([]);
      if (back.length) setInput((prev) => [...back, prev].filter(Boolean).join("\n"));
    }
    const next = end === "answered" ? followUps.current.shift() : undefined;
    if (next !== undefined) {
      setQueued((q) => {
        const i = q.findIndex((x) => x.followUp && x.text === next);
        return i < 0 ? q : [...q.slice(0, i), ...q.slice(i + 1)];
      });
      await sendText(next);
    }
  };

  /** Complete the `@path` before the caret from the granted folders. */
  const lookupPaths = (value: string, caret: number) => {
    const m = /(^|\s)@([^\s@]*)$/.exec(value.slice(0, caret));
    const ticket = ++pathQuery.current;
    if (!m || busy) {
      setPaths([]);
      return;
    }
    const q = m[2] ?? "";
    window.setTimeout(() => {
      if (ticket !== pathQuery.current) return;
      api
        .agentCompletePath(sid, q)
        .then((p) => ticket === pathQuery.current && setPaths(p))
        .catch(() => setPaths([]));
    }, 120);
  };

  const insertPath = (path: string) => {
    const quoted = /\s/.test(path) ? `"${path}"` : path;
    setInput((v) => v.replace(/(^|\s)@([^\s@]*)$/, (_m, pre: string) => `${pre}@${quoted} `));
    setPaths([]);
  };

  const setMemories = async (on: boolean) => {
    try {
      setSession(await api.agentSetMemories(sid, on));
    } catch (err) {
      setError(errorText(err));
    }
  };
  const endSession = async () => {
    if (busy && !window.confirm("The agent is working. Stop it and end this session?")) return;
    try {
      await api.agentEnd(sid);
    } catch (e) {
      setError(errorText(e));
      return;
    }
    onEnded();
  };
  const undo = async () => {
    try {
      const files = await api.agentUndo(sid);
      setEntries((l) => [
        ...l,
        { kind: "note", text: files.length ? `Undid the last turn: ${files.join(", ")}` : "Nothing to undo." },
      ]);
    } catch (e) {
      setError(errorText(e));
    }
  };

  if (!session) return <div className="page muted">Loading…</div>;

  const slash = suggestions(input, commands);
  const slashPick = slash[Math.min(slashAt, Math.max(0, slash.length - 1))];
  const useCommand = (c: { name: string; args?: string }) => setInput(c.args ? `${c.name} ` : c.name);
  const tasksDone = todos.filter((t) => t.status === "completed").length;
  /** Finished tool calls in this session: the header's growth tree grows with them. */
  const toolCount = entries.filter((e) => e.kind === "tool" && e.output !== null).length;

  return (
    <div className="gw-session">
      <div className="chat gw-session-main">
        {/* The session's tree, in the empty space beside the conversation: a branch per finished step. */}
        <div className="gw-session-tree" aria-hidden="true">
          <Growth seed={sid} width={360} height={520} roots={1} depth={7} anchor={0.55} grown={Math.min(1, 0.12 + toolCount / 40)} />
        </div>
        <header className="gw-session-head">
          <div className="gw-session-growth" aria-hidden="true" title="Grows as the agent works">
            <Growth seed={sid} width={96} height={56} roots={1} depth={5} anchor={0.5} grown={Math.min(1, 0.2 + toolCount / 30)} />
          </div>
          <div className="gw-session-title">
            <div className="gw-session-name" title={session.title}>
              {session.title}
            </div>
            <div className="gw-session-sub">
              {session.research ? (
                <>
                  <ModeBadge
                    mode={session.research}
                    onTighten={async () => {
                      try {
                        setSession(await api.agentSetWebMode(sid, "ask"));
                        note("Every search and page asks first from now on.");
                      } catch (err) {
                        setError(errorText(err));
                      }
                    }}
                  />
                  <span>No files or commands: web search and pages only</span>
                </>
              ) : (
                <>
                  <span className="mono" title={shownPaths(session.folders, "\n")}>
                    {folderNames(session.folders)}
                  </span>
                  <span>{session.sandboxed ? `${session.shell}, sandboxed, no network` : session.commandsOff ? "Commands off (see below)" : "Commands disabled (no sandbox)"}</span>
                </>
              )}
              {session.hiddenSecrets.length > 0 && (
                <button type="button" className="gw-linkbtn" onClick={() => setInspector("session")}>
                  {session.hiddenSecrets.length === 1 ? "1 secret file hidden" : `${session.hiddenSecrets.length} secret files hidden`}
                </button>
              )}
            </div>
          </div>
          <div className="gw-session-actions">
            {todos.length > 0 && (
              <button type="button" className="gw-chip" onClick={() => setInspector("tasks")} title="The agent's task list">
                Tasks {tasksDone}/{todos.length}
              </button>
            )}
            {session.background.length > 0 && (
              <button
                type="button"
                className="gw-chip gw-chip-running"
                onClick={() => setInspector("tasks")}
                title={`Running in the background:\n${session.background.join("\n")}`}
              >
                {session.background.length} running
              </button>
            )}
            {session.web && (
              <button type="button" className="gw-chip" onClick={() => setShowVisits(true)} title="Every search and page this session used, recorded by Gardenwall">
                Visited sites{session.visits > 0 ? ` (${session.visits})` : ""}
              </button>
            )}
            {session.research && (
              <button
                type="button"
                className="gw-chip"
                onClick={() => setShowRepos(true)}
                title="Model repositories on the hub that this session really opened, with their files and fit"
              >
                Repositories opened
              </button>
            )}
            {compactHead ? (
              <Menu
                label="Session actions"
                trigger={(p) => <IconButton icon={MoreHorizontal} label="Session actions: memories, undo last turn, session details, end session" {...p} />}
                items={[
                  {
                    label: "Memories",
                    icon: Lightbulb,
                    hint: session.memories ? <Pill tone="accent">on</Pill> : "off",
                    title: "Use your saved memories in this session, and let the agent suggest new ones (you approve each)",
                    disabled: busy,
                    onSelect: () => void setMemories(!session.memories),
                  },
                  ...(session.research
                    ? []
                    : [
                        {
                          label: "Undo last turn",
                          icon: Undo2,
                          title: "Puts back the files the agent changed in its last turn, with its tools or its commands",
                          disabled: busy,
                          onSelect: () => void undo(),
                        },
                      ]),
                  {
                    label: inspector ? "Hide session details" : "Show session details",
                    icon: inspector ? PanelRightClose : PanelRightOpen,
                    title: inspector ? undefined : "Changes, context, tasks, files and skills",
                    onSelect: () => setInspector((v) => (v ? null : session.research ? "context" : "changes")),
                  },
                  "separator",
                  { label: "End session", icon: X, danger: true, onSelect: () => void endSession() },
                ]}
              />
            ) : (
              <>
              <span
                className="gw-session-option gw-session-switch"
                title="Use your saved memories in this session, and let the agent suggest new ones (you approve each)"
              >
                <span aria-hidden="true">Memories</span>
                <Toggle
                  checked={session.memories}
                  disabled={busy}
                  label="Use memories in this session"
                  onChange={(on) => void setMemories(on)}
                />
              </span>
              {!session.research && (
                <IconButton
                  icon={Undo2}
                  label="Undo last turn: puts back the files the agent changed in its last turn, with its tools or its commands"
                  onClick={() => void undo()}
                  disabled={busy}
                />
              )}
              <IconButton
                icon={inspector ? PanelRightClose : PanelRightOpen}
                label={inspector ? "Hide session details" : "Show session details: changes, context, tasks, files and skills"}
                aria-pressed={inspector !== null}
                onClick={() => setInspector((v) => (v ? null : session.research ? "context" : "changes"))}
              />
              <Button
                variant="danger"
                size="sm"
                onClick={() => void endSession()}
              >
                End session
              </Button>
              </>
            )}
          </div>
        </header>
        {usage && <ContextBar usage={usage} compactions={compactions} onOpen={() => setInspector("context")} />}
        {handoff && (
          <HandoffDialog
            source={{ kind: "agent", sid }}
            focus={handoff.focus}
            onClose={() => setHandoff(null)}
            onStarted={(started: HandoffStarted) => {
              setHandoff(null);
              setHandoffReason(null);
              note("Started a new session from the handoff. This session stays open and saved.");
              if (started.agent) onOpened({ view: started.agent, entries: [], autoSend: started.message });
            }}
          />
        )}
        {showVisits && <VisitedSites load={loadVisits} onClose={() => setShowVisits(false)} />}
        {showRepos && <OpenedRepositories sid={sid} onLook={onLookAtRepo} onClose={() => setShowRepos(false)} />}
        {/* Two or more notices fold into one line (final review item 8). */}
        <NoticeFold className="gw-session-notices">
          {state.agentSettings.shell === "powershell" && session.shell?.startsWith("Windows PowerShell") && !pwshNoteHidden && (
            <div className="banner info dismissible" role="status">
              <span>
                <strong>This session uses Windows PowerShell 5.1.</strong>{" "}
                {state.powershell7
                  ? "PowerShell 7 is installed but couldn't start in the sandbox."
                  : "PowerShell 7 isn't installed on this PC. Install it with Microsoft's MSI installer (from github.com/PowerShell/PowerShell releases) and new sessions use it."}{" "}
                To use 5.1 on purpose, choose it in Settings → Agent → Shell.
              </span>
              <button
                className="small"
                onClick={() => {
                  try {
                    localStorage.setItem(PWSH_NOTE_KEY, "1");
                  } catch {
                    // Not remembered: it shows again in the next session.
                  }
                  setPwshNoteHidden(true);
                }}
              >
                Don't show again
              </button>
            </div>
          )}
          {session.commandsOff && (
            <div className="banner warn" role="status">
              <strong>Commands are off in this session.</strong> Gardenwall couldn't set up the sandbox: {session.commandsOff}{" "}
              After fixing it, restart the sandbox. The agent can still read and edit files.
            </div>
          )}
          {session.commandsOff && (
            <RestartOffer session={session} onView={setSession} text="Fixed the problem above? Try the sandbox again." />
          )}
          {!session.commandsOff && <RestartOffer session={session} onView={setSession} />}
          <InstallCardNotice
            session={session}
            onChanged={() => void api.agentState(session.id).then(setSession, () => {})}
          />
          {session.webTainted && (
            <div className="banner warn" role="status">
              <strong>Web content was read in this session.</strong> A page could have tried to steer the agent, so every
              change and command asks you first from now on. Start a fresh session (or /clear) to work without this.
            </div>
          )}
          {session.background.length > 0 && (
            <div className="banner warn dismissible" role="status">
              <span>Running in the sandbox: {session.background.join(", ")}</span>
              <button
                className="small"
                onClick={() => {
                  void api.agentStopBackground(session.id).then(
                    (v) => setSession(v),
                    () => {},
                  );
                }}
              >
                Stop
              </button>
            </div>
          )}
          {handoffReason && !busy && !handoff && (
            <HandoffCard
              reason={handoffReason}
              onOpen={() => setHandoff({ focus: "" })}
              onDismiss={() => {
                dismissedHandoff.current = { reason: handoffReason, compactions: coreCompactions.current };
                setHandoffReason(null);
              }}
            />
          )}
          {session.planMode && (
            <div className="banner info plan-banner">
              <strong>Planning mode.</strong> The agent can look but not change anything or run commands.{" "}
              <button
                className="link"
                disabled={busy}
                onClick={async () => {
                  setSession(await api.agentSetPlanMode(sid, false));
                  note("Planning mode is off.");
                }}
              >
                Turn off
              </button>
            </div>
          )}
          {slowdown && <SlowdownBanner warning={slowdown} goToModels={goToModels} onDismiss={() => setSlowdown(null)} />}
          {!loaded && (
            <div className="banner warn">
              No model is loaded. <button className="link" onClick={goToModels}>Load a model</button> to use the agent.
            </div>
          )}
        </NoticeFold>
        {memory !== null && (
          <div className="memory-editor">
            <div className="small">
              <strong>AGENTS.md</strong> in {session.folders[0]}: instructions every agent session in this folder reads.
              Applies to sessions started from now on.
            </div>
            <textarea rows={12} value={memory} onChange={(e) => setMemory(e.target.value)} />
            <div className="row">
              <button
                className="primary"
                onClick={async () => {
                  try {
                    await api.agentSaveInstructions(sid, memory);
                    setMemory(null);
                    note("Saved AGENTS.md. Undo last turn reverts it.");
                  } catch (e) {
                    setError(errorText(e));
                  }
                }}
              >
                Save
              </button>
              <button onClick={() => setMemory(null)}>Cancel</button>
            </div>
          </div>
        )}
        <div className="messages" ref={scroll.ref} onScroll={scroll.onScroll}>
          {entries.length === 0 && (
            <EmptyState
              art={<Growth seed={sid} width={120} height={72} roots={1} depth={4} />}
              title={session.research ? "Ask a research question" : "Describe what you want done"}
              body={
                session.research
                  ? "The agent searches the web and reads pages; it has no files or commands."
                  : `The agent works in ${folderNames(session.folders)} and asks before anything risky.`
              }
            />
          )}
          <Entries entries={entries} />
          {busy && (
            <div className="small muted gw-working gw-agent-working" role="status">
              <Sprig /> <span className="gw-working-text">Working…</span>
            </div>
          )}
          {session.planMode && !busy && planReady && (
            <div className="plan-actions">
              <span className="small">Approve this plan?</span>
              <button
                className="primary"
                onClick={async () => {
                  const s = await api.agentSetPlanMode(sid, false);
                  setSession(s);
                  setPlanReady(false);
                  await sendText(APPROVE_PLAN_PROMPT, "Plan approved. Go ahead.");
                }}
              >
                Approve and start
              </button>
              <span className="small muted">or type changes to the plan below</span>
            </div>
          )}
        </div>
        <SelectionRemember container={scroll.ref} onPick={setRemember} onAsk={onAsk} />
        {remember !== null && (
          <RememberForm
            initial={remember}
            scopes={[
              { label: "Personal", scope: { kind: "personal" } },
              ...(session.folders[0]
                ? [{ label: `This project (${session.folders[0]})`, scope: { kind: "project" as const, folder: session.folders[0] } }]
                : []),
            ]}
            onDone={(saved) => {
              setRemember(null);
              if (saved) note("Saved as a memory. It applies from the next session (or /reload here).");
            }}
          />
        )}
        {scroll.away && (
          <button className="jump-latest small" onClick={scroll.follow}>
            ↓ Jump to latest
          </button>
        )}
        {error && <div className="banner error">{error}</div>}
        {slash.length > 0 && (
          <div className="gw-slash" role="listbox" aria-label="Commands and skills">
            {slash.map((c) => (
              <button
                key={c.name}
                type="button"
                role="option"
                aria-selected={c === slashPick}
                className={`gw-slash-item${c === slashPick ? " active" : ""}`}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => useCommand(c)}
              >
                <span className="gw-slash-name">
                  {c.name}
                  {c.args && <span className="muted"> {c.args}</span>}
                </span>
                <span className="gw-slash-summary">{c.summary}</span>
                {c.skill && <span className="gw-slash-kind">skill</span>}
              </button>
            ))}
            <div className="gw-slash-hint">↑↓ to choose · Tab to use · Esc to close</div>
          </div>
        )}
        {forkPoints && (
          <div className="fork-picker">
            <div className="row">
              <strong className="small">Fork from before which message?</strong>
              <button className="link small" onClick={() => setForkPoints(null)}>
                Cancel
              </button>
            </div>
            <p className="small muted">
              A new saved session continues from there, with that message back in the box to edit. The original stays as
              it is. Files the agent already changed are not reverted.
            </p>
            <ul className="command-suggestions">
              {forkPoints.map((p) => (
                <li key={p.seq}>
                  <button
                    className="link"
                    onClick={async () => {
                      setForkPoints(null);
                      try {
                        applyFork(await api.agentFork(sid, p.seq), "Forked: continuing in a new session from before this message.");
                      } catch (e) {
                        setError(errorText(e));
                      }
                    }}
                  >
                    {shownRequest(p.text).slice(0, 160) || "(empty)"}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        {paths.length > 0 && (
          <ul className="command-suggestions">
            {paths.map((p) => (
              <li key={p}>
                <button className="link mono" onClick={() => insertPath(p)}>
                  @{p}
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="gw-dock">
          <DockedApproval sid={sid} />
          {stepPause && !busy && (
            <div className="gw-dock-card" role="group" aria-label="Long task paused">
              <div>
                <strong>Paused after {state.agentSettings.stepCheckpoint || "many"} steps.</strong> The agent is still
                working on this task; nothing is lost if it continues.
              </div>
              <div className="row">
                <button className="primary" onClick={() => void sendText(CONTINUE_PROMPT, "Continue")}>
                  Continue for {state.agentSettings.stepCheckpoint || "more"} more steps
                </button>
                <button onClick={() => setStepPause(false)}>Stop here</button>
                <span className="small muted">Change how often it asks in Settings → Agent.</span>
              </div>
            </div>
          )}
          <AskCard sid={sid} busy={busy} />
          <RunRequestCard sid={sid} busy={busy} />
          <ResearchCard sid={sid} busy={busy} />
        </div>
        {queued.length > 0 && (
          <ul className="queued small">
            {queued.map((q, i) => (
              <li key={i}>
                <span className="muted">{q.followUp ? "Queued, after this task:" : "Sending now:"}</span>{" "}
                {q.text}
                {q.followUp && (
                  <>
                    {queued.slice(0, i).some((x) => x.followUp) && (
                      <button
                        className="link small"
                        title="Send this follow-up earlier"
                        onClick={() => {
                          // Its place among the follow-ups (the list also shows queued steering).
                          const at = queued.slice(0, i).filter((x) => x.followUp).length;
                          const list = followUps.current;
                          [list[at - 1], list[at]] = [list[at]!, list[at - 1]!];
                          setQueued((l) => {
                            let before = i - 1;
                            while (before >= 0 && !l[before]!.followUp) before--;
                            if (before < 0) return l;
                            const next = [...l];
                            [next[before], next[i]] = [next[i]!, next[before]!];
                            return next;
                          });
                        }}
                      >
                        ↑
                      </button>
                    )}
                    <button
                      className="link small"
                      title="Take this follow-up back to edit it"
                      onClick={() => {
                        const at = queued.slice(0, i).filter((x) => x.followUp).length;
                        followUps.current.splice(at, 1);
                        setQueued((l) => l.filter((_, j) => j !== i));
                        setInput((prev) => [prev, q.text].filter(Boolean).join("\n"));
                      }}
                    >
                      Edit
                    </button>
                    <button
                      className="link small"
                      title="Remove this follow-up"
                      onClick={() => {
                        const at = queued.slice(0, i).filter((x) => x.followUp).length;
                        followUps.current.splice(at, 1);
                        setQueued((l) => l.filter((_, j) => j !== i));
                      }}
                    >
                      ✕
                    </button>
                  </>
                )}
              </li>
            ))}
          </ul>
        )}
        <AttachmentStrip items={attach.items} onRemove={attach.remove} />
        <div className="composer" ref={composerBox}>
          <div className="gw-composer-field">
          <textarea
            ref={composerRef}
            value={input}
            disabled={!loaded}
            rows={1}
            placeholder={loaded ? (session.planMode ? "Describe what you want planned…" : "Ask the agent…") : "Load a model first"}
            title="Enter sends, Shift+Enter starts a new line. / opens commands and skills, @ names a file. Drop files or paste screenshots here."
            onChange={(e) => {
              setInput(e.target.value);
              setSlashAt(0);
              historyAt.current = null;
              lookupPaths(e.target.value, e.target.selectionStart ?? e.target.value.length);
            }}
            onPaste={attach.onPaste}
            onKeyDown={(e) => {
              if (e.key === "Tab" && paths.length > 0) {
                e.preventDefault();
                insertPath(paths[0]!);
                return;
              }
              if (e.key === "Escape" && paths.length > 0) {
                setPaths([]);
                return;
              }
              // The command and skill menu: arrows choose, Tab uses, Escape closes.
              if (slash.length > 0 && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
                e.preventDefault();
                setSlashAt((i) => (i + (e.key === "ArrowDown" ? 1 : slash.length - 1)) % slash.length);
                return;
              }
              if (slash.length > 0 && e.key === "Escape") {
                e.preventDefault();
                setInput(input + " ");
                return;
              }
              // Prompt history, as in Pi: Up at the start of the box, Down to come back.
              const box = e.currentTarget;
              if (e.key === "ArrowUp" && box.selectionStart === 0 && box.selectionEnd === 0 && history.current.length > 0) {
                e.preventDefault();
                const at = historyAt.current === null ? history.current.length - 1 : Math.max(0, historyAt.current - 1);
                historyAt.current = at;
                setInput(history.current[at]!);
                return;
              }
              if (e.key === "ArrowDown" && historyAt.current !== null && box.selectionStart === box.value.length) {
                e.preventDefault();
                const at = historyAt.current + 1;
                historyAt.current = at < history.current.length ? at : null;
                setInput(at < history.current.length ? history.current[at]! : "");
                return;
              }
              if (e.key === "Enter" && e.altKey) {
                e.preventDefault();
                void send(true);
                return;
              }
              if (e.key === "Tab" && slashPick) {
                e.preventDefault();
                useCommand(slashPick);
                return;
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
          />
          <MicButton box={composerRef} setValue={setInput} disabled={!loaded} />
          </div>
          <div className="composer-actions">
            {busy ? (
              <>
                <button onClick={() => void api.agentStop(sid)}>Stop</button>
                <button
                  disabled={!input.trim()}
                  title="Send after the agent finishes this task (Alt+Enter). You can edit, reorder or remove it until then."
                  onClick={() => void send(true)}
                >
                  Queue
                </button>
                <button
                  className="primary"
                  disabled={!input.trim()}
                  title="Interrupt and send now (Enter): the reply being written stops and the agent reads this next. A command already running finishes first."
                  onClick={() => void send()}
                >
                  Send now
                </button>
              </>
            ) : (
              <>
                <Menu
                  label="Add to your request"
                  align="start"
                  trigger={(p) => <IconButton icon={Plus} label="Attach files, name a file, or use a command or skill" {...p} />}
                  items={[
                    {
                      label: attach.picking ? "Reading…" : "Attach files…",
                      icon: Paperclip,
                      title:
                        "Attach files to your request: images (for models that can see), documents (PDF, DOCX), code and text, or programs and libraries (analyzed without running them). You can also drop files here or paste screenshots.",
                      disabled: attach.picking,
                      onSelect: () => void attach.pick(),
                    },
                    {
                      label: "Name a file in the project",
                      icon: AtSign,
                      hint: "@",
                      title: "Type @ and part of a file's name to point the agent at it.",
                      disabled: session.research !== null && session.research !== undefined,
                      onSelect: () => typeIntoComposer("@"),
                    },
                    {
                      label: "Commands and skills",
                      icon: SquareSlash,
                      hint: "/",
                      title: "Type / to see the commands and skills you can use.",
                      onSelect: () => typeIntoComposer("/"),
                    },
                  ]}
                />
                <button className="primary" disabled={!input.trim() || !loaded} onClick={() => void send()}>
                  Send
                </button>
              </>
            )}
            <QuickSwitch state={state} refresh={refresh} busy={busy} onError={setError} />
          </div>
        </div>
      </div>
      {inspector && (
        <Inspector
          title="Session details"
          active={inspector}
          onActive={setInspector}
          onClose={() => setInspector(null)}
          tabs={[
            ...(session.research
              ? []
              : [
                  {
                    id: "changes",
                    label: "Changes",
                    content: <SessionChanges sid={sid} busy={busy} active={inspector === "changes"} />,
                  },
                ]),
            {
              id: "context",
              label: "Context",
              badge: usage?.contextWindow && usage.totalTokens !== null ? `${Math.round((usage.totalTokens / usage.contextWindow) * 100)}%` : undefined,
              content: usage ? <ContextDetails usage={usage} compactions={compactions} /> : <p className="muted small">Shows after the first reply.</p>,
            },
            {
              id: "tasks",
              label: "Tasks",
              badge:
                [todos.length > 0 ? `${tasksDone}/${todos.length}` : null, session.background.length > 0 ? `${session.background.length} running` : null]
                  .filter(Boolean)
                  .join(" · ") || undefined,
              content: (
                <div className="gw-inspector-list">
                  {session.background.length > 0 && (
                    <>
                      <h4>Running in the background</h4>
                      <div className="mono small">{session.background.join("\n")}</div>
                      <p className="small muted">
                        Programs you let keep running after a command. They have the same limits as the agent's commands (no
                        network, only this session's folders), and they stop when the session ends.{" "}
                        <button
                          type="button"
                          className="link"
                          onClick={() => {
                            void api.agentStopBackground(session.id).then(
                              (v) => setSession(v),
                              () => {},
                            );
                          }}
                        >
                          Stop them now
                        </button>
                      </p>
                      <h4>Task list</h4>
                    </>
                  )}
                  {todos.length > 0 ? (
                    <ul className="gw-tasks">
                      {todos.map((t, i) => (
                        <li key={i} className={`todo ${t.status}`}>
                          <span aria-hidden="true">{t.status === "completed" ? "☑" : t.status === "in_progress" ? "▶" : "☐"}</span> {t.content}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className="muted small">The agent's task list shows here when it makes one.</p>
                  )}
                </div>
              ),
            },
            ...(session.research
              ? []
              : [
                  {
                    id: "skills",
                    label: "Skills",
                    badge: session.skills.length || undefined,
                    content: (
                      <div className="gw-inspector-list">
                        {session.skills.length === 0 ? (
                          <p className="muted small">
                            No skills in this session. Skills are folders with a SKILL.md in .claude/skills, .agents/skills or your
                            personal skills folder.
                          </p>
                        ) : (
                          session.skills.map(([name, source]) => {
                            const cmd = commands.find((c) => c.skill === name);
                            return (
                              <div key={name} className="gw-inspector-item">
                                <div>
                                  <div className="mono">{cmd?.name ?? name}</div>
                                  <div className="muted small">{source} skill</div>
                                </div>
                                {cmd && (
                                  <Button size="sm" onClick={() => useCommand(cmd)} disabled={!loaded}>
                                    Use
                                  </Button>
                                )}
                              </div>
                            );
                          })
                        )}
                        {session.commands.length > 0 && (
                          <>
                            <h4>Custom commands</h4>
                            {session.commands.map((c) => (
                              <div key={c.name} className="gw-inspector-item">
                                <div>
                                  <div className="mono">/{c.name}</div>
                                  <div className="muted small">{c.description}</div>
                                </div>
                                <Button size="sm" onClick={() => useCommand({ name: `/${c.name}`, args: c.argumentHint ?? undefined })} disabled={!loaded}>
                                  Use
                                </Button>
                              </div>
                            ))}
                          </>
                        )}
                      </div>
                    ),
                  },
                ]),
            {
              id: "session",
              label: "Session",
              content: (
                <div className="gw-inspector-list">
                  {!session.research && (
                    <>
                      <h4>Folders</h4>
                      <div className="mono small">{shownPaths(session.folders, "\n")}</div>
                    </>
                  )}
                  <h4>Access</h4>
                  <div className="small">
                    {session.research
                      ? "No files or commands: web search and pages only"
                      : session.sandboxed
                        ? `Commands: ${session.shell}, sandboxed, no network`
                        : "Commands disabled (no sandbox)"}
                    {` · Web: ${session.web ?? "off"}`}
                    {session.documents && ` · Documents: ${session.documents}`}
                    {session.mcp && ` · MCP: ${session.mcp}`}
                  </div>
                  {!session.research && (
                    <>
                      <h4>Loaded</h4>
                      <div className="small" title={session.contextFiles.join("\n")}>
                        {session.contextFiles.length
                          ? session.contextFiles.map((f) => f.split(/[\\/]/).pop()).join(", ")
                          : `no AGENTS.md or CLAUDE.md at the top of ${folderNames(session.folders)}`}
                        {` · ${session.skills.length} skill${session.skills.length === 1 ? "" : "s"}`}
                        {` · ${session.commands.length} custom command${session.commands.length === 1 ? "" : "s"}`}
                      </div>
                    </>
                  )}
                  {session.hiddenSecrets.length > 0 && (
                    <>
                      <h4>
                        {session.hiddenSecrets.length === 1
                          ? "1 secret file is hidden from commands"
                          : `${session.hiddenSecrets.length} secret files are hidden from commands`}
                      </h4>
                      <div className="mono small hidden-secrets">{session.hiddenSecrets.slice(0, 50).join("\n")}</div>
                      {session.hiddenSecrets.length > 50 && <div className="small">and {session.hiddenSecrets.length - 50} more</div>}
                      <p className="small muted">
                        Commands the agent runs can't read, change or delete these. Its read tool asks you first. To keep them
                        out of the folder altogether, use "Secrets in this folder" on the project page.
                      </p>
                    </>
                  )}
                </div>
              ),
            },
          ]}
        />
      )}
    </div>
  );
}

/** How a session pane opens: the session, and what to show first. */
export interface Opening {
  view: AgentViewState;
  entries: Entry[];
  input?: string;
  /** Sent as the first request once the pane opens (a reviewed handoff). */
  autoSend?: string;
}

/** What the main area shows. */
type Selection = { kind: "session"; sid: string } | { kind: "project"; id: string } | { kind: "start" } | { kind: "dashboard" };

/** The Agent page: projects on the left, each
 * with its sessions nested under it, as in Claude Code. A project is a set
 * of folders chosen once; every session in it works on those folders. Open
 * sessions keep working while another session or page is shown. */
export function AgentView({ loaded, focus = false, goToModels, state, refresh, active, inbox, onInboxTaken, focusSid, onFocusTaken, onShowing, openSaved, onSavedTaken, onAsk, insert, onLookAtRepo }: {
  /** Show a hub repository's files on the Models page. */
  onLookAtRepo: (repo: string) => void;
  /** Ask about selected text in a side chat. */
  onAsk: (text: string) => void;
  /** A side chat's answer to add to the open session's message box. */
  insert: Insert | null;
  /** A saved session to show (picked in the Ctrl+K palette): its project's page lists it. */
  openSaved: string | null;
  onSavedTaken: () => void;
  loaded: LoadedView | null;
  /** Focus mode: the projects and sessions list is hidden (Ctrl+B, as in Chat). */
  focus?: boolean;
  /** An open session to show (a corner card was clicked). */
  focusSid: string | null;
  onFocusTaken: () => void;
  /** Reports the open session on screen, if any. */
  onShowing: (sid: string | null) => void;
  /** A session started elsewhere (a chat's handoff) to open here. */
  inbox: Opening | null;
  onInboxTaken: () => void;
  /** This page is on screen (it stays mounted while hidden). */
  active: boolean;
  goToModels: () => void;
  state: AppView;
  refresh: () => Promise<void>;
}) {
  const [cards, setCards] = useState<SessionCard[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [saved, setSaved] = useState<TranscriptSummary[]>([]);
  /** Openings of sessions started in this window, by id. */
  const [openings, setOpenings] = useState<Record<string, Opening>>({});
  const [selected, setSelected] = useState<Selection>({ kind: "start" });
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  /** Where a session exported from the sidebar was saved. */
  const [exported, setExported] = useState<string | null>(null);
  /** The research start dialog is open, with this mode preselected. */
  const [choosingResearch, setChoosingResearch] = useState<WebMode | null>(null);
  const [starting, setStarting] = useState<string | null>(null);

  const loadCards = useCallback(async () => {
    try {
      const next = await api.agentSessions();
      // Keep the same list when nothing changed, so nothing redraws.
      setCards((prev) => (JSON.stringify(prev) === JSON.stringify(next) ? prev : next));
    } catch (e) {
      setError(errorText(e));
    }
  }, []);
  const loadLists = useCallback(async () => {
    try {
      const [p, t] = await Promise.all([api.listProjects(), api.agentTranscripts()]);
      setProjects(p);
      setSaved(t);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  // Sessions left open from before this window (the window reloaded) get
  // their state from the core.
  useEffect(() => {
    void (async () => {
      await loadLists();
      try {
        const list = await api.agentSessions();
        setCards(list);
        const restored: Record<string, Opening> = {};
        for (const c of list) {
          const view = await api.agentState(c.id);
          restored[c.id] = { view, entries: [{ kind: "note", text: "This session was already open. Earlier messages are in its saved transcript." }] };
        }
        setOpenings((o) => ({ ...restored, ...o }));
        const last = list[list.length - 1];
        if (last) setSelected((s) => (s.kind === "start" ? { kind: "session", sid: last.id } : s));
      } catch (e) {
        setError(errorText(e));
      }
    })();
  }, [loadLists]);

  // Working / waiting badges.
  usePoll(() => void loadCards(), 1500, active && cards.length > 0);

  // An open session is saved after its first request: list it on its
  // project's page at once. Each saved id is looked
  // for once, so one the list never shows can't reload it again and again.
  const lookedFor = useRef<Set<string>>(new Set());
  useEffect(() => {
    const listed = new Set(saved.map((t) => t.id));
    const missing = cards
      .map((c) => c.transcript)
      .filter((t): t is string => t !== null && !listed.has(t) && !lookedFor.current.has(t));
    if (missing.length === 0) return;
    for (const t of missing) lookedFor.current.add(t);
    void loadLists();
  }, [cards, saved, loadLists]);

  /** A saved session to show read-only in its project's page (opened from Home or the palette). */
  const [readSaved, setReadSaved] = useState<string | null>(null);
  /** The list entry being renamed: "s:<session id>" or "t:<transcript id>". */
  const [renaming, setRenaming] = useState<string | null>(null);
  const opened = (o: Opening) => {
    setOpenings((all) => ({ ...all, [o.view.id]: o }));
    setSelected({ kind: "session", sid: o.view.id });
    void loadCards();
    void loadLists();
  };

  // A saved session was picked in the palette: show the page that lists it.
  useEffect(() => {
    if (!openSaved) return;
    const found = saved.find((t) => t.id === openSaved);
    if (!found && saved.length === 0) return;
    onSavedTaken();
    // Straight to the conversation: the open session, or the saved one to read and continue.
    const live = cards.find((c) => c.transcript === openSaved)?.id;
    if (live) {
      setSelected({ kind: "session", sid: live });
      return;
    }
    setSelected(found?.projectId ? { kind: "project", id: found.projectId } : { kind: "start" });
    setReadSaved(openSaved);
  }, [openSaved, onSavedTaken, saved, cards]);

  // A corner card was clicked: show that session.
  useEffect(() => {
    if (!focusSid) return;
    onFocusTaken();
    if (openings[focusSid]) setSelected({ kind: "session", sid: focusSid });
  }, [focusSid, onFocusTaken, openings]);

  useEffect(() => {
    onShowing(active && selected.kind === "session" ? selected.sid : null);
  }, [active, selected, onShowing]);

  // A chat handed off to a new agent session: open it here.
  useEffect(() => {
    if (!inbox) return;
    onInboxTaken();
    setOpenings((all) => ({ ...all, [inbox.view.id]: inbox }));
    setSelected({ kind: "session", sid: inbox.view.id });
    void loadCards();
    void loadLists();
  }, [inbox, onInboxTaken, loadCards, loadLists]);

  const ended = (sid: string, project: string | null) => {
    setOpenings((all) => {
      const next = { ...all };
      delete next[sid];
      return next;
    });
    setSelected((cur) => (cur.kind === "session" && cur.sid === sid ? (project ? { kind: "project", id: project } : { kind: "start" }) : cur));
    void loadCards();
    void loadLists();
  };

  /** Start a research session in the mode chosen in the trusted dialog. */
  const startResearch = async (mode: WebMode) => {
    setChoosingResearch(null);
    setError(null);
    setStarting("research");
    try {
      opened({ view: await api.agentStartResearch(mode), entries: [] });
    } catch (e) {
      setError(errorText(e));
    } finally {
      setStarting(null);
    }
  };

  /** Start a session in a project (its folders), or choose folders for a new project. */
  const startIn = async (project: string | null, collection: string | null = null, memories = true) => {
    setError(null);
    setStarting(project ?? "new");
    try {
      const s = await api.agentStart(project, collection, memories);
      if (s) opened({ view: s, entries: [] });
    } catch (e) {
      setError(errorText(e));
    } finally {
      setStarting(null);
    }
  };

  /** The guided New project dialog (ADR-024 §5). */
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const newProject = () => {
    setError(null);
    setNewProjectOpen(true);
  };
  /** The project was created: its first session opens with the starter
   * message in the composer, not sent. */
  const projectCreated = async (p: Project, starter: string, notes: string[]) => {
    setNewProjectOpen(false);
    await loadLists();
    setSelected({ kind: "project", id: p.id });
    if (notes.length) setError(notes.join(" "));
    setStarting(p.id);
    try {
      const s = await api.agentStart(p.id, null, true);
      if (s) opened({ view: s, entries: [], input: starter });
    } catch (e) {
      setError(errorText(e));
    } finally {
      setStarting(null);
    }
  };
  /** "Use an existing folder as it is": the plain folder picker. */
  const plainNewProject = async () => {
    setNewProjectOpen(false);
    setError(null);
    try {
      const p = await api.createProject();
      if (p) {
        await loadLists();
        setSelected({ kind: "project", id: p.id });
      }
    } catch (e) {
      setError(errorText(e));
    }
  };

  const openSession = (transcript: string) => cards.find((c) => c.transcript === transcript)?.id ?? null;
  const context = useContextMenu();
  const known = new Set(projects.map((p) => p.id));
  const looseCards = cards.filter((c) => !c.project || !known.has(c.project));
  const looseSaved = saved.filter((t) => (!t.projectId || !known.has(t.projectId)) && !openSession(t.id));
  const isSel = (s: Selection) => JSON.stringify(s) === JSON.stringify(selected);

  const rename = async (what: string, title: string) => {
    setRenaming(null);
    setError(null);
    try {
      if (what.startsWith("s:")) await api.agentRename(what.slice(2), title);
      else await api.renameTranscript(what.slice(2), title);
      await loadCards();
      await loadLists();
    } catch (e) {
      setError(errorText(e));
    }
  };
  // The same exports as the project page's list.
  const exportAs = async (id: string, format: "html" | "md" | "jsonl") => {
    setError(null);
    try {
      const path = await api.exportTranscript(id, format);
      if (path) setExported(`Saved to ${path}. The exported file isn't encrypted.`);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const exportItems = (id: string | null) => {
    const notYet = id ? undefined : "A session is saved, and can be exported, after its first request.";
    return [
      "separator" as const,
      { label: "Export", hint: "web page", icon: Download, disabled: !id, title: notYet ?? "Save as a web page you can open in a browser", onSelect: () => id && void exportAs(id, "html") },
      { label: "Markdown", hint: "text", icon: FileDown, disabled: !id, title: notYet ?? "Save as a Markdown text file", onSelect: () => id && void exportAs(id, "md") },
      { label: "JSONL", hint: "for Import", icon: FileDown, disabled: !id, title: notYet ?? "Save as JSONL, which Import can read back", onSelect: () => id && void exportAs(id, "jsonl") },
    ];
  };
  const sessionMenu = (c: SessionCard) => [
    { label: "Open", icon: Play, onSelect: () => setSelected({ kind: "session" as const, sid: c.id }) },
    {
      label: "Rename",
      icon: Pencil,
      disabled: !c.transcript,
      title: c.transcript ? undefined : "A session is saved, and can be renamed, after its first request.",
      onSelect: () => setRenaming(`s:${c.id}`),
    },
    ...exportItems(c.transcript),
  ];
  const sessionRow = (c: SessionCard) => (
    <li
      key={c.id}
      className={isSel({ kind: "session", sid: c.id }) ? "active" : undefined}
      onContextMenu={(e) => context.open(e, `Session ${c.title}`, sessionMenu(c))}
      onKeyDown={(e) => context.open(e, `Session ${c.title}`, sessionMenu(c))}
    >
      {renaming === `s:${c.id}` ? (
        <RenameField label="Session name" value={c.title} onSave={(t) => void rename(`s:${c.id}`, t)} onCancel={() => setRenaming(null)} />
      ) : (
      <button className="conversation-title" title={`${c.title}\n${shownPaths(c.folders, "\n")}\nRight-click for more`} onClick={() => setSelected({ kind: "session", sid: c.id })}>
        <span className={c.busy ? (c.waiting ? "status-dot waiting" : "status-dot working") : "status-dot open"} />
        {c.title}
        <span className="small muted session-sub">{c.waiting ? "Waiting for its turn" : c.busy ? "Working…" : "Open"}</span>
      </button>
      )}
    </li>
  );
  const continueIt = async (t: TranscriptSummary) => {
    // Continue it: in its project's folders, or (no project) folders
    // chosen now, which become its project.
    setError(null);
    try {
      await continueSaved(t.id, openSession, (sid) => setSelected({ kind: "session", sid }), opened, null, true);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const savedMenu = (t: TranscriptSummary) => [
    { label: "Continue", icon: Play, onSelect: () => void continueIt(t) },
    {
      label: "Read",
      icon: Eye,
      title: "Show it read-only",
      onSelect: () => {
        setSelected(t.projectId && known.has(t.projectId) ? { kind: "project", id: t.projectId } : { kind: "start" });
        setReadSaved(t.id);
      },
    },
    { label: "Rename", icon: Pencil, onSelect: () => setRenaming(`t:${t.id}`) },
    ...exportItems(t.id),
  ];
  const savedRow = (t: TranscriptSummary) => (
    <li
      key={t.id}
      onContextMenu={(e) => context.open(e, `Saved session ${t.title}`, savedMenu(t))}
      onKeyDown={(e) => context.open(e, `Saved session ${t.title}`, savedMenu(t))}
    >
      {renaming === `t:${t.id}` ? (
        <RenameField label="Session name" value={t.title} onSave={(v) => void rename(`t:${t.id}`, v)} onCancel={() => setRenaming(null)} />
      ) : (
      <button
        className="conversation-title saved-session"
        title={`${t.title}\nSaved ${new Date(t.updatedAt * 1000).toLocaleString()}. Click to continue it; right-click to read or rename it.`}
        onClick={() => void continueIt(t)}
      >
        <span className="status-dot" />
        {t.title}
      </button>
      )}
    </li>
  );

  const selectedProject = selected.kind === "project" ? projects.find((p) => p.id === selected.id) : undefined;

  return (
    <div className="chat-layout agent-layout">
      {newProjectOpen && (
        <NewProjectDialog
          onClose={() => setNewProjectOpen(false)}
          onCreated={(p, starter, notes) => void projectCreated(p, starter, notes)}
          onPlainPicker={() => void plainNewProject()}
        />
      )}
      <aside className="conversations agent-sessions" hidden={focus}>
        {context.node}
        <button className="primary new-chat" onClick={newProject} title="Set up a project: its folder, its toolchain and a Git repository. Its sessions all work on that folder.">
          New project…
        </button>
        <button
          className={isSel({ kind: "dashboard" }) ? "link agent-dashboard-link active" : "link agent-dashboard-link"}
          onClick={() => setSelected({ kind: "dashboard" })}
          title="Every open session: status, current step, tasks, time and context"
        >
          Dashboard
          {cards.length > 0 && <span className="small muted"> · {cards.filter((c) => c.busy).length}/{cards.length} working</span>}
        </button>
        <button
          className="link agent-dashboard-link"
          disabled={starting !== null}
          title="Research a question on the web: no files or commands, sources you can check"
          onClick={() =>
            void api.webStatus().then(
              (w) => setChoosingResearch(w.settings.defaultMode),
              () => setChoosingResearch("ask"),
            )
          }
        >
          {starting === "research" ? "Starting…" : "New research"}
        </button>
        {choosingResearch !== null && (
          <ModeChoice
            title="Start a research session?"
            intro="The agent searches the web and reads public pages to answer your question, then cites what it read. It has no files and can't run commands. Choose how its requests are approved."
            initial={choosingResearch}
            onCancel={() => setChoosingResearch(null)}
            onChoose={(m) => void startResearch(m)}
          />
        )}
        <ul className="project-tree">
          {projects.map((p) => {
            const open = cards.filter((c) => c.project === p.id);
            const closed = saved.filter((t) => t.projectId === p.id && !openSession(t.id));
            const folded = collapsed.has(p.id);
            return (
              <li key={p.id} className="project-node">
                <div className={isSel({ kind: "project", id: p.id }) ? "project-row active" : "project-row"}>
                  <button
                    className="link small fold"
                    aria-label={folded ? `Show sessions of ${p.name}` : `Hide sessions of ${p.name}`}
                    onClick={() =>
                      setCollapsed((s) => {
                        const n = new Set(s);
                        if (n.has(p.id)) n.delete(p.id);
                        else n.add(p.id);
                        return n;
                      })
                    }
                  >
                    {folded ? "▸" : "▾"}
                  </button>
                  <button className="project-name" title={shownPaths(p.folders, "\n")} onClick={() => setSelected({ kind: "project", id: p.id })}>
                    {p.name}
                  </button>
                  <button
                    className="link small new-in-project"
                    title={`New session in ${p.name}`}
                    aria-label={`New session in ${p.name}`}
                    disabled={starting !== null}
                    onClick={() => void startIn(p.id)}
                  >
                    {starting === p.id ? "…" : "+"}
                  </button>
                </div>
                {!folded && (
                  <ul>
                    {open.map(sessionRow)}
                    {closed.slice(0, 8).map(savedRow)}
                    {closed.length > 8 && (
                      <li>
                        <button className="link small more-sessions" onClick={() => setSelected({ kind: "project", id: p.id })}>
                          {closed.length - 8} more…
                        </button>
                      </li>
                    )}
                    {open.length === 0 && closed.length === 0 && <li className="small muted empty-project">No sessions yet</li>}
                  </ul>
                )}
              </li>
            );
          })}
          {(looseCards.length > 0 || looseSaved.length > 0) && (
            <li className="project-node">
              <div className={isSel({ kind: "start" }) ? "project-row active" : "project-row"}>
                <span className="fold" />
                <button className="project-name muted" onClick={() => setSelected({ kind: "start" })}>
                  Sessions without a project
                </button>
              </div>
              <ul>
                {looseCards.map(sessionRow)}
                {looseSaved.slice(0, 8).map(savedRow)}
              </ul>
            </li>
          )}
        </ul>
      </aside>
      <div className="agent-main">
        {state?.lockdown && (
          <div className="banner warn" role="status">
            <strong>Lockdown is on.</strong> Agent and research sessions can't start, and saved ones can be read but
            not continued. Chat still works, with no tools. Turn lockdown off from the top bar (you'll type UNLOCK).
          </div>
        )}
        {error && (
          <div className="banner error dismissible">
            <div>{error}</div>
            <button className="small" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        )}
        {exported && (
          <div className="banner dismissible">
            <div>{exported}</div>
            <button className="small" onClick={() => setExported(null)}>
              Dismiss
            </button>
          </div>
        )}
        <div className="page-keep" hidden={selected.kind !== "project"}>
          {selectedProject && (
            <ProjectPanel
              key={selectedProject.id}
              project={selectedProject}
              state={state}
              refresh={refresh}
              sessions={saved.filter((t) => t.projectId === selectedProject.id)}
              openSession={openSession}
              starting={starting === selectedProject.id}
              onStart={(collection, memories) => void startIn(selectedProject.id, collection, memories)}
              onOpened={opened}
              onSelect={(sid) => setSelected({ kind: "session", sid })}
              onChanged={() => void loadLists()}
              readId={readSaved}
              onReadTaken={() => setReadSaved(null)}
              onDeleted={() => {
                setSelected({ kind: "start" });
                void loadLists();
              }}
            />
          )}
        </div>
        <div className="page-keep" hidden={selected.kind !== "dashboard"}>
          <AgentDashboard active={active && selected.kind === "dashboard"} projects={projects} onOpen={(sid) => setSelected({ kind: "session", sid })} />
        </div>
        <div className="page-keep" hidden={selected.kind !== "start"}>
          <AgentStart
            hasProjects={projects.length > 0}
            sessions={looseSaved}
            openSession={openSession}
            onNewProject={newProject}
            onOpened={opened}
            onSelect={(sid) => setSelected({ kind: "session", sid })}
            onChanged={() => void loadLists()}
            readId={selected.kind === "start" ? readSaved : null}
            onReadTaken={() => setReadSaved(null)}
          />
        </div>
        {Object.entries(openings).map(([sid, o]) => (
          <div key={sid} className="page-keep" hidden={!(selected.kind === "session" && selected.sid === sid)}>
            <AgentSessionPane
              sid={sid}
              initial={o}
              loaded={loaded}
              goToModels={goToModels}
              state={state}
              refresh={refresh}
              active={active && selected.kind === "session" && selected.sid === sid}
              onAsk={onAsk}
              insert={insert}
              onLookAtRepo={onLookAtRepo}
              onEnded={() => ended(sid, o.view.project)}
              onReplaced={(next) => {
                ended(sid, o.view.project);
                opened(next);
              }}
              onHome={() => setSelected(o.view.project ? { kind: "project", id: o.view.project } : { kind: "start" })}
              onChanged={() => {
                void loadLists();
                void loadCards();
              }}
              onOpened={opened}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

/** Show the saved session `id` read-only once it is asked for. */
function useReadSaved(
  id: string | null,
  taken: () => void,
  setReading: (t: TranscriptView) => void,
  setError: (e: string | null) => void,
) {
  useEffect(() => {
    if (!id) return;
    taken();
    api.agentTranscript(id).then(setReading, (e) => setError(errorText(e)));
  }, [id, taken, setReading, setError]);
}

/** Continue a saved session, or go to it if it is open. */
async function continueSaved(
  id: string,
  openSession: (transcript: string) => string | null,
  onSelect: (sid: string) => void,
  onOpened: (o: Opening) => void,
  collection: string | null,
  memories: boolean,
) {
  const open = openSession(id);
  if (open) {
    onSelect(open);
    return;
  }
  const t = await api.agentTranscript(id);
  const s = await api.agentResume(id, collection, memories);
  if (s) onOpened({ view: s, entries: [...entriesFrom(t.entries), { kind: "note", text: "Continuing this session." }] });
}

/** A saved session opened read-only. */
function ReadSaved({
  reading,
  isOpen,
  onBack,
  onContinue,
  error,
}: {
  reading: TranscriptView;
  isOpen: boolean;
  onBack: () => void;
  onContinue: () => void;
  error: string | null;
}) {
  const scroll = useStickyScroll(reading);
  return (
    <div className="chat">
      <div className="agent-head">
        <div className="agent-head-info">
          <div>
            <strong>{reading.title}</strong> <span className="small muted">(saved session, read only)</span>
          </div>
          <div className="small muted mono" title={shownPaths(reading.folders, "\n")}>
            {shownPaths(reading.folders, " · ")}
          </div>
        </div>
        <div className="agent-head-actions">
          <button className="small" onClick={onBack}>
            Back
          </button>
          <button className="small primary" onClick={onContinue}>
            {isOpen ? "Go to session" : "Continue"}
          </button>
        </div>
      </div>
      {error && <div className="banner error">{error}</div>}
      <div className="messages" ref={scroll.ref} onScroll={scroll.onScroll}>
        <Entries entries={entriesFrom(reading.entries)} />
      </div>
    </div>
  );
}

/** Options for new sessions: documents to search and memories. */
function useSessionOptions() {
  const [useMemories, setUseMemories] = useState(true);
  const [collections, setCollections] = useState<CollectionSummary[]>([]);
  const [docChoice, setDocChoice] = useState<string>("");
  useEffect(() => {
    api
      .listCollections()
      .then((v) => setCollections(v.collections))
      .catch(() => {});
  }, []);
  const controls = (
    <div className="gw-session-options">
      {collections.length > 0 && (
        <label className="gw-session-option">
          <span>Documents the agent can search</span>
          <select value={docChoice} onChange={(e) => setDocChoice(e.target.value)}>
            <option value="">None</option>
            {collections.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <span
        className="gw-session-option"
        title="Add your saved memories to new sessions and let the agent suggest new ones (you approve each)"
      >
        <span aria-hidden="true">Use memories</span>
        <Toggle checked={useMemories} label="Use memories (the Memory page) in new sessions" onChange={setUseMemories} />
      </span>
    </div>
  );
  return { collection: docChoice || null, memories: useMemories, controls };
}

/** A project: its folders, new sessions, and its saved sessions. */
function ProjectPanel({
  project,
  state,
  refresh,
  sessions,
  openSession,
  starting,
  onStart,
  onOpened,
  onSelect,
  onChanged,
  onDeleted,
  readId,
  onReadTaken,
}: {
  project: Project;
  state: AppView;
  refresh: () => Promise<void>;
  sessions: TranscriptSummary[];
  openSession: (transcript: string) => string | null;
  starting: boolean;
  onStart: (collection: string | null, memories: boolean) => void;
  onOpened: (o: Opening) => void;
  onSelect: (sid: string) => void;
  onChanged: () => void;
  onDeleted: () => void;
  readId: string | null;
  onReadTaken: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState(project.name);
  const [reading, setReading] = useState<TranscriptView | null>(null);
  useReadSaved(readId, onReadTaken, setReading, setError);
  const options = useSessionOptions();
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      onChanged();
    } catch (e) {
      setError(errorText(e));
    }
  };
  const resume = (id: string) =>
    act(async () => {
      await continueSaved(id, openSession, onSelect, onOpened, options.collection, options.memories);
      setReading(null);
    });

  if (reading) {
    return (
      <ReadSaved
        reading={reading}
        isOpen={openSession(reading.id) !== null}
        onBack={() => setReading(null)}
        onContinue={() => void resume(reading.id)}
        error={error}
      />
    );
  }
  return (
    <div className="page gw-project">
      {/* Layout: docs/internal/design/REDESIGN-GAPS.md (start first, then grouped rows). */}
      <div className="gw-project-head">
        <input
          className="project-title"
          value={name}
          aria-label="Project name"
          maxLength={200}
          onChange={(e) => setName(e.target.value)}
          onBlur={() => name.trim() && name !== project.name && void act(() => api.renameProject(project.id, name))}
          onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
        />
        <Button variant="primary" icon={Plus} disabled={starting} onClick={() => onStart(options.collection, options.memories)}>
          {starting ? "Starting…" : "New session"}
        </Button>
      </div>
      {options.controls}
      {error && <div className="banner error">{error}</div>}

      <section className="gw-group">
        <h3 className="gw-group-title">Folders</h3>
        <p className="gw-row-help gw-project-note">
          Every session in this project works on these folders. The agent reads, edits and runs commands only inside
          them; anything else asks you first.
        </p>
        <div className="gw-group-body">
          {project.folders.map((f, i) => (
            <div key={f} className="gw-row">
              <div className="gw-row-text">
                <span className="mono gw-tool-folder" title={shownPath(f)}>
                  {shownPath(f)}
                </span>
              </div>
              {project.folders.length > 1 && (
                <div className="gw-row-control">
                  <Button size="sm" variant="ghost" onClick={() => void act(() => api.removeProjectFolder(project.id, i))}>
                    Remove
                  </Button>
                </div>
              )}
            </div>
          ))}
          <div className="gw-row">
            <div className="gw-row-text">
              <div className="gw-row-help">Folder changes apply to sessions started afterwards.</div>
            </div>
            <div className="gw-row-control">
              <Button size="sm" icon={Plus} onClick={() => void act(() => api.addProjectFolders(project.id))}>
                Add folders…
              </Button>
            </div>
          </div>
        </div>
      </section>

      <section className="gw-group">
        <h3 className="gw-group-title">Set up</h3>
        <p className="gw-row-help gw-project-note">
          The languages and build tools the agent can use in this project (read and run only, no network; changes apply
          to sessions started afterwards), and what its sandbox can't do for itself. Tools for every project are in
          Settings → Agent.
        </p>
        <div className="gw-group-body">
          <ProjectSetup project={project} state={state} refresh={refresh} />
        </div>
      </section>

      <section className="gw-group">
        <h3 className="gw-group-title">Protection</h3>
        <p className="gw-row-help gw-project-note">
          Files that usually hold passwords or keys. The agent's commands and MCP servers can't open them, and its file
          tools ask you before showing one. Moving them to your secrets folder takes them out of the project entirely.
        </p>
        <div className="gw-group-body">
          <ProjectSecrets project={project} />
          <ProjectGitignore project={project} />
        </div>
      </section>

      <SavedSessions
        items={sessions}
        heading="Sessions"
        isOpen={(id) => openSession(id) !== null}
        onContinue={(id) => void resume(id)}
        onOpen={(id) => void act(async () => setReading(await api.agentTranscript(id)))}
        onChanged={onChanged}
      />
      <section className="gw-danger-zone">
        <Button
          size="sm"
          variant="ghost"
          icon={Trash2}
          className="gw-danger-text"
          onClick={() => {
            if (!window.confirm(`Delete the project "${project.name}"? Its saved sessions are kept (under "Sessions without a project"); the folders themselves are not touched.`)) return;
            void act(async () => {
              await api.deleteProject(project.id);
              onDeleted();
            });
          }}
        >
          Delete project
        </Button>
      </section>
    </div>
  );
}

/** No project chosen: start one, or reach sessions saved before projects. */
function AgentStart({
  hasProjects,
  sessions,
  openSession,
  onNewProject,
  onOpened,
  onSelect,
  onChanged,
  readId,
  onReadTaken,
}: {
  hasProjects: boolean;
  sessions: TranscriptSummary[];
  openSession: (transcript: string) => string | null;
  onNewProject: () => void;
  onOpened: (o: Opening) => void;
  onSelect: (sid: string) => void;
  onChanged: () => void;
  readId: string | null;
  onReadTaken: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [reading, setReading] = useState<TranscriptView | null>(null);
  useReadSaved(readId, onReadTaken, setReading, setError);
  const resume = async (id: string) => {
    setError(null);
    try {
      // A session without a project asks for folders, which become its project.
      await continueSaved(id, openSession, onSelect, onOpened, null, true);
      setReading(null);
      onChanged();
    } catch (e) {
      setError(errorText(e));
    }
  };
  if (reading) {
    return (
      <ReadSaved
        reading={reading}
        isOpen={openSession(reading.id) !== null}
        onBack={() => setReading(null)}
        onContinue={() => void resume(reading.id)}
        error={error}
      />
    );
  }
  return (
    <div className="page">
      <section>
        <h2>Agent</h2>
        <p>
          Work is organized in <strong>projects</strong>. A project is the folders you work on; you choose them once,
          and every session in the project uses them. Start as many sessions in a project as you like, each with its
          own conversation; they can work at the same time.
        </p>
        <Fold summary="The agent works only inside the project's folders, asks before anything else, and each turn can be undone.">
          Files outside the folders, git hooks and CI files, and secret files always ask you first. Undo covers the
          agent's edits and what its commands changed in the project, except files over 4 MB, generated folders such as
          node_modules, and secret files.
        </Fold>
        {error && <div className="banner error">{error}</div>}
        <button className="primary" onClick={onNewProject}>
          New project…
        </button>
        {hasProjects && <p className="small muted">Or pick a project on the left and press + for a new session.</p>}
      </section>
      {sessions.length > 0 && (
        <SavedSessions
          items={sessions}
          heading="Sessions without a project"
          note="Saved before projects existed, or imported. Continuing asks for folders, which become the session's project."
          isOpen={(id) => openSession(id) !== null}
          onContinue={(id) => void resume(id)}
          onOpen={async (id) => {
            try {
              setReading(await api.agentTranscript(id));
            } catch (e) {
              setError(errorText(e));
            }
          }}
          onChanged={onChanged}
        />
      )}
    </div>
  );
}
