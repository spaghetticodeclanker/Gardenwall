//! Windows backend: AppContainer + Job Objects (ADR-012 §3, §4.1; SPIKE-001).
//!
//! Mechanisms:
//! - **AppContainer** (no capabilities): denies network access, including
//!   loopback to processes outside the container, and denies filesystem
//!   access except where an ACE for the container SID is present.
//! - **ACL grants**: an ACE for the container SID is added to each granted
//!   path and removed when the [`Sandbox`] is dropped. With a grant journal
//!   (see [`reconcile_grant_journals`]), each grant is recorded on disk first,
//!   so grants left behind by a crash are revoked on the next start. Trees containing reparse
//!   points or multiply-linked files are refused, because ACE propagation
//!   could otherwise expose locations outside the grant.
//! - **Job Object**: kill-on-close, process-count and memory limits, UI
//!   restrictions. Processes are created suspended and assigned to the job
//!   before their first instruction runs.
//! - **Handle list**: only the child's three stdio pipe ends are inherited.
//! - **Child process policy**: when `max_processes == 1`, the process may not
//!   create children at all.
//! - **Environment**: an explicit block; nothing is inherited.
//! - **No console**: `DETACHED_PROCESS`; stdio goes through pipes. (A hidden
//!   console via `CREATE_NO_WINDOW` fails DLL initialization inside an
//!   AppContainer with `STATUS_DLL_INIT_FAILED`.)
//!
//! All `unsafe` code in this crate lives in this module.

#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NOT_ENOUGH_QUOTA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    GetLastError, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, AddAccessAllowedAceEx, AddAce, CONTAINER_INHERIT_ACE,
    CreateWellKnownSid, EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
    INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor, OBJECT_INHERIT_ACE,
    PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, UNPROTECTED_DACL_SECURITY_INFORMATION, WinBuiltinAnyPackageSid,
};
use windows_sys::Win32::Security::{
    ACL, CopySid, DACL_SECURITY_INFORMATION, FreeSid, GetLengthSid, IsValidSid, NO_INHERITANCE,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    SECURITY_CAPABILITIES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FindClose, FindFirstFileNameW, FindNextFileNameW,
    GetFileInformationByHandle,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, ResumeThread, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::SandboxError;
use crate::argv;
use crate::policy::{Access, PolicyError, SandboxPolicy};

// ---- private drive letters (a per-process device map) ----
//
// cmd.exe reads the drive's label from the drive root before `dir`, and
// PowerShell checks a folder by listing the folder above it; an AppContainer
// may open neither `C:\` nor the folders above its grants. Mapping each
// granted folder to its own drive letter makes the folder the root of a
// drive, so those checks stay inside the grant. The letters are links in an
// unnamed object directory that only this sandbox's processes use as their
// device map (other names fall back to the global ones, as usual). It grants
// nothing: access is still decided by the folder's ACL. The directory's DACL
// comes from Gardenwall's token, which the AppContainer has no access to, so
// sandboxed code can't add or change letters.

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const core::ffi::c_void,
    security_quality_of_service: *const core::ffi::c_void,
}

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtCreateDirectoryObject(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
    ) -> i32;
    fn NtCreateSymbolicLinkObject(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
        target: *const UnicodeString,
    ) -> i32;
    fn NtSetInformationProcess(
        process: HANDLE,
        class: u32,
        information: *const core::ffi::c_void,
        length: u32,
    ) -> i32;
}

const DIRECTORY_ALL_ACCESS: u32 = 0x000F_000F;
const SYMBOLIC_LINK_ALL_ACCESS: u32 = 0x000F_0001;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
/// `ProcessDeviceMap` (PROCESSINFOCLASS).
const PROCESS_DEVICE_MAP: u32 = 23;

fn nt_error(op: &'static str, status: i32) -> SandboxError {
    SandboxError::Os {
        op,
        code: status as u32,
    }
}

/// A UNICODE_STRING over `buf` (no terminating NUL needed).
fn unicode(buf: &mut [u16]) -> Result<UnicodeString, SandboxError> {
    let bytes = u16::try_from(buf.len() * 2).map_err(|_| SandboxError::Os {
        op: "device map path",
        code: 206,
    })?;
    Ok(UnicodeString {
        length: bytes,
        maximum_length: bytes,
        buffer: buf.as_mut_ptr(),
    })
}

/// The NT path of a local folder (`C:\x` -> `\Device\HarddiskVolume3\x`).
/// Network and other non-drive paths have none.
fn nt_path(folder: &Path) -> Option<Vec<u16>> {
    let text = folder.to_str()?;
    let plain = text.strip_prefix(r"\\?\").unwrap_or(text);
    let bytes = plain.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    let drive: Vec<u16> = plain[..2]
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut target = vec![0u16; 1024];
    // SAFETY: `drive` is NUL-terminated; `target` is a writable buffer of
    // the stated length.
    let n = unsafe {
        windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW(
            drive.as_ptr(),
            target.as_mut_ptr(),
            target.len() as u32,
        )
    };
    if n == 0 {
        return None;
    }
    // The first string of the list is the current mapping.
    let end = target.iter().position(|&c| c == 0)?;
    let device = String::from_utf16(&target[..end]).ok()?;
    // Only real volumes: a drive that is itself a mapping (subst, network)
    // points elsewhere.
    if !device.starts_with(r"\Device\") || device.starts_with(r"\Device\Mup") {
        return None;
    }
    let rest = plain[2..].trim_end_matches('\\');
    Some(format!("{device}{rest}").encode_utf16().collect())
}

/// Private drive letters for a sandbox's processes.
struct DeviceMap {
    directory: OwnedHandle,
    /// Kept open: an unnamed directory's links live as long as their handles.
    _links: Vec<OwnedHandle>,
}

impl DeviceMap {
    fn new(drives: &[(char, PathBuf)]) -> Result<Self, SandboxError> {
        let attributes = ObjectAttributes {
            length: std::mem::size_of::<ObjectAttributes>() as u32,
            root_directory: null_mut(),
            object_name: null(),
            attributes: 0,
            security_descriptor: null(),
            security_quality_of_service: null(),
        };
        let mut raw: HANDLE = null_mut();
        // SAFETY: valid out-pointer and attributes; an unnamed directory.
        let status =
            unsafe { NtCreateDirectoryObject(&mut raw, DIRECTORY_ALL_ACCESS, &attributes) };
        if status < 0 {
            return Err(nt_error("NtCreateDirectoryObject", status));
        }
        let directory = owned(raw);
        let mut links = Vec::new();
        for (letter, folder) in drives {
            let target = nt_path(folder).ok_or(SandboxError::Os {
                op: "drive mapping: not a local folder",
                code: 87,
            })?;
            let mut name: Vec<u16> = format!("{}:", letter.to_ascii_uppercase())
                .encode_utf16()
                .collect();
            let mut target = target;
            let name_us = unicode(&mut name)?;
            let target_us = unicode(&mut target)?;
            let link_attributes = ObjectAttributes {
                length: std::mem::size_of::<ObjectAttributes>() as u32,
                root_directory: directory.as_raw_handle() as HANDLE,
                object_name: &name_us,
                attributes: OBJ_CASE_INSENSITIVE,
                security_descriptor: null(),
                security_quality_of_service: null(),
            };
            let mut link: HANDLE = null_mut();
            // SAFETY: the strings outlive the call; the root is our directory.
            let status = unsafe {
                NtCreateSymbolicLinkObject(
                    &mut link,
                    SYMBOLIC_LINK_ALL_ACCESS,
                    &link_attributes,
                    &target_us,
                )
            };
            if status < 0 {
                return Err(nt_error("NtCreateSymbolicLinkObject", status));
            }
            links.push(owned(link));
        }
        Ok(Self {
            directory,
            _links: links,
        })
    }

    /// Make this the device map of a (suspended) process.
    fn apply(&self, process: HANDLE) -> Result<(), SandboxError> {
        let dir = self.directory.as_raw_handle() as HANDLE;
        // SAFETY: valid process handle; the information is a directory handle.
        let status = unsafe {
            NtSetInformationProcess(
                process,
                PROCESS_DEVICE_MAP,
                (&dir as *const HANDLE).cast(),
                std::mem::size_of::<HANDLE>() as u32,
            )
        };
        if status < 0 {
            return Err(nt_error(
                "NtSetInformationProcess(ProcessDeviceMap)",
                status,
            ));
        }
        Ok(())
    }
}

/// `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY` (winbase.h).
const PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY: usize = 0x0002_000E;
/// `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED` (winbase.h).
const PROCESS_CREATION_CHILD_PROCESS_RESTRICTED: u32 = 0x01;
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`.
const HRESULT_ALREADY_EXISTS: i32 = 0x8007_00B7_u32 as i32;
/// Upper bound on entries inspected when pre-checking a grant tree.
const NUL_CHAR: char = char::from_u32(0).unwrap();
const CR_CHAR: char = char::from_u32(13).unwrap();
const LF_CHAR: char = char::from_u32(10).unwrap();
const MAX_GRANT_TREE_ENTRIES: usize = 500_000;

fn last_error(op: &'static str) -> SandboxError {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    SandboxError::Os { op, code }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Convert a verbatim drive path (`\\?\C:\...`, as produced by
/// `canonicalize`) to its ordinary form for APIs that reject the verbatim
/// prefix, such as `CreateProcessW`. Other paths are returned unchanged, and
/// paths too long for the ordinary form are rejected.
fn process_path(path: &Path) -> Result<PathBuf, SandboxError> {
    let Some(text) = path.to_str() else {
        return Err(argv::ArgvError::NonUnicode.into());
    };
    let plain = match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest,
        Some(_) => return Err(PolicyError::RelativePath(path.to_path_buf()).into()),
        None => text,
    };
    if plain.len() >= 260 {
        return Err(PolicyError::RelativePath(path.to_path_buf()).into());
    }
    Ok(PathBuf::from(plain))
}

/// An owned copy of a SID, independent of the API that produced it.
#[derive(Clone)]
struct OwnedSid(Vec<u8>);

impl OwnedSid {
    /// # Safety
    /// `sid` must point to a valid SID.
    unsafe fn copy_from(sid: PSID) -> Result<Self, SandboxError> {
        // SAFETY: caller guarantees `sid` is a valid SID.
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(SandboxError::Os {
                op: "IsValidSid",
                code: 0,
            });
        }
        // SAFETY: `sid` is valid (checked above).
        let len = unsafe { GetLengthSid(sid) };
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` has exactly `len` writable bytes; `sid` is valid.
        if unsafe { CopySid(len, buf.as_mut_ptr().cast(), sid) } == 0 {
            return Err(last_error("CopySid"));
        }
        Ok(Self(buf))
    }

    fn as_psid(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn to_string_sid(&self) -> Result<String, SandboxError> {
        let mut out: *mut u16 = null_mut();
        // SAFETY: `as_psid` is a valid SID; `out` receives a LocalAlloc'd
        // string that we free below.
        if unsafe { ConvertSidToStringSidW(self.as_psid(), &mut out) } == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        // SAFETY: `out` is a valid NUL-terminated wide string from the API.
        let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
        // SAFETY: `out` points to `len` initialized u16 values.
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
        // SAFETY: `out` was allocated by the API with LocalAlloc.
        unsafe { LocalFree(out.cast()) };
        Ok(s)
    }
}

/// Create the AppContainer profile if needed and return its SID.
/// Serializes profile creation within this process: concurrent
/// `CreateAppContainerProfile` calls for one name can fail with
/// `E_UNEXPECTED`.
static PROFILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const HRESULT_UNEXPECTED: u32 = 0x8000_FFFF;

/// Create the AppContainer profile if needed and return its SID, retrying
/// briefly if another process is creating the same profile.
fn app_container_sid(profile: &str) -> Result<OwnedSid, SandboxError> {
    let _guard = PROFILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut attempt = 0;
    loop {
        match app_container_sid_once(profile) {
            Err(SandboxError::Os {
                code: HRESULT_UNEXPECTED,
                ..
            }) if attempt < 5 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            other => return other,
        }
    }
}

fn app_container_sid_once(profile: &str) -> Result<OwnedSid, SandboxError> {
    let name = wide(OsStr::new(profile));
    let mut sid: PSID = null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call; no capabilities
    // are passed; `sid` receives an allocation freed with FreeSid below.
    let hr = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            name.as_ptr(),
            name.as_ptr(),
            null(),
            0,
            &mut sid,
        )
    };
    if hr == HRESULT_ALREADY_EXISTS {
        // SAFETY: as above.
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if hr != 0 {
            return Err(SandboxError::Os {
                op: "DeriveAppContainerSidFromAppContainerName",
                code: hr as u32,
            });
        }
    } else if hr != 0 {
        return Err(SandboxError::Os {
            op: "CreateAppContainerProfile",
            code: hr as u32,
        });
    }
    // SAFETY: `sid` is a valid SID returned by the API.
    let owned = unsafe { OwnedSid::copy_from(sid) };
    // SAFETY: the SID was allocated by the AppContainer API; FreeSid is the
    // documented deallocator.
    unsafe { FreeSid(sid) };
    owned
}

/// The SID an AppContainer profile name maps to, without creating the profile.
fn derive_sid(profile: &str) -> Result<OwnedSid, SandboxError> {
    let name = wide(OsStr::new(profile));
    let mut sid: PSID = null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call; `sid` receives
    // an allocation freed with FreeSid below.
    let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    if hr != 0 {
        return Err(SandboxError::Os {
            op: "DeriveAppContainerSidFromAppContainerName",
            code: hr as u32,
        });
    }
    // SAFETY: `sid` is a valid SID returned by the API.
    let owned = unsafe { OwnedSid::copy_from(sid) };
    // SAFETY: allocated by the AppContainer API; FreeSid is its deallocator.
    unsafe { FreeSid(sid) };
    owned
}

/// Delete an AppContainer profile (e.g. on uninstall or in tests).
pub fn delete_profile(profile: &str) -> Result<(), SandboxError> {
    let name = wide(OsStr::new(profile));
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let hr = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if hr == 0 {
        Ok(())
    } else {
        Err(SandboxError::Os {
            op: "DeleteAppContainerProfile",
            code: hr as u32,
        })
    }
}

fn access_mask(access: Access) -> u32 {
    match access {
        Access::Read => FILE_GENERIC_READ,
        Access::ReadExecute => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        Access::ReadWrite => FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE,
    }
}

