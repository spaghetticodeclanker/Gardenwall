#!/usr/bin/env node
// Every check a change must pass before it is reported done, in one
// command (docs/internal/PROJECT-SETUP-PLAN.md A5). CI runs its own steps
// (.github/workflows/ci.yml); this script is for people, and runs what CI
// can't: the Linux sandbox suites in WSL on a Windows PC.
//
//   node scripts/check.mjs            all checks
//   node scripts/check.mjs --quick    format, lints and the UI type-check only
//   node scripts/check.mjs --skip wsl,deny
//   node scripts/check.mjs --list     the check names
//
// Run from anywhere; it works from the repository root. Nothing is
// installed or changed except build output under target/ and ui/dist/.
// `node scripts/doctor.mjs` says what is missing when a check can't start.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WINDOWS = process.platform === "win32";
const MUSL = "x86_64-unknown-linux-musl";
// Crates with no C dependencies, so they cross-compile to Linux from
// Windows. The store, models, runtime, egress (its TLS library) and app
// need C toolchains; CI covers them on Linux.
const LINUX_LINT = ["gardenwall-policy", "gardenwall-fs", "gardenwall-sandbox", "gardenwall-agent", "gardenwall-core"];
// The Linux sandbox suites (escape tests, the sandboxed shell, policy).
const LINUX_TESTS = ["gardenwall-sandbox", "gardenwall-agent", "gardenwall-policy"];

const args = process.argv.slice(2);
const quick = args.includes("--quick");
const skipAt = args.indexOf("--skip");
const skip = new Set(skipAt >= 0 ? (args[skipAt + 1] ?? "").split(",").filter(Boolean) : []);

/** One check: a name, what it is, and how to run it (true when it passed). */
const checks = [
  { name: "ui", what: "UI type-check and bundle", quick: false, run: () => npm("run", "build") },
  { name: "ui-types", what: "UI type-check", quick: true, onlyQuick: true, run: () => npm("run", "typecheck") },
  { name: "audit", what: "npm audit (production packages)", run: () => npm("audit", "--omit=dev") },
  { name: "fmt", what: "cargo fmt --check", quick: true, run: () => cargo("fmt", "--all", "--check") },
  { name: "licences", what: "licence notices up to date", run: () => python("scripts/licence-notices.py", "--check") },
  { name: "clippy", what: "clippy -D warnings (this platform)", quick: true, run: () => cargo("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings") },
  {
    name: "clippy-linux",
    what: "clippy -D warnings (Linux, cross-compiled)",
    quick: true,
    when: () => WINDOWS && hasMusl(),
    why: `the ${MUSL} target isn't installed (rustup target add ${MUSL})`,
    run: () => cargo("clippy", "--target", MUSL, ...LINUX_LINT.flatMap((c) => ["-p", c]), "--all-targets", "--locked", "--", "-D", "warnings"),
  },
  // --no-fail-fast: one failing test program doesn't hide the others' results.
  { name: "test", what: "cargo test --workspace", run: () => cargo("test", "--workspace", "--locked", "--no-fail-fast") },
  {
    name: "no-hub",
    what: "models crate builds without network code",
    run: () =>
      cargo("clippy", "-p", "gardenwall-models", "--no-default-features", "--lib", "--tests", "--locked", "--", "-D", "warnings") &&
      cargo("test", "-p", "gardenwall-models", "--no-default-features", "--locked") &&
      noNetworkCrates(),
  },
  { name: "deny", what: "cargo deny (advisories, bans, licences, sources)", run: () => cargo("deny", "check", "advisories", "bans", "licenses", "sources") },
  {
    name: "wsl",
    what: "Linux sandbox suites in WSL Ubuntu",
    when: () => WINDOWS && hasMusl() && hasUbuntu(),
    why: "needs WSL with an Ubuntu distribution and the musl target",
    run: wslSuites,
  },
];

if (args.includes("--list")) {
  for (const c of checks) console.log(`${c.name.padEnd(14)} ${c.what}`);
  process.exit(0);
}

const results = [];
for (const c of checks) {
  if (quick ? !c.quick : c.onlyQuick) continue;
  if (skip.has(c.name)) {
    results.push([c, "skipped", "--skip"]);
    continue;
  }
  if (c.when && !c.when()) {
    results.push([c, "not run", c.why]);
    continue;
  }
  banner(c.what);
  const started = Date.now();
  let ok = false;
  try {
    ok = c.run();
  } catch (e) {
    console.error(String(e?.message ?? e));
  }
  results.push([c, ok ? "passed" : "FAILED", `${Math.round((Date.now() - started) / 1000)} s`]);
}

