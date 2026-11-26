import { useEffect, useState } from "react";
import { DatabaseBackup, RotateCcw } from "lucide-react";
import { api, BackupContents, BackupStatus, ChosenBackup, DataCounts, errorText } from "./api";
import { Button, Fold, SettingGroup, SettingRow, useToast } from "./ui";

/** A rough strength hint (the core enforces the minimum length). */
function strength(pass: string, min: number): { label: string; tone: "danger" | "warn" | "ok" } {
  const length = [...pass].length;
  if (length < min) return { label: `Too short: at least ${min} characters`, tone: "danger" };
  const kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter((r) => r.test(pass)).length;
  const words = pass.trim().split(/\s+/).length;
  if (length >= 20 || (length >= 16 && (kinds >= 3 || words >= 4))) return { label: "Strong", tone: "ok" };
  return { label: "OK, longer is stronger (a few random words work well)", tone: "warn" };
}

function when(ms: number | null | undefined): string {
  return ms ? new Date(ms).toLocaleString() : "unknown";
}

function sizeText(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

const ROWS: [keyof DataCounts, string][] = [
  ["chats", "Chats"],
  ["agentSessions", "Agent sessions"],
  ["projects", "Projects"],
  ["documentCollections", "Document collections"],
];

function BackUp({ status, reload }: { status: BackupStatus; reload: () => Promise<void> }) {
  const [pass, setPass] = useState("");
  const [again, setAgain] = useState("");
  const [activity, setActivity] = useState(true);
  // Opt-in: only if the user wants them.
  const [memories, setMemories] = useState(false);
  const [skills, setSkills] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const toast = useToast();
  const hint = strength(pass, status.minPassphrase);
  const ready = hint.tone !== "danger" && pass === again && !status.cannotBackUp;

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const done = await api.createBackup(pass, { activityLog: activity, memories, skills });
      if (done) {
        setPass("");
        setAgain("");
        toast({ text: `Backed up to ${done.fileName} (${sizeText(done.bytes)})`, tone: "ok" });
        await reload();
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingGroup title="Back up">
      <SettingRow
        label="Last backup"
        help="Gardenwall doesn't remind you; make a backup before reinstalling Windows or moving to a new computer."
      >
        <span className="small">
          {status.lastMs ? (
            <>
              {when(status.lastMs)}
              {status.lastFile && <span className="muted"> ({status.lastFile})</span>}
            </>
          ) : (
            <span className="muted">Never</span>
          )}
        </span>
      </SettingRow>
      <div className="gw-backup-form">
        <div className="gw-row-help">
          <Fold
            summary="One file with your chats, agent sessions, projects, documents, drafts, visited-sites lists and settings, locked with a passphrase you choose. Memories, skills and the activity log go in only if you tick them."
            more="What's left out"
          >
            Unless you tick them: memories, your skills folder and the activity log. Always: models and runtimes
            (download them again; they are checked again too), the Brave Search key (enter it
            again), files the agent changed in your folders, undo checkpoints and sandbox permissions. Your chats are
            already encrypted with a key only this Windows account can unlock; the backup holds that key locked with
            your passphrase instead, so it works on another computer or after reinstalling Windows.
          </Fold>
        </div>
        {status.cannotBackUp && <div className="banner warn">{status.cannotBackUp}</div>}
        <label className="gw-field">
          <span>Passphrase</span>
          <input type="password" autoComplete="new-password" value={pass} onChange={(e) => setPass(e.target.value)} disabled={busy} />
        </label>
        {pass && <div className={`small gw-strength gw-strength-${hint.tone}`}>{hint.label}</div>}
        <label className="gw-field">
          <span>Type it again</span>
          <input type="password" autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} disabled={busy} />
        </label>
        {again && again !== pass && <div className="small gw-strength gw-strength-danger">The two don't match.</div>}
        <label className="gw-check">
          <input type="checkbox" checked={memories} onChange={(e) => setMemories(e.target.checked)} disabled={busy} />
          Include your memories (the notes Gardenwall keeps for the assistant)
        </label>
        <label className="gw-check">
          <input type="checkbox" checked={skills} onChange={(e) => setSkills(e.target.checked)} disabled={busy} />
          Include your skills folder (your own skills and their files)
        </label>
        <label className="gw-check">
          <input type="checkbox" checked={activity} onChange={(e) => setActivity(e.target.checked)} disabled={busy} />
          Include the activity log (what the agent asked for and what you decided, no file names or contents)
        </label>
        <div className="banner warn small">
          Keep the passphrase somewhere safe, such as a password manager. If it's lost, the backup can't be opened by
          anyone, including Gardenwall.
        </div>
        {error && <div className="banner error">{error}</div>}
        <div>
          <Button variant="primary" icon={DatabaseBackup} disabled={!ready || busy} onClick={() => void run()}>
            {busy ? "Backing up…" : "Back up…"}
          </Button>
          {busy && <span className="small muted gw-backup-note">This takes a few seconds or more for a large history.</span>}
        </div>
      </div>
    </SettingGroup>
  );
}

function Restore({ status, reload }: { status: BackupStatus; reload: () => Promise<void> }) {
  const [file, setFile] = useState<ChosenBackup | null>(null);
  const [pass, setPass] = useState("");
  const [contents, setContents] = useState<BackupContents | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (what: string, f: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  const reset = () => {
    setFile(null);
    setPass("");
    setContents(null);
    setError(null);
  };

  if (status.restorePending) {
    return (
      <SettingGroup title="Restore">
        <div className="gw-backup-form">
          <div className="banner info">
            A backup is checked and ready. It replaces your current data when Gardenwall restarts; your current data is
            kept aside until the restored data opens, and put back if it doesn't.
          </div>
          {error && <div className="banner error">{error}</div>}
          <div className="gw-inline-controls">
            <Button variant="danger" icon={RotateCcw} disabled={busy !== null} onClick={() => void run("restart", () => api.restartToRestore())}>
              Restart now
            </Button>
            <Button disabled={busy !== null} onClick={() => void run("cancel", async () => { await api.cancelRestore(); reset(); await reload(); })}>
              Cancel the restore
            </Button>
          </div>
        </div>
      </SettingGroup>
    );
  }

  const b = contents?.backup;
  return (
    <SettingGroup title="Restore">
      <div className="gw-backup-form">
        <p className="small muted">
          Replace this computer's chats, sessions and settings with a backup. You see what's inside before anything
          changes.
        </p>
        {!file && (
          <div>
            <Button
              disabled={busy !== null}
              onClick={() =>
                void run("choose", async () => {
                  const chosen = await api.chooseBackup();
                  if (chosen) setFile(chosen);
                })
              }
            >
              Choose a backup file…
            </Button>
          </div>
        )}
        {file && !contents && (
          <>
            <div className="small">
              <strong>{file.fileName}</strong> <span className="muted">({sizeText(file.bytes)})</span>
            </div>
            <label className="gw-field">
              <span>Its passphrase</span>
              <input
                type="password"
                autoComplete="off"
                value={pass}
                onChange={(e) => setPass(e.target.value)}
                disabled={busy !== null}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && pass) void run("open", async () => setContents(await api.inspectBackup(pass)));
                }}
              />
            </label>
            {error && <div className="banner error">{error}</div>}
            <div className="gw-inline-controls">
              <Button
                variant="primary"
                disabled={!pass || busy !== null}
                onClick={() => void run("open", async () => setContents(await api.inspectBackup(pass)))}
              >
                {busy === "open" ? "Opening…" : "Open"}
              </Button>
              <Button disabled={busy !== null} onClick={reset}>
                Cancel
              </Button>
            </div>
          </>
        )}
        {file && contents && b && (
          <>
            <div className="small">
              <strong>{file.fileName}</strong>, made {when(b.createdMs)} by Gardenwall {b.appVersion}
            </div>
            <table className="gw-backup-compare">
              <thead>
                <tr>
                  <th />
                  <th>In the backup</th>
                  <th>Here now (replaced)</th>
                </tr>
              </thead>
              <tbody>
                {ROWS.map(([key, label]) => (
                  <tr key={key}>
                    <td>{label}</td>
                    <td>{b.data[key] ?? 0}</td>
                    <td>{contents.current[key] ?? 0}</td>
                  </tr>
                ))}
                <tr>
                  <td>Memories</td>
                  <td>{b.memoriesIncluded ? b.data.memories : "Not included"}</td>
                  <td>{b.memoriesIncluded ? contents.current.memories : `${contents.current.memories} (kept)`}</td>
                </tr>
                <tr>
                  <td>Latest change</td>
                  <td>{when(b.data.newestMs)}</td>
                  <td>{when(contents.current.newestMs)}</td>
                </tr>
                <tr>
                  <td>Activity log</td>
                  <td>{b.activityLog === null ? "Not included" : `${b.activityLog} entries`}</td>
                  <td>{b.activityLog === null ? `${contents.currentActivity} entries (kept)` : `${contents.currentActivity} entries`}</td>
                </tr>
                <tr>
                  <td>Your skills</td>
                  <td>{b.skills === null ? "Not included" : `${b.skills} files`}</td>
                  <td>{b.skills ? "Replaced" : "Kept"}</td>
                </tr>
              </tbody>
            </table>
            <div className="banner warn small">
              Restoring replaces your current chats, agent sessions, projects, documents and settings
              {b.memoriesIncluded ? ", your memories" : ""}
              {b.skills ? ", your skills" : ""}
              {b.activityLog !== null ? " and the activity log" : ""}.
              {!b.memoriesIncluded || !b.skills ? " What the backup leaves out stays as it is here." : ""} Make a backup of this computer first if you might
              want them. Gardenwall then restarts, which stops open agent sessions and unloads the model.
            </div>
            {error && <div className="banner error">{error}</div>}
            <div className="gw-inline-controls">
              <Button
                variant="danger"
                icon={RotateCcw}
                disabled={busy !== null}
                onClick={() =>
                  void run("restore", async () => {
                    await api.restoreBackup(pass);
                    setPass("");
                    // Shows "ready, restart now" if the restart below fails.
                    await reload();
                    await api.restartToRestore();
                  })
                }
              >
                {busy === "restore" ? "Checking the whole backup…" : "Restore and restart"}
              </Button>
              <Button disabled={busy !== null} onClick={reset}>
                Cancel
              </Button>
            </div>
          </>
        )}
      </div>
    </SettingGroup>
  );
}

/** Settings → Data: back up and restore (release plan feature 3). */
export function BackupSettings() {
  const [status, setStatus] = useState<BackupStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reload = async () => {
    try {
      setStatus(await api.backupStatus());
    } catch (e) {
      setError(errorText(e));
    }
  };
  useEffect(() => {
    void reload();
  }, []);
  if (error) return <div className="banner error">{error}</div>;
  if (!status) return null;
  return (
    <>
      <BackUp status={status} reload={reload} />
      <Restore status={status} reload={reload} />
    </>
  );
}
