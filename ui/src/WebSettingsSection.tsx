// Settings → Web access (ADR-018): the search provider, guided SearXNG
// setup, the Brave key (kept wrapped by Windows, never shown again), a
// connection test, and how research sessions and Web-switch chats start.
import { useEffect, useState } from "react";
import { Fold, SettingGroup, SettingRow, Toggle } from "./ui";
import { api, AppView, ConnectionTest, errorText, SearchProviderKind, WebMode, WebStatus } from "./api";
import { MODE_LABEL } from "./WebResearch";

const DOCKER_COMMAND =
  'docker run -d --name searxng --restart unless-stopped -p 127.0.0.1:8888:8080 -v "${PWD}/searxng:/etc/searxng" docker.io/searxng/searxng:latest';

const SETTINGS_LINES = `search:
  formats:
    - html
    - json`;

function Copy({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="link"
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        });
      }}
    >
      {done ? "Copied" : "Copy"}
    </button>
  );
}

function SearxngSetup() {
  return (
    <details className="setup-guide">
      <summary>Set up SearXNG on this computer</summary>
      <ol className="small">
        <li>
          Install Docker Desktop, then run this in PowerShell from a folder where SearXNG may keep its settings. It
          publishes SearXNG on <span className="mono">127.0.0.1</span> only, so nothing else on your network can use it.
          <pre className="mono">{DOCKER_COMMAND}</pre>
          <Copy text={DOCKER_COMMAND} />
        </li>
        <li>
          Gardenwall reads results as JSON, which SearXNG turns off by default. Open{" "}
          <span className="mono">searxng\settings.yml</span> in that folder and make its <span className="mono">search:</span>{" "}
          section list both formats (without this, SearXNG answers 403 and the test below says so):
          <pre className="mono">{SETTINGS_LINES}</pre>
          <Copy text={SETTINGS_LINES} />
        </li>
        <li>
          Restart it (<span className="mono">docker restart searxng</span>), then use <strong>Find on this computer</strong>{" "}
          and <strong>Test connection</strong> below.
        </li>
      </ol>
      <p className="small muted">
        Privacy: SearXNG doesn't keep a profile of you, but it still forwards each query to the search engines it uses,
        which see the query (not your IP address).
      </p>
    </details>
  );
}

