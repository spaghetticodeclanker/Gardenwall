//! Gardenwall's own updates (ADR-017 §3.3).
//!
//! - **Check:** the core fetches the release file (`latest.json`) from the
//!   main repository's GitHub Releases with the runtime downloader's rules:
//!   HTTPS only, GitHub release hosts re-checked on every redirect, no
//!   system proxy, a size cap. Offline mode skips it. A check never
//!   downloads the installer.
//! - **Download** (the user's choice): the same downloader, into memory, at
//!   most [`MAX_INSTALLER`]. The bytes are checked against the minisign
//!   signature in the release file before they are kept; a mismatch is
//!   dropped and nothing is installed.
//! - **Install** (one more confirmation in the UI): the Tauri updater plugin
//!   checks the signature again, starts the NSIS installer and exits.
//!   Before it exits, the model servers are stopped so their sandbox grants
//!   are revoked. The plugin's own check and download are never called and
//!   its webview commands are not granted.
//!
//! The release file isn't signed; the installer is. `restore_update` turns
//! on the plugin's signed-version check, so the version in the release file
//! must match the version signed into the installer's trusted comment (an
//! older, genuine release can't be passed off as newer). What the release
//! file alone can do is claim a release that doesn't exist (its download
//! then fails the signature check) or hide one (a check shows no update).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use gardenwall_runtime::download::{DownloadError, Downloader};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

use crate::commands::{Progress, progress_reporter};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// The release file the check reads. GitHub serves the newest release's
/// asset under this fixed address (a redirect to its asset host).
pub const RELEASE_FILE_URL: &str =
    "https://github.com/spaghetticodeclanker/gardenwall/releases/latest/download/latest.json";
/// Shown (copied, never opened) for people who prefer to download the
/// installer themselves. Fixed here, never taken from the release file.
pub const RELEASES_PAGE: &str = "https://github.com/spaghetticodeclanker/gardenwall/releases";
const MAX_RELEASE_FILE: u64 = 256 * 1024;
/// The whole installer is held in memory (the plugin installs from bytes).
pub const MAX_INSTALLER: u64 = 512 * 1024 * 1024;
const MAX_NOTES_CHARS: usize = 6_000;

/// What the last check found and what has been downloaded.
#[derive(Default)]
pub struct AppUpdates {
    found: Mutex<Option<Found>>,
    /// A downloaded installer whose signature was checked.
    ready: Mutex<Option<Ready>>,
    check_error: Mutex<Option<String>>,
    downloading: AtomicBool,
}

#[derive(Clone)]
struct Found {
    update: Update,
    security: bool,
}

struct Ready {
    update: Update,
    bytes: Vec<u8>,
}

/// The Gardenwall row of the update report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppVersionView {
    pub current: String,
    /// A newer release, when the last check found one.
    pub available: Option<String>,
    pub note: String,
    /// The release's notes, as plain text (hidden characters collapsed).
    pub notes: Option<String>,
    /// The release date (YYYY-MM-DD), when the release file gives one.
    pub published: Option<String>,
    /// The release file marks it as a security release.
    pub security: bool,
    /// Copied for people who prefer to download it themselves.
    pub releases_page: String,
    /// Why this build can't install updates itself, if it can't.
    pub cannot_install: Option<String>,
    pub downloading: bool,
    /// The installer is downloaded and its signature checked.
    pub downloaded: bool,
    /// The last check failed (shown, never retried in a loop).
    pub check_error: Option<String>,
}

/// Why this build can't install an update itself, if it can't. Checking
/// still works, and the Releases page is always offered.
fn cannot_install(pubkey: &str) -> Option<String> {
    if !cfg!(windows) {
        Some("Gardenwall installs its own updates only on Windows so far.".into())
    } else if pubkey.trim().is_empty() {
        Some("This build has no release key, so it can't check an installer's signature.".into())
    } else if cfg!(debug_assertions) {
        Some("Installing works only in the installed app; this is a development build.".into())
    } else {
        None
    }
}

