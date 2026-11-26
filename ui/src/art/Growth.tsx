// Gardenwall's signature motif: a branching
// node-tree grown by code from a seed, so each model or session gets its own.
// Drawn only behind empty space, slightly blurred and low in contrast. It can
// react to work: `grown` (0..1) is how much of the tree is shown, so it grows
// as steps complete. Reduced motion: drawn still.
import { useEffect, useMemo, useRef, useState } from "react";
import "./growth.css";

interface Segment {
  d: string;
  depth: number;
  tip?: { x: number; y: number; r: number };
}

/** A branch before it is fitted to its space: a quadratic curve. */
interface Raw {
  x0: number;
  y0: number;
  cx: number;
  cy: number;
  x1: number;
  y1: number;
  depth: number;
  tip?: number;
}

/** A small, fast seeded generator (mulberry32) from a string seed. */
function rng(seed: string) {
  let h = 1779033703 ^ seed.length;
  for (let i = 0; i < seed.length; i++) {
    h = Math.imul(h ^ seed.charCodeAt(i), 3432918353);
    h = (h << 13) | (h >>> 19);
  }
  let s = h >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) | 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * Grow a tree from `seed`, then scale and place the whole tree so it fits
 * inside `width` × `height` with a small margin (never clipped, whatever the
 * seed), its trunk rising from the bottom near `anchor`. The fit uses the
 * full tree, so a partly grown tree keeps its size as it grows.
 */
function grow(seed: string, width: number, height: number, roots: number, depth: number, anchor: number): Segment[] {
  const r = rng(seed);
  const raw: Raw[] = [];
  const branch = (x: number, y: number, a: number, len: number, d: number, level: number) => {
    if (d < 0 || len < 4) return;
    const bend = (r() - 0.5) * 0.7;
    const x1 = x + Math.cos(a) * len;
    const y1 = y + Math.sin(a) * len;
    const cx = x + Math.cos(a + bend) * len * 0.5;
    const cy = y + Math.sin(a + bend) * len * 0.5;
    const tip = d < 1 ? 2.6 : r() < 0.16 ? 1.8 : undefined;
    raw.push({ x0: x, y0: y, cx, cy, x1, y1, depth: level, tip });
    // Fork often and wide, like the original sketch: a trunk with a few
    // long limbs, then a fan of finer twigs with buds at their ends.
    const kids = d > 3 ? (r() < 0.72 ? 2 : 1) : r() < 0.65 ? 2 : 3;
    const spread = d > 3 ? 1.25 : 1.6;
    for (let i = 0; i < kids; i++) {
      const side = kids === 1 ? 0 : (i / (kids - 1) - 0.5) * spread;
      // Branches lean outward but are pulled back toward upright, so the crown fans out above the trunk.
      const lean = a + side + (r() - 0.5) * 0.45;
      const upright = lean + (-Math.PI / 2 - lean) * 0.16;
      branch(x1, y1, upright, len * (0.74 + r() * 0.16), d - 1, level + 1);
    }
  };
  const unit = 100;
  for (let i = 0; i < roots; i++) {
    const x = (i - (roots - 1) / 2) * unit * 0.55 + (r() - 0.5) * unit * 0.12;
    branch(x, 0, -Math.PI / 2 + (r() - 0.5) * 0.3, unit * (i === 0 ? 0.5 : 0.36), depth, 0);
  }

  // The tree's extent, curves included.
  let minX = Infinity, maxX = -Infinity, minY = Infinity;
  for (const s of raw) {
    minX = Math.min(minX, s.x0, s.cx, s.x1);
    maxX = Math.max(maxX, s.x0, s.cx, s.x1);
    minY = Math.min(minY, s.y0, s.cy, s.y1);
  }
  const margin = Math.max(4, Math.min(width, height) * 0.03);
  const treeW = Math.max(1, maxX - minX);
  const treeH = Math.max(1, -minY);
  const scale = Math.min((width - 2 * margin) / treeW, (height - margin) / treeH);
  const half = (treeW * scale) / 2;
  const centre = Math.min(Math.max(width * anchor, half + margin), width - half - margin);
  const mid = (minX + maxX) / 2;
  const X = (x: number) => centre + (x - mid) * scale;
  const Y = (y: number) => height + y * scale;
  const f = (n: number) => n.toFixed(1);
  const out: Segment[] = raw.map((s) => ({
    d: `M${f(X(s.x0))} ${f(Y(s.y0))}Q${f(X(s.cx))} ${f(Y(s.cy))} ${f(X(s.x1))} ${f(Y(s.y1))}`,
    depth: s.depth,
    tip: s.tip ? { x: X(s.x1), y: Y(s.y1), r: s.tip } : undefined,
  }));
  // Grow outward: shallow segments first.
  return out.sort((a, b) => a.depth - b.depth);
}

/** How long branches take to fade out when the tree shrinks. */
const RECEDE_MS = 2300;

