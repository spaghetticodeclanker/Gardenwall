// Typed wrappers around Gardenwall's IPC commands. The backend validates
// every argument; these types only mirror its views.
import { Channel, invoke } from "@tauri-apps/api/core";

export interface ProjectorSearch {
  candidates: { repo: string; path: string; size: number; sameRepo: boolean }[];
  /** Repositories looked at. */
  searched: string[];
  /** What the search was based on (the base model or the model's name). */
  basedOn: string;
}

export interface Progress {
  done: number;
  total: number;
}

export interface FindingView {
  severity: "info" | "warning";
  kind: string;
  /** Untrusted text from the model file, centred on what was flagged. */
  excerpt: string | null;
  /** "high", "review" or "info" (empty in records from older audits). */
  risk?: string;
  /** Stable kind name: url, email, encoded, hidden, construct, large,
   * instruction, boilerplate, text or unknown. */
  code?: string;
  /** The flagged word, phrase or construct. */
  detail?: string | null;
  /** The whole passage (untrusted, plain text). */
  context?: string | null;
}

export type ModelSource =
  | { kind: "hub"; hub: string; repo: string; commit: string; path: string; license: string | null }
  | { kind: "local"; original_name: string; hash_verified: boolean };

export interface ModelRecord {
  sha256: string;
  displayName: string;
  size: number;
  source: ModelSource;
  architecture: string;
  contextLength: number | null;
  tensorCount: number;
  acquiredAtUnix: number;
  template: {
    present: boolean;
    requiresAcknowledgement: boolean;
    acknowledged: boolean;
    findings: FindingView[];
    /** The template supports thinking levels (reasoning_effort). */
    reasoningLevels?: boolean;
    /** The template can turn thinking off (enable_thinking). */
    thinkingToggle?: boolean;
    /** Which reviewed template this is ("the Qwen 3 template published in …"), if any. */
    reviewed?: string | null;
  };
  /** How much the model thinks before answering. */
  thinking?: Thinking;
  load: LoadOptions;
  /** Paired image projector (another model's SHA-256), for vision models. */
  mmproj?: string | null;
  /** Faster generation (speculative decoding); off unless chosen. */
  speculation?: Speculation;
  /** Experts per layer (more than zero for a Mixture-of-Experts model). */
  experts?: number | null;
  /** GPU cache (MiB) for MoE experts kept in system RAM; 0 is off. */
  moeCacheMib?: number;
  /** Built-in multi-token-prediction layers (more than zero: MTP available). */
  mtpLayers?: number | null;
  /** The parts of a split model; absent for a single file. */
  parts?: { file_name: string; sha256: string; size: number }[];
}

export type KvCache = "f16" | "q8_0";

export interface LoadOptions {
  /** null: the largest context that fits free GPU memory. */
  contextSize: number | null;
  kvCache: KvCache;
}

export interface Generation {
  defaultSystemPrompt: string;
  temperature: number | null;
  topP: number | null;
  topK: number | null;
  minP: number | null;
  maxTokens: number | null;
}

export interface RuntimeView {
  id: string;
  backend: string;
  downloadMb: number;
  installed: boolean;
}

export interface LoadedView {
  sha256: string;
  displayName: string;
  isolation: "sandboxed" | "unsandboxed";
  contextWindow: number;
  supportsTools: boolean;
  build: string | null;
  /** An image projector is loaded: images can be attached. */
  vision: boolean;
  /** Set when a meaningful part of the model runs from system RAM. */
  placement?: Placement | null;
  /** How automatic faster generation was decided on this load, if it applied. */
  autoMtp?: AutoMtp | null;
}

export type AutoMtp = "on" | "noRoomEstimated" | "noRoomMeasured";

export interface Placement {
  gpuGib: number;
  ramGib: number;
  /** Layers whose MoE experts are in system RAM, if the runtime said. */
  expertLayersInRam: number | null;
  /** [on GPU, total] layers, if the runtime said. */
  layers: [number, number] | null;
}

/** "19.6 GiB on the GPU, 22.1 GiB in system RAM (experts of 34 layers)". */
export function placementText(p: Placement): string {
  let text = `${p.gpuGib.toFixed(1)} GiB on the GPU, ${p.ramGib.toFixed(1)} GiB in system RAM`;
  if (p.expertLayersInRam) text += ` (experts of ${p.expertLayersInRam} layers)`;
  else if (p.layers && p.layers[0] < p.layers[1]) text += ` (${p.layers[1] - p.layers[0]} of ${p.layers[1]} layers)`;
  return text;
}

/** What turning lockdown on stopped (GW-019). */
export interface LockdownReport {
  stopped: string[];
  /** What couldn't be stopped or saved; lockdown is on regardless. */
  problems: string[];
}

export interface AppView {
  dataDir: string;
  offline: boolean;
  /** Lockdown mode: everything optional is off (offline is then on too). */
  lockdown: boolean;
  /** Leftover sandbox permissions from the last run are still being cleaned up; sessions start once it's done. */
  cleaningUp: boolean;
  generation: Generation;
  agentSettings: AgentSettings;
  sandboxAvailable: boolean;
  /** Startup events the user should know about (cleanups, data moves). */
  notices: string[];
  chatEncryption: "encrypted" | "unencrypted" | "locked";
  /** Folders with programs the agent may run, read-only. */
  agentToolFolders: string[];
  /** Tool folders the sandbox can't read yet (sessions leave them out). */
  agentToolFoldersUnusable?: string[];
  /** SearXNG URL for the agent's web search, if set. */
  searxngUrl: string | null;
  runtimes: RuntimeView[];
  models: ModelRecord[];
  loaded: LoadedView | null;
  /** PowerShell 7's pwsh.exe in its install folder, if installed (the agent's preferred shell). */
  powershell7: string | null;
  /** The all-users Python offered to the agent, if one is installed. */
  python: string | null;
  /** An all-users Python the sandbox can't read yet, if any. */
  pythonUnreadable: string | null;
  gitToAdd: string | null;
  /** The favourite model's SHA-256 (still in the library), if one is set. */
  favouriteModel: string | null;
  /** Load the favourite model when Gardenwall starts (Home is skipped). */
  loadOnLaunch: boolean;
  /** Settings → Voice (release plan features 7 and 8). */
  voiceSettings: VoiceSettings;
  /** Toolchains found on this PC (Settings → Agent tools; ADR-022). */
  toolchains: ToolchainView[];
  /** Microsoft's Visual C++ runtime files missing from Windows (the engines need them). */
  vcRuntimeMissing: string[];
}

export interface VoiceSettings {
  /** A Windows voice's identifier; null uses Windows' default. */
  readVoice: string | null;
  /** 0.5 to 3.0; 1 is normal speed. */
  readRate: number;
  readAuto: boolean;
  /** Voice input's language (a whisper.cpp code such as "en"). */
  inputLanguage: string;
  /** Tidy transcripts with the loaded chat model. */
  inputCleanup: boolean;
  /** Which recogniser: the GPU when it has room (auto), always the GPU, always the processor, or off. */
  inputEngine: "auto" | "gpu" | "cpu" | "off";
}

export interface DictationStatus {
  supported: boolean;
  gpuPresent: boolean;
  gpuReady: boolean;
  cpuReady: boolean;
  recording: boolean;
  settingUp: boolean;
  downloadBytes: number;
}

export interface Dictated {
  text: string;
  raw: string;
  cleaned: boolean;
}

export interface SetupProgress {
  step: string;
  done: number;
  total: number;
}

export interface VoiceInfo {
  id: string;
  name: string;
  language: string;
}

export interface HubListing {
  repo: string;
  downloads: number;
  likes: number;
  gated: boolean;
}

export type Fit = "gpu" | "partly" | "tooBig" | "unknown";

/** What choosing a file means, in words (built by the core). */
export interface TradeOff {
  runs: string;
  speed: string;
  room: string;
  larger: string | null;
  smaller: string | null;
}

export interface Quant {
  /** As written in the file name, if found. */
  label: string | null;
  bits: number | null;
  quality: string | null;
  known: boolean;
}

export interface RepoFile {
  path: string;
  /** The whole model's size (all parts of a split model). */
  size: number;
  /** > 1: a split model, listed once with its total size. */
  parts: number;
  role: "model" | "projector" | "draft";
  quant: Quant;
  fit: Fit;
  tight: boolean;
  label: string;
  best: boolean;
  whyBest: string | null;
  card: TradeOff | null;
}

