// Tables, bars and states for the redesigned UI (Phase 0).
import { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";

export interface Column<T> {
  key: string;
  header: ReactNode;
  /** CSS width, e.g. "96px" or "30%". Columns without one share the rest. */
  width?: string;
  /** Narrowest the column may get before the table scrolls sideways. */
  minWidth?: string;
  align?: "left" | "right";
  /** Left out when the table is narrow (e.g. beside the side panel), for figures shown elsewhere too. */
  optional?: boolean;
  render: (row: T) => ReactNode;
}

/**
 * An aligned table that never squishes: columns keep their minimum width and
 * the table scrolls sideways inside its own frame instead. Columns marked
 * `optional` are left out first when the frame is narrow.
 */
export function DataTable<T>({
  columns,
  rows,
  rowKey,
  selected,
  onSelect,
  empty,
}: {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  selected?: string | null;
  onSelect?: (row: T) => void;
  empty?: ReactNode;
}) {
  if (rows.length === 0 && empty) return <>{empty}</>;
  return (
    <div className="gw-table-frame">
      <table className="gw-table">
        <colgroup>
          {columns.map((c) => (
            <col key={c.key} className={c.optional ? "gw-col-optional" : undefined} style={{ width: c.width, minWidth: c.minWidth }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} scope="col" className={c.optional ? "gw-col-optional" : undefined} style={{ textAlign: c.align ?? "left", minWidth: c.minWidth }}>
                {c.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const key = rowKey(row);
            return (
              <tr
                key={key}
                className={`${onSelect ? "selectable" : ""}${selected === key ? " selected" : ""}`}
                onClick={onSelect ? () => onSelect(row) : undefined}
                aria-selected={onSelect ? selected === key : undefined}
              >
                {columns.map((c) => (
                  <td key={c.key} className={c.optional ? "gw-col-optional" : undefined} style={{ textAlign: c.align ?? "left", minWidth: c.minWidth }}>
                    {c.render(row)}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export interface Segment {
  label: string;
  value: number;
  /** A token name such as "--accent" or a CSS colour. */
  color: string;
  /** Drawn hatched (e.g. the GPU safety margin). */
  hatched?: boolean;
}

const cssColor = (c: string) => (c.startsWith("--") ? `var(${c})` : c);

/**
 * A segmented bar (context window, GPU and RAM memory). `marker` draws a thin
 * line at a fraction of the total, e.g. where the context will be summarised.
 */
export function SegmentedBar({
  segments,
  total,
  marker,
  height = 4,
  label,
  legend = false,
  format = (n) => n.toLocaleString(),
}: {
  segments: Segment[];
  total: number;
  marker?: { at: number; label: string };
  height?: number;
  label: string;
  legend?: boolean;
  format?: (n: number) => string;
}) {
  const used = segments.reduce((s, x) => s + x.value, 0);
  const title = segments.map((s) => `${s.label}: ${format(s.value)}`).join("\n") + `\nFree: ${format(Math.max(0, total - used))}`;
  return (
    <div className="gw-seg">
      <div
        className="gw-seg-track"
        style={{ height }}
        role="img"
        aria-label={`${label}: ${format(used)} of ${format(total)}${marker ? `; ${marker.label}` : ""}`}
        title={title}
      >
        {segments.map((s) => (
          <span
            key={s.label}
            className={`gw-seg-part${s.hatched ? " hatched" : ""}`}
            style={{ width: `${total > 0 ? (s.value / total) * 100 : 0}%`, background: s.hatched ? undefined : cssColor(s.color) }}
          />
        ))}
        {marker && <span className="gw-seg-marker" style={{ left: `${marker.at * 100}%` }} />}
      </div>
      {legend && (
        <div className="gw-seg-legend">
          {segments.map((s) => (
            <span key={s.label}>
              <i className={s.hatched ? "hatched" : ""} style={{ background: s.hatched ? undefined : cssColor(s.color) }} />
              {s.label} <b>{format(s.value)}</b>
            </span>
          ))}
          <span>
            Free <b>{format(Math.max(0, total - used))}</b>
          </span>
          {marker && <span className="gw-seg-legend-marker">{marker.label}</span>}
        </div>
      )}
    </div>
  );
}

/** An empty screen: optional art, one line of direction, one action. */
export function EmptyState({ art, title, body, action }: { art?: ReactNode; title: string; body?: ReactNode; action?: ReactNode }) {
  return (
    <div className="gw-empty">
      {art && <div className="gw-empty-art" aria-hidden="true">{art}</div>}
      <div className="gw-empty-title">{title}</div>
      {body && <p className="gw-empty-body">{body}</p>}
      {action}
    </div>
  );
}

/** An error that says what happened, the likely cause and the fix (main button); the raw log behind a link. */
export function ErrorCard({ title, cause, fix, details }: { title: string; cause?: ReactNode; fix?: ReactNode; details?: string }) {
  return (
    <div className="gw-error" role="alert">
      <div className="gw-error-title">
        <AlertTriangle aria-hidden="true" size={16} strokeWidth={1.75} />
        {title}
      </div>
      {cause && <p className="gw-error-cause">{cause}</p>}
      {(fix || details) && (
        <div className="gw-error-actions">
          {fix}
          {details && (
            <details className="gw-error-details">
              <summary>View log</summary>
              <pre>{details}</pre>
            </details>
          )}
        </div>
      )}
    </div>
  );
}
