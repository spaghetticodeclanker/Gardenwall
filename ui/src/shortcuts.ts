// Every keyboard shortcut, in one list (final review item 14).
// The Ctrl+K palette shows it when you type "?" or
// "keys", and Help → Settings shows it as a table, so the two can't drift.
// `typed`: something you type in the message box rather than a key.

export interface Shortcut {
  keys: string;
  text: string;
  typed?: boolean;
}

export const SHORTCUTS: Shortcut[] = [
  { keys: "Ctrl+K", text: "Go to a page, or find a chat or agent session" },
  { keys: "Ctrl+B", text: "Hide or show the navigation and the list (focus mode)" },
  { keys: "Enter", text: "Send (while the agent works: send now, interrupting it)" },
  { keys: "Alt+Enter", text: "Queue a message until the agent's task is done" },
  { keys: "Shift+Enter", text: "New line in the message box" },
  { keys: "@", text: "Suggest files from the granted folders (agent)", typed: true },
  { keys: "Up", text: "Bring back earlier messages (at the start of the message box)" },
  { keys: "!command", text: "Run your own command in the session's sandbox; its output goes with your next message", typed: true },
  { keys: "!!command", text: "Run your own command without telling the agent", typed: true },
  { keys: "Tab", text: "Use the highlighted command or skill in the / menu, or the first @ file suggestion" },
  { keys: "Shift+F10", text: "Open the right-click menu of the focused chat or session" },
];

/** What typed in the Ctrl+K palette lists the shortcuts. */
export function asksForShortcuts(query: string): boolean {
  const q = query.trim().toLowerCase();
  return q === "?" || q === "key" || q === "keys" || q === "shortcut" || q === "shortcuts";
}
