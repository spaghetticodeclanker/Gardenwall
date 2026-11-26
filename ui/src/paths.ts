// Paths as people see them. The core canonicalizes folders it grants, so on
// Windows they can carry the long-path prefix (`\\?\C:\…`, or `\\?\UNC\server\…`
// for a network share). It is needed by the system, never by a person, so it
// is left out wherever a path is shown or copied for the user. The value the
// core gave is still what goes back to the core (some commands match it
// exactly).

/** `\\?\C:\Users\me` → `C:\Users\me`; `\\?\UNC\server\share` → `\\server\share`. */
export function shownPath(p: string): string {
  if (p.startsWith("\\\\?\\UNC\\")) return `\\\\${p.slice(8)}`;
  if (p.startsWith("\\\\?\\")) return p.slice(4);
  return p;
}

/** Several paths joined for display. */
export const shownPaths = (paths: string[], sep: string) => paths.map(shownPath).join(sep);
