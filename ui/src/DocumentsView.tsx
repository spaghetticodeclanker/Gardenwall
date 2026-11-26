// Document chat: collections of the user's files, indexed on this computer.
// Files are chosen in the native picker; PDF, DOCX and HTML are read by the
// sandboxed reader. Text is stored encrypted. Search is keyword-based, plus
// meaning-based when an embedding model is chosen.
//
// Redesign phase 6: each collection is a card
// with an aligned files table; selecting a file shows the text Gardenwall
// extracted from it (OCR page markers included) in the details panel.
import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { FilePlus2, FolderPlus, MoreHorizontal, Pencil, RefreshCw, ScanSearch, Trash2 } from "lucide-react";
import { AddReport, api, AppView, CollectionsView, DocProgress, errorText, FileSummary, formatBytes, SourceStatus } from "./api";
import { Button, CopyButton, DataTable, EmptyState, Fold, IconButton, Inspector, Menu, Pill, SegmentedBar } from "./ui";
import "./documents.css";

/** How a file compares with its saved location, in plain words. */
const STATUS: Record<SourceStatus, { label: string; hint: string; tone: "warn" | "neutral" } | null> = {
  unchanged: null,
  changed: {
    label: "Changed since it was added",
    hint: "The file on disk is different from what was indexed. Re-index to search the new version.",
    tone: "warn",
  },
  missing: {
    label: "Not found",
    hint: "The file isn't at its saved location any more (moved, renamed or deleted). Its passages are still searchable; remove it, or add it again from its new place.",
    tone: "warn",
  },
  unreadable: {
    label: "Couldn't be checked",
    hint: "The file exists but couldn't be read (it may be open in another app, or permissions changed).",
    tone: "neutral",
  },
  notSaved: {
    label: "Can't be checked",
    hint: "This file was added before Gardenwall saved file locations. Remove it and add it again to check it for changes.",
    tone: "neutral",
  },
};

/** The file whose text is shown, and its collection. */
interface Selected {
  collection: string;
  collectionName: string;
  file: FileSummary;
}

