//! Linux backend: Landlock + seccomp-BPF (ADR-012 §3).
//!
//! Every sandboxed process is set up between `fork` and `exec`, before any
//! untrusted code runs:
//! - **Own process group**, so the sandbox can kill everything it started.
//!   `setsid`/`setpgid` are then denied, so descendants cannot leave it.
//! - **`PR_SET_NO_NEW_PRIVS`**: setuid binaries cannot gain privileges.
//! - **Landlock** (ABI 1+ required, fails closed otherwise): filesystem
//!   access only beneath granted paths, plus `/dev/null` (read/write) and
//!   `/dev/zero`, `/dev/random`, `/dev/urandom` (read). With ABI 4+ all TCP bind/connect is
//!   denied; with ABI 6+ abstract Unix sockets and signals are scoped to the
//!   sandbox.
//! - **seccomp**: `socket()` is denied outright (so no UDP, raw, netlink or
//!   Unix sockets either), as are `ptrace`, cross-process memory access,
//!   namespace and mount syscalls, `io_uring`, kernel keyrings, module
//!   loading, `bpf`, `perf_event_open`, `userfaultfd` and
//!   `open_by_handle_at`. With `max_processes == 1`, creating processes is
//!   denied (threads still work).
//! - **Resource limits**: `RLIMIT_AS` from `memory_bytes` (virtual address
//!   space, not committed memory as on Windows) and no core dumps.
//! - An explicit environment; nothing is inherited.
//!
//! User and network namespaces are not used: Landlock and seccomp enforce
//! the whole policy on their own, as ADR-012 requires where unprivileged
//! namespaces are unavailable (e.g. Ubuntu's AppArmor restriction).
//!
//! All `unsafe` code in this crate's Linux backend lives in this module.

#![allow(unsafe_code)]

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::policy::{Access, SandboxPolicy};
use crate::{PolicyError, SandboxError};

// ---- Landlock ABI (linux/landlock.h) ----

const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_CHAR: u64 = 1 << 6;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SOCK: u64 = 1 << 9;
const FS_MAKE_FIFO: u64 = 1 << 10;
const FS_MAKE_BLOCK: u64 = 1 << 11;
const FS_MAKE_SYM: u64 = 1 << 12;
const FS_REFER: u64 = 1 << 13; // ABI 2
const FS_TRUNCATE: u64 = 1 << 14; // ABI 3
const FS_IOCTL_DEV: u64 = 1 << 15; // ABI 5
const NET_BIND_TCP: u64 = 1 << 0; // ABI 4
const NET_CONNECT_TCP: u64 = 1 << 1; // ABI 4
const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0; // ABI 6
const SCOPE_SIGNAL: u64 = 1 << 1; // ABI 6

/// Rights that apply to regular files (the only ones a file rule may hold).
const FILE_RIGHTS: u64 = FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_TRUNCATE | FS_IOCTL_DEV;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

