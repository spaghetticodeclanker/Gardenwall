// MCP servers the agent can use. Each runs in its own sandbox with no
// network, only while an agent session is open, and every tool call asks
// first (or is allowed for one session). Programs and folders are chosen in
// the trusted core's native pickers.
//
// Layout (docs/internal/design/MCP-SETTINGS-REDESIGN.md): one grouped row per server
// that opens in place, one at a time; arguments and variables use the
// Settings "unsaved" bar; right-click a row to rename or remove it.
import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Pencil, Play, Plus, Trash2 } from "lucide-react";
import { Button, Fold, Menu, Pill, RenameField, Toggle, useContextMenu } from "./ui";
import { api, errorText, McpServerView, McpToolView } from "./api";
import { useUnsaved } from "./unsavedSettings";
import { shownPath } from "./paths";

type Draft = { args?: string; env?: string };

const argsText = (s: McpServerView) => s.args.join("\n");
const envText = (s: McpServerView) => (s.env ?? []).map((v) => `${v.name}=${v.value}`).join("\n");

/** NAME=value lines; a line without "=" is refused by the core as an invalid name. */
function parseEnv(text: string) {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => {
      const at = l.indexOf("=");
      return at < 0 ? { name: l, value: "" } : { name: l.slice(0, at).trim(), value: l.slice(at + 1) };
    });
}

function StatusPill({ s }: { s: McpServerView }) {
  const st = s.toolStatus;
  if (!st) return <Pill tone="neutral">Not tested</Pill>;
  if (st.held > 0) return <Pill tone="warn">Needs review</Pill>;
  return <Pill tone="ok">{st.tools === 1 ? "1 tool" : `${st.tools} tools`}</Pill>;
}

const REVIEW_LABEL: Record<McpToolView["review"], string> = {
  accepted: "Accepted",
  new: "New",
  changed: "Changed",
  flagged: "Reads like instructions",
};

