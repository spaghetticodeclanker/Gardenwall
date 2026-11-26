//! GPU memory status (ROADMAP Phase 3, "VRAM slowdown warning").
//!
//! When other apps take GPU memory after a model has loaded, the Windows
//! driver moves part of the model into system memory and generation runs
//! 4–9× slower (ADR-015 §2.1). This crate reports how full the GPU is and
//! which processes hold its memory, so the app can tell the user what to
//! close.
//!
//! - DXGI gives the adapter (name, LUID, dedicated memory) and the budget
//!   the OS currently grants a process. `QueryVideoMemoryInfo` only reports
//!   the *calling* process's usage, and the model runs in a separate
//!   sandboxed process, so usage comes from the next source.
//! - Windows' GPU performance counters (PDH, the source Task Manager uses)
//!   give dedicated and shared GPU memory per process and per adapter.
//!
//! Everything here only reads. All `unsafe` code is in `windows.rs`, kept
//! to FFI calls with the invariants written beside each block. Other
//! platforms return [`GpuError::Unsupported`].

use std::fmt;

#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    Unsupported,
    /// No hardware adapter was found.
    NoAdapter,
    Os(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("GPU memory status is not available on this platform"),
            Self::NoAdapter => f.write_str("no hardware GPU was found"),
            Self::Os(e) => write!(f, "GPU memory status unavailable: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

/// GPU memory one process holds on the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessGpu {
    pub pid: u32,
    /// Executable file name (no folder), or empty when it can't be read.
    pub name: String,
    /// Bytes in the GPU's own memory.
    pub dedicated: u64,
    /// Bytes of GPU allocations in system memory.
    pub shared: u64,
}

/// A snapshot of the adapter with the most dedicated memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuReport {
    pub adapter: String,
    pub dedicated_total: u64,
    /// Dedicated memory in use by all processes, when counters are readable.
    pub dedicated_used: Option<u64>,
    /// What the OS would currently let one process use (DXGI budget).
    pub budget: u64,
    /// Processes using this adapter, most dedicated memory first.
    pub processes: Vec<ProcessGpu>,
}

/// Free dedicated memory below this counts as full: other apps' growth
/// then pushes the model out.
pub const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

impl GpuReport {
    pub fn free(&self) -> Option<u64> {
        self.dedicated_used
            .map(|u| self.dedicated_total.saturating_sub(u))
    }

    /// The GPU's own memory is (nearly) full.
    pub fn under_pressure(&self) -> bool {
        let floor = MIN_FREE_BYTES.max(self.dedicated_total / 50);
        self.free().is_some_and(|f| f < floor)
    }

    pub fn process(&self, pid: u32) -> Option<&ProcessGpu> {
        self.processes.iter().find(|p| p.pid == pid)
    }
}

/// Windows' own processes, never suggested for closing. Per-process
/// counters also overstate some of them (the desktop compositor can show
/// more than the whole adapter uses), so they are left out of rankings.
const SYSTEM_PROCESSES: &[&str] = &[
    "system",
    "dwm.exe",
    "csrss.exe",
    "winlogon.exe",
    "explorer.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "textinputhost.exe",
    "lockapp.exe",
    "ctfmon.exe",
    "applicationframehost.exe",
    "systemsettings.exe",
    "widgets.exe",
    "smartscreen.exe",
];

impl ProcessGpu {
    pub fn is_system(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        n.is_empty() || SYSTEM_PROCESSES.contains(&n.as_str())
    }
}

/// PCI vendor id of NVIDIA.
pub const VENDOR_NVIDIA: u32 = 0x10DE;

/// The main GPU's vendor and display-driver version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDriver {
    pub adapter: String,
    /// PCI vendor id (`VENDOR_NVIDIA` for NVIDIA).
    pub vendor: u32,
    /// The Windows driver version `a.b.c.d`, when the driver reports it.
    pub version: Option<[u16; 4]>,
}

impl GpuDriver {
    /// NVIDIA's own version number (such as 591.86) as (591, 86). NVIDIA
    /// encodes it in the last five digits of the Windows version:
    /// `32.0.15.9186` is 591.86.
    pub fn nvidia_version(&self) -> Option<(u32, u32)> {
        if self.vendor != VENDOR_NVIDIA {
            return None;
        }
        let [_, _, c, d] = self.version?;
        if d > 9999 {
            return None;
        }
        let digits = u32::from(c % 10) * 10_000 + u32::from(d);
        Some((digits / 100, digits % 100))
    }
}

/// Read the main GPU's vendor and driver version.
pub fn driver() -> Result<GpuDriver, GpuError> {
    #[cfg(windows)]
    {
        windows::driver()
    }
    #[cfg(not(windows))]
    {
        Err(GpuError::Unsupported)
    }
}

/// Read the current status of the main GPU.
pub fn report() -> Result<GpuReport, GpuError> {
    #[cfg(windows)]
    {
        windows::report()
    }
    #[cfg(not(windows))]
    {
        Err(GpuError::Unsupported)
    }
}

/// Installed and currently available system RAM, in bytes.
pub fn system_memory() -> Result<(u64, u64), GpuError> {
    #[cfg(windows)]
    {
        windows::system_memory()
    }
    #[cfg(not(windows))]
    {
        Err(GpuError::Unsupported)
    }
}