console.log("\nSummary");
for (const [c, status, note] of results) console.log(`  ${status.padEnd(8)} ${c.name.padEnd(14)} ${note ?? ""}`);
const failed = results.filter(([, s]) => s === "FAILED").length;
const notRun = results.filter(([, s]) => s === "not run").length;
if (notRun) console.log(`\n${notRun} check(s) couldn't run on this PC; CI or another PC must cover them. node scripts/doctor.mjs says what's missing.`);
process.exit(failed ? 1 : 0);

// ---- helpers ---------------------------------------------------------------

function banner(text) {
  console.log(`\n=== ${text} ===`);
}

/** Run a program with fixed arguments in the repository root. */
function run(program, argv, options = {}) {
  const r = spawnSync(program, argv, { cwd: ROOT, stdio: "inherit", ...options });
  if (r.error) {
    console.error(`${program}: ${r.error.message}`);
    return false;
  }
  return r.status === 0;
}

function cargo(...argv) {
  return run("cargo", argv);
}

function npm(...argv) {
  if (!existsSync(path.join(ROOT, "ui", "node_modules"))) {
    console.error("ui/node_modules is missing: run npm --prefix ui ci first.");
    return false;
  }
  // npm is a .cmd launcher on Windows, which Node starts only through the
  // shell. The arguments are fixed strings from this file.
  return WINDOWS ? run(["npm", "--prefix", "ui", ...argv].join(" "), [], { shell: true }) : run("npm", ["--prefix", "ui", ...argv]);
}

function python(...argv) {
  for (const exe of WINDOWS ? ["python", "py"] : ["python3", "python"]) {
    const probe = spawnSync(exe, ["--version"], { stdio: "ignore" });
    if (!probe.error && probe.status === 0) return run(exe, argv);
  }
  console.error("Python wasn't found (python, py or python3).");
  return false;
}

function capture(program, argv, options = {}) {
  const r = spawnSync(program, argv, { cwd: ROOT, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, ...options });
  return r.error || r.status !== 0 ? null : r.stdout;
}

function noNetworkCrates() {
  const tree = capture("cargo", ["tree", "-p", "gardenwall-models", "--no-default-features", "-e", "normal", "--locked"]);
  if (tree === null) return false;
  const found = tree.split("\n").filter((l) => /reqwest|hyper|rustls|aws-lc|tokio/.test(l));
  if (found.length) {
    console.error(`network crates found in the no-hub build:\n${found.join("\n")}`);
    return false;
  }
  return true;
}

function hasMusl() {
  return (capture("rustup", ["target", "list", "--installed"]) ?? "").split(/\r?\n/).includes(MUSL);
}

function hasUbuntu() {
  // wsl.exe prints its list in UTF-16.
  const r = spawnSync("wsl.exe", ["--list", "--quiet"], { encoding: "utf16le" });
  if (r.error || r.status !== 0) return false;
  return r.stdout.split(/\r?\n/).some((l) => l.replace(/\0/g, "").trim() === "Ubuntu");
}

/** Build the Linux test programs here, then run each inside WSL. */
function wslSuites() {
  const env = { ...process.env, CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER: "rust-lld" };
  const pkgs = LINUX_TESTS.flatMap((c) => ["-p", c]);
  if (!run("cargo", ["build", ...pkgs, "--target", MUSL, "--bins", "--locked"], { env })) return false;
  const out = capture("cargo", ["test", ...pkgs, "--target", MUSL, "--no-run", "--locked", "--message-format=json"], { env, stdio: ["ignore", "pipe", "inherit"] });
  if (out === null) return false;
  const programs = out
    .split("\n")
    .filter((l) => l.startsWith("{"))
    .map((l) => JSON.parse(l))
    .filter((m) => m.reason === "compiler-artifact" && m.profile?.test && m.executable)
    .map((m) => path.relative(ROOT, m.executable).split(path.sep).join("/"));
  if (programs.length === 0) {
    console.error("no Linux test programs were built");
    return false;
  }
  const drive = ROOT[0].toLowerCase();
  const repo = `/mnt/${drive}${ROOT.slice(2).split(path.sep).join("/")}`;
  let ok = true;
  for (const p of programs) {
    console.log(`\n--- ${path.basename(p)} ---`);
    const script = `cd '${repo.replace(/'/g, "'\\''")}' && './${p.replace(/'/g, "'\\''")}'`;
    ok = run("wsl.exe", ["-d", "Ubuntu", "--", "bash", "-c", script]) && ok;
  }
  return ok;
}
