use std::fs::OpenOptions;
use std::process::Command;

#[test]
fn separate_process_respects_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    let lock_path = directory.path().join("gardenwall.db.lock");

    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();

    lock_file.try_lock().unwrap();

    // Cargo supplies the correct executable path.
    let probe = env!("CARGO_BIN_EXE_lock_probe");

    // Another process must be denied while the lock is held.
    let blocked = Command::new(probe).arg(&lock_path).output().unwrap();

    assert_eq!(
        blocked.status.code(),
        Some(2),
        "Expected lock contention. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&blocked.stdout),
        String::from_utf8_lossy(&blocked.stderr)
    );

    assert_eq!(String::from_utf8_lossy(&blocked.stdout).trim(), "LOCK_BUSY");

    // Release the lock.
    lock_file.unlock().unwrap();
    drop(lock_file);

    // The independent process must now acquire it.
    let available = Command::new(probe).arg(&lock_path).output().unwrap();

    assert!(
        available.status.success(),
        "Expected successful acquisition. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&available.stdout),
        String::from_utf8_lossy(&available.stderr)
    );

    assert_eq!(
        String::from_utf8_lossy(&available.stdout).trim(),
        "LOCK_ACQUIRED"
    );
}
