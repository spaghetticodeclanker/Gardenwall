import { useState } from "react";
import { api, errorText, type ResearchCardView } from "./api";
import { Markdown } from "./Markdown";
import { usePoll } from "./usePoll";

/**
 * Research sub-agent (ADR-019). In a session with
 * folders the agent reads the web only through a research assistant that has
 * no access to the project. This card asks the user to approve (or edit) the
 * question first, shows progress, then shows the findings for review (unless
 * Settings → Web access turns reviewing off). The card is built from the
 * core's copy; the model can't answer it.
 */
export function ResearchCard({ sid, busy }: { sid: string; busy: boolean }) {
  const [view, setView] = useState<ResearchCardView | null>(null);
  const [text, setText] = useState("");
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  usePoll(
    () =>
      void api
        .agentResearchCard(sid)
        .then((next) =>
          setView((current) => {
            if (next?.id !== current?.id) {
              setText(
                next?.card.phase === "question"
                  ? next.card.question
                  : next?.card.phase === "findings"
                    ? next.card.findings.text
                    : "",
              );
              setEditing(false);
              setError(null);
            }
            return next;
          }),
        )
        .catch(() => setView(null)),
    700,
    busy,
  );

  if (!view || !busy) return null;
  const card = view.card;
  const answer = async (approve: boolean) => {
    try {
      await api.agentResearchAnswer(sid, view.id, approve, approve ? text : null);
      setView(null);
    } catch (e) {
      setError(errorText(e));
    }
  };

  if (card.phase === "working") {
    return (
      <div className="ask-card research-card" role="status" aria-label="Research in progress">
        <div className="small muted">Researching (a research assistant with no access to your project):</div>
        <div className="research-question">{card.question}</div>
        <div className="small">{card.step}…</div>
      </div>
    );
  }

  if (card.phase === "question") {
    return (
      <div className="ask-card research-card" role="group" aria-label="The agent wants to research a question">
        <div className="small muted">The agent wants to research this on the web (written by the model; you can edit it):</div>
        <textarea
          className="research-text"
          rows={4}
          maxLength={2000}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <p className="small muted">
          Only this question leaves your computer: a research assistant with no access to your project searches and
          reads public pages to answer it, without asking for each page. Remove anything private before you allow it.
        </p>
        {error && <div className="banner error">{error}</div>}
        <div>
          <button className="primary" disabled={!text.trim()} onClick={() => void answer(true)}>
            Allow research
          </button>{" "}
          <button className="link small" onClick={() => void answer(false)}>
            Don't research
          </button>
        </div>
      </div>
    );
  }

  const f = card.findings;
  return (
    <div className="ask-card research-card" role="group" aria-label="Research findings to review">
      <div className="small muted">
        Research findings for: <span className="research-question">{card.question}</span>
      </div>
      <div className="small muted">
        {f.searches} {f.searches === 1 ? "search" : "searches"}, {f.pages} {f.pages === 1 ? "page" : "pages"}
        {f.failed > 0 && `, ${f.failed} failed`} · from web pages, written by the model: check it before the agent relies on it
      </div>
      {!f.complete && <div className="banner warn">The research stopped before it finished, so this may be incomplete.</div>}
      {editing ? (
        <textarea className="research-text" rows={12} maxLength={16384} value={text} onChange={(e) => setText(e.target.value)} />
      ) : (
        <div className="research-findings">
          <Markdown text={text} />
        </div>
      )}
      {error && <div className="banner error">{error}</div>}
      <div>
        <button className="primary" disabled={!text.trim()} onClick={() => void answer(true)}>
          Send to the agent
        </button>{" "}
        <button onClick={() => setEditing(!editing)}>{editing ? "Done editing" : "Edit"}</button>{" "}
        <button className="link small" onClick={() => void answer(false)}>
          Discard
        </button>
      </div>
      <p className="small muted">
        Findings you send count as your own message, so the agent's later edits and commands don't need extra approval.
        To send findings back without this review, turn it off in Settings → Web access.
      </p>
    </div>
  );
}