/// Number of hard links and reparse status of a path, without following it.
fn link_info(path: &Path) -> Result<(u32, bool), SandboxError> {
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| SandboxError::Io(path.to_path_buf(), e.kind()))?;
    // SAFETY: zeroed BY_HANDLE_FILE_INFORMATION is a valid out-buffer.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` is an open handle for the duration of the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(last_error("GetFileInformationByHandle"));
    }
    let reparse = info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    Ok((info.nNumberOfLinks, reparse))
}

/// Every name of the file at `path`, volume-relative (`\Users\…`), or
/// `None` when they can't all be read.
fn link_names(path: &Path) -> Option<Vec<String>> {
    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_HANDLE_EOF: u32 = 38;
    let wpath = wide(path.as_os_str());
    let mut buf: Vec<u16> = vec![0; 512];
    let mut names = Vec::new();
    let take = |buf: &[u16]| {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16(&buf[..end]).ok()
    };
    // The first name, growing the buffer once if it is too small.
    let mut find = None;
    for _ in 0..2 {
        let mut len = buf.len() as u32;
        // SAFETY: `wpath` is NUL-terminated; `buf` holds `len` characters.
        let h = unsafe { FindFirstFileNameW(wpath.as_ptr(), 0, &mut len, buf.as_mut_ptr()) };
        if h != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            find = Some(h);
            break;
        }
        // SAFETY: GetLastError has no preconditions.
        // (Error 50, not supported, on file systems without the call:
        // the file is refused, as before.)
        if unsafe { GetLastError() } != ERROR_MORE_DATA {
            return None;
        }
        buf = vec![0; len as usize + 1];
    }
    let find = find?;
    let mut ok = true;
    match take(&buf) {
        Some(n) => names.push(n),
        None => ok = false,
    }
    while ok {
        let mut len = buf.len() as u32;
        // SAFETY: `find` is an open find handle; `buf` holds `len` characters.
        if unsafe { FindNextFileNameW(find, &mut len, buf.as_mut_ptr()) } != 0 {
            match take(&buf) {
                Some(n) => names.push(n),
                None => ok = false,
            }
            continue;
        }
        // SAFETY: GetLastError has no preconditions.
        match unsafe { GetLastError() } {
            ERROR_HANDLE_EOF => break,
            ERROR_MORE_DATA => buf = vec![0; len as usize + 1],
            _ => ok = false,
        }
    }
    // SAFETY: `find` came from FindFirstFileNameW and is closed once.
    unsafe { FindClose(find) };
    ok.then_some(names)
}

/// The folder `root` as the names [`link_names`] returns are written
/// (`\Users\you\project`): a canonical drive path only.
fn volume_relative(root: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(root).ok()?;
    let text = canonical.to_str()?;
    let rest = text.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    Some(rest[2..].trim_end_matches('\\').to_string())
}

/// Whether every name of a file with `links` hard links lies inside the
/// folder `inside` (as [`volume_relative`] wrote it). Its permissions are
/// shared by all its names, so a grant is safe only then (build tools such
/// as Rust's incremental compiler link files inside their own output).
/// Names are compared ignoring ASCII case only: any other difference
/// refuses, never the reverse.
fn links_stay_inside(path: &Path, links: u32, inside: &str) -> bool {
    let Some(names) = link_names(path) else {
        return false;
    };
    names.len() == links as usize
        && names.iter().all(|n| {
            n.len() > inside.len()
                && n.is_char_boundary(inside.len())
                && n[..inside.len()].eq_ignore_ascii_case(inside)
                && n[inside.len()..].starts_with('\\')
        })
}

/// Refuse grant targets that contain reparse points (symlinks, junctions,
/// mount points) anywhere in the tree, or files with several hard links
/// unless all their names are inside the tree. One link is allowed, in
/// read-only grants only: see [`allowed_link`].
pub fn check_grant_tree(root: &Path, access: Access) -> Result<(), SandboxError> {
    // Worked out when the first multiply-linked file is met.
    let mut inside: Option<Option<String>> = None;
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(path) = stack.pop() {
        seen += 1;
        if seen > MAX_GRANT_TREE_ENTRIES {
            return Err(PolicyError::TooManyEntries(root.to_path_buf()).into());
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            if access != Access::ReadWrite
                && meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
                && allowed_link(root, &path)
            {
                // Not walked: its target is inside this tree and checked
                // there.
                continue;
            }
            return Err(PolicyError::ReparsePoint(path).into());
        }
        if meta.is_dir() {
            let entries =
                std::fs::read_dir(&path).map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
            for entry in entries {
                let entry = entry.map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
                stack.push(entry.path());
            }
        } else {
            let (links, reparse) = link_info(&path)?;
            if reparse {
                return Err(PolicyError::ReparsePoint(path).into());
            }
            if links > 1 {
                let inside = inside.get_or_insert_with(|| volume_relative(root));
                let safe = inside
                    .as_deref()
                    .is_some_and(|i| links_stay_inside(&path, links, i));
                if !safe {
                    return Err(PolicyError::MultiplyLinked(path).into());
                }
            }
        }
    }
    Ok(())
}

/// The one link a read-only grant may hold
/// (ADR-024 §9 and ADR-025): RubyInstaller's `ssl` junction at the top of a
/// Ruby folder (one with `bin\ruby.exe`), pointing to that same folder's
/// `lib\ruby\<x.y.0>\etc\ssl`. Exact name, place and target; any other link,
/// or this one pointing anywhere else, is still refused.
fn allowed_link(root: &Path, link: &Path) -> bool {
    if link.parent() != Some(root)
        || !link
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("ssl"))
        || !root.join("bin").join("ruby.exe").is_file()
    {
        return false;
    }
    let (Ok(target), Ok(lib)) = (
        std::fs::canonicalize(link),
        std::fs::canonicalize(root.join("lib").join("ruby")),
    ) else {
        return false;
    };
    let Ok(rest) = target.strip_prefix(&lib) else {
        return false;
    };
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let version_ok = parts.first().is_some_and(|v| {
        let nums: Vec<&str> = v.split('.').collect();
        nums.len() == 3
            && nums
                .iter()
                .all(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
    });
    // The target is a real folder (no further link on the way).
    let real = std::fs::symlink_metadata(&target)
        .is_ok_and(|m| m.is_dir() && m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0);
    version_ok && parts.len() == 3 && parts[1] == "etc" && parts[2] == "ssl" && real
}

/// Apply an access-control change for `sid` on `path`.
fn set_ace(
    path: &Path,
    sid: &OwnedSid,
    mode: i32,
    mask: u32,
    inherit: u32,
) -> Result<(), SandboxError> {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wpath` is NUL-terminated; out-pointers are valid; `sd` is
    // LocalAlloc'd by the API and freed below. `dacl` points into `sd`.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "GetNamedSecurityInfoW",
            code: rc,
        });
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inherit,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.as_psid().cast(),
        },
    };
    let mut new_dacl: *mut ACL = null_mut();
    // SAFETY: `entry` references `sid`, which outlives the call; `dacl` is
    // valid (owned by `sd`); `new_dacl` is LocalAlloc'd and freed below.
    let rc = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut new_dacl) };
    if rc != ERROR_SUCCESS {
        // SAFETY: `sd` was allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(sd) };
        return Err(SandboxError::Os {
            op: "SetEntriesInAclW",
            code: rc,
        });
    }
    // SAFETY: `wpath` is NUL-terminated; `new_dacl` is a valid ACL.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            new_dacl,
            null(),
        )
    };
    // SAFETY: both were LocalAlloc'd by the APIs above and are not used again.
    unsafe {
        LocalFree(new_dacl.cast());
        LocalFree(sd);
    }
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "SetNamedSecurityInfoW",
            code: rc,
        });
    }
    Ok(())
}

/// Whether every AppContainer may already read and execute `path`: allow
/// entries for ALL APPLICATION PACKAGES that cover the path itself and, for
/// a folder, everything inherited below it. Windows sets this up for
/// Program Files and system folders, which also refuse ACL changes, so a
/// read/execute grant there would add nothing and fail. Deny entries for
/// that group, a null DACL or any error answer "no". A wrong "yes" only
/// leaves the helper unable to read the folder (it fails closed); this
/// never adds access.
/// Whether every AppContainer can already read and run `path` (Program
/// Files and system folders), so a sandbox needs no grant for it and no
/// permission is changed. "No" on any doubt.
pub fn appcontainers_can_read(path: &Path) -> bool {
    all_packages_can_read(path)
}

/// Whether a sandbox can be given read and run access to `path`: every
/// AppContainer can already read it, or this user may change its
/// permissions (checked by opening it for WRITE_DAC only, which changes
/// nothing). Installers such as Node.js's set permissions only an
/// administrator can change, and a grant there fails.
pub fn sandbox_can_use(path: &Path) -> bool {
    const WRITE_DAC: u32 = 0x0004_0000;
    all_packages_can_read(path)
        || std::fs::OpenOptions::new()
            .access_mode(WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .is_ok()
}

/// A read/execute grant on a path every AppContainer can already read:
/// the sandbox adds no permission for it.
fn needs_no_grant(grant: &crate::policy::PathGrant) -> bool {
    grant.access == Access::ReadExecute && all_packages_can_read(&grant.path)
}

fn all_packages_can_read(path: &Path) -> bool {
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    let needed = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
    let dir = path.is_dir();
    let mut any = [0u8; 68];
    let mut any_len = any.len() as u32;
    // SAFETY: `any` is SECURITY_MAX_SID_SIZE bytes and `any_len` says so.
    if unsafe {
        CreateWellKnownSid(
            WinBuiltinAnyPackageSid,
            null_mut(),
            any.as_mut_ptr().cast(),
            &mut any_len,
        )
    } == 0
    {
        return false;
    }
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below and `dacl` points into it.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let (mut on_self, mut to_files, mut to_folders) = (0u32, 0u32, 0u32);
    let mut denied = false;
    if !dacl.is_null() {
        // SAFETY: `dacl` is a valid ACL owned by `sd`.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..count as u32 {
            let mut ace: *mut core::ffi::c_void = null_mut();
            // SAFETY: `i` is below the ACE count; `ace` points into `dacl`.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
                continue;
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let header = unsafe { *(ace as *const ACE_HEADER) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE && header.AceType != ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            // SAFETY: allowed and denied ACEs share ACCESS_ALLOWED_ACE's
            // layout; the SID starts at `SidStart` within the ACE.
            let (mask, sid) = unsafe {
                let a = ace as *const ACCESS_ALLOWED_ACE;
                ((*a).Mask, core::ptr::addr_of!((*a).SidStart) as PSID)
            };
            // SAFETY: both SIDs are valid for the duration of the call.
            if unsafe { EqualSid(sid, any.as_mut_ptr().cast()) } == 0 {
                continue;
            }
            if header.AceType == ACCESS_DENIED_ACE_TYPE {
                denied = true;
                break;
            }
            let mut m = mask;
            if mask & GENERIC_READ != 0 {
                m |= FILE_GENERIC_READ;
            }
            if mask & GENERIC_EXECUTE != 0 {
                m |= FILE_GENERIC_EXECUTE;
            }
            if mask & GENERIC_ALL != 0 {
                m |= FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
            }
            let flags = header.AceFlags as u32;
            if flags & INHERIT_ONLY_ACE == 0 {
                on_self |= m;
            }
            if flags & OBJECT_INHERIT_ACE != 0 {
                to_files |= m;
            }
            if flags & CONTAINER_INHERIT_ACE != 0 {
                to_folders |= m;
            }
        }
    }
    // SAFETY: allocated by GetNamedSecurityInfoW and not used again.
    unsafe { LocalFree(sd) };
    let covers = |m: u32| m & needed == needed;
    !dacl.is_null()
        && !denied
        && covers(on_self)
        && (!dir || (covers(to_files) && covers(to_folders)))
}

// ---- read-only locations inside a grant ----

const ACE_INHERITED_FLAG: u8 = 0x10;
const SE_DACL_PROTECTED_FLAG: u16 = 0x1000;
const SDDL_REVISION: u32 = 1;

/// A path whose permissions [`Sandbox::protect`] replaced, with the
/// original DACL (SDDL) to put back.
struct SavedDacl {
    path: PathBuf,
    sddl: String,
}

/// A path's DACL as SDDL (with its protected/inherited flags).
fn dacl_sddl(path: &Path) -> Result<String, SandboxError> {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "GetNamedSecurityInfoW",
            code: rc,
        });
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sd` is valid; `text` is LocalAlloc'd by the API.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION,
            DACL_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        )
    } != 0;
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    if !ok || text.is_null() {
        return Err(last_error(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW",
        ));
    }
    // SAFETY: `text` is a NUL-terminated wide string from the API.
    let s = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: allocated by the conversion above.
    unsafe { LocalFree(text.cast()) };
    Ok(s)
}

/// The raw ACEs of `dacl`, each passed through `keep` (which may drop it
/// or change its bytes).
fn rebuild_aces(dacl: *const ACL, mut keep: impl FnMut(&mut Vec<u8>) -> bool) -> Vec<u8> {
    let mut out = Vec::new();
    if dacl.is_null() {
        return out;
    }
    // SAFETY: `dacl` is a valid ACL for the duration of the call.
    let count = unsafe { (*dacl).AceCount };
    for i in 0..count as u32 {
        let mut ace: *mut core::ffi::c_void = null_mut();
        // SAFETY: `i` is below the ACE count; `ace` points into `dacl`.
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            continue;
        }
        // SAFETY: every ACE starts with an ACE_HEADER giving its size.
        let size = unsafe { (*(ace as *const ACE_HEADER)).AceSize } as usize;
        // SAFETY: the ACE spans `size` bytes inside the ACL.
        let mut bytes = unsafe { std::slice::from_raw_parts(ace as *const u8, size) }.to_vec();
        if keep(&mut bytes) {
            out.extend_from_slice(&bytes);
        }
    }
    out
}

/// Whether a raw allowed/denied ACE names `sid`.
fn ace_names(bytes: &[u8], sid: &OwnedSid) -> bool {
    let kind = bytes[0];
    if kind != 0 && kind != 1 {
        return false;
    }
    // SAFETY: allowed and denied ACEs share ACCESS_ALLOWED_ACE's layout; the
    // SID starts 8 bytes in (header + mask) and both SIDs are valid.
    unsafe { EqualSid(bytes.as_ptr().add(8) as PSID, sid.as_psid()) != 0 }
}

