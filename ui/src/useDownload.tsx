// Model downloads with Pause/Resume and Cancel.
// Pause stops the download and keeps what was downloaded; Resume starts the
// same download again, which continues from the kept chunks. Cancel stops
// it and the core deletes the partial file (also while paused).
import { useRef, useState } from "react";
import { Pause, Play, X } from "lucide-react";
import { api, errorText, Progress } from "./api";
import { ProgressBar } from "./ProgressBar";
import { Button } from "./ui";

export type DownloadPhase = "idle" | "running" | "pausing" | "paused" | "cancelling";

type Job<T> = {
  /** The key the core stops it by: the file's path in its repository (a starter entry's model file). */
  key: string;
  run: (onProgress: (p: Progress) => void) => Promise<T>;
  onDone?: (result: T) => void | Promise<void>;
  onError?: (message: string) => void;
};

export interface Download<T> {
  phase: DownloadPhase;
  progress: Progress | null;
  /** The running or paused download's key, or null. */
  key: string | null;
  start: (job: Job<T>) => void;
  pause: () => void;
  resume: () => void;
  cancel: () => void;
}

export function useDownload<T = unknown>(): Download<T> {
  const [phase, setPhaseState] = useState<DownloadPhase>("idle");
  const [progress, setProgress] = useState<Progress | null>(null);
  const [key, setKey] = useState<string | null>(null);
  const phaseRef = useRef<DownloadPhase>("idle");
  const job = useRef<Job<T> | null>(null);
  const setPhase = (p: DownloadPhase) => {
    phaseRef.current = p;
    setPhaseState(p);
  };
  const finish = () => {
    job.current = null;
    setKey(null);
    setProgress(null);
    setPhase("idle");
  };
  const go = async () => {
    const j = job.current;
    if (!j) return;
    setPhase("running");
    try {
      const result = await j.run(setProgress);
      finish();
      await j.onDone?.(result);
    } catch (e) {
      const was = phaseRef.current;
      if (was === "pausing") {
        setPhase("paused");
        return;
      }
      finish();
      if (was !== "cancelling") j.onError?.(errorText(e));
    }
  };
  return {
    phase,
    progress,
    key,
    start: (next) => {
      if (phaseRef.current !== "idle") return;
      job.current = next;
      setKey(next.key);
      setProgress(null);
      void go();
    },
    pause: () => {
      const j = job.current;
      if (!j || phaseRef.current !== "running") return;
      setPhase("pausing");
      void api.pauseDownload(j.key);
    },
    resume: () => {
      if (phaseRef.current === "paused") void go();
    },
    cancel: () => {
      const j = job.current;
      if (!j) return;
      if (phaseRef.current === "paused") {
        // Nothing is running: the core deletes the kept partial file now.
        void api.cancelDownload(j.key).finally(finish);
      } else if (phaseRef.current === "running" || phaseRef.current === "pausing") {
        setPhase("cancelling");
        void api.cancelDownload(j.key);
      }
    },
  };
}

/** Progress bar with Pause/Resume and Cancel, for a download that isn't idle. */
export function DownloadControls<T>({ dl }: { dl: Download<T> }) {
  if (dl.phase === "idle") return null;
  const paused = dl.phase === "paused";
  const status =
    dl.phase === "pausing" ? "Pausing…" : dl.phase === "cancelling" ? "Cancelling…" : paused ? "Paused" : undefined;
  return (
    <span className="gw-download">
      <ProgressBar p={dl.progress} status={status ?? (dl.progress ? undefined : "Starting…")} />
      {paused ? (
        <Button size="sm" icon={Play} onClick={dl.resume} title="Continue from what was downloaded">
          Resume
        </Button>
      ) : (
        <Button
          size="sm"
          icon={Pause}
          onClick={dl.pause}
          disabled={dl.phase !== "running"}
          title="Stop for now and keep what was downloaded"
        >
          Pause
        </Button>
      )}
      <Button
        size="sm"
        variant="ghost"
        icon={X}
        onClick={dl.cancel}
        disabled={dl.phase === "cancelling"}
        title="Stop and delete what was downloaded"
      >
        Cancel
      </Button>
    </span>
  );
}
