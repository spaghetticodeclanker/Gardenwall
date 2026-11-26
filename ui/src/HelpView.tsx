// The Help page: every feature and command in plain language
// (Gardenwall should be friendly to newcomers). Static text only.
import { useEffect, useRef, useState } from "react";
import {
  Archive, ArrowLeft, ArrowRightLeft, ArrowUpRight, Binary, BookOpen, Bookmark, BookmarkPlus, Bot, Box, Brain, CalendarClock, Clock, Code, Columns2, Cpu, Download, FileCode, FilePlus, FileText, FileWarning, Files, Flame, FolderOpen, Folders, Gauge, GitBranch, GitCommitHorizontal, GitCompare, Globe, Hand, History, Image, KeyRound, LayoutDashboard, Lightbulb, List, ListTodo, Lock, MemoryStick, MessageSquare, MessageSquarePlus, PanelLeftClose, Paperclip, Plug, Puzzle, RefreshCw, Repeat, Rocket, ScanSearch, ScanText, ScrollText, Search, Settings, Settings2, Shield, ShieldAlert, ShieldCheck, ShieldQuestion, SlidersHorizontal, Square, SquareSlash, Stethoscope, Telescope, Terminal, ToggleRight, TrendingDown, Undo2, WifiOff, Zap, Map as MapIcon,
} from "lucide-react";
import { COMMANDS } from "./agentCommands";
import { GenerationIntro, GuideIntro, guideRows, guideTopicOf } from "./modelGuide";
import { SHORTCUTS } from "./shortcuts";
import { Button } from "./ui";
import { ApprovalDiagram, HELP_GROUPS, HelpTarget, MemoryDiagram, SandboxDiagram, TopicView, WebDiagram, type HelpTopic } from "./helpParts";

