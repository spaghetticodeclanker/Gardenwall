import { useState } from "react";
import type { AppView } from "./api";

const KEY = "gw.started";
/** The "research current models" line, shown once the first steps are done. */
const RESEARCH_KEY = "gw.started.research";

function dismissed(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function dismiss(key: string) {
  try {
    localStorage.setItem(key, "1");
  } catch {
    // Not kept; it is shown again next time.
  }
}

/**
 * The first steps, shown until they are done or dismissed: install a
 * runtime, get a model (Models opens with one suggested for this computer),
 * load it. Then one line offers to research better-suited models. Built
 * only from the app's own state.
 */
export function GettingStarted({ state, go }: { state: AppView; go: (page: "runtime" | "models" | "chat" | "agent") => void }) {
  const [hidden, setHidden] = useState(() => dismissed(KEY));
  const [researchHidden, setResearchHidden] = useState(() => dismissed(RESEARCH_KEY));
  const [expanded, setExpanded] = useState(false);
  const runtime = state.runtimes.some((r) => r.installed);
  const model = state.models.length > 0;
  const loaded = state.loaded !== null;
  if (runtime && model && loaded) {
    if (researchHidden || hidden || state.offline) return null;
    return (
      <div className="banner getting-started row">
        <span>Want something better suited? Ask it to research current models for this computer.</span>
        <button
          className="link small"
          onClick={() => {
            go("models");
            window.setTimeout(() => document.getElementById("research-models")?.scrollIntoView({ behavior: "smooth", block: "center" }), 50);
          }}
        >
          Research models
        </button>
        <button
          className="link small"
          onClick={() => {
            dismiss(RESEARCH_KEY);
            setResearchHidden(true);
          }}
        >
          Hide
        </button>
      </div>
    );
  }
  if (hidden) return null;
  const steps: { done: boolean; text: string; action: string; page: "runtime" | "models" | "chat" }[] = [
    { done: runtime, text: "Install a runtime (the program that runs models, verified before every start).", action: "Open Updates", page: "runtime" },
    { done: model, text: "Get a model: Gardenwall suggests one that suits this computer, or import a GGUF file you already have.", action: "See the suggestion", page: "models" },
    { done: loaded, text: "Load the model. It runs sandboxed, with no network access.", action: "Open Models", page: "models" },
  ];
  const next = steps.findIndex((s) => !s.done);
  const short = ["Install a runtime", "Get a model", "Load the model"];
  const step = steps[next];
  if (!expanded && step) {
    // One line: the next step and its button (the full list on request).
    return (
      <div className="banner getting-started compact" role="status">
        <strong>Getting started</strong>
        <span className="step">
          Step {next + 1} of {steps.length}: {short[next]}{" "}
          <button className="link small" onClick={() => go(step.page)}>
            {step.action}
          </button>
        </span>
        <button className="link small" onClick={() => setExpanded(true)}>
          Show all steps
        </button>
        <button
          className="link small"
          onClick={() => {
            dismiss(KEY);
            setHidden(true);
          }}
        >
          Hide
        </button>
      </div>
    );
  }
  return (
    <div className="banner getting-started">
      <div className="row">
        <strong>Getting started</strong>
        <button
          className="link small"
          onClick={() => {
            dismiss(KEY);
            setHidden(true);
          }}
        >
          Hide
        </button>
      </div>
      <ol>
        {steps.map((s, i) => (
          <li key={s.text} className={s.done ? "muted" : undefined}>
            {s.done ? "✓ " : ""}
            {s.text}{" "}
            {i === next && (
              <button className="link small" onClick={() => go(s.page)}>
                {s.action}
              </button>
            )}
          </li>
        ))}
      </ol>
      <div className="small muted">
        Then chat, or open Agent and choose a project folder: the agent works only inside the folders you give it.
      </div>
    </div>
  );
}