export interface RepoView {
  repo: string;
  commit: string;
  license: string | null;
  gated: boolean;
  files: RepoFile[];
  skipped: number;
  /** The computer the fit labels are for. */
  hardware: string;
  fitNote: string;
  quantAbout: string;
  knownPublisher: boolean;
  /** New, few downloads, gated: as in the ranking. */
  warnings: string[];
}

// ---------- model picker (ROADMAP Planned features §6) ----------

export type Purpose = "chat" | "coding" | "images";
export type Priority = "quality" | "balanced" | "speed";
export interface Answers {
  purpose: Purpose;
  priority: Priority;
  longDocuments: boolean;
}

export interface StarterEntry {
  id: string;
  name: string;
  repo: string;
  path: string;
  commit: string;
  sha256: string;
  size: number;
  /** With the image projector, for a vision entry. */
  total: number;
  projector: boolean;
  licence: string;
  publisher: string;
  kind: "general" | "coding" | "vision" | "small";
  uses: Purpose[];
  summary: string;
  reviewed: string;
  fit: Fit;
  tight: boolean;
  label: string;
  card: TradeOff;
  installed: boolean;
}

export interface StarterTier {
  minGib: number;
  label: string;
  recommended: string;
  faster: string;
  vision: string | null;
}

export interface StarterView {
  hardware: string;
  /** This computer's tier; null when GPU memory is unknown. */
  tier: number | null;
  tiers: StarterTier[];
  entries: StarterEntry[];
  reviewed: string;
  about: string;
  stale: string | null;
}

export interface Chosen {
  choices: { id: string; why: string }[];
  note: string | null;
}

export interface RankingChoice {
  id: string;
  label: string;
}

export interface RankingRow {
  repo: string;
  publisher: string;
  knownPublisher: boolean;
  /** YYYY-MM-DD */
  created: string | null;
  downloads: number | null;
  likes: number | null;
  licence: string | null;
  gated: boolean | null;
  warnings: string[];
}

export interface RankingView {
  kinds: RankingChoice[];
  sorts: RankingChoice[];
  kind: string;
  sort: string;
  note: string | null;
  intro: string;
  about: string;
  rows: RankingRow[];
}

export interface ResearchPrompt {
  withHardware: string;
  withoutHardware: string;
  hardwareSentence: string;
  /** What stops a research session now (empty: ready). */
  missing: { what: "model" | "tools" | "provider" | "offline"; text: string }[];
}

export type ChatEvent =
  | { type: "content"; text: string }
  | { type: "reasoning"; text: string }
  /** Web switch on: what the reply is doing ("Searching: …"). Plain text. */
  | { type: "step"; text: string };

export interface MessageStats {
  model: string | null;
  finishReason: string | null;
  promptTokens: number | null;
  completionTokens: number | null;
  totalTokens: number | null;
  contextWindow: number | null;
  promptPerSecond: number | null;
  generationPerSecond: number | null;
  ttftMs: number | null;
  /** Document passages quoted for this answer. */
  sources?: { file: string; part: number }[];
  /** Speculative decoding: tokens guessed, and how many the model kept. */
  draftTokens?: number | null;
  draftAccepted?: number | null;
}

export interface StoredMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  reasoning: string;
  stats: MessageStats | null;
  createdAt: number;
  /** Images the user attached (their own data, shown in the trusted UI). */
  images?: { mime: string; base64: string }[];
}

/** An image chosen in the core's file picker, held there until sent. */
export interface BinarySettingsView {
  ghidraDir: string | null;
  ghidraVersion: string | null;
  javaHome: string | null;
  javaDetected: string | null;
  ready: boolean;
  problem: string | null;
}

/** A file attached to the next message, held by the core until sent. */
export type Attachment =
  | ({ kind: "image" } & PendingImage)
  | {
      kind: "file";
      token: string;
      name: string;
      size: number;
      /** How it is given to the model. */
      what: "text" | "document" | "program" | "data" | "web";
      summary: string;
    };

/** Files attached at once (picked or dropped), and those that weren't. */
export interface AttachReport {
  attachments: Attachment[];
  failed: [string, string][];
}

export interface PendingImage {
  token: string;
  name: string;
  size: number;
  preview: string;
}

export interface ConversationSummary {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  /** The chat this one continues from a handoff, if it still exists. */
  parentId: string | null;
}

export interface Conversation extends ConversationSummary {
  systemPrompt: string;
  messages: StoredMessage[];
}

export interface SendResult {
  message: StoredMessage | null;
  cancelled: boolean;
  /** The message as typed, when it was stopped before any answer (taken back out of the chat). */
  withdrawn: string | null;
  /** Set when this reply was much slower than earlier ones, or the GPU's memory is nearly full. */
  gpuWarning: GpuWarning | null;
}

function progressChannel(onProgress: (p: Progress) => void): Channel<Progress> {
  const channel = new Channel<Progress>();
  channel.onmessage = onProgress;
  return channel;
}

