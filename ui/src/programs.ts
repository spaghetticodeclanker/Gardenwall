/** Programs left running, as short names ("node.exe (process 4242)" → "node.exe"), duplicates counted. */
export function programNames(background: string[]): string {
  const counts = new Map<string, number>();
  for (const b of background) {
    const name = b.replace(/ \(process \d+\)$/, "");
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return [...counts].map(([name, n]) => (n > 1 ? `${name} ×${n}` : name)).join(", ");
}
