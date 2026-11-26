#!/usr/bin/env node
// Build a Gardenwall release on this computer and put every file for upload
// in one folder (release checklist E2; docs/internal/RELEASING.md).
//
//   node scripts/release.mjs --version 0.1.0
//   node scripts/release.mjs --version 0.1.0 --test   (a trial build, never for upload)
//
// --test allows a working tree with changes and skips the strict preflight
// (which also wants every commit pushed), so the script and the signing
// steps can be tried before the release commit exists (checklist E3). Its
// files go to target/release-files/<version>-test/ and are marked as a test.
//
// Release files are built, checked and signed on the maintainer's computer, never
// by a hosted CI service. This script:
//   1. refuses a version that differs from the one in the six version files,
//      and a working tree with changes (the release commit must be clean);
//   2. runs the release preflight, builds with `app:bundle` (scripts/bundle.mjs,
//      the release compiler settings), then runs the preflight again --strict;
//   3. unpacks the built installer with 7-Zip and scans every file in it for
//      build-machine paths and names (checklist B9/F3);
//   4. writes the CycloneDX SBOMs (cargo-cyclonedx, pinned as in ci.yml, and
//      `npm sbom`), the build environment record (B22) and SHA256SUMS.txt;
//   5. prints the signing and release-file commands for the maintainer.
// It never signs, never uploads, never calls GitHub and needs no network
// beyond the crates Cargo may still have to download for the build.
//
// Output: target/release-files/<version>/

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir, hostname, userInfo } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CYCLONEDX_VERSION = "0.5.10"; // keep in step with .github/workflows/ci.yml
const SEVEN_ZIP = ["C:\\Program Files\\7-Zip\\7z.exe", "C:\\Program Files (x86)\\7-Zip\\7z.exe"];

const root = resolve(join(dirname(fileURLToPath(import.meta.url)), ".."));

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : undefined;
}
function fail(message) {
  console.error(`\nrelease: ${message}`);
  process.exit(1);
}
function step(title) {
  console.log(`\n== ${title}`);
}
function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { cwd: root, stdio: "inherit", ...opts });
}
function output(cmd, args, opts = {}) {
  try {
    return execFileSync(cmd, args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...opts }).trim();
  } catch {
    return "";
  }
}
function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}
function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(p));
    else out.push(p);
  }
  return out;
}

if (process.platform !== "win32") fail("releases are built on Windows (the NSIS installer)");

const version = arg("version");
if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) fail("--version must look like 0.1.0");
const test = process.argv.includes("--test");
if (test) console.log("TEST BUILD: not for upload");

// ---------------------------------------------------------------- 1. checks
step("Version and working tree");
const confVersion = JSON.parse(readFileSync(join(root, "crates", "gardenwall-app", "tauri.conf.json"), "utf8")).version;
if (confVersion !== version) fail(`tauri.conf.json says ${confVersion}, not ${version}`);
// The same files the preflight's B1 check reads; it repeats this in full.
const pkgVersion = JSON.parse(readFileSync(join(root, "ui", "package.json"), "utf8")).version;
if (pkgVersion !== version) fail(`ui/package.json says ${pkgVersion}, not ${version}`);

const allowedUntracked = new Set(["docs/template-review.md", "docs/TESTING-IN-APP_EDITED.md"]);
const dirty = output("git", ["status", "--porcelain"])
  .split("\n")
  .filter((l) => l && !(l.startsWith("?? ") && allowedUntracked.has(l.slice(3))));
if (dirty.length && test) console.log(`(test: ${dirty.length} uncommitted change(s) allowed)`);
else if (dirty.length) fail(`the working tree has ${dirty.length} change(s); release from a clean commit:\n  ${dirty.join("\n  ")}`);
const commit = output("git", ["rev-parse", "HEAD"]);
console.log(`version ${version}, commit ${commit}`);

// The maintainer's release preflight (extra checks kept outside the
// repository), named by GARDENWALL_PREFLIGHT; skipped when it isn't set.
const preflight = process.env.GARDENWALL_PREFLIGHT ? resolve(process.env.GARDENWALL_PREFLIGHT) : "";
const hasPreflight = preflight !== "" && existsSync(preflight);
if (process.env.GARDENWALL_PREFLIGHT && !hasPreflight) fail(`GARDENWALL_PREFLIGHT names no file: ${preflight}`);
// The public repository, as release-file.mjs names it: it need not be this
// folder's origin (0.1.0 is published from a one-commit export).
const publicRepo = readFileSync(join(root, "scripts", "release-file.mjs"), "utf8").match(/const REPO = "([^"]+)"/)?.[1];
if (!publicRepo) fail("scripts/release-file.mjs has no REPO");
const preflightArgs = ["-I", preflight, "--version", version, "--public-repo", publicRepo];
if (hasPreflight) {
  step("Preflight (before the build)");
  try {
    run("python", preflightArgs);
  } catch {
    if (!test) fail("the preflight has a FAIL; fix it before building");
    console.log("(test: preflight FAIL noted, build continues)");
  }
} else {
  console.log("(no GARDENWALL_PREFLIGHT set: the release preflight is skipped)");
}

