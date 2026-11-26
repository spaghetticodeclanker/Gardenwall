//! Generation slowdown detection (ROADMAP Phase 3, "VRAM slowdown warning").
//!
//! When other apps take GPU memory after a model has loaded, the Windows
//! driver silently moves part of the model to system memory. Generation
//! then runs 4–9× slower (ADR-015 §2.1). Two signals are watched:
//! - the speed of each reply against the best seen since the model loaded;
//! - the GPU's free dedicated memory (`gardenwall-gpu`), checked at most
//!   every [`GPU_CHECK_INTERVAL`], so the user hears before speed drops.
//!
//! Warnings name the apps holding the most GPU memory, leaving out
//! Windows' own processes, the model and Gardenwall.
//!
//! Longer contexts are slower too, but by far less than paging, so a reply
//! has to fall below [`SLOW_RATIO`] of the baseline to count. Short replies
//! are ignored because their speed is noisy. The warning is given once,
//! and again only after speed has recovered.

use std::time::{Duration, Instant};

use gardenwall_gpu::GpuReport;
use serde::Serialize;

/// Replies shorter than this many generated tokens are ignored.
const MIN_TOKENS: u64 = 32;
/// Replies needed before a baseline exists.
const WARMUP: u32 = 2;
/// Slower than this share of the baseline counts as a slowdown.
const SLOW_RATIO: f64 = 0.4;
/// Faster than this share of the baseline re-arms the warning.
const RECOVERED_RATIO: f64 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slowdown {
    /// Best generation speed since the model loaded (tokens/s).
    pub baseline: f64,
    /// Speed of the reply that triggered the warning (tokens/s).
    pub current: f64,
}

#[derive(Debug, Default)]
pub struct SpeedWatch {
    baseline: f64,
    samples: u32,
    warned: bool,
}

impl SpeedWatch {
    /// Record one reply's generation speed. Returns a slowdown to report.
    pub fn observe(&mut self, tokens_per_second: f64, generated: u64) -> Option<Slowdown> {
        if generated < MIN_TOKENS || !tokens_per_second.is_finite() || tokens_per_second <= 0.0 {
            return None;
        }
        self.samples = self.samples.saturating_add(1);
        if self.samples <= WARMUP || tokens_per_second > self.baseline {
            self.baseline = self.baseline.max(tokens_per_second);
            if tokens_per_second >= self.baseline * RECOVERED_RATIO {
                self.warned = false;
            }
            return None;
        }
        if tokens_per_second >= self.baseline * RECOVERED_RATIO {
            self.warned = false;
            return None;
        }
        if tokens_per_second < self.baseline * SLOW_RATIO && !self.warned {
            self.warned = true;
            return Some(Slowdown {
                baseline: self.baseline,
                current: tokens_per_second,
            });
        }
        None
    }
}

/// Pressure checks run at most this often (they read OS counters).
const GPU_CHECK_INTERVAL: Duration = Duration::from_secs(20);
/// Apps holding less than this are not worth naming.
const MIN_APP_BYTES: u64 = 64 * 1024 * 1024;
/// Most apps named in a warning.
const MAX_APPS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuApp {
    pub name: String,
    pub bytes: u64,
}

/// The GPU memory picture shown in a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuView {
    pub adapter: String,
    pub total_bytes: u64,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// GPU memory held by the model's runtime.
    pub model_bytes: Option<u64>,
    /// Other apps using the most GPU memory, largest first. Windows' own
    /// processes and Gardenwall are left out.
    pub apps: Vec<GpuApp>,
}

