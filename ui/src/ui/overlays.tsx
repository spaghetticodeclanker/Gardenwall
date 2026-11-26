// Menus, the right-hand inspector and toasts for the redesigned UI (Phase 0).
import React, { createContext, ReactNode, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { X, type LucideIcon } from "lucide-react";
import { IconButton } from "./controls";

/**
 * Draw a dialog at the top of the page, so no panel or isolated area it was
 * opened from can cover it (a dialog inside the session would otherwise sit
 * under the session details panel).
 */
export function toBody(node: ReactNode) {
  return createPortal(node, document.body);
}

/**
 * Right-hand panels (the inspector, the side chat) report how much of the
 * window's right edge they cover, so the toast corner can sit beside them
 * instead of over their inputs. A panel
 * that is hidden (its page kept mounted out of sight) covers nothing.
 *
 * The side chat takes the window's right edge; a details panel open at the
 * same time docks to its left (final review item 6), so a panel counts as
 * covering the edge when it reaches the side chat, and then covers everything
 * from its left edge to the window's.
 */
const rightPanels = new Map<object, () => number>();
function sideChatWidth() {
  const v = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--gw-side-chat"));
  return Number.isFinite(v) ? v : 0;
}
export function publishRightPanels() {
  let widest = 0;
  for (const measure of rightPanels.values()) widest = Math.max(widest, measure());
  document.documentElement.style.setProperty("--gw-right-panel", `${Math.round(widest)}px`);
}
export function useRightPanel(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const key = {};
    rightPanels.set(key, () => {
      const r = el.getBoundingClientRect();
      if (r.width === 0) return 0;
      const edge = el.classList.contains("side-chat") ? window.innerWidth : window.innerWidth - sideChatWidth();
      return r.right >= edge - 2 ? window.innerWidth - r.left : 0;
    });
    publishRightPanels();
    const watch = typeof ResizeObserver !== "undefined" ? new ResizeObserver(publishRightPanels) : null;
    watch?.observe(el);
    window.addEventListener("resize", publishRightPanels);
    // A panel sliding in or moving beside the side chat ends where it settles.
    el.addEventListener("animationend", publishRightPanels);
    el.addEventListener("transitionend", publishRightPanels);
    return () => {
      watch?.disconnect();
      window.removeEventListener("resize", publishRightPanels);
      el.removeEventListener("animationend", publishRightPanels);
      el.removeEventListener("transitionend", publishRightPanels);
      rightPanels.delete(key);
      publishRightPanels();
    };
  }, [ref]);
}

/**
 * A message box (the agent's or the chat's) reports its top edge, so the
 * corner column (notices and session cards) sits just above it and never
 * covers it, also in short windows (final review item 7). A hidden page's box
 * measures nothing.
 */
const cornerFloors = new Map<object, HTMLElement>();
function publishCornerFloor() {
  let floor = 0;
  for (const el of cornerFloors.values()) {
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) floor = Math.max(floor, window.innerHeight - r.top);
  }
  const root = document.documentElement;
  if (floor > 0) root.style.setProperty("--gw-corner-floor", `${Math.round(floor + 8)}px`);
  else root.style.removeProperty("--gw-corner-floor");
}
export function useCornerFloor(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const key = {};
    cornerFloors.set(key, el);
    publishCornerFloor();
    const watch = typeof ResizeObserver !== "undefined" ? new ResizeObserver(() => requestAnimationFrame(publishCornerFloor)) : null;
    watch?.observe(el);
    window.addEventListener("resize", publishCornerFloor);
    return () => {
      watch?.disconnect();
      window.removeEventListener("resize", publishCornerFloor);
      cornerFloors.delete(key);
      publishCornerFloor();
    };
  }, [ref]);
}

export interface MenuItem {
  label: string;
  icon?: LucideIcon;
  onSelect: () => void;
  /** A short note on the right (a count, a shortcut, "on"). */
  hint?: ReactNode;
  /** A longer explanation, shown as the item's tooltip. */
  title?: string;
  danger?: boolean;
  /** Shown but not usable yet (its hint says when it will be). */
  disabled?: boolean;
}

/**
 * A button that opens a list of actions (the gear menu, the composer's + menu).
 * Closes on Escape, on a click outside and after a choice; arrow keys move.
 */
