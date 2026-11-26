// Whether a media query matches now, kept up to date as the window changes
// (final review item 8: the session header's ⋯ menu below 1100 px, two-line
// message boxes below 720 px tall).
import { useEffect, useState } from "react";

export function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => typeof window !== "undefined" && window.matchMedia(query).matches);
  useEffect(() => {
    const list = window.matchMedia(query);
    const update = () => setMatches(list.matches);
    update();
    list.addEventListener("change", update);
    return () => list.removeEventListener("change", update);
  }, [query]);
  return matches;
}

/** Below this height the agent and chat message boxes start at two lines. */
export const SHORT_WINDOW = "(max-height: 719px)";
/** Below this width the agent session's header keeps to one line (⋯ menu). */
export const NARROW_WINDOW = "(max-width: 1100px)";
