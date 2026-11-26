// Models → Find a model (redesign; plan in
// docs/internal/design/FIND-A-MODEL-REDESIGN.md): three tabs (For you, Browse,
// Search), one side panel for a repository, a starter model's details and
// the steps after a download, and a tray of downloads in progress that
// survives switching tabs. Everything shown is built by the core as before;
// nothing downloads by itself, and every download is pinned and verified.
import { ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, BadgeCheck, ChevronRight, Download as DownloadIcon, Search } from "lucide-react";
import {
  api,
  errorText,
  formatBytes,
  type Answers,
  type AppView,
  type Chosen,
  type HubListing,
  type ModelRecord,
  type Priority,
  type Purpose,
  type RankingView,
  type RepoFile,
  type RepoView,
  type StarterEntry,
  type StarterView,
} from "./api";
import type { Opening } from "./AgentView";
import { AfterDownload, ResearchModels, TradeOffCard, fitPill } from "./ModelPicker";
import { Term } from "./modelGuide";
import { useDownloads, useStoreDownload } from "./downloads";
import { DownloadControls } from "./useDownload";
import { Button, Inspector, Pill } from "./ui";

type Tab = "forYou" | "browse" | "search";
const TAB_KEY = "gw.find.tab";

/** What the side panel shows. */
type Panel =
  | { kind: "repo"; repo: RepoView }
  | { kind: "starter"; entry: StarterEntry }
  | { kind: "model"; sha: string }
  | null;

function savedTab(): Tab {
  try {
    const t = localStorage.getItem(TAB_KEY);
    if (t === "forYou" || t === "browse" || t === "search") return t;
  } catch {
    /* first visit: For you */
  }
  return "forYou";
}

const PURPOSES: { id: Purpose; label: string }[] = [
  { id: "chat", label: "Chat and writing" },
  { id: "coding", label: "Coding" },
  { id: "images", label: "Images" },
];
const PRIORITIES: { id: Priority; label: string }[] = [
  { id: "quality", label: "Best answers" },
  { id: "balanced", label: "Balanced" },
  { id: "speed", label: "Fastest" },
];

const KNOWN_PUBLISHER = "An original model maker or a long-standing quantizer. It says who published this, not that it is safe.";