/// Build a DACL from raw ACEs (plus one allow entry for `extra`) and set
/// it on `path`, protected from inheritance or not.
fn set_dacl(
    path: &Path,
    revision: u8,
    aces: &[u8],
    extra: Option<(&OwnedSid, u32, u32)>,
    protected: bool,
) -> Result<(), SandboxError> {
    let extra_size = extra.map_or(0, |(sid, _, _)| 8 + sid.0.len());
    let size = (8 + aces.len() + extra_size + 8).next_multiple_of(4);
    let mut buf = vec![0u32; size / 4];
    let acl = buf.as_mut_ptr() as *mut ACL;
    let revision = u32::from(revision.max(2));
    // SAFETY: `buf` is `size` bytes, DWORD-aligned.
    if unsafe { InitializeAcl(acl, size as u32, revision) } == 0 {
        return Err(last_error("InitializeAcl"));
    }
    if !aces.is_empty()
        // SAFETY: `aces` holds whole ACEs copied from a valid ACL.
        && unsafe {
            AddAce(
                acl,
                revision,
                u32::MAX,
                aces.as_ptr().cast(),
                aces.len() as u32,
            )
        } == 0
    {
        return Err(last_error("AddAce"));
    }
    if let Some((sid, flags, mask)) = extra {
        // SAFETY: `acl` has room for this entry; `sid` is valid.
        if unsafe { AddAccessAllowedAceEx(acl, revision, flags, mask, sid.as_psid()) } == 0 {
            return Err(last_error("AddAccessAllowedAceEx"));
        }
    }
    let info = DACL_SECURITY_INFORMATION
        | if protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
    let wpath = wide(path.as_os_str());
    // SAFETY: `wpath` is NUL-terminated; `acl` is a valid ACL.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            info,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "SetNamedSecurityInfoW",
            code: rc,
        });
    }
    Ok(())
}

/// Turn off inheritance on `path`, keep every entry except the sandbox's
/// (now explicit), and give the sandbox `access` only (`None`: no entry
/// at all, so it can't even read the path).
fn replace_access(
    path: &Path,
    sid: &OwnedSid,
    dir: bool,
    access: Option<u32>,
) -> Result<(), SandboxError> {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "GetNamedSecurityInfoW",
            code: rc,
        });
    }
    let aces = rebuild_aces(dacl, |ace| {
        if ace_names(ace, sid) {
            return false;
        }
        ace[1] &= !ACE_INHERITED_FLAG;
        true
    });
    let revision = if dacl.is_null() {
        2
    } else {
        // SAFETY: `dacl` is non-null and valid until `sd` is freed.
        unsafe { (*dacl).AclRevision }
    };
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    let flags = if dir {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        0
    };
    set_dacl(
        path,
        revision,
        &aces,
        access.map(|mask| (sid, flags, mask)),
        true,
    )
}

