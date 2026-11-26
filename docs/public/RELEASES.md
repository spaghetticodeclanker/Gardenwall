# How releases are made and verified

## Where releases come from

Gardenwall is released only on this repository's [Releases page](https://github.com/spaghetticodeclanker/gardenwall/releases). Each release has the Windows installer, a `SHA256SUMS.txt` file with the SHA-256 of every file, software bills of materials (CycloneDX) listing every component, and the update files the app reads.

## How a release is built

- **Built on the developer's own computer**, from the tagged source, never by a hosted build service. The automated checks on GitHub run again on every change, but they never build, sign or upload release files. This keeps the signing key off third-party servers.
- **Every check passes first:** formatting, lints with warnings treated as errors, the full test suite, the sandbox escape tests on Windows and Linux, licence and dependency audits, and a scan of every program in the installer to make sure it contains no paths or names from the build machine.
- **Tested before release:** each version goes through release candidates used for real work, then a clean install on a fresh Windows machine, an upgrade over existing data, both ways of uninstalling, and lockdown and offline mode in the installed app.

## Checking a download

The installer isn't code-signed yet, so Windows shows "Windows protected your PC" the first time you run it: choose **More info → Run anyway**.

Before running it, you can check that the file you downloaded is the one released:

```powershell
Get-FileHash .\Gardenwall_0.1.0_x64-setup.exe -Algorithm SHA256
```

Compare the result with the line for the installer in `SHA256SUMS.txt` on the release page. For now that hash is published on GitHub only, so it shows the download wasn't damaged, not that it wasn't tampered with on GitHub itself. A signed hash list published outside GitHub is planned.

## How updates are checked

- Gardenwall checks for a new version on the schedule you choose on the Updates page (when it opens, daily, weekly, or only when you press Check for updates). Offline mode and lockdown skip it.
- The check reads one small release file from this repository's Releases, over HTTPS, from GitHub's release hosts only, with a size limit.
- The installer is downloaded only when you choose **Download**, and kept only if its signature matches Gardenwall's release key, which is built into the app, and names the version that was offered. A signature made for a different version is refused.
- The same or an older version is never offered as an update.
- Nothing installs until you press **Install now**.

## If something goes wrong

- **A bad release:** a fixed, higher version is published. Installed copies never move to a lower version by themselves; an older version can always be downloaded from the Releases page.
- **A security fix:** it ships as a new version, marked as a security update in the app and in the changelog, never as a replaced file.
- Only the latest 0.x version receives security fixes. See [SECURITY.md](../../SECURITY.md) to report a problem.
