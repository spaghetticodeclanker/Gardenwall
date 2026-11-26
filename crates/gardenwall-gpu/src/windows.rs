//! Windows backend: DXGI for the adapter, PDH for per-process memory.
//!
//! The only `unsafe` code in this crate. Each block is a single FFI call
//! (or a read of memory the call just filled), with its invariants noted.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ptr::{null, null_mut};

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter1, IDXGIAdapter3,
    IDXGIDevice, IDXGIFactory1,
};
use windows::core::Interface;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};

use crate::{GpuDriver, GpuError, GpuReport, accumulate, luid_name};

/// Installed and available system RAM in bytes (read only).
pub fn system_memory() -> Result<(u64, u64), GpuError> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is plain data; zeroed is a valid value, and
    // the call only writes into the struct whose size we pass.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: writes only into `status`, whose size is set above.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(GpuError::Os("GlobalMemoryStatusEx failed".into()));
    }
    Ok((status.ullTotalPhys, status.ullAvailPhys))
}

/// Most counter instances read (bounds the buffer PDH asks for).
const MAX_ITEMS: u32 = 16_384;

struct Adapter {
    name: String,
    luid: String,
    dedicated: u64,
    budget: u64,
    vendor: u32,
    /// The display driver's version (`a.b.c.d`), when the driver reports it.
    driver: Option<[u16; 4]>,
}

/// The main adapter's vendor and display-driver version (read only).
pub fn driver() -> Result<GpuDriver, GpuError> {
    let a = main_adapter()?;
    Ok(GpuDriver {
        adapter: a.name,
        vendor: a.vendor,
        version: a.driver,
    })
}

fn os(e: windows::core::Error) -> GpuError {
    GpuError::Os(e.message())
}

/// The hardware adapter with the most dedicated memory, which is the one
/// llama.cpp picks on a machine with integrated and discrete GPUs.
fn main_adapter() -> Result<Adapter, GpuError> {
    // SAFETY: plain factory creation; no arguments.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(os)?;
    let mut best: Option<(IDXGIAdapter1, String, String, u64, u32)> = None;
    for i in 0.. {
        // SAFETY: enumeration by index; DXGI_ERROR_NOT_FOUND ends it.
        let adapter = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(os(e)),
        };
        // SAFETY: reads the adapter's description into a returned struct.
        let desc = unsafe { adapter.GetDesc1() }.map_err(os)?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let dedicated = desc.DedicatedVideoMemory as u64;
        if best.as_ref().is_none_or(|b| dedicated > b.3) {
            let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
            let name = String::from_utf16_lossy(&desc.Description[..len]);
            let luid = luid_name(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
            best = Some((adapter, name, luid, dedicated, desc.VendorId));
        }
    }
    let (adapter, name, luid, dedicated, vendor) = best.ok_or(GpuError::NoAdapter)?;
    // The driver version: DXGI reports it when asked whether the adapter
    // supports the IDXGIDevice interface (the documented way to read it).
    // SAFETY: the interface id is a valid, static GUID; the call only
    // returns a number and creates no device.
    let driver = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
        .ok()
        .map(|v| {
            let v = v as u64;
            [
                (v >> 48) as u16,
                (v >> 32) as u16,
                (v >> 16) as u16,
                v as u16,
            ]
        });
    // Budget needs IDXGIAdapter3 (Windows 10+); without it, report 0.
    let budget = adapter
        .cast::<IDXGIAdapter3>()
        .ok()
        .and_then(|a3| {
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            // SAFETY: `info` is a valid, writable struct for the call.
            unsafe { a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
                .ok()
                .map(|()| info.Budget)
        })
        .unwrap_or(0);
    Ok(Adapter {
        name,
        luid,
        dedicated,
        budget,
        vendor,
        driver,
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A PDH query, closed on drop.
struct Query(PDH_HQUERY);

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle came from PdhOpenQueryW and is closed once.
        unsafe { PdhCloseQuery(self.0) };
    }
}

fn pdh(status: u32, what: &str) -> Result<(), GpuError> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(GpuError::Os(format!("{what} failed (0x{status:08X})")))
    }
}

impl Query {
    fn open() -> Result<Self, GpuError> {
        let mut q: PDH_HQUERY = null_mut();
        // SAFETY: no data source (live counters); `q` receives the handle.
        pdh(unsafe { PdhOpenQueryW(null(), 0, &mut q) }, "PdhOpenQuery")?;
        Ok(Self(q))
    }