export const api = {
  getState: () => invoke<AppView>("get_state"),
  setOffline: (offline: boolean) => invoke<void>("set_offline", { offline }),
  setFavouriteModel: (sha256: string | null, loadOnLaunch: boolean) =>
    invoke<void>("set_favourite_model", { sha256, loadOnLaunch }),
  systemMemory: () => invoke<SystemMemory>("system_memory"),
  /** Chats and agent sessions containing the text (searched in memory by the core). */
  searchHistory: (query: string) => invoke<HistoryHits>("search_history", { query }),
  /** Unsent text kept for a composer (`owner`, e.g. `chat:<id>`). */
  getDraft: (owner: string) => invoke<string | null>("get_draft", { owner }),
  setDraft: (owner: string, text: string) => invoke<void>("set_draft", { owner, text }),
  /** On: stops everything and reports what it stopped. Off: needs "UNLOCK". */
  setLockdown: (lockdown: boolean, confirm?: string) =>
    invoke<LockdownReport | null>("set_lockdown", { lockdown, confirm: confirm ?? null }),
  installRuntime: (buildId: string, onProgress: (p: Progress) => void) =>
    invoke<void>("install_runtime", { buildId, onProgress: progressChannel(onProgress) }),
  importModel: (expectedSha256: string | null, onProgress: (p: Progress) => void) =>
    invoke<ModelRecord | null>("import_model", {
      expectedSha256,
      onProgress: progressChannel(onProgress),
    }),
  acknowledgeTemplate: (sha256: string) => invoke<ModelRecord>("acknowledge_template", { sha256 }),
  deleteModel: (sha256: string) => invoke<void>("delete_model", { sha256 }),
  searchHub: (query: string) => invoke<HubListing[]>("search_hub", { query }),
  listRepoFiles: (repo: string, vramGib: number | null = null) => invoke<RepoView>("list_repo_files", { repo, vramGib }),
  starterModels: (vramGib: number | null) => invoke<StarterView>("starter_models", { vramGib }),
  chooseModel: (answers: Answers, vramGib: number | null) => invoke<Chosen>("choose_model", { answers, vramGib }),
  /** Download a starter entry exactly as reviewed (its pinned commit and hash). */
  downloadStarter: (id: string, onProgress: (p: Progress) => void) =>
    invoke<ModelRecord>("download_starter", { id, onProgress: progressChannel(onProgress) }),
  hubRanking: (kind: string, sort: string) => invoke<RankingView>("hub_ranking", { kind, sort }),
  modelResearchPrompt: (answers: Answers | null) => invoke<ResearchPrompt>("model_research_prompt", { answers }),
  researchRepositories: (sid: string) => invoke<string[]>("research_repositories", { sid }),
  downloadModel: (repo: string, path: string, onProgress: (p: Progress) => void) =>
    invoke<ModelRecord>("download_model", { repo, path, onProgress: progressChannel(onProgress) }),
  /** Pause a running download; what was downloaded is kept, and downloading it again resumes. A starter entry is paused by its `path`. */
  pauseDownload: (path: string) => invoke<void>("pause_download", { path }),
  /** Cancel a download for good: a running one stops, and its partial file is deleted (also when paused). */
  cancelDownload: (path: string) => invoke<void>("cancel_download", { path }),
  loadModel: (sha256: string, options: LoadOptions) =>
    invoke<LoadedView>("load_model", { sha256, contextSize: options.contextSize, kvCache: options.kvCache }),
  unloadModel: () => invoke<void>("unload_model"),
  setGeneration: (generation: Generation) => invoke<void>("set_generation", { generation }),
  setAgentSettings: (settings: AgentSettings) => invoke<void>("set_agent_settings", { settings }),
  /** Queue a message for the running agent (delivered after the current step). */
  /** Send now: cuts the reply being written and delivers `text` next (a running tool finishes first). */
  agentSteer: (sid: string, text: string) => invoke<void>("agent_steer", { sid, text, now: true }),
  /** The agent's open question card, if it is waiting on one. */
  agentQuestion: (sid: string) => invoke<QuestionCard | null>("agent_question", { sid }),
  /** Answer the card (one answer per question) or skip it (`null`). */
  agentAnswer: (sid: string, id: number, answers: string[] | null) =>
    invoke<void>("agent_answer", { sid, id, answers }),
  /** The agent's open "Run this yourself" card, if it is waiting on one. */
  agentRunRequest: (sid: string) => invoke<RunCard | null>("agent_run_request", { sid }),
  /** Tell the agent the user ran the command (with optional pasted output) or won't. */
  agentResearchCard: (sid: string) => invoke<ResearchCardView | null>("agent_research_card", { sid }),
  /** Approve (with the edited text) or decline the question; send or discard the findings. */
  agentResearchAnswer: (sid: string, id: number, approve: boolean, text: string | null) =>
    invoke<void>("agent_research_answer", { sid, id, approve, text }),
  agentRunAnswer: (sid: string, id: number, done: boolean, pasted: string | null) =>
    invoke<void>("agent_run_answer", { sid, id, done, pasted }),
  /** Re-read AGENTS.md/CLAUDE.md, skills and commands; apply Settings → Agent. */
  agentReload: (sid: string) => invoke<AgentView>("agent_reload", { sid }),
  /** Run the user's own command in the session sandbox (`!cmd`; `share: false` for `!!cmd`). */
  agentUserShell: (sid: string, command: string, share: boolean) =>
    invoke<{ output: string; failed: boolean }>("agent_user_shell", { sid, command, share }),
  /** Files in the granted folders matching the text after `@`. */
  agentCompletePath: (sid: string, query: string) => invoke<string[]>("agent_complete_path", { sid, query }),
  listConversations: () => invoke<ConversationSummary[]>("list_conversations"),
  createConversation: () => invoke<ConversationSummary>("create_conversation"),
  getConversation: (id: string) => invoke<Conversation>("get_conversation", { id }),
  renameConversation: (id: string, title: string) => invoke<void>("rename_conversation", { id, title }),
  setConversationPrompt: (id: string, prompt: string) =>
    invoke<void>("set_conversation_prompt", { id, prompt }),
  deleteConversation: (id: string) => invoke<void>("delete_conversation", { id }),
  /** A new chat with this one's messages before `beforeMessageId` (all when null); the original is unchanged. */
  branchConversation: (id: string, beforeMessageId: number | null) =>
    invoke<ConversationSummary>("branch_conversation", { id, beforeMessageId }),
  /** Save the chat as Markdown where the user picks; the path, or null if cancelled. */
  exportConversation: (id: string) => invoke<string | null>("export_conversation", { id }),
  truncateConversation: (id: string, fromMessageId: number) =>
    invoke<void>("truncate_conversation", { id, fromMessageId }),
  /** `content: null` regenerates the reply to the last user message. */
  sendMessage: (
    requestId: number,
    conversationId: string,
    content: string | null,
    onEvent: (e: ChatEvent) => void,
    images: string[] = [],
    files: string[] = [],
  ) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<SendResult>("send_message", { requestId, conversationId, content, images, files, onEvent: channel });
  },
  /** Opens the native file picker in the trusted core; any file can be attached. */
  attachFiles: () => invoke<AttachReport>("attach_files"),
  discardAttachment: (token: string) => invoke<void>("discard_attachment", { token }),
  binarySettings: () => invoke<BinarySettingsView>("binary_settings"),
  /** Opens the native folder picker for "ghidra" or "java"; "detectJava" and "clear" need none. */
  setBinaryFolder: (which: "ghidra" | "java" | "detectJava" | "clear") =>
    invoke<BinarySettingsView>("set_binary_folder", { which }),
  /** Sends the bytes of the user's paste; the core checks and holds them. */
  attachPastedImage: (bytes: Uint8Array) => invoke<PendingImage>("attach_pasted_image", bytes),
  /** Files dropped on the window arrive here; the core reads the OS-reported paths. */
  watchDroppedFiles: (onDrop: (d: AttachReport) => void) => {
    const channel = new Channel<AttachReport>();
    channel.onmessage = onDrop;
    return invoke<string>("watch_dropped_files", { onDrop: channel });
  },
  unwatchDroppedFiles: (token: string) => invoke<void>("unwatch_dropped_files", { token }),
  /** Image projectors (mmproj) on the hub that may fit this model ("Add vision"). */
  findProjectors: (sha256: string) => invoke<ProjectorSearch>("find_projectors", { sha256 }),
  /** Remember a model's context size and cache precision for its next load. */
  setModelLoad: (sha256: string, contextSize: number | null, kvCache: KvCache) =>
    invoke<ModelRecord>("set_model_load", { sha256, contextSize, kvCache }),
  setModelProjector: (sha256: string, projector: string | null) =>
    invoke<ModelRecord>("set_model_projector", { sha256, projector }),
  setModelSpeculation: (sha256: string, speculation: Speculation) =>
    invoke<ModelRecord>("set_model_speculation", { sha256, speculation }),
  setModelExpertCache: (sha256: string, mib: number) =>
    invoke<ModelRecord>("set_model_expert_cache", { sha256, mib }),
  suggestSettings: (sha256: string) => invoke<Suggestion>("suggest_settings", { sha256 }),
  setModelThinking: (sha256: string, thinking: Thinking) =>
    invoke<ModelRecord>("set_model_thinking", { sha256, thinking }),
  applySuggestedSettings: (sha256: string) => invoke<ModelRecord>("apply_suggested_settings", { sha256 }),
  cancelChat: (requestId: number) => invoke<void>("cancel_chat", { requestId }),
  listPendingApprovals: () => invoke<PendingApproval[]>("list_pending_approvals"),
  /** The open agent sessions, oldest first. */
  agentSessions: () => invoke<SessionCard[]>("agent_sessions"),
  agentDashboard: () => invoke<DashboardRow[]>("agent_dashboard"),
  /** Without `since`, returns only the newest number (no old cards replayed). */
  agentAttention: (since: number | null) =>
    invoke<{ latest: number; events: AttentionEvent[]; open?: string | null }>("agent_attention", { since }),
  /** `grant`: also allow this kind of request for the rest of the session (web access only). */
  decideApproval: (proposalId: string, approve: boolean, grant = false) =>
    invoke<void>("decide_approval", { proposalId, approve, grant }),
  setSearxngUrl: (url: string | null) => invoke<void>("set_searxng_url", { url }),
  getAuditLog: (limit?: number) => invoke<AuditEntry[]>("get_audit_log", { limit: limit ?? null }),
  agentState: (sid: string) => invoke<AgentView>("agent_state", { sid }),
  /** Opens the native folder picker in the trusted core. */
  addAgentToolFolder: () => invoke<void>("add_agent_tool_folder"),
  addGitToolFolder: () => invoke<void>("add_git_tool_folder"),
  removeAgentToolFolder: (path: string) => invoke<void>("remove_agent_tool_folder", { path }),
  /** Opens the native folder picker in the trusted core. */
  /** Opens the native folder picker; `collection` gives the agent search_documents. */
  /** Start a session in a project (its folders), or with `null` choose folders, which become a new project. */
  agentStart: (project: string | null, collection: string | null = null, memories = true) =>
    invoke<AgentView | null>("agent_start", { project, collection, memories }),
  listProjects: () => invoke<Project[]>("list_projects"),
  /** Opens the native folder picker; the folders become a new project. */
  createProject: () => invoke<Project | null>("create_project"),
  newProjectPick: (parent: boolean) => invoke<PickedFolder | null>("new_project_pick", { parent }),
  newProjectOptions: (recheck: boolean) => invoke<NewProjectOptions>("new_project_options", { recheck }),
  createGuidedProject: (request: NewProjectRequest) =>
    invoke<{ project: Project; starter: string; notes: string[] }>("create_guided_project", { request }),
  renameProject: (id: string, name: string) => invoke<void>("rename_project", { id, name }),
  /** Opens the native folder picker to add folders to a project. */
  addProjectFolders: (id: string) => invoke<Project>("add_project_folders", { id }),
  removeProjectFolder: (id: string, index: number) => invoke<Project>("remove_project_folder", { id, index }),
  deleteProject: (id: string) => invoke<void>("delete_project", { id }),
  /** Secret files in each of a project's folders, and what was moved to the secrets folder. */
  projectSecrets: (id: string) => invoke<FolderSecrets[]>("project_secrets", { id }),
  /** Move secret files (named by their relative path) of the folder at `index` out of the project. */
  moveProjectSecrets: (id: string, index: number, files: string[]) =>
    invoke<FolderSecrets>("move_project_secrets", { id, index, files }),
  /** For each git folder of a project, the secret patterns its ignore rules don't cover. */
  projectGitignore: (id: string) => invoke<IgnoreGap[]>("project_gitignore", { id }),
  /** Add the missing lines to that folder's .gitignore; returns how many. */
  addProjectGitignore: (id: string, index: number) => invoke<number>("add_project_gitignore", { id, index }),
  /** Put moved files back in the project folder (all of them when `files` is empty). */
  restoreProjectSecrets: (id: string, index: number, files: string[]) =>
    invoke<FolderSecrets>("restore_project_secrets", { id, index, files }),
  agentSetMemories: (sid: string, on: boolean) => invoke<AgentView>("agent_set_memories", { sid, on }),
  listMemories: () => invoke<MemoriesView>("list_memories"),
  addMemory: (scope: MemoryScope, text: string) => invoke<MemoriesView>("add_memory", { scope, text }),
  /** Opens the native folder picker in the trusted core; null when cancelled. */
  pickMemoryFolder: () => invoke<string | null>("pick_memory_folder"),
  updateMemory: (id: string, text: string, inChat: boolean, inAgent: boolean) =>
    invoke<MemoriesView>("update_memory", { id, text, inChat, inAgent }),
  deleteMemory: (id: string) => invoke<MemoriesView>("delete_memory", { id }),
  deleteMemories: (ids: string[]) => invoke<MemoriesView>("delete_memories", { ids }),
  chatMemories: (conversation: string) => invoke<boolean>("chat_memories", { conversation }),
  setChatMemories: (conversation: string, on: boolean) =>
    invoke<null>("set_chat_memories", { conversation, on }),
  agentEnd: (sid: string) => invoke<void>("agent_end", { sid }),
  agentStop: (sid: string) => invoke<void>("agent_stop", { sid }),
  agentUndo: (sid: string) => invoke<string[]>("agent_undo", { sid }),
  /** What the session changed with its file tools, as diffs. Refused while a turn runs. */
  agentChanges: (sid: string) => invoke<FolderChanges[]>("agent_changes", { sid }),
  /** Put one changed file back to how it was before the session; returns what remains. */
  agentRevertFile: (sid: string, folder: string, path: string) =>
    invoke<FolderChanges[]>("agent_revert_file", { sid, folder, path }),
  agentSetPlanMode: (sid: string, on: boolean) => invoke<AgentView>("agent_set_plan_mode", { sid, on }),
  agentStopBackground: (sid: string) => invoke<AgentView>("agent_stop_background", { sid }),
  agentRestartSandbox: (sid: string) => invoke<AgentView>("agent_restart_sandbox", { sid }),
  agentInstallCheck: (sid: string) => invoke<string | null>("agent_install_check", { sid }),
  agentInstallDismiss: (sid: string) => invoke<void>("agent_install_dismiss", { sid }),
  /** Fill in an MCP prompt; the text is for the composer, for the user to review. */
  agentMcpPrompt: (sid: string, command: string, args: string) =>
    invoke<string>("agent_mcp_prompt", { sid, command, args }),
  // Web research (ADR-018).
  agentStartResearch: (mode: WebMode) => invoke<AgentView>("agent_start_research", { mode }),
  agentSetWebMode: (sid: string, mode: WebMode) => invoke<AgentView>("agent_set_web_mode", { sid, mode }),
  agentVisits: (sid: string) => invoke<VisitsView>("agent_visits", { sid }),
  chatVisits: (conversationId: string, answerId: number | null) =>
    invoke<VisitsView>("chat_visits", { conversationId, answerId }),
  /** Run a typed `/web` or `/read` in the core; the result is an attachment. */
  chatWeb: (conversationId: string, command: "web" | "read", argument: string) =>
    invoke<Attachment>("chat_web", { conversationId, command, argument }),
  chatWebState: (conversationId: string) => invoke<ChatWebView>("chat_web_state", { conversationId }),
  /** Turn a chat's Web switch on in `mode`, or off with null. */
  chatSetWeb: (conversationId: string, mode: WebMode | null) =>
    invoke<ChatWebView>("chat_set_web", { conversationId, mode }),
  webStatus: () => invoke<WebStatus>("web_status"),
  setWebSettings: (s: WebSettings) => invoke<WebStatus>("set_web_settings", { ...s }),
  /** Save the Brave key (kept wrapped by Windows), or remove it with null. */
  setBraveKey: (key: string | null) => invoke<WebStatus>("set_brave_key", { key }),
  webTestConnection: () => invoke<ConnectionTest>("web_test_connection"),
  webFindSearxng: () => invoke<string | null>("web_find_searxng"),
  agentCompact: (sid: string, focus: string | null, onEvent: (e: AgentUiEvent) => void) => {
    const channel = new Channel<AgentUiEvent>();
    channel.onmessage = onEvent;
    return invoke<boolean>("agent_compact", { sid, focus, onEvent: channel });
  },
  agentClear: (sid: string) => invoke<AgentView>("agent_clear", { sid }),
  agentContext: (sid: string) => invoke<ContextView>("agent_context", { sid }),
  agentInstructions: (sid: string) => invoke<string>("agent_instructions", { sid }),
  agentSaveInstructions: (sid: string, text: string) => invoke<void>("agent_save_instructions", { sid, text }),
  listSkills: () => invoke<SkillView[]>("list_skills"),
  /** Opens the native folder picker to choose a skill folder (with SKILL.md). */
  addSkill: () => invoke<SkillView[]>("add_skill"),
  removeSkill: (name: string) => invoke<SkillView[]>("remove_skill", { name }),
  listMcpServers: () => invoke<McpServerView[]>("list_mcp_servers"),
  /** Opens the native file picker in the trusted core to choose the program. */
  addMcpServer: (name: string) => invoke<McpServerView[]>("add_mcp_server", { name }),
  setMcpArgs: (id: string, args: string[]) => invoke<McpServerView[]>("set_mcp_args", { id, args }),
  setMcpEnv: (id: string, env: { name: string; value: string }[]) => invoke<McpServerView[]>("set_mcp_env", { id, env }),
  setMcpEnabled: (id: string, enabled: boolean) => invoke<McpServerView[]>("set_mcp_enabled", { id, enabled }),
  /** Opens the native folder picker in the trusted core. */
  addMcpFolder: (id: string, write: boolean) => invoke<McpServerView[]>("add_mcp_folder", { id, write }),
  removeMcpFolder: (id: string, path: string) => invoke<McpServerView[]>("remove_mcp_folder", { id, path }),
  removeMcpServer: (id: string) => invoke<McpServerView[]>("remove_mcp_server", { id }),
  renameMcpServer: (id: string, name: string) => invoke<McpServerView[]>("rename_mcp_server", { id, name }),
  testMcpServer: (id: string) => invoke<McpToolView[]>("test_mcp_server", { id }),
  /** Accept the tool list last shown by Test for this server. */
  acceptMcpTools: (id: string) => invoke<McpServerView[]>("accept_mcp_tools", { id }),
  getUpdates: () => invoke<UpdatesView>("get_updates"),
  checkUpdates: () => invoke<UpdatesView>("check_updates"),
  downloadAppUpdate: (onProgress: (p: Progress) => void) =>
    invoke<UpdatesView>("download_app_update", { onProgress: progressChannel(onProgress) }),
  /** Closes Gardenwall and runs the installer; returns only on failure. */
  installAppUpdate: () => invoke<void>("install_app_update"),
  /** The diagnostics report (plain text, redacted by the core). */
  diagnosticsReport: () => invoke<string>("diagnostics_report"),
  /** A reply's Markdown as short pieces of spoken text (prose only). */
  speechPrepare: (text: string) => invoke<string[]>("speech_prepare", { text }),
  speechVoices: () => invoke<VoiceInfo[]>("speech_voices"),
  /** One piece spoken by the chosen Windows voice: a WAV file, base64. */
  speakPiece: (text: string) => invoke<string>("speak_piece", { text }),
  setVoiceSettings: (settings: VoiceSettings) => invoke<void>("set_voice_settings", { settings }),
  dictationStatus: () => invoke<DictationStatus>("dictation_status"),
  dictationSetup: (onProgress: (p: SetupProgress) => void) => {
    const progress = new Channel<SetupProgress>();
    progress.onmessage = onProgress;
    return invoke<void>("dictation_setup", { progress });
  },
  /** Start recording (the core opens the microphone). */
  dictationStart: () => invoke<void>("dictation_start"),
  /** Stop recording and turn the speech into text. */
  dictationStop: () => invoke<Dictated>("dictation_stop"),
  dictationCancel: () => invoke<void>("dictation_cancel"),
  /** The words so far while recording (raw), or null until the recogniser is ready. */
  dictationPartial: () => invoke<string | null>("dictation_partial"),
  /** The agent session on screen (no notification for it while the window is visible). */
  setShowingSession: (sid: string | null) => invoke<void>("set_showing_session", { sid }),
  /** Saves the report last shown; false when the save dialog was cancelled. */
  saveDiagnostics: () => invoke<boolean>("save_diagnostics"),
  backupStatus: () => invoke<BackupStatus>("backup_status"),
  /** Asks where to save; null when the dialog was cancelled. */
  createBackup: (passphrase: string, include: { activityLog: boolean; memories: boolean; skills: boolean }) =>
    invoke<{ fileName: string; bytes: number } | null>("create_backup", { passphrase, include }),
  chooseBackup: () => invoke<ChosenBackup | null>("choose_backup"),
  inspectBackup: (passphrase: string) => invoke<BackupContents>("inspect_backup", { passphrase }),
  restoreBackup: (passphrase: string) => invoke<void>("restore_backup", { passphrase }),
  cancelRestore: () => invoke<void>("cancel_restore"),
  restartToRestore: () => invoke<void>("restart_to_restore"),
  setUpdateSchedule: (schedule: UpdateSchedule) => invoke<UpdatesView>("set_update_schedule", { schedule }),
  setActiveRuntime: (id: string) => invoke<UpdatesView>("set_active_runtime", { id }),
  removeRuntime: (id: string) => invoke<UpdatesView>("remove_runtime", { id }),
  /** Loads the smallest approved model on this runtime, in the sandbox. */
  testRuntime: (id: string) => invoke<UpdatesView>("test_runtime", { id }),
  checkModelUpdate: (sha256: string) => invoke<ModelUpdate>("check_model_update", { sha256 }),
  agentTranscripts: () => invoke<TranscriptSummary[]>("agent_transcripts"),
  agentTranscript: (id: string) => invoke<TranscriptView>("agent_transcript", { id }),
  /** Opens the native folder picker in the trusted core, then continues the transcript. */
  agentResume: (id: string, collection: string | null = null, memories = true) =>
    invoke<AgentView | null>("agent_resume", { id, collection, memories }),
  listCollections: () => invoke<CollectionsView>("list_collections"),
  createCollection: (name: string) => invoke<CollectionsView>("create_collection", { name }),
  renameCollection: (id: string, name: string) => invoke<CollectionsView>("rename_collection", { id, name }),
  deleteCollection: (id: string) => invoke<CollectionsView>("delete_collection", { id }),
  /** Lists the files, checking each against its saved location on disk. */
  collectionFiles: (id: string) => invoke<FileSummary[]>("collection_files", { id }),
  /** The text Gardenwall extracted from a document: its stored passages, in order. */
  documentText: (collection: string, fileId: number) => invoke<string[]>("document_text", { collection, fileId }),
  removeDocument: (collection: string, fileId: number) =>
    invoke<FileSummary[]>("remove_document", { collection, fileId }),
  /** Reads files again from their saved locations (by id; the core holds the paths). */
  reindexDocuments: (collection: string, fileIds: number[], onProgress: (p: DocProgress) => void) => {
    const channel = new Channel<DocProgress>();
    channel.onmessage = onProgress;
    return invoke<AddReport>("reindex_documents", { collection, fileIds, onProgress: channel });
  },
  /** Opens the native file picker in the trusted core. */
  addDocuments: (id: string, onProgress: (p: DocProgress) => void) => {
    const channel = new Channel<DocProgress>();
    channel.onmessage = onProgress;
    return invoke<AddReport>("add_documents", { id, onProgress: channel });
  },
  indexCollection: (id: string, onProgress: (p: DocProgress) => void) => {
    const channel = new Channel<DocProgress>();
    channel.onmessage = onProgress;
    return invoke<CollectionsView>("index_collection", { id, onProgress: channel });
  },
  setEmbeddingModel: (sha256: string | null) => invoke<CollectionsView>("set_embedding_model", { sha256 }),
  setChatCollection: (conversation: string, collection: string | null) =>
    invoke<void>("set_chat_collection", { conversation, collection }),
  chatCollection: (conversation: string) => invoke<string | null>("chat_collection", { conversation }),
  agentDeleteTranscript: (id: string) => invoke<void>("agent_delete_transcript", { id }),
  agentRename: (sid: string, title: string) => invoke<AgentView>("agent_rename", { sid, title }),
  /** Rename a saved agent session (open or not). */
  renameTranscript: (id: string, title: string) => invoke<void>("rename_transcript", { id, title }),
  agentSessionInfo: (sid: string) => invoke<SessionInfo>("agent_session_info", { sid }),
  agentForkPoints: (sid: string) => invoke<{ seq: number; text: string }[]>("agent_fork_points", { sid }),
  /** `seq` null clones the session at its end. */
  agentFork: (sid: string, seq: number | null) => invoke<ForkResult>("agent_fork", { sid, seq }),
  /** Ask the model for a handoff note to review; nothing is saved. */
  handoffDraft: (source: HandoffSource, focus: string | null) =>
    invoke<HandoffDraft>("handoff_draft", { source, focus }),
  /** Start a new session from the reviewed note; send `message` as its first request. */
  handoffStart: (source: HandoffSource, target: HandoffTarget, text: string) =>
    invoke<HandoffStarted>("handoff_start", { source, target, text }),
  /** Native save dialog; returns the path, or null if cancelled. */
  handoffSaveAs: (source: HandoffSource, text: string) => invoke<string | null>("handoff_save_as", { source, text }),
  /** Saves HANDOFF.md in the session's folder after the approval dialog. */
  handoffSaveToProject: (sid: string, text: string) => invoke<string>("handoff_save_to_project", { sid, text }),
  /** Opens the native save dialog; returns the path, or null if cancelled. */
  /** The toolchains added for one project (an empty list removes them). */
  setProjectToolchains: (id: string, toolchains: string[]) => invoke<void>("set_project_toolchains", { id, toolchains }),
  parentFoldersStatus: (project: string | null) => invoke<ParentFoldersStatus>("parent_folders_status", { project }),
  setParentFolders: (project: string | null, on: boolean) => invoke<void>("set_parent_folders", { project, on }),
  /** Which project folders have a Git repository. */
  projectSetup: (id: string) => invoke<FolderSetup[]>("project_setup", { id }),
  /** Create a Git repository in a project folder (Gardenwall runs git init, outside the sandbox). */
  initProjectRepository: (id: string, index: number) => invoke<string>("init_project_repository", { id, index }),
  /** What each project folder needs and what runs by itself (PROJECT-SETUP-PLAN A1-A4). `recheck` looks for toolchains again. */
  projectReadiness: (id: string, recheck = false) => invoke<FolderReadiness[]>("project_readiness", { id, recheck }),
  /** A starter AGENTS.md from the folder's files, for the user to edit. */
  projectInstructionsDraft: (id: string, index: number) => invoke<string>("project_instructions_draft", { id, index }),
  /** Save a new AGENTS.md (refused when the folder already has one). */
  saveProjectInstructions: (id: string, index: number, text: string) =>
    invoke<void>("save_project_instructions", { id, index, text }),
  exportTranscript: (id: string, format: "html" | "md" | "jsonl") =>
    invoke<string | null>("export_transcript", { id, format }),
  /** Opens the native file picker for a .jsonl session file. */
  importTranscript: () => invoke<TranscriptSummary | null>("import_transcript"),
  agentSend: async (sid: string, text: string, onEvent: (e: AgentUiEvent) => void, images: string[] = [], files: string[] = []) => {
    const channel = new Channel<AgentUiEvent>();
    channel.onmessage = onEvent;
    return invoke<{
      end: "answered" | "stopped" | "step_limit" | "repeating";
      suggested: number;
      undelivered: string[];
      /** The request, when it was stopped before anything ran (taken back out of the session). */
      withdrawn: string | null;
      /** Why starting a fresh session from a handoff is worth suggesting now. */
      handoff: HandoffSuggestion | null;
      /** Times this session has been summarized (compacted). */
      compactions: number;
    }>("agent_send", { sid,
      text,
      images,
      files,
      onEvent: channel,
    });
  },
};

