// The model picker: "Which model should I use?" (ROADMAP Planned features §6).
//
// Everything shown is built by the core: fit labels, trade-off cards, the
// starter list, "Help me choose", the ranking's warnings and the research
// question. Nothing here downloads by itself: every download is a button
// the user presses, and goes through the verified download.
import { type ReactNode, useEffect, useState } from "react";
import { Answers, api, AppView, errorText, Fit, ModelRecord, ResearchPrompt, Suggestion, TradeOff, WebMode } from "./api";
import type { Opening } from "./AgentView";
import { Term } from "./modelGuide";
import { TemplateAudit } from "./TemplateAudit";
import { MODE_DETAIL, MODE_LABEL } from "./WebResearch";

export function fitPill(fit: Fit, tight: boolean): string {
  if (fit === "gpu") return tight ? "pill warn" : "pill ok";
  if (fit === "unknown") return "pill neutral";
  return "pill danger";
}

/** Where a file runs, how fast, how much room is left, and the sizes next to it. */
export function TradeOffCard({ card }: { card: TradeOff }) {
  const rows: [ReactNode, string | null | undefined][] = [
    ["Runs", card.runs],
    ["Speed", card.speed],
    [<Term id="context">Room for the conversation</Term>, card.room],
    ["Larger file", card.larger],
    ["Smaller file", card.smaller],
  ];
  return (
    <dl className="tradeoff">
      {rows
        .filter(([, value]) => value)
        .map(([label, value], i) => (
          <div className="tradeoff-row" key={i}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        ))}
    </dl>
  );
}


/** After a verified download: the template check, the suggested settings, and Load. */
export function AfterDownload({ model, state, refresh, setError }: { model: ModelRecord; state: AppView; refresh: () => Promise<void>; setError: (e: string | null) => void }) {
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    api.suggestSettings(model.sha256).then(setSuggestion, () => setSuggestion(null));
  }, [model.sha256]);
  const cleared = !model.template.requiresAcknowledgement || model.template.acknowledged;
  if (state.loaded?.sha256 === model.sha256) {
    return <div className="small ok-text">Loaded. Open Chat or Agent to use it.</div>;
  }
  return (
    <div className="after-download">
      {!cleared && (
        <p className="small">
          Downloaded and verified. Before the first load, read the model's chat-template check below and acknowledge
          it.
        </p>
      )}
      <TemplateAudit model={model} onChange={() => void refresh()} />
      {suggestion && (
        <div className="small">
          <div className="muted">Suggested settings for {suggestion.hardware}:</div>
          <ul>
            {suggestion.notes
              .filter((n) => ["overall", "kvCache", "speculation"].includes(n.setting))
              .map((n) => (
                <li key={n.setting}>{n.text}</li>
              ))}
          </ul>
        </div>
      )}
      <button
        className="primary"
        disabled={!cleared || loading}
        title={cleared ? "" : "Acknowledge the chat-template check first"}
        onClick={async () => {
          setLoading(true);
          setError(null);
          try {
            const rec = await api.applySuggestedSettings(model.sha256);
            await api.loadModel(rec.sha256, { contextSize: rec.load.contextSize, kvCache: rec.load.kvCache });
            await refresh();
          } catch (e) {
            setError(errorText(e));
          } finally {
            setLoading(false);
          }
        }}
      >
        {loading ? "Loading…" : "Load with these settings"}
      </button>
    </div>
  );
}

