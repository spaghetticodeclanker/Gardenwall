import { createPortal } from "react-dom";
import { usePoll } from "./usePoll";
import { useEffect, useRef, useState } from "react";
import { api, type AttentionEvent } from "./api";

const POLL_MS = 1500;
/** Most cards stacked at once; older ones are dropped. */
const MAX_CARDS = 4;
/** How long a "finished" card stays (paused while the pointer or focus is on the cards). */
const FINISHED_MS = 10000;
/** After the pointer or focus leaves, a card that was due stays this much longer. */
const GRACE_MS = 3000;
/** The fade-out before a card is removed. */
const FADE_MS = 250;

const KIND_TEXT: Record<AttentionEvent["kind"], string> = {
  finished: "finished",
  approval: "needs your approval",
  failed: "stopped with a problem",
  paused: "paused and asks whether to continue",
  security: "did something worth checking (see the notice and Settings → Activity log)",
};

/** Corner cards for agent sessions that finished, need approval or failed
 * (planned feature 2). The core records the events; a card names only the
 * session and what happened. */
export function AttentionCards({ showing, onOpen }: {
  /** The session on screen now: its events need no card. */
  showing: string | null;
  onOpen: (sid: string) => void;
}) {
  const [cards, setCards] = useState<AttentionEvent[]>([]);
  const since = useRef<number | null>(null);
  const showingRef = useRef(showing);
  showingRef.current = showing;
  const openRef = useRef(onOpen);
  openRef.current = onOpen;

  // One check at a time: showing the window and focusing it both ask at
  // once, and two checks from the same point would repeat cards.
  const inFlight = useRef(false);
  // A notification click shows and focuses the window, which checks at once.
  usePoll(() => {
    if (inFlight.current) return;
    inFlight.current = true;
    api
      .agentAttention(since.current)
      .then((r) => {
        since.current = r.latest;
        // Its system notification was clicked: the core brought the window
        // to the front; show that session.
        if (r.open) openRef.current(r.open);
        // The session on screen needs no card while the window is visible,
        // even when another program has the focus.
        const fresh = r.events.filter((e) => !(e.sid && e.sid === showingRef.current && document.visibilityState === "visible"));
        if (fresh.length) setCards((c) => [...c, ...fresh].slice(-MAX_CARDS));
      })
      .catch(() => {
        // The next check tries again.
      })
      .finally(() => {
        inFlight.current = false;
      });
  }, POLL_MS);

  // A card that asks for something (an approval, a paused session) goes as
  // soon as it is no longer pending, however it was answered; the others
  // that only report (finished) fade after a while. Problems stay until seen.
  const asking = cards.some((e) => e.kind === "approval" || e.kind === "paused");
  usePoll(
    () => {
      void Promise.all([api.agentDashboard(), api.listPendingApprovals()]).then(
        ([rows, pending]) => {
          const status = new Map(rows.map((r) => [r.id, r.status]));
          setCards((c) =>
            c.filter((e) => {
              if (e.kind === "approval") {
                if (!e.sid) return pending.length > 0;
                return status.get(e.sid) === "approval" || status.get(e.sid) === "needsYou" || pending.some((p) => p.agentSession === e.sid);
              }
              if (e.kind === "paused") return !!e.sid && status.get(e.sid) === "paused";
              return true;
            }),
          );
        },
        () => {},
      );
    },
    POLL_MS,
    asking,
  );
  // "Finished" cards dismiss themselves about 10 seconds after they appear
  // (final review item 7); cards that need you and failures stay until
  // dismissed. The countdown pauses while the pointer or keyboard focus is on
  // the cards, and a card fades out before it goes.
  const due = useRef(new Map<number, number>());
  const [held, setHeld] = useState(false);
  const [leaving, setLeaving] = useState<number[]>([]);
  const reporting = cards.filter((e) => e.kind === "finished").map((e) => e.seq).join(",");
  useEffect(() => {
    const now = Date.now();
    const finished = cards.filter((e) => e.kind === "finished");
    for (const e of finished) if (!due.current.has(e.seq)) due.current.set(e.seq, now + FINISHED_MS);
    for (const seq of [...due.current.keys()]) if (!finished.some((e) => e.seq === seq)) due.current.delete(seq);
    if (held || finished.length === 0) return;
    const next = Math.min(...due.current.values());
    const timer = window.setTimeout(() => {
      const t = Date.now();
      const gone = [...due.current].filter(([, at]) => at <= t + 50).map(([seq]) => seq);
      setLeaving((l) => [...l, ...gone]);
      window.setTimeout(() => {
        setCards((c) => c.filter((e) => !gone.includes(e.seq)));
        setLeaving((l) => l.filter((seq) => !gone.includes(seq)));
      }, FADE_MS);
    }, Math.max(0, next - now));
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reporting, held]);
  const hold = (on: boolean) => {
    if (!on) {
      const now = Date.now();
      for (const [seq, at] of due.current) due.current.set(seq, Math.max(at, now + GRACE_MS));
    }
    setHeld(on);
  };

  // While cards show, pages get bottom padding as tall as the stack, so the
  // last controls on a page can always be scrolled clear of it.
  const stackRef = useRef<HTMLDivElement>(null);
  const shown = cards.length > 0;
  useEffect(() => {
    const root = document.documentElement;
    const el = stackRef.current;
    if (!shown) setHeld(false);
    if (!shown || !el) {
      root.style.removeProperty("--gw-corner-stack");
      return;
    }
    const measure = () => {
      const r = el.getBoundingClientRect();
      root.style.setProperty("--gw-corner-stack", r.height > 0 ? `${Math.round(window.innerHeight - r.top + 16)}px` : "0px");
    };
    measure();
    const watch = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    watch?.observe(el);
    window.addEventListener("resize", measure);
    return () => {
      watch?.disconnect();
      window.removeEventListener("resize", measure);
      root.style.removeProperty("--gw-corner-stack");
    };
  }, [shown]);

  // Seen: opening a session clears its cards.
  useEffect(() => {
    if (showing) setCards((c) => c.filter((e) => e.sid !== showing));
  }, [showing]);

  if (cards.length === 0) return null;
  const dismiss = (seq: number) => setCards((c) => c.filter((e) => e.seq !== seq));
  const stack = (
    <div
      ref={stackRef}
      className="attention-stack"
      role="status"
      aria-live="polite"
      onMouseEnter={() => hold(true)}
      onMouseLeave={() => hold(false)}
      onFocus={() => hold(true)}
      onBlur={(ev) => {
        if (!ev.currentTarget.contains(ev.relatedTarget as Node | null)) hold(false);
      }}
    >
      {cards.map((e) => (
        <div key={e.seq} className={`attention-card ${e.kind}${leaving.includes(e.seq) ? " leaving" : ""}`}>
          <button
            className="attention-body"
            disabled={!e.sid}
            onClick={() => {
              dismiss(e.seq);
              if (e.sid) onOpen(e.sid);
            }}
            title={e.sid ? "Open this session" : undefined}
          >
            <strong>{e.name}</strong> {KIND_TEXT[e.kind]}
            <span className="small muted"> · {new Date(e.atMs).toLocaleTimeString()}</span>
          </button>
          <button className="link small" aria-label="Dismiss" onClick={() => dismiss(e.seq)}>
            ✕
          </button>
        </div>
      ))}
    </div>
  );
  // In the shared corner column (with the notices) when it exists.
  const corner = document.getElementById("gw-corner");
  return corner ? createPortal(stack, corner) : stack;
}
