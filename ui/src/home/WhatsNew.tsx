// "Updated to version X" on Home (final review item 15
// build the place for it now). When the update report shows a
// running version newer than the last one seen on this computer, Home shows
// one dismissible line linking to the Updates page. Until Gardenwall updates
// itself (Phase 6), this only appears after a new build is installed by hand.
// The last seen version is a per-computer convenience (localStorage): if it
// can't be read, the line simply doesn't show.
import { useEffect, useState } from "react";

const KEY = "gw.lastSeenVersion";

/**
 * Is version `a` newer than `b`? Compares the dotted numbers (1.10.0 > 1.9.2).
 * With the same numbers, a release is newer than its pre-release (0.1.0 >
 * 0.1.0-rc.1), and pre-releases compare by their trailing number (rc.2 > rc.1).
 */
export function isNewer(a: string, b: string): boolean {
  const parts = (v: string) => v.split(/[-+]/)[0]!.split(".").map((n) => parseInt(n, 10) || 0);
  const x = parts(a);
  const y = parts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d > 0;
  }
  const pre = (v: string) => v.split("+")[0]!.split("-").slice(1).join("-");
  const pa = pre(a);
  const pb = pre(b);
  if (pa === pb) return false;
  if (!pa) return true;
  if (!pb) return false;
  const num = (p: string) => parseInt(p.match(/(\d+)$/)?.[1] ?? "0", 10);
  return num(pa) > num(pb);
}

function remember(version: string) {
  try {
    localStorage.setItem(KEY, version);
  } catch {
    /* a convenience only */
  }
}

export function WhatsNew({ current, onOpen }: { current: string | null; onOpen: () => void }) {
  const [shown, setShown] = useState<string | null>(null);
  useEffect(() => {
    if (!current) return;
    let seen: string | null = null;
    try {
      seen = localStorage.getItem(KEY);
    } catch {
      return;
    }
    // The first run on this computer, or an older build: nothing to announce.
    if (!seen || !isNewer(current, seen)) {
      if (seen !== current) remember(current);
      return;
    }
    setShown(current);
  }, [current]);
  if (!shown) return null;
  const dismiss = () => {
    remember(shown);
    setShown(null);
  };
  return (
    <div className="banner info dismissible gw-whats-new" role="status">
      <span>
        Updated to version {shown}.{" "}
        <button
          className="link"
          onClick={() => {
            dismiss();
            onOpen();
          }}
        >
          Open Updates
        </button>
      </span>
      <button className="small" onClick={dismiss}>
        Dismiss
      </button>
    </div>
  );
}
