// Saving memories from a conversation (the user
// picks what to keep; no model call, so it costs nothing at generation time).
//
// - <SelectionRemember> shows a small "Remember" button next to text the
//   user selects inside a conversation. It only listens for mouse-up and key
//   events inside that conversation and reads the selection then.
// - <RememberForm> is the editable note that is saved. The core checks it
//   (hidden characters made visible, secrets refused) and it applies from
//   the next chat or session, so the open conversation stays fast.
import { RefObject, useEffect, useState } from "react";
import { api, errorText, MemoryScope } from "./api";

/** Longest memory the store keeps (characters). */
export const MAX_MEMORY_CHARS = 1000;

/** Text to start a memory from: whitespace tidied, cut to the limit. */
export function memoryDraft(text: string): string {
  const tidy = text.replace(/[ \t]+/g, " ").replace(/\n{3,}/g, "\n\n").trim();
  return tidy.length > MAX_MEMORY_CHARS ? `${tidy.slice(0, MAX_MEMORY_CHARS - 1).trimEnd()}…` : tidy;
}

/** A floating "Remember" button for text selected inside `container`. */
export function SelectionRemember({
  container,
  onPick,
  onAsk,
}: {
  container: RefObject<HTMLElement | null>;
  onPick: (text: string) => void;
  /** Also offer "Ask": a side chat about the selected text. */
  onAsk?: (text: string) => void;
}) {
  const [at, setAt] = useState<{ x: number; y: number; text: string } | null>(null);

  useEffect(() => {
    const root = container.current;
    if (!root) return;
    const check = () => {
      const sel = window.getSelection();
      const text = sel?.toString().trim() ?? "";
      if (!sel || sel.rangeCount === 0 || text.length < 3) {
        setAt(null);
        return;
      }
      const range = sel.getRangeAt(0);
      // Only selections inside this conversation (not the composer).
      if (!root.contains(range.commonAncestorContainer)) {
        setAt(null);
        return;
      }
      const r = range.getBoundingClientRect();
      setAt({ x: Math.min(r.right, window.innerWidth - 120), y: Math.max(r.top - 34, 8), text });
    };
    // Read the selection only when the user finishes making it.
    const later = () => window.setTimeout(check, 0);
    const clear = () => setAt(null);
    root.addEventListener("mouseup", later);
    root.addEventListener("keyup", later);
    root.addEventListener("scroll", clear, { passive: true });
    return () => {
      root.removeEventListener("mouseup", later);
      root.removeEventListener("keyup", later);
      root.removeEventListener("scroll", clear);
    };
  }, [container]);

  if (!at) return null;
  return (
    <span
      className="selection-remember"
      style={{ left: onAsk ? Math.max(at.x - 50, 8) : at.x, top: at.y }}
      // Keep the selection while clicking.
      onMouseDown={(e) => e.preventDefault()}
    >
      <button
        className="small"
        onClick={() => {
          onPick(memoryDraft(at.text));
          window.getSelection()?.removeAllRanges();
          setAt(null);
        }}
        title="Save the selected text as a memory (you can edit it first)"
      >
        Remember
      </button>
      {onAsk && (
        <button
          className="small"
          onClick={() => {
            onAsk(at.text.slice(0, 4000));
            window.getSelection()?.removeAllRanges();
            setAt(null);
          }}
          title="Ask about the selected text in a side chat, apart from this conversation"
        >
          Ask
        </button>
      )}
    </span>
  );
}

export interface ScopeChoice {
  label: string;
  scope: MemoryScope;
}

/** The note being saved: editable, with its scope. */
export function RememberForm({
  initial,
  scopes,
  onDone,
}: {
  initial: string;
  /** The first is the default. */
  scopes: ScopeChoice[];
  onDone: (saved: boolean) => void;
}) {
  const [text, setText] = useState(initial);
  const [pick, setPick] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  useEffect(() => setText(initial), [initial]);

  const save = async () => {
    setSaving(true);
    try {
      await api.addMemory(scopes[pick]!.scope, text);
      onDone(true);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="remember-form composer-attachments">
      <label className="small">
        <strong>Remember</strong>{" "}
        <span className="muted">
          Keep only the part worth knowing next time. Used from your next chat or session.
        </span>
        <textarea
          rows={3}
          maxLength={MAX_MEMORY_CHARS}
          value={text}
          aria-label="Memory to save"
          autoFocus
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") onDone(false);
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && text.trim()) void save();
          }}
        />
      </label>
      {error && <div className="banner error small">{error}</div>}
      <div className="row">
        {scopes.length > 1 && (
          <select value={pick} onChange={(e) => setPick(Number(e.target.value))} aria-label="Who the memory is for">
            {scopes.map((s, i) => (
              <option key={s.label} value={i}>
                {s.label}
              </option>
            ))}
          </select>
        )}
        <button className="primary small" disabled={!text.trim() || saving} onClick={() => void save()}>
          Save memory
        </button>
        <button className="small" onClick={() => onDone(false)}>
          Cancel
        </button>
        <span className="small muted">
          {text.length}/{MAX_MEMORY_CHARS}
        </span>
      </div>
    </div>
  );
}
