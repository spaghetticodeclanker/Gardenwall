// Per-model settings with plain-language explanations
// : every tuning option says what it does, its benefits and its
// drawbacks, and "Suggest for this computer" proposes a configuration with
// the reason for each choice. Nothing changes until the user applies it.
import { ReactNode, useState } from "react";
import { AddVision } from "./AddVision";
import { api, AppView, errorText, KvCache, ModelRecord, Speculation, Suggestion, Thinking } from "./api";

/** A "What is this?" toggle with benefits and drawbacks. */
export function Explain({ what, pros, cons }: { what: ReactNode; pros: string[]; cons: string[] }) {
  return (
    <details className="explain">
      <summary>What is this?</summary>
      <div className="explain-body">
        <p>{what}</p>
        <ProsCons pros={pros} cons={cons} />
      </div>
    </details>
  );
}

/** Benefits and drawbacks side by side (inside an Explain, or a page's More). */
export function ProsCons({ pros, cons }: { pros: string[]; cons: string[] }) {
  return (
    <div className="pros-cons">
      <div>
        <strong>Benefits</strong>
        <ul>
          {pros.map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
      </div>
      <div>
        <strong>Drawbacks</strong>
        <ul>
          {cons.map((c) => (
            <li key={c}>{c}</li>
          ))}
        </ul>
      </div>
    </div>
  );
}

function Setting({ label, hint, control, explain }: {
  label: string;
  hint: ReactNode;
  control: ReactNode;
  explain: ReactNode;
}) {
  return (
    <div className="setting">
      <div className="setting-label">{label}</div>
      <div className="setting-control">
        {control}
        <div className="small muted">{hint}</div>
        {explain}
      </div>
    </div>
  );
}

/** The recommended smallest context: automatic
 * context never goes below it, and a smaller one set by hand gets a warning,
 * but is allowed. Matches `RECOMMENDED_MIN_CONTEXT` in the launcher. */
export const RECOMMENDED_MIN_CONTEXT = 16384;

/** The warning for a hand-set context below the recommended minimum (or the
 * model's trained length, if that is smaller), or null. */
export function lowContextWarning(context: string, trained: number | null): string | null {
  if (!context) return null;
  const min = Math.min(RECOMMENDED_MIN_CONTEXT, trained ?? RECOMMENDED_MIN_CONTEXT);
  if (Number(context) >= min) return null;
  return `Below the recommended ${min.toLocaleString()} tokens. A thinking model can fill a small context with reasoning before it answers, and the agent's instructions alone take about 2,400 tokens.`;
}

const SPECULATION_LABELS: Record<string, string> = {
  auto: "Automatic",
  off: "Off",
  ngram: "Draft-free",
  mtp: "Built-in prediction (MTP)",
};

function speculationLabel(s: Speculation, models: ModelRecord[]): string {
  if (s.kind === "draft") return `Draft model: ${models.find((m) => m.sha256 === s.model)?.displayName ?? "?"}`;
  return SPECULATION_LABELS[s.kind] ?? s.kind;
}

const THINKING_LABELS: Record<Thinking, string> = {
  default: "Model default",
  medium: "Medium",
  low: "Low",
  off: "Off",
};

function cacheLabel(mib: number): string {
  return mib ? `${mib / 1024} GiB on the GPU` : "Off";
}

/** One line summarizing the current settings, for the collapsed panel. */
export function settingsSummary(model: ModelRecord, context: string, kvCache: KvCache, models: ModelRecord[]): string {
  const parts = [
    context ? `${Number(context).toLocaleString()}-token context` : "automatic context",
    kvCache === "q8_0" ? "8-bit cache" : "16-bit cache",
    `faster generation: ${speculationLabel(model.speculation ?? { kind: "auto" }, models).toLowerCase()}`,
  ];
  if ((model.experts ?? 0) > 0) parts.push(`expert cache: ${cacheLabel(model.moeCacheMib ?? 0).toLowerCase()}`);
  if (model.template.reasoningLevels || model.template.thinkingToggle)
    parts.push(`thinking: ${THINKING_LABELS[model.thinking ?? "default"].toLowerCase()}`);
  return parts.join(" · ");
}

export function ModelSettings({ model, state, refresh, setError, context, setContext, kvCache, setKvCache }: {
  model: ModelRecord;
  state: AppView;
  refresh: () => Promise<void>;
  setError: (e: string | null) => void;
  context: string;
  setContext: (v: string) => void;
  kvCache: KvCache;
  setKvCache: (v: KvCache) => void;
}) {
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null);
  const [busy, setBusy] = useState(false);
  const isLoaded = state.loaded?.sha256 === model.sha256;
  const projectors = state.models.filter((m) => m.architecture === "clip");
  // Draft candidates: smaller chat models. The core checks the tokenizer.
  const drafts = state.models.filter(
    (m) => m.architecture !== "clip" && m.sha256 !== model.sha256 && m.size < model.size,
  );
  const speculation = model.speculation ?? { kind: "auto" };
  const speculationValue = speculation.kind === "draft" ? `draft:${speculation.model}` : speculation.kind;
  const moe = (model.experts ?? 0) > 0;
  const mtp = (model.mtpLayers ?? 0) > 0;

  const save = async (action: () => Promise<unknown>) => {
    try {
      await action();
      setError(null);
      await refresh();
    } catch (err) {
      setError(errorText(err));
    }
  };

  const suggest = async () => {
    setBusy(true);
    try {
      setSuggestion(await api.suggestSettings(model.sha256));
      setError(null);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const apply = async () => {
    setBusy(true);
    try {
      const updated = await api.applySuggestedSettings(model.sha256);
      setContext(updated.load.contextSize?.toString() ?? "");
      setKvCache(updated.load.kvCache);
      setSuggestion(null);
      setError(null);
      await refresh();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const noteFor = (setting: string) => suggestion?.notes.find((n) => n.setting === setting)?.text;
  /** The reason without its leading "Value:" (the value is shown in bold). */
  const reasonFor = (setting: string) => {
    const text = noteFor(setting) ?? "";
    const colon = text.indexOf(": ");
    return colon > 0 && colon < 40 ? text.slice(colon + 2) : text;
  };

  return (
    <div className="model-settings">
      {isLoaded && (
        <div className="gw-loaded-note" role="note">
          <span>
            <strong>This model is loaded.</strong> Unload it to change the context size or context cache; other changes
            apply the next time it loads.
          </span>
          <button
            className="small"
            disabled={busy}
            onClick={() =>
              void save(async () => {
                await api.unloadModel();
              })
            }
          >
            Unload to change
          </button>
        </div>
      )}
      <div className="model-settings-head">
        <span className="small muted">{isLoaded ? "Suggestions apply the next time it loads." : "Applied when you load the model."}</span>
        <button className="small" disabled={busy} onClick={() => void suggest()}>
          {busy && !suggestion ? "Analyzing…" : "Suggest for this computer"}
        </button>
      </div>

      {suggestion && (
        <div className="suggestion" role="region" aria-label="Suggested settings">
          <div className="small muted">Based on {suggestion.hardware}</div>
          {noteFor("overall") && <p className={suggestion.fit === "gpu" ? "" : "warn-text"}>{noteFor("overall")}</p>}
          <dl className="suggestion-list">
            <dt>Context</dt>
            <dd>
              <strong>{suggestion.contextSize ? suggestion.contextSize.toLocaleString() : "Automatic"}</strong> — {reasonFor("context")}
            </dd>
            <dt>Context cache</dt>
            <dd>
              <strong>{suggestion.kvCache === "q8_0" ? "8-bit" : "16-bit"}</strong> — {reasonFor("kvCache")}
            </dd>
            <dt>Faster generation</dt>
            <dd>
              <strong>{speculationLabel(suggestion.speculation, state.models)}</strong> — {reasonFor("speculation")}
            </dd>
            {moe && (
              <>
                <dt>Expert cache</dt>
                <dd>
                  <strong>{cacheLabel(suggestion.moeCacheMib)}</strong> — {reasonFor("expertCache")}
                </dd>
              </>
            )}
            {noteFor("thinking") && (
              <>
                <dt>Thinking</dt>
                <dd>
                  <strong>{THINKING_LABELS[suggestion.thinking]}</strong> — {reasonFor("thinking")}
                </dd>
              </>
            )}
          </dl>
          <div className="row">
            <button className="primary small" disabled={busy} onClick={() => void apply()}>
              Apply these settings
            </button>
            <button className="small" onClick={() => setSuggestion(null)}>
              Keep mine
            </button>
          </div>
        </div>
      )}

      <Setting
        label="Context size"
        hint={
          <>
            Tokens the model can keep in mind. Leave empty for automatic (at least{" "}
            {Math.min(RECOMMENDED_MIN_CONTEXT, model.contextLength ?? RECOMMENDED_MIN_CONTEXT).toLocaleString()}).
            Remembered for this model.
            {lowContextWarning(context, model.contextLength) && (
              <span className="warn-text" role="status">
                {" "}
                {lowContextWarning(context, model.contextLength)}
              </span>
            )}
          </>
        }
        control={
          <input
            className="narrow"
            placeholder="Automatic"
            aria-label="Context size in tokens"
            value={context}
            disabled={isLoaded}
            onChange={(e) => setContext(e.target.value.replace(/[^0-9]/g, ""))}
            onBlur={() => {
              const n = context ? Number(context) : null;
              if (n === (model.load.contextSize ?? null)) return;
              void save(() => api.setModelLoad(model.sha256, n, kvCache));
            }}
          />
        }
        explain={
          <Explain
            what="The context is everything the model can see at once: the conversation, its own answers, and any files or documents it has read. A token is roughly three quarters of a word."
            pros={[
              "A larger context lets chats run longer and the agent read bigger files without forgetting earlier parts.",
              "Automatic picks the largest context that fits in GPU memory (up to what the model was trained for), and never less than 16,384 tokens: if memory is short, part of the model runs on the CPU instead.",
            ]}
            cons={[
              "Context uses GPU memory, which grows with its size.",
              "Very long contexts make reading the prompt slower, and some models get less accurate near their limit.",
              "Below about 16,000 tokens, thinking models can run out of room before they answer, and the agent loses longer tasks. You can still set a smaller context if you need to.",
            ]}
          />
        }
      />

      <Setting
        label="Context cache"
        hint="How precisely the context is stored in memory."
        control={
          <select
            value={kvCache}
            aria-label="Context cache precision"
            disabled={isLoaded}
            onChange={(e) => {
              const kv = e.target.value as KvCache;
              setKvCache(kv);
              void save(() => api.setModelLoad(model.sha256, context ? Number(context) : null, kv));
            }}
          >
            <option value="f16">16-bit (full precision)</option>
            <option value="q8_0">8-bit (half the memory)</option>
          </select>
        }
        explain={
          <Explain
            what="While it works, the model keeps a cache (the KV cache) of everything in its context. This setting chooses how many bits each value takes."
            pros={[
              "16-bit stores the context exactly.",
              "8-bit halves the memory per token, so about twice as much context fits, or a bigger model fits at all.",
            ]}
            cons={[
              "16-bit uses twice the memory.",
              "8-bit loses a very small amount of precision; it is rarely noticeable, mostly on exact recall from very long contexts.",
            ]}
          />
        }
      />

      <Setting
        label="Faster generation"
        hint="Speculative decoding: the answer quality stays the same, only the speed changes."
        control={
          <select
            value={speculationValue}
            aria-label="Faster generation"
            onChange={(e) => {
              const v = e.target.value;
              const choice: Speculation = v.startsWith("draft:")
                ? { kind: "draft", model: v.slice(6) }
                : ({ kind: v } as Speculation);
              void save(() => api.setModelSpeculation(model.sha256, choice));
            }}
          >
            <option value="auto">
              {mtp ? "Automatic (MTP when there is room for it)" : "Automatic (off: this model has no MTP layer)"}
            </option>
            <option value="off">Off</option>
            <option value="ngram">Draft-free (reuses text already in the chat)</option>
            {mtp && <option value="mtp">Built-in prediction (MTP, this model has it)</option>}
            {drafts.map((d) => (
              <option key={d.sha256} value={`draft:${d.sha256}`}>
                Draft model: {d.displayName}
              </option>
            ))}
          </select>
        }
        explain={
          <Explain
            what={
              <>
                The model normally writes one token at a time. With speculative decoding, a fast helper guesses the next
                few tokens and the model checks them all in one step, keeping only the ones it would have written
                itself. The answer quality is the same; it just arrives sooner when the guesses are good. The chat and agent
                show how many guesses were kept.
                <br />
                <strong>Automatic</strong> (the default) uses built-in prediction when the model has it and, with it
                loaded, the whole model still fits in GPU memory with at least a 16,384-token context; otherwise it
                stays off. Gardenwall checks at every load and says which way it went.{" "}
                <strong>Draft-free</strong> looks for the next words in text already in the conversation.{" "}
                <strong>Built-in prediction (MTP)</strong> uses an extra layer some models ship for exactly this.{" "}
                <strong>Draft model</strong> uses a small model from the same family.
              </>
            }
            pros={[
              "Built-in prediction made writing about 1.7-2.4x faster on a 27B model in Gardenwall's tests, and agent tasks took about half the time, with the same pass rate.",
              "Draft-free needs no extra memory and helps most when answers repeat earlier text (code edits, quoting documents).",
              "A draft model speeds up general chat on models without built-in prediction.",
            ]}
            cons={[
              "Built-in prediction uses about 1 GiB more GPU memory, reads prompts a few percent slower, and gains less on short or unpredictable answers.",
              "Draft-free gains little in free-form writing and can be slightly slower there.",
              "A draft model uses GPU memory for a second model and must share the main model's tokenizer (Gardenwall checks this).",
            ]}
          />
        }
      />

      {(model.template.reasoningLevels || model.template.thinkingToggle) && (
        <Setting
          label="Thinking"
          hint="How long the model thinks before it answers. Applies to the next message."
          control={
            <select
              value={model.thinking ?? "default"}
              aria-label="Thinking"
              onChange={(e) => void save(() => api.setModelThinking(model.sha256, e.target.value as Thinking))}
            >
              <option value="default">Model default</option>
              {model.template.reasoningLevels && <option value="medium">Medium</option>}
              {model.template.reasoningLevels && <option value="low">Low</option>}
              {model.template.thinkingToggle && <option value="off">Off (answer straight away)</option>}
            </select>
          }
          explain={
            <Explain
              what="Thinking models write out their reasoning (shown under Reasoning) before the answer. This model's template lets you choose how much. The answer comes after the thinking, so less thinking means a faster answer."
              pros={[
                "Medium or Low answers much sooner: in Gardenwall's tests medium used about a third of the tokens for an equally good answer.",
                "Less thinking leaves more of the context window for the conversation.",
                "It also lowers the chance of a heavily compressed model getting stuck repeating itself while thinking.",
              ]}
              cons={[
                "Hard problems (maths, tricky code, careful planning) can benefit from longer thinking.",
                "Off skips reasoning entirely, which can make answers to difficult questions worse.",
              ]}
            />
          }
        />
      )}

      {moe && (
        <Setting
          label="Expert cache"
          hint={`Mixture of Experts: ${model.experts} experts per layer.`}
          control={
            <select
              value={model.moeCacheMib ?? 0}
              aria-label="Expert cache"
              onChange={(e) => void save(() => api.setModelExpertCache(model.sha256, Number(e.target.value)))}
            >
              <option value={0}>Off</option>
              {[1, 2, 4, 8].map((g) => (
                <option key={g} value={g * 1024}>
                  {g} GiB on the GPU
                </option>
              ))}
            </select>
          }
          explain={
            <Explain
              what="A Mixture-of-Experts model contains many small expert networks but uses only a few of them for each token. When the model is larger than GPU memory, Gardenwall keeps the parts used for every token on the GPU and some experts in system RAM, which is slower to reach. The cache keeps the most-used experts on the GPU."
              pros={[
                "Faster generation for models that don't fit in GPU memory, because popular experts are read from fast GPU memory.",
                "Gardenwall leaves the same amount of extra GPU memory free, so the cache can never cause the driver slowdown that happens when GPU memory runs out.",
              ]}
              cons={[
                "The memory set aside can mean a smaller automatic context.",
                "No benefit when the whole model already fits in GPU memory.",
                "The best size depends on the model and your hardware; compare the speed shown after each answer.",
              ]}
            />
          }
        />
      )}

      {model.architecture !== "clip" && (
        <Setting
          label="Image projector"
          hint={
            model.mmproj
              ? "Paired: this model can see images you attach or paste."
              : "Lets the model see screenshots and pictures. Add vision finds the projector made for this model."
          }
          control={
            <>
            {!model.mmproj && <AddVision model={model} offline={state.offline} refresh={refresh} />}
            {projectors.length > 0 && (
            <select
              value={model.mmproj ?? ""}
              aria-label="Image projector"
              onChange={(e) => void save(() => api.setModelProjector(model.sha256, e.target.value || null))}
            >
              <option value="">None (text only)</option>
              {projectors.map((p) => (
                <option key={p.sha256} value={p.sha256}>
                  {p.displayName}
                </option>
              ))}
            </select>
            )}
            </>
          }
          explain={
            <Explain
              what="Vision models read images through a separate file called an image projector (mmproj), published next to the model. Images are checked by type and decoded only inside the sandbox."
              pros={["Attach pictures to chat messages and ask about them."]}
              cons={[
                "The projector uses extra GPU memory.",
                "It only works with the model it was made for; a mismatched projector fails to load.",
              ]}
            />
          }
        />
      )}
    </div>
  );
}