/// Put back a DACL saved by [`dacl_sddl`]. When it inherited, only its
/// explicit entries are set and the rest is inherited again from the
/// parent. A path that no longer exists counts as restored.
fn restore_dacl(path: &Path, sddl: &str) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return true;
    }
    let wsddl = wide(OsStr::new(sddl));
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wsddl` is NUL-terminated; `sd` is LocalAlloc'd by the API.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wsddl.as_ptr(),
            SDDL_REVISION,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return false;
    }
    let (mut present, mut defaulted) = (0i32, 0i32);
    let mut dacl: *mut ACL = null_mut();
    let mut control: u16 = 0;
    let mut rev: u32 = 0;
    // SAFETY: `sd` is valid; out-pointers are valid.
    let ok = unsafe {
        GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) != 0
            && GetSecurityDescriptorControl(sd, &mut control, &mut rev) != 0
    };
    let result = ok && present != 0 && {
        let protected = control & SE_DACL_PROTECTED_FLAG != 0;
        let aces = rebuild_aces(dacl, |ace| protected || ace[1] & ACE_INHERITED_FLAG == 0);
        let revision = if dacl.is_null() {
            2
        } else {
            // SAFETY: `dacl` is non-null and valid while `sd` lives.
            unsafe { (*dacl).AclRevision }
        };
        set_dacl(path, revision, &aces, None, protected).is_ok()
    };
    // SAFETY: allocated by the conversion above, not used again.
    unsafe { LocalFree(sd) };
    result
}

/// The SID of the user this process runs as, as a string (`S-1-5-21-…`).
fn current_user_sid() -> Result<String, SandboxError> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle of this process is always valid; `token`
    // receives a handle that `owned` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let token = owned(token);
    // A TOKEN_USER followed by its SID (at most 68 bytes), 8-byte aligned.
    let mut buf = [0u64; 16];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for the size given and aligned for
    // TOKEN_USER; `token` is a valid token handle opened for query.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            &mut len,
        )
    } == 0
    {
        return Err(last_error("GetTokenInformation"));
    }
    // SAFETY: the call succeeded, so `buf` starts with a TOKEN_USER whose
    // SID pointer points into `buf`, which outlives the copy.
    let sid = unsafe { OwnedSid::copy_from((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid) }?;
    sid.to_string_sid()
}

/// Whether this process runs as administrator (an elevated token). Files
/// it creates then belong to the Administrators group, not the user, and
/// Git in the sandbox refuses repositories it made ("dubious ownership").
/// `false` when it can't be told.
pub fn running_elevated() -> bool {
    use windows_sys::Win32::Security::{TOKEN_ELEVATION, TokenElevation};
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle of this process is always valid; `token`
    // receives a handle that `owned` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = owned(token);
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a writable TOKEN_ELEVATION of the size given;
    // `token` is a valid token handle opened for query.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    } != 0;
    ok && elevation.TokenIsElevated != 0
}

/// Leave the folder `path` open to the current user and the system only:
/// inheritance from its parent is turned off and nothing else is allowed in
/// (no other user, group or app container). New files and folders in it
/// inherit the same. For the folder where secret files are kept.
pub fn make_private(path: &Path) -> Result<(), SandboxError> {
    let user = current_user_sid()?;
    let sddl = format!("D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)");
    if restore_dacl(path, &sddl) && std::fs::symlink_metadata(path).is_ok() {
        Ok(())
    } else {
        Err(SandboxError::Os {
            op: "make_private",
            code: 0,
        })
    }
}

/// Whether `path`'s DACL has an entry for `sid` (a protection Gardenwall
/// set and hasn't restored yet).
fn dacl_names(path: &Path, sid: &OwnedSid) -> bool {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let mut found = false;
    let _ = rebuild_aces(dacl, |ace| {
        found |= ace_names(ace, sid) && ace[1] & ACE_INHERITED_FLAG == 0;
        false
    });
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    found
}

/// The program file of a running process, or `None`.
fn process_image(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain call; a failure returns null.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let process = owned(process);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: valid handle; `buf` holds `len` characters.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle() as HANDLE,
            0,
            buf.as_mut_ptr(),
            &mut len,
        )
    } != 0;
    ok.then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
}

/// Grants and permission changes held by this process's open sandboxes,
/// by container SID and path (found 2026-11-24: two agent sessions share
/// one SID, so the first to end revoked the other's access to a shared
/// project or tool folder, and a second `protect` of one path recorded the
/// first one's read-only permissions as the ones to restore). A grant is
/// revoked, and original permissions restored, only by the last sandbox
/// holding them; the original permissions are recorded once, by the first.
#[derive(Default)]
struct Shared {
    /// Open sandboxes (and grant holds) holding a grant on the path.
    grants: usize,
    /// The access that grant gives (the widest asked for).
    access: Option<Access>,
    /// The path's permissions before the first `protect`/`hide`, and how
    /// many open sandboxes hold that change.
    dacl: Option<(String, usize)>,
    /// "Folders above this project" holds on the path (ADR-025), whether
    /// the first one added the entry (it wasn't there already), and the
    /// journal recording it.
    lists: usize,
    list_added: bool,
    list_journal: Option<GrantJournal>,
    /// A grant or revoke on the path is being written (slow for a big
    /// folder, so done without the table's lock): other grants and revokes
    /// of the same path wait for it ([`shared_idle`]).
    busy: bool,
}

impl Shared {
    /// Nothing holds anything on the path any more.
    fn unused(&self) -> bool {
        self.grants == 0 && self.dacl.is_none() && self.lists == 0 && !self.busy
    }
}

type SharedKey = (Vec<u8>, PathBuf);

static SHARED: std::sync::Mutex<Option<std::collections::HashMap<SharedKey, Shared>>> =
    std::sync::Mutex::new(None);

fn with_shared<T>(f: impl FnOnce(&mut std::collections::HashMap<SharedKey, Shared>) -> T) -> T {
    let mut guard = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Default::default))
}

/// Whether access `held` includes everything `wanted` allows.
fn covers(held: Access, wanted: Access) -> bool {
    held == wanted
        || held == Access::ReadWrite
        || (held == Access::ReadExecute && wanted == Access::Read)
}

/// Whether this process already holds a grant on `path` for the profile's
/// container with at least `access` (so a new sandbox needn't check or
/// grant it again).
fn already_held(sid: &OwnedSid, path: &Path, access: Access) -> bool {
    with_shared(|shared| {
        shared
            .get(&(sid.0.clone(), path.to_path_buf()))
            .is_some_and(|e| e.grants > 0 && e.access.is_some_and(|held| covers(held, access)))
    })
}

/// A grant held by the app itself, outside any sandbox, so sessions that
/// start while it lives find the folder already granted (ADR-022 §8: a
/// large toolchain folder took about 8 s to check and grant at every
/// session start). Journaled like a sandbox's grants, so a crash leaves
/// nothing behind after the next reconciliation; revoked on drop unless an
/// open sandbox still holds it.
pub struct GrantHold {
    grant: Option<AclGrant>,
    record: Option<Record>,
}

impl GrantHold {
    /// Hold `access` on `path` for `profile`'s container. A folder every
    /// AppContainer can already read needs nothing and holds nothing.
    pub fn new(
        profile: &str,
        path: &Path,
        access: Access,
        grant_journal_dir: Option<&Path>,
    ) -> Result<Self, SandboxError> {
        let _cleanup = cleanup_done();
        let journal = match grant_journal_dir {
            Some(dir) => Some(GrantJournal::create(dir)?),
            None => None,
        };
        Self::grant(profile, path, access, journal.map(Record::Journal))
    }

    /// Like [`GrantHold::new`], but the grant outlives Gardenwall
    /// it is recorded in
    /// `kept_dir`, which start-up cleanup leaves alone, so the next start
    /// finds it in place ([`GrantHold::adopt`]) instead of removing it and
    /// granting it again (each a pass over every file below the folder:
    /// tens of seconds for a Rust toolchain). Dropping the hold still
    /// revokes it and deletes the record; only the process ending keeps it.
    pub fn kept(
        profile: &str,
        path: &Path,
        access: Access,
        kept_dir: &Path,
    ) -> Result<Self, SandboxError> {
        let _cleanup = cleanup_done();
        Self::kept_unlocked(profile, path, access, kept_dir)
    }

    fn kept_unlocked(
        profile: &str,
        path: &Path,
        access: Access,
        kept_dir: &Path,
    ) -> Result<Self, SandboxError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| SandboxError::Io(path.to_path_buf(), e.kind()))?;
        // Pending until the grant has reached every file below the folder:
        // a process ending halfway leaves a record the next start grants
        // again in full instead of taking over.
        let record = KeptRecord::create(kept_dir, profile, &canonical, access, true)?;
        Self::grant(profile, path, access, Some(Record::Kept(record)))
    }

    fn grant(
        profile: &str,
        path: &Path,
        access: Access,
        record: Option<Record>,
    ) -> Result<Self, SandboxError> {
        let spec = crate::policy::PathGrant {
            path: path.to_path_buf(),
            access,
        };
        if needs_no_grant(&spec) {
            if let Some(record) = record {
                record.remove();
            }
            return Ok(Self {
                grant: None,
                record: None,
            });
        }
        let result = (|| {
            check_grant_tree(path, access)?;
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| SandboxError::Io(path.to_path_buf(), e.kind()))?;
            let sid = app_container_sid(profile)?;
            Ok((canonical, sid))
        })();
        let (canonical, sid) = match result {
            Ok(v) => v,
            Err(e) => {
                if let Some(record) = record {
                    record.remove();
                }
                return Err(e);
            }
        };
        let mut record = record;
        // Written (and synced) before the ACE it describes is added.
        if let Some(Record::Journal(journal)) = &mut record {
            journal.record(profile, &canonical)?;
        }
        match AclGrant::new(&canonical, &sid, access) {
            Ok(grant) => {
                // The entry is on every file now: the record is complete.
                if let Some(Record::Kept(k)) = &record {
                    k.confirm(profile, &canonical, access);
                }
                Ok(Self {
                    grant: Some(grant),
                    record,
                })
            }
            Err(e) => {
                // Nothing was granted: a kept record would name an entry
                // that isn't there.
                if let Some(Record::Kept(k)) = record {
                    k.remove();
                }
                Err(e)
            }
        }
    }

    /// Take over a grant kept from an earlier run. When it was completed
    /// and the folder still has exactly that entry for the container,
    /// nothing is checked or changed (reading one folder's permissions);
    /// otherwise (pending: a process ended while granting it; or changed
    /// since) it is granted again as [`GrantHold::kept`] does. The folder's tree was checked
    /// when it was first granted, and while Gardenwall was closed nothing
    /// could pass the entry on: Windows copies it to new files only, and a
    /// new hard link keeps its file's own permissions.
    pub fn adopt(kept: KeptGrant) -> Result<Self, SandboxError> {
        let _cleanup = cleanup_done();
        let sid = app_container_sid(&kept.profile)?;
        if !kept.pending && explicit_allow_mask(&kept.path, &sid) == Some(access_mask(kept.access))
        {
            let key: SharedKey = (sid.0.clone(), kept.path.clone());
            let grant = {
                let mut guard = shared_idle(&key);
                let entry = guard
                    .get_or_insert_with(Default::default)
                    .entry(key)
                    .or_default();
                entry.access = Some(match entry.access {
                    Some(held) if covers(held, kept.access) => held,
                    _ => kept.access,
                });
                entry.grants += 1;
                AclGrant {
                    path: kept.path.clone(),
                    sid: sid.clone(),
                }
            };
            return Ok(Self {
                grant: Some(grant),
                record: Some(Record::Kept(KeptRecord { path: kept.file })),
            });
        }
        // Changed while Gardenwall was closed: start over.
        let KeptGrant {
            profile,
            path,
            access,
            file,
            ..
        } = kept;
        let dir = file.parent().map(Path::to_path_buf);
        let _ = std::fs::remove_file(&file);
        let dir =
            dir.ok_or_else(|| SandboxError::Io(file.clone(), std::io::ErrorKind::NotFound))?;
        Self::kept_unlocked(&profile, &path, access, &dir)
    }
}

impl Drop for GrantHold {
    fn drop(&mut self) {
        let revoked = self.grant.as_ref().is_none_or(AclGrant::revoke);
        if let Some(record) = self.record.take()
            && revoked
        {
            record.remove();
        }
    }
}

/// Where a [`GrantHold`] is written down: a journal (cleaned up at the next
/// start) or a kept record (left in place for the next run).
enum Record {
    Journal(GrantJournal),
    Kept(KeptRecord),
}

impl Record {
    fn remove(self) {
        match self {
            Record::Journal(j) => j.remove(),
            Record::Kept(k) => k.remove(),
        }
    }
}

/// Suffix of kept-grant records.
const KEPT_EXTENSION: &str = "kept";

/// A kept grant's record file: one line `<profile> <access> <path hex>`,
/// followed by ` pending` until the grant has reached every file below the
/// folder.
struct KeptRecord {
    path: PathBuf,
}

fn access_token(access: Access) -> &'static str {
    match access {
        Access::Read => "r",
        Access::ReadExecute => "rx",
        Access::ReadWrite => "rw",
    }
}

impl KeptRecord {
    fn create(
        dir: &Path,
        profile: &str,
        target: &Path,
        access: Access,
        pending: bool,
    ) -> Result<Self, SandboxError> {
        use std::io::Write;
        std::fs::create_dir_all(dir).map_err(|e| SandboxError::Io(dir.to_path_buf(), e.kind()))?;
        let mut name = [0u8; 16];
        getrandom::fill(&mut name).map_err(|_| SandboxError::Os {
            op: "getrandom",
            code: 0,
        })?;
        let hex: String = name.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("{hex}.{KEPT_EXTENSION}"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
        file.write_all(Self::line(profile, target, access, pending).as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
        Ok(Self { path })
    }

    fn line(profile: &str, target: &Path, access: Access, pending: bool) -> String {
        format!(
            "{profile} {} {}{}\n",
            access_token(access),
            encode_path(target),
            if pending { " pending" } else { "" }
        )
    }

    /// Mark the grant complete: the record is replaced in one step (a new
    /// file renamed over it), so it is never half written. If that fails,
    /// it stays pending and the next start grants it again: slower, never
    /// wrong.
    fn confirm(&self, profile: &str, target: &Path, access: Access) {
        use std::io::Write;
        let tmp = self.path.with_extension("tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut f| {
            f.write_all(Self::line(profile, target, access, false).as_bytes())?;
            f.sync_data()
        });
        if written.is_err() || std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn remove(self) {
        let _ = std::fs::remove_file(self.path);
    }
}

/// A grant kept from an earlier run (see [`GrantHold::kept`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptGrant {
    pub profile: String,
    /// The folder, canonical.
    pub path: PathBuf,
    pub access: Access,
    /// A process ended before the grant reached every file below the
    /// folder (or the record came from a leftover journal, which can't
    /// tell): taken over only by granting again.
    pub pending: bool,
    /// Its record file.
    file: PathBuf,
}

/// The grants kept in `dir` from earlier runs. Records that can't be read
/// or don't name a Gardenwall container are skipped (and left as they are).
pub fn kept_grants(dir: &Path) -> Vec<KeptGrant> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let file = entry.path();
        if file.extension().and_then(|e| e.to_str()) != Some(KEPT_EXTENSION)
            || !entry
                .metadata()
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_JOURNAL_BYTES)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let parsed = (|| {
            let mut parts = text.trim().split(' ');
            let profile = parts.next()?;
            let access = match parts.next()? {
                "r" => Access::Read,
                "rx" => Access::ReadExecute,
                "rw" => Access::ReadWrite,
                _ => return None,
            };
            let path = decode_path(parts.next()?)?;
            let pending = match parts.next() {
                None => false,
                Some("pending") => true,
                Some(_) => return None,
            };
            (profile.starts_with(JOURNAL_PROFILE_PREFIX)
                && crate::policy::valid_profile_name(profile)
                && parts.next().is_none())
            .then(|| (profile.to_string(), path, access, pending))
        })();
        if let Some((profile, path, access, pending)) = parsed {
            out.push(KeptGrant {
                profile,
                path,
                access,
                pending,
                file,
            });
        }
    }
    out
}

/// Revoke a kept grant no longer wanted, and delete its record. While this
/// process holds a grant on the same path (an open sandbox), the entry
/// stays for it and only the record goes. False when revoking failed (the
/// record stays, for the next try).
pub fn revoke_kept(kept: KeptGrant) -> bool {
    let _cleanup = cleanup_done();
    let Ok(sid) = derive_sid(&kept.profile) else {
        return false;
    };
    let key: SharedKey = (sid.0.clone(), kept.path.clone());
    {
        let mut guard = shared_idle(&key);
        let entry = guard
            .get_or_insert_with(Default::default)
            .entry(key.clone())
            .or_default();
        if entry.grants > 0 {
            drop(guard);
            let _ = std::fs::remove_file(&kept.file);
            return true;
        }
        entry.busy = true;
    }
    // Slow for a big folder: without the table's lock.
    let revoked = revoke_ace(&kept.path, &sid);
    finish_change(&key, |_| {});
    if revoked {
        let _ = std::fs::remove_file(&kept.file);
    }
    revoked
}

/// Revoke every kept grant in `dir` (the uninstaller). Every one is tried,
/// even after one fails.
pub fn revoke_all_kept(dir: &Path) -> bool {
    let failed = kept_grants(dir)
        .into_iter()
        .filter(|k| !revoke_kept(k.clone()))
        .count();
    failed == 0
}

/// The rights of the container's own (not inherited) allow entries on
/// `path`, or `None` when it has none or its permissions can't be read.
fn explicit_allow_mask(path: &Path, sid: &OwnedSid) -> Option<u32> {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let mut mask = None::<u32>;
    let _ = rebuild_aces(dacl, |ace| {
        if ace[0] == 0 && ace[1] & ACE_INHERITED_FLAG == 0 && ace_names(ace, sid) {
            mask = Some(mask.unwrap_or(0) | ace_mask(ace));
        }
        false
    });
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    mask
}

/// Start-up cleanup of leftover grants takes this for writing; making a
/// sandbox or a grant hold takes it for reading, so neither starts until
/// the cleanup is over: both may touch the same entry (one container SID,
/// one folder), and the cleanup would take away what the new one added.
static CLEANUP: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn cleanup_done() -> std::sync::RwLockReadGuard<'static, ()> {
    CLEANUP.read().unwrap_or_else(|e| e.into_inner())
}

/// An ACE granted to the container on one path; revoked on drop by the last
/// sandbox holding it.
struct AclGrant {
    path: PathBuf,
    sid: OwnedSid,
}

impl AclGrant {
    fn new(path: &Path, sid: &OwnedSid, access: Access) -> Result<Self, SandboxError> {
        let inherit = if path.is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        };
        let key: SharedKey = (sid.0.clone(), path.to_path_buf());
        let merged = {
            let mut guard = shared_idle(&key);
            let entry = guard
                .get_or_insert_with(Default::default)
                .entry(key.clone())
                .or_default();
            // Already granted with at least this access (a grant hold, or
            // another open sandbox): nothing to change, which is what makes
            // a session start fast when a large toolchain folder is held.
            if entry.grants > 0 && entry.access.is_some_and(|held| covers(held, access)) {
                entry.grants += 1;
                return Ok(Self {
                    path: path.to_path_buf(),
                    sid: sid.clone(),
                });
            }
            entry.busy = true;
            match entry.access {
                Some(held) if covers(held, access) => held,
                Some(held) if covers(access, held) => access,
                Some(_) => Access::ReadWrite,
                None => access,
            }
        };
        // The slow part (Windows passes the entry to every file below a
        // folder) runs without the table's lock: only changes to this same
        // folder wait for it. GRANT_ACCESS merges with an existing entry,
        // so two sandboxes with different access to one path keep theirs.
        let result = set_ace(path, sid, GRANT_ACCESS, access_mask(access), inherit);
        finish_change(&key, |entry| {
            if result.is_ok() {
                entry.access = Some(merged);
                entry.grants += 1;
            }
        });
        result.map(|()| Self {
            path: path.to_path_buf(),
            sid: sid.clone(),
        })
    }
}

impl AclGrant {
    /// Remove every explicit ACE for the container SID on this path, and
    /// their inherited copies on children, unless another open sandbox of
    /// this process still holds a grant on it.
    fn revoke(&self) -> bool {
        let key: SharedKey = (self.sid.0.clone(), self.path.clone());
        {
            let mut guard = shared_idle(&key);
            if let Some(entry) = guard.get_or_insert_with(Default::default).get_mut(&key) {
                entry.grants = entry.grants.saturating_sub(1);
                if entry.grants > 0 {
                    return true;
                }
                entry.access = None;
                entry.busy = true;
            }
        }
        // Slow for a big folder: without the table's lock (see `new`).
        let revoked = revoke_ace(&self.path, &self.sid);
        finish_change(&key, |_| {});
        revoked
    }
}

/// Signalled when a slow permission change on some path has finished.
static SHARED_IDLE: std::sync::Condvar = std::sync::Condvar::new();

type SharedGuard =
    std::sync::MutexGuard<'static, Option<std::collections::HashMap<SharedKey, Shared>>>;

/// The table, locked once no slow permission change to `key` is under way
/// (another sandbox granting or revoking the same folder).
fn shared_idle(key: &SharedKey) -> SharedGuard {
    let mut guard = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    while guard
        .as_ref()
        .and_then(|m| m.get(key))
        .is_some_and(|e| e.busy)
    {
        guard = SHARED_IDLE.wait(guard).unwrap_or_else(|e| e.into_inner());
    }
    guard
}

/// End a slow change to `key` begun by setting `busy`: `update` records its
/// outcome, an entry nothing holds any more is dropped, and anyone waiting
/// for the path goes on.
fn finish_change(key: &SharedKey, update: impl FnOnce(&mut Shared)) {
    with_shared(|shared| {
        if let Some(entry) = shared.get_mut(key) {
            entry.busy = false;
            update(entry);
            if entry.unused() {
                shared.remove(key);
            }
        }
    });
    SHARED_IDLE.notify_all();
}

fn revoke_ace(path: &Path, sid: &OwnedSid) -> bool {
    match set_ace(path, sid, REVOKE_ACCESS, 0, NO_INHERITANCE) {
        Ok(()) => true,
        // Nothing left to revoke on a path that no longer exists.
        Err(SandboxError::Os { code, .. }) => {
            code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND
        }
        Err(_) => false,
    }
}

/// Suffix of grant-journal files.
const JOURNAL_EXTENSION: &str = "grants";
/// Journals larger than this are not parsed (a real one is a few KiB).
const MAX_JOURNAL_BYTES: u64 = 1 << 20;
/// Only identities Gardenwall creates may be named in a journal.
const JOURNAL_PROFILE_PREFIX: &str = "Gardenwall.";
const ERROR_SHARING_VIOLATION_CODE: i32 = 32;

/// On-disk record of the ACEs a live sandbox has added.
///
/// The file is held open without sharing for the sandbox's lifetime, so
/// [`reconcile_grant_journals`] can tell a live sandbox's journal (cannot be
/// opened) from one left by a crash. Each record is written and synced
/// before the ACE it describes is added.
struct GrantJournal {
    file: File,
    path: PathBuf,
}

impl GrantJournal {
    fn create(dir: &Path) -> Result<Self, SandboxError> {
        std::fs::create_dir_all(dir).map_err(|e| SandboxError::Io(dir.to_path_buf(), e.kind()))?;
        let mut name = [0u8; 16];
        getrandom::fill(&mut name).map_err(|_| SandboxError::Os {
            op: "getrandom",
            code: 0,
        })?;
        let hex: String = name.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("{hex}.{JOURNAL_EXTENSION}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .map_err(|e| SandboxError::Io(path.clone(), e.kind()))?;
        Ok(Self { file, path })
    }

    fn record(&mut self, profile: &str, path: &Path) -> Result<(), SandboxError> {
        self.write_line(&format!("{profile} {}\n", encode_path(path)))
    }

    /// A path made read-only, with the DACL to put back.
    fn record_restore(
        &mut self,
        profile: &str,
        path: &Path,
        sddl: &str,
    ) -> Result<(), SandboxError> {
        self.write_line(&format!(
            "restore {profile} {} {}\n",
            encode_path(path),
            encode_path(Path::new(sddl))
        ))
    }

    fn write_line(&mut self, line: &str) -> Result<(), SandboxError> {
        use std::io::Write;
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|e| SandboxError::Io(self.path.clone(), e.kind()))
    }

    /// Close and delete the journal (all its grants were revoked).
    fn remove(self) {
        let Self { file, path } = self;
        drop(file);
        let _ = std::fs::remove_file(path);
    }
}

/// Paths are stored as hex UTF-16 so any Windows path round-trips exactly.
fn encode_path(path: &Path) -> String {
    path.as_os_str()
        .encode_wide()
        .map(|u| format!("{u:04x}"))
        .collect()
}

/// A saved SDDL string (stored like a path, but relative).
fn decode_sddl(hex: &str) -> Option<String> {
    if hex.is_empty() || !hex.len().is_multiple_of(4) || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let units: Option<Vec<u16>> = hex
        .as_bytes()
        .chunks(4)
        .map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect();
    let s = String::from_utf16(&units?).ok()?;
    s.starts_with("D:").then_some(s)
}

fn decode_path(hex: &str) -> Option<PathBuf> {
    if hex.is_empty() || !hex.len().is_multiple_of(4) || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let units: Option<Vec<u16>> = hex
        .as_bytes()
        .chunks(4)
        .map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect();
    let path = PathBuf::from(std::ffi::OsString::from_wide(&units?));
    path.is_absolute().then_some(path)
}

/// What [`reconcile_grant_journals`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Journals left by sandboxes that are no longer running.
    pub stale_journals: usize,
    /// Grants revoked from those journals.
    pub revoked: usize,
    /// Journals kept because a revocation failed (retried next time).
    pub kept: usize,
    /// Journals skipped because their sandbox is still running.
    pub in_use: usize,
    /// Malformed entries ignored.
    pub ignored: usize,
    /// Grants left in place and recorded as kept, because the app keeps
    /// them anyway ([`reconcile_grant_journals_keeping`]).
    pub kept_in_place: usize,
}

/// Revoke grants recorded by sandboxes that did not shut down cleanly
/// (a crash, power loss, or a killed process).
///
/// Call at startup with the directory passed as
/// [`SandboxPolicy::grant_journal_dir`]. Journals of running sandboxes are
/// left alone. Only ACEs for `Gardenwall.*` container identities are ever
/// removed, so a tampered journal cannot revoke other applications' grants.
pub fn reconcile_grant_journals(dir: &Path) -> Result<ReconcileReport, SandboxError> {
    reconcile_grant_journals_keeping(dir, None, &|_, _| None)
}

/// [`reconcile_grant_journals`], except that a plain grant `keep` asks for
/// (by profile and path) is kept for this run instead of revoked: written
/// to `kept_dir` as a kept grant (see [`GrantHold::kept`]), when the
/// folder's own entry for the container gives exactly that access. Used at
/// start-up for the toolchain folders the app keeps granted anyway, so a
/// Rust toolchain isn't revoked and granted again (two passes over 70,000
/// files). Sandboxes and grant holds wait until it has finished.
pub fn reconcile_grant_journals_keeping(
    dir: &Path,
    kept_dir: Option<&Path>,
    keep: &dyn Fn(&str, &Path) -> Option<Access>,
) -> Result<ReconcileReport, SandboxError> {
    let _cleanup = CLEANUP.write().unwrap_or_else(|e| e.into_inner());
    let mut report = ReconcileReport::default();
    // One kept record per container and folder, however many journals
    // name it.
    let mut kept_already: std::collections::HashSet<(String, PathBuf)> = kept_dir
        .map(kept_grants)
        .unwrap_or_default()
        .into_iter()
        .map(|k| (k.profile, k.path))
        .collect();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(SandboxError::Io(dir.to_path_buf(), e.kind())),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(JOURNAL_EXTENSION) {
            continue;
        }
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION_CODE) => {
                report.in_use += 1;
                continue;
            }
            Err(_) => continue,
        };
        report.stale_journals += 1;
        let mut text = String::new();
        let readable = {
            use std::io::Read;
            file.metadata().is_ok_and(|m| m.len() <= MAX_JOURNAL_BYTES)
                && (&file).read_to_string(&mut text).is_ok()
        };
        let mut all_revoked = readable;
        // Read-only paths first: their permissions go back to what they
        // were, and only where an entry Gardenwall set is still present.
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("restore ") else {
                continue;
            };
            let mut parts = rest.split(' ');
            let parsed = (|| {
                let profile = parts.next()?;
                let ok = profile.starts_with(JOURNAL_PROFILE_PREFIX)
                    && crate::policy::valid_profile_name(profile);
                let target = decode_path(parts.next()?)?;
                let sddl = decode_sddl(parts.next()?)?;
                Some((ok.then_some(profile)?, target, sddl))
            })();
            let Some((profile, target, sddl)) = parsed else {
                report.ignored += 1;
                continue;
            };
            match derive_sid(profile) {
                Ok(sid) if !dacl_names(&target, &sid) => {}
                Ok(_) if restore_dacl(&target, &sddl) => report.revoked += 1,
                _ => all_revoked = false,
            }
        }
        // "Folders above a project" entries (ADR-025): only this module's
        // own entry is taken off the folder.
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("list ") else {
                continue;
            };
            let parsed = rest.split_once(' ').and_then(|(profile, hex)| {
                let ok = profile.starts_with(JOURNAL_PROFILE_PREFIX)
                    && crate::policy::valid_profile_name(profile);
                Some((ok.then_some(profile)?, decode_path(hex)?))
            });
            let Some((profile, target)) = parsed else {
                report.ignored += 1;
                continue;
            };
            match derive_sid(profile) {
                Ok(sid) if remove_list_ace(&target, &sid) => report.revoked += 1,
                _ => all_revoked = false,
            }
        }
        for line in text
            .lines()
            .filter(|l| !l.starts_with("restore ") && !l.starts_with("list "))
        {
            let parsed = line.split_once(' ').and_then(|(profile, hex)| {
                let ok = profile.starts_with(JOURNAL_PROFILE_PREFIX)
                    && crate::policy::valid_profile_name(profile);
                Some((ok.then_some(profile)?, decode_path(hex)?))
            });
            let Some((profile, target)) = parsed else {
                report.ignored += 1;
                continue;
            };
            let Ok(sid) = derive_sid(profile) else {
                all_revoked = false;
                continue;
            };
            // Wanted again this run: kept in place when the entry is
            // exactly what is wanted (never wider), recorded as kept.
            if let (Some(kept_dir), Some(access)) = (kept_dir, keep(profile, &target))
                && explicit_allow_mask(&target, &sid) == Some(access_mask(access))
            {
                let key = (profile.to_string(), target.clone());
                if kept_already.contains(&key)
                    // Pending: a journal can't tell whether the grant had
                    // reached every file when the process ended, so the
                    // next `adopt` grants it again (in the background,
                    // after this cleanup) instead of trusting it.
                    || KeptRecord::create(kept_dir, profile, &target, access, true).is_ok()
                {
                    kept_already.insert(key);
                    report.kept_in_place += 1;
                    continue;
                }
            }
            if revoke_ace(&target, &sid) {
                report.revoked += 1;
            } else {
                all_revoked = false;
            }
        }
        drop(file);
        if all_revoked {
            let _ = std::fs::remove_file(&path);
        } else {
            report.kept += 1;
        }
    }
    Ok(report)
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a freshly created, valid handle that nothing else
    // owns; OwnedHandle takes over closing it.
    unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) }
}

/// A job that ends every process in it when its last handle closes, which
/// Windows also does when the owning process is killed or crashes. For
/// helpers started outside a sandbox (a development tool's model server),
/// so they never outlive their owner and keep holding GPU memory. It
/// restricts nothing else.
pub struct KillOnCloseJob(OwnedHandle);

impl std::fmt::Debug for KillOnCloseJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KillOnCloseJob")
    }
}

impl KillOnCloseJob {
    pub fn new() -> Result<Self, SandboxError> {
        // SAFETY: null attributes and name create an unnamed job object.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(last_error("CreateJobObjectW"));
        }
        let job = owned(job);
        // SAFETY: zeroed is a valid initial value for this plain-data struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `job` is valid; the buffer and its size match the info class.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(last_error("SetInformationJobObject(kill on close)"));
        }
        Ok(Self(job))
    }

    /// Put the running process `pid` (a child the caller started and still
    /// holds, so the id can't have been reused) in the job.
    pub fn assign(&self, pid: u32) -> Result<(), SandboxError> {
        // SAFETY: plain call; a failure returns null.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(last_error("OpenProcess"));
        }
        let process = owned(process);
        // SAFETY: both handles are valid and owned for the call.
        if unsafe {
            AssignProcessToJobObject(
                self.0.as_raw_handle() as HANDLE,
                process.as_raw_handle() as HANDLE,
            )
        } == 0
        {
            return Err(last_error("AssignProcessToJobObject"));
        }
        Ok(())
    }
}

fn create_job(policy: &SandboxPolicy) -> Result<OwnedHandle, SandboxError> {
    // SAFETY: null attributes and name create an unnamed job object.
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(last_error("CreateJobObjectW"));
    }
    let job = owned(job);
    // SAFETY: zeroed is a valid initial value for this plain-data struct.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    limits.BasicLimitInformation.ActiveProcessLimit = policy.limits.max_processes;
    if let Some(bytes) = policy.limits.memory_bytes {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        limits.JobMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
    }
    // SAFETY: `job` is valid; the buffer and its size match the info class.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetInformationJobObject(limits)"));
    }
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    // SAFETY: as above.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectBasicUIRestrictions,
            (&raw const ui).cast(),
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetInformationJobObject(ui)"));
    }
    Ok(job)
}

/// An anonymous pipe: `(parent_end, child_end)`. The child end is
/// inheritable; the parent end is not.
fn pipe(child_reads: bool) -> Result<(OwnedHandle, OwnedHandle), SandboxError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: out-pointers are valid; `sa` outlives the call.
    if unsafe { CreatePipe(&mut read, &mut write, &sa, 0) } == 0 {
        return Err(last_error("CreatePipe"));
    }
    let (read, write) = (owned(read), owned(write));
    let (parent, child) = if child_reads {
        (write, read)
    } else {
        (read, write)
    };
    // SAFETY: `parent` is a valid handle we own.
    if unsafe { SetHandleInformation(parent.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) }
        == 0
    {
        return Err(last_error("SetHandleInformation"));
    }
    Ok((parent, child))
}

/// Owns a `PROC_THREAD_ATTRIBUTE_LIST` buffer and deletes it on drop.
struct AttributeList {
    buf: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> Result<Self, SandboxError> {
        let mut size = 0usize;
        // SAFETY: a null list with a size out-pointer queries the needed size;
        // this call is documented to fail with ERROR_INSUFFICIENT_BUFFER.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        // usize-aligned backing storage of at least `size` bytes.
        let mut buf = vec![0usize; size.div_ceil(size_of::<usize>())];
        // SAFETY: `buf` provides `size` writable, suitably aligned bytes.
        if unsafe {
            InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }
        Ok(Self { buf })
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must point to `size` bytes that stay valid and unmoved until
    /// the process has been created and this list is dropped.
    unsafe fn set(
        &mut self,
        attribute: usize,
        value: *const c_void,
        size: usize,
    ) -> Result<(), SandboxError> {
        // SAFETY: the list is initialized; validity of `value` is the
        // caller's obligation.
        if unsafe {
            UpdateProcThreadAttribute(self.as_ptr(), 0, attribute, value, size, null_mut(), null())
        } == 0
        {
            return Err(last_error("UpdateProcThreadAttribute"));
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was successfully initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

/// A sandbox instance: one AppContainer identity, its ACL grants, and a Job
/// Object. Dropping it terminates every process in the job and revokes the
/// grants.
pub struct Sandbox {
    policy: SandboxPolicy,
    sid: OwnedSid,
    /// Always `Some` until drop, which closes it (killing the processes)
    /// before grants are revoked.
    job: Option<OwnedHandle>,
    grants: Vec<AclGrant>,
    /// Paths made read-only inside a grant, with their original DACLs.
    saved: Vec<SavedDacl>,
    journal: Option<GrantJournal>,
    /// Start processes with a hidden console instead of detached.
    hidden_console: bool,
    /// Private drive letters for the granted folders, if set up.
    device_map: Option<DeviceMap>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        drop(self.job.take());
        let mut all_revoked = true;
        for saved in self.saved.iter().rev() {
            // Restored only by the last open sandbox holding the change.
            let last = with_shared(|shared| {
                let key = (self.sid.0.clone(), saved.path.clone());
                let Some(entry) = shared.get_mut(&key) else {
                    return true;
                };
                let last = match &mut entry.dacl {
                    Some((_, holders)) if *holders > 1 => {
                        *holders -= 1;
                        false
                    }
                    _ => {
                        entry.dacl = None;
                        true
                    }
                };
                if entry.unused() {
                    shared.remove(&key);
                }
                last
            });
            if last {
                all_revoked &= restore_dacl(&saved.path, &saved.sddl);
            }
        }
        for grant in &self.grants {
            all_revoked &= grant.revoke();
        }
        // A journal whose grants could not all be revoked stays on disk for
        // the next reconciliation.
        if let Some(journal) = self.journal.take()
            && all_revoked
        {
            journal.remove();
        }
    }
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sandbox")
            .field("profile", &self.policy.profile)
            .finish_non_exhaustive()
    }
}

/// A process running inside a [`Sandbox`].
pub struct SandboxedProcess {
    process: OwnedHandle,
    pid: u32,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}

impl std::fmt::Debug for SandboxedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedProcess")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl Sandbox {
    pub fn new(policy: SandboxPolicy) -> Result<Self, SandboxError> {
        // Waits while start-up cleanup runs (see CLEANUP).
        let _cleanup = cleanup_done();
        policy.validate_shape()?;
        let mut canonical = policy.clone();
        let sid = app_container_sid(&canonical.profile)?;
        for grant in &mut canonical.grants {
            if !grant.path.exists() {
                return Err(PolicyError::MissingPath(grant.path.clone()).into());
            }
            // Held already (by the app or another open sandbox): it was
            // checked when first granted, and the grant tree check is the
            // slow part for a large toolchain folder.
            if let Ok(c) = std::fs::canonicalize(&grant.path)
                && already_held(&sid, &c, grant.access)
            {
                grant.path = c;
                continue;
            }
            // The link check exists because a permission set on one name of
            // a hard-linked file applies to every name. A read-only folder
            // every AppContainer can already read gets no permission (see
            // below), so nothing can be widened through its links: Git for
            // Windows, whose programs are hard links, is such a folder
            // (ADR-011 §2.5). Every folder that gets a permission is checked.
            if !needs_no_grant(grant) {
                check_grant_tree(&grant.path, grant.access)?;
            }
            grant.path = std::fs::canonicalize(&grant.path)
                .map_err(|e| SandboxError::Io(grant.path.clone(), e.kind()))?;
        }
        // The working folder by its long-name path, like the grants. Given
        // with 8.3 short names (`C:\Users\ABCDEF~1\...`, as `%TEMP%` often
        // is), Windows PowerShell 5.1 expands them, which needs list access
        // to parent folders the sandbox doesn't have: every command then
        // failed with "Access to the path is denied".
        if let Some(dir) = &canonical.working_dir {
            let long =
                std::fs::canonicalize(dir).map_err(|e| SandboxError::Io(dir.clone(), e.kind()))?;
            canonical.working_dir = Some(process_path(&long)?);
        }
        let job = create_job(&canonical)?;
        let journal = match &canonical.grant_journal_dir {
            Some(dir) => Some(GrantJournal::create(dir)?),
            None => None,
        };
        let grant_specs = canonical.grants.clone();
        // From here on, an early return drops `sandbox`, which revokes the
        // grants added so far.
        let mut sandbox = Self {
            policy: canonical,
            sid,
            job: Some(job),
            grants: Vec::with_capacity(grant_specs.len()),
            saved: Vec::new(),
            journal,
            hidden_console: false,
            device_map: None,
        };
        for grant in &grant_specs {
            // Folders every AppContainer can already read need no grant
            // (and, like Program Files, usually refuse ACL changes).
            if needs_no_grant(grant) {
                continue;
            }
            if let Some(journal) = &mut sandbox.journal {
                journal.record(&sandbox.policy.profile, &grant.path)?;
            }
            let added = AclGrant::new(&grant.path, &sandbox.sid, grant.access)?;
            sandbox.grants.push(added);
        }
        Ok(sandbox)
    }

    /// Give processes started from now on a hidden console of their own
    /// (for shells: console programs they launch then share it and their
    /// redirected stdio, rather than opening visible windows that read the
    /// keyboard).
    pub fn use_hidden_console(&mut self) {
        self.hidden_console = true;
    }

    fn job_handle(&self) -> HANDLE {
        self.job
            .as_ref()
            .map_or(null_mut(), |job| job.as_raw_handle() as HANDLE)
    }

    /// Give processes started from now on private drive letters for
    /// granted folders (e.g. `W:` for the project), seen by nothing outside
    /// this sandbox. Each folder must be granted read or read/write. Start
    /// processes in the folder's real path; they then switch to the letter.
    /// Passing no drives removes the mapping.
    pub fn map_drives(&mut self, drives: &[(char, PathBuf)]) -> Result<(), SandboxError> {
        if drives.is_empty() {
            self.device_map = None;
            return Ok(());
        }
        let mut resolved = Vec::with_capacity(drives.len());
        for (letter, folder) in drives {
            if !letter.is_ascii_alphabetic() {
                return Err(SandboxError::Os {
                    op: "drive mapping: invalid letter",
                    code: 87,
                });
            }
            let canonical = std::fs::canonicalize(folder)
                .map_err(|e| SandboxError::Io(folder.clone(), e.kind()))?;
            let granted = self.policy.grants.iter().any(|g| {
                g.path == canonical && matches!(g.access, Access::Read | Access::ReadWrite)
            });
            if !granted {
                return Err(PolicyError::WorkingDirNotGranted.into());
            }
            resolved.push((*letter, canonical));
        }
        // The letter points at the checked, long-name path. A path given
        // with 8.3 short names (`C:\Users\ABCDEF~1\...`, as `%TEMP%` often
        // is) would otherwise be the link's target, and Windows PowerShell
        // 5.1 expands such names when it adds the drive, which needs list
        // access to parent folders the sandbox (rightly) doesn't have.
        self.device_map = Some(DeviceMap::new(&resolved)?);
        Ok(())
    }

    /// The container's SID string (for diagnostics).
    pub fn sid_string(&self) -> Result<String, SandboxError> {
        self.sid.to_string_sid()
    }

    /// Start `program` with `args` inside the sandbox.
    pub fn spawn(&self, program: &Path, args: &[&OsStr]) -> Result<SandboxedProcess, SandboxError> {
        if !program.is_absolute() || !program.is_file() {
            return Err(PolicyError::MissingPath(program.to_path_buf()).into());
        }
        let program = process_path(program)?;
        let line = argv::command_line(program.as_os_str(), args)?;
        self.spawn_line(&program, line)
    }

    /// Start `program` with `raw_tail` appended verbatim to its quoted path.
    ///
    /// For programs that do not parse arguments the standard way, chiefly
    /// `cmd.exe /d /s /c "<command>"`. The tail may not contain NUL or line
    /// breaks. The sandbox applies exactly as for [`Sandbox::spawn`].
    pub fn spawn_raw(
        &self,
        program: &Path,
        raw_tail: &str,
    ) -> Result<SandboxedProcess, SandboxError> {
        if !program.is_absolute() || !program.is_file() {
            return Err(PolicyError::MissingPath(program.to_path_buf()).into());
        }
        if raw_tail
            .chars()
            .any(|c| matches!(c, NUL_CHAR | CR_CHAR | LF_CHAR))
        {
            return Err(argv::ArgvError::Nul.into());
        }
        let program = process_path(program)?;
        let line = argv::command_line(program.as_os_str(), &[])?;
        self.spawn_line(&program, format!("{line} {raw_tail}"))
    }

    fn spawn_line(&self, program: &Path, line: String) -> Result<SandboxedProcess, SandboxError> {
        let mut line_w: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
        let program_w = wide(program.as_os_str());
        let mut env: Vec<(String, String)> = self
            .policy
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        // CreateProcessW fails with ERROR_ENVVAR_NOT_FOUND for an AppContainer
        // unless LOCALAPPDATA is present; Windows then replaces it with the
        // container's private profile folder, so the user's real folder is
        // not exposed (verified by the escape suite).
        if !env
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("LOCALAPPDATA"))
            && let Some(v) = std::env::var_os("LOCALAPPDATA")
        {
            env.push(("LOCALAPPDATA".into(), v.to_string_lossy().into_owned()));
        }
        let env_block = argv::environment_block(&env);
        let cwd_w = self
            .policy
            .working_dir
            .as_ref()
            .map(|d| wide(d.as_os_str()));

        let (stdin_parent, stdin_child) = pipe(true)?;
        let (stdout_parent, stdout_child) = pipe(false)?;
        let (stderr_parent, stderr_child) = pipe(false)?;
        let inherit: [HANDLE; 3] = [
            stdin_child.as_raw_handle() as HANDLE,
            stdout_child.as_raw_handle() as HANDLE,
            stderr_child.as_raw_handle() as HANDLE,
        ];

        let mut caps = SECURITY_CAPABILITIES {
            AppContainerSid: self.sid.as_psid(),
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let child_policy: u32 = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
        let restrict_children = self.policy.limits.max_processes == 1;
        let mut attrs = AttributeList::new(if restrict_children { 3 } else { 2 })?;
        // SAFETY: `caps`, `inherit`, and `child_policy` live on this stack
        // frame until after CreateProcessW returns and `attrs` is dropped
        // (declared after them, so dropped first).
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&raw mut caps).cast(),
                size_of::<SECURITY_CAPABILITIES>(),
            )?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherit.as_ptr().cast(),
                size_of_val(&inherit),
            )?;
            if restrict_children {
                attrs.set(
                    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
                    (&raw const child_policy).cast(),
                    size_of::<u32>(),
                )?;
            }
        }

        // SAFETY: zeroed is a valid initial value for STARTUPINFOEXW.
        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        let console_flag = if self.hidden_console {
            // A console of its own, never shown: console programs the
            // process starts share it and inherit the redirected stdio,
            // instead of each getting a new visible window.
            si.StartupInfo.dwFlags |= STARTF_USESHOWWINDOW;
            si.StartupInfo.wShowWindow = 0; // SW_HIDE
            CREATE_NEW_CONSOLE
        } else {
            DETACHED_PROCESS
        };
        si.StartupInfo.hStdInput = inherit[0];
        si.StartupInfo.hStdOutput = inherit[1];
        si.StartupInfo.hStdError = inherit[2];
        si.lpAttributeList = attrs.as_ptr();
        // SAFETY: zeroed is a valid out-buffer for PROCESS_INFORMATION.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: all string buffers are NUL-terminated and outlive the call;
        // `line_w` is mutable as required; `env_block` is a valid Unicode
        // environment block; `si` and its attribute list are initialized.
        let ok = unsafe {
            CreateProcessW(
                program_w.as_ptr(),
                line_w.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | console_flag,
                env_block.as_ptr().cast(),
                cwd_w.as_ref().map_or(null(), |c| c.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(last_error("CreateProcessW"));
        }
        let process = owned(pi.hProcess);
        let thread = owned(pi.hThread);
        // A process that just exited can still count against the job's
        // active-process limit for a moment (seen on slower machines), which
        // fails assignment with ERROR_NOT_ENOUGH_QUOTA; retry briefly. The new
        // process stays suspended meanwhile, so nothing untrusted runs.
        let mut assigned = false;
        for attempt in 0..40u64 {
            // SAFETY: both handles are valid; the process is still suspended.
            if unsafe {
                AssignProcessToJobObject(self.job_handle(), process.as_raw_handle() as HANDLE)
            } != 0
            {
                assigned = true;
                break;
            }
            // SAFETY: trivially safe.
            if unsafe { GetLastError() } != ERROR_NOT_ENOUGH_QUOTA {
                break;
            }
            std::thread::sleep(Duration::from_millis(5 * (attempt + 1)));
        }
        if !assigned {
            let err = last_error("AssignProcessToJobObject");
            // SAFETY: valid process handle; terminating a suspended process.
            unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
            return Err(err);
        }
        // Private drive letters, before any of the process's code runs.
        if let Some(map) = &self.device_map
            && let Err(e) = map.apply(process.as_raw_handle() as HANDLE)
        {
            // SAFETY: valid process handle; terminating a suspended process.
            unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
            return Err(e);
        }
        // SAFETY: valid thread handle of the suspended primary thread.
        if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
            let err = last_error("ResumeThread");
            // SAFETY: valid process handle.
            unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
            return Err(err);
        }
        drop(thread);
        // The child now holds its own copies of the stdio ends.
        drop((stdin_child, stdout_child, stderr_child));

        Ok(SandboxedProcess {
            process,
            pid: pi.dwProcessId,
            stdin: Some(File::from(stdin_parent)),
            stdout: Some(File::from(stdout_parent)),
            stderr: Some(File::from(stderr_parent)),
        })
    }

    /// Make `path`, inside a read/write grant, read-only for the sandbox
    /// while it lives (ADR-012 §3.3, "read-only within a grant").
    ///
    /// An AppContainer may do only what an allow entry for its own identity
    /// grants (deny entries for it are not consulted, and integrity labels
    /// don't stop edits), so the sandbox's inherited write access is taken
    /// away on this path: its inheritance is turned off, every other entry
    /// is kept (copied as explicit), and the sandbox gets read and run
    /// only. It can then neither change, add, delete nor rename anything
    /// there (renaming needs "delete" on the path, or "delete child" on the
    /// parent, which a grant never gives). The original permissions are
    /// journaled first and restored exactly with the grants, also after a
    /// crash. Returns `true`: the OS enforces it (Linux returns `false`).
    pub fn protect(&mut self, path: &Path) -> Result<bool, SandboxError> {
        self.replace_access(path, Some(FILE_GENERIC_READ | FILE_GENERIC_EXECUTE))
    }

    /// Take away all of the sandbox's access to `path`, inside any grant,
    /// while it lives: a secret file (ADR-011 §2.4) that commands
    /// must not read, change, delete or rename. Done like [`Self::protect`]
    /// (inheritance off, every other entry kept, the original permissions
    /// journaled and restored), but the sandbox gets no entry at all.
    ///
    /// Calling it again for a path that is still hidden changes nothing, so
    /// it can be repeated before every command. A file that was replaced
    /// since (an editor saving a new copy inherits the folder's permissions
    /// again) is hidden anew. Returns `true`: the OS enforces it (Linux
    /// returns `false`).
    pub fn hide(&mut self, path: &Path) -> Result<bool, SandboxError> {
        let known = std::fs::canonicalize(path)
            .ok()
            .is_some_and(|c| self.saved.iter().any(|s| s.path == c));
        // `D:P…`: the permissions still don't inherit, so they are the ones
        // set here.
        if known && dacl_sddl(path).is_ok_and(|sddl| sddl.starts_with("D:P")) {
            return Ok(true);
        }
        self.replace_access(path, None)
    }

    fn replace_access(&mut self, path: &Path, access: Option<u32>) -> Result<bool, SandboxError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| SandboxError::Io(path.to_path_buf(), e.kind()))?;
        // Read-only makes sense only inside a read/write grant; hiding
        // applies inside any grant.
        let inside = self.policy.grants.iter().any(|g| {
            (access.is_none() || g.access == Access::ReadWrite)
                && canonical.starts_with(&g.path)
                && canonical != g.path
        });
        if !inside {
            return Err(PolicyError::WorkingDirNotGranted.into());
        }
        let (_, reparse) = link_info(&canonical)?;
        if reparse {
            return Err(PolicyError::ReparsePoint(canonical).into());
        }
        // This sandbox already holds a change here (a secret file hidden
        // again after an editor replaced it): apply it again, without
        // recording or counting it twice.
        if self.saved.iter().any(|s| s.path == canonical) {
            replace_access(&canonical, &self.sid, canonical.is_dir(), access)?;
            return Ok(true);
        }
        let key = (self.sid.0.clone(), canonical.clone());
        // The permissions to restore are the ones before the first change by
        // any open sandbox, never another sandbox's read-only state.
        let sddl = with_shared(|shared| -> Result<String, SandboxError> {
            let entry = shared.entry(key).or_default();
            Ok(match &mut entry.dacl {
                Some((original, holders)) => {
                    *holders += 1;
                    original.clone()
                }
                None => {
                    let original = dacl_sddl(&canonical)?;
                    entry.dacl = Some((original.clone(), 1));
                    original
                }
            })
        })?;
        if let Some(journal) = &mut self.journal {
            journal.record_restore(&self.policy.profile, &canonical, &sddl)?;
        }
        // Recorded before the change, so a failure part-way is undone too.
        self.saved.push(SavedDacl {
            path: canonical.clone(),
            sddl,
        });
        replace_access(&canonical, &self.sid, canonical.is_dir(), access)?;
        Ok(true)
    }

    /// Processes running in the sandbox now: id and program file name.
    /// The console host Windows starts for a hidden console is left out.
    pub fn processes(&self) -> Vec<(u32, String)> {
        #[repr(C)]
        struct IdList {
            assigned: u32,
            in_list: u32,
            ids: [usize; 256],
        }
        // SAFETY: zeroed is a valid value for this plain-data struct.
        let mut list: IdList = unsafe { std::mem::zeroed() };
        // SAFETY: valid job handle; the buffer and its size match.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job_handle(),
                JobObjectBasicProcessIdList,
                (&raw mut list).cast(),
                size_of::<IdList>() as u32,
                null_mut(),
            )
        } != 0;
        if !ok {
            return Vec::new();
        }
        let conhost = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("conhost.exe"));
        let count = (list.in_list as usize).min(list.ids.len());
        list.ids[..count]
            .iter()
            .filter_map(|&id| {
                let pid = u32::try_from(id).ok()?;
                let image = process_image(pid)?;
                if conhost
                    .as_deref()
                    .is_some_and(|c| image.as_os_str().eq_ignore_ascii_case(c.as_os_str()))
                {
                    return None;
                }
                let name = image.file_name()?.to_string_lossy().into_owned();
                Some((pid, name))
            })
            .collect()
    }

    /// End one process, only if it runs in this sandbox.
    pub fn terminate_process(&self, pid: u32) -> bool {
        if !self.processes().iter().any(|(p, _)| *p == pid) {
            return false;
        }
        // SAFETY: plain call; a failure returns null.
        let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return false;
        }
        let process = owned(process);
        // SAFETY: valid handle opened with PROCESS_TERMINATE.
        unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) != 0 }
    }

    /// Terminate every process in the sandbox.
    pub fn terminate_all(&self) -> Result<(), SandboxError> {
        // SAFETY: valid job handle.
        if unsafe { TerminateJobObject(self.job_handle(), 1) } == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }
}

impl SandboxedProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait for exit. Returns `Ok(None)` on timeout.
    pub fn wait_timeout(&self, timeout: Option<Duration>) -> Result<Option<u32>, SandboxError> {
        let ms = timeout.map_or(INFINITE, |d| {
            u32::try_from(d.as_millis()).unwrap_or(INFINITE - 1)
        });
        // SAFETY: valid process handle.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, ms) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: valid process handle; `code` is a valid out-pointer.
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &mut code) }
                    == 0
                {
                    return Err(last_error("GetExitCodeProcess"));
                }
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(last_error("WaitForSingleObject")),
        }
    }

    pub fn kill(&self) -> Result<(), SandboxError> {
        // SAFETY: valid process handle.
        if unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) } == 0 {
            return Err(last_error("TerminateProcess"));
        }
        Ok(())
    }
}

// ---- Folders above a project (ADR-025) ------------------------------------

/// What a folder above a project gets when the user turns on "Folders above
/// this project" (ADR-025): list the names in it, read its attributes,
/// synchronize. For that folder only (nothing inside inherits it), so no
/// file in it can be opened and nothing deeper can be listed.
pub const PARENT_LIST_MASK: u32 = 0x0010_0081;

/// Whether `profile`'s sandbox can already list `path` with
/// [`PARENT_LIST_MASK`] (a hold of this process, or the administrator step
/// for folders this user can't change). Reading the folder's permissions
/// changes nothing.
pub fn parent_listable(profile: &str, path: &Path) -> bool {
    derive_sid(profile).is_ok_and(|sid| has_list_ace(path, &sid))
}

/// Whether the current user owns `path` (its owner is this user's SID).
/// "Folders above the project" changes only folders the user owns itself;
/// Windows' own folders (owned by SYSTEM or TrustedInstaller) go through
/// the administrator step even where this user could change them.
pub fn owned_by_current_user(path: &Path) -> bool {
    let Ok(me) = current_user_sid() else {
        return false;
    };
    let wpath = wide(path.as_os_str());
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wpath` is NUL-terminated; `owner` points into `sd`, which is
    // LocalAlloc'd by the API and freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS || owner.is_null() {
        if !sd.is_null() {
            // SAFETY: allocated by GetNamedSecurityInfoW.
            unsafe { LocalFree(sd) };
        }
        return false;
    }
    // SAFETY: `owner` is a valid SID inside `sd`, copied before `sd` is freed.
    let owner = unsafe { OwnedSid::copy_from(owner) };
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    owner.and_then(|o| o.to_string_sid()).is_ok_and(|o| o == me)
}

