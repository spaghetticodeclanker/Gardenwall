// The trusted approval card (ADR-007 §5, ADR-016 §2.3 and §4).
//
// Everything shown here comes from the proposal the trusted core stored, never
// from model output, and is rendered as plain text. It is drawn outside the
// message list, in a fixed Gardenwall frame that rendered model text can't
// produce, so content in a model's answer can neither imitate nor reach these
// controls. A request from the agent session on screen docks above its
// composer (`DockedApproval`); every other request (other sessions, chats with
// the Web switch, research sub-agents) shows as the standalone card
// (`ApprovalDialog`). Ignoring a card never approves anything; the request
// simply expires.
import { usePoll } from "./usePoll";
import { useEffect, useRef, useState } from "react";
import { Lock } from "lucide-react";
import { api, ApprovalDetails, ApprovalKind, errorText, type PendingApproval } from "./api";

const TITLES: Record<ApprovalKind, string> = {
  RevealSecret: "Read a protected file?",
  WriteSecret: "Change a protected file?",
  PersistenceRisk: "Change a file that can run commands later?",
  OutsideRead: "Read outside your granted folders?",
  OutsideWrite: "Change a file outside your granted folders?",
  NetworkCommand: "Run a command that uses the network?",
  Egress: "Send data off this computer?",
  McpTool: "Use a tool from an MCP server?",
  RiskyCommand: "Run a command that may change what runs later?",
  HeldBack: "Keep changes to files that can run code later?",
  KeepRunning: "Let programs keep running in the sandbox?",
  InitRepository: "Create a Git repository?",
  AddToolchain: "Add a toolchain to the agent's sandbox?",
};

function parseDetails(raw: string | null): ApprovalDetails {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw) as unknown;
    if (!v || typeof v !== "object") return {};
    const o = v as Record<string, unknown>;
    const str = (k: string) => (typeof o[k] === "string" ? (o[k] as string) : undefined);
    return {
      reason: str("reason"),
      operation: str("operation"),
      target: str("target"),
      diff: str("diff"),
      payload: str("payload"),
      grant: str("grant"),
      source: str("source"),
      agentReason: str("agentReason"),
      scope: str("scope"),
      note: str("note"),
    };
  } catch {
    return {};
  }
}

/** The session-grant button for web requests (ADR-011 §2.6). */
function grantLabel(scope: string | undefined): { label: string; detail: string } | null {
  if (scope === "search") {
    return {
      label: "Allow searches this session",
      detail: "Later searches in this agent session run without asking. Each is still checked and logged.",
    };
  }
  if (scope?.startsWith("mcp:") && scope.endsWith("__resources")) {
    return {
      label: "Allow this server's resources this session",
      detail: "Later listing and reading of this MCP server's resources in this agent session run without asking. Each is still checked and logged.",
    };
  }
  if (scope?.startsWith("mcp:")) {
    return {
      label: "Allow this tool this session",
      detail: "Later calls to this MCP tool in this agent session run without asking. Each is still checked and logged.",
    };
  }
  const origin = scope?.startsWith("fetch:https://") ? scope.slice("fetch:https://".length) : null;
  if (origin) {
    const host = origin.replace(/:443$/, "");
    return {
      label: `Allow ${host} this session`,
      detail: `Later pages from ${host} in this agent session load without asking. Each is still checked and logged.`,
    };
  }
  return null;
}

/** The time, refreshed every `intervalMs` only while `enabled` (a request
 * is waiting), so an idle window doesn't redraw the dialog. */
function useNow(intervalMs: number, enabled: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!enabled) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs, enabled]);
  return now;
}

function diffClass(line: string): string | undefined {
  if (line.startsWith("+") && !line.startsWith("+++")) return "diff-add";
  if (line.startsWith("-") && !line.startsWith("---")) return "diff-del";
  return undefined;
}

/** The last list of pending requests, shared by every card host so the
 * docked and standalone cards always agree on where a request shows. */
let shared: PendingApproval[] = [];
const listeners = new Set<(q: PendingApproval[]) => void>();

async function refreshPending() {
  try {
    const next = await api.listPendingApprovals();
    if (JSON.stringify(next) === JSON.stringify(shared)) return;
    shared = next;
    listeners.forEach((l) => l(next));
  } catch {
    // If the store is unavailable no card shows; nothing is approved.
  }
}

/** Pending requests, polled while the window is visible. */
function usePending() {
  const [queue, setQueue] = useState<PendingApproval[]>(shared);
  useEffect(() => {
    listeners.add(setQueue);
    setQueue(shared);
    return () => {
      listeners.delete(setQueue);
    };
  }, []);
  usePoll(() => void refreshPending(), 1500);
  return { queue, poll: refreshPending };
}

