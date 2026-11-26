// Notices at the top of a page (final review item 8). Two or more fold into
// one line: how many there are, the first one's text cut short, and Show.
// Shown, every notice keeps its own buttons (Dismiss, Stop, Open Updates…).
// One notice shows as it is. Nothing is removed: folded notices are only
// hidden until Show.
import { ReactNode, useLayoutEffect, useRef, useState } from "react";

export function NoticeFold({ children, className }: { children: ReactNode; className?: string }) {
  const list = useRef<HTMLDivElement>(null);
  const [count, setCount] = useState(0);
  const [first, setFirst] = useState("");
  const [open, setOpen] = useState(false);
  // The notices are whatever the children render (some render nothing), so
  // count what is actually there, and follow changes.
  useLayoutEffect(() => {
    const el = list.current;
    if (!el) return;
    const read = () => {
      const kids = [...el.children];
      setCount(kids.length);
      // The first notice's words, without its buttons' labels (Dismiss, Stop…).
      const words: string[] = [];
      const walk = kids[0] ? document.createTreeWalker(kids[0], NodeFilter.SHOW_TEXT) : null;
      for (let n = walk?.nextNode(); n; n = walk!.nextNode()) if (!n.parentElement?.closest("button")) words.push(n.nodeValue ?? "");
      setFirst(words.join(" ").replace(/\s+/g, " ").trim());
    };
    read();
    const watch = new MutationObserver(read);
    watch.observe(el, { childList: true, subtree: true, characterData: true });
    return () => watch.disconnect();
  }, []);
  const folded = count >= 2 && !open;
  return (
    <div className={className ? `gw-notices ${className}` : "gw-notices"}>
      {count >= 2 && (
        <div className="banner info gw-notices-line">
          <strong className="gw-nowrap">{count} notices</strong>
          {folded && <span className="gw-notices-first">{first}</span>}
          <button type="button" className="gw-linkbtn" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
            {open ? "Hide" : "Show"}
          </button>
        </div>
      )}
      <div ref={list} className="gw-notices-list" hidden={folded}>
        {children}
      </div>
    </div>
  );
}
