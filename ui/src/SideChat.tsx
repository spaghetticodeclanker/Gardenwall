import { Sprig } from "./art/Sprig";
import { useEffect, useRef, useState } from "react";
import { api, errorText, type LoadedView } from "./api";
import { Markdown } from "./Markdown";
import { Button, publishRightPanels, useRightPanel } from "./ui";

// Request ids for side chats, kept apart from the main chat's counter.
let nextSideRequest = 1_000_000_000;

interface Turn {
  question: string;
  answer: string;
}

/**
 * A side chat: a short, separate conversation about text selected in a chat
 * or agent session. It starts with only that text, so the main conversation's
 * context is neither used nor changed. It is saved as an ordinary chat.
 */
export function SideChat({ seed, loaded, onClose, onUse }: {
  seed: string;
  loaded: LoadedView | null;
  onClose: () => void;
  /** Put the last answer in the message box of the page the side chat came from, for the user to review and send. */
  onUse: (text: string) => void;
}) {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const conversation = useRef<string | null>(null);
  const request = useRef<number | null>(null);
  const box = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => box.current?.focus(), []);
  // Leaving the side chat stops a reply that is still being written.
  useEffect(
    () => () => {
      if (request.current !== null) void api.cancelChat(request.current).catch(() => {});
    },
    [],
  );

  const ask = async () => {
    const question = input.trim();
    if (!question || busy || !loaded) return;
    setInput("");
    setBusy(true);
    setError(null);
    setCopied(false);
    const first = conversation.current === null;
    setTurns((t) => [...t, { question, answer: "" }]);
    const id = nextSideRequest++;
    request.current = id;
    try {
      if (!conversation.current) conversation.current = (await api.createConversation()).id;
      // The first message carries the selected text, quoted line by line.
      const quoted = seed
        .split("\n")
        .map((line) => `> ${line}`)
        .join("\n");
      const content = first ? `${quoted}\n\n${question}` : question;
      const result = await api.sendMessage(id, conversation.current, content, (e) => {
        if (e.type !== "content") return;
        setTurns((t) => t.map((turn, i) => (i === t.length - 1 ? { ...turn, answer: turn.answer + e.text } : turn)));
      });
      const final = result.message?.content;
      if (final) setTurns((t) => t.map((turn, i) => (i === t.length - 1 ? { ...turn, answer: final } : turn)));
    } catch (e) {
      setError(errorText(e));
    } finally {
      request.current = null;
      setBusy(false);
    }
  };

  const last = turns[turns.length - 1]?.answer ?? "";
  const panel = useRef<HTMLElement>(null);
  useRightPanel(panel);
  // The side chat takes the window's right edge; the session or chat moves
  // left of it, and an open details panel docks beside it (final review item
  // 6). When the conversation would keep less than 360 px, the details panel
  // is hidden while the side chat is open and comes back when it closes.
  const [hidesPanel, setHidesPanel] = useState(false);
  useEffect(() => {
    const root = document.documentElement;
    let frame = 0;
    const update = () => {
      frame = 0;
      const width = panel.current?.getBoundingClientRect().width ?? 0;
      root.style.setProperty("--gw-side-chat", `${Math.round(width)}px`);
      const main = [...document.querySelectorAll<HTMLElement>(".gw-session > .gw-session-main, .chat-layout > .chat")].find(
        (e) => e.offsetWidth > 0,
      );
      const withPanel = main?.parentElement?.querySelector(":scope > .gw-inspector") != null;
      const room = main ? window.innerWidth - width - main.getBoundingClientRect().left : Infinity;
      const hide = withPanel && room * 0.55 < 360;
      root.classList.toggle("gw-side-chat-hides-panel", hide);
      setHidesPanel(hide);
      publishRightPanels();
    };
    const later = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    root.classList.add("gw-side-chat-open");
    update();
    // A details panel opening or closing, or another page, changes the room.
    const watch = new MutationObserver(later);
    watch.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("resize", later);
    return () => {
      watch.disconnect();
      window.removeEventListener("resize", later);
      if (frame) cancelAnimationFrame(frame);
      root.classList.remove("gw-side-chat-open", "gw-side-chat-hides-panel");
      root.style.removeProperty("--gw-side-chat");
      requestAnimationFrame(publishRightPanels);
    };
  }, []);
  return (
    <aside ref={panel} className="side-chat" aria-label="Side chat">
      <div className="row">
        <strong>Side chat</strong>
        <button className="link small" onClick={onClose}>
          Close
        </button>
      </div>
      {hidesPanel && (
        <div className="small muted side-chat-note" role="status">
          The details panel is hidden while the side chat is open; it comes back when you close it.
        </div>
      )}
      <div className="small muted">About the text you selected. It doesn't see or change the main conversation.</div>
      <blockquote className="side-chat-seed small">{seed}</blockquote>
      <div className="side-chat-turns">
        {turns.map((t, i) => (
          <div key={i}>
            <div className="side-chat-question">{t.question}</div>
            {t.answer ? (
              <Markdown text={t.answer} />
            ) : (
              <div className="small muted gw-working" role="status">
                <Sprig /> Thinking…
              </div>
            )}
          </div>
        ))}
      </div>
      {error && <div className="banner error">{error}</div>}
      {!loaded && <div className="small muted">Load a model to ask.</div>}
      <textarea
        ref={box}
        rows={3}
        value={input}
        disabled={!loaded}
        placeholder="Ask about the selected text"
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void ask();
          }
        }}
      />
      <div>
        {busy ? (
          <Button onClick={() => request.current !== null && void api.cancelChat(request.current).catch(() => {})}>Stop</Button>
        ) : (
          <Button variant="primary" disabled={!loaded || !input.trim()} onClick={() => void ask()}>
            Ask
          </Button>
        )}{" "}
        {last && !busy && (
          <button
            className="link small"
            onClick={() =>
              void navigator.clipboard
                .writeText(last)
                .then(() => setCopied(true))
                .catch(() => setError("The answer couldn't be copied."))
            }
          >
            {copied ? "Copied" : "Copy the last answer"}
          </button>
        )}{" "}
        {last && !busy && (
          <button className="link small" title="Adds the last answer to the message box. Nothing is sent until you send it." onClick={() => onUse(last)}>
            Put it in the message box
          </button>
        )}
      </div>
    </aside>
  );
}
