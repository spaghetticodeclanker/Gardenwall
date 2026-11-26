// Compiler settings for release builds that leave the build machine out of
// the shipped programs (release checklist B9).
//
// Without them, panic messages and debug paths in every .exe name the
// folders the code was built from: the home folder (and so the user name),
// Cargo's registry, rustup's toolchain and the repository. Release builds
// are made on the maintainer's computer, not by a hosted service
// (docs/internal/RELEASING.md), so these must be remapped here.
//
// `trim-paths` would do this but isn't stable on the pinned toolchain, so
// this uses `--remap-path-prefix`. The flags go in CARGO_ENCODED_RUSTFLAGS
// (separated by 0x1f), because the folders may contain spaces, which plain
// RUSTFLAGS would split. When several prefixes match, rustc uses the last
// one, so the broad home folder comes first and narrower folders after it.

import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const SEP = "\x1f";

/** The 8.3 short spelling of an existing Windows folder (created if missing). */
function shortPath(p) {
  mkdirSync(p, { recursive: true });
  const out = execFileSync("cmd.exe", ["/d", "/s", "/c", `"for %I in ("${p}") do @echo %~sI"`], {
    encoding: "utf8",
    windowsVerbatimArguments: true,
  });
  return out.trim() || p;
}

/** Environment for `cargo build --release` of anything that ships. */
export function releaseEnv(root, base = process.env) {
  const home = homedir();
  // C code compiled by build scripts (aws-lc, SQLite) keeps its source paths
  // in assertion messages, and the remapping above only reaches Rust. So
  // release builds keep their own Cargo folder (registry and git sources)
  // inside the repository's target folder, a path with no user name in it,
  // and the C compiler is told to trim it. Cargo checks every downloaded
  // crate against Cargo.lock as usual.
  const cargoHome = base.GARDENWALL_RELEASE_CARGO_HOME || join(resolve(root), "target", "release-home", "cargo");
  const rustupHome = base.RUSTUP_HOME || join(home, ".rustup");
  const remaps = [
    [home, "/home"],
    [rustupHome, "/rustup"],
    [cargoHome, "/cargo"],
    [resolve(root), "/gardenwall"],
  ];
  const flags = [];
  for (const [from, to] of remaps) {
    flags.push(`--remap-path-prefix=${from}=${to}`);
    // Some paths reach rustc with forward slashes; remap those as well.
    if (from.includes("\\")) flags.push(`--remap-path-prefix=${from.replaceAll("\\", "/")}=${to}`);
  }
  if (process.platform === "win32") {
    // Keep only the file name of the debug-symbols file in each .exe, not
    // the folder it was written to.
    flags.push("-Clink-arg=/PDBALTPATH:%_PDB%");
    // Link the C runtime in, so the programs start without the Visual C++
    // Redistributable. .cargo/config.toml says the same for other builds,
    // but CARGO_ENCODED_RUSTFLAGS replaces its rustflags, so it's repeated.
    flags.push("-Ctarget-feature=+crt-static");
  }
  const existing = base.CARGO_ENCODED_RUSTFLAGS
    ? base.CARGO_ENCODED_RUSTFLAGS.split(SEP)
    : (base.RUSTFLAGS || "").split(/\s+/).filter(Boolean);
  const env = { ...base, CARGO_HOME: cargoHome, CARGO_ENCODED_RUSTFLAGS: [...existing, ...flags].join(SEP) };
  delete env.RUSTFLAGS;
  if (process.platform === "win32") {
    // MSVC's /d1trimfile removes a prefix from __FILE__. The cc crate splits
    // CFLAGS on spaces, so only paths without spaces can be listed; the
    // release Cargo folder and the repository are such paths (checked here).
    // aws-lc's build hands the compiler 8.3 short names (GARDEN~1), so trim
    // those spellings too.
    const long = [cargoHome, resolve(root)];
    const trims = [...new Set([...long, ...long.map(shortPath)])].map((p) => {
      if (/\s/.test(p)) throw new Error(`release builds need a path without spaces: ${p}`);
      return `/d1trimfile:${p}\\`;
    });
    for (const name of ["CFLAGS", "CXXFLAGS"]) {
      env[name] = [base[name], ...trims].filter(Boolean).join(" ");
    }
  }
  return env;
}
