// The message box grows with what you type: from its
// three rows up to about twelve lines or 40% of the window, whichever is
// less, then it scrolls inside. It shrinks back when the text is sent.
//
// A page that stays mounted while hidden (Chat, Agent) measures 0 high, so
// a hidden box is left alone, and the box is measured again whenever its
// width changes: when its page is shown again, or the window is resized and
// the text wraps differently.
import { RefObject, useLayoutEffect } from "react";

const MAX_LINES = 12;

function fit(el: HTMLTextAreaElement) {
  // Hidden (display: none somewhere above): nothing can be measured.
  if (el.clientWidth === 0) return;
  const style = getComputedStyle(el);
  const line = parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.4 || 20;
  const extra = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) + parseFloat(style.borderTopWidth) + parseFloat(style.borderBottomWidth);
  const max = Math.min(line * MAX_LINES + extra, window.innerHeight * 0.4);
  el.style.height = "auto";
  const wanted = el.scrollHeight + parseFloat(style.borderTopWidth) + parseFloat(style.borderBottomWidth);
  el.style.height = `${Math.min(wanted, max)}px`;
  el.style.overflowY = wanted > max ? "auto" : "hidden";
}

/** `rows`: the box's starting rows, when they change with the window (two below 720 px tall). */
export function useAutoGrow(ref: RefObject<HTMLTextAreaElement | null>, value: string, rows?: number) {
  useLayoutEffect(() => {
    const el = ref.current;
    if (el) fit(el);
  }, [ref, value, rows]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let width = el.clientWidth;
    let frame = 0;
    // Only width changes matter; the heights this hook sets are ignored. The
    // new height is set in the next frame, not inside the observer's callback
    // (changing the watched box there is a "ResizeObserver loop" error).
    const watch = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => fit(el));
    });
    watch.observe(el);
    return () => {
      watch.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [ref]);
}
