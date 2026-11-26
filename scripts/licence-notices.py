#!/usr/bin/env python3
"""Write THIRD_PARTY_LICENSES.md: the licences of everything the Windows
installer ships that Gardenwall didn't write.

- Rust: every crate compiled into the app or one of its helper programs
  (normal dependencies only, for x86_64-pc-windows-msvc), from
  `cargo metadata --locked`, with the licence files from each crate's
  published package.
- UI: the production npm packages (`npm ls --omit=dev`), with their licence
  files from ui/node_modules.

Identical licence texts are printed once, with every package that uses them.
A package that publishes no licence file is listed with its SPDX licence and
gets the standard text from the appendix.

Run from the repository root after `npm --prefix ui ci`:
    python scripts/licence-notices.py            # writes THIRD_PARTY_LICENSES.md
    python scripts/licence-notices.py --check    # exit 1 if the file is out of date
"""
import hashlib
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "THIRD_PARTY_LICENSES.md"
TARGET = "x86_64-pc-windows-msvc"
# The packages whose binaries the installer ships (see tauri.bundle.conf.json).
SHIPPED = [
    "gardenwall-app",
    "gardenwall-relay",
    "gardenwall-parse",
    "gardenwall-binary",
    "gardenwall-js",
    "gardenwall-fetch",
    "gardenwall-agent",  # gardenwall-gitscan
]
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)([-._ ].*)?$", re.I)
# Not licence texts, though their names match.
SKIP_FILE = re.compile(r"\.(rs|toml|json|js|ts|py|sh|html)$", re.I)


def licence_files(folder: Path) -> list[Path]:
    if not folder.is_dir():
        return []
    found = []
    for p in sorted(folder.iterdir()):
        if p.is_file() and LICENCE_FILE.match(p.name) and not SKIP_FILE.search(p.name):
            found.append(p)
    return found


def read_text(p: Path) -> str:
    text = p.read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n")
    return text.strip("\n﻿ ") + "\n"


def rust_packages():
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
            cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8",
        ).stdout
    )
    members = set(meta["workspace_members"])
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [p["id"] for p in meta["packages"] if p["id"] in members and p["name"] in SHIPPED]
    missing = set(SHIPPED) - {by_id[r]["name"] for r in roots}
    if missing:
        sys.exit(f"shipped packages not found in the workspace: {sorted(missing)}")
    seen, stack = set(), list(roots)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    out = []
    for pid in seen - members:
        p = by_id[pid]
        out.append({
            "name": p["name"],
            "version": p["version"],
            "license": p.get("license") or ("see file: " + p["license_file"] if p.get("license_file") else "not stated"),
            "url": p.get("repository") or p.get("homepage") or f"https://crates.io/crates/{p['name']}",
            "files": licence_files(Path(p["manifest_path"]).parent),
        })
    return sorted(out, key=lambda x: (x["name"], x["version"]))


def npm_packages():
    ui = ROOT / "ui"
    result = subprocess.run(
        "npm ls --omit=dev --all --json", cwd=ui, shell=True, capture_output=True, text=True, encoding="utf-8"
    )
    tree = json.loads(result.stdout or "{}")
    found = {}

    def walk(deps):
        for name, info in (deps or {}).items():
            version = info.get("version")
            if version and (name, version) not in found:
                folder = ui / "node_modules" / name
                pkg = {}
                if (folder / "package.json").exists():
                    pkg = json.loads((folder / "package.json").read_text(encoding="utf-8"))
                lic = pkg.get("license")
                if isinstance(lic, dict):
                    lic = lic.get("type")
                repo = pkg.get("repository")
                if isinstance(repo, dict):
                    repo = repo.get("url")
                found[(name, version)] = {
                    "name": name,
                    "version": version,
                    "license": lic or "not stated",
                    "url": (repo or f"https://www.npmjs.com/package/{name}").replace("git+", ""),
                    "files": licence_files(folder),
                }
            walk(info.get("dependencies"))

    walk(tree.get("dependencies"))
    if not found:
        sys.exit("no npm packages found: run `npm --prefix ui ci` first")
    return sorted(found.values(), key=lambda x: (x["name"], x["version"]))