/** "Research current models": the trusted dialog with the prepared question. */
export function ResearchModels({ answers, go, onStart }: {
  answers: Answers | null;
  go: (page: "settings" | "models") => void;
  onStart: (opening: Opening) => void;
}) {
  const [prompt, setPrompt] = useState<ResearchPrompt | null>(null);
  const [text, setText] = useState("");
  const [hardware, setHardware] = useState(true);
  const [mode, setMode] = useState<WebMode>("ask");
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const open = async () => {
    setError(null);
    try {
      const p = await api.modelResearchPrompt(answers);
      setPrompt(p);
      setText(p.withHardware);
      setHardware(true);
      const w = await api.webStatus().catch(() => null);
      setMode(w?.settings.defaultMode ?? "ask");
    } catch (e) {
      setError(errorText(e));
    }
  };

  const toggleHardware = (on: boolean) => {
    if (!prompt) return;
    setHardware(on);
    // Untouched text swaps whole; edited text gains or loses just the line.
    if (text === prompt.withHardware || text === prompt.withoutHardware) {
      setText(on ? prompt.withHardware : prompt.withoutHardware);
    } else if (on && !text.includes(prompt.hardwareSentence)) {
      setText(`${prompt.hardwareSentence}\n${text}`);
    } else if (!on) {
      setText(text.split(`${prompt.hardwareSentence}\n`).join("").split(prompt.hardwareSentence).join(""));
    }
  };

  return (
    <div className="research-models" id="research-models">
      <span className="small muted">Want something newer? </span>
      <button
        type="button"
        className="link small"
        title="The loaded model searches the web and reads model pages, then cites what it read"
        onClick={() => void open()}
      >
        Research current models
      </button>
      {error && <div className="banner error">{error}</div>}
      {prompt && prompt.missing.length > 0 && (
        <div className="banner warn">
          {prompt.missing.map((m) => (
            <div key={m.what}>
              {m.text}{" "}
              <button className="link" onClick={() => go(m.what === "model" || m.what === "tools" ? "models" : "settings")}>
                {m.what === "model" || m.what === "tools" ? "Open Models" : "Open Settings"}
              </button>
            </div>
          ))}
          <button className="link small" onClick={() => setPrompt(null)}>
            Close
          </button>
        </div>
      )}
      {prompt && prompt.missing.length === 0 && (
        <div className="approval-backdrop" role="presentation">
          <div className="handoff mode-choice" role="dialog" aria-modal="true" aria-labelledby="research-models-title">
            <h2 id="research-models-title">Research current models?</h2>
            <p className="small muted">
              A research session opens with this question in its message box, not sent. Read it, change it if you like,
              and send it yourself. The session has no files and can't run commands.
            </p>
            <textarea className="research-question" rows={10} value={text} onChange={(e) => setText(e.target.value)} aria-label="Research question" />
            <label className="small">
              <input type="checkbox" checked={hardware} onChange={(e) => toggleHardware(e.target.checked)} /> Include my hardware
            </label>
            <div className="small muted">
              {hardware ? (
                <>
                  These words are added: <span className="mono">{prompt.hardwareSentence}</span> The model writes search
                  queries from the question, so they can reach the search provider. Nothing else about this computer is
                  added: no user name, paths, serial numbers or installed software.
                </>
              ) : (
                "Nothing about this computer is added. Answers will be less specific."
              )}
            </div>
            {(["ask", "research"] as WebMode[]).map((m) => (
              <label key={m} className={`mode-option ${mode === m ? "chosen" : ""}`}>
                <input type="radio" name="model-research-mode" checked={mode === m} onChange={() => setMode(m)} />
                <span>
                  <strong>{MODE_LABEL[m]}</strong>
                  <span className="small muted block">{MODE_DETAIL[m]}</span>
                </span>
              </label>
            ))}
            <p className="small muted">
              The answer is the model's account of pages it read: check its sources. Under it, "Repositories opened" lists
              the hub repositories the session really fetched; nothing there downloads by itself.
            </p>
            <div className="row">
              <button
                className="primary"
                disabled={starting || !text.trim()}
                onClick={async () => {
                  setStarting(true);
                  setError(null);
                  try {
                    const view = await api.agentStartResearch(mode);
                    setPrompt(null);
                    onStart({ view, entries: [], input: text });
                  } catch (e) {
                    setError(errorText(e));
                  } finally {
                    setStarting(false);
                  }
                }}
              >
                {starting ? "Starting…" : "Start"}
              </button>
              <button onClick={() => setPrompt(null)}>Cancel</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** A research session's hub repositories, from its visited list only. */
export function OpenedRepositories({ sid, onLook, onClose }: { sid: string; onLook: (repo: string) => void; onClose: () => void }) {
  const [repos, setRepos] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.researchRepositories(sid).then(setRepos, (e) => setError(errorText(e)));
  }, [sid]);
  return (
    <div className="approval-backdrop" role="presentation" onClick={onClose}>
      <div className="handoff" role="dialog" aria-modal="true" aria-labelledby="opened-repos-title" onClick={(e) => e.stopPropagation()}>
        <h2 id="opened-repos-title">Repositories opened in this session</h2>
        <p className="small muted">
          Model repositories on the hub whose pages this session really fetched, from Gardenwall's visited list, not from
          the answer's text. "Look at files" shows their files with fit labels and warnings; nothing downloads by itself.
        </p>
        {error && <div className="banner error">{error}</div>}
        {repos && repos.length === 0 && <p>No hub repositories were opened yet.</p>}
        <ul className="list">
          {repos?.map((r) => (
            <li key={r}>
              <span className="mono">{r}</span>{" "}
              <button className="link" onClick={() => onLook(r)}>
                Look at files
              </button>
            </li>
          ))}
        </ul>
        <div className="row">
          <button onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}