/// The updater public key from `tauri.conf.json` (`plugins.updater.pubkey`).
fn pubkey(app: &AppHandle) -> String {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Plain text for the page: hidden characters collapsed, length capped.
fn clean_notes(notes: &str) -> String {
    let (text, _) = gardenwall_policy::protect::hide_invisible(notes.trim());
    if text.chars().count() <= MAX_NOTES_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_NOTES_CHARS).collect();
    cut.push_str("\n[Notes shortened. The full notes are on the Releases page.]");
    cut
}

/// The `gardenwall.security` flag Gardenwall's release files carry.
fn security_flag(release: &serde_json::Value) -> bool {
    release
        .pointer("/gardenwall/security")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn download_error(e: DownloadError) -> String {
    match e {
        DownloadError::Status(404) => "No release has been published yet.".into(),
        DownloadError::DisallowedUrl => {
            "The release points somewhere other than GitHub's release hosts, so it was refused."
                .into()
        }
        DownloadError::TooLarge => "The file is larger than Gardenwall accepts.".into(),
        other => other.to_string(),
    }
}

/// The plugin's updater, used only for `restore_update`, `verify` and
/// `install`. `build()` needs an endpoint; it is only contacted by the
/// plugin's `check()`, which Gardenwall never calls.
fn updater<R: tauri::Runtime>(app: &AppHandle<R>) -> CmdResult<Updater> {
    let handle = app.clone();
    let endpoint = url::Url::parse(RELEASE_FILE_URL).map_err(|e| e.to_string())?;
    app.updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .on_before_exit(move || {
            // The installer is about to replace the app: stop the model
            // servers so their sandbox grants are revoked, then let Tauri
            // clean up.
            use tauri::Manager;
            if let Some(state) = handle.try_state::<AppState>() {
                tauri::async_runtime::block_on(crate::state::shutdown_servers(&state));
                // Closing for an update is a normal exit.
                crate::diagnostics::end_session(&state.paths.root);
            }
            handle.cleanup_before_exit();
        })
        .build()
        .map_err(|e| e.to_string())
}

/// A release file's bytes → the newer release it names, if any.
fn parse_release<R: tauri::Runtime>(app: &AppHandle<R>, bytes: &[u8]) -> CmdResult<Option<Found>> {
    let release: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "The release file isn't valid JSON.".to_string())?;
    let security = security_flag(&release);
    let update = updater(app)?
        .restore_update(release)
        .map_err(|e| format!("The release file couldn't be read ({e})."))?;
    Ok(update.map(|update| Found { update, security }))
}

/// Read the release file and remember a newer release. Offline mode skips
/// it. Errors are kept for the page, never retried here.
pub async fn refresh(state: &AppState) {
    let Some(app) = state.app.get().cloned() else {
        return;
    };
    if state.settings().offline {
        return;
    }
    let result = fetch_release(&app).await;
    let updates = &state.app_updates;
    match result {
        Ok(found) => {
            // A different (or no) release replaces a download of the old one.
            let same = match (&found, updates.found.lock().ok().and_then(|f| f.clone())) {
                (Some(new), Some(old)) => new.update.version == old.update.version,
                _ => false,
            };
            if !same && let Ok(mut r) = updates.ready.lock() {
                *r = None;
            }
            if let Ok(mut f) = updates.found.lock() {
                *f = found;
            }
            if let Ok(mut e) = updates.check_error.lock() {
                *e = None;
            }
        }
        Err(e) => {
            crate::diagnostics::record(state, "update check", &e);
            if let Ok(mut slot) = updates.check_error.lock() {
                *slot = Some(e);
            }
        }
    }
}

async fn fetch_release(app: &AppHandle) -> CmdResult<Option<Found>> {
    let downloader = Downloader::new().map_err(|e| e.to_string())?;
    let bytes = match downloader
        .fetch_capped(RELEASE_FILE_URL, MAX_RELEASE_FILE, |_, _| true)
        .await
    {
        Ok(b) => b,
        // Before the first release there is nothing to find.
        Err(DownloadError::Status(404)) => return Ok(None),
        Err(e) => {
            return Err(format!(
                "Couldn't read the release file: {}",
                download_error(e)
            ));
        }
    };
    parse_release(app, &bytes)
}

