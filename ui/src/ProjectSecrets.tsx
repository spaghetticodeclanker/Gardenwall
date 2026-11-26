import { Fragment, useCallback, useEffect, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { api, errorText, type FolderSecrets, type IgnoreGap, type Project } from "./api";
import { Button, Pill } from "./ui";
import { shownPath } from "./paths";

/**
 * Secret files in a project's folders (`.env`, keys, credential files), and
 * the option to move them to the secrets folder outside every project.
 * Built only from what the core found; the model has no part in it.
 *
 * Rows inside the project page's Protection group (docs/internal/design/REDESIGN-GAPS.md).
 */
export function ProjectSecrets({ project }: { project: Project }) {
  const [folders, setFolders] = useState<FolderSecrets[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState<number | null>(null);
  const folderKey = project.folders.join("\n");

  const load = useCallback(async () => {
    try {
      setFolders(await api.projectSecrets(project.id));
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.id, folderKey]);
  useEffect(() => void load(), [load]);

  const act = async (index: number, f: () => Promise<FolderSecrets>) => {
    setBusy(true);
    setError(null);
    try {
      const updated = await f();
      setFolders((all) => all?.map((x, i) => (i === index ? updated : x)) ?? null);
      setAsking(null);
    } catch (e) {
      setError(errorText(e));
      void load();
    } finally {
      setBusy(false);
    }
  };

  if (!folders) {
    return error ? (
      <div className="gw-row">
        <div className="banner error">{error}</div>
      </div>
    ) : (
      <div className="gw-row">
        <div className="gw-row-text">
          <div className="gw-row-label">Secret files</div>
          <div className="gw-row-help">Looking…</div>
        </div>
      </div>
    );
  }
  const shown = folders.map((f, index) => ({ f, index })).filter(({ f }) => f.inProject.length + f.moved.length > 0);
  if (shown.length === 0) {
    return (
      <div className="gw-row">
        <div className="gw-row-text">
          <div className="gw-row-label">Secret files</div>
          <div className="gw-row-help">None found in these folders.</div>
        </div>
        <div className="gw-row-control">
          <Pill tone="ok">None found</Pill>
        </div>
      </div>
    );
  }
  const many = folders.length > 1;
  return (
    <>
      {error && (
        <div className="gw-row">
          <div className="banner error">{error}</div>
        </div>
      )}
      {shown.map(({ f, index }) => (
        <Fragment key={f.folder}>
          {f.inProject.length > 0 && (
            <div className="gw-row gw-row-stack-narrow">
              <div className="gw-row-text">
                <div className="gw-row-label">
                  Secret files {many && <span className="mono small muted">· {shownPath(f.folder)}</span>}
                </div>
                <div className="gw-row-help">
                  {f.inProject.length} in the project:{" "}
                  {f.inProject.map((name, i) => (
                    <span key={name}>
                      {i > 0 && ", "}
                      <span className="mono">{name}</span>
                    </span>
                  ))}
                </div>
                {asking === index && (
                  <div className="gw-project-confirm">
                    <p className="small">
                      These files will be moved to <span className="mono">{shownPath(f.keptIn)}</span>, which only you can open.
                      Programs that expect them in the project will stop finding them until you point them at the new
                      place. You can put them back at any time.
                    </p>
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={busy}
                      onClick={() => void act(index, () => api.moveProjectSecrets(project.id, index, f.inProject))}
                    >
                      {busy ? "Moving…" : "Move them"}
                    </Button>
                    <Button size="sm" variant="ghost" disabled={busy} onClick={() => setAsking(null)}>
                      Cancel
                    </Button>
                  </div>
                )}
              </div>
              {asking !== index && (
                <div className="gw-row-control">
                  <Button size="sm" onClick={() => setAsking(index)}>
                    Move to the secrets folder…
                  </Button>
                </div>
              )}
            </div>
          )}
          {f.moved.length > 0 && (
            <div className="gw-row gw-row-stack-narrow">
              <div className="gw-row-text">
                <div className="gw-row-label">
                  In your secrets folder ({f.moved.length}) {many && <span className="mono small muted">· {shownPath(f.folder)}</span>}
                </div>
                <div className="gw-row-help">
                  <span className="mono">{shownPath(f.keptIn)}</span>
                </div>
                <div className="gw-project-moved">
                  {f.moved.map((name) => (
                    <div key={name} className="gw-project-moved-row">
                      <span className="mono small">{name}</span>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        onClick={() => void act(index, () => api.restoreProjectSecrets(project.id, index, [name]))}
                      >
                        Put back
                      </Button>
                    </div>
                  ))}
                </div>
                {f.hints.length > 0 && (
                  <div className="gw-row-help">
                    To use them from there:
                    <ul>
                      {f.hints.map((h) => (
                        <li key={h}>{h}</li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            </div>
          )}
        </Fragment>
      ))}
    </>
  );
}

/**
 * For a git project: secret file patterns its ignore rules don't cover, and
 * a button that adds them to .gitignore in one marked block. Nothing is
 * written until the button is pressed.
 */
export function ProjectGitignore({ project }: { project: Project }) {
  const [gaps, setGaps] = useState<IgnoreGap[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<number | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const folderKey = project.folders.join("\n");

  const load = useCallback(async () => {
    try {
      setGaps(await api.projectGitignore(project.id));
    } catch {
      setGaps([]);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.id, folderKey]);
  useEffect(() => void load(), [load]);

  if (gaps.length === 0 && !done && !error) return null;
  return (
    <>
      {error && (
        <div className="gw-row">
          <div className="banner error">{error}</div>
        </div>
      )}
      {done && (
        <div className="gw-row">
          <div className="gw-row-text">
            <div className="gw-row-label">.gitignore</div>
            <div className="gw-row-help">{done}</div>
          </div>
          <div className="gw-row-control">
            <Pill tone="ok">Added</Pill>
          </div>
        </div>
      )}
      {gaps.map((g) => (
        <div key={g.folder} className="gw-row gw-row-warn gw-row-stack-narrow">
          <div className="gw-row-text">
            <div className="gw-row-label gw-row-label-icon">
              <AlertTriangle aria-hidden="true" size={15} />
              .gitignore
            </div>
            <div className="gw-row-help">
              <span className="mono">{shownPath(g.folder)}</span> is a git project, and {g.lines.length} kinds of secret file aren't
              in its ignore rules, so one could be committed by accident. Ignoring a file doesn't remove it from git if it
              was committed before.{" "}
              <button type="button" className="link small" onClick={() => setOpen(open === g.index ? null : g.index)}>
                {open === g.index ? "Hide the lines" : "Show the lines"}
              </button>
            </div>
            {open === g.index && <pre className="small mono gw-project-lines">{g.lines.join("\n")}</pre>}
          </div>
          <div className="gw-row-control">
            <Button
              size="sm"
              onClick={async () => {
                setError(null);
                try {
                  const n = await api.addProjectGitignore(project.id, g.index);
                  setDone(`Added ${n} lines to .gitignore in one marked block. Remove a line there only if you know that file is safe to publish.`);
                  setOpen(null);
                  await load();
                } catch (e) {
                  setError(errorText(e));
                }
              }}
            >
              Add them to .gitignore
            </Button>
          </div>
        </div>
      ))}
    </>
  );
}
