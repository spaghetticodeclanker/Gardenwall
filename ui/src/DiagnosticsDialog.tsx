// "Copy diagnostics" (release plan feature 2): the report the core builds,
// shown in full before anything leaves this window. Copy puts it on the
// clipboard; Save writes the exact text shown through the native save dialog
// (the core keeps the text, the webview never sends it back). Gardenwall
// sends it nowhere.

import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { api, errorText } from "./api";
import { copyText } from "./clipboard";
import { Button, toBody } from "./ui";

export function DiagnosticsDialog({ onClose }: { onClose: () => void }) {
  const [report, setReport] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [saved, setSaved] = useState(false);
  // Focus goes to the dialog, not a button: the Enter that opened it (Ctrl+K)
  // would otherwise press that button too.
  const dialog = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api.diagnosticsReport().then(setReport).catch((e) => setError(errorText(e)));
    dialog.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const copy = async () => {
    if (report && (await copyText(report))) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  };
  const save = async () => {
    setError(null);
    try {
      setSaved(await api.saveDiagnostics());
    } catch (e) {
      setError(errorText(e));
    }
  };

  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div ref={dialog} tabIndex={-1} className="modal gw-diag" role="dialog" aria-modal="true" aria-labelledby="diag-title">
        <h2 id="diag-title">Diagnostics for a problem report</h2>
        <p className="small muted">
          Versions, hardware, settings, the loaded model, recent errors and the model server's last log lines. File paths,
          web addresses, email addresses and your user name are replaced, and lines that look like secrets are left out.
          Prompts, replies, files and chat titles are never included. Nothing is sent anywhere: read it, then copy or save it
          to share it yourself.
        </p>
        {error && <div className="banner error">{error}</div>}
        {report === null && !error ? (
          <p className="small muted">Gathering…</p>
        ) : (
          report !== null && (
            <pre className="gw-diag-text" tabIndex={0} aria-label="Diagnostics report">
              {report}
            </pre>
          )
        )}
        <div className="gw-diag-actions">
          <Button variant="primary" icon={copied ? Check : Copy} disabled={!report} onClick={() => void copy()}>
            {copied ? "Copied" : "Copy report"}
          </Button>
          <Button disabled={!report} onClick={() => void save()}>
            Save as a file…
          </Button>
          {saved && <span className="small muted">Saved.</span>}
          <button className="link gw-diag-close" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>,
  );
}
