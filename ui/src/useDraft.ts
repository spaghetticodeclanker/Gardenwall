// Keeps the text typed in a composer but not sent, so it is still there
// after switching chats or restarting Gardenwall. The core stores it
// encrypted with the chats; sending (or clearing) the text removes it.
import { useEffect, useRef } from "react";
import { api } from "./api";

/** `owner` names the place being typed in (`null`: nothing is kept). */
export function useDraft(owner: string | null, value: string, setValue: (text: string) => void) {
  const current = useRef(value);
  current.current = value;
  const loaded = useRef<string | null>(null);
  const previous = useRef<string | null>(null);

  useEffect(() => {
    const before = previous.current;
    previous.current = owner;
    loaded.current = null;
    // Leaving one place for another: what was typed stays with the old one.
    if (before && before !== owner) {
      // A new chat that just got its id is the same place: the text
      // typed meanwhile moves with it.
      const sameplace = before.endsWith(":new") && owner !== null;
      void api.setDraft(before, sameplace ? "" : current.current).catch(() => {});
      if (!sameplace) {
        setValue("");
        current.current = "";
      }
    }
    if (!owner) return;
    let cancelled = false;
    api
      .getDraft(owner)
      .then((text) => {
        if (cancelled) return;
        if (text && !current.current) setValue(text);
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) loaded.current = owner;
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [owner]);

  useEffect(() => {
    if (!owner || loaded.current !== owner) return;
    const timer = window.setTimeout(() => void api.setDraft(owner, value).catch(() => {}), 800);
    return () => window.clearTimeout(timer);
  }, [owner, value]);
}
