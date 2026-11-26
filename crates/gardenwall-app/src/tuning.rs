//! Suggested settings for a model on this computer
//! (the user keeps full control, and Gardenwall can propose a
//! good configuration with the reasons for each choice).
//!
//! Nothing here is applied on its own: the Models page shows the suggestion
//! and the user applies it, or not. The suggestion only uses facts the core
//! already trusts: the model's size and GGUF header facts from intake, and
//! read-only GPU and RAM figures.

use serde::Serialize;

use crate::library::{KvCache, ModelRecord, Speculation, Thinking};

const GIB: u64 = 1 << 30;

/// GPU memory kept free: the `--fit` margin plus the desktop's own use.
const RESERVED_VRAM: u64 = 3 * GIB + GIB;
/// Room for the smallest context and the compute buffers.
const MIN_RUNTIME_OVERHEAD: u64 = 3 * GIB / 2;
/// System RAM left for Windows and other apps.
const RESERVED_RAM: u64 = 6 * GIB;
/// The expert cache suggested for a MoE model that doesn't fit. Measured on a
/// 63 GiB MoE on a 32 GiB RTX 5090 (32K context): generation 51 -> 65 tok/s
/// with 4 GiB, prompt reading 57 -> 36 tok/s.
const SUGGESTED_MOE_CACHE_MIB: u32 = 4096;

/// What Gardenwall knows about this computer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hardware {
    pub gpu_name: Option<String>,
    pub vram_total: Option<u64>,
    pub ram_total: Option<u64>,
}

impl Hardware {
    pub fn detect() -> Self {
        let gpu = gardenwall_gpu::report().ok();
        Self {
            gpu_name: gpu.as_ref().map(|g| g.adapter.clone()),
            vram_total: gpu.map(|g| g.dedicated_total),
            ram_total: gardenwall_gpu::system_memory().ok().map(|(t, _)| t),
        }
    }

    /// The same computer with a GPU memory figure the user entered ("Plan
    /// for a different amount of GPU memory"). Labels only: loading still
    /// measures the real memory.
    pub fn planned(mut self, vram_gib: Option<u32>) -> Self {
        if let Some(g) = vram_gib {
            self.vram_total = Some(u64::from(g) * GIB);
        }
        self
    }

    pub fn describe(&self) -> String {
        let gib = |b: u64| format!("{:.1} GiB", b as f64 / GIB as f64);
        let gpu = match (&self.gpu_name, self.vram_total) {
            (Some(n), Some(v)) => format!("{n} ({} of GPU memory)", gib(v)),
            _ => "GPU memory unknown".into(),
        };
        match self.ram_total {
            Some(r) => format!("{gpu} · {} of system RAM", gib(r)),
            None => gpu,
        }
    }
}

/// How the model compares with this computer's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// Everything fits in GPU memory.
    Gpu,
    /// Part of the model runs from system RAM.
    Partly,
    /// Larger than GPU memory and RAM together (it may not load).
    TooBig,
    /// GPU memory couldn't be read.
    Unknown,
}

/// One explained choice.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    /// Which setting this explains ("context", "kvCache", "speculation",
    /// "expertCache" or "overall").
    pub setting: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub hardware: String,
    pub fit: Fit,
    /// `None`: automatic (the largest context that fits).
    pub context_size: Option<u32>,
    pub kv_cache: KvCache,
    pub speculation: Speculation,
    pub moe_cache_mib: u32,
    pub thinking: Thinking,
    pub notes: Vec<Note>,
}

/// Where a model of a given size would run here: the one size rule, used by
/// `suggest` after download and by the model picker before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FitView {
    pub fit: Fit,
    /// It fits, but uses over 70 % of the usable GPU memory, so little is
    /// left for the conversation.
    pub tight: bool,
}

/// The fit of `weights` bytes (the model plus any projector) on `hw`.
pub fn fit_for(weights: u64, hw: &Hardware) -> FitView {
    let Some(vram) = hw.vram_total else {
        return FitView {
            fit: Fit::Unknown,
            tight: false,
        };
    };
    let usable = vram.saturating_sub(RESERVED_VRAM);
    let room = usable.saturating_add(hw.ram_total.unwrap_or(0).saturating_sub(RESERVED_RAM));
    let fit = if weights.saturating_add(MIN_RUNTIME_OVERHEAD) <= usable {
        Fit::Gpu
    } else if weights <= room {
        Fit::Partly
    } else {
        Fit::TooBig
    };
    FitView {
        fit,
        tight: weights as f64 > 0.7 * usable as f64,
    }
}

