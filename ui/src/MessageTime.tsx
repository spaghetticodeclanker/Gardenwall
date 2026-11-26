// When a message was sent, shown on hover or keyboard focus
// The time for today's messages, the date and time for older ones.

function sameDay(a: Date, b: Date) {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

export function formatMessageTime(ms: number, now = new Date()): string {
  const d = new Date(ms);
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (sameDay(d, now)) return time;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(d, yesterday)) return `Yesterday, ${time}`;
  const date = d.toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
  return `${date}, ${time}`;
}

/** `at` in milliseconds since 1970; nothing is shown without one. */
export function MessageTime({ at }: { at: number | null | undefined }) {
  if (!at) return null;
  const d = new Date(at);
  return (
    <time className="gw-msg-time" dateTime={d.toISOString()} title={d.toLocaleString()}>
      {formatMessageTime(at)}
    </time>
  );
}