const sevenZip = SEVEN_ZIP.find((p) => existsSync(p));
if (!sevenZip) fail("7-Zip is needed to look inside the installer (install it from 7-zip.org)");
if (!output("cargo", ["cyclonedx", "--version"]).includes(CYCLONEDX_VERSION)) {
  fail(`cargo-cyclonedx ${CYCLONEDX_VERSION} is needed:\n  cargo install cargo-cyclonedx --version ${CYCLONEDX_VERSION} --locked`);
}

// ---------------------------------------------------------------- 2. build
const out = join(root, "target", "release-files", test ? `${version}-test` : version);
rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, "sbom"), { recursive: true });

step("Build (npm --prefix ui ci, then app:bundle)");
// npm's own script run by this Node (no shell between them); npm.cmd only
// when it can't be found.
const npmCli = [process.env.npm_execpath, join(dirname(process.execPath), "node_modules", "npm", "bin", "npm-cli.js")].find(
  (p) => p && p.endsWith(".js") && existsSync(p),
);
const npm = npmCli ? [process.execPath, [npmCli]] : ["npm.cmd", []];
run(npm[0], [...npm[1], "--prefix", "ui", "ci", "--no-audit", "--no-fund"], { shell: npm[0].endsWith(".cmd") });
run(process.execPath, [join(root, "scripts", "bundle.mjs")]);

const nsis = join(root, "target", "release", "bundle", "nsis");
const installerName = `Gardenwall_${version}_x64-setup.exe`;
const builtInstaller = join(nsis, installerName);
if (!existsSync(builtInstaller)) fail(`the build didn't make ${relative(root, builtInstaller)}`);
const installer = join(out, installerName);
copyFileSync(builtInstaller, installer);

if (hasPreflight && !test) {
  step("Preflight --strict (after the build)");
  try {
    run("python", [...preflightArgs, "--strict"]);
  } catch {
    fail("the strict preflight didn't pass (see above); nothing is ready for upload");
  }
}

// ---------------------------------------------------------------- 3. scan
step("Scan the installer for build-machine paths and names");
const home = homedir();
const shortHome = output("cmd.exe", ["/d", "/s", "/c", `"for %I in ("${home}") do @echo %~sI"`], {
  windowsVerbatimArguments: true,
});
const terms = new Set(
  [
    home,
    home.replaceAll("\\", "/"),
    shortHome,
    basename(home),
    userInfo().username,
    hostname(),
    root,
    root.replaceAll("\\", "/"),
    ...(process.env.GARDENWALL_PRIVATE_TERMS || "").split(";"),
  ]
    .map((t) => (t || "").trim())
    .filter((t) => t.length >= 3),
);
const needles = [];
for (const t of terms) {
  needles.push(Buffer.from(t, "utf8"), Buffer.from(t, "utf16le"));
  needles.push(Buffer.from(t.toLowerCase(), "utf8"), Buffer.from(t.toLowerCase(), "utf16le"));
}
function containsTerm(file) {
  const data = readFileSync(file);
  const lower = Buffer.from(data.toString("latin1").toLowerCase(), "latin1");
  return needles.some((n) => data.includes(n) || lower.includes(n));
}

const unpacked = join(root, "target", "release-files", `.unpacked-${version}`);
rmSync(unpacked, { recursive: true, force: true });
execFileSync(sevenZip, ["x", "-y", `-o${unpacked}`, installer], { stdio: ["ignore", "ignore", "inherit"] });
const shipped = walk(unpacked);
const exes = shipped.filter((f) => /\.(exe|dll)$/i.test(f));
if (exes.length < 7) fail(`only ${exes.length} programs found inside the installer; expected the app and six helpers`);
// Name files only, never the matched text (it would print the home folder).
const dirtyFiles = shipped.filter(containsTerm).map((f) => relative(unpacked, f));
rmSync(unpacked, { recursive: true, force: true });
if (dirtyFiles.length) fail(`build-machine paths or names inside the installer:\n  ${dirtyFiles.join("\n  ")}`);
console.log(`${shipped.length} files (${exes.length} programs) clean`);

