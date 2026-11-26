#!/usr/bin/env node
// Writes the release file (`latest.json`) that installed copies of Gardenwall
// read to find a new version (ADR-017 §3.3, docs/internal/RELEASING.md).
//
//   node scripts/release-file.mjs --version 1.0.1 --installer <setup.exe> \
//        --notes <notes.txt> [--security] [--out latest.json]
//
// The installer must already be signed by hand:
//   npx tauri signer sign -f <key> --app-version <version> <setup.exe>
// which writes <setup.exe>.sig. This script checks that the signature names
// the same version (installed copies refuse a mismatch), and builds the
// download address from the fixed release location. It never signs anything
// and needs no key.

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { basename } from "node:path";

const REPO = "https://github.com/spaghetticodeclanker/gardenwall";

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : undefined;
}
function fail(message) {
  console.error(`release-file: ${message}`);
  process.exit(1);
}

const version = arg("version");
const installer = arg("installer");
const notesPath = arg("notes");
const out = arg("out") ?? "latest.json";
const security = process.argv.includes("--security");

if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) fail("--version must look like 1.0.1");
if (!installer || !existsSync(installer)) fail("--installer must name the signed installer");
if (!installer.endsWith("-setup.exe")) fail("the installer should be the NSIS *-setup.exe");
const sigPath = `${installer}.sig`;
if (!existsSync(sigPath)) fail(`no signature at ${sigPath}; sign the installer first`);
if (!notesPath || !existsSync(notesPath)) fail("--notes must name a plain-text notes file");

const signature = readFileSync(sigPath, "utf8").trim();
// The .sig file is base64 of a minisign signature; its trusted comment holds
// tab-separated fields, including version:<version> when signed with
// --app-version.
const decoded = Buffer.from(signature, "base64").toString("utf8");
const trusted = decoded.split("\n").find((l) => l.startsWith("trusted comment: "));
if (!trusted) fail("the signature has no trusted comment");
const signedVersion = trusted
  .slice("trusted comment: ".length)
  .split("\t")
  .find((f) => f.startsWith("version:"))
  ?.slice("version:".length);
if (signedVersion !== version) {
  fail(`the signature is for version ${signedVersion ?? "(none)"}, not ${version}; sign with --app-version ${version}`);
}

const notes = readFileSync(notesPath, "utf8").trim();
const release = {
  version,
  notes,
  pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${REPO}/releases/download/v${version}/${encodeURIComponent(basename(installer))}`,
    },
  },
  gardenwall: { security },
};
writeFileSync(out, `${JSON.stringify(release, null, 2)}\n`);
console.log(`Wrote ${out} for version ${version}${security ? " (security release)" : ""}.`);
console.log(`Upload ${basename(installer)} and ${out} to the release tagged v${version}.`);
