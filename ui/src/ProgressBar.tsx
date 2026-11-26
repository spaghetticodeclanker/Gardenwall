// Progress bar with transfer speed and time left, used for model downloads,
// imports (hashing) and runtime installs. Speed is measured over the last
// few seconds of progress events, so a resumed download shows its real
// speed rather than counting the part it already had.
import { useRef } from "react";
import { formatBytes, Progress } from "./api";

/** How far back speed is averaged. */
const WINDOW_MS = 5000;
/** Speed is shown once samples span at least this long. */
const MIN_SPAN_MS = 1000;

type Sample = { t: number; done: number };

/** Bytes per second over the recent window, or null while unknown. */
export function useRate(p: Progress | null): number | null {
  const samples = useRef<Sample[]>([]);
  const total = useRef<number | null>(null);
  if (!p) {
    samples.current = [];
    total.current = null;
    return null;
  }
  const now = performance.now();
  const list = samples.current;
  const last = list[list.length - 1];
  // A new transfer, or progress went back (a retried chunk): start over.
  if (total.current !== p.total || (last && p.done < last.done)) {
    list.length = 0;
    total.current = p.total;
  }
  const newest = list[list.length - 1];
  if (!newest || newest.done !== p.done || now - newest.t > 250) list.push({ t: now, done: p.done });
  while (list.length > 2 && now - (list[0]?.t ?? now) > WINDOW_MS) list.shift();
  const first = list[0];
  if (!first) return null;
  const span = now - first.t;
  if (span < MIN_SPAN_MS) return null;
  return ((p.done - first.done) / span) * 1000;
}

export function formatRate(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

export function formatRemaining(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "";
  if (seconds < 60) return "less than a minute left";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `about ${minutes} min left`;
  const hours = Math.floor(minutes / 60);
  return `about ${hours} h ${minutes % 60} min left`;
}

/** One line: percent, speed and time left. */
export function InlineProgress({ p }: { p: Progress | null }) {
  const rate = useRate(p);
  if (!p) return <>Starting…</>;
  const pct = p.total > 0 ? Math.round((p.done / p.total) * 100) : 0;
  const parts = [`${pct}%`];
  if (p.done < p.total && rate !== null && rate > 0) {
    parts.push(formatRate(rate));
    parts.push(formatRemaining((p.total - p.done) / rate));
  }
  return <>{parts.filter(Boolean).join(" · ")}</>;
}

/** `status` (Paused, Starting…) replaces the speed and time left. */
export function ProgressBar({ p, status }: { p: Progress | null; status?: string }) {
  const rate = useRate(status ? null : p);
  if (!p && status === undefined) return null;
  if (!p) {
    return (
      <div className="progress">
        <span>{status}</span>
      </div>
    );
  }
  const pct = p.total > 0 ? (p.done / p.total) * 100 : 0;
  const finished = p.total > 0 && p.done >= p.total;
  const parts = [`${formatBytes(p.done)} / ${formatBytes(p.total)}`];
  if (status) parts.push(status);
  else if (!finished && rate !== null && rate > 0) {
    parts.push(formatRate(rate));
    parts.push(formatRemaining((p.total - p.done) / rate));
  }
  return (
    <div className="progress">
      <div className="progress-fill" style={{ width: `${pct}%` }} />
      <span>{parts.filter(Boolean).join(" · ")}</span>
    </div>
  );
}