APACHE_MARKERS = ("Apache License", "Version 2.0, January 2004", "TERMS AND CONDITIONS FOR USE, REPRODUCTION")
APACHE_END = "END OF TERMS AND CONDITIONS"


def is_apache(text: str) -> bool:
    flat = re.sub(r"\s+", " ", text)
    return all(m in flat for m in APACHE_MARKERS)


def apache_extra(text: str) -> bool:
    """Whether an Apache-2.0 file carries more than the terms and the usual
    appendix (another licence appended, say)."""
    if APACHE_END not in text:
        return False
    rest = text[text.index(APACHE_END) + len(APACHE_END):]
    end = rest.find("limitations under the License.")
    if end >= 0:
        rest = rest[end + len("limitations under the License."):]
    return len(rest.strip()) > 300


def apache_copyrights(text: str) -> list[str]:
    """Filled-in copyright lines in an Apache-2.0 file (its appendix or
    header), leaving out the appendix template."""
    out = []
    for line in text.splitlines():
        s = line.strip()
        if re.match(r"(?i)^copyright\s*(\(c\)|©|\d{4})", s) and "[" not in s and "{" not in s and len(s) < 200:
            out.append(s)
    return out


def render(rust, npm) -> str:
    texts = {}  # digest -> text
    users = defaultdict(list)  # digest -> ["name version"]
    bare = defaultdict(list)  # SPDX expression -> ["name version"] (no file published)
    apache_text = None  # the Apache-2.0 terms, printed once
    apache_users = {}  # "name version" -> its copyright lines

    def collect(pkgs):
        for p in pkgs:
            label = f"{p['name']} {p['version']}"
            if not p["files"]:
                bare[p["license"]].append(label)
                continue
            for f in p["files"]:
                text = read_text(f)
                if is_apache(text):
                    nonlocal apache_text
                    if apache_text is None and APACHE_END in text:
                        apache_text = text[: text.index(APACHE_END) + len(APACHE_END)] + "\n"
                    apache_users.setdefault(label, [])
                    for line in apache_copyrights(text):
                        if line not in apache_users[label]:
                            apache_users[label].append(line)
                    if not apache_extra(text):
                        continue
                    # More than the Apache terms (other licences appended):
                    # the whole file is printed too.
                # Texts that differ only in spacing or line breaks are one text.
                key = re.sub(r"\s+", " ", text).strip()
                d = hashlib.sha256(key.encode()).hexdigest()
                texts.setdefault(d, text)
                if label not in users[d]:
                    users[d].append(label)

    collect(rust)
    collect(npm)

    lines = [
        "# Third-party licences",
        "",
        "Generated by `scripts/licence-notices.py` from `Cargo.lock` and the UI's production npm packages. Do not edit by hand; run the script again after dependencies change.",
        "",
        "Gardenwall is licensed under Apache-2.0 (`LICENSE`). Material ported into Gardenwall's own source (Pi) and the bundled fonts and icons are in `THIRD_PARTY_NOTICES.md`. This file covers the libraries compiled into the Gardenwall app and its helper programs, and the libraries in its user interface.",
        "",
        "## Components that are downloaded, not shipped",
        "",
        "The installer doesn't include these; Gardenwall downloads them when you ask, from their publishers' release pages, and checks each against a pinned SHA-256:",
        "",
        "- **llama.cpp** (MIT, https://github.com/ggml-org/llama.cpp): the model runtime. Its CUDA builds include NVIDIA CUDA runtime libraries, distributed by the llama.cpp project under NVIDIA's CUDA licence terms.",
        "- **whisper.cpp** (MIT, https://github.com/ggml-org/whisper.cpp) and its Whisper speech models (MIT, from OpenAI's Whisper): voice input.",
        "- **Microsoft Edge WebView2 Runtime**: installed by Microsoft's own bootstrapper if it is missing, under Microsoft's licence.",
        "- **Language models** you download keep their own licences, shown on each model's Hugging Face page; Gardenwall doesn't change or relicense them.",
        "- **Ghidra** (Apache-2.0) and a JDK, if you choose to install them for binary analysis.",
        "",
        "## Rust libraries",
        "",
        "| Crate | Version | Licence | Source |",
        "|---|---|---|---|",
    ]
    for p in rust:
        lines.append(f"| {p['name']} | {p['version']} | {p['license']} | {p['url']} |")
    lines += ["", "## User interface libraries (npm)", "", "| Package | Version | Licence | Source |", "|---|---|---|---|"]
    for p in npm:
        lines.append(f"| {p['name']} | {p['version']} | {p['license']} | {p['url']} |")
    lines += ["", "## Licence texts", "", "Each text below is printed once, with the packages that ship it.", ""]
    if apache_users:
        names = sorted(apache_users, key=str.lower)
        lines += [
            "### Apache License 2.0",
            "",
            f"Used by {len(names)} packages, listed with the copyright lines their licence files carry:",
            "",
        ]
        for n in names:
            c = apache_users[n]
            lines.append(f"- {n}" + (": " + "; ".join(c) if c else ""))
        lines += ["", "```text", (apache_text or "").rstrip("\n"), "```", ""]
    for d, text in sorted(texts.items(), key=lambda kv: (sorted(users[kv[0]])[0].lower(), kv[0])):
        names = sorted(users[d], key=str.lower)
        lines.append("### " + ", ".join(names[:6]) + (f" and {len(names) - 6} more" if len(names) > 6 else ""))
        if len(names) > 6:
            lines.append("")
            lines.append("Used by: " + ", ".join(names))
        lines += ["", "```text", text.replace("```", "'''").rstrip("\n"), "```", ""]
    if bare:
        lines += [
            "## Packages that publish no licence file",
            "",
            "These packages declare their licence in their metadata but include no licence text; the standard text of each licence named applies, with the copyright of the package's authors (see each package's source). Each of those standard texts appears in this file above, with other packages that ship it.",
            "",
        ]
        for lic, names in sorted(bare.items()):
            lines.append(f"- **{lic}**: " + ", ".join(sorted(names, key=str.lower)))
        lines.append("")
        # Make sure that claim holds.
        markers = {
            "MIT": "Permission is hereby granted, free of charge",
            "Apache-2.0": "TERMS AND CONDITIONS FOR USE, REPRODUCTION",
            "BSD-3-Clause": "Neither the name of",
            "MPL-2.0": "Mozilla Public License Version 2.0",
            "Unlicense": "This is free and unencumbered software",
        }
        body = "\n".join(lines)
        for lic in bare:
            for spdx in re.findall(r"[A-Za-z0-9.\-]+", lic):
                if spdx in markers and markers[spdx] not in body:
                    sys.exit(f"no standard {spdx} text in the file for packages without a licence file")
    return "\n".join(lines)


def main():
    rust = rust_packages()
    npm = npm_packages()
    text = render(rust, npm)
    if "--check" in sys.argv:
        current = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if current != text:
            sys.exit("THIRD_PARTY_LICENSES.md is out of date: run scripts/licence-notices.py")
        print("THIRD_PARTY_LICENSES.md is up to date")
        return
    OUT.write_text(text, encoding="utf-8", newline="\n")
    no_file = sum(1 for p in rust + npm if not p["files"])
    print(f"Wrote {OUT.name}: {len(rust)} Rust crates, {len(npm)} npm packages, {no_file} without a licence file.")


if __name__ == "__main__":
    main()