/// The Gardenwall row from what the last check found.
pub fn view(state: &AppState) -> AppVersionView {
    let key = state.app.get().map(pubkey).unwrap_or_default();
    let updates = &state.app_updates;
    let found = updates.found.lock().ok().and_then(|f| f.clone());
    let downloaded = updates.ready.lock().is_ok_and(|r| r.is_some());
    let check_error = updates.check_error.lock().ok().and_then(|e| e.clone());
    let offline = state.settings().offline;
    let note = match (&found, offline) {
        (Some(_), _) => String::new(),
        (None, true) => "Offline mode is on, so Gardenwall doesn't look for new versions.".into(),
        (None, false) => "This is the newest version Gardenwall found.".into(),
    };
    AppVersionView {
        current: env!("CARGO_PKG_VERSION").into(),
        available: found.as_ref().map(|f| f.update.version.clone()),
        note,
        notes: found
            .as_ref()
            .and_then(|f| f.update.body.as_deref())
            .map(clean_notes)
            .filter(|n| !n.is_empty()),
        published: found
            .as_ref()
            .and_then(|f| f.update.date)
            .map(|d| d.date().to_string()),
        security: found.as_ref().is_some_and(|f| f.security),
        releases_page: RELEASES_PAGE.into(),
        cannot_install: cannot_install(&key),
        downloading: updates.downloading.load(Ordering::Acquire),
        downloaded,
        check_error,
    }
}

/// Clears `downloading` however the download ends.
struct Downloading<'a>(&'a AtomicBool);

impl Drop for Downloading<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Download the installer the last check found and check its signature.
/// Nothing is installed.
#[tauri::command]
pub async fn download_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
    on_progress: Channel<Progress>,
) -> CmdResult<crate::updates::UpdatesView> {
    if let Some(why) = cannot_install(&pubkey(&app)) {
        return Err(why);
    }
    if state.settings().offline {
        return Err("Offline mode is on.".into());
    }
    let found = state
        .app_updates
        .found
        .lock()
        .ok()
        .and_then(|f| f.clone())
        .ok_or("No new version was found. Check for updates first.")?;
    if state.app_updates.downloading.swap(true, Ordering::AcqRel) {
        return Err("The update is already downloading.".into());
    }
    let _guard = Downloading(&state.app_updates.downloading);
    let downloader = Downloader::new().map_err(|e| e.to_string())?;
    let bytes = downloader
        .fetch_capped(
            found.update.download_url.as_str(),
            MAX_INSTALLER,
            progress_reporter(on_progress),
        )
        .await
        .map_err(|e| format!("The download failed: {}", download_error(e)))?;
    if found.update.verify(&bytes).is_err() {
        return Err(
            "The download doesn't carry Gardenwall's release signature for this version, \
             so it was deleted. Nothing was installed."
                .into(),
        );
    }
    if let Ok(mut r) = state.app_updates.ready.lock() {
        *r = Some(Ready {
            update: found.update,
            bytes,
        });
    }
    Ok(crate::updates::view(&state))
}

