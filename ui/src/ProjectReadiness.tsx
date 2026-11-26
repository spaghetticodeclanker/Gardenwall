// "Ready to work?" on the project page (docs/internal/PROJECT-SETUP-PLAN.md A1-A4,
// in 0.1.0-rc.5). Everything shown comes from the core, which only looked
// at the project's files: what each part is, the versions it pins, the
// toolchains it needs, whether its packages are fetched (with Gardenwall's
// fixed command, for the user to run: Gardenwall never runs it), Git,
// project instructions, and the files that run code by themselves.
// Quiet when everything is ready: one line, details folded.
import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorText, type AppView, type FolderReadiness, type Project } from "./api";
import { copyText } from "./clipboard";
import { Button, CopyButton, Fold, Pill, toBody } from "./ui";
import { shownPath } from "./paths";

type Issue = { key: string; label: string; help: React.ReactNode; control?: React.ReactNode; tone?: "warn" | "neutral" };

export function ProjectReadiness({
  project,
  state,
  reloadKey,
  onChanged,
}: {
  project: Project;
  state: AppView;
  /** Changes when something else on the page changed what's ready. */
  reloadKey: string;
  onChanged: () => Promise<void>;
}) {
  const [folders, setFolders] = useState<FolderReadiness[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ index: number; text: string } | null>(null);

  const load = useCallback(
    async (recheck = false) => {
      try {
        setFolders(await api.projectReadiness(project.id, recheck));
        setError(null);
      } catch (e) {
        setError(errorText(e));
      }
    },
    [project.id],
  );
  useEffect(() => void load(), [load, reloadKey]);

  const act = async (key: string, run: () => Promise<unknown>) => {
    setBusy(key);
    setError(null);
    try {
      await run();
      await load();
      await onChanged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  if (!folders) {
    return error ? (
      <div className="gw-row">
        <div className="banner error">{error}</div>
      </div>
    ) : null;
  }
  const many = folders.length > 1;
  return (
    <>
      {error && (
        <div className="gw-row">
          <div className="banner error">{error}</div>
        </div>
      )}
      {folders.map((f) => (
        <FolderCard
          key={f.folder}
          f={f}
          many={many}
          project={project}
          projectToolchains={state.agentSettings.projectToolchains[project.id] ?? []}
          busy={busy}
          act={act}
          recheck={() => void act(`recheck-${f.index}`, () => load(true))}
          write={() =>
            void act(`draft-${f.index}`, async () => {
              setEditing({ index: f.index, text: await api.projectInstructionsDraft(project.id, f.index) });
            })
          }
        />
      ))}
      {editing && (
        <InstructionsDialog
          folder={folders.find((f) => f.index === editing.index)?.folder ?? ""}
          initial={editing.text}
          onCancel={() => setEditing(null)}
          onSave={async (text) => {
            await api.saveProjectInstructions(project.id, editing.index, text);
            setEditing(null);
            await load();
          }}
        />
      )}
    </>
  );
}

function where(path: string) {
  return path ? ` in ${path}` : "";
}

function FolderCard({
  f,
  many,
  project,
  projectToolchains,
  busy,
  act,
  recheck,
  write,
}: {
  f: FolderReadiness;
  many: boolean;
  project: Project;
  projectToolchains: string[];
  busy: string | null;
  act: (key: string, run: () => Promise<unknown>) => Promise<void>;
  recheck: () => void;
  write: () => void;
}) {
  const issues: Issue[] = [];

  for (const t of f.toolchains) {
    const by = t.neededBy.join(", ");
    if (!t.version) {
      issues.push({
        key: `install-${t.id}`,
        label: `${t.name} isn't installed`,
        tone: "warn",
        help: (
          <>
            Needed by {by}. The agent can read and edit the code, but can't build or run it.
            {t.install && <InstallHint url={t.install.url} steps={t.install.steps} />}
          </>
        ),
        control: (
          <Button size="sm" disabled={busy !== null} onClick={recheck}>
            {busy === `recheck-${f.index}` ? "Checking…" : "Check again"}
          </Button>
        ),
      });
    } else if (t.problem) {
      issues.push({ key: `problem-${t.id}`, label: `${t.name} can't be used in the sandbox`, tone: "warn", help: t.problem });
    } else if (!t.on) {
      issues.push({
        key: `on-${t.id}`,
        label: `${t.name} ${t.version} isn't turned on for this project`,
        help: `Needed by ${by}. Turned on, the agent builds, runs and tests with it inside its sandbox, with no network.`,
        control: (
          <Button
            size="sm"
            disabled={busy !== null}
            onClick={() =>
              void act(`on-${t.id}`, () => api.setProjectToolchains(project.id, [...new Set([...projectToolchains, t.id])]))
            }
          >
            {busy === `on-${t.id}` ? "Turning on…" : "Turn on"}
          </Button>
        ),
      });
    }
  }

  for (const part of f.parts) {
    for (const pin of part.pins) {
      if (pin.matches === false) {
        issues.push({
          key: `pin-${pin.file}`,
          label: `${pin.name} ${pin.wants} wanted, ${pin.installed} found`,
          tone: "warn",
          help: (
            <>
              <span className="mono">{pin.file}</span> asks for {pin.name} {pin.wants}; the agent will use {pin.installed}. If builds
              fail because of it, install the version the project asks for.
            </>
          ),
        });
      }
    }
    for (const fetch of part.fetch) {
      if (fetch.fetched || fetch.needed === false) continue;
      issues.push({
        key: `fetch-${part.path}-${fetch.id}`,
        label: `Packages not fetched: ${fetch.label}${where(part.path)}`,
        help: (
          <>
            Commands have no network, so you fetch the packages once yourself. Run this in{" "}
            <span className="mono">{shownPath(fetch.folder)}</span> (install scripts are off and the lock file is respected; the agent can also
            ask with a card):
            <pre className="gw-ready-cmd">{fetch.command}</pre>
            {fetch.warning && <span className="gw-ready-note">{fetch.warning}</span>}
            {fetch.needsParentFolders && (
              <span className="gw-ready-note">Gradle builds run in the sandbox only with "Folders above the project" on (below).</span>
            )}
          </>
        ),
        control: <CopyButton text={fetch.command} label="Copy command" />,
      });
    }
  }

  if (!f.git) {
    issues.push({
      key: "git",
      label: "No Git repository",
      help: f.canInit
        ? "Git can't create one from inside the agent's sandbox, so Gardenwall creates it for you (git init, branch main)."
        : "Git for Windows wasn't found in Program Files. Install it for all users to create one here.",
      control: f.canInit ? (
        <Button size="sm" disabled={busy !== null} onClick={() => void act(`git-${f.index}`, () => api.initProjectRepository(project.id, f.index))}>
          {busy === `git-${f.index}` ? "Creating…" : "Create repository"}
        </Button>
      ) : undefined,
    });
  }
  if (f.gitignoreMissing > 0) {
    issues.push({
      key: "gitignore",
      label: "Secret files aren't all in .gitignore",
      tone: "warn",
      help: `${f.gitignoreMissing} kinds of secret files (like .env and private keys) could be committed by accident. Gardenwall adds them to .gitignore in a marked block; Protection below lists them.`,
      control: (
        <Button size="sm" disabled={busy !== null} onClick={() => void act(`ignore-${f.index}`, () => api.addProjectGitignore(project.id, f.index))}>
          {busy === `ignore-${f.index}` ? "Adding…" : "Add to .gitignore"}
        </Button>
      ),
    });
  }
  if (f.parentFolders && !(f.parentFolders.on && f.parentFolders.ready)) {
    issues.push({
      key: "parents",
      label: `${f.parentFolders.neededBy.join(", ")} need "Folders above the project"`,
      help: f.parentFolders.on
        ? "It's on, but a folder above the project isn't ready yet: the row below shows the one step left."
        : "Turn it on in the row below: the sandbox then sees the names (never the contents) of the folders above the project.",
    });
  }
  if (!f.instructions) {
    issues.push({
      key: "instructions",
      label: "No project instructions",
      tone: "neutral",
      help: "An AGENTS.md tells the agent how to build, test and work in this project. Gardenwall writes a draft from the project's files for you to edit before saving.",
      control: (
        <Button size="sm" disabled={busy !== null} onClick={write}>
          {busy === `draft-${f.index}` ? "Writing…" : "Write instructions…"}
        </Button>
      ),
    });
  }

  const needsWork = issues.filter((i) => i.tone !== "neutral").length;
  const known = f.parts.filter((p) => p.stacks.length > 0);
  return (
    <>
      <div className="gw-row">
        <div className="gw-row-text">
          <div className="gw-row-label">
            Ready to work? {many && <span className="mono small muted">· {shownPath(f.folder)}</span>}
          </div>
          <div className="gw-row-help">
            {known.length === 0
              ? "Gardenwall didn't recognise a kind of project here (it looks for files like Cargo.toml, package.json or pyproject.toml)."
              : `${known.map((p) => `${p.stacks.join(", ")}${where(p.path)}`).join("; ")}.`}
          </div>
          <Details f={f} />
        </div>
        <div className="gw-row-control">
          {needsWork === 0 ? <Pill tone="ok">Ready</Pill> : <Pill tone="warn">{needsWork === 1 ? "1 thing to set up" : `${needsWork} things to set up`}</Pill>}
        </div>
      </div>
      {issues.map((i) => (
        <div className="gw-row gw-ready-issue" key={i.key}>
          <div className="gw-row-text">
            <div className="gw-row-label">{i.label}</div>
            <div className="gw-row-help">{i.help}</div>
          </div>
          {i.control && <div className="gw-row-control">{i.control}</div>}
        </div>
      ))}
      <AutoRunRow f={f} />
    </>
  );
}

function InstallHint({ url, steps }: { url: string; steps: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <>
      {" "}
      Get it from <span className="mono">{url}</span>{" "}
      <button
        type="button"
        className="link"
        onClick={async () => {
          if (await copyText(url)) {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
          }
        }}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      . {steps}
    </>
  );
}

/** What Gardenwall checked, folded away. */
function Details({ f }: { f: FolderReadiness }) {
  const lines: string[] = [];
  for (const t of f.toolchains) {
    lines.push(
      t.version
        ? `${t.name} ${t.version}: ${t.everywhere ? "on for every project" : t.on ? "on for this project" : "installed, not turned on"}.`
        : `${t.name}: not installed.`,
    );
  }
  for (const p of f.parts) {
    for (const pin of p.pins) {
      const state = pin.matches === true ? "matches" : pin.matches === false ? `found ${pin.installed}` : pin.installed ? `found ${pin.installed}, can't compare` : "not installed";
      lines.push(`${pin.file} asks for ${pin.name} ${pin.wants} (${state}).`);
    }
    for (const fetch of p.fetch) {
      const state = fetch.fetched ? "fetched" : fetch.needed === false ? "no outside packages" : "not fetched";
      lines.push(`${fetch.label}${where(p.path)}: ${state}.`);
    }
  }
  lines.push(f.git ? "Git repository: yes." : "Git repository: no.");
  lines.push(f.instructions ? `Project instructions: ${f.instructions}.` : "Project instructions: none.");
  return (
    <Fold summary="" more="What Gardenwall checked">
      {lines.map((l) => (
        <p key={l}>{l}</p>
      ))}
    </Fold>
  );
}

/** The files that run code by themselves (A2), folded. */
function AutoRunRow({ f }: { f: FolderReadiness }) {
  const count = f.autoRun.length + f.autoRunMore;
  return (
    <div className="gw-row">
      <div className="gw-row-text">
        <div className="gw-row-label">Runs by itself</div>
        <div className="gw-row-help">
          {count === 0 ? (
            "No install scripts, build scripts, editor tasks or Git hooks were found that run by themselves."
          ) : (
            <Fold
              summary={`${count === 1 ? "1 file runs" : `${count} files run`} code by itself when something else happens (installing packages, building, testing, opening the folder in an editor, committing). Commands you or the agent run may start them.`}
              more="Show"
            >
              {f.autoRun.map((a) => (
                <div key={a.path} className="gw-autorun">
                  <span className="mono">{a.path}</span> · {a.when}
                  {a.details.map((d) => (
                    <div key={d} className="mono gw-autorun-detail">
                      {d}
                    </div>
                  ))}
                </div>
              ))}
              {f.autoRunMore > 0 && <p>…and {f.autoRunMore} more.</p>}
              <p>
                Packages fetched into vendor, node_modules or .venv can run code too; Gardenwall's fetch commands turn install scripts off.
              </p>
            </Fold>
          )}
        </div>
      </div>
    </div>
  );
}

/** Edit the starter AGENTS.md, then save it as a new file (A3). */
function InstructionsDialog({
  folder,
  initial,
  onCancel,
  onSave,
}: {
  folder: string;
  initial: string;
  onCancel: () => void;
  onSave: (text: string) => Promise<void>;
}) {
  const [text, setText] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    area.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);
  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="modal gw-diag" role="dialog" aria-modal="true" aria-labelledby="instructions-title">
        <h2 id="instructions-title">Project instructions</h2>
        <p className="small muted">
          Saved as <span className="mono">AGENTS.md</span> in {shownPath(folder)}. Every agent session in this project reads it. Gardenwall wrote
          this draft from the project's files; change anything before saving.
        </p>
        {error && <div className="banner error">{error}</div>}
        <textarea
          ref={area}
          className="gw-diag-text gw-instructions-text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
          aria-label="AGENTS.md"
        />
        <div className="gw-diag-actions">
          <Button
            variant="primary"
            disabled={saving || !text.trim()}
            onClick={async () => {
              setSaving(true);
              setError(null);
              try {
                await onSave(text);
              } catch (e) {
                setError(errorText(e));
                setSaving(false);
              }
            }}
          >
            {saving ? "Saving…" : "Save AGENTS.md"}
          </Button>
          <button className="link gw-diag-close" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </div>,
  );
}
