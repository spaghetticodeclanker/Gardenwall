// Model downloads held for the whole window (Find a model redesign, owner
// decisions 2026-11-22): a download started on one tab, card or row keeps
// going, and stays visible in the tray, when you switch tabs or pages.
// Same rules as `useDownload`: Pause keeps what was downloaded, Resume runs
// the same download again (the core continues from its kept chunks), and
// Cancel stops it and the core deletes the partial file.
import { createContext, ReactNode, useCallback, useContext, useMemo, useRef, useState } from "react";
import { api, errorText, type ModelRecord, type Progress } from "./api";
import type { Download, DownloadPhase } from "./useDownload";

export interface StoreJob {
  /** The key the core stops it by: the file's path in its repository. */
  key: string;
  /** What the tray calls it (the file name). */
  label: string;
  run: (onProgress: (p: Progress) => void) => Promise<ModelRecord>;
  onDone?: (model: ModelRecord) => void | Promise<void>;
  onError?: (message: string) => void;
}

export interface StoreEntry {
  key: string;
  label: string;
  phase: DownloadPhase;
  progress: Progress | null;
}

interface Store {
  entries: StoreEntry[];
  start: (job: StoreJob) => void;
  pause: (key: string) => void;
  resume: (key: string) => void;
  cancel: (key: string) => void;
}

const DownloadsContext = createContext<Store | null>(null);

export function DownloadsProvider({ children }: { children: ReactNode }) {
  const [entries, setEntries] = useState<StoreEntry[]>([]);
  const jobs = useRef(new Map<string, StoreJob>());
  const phases = useRef(new Map<string, DownloadPhase>());

  const update = useCallback((key: string, change: Partial<StoreEntry> | null) => {
    setEntries((list) => {
      if (change === null) return list.filter((e) => e.key !== key);
      return list.map((e) => (e.key === key ? { ...e, ...change } : e));
    });
  }, []);
  const setPhase = useCallback(
    (key: string, phase: DownloadPhase) => {
      phases.current.set(key, phase);
      update(key, { phase });
    },
    [update],
  );
  const finish = useCallback(
    (key: string) => {
      jobs.current.delete(key);
      phases.current.delete(key);
      update(key, null);
    },
    [update],
  );
  const go = useCallback(
    async (key: string) => {
      const job = jobs.current.get(key);
      if (!job) return;
      setPhase(key, "running");
      try {
        const model = await job.run((p) => update(key, { progress: p }));
        finish(key);
        await job.onDone?.(model);
      } catch (e) {
        const was = phases.current.get(key);
        if (was === "pausing") {
          setPhase(key, "paused");
          return;
        }
        finish(key);
        if (was !== "cancelling") job.onError?.(errorText(e));
      }
    },
    [finish, setPhase, update],
  );

  const store = useMemo<Store>(
    () => ({
      entries,
      start: (job) => {
        if (phases.current.has(job.key)) return;
        jobs.current.set(job.key, job);
        phases.current.set(job.key, "running");
        setEntries((list) => [...list, { key: job.key, label: job.label, phase: "running", progress: null }]);
        void go(job.key);
      },
      pause: (key) => {
        if (phases.current.get(key) !== "running") return;
        setPhase(key, "pausing");
        void api.pauseDownload(key);
      },
      resume: (key) => {
        if (phases.current.get(key) === "paused") void go(key);
      },
      cancel: (key) => {
        const phase = phases.current.get(key);
        if (phase === "paused") {
          // Nothing is running: the core deletes the kept partial file now.
          void api.cancelDownload(key).finally(() => finish(key));
        } else if (phase === "running" || phase === "pausing") {
          setPhase(key, "cancelling");
          void api.cancelDownload(key);
        }
      },
    }),
    [entries, finish, go, setPhase],
  );
  return <DownloadsContext.Provider value={store}>{children}</DownloadsContext.Provider>;
}

export function useDownloads(): Store {
  const store = useContext(DownloadsContext);
  if (!store) throw new Error("useDownloads needs a DownloadsProvider");
  return store;
}

/** One download in the store, shaped like `useDownload` so `DownloadControls` can show it. */
export function useStoreDownload(key: string): Download<ModelRecord> & { entry: StoreEntry | null } {
  const store = useDownloads();
  const entry = store.entries.find((e) => e.key === key) ?? null;
  return {
    entry,
    phase: entry?.phase ?? "idle",
    progress: entry?.progress ?? null,
    key: entry ? key : null,
    start: () => {},
    pause: () => store.pause(key),
    resume: () => store.resume(key),
    cancel: () => store.cancel(key),
  };
}
