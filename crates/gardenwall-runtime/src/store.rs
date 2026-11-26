//! Installed runtimes and their manifests.
//!
//! Layout under the store root (a Gardenwall-owned data directory):
//! - `runtimes/<build-id>/` — the extracted runtime (granted read/execute to
//!   the inference sandbox, never writable by it);
//! - `manifests/<build-id>.json` — SHA-256 of every runtime file, kept
//!   outside the runtime directory;
//! - `downloads/` — transient, verified archives.
//!
//! [`RuntimeStore::verify`] runs before every launch. The runtime directory
//! must match its manifest **exactly** — same file set, same hashes, no
//! links or reparse points — so a modified binary or a planted extra DLL
//! (search-order hijack) prevents launch.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::download::{DownloadError, Downloader};
use crate::extract::{self, ExtractError, ExtractLimits, Totals};
use crate::pins::{ArchiveKind, RuntimeBuild};

const MAX_MANIFEST_BYTES: u64 = 4 << 20;
const MAX_RUNTIME_FILES: usize = 20_000;

#[derive(Debug)]
pub enum StoreError {
    Download(DownloadError),
    Extract(ExtractError),
    Io(std::io::Error),
    /// The archives did not contain exactly one server binary.
    ServerBinaryNotFound,
    NotInstalled,
    /// The runtime no longer matches its manifest; it must not be launched.
    Tampered(String),
    InvalidManifest,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Download(e) => write!(f, "{e}"),
            Self::Extract(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "runtime store I/O error: {e}"),
            Self::ServerBinaryNotFound => {
                f.write_str("the runtime archive doesn't hold exactly one server program")
            }
            Self::NotInstalled => f.write_str("runtime is not installed"),
            Self::Tampered(what) => write!(f, "runtime failed verification: {what}"),
            Self::InvalidManifest => f.write_str("runtime manifest is invalid"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    build: String,
    /// Relative path (forward slashes) -> lowercase hex SHA-256.
    files: BTreeMap<String, String>,
}

/// A verified runtime ready to launch.
#[derive(Debug, Clone)]
pub struct InstalledRuntime {
    pub build: &'static RuntimeBuild,
    pub dir: PathBuf,
    pub server_exe: PathBuf,
}

pub struct RuntimeStore {
    root: PathBuf,
}

/// Build ids are lowercase letters, digits and dashes (e.g.
/// `b12283-windows-x64-cuda13`), so they are safe as one path component.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// All regular files under `dir`, as forward-slash relative paths.
/// Links, reparse points, and special files are errors.
fn list_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, StoreError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if is_link_or_reparse(&meta) {
                return Err(StoreError::Tampered(format!(
                    "link found: {}",
                    path.display()
                )));
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(dir)
                    .map_err(|_| StoreError::InvalidManifest)?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, path));
                if out.len() > MAX_RUNTIME_FILES {
                    return Err(StoreError::Tampered("too many files".into()));
                }
            } else {
                return Err(StoreError::Tampered(format!(
                    "special file: {}",
                    path.display()
                )));
            }
        }
    }
    out.sort();
    Ok(out)
}

