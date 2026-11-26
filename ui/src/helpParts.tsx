// Building blocks of the redesigned Help page (docs/internal/design/HELP-REDESIGN.md):
// a topic is a summary with key-point chips, an optional diagram, then parts:
// headings, folding rows (one-line summary, the full text a click away),
// safety callouts, and tables for keys and commands. Folded text stays in the
// page (hidden), so search still finds it and opens the row.
import { createContext, ReactNode, useContext, useEffect, useId, useRef, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronRight, type LucideIcon } from "lucide-react";

export type HelpGroup = "start" | "using" | "further" | "reference";

export const HELP_GROUPS: { id: HelpGroup; label: string }[] = [
  { id: "start", label: "Start here" },
  { id: "using", label: "Using Gardenwall" },
  { id: "further", label: "Going further" },
  { id: "reference", label: "Reference" },
];

/** The folding row to open and show (a "More in the guide" link sets it). */
export const HelpTarget = createContext<string | null>(null);

export interface HelpRow {
  /** Anchor id (for example help-guide-context), so links can open this row. */
  anchor?: string;
  icon: LucideIcon;
  title: string;
  /** One line shown while folded. */
  summary: string;
  body: ReactNode;
}

export type HelpPart =
  | { kind: "heading"; text: string }
  | { kind: "rows"; rows: HelpRow[] }
  | { kind: "callout"; title?: string; summary: ReactNode; full?: ReactNode }
  | { kind: "table"; caption?: string; rows: { key: ReactNode; text: ReactNode }[]; note?: ReactNode }
  | { kind: "node"; node: ReactNode };

export interface HelpTopic {
  id: string;
  title: string;
  group: HelpGroup;
  icon: LucideIcon;
  /** The home card's one line. */
  card: string;
  /** Topics already in the new layout. */
  summary?: ReactNode;
  chips?: string[];
  diagram?: ReactNode;
  parts?: HelpPart[];
  /** Topics not yet in the new layout keep their full text here, unchanged. */
  body?: ReactNode;
  /** Topic ids listed under "Related topics" at the end (final review item 13). */
  related?: string[];
}

