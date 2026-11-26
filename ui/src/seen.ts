// When each agent session was last on screen, so Home
// can list finished sessions you haven't looked at yet. Kept in this
// window's storage only; session ids are random, so old entries are pruned.
const KEY = "gw.seen";
const MAX_ENTRIES = 200;

function read(): Record<string, number> {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return v && typeof v === "object" ? (v as Record<string, number>) : {};
  } catch {
    return {};
  }
}

/** Milliseconds since 1970 when `sid` was last on screen, or 0. */
export function seenAt(sid: string): number {
  return read()[sid] ?? 0;
}

/** `sid` is on screen now. */
export function markSeen(sid: string) {
  const all = read();
  all[sid] = Date.now();
  const entries = Object.entries(all).sort((a, b) => b[1] - a[1]).slice(0, MAX_ENTRIES);
  try {
    localStorage.setItem(KEY, JSON.stringify(Object.fromEntries(entries)));
  } catch {
    // Not remembered: a finished session may show on Home until opened again.
  }
}