/** When Gardenwall looks for updates (ADR-017 §3.4). */
export type UpdateSchedule = "onLaunch" | "manual" | "daily" | "weekly";

export interface RuntimeUpdate {
  id: string;
  version: string;
  backend: string;
  downloadMb: number;
  installed: boolean;
  /** Pinned by this version of Gardenwall; only pinned builds can run. */
  pinned: boolean;
  active: boolean;
  /** The last smoke test on this machine. */
  smoke: SmokeResult | null;
  /** Published security problems in this release, fixed in a newer one. */
  advisories: RuntimeAdvisory[];
}

export interface GpuDriverNotice {
  adapter: string;
  installed: string;
  fixed: string;
  bulletin: string;
  source: string;
  summary: string;
  reviewed: string;
}

export interface RuntimeAdvisory {
  id: string;
  summary: string;
  fixedIn: string;
}

export interface SmokeResult {
  atMs: number;
  model: string;
  completion: boolean;
  toolCall: boolean;
  error: string | null;
  seconds: number;
}

/** The Gardenwall row (ADR-017 §3.3): the core's release check and a downloaded installer. */
/** Settings → Data: backups (release plan feature 3). */
export interface BackupStatus {
  lastMs: number | null;
  lastFile: string | null;
  restorePending: boolean;
  cannotBackUp: string | null;
  minPassphrase: number;
}
export interface DataCounts {
  chats: number;
  agentSessions: number;
  projects: number;
  memories: number;
  documentCollections: number;
  newestMs: number | null;
}
export interface BackupManifest {
  appVersion: string;
  createdMs: number;
  data: DataCounts;
  activityLog: number | null;
  memoriesIncluded: boolean;
  /** Skill files, or null when the skills folder was left out. */
  skills: number | null;
  settings: boolean;
  encrypted: boolean;
}
export interface BackupContents {
  backup: BackupManifest;
  current: DataCounts;
  currentActivity: number;
}
export interface ChosenBackup {
  fileName: string;
  bytes: number;
}

