// Files for the next chat or agent message (and
// ): chosen in the core's file picker, pasted (images only: the
// webview passes the bytes of the user's own paste), or dropped on the
// window (the core reads the paths the OS reports). Any file can be
// attached: images go to the model as images; documents, programs, text and
// other files are read by the core and its sandboxed workers. The webview
// only keeps tokens, summaries and image previews.
import { ClipboardEvent, useEffect, useRef, useState } from "react";
import { api, Attachment, AttachReport, errorText } from "./api";

export const MAX_IMAGES = 4;
export const MAX_FILES = 8;

export const NO_VISION =
  "The loaded model can't see images. Open Models and use \"Add vision\" on your model (or load a vision model), then try again.";

export function useAttachments({
  canSee,
  active,
  onError,
}: {
  /** The loaded model accepts images. */
  canSee: boolean;
  /** This composer is the one on screen (drops go to it). */
  active: boolean;
  onError: (e: string) => void;
}) {
  const [items, setItems] = useState<Attachment[]>([]);
  // Read by the drop channel, which outlives renders.
  const live = useRef({ canSee, onError, items });
  live.current = { canSee, onError, items };

  const add = (report: AttachReport) => {
    const { canSee, onError } = live.current;
    const problems = report.failed.map(([file, reason]) => `${file}: ${reason}`);
    let images = live.current.items.filter((a) => a.kind === "image").length;
    let files = live.current.items.length - images;
    const kept: Attachment[] = [];
    for (const a of report.attachments) {
      if (a.kind === "image" && !canSee) {
        void api.discardAttachment(a.token);
        problems.push(`${a.name}: ${NO_VISION}`);
      } else if (a.kind === "image" && images >= MAX_IMAGES) {
        void api.discardAttachment(a.token);
        problems.push(`${a.name}: at most ${MAX_IMAGES} images can be sent with one message`);
      } else if (a.kind === "file" && files >= MAX_FILES) {
        void api.discardAttachment(a.token);
        problems.push(`${a.name}: at most ${MAX_FILES} files can be sent with one message`);
      } else {
        if (a.kind === "image") images += 1;
        else files += 1;
        kept.push(a);
      }
    }
    if (kept.length > 0) {
      live.current.items = [...live.current.items, ...kept];
      setItems(live.current.items);
    }
    if (problems.length > 0) onError(`Not attached: ${problems.join("; ")}`);
  };

  // Dropped files go to the composer on screen; a hidden one stops
  // receiving them, and nothing dropped is read while none is open.
  useEffect(() => {
    if (!active) return;
    let token: string | null = null;
    let closed = false;
    void api
      .watchDroppedFiles(add)
      .then((t) => {
        token = t;
        if (closed) void api.unwatchDroppedFiles(t);
      })
      .catch((e) => live.current.onError(errorText(e)));
    return () => {
      closed = true;
      if (token) void api.unwatchDroppedFiles(token);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  /** Paste handler for the composer: pasted images are attached; text
   * pastes are left alone. */
  const onPaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const clip = [...e.clipboardData.items];
    const files = clip
      .filter((i) => i.kind === "file" && i.type.startsWith("image/"))
      .map((i) => i.getAsFile())
      .filter((f): f is File => f !== null);
    // Copying from a document often gives text and a picture of it; the
    // text is what's meant then.
    if (files.length === 0 || clip.some((i) => i.type === "text/plain")) return;
    e.preventDefault();
    if (!canSee) {
      onError(NO_VISION);
      return;
    }
    void (async () => {
      for (const f of files) {
        try {
          const img = await api.attachPastedImage(new Uint8Array(await f.arrayBuffer()));
          add({ attachments: [{ kind: "image", ...img }], failed: [] });
        } catch (err) {
          onError(errorText(err));
        }
      }
    })();
  };

  const [picking, setPicking] = useState(false);
  const pick = async () => {
    setPicking(true);
    try {
      add(await api.attachFiles());
    } catch (e) {
      onError(errorText(e));
    } finally {
      setPicking(false);
    }
  };

  const remove = (token: string) => {
    void api.discardAttachment(token);
    live.current.items = live.current.items.filter((x) => x.token !== token);
    setItems(live.current.items);
  };

  /** What to send; the list is cleared. */
  const take = () => {
    const taken = live.current.items;
    live.current.items = [];
    setItems([]);
    return {
      images: taken.filter((a) => a.kind === "image").map((a) => a.token),
      previews: taken.flatMap((a) => (a.kind === "image" ? [a.preview] : [])),
      files: taken.filter((a) => a.kind === "file").map((a) => a.token),
    };
  };

  return { items, add, onPaste, pick, picking, remove, take };
}

const WHAT: Record<string, string> = {
  text: "Text",
  document: "Document",
  program: "Program",
  data: "File",
  web: "Web",
};

export function AttachmentStrip({ items, onRemove }: { items: Attachment[]; onRemove: (token: string) => void }) {
  if (items.length === 0) return null;
  return (
    <div className="attachments composer-attachments">
      {items.map((a) =>
        a.kind === "image" ? (
          <span key={a.token} className="attachment">
            <img className="thumb" alt={a.name} src={a.preview} />
            <button className="link small" title={`Remove ${a.name}`} onClick={() => onRemove(a.token)}>
              ✕
            </button>
          </span>
        ) : (
          <span key={a.token} className="attachment file-chip" title={a.summary}>
            <span className="file-chip-kind">{WHAT[a.what] ?? "File"}</span>
            <span className="file-chip-name">{a.name}</span>
            <button className="link small" title={`Remove ${a.name}`} onClick={() => onRemove(a.token)}>
              ✕
            </button>
          </span>
        ),
      )}
    </div>
  );
}
