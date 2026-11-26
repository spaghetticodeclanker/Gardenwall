// Build Gardenwall's helper programs and stage them for the installer.
//
// The app runs each helper in its own sandbox and finds it next to its own
// executable (gardenwall-app's `stage_helper`). Tauri installs "external
// binaries" exactly there, but wants them named with the target triple at
// build time: binaries/<name>-<triple>[.exe]. This script builds them in
// release mode and copies them under those names. It is run by
// `npm --prefix ui run app:bundle` before `tauri build`.
//
// Usage: node scripts/stage-helpers.mjs [--debug]

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { releaseEnv } from "./release-env.mjs";

// Each helper and the package that builds it (the pre-commit secret check
// is a binary of the agent crate, so it shares its scanner exactly).
const HELPERS = [
  ["gardenwall-relay", "gardenwall-relay"],
  ["gardenwall-parse", "gardenwall-parse"],
  ["gardenwall-binary", "gardenwall-binary"],
  ["gardenwall-js", "gardenwall-js"],
  ["gardenwall-fetch", "gardenwall-fetch"],
  ["gardenwall-gitscan", "gardenwall-agent"],
];

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const debug = process.argv.includes("--debug");
const profile = debug ? "debug" : "release";

const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((l) => l.startsWith("host:"))
  ?.slice("host:".length)
  .trim();
if (!host) throw new Error("couldn't read the target triple from rustc -vV");
const exe = process.platform === "win32" ? ".exe" : "";

// Release helpers ship, so they get the settings that keep the build
// machine's folders out of them (release-env.mjs).
const env = debug ? process.env : releaseEnv(root);
for (const [bin, pkg] of HELPERS) {
  const args = ["build", "--locked", "-p", pkg, "--bin", bin];
  if (!debug) args.push("--release");
  execFileSync("cargo", args, { cwd: root, env, stdio: "inherit" });
}

const out = join(root, "crates", "gardenwall-app", "binaries");
mkdirSync(out, { recursive: true });
for (const [h] of HELPERS) {
  const from = join(root, "target", profile, `${h}${exe}`);
  const to = join(out, `${h}-${host}${exe}`);
  copyFileSync(from, to);
  console.log(`staged ${h} -> ${to}`);
}