impl RuntimeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn runtime_dir(&self, build: &RuntimeBuild) -> PathBuf {
        self.root.join("runtimes").join(build.id)
    }

    fn manifest_path(&self, build: &RuntimeBuild) -> PathBuf {
        self.root
            .join("manifests")
            .join(format!("{}.json", build.id))
    }

    pub fn is_installed(&self, build: &RuntimeBuild) -> bool {
        self.manifest_path(build).is_file() && self.runtime_dir(build).is_dir()
    }

    /// Ids of every installed runtime, including builds this version of
    /// Gardenwall no longer pins (left by an earlier version). Only names
    /// that could be build ids are listed; staging folders are not.
    pub fn installed_ids(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.root.join("runtimes")) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| valid_id(name))
            .collect();
        ids.sort();
        ids
    }

    /// Delete an installed runtime. The manifest goes first, so a runtime
    /// interrupted half-way through removal fails verification and is
    /// never launched. The caller makes sure it is not running.
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        if !valid_id(id) {
            return Err(StoreError::NotInstalled);
        }
        let manifest = self.root.join("manifests").join(format!("{id}.json"));
        let dir = self.root.join("runtimes").join(id);
        if !manifest.exists() && !dir.exists() {
            return Err(StoreError::NotInstalled);
        }
        match fs::remove_file(&manifest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Never follow a link or junction out of the store.
        if let Ok(meta) = fs::symlink_metadata(&dir) {
            if is_link_or_reparse(&meta) {
                fs::remove_dir(&dir).or_else(|_| fs::remove_file(&dir))?;
            } else {
                fs::remove_dir_all(&dir)?;
            }
        }
        Ok(())
    }

    /// Download, verify, extract, and record `build`.
    /// `progress(done, total)` covers all archives; returning `false` cancels.
    pub async fn install(
        &self,
        build: &'static RuntimeBuild,
        downloader: &Downloader,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<InstalledRuntime, StoreError> {
        let downloads = self.root.join("downloads");
        let runtimes = self.root.join("runtimes");
        let manifests = self.root.join("manifests");
        for d in [&downloads, &runtimes, &manifests] {
            fs::create_dir_all(d)?;
        }

        let total = build.download_size();
        let mut base = 0u64;
        let mut archives = Vec::new();
        for archive in build.archives {
            let dest = downloads.join(format!("{}.part", archive.sha256));
            downloader
                .fetch(
                    archive.url,
                    archive.size,
                    archive.sha256,
                    &dest,
                    |done, _| progress(base + done, total),
                )
                .await
                .map_err(StoreError::Download)?;
            base += archive.size;
            archives.push((dest, archive.kind));
        }

        let build_ref: &'static RuntimeBuild = build;
        let root = self.root.clone();
        let result = tokio::task::spawn_blocking(move || {
            let store = RuntimeStore::new(root);
            store.finish_install(build_ref, &archives)
        })
        .await
        .map_err(|_| StoreError::Io(std::io::Error::other("install task failed")))?;
        for archive in build.archives {
            let _ = fs::remove_file(downloads.join(format!("{}.part", archive.sha256)));
        }
        result
    }

    fn finish_install(
        &self,
        build: &'static RuntimeBuild,
        archives: &[(PathBuf, ArchiveKind)],
    ) -> Result<InstalledRuntime, StoreError> {
        let runtimes = self.root.join("runtimes");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let staging = runtimes.join(format!(".staging-{}-{nanos:x}", build.id));
        fs::create_dir(&staging)?;
        let result = (|| {
            let limits = ExtractLimits::default();
            let mut totals = Totals::default();
            for (path, kind) in archives {
                match kind {
                    ArchiveKind::Zip => extract::extract_zip(path, &staging, &limits, &mut totals),
                    ArchiveKind::TarGz => {
                        extract::extract_tar_gz(path, &staging, &limits, &mut totals)
                    }
                }
                .map_err(StoreError::Extract)?;
            }

            // The runtime root is the directory holding the server binary.
            let server_name = build.server_binary_name();
            let candidates: Vec<PathBuf> = list_files(&staging)?
                .into_iter()
                .filter(|(_, p)| p.file_name().is_some_and(|n| n == server_name))
                .map(|(_, p)| p)
                .collect();
            let [server] = candidates.as_slice() else {
                return Err(StoreError::ServerBinaryNotFound);
            };
            let runtime_root = server.parent().ok_or(StoreError::ServerBinaryNotFound)?;

            let final_dir = self.runtime_dir(build);
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir)?;
            }
            let manifest_path = self.manifest_path(build);
            let _ = fs::remove_file(&manifest_path);
            fs::rename(runtime_root, &final_dir)?;

            let mut files = BTreeMap::new();
            for (rel, path) in list_files(&final_dir)? {
                files.insert(rel, sha256_file(&path)?);
            }
            let manifest = Manifest {
                build: build.id.to_string(),
                files,
            };
            let tmp = manifest_path.with_extension("json.tmp");
            fs::write(
                &tmp,
                serde_json::to_vec_pretty(&manifest).map_err(|_| StoreError::InvalidManifest)?,
            )?;
            fs::rename(&tmp, &manifest_path)?;
            Ok(())
        })();
        let _ = fs::remove_dir_all(&staging);
        result?;
        self.verify(build)
    }

    /// Verify an installed runtime against its manifest (call before every
    /// launch). Any difference prevents launch.
    pub fn verify(&self, build: &'static RuntimeBuild) -> Result<InstalledRuntime, StoreError> {
        let dir = self.runtime_dir(build);
        let manifest_path = self.manifest_path(build);
        if !dir.is_dir() || !manifest_path.is_file() {
            return Err(StoreError::NotInstalled);
        }
        if fs::metadata(&manifest_path)?.len() > MAX_MANIFEST_BYTES {
            return Err(StoreError::InvalidManifest);
        }
        let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)
            .map_err(|_| StoreError::InvalidManifest)?;
        if manifest.build != build.id || manifest.files.is_empty() {
            return Err(StoreError::InvalidManifest);
        }
        let dir_meta = fs::symlink_metadata(&dir)?;
        if is_link_or_reparse(&dir_meta) {
            return Err(StoreError::Tampered("runtime directory is a link".into()));
        }

        let actual = list_files(&dir)?;
        if actual.len() != manifest.files.len() {
            return Err(StoreError::Tampered(format!(
                "expected {} files, found {}",
                manifest.files.len(),
                actual.len()
            )));
        }
        for (rel, path) in &actual {
            let Some(expected) = manifest.files.get(rel) else {
                return Err(StoreError::Tampered(format!("unexpected file {rel}")));
            };
            if &sha256_file(path)? != expected {
                return Err(StoreError::Tampered(format!("modified file {rel}")));
            }
        }
        let server_exe = dir.join(build.server_binary_name());
        if !manifest.files.contains_key(build.server_binary_name()) {
            return Err(StoreError::ServerBinaryNotFound);
        }
        Ok(InstalledRuntime {
            build,
            dir,
            server_exe,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pins::{Arch, Archive, Backend, Os};
    use axum::routing::get;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn hex(data: &[u8]) -> String {
        let d: [u8; 32] = Sha256::digest(data).into();
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// A fake pinned build served from loopback.
    async fn fake_build(entries: &[(&str, &[u8])]) -> &'static RuntimeBuild {
        fake_build_with(entries, None).await
    }

    async fn fake_build_with(
        entries: &[(&str, &[u8])],
        server: Option<&'static str>,
    ) -> &'static RuntimeBuild {
        let body = zip_bytes(entries);
        let (size, sha) = (body.len() as u64, hex(&body));
        let app = axum::Router::new().route("/r.zip", get(move || async move { body.clone() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let archives: &'static [Archive] = Box::leak(Box::new([Archive {
            url: leak(format!("http://{addr}/r.zip")),
            size,
            sha256: leak(sha),
            kind: ArchiveKind::Zip,
        }]));
        Box::leak(Box::new(RuntimeBuild {
            id: "test-windows-x64-cpu",
            version: "test",
            os: Os::Windows,
            arch: Arch::X64,
            backend: Backend::Cpu,
            server,
            archives,
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn installs_verifies_and_detects_tampering() {
        let build = fake_build(&[
            ("llama-b1-bin/llama-server.exe", b"server"),
            ("llama-b1-bin/ggml.dll", b"ggml"),
            ("llama-b1-bin/sub/extra.dll", b"extra"),
        ])
        .await;
        let root = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(root.path());
        let installed = store
            .install(build, &Downloader::for_tests(), |_, _| true)
            .await
            .unwrap();
        assert_eq!(
            installed.server_exe,
            root.path()
                .join("runtimes/test-windows-x64-cpu/llama-server.exe")
        );
        assert!(store.is_installed(build));
        assert!(store.verify(build).is_ok());
        assert_eq!(
            fs::read_dir(root.path().join("runtimes")).unwrap().count(),
            1,
            "staging removed"
        );
        assert_eq!(
            fs::read_dir(root.path().join("downloads")).unwrap().count(),
            0,
            "downloads removed"
        );

        // A planted extra DLL (search-order hijack) blocks launch.
        let planted = installed.dir.join("version.dll");
        fs::write(&planted, b"evil").unwrap();
        assert!(matches!(store.verify(build), Err(StoreError::Tampered(_))));
        fs::remove_file(&planted).unwrap();
        assert!(store.verify(build).is_ok());

        // A modified binary blocks launch.
        fs::write(installed.dir.join("ggml.dll"), b"patched").unwrap();
        assert!(matches!(store.verify(build), Err(StoreError::Tampered(_))));
        fs::write(installed.dir.join("ggml.dll"), b"ggml").unwrap();

        // A missing file blocks launch.
        fs::remove_file(installed.dir.join("sub/extra.dll")).unwrap();
        assert!(matches!(store.verify(build), Err(StoreError::Tampered(_))));

        // A corrupt manifest blocks launch.
        fs::write(
            root.path().join("manifests/test-windows-x64-cpu.json"),
            b"{",
        )
        .unwrap();
        assert!(matches!(
            store.verify(build),
            Err(StoreError::InvalidManifest)
        ));
    }

    /// whisper.cpp's archives keep everything under `Release/`, with its own
    /// server name (voice input, release plan feature 7).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_named_server_in_a_subfolder_installs() {
        let build = fake_build_with(
            &[
                ("Release/whisper-server.exe", b"server"),
                ("Release/whisper.dll", b"w"),
                ("Release/llama-server.exe", b"not this one"),
            ],
            Some("whisper-server.exe"),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(root.path());
        let installed = store
            .install(build, &Downloader::for_tests(), |_, _| true)
            .await
            .unwrap();
        assert_eq!(
            installed.server_exe,
            root.path()
                .join("runtimes/test-windows-x64-cpu/whisper-server.exe")
        );
        assert!(store.verify(build).is_ok());
    }

    /// Network, opt-in: `cargo test -p gardenwall-runtime pinned_whisper --
    /// --ignored` downloads and installs the real pinned CPU build (9 MB).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads from GitHub"]
    async fn the_pinned_whisper_cpu_build_installs() {
        let build = crate::pins::WHISPER_BUILDS
            .iter()
            .find(|b| b.backend == Backend::Cpu)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(root.path());
        let installed = store
            .install(build, &Downloader::new().unwrap(), |_, _| true)
            .await
            .unwrap();
        assert!(installed.server_exe.ends_with("whisper-server.exe"));
        assert!(installed.server_exe.is_file());
        assert!(store.verify(build).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archives_without_a_unique_server_are_refused() {
        let build = fake_build(&[("a/ggml.dll", b"x")]).await;
        let root = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(root.path());
        let r = store
            .install(build, &Downloader::for_tests(), |_, _| true)
            .await;
        assert!(matches!(r, Err(StoreError::ServerBinaryNotFound)));
        assert!(!store.is_installed(build));

        let build = fake_build(&[("a/llama-server.exe", b"1"), ("b/llama-server.exe", b"2")]).await;
        let root = tempfile::tempdir().unwrap();
        let r = RuntimeStore::new(root.path())
            .install(build, &Downloader::for_tests(), |_, _| true)
            .await;
        assert!(matches!(r, Err(StoreError::ServerBinaryNotFound)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runtimes_are_listed_and_removed_manifest_first() {
        let build = fake_build(&[("llama-server.exe", b"server")]).await;
        let root = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(root.path());
        store
            .install(build, &Downloader::for_tests(), |_, _| true)
            .await
            .unwrap();
        fs::create_dir_all(root.path().join("runtimes/.staging-x-1")).unwrap();
        fs::create_dir_all(root.path().join("runtimes/Not An Id")).unwrap();
        assert_eq!(store.installed_ids(), vec![build.id.to_string()]);
        store.remove(build.id).unwrap();
        assert!(!store.is_installed(build));
        assert!(store.installed_ids().is_empty());
        assert!(matches!(
            store.remove(build.id),
            Err(StoreError::NotInstalled)
        ));
        // Ids that could escape the store are refused outright.
        for bad in ["..", "../manifests", "a/b", "C:", ""] {
            assert!(
                matches!(store.remove(bad), Err(StoreError::NotInstalled)),
                "{bad}"
            );
        }
    }

    #[test]
    fn verify_reports_not_installed() {
        let root = tempfile::tempdir().unwrap();
        let build = crate::pins::find("b12283-windows-x64-vulkan").unwrap();
        assert!(matches!(
            RuntimeStore::new(root.path()).verify(build),
            Err(StoreError::NotInstalled)
        ));
    }
}
