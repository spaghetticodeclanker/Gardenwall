import { useCallback, useEffect, useState } from "react";
import { ParentFoldersRow } from "./ParentFolders";
import { ProjectReadiness } from "./ProjectReadiness";
import { api, errorText, type AppView, type FolderSetup, type Project } from "./api";
import { Button, Pill, Toggle } from "./ui";
import { shownPath } from "./paths";

/**
 * Set up: what the agent's sandbox can't do for a project (ADR-022 §2A).
 * Git can't create a repository from inside the sandbox, so Gardenwall
 * creates one here, outside it, with fixed arguments (`git init -b main`).
 * Rows inside the project page's Set up group.
 */
export function ProjectSetup({ project, state, refresh }: { project: Project; state: AppView; refresh: () => Promise<void> }) {
  const [folders, setFolders] = useState<FolderSetup[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  // Bumped when a row here changes what's ready, so the readiness card
  // looks again.
  const [tick, setTick] = useState(0);
  const folderKey = project.folders.join("\n");
  const toolchainKey = JSON.stringify([state.agentSettings.toolchains, state.agentSettings.projectToolchains[project.id] ?? []]);

  const load = useCallback(async () => {
    try {
      setFolders(await api.projectSetup(project.id));
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.id, folderKey]);
  useEffect(() => void load(), [load]);

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
      <ProjectReadiness
        project={project}
        state={state}
        reloadKey={`${folderKey}|${toolchainKey}|${tick}`}
        onChanged={async () => {
          await load();
          await refresh();
        }}
      />
      {error && (
        <div className="gw-row">
          <div className="banner error">{error}</div>
        </div>
      )}
      {done && (
        <div className="gw-row">
          <div className="banner info small">{done}</div>
        </div>
      )}
      <ProjectToolchains project={project} state={state} refresh={refresh} onError={setError} />
      <ParentFoldersRow
        project={project.id}
        onChanged={() => {
          setTick((t) => t + 1);
          void refresh();
        }}
      />
      {folders.map((f, index) => (
        <div className="gw-row" key={f.folder}>
          <div className="gw-row-text">
            <div className="gw-row-label">
              Git repository {many && <span className="mono small muted">· {shownPath(f.folder)}</span>}
            </div>
            <div className="gw-row-help">
              {f.hasGit
                ? "This folder has a Git repository. The agent can see history and commit, with no network."
                : f.canInit
                  ? "No repository yet. Git can't create one from inside the agent's sandbox, so Gardenwall creates it for you (git init, branch main). The agent can also ask, with a card."
                  : "No repository yet, and Git for Windows wasn't found in Program Files. Install it for all users to create one here."}
            </div>
          </div>
          <div className="gw-row-control">
            {f.hasGit ? (
              <Pill tone="ok">Ready</Pill>
            ) : f.canInit ? (
              <Button
                size="sm"
                disabled={busy !== null}
                onClick={async () => {
                  setBusy(index);
                  setError(null);
                  setDone(null);
                  try {
                    await api.initProjectRepository(project.id, index);
                    setDone(`Created a Git repository in ${shownPath(f.folder)} (branch main).`);
                    await load();
                    setTick((t) => t + 1);
                  } catch (e) {
                    setError(errorText(e));
                  } finally {
                    setBusy(null);
                  }
                }}
              >
                {busy === index ? "Creating…" : "Create repository"}
              </Button>
            ) : (
              <span className="muted small">Git not found</span>
            )}
          </div>
        </div>
      ))}
    </>
  );
}

/**
 * Languages and build tools for this project only (ADR-022 §8). Ones added
 * for every project (Settings → Agent) show as such and can't be switched
 * off here.
 */
function ProjectToolchains({
  project,
  state,
  refresh,
  onError,
}: {
  project: Project;
  state: AppView;
  refresh: () => Promise<void>;
  onError: (e: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  if (state.toolchains.length === 0) return null;
  const mine = state.agentSettings.projectToolchains[project.id] ?? [];
  const everywhere = state.agentSettings.toolchains;
  const set = async (id: string, on: boolean) => {
    setBusy(true);
    onError(null);
    try {
      await api.setProjectToolchains(project.id, on ? [...new Set([...mine, id])] : mine.filter((t) => t !== id));
      await refresh();
    } catch (e) {
      onError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {state.toolchains.map((t) => {
        const global = everywhere.includes(t.id);
        return (
          <div className="gw-row" key={t.id}>
            <div className="gw-row-text">
              <div className="gw-row-label">
                {t.name} <span className="muted small">{t.version}</span>
              </div>
              <div className="gw-row-help">
                {global
                  ? "Added for every project in Settings → Agent."
                  : mine.includes(t.id)
                    ? "The agent builds, runs and tests with it in this project, inside its sandbox, with no network."
                    : "Not used in this project."}
              </div>
            </div>
            <div className="gw-row-control">
              {global ? (
                <Pill tone="ok">Every project</Pill>
              ) : (
                <Toggle label={`${t.name} in this project`} checked={mine.includes(t.id)} disabled={busy} onChange={(on) => void set(t.id, on)} />
              )}
            </div>
          </div>
        );
      })}
    </>
  );
}