/// The PDH name of an adapter, e.g. `luid_0x00000000_0x0001D1B3`.
pub fn luid_name(high: i32, low: u32) -> String {
    format!("luid_0x{:08X}_0x{low:08X}", high as u32)
}

/// Split a PDH "GPU Process Memory" instance such as
/// `pid_1234_luid_0x00000000_0x0001D1B3_phys_0` into its process id and
/// adapter name.
pub fn parse_process_instance(instance: &str) -> Option<(u32, &str)> {
    let rest = instance.strip_prefix("pid_")?;
    let (pid, rest) = rest.split_once('_')?;
    let pid = pid.parse().ok()?;
    let luid_end = rest.find("_phys_").unwrap_or(rest.len());
    let luid = &rest[..luid_end];
    luid.starts_with("luid_0x").then_some((pid, luid))
}

/// Add one counter reading to the per-process table.
#[cfg_attr(not(windows), allow(dead_code))]
fn accumulate(
    table: &mut Vec<ProcessGpu>,
    adapter: &str,
    instance: &str,
    value: u64,
    dedicated: bool,
) {
    let Some((pid, luid)) = parse_process_instance(instance) else {
        return;
    };
    if !luid.eq_ignore_ascii_case(adapter) {
        return;
    }
    let entry = match table.iter_mut().position(|p| p.pid == pid) {
        Some(i) => &mut table[i],
        None => {
            table.push(ProcessGpu {
                pid,
                name: String::new(),
                dedicated: 0,
                shared: 0,
            });
            table.last_mut().expect("just pushed")
        }
    };
    // One process can have several instances (one per physical node).
    if dedicated {
        entry.dedicated = entry.dedicated.saturating_add(value);
    } else {
        entry.shared = entry.shared.saturating_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn nvidia_versions_come_from_the_last_five_digits() {
        let d = |vendor, version| GpuDriver {
            adapter: "GPU".into(),
            vendor,
            version,
        };
        assert_eq!(
            d(VENDOR_NVIDIA, Some([32, 0, 15, 9186])).nvidia_version(),
            Some((591, 86))
        );
        assert_eq!(
            d(VENDOR_NVIDIA, Some([32, 0, 16, 1656])).nvidia_version(),
            Some((616, 56))
        );
        assert_eq!(
            d(VENDOR_NVIDIA, Some([31, 0, 15, 5222])).nvidia_version(),
            Some((552, 22))
        );
        // Another vendor's numbers mean something else.
        assert_eq!(d(0x1002, Some([32, 0, 15, 9186])).nvidia_version(), None);
        assert_eq!(d(VENDOR_NVIDIA, None).nvidia_version(), None);
        assert_eq!(
            d(VENDOR_NVIDIA, Some([32, 0, 15, 10000])).nvidia_version(),
            None
        );
    }

    #[test]
    fn instance_names_are_parsed_strictly() {
        assert_eq!(
            parse_process_instance("pid_1234_luid_0x00000000_0x0001D1B3_phys_0"),
            Some((1234, "luid_0x00000000_0x0001D1B3"))
        );
        assert_eq!(parse_process_instance("pid_x_luid_0x0_0x1_phys_0"), None);
        assert_eq!(parse_process_instance("luid_0x0_0x1_phys_0"), None);
        assert_eq!(parse_process_instance("pid_12_engtype_3D"), None);
        assert_eq!(luid_name(0, 0x1D1B3), "luid_0x00000000_0x0001D1B3");
        assert_eq!(luid_name(-1, 1), "luid_0xFFFFFFFF_0x00000001");
    }

    #[test]
    fn readings_are_summed_per_process_on_the_chosen_adapter() {
        let a = "luid_0x00000000_0x0001D1B3";
        let mut t = Vec::new();
        accumulate(
            &mut t,
            a,
            "pid_7_luid_0x00000000_0x0001D1B3_phys_0",
            5,
            true,
        );
        accumulate(
            &mut t,
            a,
            "pid_7_luid_0x00000000_0x0001D1B3_phys_1",
            3,
            true,
        );
        accumulate(
            &mut t,
            a,
            "pid_7_luid_0x00000000_0x0001D1B3_phys_0",
            2,
            false,
        );
        // Another adapter (the integrated GPU) is ignored.
        accumulate(
            &mut t,
            a,
            "pid_8_luid_0x00000000_0x00009999_phys_0",
            9,
            true,
        );
        assert_eq!(
            t,
            vec![ProcessGpu {
                pid: 7,
                name: String::new(),
                dedicated: 8,
                shared: 2
            }]
        );
    }

    #[test]
    fn windows_processes_are_never_suggested() {
        let p = |name: &str| ProcessGpu {
            pid: 1,
            name: name.into(),
            dedicated: 1,
            shared: 0,
        };
        assert!(p("DWM.exe").is_system());
        assert!(p("").is_system());
        assert!(!p("chrome.exe").is_system());
    }

    #[test]
    fn pressure_means_little_dedicated_memory_left() {
        let mut r = GpuReport {
            adapter: "GPU".into(),
            dedicated_total: 32 * GIB,
            dedicated_used: Some(28 * GIB),
            budget: 0,
            processes: vec![],
        };
        assert!(!r.under_pressure());
        r.dedicated_used = Some(32 * GIB - 300 * 1024 * 1024);
        assert!(r.under_pressure());
        r.dedicated_used = None;
        assert!(!r.under_pressure());
    }
}
