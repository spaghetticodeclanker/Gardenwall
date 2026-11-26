// Update center (ADR-017). Lists what can be updated, lets the user choose
// when Gardenwall checks, switch or remove runtimes, and check each hub
// model for a newer file. A check only looks: nothing is downloaded or
// installed until the user chooses it here.
//
// Layout (docs/internal/design/REDESIGN-GAPS.md, round 1): grouped rows for the
// schedule, Gardenwall and the graphics driver; aligned tables for runtimes
// and models, with a status pill and actions per row.
import { VcRuntimeNotice } from "./VcRuntimeNotice";
import { useEffect, useRef, useState } from "react";
import {
  api,
  AppVersion,
  AppView,
  errorText,
  formatBytes,
  ModelRecord,
  ModelUpdate,
  Progress,
  UpdateSchedule,
  UpdatesView as UpdatesState,
} from "./api";
import { InlineProgress } from "./ProgressBar";
import { DownloadControls, useDownload } from "./useDownload";
import { Button, Pill, SettingGroup, SettingRow } from "./ui";

const SCHEDULES: { value: UpdateSchedule; label: string; detail: string }[] = [
  {
    value: "onLaunch",
    label: "Each time Gardenwall opens (recommended)",
    detail: "You hear about security fixes as soon as possible.",
  },
  { value: "manual", label: "Manual", detail: "Only when you press Check for updates." },
  { value: "daily", label: "Custom schedule: once a day", detail: "At startup or while open, once a day has passed." },
  { value: "weekly", label: "Custom schedule: once a week", detail: "At startup or while open, once a week has passed." },
];

function when(ms: number | null): string {
  return ms ? new Date(ms).toLocaleString() : "never";
}

function ModelRow({ model, source, onDownloaded }: {
  model: ModelRecord;
  source: { repo: string; commit: string };
  onDownloaded: () => Promise<void>;
}) {
  const [status, setStatus] = useState<ModelUpdate | "checking" | null>(null);
  const dl = useDownload<ModelRecord>();
  const [error, setError] = useState<string | null>(null);
  const checked = status && status !== "checking" ? status : null;
  return (
    <tr>
      <td>
        <div>{model.displayName}</div>
        <div className="small muted mono">
          {source.repo} @ {source.commit.slice(0, 10)}
        </div>
        {checked?.status === "upToDate" && <div className="small muted">Newest commit {checked.commit.slice(0, 10)}.</div>}
        {checked?.status === "missing" && (
          <div className="small warn-text">This file is no longer in the repository, or has no verifiable hash.</div>
        )}
        {checked?.status === "available" && (
          <div className="small">
            A different file is in commit {checked.commit.slice(0, 10)} ({formatBytes(checked.size)}). Downloading it adds a
            new model and keeps this one until you delete it.
          </div>
        )}
        <DownloadControls dl={dl} />
        {error && <div className="small error-text">{error}</div>}
      </td>
      <td>
        {status === "checking" ? (
          <Pill tone="neutral">Checking…</Pill>
        ) : checked?.status === "upToDate" ? (
          <Pill tone="ok">Up to date</Pill>
        ) : checked?.status === "available" ? (
          <Pill tone="accent">Update available</Pill>
        ) : checked?.status === "missing" ? (
          <Pill tone="warn">Can't check</Pill>
        ) : (
          <Pill tone="neutral">Not checked</Pill>
        )}
      </td>
      <td className="gw-utable-actions">
        {checked?.status === "available" ? (
          dl.phase === "idle" && (
            <Button
              size="sm"
              onClick={() => {
                setError(null);
                dl.start({
                  key: checked.path,
                  run: (onProgress) => api.downloadModel(checked.repo, checked.path, onProgress),
                  onDone: async () => {
                    setStatus(null);
                    await onDownloaded();
                  },
                  onError: setError,
                });
              }}
            >
              Download update
            </Button>
          )
        ) : (
          <Button
            size="sm"
            disabled={status === "checking"}
            onClick={async () => {
              setError(null);
              setStatus("checking");
              try {
                setStatus(await api.checkModelUpdate(model.sha256));
              } catch (e) {
                setStatus(null);
                setError(errorText(e));
              }
            }}
          >
            Check
          </Button>
        )}
      </td>
    </tr>
  );
}