/** The card itself: the same trusted content docked or standalone. */
function ApprovalCard({
  current,
  waiting,
  now,
  onDecided,
  docked,
}: {
  current: PendingApproval;
  /** Other requests waiting after this one. */
  waiting: number;
  now: number;
  onDecided: () => Promise<void>;
  docked: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const d = parseDetails(current.details);
  const grant = grantLabel(d.grant);
  const secondsLeft = Math.max(0, Math.ceil((current.deadlineMs - now) / 1000));
  const toolchain = current.kind === "AddToolchain";

  const decide = async (approve: boolean, grant = false) => {
    setBusy(true);
    setError(null);
    try {
      await api.decideApproval(current.id, approve, grant);
    } catch (e) {
      // Report the outcome as uncertain rather than assuming either way.
      setError(`${errorText(e)}. The request was not confirmed as ${approve ? "allowed" : "denied"}.`);
    } finally {
      setBusy(false);
      await onDecided();
    }
  };

  return (
    <>
      <div className="approval-badge">
        <Lock aria-hidden="true" size={13} strokeWidth={2} /> Gardenwall approval
      </div>
      <h2 id={`approval-title-${current.id}`}>{TITLES[current.kind]}</h2>
      {d.reason && <p>{d.reason}</p>}
      <dl className="approval-facts">
        {current.sessionTitle && !docked && (
          <>
            <dt>Session</dt>
            <dd>{current.sessionTitle}</dd>
          </>
        )}
        {d.source === "program" && (
          <>
            <dt>Asked by</dt>
            <dd>
              A program the model wrote (code mode). It waits for your answer; if you deny, the program
              stops.
            </dd>
          </>
        )}
        {d.operation && (
          <>
            <dt>Action</dt>
            <dd>{d.operation}</dd>
          </>
        )}
        {d.target && (
          <>
            <dt>{toolchain ? "Folder" : "Target"}</dt>
            <dd className="mono">{d.target}</dd>
          </>
        )}
        {d.agentReason && (
          <>
            <dt>The agent says</dt>
            <dd>{d.agentReason}</dd>
          </>
        )}
        <dt>Scope</dt>
        <dd>
          {toolchain
            ? d.scope === "every"
              ? "Every project's sessions from now on. Remove it in Settings → Agent tools."
              : "This project's sessions from now on. Remove it on the project page."
            : "This one action only. Anything else will ask again."}
        </dd>
        {d.note && (
          <>
            <dt>Note</dt>
            <dd>{d.note}</dd>
          </>
        )}
        <dt>Expires</dt>
        <dd>
          in {secondsLeft} s{waiting > 0 && ` · ${waiting} more waiting`}
        </dd>
      </dl>
      {d.payload && (
        <>
          <div className="small muted">Data that would leave this computer</div>
          <pre className="approval-pre">{d.payload}</pre>
        </>
      )}
      {d.diff && (
        <>
          <div className="small muted">Changes</div>
          <pre className="approval-pre">
            {d.diff.split("\n").map((line, i) => (
              <span key={i} className={diffClass(line)}>
                {line}
                {"\n"}
              </span>
            ))}
          </pre>
        </>
      )}
      {!current.details && <p className="muted">The details of this request are no longer available.</p>}
      {error && <div className="banner error">{error}</div>}
      <div className="approval-actions">
        <button disabled={busy} onClick={() => void decide(false)}>
          {toolchain ? "Not now" : "Deny"}
        </button>
        {grant && (
          <button disabled={busy || !current.details} onClick={() => void decide(true, true)} title={grant.detail}>
            {grant.label}
          </button>
        )}
        <button className="primary" disabled={busy || !current.details} onClick={() => void decide(true)}>
          {toolchain ? (d.scope === "every" ? "Add to every project" : "Add to this project") : "Allow once"}
        </button>
      </div>
    </>
  );
}

/**
 * The standalone card for requests not docked on screen: other agent
 * sessions, chats with the Web switch and research sub-agents.
 * `dockedSid` is the agent session whose own requests dock in its pane.
 */
export function ApprovalDialog({ dockedSid }: { dockedSid: string | null }) {
  const { queue, poll } = usePending();
  const mine = queue.filter((p) => !dockedSid || p.agentSession !== dockedSid);
  const now = useNow(500, mine.length > 0);
  const current = mine.find((p) => p.deadlineMs > now);
  if (!current) return null;
  return (
    <div className="approval-backdrop" role="presentation">
      <div className="approval" role="alertdialog" aria-modal="true" aria-labelledby={`approval-title-${current.id}`}>
        <ApprovalCard current={current} waiting={mine.length - 1} now={now} onDecided={poll} docked={false} />
      </div>
    </div>
  );
}

/**
 * This agent session's requests, docked above its composer (ADR-016 §4):
 * outside the message list, in the fixed Gardenwall frame, and focused when
 * it appears (the card, not a button, so a stray Enter approves nothing).
 */
export function DockedApproval({ sid }: { sid: string }) {
  const { queue, poll } = usePending();
  const mine = queue.filter((p) => p.agentSession === sid);
  const now = useNow(500, mine.length > 0);
  const current = mine.find((p) => p.deadlineMs > now);
  const box = useRef<HTMLDivElement>(null);
  const currentId = current?.id;
  useEffect(() => {
    if (currentId) box.current?.focus();
  }, [currentId]);
  if (!current) return null;
  return (
    <div
      className="approval approval-docked"
      role="alertdialog"
      aria-labelledby={`approval-title-${current.id}`}
      tabIndex={-1}
      ref={box}
      key={current.id}
    >
      <ApprovalCard current={current} waiting={mine.length - 1} now={now} onDecided={poll} docked />
    </div>
  );
}