function Collection({
  id,
  name,
  files,
  chunks,
  embedded,
  embeddingModel,
  onChanged,
  selected,
  onSelect,
}: {
  id: string;
  name: string;
  files: number;
  chunks: number;
  embedded: number;
  embeddingModel: string | null;
  onChanged: (v: CollectionsView) => void;
  selected: Selected | null;
  onSelect: (s: Selected | null) => void;
}) {
  const [list, setList] = useState<FileSummary[] | null>(null);
  const [progress, setProgress] = useState<DocProgress | null>(null);
  const [report, setReport] = useState<AddReport | null>(null);
  const [reindexed, setReindexed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api.collectionFiles(id).then(setList).catch((e) => setError(errorText(e)));
  }, [id]);
  useEffect(() => load(), [load]);

  const refresh = async () => {
    onChanged(await api.listCollections());
    load();
  };

  const changed = list?.filter((f) => f.status === "changed") ?? [];
  const reindex = async (ids: number[]) => {
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      setReport(await api.reindexDocuments(id, ids, setProgress));
      setReindexed(true);
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };
  const add = async () => {
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      setReport(await api.addDocuments(id, setProgress));
      setReindexed(false);
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  return (
    <section className="gw-collection">
      <div className="gw-collection-head">
        <div className="gw-collection-title">
          <h2 className="gw-collection-name">{name}</h2>
          <span className="gw-collection-meta">
          {files} file{files === 1 ? "" : "s"} · {chunks.toLocaleString()} passages
          {embeddingModel && chunks > 0 && ` · ${embedded === chunks ? "indexed for meaning search" : `${embedded.toLocaleString()} indexed`}`}
          </span>
        </div>
        <div className="gw-collection-actions">
          {changed.length > 0 && (
            <Button
              size="sm"
              variant="primary"
              icon={RefreshCw}
              disabled={busy}
              title="Reads the changed files again from where you added them, and replaces their passages"
              onClick={() => void reindex(changed.map((f) => f.id))}
            >
              Re-index {changed.length} changed
            </Button>
          )}
          <Button size="sm" icon={FilePlus2} disabled={busy} onClick={() => void add()}>
            Add files…
          </Button>
          <Menu
            label={`More for ${name}`}
            trigger={(p) => <IconButton icon={MoreHorizontal} label={`More for ${name}`} disabled={busy} {...p} />}
            items={[
              {
                label: "Check for changes",
                icon: ScanSearch,
                title: "Compares each file with the copy on disk (size and date, then content if needed)",
                onSelect: () => {
                  setError(null);
                  load();
                },
              },
              ...(embeddingModel && embedded < chunks
                ? [
                    {
                      label: "Index for meaning search",
                      icon: RefreshCw,
                      title: "Computes embeddings for passages not indexed yet, with the embedding model",
                      onSelect: async () => {
                        setBusy(true);
                        setError(null);
                        try {
                          onChanged(await api.indexCollection(id, setProgress));
                        } catch (e) {
                          setError(errorText(e));
                        } finally {
                          setBusy(false);
                          setProgress(null);
                        }
                      },
                    },
                  ]
                : []),
              {
                label: "Rename…",
                icon: Pencil,
                onSelect: async () => {
                  const next = window.prompt("Rename collection", name);
                  if (!next?.trim()) return;
                  try {
                    onChanged(await api.renameCollection(id, next));
                  } catch (e) {
                    setError(errorText(e));
                  }
                },
              },
              "separator",
              {
                label: "Delete collection…",
                icon: Trash2,
                danger: true,
                onSelect: async () => {
                  if (!window.confirm(`Delete "${name}" and its index? Your original files are not touched.`)) return;
                  try {
                    if (selected?.collection === id) onSelect(null);
                    onChanged(await api.deleteCollection(id));
                  } catch (e) {
                    setError(errorText(e));
                  }
                },
              },
            ]}
          />
        </div>
      </div>
      {progress && progress.total > 0 && (
        <div className="gw-collection-progress">
          <SegmentedBar
            label={progress.file ? `Reading ${progress.file}` : "Indexing"}
            total={progress.total}
            height={4}
            segments={[{ label: "Done", value: Math.min(progress.done, progress.total), color: "--accent" }]}
          />
          <span className="small muted">
            {progress.file ? `Reading ${progress.file} · ` : "Indexing · "}
            {Math.min(progress.done, progress.total)} of {progress.total}
          </span>
        </div>
      )}
      {report && (
        <div className="small gw-collection-report">
          {report.added.length > 0 && (
            <div>
              {reindexed ? "Re-indexed" : "Added"}: {report.added.join(", ")}
            </div>
          )}
          {report.failed.map(([file, why]) => (
            <div key={file} className="error-text">
              {file}: {why}
            </div>
          ))}
          {report.embedError && (
            <div className="warn-text">
              {reindexed ? "Re-indexed" : "Added"} for keyword search only; meaning search indexing failed: {report.embedError}
            </div>
          )}
        </div>
      )}
      {error && <div className="banner error">{error}</div>}
      {list && (
        <div className="gw-collection-body">
          <DataTable<FileSummary>
            rows={list}
            rowKey={(f) => String(f.id)}
            selected={selected?.collection === id ? String(selected.file.id) : null}
            onSelect={(f) => onSelect({ collection: id, collectionName: name, file: f })}
            empty={<p className="small muted gw-collection-empty">No files yet. Add files to search them from chats and the agent.</p>}
            columns={[
              {
                key: "name",
                header: "Name",
                minWidth: "200px",
                render: (f) => (
                  <div>
                    <div>{f.name}</div>
                    {STATUS[f.status] && (
                      <div className="gw-file-status" title={STATUS[f.status]!.hint}>
                        <Pill tone={STATUS[f.status]!.tone}>{STATUS[f.status]!.label}</Pill>
                      </div>
                    )}
                  </div>
                ),
              },
              { key: "passages", header: "Passages", width: "96px", optional: true, align: "right", render: (f) => f.chunks.toLocaleString() },
              { key: "size", header: "Size", width: "96px", optional: true, align: "right", render: (f) => formatBytes(f.size) },
              { key: "added", header: "Added", width: "120px", optional: true, render: (f) => new Date(f.addedAt * 1000).toLocaleDateString() },
              {
                key: "actions",
                header: <span className="gw-sr">Actions</span>,
                width: "180px",
                align: "right",
                render: (f) => (
                  <div className="gw-row-actions" onClick={(e) => e.stopPropagation()}>
                    {f.status === "changed" && (
                      <Button size="sm" variant="ghost" disabled={busy} onClick={() => void reindex([f.id])}>
                        Re-index
                      </Button>
                    )}
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={async () => {
                        try {
                          if (selected?.file.id === f.id) onSelect(null);
                          setList(await api.removeDocument(id, f.id));
                          onChanged(await api.listCollections());
                        } catch (e) {
                          setError(errorText(e));
                        }
                      }}
                    >
                      Remove
                    </Button>
                  </div>
                ),
              },
            ]}
          />
        </div>
      )}
    </section>
  );
}

