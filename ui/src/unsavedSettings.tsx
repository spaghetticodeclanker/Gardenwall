// Unsaved settings: a section with a draft
// registers while its draft differs from what is saved. Settings shows a
// banner with Save and Discard at the top, and leaving the page asks first,
// so a change is never lost silently.
import { useEffect, useRef, useSyncExternalStore } from "react";

export type UnsavedEntry = {
  /** The section's name, as on the page ("Agent"). */
  label: string;
  /** Save the draft; false when it couldn't be saved (the section shows why). */
  save: () => Promise<boolean>;
  /** Put the draft back to what is saved. */
  discard: () => void;
};

const entries = new Map<string, UnsavedEntry>();
const listeners = new Set<() => void>();
let snapshot: [string, UnsavedEntry][] = [];

function changed() {
  snapshot = [...entries.entries()];
  listeners.forEach((l) => l());
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Register section `id` while `dirty`. The latest callbacks are always used. */
export function useUnsaved(id: string, dirty: boolean, entry: UnsavedEntry) {
  const latest = useRef(entry);
  latest.current = entry;
  useEffect(() => {
    if (!dirty) return;
    entries.set(id, {
      label: entry.label,
      save: () => latest.current.save(),
      discard: () => latest.current.discard(),
    });
    changed();
    return () => {
      entries.delete(id);
      changed();
    };
    // The label is fixed per section.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, dirty]);
}

export function useUnsavedList(): [string, UnsavedEntry][] {
  return useSyncExternalStore(subscribe, () => snapshot);
}

export function hasUnsaved(): boolean {
  return entries.size > 0;
}

/** Save every section with changes; true when all of them saved. */
export async function saveAll(): Promise<boolean> {
  let ok = true;
  for (const [, e] of [...entries.entries()]) {
    if (!(await e.save())) ok = false;
  }
  return ok;
}

export function discardAll() {
  for (const [, e] of [...entries.entries()]) e.discard();
}

function names(list: [string, UnsavedEntry][]): string {
  const labels = list.map(([, e]) => e.label);
  return labels.length <= 1 ? (labels[0] ?? "") : `${labels.slice(0, -1).join(", ")} and ${labels[labels.length - 1]}`;
}

/** The banner at the top of Settings while anything is unsaved. */
export function UnsavedBanner() {
  const list = useUnsavedList();
  if (list.length === 0) return null;
  return (
    <div className="banner warn unsaved-banner" role="status">
      <span>
        <strong>Unsaved changes</strong> in {names(list)}. They don't apply until you save them.
      </span>
      <span className="unsaved-actions">
        <button className="primary" onClick={() => void saveAll()}>
          Save
        </button>
        <button onClick={discardAll}>Discard</button>
      </span>
    </div>
  );
}

/** Asked when leaving Settings with unsaved changes. */
export function LeaveSettingsDialog({ onLeave, onStay }: { onLeave: () => void; onStay: () => void }) {
  const list = useUnsavedList();
  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="leave-settings-title">
        <h2 id="leave-settings-title">Save your settings?</h2>
        <p>
          You changed {names(list)} but didn't save. If you leave now, those changes are lost.
        </p>
        <div className="row">
          <button
            className="primary"
            onClick={async () => {
              if (await saveAll()) onLeave();
              else onStay();
            }}
          >
            Save and leave
          </button>
          <button
            onClick={() => {
              discardAll();
              onLeave();
            }}
          >
            Discard changes
          </button>
          <button className="link" onClick={onStay}>
            Stay on Settings
          </button>
        </div>
      </div>
    </div>
  );
}