export function McpSettings() {
  const [servers, setServers] = useState<McpServerView[] | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const [tools, setTools] = useState<Record<string, McpToolView[]>>({});
  const [testing, setTesting] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [pageError, setPageError] = useState<string | null>(null);
  const context = useContextMenu();

  useEffect(() => {
    api.listMcpServers().then(setServers).catch((e) => setPageError(errorText(e)));
  }, []);

  const setError = (id: string, e: string | null) =>
    setErrors((m) => {
      const next = { ...m };
      if (e) next[id] = e;
      else delete next[id];
      return next;
    });

  /** Run a core command for one server; its error shows in that server's row. */
  const act = async (id: string, f: () => Promise<McpServerView[]>) => {
    setError(id, null);
    try {
      setServers(await f());
      return true;
    } catch (e) {
      setError(id, errorText(e));
      return false;
    }
  };

  const setDraft = (s: McpServerView, field: keyof Draft, value: string) =>
    setDrafts((d) => {
      const saved = field === "args" ? argsText(s) : envText(s);
      const next: Draft = { ...d[s.id], [field]: value === saved ? undefined : value };
      const all = { ...d };
      if (next.args === undefined && next.env === undefined) delete all[s.id];
      else all[s.id] = next;
      return all;
    });

  const dirty = Object.keys(drafts).length > 0;
  useUnsaved("mcp", dirty, {
    label: "MCP servers",
    save: async () => {
      const failed: string[] = [];
      for (const [id, d] of Object.entries(drafts)) {
        if (d.args !== undefined) {
          const args = d.args.split("\n").map((a) => a.trim()).filter(Boolean);
          if (await act(id, () => api.setMcpArgs(id, args))) setDrafts((m) => ({ ...m, [id]: { ...m[id], args: undefined } }));
          else failed.push(id);
        }
        if (d.env !== undefined) {
          const env = parseEnv(d.env);
          if (await act(id, () => api.setMcpEnv(id, env))) setDrafts((m) => ({ ...m, [id]: { ...m[id], env: undefined } }));
          else failed.push(id);
        }
      }
      // Drop entries with nothing left unsaved.
      setDrafts((m) => Object.fromEntries(Object.entries(m).filter(([, d]) => d.args !== undefined || d.env !== undefined)));
      // Show the server whose change was refused, with the core's reason.
      if (failed.length) setOpen(failed[0] ?? null);
      return failed.length === 0;
    },
    discard: () => setDrafts({}),
  });

  const add = async () => {
    setPageError(null);
    const before = new Set((servers ?? []).map((s) => s.id));
    try {
      const next = await api.addMcpServer("");
      setServers(next);
      const added = next.find((s) => !before.has(s.id));
      if (added) setOpen(added.id);
    } catch (e) {
      setPageError(errorText(e));
    }
  };

  const remove = (s: McpServerView) => {
    if (!window.confirm(`Remove the MCP server "${s.name}"? Its settings and private folder are deleted.`)) return;
    void act(s.id, () => api.removeMcpServer(s.id)).then((ok) => {
      if (!ok) return;
      setDrafts((d) => {
        const next = { ...d };
        delete next[s.id];
        return next;
      });
      if (open === s.id) setOpen(null);
    });
  };

  const test = async (s: McpServerView) => {
    setTesting(s.id);
    setError(s.id, null);
    try {
      const list = await api.testMcpServer(s.id);
      setTools((t) => ({ ...t, [s.id]: list }));
      // The core updated the row's counts.
      setServers(await api.listMcpServers());
    } catch (e) {
      setError(s.id, errorText(e));
    } finally {
      setTesting(null);
    }
  };

  const accept = async (s: McpServerView) => {
    if (await act(s.id, () => api.acceptMcpTools(s.id))) {
      setTools((t) => ({ ...t, [s.id]: (t[s.id] ?? []).map((x) => ({ ...x, review: "accepted" as const })) }));
    }
  };

  const menuFor = (s: McpServerView) => [
    { label: "Rename", icon: Pencil, onSelect: () => setRenaming(s.id) },
    "separator" as const,
    { label: "Remove server…", icon: Trash2, danger: true, onSelect: () => remove(s) },
  ];

  const unsupported = useMemo(() => servers?.some((s) => !s.supported) ?? false, [servers]);

  return (
    <section>
      <h2>MCP servers</h2>
      <Fold summary="Extra tools for the agent from MCP servers. Each runs sandboxed with no network, and every tool call asks you first." more="How to add one">
        <p>
          A server runs only while an agent session is open and can use only the folders you give it here. You can
          allow a tool for one session from its approval card.
        </p>
        <p>
          Choose the server's program: for a Node or Python server, node.exe or python.exe (add its folder under Agent
          tools) with the script path in the arguments, or the launcher an installed server came with (a .cmd file, such
          as the one npm install -g creates). Launcher arguments can't contain characters cmd.exe interprets (" &amp; |
          &lt; &gt; ^ % ! parentheses). Servers that need the internet, including ones that npx downloads, don't work yet.
        </p>
        <p>Right-click a server to rename or remove it.</p>
      </Fold>
      {unsupported && <div className="banner info">MCP servers need the OS sandbox, which isn't available on this platform yet.</div>}
      {pageError && <div className="banner error">{pageError}</div>}

      <div className="gw-group-body gw-mcp-list">
        {servers && servers.length === 0 && (
          <div className="gw-row">
            <div className="gw-row-text">
              <div className="gw-row-label">No MCP servers yet</div>
              <div className="gw-row-help">Add one to give the agent its tools. It starts switched off.</div>
            </div>
          </div>
        )}
        {servers?.map((s) => {
          const isOpen = open === s.id;
          const bodyId = `mcp-body-${s.id}`;
          const d = drafts[s.id] ?? {};
          const shown = tools[s.id];
          const waiting = shown?.filter((t) => t.review !== "accepted").length ?? 0;
          return (
            <div
              key={s.id}
              className={`gw-mcp-server${isOpen ? " open" : ""}${s.enabled ? "" : " off"}`}
              onContextMenu={(e) => context.open(e, `Server ${s.name}`, menuFor(s))}
            >
              <div className="gw-mcp-head">
                {renaming === s.id ? (
                  <div className="gw-mcp-rename">
                    <RenameField
                      label="Server name"
                      value={s.name}
                      onSave={(v) => {
                        setRenaming(null);
                        void act(s.id, () => api.renameMcpServer(s.id, v));
                      }}
                      onCancel={() => setRenaming(null)}
                    />
                  </div>
                ) : (
                  <button
                    type="button"
                    className="gw-mcp-toggle"
                    aria-expanded={isOpen}
                    aria-controls={bodyId}
                    title={`${s.name}\n${shownPath(s.program)}\nRight-click to rename or remove`}
                    onClick={() => setOpen(isOpen ? null : s.id)}
                    onKeyDown={(e) => context.open(e, `Server ${s.name}`, menuFor(s))}
                  >
                    {isOpen ? <ChevronDown aria-hidden="true" size={16} /> : <ChevronRight aria-hidden="true" size={16} />}
                    <span className="gw-mcp-name">
                      <span className="gw-mcp-title">
                        {s.name}
                        {s.id in drafts && <span className="gw-mcp-dot" title="Unsaved changes" aria-label="Unsaved changes" />}
                      </span>
                      <span className="gw-mcp-program mono">{shownPath(s.program)}</span>
                    </span>
                  </button>
                )}
                <StatusPill s={s} />
                <Toggle
                  checked={s.enabled}
                  label={`Use ${s.name} in new agent sessions`}
                  disabled={!s.supported}
                  onChange={(on) => void act(s.id, () => api.setMcpEnabled(s.id, on))}
                />
              </div>
              {!isOpen && errors[s.id] && <div className="banner error gw-mcp-error">{errors[s.id]}</div>}

              {isOpen && (
                <div className="gw-mcp-body" id={bodyId}>
                  <div className="gw-mcp-section">
                    <div className="gw-mcp-label">
                      <label htmlFor={`${bodyId}-args`}>Arguments</label>
                      <div className="gw-row-help">One per line</div>
                    </div>
                    <textarea
                      id={`${bodyId}-args`}
                      className="mono"
                      rows={Math.max(2, s.args.length + 1)}
                      spellCheck={false}
                      value={d.args ?? argsText(s)}
                      onChange={(e) => setDraft(s, "args", e.target.value)}
                    />
                  </div>

                  <div className="gw-mcp-section">
                    <div className="gw-mcp-label">
                      <label htmlFor={`${bodyId}-env`}>Environment variables</label>
                      <div className="gw-row-help">NAME=value, one per line</div>
                    </div>
                    <div>
                      <textarea
                        id={`${bodyId}-env`}
                        className="mono"
                        rows={Math.max(2, (s.env ?? []).length + 1)}
                        spellCheck={false}
                        value={d.env ?? envText(s)}
                        onChange={(e) => setDraft(s, "env", e.target.value)}
                      />
                      <Fold summary="Not encrypted, so no passwords or keys.">
                        They're stored in Gardenwall's settings file without encryption (the server has no network anyway).
                        PATH, TEMP, HOME and similar are set by the sandbox and can't be changed.
                      </Fold>
                    </div>
                  </div>

                  <div className="gw-mcp-section">
                    <div className="gw-mcp-label">
                      <div>Folders</div>
                      <div className="gw-row-help">Only these, plus its own private folder</div>
                    </div>
                    <div>
                      {s.folders.length === 0 && <div className="small muted gw-mcp-none">None yet.</div>}
                      {s.folders.map((f) => (
                        <div key={f.path} className="gw-mcp-folder">
                          <span className="mono gw-mcp-path" title={shownPath(f.path)}>
                            {shownPath(f.path)}
                          </span>
                          <Pill tone={f.write ? "warn" : "neutral"}>{f.write ? "Read and write" : "Read only"}</Pill>
                          <Button size="sm" variant="ghost" onClick={() => void act(s.id, () => api.removeMcpFolder(s.id, f.path))}>
                            Remove
                          </Button>
                        </div>
                      ))}
                      <Menu
                        label="Add a folder"
                        align="start"
                        trigger={(p) => (
                          <Button size="sm" icon={Plus} {...p}>
                            Add folder… <ChevronDown aria-hidden="true" size={14} />
                          </Button>
                        )}
                        items={[
                          { label: "Read only…", onSelect: () => void act(s.id, () => api.addMcpFolder(s.id, false)) },
                          { label: "Read and write…", onSelect: () => void act(s.id, () => api.addMcpFolder(s.id, true)) },
                        ]}
                      />
                    </div>
                  </div>

                  <div className="gw-mcp-section">
                    <div className="gw-mcp-label">
                      <div>Tools</div>
                      <div className="gw-row-help">
                        {shown ? "From the test just now" : s.toolStatus ? "Test to see the list" : "Not tested yet"}
                      </div>
                    </div>
                    <div>
                      <div className="gw-mcp-tools-head">
                        <span className="small muted">
                          {shown
                            ? `Started in its sandbox and offered ${shown.length} tool${shown.length === 1 ? "" : "s"}.`
                            : s.toolStatus
                              ? `${s.toolStatus.tools} tool${s.toolStatus.tools === 1 ? "" : "s"} at its last start${s.toolStatus.held ? `, ${s.toolStatus.held} waiting for your review` : ""}.`
                              : "Test starts it in its sandbox and lists its tools."}
                        </span>
                        <Button size="sm" icon={Play} disabled={testing === s.id || !s.supported} onClick={() => void test(s)}>
                          {testing === s.id ? "Testing…" : "Test"}
                        </Button>
                      </div>
                      {shown && shown.length > 0 && (
                        <table className="gw-mcp-tools">
                          <thead>
                            <tr>
                              <th scope="col">Tool</th>
                              <th scope="col">Description</th>
                              <th scope="col">Status</th>
                            </tr>
                          </thead>
                          <tbody>
                            {shown.map((t) => (
                              <tr key={t.name}>
                                <td className="mono">{t.name}</td>
                                <td className="muted">{t.description || "No description"}</td>
                                <td>
                                  <span className="gw-mcp-pills">
                                    <Pill tone={t.review === "accepted" ? "ok" : "warn"}>{REVIEW_LABEL[t.review]}</Pill>
                                    {t.suspicious && t.review !== "flagged" && <Pill tone="warn">Reads like instructions</Pill>}
                                  </span>
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      )}
                      {waiting > 0 && (
                        <div className="gw-mcp-review">
                          <Fold
                            summary={`${waiting} tool${waiting === 1 ? " waits" : "s wait"} for your review. The model doesn't see ${waiting === 1 ? "it" : "them"} until you accept.`}
                          >
                            Some tools are new, have changed since you accepted them, or have a description that reads like
                            instructions to the model. A tool's description is text the model reads, so a server can use it to
                            steer the model. Read each description above before accepting.
                          </Fold>
                          <Button size="sm" disabled={testing === s.id} onClick={() => void accept(s)}>
                            Accept these tools
                          </Button>
                        </div>
                      )}
                    </div>
                  </div>

                  {errors[s.id] && <div className="banner error gw-mcp-error">{errors[s.id]}</div>}

                  <div className="gw-mcp-footer">
                    <Button size="sm" variant="ghost" icon={Trash2} className="gw-danger-text" onClick={() => remove(s)}>
                      Remove server
                    </Button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
      <Button icon={Plus} onClick={() => void add()}>
        Add server…
      </Button>
      {context.node}
    </section>
  );
}
