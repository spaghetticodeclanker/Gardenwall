//! Pinned llama.cpp runtime builds (ADR-013 §2.5).
//!
//! Every archive is pinned by URL, exact size, and SHA-256. Changing a pin is
//! a reviewed source change; nothing is resolved at runtime ("latest" is
//! never used). Digests were taken from the GitHub release API and
//! cross-checked against a local download of the Windows build in use:
//! `b12283` (Vulkan) and `b12556` (CUDA 13).
//!
//! The previous release stays pinned beside the newest, so a user who
//! installed it keeps a working runtime and can switch back from the update
//! center (ADR-017 §2.2). Pins older than that are dropped.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Vulkan,
    Cuda,
    /// Apple GPU via Metal (built into the macOS arm64 release).
    Metal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Archive {
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub kind: ArchiveKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBuild {
    /// Stable identifier, also the installation directory name.
    pub id: &'static str,
    pub version: &'static str,
    pub os: Os,
    pub arch: Arch,
    pub backend: Backend,
    /// All archives are extracted into the same runtime directory.
    pub archives: &'static [Archive],
    /// The server program's file name; `None` is llama.cpp's
    /// `llama-server`. The folder holding it (anywhere in the archives)
    /// becomes the runtime directory.
    pub server: Option<&'static str>,
}

impl RuntimeBuild {
    pub fn server_binary_name(&self) -> &'static str {
        if let Some(server) = self.server {
            return server;
        }
        match self.os {
            Os::Windows => "llama-server.exe",
            Os::MacOs | Os::Linux => "llama-server",
        }
    }

    pub fn download_size(&self) -> u64 {
        self.archives.iter().map(|a| a.size).sum()
    }

    /// The release number (`b12556` → 12556).
    pub fn build_number(&self) -> u64 {
        self.version
            .strip_prefix('b')
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
}

macro_rules! archive {
    ($tag:literal, $file:literal, $size:literal, $sha:literal, $kind:ident) => {
        Archive {
            url: concat!(
                "https://github.com/ggml-org/llama.cpp/releases/download/",
                $tag,
                "/",
                $file
            ),
            size: $size,
            sha256: $sha,
            kind: ArchiveKind::$kind,
        }
    };
}

pub const BUILDS: &[RuntimeBuild] = &[
    // ---- b12556 (2026-11-19) ----
    RuntimeBuild {
        id: "b12556-windows-x64-vulkan",
        version: "b12556",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Vulkan,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-win-vulkan-x64.zip",
            38690012,
            "73f3346fbbad14db8f5a5b35c4e9dbae7debf306309bfc77aca4a17096cb2f93",
            Zip
        )],
    },
    RuntimeBuild {
        id: "b12556-windows-x64-cuda13",
        version: "b12556",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cuda,
        server: None,
        archives: &[
            archive!(
                "b12556",
                "llama-b12556-bin-win-cuda-13.5-x64.zip",
                229295176,
                "dde6d3f922b2225574ba32fe4bad227fb4784a8a5ebabc78c69fb3da7979ae23",
                Zip
            ),
            // The same CUDA runtime archive as b12283 (same size and digest).
            archive!(
                "b12556",
                "cudart-llama-bin-win-cuda-13.5-x64.zip",
                426314199,
                "c2bf4cc202bfbaaa7a0b75f5082457c3811326947bb216059b72839d844e9cba",
                Zip
            ),
        ],
    },
    RuntimeBuild {
        id: "b12556-windows-x64-cpu",
        version: "b12556",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-win-cpu-x64.zip",
            24876692,
            "98882a3ca45688ffa585c3adba49213ca90c5b2dbbf1bc2af41ac437a0987d17",
            Zip
        )],
    },
    RuntimeBuild {
        id: "b12556-macos-arm64-metal",
        version: "b12556",
        os: Os::MacOs,
        arch: Arch::Arm64,
        backend: Backend::Metal,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-macos-arm64.tar.gz",
            16449908,
            "eb92254d5ada74c6753bd6ef0d23f498ffe2872b7222043eb48743abcaac8333",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12556-macos-x64-cpu",
        version: "b12556",
        os: Os::MacOs,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-macos-x64.tar.gz",
            12227452,
            "df7cf8208e0aa67c2dd2303a969eb67495f4c55253863a5109fa6a3d765d402b",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12556-linux-x64-vulkan",
        version: "b12556",
        os: Os::Linux,
        arch: Arch::X64,
        backend: Backend::Vulkan,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-ubuntu-vulkan-x64.tar.gz",
            36721000,
            "75ea69b93eea3abfb9539cc0bb21701cfa1f7f791e6fc30b290eab4d5dbcee4f",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12556-linux-x64-cpu",
        version: "b12556",
        os: Os::Linux,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-ubuntu-x64.tar.gz",
            23057457,
            "1a573b6089d29c9048eefe8497dd40cac4115400f223d33f2a71e506bf6d5b18",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12556-linux-arm64-cpu",
        version: "b12556",
        os: Os::Linux,
        arch: Arch::Arm64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12556",
            "llama-b12556-bin-ubuntu-arm64.tar.gz",
            18592805,
            "511c8d0bf4474b2d52a0f04b31e72f8c86e3ab32e723322f1c412fd18fe49d9e",
            TarGz
        )],
    },
    // ---- b12283 (2026-11-11), the previous release ----
    RuntimeBuild {
        id: "b12283-windows-x64-vulkan",
        version: "b12283",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Vulkan,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-win-vulkan-x64.zip",
            38022362,
            "58c3426f7473bc08135c83333259366521fc940e4ad7bc6ad2b26755cc42cb03",
            Zip
        )],
    },
    RuntimeBuild {
        id: "b12283-windows-x64-cuda13",
        version: "b12283",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cuda,
        server: None,
        archives: &[
            archive!(
                "b12283",
                "llama-b12283-bin-win-cuda-13.5-x64.zip",
                171031089,
                "36b73a06d276b369d882ce7ffcfb4eab053f0f65cd394590ed5161b8b9eb8d5f",
                Zip
            ),
            archive!(
                "b12283",
                "cudart-llama-bin-win-cuda-13.5-x64.zip",
                426314199,
                "c2bf4cc202bfbaaa7a0b75f5082457c3811326947bb216059b72839d844e9cba",
                Zip
            ),
        ],
    },
    RuntimeBuild {
        id: "b12283-windows-x64-cpu",
        version: "b12283",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-win-cpu-x64.zip",
            25058174,
            "dafb19ac757bfb7cfc1f91c6e26703264df861c4331e3ef6dc9543ae8d010c5e",
            Zip
        )],
    },
    RuntimeBuild {
        id: "b12283-macos-arm64-metal",
        version: "b12283",
        os: Os::MacOs,
        arch: Arch::Arm64,
        backend: Backend::Metal,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-macos-arm64.tar.gz",
            16870708,
            "bbc2abe2001293687540fb274bf4114b27b56af1fc166ee76730f64996d68b00",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12283-macos-x64-cpu",
        version: "b12283",
        os: Os::MacOs,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-macos-x64.tar.gz",
            12613034,
            "7179905c572cd11e9eadf9669058b49846cc9519f3d8d10497cad19fbf9eb8b0",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12283-linux-x64-vulkan",
        version: "b12283",
        os: Os::Linux,
        arch: Arch::X64,
        backend: Backend::Vulkan,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-ubuntu-vulkan-x64.tar.gz",
            36106160,
            "34138a7d99f2cc4b2be8b6978a656eb77240e742ba0870a58d418024c6e2e8b8",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12283-linux-x64-cpu",
        version: "b12283",
        os: Os::Linux,
        arch: Arch::X64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-ubuntu-x64.tar.gz",
            23283834,
            "5f24a62b84a37c186efd1e3e1d77bfad3a1772d9aa35bfcc9e1572723d0c4faf",
            TarGz
        )],
    },
    RuntimeBuild {
        id: "b12283-linux-arm64-cpu",
        version: "b12283",
        os: Os::Linux,
        arch: Arch::Arm64,
        backend: Backend::Cpu,
        server: None,
        archives: &[archive!(
            "b12283",
            "llama-b12283-bin-ubuntu-arm64.tar.gz",
            18926106,
            "2e44d8517120793b0227d28012a794d114a7d4ad7c3bead4a62b6f3f78b469e7",
            TarGz
        )],
    },
];

