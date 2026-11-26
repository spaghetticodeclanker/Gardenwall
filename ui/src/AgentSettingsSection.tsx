// Settings → Agent: commands, long tasks, Git, notifications, several sessions,
// and (folded under Advanced) Pi's compaction, retry and system-prompt settings.
//
// Redesign phase 6: grouped rows, label left and
// control right. Switches and menus save at once (with Undo); text and number
// fields use the "unsaved changes" bar (`useUnsaved`) and the leave prompt.
// Changing the shell asks first.
import { useState } from "react";
import { ParentFoldersRow } from "./ParentFolders";
import { AgentSettings, api, AppView, errorText } from "./api";
import { Explain } from "./ModelSettings";
import { useUnsaved } from "./unsavedSettings";
import { Button, Disclosure, Fold, SettingGroup, SettingRow, Toggle, useToast } from "./ui";
import { shownPath } from "./paths";

/** Fields edited as text: these wait for Save; everything else saves at once. */
const TEXT_FIELDS = ["reserveTokens", "keepRecentTokens", "retries", "appendSystemPrompt", "gitName", "gitEmail"] as const;
type TextField = (typeof TEXT_FIELDS)[number];
type TextDraft = Pick<AgentSettings, TextField>;

const pickText = (s: AgentSettings): TextDraft =>
  Object.fromEntries(TEXT_FIELDS.map((k) => [k, s[k]])) as TextDraft;

