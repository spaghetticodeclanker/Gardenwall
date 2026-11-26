use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeId(Uuid);

impl RuntimeId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Debug)]
pub struct RuntimeOwnership {
    runtime_id: RuntimeId,
    lock_file: File,
}

pub fn canonical_database_path(database_path: &Path) -> io::Result<PathBuf> {
    let file_name = database_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "database path must include a filename",
        )
    })?;

    let parent = database_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "database path must include a parent directory",
        )
    })?;

    let canonical_parent = fs::canonicalize(parent)?;
    let canonical_path = canonical_parent.join(file_name);

    match fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "database file must not be a symbolic link",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(canonical_path)
}

fn database_lock_path(database_path: &Path) -> io::Result<PathBuf> {
    let canonical_path = canonical_database_path(database_path)?;

    let mut lock_name = OsString::from(canonical_path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "database filename is missing")
    })?);

    lock_name.push(".lock");

    Ok(canonical_path.with_file_name(lock_name))
}

impl RuntimeOwnership {
    pub fn acquire(database_path: &Path) -> io::Result<Self> {
        let lock_path = database_lock_path(database_path)?;

        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        lock_file.try_lock().map_err(std::io::Error::from)?;

        Ok(Self {
            runtime_id: RuntimeId::generate(),
            lock_file,
        })
    }

    pub fn runtime_id(&self) -> RuntimeId {
        self.runtime_id
    }
}

impl Drop for RuntimeOwnership {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_ids_are_distinct() {
        let first = RuntimeId::generate();
        let second = RuntimeId::generate();

        assert_ne!(first, second);
    }

    #[test]
    fn runtime_id_has_sixteen_bytes() {
        let runtime_id = RuntimeId::generate();

        assert_eq!(runtime_id.as_bytes().len(), 16);
    }

    #[test]
    fn ownership_prevents_second_acquisition() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        let first = RuntimeOwnership::acquire(&database_path).unwrap();

        let second = RuntimeOwnership::acquire(&database_path);

        assert!(second.is_err());

        drop(first);
    }

    #[test]
    fn ownership_can_be_reacquired_after_release() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("gardenwall.db");

        let first = RuntimeOwnership::acquire(&database_path).unwrap();
        let first_id = first.runtime_id();

        drop(first);

        let second = RuntimeOwnership::acquire(&database_path).unwrap();

        assert_ne!(first_id, second.runtime_id());
    }

    #[test]
    fn alternate_parent_path_cannot_bypass_lock() {
        let directory = tempfile::tempdir().unwrap();

        let canonical_path = directory.path().join("gardenwall.db");
        let alternate_path = directory
            .path()
            .join("subdirectory")
            .join("..")
            .join("gardenwall.db");

        std::fs::create_dir(directory.path().join("subdirectory")).unwrap();

        let first = RuntimeOwnership::acquire(&canonical_path).unwrap();

        let second = RuntimeOwnership::acquire(&alternate_path);

        assert!(second.is_err());

        drop(first);
    }

    #[test]
    fn lock_filename_preserves_database_extension() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("production.sqlite");

        let lock_path = database_lock_path(&database_path).unwrap();

        assert_eq!(lock_path.file_name().unwrap(), "production.sqlite.lock");
    }

    #[test]
    fn canonical_path_requires_existing_parent() {
        let directory = tempfile::tempdir().unwrap();

        let database_path = directory
            .path()
            .join("missing-directory")
            .join("gardenwall.db");

        assert!(canonical_database_path(&database_path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();

        let real_path = directory.path().join("real.db");
        let link_path = directory.path().join("linked.db");

        std::fs::write(&real_path, b"").unwrap();
        symlink(&real_path, &link_path).unwrap();

        assert!(canonical_database_path(&link_path).is_err());
    }
}
