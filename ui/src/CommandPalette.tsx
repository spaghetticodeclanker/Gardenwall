import { useEffect, useRef, useState } from "react";
import { api, errorText, type HistoryHits } from "./api";
import { asksForShortcuts, SHORTCUTS } from "./shortcuts";

export type PaletteTarget =
  | { kind: "page"; page: string }
  | { kind: "chat"; id: string }
  | { kind: "session"; id: string }
  | { kind: "action"; action: "diagnostics" | "lockdown" };

/** Things Ctrl+K can do besides going to a page, found by any of their words. */
const ACTIONS: { action: "diagnostics" | "lockdown"; label: string; detail: string; words: string[] }[] = [
  {
    action: "lockdown",
    label: "Lockdown",
    detail: "Emergency stop, or turn it off",
    words: ["lockdown", "lock down", "emergency", "stop everything", "panic", "kill switch", "unlock"],
  },
  {
    action: "diagnostics",
    label: "Copy diagnostics",
    detail: "Report a problem",
    words: ["diagnostics", "report a problem", "problem", "bug", "crash", "logs"],
  },
];

interface Row {
  key: string;
  label: string;
  detail: string;
  target: PaletteTarget;
}

/**
 * Ctrl+K: jump to a page, or find a chat or agent session by what was said
 * in it. The search runs in the core over the decrypted text in memory;
 * results are titles and short snippets of your own history, shown as
 * plain text. Typing "?" or "keys" lists every keyboard shortcut.
 */
export function CommandPalette({ pages, onPick, onClose }: { pages: string[]; onPick: (t: PaletteTarget) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<HistoryHits>({ chats: [], sessions: [] });
  const [error, setError] = useState<string | null>(null);
  const [at, setAt] = useState(0);
  const input = useRef<HTMLInputElement | null>(null);

  useEffect(() => input.current?.focus(), []);
  useEffect(() => {
    const text = query.trim();
    if (text.length < 2) {
      setHits({ chats: [], sessions: [] });
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      api
        .searchHistory(text)
        .then((h) => {
          if (!cancelled) {
            setHits(h);
            setError(null);
          }
        })
        .catch((e) => !cancelled && setError(errorText(e)));
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query]);

  const needle = query.trim().toLowerCase();
  const shortcuts = asksForShortcuts(query);
  const rows: Row[] = [
    ...pages
      .filter((p) => p.includes(needle))
      .map((p) => ({ key: `page:${p}`, label: p[0]!.toUpperCase() + p.slice(1), detail: "Page", target: { kind: "page", page: p } as PaletteTarget })),
    ...ACTIONS.filter((a) => needle !== "" && (a.label.toLowerCase().includes(needle) || a.words.some((w) => w.includes(needle)))).map((a) => ({
      key: `action:${a.action}`,
      label: a.label,
      detail: a.detail,
      target: { kind: "action", action: a.action } as PaletteTarget,
    })),
    ...hits.chats.map((h) => ({ key: `chat:${h.id}`, label: h.title, detail: `Chat · ${h.snippet}`, target: { kind: "chat", id: h.id } as PaletteTarget })),
    ...hits.sessions.map((h) => ({ key: `session:${h.id}`, label: h.title, detail: `Agent session · ${h.snippet}`, target: { kind: "session", id: h.id } as PaletteTarget })),
  ];
  const selected = Math.min(at, Math.max(rows.length - 1, 0));

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div className="palette" role="dialog" aria-label="Go to or search" onMouseDown={(e) => e.stopPropagation()}>
        <input
          ref={input}
          type="text"
          value={query}
          maxLength={200}
          placeholder="Go to a page, or search your chats and agent sessions"
          onChange={(e) => {
            setQuery(e.target.value);
            setAt(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
            else if (e.key === "ArrowDown") {
              e.preventDefault();
              setAt(Math.min(selected + 1, rows.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setAt(Math.max(selected - 1, 0));
            } else if (e.key === "Enter" && rows[selected]) onPick(rows[selected]!.target);
          }}
        />
        {error && <div className="banner error">{error}</div>}
        {shortcuts && (
          <dl className="palette-keys" aria-label="Keyboard shortcuts">
            {SHORTCUTS.map((s) => (
              <div key={s.keys} className="palette-key">
                <dt>{s.typed ? <code>{s.keys}</code> : <kbd>{s.keys}</kbd>}</dt>
                <dd className="small muted">{s.text}</dd>
              </div>
            ))}
          </dl>
        )}
        <ul className="plain-list palette-rows">
          {rows.map((r, i) => (
            <li key={r.key}>
              <button className={i === selected ? "palette-row active" : "palette-row"} onMouseEnter={() => setAt(i)} onClick={() => onPick(r.target)}>
                <span>{r.label}</span>
                <span className="small muted">{r.detail}</span>
              </button>
            </li>
          ))}
          {rows.length === 0 && needle.length >= 2 && !shortcuts && <li className="small muted palette-empty">Nothing found.</li>}
        </ul>
      </div>
    </div>
  );
}