fn landlock_abi() -> i64 {
    // SAFETY: querying the ABI version takes no pointers.
    unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

fn handled_fs(abi: i64) -> u64 {
    let mut rights = FS_EXECUTE
        | FS_WRITE_FILE
        | FS_READ_FILE
        | FS_READ_DIR
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_CHAR
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_MAKE_SOCK
        | FS_MAKE_FIFO
        | FS_MAKE_BLOCK
        | FS_MAKE_SYM;
    if abi >= 2 {
        rights |= FS_REFER;
    }
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    if abi >= 5 {
        rights |= FS_IOCTL_DEV;
    }
    rights
}

fn grant_rights(access: Access, handled: u64) -> u64 {
    let read = FS_READ_FILE | FS_READ_DIR;
    let rights = match access {
        Access::Read => read,
        Access::ReadExecute => read | FS_EXECUTE,
        // Everything except creating device nodes.
        Access::ReadWrite => handled & !(FS_MAKE_CHAR | FS_MAKE_BLOCK),
    };
    rights & handled
}

fn os_error(op: &'static str) -> SandboxError {
    SandboxError::Os {
        op,
        code: io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
    }
}

/// Build a Landlock ruleset for the policy; returns its fd.
fn build_ruleset(policy: &SandboxPolicy) -> Result<OwnedFd, SandboxError> {
    let abi = landlock_abi();
    if abi < 1 {
        // No Landlock: refuse rather than run unconfined (ADR-012 §6).
        return Err(SandboxError::Unsupported);
    }
    let handled = handled_fs(abi);
    let attr = RulesetAttr {
        handled_access_fs: handled,
        handled_access_net: if abi >= 4 {
            NET_BIND_TCP | NET_CONNECT_TCP
        } else {
            0
        },
        scoped: if abi >= 6 {
            SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL
        } else {
            0
        },
    };
    let size = match abi {
        1..=3 => 8,
        4..=5 => 16,
        _ => std::mem::size_of::<RulesetAttr>(),
    };
    // SAFETY: `attr` is valid for `size` bytes, which never exceeds its size.
    let fd = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            &attr as *const RulesetAttr,
            size,
            0u32,
        )
    };
    if fd < 0 {
        return Err(os_error("landlock_create_ruleset"));
    }
    // SAFETY: the syscall returned a new file descriptor that we now own.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
    // Harmless devices ordinary programs expect (no ioctls on them).
    for (device, rights) in [
        ("/dev/null", FS_READ_FILE | FS_WRITE_FILE),
        ("/dev/zero", FS_READ_FILE),
        ("/dev/random", FS_READ_FILE),
        ("/dev/urandom", FS_READ_FILE),
    ] {
        if Path::new(device).exists() {
            add_rule(&ruleset, Path::new(device), rights & handled)?;
        }
    }
    for grant in &policy.grants {
        let mut rights = grant_rights(grant.access, handled);
        if !grant.path.is_dir() {
            rights &= FILE_RIGHTS;
        }
        add_rule(&ruleset, &grant.path, rights)?;
    }
    Ok(ruleset)
}

/// Allow `rights` beneath `path` in the ruleset.
fn add_rule(ruleset: &OwnedFd, path: &Path, rights: u64) -> Result<(), SandboxError> {
    {
        let grant_path = path;
        let path = CString::new(grant_path.as_os_str().as_bytes()).map_err(|_| {
            SandboxError::Policy(PolicyError::MissingPath(grant_path.to_path_buf()))
        })?;
        // SAFETY: `path` is NUL-terminated; O_PATH opens without reading.
        let parent = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if parent < 0 {
            return Err(SandboxError::Io(
                grant_path.to_path_buf(),
                io::Error::last_os_error().kind(),
            ));
        }
        // SAFETY: `open` returned a new descriptor that we now own.
        let parent = unsafe { OwnedFd::from_raw_fd(parent) };
        let rule = PathBeneathAttr {
            allowed_access: rights,
            parent_fd: parent.as_raw_fd(),
        };
        // SAFETY: valid ruleset fd and a pointer to a live, packed rule.
        let rc = unsafe {
            libc::syscall(
                SYS_LANDLOCK_ADD_RULE,
                ruleset.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0u32,
            )
        };
        if rc < 0 {
            return Err(os_error("landlock_add_rule"));
        }
    }
    Ok(())
}

// ---- seccomp-BPF ----

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0: u32 = 16;

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

const LD_W_ABS: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const JEQ_K: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
const JSET_K: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const RET_K: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

/// Syscalls no sandboxed process needs, denied with EPERM.
fn denied_syscalls() -> Vec<i64> {
    let mut v = vec![
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_bpf,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_kexec_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_userfaultfd,
        libc::SYS_perf_event_open,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_move_mount,
        libc::SYS_open_tree,
        libc::SYS_mount_setattr,
        libc::SYS_personality,
        libc::SYS_acct,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_reboot,
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        libc::SYS_quotactl,
        libc::SYS_syslog,
        libc::SYS_vhangup,
        libc::SYS_setsid,
        libc::SYS_setpgid,
    ];
    #[cfg(target_arch = "x86_64")]
    v.extend([
        libc::SYS_kexec_file_load,
        libc::SYS_iopl,
        libc::SYS_ioperm,
        libc::SYS_uselib,
        libc::SYS_modify_ldt,
    ]);
    v
}

