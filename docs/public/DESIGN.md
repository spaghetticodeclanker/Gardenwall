# Design principles and key decisions

This page explains how Gardenwall is built and why. It covers the choices that shape what the app does and doesn't do. For the security details, see the [security model](SECURITY-MODEL.md); for what leaves your computer, see [PRIVACY.md](../../PRIVACY.md).

## Principles

1. **The model is untrusted.** Every permission comes from Gardenwall's core and is enforced again by the operating system. Nothing a model says can widen what it may do.
2. **You stay in control.** You choose the folders, the tools and what may reach the network. Anything outside those choices asks you first, shown exactly as it will happen.
3. **Fail closed.** If a protection isn't available, the feature it protects is off. There is no "run it anyway" fallback.
4. **Local first.** Models run on your computer. Nothing is sent anywhere unless you start it or approve it, and there is no telemetry.
5. **Friendly, but not simplified away.** The defaults work for someone new to local AI, every trade-off is explained where you make it, and the details stay available for people who want them.

## A desktop app, not a server

Gardenwall is a single Windows desktop app (built with Tauri: a Rust core and a web-based interface). A desktop app can use the operating system's own sandboxing, keeps your data on your machine, and needs no account or server. Remote access from other devices is planned for later and will get its own security design before it is built.

## Every untrusted part in its own sandbox

On Windows, each untrusted part runs in its own AppContainer with a Job Object: the model runtime, the agent's commands, each tool server, the page and document reader, the program analyser, the model downloader, and the speech engines. Each gets only the folders it needs and, apart from the downloader's single checked route to Hugging Face, no network at all.

Folder permissions Gardenwall gives a sandbox are recorded in a journal, so they are removed even after a crash, and the uninstaller removes them too. On Linux the command sandbox uses Landlock and seccomp. macOS isn't supported yet, so the app refuses to load models there instead of running them unprotected.

## Models: checked, fitted, pinned

- **Only GGUF files.** Formats that can run code when loaded are not accepted. Each file is checked against its SHA-256 and its structure is read by Gardenwall's own bounds-checked parser.
- **Chat templates are reviewed.** A template is code that runs on every message, so each one is checked for hidden instructions and odd constructs before a model loads. Templates of well-known models are already reviewed.
- **Sized to your GPU.** Fit labels come from your GPU's measured free memory, and the context size is chosen to leave a safety margin, because running out of video memory can make generation many times slower.
- **Pinned runtimes.** llama.cpp builds are pinned by version and hash and verified every time they start. Known security advisories for a build are shown on the Updates page.
- **Faster when it fits.** Models with multi-token prediction use it automatically when it fits in memory, and fall back cleanly when it doesn't.

## The agent: small, faithful, contained

The coding agent follows the design of Pi, a minimal open-source coding agent: a short system prompt and a small set of tools (read, write, edit, list, find, search, and a shell). A small, well-understood tool set is easier to secure and works well with local models. Gardenwall adds the boundaries around it:

- It works only in the folders of a project, through file access that can't escape them.
- Its shell commands (PowerShell 7, Windows PowerShell 5.1 or the Command Prompt) run in the session's sandbox with no network. Each granted folder appears as a private drive letter, so the agent never sees the folders above it unless you turn that on.
- Commands can't change files that would run code later outside the sandbox (Git hooks, CI files, editor tasks). Anything a command creates there is held back for you to keep or discard.
- Programs a command leaves running are listed for you to stop or keep.
- Every turn can be undone.

## No network for the agent's commands

Giving commands a filtered network connection was considered and rejected. A command with network access can send your code anywhere, and filtering by site is hard to get right. Instead, when the agent needs something from the internet (usually a project's packages), it shows you a **Run this yourself** card with the exact command. For package downloads, Gardenwall offers its own standard command for the project type, with install scripts off and lock files respected. You run it; the agent then builds offline.

## Your tools, inside the sandbox

The agent can use the languages and build tools already installed on your PC: Rust, Python, Node.js, Java, .NET, Go, C/C++ with Visual Studio, PHP and Ruby. You turn each on for every project or for one project. Turned on, it is readable and runnable by the agent's sandbox only. The agent's caches are kept apart from yours, and pinned versions in a project are respected. Gardenwall finds and uses these tools; it doesn't install them.

Projects get help getting started: a guided **New project** dialog, and a **Ready to work?** check on each project's page that lists what's missing (toolchains, pinned versions, packages, Git, secret files in `.gitignore`) with the fix for each. It also lists the files in a project that run code automatically, such as install scripts and build scripts, so you know what a build will execute.

Some tools (Ruby, Gradle) need to see the names of the folders above a project. That is an opt-in setting, off by default: it lets the sandbox list names in those folders, never open or change anything in them.

## Approvals you can trust

An approval is a request stored by the core, shown on a card built only from that stored request, and answered once. A request expires after five minutes, and an approval can be used only once, for exactly the operation shown. The activity log records what was decided, never the content.

## Web research you review

Web access lives in research sessions, which have no project folders, and in chats where you turn the Web switch on. Every search and page is approved, or covered by Research mode for that one session. Pages are read as plain text by a sandboxed reader and given to the model as untrusted content. An agent working on a project reaches the web only through a research assistant that can't see the project, answering a question you approved. See [how data moves](DATA-FLOWS.md#web-research-step-by-step).

## Your data

Chats, agent sessions, memories and visited-site lists are encrypted per field, with a key Windows protects for your account. Memories are saved only by you. Model output is shown as restricted text: no HTML, no remote images, and links are copied rather than opened. Backups are a single file locked with a passphrase you choose.

## Updates you choose

Gardenwall checks for updates on a schedule you set, and checking never installs anything. Releases are built and signed on the developer's own computer, not on a hosted build service, and an update installs only after its signature checks out and you confirm. See [how releases are made](RELEASES.md).

## Lockdown

For a model that starts to look risky, lockdown is one switch (the shield in the top bar). After one confirmation it ends every agent session and its programs, stops web requests, research, tool servers and downloads, takes back sandbox permissions and unloads the model. Only plain chat works until you type UNLOCK, and nothing restarts by itself.

## Not built (for now)

- **Network access for agent commands**, for the reasons above.
- **Agents that start themselves** on a schedule or a file change, and **several sessions editing one project in separate working copies**. Both widen what an agent can do without you watching; they stay on hold.
- **General sub-agents.** Only the research assistant is built, because its boundaries (no project, web only, reviewed findings) are clear.
- **A local OpenAI-compatible API** for other apps. Planned, after its own security design.
- **Code mode** (the agent writing small programs that call its tools) is included but experimental and off by default.