export function AgentSettingsSection({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const saved = state.agentSettings;
  const [text, setText] = useState<TextDraft>(() => pickText(saved));
  const [error, setError] = useState<string | null>(null);
  const toast = useToast();
  const changed = JSON.stringify(text) !== JSON.stringify(pickText(saved));
  /** What the page shows: saved settings with the unsaved text fields on top. */
  const view: AgentSettings = { ...saved, ...text };
  const num = (v: string) => (v.trim() === "" ? 0 : Math.round(Number(v)));

  /** Save a switch or menu now, keeping unsaved text fields as they are. */
  const apply = async (patch: Partial<AgentSettings>, label: string) => {
    setError(null);
    const before = saved;
    try {
      await api.setAgentSettings({ ...saved, ...patch });
      await refresh();
      toast({
        text: label,
        tone: "ok",
        action: {
          label: "Undo",
          run: () =>
            void api
              .setAgentSettings(before)
              .then(refresh)
              .catch((e) => setError(errorText(e))),
        },
      });
    } catch (e) {
      setError(errorText(e));
    }
  };
  const saveText = async (): Promise<boolean> => {
    setError(null);
    try {
      await api.setAgentSettings({ ...saved, ...text });
      await refresh();
      return true;
    } catch (e) {
      setError(errorText(e));
      return false;
    }
  };
  useUnsaved("agent", changed, { label: "Agent", save: saveText, discard: () => setText(pickText(saved)) });

  const onOff = (on: boolean) => (on ? "on" : "off");

  return (
    <section>
      <h2>Agent</h2>
      <p className="small muted">
        How the agent works in its sandbox, manages its context and recovers from errors. Each setting says when it
        applies: from the next request, or to sessions started afterwards.
      </p>
      {error && <div className="banner error">{error}</div>}

      <SettingGroup title="Commands">
        <SettingRow
          label="Shell"
          help={
            <>
              For the agent's commands and your ! commands; applies to sessions started afterwards.{" "}
              {view.shell === "powershell" &&
                (state.powershell7
                  ? `In use: PowerShell 7 (${shownPath(state.powershell7)}). If it can't start in the sandbox, sessions use Windows PowerShell 5.1.`
                  : (
                    <>
                      <strong className="gw-shell-status">PowerShell 7 isn't installed, so sessions use Windows PowerShell 5.1.</strong>{" "}
                      Install PowerShell 7 with Microsoft's MSI installer (from github.com/PowerShell/PowerShell releases) and new
                      sessions will use it, or choose Windows PowerShell 5.1 in the menu to use it on purpose. The Microsoft Store and winget
                      versions (MSIX packages) can't run in the sandbox.
                    </>
                  ))}
              <Explain
                what="Commands run inside the session's sandbox, which can reach only the project's folders and has no network. Inside it, each project folder appears as its own drive (W:, X:…), visible only to the sandbox, so commands start at the top of the project and nothing above it can be seen. Each agent session's header shows the shell it uses."
                pros={[
                  "PowerShell understands many commands models know from Linux (ls, cat, pwd, rm, cp, mv) and has richer tools for files and text.",
                  "PowerShell 7 also understands && and || between commands, as models often write them.",
                  "Command Prompt is simpler and starts a little faster.",
                ]}
                cons={[
                  "PowerShell 7 starts a little slower than Windows PowerShell 5.1.",
                  "Windows PowerShell 5.1 has no && (use ; instead); Gardenwall tells the agent when it slips.",
                  "Batch files (.bat, .cmd) can't run in the sandbox with any of these shells.",
                ]}
              />
            </>
          }
        >
          <select
            aria-label="Shell"
            value={view.shell}
            onChange={(e) => {
              const shell = e.target.value as AgentSettings["shell"];
              const name = { powershell: "PowerShell", windowspowershell: "Windows PowerShell 5.1", cmd: "Command Prompt" }[shell];
              if (!window.confirm(`Use ${name} for the agent's commands? Sessions started from now on use it; open sessions keep theirs.`)) return;
              void apply({ shell }, `Shell: ${name}`);
            }}
          >
            <option value="powershell">
              {state.powershell7 ? "PowerShell 7 (recommended)" : "PowerShell 7 once installed (5.1 until then)"}
            </option>
            <option value="windowspowershell">Windows PowerShell 5.1</option>
            <option value="cmd">Command Prompt (cmd)</option>
          </select>
        </SettingRow>
        <SettingRow
          label="Python"
          help={
            <>
              {state.python
                ? `${shownPath(state.python)}, read-only inside the sandbox; applies to sessions started afterwards.`
                : state.pythonUnreadable
                  ? `${shownPath(state.pythonUnreadable)} is installed, but the sandbox can't read it yet.`
                  : "Not installed for the agent."}
              {view.python && !state.python && state.pythonUnreadable && (
                <>
                  {" "}
                  Gardenwall never changes a system folder's permissions itself. To let sandboxes read and run this Python (but
                  not change it), run this once in an administrator terminal, then start a new session:
                  <pre className="approval-pre">{`icacls "${shownPath(state.pythonUnreadable).replace(/\\python\.exe$/i, "")}" /grant "*S-1-15-2-1:(OI)(CI)RX"`}</pre>
                  S-1-15-2-1 is Windows' "All application packages" group, which Program Files already allows to read.
                </>
              )}
              {view.python && !state.python && !state.pythonUnreadable && (
                <>
                  {" "}
                  Install Python from python.org with "Install for all users" (it goes into Program Files, which sandboxes can
                  read), then start a new session. Another Python can be added under Agent tools. The Microsoft Store version
                  can't run in the sandbox.
                </>
              )}
              <Explain
                what="Python runs like any other command: inside the session's sandbox, able to change files only in the project's folders, with no network. Its own folder is read-only, compiled files go to the session's temporary folder instead of the project, and your personal Python packages aren't visible. There is no network, so pip install doesn't work: install packages (for example pytest) into that Python yourself."
                pros={[
                  "The agent can run tests and scripts to check its work instead of guessing.",
                  "Files that make Python run code by themselves (.pth files, sitecustomize.py, virtual environments, compiled .pyc files) need your approval before the agent's file tools change them.",
                ]}
                cons={[
                  "Python is as powerful as the shell inside the sandbox: it can change any file in the project's folders.",
                  "Turn it off if a project shouldn't run code at all, or use Planning mode to review first.",
                ]}
              />
            </>
          }
        >
          <Toggle label="Python for the agent" checked={view.python} onChange={(on) => void apply({ python: on }, `Python ${onOff(on)}`)} />
        </SettingRow>
        <SettingRow
          label='"Run this yourself" cards'
          help={<Fold summary="When a step needs the network, the agent shows you the exact command to run yourself.">Its commands have no network, so for things like installing a dependency or git pull it puts the command on a card; Gardenwall never runs it. Applies to sessions started afterwards.</Fold>}
        >
          <Toggle label='"Run this yourself" cards' checked={view.runRequests} onChange={(on) => void apply({ runRequests: on }, `"Run this yourself" cards ${onOff(on)}`)} />
        </SettingRow>
        <SettingRow
          label="Question cards"
          help={<Fold summary="The agent can ask you multiple-choice questions on a card when a decision is yours.">Up to four questions at a time, and it waits for your answer. Off by default: it adds a tool to every request and hasn't been measured with the benchmark yet. Applies to sessions started afterwards.</Fold>}
        >
          <Toggle label="Question cards" checked={view.askUser} onChange={(on) => void apply({ askUser: on }, `Question cards ${onOff(on)}`)} />
        </SettingRow>      </SettingGroup>

      <SettingGroup title="Long tasks">
        <SettingRow
          label="Ask to continue"
          help={<Fold summary="Pause long tasks after this many steps so you can check they're on track; continuing keeps everything.">A step is one round trip to the model: a reply and the tools it asked for. A task that repeats itself is stopped either way. Applies from the next request.</Fold>}
        >
          <select
            aria-label="Ask to continue"
            value={String(view.stepCheckpoint)}
            onChange={(e) => {
              const n = Number(e.target.value);
              void apply({ stepCheckpoint: n }, n ? `Asks to continue every ${n} steps` : "Never asks to continue");
            }}
          >
            {[50, 100, 200, 500].map((n) => (
              <option key={n} value={n}>
                Every {n} steps{n === 100 ? " (recommended)" : ""}
              </option>
            ))}
            {![0, 50, 100, 200, 500].includes(view.stepCheckpoint) && <option value={view.stepCheckpoint}>Every {view.stepCheckpoint} steps</option>}
            <option value={0}>Never</option>
          </select>
        </SettingRow>
        <SettingRow
          label="Summarize older messages when the context fills"
          help={
            <>
            Applies from the next request.
            <Explain
              what="When the conversation nears the model's context window, older messages are replaced by a structured summary (Pi's compaction), so long tasks can continue."
              pros={["Long sessions keep working instead of failing when the window is full."]}
              cons={["Details from summarized messages are lost; the agent may need to re-read files.", "Summarizing costs one extra model call."]}
            />
            </>
          }
        >
          <Toggle label="Summarize older messages" checked={view.compaction} onChange={(on) => void apply({ compaction: on }, `Summarizing ${onOff(on)}`)} />
        </SettingRow>
      </SettingGroup>

      <GitSettings state={state} view={view} refresh={refresh} apply={apply} setText={(patch) => setText((t) => ({ ...t, ...patch }))} />
      <ToolchainSettings state={state} view={view} apply={apply} />
      <SettingGroup title="Folders above projects">
        <ParentFoldersRow project={null} onChanged={() => void refresh()} />
      </SettingGroup>

      <SettingGroup title="Several sessions at once">
        <SettingRow
          label="How sessions share the model"
          help={
            <>
            Each open agent session has its own folders, sandbox and approvals. This chooses how they share the loaded model.
            {view.sessions === "turns" ? (
              <Explain
                what="Sessions work side by side. When two need the model at once, their requests queue and are answered one after the other, each with the whole context window."
                pros={["Nothing to set up; every session keeps the full context window.", "Steps of different sessions interleave, so all of them make progress."]}
                cons={["Switching between sessions makes the model re-read each session's context, which costs some time on long sessions."]}
              />
            ) : view.sessions === "oneAtATime" ? (
              <Explain
                what="Only one session runs a task at a time. Sending to another session shows that it is waiting, and it starts as soon as the working session finishes (or you press Stop)."
                pros={["Each task runs at full speed, with no surprise slowdowns.", "Easiest to follow: one thing happens at a time."]}
                cons={["Other sessions wait, even for a quick question."]}
              />
            ) : (
              <Explain
                what="The model server runs several slots, so sessions generate at the same time. The slots share one context memory, so each session can use the whole window while their total fits."
                pros={["Several tasks progress truly at once.", "Total throughput is usually higher than taking turns."]}
                cons={[
                  "Each session generates more slowly while others are working.",
                  "When the sessions together fill the context memory, a request can fail until one finishes.",
                  "Applies when a model is loaded next (Unload, then Load on the Models page).",
                ]}
              />
            )}
            </>
          }
        >
          <select
            aria-label="How sessions share the model"
            value={view.sessions}
            onChange={(e) => {
              const sessions = e.target.value as AgentSettings["sessions"];
              void apply({ sessions }, { turns: "Sessions take turns", oneAtATime: "One session at a time", parallel: "Sessions run in parallel" }[sessions]);
            }}
          >
            <option value="turns">Take turns (default)</option>
            <option value="oneAtATime">One at a time</option>
            <option value="parallel">Parallel</option>
          </select>
          {view.sessions === "parallel" && (
            <select
              aria-label="Sessions at the same time"
              value={view.parallelSlots}
              onChange={(e) => void apply({ parallelSlots: Number(e.target.value) }, `Up to ${e.target.value} sessions at once`)}
            >
              {[2, 3, 4].map((n) => (
                <option key={n} value={n}>
                  Up to {n}
                </option>
              ))}
            </select>
          )}
        </SettingRow>
      </SettingGroup>

      <SettingGroup title="Notifications">
        {(
          [
            ["notifyFinished", "A session finishes"],
            ["notifyApproval", "A session needs approval"],
            ["notifyFailed", "A session fails"],
          ] as const
        ).map(([key, label], i) => (
          <SettingRow
            key={key}
            label={label}
            help={
              i === 2
                ? "While Gardenwall is in the background (silent; corner cards show either way). A notification names only the session and what happened, never anything the model wrote, because notifications appear outside Gardenwall (for example on the lock screen). Approvals still happen only in Gardenwall."
                : undefined
            }
          >
            <Toggle label={`Notify when ${label.toLowerCase()}`} checked={view[key]} onChange={(on) => void apply({ [key]: on }, `Notifications: ${label.toLowerCase()} ${onOff(on)}`)} />
          </SettingRow>
        ))}
      </SettingGroup>

      <Disclosure summary="Advanced">
        <SettingGroup>
          <SettingRow label="Reserve" help="Tokens left free for the reply; summarizing starts below this (Pi: 16,384). On small context windows it's capped at a quarter of the window.">
            <input
              type="number"
              aria-label="Reserve tokens"
              className="narrow"
              min={1024}
              max={262144}
              step={1024}
              value={view.reserveTokens}
              disabled={!view.compaction}
              onChange={(e) => setText((t) => ({ ...t, reserveTokens: num(e.target.value) }))}
            />
          </SettingRow>
          <SettingRow label="Keep recent" help="Tokens of the newest messages kept word for word (Pi: 20,000). On small context windows it's capped at 30 % of the window.">
            <input
              type="number"
              aria-label="Keep recent tokens"
              className="narrow"
              min={1024}
              max={262144}
              step={1024}
              value={view.keepRecentTokens}
              disabled={!view.compaction}
              onChange={(e) => setText((t) => ({ ...t, keepRecentTokens: num(e.target.value) }))}
            />
          </SettingRow>
          <SettingRow label="Retries" help="Times a request is retried when the model server is busy or unreachable, before any output (waits 2 s, 4 s, 8 s…; Pi: 3). Applies from the next request.">
            <input
              type="number"
              aria-label="Retries"
              className="narrow"
              min={0}
              max={10}
              value={view.retries}
              onChange={(e) => setText((t) => ({ ...t, retries: num(e.target.value) }))}
            />
          </SettingRow>
          <SettingRow
            label="Code mode (experimental)"
            help={<Fold summary="The agent may write one small program that makes many tool calls, using fewer steps.">The program runs in its own sandbox with no files or network, and every call it makes is checked and approved like any other. It uses less context. Applies from the next request.</Fold>}
          >
            <Toggle label="Code mode" checked={view.codeMode} onChange={(on) => void apply({ codeMode: on }, `Code mode ${onOff(on)}`)} />
          </SettingRow>
          <div className="gw-row gw-row-stack">
            <div className="gw-row-text">
              <label className="gw-row-label" htmlFor="append">
                Extra instructions
              </label>
              <div className="gw-row-help">
                Added to the end of the agent's system prompt in every session (like Pi's --append-system-prompt). For
                instructions about one project, use its AGENTS.md instead (/memory in an agent session).
              </div>
              <textarea
                id="append"
                rows={4}
                maxLength={32768}
                placeholder="For example: Prefer small commits. Explain risky commands before running them."
                value={view.appendSystemPrompt}
                onChange={(e) => setText((t) => ({ ...t, appendSystemPrompt: e.target.value }))}
              />
            </div>
          </div>
        </SettingGroup>
      </Disclosure>
      {changed && (
        <div className="row">
          <Button variant="primary" onClick={() => void saveText()}>
            Save
          </Button>
          <Button variant="ghost" onClick={() => setText(pickText(saved))}>
            Discard changes
          </Button>
        </div>
      )}
    </section>
  );
}

