import { useEffect, useRef, useState } from "react";
import { Check } from "lucide-react";
import { Sprig } from "./art/Sprig";
import {
  api,
  errorText,
  type HandoffDraft,
  type HandoffSource,
  type HandoffStarted,
  type HandoffSuggestion,
  type HandoffTarget,
  type Project,
} from "./api";
import { shownPaths } from "./paths";
import { toBody } from "./ui";

/** Longest note the core accepts (`handoff::MAX_HANDOFF_CHARS`). */
const MAX_CHARS = 32000;

let nextDraftRequest = 1_000_000;

/** Why a handoff is suggested, in plain words. */
export function suggestionText(reason: HandoffSuggestion): string {
  switch (reason) {
    case "compactions":
      return "This session has been summarized more than once, and details get lost each time.";
    case "context":
      return "The context is more than 70 % full.";
    case "tasksDone":
      return "The task list is done.";
  }
}

/** The suggestion card: Gardenwall never starts a handoff by itself. */
export function HandoffCard({
  reason,
  onOpen,
  onDismiss,
}: {
  reason: HandoffSuggestion;
  onOpen: () => void;
  onDismiss: () => void;
}) {
  return (
    <div className="banner info handoff-card" role="status">
      <div>
        <strong>Continue in a fresh session?</strong> {suggestionText(reason)} A handoff note lets a new session pick up
        where this one is, with a clean context. You review it before anything starts.
      </div>
      <div className="row">
        <button className="small primary" onClick={onOpen}>
          Write a handoff…
        </button>
        <button className="small" onClick={onDismiss}>
          Not now
        </button>
      </div>
    </div>
  );
}

/**
 * Review a handoff (trusted UI). The model drafts the note; the user edits
 * it, then starts a new session with it as the first message, or saves it.
 * The facts come from Gardenwall, not the model, and can't be edited.
 */
/** A changed file shown relative to the granted folder it is in (the full path is its tooltip). */
function relativeTo(path: string, folders: string[]): string {
  const lower = path.toLowerCase();
  for (const f of folders) {
    const base = f.replace(/[\\/]+$/, "");
    if (lower.startsWith(base.toLowerCase()) && /[\\/]/.test(path.charAt(base.length))) {
      return path.slice(base.length + 1) || path;
    }
  }
  return path;
}

