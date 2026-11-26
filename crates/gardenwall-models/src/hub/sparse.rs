//! Sparse preallocation for parallel downloads.
//!
//! A download grows its file to the full size, then several connections
//! write chunks at different offsets. On NTFS, writing past a file's valid
//! data length makes Windows zero-fill everything before that offset first.
//! Measured on the reference machine, a write 8 GB into a new file took
//! 1.6 s instead of 1 ms. Over a whole download that roughly doubles the
//! disk writes, since each region is zeroed and then written again.
//! Marking the file sparse first avoids the zero-fill.
//!
//! The only `unsafe` code in this crate: one `DeviceIoControl` call on a
//! handle owned by a live `File`. Marking is best effort. File systems
//! without sparse support (FAT32, exFAT) keep today's behaviour. On other
//! platforms, growing a file already leaves unwritten ranges unallocated.

#![allow(unsafe_code)]

use std::fs::File;

/// Mark `file` sparse. Returns whether it is now sparse.
#[cfg(windows)]
pub(super) fn mark(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let mut returned: u32 = 0;
    // SAFETY: the handle belongs to `file`, which outlives the call. No
    // input buffer means "set sparse"; no output buffer is used; the call
    // is synchronous (no OVERLAPPED).
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            null(),
            0,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    };
    ok != 0
}

#[cfg(not(windows))]
pub(super) fn mark(_file: &File) -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    #[test]
    fn files_are_marked_sparse_and_stay_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.part");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        if !super::mark(&file) {
            eprintln!("skipped: the temp folder's file system has no sparse files");
            return;
        }
        file.set_len(1 << 30).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_ne!(meta.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE, 0);
        assert_eq!(meta.len(), 1 << 30);
        // Unwritten ranges read back as zeros.
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path).unwrap();
        f.seek(SeekFrom::Start(1 << 29)).unwrap();
        let mut buf = [1u8; 16];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [0u8; 16]);
    }
}
