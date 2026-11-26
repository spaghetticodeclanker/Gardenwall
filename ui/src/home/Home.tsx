// Home (UI redesign phase 2rounds 1–3): where you start
// (load a model) and a dashboard worth coming back to while work runs: the
// model, GPU and RAM, sessions, what's waiting for you and system status.
import { useCallback, useEffect, useRef, useState } from "react";
import { Bot, Cpu, Download, FolderOpen, MessageSquare, RefreshCw, ShieldAlert, ShieldCheck, Star, Unplug, X } from "lucide-react";
import { api, errorText, type AppView, type DashboardRow, type ModelRecord, type SystemMemory, type TranscriptSummary, type UpdatesView } from "../api";
import { Growth } from "../art/Growth";
import type { Page } from "../shell/TopBar";
import { Button, EmptyState, ErrorCard, IconButton, Pill, SegmentedBar, Toggle, useToast } from "../ui";
import { programNames } from "../programs";
import { VcRuntimeNotice } from "../VcRuntimeNotice";
import { markSeen, seenAt } from "../seen";
import { usePoll } from "../usePoll";
import { WhatsNew } from "./WhatsNew";
import "./home.css";

const G = 1024 ** 3;
/** The VRAM margin llama-server is launched with (--fit-target 3072). */
const SAFETY_MARGIN = 3 * G;
const gb = (n: number) => `${(n / G).toFixed(1)} GB`;

const STATUS: Record<DashboardRow["status"], { text: string; tone: "ok" | "warn" | "danger" | "neutral" | "accent" }> = {
  open: { text: "Open", tone: "neutral" },
  working: { text: "Working", tone: "accent" },
  approval: { text: "Needs your approval", tone: "warn" },
  needsYou: { text: "Needs you", tone: "warn" },
  waitingTurn: { text: "Waiting for its turn", tone: "neutral" },
  finished: { text: "Finished", tone: "ok" },
  stopped: { text: "Stopped", tone: "neutral" },
  failed: { text: "Failed", tone: "danger" },
  paused: { text: "Paused: continue or stop", tone: "warn" },
};
const WAITING: DashboardRow["status"][] = ["approval", "needsYou", "paused", "failed"];
/** Home fits one screen: lists stop here and link to the rest. */
const MAX_WORKING = 3;
/** Bloom and focus: Home fading out, then the reveal from the centre. */
const FADE_MS = 200;
const REVEAL_MS = 900;
const MAX_WAITING = 4;
/** Fewer while something is working, when "Working now" takes room. */
const MAX_SESSIONS = 4;
const MAX_SESSIONS_BUSY = 3;
/** Order in Home's session list: lower first. */
const RANK: Record<DashboardRow["status"], number> = {
  working: 0,
  approval: 1,
  needsYou: 1,
  paused: 1,
  waitingTurn: 2,
  failed: 3,
  open: 4,
  finished: 4,
  stopped: 4,
};

function ago(ms: number) {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400 * 2) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}

