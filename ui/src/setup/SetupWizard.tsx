// First-run setup (UI redesign phase 3round 9): four short
// full-screen steps (welcome and how Gardenwall keeps you safe; the engine
// and a model; favourite and launch; done), then Home. Skippable at any step
// and re-openable from Help.
import { useEffect, useRef, useState } from "react";
import { Check, Cpu, Download, EyeOff, Lock, ShieldCheck, Undo2, X } from "lucide-react";
import { api, errorText, formatBytes, type AppView, type Progress } from "../api";
import { Growth } from "../art/Growth";
import { SetupModelPicker } from "../FindModel";
import { Mark } from "../shell/TopBar";
import { Button, ErrorCard, IconButton, Pill, SegmentedBar, Toggle } from "../ui";
import { VcRuntimeNotice } from "../VcRuntimeNotice";
import "./setup.css";

const DONE_KEY = "gw.setup.done";

/** Whether the first-run setup has been finished or skipped on this computer. */
export function setupDone(): boolean {
  try {
    return localStorage.getItem(DONE_KEY) === "1";
  } catch {
    return false;
  }
}

function markDone() {
  try {
    localStorage.setItem(DONE_KEY, "1");
  } catch {
    // Not remembered: setup is offered again next start while there's no model.
  }
}

const STEPS = ["Welcome", "Engine and model", "Favourite", "Done"] as const;

export function SetupWizard({ state, refresh, onClose }: { state: AppView; refresh: () => Promise<void>; onClose: () => void }) {
  const [step, setStep] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const dialog = useRef<HTMLDivElement>(null);
  const finish = () => {
    markDone();
    onClose();
  };
  useEffect(() => {
    dialog.current?.querySelector<HTMLElement>("h1")?.focus();
  }, [step]);
  const finishRef = useRef(finish);
  finishRef.current = finish;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") finishRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const models = state.models.filter((m) => m.architecture !== "clip");
  const runtimeReady = state.runtimes.some((r) => r.installed);

  return (
    <div className="gw-setup" role="dialog" aria-modal="true" aria-label="Set up Gardenwall" ref={dialog}>
      <div className="gw-setup-art" aria-hidden="true">
        <Growth seed="gardenwall-setup" grown={(step + 1) / STEPS.length} anchor={0.6} depth={7} />
      </div>
      <header className="gw-setup-head">
        <span className="gw-setup-brand">
          <Mark /> Gardenwall
        </span>
        <ol className="gw-setup-steps" aria-label="Steps">
          {STEPS.map((s, i) => (
            <li key={s} className={i === step ? "now" : i < step ? "done" : ""} aria-current={i === step ? "step" : undefined}>
              <span className="gw-setup-dot">{i < step ? <Check size={12} aria-hidden="true" /> : i + 1}</span>
              {s}
            </li>
          ))}
        </ol>
        <IconButton icon={X} label="Skip setup" onClick={finish} />
      </header>

      <main className="gw-setup-body">
        {step === 0 && (
          <section className="gw-setup-step gw-setup-welcome">
            <h1 tabIndex={-1}>Welcome to Gardenwall</h1>
            <p className="gw-setup-lead">Run AI models on this computer, chat with them, and let a coding agent work on your projects, safely.</p>
            <ul className="gw-setup-points">
              <li>
                <Lock aria-hidden="true" />
                <div>
                  <b>Everything stays on this computer</b>
                  <span>Models run here. Nothing leaves unless you allow it, such as a web search you approve.</span>
                </div>
              </li>
              <li>
                <ShieldCheck aria-hidden="true" />
                <div>
                  <b>The model can't act on its own</b>
                  <span>It works in a sandbox with only the folders you give it, and its commands have no network. Anything risky asks you first.</span>
                </div>
              </li>
              <li>
                <EyeOff aria-hidden="true" />
                <div>
                  <b>Secrets stay hidden</b>
                  <span>Key files and passwords in your projects are kept out of the agent's reach and stopped before they're committed.</span>
                </div>
              </li>
              <li>
                <Undo2 aria-hidden="true" />
                <div>
                  <b>You stay in control</b>
                  <span>You can undo the agent's edits and most of what its commands change, and Lockdown mode switches off everything optional for a model you don't trust.</span>
                </div>
              </li>
            </ul>
          </section>
        )}

        {step === 1 && (
          <section className="gw-setup-step wide">
            <h1 tabIndex={-1}>Get the engine and a model</h1>
            <p className="gw-setup-lead">Gardenwall runs models with llama.cpp. Install it once, then pick a model that fits this computer.</p>
            <VcRuntimeNotice state={state} refresh={refresh} />
            <RuntimeStep state={state} refresh={refresh} ready={runtimeReady} setError={setError} />
            <h2>Model</h2>
            {models.length > 0 && (
              <p className="muted">
                You have {models.length === 1 ? "1 model" : `${models.length} models`} already. You can add another or go on.
              </p>
            )}
            <SetupModelPicker state={state} refresh={refresh} setError={setError} />
            {error && <ErrorCard title="Something didn't work" cause={error} />}
          </section>
        )}

        {step === 2 && (
          <section className="gw-setup-step">
            <h1 tabIndex={-1}>Choose your favourite model</h1>
            <p className="gw-setup-lead">Home offers it first. If you like, it loads every time Gardenwall starts and you go straight to work.</p>
            <FavouriteStep state={state} refresh={refresh} setError={setError} />
            {error && <ErrorCard title="Something didn't work" cause={error} />}
          </section>
        )}

        {step === 3 && (
          <section className="gw-setup-step">
            <h1 tabIndex={-1}>You're set</h1>
            <p className="gw-setup-lead">
              Home shows your model, memory, sessions and anything waiting for you. Start an agent session from there, or open Chat.
            </p>
            <ul className="gw-setup-summary">
              <li>
                {runtimeReady ? <Check aria-hidden="true" /> : <X aria-hidden="true" className="no" />}
                Engine {runtimeReady ? "installed" : "not installed yet (Updates, in the menu)"}
              </li>
              <li>
                {models.length > 0 ? <Check aria-hidden="true" /> : <X aria-hidden="true" className="no" />}
                {models.length > 0 ? `${models.length === 1 ? "1 model" : `${models.length} models`} ready` : "No model yet (Models, in the menu)"}
              </li>
              <li>
                {state.favouriteModel ? <Check aria-hidden="true" /> : <X aria-hidden="true" className="no" />}
                {state.favouriteModel
                  ? `Favourite set${state.loadOnLaunch ? ", loads at start" : ""}`
                  : "No favourite (you can choose one on Home)"}
              </li>
            </ul>
            <p className="muted small">You can run this setup again from Help.</p>
          </section>
        )}
      </main>

      <footer className="gw-setup-foot">
        <Button variant="ghost" onClick={finish}>
          {step === STEPS.length - 1 ? "Close" : "Skip setup"}
        </Button>
        <div className="gw-setup-foot-right">
          {step > 0 && <Button onClick={() => setStep(step - 1)}>Back</Button>}
          {step < STEPS.length - 1 ? (
            <Button
              variant="primary"
              onClick={() => {
                setError(null);
                setStep(step + 1);
              }}
            >
              {step === 0 ? "Get started" : "Next"}
            </Button>
          ) : (
            <Button variant="primary" onClick={finish}>
              Go to Home
            </Button>
          )}
        </div>
      </footer>
    </div>
  );
}

