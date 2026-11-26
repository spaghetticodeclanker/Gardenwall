import { memoryDraft, RememberForm, SelectionRemember } from "./Remember";
import { type Insert, useInsert } from "./useInsert";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type AppView,
  Conversation,
  CollectionSummary,
  ConversationSummary,
  errorText,
  LoadedView,
  MessageStats,
  GpuWarning,
  StoredMessage,
  draftText,
  type ChatEvent,
  type HandoffStarted,
  type WebMode,
} from "./api";
import { ModeBadge, ModeChoice, VisitedSites } from "./WebResearch";
import { HandoffCard, HandoffDialog } from "./HandoffDialog";
import { QuickSwitch } from "./QuickSwitch";
import { useStickyScroll } from "./useStickyScroll";
import { useDraft } from "./useDraft";
import { MessageTime } from "./MessageTime";
import { Sprig } from "./art/Sprig";
import { useAutoGrow } from "./useAutoGrow";
import { Markdown } from "./Markdown";
import { BookmarkPlus, FileText, GitBranch, Globe, Lightbulb, MessageSquare, MoreHorizontal, Paperclip, Pencil, Plus, RefreshCw, ScrollText, Share2, Download, PanelRightClose, PanelRightOpen, History, Trash2 } from "lucide-react";
import { Button, CopyButton, EmptyState, IconButton, Inspector, Menu, Pill, SegmentedBar, Toggle, useContextMenu, useCornerFloor } from "./ui";
import { Growth } from "./art/Growth";
import "./chat.css";
import { SlowdownBanner } from "./SlowdownBanner";
import { ReadAloudButton } from "./ReadAloud";
import { MicButton } from "./MicButton";
import { readIfAuto } from "./speech";
import { AttachmentStrip, useAttachments } from "./useAttachments";

const ATTACHED_LABEL: Record<string, string> = {
  text: "Attached",
  document: "Text of",
  program: "Analysis of",
  data: "Strings of",
};

/** Split a user message into the typed text and the attached files the
 * core added (`<attached-file>`, `<web-content>` from `/web` and `/read`,
 * and `<file-analysis>` from older chats). */
export function splitReports(content: string): {
  text: string;
  reports: { name: string; body: string; label: string }[];
} {
  const reports: { name: string; body: string; label: string }[] = [];
  const re =
    /\n\n<(file-analysis|attached-file) name="([^"]*)"(?: type="([a-z]*)")?>\n([\s\S]*?)\n<\/(?:file-analysis|attached-file)>/g;
  const files = content.replace(re, (_m, tag: string, name: string, type: string | undefined, body: string) => {
    const label = tag === "file-analysis" ? "Analysis of" : (ATTACHED_LABEL[type ?? ""] ?? "Attached");
    reports.push({ name, body, label });
    return "";
  });
  const web = /\n\n<web-content kind="([a-z ]*)" source="([^"]*)">\n([\s\S]*?)\n<\/web-content>/g;
  const text = files.replace(web, (_m, kind: string, source: string, body: string) => {
    reports.push({ name: source, body, label: kind === "page" ? "Web page" : "Web search via" });
    return "";
  });
  return { text, reports };
}

export function UserText({ content }: { content: string }) {
  const { text, reports } = splitReports(content);
  return (
    <>
      <div className="text">{text}</div>
      {reports.map((r, i) => (
        <details key={i} className="file-analysis small">
          <summary>
            {r.label} {r.name}
          </summary>
          <pre>{r.body}</pre>
        </details>
      ))}
    </>
  );
}

let nextRequestId = 1;

/** Context-window tracker (ADR-014 §2.4). */
/** This chat's memories switch (each chat can turn memories off). */
/** Below this, a few answers from a thinking model fill the context. */
const SMALL_CONTEXT = 16384;

/** The chat's context bar: a hairline with the figures, notes only when they matter. */
function ContextMeter({ stats, window }: { stats: MessageStats | null; window: number }) {
  const used = stats?.totalTokens ?? null;
  const pct = used === null ? 0 : Math.min(100, (used / window) * 100);
  const level = pct >= 95 ? "danger" : pct >= 80 ? "warn" : "ok";
  return (
    <div className="gw-context">
      <div className="gw-context-bar static">
        <SegmentedBar label="Context" total={window} height={4} segments={used === null ? [] : [{ label: "Used", value: used, color: level === "ok" ? "--accent" : level === "warn" ? "--warn" : "--danger" }]} />
        <span className={`gw-context-text ${level}`}>
          {used === null ? "—" : used.toLocaleString()} / {window.toLocaleString()}
          {used !== null && ` (${pct.toFixed(0)}%)`}
        </span>
        {stats && stats.generationPerSecond != null && (
          <span className="gw-context-speed">
            {stats.generationPerSecond.toFixed(0)} tok/s
            {stats.ttftMs != null && ` · first token ${stats.ttftMs} ms`}
            {draftText(stats.draftTokens, stats.draftAccepted) && ` · ${draftText(stats.draftTokens, stats.draftAccepted)}`}
          </span>
        )}
      </div>
      {level !== "ok" && (
        <div className={`meter-note ${level}`}>
          {level === "danger"
            ? "The context window is almost full. Start a new chat to keep answers accurate."
            : "The context window is filling up. Consider starting a new chat soon."}
        </div>
      )}
      {window < SMALL_CONTEXT && level === "ok" && (
        <div className="meter-note warn">
          This model was loaded with a small context ({window.toLocaleString()} tokens), so a few answers fill it,
          especially with thinking. For more room, unload it and set a larger context size in the model's Settings
          on the Models page; part of the model then moves to system RAM, which is a little slower.
        </div>
      )}
    </div>
  );
}

