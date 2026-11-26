import { useEffect, useState } from "react";
import { api, AppView, AutoMtp, errorText, formatBytes, KvCache, ModelRecord, placementText, Progress } from "./api";
import type { Opening } from "./AgentView";
import { ModelSettings, settingsSummary } from "./ModelSettings";
import { ProgressBar } from "./ProgressBar";
import { TemplateAudit } from "./TemplateAudit";
import { Button, DataTable, EmptyState, Inspector, Pill } from "./ui";
import { FindModel } from "./FindModel";
import "./models.css";

function sourceText(m: ModelRecord): string {
  if (m.source.kind === "hub") {
    return `Hugging Face · ${m.source.repo} @ ${m.source.commit.slice(0, 10)} · hash verified`;
  }
  return `Imported file · ${m.source.hash_verified ? "hash verified" : "hash not verified"}`;
}

/** Everything about one library model (the Models page's details panel):
 * facts, Load with these settings, load settings and the template audit. */
function ModelDetails({ model, state, refresh, setError, tab }: {
  model: ModelRecord;
  state: AppView;
  refresh: () => Promise<void>;
  setError: (e: string | null) => void;
  tab: "details" | "settings" | "template";
}) {
  const [context, setContext] = useState(model.load.contextSize?.toString() ?? "");
  const [kvCache, setKvCache] = useState<KvCache>(model.load.kvCache);
  const [loading, setLoading] = useState(false);
  const isLoaded = state.loaded?.sha256 === model.sha256;
  const cleared = !model.template.requiresAcknowledgement || model.template.acknowledged;
  const isProjector = model.architecture === "clip";

  if (tab === "settings") {
    if (isProjector) return <p className="muted small">Image projectors have no load settings; they load with the vision model they're paired with.</p>;
    return (
      <div className="gw-inspector-list">
        <p className="small muted">{settingsSummary(model, context, kvCache, state.models)}</p>
        <ModelSettings
          model={model}
          state={state}
          refresh={refresh}
          setError={setError}
          context={context}
          setContext={setContext}
          kvCache={kvCache}
          setKvCache={setKvCache}
        />
      </div>
    );
  }
  if (tab === "template") return <TemplateAudit model={model} onChange={() => void refresh()} />;

  return (
    <div className="gw-inspector-list">
      <div className="gw-model-detail-name">{model.displayName}</div>
      <dl className="gw-facts">
        <dt>Architecture</dt>
        <dd>{model.architecture}</dd>
        <dt>Size</dt>
        <dd>
          {formatBytes(model.size)}
          {(model.parts?.length ?? 0) > 1 && ` in ${model.parts!.length} parts`}
        </dd>
        <dt>Trained context</dt>
        <dd>{model.contextLength?.toLocaleString() ?? "unknown"} tokens</dd>
        <dt>Tensors</dt>
        <dd>{model.tensorCount.toLocaleString()}</dd>
        <dt>Source</dt>
        <dd>{sourceText(model)}</dd>
        <dt>SHA-256</dt>
        <dd className="mono">{model.sha256}</dd>
      </dl>
      {isProjector && <p className="small">Image projector (mmproj): pair it with a vision model to let it see images.</p>}
      {isLoaded && state.loaded?.placement && (
        <p className="small warn-text">
          Running partly from system RAM: {placementText(state.loaded.placement)}. Generation is slower than with the whole
          model on the GPU{(model.experts ?? 0) > 0 && !model.moeCacheMib ? "; an expert cache may help." : "."}
        </p>
      )}
      {isLoaded && state.loaded?.autoMtp && <p className="small muted">{autoMtpText(state.loaded.autoMtp)}</p>}
      {!cleared && <p className="small warn-text">Review the chat-template audit (Template tab) before loading this model.</p>}
      <div className="gw-model-detail-actions">
        {!isProjector &&
          (isLoaded ? (
            <span className="gw-pill gw-pill-ok">Loaded</span>
          ) : (
            <button
              className="primary"
              disabled={!cleared || loading}
              title={cleared ? `Load with: ${settingsSummary(model, context, kvCache, state.models)}` : "Review the chat-template audit first"}
              onClick={async () => {
                setLoading(true);
                setError(null);
                try {
                  await api.loadModel(model.sha256, { contextSize: context ? Number(context) : null, kvCache });
                  await refresh();
                } catch (e) {
                  setError(errorText(e));
                } finally {
                  setLoading(false);
                }
              }}
            >
              {loading ? "Loading…" : "Load"}
            </button>
          ))}
        <button
          className="danger"
          disabled={isLoaded || loading}
          onClick={async () => {
            if (!window.confirm(`Delete ${model.displayName}? The file will be removed from Gardenwall.`)) return;
            try {
              await api.deleteModel(model.sha256);
              await refresh();
            } catch (e) {
              setError(errorText(e));
            }
          }}
        >
          Delete
        </button>
      </div>
    </div>
  );
}

