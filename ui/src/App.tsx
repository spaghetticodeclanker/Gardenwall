import { markSeen } from "./seen";
import { DownloadsProvider } from "./downloads";
import { MemorySettings } from "./MemorySettings";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, AppView, errorText, UpdatesView as UpdatesState } from "./api";
import { AgentView, type Opening } from "./AgentView";
import { ApprovalDialog } from "./ApprovalDialog";
import { AttentionCards } from "./AttentionCards";
import { ChatView } from "./ChatView";
import { DocumentsView } from "./DocumentsView";
import { HelpView, PAGE_HELP, SETTINGS_HELP, type HelpOpen } from "./HelpView";
import { ModelsView } from "./ModelsView";
import { SettingsView } from "./SettingsView";
import { UpdatesView } from "./UpdatesView";
import { Lightbox } from "./Lightbox";
import { GettingStarted } from "./GettingStarted";
import { CommandPalette } from "./CommandPalette";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { LockdownDialog } from "./Lockdown";
import { SideChat } from "./SideChat";
import type { Insert } from "./useInsert";
import { GuideNav } from "./modelGuide";
import { hasUnsaved, LeaveSettingsDialog } from "./unsavedSettings";
import { usePoll } from "./usePoll";
import { PageHeader, TopBar, type Page, type Target } from "./shell/TopBar";
import { Home } from "./home/Home";
import { SetupWizard, setupDone } from "./setup/SetupWizard";
import { NoticeFold, ToastHost } from "./ui";
import { syncVoice } from "./speech";

/** How often the window picks up results of scheduled checks. */
const UPDATE_POLL_MS = 10 * 60 * 1000;

/** Pages that are workspaces (no back control); the rest sit behind the gear menu. */
const WORKSPACES: Page[] = ["home", "agent", "chat"];

