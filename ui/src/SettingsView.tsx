import { ReactNode, useEffect, useRef, useState } from "react";
import { api, AppView, AuditEntry, errorText, Generation } from "./api";
import { McpSettings } from "./McpSettings";
import { BinarySettings } from "./BinarySettings";
import { AgentSettingsSection } from "./AgentSettingsSection";
import { SkillsSettings } from "./SkillsSettings";
import { WebSettingsSection } from "./WebSettingsSection";
import { VoiceSettingsSection } from "./VoiceSettings";
import { Term } from "./modelGuide";
import { UnsavedBanner, useUnsaved } from "./unsavedSettings";
import { Button, CopyButton, DataTable, Fold, Pill, SettingGroup, SettingRow, Toggle, useToast } from "./ui";
import { shownPath } from "./paths";
import { BackupSettings } from "./BackupSettings";
import { LockdownDialog } from "./Lockdown";

type NumericKey = "temperature" | "topP" | "topK" | "minP" | "maxTokens";

const FIELDS: { key: NumericKey; label: string; hint: string; guide: string; integer: boolean }[] = [
  { key: "temperature", label: "Temperature", hint: "0–5. Lower is steadier, higher is more varied.", guide: "temperature", integer: false },
  { key: "topP", label: "Top-p", hint: "0–1. Picks only from the likeliest words.", guide: "top-p", integer: false },
  { key: "topK", label: "Top-k", hint: "0–1000. At most this many candidate words.", guide: "top-k", integer: true },
  { key: "minP", label: "Min-p", hint: "0–1. Drops words far less likely than the top one.", guide: "min-p", integer: false },
  { key: "maxTokens", label: "Max response tokens", hint: "Empty for no limit.", guide: "max-tokens", integer: true },
];

function toText(v: number | null): string {
  return v === null ? "" : String(v);
}