/** Plain text with the matches of `query` marked (no HTML is interpreted). */
function Marked({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const parts = text.split(new RegExp(`(${query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "gi"));
  return (
    <>
      {parts.map((p, i) => (i % 2 === 1 ? <mark key={i}>{p}</mark> : <Fragment key={i}>{p}</Fragment>))}
    </>
  );
}

/** The extracted text of the selected file, with Find and Copy. */
function DocumentText({ selected }: { selected: Selected }) {
  const [passages, setPassages] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  useEffect(() => {
    setPassages(null);
    setError(null);
    api.documentText(selected.collection, selected.file.id).then(setPassages, (e) => setError(errorText(e)));
  }, [selected.collection, selected.file.id]);
  const matches = useMemo(() => {
    if (!passages || !query) return 0;
    const q = query.toLowerCase();
    return passages.reduce((n, p) => n + p.toLowerCase().split(q).length - 1, 0);
  }, [passages, query]);
  const status = STATUS[selected.file.status];
  return (
    <div className="gw-doc-text">
      <div className="gw-doc-text-head">
        <div className="gw-doc-text-name">{selected.file.name}</div>
        <div className="muted small">
          {selected.collectionName} · {formatBytes(selected.file.size)} · {selected.file.chunks} passages
        </div>
        {status && <p className="small warn-text">{status.label}. {status.hint}</p>}
      </div>
      <div className="gw-doc-text-tools">
        <input type="search" placeholder="Find in the text" aria-label="Find in the extracted text" value={query} onChange={(e) => setQuery(e.target.value)} />
        {query && <span className="small muted">{matches} found</span>}
        {passages && <CopyButton text={passages.join("\n\n")} label="Copy all the text" />}
      </div>
      {error && <div className="banner error">{error}</div>}
      {!passages && !error && <p className="muted small">Loading…</p>}
      {passages && passages.length === 0 && <p className="muted small">No text was extracted from this file.</p>}
      {passages && passages.length > 0 && (
        <div className="gw-doc-passages">
          {passages.map((p, i) => (
            <div key={i} className="gw-doc-passage">
              <div className="gw-doc-passage-n">Passage {i + 1}</div>
              <div className="gw-doc-passage-text">
                <Marked text={p} query={query} />
              </div>
            </div>
          ))}
        </div>
      )}
      <p className="muted small">
        This is the text Gardenwall extracted and searches, split into the passages it quotes to the model. Scanned PDFs show
        their page markers. It's stored encrypted on this computer; your original file isn't changed.
      </p>
    </div>
  );
}

export function DocumentsView({ state }: { state: AppView }) {
  const [view, setView] = useState<CollectionsView | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Selected | null>(null);
  /** The collection shown on the right (the first one until another is picked). */
  const [currentId, setCurrentId] = useState<string | null>(null);

  useEffect(() => {
    api.listCollections().then(setView).catch((e) => setError(errorText(e)));
  }, []);

  if (!view) return <div className="page muted">Loading…</div>;
  const usable = state.models.filter((m) => m.template.acknowledged || !m.template.requiresAcknowledgement);
  const create = async () => {
    try {
      const before = new Set(view.collections.map((c) => c.id));
      const next = await api.createCollection(name.trim());
      setView(next);
      setName("");
      // Show the new collection, ready for files.
      const made = next.collections.find((c) => !before.has(c.id));
      if (made) setCurrentId(made.id);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const current = view.collections.find((c) => c.id === currentId) ?? view.collections[0] ?? null;
  const createForm = (
    <form
      className="gw-docs-create"
      onSubmit={(e) => {
        e.preventDefault();
        if (name.trim()) void create();
      }}
    >
      <input value={name} placeholder="New collection name" aria-label="New collection name" onChange={(e) => setName(e.target.value)} />
      <Button type="submit" variant="primary" icon={FolderPlus} disabled={!name.trim()}>
        Create
      </Button>
    </form>
  );
  const searchSetting = (
    <div className="gw-docs-search">
      <label htmlFor="gw-docs-embedding">Search</label>
      <select
        id="gw-docs-embedding"
        value={view.embeddingModel ?? ""}
        onChange={async (e) => {
          try {
            setView(await api.setEmbeddingModel(e.target.value || null));
          } catch (err) {
            setError(errorText(err));
          }
        }}
      >
        <option value="">Keyword search only</option>
        {usable.map((m) => (
          <option key={m.sha256} value={m.sha256}>
            Meaning: {m.displayName}
          </option>
        ))}
      </select>
      <Fold summary="Keyword search always works." more="Meaning search">
        An embedding model also finds passages phrased differently from your question. Download a small embedding model
        (for example a 0.6B "Embedding" GGUF) on the Models page and choose it here; it runs in its own sandbox next to
        your chat model.
      </Fold>
    </div>
  );

  return (
    <div className="gw-docs">
      {view.collections.length > 0 && (
        <aside className="conversations gw-docs-list" aria-label="Collections">
          {createForm}
          <ul>
            {view.collections.map((c) => (
              <li key={c.id} className={c.id === current?.id ? "active" : undefined}>
                <button type="button" className="conversation-title gw-docs-item" onClick={() => setCurrentId(c.id)} aria-current={c.id === current?.id}>
                  <span className="gw-docs-item-name">{c.name}</span>
                  <span className="small muted">
                    {c.files} file{c.files === 1 ? "" : "s"}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          {searchSetting}
        </aside>
      )}
      <div className="page gw-docs-main">
        {error && <div className="banner error">{error}</div>}
        {current ? (
          <>
            <Collection
              key={current.id}
              {...current}
              embeddingModel={view.embeddingModel}
              onChanged={setView}
              selected={selected}
              onSelect={setSelected}
            />
            <Fold summary="Collections of your files that chats and the agent can search. Your original files are never changed." more="How documents work">
              Text, Markdown, code, PDF and DOCX are supported. PDF, DOCX and HTML are read by a sandboxed helper, and
              scanned PDFs with Windows' built-in OCR. The extracted text is stored encrypted on this computer. Gardenwall
              remembers where each file came from and checks it for changes when you open the collection; changed files are
              read again only when you choose Re-index.
            </Fold>
          </>
        ) : (
          <div className="gw-docs-empty">
            <EmptyState
              title="Create your first collection"
              body="Group files you want to ask about, such as tax papers or a project's notes. Chats and the agent can then search them; your original files are never changed."
            />
            {createForm}
            {searchSetting}
          </div>
        )}
      </div>
      {selected && (
        <Inspector
          title="Document"
          active="text"
          onActive={() => {}}
          onClose={() => setSelected(null)}
          tabs={[{ id: "text", label: "Extracted text", content: <DocumentText selected={selected} /> }]}
        />
      )}
    </div>
  );
}
