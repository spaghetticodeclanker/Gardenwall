// The Memory page: everything
// the assistant is told about you, in one place to review, search, edit,
// switch and clean up. Nothing here calls the model.
//
// Layout (docs/internal/design/REDESIGN-GAPS.md, round 1): a budget line, an amber
// review card while suggestions wait, Add and Search rows, then grouped rows
// by scope with each note cut to two lines.
import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, MoreHorizontal, Pencil, Search, Trash2 } from "lucide-react";
import { Button, Fold, Menu, Pill, Toggle, useContextMenu } from "./ui";
import { api, errorText, Memory, MemoriesView, MemoryScope } from "./api";
import { ProsCons } from "./ModelSettings";
import { MAX_MEMORY_CHARS } from "./Remember";
import { shownPath } from "./paths";

function scopeKey(s: MemoryScope): string {
  return s.kind === "project" ? `project:${s.folder}` : "personal";
}

function when(secs: number): string {
  return secs > 0 ? new Date(secs * 1000).toLocaleDateString() : "";
}

const isLong = (text: string) => text.length > 180 || text.split("\n").length > 2;

/** A suggestion from the agent, in the review card: edit, accept or reject. */
function PendingRow({ m, onChange, onError }: { m: Memory; onChange: (v: MemoriesView) => void; onError: (e: string | null) => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(m.text);
  useEffect(() => setText(m.text), [m.text]);
  const run = async (f: () => Promise<MemoriesView>) => {
    try {
      onChange(await f());
      onError(null);
    } catch (e) {
      onError(errorText(e));
    }
  };
  const scope = m.scope.kind === "project" ? `Project: ${shownPath(m.scope.folder)}` : "Personal";
  return (
    <div className="gw-mem-pending">
      <div className="gw-mem-main">
        {editing ? (
          <textarea
            className="gw-mem-edit"
            rows={2}
            maxLength={MAX_MEMORY_CHARS}
            value={text}
            aria-label="Memory text"
            onChange={(e) => setText(e.target.value)}
          />
        ) : (
          <div className="gw-mem-text">{m.text}</div>
        )}
        <div className="gw-row-help">
          Suggested by the agent{when(m.createdAt) ? ` · ${when(m.createdAt)}` : ""} · {scope}
        </div>
      </div>
      <div className="gw-mem-actions">
        {!editing && (
          <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>
            Edit
          </Button>
        )}
        <Button size="sm" onClick={() => void run(() => api.deleteMemory(m.id))}>
          Reject
        </Button>
        <Button size="sm" variant="primary" disabled={!text.trim()} onClick={() => void run(() => api.updateMemory(m.id, text, true, true))}>
          Accept
        </Button>
      </div>
    </div>
  );
}

function MemoryRow({ m, overBudget, selecting, selected, onSelect, onChange, onError, onMenu }: {
  m: Memory;
  overBudget: boolean;
  selecting: boolean;
  selected: boolean;
  onSelect: (on: boolean) => void;
  onChange: (v: MemoriesView) => void;
  onError: (e: string | null) => void;
  onMenu: (e: React.MouseEvent | React.KeyboardEvent, items: Parameters<ReturnType<typeof useContextMenu>["open"]>[2]) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(m.text);
  /** Long memories show two lines until opened. */
  const [expanded, setExpanded] = useState(false);
  const long = isLong(m.text);
  useEffect(() => setText(m.text), [m.text]);
  const save = async (inChat: boolean, inAgent: boolean, newText = text) => {
    try {
      onChange(await api.updateMemory(m.id, newText, inChat, inAgent));
      onError(null);
      setEditing(false);
    } catch (e) {
      onError(errorText(e));
    }
  };
  const remove = async () => {
    if (!window.confirm("Delete this memory?")) return;
    try {
      onChange(await api.deleteMemory(m.id));
      onError(null);
    } catch (e) {
      onError(errorText(e));
    }
  };
  const items = [
    { label: "Edit", icon: Pencil, onSelect: () => setEditing(true) },
    "separator" as const,
    { label: "Delete…", icon: Trash2, danger: true, onSelect: () => void remove() },
  ];
  const origin = m.source === "model" ? "Suggested by the agent" : "Saved by you";
  const date = when(m.updatedAt || m.createdAt);
  return (
    <div
      className={`gw-mem-row${overBudget ? " over" : ""}${selected ? " selected" : ""}`}
      onContextMenu={(e) => !editing && onMenu(e, items)}
    >
      {selecting && (
        <input type="checkbox" className="gw-mem-check" checked={selected} onChange={(e) => onSelect(e.target.checked)} aria-label="Select this memory" />
      )}
      <div className="gw-mem-main">
        {editing ? (
          <>
            <textarea
              className="gw-mem-edit"
              rows={Math.min(8, Math.max(2, Math.ceil(text.length / 90)))}
              maxLength={MAX_MEMORY_CHARS}
              value={text}
              aria-label="Memory text"
              autoFocus
              onChange={(e) => setText(e.target.value)}
            />
            <div className="gw-mem-edit-actions">
              <Button size="sm" variant="primary" disabled={!text.trim()} onClick={() => void save(m.inChat, m.inAgent)}>
                Save
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setText(m.text);
                  setEditing(false);
                }}
              >
                Cancel
              </Button>
            </div>
          </>
        ) : (
          <div className={`gw-mem-text${long && !expanded ? " clamped" : ""}`}>{m.text}</div>
        )}
        <div className="gw-row-help gw-mem-meta">
          {long && !editing && (
            <>
              <button type="button" className="link small" onClick={() => setExpanded((x) => !x)}>
                {expanded ? "Less" : "More"}
              </button>
              {" · "}
            </>
          )}
          {origin}
          {date ? ` · ${date}` : ""}
          {overBudget && (
            <>
              {" "}
              <span title="The memory budget is full; newer memories are used first">
                <Pill tone="warn">Not used in chats (budget full)</Pill>
              </span>
            </>
          )}
        </div>
      </div>
      <div className="gw-mem-switches">
        <span className="gw-mem-switch">
          <span aria-hidden="true">Chat</span>
          <Toggle checked={m.inChat} label="Add this memory to chats" onChange={(on) => void save(on, m.inAgent, m.text)} />
        </span>
        <span className="gw-mem-switch">
          <span aria-hidden="true">Agent</span>
          <Toggle checked={m.inAgent} label="Add this memory to agent sessions" onChange={(on) => void save(m.inChat, on, m.text)} />
        </span>
        <Menu
          label="Memory actions"
          items={items}
          trigger={(p) => (
            <button type="button" className="gw-icon-btn" aria-label="More actions" title="More actions" {...p}>
              <MoreHorizontal aria-hidden="true" size={16} strokeWidth={1.75} />
            </button>
          )}
        />
      </div>
    </div>
  );
}

