// Slash commands in Agent mode (typed as /name in the message box). Each is a
// trusted UI action: the model never runs them. Commands that ask the agent
// to do work (/init, /review) send a fixed request on the user's behalf;
// everything they then do goes through policy and approval as usual.

import type { McpPromptView, UserCommand } from "./api";

export interface AgentCommand {
  name: string;
  /** Shown after the name in suggestions, e.g. "[focus]". */
  args?: string;
  summary: string;
  /** A custom command's prompt template (from the granted folder). */
  template?: string;
  /** An MCP server's prompt: filled in by the core and put in the
   * composer for the user to review (never sent on its own). */
  mcpServer?: string;
  /** A skill to run now: the agent loads it
   * with its skill tool and applies it to the arguments. */
  skill?: string;
}

/** Built-in commands, then the folder's custom commands, then MCP prompts,
 * then skills. A skill is `/<name>`, or `/skill:<name>` when that name is
 * already taken by a command. */
export function allCommands(
  custom: UserCommand[] = [],
  prompts: McpPromptView[] = [],
  skills: [string, string][] = [],
): AgentCommand[] {
  const list: AgentCommand[] = [
    ...COMMANDS,
    ...custom.map((c) => ({
      name: `/${c.name}`,
      args: c.argumentHint ?? undefined,
      summary: c.description,
      template: c.template,
    })),
    ...prompts.map((p) => ({
      name: p.command,
      args: p.arguments.map((a) => (a.required ? `<${a.name}>` : `[${a.name}]`)).join(" ") || undefined,
      summary: `${p.description || "MCP prompt"} (from ${p.server}; you review it before it's sent)`,
      mcpServer: p.server,
    })),
  ];
  const taken = new Set(list.map((c) => c.name.toLowerCase()));
  for (const [name, source] of skills) {
    const plain = `/${name}`;
    const command = taken.has(plain.toLowerCase()) ? `/skill:${name}` : plain;
    if (taken.has(command.toLowerCase())) continue;
    taken.add(command.toLowerCase());
    list.push({ name: command, args: "[what to do]", summary: `Use the ${source} skill "${name}" now`, skill: name });
  }
  return list;
}

/** The request a skill command sends: load the skill, then apply it. */
export function skillRequest(name: string, args: string): string {
  const task = args ? `\n\nWhat to do: ${args}` : "\n\nApply it to what we're working on.";
  return `Use the "${name}" skill: load it with the skill tool first and follow its instructions.${task}`;
}

/** A custom command's request: `$ARGUMENTS` replaced by the arguments,
 * or the arguments added at the end when the template doesn't use them. */
export function expandTemplate(template: string, args: string): string {
  if (template.includes("$ARGUMENTS")) return template.split("$ARGUMENTS").join(args);
  return args ? `${template}\n\n${args}` : template;
}

export const COMMANDS: AgentCommand[] = [
  { name: "/help", summary: "List the commands" },
  { name: "/plan", summary: "Planning mode on or off: the agent looks, then proposes a plan for you to approve" },
  { name: "/init", summary: "Have the agent write an AGENTS.md describing this project (you approve the file)" },
  { name: "/compact", args: "[focus]", summary: "Summarize older messages now, optionally saying what to keep" },
  { name: "/context", summary: "Show what fills the context window" },
  { name: "/clear", summary: "Start a fresh session with the same folders" },
  { name: "/resume", summary: "End this session and pick a saved one" },
  { name: "/undo", summary: "Undo the agent's file edits from the last turn" },
  { name: "/review", summary: "Have the agent review the uncommitted changes (it changes nothing)" },
  { name: "/memory", summary: "Edit the folder's AGENTS.md (instructions for every session)" },
  { name: "/skills", summary: "List the skills the agent can use" },
  { name: "/remember", args: "[project:] text", summary: "Save a memory; start with 'project:' to tie it to these folders" },
  { name: "/model", summary: "Choose or load a model" },
  { name: "/thinking", args: "[default|medium|low|off]", summary: "Show or set how much the loaded model thinks" },
  { name: "/reload", summary: "Read AGENTS.md, CLAUDE.md, skills and commands again" },
  { name: "/name", args: "[name]", summary: "Show or set this session's name" },
  { name: "/session", summary: "Show this session's details and size" },
  { name: "/fork", summary: "Continue from an earlier message in a new saved session" },
  { name: "/clone", summary: "Continue in a copy of this session (the original stays as it is)" },
  { name: "/handoff", args: "[focus]", summary: "Continue in a fresh session from a handoff note you review (same project and folders)" },
  { name: "/copy", summary: "Copy the agent's last reply" },
  { name: "/export", args: "[html|md|jsonl]", summary: "Save this session as a web page, a Markdown file or a JSONL file" },
  { name: "/import", summary: "Import a session from a Gardenwall JSONL file" },
];

/** Split "/compact keep file names" into the command and its argument. */
export function parseCommand(
  input: string,
  commands: AgentCommand[] = COMMANDS,
): { command: AgentCommand; arg: string } | null {
  const text = input.trim();
  if (!text.startsWith("/")) return null;
  const match = /^(\S+)\s*([\s\S]*)$/.exec(text);
  const name = match?.[1]?.toLowerCase();
  const command = commands.find((c) => c.name.toLowerCase() === name);
  return command ? { command, arg: match?.[2]?.trim() ?? "" } : null;
}

/** Commands matching what has been typed so far ("/co" → /compact, /context). */
export function suggestions(input: string, commands: AgentCommand[] = COMMANDS): AgentCommand[] {
  const text = input.trimStart();
  if (!text.startsWith("/") || /\s/.test(text)) return [];
  return commands.filter((c) => c.name.toLowerCase().startsWith(text.toLowerCase()));
}

export const INIT_PROMPT =
  "Please study this folder and write an AGENTS.md in its root to help future agents work here. Include: the exact commands to build, test, lint and run the project; a short overview of its structure and architecture; and the code style and conventions you can see. Keep it concise and specific to this project, without generic advice. If an AGENTS.md already exists, improve it rather than starting over. Use the write tool; the user will be asked to approve the file.";

export const REVIEW_PROMPT =
  "Please review the uncommitted changes in this repository. Run `git status`, `git diff` and `git diff --staged`, read the changed files where you need more context, and report bugs, risky changes, security problems and missing tests, most important first, with file and line. Do not change any files.";

/** Sent when the user continues a task that paused at the step checkpoint. */
export const CONTINUE_PROMPT =
  "The user asked you to continue. Carry on with the task from where you stopped, without repeating work already done.";

export const APPROVE_PLAN_PROMPT =
  "The user approved your plan. Carry it out now, keeping your task list up to date, and summarize what you changed at the end.";
