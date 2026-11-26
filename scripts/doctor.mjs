#!/usr/bin/env node
// What a PC needs to build, run and check Gardenwall, and how to fix each
// missing piece (docs/internal/PROJECT-SETUP-PLAN.md A5). Read-only: it looks and
// reports, and never installs or changes anything.
//
//   node scripts/doctor.mjs

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WINDOWS = process.platform === "win32";
const MUSL = "x86_64-unknown-linux-musl";

const wantedRust = (() => {
  const t = readFileSync(path.join(ROOT, "rust-toolchain.toml"), "utf8");
  return /channel\s*=\s*"([^"]+)"/.exec(t)?.[1] ?? "1.98";
})();

/** Output of a program with fixed arguments, or null. */
function out(program, argv, options = {}) {
  const r = spawnSync(program, argv, { cwd: ROOT, encoding: "utf8", ...options });
  return r.error || r.status !== 0 ? null : `${r.stdout ?? ""}${r.stderr ?? ""}`.trim();
}

function version(text) {
  return /(\d+)(?:\.(\d+))?(?:\.(\d+))?/.exec(text ?? "")?.slice(1).map((n) => Number(n ?? 0)) ?? null;
}

function atLeast(v, want) {
  const w = version(want);
  if (!v || !w) return false;
  for (let i = 0; i < 3; i++) {
    if ((v[i] ?? 0) !== (w[i] ?? 0)) return (v[i] ?? 0) > (w[i] ?? 0);
  }
  return true;
}

const rows = [];
/** level: "ok", "missing" (needed), "optional" (only some checks need it). */
function row(level, name, detail, fix) {
  rows.push({ level, name, detail, fix });
}

// Rust, the pinned version, and its parts.
const rustc = out("rustc", ["--version"]);
if (!rustc) {
  row("missing", "Rust", "rustc wasn't found", "Install rustup from https://rustup.rs; this repository's rust-toolchain.toml then picks the version.");
} else {
  const v = version(rustc);
  row(atLeast(v, wantedRust) ? "ok" : "missing", "Rust", rustc, atLeast(v, wantedRust) ? null : `rustup toolchain install ${wantedRust} (rust-toolchain.toml asks for it)`);
  const components = out("rustup", ["component", "list", "--installed"]) ?? "";
  for (const c of ["rustfmt", "clippy"]) {
    const has = components.split(/\r?\n/).some((l) => l.startsWith(c));
    row(has ? "ok" : "missing", c, has ? "installed" : "not installed", has ? null : `rustup component add ${c}`);
  }
  if (WINDOWS) {
    const targets = (out("rustup", ["target", "list", "--installed"]) ?? "").split(/\r?\n/);
    const has = targets.includes(MUSL);
    row(has ? "ok" : "optional", "Linux target", has ? MUSL : "not installed (Linux lints and the WSL suites need it)", has ? null : `rustup target add ${MUSL}`);
  }
}

// cargo-deny.
const deny = out("cargo", ["deny", "--version"]);
row(deny ? "ok" : "missing", "cargo-deny", deny ?? "not installed", deny ? null : "cargo install cargo-deny --locked");

// Node.js 24 and the UI's packages.
const node = process.versions.node;
row(atLeast(version(node), "24") ? "ok" : "missing", "Node.js", `v${node}`, atLeast(version(node), "24") ? null : "Install Node.js 24 LTS from https://nodejs.org");
const modules = existsSync(path.join(ROOT, "ui", "node_modules"));
row(modules ? "ok" : "missing", "UI packages", modules ? "ui/node_modules present" : "not installed", modules ? null : "npm --prefix ui ci");

// Python (the licence check).
const py = (WINDOWS ? ["python", "py"] : ["python3", "python"]).map((p) => out(p, ["--version"])).find(Boolean);
row(py ? "ok" : "optional", "Python", py ?? "not found (the licence check needs it)", py ? null : "Install Python 3 from https://www.python.org");

if (WINDOWS) {
  // Visual Studio's C++ build tools (the MSVC linker and Windows SDK).
  const vswhere = path.join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Microsoft Visual Studio", "Installer", "vswhere.exe");
  const vs = existsSync(vswhere)
    ? out(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "displayName"])
    : null;
  row(vs ? "ok" : "missing", "C++ build tools", vs || "not found", vs ? null : 'Install Build Tools for Visual Studio with "Desktop development with C++": https://visualstudio.microsoft.com/visual-cpp-build-tools/');

  // WebView2 (the app's window).
  const key = "HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
  const wv = out("reg.exe", ["query", key, "/v", "pv"]);
  const wvVersion = /pv\s+REG_SZ\s+(\S+)/.exec(wv ?? "")?.[1];
  row(wvVersion && wvVersion !== "0.0.0.0" ? "ok" : "missing", "WebView2", wvVersion ?? "not found", wvVersion ? null : "Install the Evergreen WebView2 Runtime: https://developer.microsoft.com/microsoft-edge/webview2/");

  // WSL Ubuntu (the Linux sandbox suites).
  const r = spawnSync("wsl.exe", ["--list", "--quiet"], { encoding: "utf16le" });
  const ubuntu = !r.error && r.status === 0 && r.stdout.split(/\r?\n/).some((l) => l.replace(/\0/g, "").trim() === "Ubuntu");
  row(ubuntu ? "ok" : "optional", "WSL Ubuntu", ubuntu ? "installed" : "not found (only the Linux sandbox suites need it)", ubuntu ? null : "wsl --install -d Ubuntu");

  // Git for Windows (the agent's Git features and their tests).
  const git = ["ProgramFiles", "ProgramW6432"].map((v) => process.env[v]).filter(Boolean).some((p) => existsSync(path.join(p, "Git", "cmd", "git.exe")));
  row(git ? "ok" : "optional", "Git for Windows", git ? "installed in Program Files" : "not in Program Files (Git tests skip)", git ? null : "Install Git for Windows for all users: https://git-scm.com/download/win");
} else if (process.platform === "linux") {
  const webkit = out("pkg-config", ["--modversion", "webkit2gtk-4.1"]);
  row(webkit ? "ok" : "missing", "WebKitGTK 4.1", webkit ?? "not found", webkit ? null : "sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev");
}

const width = Math.max(...rows.map((r) => r.name.length));
const mark = { ok: "ok      ", missing: "MISSING ", optional: "optional" };
for (const r of rows) {
  console.log(`${mark[r.level]}  ${r.name.padEnd(width)}  ${r.detail}`);
  if (r.fix) console.log(`${" ".repeat(10 + width)}  → ${r.fix}`);
}
const missing = rows.filter((r) => r.level === "missing").length;
console.log(missing ? `\n${missing} thing(s) to install before building.` : "\nReady to build. Run node scripts/check.mjs before reporting a change done.");
process.exit(missing ? 1 : 0);