function GenerationSettings({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const g = state.generation;
  const [prompt, setPrompt] = useState(g.defaultSystemPrompt);
  const [values, setValues] = useState<Record<NumericKey, string>>({
    temperature: toText(g.temperature),
    topP: toText(g.topP),
    topK: toText(g.topK),
    minP: toText(g.minP),
    maxTokens: toText(g.maxTokens),
  });
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const changed =
    prompt !== g.defaultSystemPrompt || FIELDS.some((f) => values[f.key].trim() !== toText(g[f.key]));

  const save = async (): Promise<boolean> => {
    setError(null);
    setStatus(null);
    const parsed: Partial<Generation> = {};
    for (const f of FIELDS) {
      const raw = values[f.key].trim();
      if (raw === "") {
        parsed[f.key] = null;
        continue;
      }
      const n = Number(raw);
      if (!Number.isFinite(n) || (f.integer && !Number.isInteger(n))) {
        setError(`${f.label} must be ${f.integer ? "a whole number" : "a number"}.`);
        document.getElementById("generation")?.scrollIntoView({ behavior: "smooth", block: "start" });
        return false;
      }
      parsed[f.key] = n;
    }
    try {
      await api.setGeneration({ ...(parsed as Generation), defaultSystemPrompt: prompt });
      await refresh();
      setStatus("Saved");
      return true;
    } catch (e) {
      setError(errorText(e));
      return false;
    }
  };
  useUnsaved("generation", changed, {
    label: "Generation",
    save,
    discard: () => {
      setPrompt(g.defaultSystemPrompt);
      setValues({
        temperature: toText(g.temperature),
        topP: toText(g.topP),
        topK: toText(g.topK),
        minP: toText(g.minP),
        maxTokens: toText(g.maxTokens),
      });
    },
  });

  return (
    <section>
      <h2>Generation</h2>
      <p className="small muted">
        Empty fields use the model's own defaults. Changes apply to the next message in every chat.
      </p>
      {error && <div className="banner error">{error}</div>}
      <SettingGroup>
        <div className="gw-row gw-row-stack">
          <div className="gw-row-text">
            <label className="gw-row-label" htmlFor="default-prompt">
              Default system prompt for new chats
            </label>
            <textarea id="default-prompt" rows={3} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="None" />
          </div>
        </div>
        {FIELDS.map((f) => (
          <SettingRow
            key={f.key}
            label={f.label}
            help={
              <>
                {f.hint} <Term id={f.guide}>{""}</Term>
              </>
            }
          >
            <input
              className="narrow"
              inputMode="decimal"
              aria-label={f.label}
              value={values[f.key]}
              placeholder="Model default"
              onChange={(e) => setValues({ ...values, [f.key]: e.target.value })}
            />
          </SettingRow>
        ))}
      </SettingGroup>
      {(changed || status) && (
        <div className="row">
          {changed && (
            <Button variant="primary" onClick={() => void save()}>
              Save
            </Button>
          )}
          {status && !changed && <span className="small muted">{status}</span>}
        </div>
      )}
    </section>
  );
}

const EVENT_TEXT: Record<string, string> = {
  SessionStarted: "Agent session started",
  SessionEnded: "Agent session ended",
  Proposed: "Approval requested",
  Approved: "You allowed it",
  Rejected: "You denied it",
  Cancelled: "Request cancelled",
  Expired: "Request expired",
  Claimed: "Approved action started",
  Outcome: "Approved action finished",
  SecurityFlag: "Gardenwall flagged something",
};

const KIND_TEXT: Record<string, string> = {
  RevealSecret: "Read a protected file",
  WriteSecret: "Change a protected file",
  PersistenceRisk: "Change a file that can run commands later",
  OutsideRead: "Read outside the granted folders",
  OutsideWrite: "Change a file outside the granted folders",
  NetworkCommand: "Run a command that uses the network",
  Egress: "Send data off this computer",
  McpTool: "Use an MCP server tool",
  RiskyCommand: "Run a command that may change what runs later",
  HeldBack: "Keep a command's changes to protected files",
  KeepRunning: "Let programs keep running in the sandbox",
  InitRepository: "Create a Git repository",
  AddToolchain: "Add a toolchain",
  SecretThenOutbound: "A secret was read, then data was passed outside the session",
  SecretInCommit: "A commit contains what looks like a secret",
};

/** Plain-language detail for an audit entry (the log never holds paths). */
function detailText(e: AuditEntry): string {
  const r = e.reason ?? "";
  switch (e.event) {
    case "Proposed":
      return r ? `Requested by the ${r} tool` : "";
    case "Outcome":
      return r === "Completed" ? "Completed successfully" : r === "Failed" ? "Failed" : r;
    case "SessionEnded":
      return (
        { User: "You ended it", Restart: "Gardenwall restarted", Idle: "30 minutes without activity", Absolute: "12-hour limit reached" }[r] ?? r
      );
    case "Cancelled":
      return (
        { Restart: "Gardenwall restarted before it was answered", SessionEnded: "The session ended first", Requested: "The agent was stopped" }[r] ?? r
      );
    case "Expired":
      return { ProposalDeadline: "Not answered within 5 minutes", ClaimDeadline: "Allowed, but not used within 30 seconds" }[r] ?? r;
    default:
      return r;
  }
}

function AgentTools({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const [error, setError] = useState<string | null>(null);
  const act = async (f: () => Promise<void>) => {
    setError(null);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <section>
      <h2>Agent tools</h2>
      <Fold summary="Folders with programs the agent may run, such as Python, Node.js or Rust. It can run them but not change them.">
        They are added to the agent's PATH, along with a <span className="mono">bin</span> subfolder if present, for
        agent sessions started afterwards. Launchers such as npm, npx and other .cmd files work too.
      </Fold>
      {error && <div className="banner error">{error}</div>}
      <SettingGroup>
        {state.agentToolFolders.length === 0 && (
          <SettingRow label="No tool folders yet" help="Add one to let the agent run its programs.">
            <span />
          </SettingRow>
        )}
        {state.agentToolFolders.map((p) => (
          <SettingRow
            key={p}
            label={<span className="mono gw-tool-folder">{shownPath(p)}</span>}
            help={
              (state.agentToolFoldersUnusable ?? []).includes(p) ? (
                <>
                  <span className="gated-note">The sandbox can't read this folder yet, so sessions leave it out.</span>{" "}
                  Its installer set permissions only an administrator can change, and Gardenwall never changes a system
                  folder's permissions itself. To let sandboxes read and run it (but not change it), run this once in an
                  administrator terminal, then start a new session:
                  <pre className="approval-pre">{`icacls "${shownPath(p)}" /grant "*S-1-15-2-1:(OI)(CI)RX"`}</pre>
                </>
              ) : (
                "Read-only for the agent, and on its PATH."
              )
            }
          >
            <Button size="sm" variant="ghost" onClick={() => void act(() => api.removeAgentToolFolder(p))}>
              Remove
            </Button>
          </SettingRow>
        ))}
      </SettingGroup>
      <Button onClick={() => void act(() => api.addAgentToolFolder())}>Add tool folder…</Button>
    </section>
  );
}

function ActivityLog() {
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api
      .getAuditLog(200)
      .then(setEntries)
      .catch((e) => setError(errorText(e)));
  }, []);
  return (
    <section>
      <h2>Activity log</h2>
      <p className="small muted">
        Every agent session, approval request, decision and result, kept for 30 days. By design only the kind of
        action is recorded, never paths, file contents or messages; the exact details are shown in the approval
        dialog when you decide.
      </p>
      {error && <div className="banner error">{error}</div>}
      {entries && (
        <DataTable<AuditEntry & { i: number }>
          rows={entries.map((e, i) => ({ ...e, i }))}
          rowKey={(e) => String(e.i)}
          empty={<p className="small muted">Nothing yet.</p>}
          columns={[
            { key: "when", header: "When", width: "190px", render: (e) => <span className="gw-nowrap">{new Date(e.atMs).toLocaleString()}</span> },
            { key: "what", header: "What happened", minWidth: "160px", render: (e) => EVENT_TEXT[e.event] ?? e.event },
            { key: "action", header: "Action", minWidth: "200px", render: (e) => (e.kind ? (KIND_TEXT[e.kind] ?? e.kind) : "") },
            { key: "detail", header: "Detail", minWidth: "120px", render: (e) => <span className="muted gw-cell-wrap">{detailText(e)}</span> },
          ]}
        />
      )}
    </section>
  );
}