/// The container SID of `profile` as a string (`S-1-15-2-…`), for the
/// administrator step's script.
pub fn profile_sid(profile: &str) -> Option<String> {
    derive_sid(profile).ok()?.to_string_sid().ok()
}

/// The folder's DACL entries split into explicit and inherited ones, its
/// revision and its control bits.
fn dacl_parts(path: &Path) -> Result<(Vec<u8>, Vec<u8>, u8, u16), SandboxError> {
    let wpath = wide(path.as_os_str());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: as in `set_ace`; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(SandboxError::Os {
            op: "GetNamedSecurityInfoW",
            code: rc,
        });
    }
    let explicit = rebuild_aces(dacl, |ace| ace[1] & ACE_INHERITED_FLAG == 0);
    let inherited = rebuild_aces(dacl, |ace| ace[1] & ACE_INHERITED_FLAG != 0);
    let revision = if dacl.is_null() {
        2
    } else {
        // SAFETY: `dacl` is non-null and valid until `sd` is freed.
        unsafe { (*dacl).AclRevision }
    };
    let mut control: u16 = 0;
    let mut rev: u32 = 0;
    // SAFETY: `sd` is valid; out-pointers are valid.
    let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut rev) } != 0;
    // SAFETY: allocated by GetNamedSecurityInfoW, not used again.
    unsafe { LocalFree(sd) };
    if !ok {
        return Err(last_error("GetSecurityDescriptorControl"));
    }
    Ok((explicit, inherited, revision, control))
}