export function Home({
  state,
  refresh,
  go,
  updates,
  onOpenSession,
  onOpenSaved,
}: {
  state: AppView;
  refresh: () => Promise<void>;
  go: (p: Page) => void;
  updates: UpdatesView | null;
  onOpenSession: (sid: string) => void;
  onOpenSaved: (id: string) => void;
}) {
  const toast = useToast();
  const [memory, setMemory] = useState<SystemMemory | null>(null);
  const [rows, setRows] = useState<DashboardRow[]>([]);
  const [saved, setSaved] = useState<TranscriptSummary[]>([]);
  const [approvals, setApprovals] = useState(0);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  // A model loading or unloading while Home is open
  // (round 8, "bloom and focus", tuned toward option A): loading fades Home
  // out (0.2 s) while it still shows the old state, then the updated Home is
  // revealed outward from the centre by a widening soft-edged circle with a
  // diffuse band of green light on its edge, sharpening from a slight blur
  // and settling from 102% to 100% size; the model's dot pulses once and the
  // tree grows after. Unloading just fades out and back in, and the tree
  // recedes.
  // Switching straight between two models changes instantly; reduced motion
  // is a plain fade.
  const actual = state.loaded;
  const actualSha = actual?.sha256 ?? null;
  const [loaded, setShownLoaded] = useState(actual);
  const loadedSha = loaded?.sha256 ?? null;
  const [treeKey, setTreeKey] = useState(loadedSha ?? "none");
  const [pulse, setPulse] = useState(false);
  const page = useRef<HTMLDivElement>(null);
  const bloom = useRef<HTMLDivElement>(null);
  const changing = useRef(false);
  useEffect(() => {
    const was = loadedSha;
    const crossing = (was === null) !== (actualSha === null);
    if (!crossing || changing.current) {
      // Same model (fresh facts) or one model to another: show it as it is.
      if (!changing.current) {
        setShownLoaded(actual);
        setTreeKey(actualSha ?? "none");
      }
      return;
    }
    const el = page.current;
    const glow = bloom.current;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    changing.current = true;
    let frame = 0;
    const timers: number[] = [];
    const clear = () => {
      if (!el) return;
      el.style.opacity = "";
      el.style.transition = "";
      el.style.filter = "";
      el.style.transform = "";
      el.style.maskImage = "";
      el.style.webkitMaskImage = "";
      if (glow) glow.style.opacity = "0";
    };
    const swap = () => {
      setShownLoaded(actual);
      if (actualSha !== null) {
        setPulse(true);
        timers.push(window.setTimeout(() => setPulse(false), 1200));
      }
      // Unloading: the tree recedes as the new Home appears.
      if (actualSha === null) setTreeKey("none");
    };
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      clear();
      changing.current = false;
      if (actualSha !== null) setTreeKey(actualSha);
    };
    if (!el || still || actualSha === null) {
      // Unloading (and reduced motion): Home just fades out and back in.
      if (el) {
        el.style.transition = `opacity ${FADE_MS}ms ease`;
        el.style.opacity = "0";
      }
      timers.push(
        window.setTimeout(() => {
          swap();
          if (el) {
            el.style.transition = "opacity 300ms ease";
            el.style.opacity = "1";
          }
          timers.push(window.setTimeout(finish, 320));
        }, FADE_MS + 20),
      );
    } else {
      el.style.transition = `opacity ${FADE_MS}ms ease`;
      el.style.opacity = "0";
      timers.push(
        window.setTimeout(() => {
          swap();
          el.style.transition = "none";
          el.style.opacity = "1";
          const w = el.clientWidth;
          const h = el.clientHeight;
          const radius = Math.hypot(w, h) * 0.62;
          const cy = el.scrollTop + h * 0.45;
          if (glow) {
            // A layer over the visible part of Home that carries the green light.
            glow.style.top = `${el.scrollTop}px`;
            glow.style.height = `${h}px`;
            glow.style.opacity = "1";
          }
          const start = performance.now();
          const ease = (t: number) => 1 - Math.pow(1 - t, 3);
          const step = (now: number) => {
            const t = Math.min(1, (now - start) / REVEAL_MS);
            const r = ease(Math.max(0, (t - 0.1) / 0.9));
            const edge = r * radius;
            // A faint band of green light rides the soft edge of the reveal
            // (no dot at the centre), fading as it spreads.
            if (glow) {
              const light = 0.2 * (1 - r * 0.85);
              glow.style.background = `radial-gradient(circle at 50% ${h * 0.45}px, transparent ${Math.max(0, edge - 300)}px, rgba(79, 191, 127, ${(light * 0.45).toFixed(3)}) ${Math.max(0, edge - 190)}px, rgba(79, 191, 127, ${light.toFixed(3)}) ${Math.max(0, edge - 95)}px, rgba(79, 191, 127, ${(light * 0.35).toFixed(3)}) ${edge}px, transparent ${edge + 70}px)`;
            }
            const mask = `radial-gradient(circle at 50% ${cy}px, #000 ${Math.max(0, edge - 180)}px, transparent ${edge}px)`;
            el.style.maskImage = mask;
            el.style.webkitMaskImage = mask;
            el.style.filter = `blur(${(1 - r) * 6}px)`;
            el.style.transform = `scale(${1.02 - 0.02 * r})`;
            if (t < 1) frame = requestAnimationFrame(step);
            else finish();
          };
          frame = requestAnimationFrame(step);
          // Frames pause while the window is hidden: never leave Home blurred.
          timers.push(
            window.setTimeout(() => {
              cancelAnimationFrame(frame);
              finish();
            }, REVEAL_MS + 400),
          );
        }, FADE_MS + 20),
      );
    }
    return () => {
      timers.forEach((t) => window.clearTimeout(t));
      cancelAnimationFrame(frame);
      clear();
      changing.current = false;
    };
    // Only a model loading or unloading starts (or stops) a transition.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actualSha]);
  // Fresh facts about the same model (context, placement) show at once.
  useEffect(() => {
    if (!changing.current && (actual?.sha256 ?? null) === loadedSha) setShownLoaded(actual);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actual]);
  const treeLoaded = treeKey !== "none" && treeKey === loadedSha;

  usePoll(() => void api.systemMemory().then(setMemory).catch(() => setMemory(null)), 3000);
  usePoll(() => {
    void api.agentDashboard().then(setRows).catch(() => {});
    void api.listPendingApprovals().then((p) => setApprovals(p.length)).catch(() => {});
  }, 2000);
  usePoll(() => void api.agentTranscripts().then(setSaved).catch(() => {}), 15000);

  const models = state.models.filter((m) => m.architecture !== "clip");
  const favourite = models.find((m) => m.sha256 === state.favouriteModel) ?? null;
  const suggestion: ModelRecord | null = favourite ?? models[0] ?? null;
  // Keyed by its seed: when the model that loads is the one Home suggested
  // (the usual case), the same tree grows on from its partial state instead
  // of being drawn again from nothing.
  const treeSeed = treeLoaded ? treeKey : (suggestion?.sha256 ?? "gardenwall");

  const load = useCallback(
    async (m: ModelRecord) => {
      setLoading(true);
      setLoadError(null);
      try {
        await api.loadModel(m.sha256, m.load);
        await refresh();
        toast({ text: `${m.displayName} is loaded`, tone: "ok" });
      } catch (e) {
        setLoadError(errorText(e));
      } finally {
        setLoading(false);
      }
    },
    [refresh, toast],
  );

  const setFavourite = async (sha: string | null, onLaunch: boolean) => {
    try {
      await api.setFavouriteModel(sha, onLaunch);
      await refresh();
    } catch (e) {
      toast({ text: errorText(e), tone: "danger" });
    }
  };

  // Finished sessions you haven't looked at since they finished also wait for you.
  const [, setSeenTick] = useState(0);
  const unseen = rows.filter((r) => r.status === "finished" && r.endedAtMs !== null && r.endedAtMs > seenAt(r.id));
  const waiting = [...rows.filter((r) => WAITING.includes(r.status)), ...unseen];
  // Also sessions that are idle but left a program running in their sandbox.
  const working = rows.filter((r) => ["working", "approval", "needsYou", "waitingTurn"].includes(r.status) || r.background.length > 0);
  // A session that is open is listed once, as the live one (not again as saved).
  const liveTranscripts = new Set(rows.map((r) => r.transcript).filter((t): t is string => !!t));
  const savedOnly = saved.filter((t) => !liveTranscripts.has(t.id));
  const maxSessions = working.length > 0 ? MAX_SESSIONS_BUSY : MAX_SESSIONS;
  // Sessions: the active ones first (working, then waiting for you), then the most recent.
  const now = Date.now();
  const listed = [...rows].sort(
    (a, b) => RANK[a.status] - RANK[b.status] || (b.endedAtMs ?? now) - (a.endedAtMs ?? now),
  );
  const report = updates?.report ?? null;
  const installedRuntimes = state.runtimes.filter((r) => r.installed);

  const gpu = memory?.gpu ?? null;
  const gpuSegments = gpu
    ? (() => {
        const model = gpu.modelBytes ?? 0;
        const used = gpu.usedBytes ?? model;
        const other = Math.max(0, used - model);
        const margin = Math.min(SAFETY_MARGIN, Math.max(0, gpu.totalBytes - used));
        return [
          ...(model > 0 ? [{ label: "Model and context", value: model, color: "--accent" }] : []),
          { label: "Other apps", value: other, color: "#6b7a72" },
          { label: "Safety margin", value: margin, color: "", hatched: true },
        ];
      })()
    : [];

  return (
    <div className={`page gw-home${working.length > 0 ? " busy" : ""}`} ref={page}>
      <div className="gw-home-bloom" ref={bloom} aria-hidden="true" />
      <WhatsNew current={updates?.report?.app.current ?? null} onOpen={() => go("updates")} />
      <VcRuntimeNotice state={state} refresh={refresh} />
      <section className="gw-home-hero">
        <div className="gw-home-art">
          <Growth
            seed={treeSeed}
            anchor={0.5}
            depth={7}
            roots={2}
            fit
            grown={treeLoaded ? 1 : 0.35}
            animate={treeLoaded}
            key={treeSeed}
          />
        </div>
        <div className="gw-home-hero-text">
          <h1>{loaded ? "Ready to work" : "Load a model to start working"}</h1>
          <p>Everything runs on this computer. Nothing leaves it unless you allow it.</p>
        </div>
        <div className={`gw-home-model gw-card${pulse ? " arrived" : ""}`}>
          {loaded ? (
            <>
              <div className="gw-home-model-head">
                <span className="gw-model-dot" />
                <div className="gw-home-model-name">{loaded.displayName}</div>
              </div>
              <div className="gw-home-model-facts">
                <span>{loaded.contextWindow.toLocaleString()}-token context</span>
                {loaded.isolation === "sandboxed" ? (
                  <span className="ok"><ShieldCheck size={14} aria-hidden="true" /> Sandboxed</span>
                ) : (
                  <span className="danger"><ShieldAlert size={14} aria-hidden="true" /> Not isolated</span>
                )}
                {loaded.placement && <Pill tone="warn">Partly in system RAM</Pill>}
              </div>
              <div className="gw-home-actions">
                <Button variant="primary" icon={Bot} onClick={() => go("agent")}>Open Agent</Button>
                <Button icon={MessageSquare} onClick={() => go("chat")}>Open Chat</Button>
                <Button
                  variant="ghost"
                  icon={Unplug}
                  onClick={async () => {
                    await api.unloadModel();
                    await refresh();
                  }}
                >
                  Unload
                </Button>
              </div>
            </>
          ) : suggestion ? (
            <>
              <div className="gw-home-model-head">
                <span className="gw-model-dot off" />
                <div className="gw-home-model-name">{suggestion.displayName}</div>
                {favourite && <Pill tone="accent">Favourite</Pill>}
              </div>
              <div className="gw-home-model-facts">
                <span>{gb(suggestion.size)}</span>
                {suggestion.contextLength && <span>trained for {suggestion.contextLength.toLocaleString()} tokens</span>}
              </div>
              <div className="gw-home-actions">
                <Button variant="primary" icon={Download} disabled={loading} onClick={() => void load(suggestion)}>
                  {loading ? "Loading…" : "Load model"}
                </Button>
                <Button onClick={() => go("models")}>Choose another</Button>
              </div>
            </>
          ) : (
            <>
              <div className="gw-home-model-name">No models yet</div>
              <p className="muted">Download one that fits this computer, or import a GGUF file you already have.</p>
              <div className="gw-home-actions">
                <Button variant="primary" icon={Download} onClick={() => go("models")}>Find a model</Button>
              </div>
            </>
          )}
          {loadError && (
            <div style={{ marginTop: 12 }}>
              <ErrorCard
                title="The model didn't load"
                cause={loadError}
                fix={<Button variant="primary" onClick={() => go("models")}>Open Models</Button>}
              />
            </div>
          )}
          {(loaded || suggestion) && (
            <div className="gw-home-fav">
              <Star size={14} aria-hidden="true" className={favourite ? "on" : ""} />
              {favourite ? (
                <>
                  <span>
                    Favourite: <b>{favourite.displayName}</b>
                  </span>
                  <label className="gw-home-fav-launch">
                    Load when Gardenwall starts
                    <Toggle
                      label="Load the favourite model when Gardenwall starts"
                      checked={state.loadOnLaunch}
                      onChange={(v) => void setFavourite(favourite.sha256, v)}
                    />
                  </label>
                  <button type="button" className="gw-linkbtn" onClick={() => void setFavourite(null, false)}>Clear</button>
                </>
              ) : (
                <button
                  type="button"
                  className="gw-linkbtn"
                  onClick={() => void setFavourite((loaded?.sha256 ?? suggestion?.sha256)!, false)}
                >
                  Make {loaded ? "this" : "it"} your favourite model
                </button>
              )}
            </div>
          )}
        </div>
      </section>

      {working.length > 0 && (
        <section className="gw-card gw-home-working" aria-live="polite">
          <div className="gw-home-card-head">
            <h2>Working now</h2>
            <span className="muted gw-home-count">{working.length === 1 ? "1 session" : `${working.length} sessions`}</span>
          </div>
          <ul className="gw-home-work">
            {working.slice(0, MAX_WORKING).map((r) => (
              <WorkRow key={r.id} row={r} onOpen={() => onOpenSession(r.id)} />
            ))}
          </ul>
          {working.length > MAX_WORKING && (
            <button type="button" className="gw-linkbtn gw-home-more" onClick={() => go("agent")}>
              {working.length - MAX_WORKING} more working: open the Agent dashboard
            </button>
          )}
        </section>
      )}

      <div className="gw-home-grid">
        <section className="gw-card gw-home-waiting">
          <h2>Waiting for you</h2>
          {waiting.length === 0 && approvals === 0 ? (
            <p className="muted gw-home-quiet">Nothing needs you right now.</p>
          ) : (
            <ul className="gw-home-list">
              {approvals > 0 && waiting.every((w) => w.status !== "approval") && (
                <li>
                  <Pill tone="warn">Approval</Pill>
                  <span className="gw-home-list-title">{approvals === 1 ? "1 request is waiting" : `${approvals} requests are waiting`}</span>
                  <Button size="sm" onClick={() => go("agent")}>Open</Button>
                </li>
              )}
              {waiting.slice(0, MAX_WAITING).map((r) => (
                <li key={r.id}>
                  <Pill tone={STATUS[r.status].tone}>{STATUS[r.status].text}</Pill>
                  <span className="gw-home-list-title" title={r.step ? `${r.title}: ${r.step}` : r.title}>
                    {r.title}
                    {r.status === "needsYou" && r.step && <span className="muted"> · {r.step}</span>}
                  </span>
                  <Button size="sm" onClick={() => onOpenSession(r.id)}>Open</Button>
                  {r.status === "finished" && (
                    <IconButton
                      icon={X}
                      label="Dismiss: you've seen it"
                      onClick={() => {
                        markSeen(r.id);
                        setSeenTick((n) => n + 1);
                      }}
                    />
                  )}
                </li>
              ))}
              {waiting.length > MAX_WAITING && (
                <li className="gw-home-more">
                  <button type="button" className="gw-linkbtn" onClick={() => go("agent")}>
                    {waiting.length - MAX_WAITING} more in Agent
                  </button>
                </li>
              )}
            </ul>
          )}
        </section>

        <section className="gw-card gw-home-memory">
          <h2>Memory</h2>
          {gpu ? (
            <div className="gw-home-bar">
              <div className="gw-home-bar-head">
                <span>{gpu.adapter}</span>
                <span className="muted">
                  {gb(gpu.usedBytes ?? 0)} of {gb(gpu.totalBytes)}
                </span>
              </div>
              <SegmentedBar label="GPU memory" total={gpu.totalBytes} height={10} legend format={gb} segments={gpuSegments} />
              {gpu.apps.length > 0 && (
                <div className="gw-home-apps muted">
                  Other apps: {gpu.apps.map((a) => `${a.name.replace(/\.exe$/i, "")} ${gb(a.bytes)}`).join(", ")}
                </div>
              )}
            </div>
          ) : (
            <p className="muted gw-home-quiet">GPU memory can't be read on this computer.</p>
          )}
          {memory?.ramTotalBytes && memory.ramAvailableBytes !== null && (
            <div className="gw-home-bar">
              <div className="gw-home-bar-head">
                <span>System RAM</span>
                <span className="muted">
                  {gb(memory.ramTotalBytes - memory.ramAvailableBytes)} of {gb(memory.ramTotalBytes)}
                </span>
              </div>
              <SegmentedBar
                label="System RAM"
                total={memory.ramTotalBytes}
                height={10}
                legend
                format={gb}
                segments={[{ label: "In use", value: memory.ramTotalBytes - memory.ramAvailableBytes, color: "#6b7a72" }]}
              />
            </div>
          )}
          {gpu && (
            <p className="muted gw-home-note">
              The safety margin keeps 3 GB free so the model isn't slowed down by the driver moving memory around.
            </p>
          )}
        </section>

        <section className="gw-card gw-home-sessions">
          <div className="gw-home-card-head">
            <h2>Sessions</h2>
            <Button size="sm" icon={FolderOpen} onClick={() => go("agent")}>New session</Button>
          </div>
          {listed.length === 0 && savedOnly.length === 0 ? (
            <EmptyState
              art={<Growth seed="first-session" width={120} height={72} roots={1} depth={4} />}
              title="Start your first session"
              body="Pick a folder and tell the agent what to do. It can only touch what you allow."
            />
          ) : (
            <ul className="gw-home-list">
              {listed.slice(0, maxSessions).map((r) => (
                <li key={r.id}>
                  <span className="gw-home-status" data-status={r.status} aria-hidden="true" />
                  <span className="gw-home-list-title">
                    {r.title}
                    <span className="muted"> · {r.step || STATUS[r.status].text}</span>
                  </span>
                  <Button size="sm" variant="ghost" onClick={() => onOpenSession(r.id)}>Open</Button>
                </li>
              ))}
              {savedOnly
                .slice()
                .sort((a, b) => b.updatedAt - a.updatedAt)
                .slice(0, Math.max(0, maxSessions - listed.length))
                .map((t) => (
                  <li key={t.id}>
                    <span className="gw-home-status" data-status="saved" aria-hidden="true" />
                    <span className="gw-home-list-title">
                      {t.title}
                      <span className="muted"> · {ago(t.updatedAt * 1000)}</span>
                    </span>
                    <Button size="sm" variant="ghost" onClick={() => onOpenSaved(t.id)}>Open</Button>
                  </li>
                ))}
              {listed.length + savedOnly.length > maxSessions && (
                <li className="gw-home-more">
                  <button type="button" className="gw-linkbtn" onClick={() => go("agent")}>
                    All sessions in Agent
                  </button>
                </li>
              )}
            </ul>
          )}
        </section>

        <section className="gw-card gw-home-status-card">
          <div className="gw-home-card-head">
            <h2>System</h2>
            <Button size="sm" variant="ghost" icon={RefreshCw} onClick={() => go("updates")}>Updates</Button>
          </div>
          <ul className="gw-home-facts">
            <li>
              <Cpu size={15} aria-hidden="true" />
              <span>Runtime</span>
              <span className="muted">
                {installedRuntimes.length > 0 ? installedRuntimes.map((r) => r.id).join(", ") : "None installed"}
              </span>
            </li>
            <li>
              <ShieldCheck size={15} aria-hidden="true" />
              <span>Sandbox</span>
              <span className="muted">{state.sandboxAvailable ? "Available" : "Not available on this system"}</span>
            </li>
            <li>
              <RefreshCw size={15} aria-hidden="true" />
              <span>Updates</span>
              <span className={report?.security ? "danger" : report && report.available > 0 ? "warn" : "muted"}>
                {!report
                  ? "Not checked yet"
                  : report.security
                    ? "A security fix is available"
                    : report.available > 0
                      ? `${report.available} available`
                      : `Up to date · checked ${ago(report.checkedAtMs)}`}
              </span>
            </li>
            {report?.gpuDriver && (
              <li>
                <ShieldAlert size={15} aria-hidden="true" />
                <span>Graphics driver</span>
                <span className="warn">Older than the latest security fix</span>
              </li>
            )}
            {state.lockdown ? (
              <li>
                <ShieldAlert size={15} aria-hidden="true" />
                <span>Lockdown mode</span>
                <span className="warn">On</span>
              </li>
            ) : (
              state.offline && (
                <li>
                  <ShieldAlert size={15} aria-hidden="true" />
                  <span>Offline mode</span>
                  <span className="muted">On</span>
                </li>
              )
            )}
          </ul>
        </section>
      </div>
    </div>
  );
}

function duration(ms: number | null): string | null {
  if (ms === null || ms < 0) return null;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ${s % 60} s`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

/** One running session on Home: what it's doing now, how far it is, and its latest steps. */
function WorkRow({ row: r, onOpen }: { row: DashboardRow; onOpen: () => void }) {
  const facts = [
    r.steps > 0 ? (r.steps === 1 ? "1 step" : `${r.steps} steps`) : null,
    r.todosTotal > 0 ? `${r.todosDone} of ${r.todosTotal} tasks` : null,
    duration(r.elapsedMs),
    r.contextTokens && r.contextWindow ? `context ${Math.round((r.contextTokens / r.contextWindow) * 100)} %` : null,
  ].filter(Boolean);
  return (
    <li className="gw-home-work-row" data-status={r.status}>
      <span className="gw-home-status" data-status={r.status} aria-hidden="true" />
      <div className="gw-home-work-main">
        <div className="gw-home-work-title">
          <button type="button" className="gw-home-work-name" onClick={onOpen} title={r.title}>
            {r.title}
          </button>
          <Pill tone={STATUS[r.status].tone}>{STATUS[r.status].text}</Pill>
        </div>
        <div className="gw-home-work-step">{r.step || STATUS[r.status].text}</div>
        {facts.length > 0 && <div className="gw-home-work-facts">{facts.join(" · ")}</div>}
        {r.background.length > 0 && (
          <div className="gw-home-work-facts gw-left-running" title={r.background.join("\n")}>
            Left running in the sandbox: {programNames(r.background)}
          </div>
        )}
        {r.todosTotal > 0 && (
          <div className="gw-home-work-progress" role="progressbar" aria-valuemin={0} aria-valuemax={r.todosTotal} aria-valuenow={r.todosDone} aria-label="Tasks done">
            <span style={{ width: `${(r.todosDone / r.todosTotal) * 100}%` }} />
          </div>
        )}
        {r.recent.length > 0 && (
          <ol className="gw-home-work-recent" aria-label="Latest steps">
            {r.recent.map((t, i) => (
              <li key={i} className={i === r.recent.length - 1 && r.status === "working" ? "now" : undefined}>{t}</li>
            ))}
          </ol>
        )}
      </div>
      <Button size="sm" onClick={onOpen}>Open</Button>
    </li>
  );
}