/// A published security problem in a range of llama.cpp releases.
///
/// Adding one is a reviewed source change, like a pin. The range is by
/// release number: `from` is the first affected release and `fixed` the
/// first release with the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advisory {
    pub id: &'static str,
    /// One plain sentence for the update center.
    pub summary: &'static str,
    pub from: u64,
    pub fixed: u64,
    pub source: &'static str,
}

/// Known advisories that touch a release Gardenwall pins or has pinned.
/// The sandbox contains the runtime either way; this list is so the user
/// is told to move to the fixed build.
pub const ADVISORIES: &[Advisory] = &[Advisory {
    id: "CVE-2026-133710",
    summary: "A crafted model file can write outside its memory while loading (hrm_text model loader).",
    from: 11003,
    fixed: 12556,
    source: "https://www.vulncheck.com/advisories/llama-cpp-b11003-before-b12556-heap-out-of-bounds-write-via-hrm-text-model-loader",
}];

/// The advisories that apply to a release number.
pub fn advisories_for(build_number: u64) -> Vec<&'static Advisory> {
    ADVISORIES
        .iter()
        .filter(|a| (a.from..a.fixed).contains(&build_number))
        .collect()
}

/// The platform this binary was compiled for, if supported.
pub fn current_platform() -> Option<(Os, Arch)> {
    let os = if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        Arch::X64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else {
        return None;
    };
    Some((os, arch))
}