export function Growth({
  seed,
  width = 680,
  height = 360,
  roots = 2,
  depth = 6,
  anchor = 0.7,
  grown = 1,
  animate = false,
  fit = false,
  className = "",
}: {
  seed: string;
  width?: number;
  height?: number;
  roots?: number;
  depth?: number;
  /** Where the trunks rise, as a fraction of the width (0 left, 1 right). */
  anchor?: number;
  /** How much of the tree to show, 0..1 (e.g. agent steps done). */
  grown?: number;
  /** Draw the branches in once (on load). */
  animate?: boolean;
  /** Grow the tree to the shape of the box it is drawn in (a tall box gives
   * a tall tree), instead of the fixed `width` × `height`. The same seed
   * keeps the same tree; only its proportions follow the box. */
  fit?: boolean;
  className?: string;
}) {
  const svg = useRef<SVGSVGElement>(null);
  const [box, setBox] = useState<{ w: number; h: number } | null>(null);
  useEffect(() => {
    if (!fit || !svg.current) return;
    const el = svg.current;
    // Rounded so small resizes don't regrow the tree.
    const measure = () => {
      const w = Math.round(el.clientWidth / 24) * 24;
      const h = Math.round(el.clientHeight / 24) * 24;
      if (w > 0 && h > 0) setBox((b) => (b && b.w === w && b.h === h ? b : { w, h }));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [fit]);
  const W = fit && box ? box.w : width;
  const H = fit && box ? box.h : height;
  const segments = useMemo(() => grow(seed, W, H, roots, depth, anchor), [seed, W, H, roots, depth, anchor]);
  const shown = segments.slice(0, Math.max(1, Math.round(segments.length * Math.min(1, Math.max(0, grown)))));
  const newest = shown.length - 1;
  // Branches added since the last render draw themselves in, each after its
  // parent, so the tree grows smoothly rather than jumping (round 4).
  // `from` stays put while they draw, so a re-render meanwhile (polling)
  // doesn't cut the animation short.
  // Branches taken away (the tree shrinking) fade out, outermost first,
  // instead of vanishing.
  const growth = useRef<{ seed: string; count: number; from: number; at: number; gone: number; goneAt: number } | null>(null);
  const [, setTick] = useState(0);
  const now = performance.now();
  let g = growth.current;
  if (!g || g.seed !== seed) {
    g = { seed, count: shown.length, from: animate ? 0 : shown.length, at: now, gone: 0, goneAt: 0 };
    growth.current = g;
  } else if (shown.length > g.count) {
    g.from = g.count;
    g.count = shown.length;
    g.at = now;
    g.gone = 0;
  } else if (shown.length < g.count) {
    g.gone = Math.max(g.gone, g.count);
    g.goneAt = now;
    g.count = shown.length;
    g.from = shown.length;
  }
  const leaving = g.gone > shown.length && now - g.goneAt < RECEDE_MS ? segments.slice(shown.length, g.gone) : [];
  useEffect(() => {
    if (leaving.length === 0) return;
    const t = window.setTimeout(() => setTick((n) => n + 1), RECEDE_MS + 50);
    return () => window.clearTimeout(t);
  }, [leaving.length]);
  const maxDepth = leaving.reduce((m, x) => Math.max(m, x.depth), 0);
  const fade = (x: Segment) => ({ animationDelay: `${(maxDepth - x.depth) * 180}ms` });
  const settled = now - g.at > 4000;
  const firstNew = settled ? shown.length : Math.min(g.from, shown.length);
  const baseDepth = shown[firstNew]?.depth ?? 0;
  const delay = (s: Segment, i: number) => (i >= firstNew ? Math.max(0, s.depth - baseDepth) * 140 : 0);
  return (
    <svg
      ref={svg}
      className={`gw-growth ${className}`}
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="xMidYMax meet"
      aria-hidden="true"
      focusable="false"
    >
      <g className="gw-growth-lines">
        {shown.map((s, i) => (
          <path
            key={`${seed}-${i}`}
            d={s.d}
            pathLength={1}
            className={i >= firstNew ? "fresh" : undefined}
            style={i >= firstNew ? { animationDelay: `${delay(s, i)}ms` } : undefined}
          />
        ))}
        {leaving.map((x, i) => (
          <path key={`${seed}-gone-${shown.length + i}`} d={x.d} className="leaving" style={fade(x)} />
        ))}
      </g>
      <g className="gw-growth-nodes">
        {shown.map((s, i) =>
          s.tip ? (
            <circle
              key={`${seed}-${i}`}
              cx={s.tip.x}
              cy={s.tip.y}
              r={s.tip.r}
              className={[i === newest ? "newest" : "", i >= firstNew ? "fresh" : ""].join(" ").trim() || undefined}
              style={i >= firstNew ? { animationDelay: `${delay(s, i) + 500}ms` } : undefined}
            />
          ) : null,
        )}
        {leaving.map((x, i) =>
          x.tip ? <circle key={`${seed}-gone-${shown.length + i}`} cx={x.tip.x} cy={x.tip.y} r={x.tip.r} className="leaving" style={fade(x)} /> : null,
        )}
      </g>
    </svg>
  );
}
