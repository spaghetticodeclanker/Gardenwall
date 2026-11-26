//! Microsoft's Visual C++ runtime, which the pinned llama.cpp and whisper.cpp
//! builds load from Windows (a fresh Windows doesn't always have it). Owner
//! decision 2026-11-24: Gardenwall doesn't ship or install it; it checks for
//! it and asks the user to install Microsoft's package.

/// The files the engines import, all from the same Microsoft package
/// (`vcomp140.dll` is whisper.cpp's OpenMP runtime).
pub const FILES: [&str; 4] = [
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "msvcp140.dll",
    "vcomp140.dll",
];

/// Where to get it, shown to the user (the app never opens web addresses).
pub const DOWNLOAD_PAGE: &str =
    "https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist";
pub const DOWNLOAD_FILE: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// The runtime files missing from Windows' System32 folder (empty when it's
/// installed, and always on systems other than Windows).
pub fn missing() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        missing_in(&std::path::Path::new(&root).join("System32"))
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// The runtime files missing from `dir`.
pub fn missing_in(dir: &std::path::Path) -> Vec<&'static str> {
    FILES
        .into_iter()
        .filter(|f| !dir.join(f).is_file())
        .collect()
}

/// What to tell the user when it's missing.
pub fn message(missing: &[&str]) -> String {
    format!(
        "This PC is missing Microsoft's Visual C++ Redistributable ({}), which Gardenwall's engine needs to run models. Install \"Microsoft Visual C++ Redistributable for Visual Studio, x64\" from Microsoft ({DOWNLOAD_FILE}, listed on {DOWNLOAD_PAGE}), then try again. You don't need to restart Gardenwall.",
        missing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(missing_in(dir.path()), FILES.to_vec());
        for f in FILES {
            std::fs::write(dir.path().join(f), b"").unwrap();
        }
        assert!(missing_in(dir.path()).is_empty());
        std::fs::remove_file(dir.path().join("vcomp140.dll")).unwrap();
        assert_eq!(missing_in(dir.path()), ["vcomp140.dll"]);
        assert!(message(&["vcomp140.dll"]).contains("vcomp140.dll"));
    }

    #[cfg(windows)]
    #[test]
    fn this_machine_has_it() {
        // The build machine has Visual Studio, so the runtime is installed.
        assert!(missing().is_empty(), "{:?}", missing());
    }
}