/** A folding row. It opens by itself when the search matches its text. */
function Row({ row, query }: { row: HelpRow; query: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const id = useId();
  const target = useContext(HelpTarget);
  useEffect(() => {
    const q = query.trim().toLowerCase();
    if (q && ref.current?.textContent?.toLowerCase().includes(q)) setOpen(true);
  }, [query]);
  useEffect(() => {
    if (!row.anchor || target !== row.anchor) return;
    setOpen(true);
    window.setTimeout(() => ref.current?.scrollIntoView({ behavior: "smooth", block: "start" }), 60);
  }, [target, row.anchor]);
  const Icon = row.icon;
  return (
    <div className={`gw-help-row${open ? " open" : ""}`} ref={ref} id={row.anchor}>
      <button type="button" className="gw-help-row-head" aria-expanded={open} aria-controls={id} onClick={() => setOpen((o) => !o)}>
        <Icon aria-hidden="true" size={16} strokeWidth={1.75} className="gw-help-row-icon" />
        <span className="gw-help-row-text">
          <span className="gw-help-row-title">{row.title}</span>
          <span className="gw-help-row-summary">{row.summary}</span>
        </span>
        {open ? <ChevronDown aria-hidden="true" size={16} /> : <ChevronRight aria-hidden="true" size={16} />}
      </button>
      <div className="gw-help-row-body" id={id} hidden={!open}>
        {row.body}
      </div>
    </div>
  );
}

function Callout({ part, query }: { part: Extract<HelpPart, { kind: "callout" }>; query: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const id = useId();
  useEffect(() => {
    const q = query.trim().toLowerCase();
    if (q && ref.current?.textContent?.toLowerCase().includes(q)) setOpen(true);
  }, [query]);
  return (
    <div className="gw-help-callout" ref={ref}>
      <AlertTriangle aria-hidden="true" size={16} className="gw-help-callout-icon" />
      <div>
        <div>
          {part.title && <strong>{part.title}</strong>} {part.summary}{" "}
          {part.full && (
            <button type="button" className="link small" aria-expanded={open} aria-controls={id} onClick={() => setOpen((o) => !o)}>
              {open ? "Less" : "Full text"}
            </button>
          )}
        </div>
        {part.full && (
          <div id={id} className="gw-help-callout-full" hidden={!open}>
            {part.full}
          </div>
        )}
      </div>
    </div>
  );
}

function Part({ part, query }: { part: HelpPart; query: string }) {
  switch (part.kind) {
    case "heading":
      return <h4 className="gw-help-heading">{part.text}</h4>;
    case "rows":
      return (
        <div className="gw-help-rows">
          {part.rows.map((r) => (
            <Row key={r.anchor ?? r.title} row={r} query={query} />
          ))}
        </div>
      );
    case "callout":
      return <Callout part={part} query={query} />;
    case "table":
      return (
        <div className="gw-help-table-wrap">
          <table className="gw-help-table">
            {part.caption && <caption className="gw-sr">{part.caption}</caption>}
            <tbody>
              {part.rows.map((r, i) => (
                <tr key={i}>
                  <th scope="row">{r.key}</th>
                  <td>{r.text}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {part.note && <p className="gw-help-table-note">{part.note}</p>}
        </div>
      );
    case "node":
      return <>{part.node}</>;
  }
}

/** One topic in the reader. */
export function TopicView({
  topic,
  query,
  onOpen,
  titleOf,
}: {
  topic: HelpTopic;
  query: string;
  /** Open another topic (the "Related topics" links). */
  onOpen?: (id: string) => void;
  titleOf?: (id: string) => string;
}) {
  return (
    <article className="gw-help-topic">
      <h3 className="gw-help-title">{topic.title}</h3>
      {topic.summary && <p className="gw-help-summary">{topic.summary}</p>}
      {topic.chips && (
        <div className="gw-help-chips">
          {topic.chips.map((c) => (
            <span key={c} className="gw-help-chip">
              {c}
            </span>
          ))}
        </div>
      )}
      {topic.diagram && <div className="gw-help-diagram">{topic.diagram}</div>}
      {topic.parts?.map((p, i) => (
        <Part key={i} part={p} query={query} />
      ))}
      {topic.body && <div className="gw-help-legacy">{topic.body}</div>}
      {/* Left out while searching, so topic names in it don't make every topic match. */}
      {topic.related && topic.related.length > 0 && onOpen && titleOf && !query.trim() && (
        <nav className="gw-help-related" aria-label="Related topics">
          <span className="gw-help-related-label">Related topics</span>
          {topic.related.map((id) => (
            <button key={id} type="button" className="link" onClick={() => onOpen(id)}>
              {titleOf(id)}
            </button>
          ))}
        </nav>
      )}
    </article>
  );
}

/** The sandbox and what the agent's commands can reach (Agent topic). */
export function SandboxDiagram() {
  return (
    <svg
      viewBox="0 0 560 150"
      className="gw-help-svg"
      role="img"
      aria-label="The agent's commands run inside a sandbox with no network that holds only the project folders, as drives. Other folders ask you first; secret files can't be opened by commands; the internet is blocked for commands."
    >
      <rect x="8" y="10" width="300" height="130" rx="10" className="d-ok-box" />
      <text x="22" y="30" className="d-ok">Sandbox (no network)</text>
      <rect x="22" y="42" width="120" height="40" rx="7" className="d-box" />
      <text x="32" y="60" className="d-text">Agent commands</text>
      <text x="32" y="74" className="d-muted">PowerShell / cmd</text>
      <rect x="160" y="42" width="136" height="40" rx="7" className="d-box" />
      <text x="170" y="60" className="d-text">W:\ your project</text>
      <text x="170" y="74" className="d-muted">read, edit, run</text>
      <rect x="22" y="92" width="274" height="36" rx="7" className="d-box" />
      <text x="32" y="114" className="d-soft">Undo copies of every change, kept outside the project</text>
      <line x1="308" y1="45" x2="345" y2="45" className="d-line-warn" />
      <line x1="308" y1="85" x2="345" y2="85" className="d-line-danger" />
      <line x1="308" y1="122" x2="345" y2="122" className="d-line-danger" />
      <rect x="350" y="28" width="200" height="34" rx="7" className="d-warn-box" />
      <text x="362" y="49" className="d-warn">Other folders · asks you first</text>
      <rect x="350" y="68" width="200" height="34" rx="7" className="d-danger-box" />
      <text x="362" y="89" className="d-danger">Secret files · commands can't open</text>
      <rect x="350" y="106" width="200" height="34" rx="7" className="d-danger-box" />
      <text x="362" y="127" className="d-danger">Internet · blocked for commands</text>
    </svg>
  );
}

/** How an approval works (Privacy and security). */
export function ApprovalDiagram() {
  return (
    <svg viewBox="0 0 560 96" className="gw-help-svg" role="img" aria-label="How an approval works: the agent asks for something outside what you granted; Gardenwall shows exactly what it will do; you choose Allow once, allow for the session, or Deny; only then does it run.">
      <rect x="6" y="20" width="118" height="56" rx="8" className="d-box" />
      <text x="18" y="44" className="d-text">The agent asks</text>
      <text x="18" y="60" className="d-muted">outside your grants</text>
      <rect x="146" y="20" width="132" height="56" rx="8" className="d-warn-box" />
      <text x="158" y="44" className="d-warn">Gardenwall shows</text>
      <text x="158" y="60" className="d-muted">exactly what it will do</text>
      <rect x="300" y="20" width="132" height="56" rx="8" className="d-box" />
      <text x="312" y="44" className="d-text">You choose</text>
      <text x="312" y="60" className="d-muted">once · session · deny</text>
      <rect x="454" y="20" width="100" height="56" rx="8" className="d-ok-box" />
      <text x="466" y="44" className="d-ok">Only then</text>
      <text x="466" y="60" className="d-muted">it runs</text>
      <line x1="124" y1="48" x2="146" y2="48" className="d-arrow" />
      <line x1="278" y1="48" x2="300" y2="48" className="d-arrow" />
      <line x1="432" y1="48" x2="454" y2="48" className="d-arrow" />
    </svg>
  );
}

/** How an agent on a project researches the web (Web research). */
export function WebDiagram() {
  return (
    <svg viewBox="0 0 560 110" className="gw-help-svg" role="img" aria-label="Web research from a project: the agent asks to research a question; you allow, edit or refuse it; a research assistant with no access to the project searches and reads public pages; its findings come back on a card you send, edit or discard.">
      <rect x="6" y="26" width="110" height="56" rx="8" className="d-box" />
      <text x="18" y="50" className="d-text">Agent on</text>
      <text x="18" y="66" className="d-text">your project</text>
      <rect x="138" y="26" width="120" height="56" rx="8" className="d-warn-box" />
      <text x="150" y="50" className="d-warn">Question card</text>
      <text x="150" y="66" className="d-muted">allow · edit · no</text>
      <rect x="280" y="10" width="140" height="88" rx="8" className="d-ok-box" />
      <text x="292" y="32" className="d-ok">Research assistant</text>
      <text x="292" y="50" className="d-muted">no project access</text>
      <text x="292" y="66" className="d-muted">searches, reads</text>
      <text x="292" y="82" className="d-muted">public pages</text>
      <rect x="442" y="26" width="112" height="56" rx="8" className="d-warn-box" />
      <text x="454" y="50" className="d-warn">Findings card</text>
      <text x="454" y="66" className="d-muted">send · edit · discard</text>
      <line x1="116" y1="54" x2="138" y2="54" className="d-arrow" />
      <line x1="258" y1="54" x2="280" y2="54" className="d-arrow" />
      <line x1="420" y1="54" x2="442" y2="54" className="d-arrow" />
    </svg>
  );
}

/** How memories reach a conversation (Memory). */
export function MemoryDiagram() {
  return (
    <svg viewBox="0 0 560 96" className="gw-help-svg" role="img" aria-label="How memories are used: you save one, or accept one the agent suggested; it is kept on the Memory page within a budget; it is added to your next chat or agent session.">
      <rect x="6" y="20" width="150" height="56" rx="8" className="d-box" />
      <text x="18" y="44" className="d-text">You save one</text>
      <text x="18" y="60" className="d-muted">or accept a suggestion</text>
      <rect x="196" y="20" width="160" height="56" rx="8" className="d-ok-box" />
      <text x="208" y="44" className="d-ok">Memory page</text>
      <text x="208" y="60" className="d-muted">encrypted, within a budget</text>
      <rect x="396" y="20" width="158" height="56" rx="8" className="d-box" />
      <text x="408" y="44" className="d-text">Next chat or session</text>
      <text x="408" y="60" className="d-muted">gets it added</text>
      <line x1="156" y1="48" x2="196" y2="48" className="d-arrow" />
      <line x1="356" y1="48" x2="396" y2="48" className="d-arrow" />
    </svg>
  );
}