pub fn gpu_view(r: &GpuReport, model_pid: Option<u32>) -> GpuView {
    let own = std::process::id();
    let apps = r
        .processes
        .iter()
        .filter(|p| Some(p.pid) != model_pid && p.pid != own && !p.is_system())
        .filter(|p| p.dedicated >= MIN_APP_BYTES)
        .take(MAX_APPS)
        .map(|p| GpuApp {
            name: p.name.clone(),
            bytes: p.dedicated,
        })
        .collect();
    GpuView {
        adapter: r.adapter.clone(),
        total_bytes: r.dedicated_total,
        used_bytes: r.dedicated_used,
        free_bytes: r.free(),
        model_bytes: model_pid
            .and_then(|pid| r.process(pid))
            .map(|p| p.dedicated),
        apps,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WarningKind {
    /// Replies became much slower.
    Slowdown,
    /// The GPU's own memory is (nearly) full, so the model may be pushed
    /// into system memory soon.
    Pressure,
}

/// A warning for the user about generation speed or GPU memory.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuWarning {
    pub kind: WarningKind,
    pub baseline: Option<f64>,
    pub current: Option<f64>,
    /// Absent when GPU status can't be read (other platforms, no access).
    pub gpu: Option<GpuView>,
}

/// Decides when GPU memory pressure is worth a warning.
#[derive(Debug, Default)]
pub struct GpuWatch {
    last_check: Option<Instant>,
    warned: bool,
}

impl GpuWatch {
    /// Whether a pressure check is due now.
    pub fn due(&mut self, now: Instant) -> bool {
        if self
            .last_check
            .is_some_and(|t| now.duration_since(t) < GPU_CHECK_INTERVAL)
        {
            return false;
        }
        self.last_check = Some(now);
        true
    }

    /// Warn once per episode of pressure.
    pub fn pressure(&mut self, under_pressure: bool) -> bool {
        let warn = under_pressure && !self.warned;
        self.warned = under_pressure;
        warn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gardenwall_gpu::ProcessGpu;

    #[test]
    fn pressure_is_checked_sparingly_and_warned_once() {
        let mut w = GpuWatch::default();
        let t0 = Instant::now();
        assert!(w.due(t0));
        assert!(!w.due(t0 + Duration::from_secs(5)));
        assert!(w.due(t0 + Duration::from_secs(21)));
        assert!(!w.pressure(false));
        assert!(w.pressure(true));
        assert!(!w.pressure(true));
        assert!(!w.pressure(false));
        assert!(w.pressure(true));
    }

    #[test]
    fn warnings_name_other_apps_not_windows_or_the_model() {
        let gib = 1u64 << 30;
        let p = |pid: u32, name: &str, dedicated: u64| ProcessGpu {
            pid,
            name: name.into(),
            dedicated,
            shared: 0,
        };
        let r = GpuReport {
            adapter: "RTX".into(),
            dedicated_total: 32 * gib,
            dedicated_used: Some(31 * gib),
            budget: 0,
            processes: vec![
                p(1, "dwm.exe", 19 * gib),
                p(2, "llama-server.exe", 22 * gib),
                p(3, "Game.exe", 6 * gib),
                p(4, "chrome.exe", gib),
                p(5, "tiny.exe", 1024),
            ],
        };
        let v = gpu_view(&r, Some(2));
        assert_eq!(v.model_bytes, Some(22 * gib));
        assert_eq!(v.free_bytes, Some(gib));
        let names: Vec<&str> = v.apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Game.exe", "chrome.exe"]);
    }

    #[test]
    fn warns_once_on_a_sharp_drop_and_rearms_after_recovery() {
        let mut w = SpeedWatch::default();
        assert_eq!(w.observe(54.0, 200), None);
        assert_eq!(w.observe(55.0, 200), None);
        // Long-context slowdowns stay quiet.
        assert_eq!(w.observe(38.0, 200), None);
        // Short replies are ignored.
        assert_eq!(w.observe(5.0, 10), None);
        assert_eq!(
            w.observe(12.5, 300),
            Some(Slowdown {
                baseline: 55.0,
                current: 12.5
            })
        );
        assert_eq!(w.observe(12.0, 300), None);
        assert_eq!(w.observe(50.0, 300), None);
        assert!(w.observe(11.0, 300).is_some());
    }

    #[test]
    fn no_warning_before_a_baseline_exists() {
        let mut w = SpeedWatch::default();
        assert_eq!(w.observe(12.0, 100), None);
        assert_eq!(w.observe(f64::NAN, 100), None);
        assert_eq!(w.observe(13.0, 100), None);
        assert_eq!(w.observe(12.5, 100), None);
    }
}
