// Runs stock Pi for one benchmark task (the pi arm of gardenwall-bench).
//
//   node bench/pi-wrapper.mjs {url} {model} {context} {prompt}
//
// Pi gets a fresh config folder for the run (nothing is read from or
// written to ~/.pi), with one custom provider pointing at the benchmark's
// llama-server ({url}/v1). The key is read from GARDENWALL_BENCH_KEY, which
// the benchmark sets, so it never appears on a command line or on disk.
// Like the Gardenwall arm, no sampling or thinking settings are sent: the
// server's defaults apply to both. Pi runs with its default tools and its
// JSON event stream; the final answer goes to stdout, and one
// `GARDENWALL_BENCH_STATS {...}` line on stderr gives the benchmark the
// same counts it records for Gardenwall (requests, tokens, tool calls,
// failed edits, compactions, first token, tool names).
//
// Pi's location: PI_CLI, or the global npm install.

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const [url, model, context, ...rest] = process.argv.slice(2);
const prompt = rest.join(" ");
if (!url || !model || !context || !prompt) {
  console.error("usage: pi-wrapper.mjs <url> <model> <context> <prompt>");
  process.exit(2);
}
if (!process.env.GARDENWALL_BENCH_KEY) {
  console.error("GARDENWALL_BENCH_KEY is not set");
  process.exit(2);
}

const cli =
  process.env.PI_CLI ??
  join(process.env.APPDATA ?? "", "npm", "node_modules", "@mariozechner", "pi-coding-agent", "dist", "cli.js");

const dir = mkdtempSync(join(tmpdir(), "gw-bench-pi-"));
const models = {
  providers: {
    bench: {
      baseUrl: `${url}/v1`,
      api: "openai-completions",
      // The name of the environment variable holding the key.
      apiKey: "GARDENWALL_BENCH_KEY",
      compat: { supportsDeveloperRole: false, supportsReasoningEffort: false },
      models: [{ id: model, name: model, contextWindow: Number(context) }],
    },
  },
};
writeFileSync(join(dir, "models.json"), JSON.stringify(models, null, 2));

const child = spawn(
  process.execPath,
  [cli, "--provider", "bench", "--model", model, "--no-session", "--offline", "--mode", "json", prompt],
  {
    stdio: ["ignore", "pipe", "inherit"],
    env: { ...process.env, PI_CODING_AGENT_DIR: dir, PI_OFFLINE: "1", PI_SKIP_VERSION_CHECK: "1" },
  },
);

const stats = {
  requests: 0,
  promptTokens: 0,
  completionTokens: 0,
  toolCalls: 0,
  editFailures: 0,
  compactions: 0,
  ttftMs: null,
  firstPromptTokens: null,
  tools: [],
};
let answer = "";
let turnStarted = null;
const text = (message) =>
  (message?.content ?? [])
    .filter((c) => c.type === "text")
    .map((c) => c.text)
    .join("");

createInterface({ input: child.stdout }).on("line", (line) => {
  let e;
  try {
    e = JSON.parse(line);
  } catch {
    return;
  }
  switch (e.type) {
    case "turn_start":
      turnStarted ??= Date.now();
      break;
    case "message_update":
      if (stats.ttftMs === null && turnStarted !== null) stats.ttftMs = Date.now() - turnStarted;
      break;
    case "message_end":
      if (e.message?.role === "assistant") {
        stats.requests += 1;
        const u = e.message.usage ?? {};
        stats.promptTokens += (u.input ?? 0) + (u.cacheRead ?? 0);
        stats.firstPromptTokens ??= (u.input ?? 0) + (u.cacheRead ?? 0);
        stats.completionTokens += u.output ?? 0;
        const t = text(e.message);
        if (t.trim()) answer = t;
      }
      break;
    case "tool_execution_start":
      stats.toolCalls += 1;
      stats.tools.push(e.toolName);
      break;
    case "tool_execution_end":
      if (e.isError && e.toolName === "edit") stats.editFailures += 1;
      break;
    case "compaction_end":
      if (!e.aborted) stats.compactions += 1;
      break;
  }
});

const done = (code) => {
  rmSync(dir, { recursive: true, force: true });
  process.stdout.write(answer);
  process.stderr.write(`\nGARDENWALL_BENCH_STATS ${JSON.stringify(stats)}\n`);
  process.exit(code ?? 1);
};
child.on("error", (e) => {
  console.error(`couldn't start Pi: ${e.message}`);
  done(1);
});
child.on("close", (code) => done(code));
