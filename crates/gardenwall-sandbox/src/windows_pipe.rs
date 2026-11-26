//! Core-owned named pipe that only one sandbox's processes can open
//! (ADR-012 §4.1, SPIKE-002).
//!
//! The pipe:
//! - has an unpredictable name (`\\.\pipe\gardenwall-<128-bit random hex>`);
//! - is created as the first instance, so the name cannot be squatted, and
//!   rejects remote clients;
//! - carries a security descriptor granting full control to SYSTEM and the
//!   owner, read/write to the sandbox's AppContainer SID only, and a
//!   low-integrity mandatory label so the (Low-IL) container may write.
//!
//! Callers must additionally check the returned client PID against the
//! process they spawned (same-user processes outside the container are
//! covered by the owner ACE).

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;

use crate::SandboxError;
use crate::windows::Sandbox;

/// An owned, immutable security descriptor allocated by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is never mutated after creation and is only read by
// the OS during pipe-instance creation; LocalFree in Drop may run on any
// thread.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above; shared references only read the pointer value.
unsafe impl Sync for SecurityDescriptor {}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by the conversion API.
        unsafe { LocalFree(self.0) };
    }
}

/// SDDL for a pipe usable only by the container SID (and SYSTEM/owner).
pub fn pipe_sddl(container_sid: &str) -> Result<String, SandboxError> {
    // SID strings are "S-" followed by digits and dashes; refuse anything
    // else so the SDDL cannot be altered.
    let valid = container_sid.starts_with("S-1-15-2-")
        && container_sid[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(SandboxError::Os {
            op: "pipe_sddl(invalid SID)",
            code: 0,
        });
    }
    Ok(format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;OW)(A;;GRGW;;;{container_sid})S:(ML;;NW;;;LW)"
    ))
}

fn security_descriptor(sddl: &str) -> Result<SecurityDescriptor, SandboxError> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `sd` receives a
    // LocalAlloc'd descriptor owned by the returned guard.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: GetLastError has no preconditions.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(SandboxError::Os {
            op: "ConvertStringSecurityDescriptorToSecurityDescriptorW",
            code,
        });
    }
    Ok(SecurityDescriptor(sd))
}

fn random_pipe_name() -> Result<String, SandboxError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| SandboxError::Os {
        op: "getrandom",
        code: 0,
    })?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(r"\\.\pipe\gardenwall-{hex}"))
}

/// Listener that keeps one pending pipe instance and hands out connected
/// instances with their client PID.
pub struct PipeListener {
    name: String,
    sd: SecurityDescriptor,
    pending: NamedPipeServer,
}

impl std::fmt::Debug for PipeListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeListener")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PipeListener {
    /// Create a listener whose pipe only `sandbox`'s processes may open.
    /// Must be called within a Tokio runtime.
    pub fn for_sandbox(sandbox: &Sandbox) -> Result<Self, SandboxError> {
        let sd = security_descriptor(&pipe_sddl(&sandbox.sid_string()?)?)?;
        let name = random_pipe_name()?;
        let pending = create_instance(&name, &sd, true)
            .map_err(|e| SandboxError::Io(name.clone().into(), e.kind()))?;
        Ok(Self { name, sd, pending })
    }

    /// The pipe name to pass to the relay.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait for the next client. Returns the connected instance and the
    /// client's process ID; a new pending instance is created first so the
    /// name stays owned by this listener.
    pub async fn accept(&mut self) -> io::Result<(NamedPipeServer, u32)> {
        self.pending.connect().await?;
        let next = create_instance(&self.name, &self.sd, false)?;
        let connected = std::mem::replace(&mut self.pending, next);
        let mut pid = 0u32;
        // SAFETY: `connected` is a valid, connected pipe handle; `pid` is a
        // valid out-pointer.
        let ok =
            unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle() as HANDLE, &mut pid) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((connected, pid))
    }
}

fn create_instance(
    name: &str,
    sd: &SecurityDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    // SAFETY: `attributes` points to a valid SECURITY_ATTRIBUTES whose
    // descriptor (`sd`) outlives this call, as Tokio requires.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast::<c_void>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_accepts_only_appcontainer_sids() {
        let sid =
            "S-1-15-2-2087215187-2257605435-808988923-931119314-2021059029-2960977608-1767399961";
        let sddl = pipe_sddl(sid).unwrap();
        assert!(sddl.contains(&format!("(A;;GRGW;;;{sid})")));
        assert!(sddl.ends_with("S:(ML;;NW;;;LW)"));
        for bad in [
            "",
            "S-1-5-32-544",
            "S-1-15-2-1)(A;;GA;;;WD",
            "S-1-15-2-1;x",
            "WD",
        ] {
            assert!(pipe_sddl(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pipe_names_are_random_and_well_formed() {
        let a = random_pipe_name().unwrap();
        let b = random_pipe_name().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(r"\\.\pipe\gardenwall-"));
        assert_eq!(a.len(), r"\\.\pipe\gardenwall-".len() + 32);
    }

    #[test]
    fn descriptor_converts() {
        let sddl = pipe_sddl("S-1-15-2-1-2-3-4-5-6-7").unwrap();
        assert!(security_descriptor(&sddl).is_ok());
    }
}
