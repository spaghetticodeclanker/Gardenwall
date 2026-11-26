// The working indicator (round 5: option B "bud chase", the
// medium weight): a small vector sprig whose three buds light in turn.
// Stems keep the same on-screen thickness at any size (non-scaling stroke);
// reduced motion shows it still, every bud lit.
import "./sprig.css";

/** Stem width on screen and bud radius (viewBox units) for a size, as designed at 16, 20 and 72 px. */
function weights(size: number): { stroke: number; bud: number } {
  if (size <= 16) return { stroke: 1.25, bud: 2.2 };
  if (size <= 24) return { stroke: 1.45, bud: 2 };
  return { stroke: Math.max(1.6, (2.4 * size) / 72), bud: 1.5 };
}

export function Sprig({ size = 16, label, className = "" }: { size?: number; label?: string; className?: string }) {
  const { stroke, bud } = weights(size);
  return (
    <svg
      className={`gw-sprig ${className}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      <path
        className="gw-sprig-stem"
        style={{ strokeWidth: `${stroke}px` }}
        d="M12 22 Q12.4 16.5 12 12.5 M12 15.2 Q9 12.8 7.2 9.6 M12 13.6 Q15 11.6 16.8 8.6 M12 12.5 Q12 8.8 12.4 5.8"
      />
      <circle className="gw-sprig-bud" cx="7.2" cy="9.6" r={bud} />
      <circle className="gw-sprig-bud" cx="12.4" cy="5.8" r={bud} />
      <circle className="gw-sprig-bud" cx="16.8" cy="8.6" r={bud} />
    </svg>
  );
}