/// A raw ACE that is this module's entry: allowed, for `sid`, for this
/// folder only (no inheritance flags, not inherited).
fn is_list_ace(ace: &[u8], sid: &OwnedSid) -> bool {
    const ANY_INHERIT: u8 = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE) as u8;
    ace[0] == 0 && ace[1] & (ANY_INHERIT | ACE_INHERITED_FLAG) == 0 && ace_names(ace, sid)
}

fn ace_mask(ace: &[u8]) -> u32 {
    u32::from_le_bytes([ace[4], ace[5], ace[6], ace[7]])
}

fn has_list_ace(path: &Path, sid: &OwnedSid) -> bool {
    let Ok((explicit, _, _, _)) = dacl_parts(path) else {
        return false;
    };
    let mut found = false;
    let mut i = 0;
    while i + 4 <= explicit.len() {
        let size = u16::from_le_bytes([explicit[i + 2], explicit[i + 3]]) as usize;
        if size < 8 || i + size > explicit.len() {
            break;
        }
        let ace = &explicit[i..i + size];
        found |= is_list_ace(ace, sid) && ace_mask(ace) & PARENT_LIST_MASK == PARENT_LIST_MASK;
        i += size;
    }
    found
}

/// Write a DACL to `path` itself with `SetFileSecurityW`, which (unlike
/// `SetNamedSecurityInfoW`) changes nothing below the folder: explicit
/// entries, then `extra`, then the inherited ones, with the folder's own
/// protected/auto-inherited control bits kept.
fn write_dacl_here(
    path: &Path,
    revision: u8,
    explicit: &[u8],
    extra: Option<(&OwnedSid, u32)>,
    inherited: &[u8],
    control: u16,
) -> Result<(), SandboxError> {
    const SE_DACL_AUTO_INHERITED: u16 = 0x0400;
    let extra_size = extra.map_or(0, |(sid, _)| 8 + sid.0.len());
    let size = (8 + explicit.len() + extra_size + inherited.len() + 8).next_multiple_of(4);
    let mut buf = vec![0u32; size / 4];
    let acl = buf.as_mut_ptr() as *mut ACL;
    let revision = u32::from(revision.max(2));
    // SAFETY: `buf` is `size` bytes, DWORD-aligned.
    if unsafe { InitializeAcl(acl, size as u32, revision) } == 0 {
        return Err(last_error("InitializeAcl"));
    }
    if !explicit.is_empty()
        // SAFETY: `explicit` holds whole ACEs copied from a valid ACL.
        && unsafe {
            AddAce(
                acl,
                revision,
                u32::MAX,
                explicit.as_ptr().cast(),
                explicit.len() as u32,
            )
        } == 0
    {
        return Err(last_error("AddAce"));
    }
    if let Some((sid, mask)) = extra {
        // SAFETY: `acl` has room for this entry; `sid` is valid.
        if unsafe { AddAccessAllowedAceEx(acl, revision, 0, mask, sid.as_psid()) } == 0 {
            return Err(last_error("AddAccessAllowedAceEx"));
        }
    }
    if !inherited.is_empty()
        // SAFETY: `inherited` holds whole ACEs copied from a valid ACL.
        && unsafe {
            AddAce(
                acl,
                revision,
                u32::MAX,
                inherited.as_ptr().cast(),
                inherited.len() as u32,
            )
        } == 0
    {
        return Err(last_error("AddAce"));
    }
    // A SECURITY_DESCRIPTOR is at most 40 bytes; 8-byte aligned.
    let mut sd_buf = [0u64; 8];
    let sd = sd_buf.as_mut_ptr().cast::<core::ffi::c_void>();
    // Keeping a DACL's auto-inherited flag needs the request flag too.
    const SE_DACL_AUTO_INHERIT_REQ: u16 = 0x0100;
    let bits = SE_DACL_PROTECTED_FLAG | SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ;
    let wanted = (control & (SE_DACL_PROTECTED_FLAG | SE_DACL_AUTO_INHERITED))
        | if control & SE_DACL_AUTO_INHERITED != 0 {
            SE_DACL_AUTO_INHERIT_REQ
        } else {
            0
        };
    // SAFETY: `sd` points to a zeroed, aligned buffer large enough for an
    // absolute security descriptor; `acl` outlives the SetFileSecurityW
    // call; `wpath` is NUL-terminated.
    let ok = unsafe {
        InitializeSecurityDescriptor(sd, 1) != 0
            && SetSecurityDescriptorDacl(sd, 1, acl, 0) != 0
            && SetSecurityDescriptorControl(sd, bits, wanted) != 0
    };
    if !ok {
        return Err(last_error("InitializeSecurityDescriptor"));
    }
    let wpath = wide(path.as_os_str());
    // SAFETY: as above.
    if unsafe { SetFileSecurityW(wpath.as_ptr(), DACL_SECURITY_INFORMATION, sd) } == 0 {
        return Err(last_error("SetFileSecurityW"));
    }
    Ok(())
}

