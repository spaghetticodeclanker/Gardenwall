import { useCallback, useEffect, useState } from "react";
import { api, errorText, type FolderChanges } from "./api";
import { usePoll } from "./usePoll";
import { shownPath } from "./paths";

/** The core answers this while a turn holds the session (not a failure). */
const isBusy = (e: unknown) => /is working/i.test(errorText(e));

const label = { created: "new", modified: "changed", deleted: "deleted", unchanged: "" } as const;

function lineClass(line: string): string | undefined {
  if (line.startsWith("+") && !line.startsWith("+++")) return "diff-add";
  if (line.startsWith("-") && !line.startsWith("---")) return "diff-del";
  return undefined;
}

/**
 * Everything the session changed with its file tools: one diff per file,
 * from before the session first touched it to now. Read from the core's
 * checkpoints, so it shows what really changed, not what the model says.
 */
export function SessionChanges({ sid, busy, active = true }: { sid: string; busy: boolean; active?: boolean }) {
  const [folders, setFolders] = useState<FolderChanges[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [waiting, setWaiting] = useState(false);
  const [why, setWhy] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  /** The file the user is being asked to confirm putting back. */
  const [asking, setAsking] = useState<string | null>(null);
  const putBack = (folder: string, path: string) => {
    setError(null);
    api
      .agentRevertFile(sid, folder, path)
      .then((left) => {
        setFolders(left);
        setAsking(null);
      })
      .catch((e) => setError(errorText(e)));
  };

  // Kept up to date while the tab is open: after each step, and at once when the agent stops.
  const load = useCallback(() => {
    api.agentChanges(sid).then(
      (f) => {
        setFolders(f);
        setWaiting(false);
        setError((e) => (e && /is working/i.test(e) ? null : e));
      },
      (e) => (isBusy(e) ? setWaiting(true) : setError(errorText(e))),
    );
  }, [sid]);
  useEffect(() => load(), [load, busy]);
  usePoll(load, busy ? 2500 : 5000, active);

  const count = folders?.reduce((n, f) => n + f.files.length, 0) ?? 0;
  return (
    <div className="session-changes">
      <div className="session-changes-head">
        <strong>{folders ? (count === 0 ? "No changes yet" : `${count} ${count === 1 ? "file" : "files"} changed`) : "Changes"}</strong>
        {(busy || waiting) && <span className="muted small">Updating as the agent works…</span>}
      </div>
      <p className="small muted">
        What this session changed, with its tools or its commands, and how to put a file back.{" "}
        <button className="link small" aria-expanded={why} onClick={() => setWhy((w) => !w)}>
          {why ? "Less" : "What's covered?"}
        </button>
      </p>
      {why && (
        <p className="small muted session-changes-why">
          Each diff runs from before the session first touched the file to now. Files over 4 MB, generated folders
          (node_modules, target, .git) and secret files aren't covered. Putting a file back restores the copy saved
          before the session changed it, as a step of its own.
        </p>
      )}
      {error && <div className="banner error">{error}</div>}
      {folders && count === 0 && <p className="small muted">Files the agent changes will be listed here.</p>}
      {folders?.map((f) => (
        <div key={f.folder}>
          {folders.length > 1 && <div className="mono small muted">{shownPath(f.folder)}</div>}
          <ul className="plain-list">
            {f.files.map((d) => {
              const key = `${f.folder}\n${d.path}`;
              return (
                <li key={key}>
                  <button className="link small" onClick={() => setOpen(open === key ? null : key)}>
                    <span className="mono">{d.path}</span> <span className="muted">({label[d.change]})</span>
                  </button>{" "}
                  {asking === key ? (
                    <span className="small">
                      {d.change === "created" ? "Remove this file again?" : "Put it back to how it was before this session?"}{" "}
                      <button className="small" onClick={() => putBack(f.folder, d.path)}>
                        Put back
                      </button>{" "}
                      <button className="link small" onClick={() => setAsking(null)}>
                        Cancel
                      </button>
                    </span>
                  ) : (
                    <button className="link small" onClick={() => setAsking(key)}>
                      Put back
                    </button>
                  )}
                  {open === key &&
                    (d.unified ? (
                      <pre className="approval-pre">
                        {d.unified.split("\n").map((line, i) => (
                          <span key={i} className={lineClass(line)}>
                            {line}
                            {"\n"}
                          </span>
                        ))}
                      </pre>
                    ) : (
                      <p className="small muted">
                        No text to show (a binary or very large file): {d.beforeBytes ?? 0} bytes before,{" "}
                        {d.afterBytes ?? 0} after.
                      </p>
                    ))}
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </div>
  );
}