export function Menu({
  trigger,
  items,
  align = "end",
  label,
}: {
  trigger: (props: { onClick: () => void; "aria-expanded": boolean; "aria-haspopup": "menu" }) => ReactNode;
  items: (MenuItem | "separator")[];
  align?: "start" | "end";
  label: string;
}) {
  const [open, setOpen] = useState(false);
  const [up, setUp] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  // Before it shows: open upward when it doesn't fit below and there is
  // more room above (the last rows of a list).
  useLayoutEffect(() => {
    if (!open) {
      setUp(false);
      return;
    }
    const anchor = box.current?.getBoundingClientRect();
    const height = list.current?.offsetHeight ?? 0;
    if (!anchor) return;
    const below = window.innerHeight - anchor.bottom;
    setUp(height + 14 > below && anchor.top > below);
  }, [open]);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const els = [...(list.current?.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)") ?? [])];
        const i = els.indexOf(document.activeElement as HTMLButtonElement);
        els[(i + (e.key === "ArrowDown" ? 1 : els.length - 1)) % els.length]?.focus();
      }
    };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", key);
    list.current?.querySelector<HTMLButtonElement>("[role=menuitem]:not(:disabled)")?.focus({ preventScroll: true });
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", key);
    };
  }, [open]);
  return (
    <div className="gw-menu-anchor" ref={box}>
      {trigger({ onClick: () => setOpen((o) => !o), "aria-expanded": open, "aria-haspopup": "menu" })}
      {open && (
        <div className={`gw-menu gw-menu-${align}${up ? " gw-menu-up" : ""}`} role="menu" aria-label={label} ref={list}>
          {items.map((it, i) =>
            it === "separator" ? (
              <div key={`sep${i}`} className="gw-menu-sep" role="separator" />
            ) : (
              <button
                key={it.label}
                type="button"
                role="menuitem"
                className={`gw-menu-item${it.danger ? " danger" : ""}`}
                title={it.title}
                disabled={it.disabled}
                onClick={() => {
                  setOpen(false);
                  it.onSelect();
                }}
              >
                {it.icon && <it.icon aria-hidden="true" size={16} strokeWidth={1.75} />}
                <span className="gw-menu-label">{it.label}</span>
                {it.hint && <span className="gw-menu-hint">{it.hint}</span>}
              </button>
            ),
          )}
        </div>
      )}
    </div>
  );
}

/**
 * A right-click menu (also Shift+F10 or the context-menu key): `open` from
 * an `onContextMenu` or `onKeyDown` handler, and render `node` anywhere.
 * Same look and keys as `Menu`; closes on Escape, a click elsewhere or a
 * choice, and stays inside the window.
 */
export function useContextMenu() {
  const [menu, setMenu] = useState<{ x: number; y: number; label: string; items: (MenuItem | "separator")[] } | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const close = useCallback(() => {
    setMenu(null);
    setPos(null);
  }, []);
  const open = useCallback(
    (e: React.MouseEvent | React.KeyboardEvent, label: string, items: (MenuItem | "separator")[]) => {
      if ("key" in e) {
        if (!(e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey))) return;
      }
      e.preventDefault();
      e.stopPropagation();
      let x: number;
      let y: number;
      if ("clientX" in e && e.clientX) {
        x = e.clientX;
        y = e.clientY;
      } else {
        const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
        x = r.left + 12;
        y = r.bottom;
      }
      setPos(null);
      setMenu({ x, y, label, items });
    },
    [],
  );
  useEffect(() => {
    if (!menu) return;
    const el = list.current;
    if (el) {
      const w = el.offsetWidth;
      const h = el.offsetHeight;
      setPos({
        left: Math.max(8, Math.min(menu.x, window.innerWidth - w - 8)),
        top: Math.max(8, menu.y + h > window.innerHeight - 8 ? menu.y - h : menu.y),
      });
      el.querySelector<HTMLButtonElement>("[role=menuitem]:not(:disabled)")?.focus({ preventScroll: true });
    }
    const away = (e: MouseEvent) => {
      if (!list.current?.contains(e.target as Node)) close();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const els = [...(list.current?.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)") ?? [])];
        const i = els.indexOf(document.activeElement as HTMLButtonElement);
        els[(i + (e.key === "ArrowDown" ? 1 : els.length - 1)) % els.length]?.focus();
      }
    };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", key);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", key);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, [menu, close]);
  const node = menu ? (
    <div
      className="gw-menu gw-context-menu"
      role="menu"
      aria-label={menu.label}
      ref={list}
      style={{ position: "fixed", left: pos?.left ?? menu.x, top: pos?.top ?? menu.y, visibility: pos ? "visible" : "hidden" }}
    >
      {menu.items.map((it, i) =>
        it === "separator" ? (
          <div key={`sep${i}`} className="gw-menu-sep" role="separator" />
        ) : (
          <button
            key={it.label}
            type="button"
            role="menuitem"
            className={`gw-menu-item${it.danger ? " danger" : ""}`}
            title={it.title}
            disabled={it.disabled}
            onClick={() => {
              close();
              it.onSelect();
            }}
          >
            {it.icon && <it.icon aria-hidden="true" size={16} strokeWidth={1.75} />}
            <span className="gw-menu-label">{it.label}</span>
            {it.hint && <span className="gw-menu-hint">{it.hint}</span>}
          </button>
        ),
      )}
    </div>
  ) : null;
  return { open, node };
}

/** A name being edited in a list: Enter or leaving the field saves, Escape cancels. */
export function RenameField({ value, onSave, onCancel, label }: { value: string; onSave: (v: string) => void; onCancel: () => void; label: string }) {
  const [draft, setDraft] = useState(value);
  const done = useRef(false);
  return (
    <input
      className="gw-rename-field"
      autoFocus
      aria-label={label}
      value={draft}
      maxLength={200}
      onFocus={(e) => e.currentTarget.select()}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => {
        if (done.current) return;
        done.current = true;
        if (draft.trim() && draft.trim() !== value) onSave(draft.trim());
        else onCancel();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") {
          done.current = true;
          onCancel();
        }
      }}
    />
  );
}

