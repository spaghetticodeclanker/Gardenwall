// Development-only: the model picker's commands for the mock UI
// (mock.html). Never loaded by the desktop app. The figures follow the real
// starter list on a 32 GiB card; `?gpu=unknown` shows unknown hardware.
import type { Chosen, RankingView, RepoView, ResearchPrompt, StarterEntry, StarterView, TradeOff } from "./api";

const G = 2 ** 30;
const unknown = new URLSearchParams(location.search).get("gpu") === "unknown";

const roomy: TradeOff = { runs: "All on the graphics card (GPU)", speed: "Fastest", room: "Room for long conversations", larger: null, smaller: null };
const card = unknown
  ? { runs: "Unknown: Gardenwall couldn't read this computer's GPU memory", speed: "Unknown", room: "Unknown", larger: null, smaller: null }
  : roomy;

function entry(id: string, name: string, repo: string, path: string, gib: number, kind: StarterEntry["kind"], summary: string, projector = 0): StarterEntry {
  return {
    id,
    name,
    repo,
    path,
    commit: "c1869413dece09109e55b7dcc2c77ef382b0d0bd",
    sha256: id.padEnd(64, "0").slice(0, 64),
    size: gib * G,
    total: (gib + projector) * G,
    projector: projector > 0,
    licence: "apache-2.0",
    publisher: repo.split("/")[0]!,
    kind,
    uses: kind === "vision" ? ["chat", "images"] : kind === "small" ? ["chat"] : ["chat", "coding"],
    summary,
    reviewed: "2026-11-20",
    fit: unknown ? "unknown" : "gpu",
    tight: false,
    label: unknown ? "GPU memory unknown" : "Fits on the GPU",
    card,
    installed: false,
  };
}

const entries: StarterEntry[] = [
  entry("qwen3-0.6b", "Qwen3 0.6B", "Qwen/Qwen3-0.6B-GGUF", "Qwen3-0.6B-Q8_0.gguf", 0.6, "small", "A tiny model for trying Gardenwall on any computer."),
  entry("gemma-4-26b-a4b", "Gemma 4 26B-A4B", "google/gemma-4-26B-A4B-it-qat-q4_0-gguf", "gemma-4-26B_q4_0-it.gguf", 13.4, "general", "A Mixture-of-Experts model: the knowledge of a large model at the speed of a small one."),
  entry("qwen3.8-27b", "Qwen3.8 27B", "ggml-org/Qwen3.8-27B-GGUF", "Qwen3.8-27B-Q4_K_M.gguf", 18.1, "general", "A large dense model, the strongest here for coding with the agent."),
  entry("qwen3.8-27b-vision", "Qwen3.8 27B (with images)", "ggml-org/Qwen3.8-27B-GGUF", "Qwen3.8-27B-Q4_K_M.gguf", 18.1, "vision", "The large 27B model with its image projector.", 0.6),
];
entries[2]!.card = unknown ? card : { ...roomy, smaller: "The smaller file (Q4_0, 15.2 GiB) is faster to download and leaves more room, with some loss of quality (its quality: small loss)." };

const starter: StarterView = {
  hardware: unknown ? "GPU memory unknown · 62.0 GiB of system RAM" : "NVIDIA GeForce RTX 5090 (31.5 GiB of GPU memory) · 62.0 GiB of system RAM",
  tier: unknown ? null : 1,
  tiers: [
    { minGib: 24, label: "24 GiB of GPU memory", recommended: "gemma-4-26b-a4b", faster: "qwen3-0.6b", vision: null },
    { minGib: 32, label: "32 GiB of GPU memory or more", recommended: "qwen3.8-27b", faster: "gemma-4-26b-a4b", vision: "qwen3.8-27b-vision" },
  ],
  entries,
  reviewed: "Reviewed November 2026",
  about: "version 1, reviewed 20 November 2026",
  stale: null,
};

const ranking = (kind: string, sort: string): RankingView => ({
  kinds: [
    { id: "text-generation", label: "Chat and coding" },
    { id: "image-text-to-text", label: "Vision" },
  ],
  sorts: [
    { id: "trendingScore", label: "Trending" },
    { id: "downloads", label: "Most downloaded (last 30 days)" },
    { id: "createdAt", label: "Newest" },
  ],
  kind,
  sort,
  note: null,
  intro: "This is what people are downloading, not a test of quality. Gardenwall hasn't tried these models. Every download is still checked.",
  about: "Kinds and orders: version 1, reviewed 20 November 2026. Known publishers: version 1, reviewed 20 November 2026.",
  rows: [
    { repo: "unsloth/Qwen3.8-27B-GGUF", publisher: "unsloth", knownPublisher: true, created: "2026-08-13", downloads: 7383193, likes: 812, licence: "apache-2.0", gated: false, warnings: [] },
    { repo: "someone/Brand-New-Model-GGUF", publisher: "someone", knownPublisher: false, created: "2026-11-19", downloads: 212, likes: 3, licence: null, gated: false, warnings: ["New: created under 30 days ago", "Few downloads"] },
    { repo: "example/Gated-Model-GGUF", publisher: "example", knownPublisher: false, created: null, downloads: null, likes: null, licence: null, gated: true, warnings: ["Gated: needs a hub account"] },
  ],
});

