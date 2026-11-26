import { useState } from "react";
import { api, errorText, type RunCard } from "./api";
import { copyText } from "./Markdown";
import { usePoll } from "./usePoll";
import { shownPath } from "./paths";

/**
 * "Run this yourself": the agent's commands have
 * no network, so it can ask the user to run one. The card shows the core's
 * checked copy of the request: the exact command, why, the real folder, a
 * warning when it looks risky. Gardenwall never runs it.
 */
export function RunRequestCard({ sid, busy }: { sid: string; busy: boolean }) {
  const [card, setCard] = useState<RunCard | null>(null);
  const [pasted, setPasted] = useState("");
  const [copied, setCopied] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  usePoll(
    () =>
      void api
        .agentRunRequest(sid)
        .then((next) =>
          setCard((current) => {
            if (next?.id !== current?.id) {
              setPasted("");
              setCopied(null);
              setError(null);
            }
            return next;
          }),
        )
        .catch(() => setCard(null)),
    700,
    busy,
  );

  if (!card || !busy) return null;
  const r = card.request;
  const send = async (done: boolean) => {
    try {
      await api.agentRunAnswer(sid, card.id, done, done ? pasted : null);
      setCard(null);
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <div className="ask-card run-card" role="group" aria-label="The agent asks you to run a command">
      <div className="small muted">
        {r.byGardenwall
          ? "The agent needs this project's dependencies. Gardenwall's standard command for them (written by Gardenwall, not the model):"
          : "The agent asks you to run a command yourself (written by the model):"}
      </div>
      <div className="run-reason">{r.reason}</div>
      <pre className="approval-pre run-command">{r.command}</pre>
      <div className="small">
        Run it in <span className="mono">{shownPath(r.folder)}</span>
      </div>
      {r.hidden > 0 && (
        <div className="banner warn">
          The command had {r.hidden} hidden character{r.hidden === 1 ? "" : "s"}, shown above as [U+…]. Don't run it
          unless you understand why they are there.
        </div>
      )}
      {r.warning && <div className="banner warn">{r.warning}</div>}
      {r.afterWeb && (
        <div className="banner warn">
          This session read web content before asking. A web page may have suggested this command.
        </div>
      )}
      <p className="small muted">
        Gardenwall won't run this. If you run it, it runs with your full rights, outside the sandbox, with network
        access. Read it first, and run it only if you understand it.
      </p>
      <div className="row">
        <button
          onClick={() =>
            void copyText(r.command).then((ok) => setCopied(ok ? "Copied." : "Couldn't copy; select the text instead."))
          }
        >
          Copy command
        </button>
        <button
          onClick={() =>
            void copyText(r.folder).then((ok) => setCopied(ok ? "Folder copied." : "Couldn't copy; select the text instead."))
          }
        >
          Copy folder
        </button>
        {copied && <span className="small muted">{copied}</span>}
      </div>
      <textarea
        className="run-pasted"
        rows={3}
        maxLength={8000}
        placeholder="Optional: paste what the command printed, so the agent can see how it went"
        value={pasted}
        onChange={(e) => setPasted(e.target.value)}
      />
      {error && <div className="banner error">{error}</div>}
      <div>
        <button className="primary" onClick={() => void send(true)}>
          I ran it
        </button>{" "}
        <button className="link small" onClick={() => void send(false)}>
          Don't run it
        </button>
      </div>
    </div>
  );
}
