# Gardenwall security model

This page explains what Gardenwall protects, what it protects against, and how. It describes version 0.1.0 on Windows. To report a security problem, see [SECURITY.md](../../SECURITY.md).

## The principle

**The model is untrusted.** A language model can be poisoned in training, fine-tuned to misbehave, or steered by a web page or a file it reads. Gardenwall never relies on the model behaving well. Instead:

- The model can only *propose* actions. The trusted part of Gardenwall (its core) decides whether each one is allowed, and asks you when it isn't clearly safe.
- The operating system enforces the same limits a second time. Even if a decision in the core were wrong, the sandbox still holds.
- When a protection can't be put in place, the feature it guards is turned off. Gardenwall never falls back to running a model or a command without its sandbox.

## What Gardenwall protects

- Your computer and your files outside the folders you choose
- Passwords, keys, tokens and secret files, including those inside your project folders
- Your chats, agent sessions, documents and memories
- Gardenwall itself: its settings, its database and its update channel
- Other devices and services on your network

## What Gardenwall protects against

- Models that are poisoned, modified or simply wrong
- Malformed or malicious model files and chat templates
- A compromised model runtime
- Hostile web pages, documents and programs the model reads
- Files in a project that try to give the model instructions
- Tampered downloads and updates
- Tool servers (MCP) that misbehave or change after you accepted them

## How

**Separate processes with the least access.** The model runs in its own sandbox (an AppContainer on Windows) with no network and access only to its own model files. Web pages, documents and programs are read by separate sandboxed helpers, never by the core. Model downloads, voice input and read-aloud each run in sandboxes of their own.

**The agent works only where you allow it.** It reads and writes only inside the folders you grant, through file access that can't follow links or junctions out of them. Its commands run in a per-session sandbox with **no network**. When a step needs the internet, the agent asks you to run the command yourself and shows you exactly what it is.

**You approve what matters.** Writing outside your folders, reading secret files, changing files that make code run later (Git hooks, CI settings, editor tasks, package scripts), and risky commands all wait for you. Approval cards are built by the core from the exact operation, show hidden characters, and expire if you don't answer.

**Every change can be undone.** The agent's edits and the files its commands change are recorded per turn, so you can undo a turn or put a single file back.

**Secret files stay secret.** Files that look like credentials (`.env`, private keys, credential files) are hidden from the agent's commands and tool servers, and the agent's file tools ask before reading one. Commits the agent makes are checked for secrets before they're created.

**Nothing leaves without a reason you can see.** Web requests go through one gateway in the core: public HTTPS addresses only, no local or private addresses, and requests that look like they carry a secret are refused. Each request is approved, or covered by "Research mode", which you choose for one research session. Your conversation is never sent anywhere by itself. See [PRIVACY.md](../../PRIVACY.md) for every network use and how to turn it off.

**Untrusted content is marked as such.** Web pages and documents reach the model inside a clearly marked block, with hidden characters removed and text addressed to an AI flagged. After the agent reads web content, every change and command asks first.

**Models are checked before they load.** Only GGUF files are accepted. Each one is verified against its expected SHA-256, its structure is checked by Gardenwall's own parser, and its chat template is checked for hidden instructions. The llama.cpp runtime is a pinned build, verified again every time it starts.

**Updates are signed.** An update installs only if it verifies against Gardenwall's release key (kept by the developer, never on a build server or GitHub), the signature matches the version offered, and you confirm. Older versions are never offered as updates.

**Your data is encrypted on disk.** Chats, sessions, memories and keys are encrypted with a key that Windows protects for your account. Backups are locked with a passphrase you choose.

**One switch for a model you don't trust.** Lockdown is an emergency stop: after one confirmation it ends every agent session, kills their commands, stops web requests, research, tool servers and downloads, takes back the sandbox permissions and unloads the model. Only plain chat works until you turn it off.

## What always holds

1. A model's output can never create or widen a permission.
2. No command runs outside the sandbox. The sandbox, not the prompt, is the boundary.
3. No web request is sent without your approval, except under a Research mode you chose for one research session or chat.
4. Your conversation history is never sent to a search service by itself.
5. The model's process holds no application secrets.
6. Web requests can't reach your local network.
7. The activity log records what happened, never your content or credentials.

## Known limits

These are the edges you should know about:

- Approvals are shown and decided in the main window. It renders no HTML or remote content, but it isn't yet a separate, isolated window, so a compromised window could approve a pending request. A separate approval window is planned.
- Secret files are recognised by name. A secret stored under an ordinary file name isn't hidden.
- Detection of secrets in web requests and commits is pattern-based. It is a second line of defence, not a guarantee; your approval of exact requests is the main control.
- A command you run yourself from a "Run this yourself" card runs with your full rights, outside the sandbox. Read it before you run it.
- Exports and backups are outside Gardenwall's protection once saved (backups stay passphrase-locked).
- Windows protects the encryption key from other accounts and from offline copying, not from other programs running as you.
- Gardenwall can't contain bugs in your GPU driver or Windows itself. The Updates page tells you when your NVIDIA driver is older than the latest security fix.
- The installer isn't code-signed yet, so a first download can only be checked against the SHA-256 on the release page. Every update after that is signature-checked.
- macOS isn't supported (the app refuses to load models there rather than run them unprotected). On Linux, the command sandbox works when built from source; model loading isn't finished.

## How it is tested

The sandbox has escape test suites on Windows and Linux that try to read, write and connect where they shouldn't. Every parser of untrusted data has robustness tests on every change and coverage-guided fuzzing every week. Policy decisions, approvals, web rules, update signatures and backups have their own tests. Before each release, the source is reviewed for security issues and the installer is checked on a clean Windows machine.