/// Install the downloaded update: the plugin checks the signature again,
/// stops the model servers, starts the installer and exits Gardenwall. It
/// returns only when something went wrong.
#[tauri::command]
pub async fn install_app_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(why) = cannot_install(&pubkey(&app)) {
        return Err(why);
    }
    let ready = state
        .app_updates
        .ready
        .lock()
        .map_err(|_| "update state unavailable")?
        .take()
        .ok_or("Download the update first.")?;
    tauri::async_runtime::spawn_blocking(move || ready.update.install(ready.bytes))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("The update couldn't be installed ({e}). Download it again to retry."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_plain_and_capped() {
        assert_eq!(clean_notes("  Fixes.\n"), "Fixes.");
        let hidden = clean_notes("Fixes\u{200B}\u{200B} ok");
        assert!(!hidden.contains('\u{200B}'));
        let long = clean_notes(&"a".repeat(MAX_NOTES_CHARS + 50));
        assert!(long.ends_with("on the Releases page.]"));
        assert!(long.chars().count() < MAX_NOTES_CHARS + 100);
    }

    #[test]
    fn the_security_flag_is_read_only_from_its_own_field() {
        let yes = serde_json::json!({"version": "1.0.1", "gardenwall": {"security": true}});
        let no = serde_json::json!({"version": "1.0.1", "notes": "security: true"});
        let wrong_type = serde_json::json!({"gardenwall": {"security": "true"}});
        assert!(security_flag(&yes));
        assert!(!security_flag(&no));
        assert!(!security_flag(&wrong_type));
    }

    #[test]
    fn a_build_without_a_release_key_never_installs() {
        assert!(cannot_install("").is_some());
        assert!(cannot_install("   ").is_some());
    }

    /// The plugin refuses to start (and with it the app) if its block in
    /// `tauri.conf.json` doesn't parse.
    #[test]
    fn the_updater_config_parses_and_asks_for_no_network_of_its_own() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let block = conf.pointer("/plugins/updater").unwrap().clone();
        assert!(
            block.get("endpoints").is_none(),
            "the plugin's own check must stay unused"
        );
        let parsed: tauri_plugin_updater::Config = serde_json::from_value(block).unwrap();
        assert!(!parsed.dangerous_insecure_transport_protocol);
        assert!(!parsed.dangerous_accept_invalid_certs);
        assert!(!parsed.allow_downgrades);
    }

    const PUBKEY: &str = include_str!("../tests/fixtures/updater/test-key.pub");
    const INSTALLER: &[u8] = include_bytes!("../tests/fixtures/updater/installer.bin");
    const SIGNATURE: &str = include_str!("../tests/fixtures/updater/installer.bin.sig");

    /// A mock app with the updater plugin trusting the test key.
    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.package_info_mut().version = "1.0.0".parse().unwrap();
        context.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({ "pubkey": PUBKEY.trim() }),
        );
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap()
    }

    fn release_file(version: &str, signature: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": version,
            "notes": "Fixes.",
            "pub_date": "2026-11-22T00:00:00Z",
            "platforms": {
                "windows-x86_64": { "signature": signature, "url": "https://github.com/spaghetticodeclanker/gardenwall/releases/download/v1/setup.exe" },
                "linux-x86_64": { "signature": signature, "url": "https://github.com/spaghetticodeclanker/gardenwall/releases/download/v1/app.AppImage" },
                "darwin-aarch64": { "signature": signature, "url": "https://github.com/spaghetticodeclanker/gardenwall/releases/download/v1/app.tar.gz" }
            },
            "gardenwall": { "security": true }
        }))
        .unwrap()
    }

    #[test]
    fn a_signed_installer_verifies_and_a_changed_one_does_not() {
        let app = mock_app();
        let found = parse_release(app.handle(), &release_file("1.0.1", SIGNATURE.trim()))
            .unwrap()
            .expect("1.0.1 is newer than 1.0.0");
        assert_eq!(found.update.version, "1.0.1");
        assert!(found.security);
        found.update.verify(INSTALLER).unwrap();

        let mut changed = INSTALLER.to_vec();
        changed[0] ^= 1;
        assert!(found.update.verify(&changed).is_err());
    }

    #[test]
    fn an_older_release_cannot_pose_as_a_newer_version() {
        // The installer was signed for 1.0.1; a release file that calls it
        // 9.9.9 must not verify (the plugin's signed-version check).
        let app = mock_app();
        let found = parse_release(app.handle(), &release_file("9.9.9", SIGNATURE.trim()))
            .unwrap()
            .unwrap();
        assert!(found.update.verify(INSTALLER).is_err());
    }

    #[test]
    fn the_same_or_an_older_version_is_not_an_update() {
        let app = mock_app();
        assert!(
            parse_release(app.handle(), &release_file("1.0.0", SIGNATURE.trim()))
                .unwrap()
                .is_none()
        );
        assert!(
            parse_release(app.handle(), &release_file("0.9.0", SIGNATURE.trim()))
                .unwrap()
                .is_none()
        );
        assert!(parse_release(app.handle(), b"not json").is_err());
    }

    #[test]
    fn the_release_addresses_are_on_the_downloaders_hosts() {
        for u in [RELEASE_FILE_URL, RELEASES_PAGE] {
            let url = url::Url::parse(u).unwrap();
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.host_str(), Some("github.com"));
        }
    }
}
