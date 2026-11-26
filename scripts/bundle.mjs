// Build the Windows installer on this computer: stage the helper programs
// and run `tauri build`, all with the release settings from release-env.mjs,
// so no shipped program names the folders it was built from.
//
// Run by `npm --prefix ui run app:bundle`. Release builds are made here, on
// the maintainer's computer, never by a hosted CI service
// (docs/internal/RELEASING.md).

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { releaseEnv } from "./release-env.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const env = releaseEnv(root);
// The Tauri CLI pinned in ui/package.json, run directly (never through npx,
// which could fetch a different copy).
const tauri = join(root, "ui", "node_modules", "@tauri-apps", "cli", "tauri.js");
if (!existsSync(tauri)) throw new Error("the Tauri CLI isn't installed: run `npm --prefix ui ci` first");

// The installer ships THIRD_PARTY_LICENSES.md: refuse to build with one that
// no longer matches the dependencies.
execFileSync("python", [join(root, "scripts", "licence-notices.py"), "--check"], { cwd: root, stdio: "inherit" });

execFileSync(process.execPath, [join(root, "scripts", "stage-helpers.mjs")], { cwd: root, env, stdio: "inherit" });

// Every shipped program must start on a PC without the Visual C++
// Redistributable (found on a clean Windows Sandbox, 2026-11-24).
const startsAnywhere = (files) =>
  process.platform !== "win32" ||
  execFileSync("python", [join(root, "scripts", "pe-imports.py"), "--check", ...files], { cwd: root, stdio: "inherit" });
const binaries = join(root, "crates", "gardenwall-app", "binaries");
startsAnywhere(readdirSync(binaries).filter((f) => f.endsWith(".exe")).map((f) => join(binaries, f)));

execFileSync(process.execPath, [tauri, "build", "--config", "tauri.bundle.conf.json", ...process.argv.slice(2)], {
  cwd: join(root, "crates", "gardenwall-app"),
  env,
  stdio: "inherit",
});
startsAnywhere([join(root, "target", "release", "gardenwall-app.exe")]);