export interface AppVersion {
  current: string;
  available: string | null;
  note: string;
  /** Release notes, plain text. */
  notes: string | null;
  published: string | null;
  security: boolean;
  /** Copied, never opened. */
  releasesPage: string;
  /** Why this build can't install updates itself. */
  cannotInstall: string | null;
  downloading: boolean;
  /** Downloaded, and its signature checked. */
  downloaded: boolean;
  checkError: string | null;
}

export interface UpdateReport {
  checkedAtMs: number;
  offline: boolean;
  app: AppVersion;
  runtimes: RuntimeUpdate[];
  /** Set when the GPU driver is older than the vendor's latest security fix. */
  gpuDriver: GpuDriverNotice | null;
  /** The NVIDIA driver was checked and has the latest fix. */
  gpuDriverCurrent: { adapter: string; installed: string; bulletin: string; reviewed: string } | null;
  /** Updates worth the user's attention (badge count). */
  available: number;
  /** A security fix is available. */
  security: boolean;
}

export interface UpdatesView {
  schedule: UpdateSchedule;
  lastCheckMs: number | null;
  report: UpdateReport | null;
}

export type ModelUpdate =
  | { status: "upToDate"; commit: string }
  | { status: "available"; repo: string; path: string; commit: string; size: number }
  | { status: "missing" }
  | { status: "notFromHub" };