/** The whole Find a model view: its main column and (when open) its side panel. */
export function FindModel({ state, refresh, setError, go, onResearch, openRepoName, onRepoTaken, header }: {
  state: AppView;
  refresh: () => Promise<void>;
  setError: (e: string | null) => void;
  go: (page: "settings" | "models") => void;
  onResearch: (o: Opening) => void;
  /** A repository to open (from a research session). */
  openRepoName: string | null;
  onRepoTaken: () => void;
  /** The Models page's own tabs and banners, shown above. */
  header: ReactNode;
}) {
  const [tab, setTabState] = useState<Tab>(savedTab);
  const setTab = (t: Tab) => {
    setTabState(t);
    try {
      localStorage.setItem(TAB_KEY, t);
    } catch {
      /* not remembered */
    }
  };
  const [planGib, setPlanGib] = useState<number | null>(null);
  const [starter, setStarter] = useState<StarterView | null>(null);
  const [panel, setPanel] = useState<Panel>(null);
  const [answers, setAnswers] = useState<Answers | null>(null);
  const store = useDownloads();
  const modelCount = state.models.length;

  useEffect(() => {
    api.starterModels(planGib).then(setStarter, (e) => setError(errorText(e)));
  }, [planGib, modelCount, setError]);

  const openRepo = useCallback(
    async (name: string) => {
      setError(null);
      try {
        setPanel({ kind: "repo", repo: await api.listRepoFiles(name, planGib) });
      } catch (e) {
        setError(errorText(e));
      }
    },
    [planGib, setError],
  );
  useEffect(() => {
    if (!openRepoName || state.offline) return;
    onRepoTaken();
    void openRepo(openRepoName);
    // Opens once per request.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openRepoName]);

  /** Start a download; when it is verified the panel opens on the model's next steps. */
  const download = (key: string, label: string, run: StoreJob["run"]) => {
    setError(null);
    store.start({
      key,
      label,
      run,
      onDone: async (model) => {
        await refresh();
        setPanel({ kind: "model", sha: model.sha256 });
      },
      onError: setError,
    });
  };
  const ctx: Ctx = { state, refresh, setError, planGib, openRepo, setPanel, download };

  const panelNode = panel && (
    <Inspector
      title={panel.kind === "repo" ? panel.repo.repo : panel.kind === "starter" ? panel.entry.name : "Downloaded model"}
      active="details"
      onActive={() => {}}
      onClose={() => setPanel(null)}
      tabs={[
        {
          id: "details",
          label: panel.kind === "repo" ? "Files" : panel.kind === "starter" ? "Details" : "Next steps",
          content:
            panel.kind === "repo" ? (
              <RepoPanel repo={panel.repo} ctx={ctx} />
            ) : panel.kind === "starter" ? (
              <StarterPanel entry={panel.entry} ctx={ctx} reviewed={starter?.reviewed ?? ""} />
            ) : (
              <ModelPanel sha={panel.sha} ctx={ctx} />
            ),
        },
      ]}
    />
  );

  return (
    <>
      <div className="page gw-models-main gw-find">
        {header}
        <div className="gw-find-head">
          <HardwareLine hardware={starter?.hardware ?? null} planGib={planGib} onPlan={setPlanGib} />
          <div className="gw-find-tabs" role="tablist" aria-label="Find a model">
            {(
              [
                ["forYou", "For you"],
                ["browse", "Browse"],
                ["search", "Search"],
              ] as const
            ).map(([id, label]) => (
              <button key={id} type="button" role="tab" aria-selected={tab === id} className={`gw-find-tab${tab === id ? " active" : ""}`} onClick={() => setTab(id)}>
                {label}
              </button>
            ))}
          </div>
          <DownloadsTray />
        </div>
        {tab === "forYou" && (
          <ForYou ctx={ctx} view={starter} answers={answers} onAnswers={setAnswers} footer={<ResearchModels answers={answers} go={go} onStart={onResearch} />} />
        )}
        {tab === "browse" && <Browse ctx={ctx} />}
        {tab === "search" && <SearchTab ctx={ctx} />}
      </div>
      {panelNode}
    </>
  );
}

type StoreJob = Parameters<ReturnType<typeof useDownloads>["start"]>[0];

interface Ctx {
  state: AppView;
  refresh: () => Promise<void>;
  setError: (e: string | null) => void;
  planGib: number | null;
  openRepo: (name: string) => Promise<void>;
  setPanel: (p: Panel) => void;
  download: (key: string, label: string, run: StoreJob["run"]) => void;
}

/** "For NVIDIA … (32 GB) · Change": plan for a different amount of GPU memory (labels only). */
function HardwareLine({ hardware, planGib, onPlan }: { hardware: string | null; planGib: number | null; onPlan: (v: number | null) => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(planGib?.toString() ?? "");
  return (
    <div className="gw-find-hardware">
      <span className="muted">{planGib !== null ? `Planning for ${planGib} GB of GPU memory` : hardware ? `For ${hardware}` : "Checking this computer…"}</span>
      {!editing ? (
        <button
          type="button"
          className="link small"
          title="Plan for a different amount of GPU memory, to leave room for other programs or for another computer. It changes the labels only."
          onClick={() => setEditing(true)}
        >
          Change
        </button>
      ) : (
        <form
          className="gw-find-plan"
          onSubmit={(e) => {
            e.preventDefault();
            const n = Math.round(Number(text));
            onPlan(Number.isFinite(n) && n >= 1 && n <= 1024 ? n : null);
            setEditing(false);
          }}
        >
          <input type="number" min={1} max={1024} placeholder="GB" value={text} onChange={(e) => setText(e.target.value)} aria-label="GPU memory in GB" autoFocus />
          <Button size="sm" type="submit">
            Use
          </Button>
          <button
            type="button"
            className="link small"
            onClick={() => {
              setText("");
              onPlan(null);
              setEditing(false);
            }}
          >
            This computer
          </button>
        </form>
      )}
    </div>
  );
}

/** Downloads in progress, on every tab, until each finishes. */
function DownloadsTray() {
  const store = useDownloads();
  if (store.entries.length === 0) return null;
  return (
    <ul className="gw-find-tray" aria-label="Downloads">
      {store.entries.map((e) => (
        <TrayRow key={e.key} id={e.key} label={e.label} />
      ))}
    </ul>
  );
}

function TrayRow({ id, label }: { id: string; label: string }) {
  const dl = useStoreDownload(id);
  return (
    <li>
      <DownloadIcon size={14} aria-hidden="true" />
      <span className="gw-find-tray-name mono" title={label}>
        {label}
      </span>
      <DownloadControls dl={dl} />
    </li>
  );
}

/** A Download button, or its progress and controls while it runs. */
function DownloadButton({ dlKey, label, run, ctx, primary, disabled, title }: {
  dlKey: string;
  label: string;
  run: StoreJob["run"];
  ctx: Ctx;
  primary?: boolean;
  disabled?: boolean;
  title?: string;
}) {
  const dl = useStoreDownload(dlKey);
  if (dl.phase !== "idle") return <DownloadControls dl={dl} />;
  return (
    <Button size="sm" variant={primary ? "primary" : "secondary"} disabled={disabled || ctx.state.offline} title={ctx.state.offline ? "Offline mode is on" : title} onClick={() => ctx.download(dlKey, label, run)}>
      Download
    </Button>
  );
}

// ---------- For you ----------

/** The recommendation for this computer, chips that change it, two alternatives and the full list. */
export function ForYou({ ctx, view, answers, onAnswers, footer, inline = false }: {
  ctx: Ctx;
  view: StarterView | null;
  answers: Answers | null;
  onAnswers: (a: Answers | null) => void;
  footer?: ReactNode;
  /** In the setup wizard: no side panel; details and next steps open in the card. */
  inline?: boolean;
}) {
  const [chosen, setChosen] = useState<Chosen | null>(null);
  const [all, setAll] = useState(false);
  const asked = useRef(0);
  useEffect(() => {
    if (!answers) {
      setChosen(null);
      return;
    }
    const id = ++asked.current;
    api.chooseModel(answers, ctx.planGib).then(
      (c) => id === asked.current && setChosen(c),
      (e) => ctx.setError(errorText(e)),
    );
    // A new answer or plan figure asks again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [answers, ctx.planGib]);

  if (!view) return <p className="muted small">Checking this computer…</p>;
  const byId = (id: string | null | undefined) => (id ? view.entries.find((e) => e.id === id) : undefined);
  const tier = view.tier !== null ? view.tiers[view.tier] : null;
  const pick = chosen
    ? chosen.choices.map((c) => ({ entry: byId(c.id), why: c.why }))
    : tier
      ? [tier.recommended, tier.faster, tier.vision].map((id) => ({ entry: byId(id), why: "" }))
      : [];
  const [first, ...rest] = pick.filter((p): p is { entry: StarterEntry; why: string } => !!p.entry);
  const set = (change: Partial<Answers>) =>
    onAnswers({ purpose: "chat", priority: "balanced", longDocuments: false, ...answers, ...change });

  return (
    <div className="gw-foryou">
      <div className="gw-foryou-chips">
        <div role="group" aria-label="What is it for?">
          <span className="gw-chip-label">What for</span>
          {PURPOSES.map((p) => (
            <button key={p.id} type="button" className={`gw-filter-chip${answers?.purpose === p.id ? " on" : ""}`} aria-pressed={answers?.purpose === p.id} onClick={() => set({ purpose: p.id })}>
              {p.label}
            </button>
          ))}
        </div>
        <div role="group" aria-label="What matters more?">
          <span className="gw-chip-label">What matters</span>
          {PRIORITIES.map((p) => (
            <button key={p.id} type="button" className={`gw-filter-chip${answers?.priority === p.id ? " on" : ""}`} aria-pressed={answers?.priority === p.id} onClick={() => set({ priority: p.id })}>
              {p.label}
            </button>
          ))}
          <button
            type="button"
            className={`gw-filter-chip${answers?.longDocuments ? " on" : ""}`}
            aria-pressed={!!answers?.longDocuments}
            title="Long documents need a long context"
            onClick={() => set({ longDocuments: !answers?.longDocuments })}
          >
            Long documents
          </button>
          {answers && (
            <button type="button" className="link small" onClick={() => onAnswers(null)}>
              Clear
            </button>
          )}
        </div>
      </div>
      {chosen?.note && <p className="small warn-text">{chosen.note}</p>}

      {first ? (
        <StarterCard entry={first.entry} why={first.why} ctx={ctx} badge={chosen ? "Your answer" : "Recommended"} big inline={inline} />
      ) : (
        <p className="small warn-text">
          GPU memory couldn't be read, so no single model is recommended. Pick what you'll use it for above, see all starter models
          below, or use Change to enter a figure.
        </p>
      )}
      {rest.length > 0 && (
        <>
          <h3 className="gw-foryou-sub">Also good</h3>
          <ul className="gw-find-rows">
            {rest.slice(0, 2).map((p) => (
              <StarterRow key={p.entry.id} entry={p.entry} why={p.why} ctx={ctx} inline={inline} />
            ))}
          </ul>
        </>
      )}

      <button type="button" className="link small gw-foryou-all" aria-expanded={all} onClick={() => setAll((a) => !a)}>
        {all ? "Hide the full list" : "See all starter models"}
      </button>
      {all &&
        view.tiers.map((t, i) => (
          <section key={t.minGib} className="gw-foryou-tier">
            <h3 className="gw-foryou-sub">
              {t.label}
              {i === view.tier && <span className="muted"> · this computer</span>}
            </h3>
            <ul className="gw-find-rows">
              {[t.recommended, t.faster, t.vision].map((id) => {
                const e = byId(id);
                return e ? <StarterRow key={`${t.minGib}-${e.id}`} entry={e} why="" ctx={ctx} inline={inline} /> : null;
              })}
            </ul>
          </section>
        ))}
      {view.stale && <p className="small warn-text">{view.stale}</p>}
      {footer && <div className="gw-foryou-footer">{footer}</div>}
    </div>
  );
}

function starterRun(entry: StarterEntry): StoreJob["run"] {
  return (onProgress) => api.downloadStarter(entry.id, onProgress);
}

function StarterCard({ entry, why, ctx, badge, big, inline }: { entry: StarterEntry; why: string; ctx: Ctx; badge?: string; big?: boolean; inline: boolean }) {
  const [details, setDetails] = useState(false);
  const model = ctx.state.models.find((m) => m.sha256 === entry.sha256);
  return (
    <div className={`gw-starter-card${big ? " big" : ""}`}>
      <div className="gw-starter-card-main">
        <div className="gw-starter-card-title">
          <span>{entry.name}</span>
          {badge && <Pill tone="accent">{badge}</Pill>}
        </div>
        <p className="gw-starter-card-summary">{entry.summary}</p>
        <div className="gw-starter-card-facts">
          <span className={fitPill(entry.fit, entry.tight)}>{entry.label}</span>
          <span>{formatBytes(entry.total)}</span>
          <span>{entry.publisher}</span>
        </div>
        {why && <p className="small gw-starter-card-why">Why: {why}</p>}
      </div>
      <div className="gw-starter-card-actions">
        {model || entry.installed ? (
          <>
            <Pill tone="ok">Downloaded</Pill>
            {model && !inline && (
              <button type="button" className="link small" onClick={() => ctx.setPanel({ kind: "model", sha: model.sha256 })}>
                Set up
              </button>
            )}
          </>
        ) : (
          <DownloadButton dlKey={entry.path} label={entry.path} run={starterRun(entry)} ctx={ctx} primary title={`From ${entry.repo}, pinned to the reviewed commit and SHA-256`} />
        )}
        <button type="button" className="link small" onClick={() => (inline ? setDetails((d) => !d) : ctx.setPanel({ kind: "starter", entry }))}>
          {inline && details ? "Hide details" : "Details"}
        </button>
      </div>
      {inline && details && <TradeOffCard card={entry.card} />}
      {inline && model && <AfterDownload model={model} state={ctx.state} refresh={ctx.refresh} setError={ctx.setError} />}
    </div>
  );
}

function StarterRow({ entry, why, ctx, inline }: { entry: StarterEntry; why: string; ctx: Ctx; inline: boolean }) {
  const model = ctx.state.models.find((m) => m.sha256 === entry.sha256);
  return (
    <li className="gw-find-row">
      <button
        type="button"
        className="gw-find-row-main"
        onClick={() => !inline && ctx.setPanel({ kind: "starter", entry })}
        disabled={inline}
        title={inline ? undefined : "Details"}
      >
        <span className="gw-find-row-name">{entry.name}</span>
        <span className="gw-find-row-note">{why || entry.summary}</span>
      </button>
      <span className="gw-find-row-size">{formatBytes(entry.total)}</span>
      <span className={fitPill(entry.fit, entry.tight)}>{entry.label}</span>
      {model || entry.installed ? <Pill tone="ok">Downloaded</Pill> : <DownloadButton dlKey={entry.path} label={entry.path} run={starterRun(entry)} ctx={ctx} />}
    </li>
  );
}

// ---------- Browse ----------

interface RowFit {
  best: RepoFile | null;
  gated: boolean;
}

function Browse({ ctx }: { ctx: Ctx }) {
  const [view, setView] = useState<RankingView | null>(null);
  const [kind, setKind] = useState("text-generation");
  const [sort, setSort] = useState("trendingScore");
  const [onlyFits, setOnlyFits] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fits, setFits] = useState<Record<string, RowFit | "error">>({});
  const offline = ctx.state.offline;

  useEffect(() => {
    if (offline) return;
    setError(null);
    setView(null);
    api.hubRanking(kind, sort).then(setView, (e) => setError(errorText(e)));
  }, [kind, sort, offline]);

  const resolve = useCallback(
    async (repo: string) => {
      try {
        const r = await api.listRepoFiles(repo, ctx.planGib);
        setFits((m) => ({ ...m, [repo]: { best: r.files.find((f) => f.best) ?? null, gated: r.gated } }));
      } catch {
        setFits((m) => ({ ...m, [repo]: "error" }));
      }
    },
    [ctx.planGib],
  );
  // Sizes and fit for the first twelve rows, two at a time; the rest when opened.
  useEffect(() => {
    setFits({});
    if (!view) return;
    let stop = false;
    const queue = view.rows.slice(0, 12).map((r) => r.repo);
    const worker = async () => {
      while (!stop && queue.length) await resolve(queue.shift()!);
    };
    void Promise.all([worker(), worker()]);
    return () => {
      stop = true;
    };
  }, [view, resolve]);

  const rows = useMemo(() => {
    if (!view) return [];
    return view.rows.filter((r) => {
      const f = fits[r.repo];
      return !onlyFits || (f && f !== "error" && f.best);
    });
  }, [view, fits, onlyFits]);

  if (offline) return <p className="small muted">Offline mode is on, so the hub isn't asked. For you still works.</p>;
  return (
    <div className="gw-browse">
      <div className="gw-browse-chips">
        {(view?.kinds ?? [{ id: kind, label: "Chat and coding" }]).map((k) => (
          <button key={k.id} type="button" className={`gw-filter-chip${kind === k.id ? " on" : ""}`} aria-pressed={kind === k.id} onClick={() => setKind(k.id)}>
            {k.label}
          </button>
        ))}
        <span className="gw-chip-sep" aria-hidden="true" />
        {(view?.sorts ?? [{ id: sort, label: "Trending" }]).map((s) => (
          <button key={s.id} type="button" className={`gw-filter-chip${sort === s.id ? " on" : ""}`} aria-pressed={sort === s.id} onClick={() => setSort(s.id)}>
            {s.label}
          </button>
        ))}
        <span className="gw-chip-sep" aria-hidden="true" />
        <button
          type="button"
          className={`gw-filter-chip${onlyFits ? " on" : ""}`}
          aria-pressed={onlyFits}
          title="Only repositories with a file that fits on this GPU (of the rows checked so far)"
          onClick={() => setOnlyFits((o) => !o)}
        >
          Fits my GPU
        </button>
      </div>
      {view?.note && <p className="small warn-text">{view.note}</p>}
      {error && <div className="banner error">{error}</div>}
      {!view && !error && <p className="small muted">Asking the hub…</p>}
      <ul className="gw-find-rows">
        {rows.map((r) => {
          const f = fits[r.repo];
          const fit = f && f !== "error" ? f : null;
          return (
            <RepoRow
              key={r.repo}
              repo={r.repo}
              known={r.knownPublisher}
              warnings={r.warnings}
              size={fit?.best ? formatBytes(fit.best.size) : f === "error" ? "—" : f ? "no fit" : "…"}
              fit={fit?.best ? <span className={fitPill(fit.best.fit, fit.best.tight)}>{fit.best.label}</span> : null}
              onOpen={() => void ctx.openRepo(r.repo)}
            />
          );
        })}
      </ul>
      {view && <p className="small muted gw-find-about" title={view.about}>{view.intro}</p>}
    </div>
  );
}

/** One repository in Browse or Search; the panel holds the details and warnings. */
function RepoRow({ repo, known, warnings, size, fit, onOpen }: {
  repo: string;
  known: boolean;
  warnings: string[];
  size: string;
  fit: ReactNode;
  onOpen: () => void;
}) {
  return (
    // The whole row opens the panel; the button gives it keyboard focus (its click bubbles here).
    <li className="gw-find-row clickable" onClick={onOpen}>
      <button type="button" className="gw-find-row-main">
        <span className="gw-find-row-name mono">
          {repo}
          {known && <BadgeCheck className="gw-find-known" size={14} aria-label="Known publisher" />}
        </span>
      </button>
      <span className="gw-find-row-size">{size}</span>
      <span className="gw-find-row-fit">{fit}</span>
      <span className="gw-find-row-warn" title={warnings.join("\n")}>
        {warnings.length > 0 && <AlertTriangle size={15} aria-label={`${warnings.length} warning${warnings.length === 1 ? "" : "s"}: ${warnings.join("; ")}`} />}
      </span>
      <ChevronRight size={16} className="muted" aria-hidden="true" />
    </li>
  );
}

// ---------- Search ----------

const SEARCH_PAUSE_MS = 350;

function SearchTab({ ctx }: { ctx: Ctx }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<HubListing[] | null>(null);
  const [searching, setSearching] = useState(false);
  const asked = useRef(0);
  const term = query.trim();
  const isRepo = /^[\w.-]+\/[\w.-]+$/.test(term);
  const search = useCallback(
    (t: string) => {
      const id = ++asked.current;
      setSearching(true);
      api.searchHub(t).then(
        (r) => {
          if (id !== asked.current) return;
          setResults(r);
          setSearching(false);
        },
        (e) => {
          if (id !== asked.current) return;
          setSearching(false);
          ctx.setError(errorText(e));
        },
      );
    },
    [ctx],
  );
  // Results as you type, after a short pause.
  useEffect(() => {
    if (term.length < 2 || isRepo) {
      asked.current++;
      setSearching(false);
      if (!term) setResults(null);
      return;
    }
    const t = window.setTimeout(() => search(term), SEARCH_PAUSE_MS);
    return () => window.clearTimeout(t);
  }, [term, isRepo, search]);

  if (ctx.state.offline) return <p className="small muted">Offline mode is on, so the hub isn't asked. You can still import local files.</p>;
  return (
    <div className="gw-search">
      <form
        className="gw-hub-search-field"
        onSubmit={(e) => {
          e.preventDefault();
          if (isRepo) void ctx.openRepo(term);
          else if (term) search(term);
        }}
      >
        <input
          type="search"
          placeholder="Search Hugging Face (e.g. qwen3), or owner/repository"
          aria-label="Search Hugging Face for GGUF models, or type owner/repository"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
        <button type="submit" className="gw-hub-search-go" aria-label="Search" title="Search">
          <Search size={18} aria-hidden="true" />
        </button>
      </form>
      <p className="small muted gw-search-note">Hugging Face sees what you type and what you download.</p>
      {isRepo && (
        <button type="button" className="link small" onClick={() => void ctx.openRepo(term)}>
          Open {term}
        </button>
      )}
      {searching && <p className="small muted">Searching…</p>}
      {!searching && results && results.length === 0 && <p className="small muted">No GGUF models match “{term}”.</p>}
      {results && results.length > 0 && (
        <ul className="gw-find-rows">
          {results.map((r) => (
            <RepoRow
              key={r.repo}
              repo={r.repo}
              known={false}
              warnings={r.gated ? ["Gated: needs a signed-in Hugging Face account"] : []}
              size={`${r.downloads.toLocaleString()} downloads`}
              fit={null}
              onOpen={() => void ctx.openRepo(r.repo)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

// ---------- The side panel ----------

function RepoPanel({ repo, ctx }: { repo: RepoView; ctx: Ctx }) {
  const [open, setOpen] = useState<string | null>(null);
  const best = repo.files.find((f) => f.best);
  return (
    <div className="gw-repo-panel">
      {repo.warnings.length > 0 && (
        <ul className="gw-repo-warnings" aria-label="Warnings">
          {repo.warnings.map((w) => (
            <li key={w}>
              <AlertTriangle size={15} aria-hidden="true" /> {w}
            </li>
          ))}
        </ul>
      )}
      {repo.gated && (
        <p className="small warn-text">
          Gated: Hugging Face gives these files only to a signed-in account that accepted the model's terms, so Gardenwall
          can't download them. Download the file in your browser and add it under Import a file; it's checked the same way.
        </p>
      )}
      <p className="small muted">
        {repo.knownPublisher && (
          <span title={KNOWN_PUBLISHER}>
            <BadgeCheck size={13} className="gw-find-known" aria-hidden="true" /> Known publisher ·{" "}
          </span>
        )}
        <Term id="licence">Licence</Term> {repo.license ?? "not declared"} · commit {repo.commit.slice(0, 10)}
        {repo.skipped > 0 && ` · ${repo.skipped} file(s) hidden (no verifiable hash or unsafe name)`}
      </p>
      {best ? (
        <p className="small">
          <strong>Best fit here:</strong> {best.quant.label ?? best.path}. {best.whyBest}
        </p>
      ) : (
        repo.files.some((f) => f.role === "model") && (
          <p className="small warn-text">No file here fits on this computer's GPU, so none is marked as the best fit.</p>
        )
      )}
      <ul className="gw-repo-files">
        {repo.files.map((f) => (
          <li key={f.path} className={f.best ? "best" : undefined}>
            <div className="gw-repo-file">
              <div className="gw-repo-file-name">
                <span className="mono">{f.quant.label ?? f.path}</span>
                {f.best && <Pill tone="ok">Best fit</Pill>}
                {f.role !== "model" && <span className="small muted">{f.label}</span>}
              </div>
              <span className="gw-find-row-size">{formatBytes(f.size)}</span>
              {f.role === "model" && <span className={fitPill(f.fit, f.tight)}>{f.label}</span>}
              <DownloadButton
                dlKey={f.path}
                label={f.path}
                run={(onProgress) => api.downloadModel(repo.repo, f.path, onProgress)}
                ctx={ctx}
                primary={f.best}
                disabled={repo.gated}
                title={repo.gated ? "Gated: needs a signed-in Hugging Face account" : f.parts > 1 ? `${f.parts} parts, downloaded and checked together` : f.path}
              />
            </div>
            {f.quant.quality && <div className="small muted">{f.quant.quality}</div>}
            {f.card && (
              <button type="button" className="link small" aria-expanded={open === f.path} onClick={() => setOpen(open === f.path ? null : f.path)}>
                {open === f.path ? "Hide" : "What this choice means"}
              </button>
            )}
            {open === f.path && f.card && <TradeOffCard card={f.card} />}
          </li>
        ))}
      </ul>
      <p className="small muted">
        {repo.fitNote} <span title={repo.quantAbout}>About file sizes</span>
      </p>
    </div>
  );
}

function StarterPanel({ entry, ctx, reviewed }: { entry: StarterEntry; ctx: Ctx; reviewed: string }) {
  const model = ctx.state.models.find((m) => m.sha256 === entry.sha256);
  return (
    <div className="gw-repo-panel">
      <p className="gw-starter-card-summary">{entry.summary}</p>
      <div className="gw-starter-card-facts">
        <span className={fitPill(entry.fit, entry.tight)}>{entry.label}</span>
        <span>{formatBytes(entry.total)}</span>
        {entry.projector && <span>with its image projector</span>}
      </div>
      <div>
        {model || entry.installed ? (
          <Pill tone="ok">Downloaded</Pill>
        ) : (
          <DownloadButton dlKey={entry.path} label={entry.path} run={starterRun(entry)} ctx={ctx} primary title={`From ${entry.repo}, pinned to the reviewed commit and SHA-256`} />
        )}
      </div>
      <TradeOffCard card={entry.card} />
      <p className="small muted">
        <Term id="licence">Licence</Term> {entry.licence} · {entry.publisher}
        <br />
        <span className="mono">
          {entry.repo} · {entry.path} · commit {entry.commit.slice(0, 10)}
        </span>
        <br />
        {reviewed ? `${reviewed}: pinned to the exact file that was reviewed.` : `Reviewed ${entry.reviewed}.`}
      </p>
    </div>
  );
}

function ModelPanel({ sha, ctx }: { sha: string; ctx: Ctx }) {
  const model: ModelRecord | undefined = ctx.state.models.find((m) => m.sha256 === sha);
  if (!model) return <p className="muted small">Checking the download…</p>;
  return (
    <div className="gw-repo-panel">
      <p>
        <strong>{model.displayName}</strong> is downloaded and verified.
      </p>
      <AfterDownload model={model} state={ctx.state} refresh={ctx.refresh} setError={ctx.setError} />
    </div>
  );
}

/** The For you view for the setup wizard: no tabs or side panel, details and next steps in the card. */
export function SetupModelPicker({ state, refresh, setError }: { state: AppView; refresh: () => Promise<void>; setError: (e: string | null) => void }) {
  const [view, setView] = useState<StarterView | null>(null);
  const [answers, setAnswers] = useState<Answers | null>(null);
  const store = useDownloads();
  const modelCount = state.models.length;
  useEffect(() => {
    api.starterModels(null).then(setView, (e) => setError(errorText(e)));
  }, [modelCount, setError]);
  const ctx: Ctx = {
    state,
    refresh,
    setError,
    planGib: null,
    openRepo: async () => {},
    setPanel: () => {},
    download: (key, label, run) => store.start({ key, label, run, onDone: () => refresh(), onError: setError }),
  };
  return (
    <>
      {view && <p className="small muted">For {view.hardware}</p>}
      <ForYou ctx={ctx} view={view} answers={answers} onAnswers={setAnswers} inline />
    </>
  );
}
