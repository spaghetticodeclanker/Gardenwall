// "Folders above the project" (ADR-025): an
// opt-in, per project or for every project, that lets the agent's sandbox
// see the names in each folder above a project (not open, read or change
// anything in them), so Ruby, Rails and Gradle can run there. Everything
// shown comes from the core: the exact folders, what of each becomes
// visible, and the administrator step's command (copied, never run by the
// app).
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type ParentFoldersStatus } from "./api";
import { copyText } from "./clipboard";
import { Button, Pill, SettingRow, toBody } from "./ui";

const DESCRIPTION =
  "When on, the agent's sandbox can see the names of all the files and folders in each folder above the project (for example your user folder), but it still can't open, read or change any of them. Ruby, Rails and Gradle builds (Java, Kotlin) need this to run in the sandbox.";

/** The Set up row (project page) or the Settings → Agent row (`project` null). */
export function ParentFoldersRow({ project, onChanged }: { project: string | null; onChanged?: () => void }) {
  const [status, setStatus] = useState<ParentFoldersStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      setStatus(await api.parentFoldersStatus(project));
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, [project]);
  useEffect(() => void load(), [load]);

  const global = project === null;
  const onHere = status ? (global ? status.global : status.on) : false;
  const missing = status ? status.folders.some((f) => f.admin && !f.done && !f.problem) : false;
  let state: string;
  if (!status) state = "";
  else if (!onHere) state = "Off";
  else if (!global && status.global && !status.projectOn) state = "On for every project";
  else if (missing) state = "On, one step left";
  else state = "On";
  return (
    <>
      <SettingRow
        label={global ? "Folders above projects (every project)" : "Folders above this project"}
        help={
          <>
            {DESCRIPTION}
            {onHere && missing && (
              <div className="gw-shell-status">
                Some folders need a one-time step by an administrator; until then Ruby and Gradle still can't run.
              </div>
            )}
            {!global && status?.global && !status.projectOn && (
              <div className="small muted">Turned on for every project in Settings → Agent.</div>
            )}
            {error && <div className="banner error small">{error}</div>}
          </>
        }
      >
        {state && <Pill tone={state === "On" || state === "On for every project" ? "ok" : state === "Off" ? "neutral" : "warn"}>{state}</Pill>}{" "}
        <Button size="sm" disabled={!status} onClick={() => setOpen(true)}>
          {onHere ? "Details…" : "Turn on…"}
        </Button>
      </SettingRow>
      {open && status && (
        <ParentFoldersPanel
          project={project}
          status={status}
          reload={load}
          onClose={() => setOpen(false)}
          onChanged={async () => {
            await load();
            onChanged?.();
          }}
        />
      )}
    </>
  );
}

function ParentFoldersPanel({
  project,
  status,
  reload,
  onClose,
  onChanged,
}: {
  project: string | null;
  status: ParentFoldersStatus;
  reload: () => Promise<void>;
  onClose: () => void;
  onChanged: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const global = project === null;
  const on = global ? status.global : status.projectOn;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const set = async (value: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.setParentFolders(project, value);
      await onChanged();
      if (!value) onClose();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const copy = async (text: string) => {
    if (await copyText(text)) setCopied(text);
  };
  const where = global ? "each project" : "this project";
  return toBody(
    <div className="modal-backdrop" role="presentation" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal gw-parent" role="dialog" aria-modal="true" aria-labelledby="parent-title">
        <h2 id="parent-title">Let the agent's sandbox see the folders above {where}?</h2>
        <p className="small">
          Some tools check every folder from the top of the drive down to the project before they work. The agent's
          sandbox can't see those folders, so these tools fail inside it.
        </p>

        <h3>What it does, exactly</h3>
        <p className="small">
          For each folder listed below, the agent's sandbox gets one permission: to see the names of all the files and
          folders directly inside it. That's all. It can't open or read any of those files, can't look inside any of
          those folders, and can't create, change or delete anything. The permission is set on each listed folder
          itself, not on anything inside it. Your project's own folders don't change, and no other app gets anything.
        </p>

        <h3>What the agent will be able to see: names only, never contents</h3>
        {status.folders.length === 0 ? (
          <p className="small muted">There are no projects yet, so there are no folders above them.</p>
        ) : (
          <table className="gw-parent-table">
            <tbody>
              {status.folders.map((f) => (
                <tr key={f.path}>
                  <td className="mono">{f.path}</td>
                  <td>
                    {f.what}
                    <div className="small muted">
                      {f.problem
                        ? f.problem
                        : f.done
                          ? "Ready"
                          : f.admin
                            ? "Needs the administrator step below"
                            : "Gardenwall does this while the setting is on"}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="small muted">
          The agent still has no internet. Names it sees reach the model, like anything else its commands print.
        </p>

        <h3>What it makes work</h3>
        <ul className="small">
          <li>Ruby and Ruby on Rails</li>
          <li>Gradle builds for Java and Kotlin (offline, after you fetch the dependencies)</li>
          <li>Java programs and tests that use temporary files</li>
          <li>Git creating a repository by itself</li>
        </ul>

        {status.adminCommand && (
          <>
            <h3>One step for an administrator</h3>
            <div className="banner warn small">
              Some of these folders belong to Windows, so only an administrator can add this permission there.
              Gardenwall never asks for administrator rights itself. Open PowerShell as administrator, paste the
              script below and press Enter. Once is enough for every project. The whole script is shown so you can
              read what runs; it isn't saved anywhere another program could change it.
            </div>
            <pre className="approval-pre gw-parent-script">{status.adminCommand}</pre>
            <div className="row">
              <Button size="sm" onClick={() => void copy(status.adminCommand!)}>
                {copied === status.adminCommand ? "Copied" : "Copy script"}
              </Button>
              <Button size="sm" onClick={() => void reload()}>
                Check again
              </Button>
            </div>
          </>
        )}
        <p className="small muted">
          Gardenwall adds the permission to your own folders while this is on, and removes it when you turn it off or
          close Gardenwall. Sessions started afterwards, or after Restart sandbox, use it.
        </p>
        {status.removeCommand && (
          <details className="small">
            <summary>Taking the administrator step back</summary>
            <p className="muted">
              Turning this off doesn't undo the administrator step (that needs an administrator again). To remove it,
              paste this into PowerShell run as administrator:
            </p>
            <pre className="approval-pre gw-parent-script">{status.removeCommand}</pre>
            <Button size="sm" onClick={() => void copy(status.removeCommand!)}>
              {copied === status.removeCommand ? "Copied" : "Copy script"}
            </Button>
          </details>
        )}
        {error && <div className="banner error">{error}</div>}
        <div className="gw-parent-actions">
          <Button variant="ghost" onClick={onClose}>
            {on ? "Close" : "Cancel"}
          </Button>
          {on ? (
            <Button disabled={busy} onClick={() => void set(false)}>
              Turn off
            </Button>
          ) : (
            <Button variant="primary" disabled={busy} onClick={() => void set(true)}>
              {global ? "Turn on for every project" : "Turn on for this project"}
            </Button>
          )}
        </div>
      </div>
    </div>,
  );
}