/** A project: its folders (chosen in the native picker) and its sessions. */
export interface Project {
  id: string;
  name: string;
  folders: string[];
  createdAt: number;
  updatedAt: number;
}

/** A folder picked in the New project dialog (the core keeps the path). */
export interface PickedFolder {
  token: string;
  folder: string;
  hasGit: boolean;
  empty: boolean;
  /** What the folder already has, from file names ("Rust (Cargo.toml)"). */
  detected: string[];
}

export interface ToolchainInstall {
  id: string;
  name: string;
  url: string;
  steps: string;
}

export interface NewProjectOptions {
  kinds: {
    id: string;
    label: string;
    toolchains: { id: string; name: string; version: string | null; problem: string | null; install: ToolchainInstall | null }[];
  }[];
  /** Git for Windows is installed. */
  git: boolean;
}

export interface NewProjectRequest {
  pick: string;
  newFolderName: string | null;
  kind: string;
  toolchains: string[];
  initGit: boolean;
  /** Add Gardenwall's secret-file patterns to .gitignore (needs a repository). */
  ignoreSecrets: boolean;
}

/** Secret patterns one git project folder doesn't ignore yet. */
export interface IgnoreGap {
  index: number;
  folder: string;
  lines: string[];
}

/** Secret files of one project folder (relative paths with `/`). */
export interface FolderSecrets {
  folder: string;
  inProject: string[];
  moved: string[];
  keptIn: string;
  hints: string[];
}

/** An open agent session in the session list. */
export interface SessionCard {
  id: string;
  title: string;
  folders: string[];
  transcript: string | null;
  project: string | null;
  busy: boolean;
  /** Waiting for another session to finish ("one at a time"). */
  waiting: boolean;
  createdAt: number;
  /** Programs the user let keep running in the session's sandbox. */
  background: string[];
}

export interface AgentView {
  /** The session's id, passed to every session command. */
  id: string;
  title: string;
  busy: boolean;
  project: string | null;
  active: boolean;
  folders: string[];
  shell: string | null;
  sandboxed: boolean;
  /** The saved transcript of this session, if any. */
  transcript: string | null;
  /** Older messages of this session were compacted into a summary. */
  compacted: boolean;
  /** Web access offered to the agent, if any. */
  web: string | null;
  /** The attached document collection, if any. */
  documents: string | null;
  /** MCP servers in use, with tool counts. */
  mcp: string | null;
  /** Planning mode: the agent may look but not change anything. */
  planMode: boolean;
  /** Memories are used in this session. */
  memories: boolean;
  /** Skills offered to the agent: [name, "project" | "personal"]. */
  skills: [string, string][];
  /** Custom commands from the granted folders (`.gardenwall/commands`). */
  commands: UserCommand[];
  /** Prompts from the session's MCP servers. */
  mcpPrompts: McpPromptView[];
  /** The agent's task list. */
  todos: Todo[];
  /** Project instruction files in the system prompt (AGENTS.md, CLAUDE.md). */
  contextFiles: string[];
  /** Programs the user let keep running in the sandbox. */
  background: string[];
  /** A research session's approval mode (null: not a research session). */
  research: WebMode | null;
  /** Web content was read: changes and commands ask first. */
  webTainted: boolean;
  /** Searches and pages in the visited list. */
  visits: number;
  /** Secret files in the folders that the agent's commands can't reach. */
  hiddenSecrets: string[];
  /** Why this session has no commands, when its sandbox couldn't be set up. */
  commandsOff: string | null;
  /** Toolchains changed after this session started: offer a sandbox restart (ADR-024). */
  restartOffer: string | null;
  /** A toolchain the agent asked for isn't installed: where to get it. */
  installCard: InstallCard | null;
}

/** Where to get a toolchain that isn't installed (Gardenwall's own table). */
export interface InstallCard {
  id: string;
  name: string;
  url: string;
  steps: string;
}

/** How web requests are approved (ADR-018). */
export type WebMode = "ask" | "research";
export type SearchProviderKind = "searxng" | "brave";

export interface WebSettings {
  provider: SearchProviderKind;
  defaultMode: WebMode;
  maxSearches: number;
  maxPages: number;
  /** Review a research sub-agent's findings before the agent gets them. */
  reviewFindings: boolean;
}

/** What a research sub-agent found (bounded by the core). */
export interface ResearchFindings {
  text: string;
  complete: boolean;
  searches: number;
  pages: number;
  failed: number;
}

/** The session's research card (ADR-019): approve the question, watch, review the findings. */
export type ResearchCardState =
  | { phase: "question"; question: string }
  | { phase: "working"; question: string; step: string }
  | { phase: "findings"; question: string; findings: ResearchFindings };

export interface ResearchCardView {
  id: number;
  card: ResearchCardState;
}

export interface WebStatus {
  settings: WebSettings;
  searxngUrl: string | null;
  braveKeySaved: boolean;
  keysSupported: boolean;
  offline: boolean;
}

export interface ConnectionTest {
  ok: boolean;
  provider: string;
  results: number;
  millis: number;
  sample: string | null;
  error: string | null;
}

/** One entry of the visited-sites list, built by the core. */
export interface Visit {
  atMs: number;
  kind: "search" | "page";
  /** The query or the page address (data: shown as text, never opened). */
  target: string;
  via: string;
  allowed: "pending" | "approved" | "granted" | "typed" | "denied" | "expired" | "blocked";
  ok: boolean | null;
  bytes: number | null;
  note: string | null;
}

export interface VisitsView {
  visits: Visit[];
  dropped: number;
  /** Links in the latest answer to pages never fetched in this session. */
  unopened: string[];
}