export function HandoffDialog({
  source,
  focus: initialFocus,
  onClose,
  onStarted,
}: {
  source: HandoffSource;
  /** Extra focus for the draft (`/handoff <focus>`). */
  focus?: string;
  onClose: () => void;
  /** A new session was created: open it and send `started.message`. */
  onStarted: (started: HandoffStarted) => void;
}) {
  const [draft, setDraft] = useState<HandoffDraft | null>(null);
  const [text, setText] = useState("");
  const [focus, setFocus] = useState(initialFocus ?? "");
  const [drafting, setDrafting] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  /** For a chat: a new chat, or an agent session in a project ("" asks for folders). */
  const [target, setTarget] = useState<"chat" | "agent">("chat");
  const [project, setProject] = useState("");
  const request = useRef<number | null>(null);
  const isChat = source.kind === "chat";
  /** The draft being written, so a second one is never asked for at once and Cancel can wait for it to stop. */
  const pending = useRef<Promise<void> | null>(null);
  const [ready, setReady] = useState(false);
  const [closing, setClosing] = useState(false);

  const write = (withFocus: string) => {
    if (pending.current) return pending.current;
    const job = writeOnce(withFocus).finally(() => {
      pending.current = null;
    });
    pending.current = job;
    return job;
  };

  const writeOnce = async (withFocus: string) => {
    setDrafting(true);
    setReady(false);
    setError(null);
    setSaved(null);
    let src = source;
    if (source.kind === "chat") {
      request.current = nextDraftRequest++;
      src = { ...source, request: request.current };
    }
    try {
      const d = await api.handoffDraft(src, withFocus.trim() || null);
      setDraft(d);
      setText(d.text);
      setError(null);
      setReady(true);
    } catch (e) {
      if (!closingRef.current) setError(errorText(e));
    } finally {
      request.current = null;
      setDrafting(false);
    }
  };

  useEffect(() => {
    void write(initialFocus ?? "");
    if (isChat) {
      api
        .listProjects()
        .then((p) => {
          setProjects(p);
          if (p[0]) setProject(p[0].id);
        })
        .catch(() => {});
    }
    // Drafted once when opened; "Write again" drafts again.
  }, []);

  const stop = () => {
    if (source.kind === "agent") void api.agentStop(source.sid);
    else if (request.current !== null) void api.cancelChat(request.current);
  };
  // "Draft ready" shows for a few seconds after a draft arrives.
  useEffect(() => {
    if (!ready) return;
    const t = window.setTimeout(() => setReady(false), 4000);
    return () => window.clearTimeout(t);
  }, [ready]);
  const closingRef = useRef(false);
  /** Close; a draft still being written is stopped first, so the session is free again (Continue works at once). */
  const close = async () => {
    if (pending.current) {
      closingRef.current = true;
      setClosing(true);
      stop();
      await pending.current.catch(() => {});
    }
    onClose();
  };

  const run = async (f: () => Promise<void>) => {
    setWorking(true);
    setError(null);
    setSaved(null);
    try {
      await f();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setWorking(false);
    }
  };

  const start = () =>
    run(async () => {
      const to: HandoffTarget =
        source.kind === "agent" || target === "agent"
          ? { kind: "agent", project: source.kind === "agent" ? null : project || null }
          : { kind: "chat" };
      onStarted(await api.handoffStart(source, to, text));
    });

  const tooLong = text.length > MAX_CHARS;
  const facts = draft?.facts ?? null;
  const busy = drafting || working;

  return toBody(
    <div className="approval-backdrop" role="presentation">
      <div className="handoff" role="dialog" aria-modal="true" aria-labelledby="handoff-title">
        <h2 id="handoff-title">Hand off to a new session</h2>
        <p className="small muted">
          {draft ? `From “${draft.title}”. ` : ""}The model drafted this note from the {isChat ? "chat" : "session"}. Check
          and edit it: the new session starts with it as its first message, and this one stays as it is, linked as its
          parent.
        </p>
        {drafting ? (
          <div className="handoff-drafting" role="status">
            <Sprig />
            <span>{closing ? "Stopping the draft…" : "Writing the handoff note from the session…"}</span>
            <button className="small" onClick={stop} disabled={closing}>
              Stop
            </button>
          </div>
        ) : (
          <>
            <textarea
              className="handoff-text"
              rows={16}
              value={text}
              aria-label="Handoff note"
              placeholder="Goal, what was done, decisions, next steps, open questions, checks"
              onChange={(e) => setText(e.target.value)}
            />
            <div className="handoff-count">
              <span className={`small ${tooLong ? "danger-text" : "muted"}`}>
                {text.length.toLocaleString()} / {MAX_CHARS.toLocaleString()} characters
              </span>
              {ready && (
                <span className="small ok-text handoff-ready" role="status">
                  <Check size={14} aria-hidden="true" /> Draft ready: check and edit it
                </span>
              )}
            </div>
          </>
        )}
        {draft && draft.redacted > 0 && (
          <div className="banner warn small">
            {draft.redacted === 1 ? "A value" : `${draft.redacted} values`} from protected files this session read{" "}
            {draft.redacted === 1 ? "was" : "were"} removed from the draft. Secrets are never carried into a handoff.
          </div>
        )}
        {facts && (
          <details className="handoff-facts" open>
            <summary className="small">
              <strong>Added by Gardenwall</strong> (from its own records, not the model)
            </summary>
            <dl className="approval-facts">
              {facts.project && (
                <>
                  <dt>Project</dt>
                  <dd>{facts.project}</dd>
                </>
              )}
              <dt>Folders</dt>
              <dd className="mono">{shownPaths(facts.folders, "\n")}</dd>
              {facts.changedFiles && (
                <>
                  <dt>Changed files</dt>
                  <dd className="mono">
                    {facts.changedFiles.length === 0 ? (
                      "None recorded by the file tools"
                    ) : (
                      <ul className="handoff-files">
                        {facts.changedFiles.slice(0, 200).map((f) => (
                          <li key={f} title={f}>
                            {relativeTo(f, facts.folders)}
                          </li>
                        ))}
                      </ul>
                    )}
                  </dd>
                </>
              )}
            </dl>
          </details>
        )}
        <div className="handoff-focus">
          <input
            value={focus}
            placeholder="Focus for a new draft (optional), e.g. the open bug"
            aria-label="Focus for a new draft"
            maxLength={500}
            onChange={(e) => setFocus(e.target.value)}
          />
          <button className="small" disabled={busy} onClick={() => void write(focus)}>
            Write again
          </button>
        </div>
        {isChat && (
          <fieldset className="handoff-target">
            <legend className="small">Continue in</legend>
            <label className="small">
              <input type="radio" checked={target === "chat"} onChange={() => setTarget("chat")} /> A new chat
            </label>
            <label className="small">
              <input type="radio" checked={target === "agent"} onChange={() => setTarget("agent")} /> An agent session in{" "}
              <select
                value={project}
                disabled={target !== "agent"}
                aria-label="Project for the agent session"
                onChange={(e) => setProject(e.target.value)}
              >
                {projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
                <option value="">New project (choose folders)…</option>
              </select>
            </label>
          </fieldset>
        )}
        {saved && <div className="banner info small">{saved}</div>}
        {error && <div className="banner error">{error}</div>}
        <div className="approval-actions handoff-actions">
          <button onClick={() => void close()} disabled={working || closing}>
            Cancel
          </button>
          <button
            disabled={busy || !text.trim()}
            title="Copy the note to the clipboard"
            onClick={() => void navigator.clipboard.writeText(text).then(() => setSaved("Copied."))}
          >
            Copy
          </button>
          <button
            disabled={busy || !text.trim() || tooLong}
            title="Save the handoff as a Markdown file where you choose"
            onClick={() =>
              void run(async () => {
                const path = await api.handoffSaveAs(source, text);
                if (path) setSaved(`Saved ${path}`);
              })
            }
          >
            Save as…
          </button>
          {source.kind === "agent" && (
            <button
              disabled={busy || !text.trim() || tooLong}
              title="Save it as HANDOFF.md in the project. Later sessions may read it, so Gardenwall asks for approval first."
              onClick={() => void run(async () => setSaved(await api.handoffSaveToProject(source.sid, text)))}
            >
              Save to project…
            </button>
          )}
          <button className="primary" disabled={busy || !text.trim() || tooLong} onClick={() => void start()}>
            {working ? "Starting…" : "Start new session"}
          </button>
        </div>
      </div>
    </div>
  );
}