export function MemorySettings() {
  const [view, setView] = useState<MemoriesView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [scope, setScope] = useState("personal");
  const [picked, setPicked] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const context = useContextMenu();

  useEffect(() => {
    api.listMemories().then(setView).catch((e) => setError(errorText(e)));
  }, []);

  const changed = (v: MemoriesView) => {
    setView(v);
    // Forget selections of memories that are gone.
    setSelected((s) => new Set([...s].filter((id) => v.memories.some((m) => m.id === id))));
  };

  const memories = view?.memories ?? [];
  const over = useMemo(() => new Set(view?.chatOverBudget ?? []), [view]);
  const pending = memories.filter((m) => m.pending);
  const folders = [
    ...new Set([...memories.flatMap((m) => (m.scope.kind === "project" ? [m.scope.folder] : [])), ...picked]),
  ];
  const q = query.trim().toLowerCase();
  const shown = memories.filter(
    (m) =>
      !m.pending &&
      (!q || m.text.toLowerCase().includes(q)) &&
      (filter === "all" ||
        (filter === "off" && !m.inChat && !m.inAgent) ||
        (filter === "over" && over.has(m.id)) ||
        (filter === "agent" && m.source === "model") ||
        scopeKey(m.scope) === filter),
  );
  const groups = new Map<string, Memory[]>();
  for (const m of shown) {
    const k = scopeKey(m.scope);
    groups.set(k, [...(groups.get(k) ?? []), m]);
  }
  const active = memories.length - pending.length;
  const used = view ? Math.min(100, Math.round((view.chatChars / view.budget) * 100)) : 0;

  const chooseScope = async (value: string) => {
    if (value !== "pick") {
      setScope(value);
      return;
    }
    try {
      const folder = await api.pickMemoryFolder();
      if (folder) {
        setPicked((p) => [...p, folder]);
        setScope(`project:${folder}`);
      }
    } catch (e) {
      setError(errorText(e));
    }
  };

  const add = async () => {
    const s: MemoryScope = scope.startsWith("project:") ? { kind: "project", folder: scope.slice(8) } : { kind: "personal" };
    try {
      changed(await api.addMemory(s, text));
      setText("");
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const deleteSelected = async () => {
    if (!window.confirm(`Delete ${selected.size} selected memor${selected.size === 1 ? "y" : "ies"}?`)) return;
    try {
      changed(await api.deleteMemories([...selected]));
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const stopSelecting = () => {
    setSelecting(false);
    setSelected(new Set());
  };

  return (
    <section className="gw-memory">
      <Fold summary="Notes the assistant keeps about you and your projects, stored encrypted on this computer.">
        <p>
          Save one by selecting text in a chat or agent session and choosing <strong>Remember</strong>; the agent can only
          suggest them. They are never sent anywhere. A new memory is used from your next chat or session; edits,
          switches and deletions apply at once.
        </p>
        <p>
          Personal memories (your preferences, how you like answers) go into every chat and agent session. Project
          memories belong to an agent folder and are used only when that folder is granted. Each memory has its own Chat
          and Agent switch, and any chat or agent session can turn memories off.
        </p>
        <ProsCons
          pros={[
            "The assistant remembers your preferences and project details between conversations.",
            "You see and control exactly what it's told: every memory is listed here.",
          ]}
          cons={[
            "Memories use part of the context window in every conversation (the budget below).",
            "An outdated memory can steer answers the wrong way; edit or switch it off when things change.",
            "Text that looks like a password or key is refused: memories go into every conversation.",
          ]}
        />
      </Fold>
      {error && <div className="banner error">{error}</div>}

      {view && (
        <div className="gw-mem-budget">
          <div
            className={`gw-mem-meter${used >= 90 ? " full" : ""}`}
            role="meter"
            aria-label="Memory budget used in chats"
            aria-valuemin={0}
            aria-valuemax={view.budget}
            aria-valuenow={view.chatChars}
            title={`${view.chatChars.toLocaleString()} of ${view.budget.toLocaleString()} characters`}
          >
            <span style={{ width: `${used}%` }} />
          </div>
          <span className="small muted">
            {active} memor{active === 1 ? "y" : "ies"} · chats receive {view.chatChars.toLocaleString()} of{" "}
            {view.budget.toLocaleString()} characters
            {over.size > 0 ? ` · ${over.size} left out because the budget is full (newest first)` : ""}
          </span>
        </div>
      )}

      {pending.length > 0 && (
        <div className="gw-mem-review">
          <div className="gw-mem-review-title">
            <AlertTriangle aria-hidden="true" size={16} />
            {pending.length === 1 ? "1 suggestion waiting for your review" : `${pending.length} suggestions waiting for your review`}
          </div>
          {pending.map((m) => (
            <PendingRow key={m.id} m={m} onChange={changed} onError={setError} />
          ))}
        </div>
      )}

      <div className="gw-mem-bar">
        <input
          placeholder="Add a memory: I prefer short answers with code examples"
          maxLength={MAX_MEMORY_CHARS}
          value={text}
          aria-label="New memory"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && text.trim()) void add();
          }}
        />
        <select value={scope} onChange={(e) => void chooseScope(e.target.value)} aria-label="Memory scope">
          <option value="personal">Personal</option>
          {folders.map((f) => (
            <option key={f} value={`project:${f}`}>
              Project: {shownPath(f)}
            </option>
          ))}
          <option value="pick">Another project folder…</option>
        </select>
        <Button disabled={!text.trim()} onClick={() => void add()}>
          Add
        </Button>
      </div>
      <p className="gw-row-help gw-mem-note">
        A project memory is used only in agent sessions on that folder. Choosing a folder here gives nothing access to it.
      </p>

      {active > 0 && (
        <div className="gw-mem-bar">
          <span className="gw-mem-search">
            <Search aria-hidden="true" size={14} />
            <input type="search" placeholder="Search memories" value={query} aria-label="Search memories" onChange={(e) => setQuery(e.target.value)} />
          </span>
          <select value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Show">
            <option value="all">All</option>
            <option value="personal">Personal</option>
            {folders.map((f) => (
              <option key={f} value={`project:${f}`}>
                Project: {shownPath(f)}
              </option>
            ))}
            <option value="agent">Suggested by the agent</option>
            <option value="off">Switched off</option>
            <option value="over">Left out (budget full)</option>
          </select>
          {selecting ? (
            <Button variant="ghost" onClick={stopSelecting}>
              Done
            </Button>
          ) : (
            <Button variant="ghost" onClick={() => setSelecting(true)}>
              Select
            </Button>
          )}
        </div>
      )}

      {selecting && (
        <div className="gw-mem-selectbar">
          <span className="small">{selected.size} selected</span>
          <Button size="sm" variant="ghost" disabled={shown.length === 0} onClick={() => setSelected(new Set(shown.map((m) => m.id)))}>
            Select shown
          </Button>
          {selected.size > 0 && (
            <>
              <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
                Clear selection
              </Button>
              <Button size="sm" variant="danger" icon={Trash2} onClick={() => void deleteSelected()}>
                Delete selected ({selected.size})
              </Button>
            </>
          )}
        </div>
      )}

      {active === 0 && pending.length === 0 && <p className="muted">No memories yet.</p>}
      {active > 0 && shown.length === 0 && <p className="muted">No memories match.</p>}
      {[...groups.entries()].map(([k, list]) => (
        <section key={k} className="gw-group">
          <h3 className="gw-group-title">{k === "personal" ? "Personal" : `Project: ${shownPath(k.slice(8))}`}</h3>
          <div className="gw-group-body">
            {list.map((m) => (
              <MemoryRow
                key={m.id}
                m={m}
                overBudget={over.has(m.id)}
                selecting={selecting}
                selected={selected.has(m.id)}
                onSelect={(on) =>
                  setSelected((s) => {
                    const next = new Set(s);
                    if (on) next.add(m.id);
                    else next.delete(m.id);
                    return next;
                  })
                }
                onChange={changed}
                onError={setError}
                onMenu={(e, items) => context.open(e, "Memory", items)}
              />
            ))}
          </div>
        </section>
      ))}
      {context.node}
    </section>
  );
}