export interface InspectorTab {
  id: string;
  label: string;
  /** A count or short status beside the tab name. */
  badge?: ReactNode;
  content: ReactNode;
}

const INSPECTOR_KEY = "gw.inspector.width";
const INSPECTOR_DEFAULT = 440;
const INSPECTOR_MIN = 320;

function savedInspectorWidth(): number {
  try {
    const w = Number(localStorage.getItem(INSPECTOR_KEY));
    if (Number.isFinite(w) && w >= INSPECTOR_MIN) return Math.min(w, 1200);
  } catch {
    /* no storage: the default */
  }
  return INSPECTOR_DEFAULT;
}

/** The right-hand detail panel with tabs (Agent sessions, Chat, Models, Documents). Drag its left edge to resize; the width is remembered. */
export function Inspector({
  tabs,
  active,
  onActive,
  onClose,
  title,
}: {
  tabs: InspectorTab[];
  active: string;
  onActive: (id: string) => void;
  onClose?: () => void;
  title: string;
}) {
  const current = tabs.find((t) => t.id === active) ?? tabs[0];
  const [width, setWidth] = useState(savedInspectorWidth);
  const panel = useRef<HTMLElement>(null);
  useRightPanel(panel);
  const drag = useRef<{ x: number; w: number } | null>(null);
  const clamp = (w: number) => Math.round(Math.min(Math.max(w, INSPECTOR_MIN), Math.max(INSPECTOR_MIN, window.innerWidth * 0.6)));
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, w: width };
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (drag.current) setWidth(clamp(drag.current.w + drag.current.x - e.clientX));
  };
  const onPointerUp = () => {
    if (!drag.current) return;
    drag.current = null;
    try {
      localStorage.setItem(INSPECTOR_KEY, String(width));
    } catch {
      /* the width is a convenience */
    }
  };
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const w = clamp(width + (e.key === "ArrowLeft" ? step : -step));
    setWidth(w);
    try {
      localStorage.setItem(INSPECTOR_KEY, String(w));
    } catch {
      /* the width is a convenience */
    }
  };
  return (
    <aside ref={panel} className="gw-inspector" aria-label={title} style={{ width: `min(${width}px, 100%)`, ["--gw-inspector-w" as string]: `${width}px` }}>
      <div
        className="gw-inspector-grip"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the panel"
        aria-valuenow={width}
        tabIndex={0}
        title="Drag to resize"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={() => setWidth(INSPECTOR_DEFAULT)}
        onKeyDown={onKeyDown}
      />
      <div className="gw-inspector-head">
        <div className="gw-tabs" role="tablist" aria-label={title}>
          {tabs.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={t.id === current?.id}
              className={`gw-tab${t.id === current?.id ? " active" : ""}`}
              onClick={() => onActive(t.id)}
            >
              {t.label}
              {t.badge !== undefined && <span className="gw-tab-badge">{t.badge}</span>}
            </button>
          ))}
        </div>
        {onClose && <IconButton icon={X} label="Close the panel" onClick={onClose} />}
      </div>
      <div className="gw-inspector-body" role="tabpanel">
        {current?.content}
      </div>
    </aside>
  );
}

export interface Toast {
  id: number;
  text: string;
  tone?: "ok" | "warn" | "danger" | "info";
  action?: { label: string; run: () => void };
  /** Stays until dismissed (things that need a decision). */
  sticky?: boolean;
}

const ToastContext = createContext<(t: Omit<Toast, "id">) => void>(() => {});

/** Brief notices in the bottom-right corner. */
export function ToastHost({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const next = useRef(1);
  const dismiss = useCallback((id: number) => setToasts((ts) => ts.filter((t) => t.id !== id)), []);
  const push = useCallback(
    (t: Omit<Toast, "id">) => {
      const id = next.current++;
      setToasts((ts) => [...ts.slice(-3), { ...t, id }]);
      if (!t.sticky) window.setTimeout(() => dismiss(id), t.action ? 6000 : 3500);
    },
    [dismiss],
  );
  return (
    <ToastContext.Provider value={push}>
      {children}
      <div className="gw-toasts">
        {/* Session corner cards (AttentionCards) render here, above the notices, so nothing overlaps. */}
        <div id="gw-corner" className="gw-corner" />
        <div className="gw-toast-list" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`gw-toast gw-toast-${t.tone ?? "info"}`}>
            <span className="gw-toast-text">{t.text}</span>
            {t.action && (
              <button
                type="button"
                className="gw-toast-action"
                onClick={() => {
                  t.action?.run();
                  dismiss(t.id);
                }}
              >
                {t.action.label}
              </button>
            )}
            <IconButton icon={X} label="Dismiss" onClick={() => dismiss(t.id)} />
          </div>
        ))}
        </div>
      </div>
    </ToastContext.Provider>
  );
}

export const useToast = () => useContext(ToastContext);