// ---------------------------------------------------------------- 4. SBOMs, environment, sums
step("SBOMs (CycloneDX)");
run("cargo", ["cyclonedx", "--format", "json", "--all-features"]);
for (const f of walk(join(root, "crates")).filter((f) => f.endsWith(".cdx.json"))) {
  renameSync(f, join(out, "sbom", basename(f)));
}
const uiSbom = execFileSync(npm[0], [...npm[1], "sbom", "--sbom-format", "cyclonedx", "--omit", "dev"], {
  cwd: join(root, "ui"),
  encoding: "utf8",
  shell: npm[0].endsWith(".cmd"),
  maxBuffer: 64 * 1024 * 1024,
});
writeFileSync(join(out, "sbom", "gardenwall-ui.cdx.json"), uiSbom);
// cargo-cyclonedx names Gardenwall's own crates by their folder
// ("path+file:///C:/…/crates/gardenwall-core#0.1.0"). Replace the repository
// folder with "/gardenwall", as the release build's path remapping does; the
// same text is replaced everywhere, so references between entries still match.
const rootForms = [root.replaceAll("\\", "/"), root.replaceAll("\\", "\\\\"), root];
for (const f of walk(join(out, "sbom"))) {
  let text = readFileSync(f, "utf8");
  for (const form of rootForms) text = text.split(form).join("/gardenwall");
  JSON.parse(text); // still valid JSON
  writeFileSync(f, text);
}
const dirtySboms = walk(join(out, "sbom")).filter(containsTerm).map((f) => basename(f));
if (dirtySboms.length) fail(`build-machine paths or names inside the SBOMs: ${dirtySboms.join(", ")}`);
const sboms = readdirSync(join(out, "sbom"));
console.log(`${sboms.length} SBOM files`);

step("Build environment (checklist B22)");
const tauriCli = JSON.parse(
  readFileSync(join(root, "ui", "node_modules", "@tauri-apps", "cli", "package.json"), "utf8"),
).version;
const nsisDir = join(process.env.LOCALAPPDATA || "", "tauri", "NSIS");
const makensis = existsSync(join(nsisDir, "makensis.exe")) ? join(nsisDir, "makensis.exe") : join(nsisDir, "Bin", "makensis.exe");
const vswhere = join(process.env["ProgramFiles(x86)"] || "", "Microsoft Visual Studio", "Installer", "vswhere.exe");
const msvc = existsSync(vswhere)
  ? output(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "catalog_productDisplayVersion"])
  : "";
const environment = [
  `Gardenwall ${version}, commit ${commit}${test ? " (TEST BUILD, not for upload)" : ""}`,
  `rustc: ${output("rustc", ["-V"])}`,
  `cargo: ${output("cargo", ["-V"])}`,
  `node: ${process.version}`,
  `npm: ${output(npm[0], [...npm[1], "-v"], { shell: npm[0].endsWith(".cmd") })}`,
  `Tauri CLI: ${tauriCli}`,
  `NSIS: ${existsSync(makensis) ? output(makensis, ["/VERSION"]) : "not found"}`,
  `Windows: ${output("cmd.exe", ["/d", "/c", "ver"])}`,
  `Visual Studio (MSVC): ${msvc || "not found"}`,
  `cargo-cyclonedx: ${CYCLONEDX_VERSION}`,
].join("\n");
writeFileSync(join(out, "BUILD-ENVIRONMENT.txt"), `${environment}\n`);
console.log(environment);

step("SHA256SUMS.txt");
const sums = [installer, ...sboms.map((f) => join(out, "sbom", f))]
  .map((f) => `${sha256(f)}  ${relative(out, f).replaceAll("\\", "/")}`)
  .join("\n");
writeFileSync(join(out, "SHA256SUMS.txt"), `${sums}\n`);
console.log(sums);

// ---------------------------------------------------------------- 5. next steps
const size = (statSync(installer).size / 1024 / 1024).toFixed(1);
const notes = join(root, "docs", "internal", "release-notes", `${version}.txt`);
if (test) writeFileSync(join(out, "TEST-BUILD-NOT-FOR-UPLOAD.txt"), `Trial build of ${commit}; never upload these files.\n`);
console.log(`
${test ? "TEST BUILD (not for upload) " : ""}Ready in ${relative(root, out)}  (installer ${size} MB, SHA-256 ${sha256(installer)})

Nothing is signed or uploaded. Next steps, in order:
  1. Sign the update (asks for the key's password):
       npx --prefix ui tauri signer sign -f "<path to gardenwall-update.key>" --app-version ${version} "${installer}"
  2. Write latest.json:
       node scripts/release-file.mjs --version ${version} --installer "${installer}" --notes "${notes}" --out "${join(out, "latest.json")}"
  3. Tag v${version} on ${commit.slice(0, 7)}, push, and create the GitHub release by hand (docs/internal/RELEASING.md).
`);
