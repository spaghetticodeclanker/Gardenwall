// Web research pieces shared by the agent and chat views (ADR-018): the
// approval-mode choice, the mode badge, and the visited-sites list.
//
// Everything listed here comes from the core's own log. Addresses and
// queries were chosen by the model, so they are shown as plain text and
// never opened or turned into links.
import { useEffect, useState } from "react";
import { errorText, Visit, VisitsView, WebMode } from "./api";
import { toBody } from "./ui";

export const MODE_LABEL: Record<WebMode, string> = {
  ask: "Ask for each page",
  research: "Research mode",
};

export const MODE_DETAIL: Record<WebMode, string> = {
  ask: "Every search and every new page asks you first, on a card in the session. From the card you can also allow searches, or one site, for the rest of the session.",
  research:
    "Searches and public https:// pages run without asking, for this session only. Private addresses, plain http:// and anything that looks like a secret stay blocked, and every request is checked and listed.",
};

/** The trusted choice of approval mode, shown before web access starts. */
export function ModeChoice({
  title,
  intro,
  initial,
  onChoose,
  onCancel,
}: {
  title: string;
  intro: string;
  initial: WebMode;
  onChoose: (mode: WebMode) => void;
  onCancel: () => void;
}) {
  const [mode, setMode] = useState<WebMode>(initial);
  return toBody(
    <div className="approval-backdrop" role="presentation">
      <div className="handoff mode-choice" role="dialog" aria-modal="true" aria-labelledby="mode-title">
        <h2 id="mode-title">{title}</h2>
        <p className="small muted">{intro}</p>
        {(["ask", "research"] as WebMode[]).map((m) => (
          <label key={m} className={`mode-option ${mode === m ? "chosen" : ""}`}>
            <input type="radio" name="web-mode" checked={mode === m} onChange={() => setMode(m)} />
            <span>
              <strong>{MODE_LABEL[m]}</strong>
              <span className="small muted block">{MODE_DETAIL[m]}</span>
            </span>
          </label>
        ))}
        <p className="small muted">
          Either way, pages are read as plain text by a sandboxed reader: nothing on a page runs, and links on a page are
          never followed by themselves. A list of every search and page is kept with the conversation.
        </p>
        <div className="row">
          <button className="primary" onClick={() => onChoose(mode)}>
            Start
          </button>
          <button onClick={onCancel}>Cancel</button>
        </div>
      </div>
    </div>
  );
}

/** The mode in a header, with a one-click way to tighten it. */
export function ModeBadge({ mode, onTighten }: { mode: WebMode; onTighten: () => void }) {
  return (
    <span className={`mode-badge ${mode}`}>
      {MODE_LABEL[mode]}
      {mode === "research" && (
        <button className="link" onClick={onTighten} title="End the research grant: every request asks from now on">
          Ask for each page from now on
        </button>
      )}
    </span>
  );
}

const ALLOWED_LABEL: Record<Visit["allowed"], string> = {
  pending: "waiting",
  approved: "approved by you",
  granted: "covered by a grant",
  typed: "typed by you",
  denied: "denied",
  expired: "expired",
  blocked: "blocked",
};

function size(bytes: number | null): string {
  if (bytes === null) return "";
  return bytes < 1024 ? `${bytes} B` : bytes < 1 << 20 ? `${Math.round(bytes / 1024)} KB` : `${(bytes / (1 << 20)).toFixed(1)} MB`;
}

function outcome(v: Visit): string {
  if (v.allowed === "blocked" || v.allowed === "denied" || v.allowed === "expired") return v.note ?? "";
  if (v.ok === false) return `failed: ${v.note ?? ""}`;
  if (v.ok === true) return v.kind === "page" ? size(v.bytes) : "results";
  return "";
}

/** Every search and page, from the core's log, plus the source check. */
export function VisitedSites({ load, onClose }: { load: () => Promise<VisitsView>; onClose: () => void }) {
  const [view, setView] = useState<VisitsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    load().then(setView, (e) => setError(errorText(e)));
  }, [load]);
  const searches = view?.visits.filter((v) => v.kind === "search").length ?? 0;
  const pages = view?.visits.filter((v) => v.kind === "page").length ?? 0;
  return toBody(
    <div className="approval-backdrop" role="presentation" onClick={onClose}>
      <div className="handoff visited" role="dialog" aria-modal="true" aria-labelledby="visited-title" onClick={(e) => e.stopPropagation()}>
        <h2 id="visited-title">Visited sites</h2>
        <p className="small muted">
          Recorded by Gardenwall as each request was made, not written by the model. Addresses are shown as text and
          aren't opened.
        </p>
        {error && <div className="banner error">{error}</div>}
        {view && view.visits.length === 0 && <p>No searches or pages yet.</p>}
        {view && view.visits.length > 0 && (
          <>
            <div className="small">
              {searches} {searches === 1 ? "search" : "searches"} · {pages} {pages === 1 ? "page" : "pages"}
              {view.dropped > 0 && ` · ${view.dropped} more not listed`}
            </div>
            <table className="visited-table">
              <thead>
                <tr>
                  <th>Time</th>
                  <th>What</th>
                  <th>Allowed</th>
                  <th>Result</th>
                </tr>
              </thead>
              <tbody>
                {view.visits.map((v, i) => (
                  <tr key={i} className={v.ok === false || ["blocked", "denied", "expired"].includes(v.allowed) ? "bad" : ""}>
                    <td className="nowrap">{new Date(v.atMs).toLocaleTimeString()}</td>
                    <td className="mono wrap">
                      {v.kind === "search" ? `Search (${v.via}): ` : ""}
                      {v.target}
                    </td>
                    <td>{ALLOWED_LABEL[v.allowed]}</td>
                    <td className="wrap">{outcome(v)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}
        {view && view.unopened.length > 0 && (
          <div className="banner warn">
            <strong>Not opened in this session.</strong> The latest answer links to{" "}
            {view.unopened.length === 1
              ? "a page that was never fetched here, so it wasn't checked:"
              : "pages that were never fetched here, so they weren't checked:"}
            <ul className="mono">
              {view.unopened.map((u) => (
                <li key={u}>{u}</li>
              ))}
            </ul>
          </div>
        )}
        <div className="row">
          <button onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}