/** Links never open from Gardenwall; the address is copied for the user's own browser. */
function CopyAddress({ url, label = "Copy the bulletin's address" }: { url: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      className="link small"
      title={url}
      onClick={() => void navigator.clipboard.writeText(url).then(() => setCopied(true))}
    >
      {copied ? "Address copied" : label}
    </button>
  );
}

/**
 * A newer Gardenwall (ADR-017 §3.3): notes, Download (signature checked by
 * the core), then Install with one more confirmation. The Releases page is
 * always offered for people who prefer to install it themselves.
 */
function AppUpdatePanel({ app, onChanged }: { app: AppVersion; onChanged: (u: UpdatesState) => void }) {
  const [progress, setProgress] = useState<Progress | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const version = app.available!;

  const download = async () => {
    setError(null);
    setProgress(null);
    setDownloading(true);
    try {
      onChanged(await api.downloadAppUpdate(setProgress));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setDownloading(false);
    }
  };
  const install = async () => {
    setError(null);
    setInstalling(true);
    try {
      // Returns only when something went wrong: on success the app closes.
      await api.installAppUpdate();
    } catch (e) {
      setError(errorText(e));
      setConfirming(false);
      onChanged(await api.getUpdates());
    } finally {
      setInstalling(false);
    }
  };

  return (
    <section className="gw-app-update" aria-label={`Gardenwall ${version}`}>
      {app.security && (
        <div className="banner warn">
          <strong>Version {version} fixes a security problem.</strong> Install it soon.
        </div>
      )}
      <h3>
        What's new in {version}
        {app.published && <span className="small muted"> · released {app.published}</span>}
      </h3>
      {app.notes ? <pre className="gw-release-notes">{app.notes}</pre> : <p className="small muted">This release has no notes.</p>}
      {error && <div className="banner error">{error}</div>}
      {app.cannotInstall ? (
        <p className="small muted">{app.cannotInstall}</p>
      ) : confirming ? (
        <div className="banner info">
          <p>
            Gardenwall will close, install version {version} and open again. The model is unloaded and running agent
            sessions stop; saved chats and sessions stay as they are.
          </p>
          <div className="gw-app-update-actions">
            <Button variant="primary" disabled={installing} onClick={() => void install()}>
              {installing ? "Starting the installer…" : "Install now"}
            </Button>
            <Button disabled={installing} onClick={() => setConfirming(false)}>
              Not now
            </Button>
          </div>
        </div>
      ) : app.downloaded ? (
        <div className="gw-app-update-actions">
          <Button variant="primary" onClick={() => setConfirming(true)}>
            Install and restart
          </Button>
          <span className="small muted">Downloaded, and its signature matches Gardenwall's release key.</span>
        </div>
      ) : downloading || app.downloading ? (
        <p className="small">
          Downloading version {version}: <InlineProgress p={progress} />
        </p>
      ) : (
        <div className="gw-app-update-actions">
          <Button variant="primary" onClick={() => void download()}>
            Download version {version}
          </Button>
          <span className="small muted">
            Nothing installs yet. The download is checked against Gardenwall's release signature first.
          </span>
        </div>
      )}
      <p className="small muted">
        Prefer to do it yourself? Download the installer from GitHub Releases and run it; it updates Gardenwall in
        place and keeps your data. <CopyAddress url={app.releasesPage} label="Copy the Releases page address" />
      </p>
    </section>
  );
}