const NAMESPACE_FLAGS: u32 = (libc::CLONE_NEWNS
    | libc::CLONE_NEWUSER
    | libc::CLONE_NEWNET
    | libc::CLONE_NEWPID
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWUTS
    | libc::CLONE_NEWCGROUP) as u32;

fn seccomp_filter(allow_processes: bool) -> Vec<libc::sock_filter> {
    let eperm = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let eacces = SECCOMP_RET_ERRNO | libc::EACCES as u32;
    let enosys = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    let mut f = vec![
        // Wrong architecture (or x32 on x86_64): kill.
        stmt(LD_W_ABS, OFF_ARCH),
        jump(JEQ_K, AUDIT_ARCH, 1, 0),
        stmt(RET_K, SECCOMP_RET_KILL_PROCESS),
        stmt(LD_W_ABS, OFF_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        f.push(jump(JSET_K, 0x4000_0000, 0, 1));
        f.push(stmt(RET_K, SECCOMP_RET_KILL_PROCESS));
    }
    for nr in denied_syscalls() {
        f.push(jump(JEQ_K, nr as u32, 0, 1));
        f.push(stmt(RET_K, eperm));
    }
    // No sockets of any kind.
    f.push(jump(JEQ_K, libc::SYS_socket as u32, 0, 1));
    f.push(stmt(RET_K, eacces));
    // clone3 passes flags in memory BPF cannot read: make libc fall back to
    // clone, whose flags can be checked.
    f.push(jump(JEQ_K, libc::SYS_clone3 as u32, 0, 1));
    f.push(stmt(RET_K, enosys));
    #[cfg(target_arch = "x86_64")]
    {
        for nr in [libc::SYS_fork, libc::SYS_vfork] {
            f.push(jump(JEQ_K, nr as u32, 0, 1));
            f.push(stmt(
                RET_K,
                if allow_processes {
                    SECCOMP_RET_ALLOW
                } else {
                    eperm
                },
            ));
        }
    }
    // clone: never new namespaces; threads always; processes only if allowed.
    f.push(jump(JEQ_K, libc::SYS_clone as u32, 0, 6));
    f.push(stmt(LD_W_ABS, OFF_ARG0));
    f.push(jump(JSET_K, NAMESPACE_FLAGS, 0, 1));
    f.push(stmt(RET_K, eperm));
    f.push(jump(JSET_K, libc::CLONE_THREAD as u32, 1, 0));
    f.push(stmt(
        RET_K,
        if allow_processes {
            SECCOMP_RET_ALLOW
        } else {
            eperm
        },
    ));
    f.push(stmt(RET_K, SECCOMP_RET_ALLOW));
    f.push(stmt(RET_K, SECCOMP_RET_ALLOW));
    f
}

// ---- processes ----

/// A sandbox: the policy, its prepared Landlock ruleset and seccomp filter,
/// and the process groups it started (killed on drop).
pub struct Sandbox {
    policy: SandboxPolicy,
    ruleset: OwnedFd,
    filter: Vec<libc::sock_filter>,
    groups: Mutex<Vec<libc::pid_t>>,
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
    child: Child,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}

impl std::fmt::Debug for SandboxedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedProcess")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

fn into_file(fd: impl Into<OwnedFd>) -> File {
    File::from(fd.into())
}

impl Sandbox {
    pub fn new(policy: SandboxPolicy) -> Result<Self, SandboxError> {
        policy.validate_shape()?;
        let mut canonical = policy.clone();
        for grant in &mut canonical.grants {
            if !grant.path.exists() {
                return Err(PolicyError::MissingPath(grant.path.clone()).into());
            }
            grant.path = std::fs::canonicalize(&grant.path)
                .map_err(|e| SandboxError::Io(grant.path.clone(), e.kind()))?;
        }
        let ruleset = build_ruleset(&canonical)?;
        let filter = seccomp_filter(canonical.limits.max_processes > 1);
        Ok(Self {
            policy: canonical,
            ruleset,
            filter,
            groups: Mutex::new(Vec::new()),
        })
    }

    pub fn spawn(&self, program: &Path, args: &[&OsStr]) -> Result<SandboxedProcess, SandboxError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(self.policy.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(dir) = &self.policy.working_dir {
            command.current_dir(dir);
        }
        let ruleset = self.ruleset.as_raw_fd();
        // Captured as plain integers (the closure must be `Send`); the
        // filter memory is owned by `self` and outlives the spawn.
        let filter_ptr = self.filter.as_ptr() as usize;
        let filter_len = self.filter.len() as u16;
        let memory = self.policy.limits.memory_bytes;
        // SAFETY: the closure runs in the forked child before exec and uses
        // only async-signal-safe system calls on memory prepared beforehand
        // (the filter lives in `self.filter`, which outlives the spawn).
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let no_core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some(bytes) = memory {
                    let limit = libc::rlimit {
                        rlim_cur: bytes,
                        rlim_max: bytes,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset, 0u32) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let prog = libc::sock_fprog {
                    len: filter_len,
                    filter: filter_ptr as *mut libc::sock_filter,
                };
                if libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &prog as *const libc::sock_fprog,
                    0,
                    0,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| SandboxError::Io(program.to_path_buf(), e.kind()))?;
        if let Ok(mut groups) = self.groups.lock() {
            groups.push(child.id() as libc::pid_t);
        }
        Ok(SandboxedProcess {
            stdin: child.stdin.take().map(into_file),
            stdout: child.stdout.take().map(into_file),
            stderr: child.stderr.take().map(into_file),
            child,
        })
    }

    /// No-op on Linux (processes have no console windows); kept for API
    /// parity with Windows.
    pub fn use_hidden_console(&mut self) {}

    /// Landlock can only grant access, never take part of a granted folder
    /// back, so nothing is enforced here and this returns `Ok(false)`. The
    /// agent's after-command check covers protected locations on Linux.
    pub fn protect(&mut self, _path: &std::path::Path) -> Result<bool, SandboxError> {
        Ok(false)
    }

    /// Not enforced on Linux, for the same reason as [`Self::protect`]: a
    /// file can't be taken out of a granted folder. Returns `Ok(false)`, so
    /// secret files in a granted folder stay readable to commands here.
    pub fn hide(&mut self, _path: &std::path::Path) -> Result<bool, SandboxError> {
        Ok(false)
    }

    /// Process groups this sandbox started that still have a live process
    /// (Linux can't name them without reading other processes' details).
    pub fn processes(&self) -> Vec<(u32, String)> {
        let Ok(groups) = self.groups.lock() else {
            return Vec::new();
        };
        groups
            .iter()
            .filter(|&&pgid| {
                // SAFETY: signal 0 only checks that the group exists.
                unsafe { libc::kill(-pgid, 0) == 0 }
            })
            .map(|&pgid| (pgid as u32, "process group".to_string()))
            .collect()
    }

    /// Kill one process group this sandbox started (ids from
    /// [`Sandbox::processes`]).
    pub fn terminate_process(&self, pgid: u32) -> bool {
        let Ok(groups) = self.groups.lock() else {
            return false;
        };
        let Some(&g) = groups.iter().find(|&&g| g as u32 == pgid) else {
            return false;
        };
        // SAFETY: signalling a process group we created.
        unsafe { libc::kill(-g, libc::SIGKILL) == 0 }
    }

    /// Kill every process this sandbox started, with their descendants.
    pub fn terminate_all(&self) -> Result<(), SandboxError> {
        if let Ok(groups) = self.groups.lock() {
            for &pgid in groups.iter() {
                // SAFETY: signalling a process group we created; a stale
                // group simply yields ESRCH.
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
        }
        Ok(())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.terminate_all();
    }
}

impl SandboxedProcess {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for exit; `None` if still running at the timeout.
    pub fn wait_timeout(&mut self, timeout: Option<Duration>) -> Result<Option<u32>, SandboxError> {
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt;
                    let code = status
                        .code()
                        .map(|c| c as u32)
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0) as u32);
                    return Ok(Some(code));
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(SandboxError::Io(
                        Path::new("<child>").to_path_buf(),
                        e.kind(),
                    ));
                }
            }
            if timeout.is_some_and(|t| start.elapsed() >= t) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn kill(&mut self) -> Result<(), SandboxError> {
        // SAFETY: signalling the process group we created for this child.
        unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
        Ok(())
    }
}
