import { useEffect, useRef, useState } from "react";
import { Check, CornerDownLeft, MessageCircleQuestion, Pencil } from "lucide-react";
import { api, errorText, type QuestionCard } from "./api";
import { usePoll } from "./usePoll";

/**
 * The agent's question card (`ask_user`): up to four questions, each with a
 * few answers to pick from or a typed one. The questions are the model's
 * text, checked and shortened by the core and shown as plain text.
 *
 * One question at a time: choices are numbered rows,
 * picking one (click, or its number) answers it and moves on, and the last
 * answer sends. "Something else" opens a field for a typed answer. Arrow
 * keys move between rows, Escape skips.
 */
export function AskCard({ sid, busy }: { sid: string; busy: boolean }) {
  const [card, setCard] = useState<QuestionCard | null>(null);
  const [answers, setAnswers] = useState<string[]>([]);
  const [at, setAt] = useState(0);
  const [typing, setTyping] = useState(false);
  const [typed, setTyped] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);

  usePoll(
    () =>
      void api
        .agentQuestion(sid)
        .then((next) =>
          setCard((current) => {
            if (next?.id !== current?.id) {
              setAnswers([]);
              setAt(0);
              setTyping(false);
              setTyped("");
              setSending(false);
              setError(null);
            }
            return next;
          }),
        )
        .catch(() => setCard(null)),
    700,
    busy,
  );

  const question = card?.questions[at];
  // A new question: put the cursor in its field when typing, else leave focus where it is.
  useEffect(() => {
    if (typing) box.current?.querySelector<HTMLInputElement>(".ask-typed input")?.focus();
  }, [typing, at]);

  if (!card || !busy || !question) return null;
  const total = card.questions.length;

  const send = async (list: string[] | null) => {
    setSending(true);
    try {
      await api.agentAnswer(sid, card.id, list);
      setCard(null);
    } catch (e) {
      setError(errorText(e));
      setSending(false);
    }
  };
  const answer = (text: string) => {
    if (sending || !text.trim()) return;
    const next = [...answers];
    next[at] = text.trim();
    setAnswers(next);
    setTyping(false);
    setTyped("");
    if (at + 1 < total) setAt(at + 1);
    else void send(card.questions.map((_, i) => next[i] ?? ""));
  };

  const rows = () => [...(box.current?.querySelectorAll<HTMLButtonElement>(".ask-choice") ?? [])];
  const onKeyDown = (e: React.KeyboardEvent) => {
    const inField = (e.target as HTMLElement).tagName === "INPUT";
    if (e.key === "Escape") {
      e.preventDefault();
      if (typing) setTyping(false);
      else void send(null);
      return;
    }
    if (inField) return;
    const n = Number(e.key);
    if (Number.isInteger(n) && n >= 1 && n <= question.options.length) {
      e.preventDefault();
      answer(question.options[n - 1]!);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const all = rows();
      const i = all.indexOf(document.activeElement as HTMLButtonElement);
      all[(i + (e.key === "ArrowDown" ? 1 : all.length - 1 + (i < 0 ? 1 : 0))) % all.length]?.focus();
    }
  };

  return (
    <div className="ask-card" role="group" aria-label="The agent is asking you" ref={box} onKeyDown={onKeyDown}>
      <div className="ask-head">
        <MessageCircleQuestion size={16} aria-hidden="true" />
        <span>The agent is asking</span>
        <span className="ask-tag" title="This text was written by the model, not by Gardenwall">
          Written by the model
        </span>
        {total > 1 && (
          <span className="ask-count">
            {at + 1} of {total}
          </span>
        )}
      </div>
      <div className="ask-question" id={`ask-q-${card.id}-${at}`}>
        {question.question}
      </div>
      <div className="ask-choices" role="radiogroup" aria-labelledby={`ask-q-${card.id}-${at}`}>
        {question.options.map((o, i) => {
          const chosen = answers[at] === o;
          return (
            <button
              key={o}
              type="button"
              role="radio"
              aria-checked={chosen}
              className={`ask-choice${chosen ? " chosen" : ""}`}
              disabled={sending}
              onClick={() => answer(o)}
            >
              <span className="ask-key" aria-hidden="true">
                {i + 1}
              </span>
              <span className="ask-label">{o}</span>
              {chosen && <Check size={16} aria-hidden="true" className="ask-check" />}
            </button>
          );
        })}
        {typing ? (
          <form
            className="ask-typed"
            onSubmit={(e) => {
              e.preventDefault();
              answer(typed);
            }}
          >
            <input
              type="text"
              placeholder="Type your answer"
              aria-label="Your own answer"
              maxLength={500}
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
            <button type="submit" className="ask-send" disabled={!typed.trim() || sending} aria-label="Send this answer" title="Send (Enter)">
              <CornerDownLeft size={16} aria-hidden="true" />
            </button>
          </form>
        ) : (
          <button type="button" className="ask-choice other" disabled={sending} onClick={() => setTyping(true)}>
            <span className="ask-key" aria-hidden="true">
              <Pencil size={12} />
            </span>
            <span className="ask-label">Something else…</span>
          </button>
        )}
      </div>
      {error && <div className="banner error">{error}</div>}
      <div className="ask-foot">
        {at > 0 && (
          <button type="button" className="link small" disabled={sending} onClick={() => setAt(at - 1)}>
            Back
          </button>
        )}
        <span className="small muted">{sending ? "Sending…" : "Pick an answer or press its number · Esc skips"}</span>
        <button type="button" className="link small" disabled={sending} onClick={() => void send(null)}>
          Skip
        </button>
      </div>
    </div>
  );
}
