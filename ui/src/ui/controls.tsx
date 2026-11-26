// Shared building blocks for the redesigned UI (docs/internal/design/UI-REDESIGN-PLAN.md,
// Phase 0). Styling lives in ui.css and reads only the tokens in styles.css.
import { ButtonHTMLAttributes, ReactNode, useEffect, useId, useRef, useState } from "react";
import { Check, Copy, type LucideIcon } from "lucide-react";
import { copyText } from "../clipboard";

type Variant = "primary" | "secondary" | "ghost" | "danger";

export function Button({
  variant = "secondary",
  size = "md",
  icon: Icon,
  children,
  className = "",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: "sm" | "md"; icon?: LucideIcon }) {
  return (
    <button type="button" className={`gw-btn gw-btn-${variant} gw-btn-${size} ${className}`} {...rest}>
      {Icon && <Icon aria-hidden="true" size={size === "sm" ? 14 : 16} strokeWidth={1.75} />}
      {children}
    </button>
  );
}

/** An icon-only button. The label is required: it is the accessible name and the tooltip. */
export function IconButton({
  icon: Icon,
  label,
  className = "",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { icon: LucideIcon; label: string }) {
  return (
    <button type="button" className={`gw-icon-btn ${className}`} aria-label={label} title={label} {...rest}>
      <Icon aria-hidden="true" size={16} strokeWidth={1.75} />
    </button>
  );
}

/** Copy to the clipboard: the icon turns into a check for a moment. */
export function CopyButton({ text, label = "Copy" }: { text: string | (() => string); label?: string }) {
  const [done, setDone] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const Icon = done ? Check : Copy;
  return (
    <button
      type="button"
      className={`gw-icon-btn gw-copy${done ? " done" : ""}`}
      aria-label={done ? "Copied" : label}
      title={done ? "Copied" : label}
      onClick={async () => {
        if (await copyText(typeof text === "function" ? text() : text)) {
          setDone(true);
          window.clearTimeout(timer.current);
          timer.current = window.setTimeout(() => setDone(false), 1500);
        }
      }}
    >
      <Icon aria-hidden="true" size={16} strokeWidth={1.75} />
      <span className="gw-sr" aria-live="polite">{done ? "Copied" : ""}</span>
    </button>
  );
}

/** An on/off switch. Saves through onChange; the caller decides when that persists. */
export function Toggle({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={`gw-toggle${checked ? " on" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span className="gw-toggle-knob" />
    </button>
  );
}

/** A group of setting rows: label and help on the left, the control on the right. */
export function SettingGroup({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <section className="gw-group">
      {title && <h3 className="gw-group-title">{title}</h3>}
      <div className="gw-group-body">{children}</div>
    </section>
  );
}

export function SettingRow({
  label,
  help,
  children,
}: {
  label: ReactNode;
  help?: ReactNode;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <div className="gw-row" role="group" aria-labelledby={id}>
      <div className="gw-row-text">
        <div id={id} className="gw-row-label">{label}</div>
        {help && <div className="gw-row-help">{help}</div>}
      </div>
      <div className="gw-row-control">{children}</div>
    </div>
  );
}

/**
 * A one-line summary with the rest folded away behind "More"
 * (short text first, detail one click away). Nothing is removed.
 */
export function Fold({ summary, children, more = "More" }: { summary: ReactNode; children: ReactNode; more?: string }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <div className="gw-fold small muted">
      <p>
        {summary}{" "}
        <button type="button" className="link small gw-fold-toggle" aria-expanded={open} aria-controls={id} onClick={() => setOpen((o) => !o)}>
          {open ? "Less" : more}
        </button>
      </p>
      {open && (
        <div className="gw-fold-more" id={id}>
          {children}
        </div>
      )}
    </div>
  );
}

/** Folded-away extra settings ("Advanced"). */
export function Disclosure({ summary, children }: { summary: string; children: ReactNode }) {
  return (
    <details className="gw-disclosure">
      <summary>{summary}</summary>
      <div className="gw-disclosure-body">{children}</div>
    </details>
  );
}

export function Pill({ tone = "neutral", children }: { tone?: "ok" | "warn" | "danger" | "neutral" | "accent"; children: ReactNode }) {
  return <span className={`gw-pill gw-pill-${tone}`}>{children}</span>;
}