function statsLine(s: MessageStats | null): string {
  if (!s) return "";
  const parts: string[] = [];
  if (s.finishReason === "cancelled") parts.push("stopped");
  else if (s.finishReason === "repetition") parts.push("stopped: repeating itself");
  else if (s.finishReason === "length") parts.push("cut off at the length limit");
  if (s.completionTokens != null) parts.push(`${s.completionTokens.toLocaleString()} tokens`);
  if (s.generationPerSecond != null) parts.push(`${s.generationPerSecond.toFixed(0)} tok/s`);
  const draft = draftText(s.draftTokens, s.draftAccepted);
  if (draft) parts.push(draft);
  if (s.model) parts.push(s.model);
  return parts.join(" · ");
}

function MessageView({
  message,
  streaming,
  canRegenerate,
  onRegenerate,
  onRemember,
  onBranch,
  onEdit,
}: {
  message: {
    role: "user" | "assistant";
    content: string;
    reasoning: string;
    stats: MessageStats | null;
    images?: { mime: string; base64: string }[];
    /** Seconds since 1970 (as stored). */
    createdAt?: number;
  };
  streaming: boolean;
  canRegenerate: boolean;
  onRegenerate: () => void;
  /** Offer "Remember…" for this message (opens an editable memory). */
  onRemember?: (text: string) => void;
  /** Offer "Branch": a new chat with the messages up to this one. */
  onBranch?: () => void;
  /** Offer "Edit" on the user's message: send the edited text as a new branch, or replace from here. */
  onEdit?: (text: string, replace: boolean) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  if (message.role === "user") {
    if (editing !== null && onEdit) {
      return (
        <div className="message user editing">
          <textarea
            className="edit-message"
            autoFocus
            rows={Math.min(12, Math.max(3, editing.split("\n").length + 1))}
            value={editing}
            onChange={(e) => setEditing(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setEditing(null);
            }}
          />
          <div className="message-foot small user-foot">
            <button
              className="primary small"
              disabled={!editing.trim()}
              title="Keep this chat as it is and continue the edited version in a new chat (a branch)"
              onClick={() => {
                onEdit(editing.trim(), false);
                setEditing(null);
              }}
            >
              Send as new branch
            </button>
            <button
              className="small"
              disabled={!editing.trim()}
              title="Remove this message and everything after it, then send the edited version here"
              onClick={() => {
                onEdit(editing.trim(), true);
                setEditing(null);
              }}
            >
              Replace from here
            </button>
            <button className="link small" onClick={() => setEditing(null)}>
              Cancel
            </button>
          </div>
        </div>
      );
    }
    // The user's own text and images: shown exactly as given.
    return (
      <div className="message user">
        {message.images && message.images.length > 0 && (
          <div className="attachments">
            {message.images.map((img, i) => (
              <img key={i} className="thumb" alt="Attached image" src={`data:${img.mime};base64,${img.base64}`} />
            ))}
          </div>
        )}
        <UserText content={message.content} />
        {(onRemember || onEdit || onBranch) && !streaming && (
          <div className="gw-msg-actions user-foot">
            <MessageTime at={message.createdAt ? message.createdAt * 1000 : null} />
            {onEdit && (
              <IconButton
                icon={Pencil}
                label="Edit: change this message and send it again (as a new branch, or replacing what came after)"
                onClick={() => setEditing(message.content)}
              />
            )}
            {onBranch && <IconButton icon={GitBranch} label="Branch: start a new chat with the messages up to here; this chat stays as it is" onClick={onBranch} />}
            {onRemember && (
              <IconButton icon={BookmarkPlus} label="Remember: keep something from this message as a memory (you edit it first)" onClick={() => onRemember(message.content)} />
            )}
          </div>
        )}
      </div>
    );
  }
  return (
    <div className={`message assistant${canRegenerate ? " latest" : ""}`}>
      {message.reasoning && (
        <details className="reasoning" open={streaming && !message.content}>
          <summary>
            {streaming && !message.content ? (
              <span className="gw-working">
                <Sprig /> Thinking…
              </span>
            ) : (
              "Reasoning"
            )}
          </summary>
          <div className="text">{message.reasoning}</div>
        </details>
      )}
      {message.content ? (
        <Markdown text={message.content} />
      ) : (
        streaming &&
        !message.reasoning && (
          <div className="muted small gw-working" role="status">
            <Sprig /> Thinking…
          </div>
        )
      )}
      {!streaming && message.stats?.finishReason === "repetition" && (
        <div className="banner warn reply-notice">
          <strong>The model started repeating itself, so Gardenwall stopped it.</strong> This happens most with heavily
          compressed models (such as 2-bit versions) and very long thinking. Try <em>Regenerate</em>, or set{" "}
          <em>Thinking</em> to Medium or Low in the model's Settings on the Models page.
        </div>
      )}
      {!streaming && !message.content && message.stats?.finishReason !== "repetition" &&
        message.stats?.finishReason !== "cancelled" && (
          <div className="banner warn reply-notice">
            <strong>The model finished without writing an answer.</strong>{" "}
            {message.reasoning
              ? "It spent the whole reply thinking (open Reasoning above to see it). Try Regenerate, or choose less thinking in the model's Settings on the Models page."
              : "Try Regenerate. If it keeps happening, check the model's chat-template audit and settings on the Models page."}
          </div>
        )}
      {!streaming && (
        <div className="gw-msg-foot">
          <div className="gw-msg-actions">
            {message.content && <CopyButton text={message.content} label="Copy the reply" />}
            {message.content && message.role === "assistant" && <ReadAloudButton text={message.content} />}
            {onRemember && message.content && (
              <IconButton icon={BookmarkPlus} label="Remember: keep something from this message as a memory (you edit it first)" onClick={() => onRemember(message.content)} />
            )}
            {onBranch && <IconButton icon={GitBranch} label="Branch: start a new chat with the messages up to here; this chat stays as it is" onClick={onBranch} />}
            {canRegenerate && <IconButton icon={RefreshCw} label="Regenerate this reply" onClick={onRegenerate} />}
          </div>
          <MessageTime at={message.createdAt ? message.createdAt * 1000 : null} />
          <span className="gw-msg-stats">{statsLine(message.stats)}</span>
          {message.stats?.sources && message.stats.sources.length > 0 && (
            <span className="gw-msg-stats" title="Passages from your documents quoted to the model for this answer">
              Sources: {message.stats.sources.map((s, i) => `[${i + 1}] ${s.file} (part ${s.part})`).join(" · ")}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/** Chats in list order, each branch (or continuation) right under the chat it came from. */
function nestChats(items: ConversationSummary[]): { chat: ConversationSummary; depth: number }[] {
  const ids = new Set(items.map((c) => c.id));
  const children = new Map<string, ConversationSummary[]>();
  for (const c of items) {
    if (c.parentId && ids.has(c.parentId)) children.set(c.parentId, [...(children.get(c.parentId) ?? []), c]);
  }
  const out: { chat: ConversationSummary; depth: number }[] = [];
  const seen = new Set<string>();
  const walk = (c: ConversationSummary, depth: number) => {
    if (seen.has(c.id)) return;
    seen.add(c.id);
    out.push({ chat: c, depth });
    for (const child of children.get(c.id) ?? []) walk(child, depth + 1);
  };
  for (const c of items) if (!c.parentId || !ids.has(c.parentId)) walk(c, 0);
  for (const c of items) walk(c, 0);
  return out;
}

function ConversationList({
  items,
  activeId,
  busy,
  onSelect,
  onNew,
  onRename,
  onDelete,
  onExport,
}: {
  items: ConversationSummary[];
  activeId: string | null;
  busy: boolean;
  onSelect: (id: string) => void;
  onNew: () => void;
  onRename: (id: string, title: string) => void;
  onDelete: (c: ConversationSummary) => void;
  onExport: (c: ConversationSummary) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const context = useContextMenu();
  const chatMenu = (c: ConversationSummary) => [
    { label: "Open", icon: MessageSquare, disabled: busy && c.id !== activeId, onSelect: () => onSelect(c.id) },
    {
      label: "Rename",
      icon: Pencil,
      onSelect: () => {
        setDraft(c.title);
        setEditing(c.id);
      },
    },
    { label: "Export as Markdown…", icon: Download, onSelect: () => onExport(c) },
    "separator" as const,
    { label: "Delete…", icon: Trash2, danger: true, disabled: busy && c.id === activeId, onSelect: () => onDelete(c) },
  ];
  // Branches and continuations sit under the chat they came from.
  const ordered = nestChats(items);
  const [draft, setDraft] = useState("");
  return (
    <aside className="conversations chat-list">
      {context.node}
      <button className="primary new-chat" onClick={onNew} disabled={busy}>
        New chat
      </button>
      <ul>
        {activeId === null && (
          // The new chat exists once its first message is sent; until then
          // it is shown here so the button visibly did something.
          <li className="active draft-chat" aria-current="true">
            <span className="conversation-title">New chat</span>
            <span className="small muted draft-hint">not saved yet</span>
          </li>
        )}
        {ordered.map(({ chat: c, depth }) => (
          <li
            key={c.id}
            className={[c.id === activeId ? "active" : "", depth > 0 ? "branch-chat" : ""].join(" ").trim() || undefined}
            style={depth > 0 ? { paddingLeft: `${Math.min(depth, 4) * 12}px` } : undefined}
            onContextMenu={(e) => context.open(e, `Chat ${c.title}`, chatMenu(c))}
            onKeyDown={(e) => context.open(e, `Chat ${c.title}`, chatMenu(c))}
          >
            {editing === c.id ? (
              <input
                autoFocus
                value={draft}
                maxLength={200}
                onChange={(e) => setDraft(e.target.value)}
                onBlur={() => {
                  setEditing(null);
                  if (draft.trim() && draft !== c.title) onRename(c.id, draft);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                  if (e.key === "Escape") setEditing(null);
                }}
              />
            ) : (
              <>
                <button
                  className="conversation-title"
                  title={c.title}
                  disabled={busy && c.id !== activeId}
                  onClick={() => onSelect(c.id)}
                  onDoubleClick={() => {
                    setDraft(c.title);
                    setEditing(c.id);
                  }}
                >
                  {depth > 0 && <span className="branch-mark" aria-hidden="true">↳ </span>}
                  {c.title}
                </button>
                {/* Export, Rename and Delete in a ⋯ menu shown on hover and focus (and on right-click),
                    so titles get the full width (final review item 10). */}
                <span className="conversation-actions">
                  {/* The right-click menu: drawn above the list, so its scrolling never cuts it off. */}
                  <button
                    type="button"
                    className="gw-icon-btn"
                    aria-label="More actions: export, rename, delete"
                    aria-haspopup="menu"
                    title="More actions"
                    onClick={(e) => context.open(e, `Chat ${c.title}`, chatMenu(c))}
                  >
                    <MoreHorizontal aria-hidden="true" size={16} strokeWidth={1.75} />
                  </button>
                </span>
              </>
            )}
          </li>
        ))}
      </ul>
      {items.length === 0 && activeId !== null && <p className="small muted pad">No saved chats yet.</p>}
    </aside>
  );
}

interface Draft {
  user: string | null;
  content: string;
  reasoning: string;
  /** With the Web switch on: what the reply did ("Searching: …"). */
  steps: string[];
}

/** `/web <query>` or `/read <address>` typed in the composer. */
function webCommand(text: string): { command: "web" | "read"; argument: string } | null {
  const m = /^\/(web|read)(?:\s+([\s\S]*))?$/i.exec(text.trim());
  if (!m || !m[1]) return null;
  return { command: m[1].toLowerCase() as "web" | "read", argument: (m[2] ?? "").trim() };
}

export function ChatView({ loaded, goToModels, state, refresh, focus, active, onAgentHandoff, openId, onOpened, onAsk, insert }: {
  /** A side chat's answer to add to the message box. */
  insert: Insert | null;
  /** Ask about selected text in a side chat. */
  onAsk: (text: string) => void;
  /** A chat to open (picked in the Ctrl+K palette). */
  openId: string | null;
  onOpened: () => void;
  loaded: LoadedView | null;
  /** This view is on screen (it stays mounted while hidden). */
  active: boolean;
  goToModels: () => void;
  state: AppView;
  refresh: () => Promise<void>;
  /** Focus mode: the chat list is hidden. */
  focus: boolean;
  /** A chat was handed off to a new agent session: show it there. */
  onAgentHandoff: (started: HandoffStarted) => void;
}) {
  const [items, setItems] = useState<ConversationSummary[]>([]);
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [slowdown, setSlowdown] = useState<GpuWarning | null>(null);
  const [collections, setCollections] = useState<CollectionSummary[]>([]);
  const [linked, setLinked] = useState<string | null>(null);
  useEffect(() => {
    api
      .listCollections()
      .then((v) => setCollections(v.collections))
      .catch(() => {});
  }, []);
  const [input, setInput] = useState("");
  useDraft(`chat:${conversation?.id ?? "new"}`, input, setInput);
  useInsert(insert, active, setInput);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const attach = useAttachments({ canSee: loaded?.vision === true, active, onError: setError });
  const [prompt, setPrompt] = useState("");
  const requestRef = useRef<number | null>(null);
  const conversationIdRef = useRef<string | null>(null);
  const [memOn, setMemOn] = useState(true);
  const [remember, setRemember] = useState<string | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  // Corner cards and notices sit above the message box, never over it.
  const composerBox = useRef<HTMLDivElement>(null);
  useCornerFloor(composerBox);
  // One line to start; it grows as you type.
  useAutoGrow(composerRef, input, 1);
  /** The handoff review dialog is open; chats whose suggestion was dismissed. */
  const [handoffOpen, setHandoffOpen] = useState(false);
  const [handoffDismissed, setHandoffDismissed] = useState<Set<string>>(new Set());
  /** This chat's Web switch (null: off), the mode dialog, the visited list. */
  const [webMode, setWebMode] = useState<WebMode | null>(null);
  const [choosingMode, setChoosingMode] = useState<WebMode | null>(null);
  const [showVisits, setShowVisits] = useState(false);
  const [fetching, setFetching] = useState(false);
  const scroll = useStickyScroll([conversation, draft]);

  const refreshList = useCallback(async () => {
    try {
      setItems(await api.listConversations());
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const open = useCallback(async (id: string) => {
    try {
      const c = await api.getConversation(id);
      setMemOn(await api.chatMemories(id).catch(() => true));
      const switching = c.id !== conversationIdRef.current;
      conversationIdRef.current = c.id;
      setConversation(c);
      if (switching) requestAnimationFrame(() => scroll.follow());
      setLinked(await api.chatCollection(id).catch(() => null));
      setWebMode(await api.chatWebState(id).then((w) => (w.on ? w.mode : null)).catch(() => null));
      setPrompt(c.systemPrompt);
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  useEffect(() => {
    if (!openId) return;
    onOpened();
    void open(openId);
  }, [openId, onOpened, open]);

  const run = async (conversationId: string, content: string | null, images: string[] = [], files: string[] = []) => {
    scroll.follow();
    setBusy(true);
    setError(null);
    setDraft({ user: content, content: "", reasoning: "", steps: [] });
    const requestId = nextRequestId++;
    requestRef.current = requestId;
    // Tokens are applied once per frame, in order, rather than one redraw
    // per token; the rest is applied when the request ends.
    let queued: ChatEvent[] = [];
    let frame: number | null = null;
    // The reply's text, for "read new replies automatically".
    let replyText = "";
    const flush = () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      frame = null;
      const events = queued;
      queued = [];
      if (events.length === 0) return;
      setDraft((d) => {
        if (!d) return d;
        let next = d;
        for (const event of events) {
          next =
            event.type === "step"
              ? { ...next, steps: [...next.steps, event.text] }
              : { ...next, [event.type]: next[event.type] + event.text };
        }
        return next;
      });
    };
    try {
      const result = await api
        .sendMessage(
          requestId,
          conversationId,
          content,
          (event) => {
            if (event.type === "content") replyText += event.text;
            queued.push(event);
            if (frame === null) frame = window.requestAnimationFrame(flush);
          },
          images,
          files,
        )
        .finally(flush);
      if (result.gpuWarning) setSlowdown(result.gpuWarning);
      if (!result.withdrawn) readIfAuto(replyText);
      // Stopped before any answer: the message was taken back out of the
      // chat and returns to the box to edit or send again.
      if (result.withdrawn) {
        const back = result.withdrawn;
        setInput((prev) => (prev.trim() ? `${back}
${prev}` : back));
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      requestRef.current = null;
      setDraft(null);
      setBusy(false);
      await open(conversationId);
      await refreshList();
    }
  };

  /** The open chat, created first if this is a new one. */
  const ensureChat = async (): Promise<string | null> => {
    if (conversation?.id) return conversation.id;
    try {
      const created = await api.createConversation();
      if (!memOn) await api.setChatMemories(created.id, false);
      await open(created.id);
      return created.id;
    } catch (e) {
      setError(errorText(e));
      return null;
    }
  };

  /** `/web` and `/read`: the core runs the request and holds the result as
   * an attachment for the next message. */
  const runWeb = async (cmd: { command: "web" | "read"; argument: string }) => {
    const id = await ensureChat();
    if (!id) return;
    setFetching(true);
    setError(null);
    try {
      const a = await api.chatWeb(id, cmd.command, cmd.argument);
      attach.add({ attachments: [a], failed: [] });
      setInput("");
    } catch (e) {
      setError(errorText(e));
    } finally {
      setFetching(false);
    }
  };

  const send = async () => {
    const text = input.trim();
    if (!text || busy || !loaded) return;
    const cmd = webCommand(text);
    if (cmd) {
      await runWeb(cmd);
      return;
    }
    const id = await ensureChat();
    if (!id) return;
    setInput("");
    const taken = attach.take();
    await run(id, text, taken.images, taken.files);
  };

  const regenerate = async (last: StoredMessage) => {
    if (!conversation || busy) return;
    try {
      await api.truncateConversation(conversation.id, last.id);
    } catch (e) {
      setError(errorText(e));
      return;
    }
    await run(conversation.id, null);
  };

  /** A new chat with the messages before `beforeId` (all when null), opened. */
  const branchChat = async (beforeId: number | null): Promise<string | null> => {
    if (!conversation || busy) return null;
    try {
      const created = await api.branchConversation(conversation.id, beforeId);
      await refreshList();
      await open(created.id);
      return created.id;
    } catch (e) {
      setError(errorText(e));
      return null;
    }
  };

  /** Branch after `m` (it and everything before it are copied). */
  const branchAfter = (m: StoredMessage) => {
    const all = conversation?.messages ?? [];
    const next = all[all.findIndex((x) => x.id === m.id) + 1];
    void branchChat(next ? next.id : null);
  };

  /** Send an edited version of the user's message `m`. */
  const editMessage = async (m: StoredMessage, text: string, replace: boolean) => {
    if (!conversation || busy || !loaded) return;
    if (replace) {
      if (!window.confirm("Remove this message and everything after it, then send the edited version? This can't be undone. (Send as new branch keeps them.)")) return;
      try {
        await api.truncateConversation(conversation.id, m.id);
      } catch (e) {
        setError(errorText(e));
        return;
      }
      await run(conversation.id, text);
      return;
    }
    const id = await branchChat(m.id);
    if (id) await run(id, text);
  };

  const messages = conversation?.messages ?? [];
  const last = messages[messages.length - 1];
  const lastStats = [...messages].reverse().find((m) => m.stats?.totalTokens != null)?.stats ?? null;
  const awaitingReply = !busy && last?.role === "user";
  // Suggest a handoff past 70 % of the window.
  const suggestHandoff =
    Boolean(loaded && conversation && lastStats?.totalTokens != null) &&
    !busy &&
    !handoffOpen &&
    !handoffDismissed.has(conversation?.id ?? "") &&
    (lastStats?.totalTokens ?? 0) * 100 >= (loaded?.contextWindow ?? Infinity) * 70;

  /** The open tab of the chat details panel (null: closed). */
  const [details, setDetails] = useState<string | null>(null);
  const setMemories = async (on: boolean) => {
    if (!conversation) {
      setMemOn(on);
      return;
    }
    try {
      await api.setChatMemories(conversation.id, on);
      setMemOn(on);
    } catch (err) {
      setError(errorText(err));
    }
  };
  const setWeb = async (on: boolean) => {
    if (!conversation) return;
    if (on) {
      const status = await api.webStatus().catch(() => null);
      setChoosingMode(status?.settings.defaultMode ?? "ask");
      return;
    }
    try {
      await api.chatSetWeb(conversation.id, null);
      setWebMode(null);
    } catch (err) {
      setError(errorText(err));
    }
  };
  const setDocuments = async (value: string | null) => {
    if (!conversation) return;
    try {
      await api.setChatCollection(conversation.id, value);
      setLinked(value);
    } catch (err) {
      setError(errorText(err));
    }
  };
  const savePrompt = async () => {
    if (!conversation || prompt === conversation.systemPrompt) return;
    try {
      await api.setConversationPrompt(conversation.id, prompt);
      await open(conversation.id);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const exportChat = async (id: string) => {
    try {
      const path = await api.exportConversation(id);
      if (path) setNote(`Saved to ${path}`);
    } catch (e) {
      setError(errorText(e));
    }
  };
  /** Options switched on for this chat: shown as a mark on the + button. */
  const activeOptions = [memOn, webMode !== null, linked !== null, Boolean(conversation?.systemPrompt)].filter(Boolean).length;
  const linkedName = collections.find((c) => c.id === linked)?.name ?? null;

  return (
    <div className="chat-layout">
      {!focus && <ConversationList
        items={items}
        activeId={conversation?.id ?? null}
        busy={busy}
        onSelect={(id) => void open(id)}
        onNew={() => {
          setConversation(null);
          conversationIdRef.current = null;
          setMemOn(true);
          setPrompt("");
          setError(null);
          composerRef.current?.focus();
        }}
        onRename={async (id, title) => {
          try {
            await api.renameConversation(id, title);
            await refreshList();
            if (conversation?.id === id) await open(id);
          } catch (e) {
            setError(errorText(e));
          }
        }}
        onExport={(c) => void exportChat(c.id)}
        onDelete={async (c) => {
          if (!window.confirm(`Delete "${c.title}"? This cannot be undone.`)) return;
          try {
            await api.deleteConversation(c.id);
            if (conversation?.id === c.id) setConversation(null);
            await refreshList();
          } catch (e) {
            setError(errorText(e));
          }
        }}
      />}
      <div className="chat">
        <div className="gw-chat-head">
          <div className="gw-session-title">
            <div className="gw-session-name">{conversation?.title ?? "New chat"}</div>
            <div className="gw-session-sub">
              <span>{memOn ? "Memories on" : "Memories off"}</span>
              {webMode !== null && <span className="gw-on">Web on</span>}
              {linkedName && <span className="gw-on">Documents: {linkedName}</span>}
              {conversation?.systemPrompt && <span>System prompt set</span>}
            </div>
          </div>
          {webMode !== null && conversation && (
            <ModeBadge
              mode={webMode}
              onTighten={async () => {
                try {
                  const w = await api.chatSetWeb(conversation.id, "ask");
                  setWebMode(w.mode);
                } catch (err) {
                  setError(errorText(err));
                }
              }}
            />
          )}
          <IconButton
            icon={details ? PanelRightClose : PanelRightOpen}
            label={details ? "Hide chat details" : "Show chat details: system prompt, documents, web, memories, export"}
            aria-pressed={details !== null}
            onClick={() => setDetails((d) => (d ? null : "chat"))}
          />
        </div>
        {webMode !== null && loaded && !loaded.supportsTools && (
          <div className="banner warn">This model can't call tools, so replies won't use the web.</div>
        )}
        {slowdown && (
          <SlowdownBanner warning={slowdown} goToModels={goToModels} onDismiss={() => setSlowdown(null)} />
        )}
        {loaded ? (
          <ContextMeter stats={lastStats} window={loaded.contextWindow} />
        ) : (
          <div className="banner warn">
            No model is loaded. You can read saved chats; <button className="link" onClick={goToModels}>load a model</button> to continue one.
          </div>
        )}
        {suggestHandoff && conversation && (
          <HandoffCard
            reason="context"
            onOpen={() => setHandoffOpen(true)}
            onDismiss={() => setHandoffDismissed((d) => new Set(d).add(conversation.id))}
          />
        )}
        {choosingMode !== null && conversation && (
          <ModeChoice
            title="Turn on web access for this chat?"
            intro="Replies in this chat may search the web and read public pages until you turn the switch off. Choose how requests are approved."
            initial={choosingMode}
            onCancel={() => setChoosingMode(null)}
            onChoose={async (mode) => {
              setChoosingMode(null);
              try {
                const w = await api.chatSetWeb(conversation.id, mode);
                setWebMode(w.on ? w.mode : null);
              } catch (err) {
                setError(errorText(err));
              }
            }}
          />
        )}
        {showVisits && conversation && (
          <VisitedSites
            load={() => api.chatVisits(conversation.id, last && last.role === "assistant" ? last.id : null)}
            onClose={() => setShowVisits(false)}
          />
        )}
        {handoffOpen && conversation && (
          <HandoffDialog
            source={{ kind: "chat", id: conversation.id }}
            onClose={() => setHandoffOpen(false)}
            onStarted={async (started) => {
              setHandoffOpen(false);
              setHandoffDismissed((d) => new Set(d).add(conversation.id));
              if (started.chat) {
                await refreshList();
                await open(started.chat.id);
                await run(started.chat.id, started.message);
              } else {
                onAgentHandoff(started);
              }
            }}
          />
        )}
        <div className="messages" ref={scroll.ref} onScroll={scroll.onScroll}>
          {messages.length === 0 && !draft && (
            <EmptyState
              art={<Growth seed={conversation?.id ?? "new-chat"} width={120} height={72} roots={1} depth={4} />}
              title={conversation ? "Ask anything" : "Start a new chat"}
              body={
                loaded
                  ? "Everything runs on this computer; nothing leaves it unless you turn on the web. Use + to add files, documents or memories."
                  : "Load a model to start. You can still read your saved chats."
              }
            />
          )}
          {messages.map((m) => (
            <MessageView
              key={m.id}
              message={m}
              streaming={false}
              canRegenerate={Boolean(loaded) && !busy && m === last && m.role === "assistant"}
              onRegenerate={() => void regenerate(m)}
              onRemember={(text) => setRemember(memoryDraft(text))}
              onBranch={busy ? undefined : () => branchAfter(m)}
              onEdit={busy || !loaded || m.role !== "user" ? undefined : (text, replace) => void editMessage(m, text, replace)}
            />
          ))}
          {draft && (
            <>
              {draft.user !== null && (
                <MessageView
                  message={{ role: "user", content: draft.user, reasoning: "", stats: null }}
                  streaming
                  canRegenerate={false}
                  onRegenerate={() => {}}
                />
              )}
              {draft.steps.length > 0 && (
                <div className="web-steps small muted">
                  {draft.steps.map((s, i) => (
                    <div key={i}>{s}</div>
                  ))}
                </div>
              )}
              <MessageView
                message={{ role: "assistant", content: draft.content, reasoning: draft.reasoning, stats: null }}
                streaming
                canRegenerate={false}
                onRegenerate={() => {}}
              />
            </>
          )}
          {awaitingReply && loaded && conversation && (
            <div className="retry">
              <button onClick={() => void run(conversation.id, null)}>Get a reply</button>
            </div>
          )}
        </div>
        <SelectionRemember container={scroll.ref} onPick={setRemember} onAsk={onAsk} />
        {remember !== null && (
          <RememberForm
            initial={remember}
            scopes={[{ label: "Personal", scope: { kind: "personal" } }]}
            onDone={() => setRemember(null)}
          />
        )}
        {scroll.away && (
          <button className="jump-latest small" onClick={scroll.follow}>
            ↓ Jump to latest
          </button>
        )}
        {error && <div className="banner error">{error}</div>}
        {note && (
          <div className="banner info">
            {note}{" "}
            <button className="link small" onClick={() => setNote(null)}>
              Dismiss
            </button>
          </div>
        )}
        <AttachmentStrip items={attach.items} onRemove={attach.remove} />
        <div className="composer" ref={composerBox}>
          <div className="gw-composer-field">
          <textarea
            ref={composerRef}
            value={input}
            disabled={!loaded}
            placeholder={
              loaded
                ? "Message the model…"
                : "Load a model to chat"
            }
            title="Enter sends, Shift+Enter starts a new line. /web searches, /read reads a page. Drop files or paste screenshots here."
            onChange={(e) => setInput(e.target.value)}
            onPaste={attach.onPaste}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            rows={1}
          />
          <MicButton box={composerRef} setValue={setInput} disabled={!loaded} />
          </div>
          <div className="composer-actions">
            {busy ? (
              <button
                onClick={() => {
                  if (requestRef.current !== null) void api.cancelChat(requestRef.current);
                }}
              >
                Stop
              </button>
            ) : (
              <>
                <Menu
                  label="Add to this chat"
                  align="start"
                  trigger={(p) => (
                    <span className="gw-plus">
                      <IconButton icon={Plus} label="Add files, or switch the web, documents and memories for this chat" {...p} />
                      {activeOptions > 0 && (
                        <span className="gw-plus-mark" aria-label={`${activeOptions} on`}>
                          {activeOptions}
                        </span>
                      )}
                    </span>
                  )}
                  items={[
                    {
                      label: attach.picking ? "Reading…" : "Attach files…",
                      icon: Paperclip,
                      title:
                        "Attach files to your message: images (for models that can see), documents (PDF, DOCX), code and text, or programs and libraries (analyzed without running them). You can also drop files here or paste screenshots.",
                      onSelect: () => void attach.pick(),
                    },
                    "separator",
                    {
                      label: "Search the web",
                      icon: Globe,
                      hint: !conversation ? "after you send" : state.offline ? "offline" : webMode !== null ? <Pill tone="accent">on</Pill> : "off",
                      disabled: !conversation || state.offline,
                      title: !conversation ? "Available once the chat has its first message." : undefined,
                      onSelect: () => {
                        if (conversation && !state.offline && !busy) void setWeb(webMode === null);
                      },
                    },
                    {
                      label: "Use documents…",
                      icon: FileText,
                      hint: !conversation ? "after you send" : linkedName ? <Pill tone="accent">{linkedName}</Pill> : "none",
                      disabled: !conversation,
                      title: !conversation ? "Available once the chat has its first message." : undefined,
                      onSelect: () => setDetails("chat"),
                    },
                    {
                      label: "Memories",
                      icon: Lightbulb,
                      hint: memOn ? <Pill tone="accent">on</Pill> : "off",
                      onSelect: () => void setMemories(!memOn),
                    },
                    {
                      label: "System prompt…",
                      icon: ScrollText,
                      hint: conversation?.systemPrompt ? <Pill tone="accent">set</Pill> : undefined,
                      onSelect: () => setDetails("chat"),
                    },
                  ]}
                />
                {fetching && <span className="small muted">Fetching…</span>}
                <button className="primary" onClick={() => void send()} disabled={!input.trim() || !loaded}>
                  Send
                </button>
              </>
            )}
            <QuickSwitch state={state} refresh={refresh} busy={busy} onError={setError} />
          </div>
        </div>
      </div>
      {details && (
        <Inspector
          title="Chat details"
          active={details}
          onActive={setDetails}
          onClose={() => setDetails(null)}
          tabs={[
            {
              id: "chat",
              label: "This chat",
              content: (
                <div className="gw-inspector-list">
                  <h4>System prompt</h4>
                  {conversation ? (
                    <textarea
                      className="gw-chat-prompt"
                      rows={5}
                      value={prompt}
                      placeholder="Instructions applied to this whole chat"
                      onChange={(e) => setPrompt(e.target.value)}
                      onBlur={() => void savePrompt()}
                    />
                  ) : (
                    <p className="muted small">Available after the first message.</p>
                  )}
                  {conversation && prompt !== conversation.systemPrompt && (
                    <div className="row">
                      <Button size="sm" variant="primary" onClick={() => void savePrompt()}>
                        Save prompt
                      </Button>
                      <Button size="sm" onClick={() => setPrompt(conversation.systemPrompt)}>
                        Discard
                      </Button>
                    </div>
                  )}
                  <h4>Documents</h4>
                  {!conversation ? (
                    <p className="muted small">Available after the first message.</p>
                  ) : collections.length === 0 ? (
                    <p className="muted small">No document collections yet. Add some on the Documents page (in the menu).</p>
                  ) : (
                    <select value={linked ?? ""} onChange={(e) => void setDocuments(e.target.value || null)} aria-label="Documents for this chat">
                      <option value="">None</option>
                      {collections.map((c) => (
                        <option key={c.id} value={c.id}>
                          {c.name}
                        </option>
                      ))}
                    </select>
                  )}
                  <h4>Switches</h4>
                  <div className="gw-inspector-item">
                    <div>
                      <div>Search the web</div>
                      <div className="muted small">Replies may search and read pages, with the same approvals and guardrails as a research session. Off again after a restart.</div>
                    </div>
                    <Toggle
                      label="Search the web in this chat"
                      checked={webMode !== null}
                      disabled={!conversation || busy || state.offline}
                      onChange={(on) => void setWeb(on)}
                    />
                  </div>
                  <div className="gw-inspector-item">
                    <div>
                      <div>Memories</div>
                      <div className="muted small">Add your saved memories (the Memory page) to this chat.</div>
                    </div>
                    <Toggle label="Use memories in this chat" checked={memOn} onChange={(on) => void setMemories(on)} />
                  </div>
                  {conversation && (
                    <>
                      <h4>More</h4>
                      <div className="gw-chat-more">
                        <Button size="sm" icon={History} onClick={() => setShowVisits(true)}>
                          Visited sites
                        </Button>
                        {loaded && messages.length > 0 && (
                          <Button size="sm" icon={Share2} disabled={busy} onClick={() => setHandoffOpen(true)} title="Continue this chat in a fresh chat or an agent session, starting from a handoff note you review">
                            Hand off…
                          </Button>
                        )}
                        <Button size="sm" icon={Download} onClick={() => void exportChat(conversation.id)}>
                          Export as Markdown
                        </Button>
                      </div>
                    </>
                  )}
                </div>
              ),
            },
          ]}
        />
      )}
    </div>
  );
}
