// Model and thinking switches next to the message box
// . The same settings stay on the Models page; these are shortcuts.
import { useState } from "react";
import { api, AppView, errorText, Thinking } from "./api";

const THINKING: { value: Thinking; label: string; needs: "levels" | "toggle" | null }[] = [
  { value: "default", label: "Model default", needs: null },
  { value: "medium", label: "Medium", needs: "levels" },
  { value: "low", label: "Low", needs: "levels" },
  { value: "off", label: "Off", needs: "toggle" },
];

export function QuickSwitch({ state, refresh, busy, onError }: {
  state: AppView;
  refresh: () => Promise<void>;
  /** A reply is being written: switching waits. */
  busy: boolean;
  onError: (message: string | null) => void;
}) {
  const [loading, setLoading] = useState<string | null>(null);
  const models = state.models.filter((m) => m.architecture !== "clip");
  const loadedSha = state.loaded?.sha256 ?? "";
  const current = models.find((m) => m.sha256 === loadedSha) ?? null;
  const levels = current?.template.reasoningLevels ?? false;
  const toggle = current?.template.thinkingToggle ?? false;

  const switchTo = async (sha: string) => {
    const target = models.find((m) => m.sha256 === sha);
    if (!target) return;
    if (target.template.requiresAcknowledgement && !target.template.acknowledged) {
      onError(`Review ${target.displayName}'s chat-template audit on the Models page before loading it.`);
      return;
    }
    if (
      current &&
      !window.confirm(
        `Switch to ${target.displayName}? ${current.displayName} is unloaded first. Loading a large model can take a minute; this chat continues with the new model.`,
      )
    )
      return;
    setLoading(target.displayName);
    onError(null);
    try {
      await api.loadModel(target.sha256, target.load);
    } catch (e) {
      onError(errorText(e));
    } finally {
      setLoading(null);
      await refresh();
    }
  };

  if (models.length === 0) return null;
  return (
    <div className="quick-switch">
      <label className="small">
        <span className="muted">Model</span>
        <select
          value={loadedSha}
          disabled={busy || loading !== null}
          title="Load another model from your library, with the settings saved for it on the Models page"
          aria-label="Model"
          onChange={(e) => void switchTo(e.target.value)}
        >
          {!current && <option value="">{loading ? `Loading ${loading}…` : "No model loaded"}</option>}
          {models.map((m) => (
            <option key={m.sha256} value={m.sha256}>
              {loading && m.sha256 !== loadedSha && loading === m.displayName ? `Loading ${m.displayName}…` : m.displayName}
            </option>
          ))}
        </select>
      </label>
      {current && (levels || toggle) && (
        <label className="small">
          <span className="muted">Thinking</span>
          <select
            value={current.thinking ?? "default"}
            disabled={loading !== null}
            title="How long the model thinks before answering; applies to your next message (also in the model's Settings)"
            aria-label="Thinking"
            onChange={async (e) => {
              try {
                await api.setModelThinking(current.sha256, e.target.value as Thinking);
                onError(null);
                await refresh();
              } catch (err) {
                onError(errorText(err));
              }
            }}
          >
            {THINKING.filter((t) => t.needs === null || (t.needs === "levels" ? levels : toggle)).map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </select>
        </label>
      )}
      {loading && <span className="small muted working">Loading {loading}…</span>}
    </div>
  );
}