/// Builds available for a platform, in preference order: GPU first, and
/// within a backend the newest release first.
pub fn builds_for(os: Os, arch: Arch) -> Vec<&'static RuntimeBuild> {
    let rank = |b: &RuntimeBuild| match b.backend {
        Backend::Cuda => 0,
        Backend::Metal => 0,
        Backend::Vulkan => 1,
        Backend::Cpu => 2,
    };
    let mut builds: Vec<_> = BUILDS
        .iter()
        .filter(|b| b.os == os && b.arch == arch)
        .collect();
    builds.sort_by_key(|b| (rank(b), std::cmp::Reverse(b.build_number())));
    builds
}

pub fn find(id: &str) -> Option<&'static RuntimeBuild> {
    BUILDS.iter().find(|b| b.id == id)
}

/// whisper.cpp builds for voice input (release plan feature 7), pinned the
/// same way, from the GitHub release API's SHA-256 digests (b5907,
/// 2026-11-20), cross-checked against a local download. The CUDA 12.4
/// build has no native kernels for compute capability 12.0 (RTX 50
/// series): the driver compiles them on first use (about 27 s measured on
/// an RTX 5090) and caches them, so the sandbox gets a cache folder.
pub const WHISPER_BUILDS: &[RuntimeBuild] = &[
    RuntimeBuild {
        id: "whisper-b5907-windows-x64-cuda12",
        version: "b5907",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cuda,
        archives: &[Archive {
            url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5907/whisper-bin-win-cuda-12.4.0-x64.zip",
            size: 735571214,
            sha256: "34f872ce724ca1ff0a4afca481853e5ee74250f8cb22153e499f953562846039",
            kind: ArchiveKind::Zip,
        }],
        server: Some("whisper-server.exe"),
    },
    RuntimeBuild {
        id: "whisper-b5907-windows-x64-cpu",
        version: "b5907",
        os: Os::Windows,
        arch: Arch::X64,
        backend: Backend::Cpu,
        archives: &[Archive {
            url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5907/whisper-bin-x64.zip",
            size: 9168428,
            sha256: "ecca07b277d943e9b9f4712856a144b857271a6b333104e511e1988e80db582b",
            kind: ArchiveKind::Zip,
        }],
        server: Some("whisper-server.exe"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://github.com/ggml-org/llama.cpp/releases/download/";

    #[test]
    fn pins_are_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for build in BUILDS {
            assert!(ids.insert(build.id), "duplicate id {}", build.id);
            assert!(
                build
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                "{}",
                build.id
            );
            assert!(build.id.starts_with(&format!("{}-", build.version)));
            assert!(build.build_number() > 0, "{}", build.version);
            assert!(!build.archives.is_empty());
            for a in build.archives {
                // Each archive comes from its own build's release.
                let base = format!("{BASE}{}/", build.version);
                assert!(a.url.starts_with(&base), "{}", a.url);
                assert_eq!(a.sha256.len(), 64);
                assert!(
                    a.sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                );
                assert!(a.size > 1_000_000);
                let zip = a.url.ends_with(".zip");
                assert_eq!(zip, a.kind == ArchiveKind::Zip);
            }
        }
    }

    #[test]
    fn the_newest_release_has_no_known_advisory() {
        let newest = BUILDS.iter().map(|b| b.build_number()).max().unwrap();
        assert!(advisories_for(newest).is_empty());
        for a in ADVISORIES {
            assert!(a.from < a.fixed, "{}", a.id);
            assert!(a.source.starts_with("https://"), "{}", a.id);
            assert!(!a.summary.is_empty());
            assert!(advisories_for(a.from).contains(&a));
            assert!(!advisories_for(a.fixed).contains(&a));
        }
        // The previous release is inside CVE-2026-133710's range.
        assert_eq!(advisories_for(12283).len(), 1);
    }

    #[test]
    fn every_release_covers_the_same_platforms() {
        let mut versions: Vec<&str> = BUILDS.iter().map(|b| b.version).collect();
        versions.dedup();
        assert_eq!(versions.len(), 2, "the newest and the previous release");
        let shape = |v: &str| {
            BUILDS
                .iter()
                .filter(|b| b.version == v)
                .map(|b| (b.os, b.arch, b.backend))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(versions[0]), shape(versions[1]));
    }

    #[test]
    fn preference_order_puts_gpu_and_newest_first() {
        let windows = builds_for(Os::Windows, Arch::X64);
        let order: Vec<_> = windows.iter().map(|b| (b.backend, b.version)).collect();
        assert_eq!(
            order,
            [
                (Backend::Cuda, "b12556"),
                (Backend::Cuda, "b12283"),
                (Backend::Vulkan, "b12556"),
                (Backend::Vulkan, "b12283"),
                (Backend::Cpu, "b12556"),
                (Backend::Cpu, "b12283"),
            ]
        );
        assert_eq!(
            builds_for(Os::MacOs, Arch::Arm64)[0].backend,
            Backend::Metal
        );
        assert!(find("b12283-windows-x64-vulkan").is_some());
        assert!(find("b12556-windows-x64-cuda13").is_some());
        assert!(find("latest").is_none());
    }
}
