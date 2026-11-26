// Voice input in a message box (release plan feature 7): a microphone on
// the right of the text box, where people expect it in chat apps.
// Click to start and click again to stop, or hold Ctrl+Space while
// speaking. The core records and transcribes (the page never gets the
// microphone). The words appear at the cursor as you speak
// , refreshed about once a second; when you stop, the whole
// recording is transcribed once more (and tidied, if that's on) and
// replaces them. Nothing is sent: the user reads it first. Tidying is
// silent; "Tidy what I say" in Settings turns it off.
// Recording shows only a red square. Hidden when voice input is off in
// Settings → Voice.
import { RefObject, useCallback, useEffect, useRef, useState } from "react";
import { Loader2, Mic, Square } from "lucide-react";
import { api, errorText } from "./api";
import { useVoiceInputOn } from "./speech";
import { useToast } from "./ui";

type Phase = "idle" | "recording" | "transcribing";

/** Where the dictated words sit in the box: from `at`, `len` characters
 * (spacing included). */
type Span = { at: number; len: number };

/** Put `text` in place of `span` in `value`, with a space before and after
 * when the neighbours need one. */
export function placeInSpan(value: string, span: Span, text: string): { value: string; span: Span } {
  const before = value.slice(0, span.at);
  const after = value.slice(span.at + span.len);
  const lead = text && before && !/\s$/.test(before) ? " " : "";
  const trail = text && after && !/^\s/.test(after) ? " " : "";
  const inserted = text ? lead + text + trail : "";
  return { value: before + inserted + after, span: { at: span.at, len: inserted.length } };
}

export function MicButton({
  box,
  setValue,
  disabled,
}: {
  box: RefObject<HTMLTextAreaElement | null>;
  setValue: (update: (prev: string) => string) => void;
  disabled?: boolean;
}) {
  const on = useVoiceInputOn();
  const [phase, setPhase] = useState<Phase>("idle");
  const phaseRef = useRef<Phase>("idle");
  const span = useRef<Span>({ at: 0, len: 0 });
  const toast = useToast();

  const go = (p: Phase) => {
    phaseRef.current = p;
    setPhase(p);
  };

  /** Show `text` in the dictation's place in the box. */
  const show = useCallback(
    (text: string) => {
      // The updater must give the same result if React runs it twice, so it
      // works from this snapshot of the place, not the live ref.
      const place = { ...span.current };
      setValue((prev) => {
        // Edited meanwhile so the place no longer exists: start at the end.
        const from = place.at + place.len > prev.length ? { at: prev.length, len: 0 } : place;
        const next = placeInSpan(prev, from, text);
        span.current = next.span;
        return next.value;
      });
    },
    [setValue],
  );

  // Live text: one request at a time, about once a second.
  useEffect(() => {
    if (phase !== "recording") return;
    let stopped = false;
    void (async () => {
      while (!stopped && phaseRef.current === "recording") {
        await new Promise((r) => window.setTimeout(r, 800));
        if (stopped || phaseRef.current !== "recording") break;
        try {
          const words = await api.dictationPartial();
          if (!stopped && phaseRef.current === "recording" && words) show(words);
        } catch {
          // The final transcript reports any real problem.
        }
      }
    })();
    return () => {
      stopped = true;
    };
  }, [phase, show]);

  const start = useCallback(async () => {
    if (phaseRef.current !== "idle" || disabled || !on) return;
    const el = box.current;
    const focused = el && document.activeElement === el;
    const length = el?.value.length ?? 0;
    span.current = focused ? { at: el.selectionStart, len: el.selectionEnd - el.selectionStart } : { at: length, len: 0 };
    go("recording");
    try {
      await api.dictationStart();
    } catch (e) {
      go("idle");
      toast({ text: errorText(e), tone: "warn" });
    }
  }, [box, disabled, on, toast]);

  const stop = useCallback(async () => {
    if (phaseRef.current !== "recording") return;
    go("transcribing");
    try {
      const r = await api.dictationStop();
      show(r.text);
      const placed = { ...span.current };
      const el = box.current;
      window.requestAnimationFrame(() => {
        if (el) {
          el.focus();
          const end = placed.at + placed.len;
          el.setSelectionRange(end, end);
        }
      });
    } catch (e) {
      // Live words already shown stay; the user can edit or delete them.
      toast({ text: errorText(e), tone: "warn" });
    } finally {
      go("idle");
    }
  }, [box, show, toast]);

  // Hold Ctrl+Space while speaking (only for the message box on screen).
  useEffect(() => {
    if (!on) return;
    const visible = () => Boolean(box.current && box.current.offsetParent !== null);
    const down = (e: KeyboardEvent) => {
      if (e.code === "Space" && e.ctrlKey && !e.repeat && visible()) {
        e.preventDefault();
        void start();
      }
    };
    const up = (e: KeyboardEvent) => {
      if ((e.code === "Space" || e.key === "Control") && phaseRef.current === "recording" && visible()) {
        e.preventDefault();
        void stop();
      }
    };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
    };
  }, [box, on, start, stop]);

  // Leaving the page while recording throws the audio away.
  useEffect(
    () => () => {
      if (phaseRef.current === "recording") void api.dictationCancel();
    },
    [],
  );

  if (!on && phase === "idle") return null;
  if (phase === "transcribing")
    return (
      <button type="button" className="gw-mic busy" aria-label="Finishing the text…" title="Finishing the text…" disabled>
        <Loader2 size={16} />
      </button>
    );
  if (phase === "recording")
    return (
      <button
        type="button"
        className="gw-mic recording"
        aria-label="Stop recording"
        title="Listening; the words appear as you speak. Click to stop (or let go of Ctrl+Space)."
        onClick={() => void stop()}
      >
        <Square size={12} fill="currentColor" />
      </button>
    );
  return (
    <button
      type="button"
      className="gw-mic"
      aria-label="Speak instead of typing"
      title="Speak instead of typing: click to start and stop, or hold Ctrl+Space. Turned into text on this computer."
      disabled={disabled}
      onClick={() => void start()}
    >
      <Mic size={16} />
    </button>
  );
}
