// The guided "New project…" dialog (ADR-024 §5:
// layout A, one card that fills in as you go). Folders come only from the
// native picker: the core keeps the picked folder and gives this window a
// one-time token. Toolchain status and download addresses come from the
// core's own table; addresses are copied, never opened. The project's first
// session opens with a starter message in the composer, not sent.
import { useEffect, useRef, useState } from "react";
import { api, errorText, type NewProjectOptions, type PickedFolder, type Project } from "./api";
import { copyText } from "./clipboard";
import { Button, toBody } from "./ui";

export function NewProjectDialog({
  onClose,
  onCreated,
  onPlainPicker,
}: {
  onClose: () => void;
  /** The project was created: open its first session with `starter` in the composer. */
  onCreated: (project: Project, starter: string, notes: string[]) => void;
  /** "Use an existing folder as it is": today's plain picker. */
  onPlainPicker: () => void;
}) {
  const [options, setOptions] = useState<NewProjectOptions | null>(null);
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [name, setName] = useState("");
  const [picked, setPicked] = useState<PickedFolder | null>(null);
  const [kind, setKind] = useState<string | null>(null);
  const [git, setGit] = useState(true);
  const [ignoreSecrets, setIgnoreSecrets] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const dialog = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api.newProjectOptions(false).then(setOptions, (e) => setError(errorText(e)));
    dialog.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const choose = async () => {
    setError(null);
    try {
      const p = await api.newProjectPick(mode === "new");
      if (p) setPicked(p);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const switchMode = (m: "new" | "existing") => {
    if (m !== mode) {
      setMode(m);
      setPicked(null);
    }
  };

  const trimmed = name.trim();
  const folderDone = picked !== null && (mode === "existing" || trimmed.length > 0);
  const k = options?.kinds.find((x) => x.id === kind) ?? null;
  const hasGit = mode === "existing" && picked?.hasGit === true;
  const offerGit = options?.git === true && !hasGit;
  // There will be a repository: offer to keep secret files out of it.
  const willHaveGit = hasGit || (offerGit && git);
  const folderShown = picked ? (mode === "new" ? `${picked.folder.replace(/[\\/]$/, "")}\\${trimmed || "…"}` : picked.folder) : null;
  // Toolchains that are found and usable are added; the rest are listed.
  const adding = k?.toolchains.filter((t) => t.version && !t.problem) ?? [];
  const ready = folderDone && k !== null && !busy;

  const create = async () => {
    if (!picked || !k) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.createGuidedProject({
        pick: picked.token,
        newFolderName: mode === "new" ? trimmed : null,
        kind: k.id,
        toolchains: adding.map((t) => t.id),
        initGit: offerGit && git,
        ignoreSecrets: willHaveGit && ignoreSecrets,
      });
      onCreated(r.project, r.starter, r.notes);
    } catch (e) {
      setError(errorText(e));
      // The pick is used up by a failed attempt too: choose again.
      setPicked(null);
    } finally {
      setBusy(false);
    }
  };

  const copy = async (url: string) => {
    if (await copyText(url)) setCopied(url);
  };
  const checkAgain = async () => {
    try {
      setOptions(await api.newProjectOptions(true));
    } catch (e) {
      setError(errorText(e));
    }
  };

  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div ref={dialog} tabIndex={-1} className="modal gw-newproj" role="dialog" aria-modal="true" aria-labelledby="newproj-title">
        <h2 id="newproj-title">New project</h2>

        <section className="gw-newproj-step">
          <h3>
            <span className={folderDone ? "gw-step done" : "gw-step"}>1</span> Folder
          </h3>
          <div className="gw-newproj-seg" role="radiogroup" aria-label="Folder">
            <button role="radio" aria-checked={mode === "new"} className={mode === "new" ? "on" : ""} onClick={() => switchMode("new")}>
              Create a new folder
            </button>
            <button role="radio" aria-checked={mode === "existing"} className={mode === "existing" ? "on" : ""} onClick={() => switchMode("existing")}>
              Use a folder I have
            </button>
          </div>
          {mode === "new" && (
            <label className="gw-newproj-field">
              <span>Name</span>
              <input value={name} maxLength={100} placeholder="my-app" onChange={(e) => setName(e.target.value)} />
            </label>
          )}
          <div className="gw-newproj-field">
            <span>{mode === "new" ? "Inside" : "Folder"}</span>
            <span className="mono gw-newproj-path">{picked?.folder ?? <span className="muted">Not chosen yet</span>}</span>
            <Button size="sm" onClick={() => void choose()}>
              Choose…
            </Button>
          </div>
          {mode === "existing" && picked && (
            <p className="small muted">
              {picked.empty
                ? "The folder is empty."
                : [picked.hasGit ? "Has a Git repository" : "", ...picked.detected].filter(Boolean).join(" · ") || "Has files, none Gardenwall recognises."}
            </p>
          )}
        </section>

        <section className="gw-newproj-step" aria-disabled={!folderDone} data-later={!folderDone || undefined}>
          <h3>
            <span className={k ? "gw-step done" : "gw-step"}>2</span> What you're building
          </h3>
          {options ? (
            <div className="gw-newproj-kinds" role="radiogroup" aria-label="What you're building">
              {options.kinds.map((x) => (
                <button
                  key={x.id}
                  role="radio"
                  aria-checked={kind === x.id}
                  className={kind === x.id ? "on" : ""}
                  disabled={!folderDone}
                  onClick={() => setKind(x.id)}
                >
                  {x.label}
                </button>
              ))}
            </div>
          ) : (
            <p className="small muted">Looking for toolchains…</p>
          )}
          {k?.toolchains.map((t) =>
            t.version && !t.problem ? (
              <div key={t.id} className="gw-newproj-status ok">
                <strong>
                  Found: {t.name} {t.version}.
                </strong>{" "}
                It will be added to this project (read and run only).
              </div>
            ) : t.version && t.problem ? (
              <div key={t.id} className="gw-newproj-status warn">
                <strong>
                  {t.name} {t.version} can't be added:
                </strong>{" "}
                {t.problem} You can create the project now and add it later on the project page.
              </div>
            ) : (
              <div key={t.id} className="gw-newproj-status warn">
                <strong>Not installed: {t.name}.</strong>{" "}
                {t.install && (
                  <>
                    Get it from <span className="mono">{t.install.url}</span>{" "}
                    <button type="button" className="link" onClick={() => void copy(t.install!.url)}>
                      {copied === t.install.url ? "Copied" : "Copy"}
                    </button>
                    . {t.install.steps}{" "}
                  </>
                )}
                Then{" "}
                <button type="button" className="link" onClick={() => void checkAgain()}>
                  Check again
                </button>
                . You can create the project now and add {t.name} later.
              </div>
            ),
          )}
          {kind === "other" && (
            <p className="small muted">
              The agent can read and edit the code whatever the language. Some languages can't run in its sandbox, so
              you may need to run their commands yourself; the agent tells you which.
            </p>
          )}
        </section>

        <section className="gw-newproj-step" data-later={!k || undefined}>
          <h3>
            <span className={k ? "gw-step done" : "gw-step"}>3</span> Version control
          </h3>
          {hasGit ? (
            <p className="small muted">The folder already has a Git repository.</p>
          ) : offerGit ? (
            <label className="gw-newproj-check">
              <input type="checkbox" checked={git} disabled={!k} onChange={(e) => setGit(e.target.checked)} />
              <span>
                Create a Git repository <span className="small muted">· branch main, created by Gardenwall</span>
              </span>
            </label>
          ) : (
            <p className="small muted">Git for Windows isn't installed, so no repository is created. You can add one later.</p>
          )}
          {willHaveGit && (
            <label className="gw-newproj-check">
              <input
                type="checkbox"
                checked={ignoreSecrets}
                disabled={!k}
                onChange={(e) => setIgnoreSecrets(e.target.checked)}
              />
              <span>
                Keep secret files out of Git{" "}
                <span className="small muted">
                  · adds Gardenwall's list (.env files, private keys, credential files) to .gitignore, so they aren't
                  committed by accident
                </span>
              </span>
            </label>
          )}
        </section>

        <section className="gw-newproj-step" data-later={!ready || undefined}>
          <h3>
            <span className="gw-step">4</span> Summary
          </h3>
          <dl className="gw-newproj-summary">
            <dt>{mode === "new" ? "New folder" : "Folder"}</dt>
            <dd className="mono">{folderShown ?? "—"}</dd>
            <dt>Toolchain</dt>
            <dd>{k ? (adding.length ? adding.map((t) => `${t.name} ${t.version}`).join(", ") : "None added") : "—"}</dd>
            <dt>Git</dt>
            <dd>
              {hasGit ? "Already there" : offerGit && git ? "New repository" : "None"}
              {willHaveGit && ignoreSecrets ? ", secret files kept out (.gitignore)" : ""}
            </dd>
            <dt>First message</dt>
            <dd>{k ? "Prefilled in the session's message box; you edit or send it." : "—"}</dd>
          </dl>
        </section>

        {error && <div className="banner error">{error}</div>}
        <div className="gw-newproj-actions">
          <button className="link" onClick={onPlainPicker}>
            Use an existing folder as it is
          </button>
          <span>
            <Button variant="ghost" onClick={onClose}>
              Cancel
            </Button>{" "}
            <Button variant="primary" disabled={!ready} onClick={() => void create()}>
              {busy ? "Creating…" : "Create project"}
            </Button>
          </span>
        </div>
      </div>
    </div>,
  );
}
