// The redesigned shell's top bar (Phase 1): the Gardenwall mark opens Home,
// Agent and Chat are the two workspaces, the model chip shows what's loaded,
// and every other page sits behind the gear menu.
import { Bot, ChevronLeft, Cpu, FileText, HardDrive, HelpCircle, Lightbulb, Lock, MessageSquare, PanelLeftClose, PanelLeftOpen, RefreshCw, Settings, Shield, ShieldAlert, ShieldCheck, Unplug } from "lucide-react";
import type { AppView } from "../api";
import { placementText } from "../api";
import { helpTopicTitle } from "../HelpView";
import { IconButton, Menu, Pill } from "../ui";
import "./shell.css";

export type Page = "home" | "chat" | "agent" | "documents" | "memory" | "models" | "updates" | "settings" | "help";
/** Where a link can go: a page, or "runtime", the Updates page at Inference runtime (the Runtime page was folded into Updates, final review item 9). */
export type Target = Page | "runtime";
export type Workspace = "agent" | "chat";

export const PAGE_TITLES: Record<Page, string> = {
  home: "Home",
  agent: "Agent",
  chat: "Chat",
  documents: "Documents",
  memory: "Memory",
  models: "Models",
  updates: "Updates",
  settings: "Settings",
  help: "Help",
};

/** The Gardenwall mark: a sprouting node inside the wall. */
export function Mark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 20 20" aria-hidden="true" className="gw-mark">
      <rect x="1.75" y="1.75" width="16.5" height="16.5" rx="4.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M10 15.5V9M10 9 7 6M10 11.2l3-3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <circle cx="7" cy="6" r="1.3" fill="currentColor" />
      <circle cx="13" cy="8.2" r="1.3" fill="currentColor" />
    </svg>
  );
}

function shortContext(n: number) {
  return n >= 1024 ? `${Math.round(n / 1024)}K` : String(n);
}