export function SettingsView({
  state,
  refresh,
  onCategory,
}: {
  state: AppView;
  refresh: () => Promise<void>;
  /** The category on screen (for the "?" beside the page title, which opens its Help topic). */
  onCategory?: (id: string) => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [lockdownDialog, setLockdownDialog] = useState<"on" | "off" | null>(null);
  const [active, setActive] = useState(SECTIONS[0]![0]);
  useEffect(() => onCategory?.(active), [active, onCategory]);
  const pageRef = useRef<HTMLDivElement>(null);
  const toast = useToast();

  // One category per page. Every section stays
  // mounted (hidden when not shown) so an unsaved draft survives switching.
  const group = (id: string, child: ReactNode) => (
    <div id={id} className="settings-group gw-settings-page" hidden={active !== id}>
      {child}
    </div>
  );

  return (
    <div className="page gw-settings" ref={pageRef}>
      <div className="settings-layout">
        <nav className="section-index gw-settings-nav" aria-label="Settings categories">
          {SECTIONS.map(([id, label]) => (
            <button
              key={id}
              className={active === id ? "active" : undefined}
              aria-current={active === id ? "page" : undefined}
              onClick={() => {
                setActive(id);
                pageRef.current?.scrollTo({ top: 0 });
              }}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="settings-body">
          <UnsavedBanner />
          {error && <div className="banner error">{error}</div>}
          {group("generation", <GenerationSettings state={state} refresh={refresh} />)}
          {group(
            "privacy",
            <section>
              <h2>Privacy</h2>
              <SettingGroup>
                <SettingRow
                  label="Offline mode"
                  help="Disables all model-hub access (search and downloads). Models can still be imported from local files. Models never have network access, regardless of this setting."
                >
                  <Toggle
                    label="Offline mode"
                    checked={state.offline}
                    disabled={state.lockdown}
                    onChange={async (on) => {
                      try {
                        await api.setOffline(on);
                        await refresh();
                        toast({ text: on ? "Offline mode is on" : "Offline mode is off", tone: "ok", action: { label: "Undo", run: () => void api.setOffline(!on).then(refresh) } });
                      } catch (err) {
                        setError(errorText(err));
                      }
                    }}
                  />
                </SettingRow>
                <SettingRow
                  label="Lockdown mode"
                  help={
                    <>
                      An emergency stop for a model that seems to be a risk (also the shield in the top bar, and Ctrl+K →
                      Lockdown). It asks once, then stops everything at once: agent sessions end (their commands and
                      programs are killed, their cards withdrawn, their permissions taken back), web requests, chat Web
                      switches, research assistants, MCP servers and downloads stop, and the model is unloaded. While it is
                      on, only chat works, with no tools, web or memories, and offline mode is on. To turn it off you type
                      UNLOCK; nothing restarts by itself, and your other settings apply again as you set them.
                      <span className="block" style={{ marginTop: 6 }}>
                        For the strongest isolation, also run Gardenwall inside a virtual machine. On Windows a virtual
                        machine usually can't use the graphics card, so the model runs much slower there.
                      </span>
                    </>
                  }
                >
                  <Toggle
                    label="Lockdown mode"
                    checked={state.lockdown}
                    onChange={(on) => setLockdownDialog(on ? "on" : "off")}
                  />
                  {lockdownDialog && (
                    <LockdownDialog mode={lockdownDialog} onClose={() => setLockdownDialog(null)} onChanged={() => void refresh()} />
                  )}
                </SettingRow>
              </SettingGroup>
            </section>,
          )}
          {group("web", <WebSettingsSection state={state} refresh={refresh} />)}
          {group("voice", <VoiceSettingsSection state={state} refresh={refresh} />)}
          {group("mcp", <McpSettings />)}
          {group("skills", <SkillsSettings />)}
          {group("binary", <BinarySettings />)}
          {group("agent", <AgentSettingsSection state={state} refresh={refresh} />)}
          {group("agent-tools", <AgentTools state={state} refresh={refresh} />)}
          {group("activity", <ActivityLog />)}
          {group(
            "storage",
            <section>
              <h2>Data</h2>
              <SettingGroup>
                <SettingRow label="Where your data is" help="Models, runtimes, settings, and chats are stored only on this computer.">
                  <span className="mono small gw-settings-path" title={shownPath(state.dataDir)}>
                    {shownPath(state.dataDir)}
                  </span>
                  <CopyButton text={shownPath(state.dataDir)} label="Copy the folder's path" />
                </SettingRow>
                <SettingRow
                  label="Chats"
                  help={
                    state.chatEncryption === "encrypted" ? (
                      <>
                        Encrypted with a key only your Windows account on this computer can unlock.{" "}
                        <details className="explain gw-inline-explain">
                          <summary>What this protects</summary>
                          <div className="explain-body">
                            Messages, titles and system prompts are encrypted, so copies of the file (backups, another
                            account, a removed disk) can't be read. Software running as you can still read them, as
                            Gardenwall does. Deleted chats are overwritten, not just hidden.
                          </div>
                        </details>
                      </>
                    )
                      : state.chatEncryption === "unencrypted"
                        ? "Not encrypted by Gardenwall on this platform yet. Use your operating system's disk encryption to protect them at rest. Deleted chats are overwritten, not just hidden."
                        : "Saved chats are encrypted with a key this account cannot unlock (for example after a Windows profile reset). They are kept unchanged and cannot be read or edited here."
                  }
                >
                  <Pill tone={state.chatEncryption === "encrypted" ? "ok" : state.chatEncryption === "locked" ? "warn" : "neutral"}>
                    {state.chatEncryption === "encrypted" ? "Encrypted" : state.chatEncryption === "locked" ? "Locked" : "Not encrypted"}
                  </Pill>
                </SettingRow>
              </SettingGroup>
              <BackupSettings />
            </section>,
          )}
        </div>
      </div>
    </div>
  );
}

const SECTIONS: [string, string][] = [
  ["generation", "Generation"],
  ["privacy", "Privacy"],
  ["web", "Web access"],
  ["voice", "Voice"],
  ["mcp", "MCP servers"],
  ["skills", "Skills"],
  ["binary", "Binary analysis"],
  ["agent", "Agent"],
  ["agent-tools", "Agent tools"],
  ["activity", "Activity log"],
  ["storage", "Data"],
];