// Layout: docs/internal/design/HELP-REDESIGN.md. Topics not yet in the new layout keep
// their full text in `body`, unchanged.
const TOPICS: HelpTopic[] = [
  {
    id: "start",
    title: "Getting started",
    group: "start",
    icon: Rocket,
    card: "Four steps to your first chat",
    summary: "Four steps take you from nothing to your first chat. Everything runs on this computer.",
    chips: ["Runs on this computer", "Verified downloads", "A few steps"],
    parts: [
{ kind: "node", node: (
<>
<p>Gardenwall runs AI language models entirely on your computer. Nothing you type is sent anywhere.</p>
</>
) },
{ kind: "node", node: (
<>
<ol>
          <li>
            <strong>Install a runtime.</strong> On the <em>Updates</em> page, under <em>Inference runtime</em>, install the inference runtime: the
            program that runs models (llama.cpp). Gardenwall downloads a fixed, verified version.
          </li>
          <li>
            <strong>Get a model.</strong> <em>Models → Find a model</em> suggests one for this computer under{" "}
            <em>For you</em>; <em>Browse</em> shows what people are downloading, <em>Search</em> finds any GGUF on Hugging Face, and you
            can also import a GGUF file you already have.
            Every file is checked before it is accepted. See <em>Choosing a model</em> below for what the choices mean.
          </li>
          <li>
            <strong>Review the template audit</strong> in the model's side panel (<em>Models → Library</em> → the model), then press <em>Load</em>. Open its{" "}
            <em>Settings</em> tab and use <em>Suggest for this computer</em> if you're not sure what to
            choose.
          </li>
          <li>
            <strong>Chat</strong> on the <em>Chat</em> page, or let the <em>Agent</em> work on a folder you choose.
          </li>
        </ol>
</>
) }
    ],
  },
  {
    id: "choosing",
    title: "Choosing a model",
    group: "start",
    icon: Cpu,
    card: "Size, quality and what fits this computer",
    summary: "What the choices on the Models page mean: size, quantization, context and what fits your GPU.",
    chips: ["Bigger is smarter but slower", "Fit to GPU memory first", "Q4-Q6 is the usual balance"],
    parts: [
{ kind: "node", node: <GuideIntro /> },
{ kind: "rows", rows: guideRows("choosing") }
    ],
  },
  {
    id: "generation",
    title: "Generation settings",
    group: "further",
    icon: SlidersHorizontal,
    card: "Temperature, top-p and friends",
    summary: "Temperature, top-p and the other dials that shape how the model writes.",
    chips: ["Empty uses the model's defaults", "Per chat or default"],
    parts: [
{ kind: "node", node: <GenerationIntro /> },
{ kind: "rows", rows: guideRows("generation") }
    ],
  },
  {
    id: "security",
    title: "Privacy and security",
    group: "start",
    icon: ShieldCheck,
    card: "What stays here, what can leave, and who decides",
    summary: "Every model is treated as untrusted: it can only do what you allow, and Windows enforces it separately.",
    chips: ["Sandboxed models", "Nothing leaves unless you ask", "Encrypted chats"],
    diagram: <ApprovalDiagram />,
    parts: [
{ kind: "node", node: (
<>
<p>
          Gardenwall treats every model as untrusted. Whatever a model writes, it can only do what you allow, and the
          operating system enforces that separately.
        </p>
</>
) },
{ kind: "callout", summary: "If the sandbox isn't available, Gardenwall refuses to run the model rather than running it unprotected." },
{ kind: "rows", rows: [
{ icon: Shield, title: "Sandbox", summary: "Each model runs locked down, or not at all", body: (
<>
<p>Each model runs in its own locked-down process with no network access, able to
            read only its own files. If the sandbox isn't available, Gardenwall refuses to run the model rather than
            running it unprotected.</p>
</>
) },
{ icon: Hand, title: "Approvals", summary: "Nothing outside your grants happens until you allow it", body: (
<>
<p>When the agent wants to do something outside what you granted (a file
            outside its folders, a web page, an MCP tool), a dialog shows exactly what it will do. Nothing happens
            until you choose <em>Allow once</em>. Some requests also offer to allow similar ones for the rest of the
            session.</p>
</>
) },
{ icon: WifiOff, title: "Offline mode", summary: "One switch turns off everything that uses the internet", body: (
<>
<p><strong>Offline mode</strong> (Settings → Privacy) turns off everything that uses the internet: model
            search and downloads, update checks and web tools.</p>
</>
) },
{ icon: Lock, title: "Encrypted chats", summary: "Chats, sessions and collections, locked to your Windows account", body: (
<>
<p>Saved chats, agent sessions and document collections are encrypted with
            a key only your Windows account can unlock.</p>
</>
) },
{ icon: ArrowUpRight, title: "What leaves your computer", summary: "Only what you ask for, listed", body: (
<>
<p>Only what you ask for: model searches, the model ranking and
            downloads (Hugging Face), update checks, and web searches and pages the agent fetches with your approval.
            The ranking sends fixed settings only, never your search words or anything about this computer; the
            "Research current models" question includes your hardware only if you leave "Include my hardware" ticked.</p>
</>
) }
] }
    ],
  },
  {
    id: "chat",
    title: "Chat",
    group: "using",
    icon: MessageSquare,
    card: "Files, images, the web and memories in a chat",
    summary: "Talk with the loaded model, with files, images, documents and the web when you want them.",
    chips: ["Saved encrypted", "Branch from any message", "Links are never opened"],
    parts: [
{ kind: "callout", summary: (
<>
Links in answers are copied, never opened, and risky-looking shell commands are flagged.
</>
) },
{ kind: "rows", rows: [
{ icon: MessageSquarePlus, title: "New chat", summary: "Start, rename or delete chats", body: (
<>
<p><strong>New chat</strong> starts a conversation. Rename or delete chats from the list; deleted chats are
          overwritten, not just hidden.</p>
</>
) },
{ icon: GitBranch, title: "Edit, Branch and Export", summary: "Change a message, branch from one, save as Markdown", body: (
<>
<p><strong>Edit</strong> under one of your messages changes it and
          sends it again, either as a new branch (a new chat that starts from the messages before it; this chat stays
          as it is) or replacing it and everything after it. <strong>Branch</strong> under any message starts a new chat
          with the messages up to there. Branches are listed under the chat they came from. The export button (⤓) in
          the chat list saves a chat as a Markdown file where you choose; the model's reasoning is left out.</p>
</>
) },
{ icon: ScrollText, title: "System prompt", summary: "Standing instructions for one chat", body: (
<>
<p>Standing instructions for one chat (for example "answer briefly"). A
          default for new chats is in Settings → Generation.</p>
</>
) },
{ icon: Files, title: "Documents", summary: "Answers from your files, with sources", body: (
<>
<p>Attach a document collection and each question gets the most relevant passages
          from your files, with the sources shown under the answer.</p>
</>
) },
{ icon: Paperclip, title: "Files", summary: "Attach or drop files; how each kind is read", body: (
<>
<p><strong>Attach…</strong>, or drop files on the window, adds them to your message
          (the same works in Agent mode). Documents (PDF, DOCX, HTML) are read to text in a sandbox; code and text files
          are included as they are (up to 120,000 characters); programs and libraries get the binary analyzer's report;
          other files get their readable strings. Each shows folded under your message.</p>
</>
) },
{ icon: Image, title: "Images and screenshots", summary: "Paste, attach or drop; needs a vision model", body: (
<>
<p><strong>Images and screenshots</strong> (paste with Ctrl+V, attach or drop) need a model that can see. If
          yours can't, open its Settings on the Models page and use <strong>Add vision…</strong>: Gardenwall looks for
          the image projector made for that model, downloads it verified, and pairs it. Up to 4 images per message,
          PNG, JPEG, WebP or GIF, 10 MB each. Gardenwall checks each image's type but never opens it; the sandboxed
          model runtime does.</p>
</>
) },
{ icon: Brain, title: "Model and Thinking", summary: "Switch model and thinking without leaving", body: (
<>
<p><strong>Model</strong> and <strong>Thinking</strong> next to the message box switch the loaded model (with the
          settings saved for it) and how much it thinks, without leaving the chat.</p>
</>
) },
{ icon: PanelLeftClose, title: "Focus mode", summary: "Hide the navigation (Ctrl+B)", body: (
<>
<p>The button at the top left (or Ctrl+B) hides the navigation and the chat list;
          press it again to bring them back.</p>
</>
) },
{ icon: Square, title: "Stop", summary: "End an answer early; context and speed bar", body: (
<>
<p><strong>Stop</strong> ends an answer early. The bar above the chat shows how full the context window is
          and the speed of the last answer.</p>
</>
) }
] }
    ],
  },
  {
    id: "agent",
    title: "Agent",
    group: "using",
    icon: Bot,
    card: "Projects, sessions, undo and approvals",
    summary:
      "The agent reads, edits and runs commands only inside the folders you choose. Anything else asks you first, and each turn can be undone.",
    chips: ["Sandboxed, no network", "Undo per turn", "Asks before secrets", "Several sessions at once"],
    diagram: <SandboxDiagram />,
    parts: [
      { kind: "heading", text: "How it works" },
      {
        kind: "rows",
        rows: [
          {
            icon: Terminal,
            title: "Commands and the shell",
            summary: "PowerShell 7 when installed; each folder is its own drive",
            body: (
              <p>
                The agent can read, edit and create files and run commands, but only inside the folders you choose when
                you start a session. Commands run in a sandbox with no network, in PowerShell by default: PowerShell 7
                when it is installed (Microsoft's MSI installer, not the Store version), otherwise Windows PowerShell 5.1.
                Windows PowerShell 5.1 and Command Prompt are choices in Settings → Agent, and each session's header
                shows the shell it uses. Inside the sandbox each project folder appears as its own
                drive (W:, X:…), so commands start at the top of the project and can't see anything above it.
              </p>
            ),
          },
          {
            icon: FileCode,
            title: "Python",
            summary: "Runs in the same sandbox, without network or your packages",
            body: (
              <p>
                When Python is installed for all users (python.org, "Install for all users"), the
                agent can run it in the same sandbox, for example to run tests. Its own folder is read-only there, it has
                no network (so pip install doesn't work), it sees none of your personal Python packages, and compiled files
                go to a temporary folder rather than the project. Files that make Python run code by themselves (.pth
                files, sitecustomize.py, virtual environments, compiled .pyc files) need your approval before the agent
                edits them. Turn it off in Settings → Agent.
              </p>
            ),
          },
          {
            icon: Folders,
            title: "Projects",
            summary: "The folders you work on, chosen once",
            body: (
              <p>
                A project is the folders you work on. <strong>New project…</strong> asks for
                them once (a new or existing folder, what you're building so the right toolchain is added, and a Git
                repository if you want one; its first session starts with a suggested message you can edit); after that, <strong>+</strong> next to the project starts a new session on those folders, and
                its sessions are listed under it. The project page edits its name and folders and lists all its sessions.
              </p>
            ),
          },
          {
            icon: Columns2,
            title: "Several sessions",
            summary: "Each with its own conversation, sandbox and approvals",
            body: (
              <p>
                Several sessions can be open and working at once; each has its own conversation,
                sandbox and approvals. The list shows which are working. How they share the model (take turns, one at a
                time, or in parallel) is in Settings → Agent. Approval requests name the session that asks.
              </p>
            ),
          },
          {
            icon: Flame,
            title: "Warm-up",
            summary: "The first reply starts sooner",
            body: (
              <p>
                When a session opens, Gardenwall has the model read its instructions and tools in the background while you
                write, so the first reply starts sooner. It waits if another session is working.
              </p>
            ),
          },
        ],
      },
      { kind: "heading", text: "Staying in control" },
      {
        kind: "callout",
        title: "Files that run later",
        summary:
          "(Git hooks, editor tasks, agent settings, package scripts, Python environments) can't be changed by the agent's commands. If a command creates one, Gardenwall moves it out of the project and asks.",
        full: (
          <p>
            Some files make code run outside the sandbox later: Git hooks and
            settings, editor tasks (.vscode, .idea), agent settings (.claude, .mcp.json, AGENTS.md), package scripts and
            Python environments. The agent's commands can't change them. If a command creates one, Gardenwall moves it
            out of the project and asks whether to keep it. Commands that look like they change such files show a
            warning first, and programs a command leaves running are stopped unless you let them run (they then show in
            the session header with a Stop button). To change one of these files, the agent uses its edit tool, which
            shows you the change first.
          </p>
        ),
      },
      {
        kind: "rows",
        rows: [
          {
            icon: Undo2,
            title: "Undo last turn",
            summary: "Puts back files from the last turn; not files over 4 MB or a git commit",
            body: (
              <p>
                <strong>Undo last turn</strong> restores files the agent changed in its last turn, with its edit and write
                tools or with commands. Files too large to copy (over 4 MB), generated folders such as node_modules, target and .git, and secret files aren't covered. Undo can't stop what a command did outside files, such as a
                git commit; a note after the command says when something wasn't covered.
              </p>
            ),
          },
          {
            icon: Hand,
            title: "Asks first",
            summary: "Outside folders, git hooks, CI files, secrets, instruction files",
            body: (
              <p>
                <strong>Asks first</strong> before touching files outside its folders, git hooks, CI files, secret files
                (keys, passwords) and instruction files such as AGENTS.md.
              </p>
            ),
          },
          {
            icon: KeyRound,
            title: "Secret files",
            summary: ".env and keys: commands can't open them",
            body: (
              <p>
                <strong>Secret files</strong> such as <code>.env</code> and keys can't be opened by the agent's commands
                at all; its read tool asks you first. A project's page lists them and can move them to your secrets
                folder, outside every project, and put them back. For a git project it also offers to add the
                missing lines to <code>.gitignore</code>.
              </p>
            ),
          },
          {
            icon: GitCompare,
            title: "Changes",
            summary: "Every file the session changed, as a diff",
            body: (
              <p>
                <strong>Changes</strong> shows every file the session changed with its file tools, as a diff from
                before the session started on it.
              </p>
            ),
          },
          {
            icon: Lock,
            title: "Lockdown mode",
            summary: "Emergency stop for a model that seems to be a risk",
            body: (
              <>
                <p>
                  <strong>Lockdown mode</strong> is an emergency stop: the shield in the top bar, Ctrl+K → Lockdown, or
                  Settings → Privacy. It asks once, then stops everything at once: agent sessions end (their commands
                  and programs are killed, their cards withdrawn, their permissions taken back), web requests, chat Web
                  switches, research assistants, MCP servers and downloads stop, and the model is unloaded. It tells
                  you what it stopped, and anything that wouldn't.
                </p>
                <p>
                  While it is on, only chat works, with no tools, web or memories. To turn it off you type UNLOCK;
                  nothing restarts by itself.
                </p>
              </>
            ),
          },
          {
            icon: MapIcon,
            title: "Planning mode",
            summary: "Look first, change nothing, then propose a plan",
            body: (
              <p>
                <strong>Planning mode</strong> (<code>/plan</code>): the agent can look but not change anything, then
                proposes a plan. <em>Approve and start</em> lets it carry the plan out.
              </p>
            ),
          },
        ],
      },
      { kind: "heading", text: "While it works" },
      {
        kind: "rows",
        rows: [
          {
            icon: ListTodo,
            title: "Task list",
            summary: "The steps of longer work, above the conversation",
            body: (
              <p>
                During longer work the agent keeps a list of steps, shown above the
                conversation.
              </p>
            ),
          },
        ],
      },
      {
        kind: "table",
        caption: "Keys and shortcuts while the agent works",
        rows: [
          {
            key: <kbd>Enter</kbd>,
            text: (
              <>
                <strong>Send now</strong>: while it works, type and press Enter to interrupt: the reply being
                written stops (what it wrote so far is kept, marked as interrupted) and the agent reads your message next. A
                command already running finishes first; an approval card that was waiting is withdrawn.
              </>
            ),
          },
          {
            key: <kbd>Alt+Enter</kbd>,
            text: (
              <>
                <strong>Queue</strong>: sends it after the task instead; queued messages can be edited,
                moved earlier or removed.
              </>
            ),
          },
          { key: "Stop", text: <>Stop puts anything not yet delivered back in the message box.</> },
          {
            key: <code>@name</code>,
            text: (
              <>
                <strong>@</strong> followed by part of a file name suggests files from the granted folders (Tab inserts
                the first).
              </>
            ),
          },
          {
            key: <kbd>Up</kbd>,
            text: (
              <>
                <strong>Up</strong> at the start of the box brings back earlier messages.
              </>
            ),
          },
          {
            key: <code>!command</code>,
            text: (
              <>
                <strong>!command</strong> runs your own command in the session's sandbox and adds its output to your next
                message.
              </>
            ),
          },
          {
            key: <code>!!command</code>,
            text: (
              <>
                <strong>!!command</strong> runs it without telling the agent.
              </>
            ),
          },
        ],
      },
      { kind: "heading", text: "Sessions" },
      {
        kind: "table",
        caption: "Session commands",
        rows: [
          { key: <code>/name</code>, text: "Renames." },
          { key: <code>/session</code>, text: "Shows details." },
          { key: <code>/copy</code>, text: "Copies the last reply." },
          {
            key: <code>/fork</code>,
            text: "Continues from before an earlier message in a new saved session (files already changed stay changed).",
          },
          { key: <code>/clone</code>, text: "Continues in a copy." },
        ],
        note: "Forks appear under their original in Saved sessions.",
      },
      {
        kind: "rows",
        rows: [
          {
            icon: ArrowRightLeft,
            title: "Handoff",
            summary: "Continue long work in a fresh session, with a note",
            body: (
              <p>
                For long work, <code>/handoff [focus]</code> (or the card Gardenwall shows when
                the context passes 70 %, after two summaries, or when the task list is done) has the model write a handoff
                note: goal, what was done, decisions, next steps, open questions and checks. Gardenwall adds the project,
                folders and changed files from its own records. You edit the note, then <strong>Start new session</strong>{" "}
                opens a fresh session in the same project with it as the first message; the old session stays, linked.
                Values from protected files are removed. <strong>Save to project…</strong> writes HANDOFF.md after asking
                you; <strong>Save as…</strong> saves it anywhere. Chats have <strong>Hand off…</strong>{" "}
                too, into a new chat or an agent session.
              </p>
            ),
          },
          {
            icon: Download,
            title: "Export and Import",
            summary: "A web page or a JSONL file; Import reads JSONL back",
            body: (
              <p>
                <strong>Export</strong> (<code>/export</code>, or Export in Saved sessions) saves a session as a web page or
                a JSONL file; the exported file isn't encrypted. <strong>Import…</strong> reads a JSONL file back.
              </p>
            ),
          },
          {
            icon: Gauge,
            title: "Context tracker",
            summary: "How full the context is; older messages summarized when full",
            body: (
              <p>
                <strong>Context tracker</strong>: shows how full the context is. When it gets full, older messages are
                summarized automatically (or now, with <code>/compact</code>).
              </p>
            ),
          },
          {
            icon: LayoutDashboard,
            title: "Dashboard",
            summary: "Every open session at a glance, with notifications",
            body: (
              <p>
                <strong>Dashboard</strong> (top of the Agent list) shows every open session: working, waiting for your
                approval, waiting for its turn, finished or failed, with its current step, task list progress, time and
                context use. Click a row to open the session. When a session finishes, needs approval or fails, a card
                appears in the corner; while Gardenwall is in the background, a silent system notification says so too. It
                names only the session and what happened, never what the model wrote. Turn each kind off in Settings →
                Agent → Notifications.
              </p>
            ),
          },
          {
            icon: Archive,
            title: "Saved sessions",
            summary: "Kept encrypted; continue one on its project's folders",
            body: (
              <p>
                <strong>Saved sessions</strong> are kept encrypted. Click one under its project to continue it on the
                project's folders. <strong>End session</strong> closes a session; it stays saved.
              </p>
            ),
          },
          {
            icon: FileText,
            title: "AGENTS.md and CLAUDE.md",
            summary: "Project instructions the agent reads",
            body: (
              <p>
                <strong>AGENTS.md and CLAUDE.md</strong> in a granted folder are given to the agent as project
                instructions; <code>/reload</code> reads them, skills and commands again. Settings → Agent sets
                summarizing, retries and extra instructions for every session.
              </p>
            ),
          },
          {
            icon: Image,
            title: "Images",
            summary: "With a vision model, read shows it image files",
            body: (
              <p>
                <strong>Images</strong>: with a vision model, the agent's <code>read</code> tool shows it image files.
              </p>
            ),
          },
        ],
      },
    ],
  },
  {
    id: "commands",
    title: "Agent commands",
    group: "further",
    icon: SquareSlash,
    card: "Every / command in one table",
    summary: "Type / in the agent's message box for actions of the app itself; the model can't run them.",
    chips: ["Tab completes", "Your own commands", "Run this yourself cards"],
    parts: [
{ kind: "node", node: (
<>
<p>
          Type <code>/</code> in the agent's message box to see the commands; Tab completes the first match. Commands
          are actions of the app itself: the model can't run them.
        </p>
</>
) },
{ kind: "node", node: (
<>
<table className="help-table">
          <tbody>
            {COMMANDS.map((c) => (
              <tr key={c.name}>
                <td className="mono">
                  {c.name}
                  {c.args && <span className="muted"> {c.args}</span>}
                </td>
                <td>{c.summary}</td>
              </tr>
            ))}
          </tbody>
        </table>
</>
) },
{ kind: "callout", summary: "Gardenwall never runs it. If you do, it runs with your full rights, outside the sandbox, so read it first." },
{ kind: "rows", rows: [
{ icon: Globe, title: "Commands that need the internet", summary: "The agent shows a Run this yourself card instead", body: (
<>
<p>The agent's commands have no network, so installing a
          package or running <code>git pull</code> fails inside the sandbox. Instead the agent can show a{" "}
          <strong>Run this yourself</strong> card: the exact command, why it's needed and the folder to run it in, with
          Copy buttons and a warning when it looks risky. Gardenwall never runs it. If you do, it runs with your full
          rights, outside the sandbox, so read it first. Then press <strong>I ran it</strong> (you can paste what it
          printed) or <strong>Don't run it</strong>. Turn the cards off in Settings → Agent.</p>
</>
) },
{ icon: GitCommitHorizontal, title: "Git", summary: "Diffs, history and commits, with a secret check", body: (
<>
<p>When Git for Windows is installed, Settings → Agent → Git offers{" "}
          <strong>Add Git</strong>. The agent can then see diffs and history and commit, with no network (no push or
          pull). <strong>Commits by</strong> chooses the name on its commits: yours (copied from your git settings),
          "Gardenwall Agent", or one you type. Commands that throw away uncommitted work (<code>git reset --hard</code>,{" "}
          <code>git clean</code>, <code>git checkout -- .</code>) ask first. Before each of the agent's commits,
          Gardenwall checks what's about to be committed: if it looks like it contains a secret (an API key, a private
          key, a <code>.env</code> file), the commit is stopped and a card asks whether to commit anyway (for a test
          value, say). The card names the kind of secret, never the value. Skipping the check (
          <code>--no-verify</code>) asks first. A project's own pre-commit hook still runs after the check, unless it's a
          script, which can't run in the sandbox.</p>
</>
) },
{ icon: Undo2, title: "Undo covers commands too", summary: "What a command changed is undone with the turn", body: (
<>
<p>Before each command Gardenwall keeps copies of the project's files
          (only those that changed since the last command), so <strong>Undo last turn</strong> and the Changes panel
          include what a command created, changed or deleted. Not covered: files over 4 MB, generated folders such as{" "}
          <code>node_modules</code>, <code>target</code> and <code>.git</code>, secret files, and projects with more than
          20,000 files. A note after the command says when something wasn't covered.</p>
</>
) },
{ icon: FilePlus, title: "Your own commands", summary: "A .gardenwall/commands/<name>.md file becomes /<name>", body: (
<>
<p>A file <code>.gardenwall/commands/&lt;name&gt;.md</code> in a granted
          folder becomes <code>/&lt;name&gt;</code>. The file holds the request to send; <code>$ARGUMENTS</code> is
          replaced by whatever you type after the command. An optional header can give a <code>description</code>{" "}
          and an <code>argument-hint</code>.</p>
</>
) }
] }
    ],
  },
  {
    id: "skills",
    title: "Skills, AGENTS.md and MCP",
    group: "further",
    icon: Puzzle,
    card: "Teach the agent and give it tools",
    summary: "Teach the agent your project and tasks, and give it tools from programs on your computer.",
    chips: ["AGENTS.md read each session", "Skills load when needed", "MCP tools ask first"],
    parts: [
{ kind: "rows", rows: [
{ icon: FileText, title: "AGENTS.md", summary: "How to build and test, read every session", body: (
<>
<p><strong>AGENTS.md</strong> in a granted folder is read at the start of every session: describe how to
          build and test the project and its conventions. <code>/init</code> drafts one; <code>/memory</code> edits
          it.</p>
</>
) },
{ icon: Puzzle, title: "Skills", summary: "Instructions for particular tasks", body: (
<>
<p><strong>Skills</strong> are instructions for particular tasks, each in a folder with a SKILL.md (a name and
          description at the top). The agent sees the list and loads a skill when a task matches. To use one now, type{" "}
          <code>/</code> and its name (for example <code>/release-notes for v0.2</code>); if a command already has that
          name, it's <code>/skill:</code> and the name. Project skills live in <code>.gardenwall/skills</code>; personal
          skills are added in Settings → Skills.</p>
</>
) },
{ icon: Plug, title: "MCP servers", summary: "Tools from programs, each sandboxed", body: (
<>
<p><strong>MCP servers</strong> (Settings → MCP servers) add tools from programs on your computer. Each runs in
          its own sandbox with no network, sees only the folders you give it, and every tool call asks first (or is
          allowed for the session). Choose the server's program, or the .cmd launcher an installed server came with.
          A server's prompts appear as commands such as <code>/mcp__notes__summarize</code>: the filled-in prompt goes
          into the message box for you to read and edit, and is only sent when you send it. Skills can bundle extra files (references, templates), which the agent can read when it loads the skill.</p>
</>
) }
] },
{ kind: "node", node: (
<>
<li>Skills and instructions only guide the agent; they never give it extra permissions.</li>
</>
) }
    ],
  },
  {
    id: "memory",
    title: "Memory",
    group: "using",
    icon: Bookmark,
    card: "Notes the assistant keeps about you",
    summary: "Short notes the assistant keeps about you and your projects, so you don't repeat yourself.",
    chips: ["Encrypted here", "You approve every suggestion", "Switch each one on or off"],
    diagram: <MemoryDiagram />,
    parts: [
{ kind: "callout", summary: (
<>
Memories go into every conversation, so text that looks like a password or key is refused, and hidden
          characters are shown as a visible note.
</>
) },
{ kind: "rows", rows: [
{ icon: Bookmark, title: "What memories are", summary: "Personal everywhere, project only on its folder", body: (
<>
<p>Memories are short notes the assistant keeps about you and your projects, so you don't have to repeat
          yourself. They're added to new chats and agent sessions, and stored encrypted on this computer.</p>
<p><strong>Personal</strong> memories apply everywhere; <strong>project</strong> memories belong to an agent
          folder and are used only when it's granted.</p>
</>
) },
{ icon: BookmarkPlus, title: "Save one", summary: "Select text and choose Remember", body: (
<>
<p>Select text in a chat or agent session and choose <em>Remember</em> (or use{" "}
          <em>Remember…</em> under a chat message), edit it to the part worth keeping, and save. In the agent you can
          also type <code>/remember</code> (<code>/remember project: …</code> for the current folder), or add one on
          the Memory page. Saving never runs the model.</p>
</>
) },
{ icon: Clock, title: "When they're used", summary: "From your next chat or session", body: (
<>
<p>A new memory is used from your <strong>next</strong> chat or agent session (or <code>/reload</code>), so
          the open conversation doesn't have to be read again. Edits, switches and deletions apply at once.</p>
</>
) },
{ icon: Lightbulb, title: "Suggestions", summary: "The agent suggests; nothing is used until you accept", body: (
<>
<p>The agent can <strong>suggest</strong> memories, but nothing is used until you accept it, either in the
          card that appears in the agent or on the Memory page.</p>
</>
) },
{ icon: List, title: "The Memory page", summary: "Search, filter, budget, delete several", body: (
<>
<p>The <strong>Memory</strong> page in the sidebar lists everything: search, filter, see which memories are
          left out when the budget is full, and select several to delete.</p>
</>
) },
{ icon: ToggleRight, title: "Switches", summary: "Chat and Agent per memory, and per conversation", body: (
<>
<p>Each memory has its own <em>Chat</em> and <em>Agent</em> switch, and any chat or agent session can turn
          memories off with its <em>Memories</em> switch.</p>
</>
) }
] }
    ],
  },
  {
    id: "web",
    title: "Web research",
    group: "using",
    icon: Globe,
    card: "Search and read pages, with sources",
    summary: "Search the web and read pages to answer a question, with every source recorded.",
    chips: ["You approve searches", "Pages can't run anything", "Every visit listed"],
    diagram: <WebDiagram />,
    parts: [
{ kind: "callout", summary: (
<>
Pages can't run anything: they're turned into plain text by a sandboxed reader with no network, links on them
          are never followed by themselves, and text that tries to instruct the AI is flagged. Private addresses, plain
          http:// pages and anything that looks like a secret are always refused.
</>
) },
{ kind: "callout", summary: (
<>
If web content reaches an agent session with folders without your review, its changes and commands ask first,
          and it can't suggest memories.
</>
) },
{ kind: "rows", rows: [
{ icon: Settings2, title: "Set up search", summary: "SearXNG or Brave, in Settings → Web access", body: (
<>
<p><strong>Set up search</strong> in Settings → Web access: your own SearXNG (there's a guided setup and a
          Find on this computer button) or the Brave Search API with your own key. Test connection checks it.</p>
</>
) },
{ icon: Telescope, title: "Research sessions", summary: "Agent → New research, with citations", body: (
<>
<p><strong>Research sessions</strong> (Agent → New research) search and read pages to answer a question, then
          cite what they read. They have no files and can't run commands.</p>
</>
) },
{ icon: MessageSquare, title: "In chat", summary: "/web, /read or the Web switch", body: (
<>
<p><strong>In chat</strong>, type <span className="mono">/web your search</span> or{" "}
          <span className="mono">/read https://…</span> to attach results to your next message, or turn on a chat's{" "}
          <strong>Web</strong> switch to let replies search on their own (needs a model that can call tools).</p>
</>
) },
{ icon: ShieldQuestion, title: "Ask for each page or Research mode", summary: "Ask every time, or allow searches for one session", body: (
<>
<p><strong>Ask for each page</strong> asks before every search and new page; <strong>Research mode</strong>{" "}
          allows searches and public https:// pages for that session only. You choose when it starts, and can switch to
          asking at any time.</p>
</>
) },
{ icon: Bot, title: "An agent working on your project", summary: "Researches through an assistant with no project access", body: (
<>
<p><strong>An agent working on your project</strong> reads the web only through a research assistant that has no
          access to the project. A card shows the question it wants to research: allow it, edit it or say no. The
          assistant then searches and reads public pages without asking for each one, and its findings come back on a
          card for you to send to the agent (as they are or edited) or discard. Findings you send count as your own
          message. You can turn the review off in Settings → Web access; the findings are then treated as web content.</p>
</>
) },
{ icon: History, title: "Visited sites", summary: "Every search and page, recorded by Gardenwall", body: (
<>
<p><strong>Visited sites</strong> lists every search and page, recorded by Gardenwall (not the model), and warns
          when an answer cites a page that was never opened.</p>
</>
) }
] }
    ],
  },
  {
    id: "binaries",
    title: "Programs and libraries (binary analysis)",
    group: "using",
    icon: Binary,
    card: "Examine EXE and DLL files without running them",
    summary: "The agent can examine programs and libraries without ever running them.",
    chips: ["Never executed", "Sandboxed reader", "Decompiling optional (Ghidra)"],
    parts: [
{ kind: "callout", summary: (
<>
Files are read by separate helpers in their own sandboxes with no network. Nothing in the file is ever
          executed, and its contents are treated as data, not instructions. Files outside the agent's folders ask
          first, and protected files (keys, .env) are refused.
</>
) },
{ kind: "rows", rows: [
{ icon: ScanSearch, title: "What it reads", summary: "Formats, security flags, imports, strings, disassembly", body: (
<>
<p>The agent can examine EXE, DLL, SYS, ELF and Mach-O files without running them, with its <code>binary</code>{" "}
          tool: format and architecture, security flags (ASLR, DEP, Control Flow Guard), sections and their entropy
          (packed or encrypted code is close to 8), imports, exports, signature and debug information, strings,
          functions, hex, and x86/x64 disassembly with calls to Windows functions named.</p>
</>
) },
{ icon: Paperclip, title: "Attaching a program", summary: "In chat or by name in the agent", body: (
<>
<p>In chat and Agent mode, <strong>Attach…</strong> (or dropping a file) adds a program's analyzer report to
          your message. In Agent mode, files inside the session's folders can simply be named.</p>
</>
) },
{ icon: Code, title: "Decompiling to C", summary: "With Ghidra and a JDK you install", body: (
<>
<p><strong>Decompiling to C</strong> uses Ghidra, which you install yourself with a Java JDK (Settings → Binary
          analysis). The agent then has a <code>decompile</code> tool. The first decompile of a file takes minutes;
          later ones reuse its project.</p>
</>
) }
] }
    ],
  },
  {
    id: "documents",
    title: "Documents",
    group: "using",
    icon: Files,
    card: "Collections chats and the agent can search",
    summary: "Collections of your files that chats and the agent can search. Your originals are never changed.",
    chips: ["Text, code, PDF, Word", "Scanned PDFs read offline", "Keyword or meaning search"],
    parts: [
{ kind: "rows", rows: [
{ icon: FolderOpen, title: "Collections", summary: "What you can add and how it's read", body: (
<>
<p>Group your files into collections: text, Markdown, code, PDF and Word (DOCX). PDF and Word files are read
          by a sandboxed helper; your original files are never changed.</p>
</>
) },
{ icon: ScanText, title: "Scanned PDFs (OCR)", summary: "Read offline with Windows' text recognition", body: (
<>
<p>Scanned PDFs (pages that are only pictures) are read with Windows' built-in text recognition (OCR), inside
          the same sandbox and offline. It takes about a second a page, reads at most 300 pages, and marks each page
          "read by OCR". Check numbers and names that matter: OCR can misread them. Not available on Linux yet.</p>
</>
) },
{ icon: MessageSquare, title: "Using a collection", summary: "In a chat, or as the agent's search tool", body: (
<>
<p>Attach a collection to a chat, or give it to the agent when starting a session (as a search tool).</p>
</>
) },
{ icon: Search, title: "Keyword and meaning search", summary: "Meaning search needs an embedding model", body: (
<>
<p>Keyword search always works. Choosing an embedding model adds <em>meaning-based search</em>, which also
          finds passages worded differently from your question.</p>
</>
) },
{ icon: FileWarning, title: "Changed and missing files", summary: "Marked when you open the collection; Re-index reads again", body: (
<>
<p>Gardenwall remembers where each file came from (encrypted) and checks it when you open the collection. A
          changed file is marked, and <strong>Re-index</strong> reads it again from the same place. Nothing is re-read
          until you choose to. A file that was moved or deleted is marked "Not found" and keeps its passages until you
          remove it.</p>
</>
) }
] }
    ],
  },
  {
    id: "models",
    title: "Models",
    group: "using",
    icon: Box,
    card: "Library, downloads, template audit, vision",
    summary: "Download or import models, check them, and tune how each one runs on your computer.",
    chips: ["Checked before use", "Suggested settings", "Partly-in-RAM warning"],
    parts: [
{ kind: "rows", rows: [
{ icon: Download, title: "Download and import", summary: "From Hugging Face with checksums, or a file you have", body: (
<>
<p><strong>Download</strong> from Hugging Face (only files with a published checksum are offered; downloads
          resume if interrupted) or <strong>import</strong> a file, optionally with its expected SHA-256. Models
          larger than 50 GB are published in several parts; Gardenwall lists them once, downloads and checks every
          part, and keeps them together. To import a split model from disk, choose any of its parts; the others must be
          in the same folder with their original names.</p>
</>
) },
{ icon: ShieldAlert, title: "Chat-template audit", summary: "The publisher's prompt code, reviewed once before loading", body: (
<>
<p>The template is code from the model's publisher that formats every
          prompt. Gardenwall flags suspicious content; you review it once before loading.</p>
</>
) },
{ icon: Settings2, title: "Settings", summary: "Context, cache, faster generation and more, per model", body: (
<>
<p><strong>Settings</strong> in each model's side panel: context size, context cache, faster generation, the expert
          cache (Mixture-of-Experts models) and the image projector. Each has a <em>What is this?</em> explanation.</p>
</>
) },
{ icon: Brain, title: "Thinking", summary: "How long the model reasons before answering", body: (
<>
<p><strong>Thinking</strong> (for models that support it): how long the model reasons before answering:
          Model default, Medium, Low or Off. Less thinking answers sooner and leaves more room in the context.</p>
</>
) },
{ icon: Repeat, title: "If a model repeats itself", summary: "Gardenwall stops it and says so", body: (
<>
<p><strong>If a model repeats itself</strong>, Gardenwall stops it and says so under the reply. Heavily
          compressed models (2-bit versions) do this more often, especially with long thinking; try Regenerate or less
          thinking.</p>
</>
) },
{ icon: Gauge, title: "Suggest for this computer", summary: "Proposed settings with a reason for each", body: (
<>
<p><strong>Suggest for this computer</strong> looks at the model (size, architecture, built-in features) and
          your GPU and RAM, and proposes settings with the reason for each. You decide whether to apply them.</p>
</>
) },
{ icon: MemoryStick, title: "Partly in system RAM", summary: "Part of the model runs from slower RAM", body: (
<>
<p>Shown when a model is too big for GPU memory and part of it runs from
          RAM, which is slower.</p>
</>
) },
{ icon: TrendingDown, title: "Slowdown warning", summary: "When answers slow down or GPU memory runs low", body: (
<>
<p>If answers get much slower than usual or GPU memory runs low, a banner
          names the apps using the most GPU memory and offers lighter settings.</p>
</>
) }
] }
    ],
  },
  {
    id: "faster",
    title: "Faster generation, explained",
    group: "further",
    icon: Zap,
    card: "MTP and draft models, explained",
    summary: "Ways to get answers sooner with the same quality, and what each costs.",
    chips: ["Same answers, sooner", "MTP is automatic", "Costs some GPU memory"],
    parts: [
{ kind: "node", node: (
<>
<p>
          A model normally writes one token at a time. <strong>Speculative decoding</strong> lets a quick helper
          guess the next few tokens; the model checks all of them in one step and keeps only those it would have
          written anyway. The answer quality is the same; answers just arrive sooner when the guesses are good.
        </p>
</>
) },
{ kind: "table", caption: "The three ways to generate faster", rows: [
{ key: "Built-in prediction (MTP)", text: (
<>
Some models include an extra layer for this. Biggest gains
            (about 1.7-2.4x in Gardenwall's tests on a 27B model) for about 1 GiB of GPU memory and slightly slower
            prompt reading. It is on by default (<strong>Automatic</strong>) when the model has the layer and the whole
            model still fits in GPU memory with it and at least a 16,384-token context.
</>
) },
{ key: "Draft-free", text: (
<>
Guesses from text already in the conversation. No extra memory; helps most
            with repetitive output such as code edits.
</>
) },
{ key: "Draft model", text: (
<>
A small model from the same family guesses. Uses memory for the second
            model; Gardenwall checks that both share a tokenizer.
</>
) }
], note: (
<>
Draft-free and draft models are off unless you choose them; built-in prediction is automatic. The speed and the share of guesses kept are shown after each answer.
</>
) }
    ],
  },
  {
    id: "runtime",
    title: "Runtime and updates",
    group: "further",
    icon: RefreshCw,
    card: "The engine, and undoing an update",
    summary: "The engine that runs models, how it's checked, and how updates can be undone.",
    chips: ["Checked at every load", "Updates never install themselves", "Undo by switching back"],
    parts: [
{ kind: "rows", rows: [
{ icon: Cpu, title: "The runtime", summary: "Checked against its checksum at every load", body: (
<>
<p>The runtime is checked against its known checksum every time a model loads. <em>Test</em> on the Updates
          page loads a small model in the sandbox to confirm a runtime works.</p>
</>
) },
{ icon: RefreshCw, title: "Updates", summary: "Listed, never installed for you", body: (
<>
<p><strong>Updates</strong> lists newer versions of Gardenwall's parts and your models' sources. Checking
          never installs anything; previous runtime versions are kept so an update can be undone.</p>
</>
) },
{ icon: CalendarClock, title: "When to check", summary: "Each time it opens, on request, daily or weekly", body: (
<>
<p>Each time Gardenwall opens (recommended, so security fixes reach you
          quickly), only when you ask, or daily or weekly.</p>
</>
) }
] }
    ],
  },
  {
    id: "settings",
    title: "Settings",
    group: "reference",
    icon: Settings,
    card: "What each Settings page holds",
    summary: "What each Settings page holds.",
    chips: [],
    parts: [
{ kind: "table", caption: "Settings pages", rows: [
{ key: "Generation", text: (
<>
The default system prompt and sampling (temperature, top-p, top-k, min-p,
          maximum answer length). Empty fields use the model's defaults.
</>
) },
{ key: "Privacy", text: (
<>
Offline mode and lockdown mode.
</>
) },
{ key: "Web access, MCP servers and Skills", text: "See their own Help topics." },
{ key: "Binary analysis", text: "The built-in analyzer, and Ghidra with a Java JDK for decompiling to C." },
{ key: "Agent", text: "The shell, Python, Git, the step checkpoint, notifications, summarizing and retries." },
{ key: "Agent tools", text: (
<>
Folders with programs (such as Python or Node.js) the agent's commands may
          run, read-only. Launchers such as <span className="mono">npm</span>, <span className="mono">npx</span> and
          other .cmd files work too (Node.js 20.6 or newer), but installing packages doesn't, because commands have no
          network. Some installers, Node.js's included, set permissions the sandbox can't read; Settings then shows a
          one-time command to run as administrator that lets sandboxes read and run that folder without changing it.
          Until then, sessions leave that folder out.
</>
) },
{ key: "Activity log", text: (
<>
30 days of agent actions and approval decisions, without file contents or
          messages.
</>
) },
{ key: "Data", text: (
<>
Where Gardenwall keeps its data, and whether chats are encrypted. Back up makes one file, locked with a passphrase
          you choose, holding your chats, agent sessions, projects, documents and settings, plus your memories, skills
          folder and activity log if you tick them; models are downloaded again. What a backup leaves out stays as it
          is when you restore it. Restore shows what a backup holds before anything
          changes, then restarts Gardenwall. If the passphrase is lost, nobody can open the backup.
</>
) }
] },
// Every shortcut, from the list the Ctrl+K palette also shows (final review item 14).
{ kind: "table", caption: "Keyboard shortcuts", rows: SHORTCUTS.map((s) => ({ key: s.typed ? <code>{s.keys}</code> : <kbd>{s.keys}</kbd>, text: s.text })) }
    ],
  },
  {
    id: "glossary",
    title: "Glossary",
    group: "reference",
    icon: BookOpen,
    card: "Every term, in plain words",
    summary: "The words Help uses, in plain language.",
    chips: [],
    parts: [
{ kind: "node", node: (
<>
<dl className="glossary">
        <dt>Token</dt>
        <dd>The unit models read and write, roughly three quarters of a word.</dd>
        <dt>Context (window)</dt>
        <dd>How many tokens the model can see at once: the conversation, files and its own answers.</dd>
        <dt>GGUF</dt>
        <dd>The file format of models that llama.cpp runs.</dd>
        <dt>Quantization (Q4, Q6, Q8…)</dt>
        <dd>
          How many bits each weight is stored with. Fewer bits make a smaller, faster file with slightly lower quality.
        </dd>
        <dt>GPU memory (VRAM)</dt>
        <dd>The graphics card's own memory. Models run fastest when they fit in it entirely.</dd>
        <dt>KV cache (context cache)</dt>
        <dd>The memory holding the model's working state for the context; it grows with the context size.</dd>
        <dt>Mixture of Experts (MoE)</dt>
        <dd>
          A model made of many small expert networks, of which only a few run for each token, so it is fast for its
          size.
        </dd>
        <dt>Speculative decoding</dt>
        <dd>Guessing several tokens ahead and checking them in one step; see "Faster generation, explained".</dd>
        <dt>Sandbox</dt>
        <dd>An operating-system-enforced boundary that limits what a program can access.</dd>
        <dt>MCP</dt>
        <dd>Model Context Protocol: a standard way for programs to offer tools to AI agents.</dd>
      </dl>
</>
) }
    ],
  },
];

/** "Related topics" at the end of each topic (final review item 13). */
const RELATED: Record<string, string[]> = {
  start: ["choosing", "models", "security"],
  choosing: ["models", "faster", "start"],
  generation: ["models", "faster", "settings"],
  security: ["agent", "web", "settings"],
  chat: ["documents", "memory", "web"],
  agent: ["commands", "skills", "security"],
  commands: ["agent", "skills", "binaries"],
  skills: ["agent", "commands", "security"],
  memory: ["chat", "agent", "security"],
  web: ["chat", "agent", "security"],
  binaries: ["agent", "commands"],
  documents: ["chat", "memory"],
  models: ["choosing", "generation", "faster"],
  faster: ["models", "generation", "runtime"],
  runtime: ["models", "faster", "start"],
  settings: ["security", "generation", "glossary"],
  glossary: ["start", "settings"],
};
for (const t of TOPICS) t.related = RELATED[t.id];

/** What a link into Help opens: a guide entry (`id`) or a topic (`topic`); `seq` changes on every click. */
export interface HelpOpen {
  id?: string;
  topic?: string;
  seq: number;
}

/** The Help topic for each page with a "?" beside its title, and for each Settings category (final review item 12). */
export const PAGE_HELP: Record<string, string> = {
  memory: "memory",
  documents: "documents",
  updates: "runtime",
  models: "models",
};
export const SETTINGS_HELP: Record<string, string> = {
  generation: "generation",
  privacy: "security",
  web: "web",
  mcp: "skills",
  skills: "skills",
  binary: "binaries",
  agent: "agent",
  "agent-tools": "agent",
  activity: "settings",
  storage: "settings",
};
/** A topic's title, for the "?" button's label. */
export function helpTopicTitle(id: string): string {
  return TOPICS.find((t) => t.id === id)?.title ?? "Help";
}

export function HelpView({
  onSetup,
  guide,
  onDiagnostics,
}: {
  onSetup?: () => void;
  /** Opens the diagnostics report ("Report a problem"). */
  onDiagnostics?: () => void;
  /** What to open (seq changes on every click): a guide entry ("More in the guide"), or a topic (a page's "?"). */
  guide?: HelpOpen | null;
}) {
  const [query, setQuery] = useState("");
  /** The topic open in the reader (null: the home with topic cards). */
  const [topicId, setTopicId] = useState<string | null>(null);
  const [visible, setVisible] = useState<string[]>(TOPICS.map((t) => t.id));
  const pageRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const searching = query.trim() !== "";
  const topic = TOPICS.find((t) => t.id === topicId) ?? null;
  const [target, setTarget] = useState<string | null>(null);

  // "More in the guide" from the Models page: open the topic holding that
  // entry and unfold it.
  // A page's "?" (final review item 12): open that topic at its top.
  useEffect(() => {
    if (!guide) return;
    setQuery("");
    setTarget(null);
    if (guide.topic) {
      setTopicId(guide.topic);
      pageRef.current?.scrollTo({ top: 0 });
      return;
    }
    if (!guide.id) return;
    const id = guide.id;
    setTopicId(guideTopicOf(id));
    const t = window.setTimeout(() => setTarget(`help-guide-${id}`), 0);
    return () => window.clearTimeout(t);
  }, [guide]);

  // While searching, every topic is shown in full and filtered by its text
  // (folded rows included, which open by themselves when they match).
  useEffect(() => {
    const q = query.trim().toLowerCase();
    const root = bodyRef.current;
    if (!root || !q) {
      setVisible(TOPICS.map((t) => t.id));
      return;
    }
    const ids: string[] = [];
    for (const t of TOPICS) {
      const el = root.querySelector<HTMLElement>(`#help-${t.id}`);
      const match = (el?.textContent ?? "").toLowerCase().includes(q);
      if (el) el.hidden = !match;
      if (match) ids.push(t.id);
    }
    setVisible(ids);
  }, [query]);

  const open = (id: string | null) => {
    setQuery("");
    setTopicId(id);
    pageRef.current?.scrollTo({ top: 0 });
  };

  const search = (
    <label className="gw-help-search">
      <Search aria-hidden="true" size={15} />
      <input
        type="search"
        placeholder="Search help (for example: context, approvals, /plan)"
        aria-label="Search help"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
    </label>
  );

  // The home: grouped topic cards.
  if (!topic && !searching) {
    return (
      <div className="page gw-help-home" ref={pageRef}>
        <div className="gw-help-home-head">
          {search}
          {onSetup && (
            <span className="small muted gw-nowrap">
              New here or changed computers?{" "}
              <button className="link" onClick={onSetup}>
                Run the setup again
              </button>
            </span>
          )}
        </div>
        {HELP_GROUPS.map((g) => (
          <section key={g.id} className="gw-help-group">
            <h3 className="gw-group-title">{g.label}</h3>
            <div className="gw-help-cards">
              {TOPICS.filter((t) => t.group === g.id).map((t) => {
                const Icon = t.icon;
                return (
                  <button key={t.id} type="button" className="gw-help-card" onClick={() => open(t.id)}>
                    <Icon aria-hidden="true" size={18} strokeWidth={1.75} className="gw-help-card-icon" />
                    <span className="gw-help-card-title">{t.title}</span>
                    <span className="gw-help-card-text">{t.card}</span>
                  </button>
                );
              })}
            </div>
          </section>
        ))}
        {onDiagnostics && (
          <section className="gw-help-group gw-help-report" aria-labelledby="help-report-title">
            <h3 className="gw-group-title" id="help-report-title">
              Report a problem
            </h3>
            <p className="small muted">
              Something not working? Copy diagnostics gives you a report to share with a bug report: versions, hardware,
              settings, the loaded model, recent errors and the model server's last log lines. You read it first; paths,
              web addresses, your user name and anything that looks like a secret are replaced, and your chats and files
              are never in it. Gardenwall sends it nowhere.
            </p>
            <Button icon={Stethoscope} onClick={onDiagnostics}>
              Copy diagnostics…
            </Button>
          </section>
        )}
      </div>
    );
  }

  const shown = searching ? TOPICS : topic ? [topic] : [];
  return (
    <HelpTarget.Provider value={target}>
    <div className="page" ref={pageRef}>
      <div className="settings-layout gw-help-layout">
        <nav className="section-index" aria-label="Help topics">
          <button className="gw-help-back" onClick={() => open(null)}>
            <ArrowLeft aria-hidden="true" size={14} /> All topics
          </button>
          {HELP_GROUPS.map((g) => {
            const list = TOPICS.filter((t) => t.group === g.id && (!searching || visible.includes(t.id)));
            if (list.length === 0) return null;
            return (
              <div key={g.id} className="gw-help-nav-group">
                <div className="gw-help-nav-label">{g.label}</div>
                {list.map((t) => (
                  <button
                    key={t.id}
                    className={!searching && t.id === topic?.id ? "active" : undefined}
                    aria-current={!searching && t.id === topic?.id ? "page" : undefined}
                    onClick={() =>
                      searching
                        ? document.getElementById(`help-${t.id}`)?.scrollIntoView({ behavior: "smooth", block: "start" })
                        : open(t.id)
                    }
                  >
                    {t.title}
                  </button>
                ))}
              </div>
            );
          })}
        </nav>
        <div className="settings-body help" ref={bodyRef}>
          {search}
          {searching && visible.length === 0 && <p className="muted">No help topics match "{query}".</p>}
          {shown.map((t) => (
            <section key={t.id} id={`help-${t.id}`} className="settings-group gw-help-section">
              <TopicView topic={t} query={query} onOpen={open} titleOf={helpTopicTitle} />
            </section>
          ))}
        </div>
      </div>
    </div>
    </HelpTarget.Provider>
  );
}
