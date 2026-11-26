// One polling helper for the window's status checks (approvals, attention
// cards, the dashboard, session badges, update results). It runs only while the window is
// visible, so a minimized Gardenwall wakes neither the webview nor the core;
// it checks once at once when the window is shown again or gains focus (the
// core's system notifications bring it back for anything urgent).
import { useEffect, useRef } from "react";

export function usePoll(fn: () => void, ms: number, enabled = true) {
  const latest = useRef(fn);
  latest.current = fn;

  useEffect(() => {
    if (!enabled) return;
    let timer: number | undefined;
    const tick = () => latest.current();
    const start = () => {
      if (timer !== undefined || document.visibilityState !== "visible") return;
      tick();
      timer = window.setInterval(tick, ms);
    };
    const stop = () => {
      if (timer !== undefined) window.clearInterval(timer);
      timer = undefined;
    };
    const onVisibility = () => (document.visibilityState === "visible" ? start() : stop());
    start();
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("focus", tick);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", tick);
    };
  }, [ms, enabled]);
}