/**
 * Languages and build tools found on this PC (ADR-022 §2B): each can be added
 * to the agent's sandbox, read and run only, with its caches kept apart from
 * the user's own.
 */
function ToolchainSettings({
  state,
  view,
  apply,
}: {
  state: AppView;
  view: AgentSettings;
  apply: (patch: Partial<AgentSettings>, label: string) => Promise<void>;
}) {
  const set = (id: string, name: string, on: boolean) => {
    const next = on ? [...new Set([...view.toolchains, id])] : view.toolchains.filter((t) => t !== id);
    void apply({ toolchains: next }, `${name}: ${on ? "added to" : "removed from"} the agent's sandbox`);
  };
  return (
    <SettingGroup title="Languages and build tools (every project)">
      {state.toolchains.length === 0 ? (
        <SettingRow
          label="Found on this PC"
          help="None of the toolchains Gardenwall knows (Rust, Java, .NET, Go, C and C++ with Visual Studio) were found. Git, Python and Node.js are set up above and under Agent tools."
        >
          <span className="muted small">None found</span>
        </SettingRow>
      ) : (
        state.toolchains.map((t) => {
          const on = view.toolchains.includes(t.id);
          return (
            <SettingRow
              key={t.id}
              label={`${t.name} ${t.version}`}
              help={
                <>
                  {on
                    ? `In the agent's sandbox in every project: it can build, run and test with it, with no network. Read and run only: ${shownPath(t.folder)}.`
                    : `Found at ${shownPath(t.folder)}. Add it here for every project, or on a project's page for that project only.`}{" "}
                  Applies to sessions started afterwards.
                  {t.note && <div className="gw-shell-status">{t.note}</div>}
                  <Explain
                    what="The sandbox gets this one folder, read and run only. The tool's caches (such as Rust's Cargo folder) go in Gardenwall's own agent folder, not yours, so your settings and any saved tokens stay out of reach, and a build can't change what your own builds use."
                    pros={["The agent can compile, run tests and fix errors itself.", "Nothing is installed or downloaded."]}
                    cons={[
                      "Commands have no network: for dependencies from the internet, the agent asks you to run the download step (for Rust, cargo vendor).",
                      "Sessions take a few seconds longer to start while the sandbox is given the folder.",
                    ]}
                  />
                </>
              }
            >
              {on ? (
                <Button onClick={() => set(t.id, t.name, false)}>Remove</Button>
              ) : (
                <Button onClick={() => set(t.id, t.name, true)}>Add</Button>
              )}
            </SettingRow>
          );
        })
      )}
    </SettingGroup>
  );
}