export function repoView(repo: string, gated: boolean): RepoView {
  const fit = unknown ? "unknown" : "gpu";
  const file = (path: string, gib: number, label: string | null, bits: number | null, quality: string | null, extra: Partial<RepoView["files"][number]> = {}) => ({
    path,
    size: gib * G,
    parts: 1,
    role: "model" as const,
    quant: { label, bits, quality, known: label !== null && bits !== null },
    fit: fit as "gpu" | "unknown",
    tight: false,
    label: unknown ? "GPU memory unknown" : "Fits on the GPU",
    best: false,
    whyBest: null,
    card: { ...card },
    ...extra,
  });
  return {
    repo,
    commit: "0123456789abcdef0123456789abcdef01234567",
    license: "apache-2.0",
    gated,
    files: [
      file("Model-Q4_K_M.gguf", 18.1, "Q4_K_M", 4.85, "small loss", unknown ? {} : { best: true, whyBest: "The largest 4- to 6-bit file that fits on the GPU with room left for the conversation. Larger files add little quality for their size." }),
      file("Model-Q8_0.gguf", 26.6, "Q8_0", 8.5, "almost the same as the original", unknown ? {} : { fit: "partly" as never, label: "Part runs from system RAM (slower)", card: { ...card, runs: "Part on the GPU, part in system RAM", speed: "Several times slower than a file that fits on the GPU (a Mixture-of-Experts model loses less)" } }),
      file("Model-Q4_K_ZZ.gguf", 17.0, "Q4_K_ZZ", null, null),
      { ...file("mmproj-Model-Q8_0.gguf", 0.6, "Q8_0", 8.5, null), role: "projector" as const, label: "Image projector: download it with a vision model so the model can see images", card: null },
    ],
    skipped: 0,
    hardware: starter.hardware,
    fitNote: "Judged from the file size alone: before download Gardenwall can't tell whether a model is Mixture-of-Experts or exactly how long a conversation will fit.",
    quantAbout: "Bits per weight and quality words: Gardenwall's quantization table, version 1, reviewed 20 November 2026.",
    knownPublisher: repo.startsWith("unsloth/") || repo.startsWith("Qwen/"),
    warnings: repo.startsWith("someone/") ? ["New: created under 30 days ago", "Few downloads"] : gated ? ["Gated: needs a hub account"] : [],
  };
}

const research: ResearchPrompt = {
  withHardware:
    "Today is 20 November 2026.\nMy computer: NVIDIA GeForce RTX 5090 with 32 GB of graphics memory, 62 GB of system RAM, Windows.\nWhich open-weight models released or updated recently are the best choice for this computer, running as GGUF files with llama.cpp? …",
  withoutHardware:
    "Today is 20 November 2026.\nWhich open-weight models released or updated recently are the best choice for this computer, running as GGUF files with llama.cpp? …",
  hardwareSentence: "My computer: NVIDIA GeForce RTX 5090 with 32 GB of graphics memory, 62 GB of system RAM, Windows.",
  missing: [],
};

/** The picker's answer to `cmd`, or `undefined` when it isn't a picker command. */
export async function pickerMock(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  switch (cmd) {
    case "starter_models":
      return starter;
    case "choose_model": {
      if (unknown) return { choices: [], note: "Gardenwall couldn't read this computer's GPU memory, so it can't choose for it." } satisfies Chosen;
      const a = args.answers as { purpose: string; priority: string };
      if (a.purpose === "images") return { choices: [{ id: "qwen3.8-27b-vision", why: "It can look at pictures, screenshots and scanned pages." }], note: null } satisfies Chosen;
      return {
        choices: [
          { id: a.priority === "speed" ? "gemma-4-26b-a4b" : "qwen3.8-27b", why: "The strongest model that still leaves room for the conversation on this computer." },
          { id: a.priority === "speed" ? "qwen3.8-27b" : "gemma-4-26b-a4b", why: "If you prefer quicker replies: it is smaller and faster." },
        ],
        note: null,
      } satisfies Chosen;
    }
    case "hub_ranking":
      await new Promise((r) => setTimeout(r, 200));
      return ranking(String(args.kind), String(args.sort));
    case "model_research_prompt":
      return research;
    case "research_repositories":
      return ["Qwen/Qwen3-8B-GGUF", "unsloth/Qwen3.8-27B-GGUF"];
    case "list_repo_files":
      return repoView(String(args.repo), String(args.repo).startsWith("example/Gated"));
    case "download_starter":
      return undefined; // handled with the other downloads
    default:
      return undefined;
  }
}
