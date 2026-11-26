//! GPU driver freshness (security review 2026-11-20).
//!
//! The model runtime is sandboxed, but it still talks to the GPU driver,
//! and driver bugs are how one program reads another's GPU memory. That is
//! outside what Gardenwall can contain, so the one useful thing it can do
//! is tell the user when the installed driver is older than the vendor's
//! latest security fix.
//!
//! The fixed versions are a dated data file read from the vendor's own
//! bulletin (`data/gpu-driver-advisories.json`). Nothing is fetched: the
//! check compares the installed driver's number with that file. NVIDIA on
//! Windows only for now; other vendors and platforms report nothing.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const DATA_JSON: &str = include_str!("../data/gpu-driver-advisories.json");

#[derive(Debug, Deserialize)]
struct Data {
    reviewed: String,
    nvidia: Vendor,
}

#[derive(Debug, Deserialize)]
struct Vendor {
    bulletin: String,
    source: String,
    summary: String,
    branches: Vec<Branch>,
}

/// One driver branch: major versions `from..before`, fixed from `fixed`.
/// (The file also names each branch, for people reading it.)
#[derive(Debug, Deserialize)]
struct Branch {
    from: u32,
    before: u32,
    fixed: String,
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    // Checked by tests; a broken file can only come from a bad build.
    DATA.get_or_init(|| serde_json::from_str(DATA_JSON).expect("gpu-driver-advisories.json parses"))
}

/// `"582.78"` as (582, 78).
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Shown on the Updates page when the driver predates the latest fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverNotice {
    pub adapter: String,
    pub installed: String,
    /// The first fixed driver to install (in the same branch when the
    /// branch still gets fixes, otherwise the oldest fixed one).
    pub fixed: String,
    pub bulletin: String,
    pub source: String,
    pub summary: String,
    /// When the list of fixed versions was last read from the vendor.
    pub reviewed: String,
}

/// Whether an NVIDIA driver `installed` predates the fix for its branch.
/// A driver older than every listed branch is out of date; one in a
/// branch newer than every listed branch is taken as current.
fn nvidia_fix_for(installed: (u32, u32)) -> Option<&'static str> {
    let branches = &data().nvidia.branches;
    if let Some(b) = branches
        .iter()
        .find(|b| (b.from..b.before).contains(&installed.0))
    {
        let fixed = parse_version(&b.fixed)?;
        return (installed < fixed).then_some(b.fixed.as_str());
    }
    let oldest = branches.iter().min_by_key(|b| b.from)?;
    (installed.0 < oldest.from).then_some(oldest.fixed.as_str())
}

fn notice_for(driver: &gardenwall_gpu::GpuDriver) -> Option<DriverNotice> {
    let installed = driver.nvidia_version()?;
    let fixed = nvidia_fix_for(installed)?;
    let d = data();
    Some(DriverNotice {
        adapter: driver.adapter.chars().take(80).collect(),
        installed: format!("{}.{:02}", installed.0, installed.1),
        fixed: fixed.to_string(),
        bulletin: d.nvidia.bulletin.clone(),
        source: d.nvidia.source.clone(),
        summary: d.nvidia.summary.clone(),
        reviewed: d.reviewed.clone(),
    })
}

/// An NVIDIA driver that has the latest listed fix, so the Updates page can
/// say it was checked (a missing line looked like
/// a missing feature).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverCurrent {
    pub adapter: String,
    pub installed: String,
    pub bulletin: String,
    pub reviewed: String,
}

fn current_for(driver: &gardenwall_gpu::GpuDriver) -> Option<DriverCurrent> {
    let installed = driver.nvidia_version()?;
    if nvidia_fix_for(installed).is_some() {
        return None;
    }
    let d = data();
    Some(DriverCurrent {
        adapter: driver.adapter.chars().take(80).collect(),
        installed: format!("{}.{:02}", installed.0, installed.1),
        bulletin: d.nvidia.bulletin.clone(),
        reviewed: d.reviewed.clone(),
    })
}

/// This computer's main GPU driver, compared with the latest fix: a notice
/// when it is out of date, or the up-to-date facts. Reads the driver
/// version (a quick, read-only DXGI call); both `None` when the vendor
/// isn't covered or the version can't be read.
pub fn status() -> (Option<DriverNotice>, Option<DriverCurrent>) {
    match read_driver() {
        Some(driver) => (notice_for(&driver), current_for(&driver)),
        None => (None, None),
    }
}

fn read_driver() -> Option<gardenwall_gpu::GpuDriver> {
    // A hardware read: kept off the async runtime's worker threads, like
    // the GPU memory report.
    let query = || gardenwall_gpu::driver().ok();
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(query)
        }
        _ => query(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gardenwall_gpu::{GpuDriver, VENDOR_NVIDIA};

    #[test]
    fn the_data_file_is_well_formed() {
        let d = data();
        assert!(
            d.nvidia
                .source
                .starts_with("https://github.com/NVIDIA/product-security/")
        );
        assert!(!d.nvidia.bulletin.is_empty() && !d.nvidia.summary.is_empty());
        assert_eq!(d.reviewed.len(), 10);
        let mut end = 0;
        for b in &d.nvidia.branches {
            // Branches are in order and don't overlap.
            assert!(b.from >= end && b.from < b.before, "{}", b.fixed);
            end = b.before;
            let fixed = parse_version(&b.fixed).expect("a version");
            assert!((b.from..b.before).contains(&fixed.0), "{}", b.fixed);
        }
    }

    #[test]
    fn a_driver_is_compared_with_the_fix_for_its_own_branch() {
        // Same branch: older, exactly fixed, newer.
        assert_eq!(nvidia_fix_for((610, 47)), Some("610.88"));
        assert_eq!(nvidia_fix_for((610, 88)), None);
        assert_eq!(nvidia_fix_for((611, 2)), None);
        assert_eq!(nvidia_fix_for((596, 36)), Some("596.86"));
        assert_eq!(nvidia_fix_for((582, 78)), None);
        // Older than every listed branch: out of date.
        assert_eq!(nvidia_fix_for((552, 22)), Some("582.78"));
        // A branch newer than the file knows: taken as current.
        assert_eq!(nvidia_fix_for((640, 1)), None);
    }

    #[test]
    fn only_nvidia_drivers_with_a_readable_version_get_a_notice() {
        let old = GpuDriver {
            adapter: "NVIDIA GeForce RTX 5090".into(),
            vendor: VENDOR_NVIDIA,
            version: Some([32, 0, 16, 1047]),
        };
        let n = notice_for(&old).unwrap();
        assert_eq!(
            (n.installed.as_str(), n.fixed.as_str()),
            ("610.47", "610.88")
        );
        let other = GpuDriver {
            vendor: 0x1002,
            ..old.clone()
        };
        assert_eq!(notice_for(&other), None);
        let unreadable = GpuDriver {
            version: None,
            ..old
        };
        assert_eq!(notice_for(&unreadable), None);
        assert_eq!(current_for(&unreadable), None);
    }

    #[test]
    fn a_driver_with_the_fix_is_reported_as_current() {
        let fixed = GpuDriver {
            adapter: "NVIDIA GeForce RTX 5090".into(),
            vendor: VENDOR_NVIDIA,
            version: Some([32, 0, 16, 1088]),
        };
        assert_eq!(notice_for(&fixed), None);
        assert_eq!(current_for(&fixed).unwrap().installed, "610.88");
        let old = GpuDriver {
            version: Some([32, 0, 16, 1047]),
            ..fixed
        };
        assert_eq!(current_for(&old), None);
    }
}
