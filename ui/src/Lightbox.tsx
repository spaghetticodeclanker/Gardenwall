// Clicking any image thumbnail (chat, agent, composer) shows it full size.
// Only `data:` images held by the app are shown; Escape or a click closes.
import { useEffect, useState } from "react";

export function Lightbox() {
  const [src, setSrc] = useState<{ url: string; alt: string } | null>(null);
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const t = e.target;
      if (t instanceof HTMLImageElement && t.classList.contains("thumb") && t.src.startsWith("data:image/")) {
        setSrc({ url: t.src, alt: t.alt || "Image" });
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSrc(null);
    };
    document.addEventListener("click", onClick);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("click", onClick);
      window.removeEventListener("keydown", onKey);
    };
  }, []);
  if (!src) return null;
  return (
    <div className="lightbox" role="dialog" aria-label={src.alt} onClick={() => setSrc(null)}>
      <img src={src.url} alt={src.alt} />
      <span className="small muted">Click anywhere or press Escape to close</span>
    </div>
  );
}