export function TopBar({
  page,
  go,
  state,
  onUnload,
  updatesBadge,
  updatesSecurity,
  focus,
  onToggleFocus,
  onLockdown,
}: {
  page: Page;
  go: (p: Target) => void;
  state: AppView | null;
  onUnload: () => void;
  updatesBadge: number;
  updatesSecurity: boolean;
  focus: boolean;
  onToggleFocus: () => void;
  /** Open the lockdown dialog: turn on (emergency stop) or, when on, off. */
  onLockdown: () => void;
}) {
  const loaded = state?.loaded ?? null;
  const tab = (w: Workspace, Icon: typeof Bot) => (
    <button
      type="button"
      role="tab"
      aria-selected={page === w}
      className={`gw-ws-tab${page === w ? " active" : ""}`}
      onClick={() => go(w)}
    >
      <Icon aria-hidden="true" size={15} strokeWidth={1.75} />
      {PAGE_TITLES[w]}
    </button>
  );
  return (
    <header className="gw-topbar">
      <button
        type="button"
        className={`gw-brand${page === "home" ? " active" : ""}`}
        onClick={() => go("home")}
        title="Home: your model, sessions and what's waiting for you"
      >
        <Mark />
        <span>Gardenwall</span>
      </button>
      <div className="gw-ws-tabs" role="tablist" aria-label="Workspaces">
        {tab("agent", Bot)}
        {tab("chat", MessageSquare)}
      </div>
      {(page === "chat" || page === "agent") && (
        <IconButton
          icon={focus ? PanelLeftOpen : PanelLeftClose}
          label={
            page === "chat"
              ? focus
                ? "Show the chat list (Ctrl+B)"
                : "Hide the chat list to focus on the conversation (Ctrl+B)"
              : focus
                ? "Show the projects and sessions (Ctrl+B)"
                : "Hide the projects and sessions to focus on the session (Ctrl+B)"
          }
          aria-pressed={focus}
          onClick={onToggleFocus}
        />
      )}
      <div className="gw-topbar-spacer" />
      {state?.lockdown ? (
        <button
          type="button"
          className="gw-lockdown-on"
          onClick={onLockdown}
          title="Lockdown is on: only chat works, with no tools, web or memories. Click to turn it off (you'll type UNLOCK)."
        >
          <Lock aria-hidden="true" size={14} strokeWidth={2} />
          Locked down
        </button>
      ) : (
        <>
          {state?.cleaningUp && (
            <span title="Removing sandbox permissions left from the last time Gardenwall ran. A new agent session starts as soon as this is done.">
              <Pill>Finishing cleanup…</Pill>
            </span>
          )}
          {state?.offline && <Pill>Offline mode</Pill>}
          <IconButton
            icon={Shield}
            label="Lockdown: stop everything now, for a model that seems to be a risk (asks once)"
            onClick={onLockdown}
          />
        </>
      )}
      {loaded ? (
        <Menu
          label="Loaded model"
          trigger={(p) => (
            <button type="button" className="gw-model-chip" title={loaded.sha256} {...p}>
              <span className={`gw-model-dot${loaded.isolation === "sandboxed" ? "" : " danger"}`} />
              <span className="gw-model-name">{loaded.displayName}</span>
              <span className="gw-model-meta">{shortContext(loaded.contextWindow)}</span>
            </button>
          )}
          items={[
            {
              label: `${loaded.contextWindow.toLocaleString()}-token context`,
              icon: Lightbulb,
              onSelect: () => go("models"),
            },
            {
              label: loaded.isolation === "sandboxed" ? "Sandboxed" : "Not isolated",
              icon: loaded.isolation === "sandboxed" ? ShieldCheck : ShieldAlert,
              danger: loaded.isolation !== "sandboxed",
              onSelect: () => go("runtime"),
            },
            ...(loaded.placement
              ? [{ label: `Partly in system RAM (${placementText(loaded.placement)})`, icon: Cpu, onSelect: () => go("models") }]
              : []),
            "separator" as const,
            { label: "Models", icon: HardDrive, onSelect: () => go("models") },
            { label: "Unload", icon: Unplug, onSelect: onUnload },
          ]}
        />
      ) : (
        <button type="button" className="gw-model-chip empty" onClick={() => go("models")}>
          <span className="gw-model-dot off" />
          <span className="gw-model-name">No model loaded</span>
        </button>
      )}
      {/* Security states stay visible outside the menu. */}
      {loaded && loaded.isolation !== "sandboxed" && <Pill tone="danger">Not isolated</Pill>}
      {loaded?.placement && (
        <span title={`Weights: ${placementText(loaded.placement)}`}>
          <Pill tone="warn">Partly in system RAM</Pill>
        </span>
      )}
      <Menu
        label="Gardenwall menu"
        trigger={(p) => (
          <span className="gw-gear">
            <IconButton icon={Settings} label="Menu: models, documents, settings and more" {...p} />
            {updatesBadge > 0 && <span className={`gw-gear-badge${updatesSecurity ? " danger" : ""}`} aria-hidden="true" />}
          </span>
        )}
        items={[
          { label: "Models", icon: HardDrive, onSelect: () => go("models") },
          "separator",
          { label: "Documents", icon: FileText, onSelect: () => go("documents") },
          { label: "Memory", icon: Lightbulb, onSelect: () => go("memory") },
          "separator",
          {
            label: "Updates",
            icon: RefreshCw,
            hint: updatesBadge > 0 ? <span className={updatesSecurity ? "gw-hint-danger" : "gw-hint-warn"}>{updatesBadge}</span> : undefined,
            onSelect: () => go("updates"),
          },
          { label: "Settings", icon: Settings, onSelect: () => go("settings") },
          { label: "Help", icon: HelpCircle, onSelect: () => go("help") },
        ]}
      />
    </header>
  );
}

/** The header of a page behind the gear menu: back to where you were, and the title. */
export function PageHeader({
  page,
  backTo,
  onBack,
  helpTopic,
  onHelp,
}: {
  page: Page;
  backTo: Page;
  onBack: () => void;
  /** The Help topic for this page (or the Settings category on screen): a "?" beside the title opens it. */
  helpTopic?: string;
  onHelp?: (topic: string) => void;
}) {
  return (
    <div className="gw-page-head">
      <button type="button" className="gw-back" onClick={onBack}>
        <ChevronLeft aria-hidden="true" size={16} strokeWidth={1.75} />
        {PAGE_TITLES[backTo]}
      </button>
      <h1 className="gw-page-title">{PAGE_TITLES[page]}</h1>
      {helpTopic && onHelp && (
        <IconButton
          icon={HelpCircle}
          className="gw-page-help"
          label={`Help: ${helpTopicTitle(helpTopic)}`}
          onClick={() => onHelp(helpTopic)}
        />
      )}
    </div>
  );
}