export function App() {
  const [page, setPageNow] = useState<Page>("home");
  /** The workspace a gear page returns to. */
  const [backTo, setBackTo] = useState<Page>("home");
  useEffect(() => {
    if (WORKSPACES.includes(page)) setBackTo(page);
  }, [page]);
  /** A page the user asked for while Settings had unsaved changes. */
  const [leavingTo, setLeavingTo] = useState<Page | null>(null);
  const setPage = useCallback(
    (next: Page) => {
      setPageNow((current) => {
        if (current === "settings" && next !== "settings" && hasUnsaved()) {
          setLeavingTo(next);
          return current;
        }
        return next;
      });
    },
    [],
  );
  /** Bumped when a link to the runtimes opens Updates, so it scrolls to Inference runtime. */
  const [runtimeFocus, setRuntimeFocus] = useState(0);
  /** Go to a page, or to the runtimes on the Updates page (the old Runtime page). */
  const go = useCallback(
    (to: Target) => {
      if (to === "runtime") {
        setPage("updates");
        setRuntimeFocus((n) => n + 1);
      } else setPage(to);
    },
    [setPage],
  );
  /** An agent session a chat was handed off to, for the Agent page to open. */
  const [agentInbox, setAgentInbox] = useState<Opening | null>(null);
  /** An open agent session to show (a corner card was clicked). */
  const [agentFocus, setAgentFocus] = useState<string | null>(null);
  /** The open agent session on screen, if any. */
  const [agentShowing, setAgentShowing] = useState<string | null>(null);
  // The session on screen counts as seen (Home lists finished, unseen ones).
  const seenSid = page === "agent" ? agentShowing : null;
  usePoll(() => {
    if (seenSid && document.hasFocus()) markSeen(seenSid);
  }, 2000, seenSid !== null);
  // Leaving a session counts too, so one that finished while you watched isn't listed as unseen.
  useEffect(() => {
    if (!seenSid) return;
    markSeen(seenSid);
    return () => markSeen(seenSid);
  }, [seenSid]);
  const takeFocus = useCallback(() => setAgentFocus(null), []);
  const [state, setState] = useState<AppView | null>(null);
  // The core skips notifications for the session on screen.
  const showingNow = page === "agent" ? agentShowing : null;
  useEffect(() => {
    void api.setShowingSession(showingNow).catch(() => {});
  }, [showingNow]);
  // Read aloud follows Settings → Voice wherever replies finish.
  useEffect(() => {
    syncVoice(Boolean(state?.voiceSettings?.readAuto), state?.voiceSettings?.inputEngine !== "off");
  }, [state?.voiceSettings?.readAuto, state?.voiceSettings?.inputEngine]);
  /** The Ctrl+K palette, and what it asked a page to open. */
  const [palette, setPalette] = useState(false);
  /** The diagnostics report dialog (Help, Updates, Ctrl+K). */
  const [diagnostics, setDiagnostics] = useState(false);
  /** The lockdown dialog: turning it on (emergency stop) or off. */
  const [lockdownDialog, setLockdownDialog] = useState<"on" | "off" | null>(null);
  const openLockdown = () => setLockdownDialog(state?.lockdown ? "off" : "on");
  /** The text a side chat is about (null: closed). */
  /** The open side chat: the selected text, and the page it was selected on. */
  const [side, setSide] = useState<{ seed: string; from: "chat" | "agent" } | null>(null);
  /** A side chat's answer on its way to that page's message box. */
  const [insert, setInsert] = useState<(Insert & { to: "chat" | "agent" }) | null>(null);
  const [chatToOpen, setChatToOpen] = useState<string | null>(null);
  /** A hub repository for the Models page to open (from a research session). */
  const [repoToOpen, setRepoToOpen] = useState<string | null>(null);
  const openResearch = useCallback((opening: Opening) => {
    setAgentInbox(opening);
    setPage("agent");
  }, []);
  const lookAtRepo = useCallback((repo: string) => {
    setRepoToOpen(repo);
    setPage("models");
  }, []);
  /** "More in the guide": Help, scrolled to the section once it is shown. */
  const [helpGuide, setHelpGuide] = useState<HelpOpen | null>(null);
  /** A page's "?" beside its title: Help at that page's topic (final review item 12). */
  const openHelpTopic = useCallback((topic: string) => {
    setPage("help");
    setHelpGuide({ topic, seq: Date.now() });
  }, []);
  /** The Settings category on screen, for the "?" beside the Settings title. */
  const [settingsCategory, setSettingsCategory] = useState("generation");
  const openGuide = useCallback((id: string) => {
    setPage("help");
    // Help opens the topic holding this entry and unfolds it.
    setHelpGuide({ id, seq: Date.now() });
  }, []);
  // Leaving Help forgets the entry, so Help opened later starts at its home.
  useEffect(() => {
    if (page !== "help") setHelpGuide(null);
  }, [page]);
  const takeRepo = useCallback(() => setRepoToOpen(null), []);
  const [savedToOpen, setSavedToOpen] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [noticesSeen, setNoticesSeen] = useState(false);
  const [updates, setUpdates] = useState<UpdatesState | null>(null);
  const [securitySeen, setSecuritySeen] = useState<number | null>(null);
  // Focus mode hides the navigation and the chat list (remembered locally).
  const [focus, setFocus] = useState<boolean>(() => {
    try {
      return localStorage.getItem("gw.focus") === "1";
    } catch {
      return false;
    }
  });
  const toggleFocus = useCallback(() => {
    setFocus((f) => {
      try {
        localStorage.setItem("gw.focus", f ? "0" : "1");
      } catch {
        // Not remembered; still works for this session.
      }
      return !f;
    });
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "b") {
        e.preventDefault();
        toggleFocus();
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggleFocus]);

  /** Opens in Home until set up; with a model already loaded, straight into the Agent. */
  const opened = useRef(false);
  /** First-run setup: shown once while there's no model; re-opened from Help. */
  const [setup, setSetup] = useState(false);
  const refresh = useCallback(async () => {
    try {
      const next = await api.getState();
      setState(next);
      setError(null);
      if (!opened.current) {
        opened.current = true;
        if (!setupDone() && next.models.length === 0 && !next.loaded) setSetup(true);
        if (next.loaded) setPageNow((p) => (p === "home" ? "agent" : p));
        else if (next.loadOnLaunch && next.favouriteModel) {
          // The favourite model loads at start; Home stays if it fails, with the reason.
          const fav = next.models.find((m) => m.sha256 === next.favouriteModel);
          if (fav) {
            setPageNow((p) => (p === "home" ? "agent" : p));
            void api
              .loadModel(fav.sha256, fav.load)
              .then(async () => setState(await api.getState()))
              .catch((e) => {
                setError(`Your favourite model didn't load: ${errorText(e)}`);
                setPageNow((p) => (p === "agent" ? "home" : p));
              });
          }
        }
      }
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Start-up cleanup runs in the core after the window opens: follow it until done.
  usePoll(() => void refresh(), 1000, state?.cleaningUp === true);

  // Scheduled checks run in the trusted core; the window shows their results.
  usePoll(() => void api.getUpdates().then(setUpdates).catch(() => {}), UPDATE_POLL_MS);

  const report = updates?.report ?? null;
  const badge = report?.available ?? 0;
  const securityBanner = report?.security === true && securitySeen !== report.checkedAtMs;

  const loaded = state?.loaded ?? null;

  return (
    <GuideNav.Provider value={openGuide}>
    <ToastHost>
      <DownloadsProvider>
    <div className={focus ? "shell focus" : "shell"}>
      <Lightbox />
      {side !== null && (
        <SideChat
          key={side.seed}
          seed={side.seed}
          loaded={loaded}
          onClose={() => setSide(null)}
          onUse={(text) => {
            setInsert({ to: side.from, text, n: Date.now() });
            setPage(side.from);
            setSide(null);
          }}
        />
      )}
      {diagnostics && <DiagnosticsDialog onClose={() => setDiagnostics(false)} />}
      {lockdownDialog && <LockdownDialog mode={lockdownDialog} onClose={() => setLockdownDialog(null)} onChanged={() => void refresh()} />}
      {palette && (
        <CommandPalette
          pages={["home", "agent", "chat", "documents", "memory", "models", "runtime", "updates", "settings", "help"]}
          onClose={() => setPalette(false)}
          onPick={(t) => {
            setPalette(false);
            if (t.kind === "page") go(t.page as Target);
            else if (t.kind === "action") {
              if (t.action === "lockdown") openLockdown();
              else setDiagnostics(true);
            }
            else if (t.kind === "chat") {
              setChatToOpen(t.id);
              setPage("chat");
            } else {
              setSavedToOpen(t.id);
              setPage("agent");
            }
          }}
        />
      )}
      <TopBar
        page={page}
        go={go}
        state={state}
        onUnload={async () => {
          await api.unloadModel();
          await refresh();
        }}
        updatesBadge={badge}
        updatesSecurity={report?.security === true}
        focus={focus}
        onToggleFocus={toggleFocus}
        onLockdown={openLockdown}
      />
      <main className="content">
        {!WORKSPACES.includes(page) && <PageHeader
            page={page}
            backTo={backTo}
            onBack={() => setPage(backTo)}
            helpTopic={page === "settings" ? SETTINGS_HELP[settingsCategory] : PAGE_HELP[page]}
            onHelp={openHelpTopic}
          />}
        {/* Two or more notices fold into one line (final review item 8). */}
        <NoticeFold>
          {error && <div className="banner error">{error}</div>}
          {securityBanner && report && (
            <div className="banner error" role="alert">
              <strong>The runtime in use has a known security problem.</strong> A fixed build is on the Updates page.{" "}
              <button className="link" onClick={() => setPage("updates")}>
                Open Updates
              </button>{" "}
              ·{" "}
              <button className="link" onClick={() => setSecuritySeen(report.checkedAtMs)}>
                Remind me later
              </button>
            </div>
          )}
          {state && !noticesSeen && state.notices.length > 0 && (
            <div className="banner info dismissible" role="status">
              <div>
                {state.notices.map((n) => (
                  <div key={n}>{n}</div>
                ))}
              </div>
              <button className="small" onClick={() => setNoticesSeen(true)}>
                Dismiss
              </button>
            </div>
          )}
          {/* Only where the first steps happen; not on Settings, Help and the like. */}
          {state && ["home", "chat", "agent", "models", "updates"].includes(page) && <GettingStarted state={state} go={go} />}
        </NoticeFold>
        {/* Chat and Agent stay mounted while another page is shown, so a
            reply or an agent task keeps streaming and nothing is lost. */}
        {state && (
          <div className="page-keep" hidden={page !== "chat"}>
            <ChatView
              loaded={loaded}
              goToModels={() => setPage("models")}
              state={state}
              refresh={refresh}
              focus={focus}
              active={page === "chat"}
              openId={chatToOpen}
              onAsk={(seed) => setSide({ seed, from: "chat" })}
              insert={insert?.to === "chat" ? insert : null}
              onOpened={() => setChatToOpen(null)}
              onAgentHandoff={(started) => {
                if (!started.agent) return;
                setAgentInbox({ view: started.agent, entries: [], autoSend: started.message });
                setPage("agent");
              }}
            />
          </div>
        )}
        {state && (
          <div className="page-keep" hidden={page !== "agent"}>
            <AgentView
              loaded={loaded}
              focus={focus}
              goToModels={() => setPage("models")}
              state={state}
              refresh={refresh}
              active={page === "agent"}
              openSaved={savedToOpen}
              onSavedTaken={() => setSavedToOpen(null)}
              inbox={agentInbox}
              onInboxTaken={() => setAgentInbox(null)}
              focusSid={agentFocus}
              onFocusTaken={takeFocus}
              onShowing={setAgentShowing}
              onAsk={(seed) => setSide({ seed, from: "agent" })}
              insert={insert?.to === "agent" ? insert : null}
              onLookAtRepo={lookAtRepo}
            />
          </div>
        )}
        {state && page === "home" && (
          <Home
            state={state}
            refresh={refresh}
            go={setPage}
            updates={updates}
            onOpenSession={(sid) => {
              setPage("agent");
              setAgentFocus(sid);
            }}
            onOpenSaved={(id) => {
              setSavedToOpen(id);
              setPage("agent");
            }}
          />
        )}
        {state && page === "documents" && <DocumentsView state={state} />}
        {page === "memory" && (
          <div className="page">
            <div className="memory-page">
              <MemorySettings />
            </div>
          </div>
        )}
        {state && page === "models" && (
          <ModelsView
            state={state}
            refresh={refresh}
            go={setPage}
            onResearch={openResearch}
            openRepo={repoToOpen}
            onRepoTaken={takeRepo}
          />
        )}
        {state && page === "updates" && <UpdatesView state={state} refresh={refresh} onChanged={setUpdates} focusRuntime={runtimeFocus} onDiagnostics={() => setDiagnostics(true)} />}
        {state && page === "settings" && <SettingsView state={state} refresh={refresh} onCategory={setSettingsCategory} />}
        {page === "help" && <HelpView onSetup={() => setSetup(true)} guide={helpGuide} onDiagnostics={() => setDiagnostics(true)} />}
      </main>
      {leavingTo && (
        <LeaveSettingsDialog
          onLeave={() => {
            setPageNow(leavingTo);
            setLeavingTo(null);
          }}
          onStay={() => setLeavingTo(null)}
        />
      )}
      {setup && state && <SetupWizard state={state} refresh={refresh} onClose={() => { setSetup(false); setPage("home"); }} />}
      <ApprovalDialog dockedSid={page === "agent" ? agentShowing : null} />
      <AttentionCards
        showing={page === "agent" ? agentShowing : null}
        onOpen={(sid) => {
          setPage("agent");
          setAgentFocus(sid);
        }}
      />
    </div>
      </DownloadsProvider>
    </ToastHost>
    </GuideNav.Provider>
  );
}