export interface ChatWebView {
  on: boolean;
  mode: WebMode | null;
}

/** Settings → Agent (Pi's compaction, retry and prompt settings). */
/** A toolchain found on this PC (ADR-022 §2B). */
export interface ToolchainView {
  id: string;
  name: string;
  version: string;
  /** The folder the agent's sandbox gets, read and run only. */
  folder: string;
  added: boolean;
  /** Something to know, such as a missing linker. */
  note: string | null;
}

/** Is this project folder ready? (docs/internal/PROJECT-SETUP-PLAN.md A1-A4) */
export interface FolderReadiness {
  index: number;
  folder: string;
  git: boolean;
  canInit: boolean;
  /** The folder itself first (path ""), then its parts ("apps/web"). */
  parts: ReadinessPart[];
  toolchains: ToolchainNeed[];
  /** "AGENTS.md" or "CLAUDE.md" when the folder has one. */
  instructions: string | null;
  /** Secret patterns the .gitignore doesn't cover. */
  gitignoreMissing: number;
  parentFolders: { neededBy: string[]; on: boolean; ready: boolean } | null;
  autoRun: AutoRun[];
  autoRunMore: number;
}

export interface ReadinessPart {
  path: string;
  stacks: string[];
  pins: { tool: string; name: string; wants: string; file: string; installed: string | null; matches: boolean | null }[];
  fetch: {
    id: string;
    label: string;
    command: string;
    folder: string;
    warning: string | null;
    fetched: boolean;
    needed: boolean | null;
    needsParentFolders: boolean;
  }[];
}

export interface ToolchainNeed {
  id: string;
  name: string;
  neededBy: string[];
  version: string | null;
  on: boolean;
  everywhere: boolean;
  problem: string | null;
  install: ToolchainInstall | null;
}

/** A file that runs code by itself (on install, build, test, folder open, commit). */
export interface AutoRun {
  path: string;
  when: string;
  details: string[];
}

/** A project folder and whether it has a Git repository (ADR-022 §2A). */
export interface FolderSetup {
  folder: string;
  hasGit: boolean;
  canInit: boolean;
}

export interface AgentSettings {
  compaction: boolean;
  reserveTokens: number;
  keepRecentTokens: number;
  retries: number;
  appendSystemPrompt: string;
  /** How open agent sessions share the model. */
  sessions: "turns" | "oneAtATime" | "parallel";
  /** Model server slots in parallel mode (2 to 4). */
  parallelSlots: number;
  /** The shell for agent commands on Windows. */
  shell: "powershell" | "windowspowershell" | "cmd";
  python: boolean;
  /** System notifications while Gardenwall is in the background. */
  notifyFinished: boolean;
  notifyApproval: boolean;
  notifyFailed: boolean;
  /** Experimental: offer the agent `run_js` (one program instead of many tool calls). */
  codeMode: boolean;
  /** Offer the agent `ask_user` (multiple-choice question cards). */
  askUser: boolean;
  runRequests: boolean;
  gitIdentity: "mine" | "agent" | "custom";
  gitName: string;
  gitEmail: string;
  /** Steps before a long task pauses and asks to continue; 0 never asks. */
  stepCheckpoint: number;
  /** Toolchains added to the agent's sandbox in every project, by id (ADR-022). */
  toolchains: string[];
  /** Toolchains added for one project only, by project id. */
  projectToolchains: Record<string, string[]>;
  /** "Folders above the project" for every project (ADR-025). */
  parentFolders: boolean;
  /** The same, for these projects only (by id). */
  projectParentFolders: string[];
}

/** One folder above a project (ADR-025), as the core describes it. */
export interface ParentFolder {
  path: string;
  /** What of it the agent could see (names only). */
  what: string;
  /** Only an administrator can change it. */
  admin: boolean;
  /** The sandbox can list it now. */
  done: boolean;
  problem: string | null;
}

export interface ParentFoldersStatus {
  global: boolean;
  projectOn: boolean;
  on: boolean;
  folders: ParentFolder[];
  adminCommand: string | null;
  removeCommand: string | null;
  ready: boolean;
}

/** One chat or agent session that matched a search. */
export interface SearchHit {
  id: string;
  title: string;
  snippet: string;
  updatedAt: number;
}

export interface HistoryHits {
  chats: SearchHit[];
  sessions: SearchHit[];
}

/** One file's change, from the core's checkpoints. */
export interface FileDiff {
  path: string;
  change: "created" | "modified" | "deleted" | "unchanged";
  /** Unified diff text; null for binary or very large files. */
  unified: string | null;
  beforeBytes: number | null;
  afterBytes: number | null;
}

/** One granted folder's changes in a session. */
export interface FolderChanges {
  folder: string;
  files: FileDiff[];
}

/** A "Run this yourself" card (`ask_user_to_run`), checked by the core. */
export interface RunCard {
  id: number;
  request: {
    command: string;
    hidden: number;
    reason: string;
    folder: string;
    warning: string | null;
    afterWeb: boolean;
    /** Gardenwall wrote the command (fetch dependencies), not the model. */
    byGardenwall: boolean;
  };
}

/** The agent's question card (`ask_user`), cleaned by the core. */
export interface QuestionCard {
  id: number;
  questions: { question: string; options: string[] }[];
}

/** One open agent session on the dashboard, built by the core. */
export interface DashboardRow {
  id: string;
  title: string;
  project: string | null;
  folders: string[];
  /** `needsYou`: a card (run it yourself, a question, research) waits for the user. */
  status: "open" | "working" | "approval" | "needsYou" | "waitingTurn" | "finished" | "stopped" | "failed" | "paused";
  /** What it is doing now (tool names only, never model text). */
  step: string;
  todosDone: number;
  todosTotal: number;
  /** The running turn's time so far, or the last turn's length. */
  elapsedMs: number | null;
  contextTokens: number | null;
  contextWindow: number | null;
  endedAtMs: number | null;
  detail: string | null;
  /** Tool calls in the running (or last) turn. */
  steps: number;
  /** The latest tool names, newest last. */
  recent: string[];
  /** The session's saved transcript (so it is listed once). */
  transcript: string | null;
  /** Programs the user let keep running in the session's sandbox. */
  background: string[];
}

export type AttentionKind = "finished" | "approval" | "failed" | "paused" | "security";

export interface AttentionEvent {
  seq: number;
  sid: string | null;
  name: string;
  kind: AttentionKind;
  atMs: number;
}

export interface Todo {
  content: string;
  status: "pending" | "in_progress" | "completed";
}

export interface ContextView {
  tokens: number;
  contextWindow: number | null;
  breakdown: ContextBreakdown;
  compacted: boolean;
}

export interface SkillView {
  name: string;
  description: string;
  source: string;
  /** Text files bundled with the skill. */
  files?: number;
}

export interface McpServerView {
  id: string;
  name: string;
  program: string;
  args: string[];
  folders: { path: string; write: boolean }[];
  enabled: boolean;
  /** Environment variables for the server (plain settings, not secrets). */
  env?: { name: string; value: string }[];
  /** Whether this platform can run MCP servers. */
  supported: boolean;
  /** Counts from its last start or Test (null: never started). Written by the core. */
  toolStatus?: { tools: number; held: number } | null;
}

export interface McpToolView {
  name: string;
  description: string;
  /** Where the tool stands against what you accepted; only "accepted" tools reach the model. */
  review: "accepted" | "new" | "changed" | "flagged";
  /** Its text reads like instructions to the model, or hides characters. */
  suspicious: boolean;
}

export interface CollectionSummary {
  id: string;
  name: string;
  files: number;
  chunks: number;
  /** Passages indexed with the current embedding model. */
  embedded: number;
  createdAt: number;
  updatedAt: number;
}

export interface CollectionsView {
  collections: CollectionSummary[];
  embeddingModel: string | null;
}

export interface FileSummary {
  id: number;
  name: string;
  size: number;
  chunks: number;
  addedAt: number;
  /** The file's location was saved, so it can be checked and read again. */
  hasSource: boolean;
  /** Compared with the file on disk when the list was loaded. */
  status: SourceStatus;
}

export type SourceStatus = "unchanged" | "changed" | "missing" | "unreadable" | "notSaved";

export interface DocProgress {
  file: string;
  done: number;
  total: number;
}

export interface AddReport {
  added: string[];
  failed: [string, string][];
  embedError: string | null;
}

