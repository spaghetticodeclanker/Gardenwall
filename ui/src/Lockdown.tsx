// Lockdown mode as an emergency stop (GW-019
// ADR-011 §2.6a). Turning it on takes one confirmation; the core then stops
// everything and reports only once it is done. Turning it off needs UNLOCK
// typed, and nothing restarts by itself.
import { useEffect, useRef, useState } from "react";
import { api, errorText, type LockdownReport } from "./api";
import { Button, toBody } from "./ui";

/** What turning lockdown on does, in the confirmation. */
const STOPS = [
  "Every agent session ends now: its running command and every program in its sandbox are killed, its cards are withdrawn, and its approvals and permissions end. Sessions are kept and nothing resumes by itself.",
  "Web requests in progress, chat Web switches, research assistants and model downloads stop (downloads are kept for resuming).",
  "MCP servers stop, and the toolchain and \"folders above the project\" permissions are taken back.",
  "The model is unloaded.",
];

/** What stays available while it is on. */
const STILL = "Chat still works, with no tools, web or memories: load a model and talk to it. Documents, voice input and read aloud work too.";

/** What comes back when it is turned off. */
const BACK = [
  "Agent and research sessions can start again, with commands, the web and MCP servers as set in Settings.",
  "Offline mode, memories, tool folders, Python, code mode and the decompiler go back to how you set them.",
];

/** Dialogs a parent opens with `mode`; `onChanged` refreshes the app's state. */
export function LockdownDialog({ mode, onClose, onChanged }: { mode: "on" | "off"; onClose: () => void; onChanged: () => void }) {
  return mode === "on" ? <EngageDialog onClose={onClose} onChanged={onChanged} /> : <UnlockDialog onClose={onClose} onChanged={onChanged} />;
}

function useEscape(onClose: () => void, enabled: boolean) {
  useEffect(() => {
    if (!enabled) return;
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose, enabled]);
}

function EngageDialog({ onClose, onChanged }: { onClose: () => void; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<LockdownReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  // While it stops things, the dialog can't be dismissed.
  useEscape(onClose, !busy);
  useEffect(() => box.current?.focus(), []);

  const engage = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.setLockdown(true);
      setReport(r ?? { stopped: [], problems: [] });
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      onChanged();
    }
  };

  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => !busy && e.target === e.currentTarget && onClose()}>
      <div ref={box} tabIndex={-1} className="modal gw-lockdown" role="dialog" aria-modal="true" aria-labelledby="lockdown-title">
        {report ? (
          <>
            <h2 id="lockdown-title">Lockdown is on</h2>
            {report.stopped.length > 0 ? (
              <>
                <p>Stopped:</p>
                <ul>
                  {report.stopped.map((s) => (
                    <li key={s}>{s}</li>
                  ))}
                </ul>
              </>
            ) : (
              <p>Nothing was running.</p>
            )}
            {report.problems.length > 0 && (
              <div className="banner error" role="alert">
                {report.problems.map((p) => (
                  <div key={p}>{p}</div>
                ))}
              </div>
            )}
            <p className="small muted">{STILL}</p>
            <div className="gw-lockdown-actions">
              <Button variant="primary" onClick={onClose}>
                Close
              </Button>
            </div>
          </>
        ) : (
          <>
            <h2 id="lockdown-title">Lock down Gardenwall?</h2>
            <p>For a model that seems to be a risk. Everything that could act for it stops at once:</p>
            <ul>
              {STOPS.map((s) => (
                <li key={s}>{s}</li>
              ))}
            </ul>
            <p className="small muted">{STILL} To turn lockdown off you'll type UNLOCK.</p>
            {error && <div className="banner error">{error}</div>}
            <div className="gw-lockdown-actions">
              <Button variant="ghost" disabled={busy} onClick={onClose}>
                Cancel
              </Button>
              <Button variant="danger" disabled={busy} onClick={() => void engage()}>
                {busy ? "Stopping everything…" : "Lock down now"}
              </Button>
            </div>
          </>
        )}
      </div>
    </div>,
  );
}

function UnlockDialog({ onClose, onChanged }: { onClose: () => void; onChanged: () => void }) {
  const [word, setWord] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  useEscape(onClose, !busy);
  useEffect(() => input.current?.focus(), []);
  const ready = word.trim() === "UNLOCK" && !busy;

  const unlock = async () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      await api.setLockdown(false, word.trim());
      onChanged();
      onClose();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => !busy && e.target === e.currentTarget && onClose()}>
      <div className="modal gw-lockdown" role="dialog" aria-modal="true" aria-labelledby="unlock-title">
        <h2 id="unlock-title">Turn lockdown off?</h2>
        <p>What comes back:</p>
        <ul>
          {BACK.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ul>
        <p className="small muted">
          Nothing restarts by itself: sessions, MCP servers and the model stay off until you start them.
        </p>
        <label className="gw-lockdown-field">
          <span>
            Type <strong>UNLOCK</strong> to confirm
          </span>
          <input
            ref={input}
            value={word}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => setWord(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void unlock();
            }}
          />
        </label>
        {error && <div className="banner error">{error}</div>}
        <div className="gw-lockdown-actions">
          <Button variant="ghost" disabled={busy} onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!ready} onClick={() => void unlock()}>
            Turn lockdown off
          </Button>
        </div>
      </div>
    </div>,
  );
}