/** Size, bits, fit and quality for each file of one repository, with the best fit marked. */
export function ModelsView({ state, refresh, go, onResearch, openRepo, onRepoTaken }: {
  state: AppView;
  refresh: () => Promise<void>;
  go: (page: "settings" | "models") => void;
  /** A research session was started: show it on the Agent page. */
  onResearch: (opening: Opening) => void;
  /** A repository to open in the hub browser (from a research session). */
  openRepo: string | null;
  onRepoTaken: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [expected, setExpected] = useState("");
  const [importing, setImporting] = useState<Progress | null | "picking">(null);
  const [look, setLook] = useState<string | null>(null);
  const toOpen = openRepo ?? look;
  /** Library, Find a model, or Import (a fresh install starts on Find). */
  const [tab, setTab] = useState<"library" | "find" | "import">(state.models.length > 0 ? "library" : "find");
  useEffect(() => {
    if (toOpen) setTab("find");
  }, [toOpen]);
  const [selected, setSelected] = useState<string | null>(null);
  const [detailTab, setDetailTab] = useState<"details" | "settings" | "template">("details");
  const chosen = state.models.find((m) => m.sha256 === selected) ?? null;
  const [loadingSha, setLoadingSha] = useState<string | null>(null);

  const quickLoad = async (m: ModelRecord) => {
    setLoadingSha(m.sha256);
    setError(null);
    try {
      await api.loadModel(m.sha256, m.load);
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoadingSha(null);
    }
  };

  const library = (
    <DataTable<ModelRecord>
      rows={state.models}
      rowKey={(m) => m.sha256}
      selected={selected}
      onSelect={(m) => setSelected(m.sha256)}
      empty={
        <EmptyState
          title="No models yet"
          body="Find one that fits this computer, or import a GGUF file you already have."
          action={
            <Button variant="primary" onClick={() => setTab("find")}>
              Find a model
            </Button>
          }
        />
      }
      columns={[
        {
          key: "name",
          header: "Model",
          minWidth: "240px",
          render: (m) => (
            <div>
              <div className="gw-model-row-name">{m.displayName}</div>
              <div className="muted small">
                {m.architecture}
                {m.source.kind === "hub" ? ` · ${m.source.repo}` : " · imported"}
              </div>
            </div>
          ),
        },
        { key: "size", header: "Size", width: "100px", optional: true, align: "right", render: (m) => formatBytes(m.size) },
        {
          key: "context",
          header: "Trained context",
          width: "130px",
          optional: true,
          align: "right",
          render: (m) => (m.contextLength ? m.contextLength.toLocaleString() : "unknown"),
        },
        {
          key: "status",
          header: "Status",
          width: "150px",
          render: (m) =>
            state.loaded?.sha256 === m.sha256 ? (
              <Pill tone="ok">Loaded</Pill>
            ) : m.architecture === "clip" ? (
              <Pill>Image projector</Pill>
            ) : m.template.requiresAcknowledgement && !m.template.acknowledged ? (
              <Pill tone="warn">Review template</Pill>
            ) : state.favouriteModel === m.sha256 ? (
              <Pill tone="accent">Favourite</Pill>
            ) : null,
        },
        {
          key: "actions",
          header: <span className="gw-sr">Actions</span>,
          width: "110px",
          align: "right",
          render: (m) => {
            if (m.architecture === "clip" || state.loaded?.sha256 === m.sha256) return null;
            const cleared = !m.template.requiresAcknowledgement || m.template.acknowledged;
            return (
              <span onClick={(e) => e.stopPropagation()}>
                <Button
                  size="sm"
                  disabled={!cleared || loadingSha !== null}
                  title={cleared ? `Load with: ${settingsSummary(m, m.load.contextSize?.toString() ?? "", m.load.kvCache, state.models)}` : "Review the chat-template audit first"}
                  onClick={() => (cleared ? void quickLoad(m) : (setSelected(m.sha256), setDetailTab("template")))}
                >
                  {loadingSha === m.sha256 ? "Loading…" : "Load"}
                </Button>
              </span>
            );
          },
        },
      ]}
    />
  );

  const pageTabs = (
    <>
      {error && <div className="banner error">{error}</div>}
      <div className="gw-page-tabs" role="tablist" aria-label="Models">
        {(
          [
            ["library", `Library${state.models.length ? ` (${state.models.length})` : ""}`],
            ["find", "Find a model"],
            ["import", "Import a file"],
          ] as const
        ).map(([id, label]) => (
          <button key={id} type="button" role="tab" aria-selected={tab === id} className={`gw-page-tab${tab === id ? " active" : ""}`} onClick={() => setTab(id)}>
            {label}
          </button>
        ))}
      </div>
    </>
  );

  if (tab === "find") {
    return (
      <div className="gw-models">
        <FindModel
          state={state}
          refresh={refresh}
          setError={setError}
          go={go}
          onResearch={onResearch}
          openRepoName={toOpen}
          onRepoTaken={() => {
            setLook(null);
            onRepoTaken();
          }}
          header={pageTabs}
        />
      </div>
    );
  }

  return (
    <div className="gw-models">
      <div className="page gw-models-main">
        {pageTabs}
        {tab === "library" && library}

        {tab === "import" && (
          <section>
            <h2>Import a local file</h2>
            <p className="small muted">
              The file is copied into Gardenwall and validated; your original is not modified. Paste the SHA-256 from the
              model's download page to verify the file has not been altered.
            </p>
            <p className="small muted">
              For a model in several parts (<span className="mono">name-00001-of-00003.gguf</span> …), choose any part:
              Gardenwall imports every part from the same folder and checks that they belong together. Leave the SHA-256
              empty for split models.
            </p>
            <div className="row">
              <input className="mono" placeholder="Expected SHA-256 (optional)" value={expected} onChange={(e) => setExpected(e.target.value.trim())} />
              <button
                disabled={importing !== null}
                onClick={async () => {
                  setError(null);
                  setImporting("picking");
                  try {
                    await api.importModel(expected || null, (p) => setImporting(p));
                    setExpected("");
                    await refresh();
                    setTab("library");
                  } catch (e) {
                    setError(errorText(e));
                  } finally {
                    setImporting(null);
                  }
                }}
              >
                Choose file…
              </button>
            </div>
            {importing && importing !== "picking" && <ProgressBar p={importing} />}
          </section>
        )}
      </div>
      {tab === "library" && chosen && (
        <Inspector
          title="Model details"
          active={detailTab}
          onActive={(id) => setDetailTab(id as "details" | "settings" | "template")}
          onClose={() => setSelected(null)}
          tabs={[
            { id: "details", label: "Details", content: <ModelDetails key={chosen.sha256} model={chosen} state={state} refresh={refresh} setError={setError} tab="details" /> },
            { id: "settings", label: "Settings", content: <ModelDetails key={chosen.sha256} model={chosen} state={state} refresh={refresh} setError={setError} tab="settings" /> },
            {
              id: "template",
              label: "Template",
              badge: chosen.template.requiresAcknowledgement && !chosen.template.acknowledged ? "!" : undefined,
              content: <ModelDetails key={chosen.sha256} model={chosen} state={state} refresh={refresh} setError={setError} tab="template" />,
            },
          ]}
        />
      )}
    </div>
  );
}

/** Which way automatic faster generation went on this load, and why. */
function autoMtpText(outcome: AutoMtp): string {
  switch (outcome) {
    case "on":
      return "Faster generation (automatic): built-in prediction (MTP) is on. The whole model fits in GPU memory with it, with at least a 16,384-token context.";
    case "noRoomEstimated":
      return "Faster generation (automatic): built-in prediction (MTP) is off. There isn't enough GPU memory for it next to the model and a 16,384-token context.";
    case "noRoomMeasured":
      return "Faster generation (automatic): built-in prediction (MTP) is off. With it, part of the model would have run from system RAM or the context would have been under 16,384 tokens, so the model was loaded without it. Closing other GPU-heavy apps may make room next time.";
  }
}