export function WebSettingsSection({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const [status, setStatus] = useState<WebStatus | null>(null);
  const [url, setUrl] = useState(state.searxngUrl ?? "");
  const [key, setKey] = useState("");
  const [test, setTest] = useState<ConnectionTest | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.webStatus().then(setStatus, (e) => setError(errorText(e)));
  }, []);

  const run = async (label: string, f: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    setNote(null);
    try {
      await f();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  if (!status) {
    return (
      <section>
        <h2>Web access</h2>
        {error ? <div className="banner error">{error}</div> : <p className="small muted">Loading…</p>}
      </section>
    );
  }
  const s = status.settings;
  const saveSettings = (next: Partial<typeof s>) =>
    run("settings", async () => {
      setStatus(await api.setWebSettings({ ...s, ...next }));
      setTest(null);
    });

  return (
    <section>
      <h2>Web access</h2>
      <Fold summary="Research sessions, chats with the Web switch on, and agent sessions can search the web and read public pages.">
        Pages are read as plain text by a sandboxed reader with no network; nothing on a page runs. Offline mode turns
        all web access off.
      </Fold>
      {status.offline && <div className="banner info">Offline mode is on, so nothing uses the web.</div>}

      <SettingGroup title="Search">
        <SettingRow label="Search with" help="Your own search service: Gardenwall has no search of its own.">
          <select
            id="provider"
            aria-label="Search with"
            value={s.provider}
            onChange={(e) => void saveSettings({ provider: e.target.value as SearchProviderKind })}
          >
            <option value="searxng">SearXNG (your own, usually on this computer)</option>
            <option value="brave">Brave Search API (your own key)</option>
          </select>
        </SettingRow>
        {s.provider === "searxng" && (
          <div className="gw-row gw-row-stack">
            <div className="gw-row-text">
              <label className="gw-row-label" htmlFor="searxng-url">
                SearXNG address
              </label>
              <div className="gw-row-help">A SearXNG on another computer must use https://. Find only looks on this computer.</div>
              <div className="gw-inline-controls">
                <input id="searxng-url" value={url} placeholder="http://127.0.0.1:8888" onChange={(e) => setUrl(e.target.value)} />
                <button
                  disabled={busy !== null}
                  onClick={() =>
                    void run("url", async () => {
                      await api.setSearxngUrl(url.trim() || null);
                      await refresh();
                      setNote(url.trim() ? "Saved. Applies to sessions and chats started from now on." : "Search turned off.");
                    })
                  }
                >
                  Save
                </button>
                <button
                  disabled={busy !== null}
                  onClick={() =>
                    void run("find", async () => {
                      const found = await api.webFindSearxng();
                      if (found) {
                        setUrl(found);
                        setNote(`Found SearXNG at ${found}. Save it to use it.`);
                      } else {
                        setNote("No SearXNG answered on this computer's usual ports (8888, 8080, 8081, 4000).");
                      }
                    })
                  }
                >
                  {busy === "find" ? "Looking…" : "Find on this computer"}
                </button>
              </div>
              <SearxngSetup />
            </div>
          </div>
        )}
        {s.provider === "brave" && (
          <div className="gw-row gw-row-stack">
            <div className="gw-row-text">
              <label className="gw-row-label" htmlFor="brave-key">
                Brave Search API key
              </label>
              {!status.keysSupported && (
                <div className="banner info">Keys can only be stored on Windows for now (wrapped by your Windows account).</div>
              )}
              <div className="gw-row-help">
                <Fold summary="Get a key at brave.com/search/api (the free plan includes a monthly allowance). Brave sees your queries and your IP address." more="How the key is kept">
                  The key is wrapped by your Windows account and stays in Gardenwall's core: it is never shown again, sent
                  to the model, logged, or put in an address.
                </Fold>
              </div>
              <div className="gw-inline-controls">
                <input
                  id="brave-key"
                  type="password"
                  autoComplete="off"
                  value={key}
                  placeholder={status.braveKeySaved ? "A key is saved (enter a new one to replace it)" : "Brave Search API key"}
                  onChange={(e) => setKey(e.target.value)}
                />
                <button
                  disabled={busy !== null || !key.trim() || !status.keysSupported}
                  onClick={() =>
                    void run("key", async () => {
                      setStatus(await api.setBraveKey(key));
                      setKey("");
                      setNote("Key saved.");
                    })
                  }
                >
                  Save key
                </button>
                {status.braveKeySaved && (
                  <button
                    className="link danger"
                    disabled={busy !== null}
                    onClick={() =>
                      void run("key", async () => {
                        setStatus(await api.setBraveKey(null));
                        setNote("Key removed.");
                      })
                    }
                  >
                    Remove key
                  </button>
                )}
              </div>
            </div>
          </div>
        )}
        <SettingRow label="Test the connection" help='Sends one fixed query ("open source software") to check the setup.'>
          <button disabled={busy !== null || status.offline} onClick={() => void run("test", async () => setTest(await api.webTestConnection()))}>
            {busy === "test" ? "Testing…" : "Test connection"}
          </button>
        </SettingRow>
      </SettingGroup>
      {test &&
        (test.ok ? (
          <div className="banner info">
            {test.provider} works: {test.results} results in {test.millis} ms
            {test.sample && <> (first: “{test.sample}”)</>}.
          </div>
        ) : (
          <div className="banner error">
            {test.provider}: {test.error}
          </div>
        ))}
      {note && <div className="small">{note}</div>}
      {error && <div className="banner error">{error}</div>}

      <SettingGroup title="Research sessions and the chat Web switch">
        <SettingRow label="Start in" help="Preselected when one starts; you choose each time.">
          <select
            id="default-mode"
            aria-label="Start in"
            value={s.defaultMode}
            onChange={(e) => void saveSettings({ defaultMode: e.target.value as WebMode })}
          >
            {(["ask", "research"] as WebMode[]).map((m) => (
              <option key={m} value={m}>
                {MODE_LABEL[m]}
              </option>
            ))}
          </select>
        </SettingRow>
        <SettingRow label="Limits per session" help="Most searches and pages one session or chat may use.">
          <input
            id="max-searches"
            type="number"
            min={1}
            max={500}
            className="narrow"
            aria-label="Searches per session"
            defaultValue={s.maxSearches}
            onBlur={(e) => {
              const n = Number(e.target.value);
              if (Number.isInteger(n) && n !== s.maxSearches) void saveSettings({ maxSearches: n });
            }}
          />
          <span className="small muted">searches</span>
          <input
            type="number"
            min={1}
            max={1000}
            className="narrow"
            defaultValue={s.maxPages}
            aria-label="Pages per session"
            onBlur={(e) => {
              const n = Number(e.target.value);
              if (Number.isInteger(n) && n !== s.maxPages) void saveSettings({ maxPages: n });
            }}
          />
          <span className="small muted">pages</span>
        </SettingRow>
      </SettingGroup>

      <SettingGroup title="Research in agent sessions with folders">
        <SettingRow
          label="Review the findings before the agent gets them (recommended)"
          help={
            <Fold
              summary={
                s.reviewFindings
                  ? "Findings you send count as your own message, so later edits and commands don't ask again because of the web."
                  : "Findings go straight to the agent as web content, so every later edit and command in that session asks first."
              }
              more="How research works"
            >
              An agent working on your project reads the web only through a research assistant that has no access to the
              project. You approve each research question first; the assistant then searches and reads public pages
              without asking for each one.
            </Fold>
          }
        >
          <Toggle
            label="Review the findings before the agent gets them"
            checked={s.reviewFindings}
            onChange={(v) => void saveSettings({ reviewFindings: v })}
          />
        </SettingRow>
      </SettingGroup>
    </section>
  );
}
