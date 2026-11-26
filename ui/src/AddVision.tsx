// "Add vision": find an image projector made
// for a model on Hugging Face, download it (verified like any model) and
// pair it, so screenshots can be shared with the model.
import { useState } from "react";
import { api, errorText, ModelRecord, ProjectorSearch } from "./api";
import { DownloadControls, useDownload } from "./useDownload";

function mb(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`;
}

export function AddVision({ model, offline, refresh }: { model: ModelRecord; offline: boolean; refresh: () => Promise<void> }) {
  const [search, setSearch] = useState<ProjectorSearch | null>(null);
  const [looking, setLooking] = useState(false);
  const dl = useDownload<ModelRecord>();
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const find = async () => {
    setLooking(true);
    setError(null);
    try {
      setSearch(await api.findProjectors(model.sha256));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLooking(false);
    }
  };

  const add = (repo: string, path: string) => {
    setError(null);
    dl.start({
      key: path,
      run: (onProgress) => api.downloadModel(repo, path, onProgress),
      onDone: async (projector) => {
        try {
          await api.setModelProjector(model.sha256, projector.sha256);
          await refresh();
          setDone(`Vision added. Load ${model.displayName} again to use it; then attach or paste images.`);
          setSearch(null);
        } catch (e) {
          setError(errorText(e));
        }
      },
      onError: setError,
    });
  };

  if (offline) {
    return <p className="small muted">Offline mode is on, so Gardenwall can't look for an image projector.</p>;
  }
  return (
    <div className="add-vision">
      {!search && (
        <button className="small" disabled={looking} onClick={() => void find()}>
          {looking ? "Looking on Hugging Face…" : "Add vision…"}
        </button>
      )}
      {done && <p className="small ok-text">{done}</p>}
      {error && <div className="banner error">{error}</div>}
      {search && (
        <div className="vision-results">
          <p className="small muted">
            Looked for image projectors (mmproj) for <strong>{search.basedOn}</strong> in{" "}
            {search.searched.length ? search.searched.join(", ") : "no repositories"}.
          </p>
          {search.candidates.length === 0 ? (
            <p className="small">
              None found. This model may be text-only. Models that can see usually have "VL", "vision" or "omni" in
              their name, or publish an mmproj file next to the model; search for one on the Models page.
            </p>
          ) : (
            <ul className="plain-list">
              {search.candidates.map((c) => (
                <li key={`${c.repo}/${c.path}`} className="row">
                  <span>
                    <span className="mono small">{c.path}</span>{" "}
                    <span className="small muted">
                      {mb(c.size)} · {c.repo}
                      {c.sameRepo ? " · published with this model (best match)" : ""}
                    </span>
                  </span>
                  {dl.key === c.path ? (
                    <DownloadControls dl={dl} />
                  ) : (
                    <button className="small primary" disabled={dl.phase !== "idle"} onClick={() => add(c.repo, c.path)}>
                      Download and pair
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
          <p className="small muted">
            The file is downloaded and checked like any model. Gardenwall refuses a projector whose output doesn't match
            this model. It uses extra GPU memory (about its file size).
          </p>
          <button className="link small" onClick={() => setSearch(null)}>
            Close
          </button>
        </div>
      )}
    </div>
  );
}
