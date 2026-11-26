// A session's sandbox restart and the install card for a toolchain the
// agent asked for that isn't on this PC (ADR-024 §3 A1, A2). Everything shown
// comes from the core's own data (its toolchain table and official
// addresses); addresses are shown with Copy buttons and never opened by the
// app, as in VcRuntimeNotice.
import { useState } from "react";
import { api, errorText, type AgentView } from "./api";
import { copyText } from "./clipboard";
import { Button } from "./ui";

/** "Rust added. Restart this session's sandbox to use it." with one click. */
export function RestartOffer({
  session,
  onView,
  text,
}: {
  session: AgentView;
  onView: (v: AgentView) => void;
  /** Overrides the offer text (the commands-off banner uses its own). */
  text?: string;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const offer = text ?? session.restartOffer;
  if (!offer) return null;
  const restart = async () => {
    setBusy(true);
    setError(null);
    try {
      onView(await api.agentRestartSandbox(session.id));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="banner dismissible" role="status">
      <span>
        {offer}
        {session.background.length > 0 && " Programs left running stop."}
        {error && <span className="error-text"> {error}</span>}
      </span>
      <button className="small" disabled={busy || session.busy} onClick={() => void restart()}>
        {busy ? "Restarting…" : "Restart sandbox"}
      </button>
    </div>
  );
}

/** Where to get a toolchain the agent asked for. */
export function InstallCardNotice({ session, onChanged }: { session: AgentView; onChanged: () => void }) {
  const [copied, setCopied] = useState(false);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const card = session.installCard;
  if (!card) return null;
  return (
    <div className="banner warn" role="status">
      <div>
        <strong>The agent asked for {card.name}, which isn't installed on this PC.</strong> Install it yourself if you
        want the agent to use it; the agent never installs anything.
      </div>
      <ol className="small">
        <li>
          Get it from <span className="mono">{card.url}</span>{" "}
          <button
            type="button"
            className="link"
            onClick={() => void copyText(card.url).then((ok) => setCopied(ok))}
          >
            {copied ? "Copied" : "Copy"}
          </button>
        </li>
        <li>{card.steps}</li>
        <li>Come back and choose Check again, then tell the agent to continue.</li>
      </ol>
      {result && <div className="small muted">{result}</div>}
      <div className="row gap-sm">
        <Button
          size="sm"
          disabled={checking}
          onClick={async () => {
            setChecking(true);
            try {
              const found = await api.agentInstallCheck(session.id);
              if (found) {
                setResult(null);
                onChanged();
              } else {
                setResult(`${card.name} still isn't found. Installed it somewhere unusual? Settings → Agent tools can add its folder.`);
              }
            } catch (e) {
              setResult(errorText(e));
            } finally {
              setChecking(false);
            }
          }}
        >
          {checking ? "Checking…" : "Check again"}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => void api.agentInstallDismiss(session.id).then(onChanged, () => {})}
        >
          Close
        </Button>
      </div>
    </div>
  );
}