fn add_list_ace(path: &Path, sid: &OwnedSid) -> Result<(), SandboxError> {
    let (explicit, inherited, revision, control) = dacl_parts(path)?;
    write_dacl_here(
        path,
        revision,
        &explicit,
        Some((sid, PARENT_LIST_MASK)),
        &inherited,
        control,
    )
}

/// Take this module's entry for `sid` off `path`: the this-folder-only
/// entry with exactly [`PARENT_LIST_MASK`]. An entry for the sandbox with
/// other rights isn't this module's and stays, as do inherited entries and
/// everyone else's. (Windows keeps one copy of identical entries, so the
/// administrator step on a folder that already has the hold's entry adds
/// nothing of its own: such a folder is this user's, which doesn't need
/// that step, and turning the setting off takes the access away there, as
/// the panel says.) A folder that no longer exists, or no longer has the
/// entry, counts as done.
fn remove_list_ace(path: &Path, sid: &OwnedSid) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return true;
    }
    let Ok((explicit, inherited, revision, control)) = dacl_parts(path) else {
        return false;
    };
    let mut kept = Vec::new();
    let mut removed = false;
    let mut i = 0;
    while i + 4 <= explicit.len() {
        let size = u16::from_le_bytes([explicit[i + 2], explicit[i + 3]]) as usize;
        if size < 8 || i + size > explicit.len() {
            return false;
        }
        let ace = &explicit[i..i + size];
        if !removed && is_list_ace(ace, sid) && ace_mask(ace) == PARENT_LIST_MASK {
            removed = true;
        } else {
            kept.extend_from_slice(ace);
        }
        i += size;
    }
    if !removed {
        return true;
    }
    write_dacl_here(path, revision, &kept, None, &inherited, control).is_ok()
}

/// "Folders above this project" on one folder, held by the app (ADR-025):
/// the first hold adds [`PARENT_LIST_MASK`] for the profile's container to
/// that folder only, journaled first; the last one removes it. A folder
/// that already lets the sandbox list it (the administrator step) is left
/// as it is and never changed.
pub struct ParentListHold {
    key: SharedKey,
    path: PathBuf,
    sid: OwnedSid,
}

