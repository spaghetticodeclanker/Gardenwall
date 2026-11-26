# Privacy

**Your models. Your data. Your permissions.**

Gardenwall runs language models on your own computer. It has no account, no telemetry, no analytics and no crash reporting. Nothing is sent to the people who make Gardenwall, and there is no Gardenwall server.

This page lists every way data can leave your computer, what each one sends, and how to turn it off. It describes Gardenwall 0.1.0 on Windows.

## What can use the network

Only the trusted part of Gardenwall (its core) can reach the network. The model, the agent's commands, MCP servers and the helpers that read pages and documents run in sandboxes with no network of their own.

| What | What it sends, and to whom | When | How to turn it off |
|---|---|---|---|
| **Update check** | One request for the release file (`latest.json`) to this project's GitHub Releases; the installer download only if you choose Download | Each time the app opens, by default | On the Updates page, choose Manual (or daily or weekly). Off in offline mode. |
| **Model search and downloads** | Your search words, and the repository and file names you pick, to Hugging Face. Downloads come from Hugging Face only, checked against the expected SHA-256. | Only when you search or start a download | Don't use them, or turn on offline mode |
| **Model ranking** (Find a model) | Fixed list settings only (kind, order, number of results), to Hugging Face; nothing about you or your computer | When you open the list; cached for 15 minutes | Offline mode |
| **Runtime downloads** | Requests for the llama.cpp and whisper.cpp builds this version pins, from GitHub; each file is checked against its pinned SHA-256 | Only when you install or update a runtime | Offline mode |
| **Speech models** (voice input) | Downloads of the two pinned speech models from Hugging Face | Only when you set up voice input | Don't set it up |
| **Web research** | The search words or page address the model or you wrote, to the search service you chose (your own SearXNG or Brave Search) or to the page's website | Only in research sessions, chats with the Web switch on, and your own `/web` and `/read`; each request asks first unless you chose "Research mode" for that session | Settings → Web access, offline mode or lockdown mode |
| **Hardware in a model question** | Your GPU's name and memory, system memory and Windows version, inside the research question of "Help me choose" | Only while "Include my hardware" stays ticked; you see the exact words before anything is sent | Untick it |

Every web request is checked before it leaves: public HTTPS addresses only, no private or local addresses, and requests that look like they carry a password or key are refused. Each session or chat keeps a list of the sites it visited.

Your chat history is never sent anywhere by itself. A web request carries only the query or address in it.

## What never uses the network

- **The model.** It runs in its own sandbox with no network and can read only its own files.
- **Agent commands.** They run in a sandbox with no network. When a command needs the internet (for example to download a project's packages), the agent shows you a "Run this yourself" card with the exact command; Gardenwall never runs it for you.
- **MCP servers** you add. Each runs in its own sandbox with no network, and every tool call asks first.
- **Voice input and read aloud.** Speech is recognised and spoken on your computer, in sandboxes with no network. Recordings are held in memory only (at most 5 minutes) and never saved.

**Offline mode** stops update checks, model search, downloads and web access. **Lockdown mode** turns on offline mode, turns off everything optional, and stops whatever is running.

## What stays on your computer

- **Chats, agent sessions, memories, drafts and visited-site lists** are stored in `%LOCALAPPDATA%\ai.gardenwall.desktop`, encrypted with a key that Windows protects for your account.
- **API keys** (such as a Brave Search key) are kept encrypted the same way, and are never shown to the model or written to logs.
- **The activity log** records what was approved or refused and when, never the content.
- **Notifications** show a session's name and what happened, never the model's words.

Deleting a chat or session deletes its messages and its visited-site list.

## Files you choose to save

Some actions write a file where you choose. Once saved, that file is yours to keep safe.

- **Exports** (a chat or session as Markdown, a web page or JSONL) contain the conversation in plain text.
- **Copy diagnostics** builds a report and shows it to you in full before you copy or save it. Gardenwall never sends it anywhere. It holds versions, Windows, GPU and memory details, the runtime and sandbox state, settings that change behaviour, the loaded model's name, recent model-server log lines and recent errors. It never holds prompts, replies, file contents, chat titles or MCP server details, and file paths, web addresses, email addresses, your name and home folder are replaced in it.
- **Backups** (`.gwbackup`) are locked with a passphrase you choose. Without it, the file can't be read.

## Uninstalling

The uninstaller asks whether to keep or remove your data, and removes the folder permissions Gardenwall gave its sandboxes either way.

## Questions and reports

For a privacy or security problem, use the private reporting described in [SECURITY.md](SECURITY.md). The technical details are in [how data moves through Gardenwall](docs/public/DATA-FLOWS.md) and the [security model](docs/public/SECURITY-MODEL.md).