/** GPU memory picture from the trusted core (Windows performance counters). */
export interface GpuView {
  adapter: string;
  totalBytes: number;
  usedBytes: number | null;
  freeBytes: number | null;
  /** Held by the model's runtime. */
  modelBytes: number | null;
  /** Other apps using the most GPU memory (Windows and Gardenwall left out). */
  apps: { name: string; bytes: number }[];
}

/** GPU and system memory for Home's memory bars (read-only). */
export interface SystemMemory {
  gpu: GpuView | null;
  ramTotalBytes: number | null;
  ramAvailableBytes: number | null;
}

export interface GpuWarning {
  kind: "slowdown" | "pressure";
  /** Best generation speed since the model loaded (tokens/s), for a slowdown. */
  baseline: number | null;
  /** Speed of the slow reply (tokens/s), for a slowdown. */
  current: number | null;
  gpu: GpuView | null;
}

/** Estimated tokens per part of the agent's context (characters / 4). */
export interface ContextBreakdown {
  system: number;
  tools: number;
  summary: number;
  conversation: number;
  toolResults: number;
}

export interface AgentUsage {
  promptTokens: number | null;
  totalTokens: number | null;
  contextWindow: number | null;
  /** Compaction starts above this many tokens. */
  compactAt: number | null;
  breakdown: ContextBreakdown;
  promptPerSecond: number | null;
  generationPerSecond: number | null;
  ttftMs: number | null;
  draftTokens?: number | null;
  draftAccepted?: number | null;
}

export type AgentUiEvent =
  | { type: "content"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "toolStart"; id: string; name: string; arguments: string; parent?: string }
  | { type: "toolEnd"; id: string; output: string; isError: boolean }
  | { type: "approvalWaiting"; tool: string }
  | ({ type: "usage" } & AgentUsage)
  | { type: "compacting" }
  | { type: "compacted"; tokensBefore: number; summary: string }
  | { type: "compactionFailed"; message: string }
  | ({ type: "gpuWarning" } & GpuWarning)
  | { type: "notSaved"; message: string }
  | { type: "todos"; todos: Todo[] }
  | { type: "retrying"; attempt: number; max: number; delayMs: number; error: string }
  | { type: "steered"; text: string }
  | { type: "interrupted" }
  | { type: "contextChanged"; files: string[] }
  | { type: "waiting" };

export interface TranscriptSummary {
  id: string;
  title: string;
  /** Folders the session had, for display only. Continuing asks for folders again. */
  folders: string[];
  createdAt: number;
  updatedAt: number;
  /** The session this one was forked or cloned from, if it still exists. */
  parentId: string | null;
  /** Entries before this sequence number came from the parent. */
  forkSeq: number | null;
  /** The project the session belongs to, if any. */
  projectId: string | null;
  /** Started from a reviewed handoff of `parentId` (a session) or `parentChatId` (a chat). */
  handoff: boolean;
  /** The chat this session continues from a handoff, if it still exists. */
  parentChatId: string | null;
}

/** Why Gardenwall suggests a handoff (owner thresholds: context past 70 %,
 * two or more summaries, or a finished task list of three or more). */
export type HandoffSuggestion = "compactions" | "context" | "tasksDone";

/** Where a handoff comes from. `request` lets Stop cancel a chat's draft (cancelChat). */
export type HandoffSource = { kind: "agent"; sid: string } | { kind: "chat"; id: string; request?: number };

/** Where the new session goes: an agent session (a chat picks the project,
 * null asks for folders) or a new chat (from a chat only). */
export type HandoffTarget = { kind: "agent"; project: string | null } | { kind: "chat" };

/** Facts Gardenwall adds to an agent handoff (never written by the model). */
export interface HandoffFacts {
  project: string | null;
  folders: string[];
  /** Files changed by the session's file tools, from checkpoints; null for a chat. */
  changedFiles: string[] | null;
}

export interface HandoffDraft {
  text: string;
  /** Values from protected files removed from the draft. */
  redacted: number;
  facts: HandoffFacts | null;
  title: string;
}

export interface HandoffStarted {
  agent: AgentView | null;
  chat: ConversationSummary | null;
  /** The new session's first message (the note framed, with the facts). */
  message: string;
}

export interface SessionInfo {
  id: string | null;
  title: string | null;
  folders: string[];
  createdAt: number | null;
  userMessages: number;
  assistantMessages: number;
  toolResults: number;
  compactions: number;
  contextTokens: number;
  contextWindow: number | null;
  parentTitle: string | null;
}

export interface ForkResult {
  view: AgentView;
  transcript: TranscriptView;
  text: string;
}

/** A saved context message (model output inside is untrusted text). */
export interface SavedMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string;
  reasoning_content?: string;
  /** Images of a user message, as data: URLs. */
  images?: string[];
  tool_calls?: { id: string; type: string; function: { name: string; arguments: string } }[];
  tool_call_id?: string;
}

export type TranscriptEntry =
  | { kind: "message"; seq: number; message: SavedMessage; /** Unix seconds; absent from older builds. */ createdAt?: number }
  | { kind: "compaction"; tokensBefore: number; summary: string };

export interface TranscriptView extends TranscriptSummary {
  entries: TranscriptEntry[];
}

export type ApprovalKind =
  | "RevealSecret"
  | "WriteSecret"
  | "PersistenceRisk"
  | "OutsideRead"
  | "OutsideWrite"
  | "NetworkCommand"
  | "Egress"
  | "McpTool"
  | "RiskyCommand"
  | "HeldBack"
  | "KeepRunning"
  | "InitRepository"
  | "AddToolchain";

/** A stored approval request (ADR-016). `details` is JSON written by the trusted core. */
export interface ProposalView {
  id: string;
  kind: ApprovalKind;
  state: string;
  details: string | null;
  createdAtMs: number;
  deadlineMs: number;
}

/** A waiting request with where it came from (metadata from the core). */
export interface PendingApproval extends ProposalView {
  sessionTitle: string;
  /** The open agent session it belongs to (docks there), or null (standalone card). */
  agentSession: string | null;
}

/** Shape of `ProposalView.details` (version 1). Every field is shown as plain text. */
export interface ApprovalDetails {
  /** Plain-language reason from the policy engine. */
  reason?: string;
  /** What will be done, e.g. "Write file". */
  operation?: string;
  /** Exact path, command or destination. */
  target?: string;
  /** Unified diff for writes. */
  diff?: string;
  /** Exact data that would leave the machine, for egress. */
  payload?: string;
  /** "program" when a code-mode program asked for this call (set by the core). */
  source?: string;
  /** AddToolchain: the agent's own words (model text, shown as such). */
  agentReason?: string;
  /** AddToolchain: "project" or "every". */
  scope?: string;
  /** Something the user should know before deciding. */
  note?: string;
  /** Session-grant scope: `search` or `fetch:<origin>`. */
  grant?: string;
}

export interface AuditEntry {
  atMs: number;
  event: string;
  session: string | null;
  proposal: string | null;
  kind: string | null;
  reason: string | null;
}

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : "Unexpected error";
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
  return `${(n / 1e3).toFixed(0)} KB`;
}

export interface McpPromptView {
  /** `/mcp__<server>__<prompt>` */
  command: string;
  server: string;
  description: string;
  arguments: { name: string; description: string; required: boolean }[];
}

export interface UserCommand {
  name: string;
  description: string;
  argumentHint: string | null;
  template: string;
}

export type Thinking = "default" | "medium" | "low" | "off";

export type Speculation = { kind: "auto" } | { kind: "off" } | { kind: "ngram" } | { kind: "mtp" } | { kind: "draft"; model: string };

/** Settings Gardenwall suggests for a model on this computer, with reasons. */
export interface Suggestion {
  hardware: string;
  fit: "gpu" | "partly" | "tooBig" | "unknown";
  contextSize: number | null;
  kvCache: KvCache;
  speculation: Speculation;
  moeCacheMib: number;
  thinking: Thinking;
  notes: { setting: string; text: string }[];
}

/** "draft 64% kept" when speculative decoding ran, else null. */
export function draftText(tokens?: number | null, accepted?: number | null): string | null {
  if (!tokens || accepted == null) return null;
  return `draft ${Math.round((100 * accepted) / tokens)}% kept`;
}

export type MemoryScope = { kind: "personal" } | { kind: "project"; folder: string };

export interface Memory {
  id: string;
  scope: MemoryScope;
  text: string;
  /** Suggested by the model, waiting for the user's review. */
  pending: boolean;
  source: "user" | "model";
  inChat: boolean;
  inAgent: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface MemoriesView {
  memories: Memory[];
  /** Characters of memories a chat receives, and the budget. */
  chatChars: number;
  budget: number;
  /** Active memories left out of chats because the budget is full. */
  chatOverBudget: string[];
}
