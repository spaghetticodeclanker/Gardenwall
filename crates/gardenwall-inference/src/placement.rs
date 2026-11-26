//! Where a loaded model's weights ended up (GPU memory or system RAM), read
//! from the runtime's own load log.
//!
//! With `--fit`, llama.cpp decides the split itself: for a model larger than
//! device memory it keeps attention and dense weights on the GPU and moves
//! the expert tensors of overflowing layers to system RAM. Gardenwall only
//! reports the result so the user knows why generation is slower.
//!
//! The log comes from an untrusted process. Lines are matched against fixed
//! shapes, numbers are bounded, and the result is display-only: nothing here
//! affects isolation or what the runtime may access.

/// Facts gathered from the load log, all optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placement {
    /// Model weights in device memory (MiB).
    pub gpu_mib: f64,
    /// Model weights in system RAM (MiB): CPU buffers and pinned host memory.
    pub ram_mib: f64,
    /// `offloaded X/Y layers to GPU`.
    pub layers: Option<(u32, u32)>,
    /// Layers whose expert weights `--fit` moved to system RAM.
    pub overflowing_layers: Option<u32>,
}

/// Largest buffer size believed (16 TiB in MiB); anything larger is ignored.
const MAX_MIB: f64 = 16.0 * 1024.0 * 1024.0;

impl Placement {
    /// Take one sanitized log line into account.
    pub fn observe(&mut self, line: &str) {
        if let Some((name, mib)) = model_buffer(line) {
            if is_host_buffer(name) {
                self.ram_mib += mib;
            } else {
                self.gpu_mib += mib;
            }
        } else if let Some(layers) = offloaded_layers(line) {
            // The main model loads first; a draft model's line comes later.
            self.layers.get_or_insert(layers);
        } else if let Some(n) = overflowing(line) {
            *self.overflowing_layers.get_or_insert(0) += n;
        }
    }

    /// Whether a meaningful share of the weights runs from system RAM. Token
    /// embeddings always stay in RAM, so small amounts don't count.
    pub fn partly_in_ram(&self) -> bool {
        self.overflowing_layers.is_some_and(|n| n > 0)
            || self.layers.is_some_and(|(on_gpu, total)| on_gpu < total)
            || (self.ram_mib > 2048.0 && self.ram_mib > self.gpu_mib * 0.1)
    }
}

/// `load_tensors:        CUDA0 model buffer size = 24019.53 MiB`
fn model_buffer(line: &str) -> Option<(&str, f64)> {
    // Lines may carry a timestamp and level first: `0.01.912 I load_tensors: ...`.
    let rest = line.split_once("load_tensors:")?.1;
    let (name, size) = rest.split_once(" model buffer size = ")?;
    let name = name.trim();
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    let mib: f64 = size.trim().strip_suffix("MiB")?.trim().parse().ok()?;
    (mib.is_finite() && (0.0..=MAX_MIB).contains(&mib)).then_some((name, mib))
}

/// CPU buffers (`CPU`, `CPU_Mapped`, `CPU_REPACK`) and pinned host memory
/// (`CUDA_Host`) are system RAM; device buffers (`CUDA0`, `Vulkan0`,
/// `ROCm0`, `Metal`) are GPU memory.
fn is_host_buffer(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("CPU") || upper.ends_with("_HOST")
}

/// `load_tensors: offloaded 49/49 layers to GPU`
fn offloaded_layers(line: &str) -> Option<(u32, u32)> {
    let rest = line.split_once("offloaded ")?.1;
    let (counts, tail) = rest.split_once(' ')?;
    if !tail.starts_with("layers to GPU") {
        return None;
    }
    let (on, total) = counts.split_once('/')?;
    let (on, total): (u32, u32) = (on.parse().ok()?, total.parse().ok()?);
    (on <= total && total <= 10_000).then_some((on, total))
}

/// `... CUDA0 (...): 48 layers (34 overflowing), 19187 MiB used, ...`
/// (llama.cpp's fitter, one line per device).
fn overflowing(line: &str) -> Option<u32> {
    let before = line.split_once(" overflowing)")?.0;
    let digits = before.rsplit_once('(')?.1.trim();
    let n: u32 = digits.parse().ok()?;
    (n <= 10_000).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_moe_load_log() {
        let mut p = Placement::default();
        for line in [
            "llama_params_fit_impl: distributing layers across devices with overflow to next device/system memory:",
            "llama_params_fit_impl:   - CUDA0 (NVIDIA GeForce RTX 5090): 48 layers (34 overflowing), 26187 MiB used, 3199 MiB free",
            "load_tensors: offloaded 49/49 layers to GPU",
            "load_tensors:        CUDA0 model buffer size = 24019.53 MiB",
            "0.01.912.088 I load_tensors:   CPU_Mapped model buffer size = 20480.25 MiB",
            "load_tensors:    CUDA_Host model buffer size =   512.00 MiB",
            // A draft model loading afterwards doesn't replace the main counts.
            "load_tensors: offloaded 29/29 layers to GPU",
        ] {
            p.observe(line);
        }
        assert_eq!(p.layers, Some((49, 49)));
        assert_eq!(p.overflowing_layers, Some(34));
        assert!((p.gpu_mib - 24019.53).abs() < 0.01);
        assert!((p.ram_mib - 20992.25).abs() < 0.01);
        assert!(p.partly_in_ram());
    }

    #[test]
    fn a_model_that_fits_is_not_reported() {
        let mut p = Placement::default();
        for line in [
            "load_tensors: offloaded 65/65 layers to GPU",
            "load_tensors:        CUDA0 model buffer size = 20500.00 MiB",
            "load_tensors:   CPU_Mapped model buffer size =   782.00 MiB",
        ] {
            p.observe(line);
        }
        assert!(!p.partly_in_ram());
        // Layers left on the CPU (no fitting, dense model) are reported.
        p.layers = Some((40, 65));
        assert!(p.partly_in_ram());
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let mut p = Placement::default();
        for line in [
            "load_tensors: bad name! model buffer size = 10.00 MiB",
            "load_tensors: CUDA0 model buffer size = NaN MiB",
            "load_tensors: CUDA0 model buffer size = 1e30 MiB",
            "load_tensors: CUDA0 model buffer size = -5 MiB",
            "something else: CUDA0 model buffer size = 10.00 MiB",
            "I llama_kv_cache:      CUDA0 KV buffer size =  7456.00 MiB",
            "offloaded 50/49 layers to GPU",
            "offloaded 3/4 layers somewhere",
            "48 layers (lots overflowing)",
        ] {
            p.observe(line);
        }
        assert_eq!(p, Placement::default());
    }
}
