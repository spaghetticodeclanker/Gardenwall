// Follow new output only while the reader is at the bottom
// : scrolling up to reread stops the following, and "Jump to
// latest" (or sending a message) resumes it.
import { useCallback, useEffect, useRef, useState } from "react";

/** How close to the bottom (px) still counts as following. */
const NEAR_BOTTOM = 80;

export function useStickyScroll(dependency: unknown) {
  const ref = useRef<HTMLDivElement | null>(null);
  const following = useRef(true);
  const [away, setAway] = useState(false);

  const onScroll = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM;
    following.current = atBottom;
    setAway(!atBottom);
  }, []);

  // New content: keep the latest in view only while following.
  useEffect(() => {
    const el = ref.current;
    if (el && following.current) el.scrollTop = el.scrollHeight;
  }, [dependency]);

  /** Follow the output again (after sending, or when opening a chat). */
  const follow = useCallback(() => {
    following.current = true;
    setAway(false);
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  return { ref, onScroll, away, follow };
}
