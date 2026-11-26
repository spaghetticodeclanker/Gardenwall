# Changelog

All notable changes to Gardenwall, newest first.

**Versioning.** Gardenwall uses [semantic versioning](https://semver.org). While it is at 0.x, a minor version (0.2, 0.3…) may change behaviour or data formats, and says so under "Upgrade notes". Data is always carried forward: an update moves your chats, sessions and settings to the new format automatically, and never needs you to export and re-import. Only the latest 0.x version gets security fixes (see `SECURITY.md`).

## 0.1.0 — 2026-11-26

The first public release: an early **preview** for Windows.

Gardenwall is an open-source, local-first, zero-trust AI platform that separates AI models from system authority. It finds a model that fits your GPU, chats with it, and puts it to work as a coding and research agent, while the model itself is treated as untrusted: every file it touches, every command it runs and every web request goes through a sandbox and your approval.

### Models
- **Find a model** suggests one for this computer from a reviewed list, with fit labels from your GPU's measured memory, a trade-off card and a compare table. You can also browse what people are downloading (with plain warnings) or search Hugging Face.
- Downloads run in their own sandbox that can reach only Hugging Face, in parallel parts with pause, resume and cancel, and are checked against their SHA-256 before use.
- Only GGUF files are accepted, and each one's structure is checked by Gardenwall's own parser. Chat templates are checked for hidden instructions before a model loads; 74 well-known templates are already reviewed.
- llama.cpp runtimes are pinned and verified again at every launch. The model runs in its own sandbox with no network, its context is fitted to your free GPU memory, and generation is faster with the model's own multi-token prediction when it has it and it fits.

### Chat
- Chats are saved encrypted on disk with a key protected by Windows.
- Edit a message as a new branch or in place, branch from any message, and export a chat as Markdown.
- Document chat with text, code, PDF and DOCX files, including scanned PDFs read by Windows' built-in OCR in a sandbox.
- Images for vision models.
- Model output is shown as restricted text: no HTML, no remote images, and links are copied, never opened.

### Coding agent
- A Pi-style agent works only in folders you choose. Its commands run in a sandbox with no network, using PowerShell 7, Windows PowerShell 5.1 or the Command Prompt.
- Writes outside the folders, secret files, files that make code run later (Git hooks, CI files, editor tasks) and risky commands wait for your approval. Secret files are hidden from commands and tool servers.
- Every change, by the agent's tools or its commands, can be undone, with a diff per file and a Put back button.
- Commands start without a pause in large projects: the copies Undo needs are made in the background from your first message, and finding what a command changed reads the folder listing instead of opening every file (about 1 second saved per command in a 10,000-file project).
- When a step needs the internet, the agent shows you the exact command on a "Run this yourself" card; Gardenwall never runs it.
- In PowerShell, a command's output and its programs' error messages reach the agent in the order they happened.
- A secret check before each of the agent's commits that tells types in code from real secrets and says which folder its findings are in, and Git for Windows as an optional read-only tool.
- Languages and build tools for the agent: Rust, Java (JDK), the .NET SDK, Go, and C/C++ with Visual Studio, found on your PC, can be added for every project (Settings → Agent) or for one project (its page), so the agent builds, runs and tests your code inside its sandbox, with no network. Their caches stay separate from yours, a project's Rust version pin is followed, and sessions start quickly because Gardenwall keeps the tools ready while it runs.
- A new project can get a Git repository in one click on the project page, or when the agent asks (Gardenwall creates it; Git can't do this from inside the sandbox). The agent is told which tools it has, so it doesn't suggest installing one you already have.
- **New project…** walks you through it on one card: a new or existing folder, what you're building (with the toolchain it needs, found on your PC or where to get it), a Git repository with Gardenwall's list of secret files in its `.gitignore` (on by default), and a first message for the agent, ready in the message box for you to edit or send.
- Projects with Rust build output work: the sandbox accepts hard links whose names all stay inside the project (Rust's incremental compiler makes them), and still refuses any that reach outside. `cargo new` no longer tries to make a Git repository inside the sandbox.
- **Lockdown is an emergency stop** (the shield in the top bar, Ctrl+K or Settings → Privacy): after one confirmation it ends every agent session at once (their commands and programs killed, their cards withdrawn, their permissions taken back), stops web requests, research, MCP servers and downloads, unloads the model, and tells you what it stopped. While it is on, only chat works, with no tools; you type UNLOCK to turn it off, and nothing restarts by itself. Offline mode now also ends web requests and downloads in progress.
- Gardenwall says at start-up when it's running as administrator, which it never needs: what it creates would belong to Administrators, and Git in the sandbox refuses those repositories.
- The agent can ask for a toolchain it needs: you decide on a card, and its sandbox restarts with it without losing the conversation. A toolchain that isn't installed shows where to get it. Adding one in Settings offers the same restart to open sessions.
- Python (including installs for your account only and Pythons from uv), Node.js (including fnm, nvm-windows and Volta; TypeScript from your project's own packages) and PHP join the toolchains. Python's temporary folders and virtual environments now work in the sandbox.
- **Folders above the project** (off by default; per project on its page, or for every project in Settings → Agent): when on, the agent's sandbox can see the names of the files and folders in each folder above the project, never their contents, so Ruby and Rails, Gradle builds for Java and Kotlin, and Java's temporary files work there. Windows' own folders (C:\ and C:\Users) need a one-time step you run as administrator; Gardenwall shows the whole script to paste into PowerShell, never saves it as a file another program could change first, and never asks for administrator rights itself.
- Fetching a project's packages takes one step: the agent asks, and Gardenwall shows you its standard command for the project (Rust, Node.js, Python, Go, .NET or PHP; install scripts off, lock files respected) to run yourself; then the agent builds offline. When Rust's `Cargo.lock` is missing a dependency added since the last build, the command adds it and keeps every version already locked, and the card says so. After you say it ran, Gardenwall looks for the packages itself before the agent builds, and tells the agent what's missing if the fetch didn't work. Projects with several parts (a web app in `apps/web`, a backend in `backend`) get one command per part. The command isn't offered while the folder holds files named like the programs it starts (which the Command Prompt would run instead) or an `.npmrc` that names a program; the agent's own "Run this yourself" cards warn about such files. The agent's commands can't change a .NET project's `.csproj`, `.props` or `.targets` files, which `dotnet restore` reads on your PC.
- **Ready to work?** on each project's page: what the project is and its parts, the versions it pins (`.nvmrc`, `.python-version`, `global.json`, `go.mod`, `rust-toolchain.toml`, `.tool-versions`, dev-container settings) compared with what's installed, toolchains that are missing or not turned on, packages not fetched yet (with the command to copy), Git, secret files missing from `.gitignore` and "Folders above the project", each with its fix. It only reads the project's files.
- **Runs by itself**: the project page lists the files that run code when something else happens, such as install scripts in `package.json`, `build.rs`, `conftest.py`, Gradle and MSBuild files, VS Code tasks that run when the folder opens, dev-container commands, installed Git hooks and Git settings that start programs.
- **Project instructions**: a project without an `AGENTS.md` gets a draft written from its files (how to build, test and fetch, pinned versions), for you to edit before saving.
- Agent sessions export as Markdown as well as a web page and JSONL. In Markdown exports (sessions and chats), image links and HTML tags in the model's text are made inert, so opening the file in a previewer never loads an address the model wrote.
- Skills, memories you choose to save, optional question cards, Send now to interrupt, Queue to send after the task, and a pause every 100 steps (adjustable) on long tasks.
- Home shows the loaded model, GPU and system memory, and the sessions that are working or waiting for you. System notifications name only the session and the event, never model text.

### Web research
- Search through your own SearXNG or a Brave Search key. Each request is approved, or covered by a research grant you choose for one research session.
- Pages are read as plain text by a sandboxed helper and reach the model marked as untrusted. A list of visited sites, recorded by Gardenwall, shows every search and page.
- An agent working on a project reaches the web only through research questions you approve, answered by an assistant that can't see the project; you can review the findings before the agent gets them.

### Tools
- MCP servers, each in its own offline sandbox. Every tool call asks first, and a new or changed tool waits for you to accept it.
- Binary analysis that describes programs without running them, with an optional Ghidra decompiler you install.
- Code mode (experimental, off by default).

### Voice
- Speak instead of typing: Whisper runs on this computer in a sandbox with no network, and the audio is never saved.
- Read replies aloud with Windows' voices.

### Updates and your data
- An update center with a schedule you choose. Checking never installs anything. App updates are verified against a signature made with an offline key, and install only when you confirm.
- Back up everything to one passphrase-locked file and restore it, also on another computer.
- Copy diagnostics: a redacted report for bug reports, shown in full before you copy or save it, and sent nowhere.
- Lockdown mode turns off everything optional for a model you don't trust; offline mode stops model search and downloads.
- When you uninstall, you choose whether to keep your data; sandbox permissions are always removed.
- Gardenwall's window opens straight away: permissions left from the last run are cleaned up in the background (the top bar says "Finishing cleanup…" while a new agent session waits for it). The read-and-run permission on the toolchain folders you turned on is kept between runs, for the agent's sandbox only, so it isn't removed and added again at every start; turning a toolchain off, lockdown and uninstalling remove it.
- Closing with X asks first only while something is running (the agent at work, a chat reply, a download), listing what closing ends; otherwise it closes at once and ends sessions properly.

### Known limitations
- Ruby (and Rails) and Gradle builds run in the agent's sandbox only with "Folders above the project" turned on; otherwise the agent reads and edits the code and you run those commands yourself.
- Windows only. macOS isn't supported (the app refuses to run models there rather than run them unprotected); on Linux the command sandbox works from source, but model loading isn't finished.
- The installer isn't code-signed, so Windows shows "Windows protected your PC" the first time: choose **More info → Run anyway**.
- Models and voice input need Microsoft's Visual C++ Redistributable (x64). Many PCs already have it; if yours doesn't, Gardenwall shows where to get it from Microsoft.
- Approvals are shown and decided in the main window, which renders no HTML or remote content but isn't yet a separate trusted window.
- Lockdown waits up to 10 seconds for running agent turns to end, and says so if one hasn't (its commands are already killed). Downloads stop at their next progress report rather than instantly.
- Secret files are found by name pattern; one created while a session runs is hidden at the next check.
- With the Command Prompt shell, `del` is refused; PowerShell's delete works.
- Undo for commands' changes skips package and build folders (such as `node_modules`, `target` and `vendor`), files over 4 MB and folders with more than 20,000 files; the agent's own file edits are always covered.
- Programs with a window, including web browsers, don't start from the agent's sandbox: the agent builds them and asks you to run them.
- PDFs that mix text pages and scanned pages are read from their text only.
- The research assistant, "Run this yourself" cards and question cards haven't been measured with Gardenwall's agent benchmark yet.
- No local OpenAI-compatible API server yet.

### Building from source
- `node scripts/doctor.mjs` checks a PC for everything needed to build Gardenwall and says how to fix what's missing; `node scripts/check.mjs` runs every check (with `--quick` for format, lints and the UI type-check). `docs/public/BUILDING.md` has the setup, and `rust-toolchain.toml` pins the compiler CI uses.

### Upgrade notes
- First release: nothing to upgrade. If you ran a development build, your data in `%LOCALAPPDATA%\ai.gardenwall.desktop` is kept, and an older copy in the Roaming folder is moved there automatically.
- From now on, installed copies offer new versions on the Updates page. The first real update will come with 0.1.1.
