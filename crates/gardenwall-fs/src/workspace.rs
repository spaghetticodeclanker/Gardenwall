//! A granted workspace folder, reached only through a capability handle.
//!
//! Every operation names a validated [`RelPath`] and is performed relative to
//! a `cap_std::fs::Dir` opened on the folder, so absolute paths, `..`,
//! symlinks and junctions cannot resolve outside it. `RelPath` validation
//! additionally rejects drive letters, NTFS alternate data streams and device
//! names before anything reaches the filesystem.
//!
//! This is the policy-layer half of the boundary. Processes the agent starts
//! are confined to the same folders by the OS sandbox (ADR-012).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use gardenwall_policy::paths::RelPath;

use crate::FsError;

/// Most entries returned by one listing.
pub const MAX_LIST_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symlink or junction. Never followed by listings.
    Link,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    dir: Dir,
}

impl Workspace {
    /// Open a folder the user granted. Refuses roots so broad that granting
    /// them would defeat the lockdown (a drive root, the home folder itself,
    /// system folders).
    pub fn open(root: &Path) -> Result<Self, FsError> {
        let canonical = std::fs::canonicalize(root).map_err(|e| FsError::io(root, e))?;
        if !canonical.is_dir() {
            return Err(FsError::NotADirectory(canonical));
        }
        if too_broad(&canonical) {
            return Err(FsError::GrantTooBroad(canonical));
        }
        if is_secret_store(&canonical) {
            return Err(FsError::SecretStore(canonical));
        }
        let dir = Dir::open_ambient_dir(&canonical, ambient_authority())
            .map_err(|e| FsError::io(&canonical, e))?;
        Ok(Self {
            root: canonical,
            dir,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn rel(path: &RelPath) -> PathBuf {
        path.components().iter().collect()
    }

    /// What `path` is, `None` if it does not exist. A path that exists but
    /// cannot be reached safely (for example through a link that leads
    /// outside the workspace) is an error, never "missing".
    pub fn kind(&self, path: &RelPath) -> Result<Option<EntryKind>, FsError> {
        let rel = Self::rel(path);
        match self.dir.symlink_metadata(&rel) {
            Ok(meta) => Ok(Some(kind_of(&meta))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(FsError::io(&rel, e)),
        }
    }

    /// Read a whole file, refusing files larger than `max_bytes`.
    pub fn read(&self, path: &RelPath, max_bytes: u64) -> Result<Vec<u8>, FsError> {
        let rel = Self::rel(path);
        let file = self.dir.open(&rel).map_err(|e| FsError::io(&rel, e))?;
        let meta = file.metadata().map_err(|e| FsError::io(&rel, e))?;
        if !meta.is_file() {
            return Err(FsError::NotAFile(path.as_display()));
        }
        if meta.len() > max_bytes {
            return Err(FsError::TooLarge {
                path: path.as_display(),
                size: meta.len(),
                limit: max_bytes,
            });
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| FsError::io(&rel, e))?;
        if bytes.len() as u64 > max_bytes {
            return Err(FsError::TooLarge {
                path: path.as_display(),
                size: bytes.len() as u64,
                limit: max_bytes,
            });
        }
        Ok(bytes)
    }

    /// Read a file if it exists (used to snapshot before a change).
    pub fn read_optional(
        &self,
        path: &RelPath,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, FsError> {
        match self.kind(path)? {
            None => Ok(None),
            Some(EntryKind::File) => self.read(path, max_bytes).map(Some),
            Some(_) => Err(FsError::NotAFile(path.as_display())),
        }
    }

    /// List a directory (`None` for the workspace root). Links are reported,
    /// never followed.
    pub fn list(&self, path: Option<&RelPath>) -> Result<Vec<DirEntry>, FsError> {
        let entries = match path {
            None => self.dir.entries(),
            Some(p) => self.dir.read_dir(Self::rel(p)),
        }
        .map_err(|e| {
            FsError::io(
                Path::new(&path.map(RelPath::as_display).unwrap_or_default()),
                e,
            )
        })?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| FsError::io(&self.root, e))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            let kind = match entry.file_type() {
                Ok(t) if t.is_symlink() => EntryKind::Link,
                _ => kind_of(&meta),
            };
            out.push(DirEntry {
                name,
                kind,
                size: if kind == EntryKind::File {
                    meta.len()
                } else {
                    0
                },
            });
            if out.len() >= MAX_LIST_ENTRIES {
                break;
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Replace a file's contents atomically (write a temporary file in the
    /// same folder, then rename over the target). Parent folders are
    /// created as needed. Callers checkpoint first.
    pub fn write(&self, path: &RelPath, contents: &[u8]) -> Result<(), FsError> {
        let rel = Self::rel(path);
        if matches!(
            self.kind(path)?,
            Some(EntryKind::Dir | EntryKind::Link | EntryKind::Other)
        ) {
            return Err(FsError::NotAFile(path.as_display()));
        }
        let parent = rel.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(parent) = parent {
            self.dir
                .create_dir_all(parent)
                .map_err(|e| FsError::io(parent, e))?;
        }
        // A new, uniquely named temporary file, created only if nothing has
        // that name: never an existing file or a link someone put there
        // (review GW-006).
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut opened = None;
        for attempt in 0..8u32 {
            let tmp_name = format!(".{}.{}.gardenwall-tmp", path.file_name(), unique(attempt));
            let tmp = match parent {
                Some(p) => p.join(&tmp_name),
                None => PathBuf::from(&tmp_name),
            };
            match self.dir.open_with(&tmp, &options) {
                Ok(file) => {
                    opened = Some((tmp, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(FsError::io(&rel, e)),
            }
        }
        let Some((tmp, file)) = opened else {
            return Err(FsError::io(
                &rel,
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "no free name for a temporary file",
                ),
            ));
        };
        let result = (|| {
            let mut file = file;
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            self.dir.rename(&tmp, &self.dir, &rel)
        })();
        if let Err(e) = result {
            let _ = self.dir.remove_file(&tmp);
            return Err(FsError::io(&rel, e));
        }
        Ok(())
    }

    /// Delete a file. Folders are never deleted recursively by the core.
    pub fn delete(&self, path: &RelPath) -> Result<(), FsError> {
        let rel = Self::rel(path);
        match self.kind(path)? {
            Some(EntryKind::File | EntryKind::Link) => {
                self.dir.remove_file(&rel).map_err(|e| FsError::io(&rel, e))
            }
            Some(EntryKind::Dir) => self.dir.remove_dir(&rel).map_err(|e| FsError::io(&rel, e)),
            Some(EntryKind::Other) => Err(FsError::NotAFile(path.as_display())),
            None => Ok(()),
        }
    }
}

/// A hard-to-guess suffix for a temporary file name (the standard library's
/// randomly seeded hasher over the time, a counter and the attempt).
fn unique(attempt: u32) -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(attempt);
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

fn kind_of(meta: &cap_std::fs::Metadata) -> EntryKind {
    if meta.file_type().is_symlink() {
        EntryKind::Link
    } else if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Where secret files moved out of projects are kept: `Gardenwall Secrets`
/// in the user's home folder, outside every folder that can be granted
/// (the home folder itself and everything above it are refused as too
/// broad). `None` when the home folder is unknown.
pub fn secrets_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(SECRETS_HOME))
}

const SECRETS_HOME: &str = "Gardenwall Secrets";

/// Folder names that hold keys or credentials. A folder with one of these
/// names, or anywhere below one, is never granted.
const SECRET_STORES: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".docker",
    ".password-store",
];

/// Whether `path` (canonical) is a folder of secrets: the secrets folder
/// or anything in it, or a key or credential folder or anything in one.
pub fn is_secret_store(path: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    if let Some(home) = secrets_home() {
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        if Path::new(&lower(path)).starts_with(lower(&home)) {
            return true;
        }
    }
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        name == SECRETS_HOME.to_lowercase() || SECRET_STORES.contains(&name.as_str())
    })
}

/// Folders that must never be granted whole.
fn too_broad(path: &Path) -> bool {
    // A drive or filesystem root.
    if path.parent().is_none() {
        return true;
    }
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let target = lower(path);
    let same = |var: &str| {
        std::env::var_os(var)
            .and_then(|v| std::fs::canonicalize(v).ok())
            .is_some_and(|v| lower(&v) == target)
    };
    if [
        "USERPROFILE",
        "HOME",
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "APPDATA",
        "LOCALAPPDATA",
    ]
    .iter()
    .any(|v| same(v))
    {
        return true;
    }
    // The parent of the home folder (e.g. C:\Users or /home).
    if std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .and_then(|h| std::fs::canonicalize(h).ok())
        .and_then(|h| h.parent().map(lower))
        .is_some_and(|p| p == target)
    {
        return true;
    }
    #[cfg(unix)]
    {
        const SYSTEM: &[&str] = &[
            "/bin",
            "/boot",
            "/dev",
            "/etc",
            "/lib",
            "/proc",
            "/sbin",
            "/sys",
            "/usr",
            "/var",
            "/System",
            "/Library",
            "/Applications",
        ];
        if SYSTEM.iter().any(|s| Path::new(s) == path) {
            return true;
        }
    }
    false
}