/** Git for the agent's commands: add Git for Windows, and whose name commits carry. */
function GitSettings({
  state,
  view,
  refresh,
  apply,
  setText,
}: {
  state: AppView;
  view: AgentSettings;
  refresh: () => Promise<void>;
  apply: (patch: Partial<AgentSettings>, label: string) => Promise<void>;
  setText: (patch: Partial<TextDraft>) => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const added = state.agentToolFolders.some((p) => /[\\/]Git[\\/]?$/i.test(p));
  return (
    <SettingGroup title="Git">
      {error && <div className="banner error">{error}</div>}
      <SettingRow
        label="Git for the agent"
        help={
          state.gitToAdd
            ? `Git for Windows is installed (${shownPath(state.gitToAdd)}) but the agent can't use it yet.`
            : added
              ? "Git for Windows is a tool folder: the agent's commands can use git, with no network (no push or pull)."
              : "Git for Windows wasn't found in Program Files. Install it for all users, or add another git under Agent tools."
        }
      >
        {state.gitToAdd ? (
          <Button
            onClick={() =>
              void api
                .addGitToolFolder()
                .then(refresh)
                .catch((e) => setError(errorText(e)))
            }
          >
            Add Git
          </Button>
        ) : (
          <span className={added ? "ok-text small" : "muted small"}>{added ? "Ready" : "Not found"}</span>
        )}
      </SettingRow>
      <SettingRow
        label="Commits by"
        help={
          <>
            The name and email on commits the agent makes; applies to sessions started afterwards.
            <Explain
              what={
                view.gitIdentity === "mine"
                  ? "Only your git name and email are copied into the sandbox (the project's own setting first, then your global one). Nothing else from your git settings reaches the agent: no aliases, credential helpers or hooks."
                  : view.gitIdentity === "agent"
                    ? 'Commits show "Gardenwall Agent <agent@gardenwall.invalid>", so anyone reading the history can see the agent made them. .invalid is a reserved address that can never receive mail.'
                    : "Commits use the name and email you type here."
              }
              pros={["The agent can see diffs and history, check git status and commit its work.", "Commits are scanned for secrets, and you are told if one slips in."]}
              cons={[
                "Commands like git reset --hard or git clean can throw away uncommitted work; they get a warning card, and Undo can put files back.",
                "Commands have no network, so the agent can't push, pull or clone. When it needs that, it asks you to run the command.",
              ]}
            />
          </>
        }
      >
        <select
          aria-label="Commits by"
          value={view.gitIdentity}
          onChange={(e) => {
            const gitIdentity = e.target.value as AgentSettings["gitIdentity"];
            void apply({ gitIdentity }, `Commits by: ${{ mine: "me", agent: "Gardenwall Agent", custom: "someone else" }[gitIdentity]}`);
          }}
        >
          <option value="mine">Me (my name and email from git)</option>
          <option value="agent">Gardenwall Agent</option>
          <option value="custom">Someone else…</option>
        </select>
      </SettingRow>
      {view.gitIdentity === "custom" && (
        <SettingRow label="Name and email" help="Saved with the Save button or the bar at the top.">
          <input aria-label="Commit name" placeholder="Name" value={view.gitName} onChange={(e) => setText({ gitName: e.target.value })} />
          <input aria-label="Commit email" placeholder="Email" value={view.gitEmail} onChange={(e) => setText({ gitEmail: e.target.value })} />
        </SettingRow>
      )}
    </SettingGroup>
  );
}