impl ParentListHold {
    pub fn new(
        profile: &str,
        path: &Path,
        grant_journal_dir: Option<&Path>,
    ) -> Result<Self, SandboxError> {
        let _cleanup = cleanup_done();
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| SandboxError::Io(path.to_path_buf(), e.kind()))?;
        if !canonical.is_dir() {
            return Err(SandboxError::Io(
                path.to_path_buf(),
                std::io::ErrorKind::NotADirectory,
            ));
        }
        let sid = app_container_sid(profile)?;
        let key: SharedKey = (sid.0.clone(), canonical.clone());
        with_shared(|shared| -> Result<(), SandboxError> {
            let entry = shared.entry(key.clone()).or_default();
            if entry.lists == 0 {
                entry.list_journal = None;
                entry.list_added = false;
                if !has_list_ace(&canonical, &sid) {
                    let mut journal = match grant_journal_dir {
                        Some(dir) => Some(GrantJournal::create(dir)?),
                        None => None,
                    };
                    if let Some(journal) = &mut journal {
                        journal
                            .write_line(&format!("list {profile} {}\n", encode_path(&canonical)))?;
                    }
                    if let Err(e) = add_list_ace(&canonical, &sid) {
                        if let Some(journal) = journal {
                            journal.remove();
                        }
                        return Err(e);
                    }
                    entry.list_added = true;
                    entry.list_journal = journal;
                }
            }
            entry.lists += 1;
            Ok(())
        })?;
        Ok(Self {
            key,
            path: canonical,
            sid,
        })
    }

    /// The folder, as held (canonical).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ParentListHold {
    fn drop(&mut self) {
        with_shared(|shared| {
            let Some(entry) = shared.get_mut(&self.key) else {
                return;
            };
            entry.lists = entry.lists.saturating_sub(1);
            if entry.lists > 0 {
                return;
            }
            let journal = entry.list_journal.take();
            if entry.list_added
                && remove_list_ace(&self.path, &self.sid)
                && let Some(journal) = journal
            {
                journal.remove();
            }
            // A removal that failed keeps its journal file (closed here),
            // so the next start removes the entry.
            entry.list_added = false;
            if entry.unused() {
                shared.remove(&self.key);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Limits, Network, PathGrant};

    /// ADR-025: an entry the administrator step put there is never changed
    /// by a hold; an entry a hold added and a crash left behind is removed
    /// at the next start, and only that entry.
    #[test]
    fn parent_list_entries_are_journaled_and_admin_entries_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let folder = root.join("above");
        let journal = root.join("journal");
        std::fs::create_dir_all(&folder).unwrap();
        let profile = "Gardenwall.Test.ParentJournal";
        let sid = app_container_sid(profile).unwrap();
        let before = dacl_sddl(&folder).unwrap();

        // Crash: the hold's entry stays, its journal is closed, not removed.
        let hold = ParentListHold::new(profile, &folder, Some(&journal)).unwrap();
        assert!(has_list_ace(&folder, &sid));
        with_shared(|shared| {
            let entry = shared.get_mut(&hold.key).unwrap();
            drop(entry.list_journal.take());
            shared.remove(&hold.key);
        });
        std::mem::forget(hold);
        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.revoked, 1, "{report:?}");
        assert!(!has_list_ace(&folder, &sid));
        assert_eq!(dacl_sddl(&folder).unwrap(), before);

        // An administrator's entry: used, never removed.
        add_list_ace(&folder, &sid).unwrap();
        let hold = ParentListHold::new(profile, &folder, Some(&journal)).unwrap();
        drop(hold);
        assert!(
            has_list_ace(&folder, &sid),
            "the administrator's entry stays"
        );
        assert!(remove_list_ace(&folder, &sid));
        assert_eq!(dacl_sddl(&folder).unwrap(), before);

        // Windows keeps one copy of identical entries; removing twice is
        // harmless.
        add_list_ace(&folder, &sid).unwrap();
        add_list_ace(&folder, &sid).unwrap();
        assert!(remove_list_ace(&folder, &sid));
        assert!(remove_list_ace(&folder, &sid));
        assert_eq!(dacl_sddl(&folder).unwrap(), before);

        // An entry for the sandbox with other rights (not this module's) is
        // never taken off.
        let (explicit, inherited, revision, control) = dacl_parts(&folder).unwrap();
        write_dacl_here(
            &folder,
            revision,
            &explicit,
            Some((&sid, 0x0012_00A9)),
            &inherited,
            control,
        )
        .unwrap();
        let broader = dacl_sddl(&folder).unwrap();
        assert!(remove_list_ace(&folder, &sid));
        assert_eq!(dacl_sddl(&folder).unwrap(), broader);
        assert!(restore_dacl(&folder, &before));
        let _ = delete_profile(profile);
    }

    fn icacls(path: &Path) -> String {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .output()
            .expect("icacls runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn policy(profile: &str, target: &Path, journal: &Path) -> SandboxPolicy {
        SandboxPolicy {
            profile: profile.into(),
            grants: vec![PathGrant {
                path: target.to_path_buf(),
                access: Access::Read,
            }],
            network: Network::Denied,
            limits: Limits::default(),
            env: vec![],
            working_dir: None,
            grant_journal_dir: Some(journal.to_path_buf()),
        }
    }

    #[test]
    fn program_files_is_readable_by_all_packages_but_user_folders_are_not() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        assert!(all_packages_can_read(&system));
        let mine = tempfile::tempdir().unwrap();
        assert!(!all_packages_can_read(mine.path()));
        assert!(!all_packages_can_read(Path::new(
            r"C:\definitely\missing\path"
        )));
    }

    #[test]
    fn sandboxes_can_use_shared_and_own_folders_only() {
        // Readable by every AppContainer already.
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        assert!(sandbox_can_use(&system));
        // This user's own folder: its permissions can be changed.
        let mine = tempfile::tempdir().unwrap();
        assert!(sandbox_can_use(mine.path()));
        assert!(!sandbox_can_use(Path::new(r"C:\definitely\missing\path")));
        // A folder only administrators may change and AppContainers can't
        // read (Node.js's MSI install sets this up), when it exists.
        let node = Path::new(r"C:\Program Files\nodejs");
        if node.is_dir() && !all_packages_can_read(node) {
            assert!(!sandbox_can_use(node));
        }
    }

    #[test]
    fn journal_paths_round_trip() {
        let p = PathBuf::from(r"\\?\C:\Users\A B\données\x.gguf");
        assert!(p.is_absolute());
        assert_eq!(decode_path(&encode_path(&p)), Some(p));
        assert_eq!(decode_path("zz"), None);
        assert_eq!(decode_path(&encode_path(Path::new("relative"))), None);
    }

    #[test]
    fn clean_shutdown_revokes_grants_and_removes_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, journal) = (tmp.path().join("target"), tmp.path().join("journal"));
        std::fs::create_dir_all(&target).unwrap();
        let profile = "Gardenwall.Test.JournalClean";
        let sandbox = Sandbox::new(policy(profile, &target, &journal)).unwrap();
        let sid = sandbox.sid_string().unwrap();
        assert!(icacls(&target).contains(&sid));
        assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 1);
        // A live sandbox's journal is never touched by reconciliation.
        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.in_use, 1);
        assert!(icacls(&target).contains(&sid));
        drop(sandbox);
        assert!(!icacls(&target).contains(&sid));
        assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 0);
        let _ = delete_profile(profile);
    }

    #[test]
    fn grants_left_by_a_crash_are_revoked_at_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, journal) = (tmp.path().join("target"), tmp.path().join("journal"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("model.gguf"), b"x").unwrap();
        let profile = "Gardenwall.Test.JournalCrash";
        let mut sandbox = Sandbox::new(policy(profile, &target, &journal)).unwrap();
        let sid = sandbox.sid_string().unwrap();
        // Simulate a crash: the process dies, so the journal's handle is
        // closed, but nothing revokes the grants.
        let crashed = sandbox.journal.take().unwrap();
        drop(crashed.file);
        std::mem::forget(sandbox);
        assert!(icacls(&target).contains(&sid));
        assert!(icacls(&target.join("model.gguf")).contains(&sid));

        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.stale_journals, 1);
        assert_eq!(report.revoked, 1);
        assert_eq!(report.kept, 0);
        assert!(!icacls(&target).contains(&sid));
        assert!(!icacls(&target.join("model.gguf")).contains(&sid));
        assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 0);
        let _ = delete_profile(profile);
    }

    /// The process ending: nothing is dropped, so nothing is revoked, and
    /// this process's count of the grant goes with it.
    fn end_process(hold: GrantHold, profile: &str, path: &Path) {
        let mut hold = hold;
        if let Some(Record::Journal(j)) = hold.record.take() {
            drop(j.file);
        }
        std::mem::forget(hold);
        let sid = app_container_sid(profile).unwrap();
        let canonical = std::fs::canonicalize(path).unwrap();
        with_shared(|s| s.remove(&(sid.0.clone(), canonical)));
    }

    /// A slow grant or revoke on one folder (a toolchain's 70,000 files)
    /// holds up only grants of that folder, never another sandbox's.
    #[test]
    fn a_slow_change_holds_up_only_its_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (slow, other) = (tmp.path().join("slow"), tmp.path().join("other"));
        std::fs::create_dir_all(&slow).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let profile = "Gardenwall.Test.SlowChange";
        let sid = app_container_sid(profile).unwrap();
        let slow_key: SharedKey = (sid.0.clone(), std::fs::canonicalize(&slow).unwrap());
        // A change to `slow` under way.
        with_shared(|s| s.entry(slow_key.clone()).or_default().busy = true);
        let other_grant =
            AclGrant::new(&std::fs::canonicalize(&other).unwrap(), &sid, Access::Read).unwrap();
        // The same folder waits until the change has finished.
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, sid2) = (slow_key.1.clone(), sid.clone());
        let waiter = std::thread::spawn(move || {
            let g = AclGrant::new(&path, &sid2, Access::Read).unwrap();
            tx.send(()).unwrap();
            g
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "waits"
        );
        finish_change(&slow_key, |_| {});
        rx.recv_timeout(Duration::from_secs(10))
            .expect("goes on once done");
        let slow_grant = waiter.join().unwrap();
        assert!(slow_grant.revoke() && other_grant.revoke());
        assert!(with_shared(|s| !s.contains_key(&slow_key)));
        let _ = delete_profile(profile);
    }

    /// ADR-022 §8a: a kept toolchain grant
    /// outlives the process and is taken over at the next start without
    /// granting again; dropping the hold still revokes it.
    #[test]
    fn kept_grants_outlive_the_process_and_are_taken_over() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, kept) = (tmp.path().join("toolchain"), tmp.path().join("kept"));
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin").join("tool.exe"), b"x").unwrap();
        let profile = "Gardenwall.Test.Kept";
        let sid = profile_sid(profile).unwrap();

        let hold = GrantHold::kept(profile, &target, Access::ReadExecute, &kept).unwrap();
        assert!(icacls(&target).contains(&sid));
        end_process(hold, profile, &target);
        // The next start finds it, in place.
        assert!(icacls(&target).contains(&sid));
        let found = kept_grants(&kept);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, std::fs::canonicalize(&target).unwrap());
        assert_eq!(found[0].access, Access::ReadExecute);
        assert!(
            !found[0].pending,
            "a finished grant is recorded as complete"
        );
        let adopted = GrantHold::adopt(found[0].clone()).unwrap();
        let osid = app_container_sid(profile).unwrap();
        assert!(already_held(&osid, &found[0].path, Access::ReadExecute));
        assert!(icacls(&target.join("bin").join("tool.exe")).contains(&sid));
        // Let go: revoked, record gone.
        drop(adopted);
        assert!(!icacls(&target).contains(&sid));
        assert!(kept_grants(&kept).is_empty());

        // Taken away while Gardenwall was closed: taken over by granting
        // again.
        let hold = GrantHold::kept(profile, &target, Access::ReadExecute, &kept).unwrap();
        end_process(hold, profile, &target);
        assert!(revoke_ace(&std::fs::canonicalize(&target).unwrap(), &osid));
        let found = kept_grants(&kept).pop().unwrap();
        let adopted = GrantHold::adopt(found).unwrap();
        assert!(icacls(&target).contains(&sid));
        assert_eq!(kept_grants(&kept).len(), 1);
        drop(adopted);
        assert!(kept_grants(&kept).is_empty());

        // The process ended while granting (the entry on the folder, maybe
        // not yet on every file below): the record is still pending, and
        // taking it over grants again in full, then completes it.
        let canonical = std::fs::canonicalize(&target).unwrap();
        set_ace(
            &canonical,
            &osid,
            GRANT_ACCESS,
            access_mask(Access::ReadExecute),
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )
        .unwrap();
        KeptRecord::create(&kept, profile, &canonical, Access::ReadExecute, true).unwrap();
        let found = kept_grants(&kept).pop().unwrap();
        assert!(found.pending);
        let adopted = GrantHold::adopt(found).unwrap();
        let records = kept_grants(&kept);
        assert_eq!(records.len(), 1);
        assert!(!records[0].pending, "granted again, then completed");
        assert!(icacls(&target.join("bin").join("tool.exe")).contains(&sid));
        drop(adopted);
        assert!(kept_grants(&kept).is_empty());

        // No longer wanted at the next start: revoked with its record.
        let hold = GrantHold::kept(profile, &target, Access::ReadExecute, &kept).unwrap();
        end_process(hold, profile, &target);
        assert!(revoke_kept(kept_grants(&kept).pop().unwrap()));
        assert!(!icacls(&target).contains(&sid));
        assert!(kept_grants(&kept).is_empty());
        let _ = delete_profile(profile);
    }

    /// Start-up cleanup keeps a leftover grant the app wants anyway (as a
    /// kept grant) instead of revoking it, but only when the entry gives
    /// exactly the access wanted (never more, never something else).
    #[test]
    fn cleanup_keeps_wanted_grants_and_revokes_others() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let project = tmp.path().join("project");
        let (journal, kept) = (tmp.path().join("journal"), tmp.path().join("kept"));
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let profile = "Gardenwall.Test.KeepAtStart";
        let sid = profile_sid(profile).unwrap();
        let hold = GrantHold::new(profile, &tools, Access::ReadExecute, Some(&journal)).unwrap();
        end_process(hold, profile, &tools);
        // A session's read-only grant on another folder, left by a crash.
        let mut sandbox = Sandbox::new(policy(profile, &project, &journal)).unwrap();
        let crashed = sandbox.journal.take().unwrap();
        drop(crashed.file);
        std::mem::forget(sandbox);
        let osid = app_container_sid(profile).unwrap();
        for p in [&tools, &project] {
            let c = std::fs::canonicalize(p).unwrap();
            with_shared(|s| s.remove(&(osid.0.clone(), c)));
        }

        // Both are "wanted" read-and-run: only the exact one is kept.
        let report = reconcile_grant_journals_keeping(&journal, Some(&kept), &|p, _| {
            (p == profile).then_some(Access::ReadExecute)
        })
        .unwrap();
        assert_eq!(report.kept_in_place, 1, "{report:?}");
        assert_eq!(report.revoked, 1, "{report:?}");
        assert!(icacls(&tools).contains(&sid));
        assert!(
            !icacls(&project).contains(&sid),
            "a grant giving other access is never kept"
        );
        assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 0);
        let found = kept_grants(&kept);
        assert_eq!(found.len(), 1);
        assert!(
            found[0].pending,
            "a journal can't tell if the grant finished"
        );
        assert!(revoke_kept(found[0].clone()));
        assert!(!icacls(&tools).contains(&sid));
        let _ = delete_profile(profile);
    }

    #[test]
    fn read_only_paths_left_by_a_crash_are_restored_at_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, journal) = (tmp.path().join("ws"), tmp.path().join("journal"));
        std::fs::create_dir_all(ws.join(".vscode")).unwrap();
        std::fs::write(ws.join(".vscode").join("settings.json"), b"{}").unwrap();
        let profile = "Gardenwall.Test.ProtectCrash";
        let mut p = policy(profile, &ws, &journal);
        p.grants[0].access = Access::ReadWrite;
        let mut sandbox = Sandbox::new(p).unwrap();
        let sid = sandbox.sid_string().unwrap();
        let before = dacl_sddl(&std::fs::canonicalize(ws.join(".vscode")).unwrap()).unwrap();
        assert!(sandbox.protect(&ws.join(".vscode")).unwrap());
        let during = icacls(&ws.join(".vscode"));
        assert!(during.contains(&sid) && !during.contains("(I)"), "{during}");
        // Crash: nothing restores the permissions.
        let crashed = sandbox.journal.take().unwrap();
        drop(crashed.file);
        std::mem::forget(sandbox);

        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.kept, 0, "{report:?}");
        let after = icacls(&ws.join(".vscode"));
        assert!(!after.contains(&sid), "{after}");
        assert!(after.contains("(I)"), "inheritance is back: {after}");
        let restored = dacl_sddl(&std::fs::canonicalize(ws.join(".vscode")).unwrap()).unwrap();
        // The same once the (also revoked) grant's inherited entry is gone.
        assert!(!restored.contains("P("), "{restored} vs {before}");
        assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 0);
        let _ = delete_profile(profile);
    }

    #[test]
    fn a_forged_restore_line_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let target = std::fs::canonicalize(tmp.path()).unwrap().join("x");
        std::fs::create_dir(&target).unwrap();
        let before = icacls(&target);
        // A valid Gardenwall identity, but no entry of it on the path: the
        // "restore" (to an everyone-full-control DACL) is skipped.
        std::fs::write(
            journal.join("forged.grants"),
            format!(
                "restore Gardenwall.Test.Forged {} {}\n",
                encode_path(&target),
                encode_path(Path::new("D:P(A;;FA;;;WD)"))
            ),
        )
        .unwrap();
        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.revoked, 0, "{report:?}");
        assert_eq!(icacls(&target), before);
    }

    #[test]
    fn tampered_journal_cannot_target_other_identities() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let target = tmp.path().join("x");
        std::fs::write(&target, b"x").unwrap();
        std::fs::write(
            journal.join("forged.grants"),
            format!(
                "Microsoft.Something {}\nGardenwall.Ok zz\nnot-a-record\n",
                encode_path(&std::fs::canonicalize(&target).unwrap())
            ),
        )
        .unwrap();
        let report = reconcile_grant_journals(&journal).unwrap();
        assert_eq!(report.ignored, 3);
        assert_eq!(report.revoked, 0);
    }
}
