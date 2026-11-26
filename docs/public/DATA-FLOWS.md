# How data moves through Gardenwall

Gardenwall is one desktop program made of several parts that trust each other very differently. This page shows what each part can reach and what is never allowed. For every way data can leave your computer and how to turn it off, see [PRIVACY.md](../../PRIVACY.md).

## The parts

| Part | Trusted? | What it can reach |
|---|---|---|
| **The core** (Gardenwall's Rust backend) | Yes | Makes every decision: permissions, approvals, storage, network. The only part that can use the network. |
| **The window** | No | Shows the interface. Every request it makes is checked again by the core, and it names files by IDs the core gave it, never by paths. |
| **The model runtime** (llama.cpp) | No | Its own model files, read-only. No network, no other files. The core talks to it through a private pipe. |
| **The agent's commands** | No | The folders you granted (as private drive letters), the tools you turned on, read-only. No network. Secret files hidden. |
| **Tool servers (MCP)** | No | Each in its own sandbox with no network. Every tool call asks first. |
| **Readers** (web pages, documents, programs) | No | Only the bytes the core hands them and their own folder. No network. What they return is treated as untrusted text. |
| **Model downloader** | No | One staging folder. Its only connection out goes through the core, which allows Hugging Face only. The core checks every file afterwards. |
| **Speech** (voice input, read aloud) | No | Its own folder and, for voice input, its pinned model. No network. |

## What flows where

- **You → the core:** what you type, the files you attach or drop, the folders you pick in Windows' own folder dialog.
- **The core → the model:** the messages, attachments and tool results for the current conversation. Never application secrets.
- **The model → the core:** replies and proposed tool calls. Every call goes through the policy, your approval where needed, and the sandbox.
- **The core → your folders:** the agent's file edits, inside granted folders only, each recorded so it can be undone.
- **The core → the web:** only search words or page addresses that you approved (or that fall under a Research mode you chose for that session), to public HTTPS addresses, checked for anything that looks like a secret. Pages are read as text by a sandboxed reader.
- **The core → Hugging Face:** your model searches and downloads, when you start them. A fixed ranking request when you open the model list.
- **The core → GitHub:** the update check (one small file) on the schedule you choose, and runtime and app downloads you start. Everything downloaded is checked against a pinned hash or a signature.
- **The core → your disk:** chats, sessions, memories and settings, encrypted with a key Windows protects for your account.
- **The core → you:** approval cards, notices and system notifications. Notifications name the session and the event, never the model's words.

## Never allowed

- The model reaching the network, application secrets, or files other than its own.
- A command running outside the sandbox, or a model's output widening any permission.
- The agent's commands opening network connections.
- Reading or writing outside the granted folders without your approval.
- Web requests to local, private or plain-HTTP addresses.
- Your conversation history being sent anywhere by itself.
- Details about your computer in any request, except the hardware summary you can choose to include when asking for a model recommendation, shown word for word first.
- Keys (the chat key, a search API key) reaching the window, the model, logs or any address.
- The window loading anything from the internet that a model mentions. Links are copied, never opened.

## Web research, step by step

1. Web access exists only in research sessions (which have no folders), in chats where you turn the Web switch on, and in your own `/web` and `/read` commands.
2. Before a research session or Web chat starts, you choose: **Ask for each page**, or **Research mode**.
3. In *Ask for each page*, every search and page is shown to you first, exactly as it will be sent.
4. In *Research mode*, searches and public pages run without asking for that one session, still checked, limited and listed. It ends with the session.
5. Research mode only ever covers web requests. It can't approve a file change, a command or a secret read.
6. An agent working on a project can't browse. It can ask a research assistant a question you approve; the assistant can't see the project, and you can review its findings before the agent gets them.
7. Every session keeps a list of the sites it visited, recorded by Gardenwall, not by the model.

## What is kept

- The activity log records the kind of action, your decision, the result and the time, never content.
- Chats and sessions are kept until you delete them; deleting one deletes its visited-sites list too.
- Recordings for voice input are held in memory only and never saved.