/// GPU memory MTP adds: its layer, its own context and buffers. Measured
/// 1.06 GiB on a 20.9 GiB 27B model; larger models get a proportional share.
fn mtp_overhead(weights: u64) -> u64 {
    GIB.max(weights / 20)
}

/// Whether automatic MTP is worth trying on this load: the model has the
/// layer and, by this estimate, still fits in GPU memory with it and a
/// minimum context. Only a first filter that saves a load bound to fail;
/// the load itself has the final say (see `commands::load_model`). Unknown
/// GPU memory: try.
pub fn auto_mtp_may_fit(model: &ModelRecord, extra: u64, hw: &Hardware) -> bool {
    if model.mtp_layers.unwrap_or(0) == 0 {
        return false;
    }
    let Some(vram) = hw.vram_total else {
        return true;
    };
    let weights = model.size + extra;
    weights + mtp_overhead(weights) + MIN_RUNTIME_OVERHEAD <= vram.saturating_sub(RESERVED_VRAM)
}

/// Suggest settings. `extra` is the size of files loaded with the model
/// (an image projector).
pub fn suggest(model: &ModelRecord, extra: u64, hw: &Hardware) -> Suggestion {
    let weights = model.size + extra;
    let moe = model.experts.unwrap_or(0) > 0;
    let mtp = model.mtp_layers.unwrap_or(0) > 0;
    let mut notes = Vec::new();
    let mut note = |setting, text: &str| {
        notes.push(Note {
            setting,
            text: text.into(),
        })
    };

    let FitView { fit, tight } = fit_for(weights, hw);

    match (fit, moe) {
        (Fit::Gpu, _) => note(
            "overall",
            "The whole model fits in GPU memory, which is the fastest way to run it.",
        ),
        (Fit::Partly, true) => note(
            "overall",
            "The model is larger than GPU memory. As a Mixture-of-Experts model it still runs well: the parts used for every word stay on the GPU, and some experts are read from system RAM.",
        ),
        (Fit::Partly, false) => note(
            "overall",
            "The model is larger than GPU memory, so part of it will run on the CPU, often several times slower. A smaller version of the model (fewer bits per weight, such as Q4 instead of Q6) would fit and run much faster.",
        ),
        (Fit::TooBig, _) => note(
            "overall",
            "The model is larger than GPU memory and system RAM together. It may fail to load, or run very slowly. A smaller version of the model is recommended.",
        ),
        (Fit::Unknown, _) => note(
            "overall",
            "Gardenwall couldn't read this computer's GPU memory, so these suggestions assume the model fits.",
        ),
    }

    if moe {
        note(
            "context",
            "Automatic: Gardenwall picks the largest context that fits, and for a Mixture-of-Experts model never less than 32,768 tokens; when memory is short it moves a few more experts to system RAM instead, which costs a little speed.",
        );
    } else {
        note(
            "context",
            "Automatic: Gardenwall picks the largest context that fits in the GPU memory left after the model, keeping 3 GiB free so other apps can't slow it down. It never picks less than 16,384 tokens (or the model's trained length, if smaller), so a thinking model has room to finish its answer; when memory is short it moves some layers to the CPU instead, which is slower.",
        );
    }

    let kv_cache = if tight || matches!(fit, Fit::Partly | Fit::TooBig) {
        note(
            "kvCache",
            "8-bit: GPU memory is tight, and the 8-bit cache stores twice as much context in the same memory. The effect on answer quality is very small.",
        );
        KvCache::Q8_0
    } else {
        note(
            "kvCache",
            "16-bit: there is enough GPU memory for the full-precision cache, so nothing is traded away.",
        );
        KvCache::F16
    };

    let speculation = if mtp {
        let text = if auto_mtp_may_fit(model, extra, hw) {
            "Automatic: this model has its own prediction layer (MTP), and it should fit. Gardenwall turns MTP on when, with it loaded, the whole model still fits in GPU memory with at least a 16,384-token context, and off otherwise. In Gardenwall's tests it made writing about 1.7-2.4x faster on a 27B model, for about 1 GiB more GPU memory and slightly slower prompt reading, with the same answer quality."
        } else {
            "Automatic: this model has its own prediction layer (MTP), but there doesn't look to be GPU memory to spare for it, so it will most likely stay off. Gardenwall still checks at each load, in case more memory is free."
        };
        note("speculation", text);
        Speculation::Auto
    } else {
        note(
            "speculation",
            "Off: this model has no built-in prediction layer. Draft-free can help when answers repeat earlier text (coding, the agent), and a small draft model from the same family can speed up everything.",
        );
        Speculation::Off
    };

    let moe_cache_mib = if moe && fit == Fit::Partly {
        note(
            "expertCache",
            "4 GiB: keeps the most-used experts on the GPU instead of reading them from system RAM. In Gardenwall's tests on a model twice the size of GPU memory it made writing about 25% faster, while reading long prompts got slower. Turn it off if you mostly paste long documents.",
        );
        SUGGESTED_MOE_CACHE_MIB
    } else {
        if moe {
            note(
                "expertCache",
                "Off: every expert is already in GPU memory, so there is nothing to cache.",
            );
        }
        0
    };

    let thinking = if model.template.reasoning_levels {
        note(
            "thinking",
            "Medium: this model's template supports thinking levels, and its default can be very long. In Gardenwall's tests medium thinking gave an equally good answer with a third of the tokens, and very long thinking sometimes made a heavily compressed model repeat itself instead of answering.",
        );
        Thinking::Medium
    } else {
        Thinking::Default
    };

    Suggestion {
        hardware: hw.describe(),
        fit,
        context_size: None,
        kv_cache,
        speculation,
        moe_cache_mib,
        thinking,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(size_gib: f64, experts: u64, mtp: u64) -> ModelRecord {
        let mut m: ModelRecord = serde_json::from_value(serde_json::json!({
            "sha256": "ab".repeat(32),
            "displayName": "m.gguf",
            "size": 0,
            "source": {"kind": "local", "original_name": "m.gguf", "hash_verified": false},
            "architecture": "qwen35",
            "contextLength": 262144,
            "tensorCount": 1,
            "acquiredAtUnix": 0,
            "template": {"present": false, "requiresAcknowledgement": false, "acknowledged": false, "findings": []}
        }))
        .unwrap();
        m.size = (size_gib * GIB as f64) as u64;
        m.experts = Some(experts);
        m.mtp_layers = Some(mtp);
        m
    }

    fn rtx5090() -> Hardware {
        Hardware {
            gpu_name: Some("RTX 5090".into()),
            vram_total: Some(32 * GIB),
            ram_total: Some(62 * GIB),
        }
    }

    #[test]
    fn a_dense_model_with_mtp_that_fits() {
        let s = suggest(&model(19.9, 0, 1), 0, &rtx5090());
        assert_eq!(s.fit, Fit::Gpu);
        assert_eq!(s.speculation, Speculation::Auto);
        assert_eq!(s.moe_cache_mib, 0);
        assert_eq!(s.context_size, None);
        // 19.9 of 28 usable GiB is tight: the 8-bit cache leaves room for context.
        assert_eq!(s.kv_cache, KvCache::Q8_0);
        assert!(s.notes.iter().all(|n| n.setting != "expertCache"));
    }

    #[test]
    fn templates_with_thinking_levels_get_medium() {
        let mut m = model(19.9, 0, 0);
        assert_eq!(suggest(&m, 0, &rtx5090()).thinking, Thinking::Default);
        m.template.reasoning_levels = true;
        let s = suggest(&m, 0, &rtx5090());
        assert_eq!(s.thinking, Thinking::Medium);
        assert!(s.notes.iter().any(|n| n.setting == "thinking"));
    }

    #[test]
    fn a_small_model_keeps_the_full_cache() {
        let s = suggest(&model(0.6, 0, 0), 0, &rtx5090());
        assert_eq!(s.fit, Fit::Gpu);
        assert_eq!(s.kv_cache, KvCache::F16);
        assert_eq!(s.speculation, Speculation::Off);
    }

    #[test]
    fn a_moe_model_larger_than_vram_gets_the_expert_cache() {
        let s = suggest(&model(45.2, 512, 0), 0, &rtx5090());
        assert_eq!(s.fit, Fit::Partly);
        assert_eq!(s.moe_cache_mib, SUGGESTED_MOE_CACHE_MIB);
        assert_eq!(s.kv_cache, KvCache::Q8_0);
    }

    #[test]
    fn too_big_and_unknown_hardware() {
        let s = suggest(&model(120.0, 128, 0), 0, &rtx5090());
        assert_eq!(s.fit, Fit::TooBig);
        assert_eq!(s.moe_cache_mib, 0);
        let s = suggest(&model(20.0, 0, 0), 0, &Hardware::default());
        assert_eq!(s.fit, Fit::Unknown);
        assert!(s.hardware.contains("unknown"));
    }

    #[test]
    fn automatic_mtp_is_tried_only_with_the_layer_and_room() {
        let hw = rtx5090();
        // a 27B model (20.9 GiB) on 32 GiB: room to spare.
        assert!(auto_mtp_may_fit(&model(20.9, 0, 1), 0, &hw));
        assert!(!auto_mtp_may_fit(&model(20.9, 0, 0), 0, &hw));
        // 26 GiB fits alone (see below) but not with MTP's extra memory.
        assert!(!auto_mtp_may_fit(&model(26.0, 0, 1), 0, &hw));
        // A large MoE partly in system RAM never tries.
        assert!(!auto_mtp_may_fit(&model(45.2, 512, 1), 0, &hw));
        let s = suggest(&model(26.0, 0, 1), 0, &hw);
        assert_eq!(s.speculation, Speculation::Auto);
        assert!(
            s.notes
                .iter()
                .any(|n| n.text.contains("most likely stay off"))
        );
        // Unknown GPU memory: let the load decide.
        assert!(auto_mtp_may_fit(
            &model(20.9, 0, 1),
            0,
            &Hardware::default()
        ));
    }

    #[test]
    fn fit_for_at_each_boundary() {
        let hw = rtx5090();
        // 32 GiB: 28 usable; GPU while weights + 1.5 GiB <= 28.
        let usable = 28 * GIB;
        let edge = usable - MIN_RUNTIME_OVERHEAD;
        assert_eq!(fit_for(edge, &hw).fit, Fit::Gpu);
        assert_eq!(fit_for(edge + 1, &hw).fit, Fit::Partly);
        // Partly while it fits usable GPU memory + (RAM - 6 GiB) = 28 + 56.
        assert_eq!(fit_for(84 * GIB, &hw).fit, Fit::Partly);
        assert_eq!(fit_for(84 * GIB + 1, &hw).fit, Fit::TooBig);
        // Tight above 70 % of usable memory.
        let seventy = (0.7 * usable as f64) as u64;
        assert!(!fit_for(seventy, &hw).tight);
        assert!(fit_for(seventy + 1, &hw).tight);
        // Unknown memory: unknown, never tight.
        assert_eq!(
            fit_for(GIB, &Hardware::default()),
            FitView {
                fit: Fit::Unknown,
                tight: false
            }
        );
        // A planned figure replaces the measured one, and gives a figure
        // where none was readable.
        let planned = rtx5090().planned(Some(12));
        assert_eq!(planned.vram_total, Some(12 * GIB));
        assert_eq!(fit_for(20 * GIB, &planned).fit, Fit::Partly);
        assert_eq!(
            fit_for(5 * GIB, &Hardware::default().planned(Some(12))).fit,
            Fit::Gpu
        );
        assert_eq!(rtx5090().planned(None), rtx5090());
    }

    #[test]
    fn a_projector_counts_toward_the_size() {
        // 26 GiB alone fits; 26 + 2 GiB of projector doesn't.
        assert_eq!(suggest(&model(26.0, 0, 0), 0, &rtx5090()).fit, Fit::Gpu);
        assert_eq!(
            suggest(&model(26.0, 0, 0), 2 * GIB, &rtx5090()).fit,
            Fit::Partly
        );
    }
}