    fn add(&self, path: &str) -> Result<PDH_HCOUNTER, GpuError> {
        let path = wide(path);
        let mut c: PDH_HCOUNTER = null_mut();
        // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the call.
        pdh(
            unsafe { PdhAddEnglishCounterW(self.0, path.as_ptr(), 0, &mut c) },
            "PdhAddEnglishCounter",
        )?;
        Ok(c)
    }

    fn collect(&self) -> Result<(), GpuError> {
        // SAFETY: a valid open query.
        pdh(
            unsafe { PdhCollectQueryData(self.0) },
            "PdhCollectQueryData",
        )
    }
}

/// Every (instance name, value) of a wildcard counter.
fn read_array(counter: PDH_HCOUNTER) -> Result<Vec<(String, u64)>, GpuError> {
    let mut bytes: u32 = 0;
    let mut count: u32 = 0;
    // SAFETY: a size query with a null buffer; PDH writes the sizes.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut bytes, &mut count, null_mut())
    };
    if status != PDH_MORE_DATA {
        // No instances (for example, no process uses the GPU yet).
        return if status == ERROR_SUCCESS {
            Ok(Vec::new())
        } else {
            Err(GpuError::Os(format!(
                "PdhGetFormattedCounterArray failed (0x{status:08X})"
            )))
        };
    }
    if count > MAX_ITEMS {
        return Err(GpuError::Os("too many GPU counter instances".into()));
    }
    // u64 storage keeps the buffer aligned for the item structs.
    let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
    let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    // SAFETY: `buf` holds at least `bytes` bytes, aligned for the items.
    pdh(
        unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut bytes, &mut count, items)
        },
        "PdhGetFormattedCounterArray",
    )?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: PDH wrote `count` items at the start of `buf`; their name
        // pointers point at NUL-terminated strings later in `buf`.
        let (name, value) = unsafe {
            let item = &*items.add(i);
            if item.FmtValue.CStatus != ERROR_SUCCESS {
                continue;
            }
            let mut len = 0;
            while *item.szName.add(len) != 0 {
                len += 1;
            }
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(item.szName, len));
            (name, item.FmtValue.Anonymous.largeValue)
        };
        out.push((name, u64::try_from(value).unwrap_or(0)));
    }
    Ok(out)
}

/// Executable file names of running processes, from a process snapshot.
/// Unlike opening each process, this also names protected ones (such as
/// the desktop compositor) and needs no access to them.
fn process_names() -> HashMap<u32, String> {
    let mut names = HashMap::new();
    // SAFETY: a snapshot of all processes; checked for failure below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return names;
    }
    // SAFETY: an all-zero PROCESSENTRY32W is valid; dwSize is set before use.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snap` is a valid snapshot and `entry` is sized for the call.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
    while ok != 0 {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        names.insert(
            entry.th32ProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..len]),
        );
        // SAFETY: as above.
        ok = unsafe { Process32NextW(snap, &mut entry) };
    }
    // SAFETY: the snapshot handle is ours and closed once.
    unsafe { CloseHandle(snap) };
    names
}

pub fn report() -> Result<GpuReport, GpuError> {
    let adapter = main_adapter()?;
    let mut processes = Vec::new();
    let mut dedicated_used = None;
    // Counters are optional: without them the adapter and budget remain.
    if let Ok(q) = Query::open() {
        let per_dedicated = q.add(r"\GPU Process Memory(*)\Dedicated Usage");
        let per_shared = q.add(r"\GPU Process Memory(*)\Shared Usage");
        let adapter_used = q.add(r"\GPU Adapter Memory(*)\Dedicated Usage");
        if q.collect().is_ok() {
            for (counter, dedicated) in [(per_dedicated, true), (per_shared, false)] {
                if let Ok(rows) = counter.and_then(read_array) {
                    for (instance, value) in rows {
                        accumulate(&mut processes, &adapter.luid, &instance, value, dedicated);
                    }
                }
            }
            if let Ok(rows) = adapter_used.and_then(read_array) {
                let total: u64 = rows
                    .iter()
                    .filter(|(i, _)| {
                        i.to_ascii_lowercase()
                            .starts_with(&adapter.luid.to_ascii_lowercase())
                    })
                    .map(|(_, v)| *v)
                    .sum();
                dedicated_used = Some(total);
            }
        }
    }
    processes.retain(|p| p.dedicated > 0 || p.shared > 0);
    processes.sort_by(|a, b| b.dedicated.cmp(&a.dedicated).then(a.pid.cmp(&b.pid)));
    let names = process_names();
    for p in &mut processes {
        p.name = names.get(&p.pid).cloned().unwrap_or_default();
    }
    Ok(GpuReport {
        adapter: adapter.name,
        dedicated_total: adapter.dedicated,
        dedicated_used,
        budget: adapter.budget,
        processes,
    })
}