function RuntimeStep({ state, refresh, ready, setError }: { state: AppView; refresh: () => Promise<void>; ready: boolean; setError: (e: string | null) => void }) {
  const [progress, setProgress] = useState<Progress | null>(null);
  const [busy, setBusy] = useState(false);
  const preferred = state.runtimes[0];
  const installed = state.runtimes.filter((r) => r.installed);
  return (
    <div className="gw-setup-runtime">
      <Cpu aria-hidden="true" />
      <div className="gw-setup-runtime-text">
        <b>Engine</b>
        {ready ? (
          <span className="muted">{installed.map((r) => r.id).join(", ")}</span>
        ) : preferred ? (
          <span className="muted">
            {preferred.id} · {preferred.backend} · about {preferred.downloadMb} MB, checked against its pinned hash
          </span>
        ) : (
          <span className="muted">No build is available for this system.</span>
        )}
        {busy && progress && progress.total > 0 && (
          <SegmentedBar
            label="Engine download"
            total={progress.total}
            height={6}
            format={formatBytes}
            segments={[{ label: "Downloaded", value: progress.done, color: "--accent" }]}
          />
        )}
      </div>
      {ready ? (
        <Pill tone="ok">Installed</Pill>
      ) : (
        preferred && (
          <Button
            variant="primary"
            icon={Download}
            disabled={busy}
            onClick={async () => {
              setError(null);
              setBusy(true);
              try {
                await api.installRuntime(preferred.id, setProgress);
                await refresh();
              } catch (e) {
                setError(errorText(e));
              } finally {
                setBusy(false);
                setProgress(null);
              }
            }}
          >
            {busy ? "Installing…" : "Install"}
          </Button>
        )
      )}
    </div>
  );
}

function FavouriteStep({ state, refresh, setError }: { state: AppView; refresh: () => Promise<void>; setError: (e: string | null) => void }) {
  const models = state.models.filter((m) => m.architecture !== "clip");
  if (models.length === 0) {
    return <p className="muted">No model yet. Download one in the previous step, or choose a favourite later on Home.</p>;
  }
  const save = async (sha: string | null, onLaunch: boolean) => {
    setError(null);
    try {
      await api.setFavouriteModel(sha, onLaunch);
      await refresh();
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <>
      <div className="gw-setup-models" role="radiogroup" aria-label="Favourite model">
        {models.map((m) => (
          <label key={m.sha256} className={`gw-setup-model${state.favouriteModel === m.sha256 ? " on" : ""}`}>
            <input type="radio" name="favourite" checked={state.favouriteModel === m.sha256} onChange={() => void save(m.sha256, state.loadOnLaunch)} />
            <span className="gw-setup-model-name">{m.displayName}</span>
            <span className="muted">{formatBytes(m.size)}</span>
          </label>
        ))}
      </div>
      <div className="gw-setup-launch">
        <div>
          <b>Load it when Gardenwall starts</b>
          <span className="muted">Skips Home and opens the Agent with the model ready. Loading takes a moment and uses GPU memory.</span>
        </div>
        <Toggle
          label="Load the favourite model when Gardenwall starts"
          checked={state.loadOnLaunch}
          disabled={!state.favouriteModel}
          onChange={(v) => void save(state.favouriteModel, v)}
        />
      </div>
    </>
  );
}