export function UpdatesView({
  state,
  refresh,
  onChanged,
  focusRuntime = 0,
  onDiagnostics,
}: {
  state: AppView;
  /** Opens the diagnostics report. */
  onDiagnostics?: () => void;
  refresh: () => Promise<void>;
  onChanged: (u: UpdatesState) => void;
  /** Changes when a link to the runtimes (the old Runtime page's) opens this page: scroll to Inference runtime. */
  focusRuntime?: number;
}) {
  const [updates, setUpdates] = useState<UpdatesState | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [install, setInstall] = useState<{ id: string; progress: Progress | null } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const apply = (u: UpdatesState) => {
    setUpdates(u);
    onChanged(u);
  };
  const run = async (what: string, f: () => Promise<UpdatesState>) => {
    setError(null);
    setBusy(what);
    try {
      apply(await f());
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    api.getUpdates().then(apply).catch((e) => setError(errorText(e)));
    // Loaded once when the page opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The Runtime page was folded into this page (final review item 9): its
  // links open Updates at the Inference runtime section.
  const runtimeSection = useRef<HTMLElement>(null);
  const loadedPage = updates !== null;
  useEffect(() => {
    if (focusRuntime > 0 && loadedPage) runtimeSection.current?.scrollIntoView({ block: "start" });
  }, [focusRuntime, loadedPage]);
  const installFirst = async (id: string) => {
    setError(null);
    setInstall({ id, progress: null });
    try {
      await api.installRuntime(id, (p) => setInstall({ id, progress: p }));
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setInstall(null);
    }
  };

  if (!updates) return <div className="page muted">Loading…</div>;
  const report = updates.report;
  const loadedRuntime = state.loaded !== null;
  const hubModels = state.models.flatMap((m) => (m.source.kind === "hub" ? [{ m, source: m.source }] : []));
  const schedule = SCHEDULES.find((s) => s.value === updates.schedule) ?? SCHEDULES[0]!;

  return (
    <div className="page gw-updates">
      {error && <div className="banner error">{error}</div>}
      <div className="gw-updates-head">
        <p className="small muted">
          Checking only looks. Nothing is downloaded or installed until you choose it here, and every update can be
          undone.
        </p>
        <span className="small muted gw-nowrap">Last checked: {when(updates.lastCheckMs)}</span>
        <Button variant="primary" disabled={busy !== null} onClick={() => void run("check", api.checkUpdates)}>
          {busy === "check" ? "Checking…" : "Check for updates"}
        </Button>
        {onDiagnostics && (
          <Button onClick={onDiagnostics} title="A report of versions, hardware and recent errors to share with a bug report">
            Copy diagnostics…
          </Button>
        )}
      </div>
      {report?.offline && <div className="banner info">Offline mode is on, so checks that need the network are skipped.</div>}

      <SettingGroup>
        <SettingRow label="When to check" help={schedule.detail}>
          <select
            value={updates.schedule}
            disabled={busy !== null}
            aria-label="When to check for updates"
            onChange={(e) => void run("schedule", () => api.setUpdateSchedule(e.target.value as UpdateSchedule))}
          >
            {SCHEDULES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </SettingRow>
        <SettingRow
          label="Gardenwall"
          help={
            <>
              Version {report?.app.current ?? "unknown"}
              {report?.app.available && <strong> · version {report.app.available} is available</strong>}
              {report?.app.note && <div>{report.app.note}</div>}
              {report?.app.checkError && <div>{report.app.checkError}</div>}
            </>
          }
        >
          {!report ? (
            <Pill tone="neutral">Not checked</Pill>
          ) : report.app.available ? (
            <Pill tone={report.app.security ? "warn" : "accent"}>{report.app.security ? "Security update" : "Update available"}</Pill>
          ) : report.app.checkError ? (
            <Pill tone="warn">Can't check</Pill>
          ) : (
            <Pill tone="ok">Up to date</Pill>
          )}
        </SettingRow>
        {report?.gpuDriver && (
          <SettingRow label="Graphics driver" help={`${report.gpuDriver.adapter}: driver ${report.gpuDriver.installed}; the fix is in ${report.gpuDriver.fixed} and later.`}>
            <Pill tone="warn">Update needed</Pill>
          </SettingRow>
        )}
        {report?.gpuDriverCurrent && !report.gpuDriver && (
          <SettingRow
            label="Graphics driver"
            help={
              <>
                {report.gpuDriverCurrent.adapter} has driver {report.gpuDriverCurrent.installed}, which includes the latest
                security fix. Compared with {report.gpuDriverCurrent.bulletin} (fixed versions last read from NVIDIA on{" "}
                {report.gpuDriverCurrent.reviewed}). Only NVIDIA drivers are checked so far.
              </>
            }
          >
            <Pill tone="ok">Up to date</Pill>
          </SettingRow>
        )}
      </SettingGroup>

      {report?.app.available && <AppUpdatePanel app={report.app} onChanged={apply} />}

      {report?.gpuDriver && (
        <div className="banner warn">
          <p>
            <strong>Your graphics driver is older than its latest security fix.</strong> {report.gpuDriver.adapter} has
            driver {report.gpuDriver.installed}; the fix is in {report.gpuDriver.fixed} and later.
          </p>
          <p className="small">
            {report.gpuDriver.summary} Gardenwall keeps the model in a sandbox, but the model still uses the graphics
            driver, and a driver problem is outside what a sandbox can contain. Gardenwall never installs drivers: get the
            update from the NVIDIA app or NVIDIA's driver download page.
          </p>
          <p className="small muted">
            {report.gpuDriver.bulletin}. Fixed versions last read from NVIDIA on {report.gpuDriver.reviewed}.{" "}
            <CopyAddress url={report.gpuDriver.source} />
          </p>
        </div>
      )}

      <section className="gw-group gw-updates-runtime" ref={runtimeSection}>
        <h3 className="gw-group-title">Inference runtime</h3>
        <VcRuntimeNotice state={state} refresh={refresh} />
        <p className="gw-row-help gw-updates-note">
          The active runtime loads your models. Switching back to another installed build is how you undo a runtime
          update. Builds that this version of Gardenwall no longer pins can't run and can be removed.
        </p>
        <p className="gw-row-help gw-updates-note">
          Gardenwall runs models with llama.cpp. Each build is pinned by version and SHA-256 in Gardenwall's source,
          downloaded from the official GitHub release, verified, and checked file by file before every launch.
        </p>
        {!state.sandboxAvailable && (
          <div className="banner warn">Process sandboxing is not yet available on this platform, so models cannot be loaded here.</div>
        )}
        {!report ? (
          state.runtimes.length === 0 ? (
            <div className="gw-group-body gw-updates-empty small muted">No build is available for this system.</div>
          ) : (
            // Before the first check: the builds for this system, so a first
            // install needs no check (what the Runtime page showed).
            <div className="gw-group-body">
              <table className="gw-utable">
                <thead>
                  <tr>
                    <th scope="col">Build</th>
                    <th scope="col">Status</th>
                    <th scope="col">
                      <span className="gw-sr">Actions</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {state.runtimes.map((r) => (
                    <tr key={r.id}>
                      <td>
                        <div>{r.backend || "Unknown"}</div>
                        <div className="small muted">
                          <span className="mono">{r.id}</span> · {r.downloadMb} MB download
                        </div>
                        {install?.id === r.id && (
                          <div className="small">
                            <InlineProgress p={install.progress} />
                          </div>
                        )}
                      </td>
                      <td>
                        <span className="gw-utable-pills">
                          {r.installed ? <Pill tone="ok">Installed</Pill> : <Pill tone="neutral">Not installed</Pill>}
                        </span>
                      </td>
                      <td className="gw-utable-actions">
                        {!r.installed && (
                          <Button size="sm" disabled={install !== null || busy !== null} onClick={() => void installFirst(r.id)}>
                            Install ({r.downloadMb} MB)
                          </Button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <p className="gw-updates-empty small muted">
                Check for updates to see test results, security notices and builds this version no longer pins.
              </p>
            </div>
          )
        ) : report.runtimes.length === 0 ? (
          <div className="gw-group-body gw-updates-empty small muted">No build is available for this system.</div>
        ) : (
          <div className="gw-group-body">
            <table className="gw-utable">
              <thead>
                <tr>
                  <th scope="col">Build</th>
                  <th scope="col">Status</th>
                  <th scope="col">
                    <span className="gw-sr">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {report.runtimes.map((r) => (
                  <tr key={r.id}>
                    <td>
                      <div>
                        {r.backend || "Unknown"} · {r.version}
                      </div>
                      <div className="small muted mono">{r.id}</div>
                      {r.advisories.map((a) => (
                        <div key={a.id} className="small error-text">
                          Known security problem ({a.id}): {a.summary} Fixed in {a.fixedIn}.{" "}
                          {r.active
                            ? "Install that build and make it active. Until then, load only models you trust."
                            : "Don't switch back to this build unless you must."}
                        </div>
                      ))}
                      {r.smoke && (
                        <div className={r.smoke.completion ? "small muted" : "small error-text"}>
                          {!r.smoke.completion
                            ? `Test failed with ${r.smoke.model}: ${r.smoke.error ?? "no answer"}`
                            : r.smoke.toolCall
                              ? `Tested with ${r.smoke.model}: answers and tool calls work (${r.smoke.seconds} s).`
                              : `Tested with ${r.smoke.model}: answers work, but the model made no tool call. Small models sometimes don't; try a larger one.`}
                        </div>
                      )}
                      {busy === `test:${r.id}` && <div className="small muted">Testing: loading the smallest model…</div>}
                      {install?.id === r.id && (
                        <div className="small">
                          <InlineProgress p={install.progress} />
                        </div>
                      )}
                    </td>
                    <td>
                      <span className="gw-utable-pills">
                        {r.active ? (
                          <Pill tone="accent">Active</Pill>
                        ) : r.installed ? (
                          <Pill tone="neutral">Installed</Pill>
                        ) : (
                          <Pill tone="neutral">Not installed</Pill>
                        )}
                        {!r.pinned && <Pill tone="neutral">No longer pinned</Pill>}
                        {r.advisories.length > 0 && <Pill tone="danger">Security problem</Pill>}
                      </span>
                    </td>
                    <td className="gw-utable-actions">
                      {r.pinned && !r.installed && (
                        <Button
                          size="sm"
                          disabled={install !== null || busy !== null}
                          onClick={async () => {
                            setError(null);
                            setInstall({ id: r.id, progress: null });
                            try {
                              await api.installRuntime(r.id, (p) => setInstall({ id: r.id, progress: p }));
                              apply(await api.checkUpdates());
                              await refresh();
                            } catch (e) {
                              setError(errorText(e));
                            } finally {
                              setInstall(null);
                            }
                          }}
                        >
                          Install ({r.downloadMb} MB)
                        </Button>
                      )}
                      {r.pinned && r.installed && (
                        <Button
                          size="sm"
                          disabled={busy !== null || install !== null}
                          title="Loads your smallest model on this runtime in the sandbox, then asks for an answer and a tool call"
                          onClick={() => void run(`test:${r.id}`, () => api.testRuntime(r.id))}
                        >
                          Test
                        </Button>
                      )}
                      {r.pinned && r.installed && !r.active && (
                        <Button
                          size="sm"
                          disabled={busy !== null}
                          title="Used the next time a model loads"
                          onClick={() => void run(r.id, () => api.setActiveRuntime(r.id))}
                        >
                          Use this runtime
                        </Button>
                      )}
                      {r.installed && !r.active && (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="gw-danger-text"
                          disabled={busy !== null || (loadedRuntime && r.active)}
                          onClick={() => {
                            if (!window.confirm(`Remove runtime ${r.id}? You can install it again later if it is still pinned.`)) return;
                            void run(r.id, () => api.removeRuntime(r.id));
                          }}
                        >
                          Remove
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="gw-group">
        <h3 className="gw-group-title">Models</h3>
        <p className="gw-row-help gw-updates-note">
          Each check asks Hugging Face for the newest commit of the model's repository, so models are checked only when
          you ask.
        </p>
        {hubModels.length === 0 ? (
          <div className="gw-group-body gw-updates-empty small muted">No downloaded models. Imported files can't be checked.</div>
        ) : (
          <div className="gw-group-body">
            <table className="gw-utable">
              <thead>
                <tr>
                  <th scope="col">Model</th>
                  <th scope="col">Status</th>
                  <th scope="col">
                    <span className="gw-sr">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {hubModels.map(({ m, source }) => (
                  <ModelRow key={m.sha256} model={m} source={source} onDownloaded={refresh} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
