import { usePoll } from "./usePoll";
import { useState } from "react";
import { api, errorText, type DashboardRow, type Project } from "./api";

/** How often the dashboard refreshes while it is on screen. */
const POLL_MS = 1000;

const STATUS_TEXT: Record<DashboardRow["status"], string> = {
  open: "Open",
  working: "Working",
  approval: "Needs your approval",
  needsYou: "Needs you",
  waitingTurn: "Waiting for its turn",
  finished: "Finished",
  stopped: "Stopped",
  failed: "Failed",
  paused: "Paused: continue or stop",
};

const STATUS_DOT: Record<DashboardRow["status"], string> = {
  open: "status-dot open",
  working: "status-dot working",
  approval: "status-dot waiting",
  needsYou: "status-dot waiting",
  waitingTurn: "status-dot waiting",
  finished: "status-dot finished",
  stopped: "status-dot",
  failed: "status-dot failed",
  paused: "status-dot waiting",
};

export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ${s % 60} s`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

/** Every open agent session at a glance (planned feature 2). Everything
 * shown comes from the core: flags, tool names, counts and times. */
export function AgentDashboard({ active, projects, onOpen }: {
  /** On screen; it polls only then. */
  active: boolean;
  projects: Project[];
  onOpen: (sid: string) => void;
}) {
  const [rows, setRows] = useState<DashboardRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  usePoll(
    () =>
      void api
        .agentDashboard()
        .then((r) => {
          // Keep the same rows when nothing changed, so nothing redraws.
          setRows((prev) => (JSON.stringify(prev) === JSON.stringify(r) ? prev : r));
          setError(null);
        })
        .catch((e) => setError(errorText(e))),
    POLL_MS,
    active,
  );

  const projectName = (id: string | null) => (id ? projects.find((p) => p.id === id)?.name ?? null : null);
  const attention = rows?.filter((r) => r.status === "approval" || r.status === "needsYou" || r.status === "failed" || r.status === "paused").length ?? 0;

  return (
    <div className="dashboard">
      <div className="dashboard-head">
        <h2>Dashboard</h2>
        {rows && (
          <span className="small muted">
            {rows.length === 0
              ? "No open sessions"
              : `${rows.length} open · ${rows.filter((r) => r.status === "working" || r.status === "approval" || r.status === "needsYou" || r.status === "waitingTurn").length} working${attention ? ` · ${attention} need attention` : ""}`}
          </span>
        )}
      </div>
      {error && <div className="banner error">{error}</div>}
      {rows && rows.length === 0 && (
        <p className="muted">Open sessions appear here with what each one is doing. Start one from a project on the left.</p>
      )}
      <ul className="dashboard-list">
        {rows?.map((r) => {
          const ctx = r.contextTokens != null && r.contextWindow ? Math.min(100, Math.round((r.contextTokens / r.contextWindow) * 100)) : null;
          const project = projectName(r.project);
          return (
            <li key={r.id} className={`dashboard-row ${r.status}`}>
              <button className="dashboard-open" onClick={() => onOpen(r.id)} title="Open this session">
                <span className={STATUS_DOT[r.status]} />
                <span className="dashboard-title">{r.title}</span>
                <span className="dashboard-status">{STATUS_TEXT[r.status]}</span>
                <span className="small muted dashboard-sub">
                  {project ?? r.folders.map((f) => f.split(/[\\/]/).filter(Boolean).pop()).join(", ")}
                  {r.step && ` · ${r.step}`}
                  {r.detail && ` · ${r.detail}`}
                </span>
                <span className="dashboard-facts small">
                  {r.todosTotal > 0 && (
                    <span title="Task list">
                      Tasks {r.todosDone}/{r.todosTotal}
                    </span>
                  )}
                  {r.elapsedMs != null && (
                    <span title={r.endedAtMs ? "Length of the last request" : "Time on this request so far"}>{duration(r.elapsedMs)}</span>
                  )}
                  {ctx != null && (
                    <span className="dashboard-ctx" title={`${r.contextTokens!.toLocaleString()} of ${r.contextWindow!.toLocaleString()} tokens`}>
                      <span className="meter-bar">
                        <span className={`meter-fill ${ctx >= 85 ? "danger" : ctx >= 70 ? "warn" : "ok"}`} style={{ width: `${ctx}%`, display: "block" }} />
                      </span>
                      {ctx}% context
                    </span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      {attention > 0 && (
        <p className="small muted">Approvals wait on a card in their session; open the session to answer them.</p>
      )}
    </div>
  );
}
