import { useEffect, useRef } from "react";

/** Text on its way to a message box (a side chat's answer). `n` tells two
 * inserts of the same text apart. */
export interface Insert {
  text: string;
  n: number;
}

/**
 * Adds each new `insert` to a message box once, if that box is on screen
 * (`active`). The text is only added: the user reviews and sends it. An
 * insert that was already there when the box appeared is ignored, so a
 * session opened later doesn't receive an old answer.
 */
export function useInsert(insert: Insert | null, active: boolean, setInput: (update: (old: string) => string) => void) {
  const seen = useRef(insert?.n);
  useEffect(() => {
    if (!insert || insert.n === seen.current) return;
    seen.current = insert.n;
    if (active) setInput((old) => (old.trim() ? `${old}\n\n${insert.text}` : insert.text));
  }, [insert, active, setInput]);
}
