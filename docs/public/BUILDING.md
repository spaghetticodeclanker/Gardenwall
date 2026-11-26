# Building Gardenwall from source

Gardenwall is a security-first desktop app for running local AI models. Its one rule shapes everything below: **the model is untrusted.** Every capability is granted by the trusted core and enforced again by the operating system's sandbox, whatever the model does.

Gardenwall isn't open to outside contributions at the moment. This page is for building and checking it yourself. To report a security problem, see [`SECURITY.md`](../../SECURITY.md).

## Setting up

1. Check your PC. This only reports what's missing and how to fix it; it never installs anything:

   ```bash
   node scripts/doctor.mjs
   ```

   You need:
   - Rust, at the version in `rust-toolchain.toml`, with rustfmt and clippy (rustup picks the version up automatically);
   - Node.js 24;
   - cargo-deny;
   - on Windows, Visual Studio's C++ build tools and the WebView2 runtime;
   - on Linux, the WebKitGTK development packages that Tauri uses.

   Optional: Python 3 (for the licence check), the `x86_64-unknown-linux-musl` target and WSL Ubuntu (both for the Linux sandbox tests on Windows).

2. Install the UI's packages and run the app:

   ```bash
   npm --prefix ui ci
   npm --prefix ui run app:dev
   ```

   Models load only on Windows for now. The process sandbox is built there, and other platforms refuse to load a model rather than run it unsandboxed.

3. To work on the UI without the Rust core, run `npm --prefix ui run dev -- --port 1421` and open `http://localhost:1421/mock.html`. `ui/src/devMock.ts` fakes every command the core offers.

## Checks

Run every check in one go:

```bash
node scripts/check.mjs           # everything (several minutes)
node scripts/check.mjs --quick   # format, lints and the UI type-check
node scripts/check.mjs --list    # the checks, by name; --skip wsl,deny leaves some out
```

That covers:
- `cargo fmt`;
- clippy with warnings as errors, plus the Linux cross-lint on Windows;
- the workspace tests;
- the build of the models crate without network code;
- `cargo deny`;
- the licence notices;
- `npm audit` and the UI build;
- on Windows with WSL, the Linux sandbox suites.

CI runs the same checks on Windows, Linux and macOS, except the WSL suites.

## Releases

Release installers are built and signed on the maintainer's computer, never by CI; CI only runs the checks again. See [how releases are made](RELEASES.md).
