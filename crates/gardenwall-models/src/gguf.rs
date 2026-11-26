//! Bounds-checked GGUF header validation (ADR-013 §2.3).
//!
//! This parser runs **before** any inference runtime opens a model file. It
//! validates the header, metadata, and tensor table of a GGUF v2/v3 file
//! without reading tensor data, and without trusting any length, count, or
//! offset found in the file.
//!
//! Design rules:
//! - Every length and count is checked against both a configured limit and the
//!   bytes actually remaining in the file before anything is allocated.
//! - All arithmetic on file-supplied values is checked; overflow is an error.
//! - Large arrays (e.g. tokenizer vocabularies) are validated element by
//!   element but only summarized, so memory use is bounded by the limits and
//!   not by the file.
//! - Unknown value types, tensor types, or structural anomalies are rejected
//!   (fail closed). Passing validation does not make a model trusted.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

/// `"GGUF"` interpreted as a little-endian `u32`.
const GGUF_MAGIC: [u8; 4] = *b"GGUF";
const DEFAULT_ALIGNMENT: u64 = 32;
/// llama.cpp's `GGML_MAX_DIMS`.
const MAX_TENSOR_DIMS: u32 = 4;

/// Limits applied to file-supplied sizes. Defaults are generous enough for
/// current large models while bounding memory and CPU use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GgufLimits {
    pub max_tensors: u64,
    pub max_metadata_entries: u64,
    pub max_key_bytes: u64,
    /// Maximum bytes of a single string value (e.g. a chat template).
    pub max_string_bytes: u64,
    /// Maximum element count of a single metadata array.
    pub max_array_len: u64,
    /// Maximum bytes of a string inside an array (e.g. one vocabulary token).
    pub max_array_string_bytes: u64,
    /// Maximum total bytes of header + metadata + tensor table.
    pub max_header_bytes: u64,
}

impl Default for GgufLimits {
    fn default() -> Self {
        Self {
            max_tensors: 1 << 16,
            max_metadata_entries: 1 << 16,
            max_key_bytes: 256,
            max_string_bytes: 1 << 20,
            max_array_len: 1 << 24,
            max_array_string_bytes: 1 << 16,
            max_header_bytes: 1 << 30,
        }
    }
}

/// Why a file failed validation. Variants carry only non-sensitive structural
/// information so they are safe to log.
#[derive(Debug)]
pub enum GgufError {
    Io(io::Error),
    /// The file ended before a structure that it declared.
    Truncated {
        offset: u64,
    },
    BadMagic,
    UnsupportedVersion(u32),
    LimitExceeded {
        what: &'static str,
        value: u64,
        limit: u64,
    },
    InvalidUtf8 {
        offset: u64,
    },
    InvalidKey {
        offset: u64,
    },
    DuplicateKey(String),
    UnknownValueType {
        offset: u64,
        value_type: u32,
    },
    NestedArray {
        offset: u64,
    },
    InvalidBool {
        offset: u64,
        value: u8,
    },
    InvalidAlignment(u64),
    WrongMetadataType {
        key: &'static str,
    },
    MissingArchitecture,
    DuplicateTensorName(String),
    InvalidTensorDims {
        tensor: String,
        n_dims: u32,
    },
    ZeroTensorDim {
        tensor: String,
    },
    UnknownTensorType {
        tensor: String,
        ggml_type: u32,
    },
    MisalignedTensorRow {
        tensor: String,
    },
    MisalignedTensorOffset {
        tensor: String,
        offset: u64,
    },
    TensorOutOfBounds {
        tensor: String,
    },
    /// A per-token array (scores, token types) is not as long as the
    /// vocabulary, so a runtime indexing it by token would read past it.
    VocabArrayMismatch {
        key: &'static str,
        len: u64,
        vocab: u64,
    },
    /// A special token (start, end, padding…) names a token that is not
    /// in the vocabulary.
    SpecialTokenOutOfRange {
        key: &'static str,
        id: u64,
        vocab: u64,
    },
    OverlappingTensors {
        first: String,
        second: String,
    },
    ArithmeticOverflow {
        what: &'static str,
    },
}

impl fmt::Display for GgufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error while reading GGUF: {error}"),
            Self::Truncated { offset } => write!(f, "file truncated at byte {offset}"),
            Self::BadMagic => write!(f, "not a GGUF file (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported GGUF version {v}"),
            Self::LimitExceeded { what, value, limit } => {
                write!(f, "{what} {value} exceeds limit {limit}")
            }
            Self::InvalidUtf8 { offset } => write!(f, "invalid UTF-8 string at byte {offset}"),
            Self::InvalidKey { offset } => write!(f, "invalid metadata key at byte {offset}"),
            Self::DuplicateKey(key) => write!(f, "duplicate metadata key {key:?}"),
            Self::UnknownValueType { offset, value_type } => {
                write!(
                    f,
                    "unknown metadata value type {value_type} at byte {offset}"
                )
            }
            Self::NestedArray { offset } => write!(f, "nested metadata array at byte {offset}"),
            Self::InvalidBool { offset, value } => {
                write!(f, "invalid boolean value {value} at byte {offset}")
            }
            Self::InvalidAlignment(a) => write!(f, "invalid general.alignment {a}"),
            Self::WrongMetadataType { key } => write!(f, "metadata key {key} has the wrong type"),
            Self::MissingArchitecture => write!(f, "missing general.architecture"),
            Self::DuplicateTensorName(name) => write!(f, "duplicate tensor name {name:?}"),
            Self::InvalidTensorDims { tensor, n_dims } => {
                write!(f, "tensor {tensor:?} has invalid dimension count {n_dims}")
            }
            Self::ZeroTensorDim { tensor } => write!(f, "tensor {tensor:?} has a zero dimension"),
            Self::UnknownTensorType { tensor, ggml_type } => {
                write!(f, "tensor {tensor:?} has unknown ggml type {ggml_type}")
            }
            Self::MisalignedTensorRow { tensor } => {
                write!(
                    f,
                    "tensor {tensor:?} row length is not a multiple of its block size"
                )
            }
            Self::MisalignedTensorOffset { tensor, offset } => {
                write!(f, "tensor {tensor:?} offset {offset} is not aligned")
            }
            Self::TensorOutOfBounds { tensor } => {
                write!(f, "tensor {tensor:?} extends beyond the end of the file")
            }
            Self::VocabArrayMismatch { key, len, vocab } => write!(
                f,
                "{key} has {len} entries but the vocabulary has {vocab} tokens"
            ),
            Self::SpecialTokenOutOfRange { key, id, vocab } => {
                write!(f, "{key} is {id}, outside the vocabulary of {vocab} tokens")
            }
            Self::OverlappingTensors { first, second } => {
                write!(f, "tensors {first:?} and {second:?} overlap")
            }
            Self::ArithmeticOverflow { what } => write!(f, "arithmetic overflow computing {what}"),
        }
    }
}

impl std::error::Error for GgufError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Metadata value type tags from the GGUF specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    Bool,
    String,
    Array,
    U64,
    I64,
    F64,
}

impl ValueType {
    fn from_tag(tag: u32) -> Option<Self> {
        Some(match tag {
            0 => Self::U8,
            1 => Self::I8,
            2 => Self::U16,
            3 => Self::I16,
            4 => Self::U32,
            5 => Self::I32,
            6 => Self::F32,
            7 => Self::Bool,
            8 => Self::String,
            9 => Self::Array,
            10 => Self::U64,
            11 => Self::I64,
            12 => Self::F64,
            _ => return None,
        })
    }

    /// Encoded size of fixed-width scalar types.
    fn fixed_size(self) -> Option<u64> {
        match self {
            Self::U8 | Self::I8 | Self::Bool => Some(1),
            Self::U16 | Self::I16 => Some(2),
            Self::U32 | Self::I32 | Self::F32 => Some(4),
            Self::U64 | Self::I64 | Self::F64 => Some(8),
            Self::String | Self::Array => None,
        }
    }
}

/// A validated metadata value. Arrays are summarized rather than retained.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Array { element_type: ValueType, len: u64 },
}

impl MetadataValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Self::Unsigned(v) => Some(v),
            Self::Signed(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }
}

/// One validated tensor table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorInfo {
    pub name: String,
    pub dims: Vec<u64>,
    pub ggml_type: u32,
    /// Offset relative to the start of the tensor data section.
    pub offset: u64,
    pub size_bytes: u64,
}

/// The result of successful validation.
#[derive(Debug, Clone, PartialEq)]
pub struct GgufSummary {
    pub version: u32,
    pub alignment: u64,
    /// Absolute file offset at which tensor data begins.
    pub data_offset: u64,
    pub file_size: u64,
    pub metadata: BTreeMap<String, MetadataValue>,
    pub tensors: Vec<TensorInfo>,
    pub total_tensor_bytes: u64,
}

impl GgufSummary {
    pub fn get(&self, key: &str) -> Option<&MetadataValue> {
        self.metadata.get(key)
    }

    pub fn architecture(&self) -> &str {
        // Presence and type are guaranteed by validation.
        self.get("general.architecture")
            .and_then(MetadataValue::as_str)
            .unwrap_or_default()
    }

    pub fn name(&self) -> Option<&str> {
        self.get("general.name").and_then(MetadataValue::as_str)
    }

    /// Value of an architecture-scoped key such as `{arch}.context_length`.
    pub fn arch_u64(&self, suffix: &str) -> Option<u64> {
        self.get(&format!("{}.{suffix}", self.architecture()))
            .and_then(MetadataValue::as_u64)
    }

    pub fn context_length(&self) -> Option<u64> {
        self.arch_u64("context_length")
    }

    pub fn block_count(&self) -> Option<u64> {
        self.arch_u64("block_count")
    }

    pub fn embedding_length(&self) -> Option<u64> {
        self.arch_u64("embedding_length")
    }

    /// The embedded Jinja chat template, if any. This is untrusted content.
    pub fn chat_template(&self) -> Option<&str> {
        self.get("tokenizer.chat_template")
            .and_then(MetadataValue::as_str)
    }

    /// `(split index, split count)` for multi-file models.
    pub fn split(&self) -> Option<(u64, u64)> {
        let no = self.get("split.no")?.as_u64()?;
        let count = self.get("split.count")?.as_u64()?;
        Some((no, count))
    }

    /// What identifies the tokenizer, for pairing a draft model.
    pub fn tokenizer(&self) -> Tokenizer {
        let s = |k: &str| {
            self.get(k)
                .and_then(MetadataValue::as_str)
                .map(str::to_string)
        };
        let n = |k: &str| self.get(k).and_then(MetadataValue::as_u64);
        Tokenizer {
            model: s("tokenizer.ggml.model"),
            pre: s("tokenizer.ggml.pre"),
            vocab_size: match self.get("tokenizer.ggml.tokens") {
                Some(MetadataValue::Array { len, .. }) => Some(*len),
                _ => None,
            },
            bos: n("tokenizer.ggml.bos_token_id"),
            eos: n("tokenizer.ggml.eos_token_id"),
        }
    }
}

/// The tokenizer facts two models must share for one to draft for the
/// other (speculative decoding). Token texts are compared again by the
/// runtime when it loads the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokenizer {
    /// `tokenizer.ggml.model`, e.g. "gpt2" or "llama".
    pub model: Option<String>,
    /// `tokenizer.ggml.pre`, the pre-tokenizer, e.g. "qwen2".
    pub pre: Option<String>,
    pub vocab_size: Option<u64>,
    pub bos: Option<u64>,
    pub eos: Option<u64>,
}

/// Vocabulary sizes may differ by padding up to this many tokens (the
/// same allowance llama.cpp makes).
pub const MAX_VOCAB_SIZE_DIFFERENCE: u64 = 128;

impl Tokenizer {
    /// Why `draft` can't draft for `self`, or `Ok` when it can.
    pub fn check_draft(&self, draft: &Tokenizer) -> Result<(), String> {
        let (Some(model), Some(size)) = (&self.model, self.vocab_size) else {
            return Err("the main model has no tokenizer information".into());
        };
        let (Some(draft_model), Some(draft_size)) = (&draft.model, draft.vocab_size) else {
            return Err("the draft model has no tokenizer information".into());
        };
        if model != draft_model || self.pre != draft.pre {
            return Err(format!(
                "the models use different tokenizers ({} and {})",
                describe(model, self.pre.as_deref()),
                describe(draft_model, draft.pre.as_deref())
            ));
        }
        if size.abs_diff(draft_size) > MAX_VOCAB_SIZE_DIFFERENCE {
            return Err(format!(
                "the vocabularies differ in size ({size} and {draft_size} tokens)"
            ));
        }
        if self.bos != draft.bos || self.eos != draft.eos {
            return Err("the models use different start or end tokens".into());
        }
        Ok(())
    }
}

fn describe(model: &str, pre: Option<&str>) -> String {
    match pre {
        Some(pre) => format!("{model}/{pre}"),
        None => model.to_string(),
    }
}

/// `(block size in elements, bytes per block)` for each supported ggml type.
/// Unknown or removed types return `None` and are rejected.
fn ggml_type_layout(ggml_type: u32) -> Option<(u64, u64)> {
    Some(match ggml_type {
        0 => (1, 4),      // F32
        1 => (1, 2),      // F16
        2 => (32, 18),    // Q4_0
        3 => (32, 20),    // Q4_1
        6 => (32, 22),    // Q5_0
        7 => (32, 24),    // Q5_1
        8 => (32, 34),    // Q8_0
        9 => (32, 36),    // Q8_1
        10 => (256, 84),  // Q2_K
        11 => (256, 110), // Q3_K
        12 => (256, 144), // Q4_K
        13 => (256, 176), // Q5_K
        14 => (256, 210), // Q6_K
        15 => (256, 292), // Q8_K
        16 => (256, 66),  // IQ2_XXS
        17 => (256, 74),  // IQ2_XS
        18 => (256, 98),  // IQ3_XXS
        19 => (256, 50),  // IQ1_S
        20 => (32, 18),   // IQ4_NL
        21 => (256, 110), // IQ3_S
        22 => (256, 82),  // IQ2_S
        23 => (256, 136), // IQ4_XS
        24 => (1, 1),     // I8
        25 => (1, 2),     // I16
        26 => (1, 4),     // I32
        27 => (1, 8),     // I64
        28 => (1, 8),     // F64
        29 => (256, 56),  // IQ1_M
        30 => (1, 2),     // BF16
        34 => (256, 54),  // TQ1_0
        35 => (256, 66),  // TQ2_0
        39 => (32, 17),   // MXFP4
        // Added in llama.cpp before b12283 (ggml.h `enum ggml_type`,
        // ggml-common.h block structs and their static_asserts):
        40 => (64, 36),  // NVFP4: 4 E4M3 scales + 32 bytes of 4-bit values
        41 => (128, 18), // Q1_0: f16 scale + 16 bytes of 1-bit values
        42 => (64, 18),  // Q2_0: f16 scale + 16 bytes of 2-bit values
        43 => (1, 1),    // F8_E4M3
        _ => return None,
    })
}

/// Reader that tracks its position and refuses to read past a known length.
struct BoundedReader<R> {
    inner: R,
    pos: u64,
    len: u64,
    header_limit: u64,
}

impl<R: Read> BoundedReader<R> {
    /// Ensure `n` more bytes exist and fit in the header budget.
    fn ensure(&self, n: u64) -> Result<(), GgufError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(GgufError::ArithmeticOverflow { what: "read end" })?;
        if end > self.len {
            return Err(GgufError::Truncated { offset: self.pos });
        }
        if end > self.header_limit {
            return Err(GgufError::LimitExceeded {
                what: "header size",
                value: end,
                limit: self.header_limit,
            });
        }
        Ok(())
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], GgufError> {
        self.ensure(N as u64)?;
        let mut buf = [0u8; N];
        self.inner.read_exact(&mut buf).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                GgufError::Truncated { offset: self.pos }
            } else {
                GgufError::Io(error)
            }
        })?;
        self.pos += N as u64;
        Ok(buf)
    }

    fn read_u8(&mut self) -> Result<u8, GgufError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_u32(&mut self) -> Result<u32, GgufError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, GgufError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    /// Read exactly `n` bytes into a new buffer. Callers must have checked `n`
    /// against a limit; `ensure` additionally checks it against the file.
    fn read_bytes(&mut self, n: u64) -> Result<Vec<u8>, GgufError> {
        self.ensure(n)?;
        let size = usize::try_from(n).map_err(|_| GgufError::ArithmeticOverflow {
            what: "buffer size",
        })?;
        let mut buf = vec![0u8; size];
        self.inner.read_exact(&mut buf).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                GgufError::Truncated { offset: self.pos }
            } else {
                GgufError::Io(error)
            }
        })?;
        self.pos += n;
        Ok(buf)
    }

    /// Skip `n` bytes after verifying they exist, by reading and discarding
    /// them (works for any `Read`, no seeking needed).
    fn skip(&mut self, n: u64) -> Result<(), GgufError> {
        self.ensure(n)?;
        let copied =
            io::copy(&mut (&mut self.inner).take(n), &mut io::sink()).map_err(GgufError::Io)?;
        if copied != n {
            return Err(GgufError::Truncated { offset: self.pos });
        }
        self.pos += n;
        Ok(())
    }

    fn read_string(&mut self, max: u64, what: &'static str) -> Result<String, GgufError> {
        let start = self.pos;
        let len = self.read_u64()?;
        if len > max {
            return Err(GgufError::LimitExceeded {
                what,
                value: len,
                limit: max,
            });
        }
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes).map_err(|_| GgufError::InvalidUtf8 { offset: start })
    }

    /// Validate a string without retaining it.
    fn skip_string(&mut self, max: u64, what: &'static str) -> Result<(), GgufError> {
        let start = self.pos;
        let len = self.read_u64()?;
        if len > max {
            return Err(GgufError::LimitExceeded {
                what,
                value: len,
                limit: max,
            });
        }
        let bytes = self.read_bytes(len)?;
        std::str::from_utf8(&bytes).map_err(|_| GgufError::InvalidUtf8 { offset: start })?;
        Ok(())
    }
}

fn check_limit(what: &'static str, value: u64, limit: u64) -> Result<(), GgufError> {
    if value > limit {
        Err(GgufError::LimitExceeded { what, value, limit })
    } else {
        Ok(())
    }
}

/// Keys are restricted to printable ASCII without whitespace, matching the
/// GGUF naming convention (`lower_snake_case.dotted`).
fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_graphic())
}

/// Tensor names must be non-empty printable ASCII.
fn is_valid_tensor_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic())
}

fn read_scalar<R: Read>(
    r: &mut BoundedReader<R>,
    value_type: ValueType,
) -> Result<MetadataValue, GgufError> {
    let offset = r.pos;
    Ok(match value_type {
        ValueType::U8 => MetadataValue::Unsigned(r.read_u8()?.into()),
        ValueType::I8 => MetadataValue::Signed(i8::from_le_bytes(r.read_array()?).into()),
        ValueType::U16 => MetadataValue::Unsigned(u16::from_le_bytes(r.read_array()?).into()),
        ValueType::I16 => MetadataValue::Signed(i16::from_le_bytes(r.read_array()?).into()),
        ValueType::U32 => MetadataValue::Unsigned(r.read_u32()?.into()),
        ValueType::I32 => MetadataValue::Signed(i32::from_le_bytes(r.read_array()?).into()),
        ValueType::F32 => MetadataValue::Float(f32::from_le_bytes(r.read_array()?).into()),
        ValueType::U64 => MetadataValue::Unsigned(r.read_u64()?),
        ValueType::I64 => MetadataValue::Signed(i64::from_le_bytes(r.read_array()?)),
        ValueType::F64 => MetadataValue::Float(f64::from_le_bytes(r.read_array()?)),
        ValueType::Bool => match r.read_u8()? {
            0 => MetadataValue::Bool(false),
            1 => MetadataValue::Bool(true),
            value => return Err(GgufError::InvalidBool { offset, value }),
        },
        // Callers only pass fixed-width types; fail closed rather than panic.
        ValueType::String | ValueType::Array => {
            return Err(GgufError::UnknownValueType {
                offset,
                value_type: u32::MAX,
            });
        }
    })
}

fn read_value<R: Read>(
    r: &mut BoundedReader<R>,
    limits: &GgufLimits,
) -> Result<MetadataValue, GgufError> {
    let type_offset = r.pos;
    let tag = r.read_u32()?;
    let value_type = ValueType::from_tag(tag).ok_or(GgufError::UnknownValueType {
        offset: type_offset,
        value_type: tag,
    })?;

    match value_type {
        ValueType::String => Ok(MetadataValue::String(
            r.read_string(limits.max_string_bytes, "string value length")?,
        )),
        ValueType::Array => {
            let element_offset = r.pos;
            let element_tag = r.read_u32()?;
            let element_type =
                ValueType::from_tag(element_tag).ok_or(GgufError::UnknownValueType {
                    offset: element_offset,
                    value_type: element_tag,
                })?;
            if element_type == ValueType::Array {
                return Err(GgufError::NestedArray {
                    offset: element_offset,
                });
            }
            let len = r.read_u64()?;
            check_limit("array length", len, limits.max_array_len)?;

            match element_type.fixed_size() {
                Some(size) if element_type != ValueType::Bool => {
                    let total = len
                        .checked_mul(size)
                        .ok_or(GgufError::ArithmeticOverflow { what: "array size" })?;
                    r.skip(total)?;
                }
                Some(_) => {
                    // Booleans must each be validated as 0 or 1.
                    for _ in 0..len {
                        read_scalar(r, ValueType::Bool)?;
                    }
                }
                None => {
                    // Strings: each element is validated, none are retained.
                    // The per-element minimum of 8 bytes (length prefix)
                    // bounds the loop by the file size via `ensure`.
                    for _ in 0..len {
                        r.skip_string(limits.max_array_string_bytes, "array string length")?;
                    }
                }
            }
            Ok(MetadataValue::Array { element_type, len })
        }
        scalar => read_scalar(r, scalar),
    }
}

/// Per-token arrays that must be as long as the vocabulary.
const VOCAB_ARRAYS: &[&str] = &["tokenizer.ggml.scores", "tokenizer.ggml.token_type"];

/// Special-token ids that must name a token in the vocabulary.
const SPECIAL_TOKEN_IDS: &[&str] = &[
    "tokenizer.ggml.bos_token_id",
    "tokenizer.ggml.eos_token_id",
    "tokenizer.ggml.eot_token_id",
    "tokenizer.ggml.eom_token_id",
    "tokenizer.ggml.unknown_token_id",
    "tokenizer.ggml.seperator_token_id",
    "tokenizer.ggml.padding_token_id",
    "tokenizer.ggml.mask_token_id",
];

/// The tokenizer's parts must agree with each other. A file where they
/// don't is malformed or tampered with, and has led runtimes to read
/// outside the vocabulary (the llama.cpp vocabulary CVEs of 2025).
fn check_tokenizer(metadata: &BTreeMap<String, MetadataValue>) -> Result<(), GgufError> {
    let Some(MetadataValue::Array { len: vocab, .. }) = metadata.get("tokenizer.ggml.tokens")
    else {
        return Ok(());
    };
    let vocab = *vocab;
    for &key in VOCAB_ARRAYS {
        if let Some(MetadataValue::Array { len, .. }) = metadata.get(key)
            && *len != vocab
        {
            return Err(GgufError::VocabArrayMismatch {
                key,
                len: *len,
                vocab,
            });
        }
    }
    for &key in SPECIAL_TOKEN_IDS {
        // A negative id, or the 32-bit "none" value, means "no such token".
        if let Some(id) = metadata.get(key).and_then(MetadataValue::as_u64)
            && id != u64::from(u32::MAX)
            && id >= vocab
        {
            return Err(GgufError::SpecialTokenOutOfRange { key, id, vocab });
        }
    }
    Ok(())
}

/// Validate a GGUF file from any seekable reader. Only the header, metadata,
/// and tensor table are read; tensor data is bounds-checked but not read.
pub fn validate<R: Read + Seek>(reader: R, limits: &GgufLimits) -> Result<GgufSummary, GgufError> {
    validate_inner(reader, limits, true)
}

/// Validate one part of a split model ([`crate::split`]). Identical checks,
/// except that only the first part carries `general.architecture`; the set
/// as a whole is checked with [`crate::split::check_parts`].
pub fn validate_part<R: Read + Seek>(
    reader: R,
    limits: &GgufLimits,
) -> Result<GgufSummary, GgufError> {
    validate_inner(reader, limits, false)
}

fn validate_inner<R: Read + Seek>(
    mut reader: R,
    limits: &GgufLimits,
    require_architecture: bool,
) -> Result<GgufSummary, GgufError> {
    let file_size = reader.seek(SeekFrom::End(0)).map_err(GgufError::Io)?;
    reader.seek(SeekFrom::Start(0)).map_err(GgufError::Io)?;

    let mut r = BoundedReader {
        inner: io::BufReader::new(reader),
        pos: 0,
        len: file_size,
        header_limit: limits.max_header_bytes,
    };

    if r.read_array::<4>()? != GGUF_MAGIC {
        return Err(GgufError::BadMagic);
    }
    let version = r.read_u32()?;
    // v1 used 32-bit counts and is obsolete; only v2 and v3 are accepted.
    if !(2..=3).contains(&version) {
        return Err(GgufError::UnsupportedVersion(version));
    }

    let tensor_count = r.read_u64()?;
    check_limit("tensor count", tensor_count, limits.max_tensors)?;
    let kv_count = r.read_u64()?;
    check_limit(
        "metadata entry count",
        kv_count,
        limits.max_metadata_entries,
    )?;

    let mut metadata = BTreeMap::new();
    for _ in 0..kv_count {
        let key_offset = r.pos;
        let key = r.read_string(limits.max_key_bytes, "metadata key length")?;
        if !is_valid_key(&key) {
            return Err(GgufError::InvalidKey { offset: key_offset });
        }
        if metadata.contains_key(&key) {
            return Err(GgufError::DuplicateKey(key));
        }
        let value = read_value(&mut r, limits)?;
        metadata.insert(key, value);
    }

    match metadata.get("general.architecture") {
        Some(MetadataValue::String(arch)) if is_valid_key(arch) => {}
        None if !require_architecture => {}
        Some(MetadataValue::String(_)) | None => return Err(GgufError::MissingArchitecture),
        Some(_) => {
            return Err(GgufError::WrongMetadataType {
                key: "general.architecture",
            });
        }
    }

    check_tokenizer(&metadata)?;

    let alignment = match metadata.get("general.alignment") {
        None => DEFAULT_ALIGNMENT,
        Some(MetadataValue::Unsigned(a)) => *a,
        Some(_) => {
            return Err(GgufError::WrongMetadataType {
                key: "general.alignment",
            });
        }
    };
    if alignment == 0 || !alignment.is_power_of_two() || alignment > 1 << 20 {
        return Err(GgufError::InvalidAlignment(alignment));
    }

    let capacity = usize::try_from(tensor_count).unwrap_or(0);
    let mut tensors = Vec::with_capacity(capacity.min(1 << 12));
    let mut names = HashSet::with_capacity(capacity.min(1 << 12));
    for _ in 0..tensor_count {
        let name_offset = r.pos;
        let name = r.read_string(limits.max_key_bytes, "tensor name length")?;
        if !is_valid_tensor_name(&name) {
            return Err(GgufError::InvalidKey {
                offset: name_offset,
            });
        }
        if !names.insert(name.clone()) {
            return Err(GgufError::DuplicateTensorName(name));
        }

        let n_dims = r.read_u32()?;
        if n_dims == 0 || n_dims > MAX_TENSOR_DIMS {
            return Err(GgufError::InvalidTensorDims {
                tensor: name,
                n_dims,
            });
        }
        let mut dims = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            dims.push(r.read_u64()?);
        }
        if dims.contains(&0) {
            return Err(GgufError::ZeroTensorDim { tensor: name });
        }

        let ggml_type = r.read_u32()?;
        let Some((block_size, block_bytes)) = ggml_type_layout(ggml_type) else {
            return Err(GgufError::UnknownTensorType {
                tensor: name,
                ggml_type,
            });
        };
        let offset = r.read_u64()?;

        // dims[0] is the row length; quantized rows are stored in whole blocks.
        if dims[0] % block_size != 0 {
            return Err(GgufError::MisalignedTensorRow { tensor: name });
        }
        let overflow = || GgufError::ArithmeticOverflow {
            what: "tensor size",
        };
        let row_bytes = (dims[0] / block_size)
            .checked_mul(block_bytes)
            .ok_or_else(overflow)?;
        let size_bytes = dims[1..]
            .iter()
            .try_fold(row_bytes, |acc, &d| acc.checked_mul(d))
            .ok_or_else(overflow)?;

        if offset % alignment != 0 {
            return Err(GgufError::MisalignedTensorOffset {
                tensor: name,
                offset,
            });
        }

        tensors.push(TensorInfo {
            name,
            dims,
            ggml_type,
            offset,
            size_bytes,
        });
    }

    // Tensor data starts at the next alignment boundary after the table.
    let data_offset =
        r.pos
            .checked_next_multiple_of(alignment)
            .ok_or(GgufError::ArithmeticOverflow {
                what: "data offset",
            })?;
    if data_offset > file_size && !tensors.is_empty() {
        return Err(GgufError::Truncated { offset: r.pos });
    }
    let data_len = file_size.saturating_sub(data_offset);

    // Every tensor must lie inside the data section, without overlap.
    let mut order: Vec<usize> = (0..tensors.len()).collect();
    order.sort_by_key(|&i| tensors[i].offset);
    let mut total_tensor_bytes: u64 = 0;
    let mut previous_end: Option<(u64, usize)> = None;
    for &i in &order {
        let tensor = &tensors[i];
        let end = tensor
            .offset
            .checked_add(tensor.size_bytes)
            .ok_or(GgufError::ArithmeticOverflow { what: "tensor end" })?;
        if end > data_len {
            return Err(GgufError::TensorOutOfBounds {
                tensor: tensor.name.clone(),
            });
        }
        if let Some((prev_end, prev)) = previous_end
            && tensor.offset < prev_end
        {
            return Err(GgufError::OverlappingTensors {
                first: tensors[prev].name.clone(),
                second: tensor.name.clone(),
            });
        }
        previous_end = Some((end, i));
        total_tensor_bytes = total_tensor_bytes.checked_add(tensor.size_bytes).ok_or(
            GgufError::ArithmeticOverflow {
                what: "total tensor bytes",
            },
        )?;
    }

    Ok(GgufSummary {
        version,
        alignment,
        data_offset,
        file_size,
        metadata,
        tensors,
        total_tensor_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal GGUF writer used to build test fixtures.
    #[derive(Default)]
    struct Builder {
        version: u32,
        kv: Vec<u8>,
        kv_count: u64,
        tensors: Vec<u8>,
        tensor_count: u64,
        alignment: u64,
        data_len: u64,
    }

    fn put_str(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    impl Builder {
        fn new() -> Self {
            Self {
                version: 3,
                alignment: 32,
                ..Self::default()
            }
            .kv_str("general.architecture", "llama")
        }

        fn raw_kv(mut self, key: &str, tag: u32, value: &[u8]) -> Self {
            put_str(&mut self.kv, key);
            self.kv.extend_from_slice(&tag.to_le_bytes());
            self.kv.extend_from_slice(value);
            self.kv_count += 1;
            self
        }

        fn kv_str(self, key: &str, value: &str) -> Self {
            let mut v = Vec::new();
            put_str(&mut v, value);
            self.raw_kv(key, 8, &v)
        }

        fn kv_u32(self, key: &str, value: u32) -> Self {
            self.raw_kv(key, 4, &value.to_le_bytes())
        }

        fn kv_str_array(self, key: &str, items: &[&str]) -> Self {
            let mut v = Vec::new();
            v.extend_from_slice(&8u32.to_le_bytes());
            v.extend_from_slice(&(items.len() as u64).to_le_bytes());
            for item in items {
                put_str(&mut v, item);
            }
            self.raw_kv(key, 9, &v)
        }

        fn tensor(mut self, name: &str, dims: &[u64], ggml_type: u32, offset: u64) -> Self {
            put_str(&mut self.tensors, name);
            self.tensors
                .extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in dims {
                self.tensors.extend_from_slice(&d.to_le_bytes());
            }
            self.tensors.extend_from_slice(&ggml_type.to_le_bytes());
            self.tensors.extend_from_slice(&offset.to_le_bytes());
            self.tensor_count += 1;
            self
        }

        fn data_len(mut self, len: u64) -> Self {
            self.data_len = len;
            self
        }

        fn build(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&self.version.to_le_bytes());
            out.extend_from_slice(&self.tensor_count.to_le_bytes());
            out.extend_from_slice(&self.kv_count.to_le_bytes());
            out.extend_from_slice(&self.kv);
            out.extend_from_slice(&self.tensors);
            let aligned = (out.len() as u64).next_multiple_of(self.alignment);
            out.resize(aligned as usize, 0);
            out.resize((aligned + self.data_len) as usize, 0xAB);
            out
        }
    }

    fn check(bytes: &[u8]) -> Result<GgufSummary, GgufError> {
        validate(Cursor::new(bytes), &GgufLimits::default())
    }

    fn valid_model() -> Builder {
        Builder::new()
            .kv_str("general.name", "Test Model")
            .kv_u32("llama.context_length", 4096)
            .kv_u32("llama.block_count", 2)
            .kv_str("tokenizer.chat_template", "{{ messages }}")
            .kv_str_array("tokenizer.ggml.tokens", &["<s>", "</s>", "hello"])
            // F32 4x2 = 32 bytes at 0; Q8_0 64x1 = 2 blocks * 34 = 68 bytes at 32.
            .tensor("a.weight", &[4, 2], 0, 0)
            .tensor("b.weight", &[64], 8, 32)
            .data_len(32 + 68)
    }

    #[test]
    fn tokenizer_parts_must_agree() {
        // Ids inside the vocabulary of three tokens, and "none", are fine.
        let ok = valid_model()
            .kv_u32("tokenizer.ggml.bos_token_id", 0)
            .kv_u32("tokenizer.ggml.eos_token_id", 2)
            .kv_u32("tokenizer.ggml.padding_token_id", u32::MAX)
            .build();
        assert!(check(&ok).is_ok());
        let out_of_range = valid_model()
            .kv_u32("tokenizer.ggml.eos_token_id", 3)
            .build();
        assert!(matches!(
            check(&out_of_range),
            Err(GgufError::SpecialTokenOutOfRange {
                key: "tokenizer.ggml.eos_token_id",
                id: 3,
                vocab: 3
            })
        ));
        // A token-type list shorter than the vocabulary.
        let short = valid_model()
            .kv_str_array("tokenizer.ggml.token_type", &["a", "b"])
            .build();
        assert!(matches!(
            check(&short),
            Err(GgufError::VocabArrayMismatch {
                len: 2,
                vocab: 3,
                ..
            })
        ));
        let same = valid_model()
            .kv_str_array("tokenizer.ggml.token_type", &["a", "b", "c"])
            .build();
        assert!(check(&same).is_ok());
    }

    #[test]
    fn draft_tokenizers_must_match() {
        let base = Tokenizer {
            model: Some("gpt2".into()),
            pre: Some("qwen2".into()),
            vocab_size: Some(151_936),
            bos: Some(151_643),
            eos: Some(151_645),
        };
        assert!(base.check_draft(&base).is_ok());
        // Padding differences are allowed.
        let padded = Tokenizer {
            vocab_size: Some(151_900),
            ..base.clone()
        };
        assert!(base.check_draft(&padded).is_ok());
        for bad in [
            Tokenizer {
                pre: Some("llama-bpe".into()),
                ..base.clone()
            },
            Tokenizer {
                model: Some("llama".into()),
                ..base.clone()
            },
            Tokenizer {
                vocab_size: Some(128_256),
                ..base.clone()
            },
            Tokenizer {
                eos: Some(2),
                ..base.clone()
            },
            Tokenizer {
                model: None,
                ..base.clone()
            },
        ] {
            assert!(base.check_draft(&bad).is_err(), "{bad:?}");
        }
        let summary = check(&valid_model().build()).unwrap();
        assert_eq!(summary.tokenizer().vocab_size, Some(3));
    }

    #[test]
    fn accepts_a_well_formed_file() {
        let summary = check(&valid_model().build()).unwrap();
        assert_eq!(summary.version, 3);
        assert_eq!(summary.architecture(), "llama");
        assert_eq!(summary.name(), Some("Test Model"));
        assert_eq!(summary.context_length(), Some(4096));
        assert_eq!(summary.block_count(), Some(2));
        assert_eq!(summary.chat_template(), Some("{{ messages }}"));
        assert_eq!(
            summary.get("tokenizer.ggml.tokens"),
            Some(&MetadataValue::Array {
                element_type: ValueType::String,
                len: 3
            })
        );
        assert_eq!(summary.tensors.len(), 2);
        assert_eq!(summary.tensors[1].size_bytes, 68);
        assert_eq!(summary.total_tensor_bytes, 100);
        assert_eq!(summary.data_offset % 32, 0);
    }

    #[test]
    fn every_truncation_is_rejected_without_panicking() {
        let bytes = valid_model().build();
        for len in 0..bytes.len() {
            let result = check(&bytes[..len]);
            assert!(result.is_err(), "truncation to {len} bytes was accepted");
        }
    }

    #[test]
    fn single_byte_corruption_never_panics() {
        let bytes = valid_model().build();
        let header_len = bytes.len() - 100;
        for i in 0..header_len {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = bytes.clone();
                corrupted[i] ^= flip;
                let _ = check(&corrupted);
            }
        }
    }

    #[test]
    fn rejects_bad_magic_and_versions() {
        let mut bytes = valid_model().build();
        bytes[0] = b'X';
        assert!(matches!(check(&bytes), Err(GgufError::BadMagic)));

        for version in [0, 1, 4, u32::MAX] {
            let mut b = valid_model();
            b.version = version;
            assert!(matches!(
                check(&b.build()),
                Err(GgufError::UnsupportedVersion(v)) if v == version
            ));
        }
    }

    #[test]
    fn rejects_huge_declared_counts_before_allocating() {
        let mut bytes = valid_model().build();
        bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            check(&bytes),
            Err(GgufError::LimitExceeded {
                what: "tensor count",
                ..
            })
        ));

        let mut bytes = valid_model().build();
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            check(&bytes),
            Err(GgufError::LimitExceeded {
                what: "metadata entry count",
                ..
            })
        ));
    }

    #[test]
    fn rejects_string_lengths_beyond_file_or_limit() {
        // Key length claims 200 bytes but the file is shorter: truncated.
        let mut b = Builder::new();
        b.kv.clear();
        b.kv_count = 1;
        b.kv.extend_from_slice(&200u64.to_le_bytes());
        b.kv.extend_from_slice(b"general");
        assert!(matches!(
            check(&b.build()),
            Err(GgufError::Truncated { .. })
        ));

        // Key length beyond the configured limit.
        let mut b = Builder::new();
        b.kv.clear();
        b.kv_count = 1;
        b.kv.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            check(&b.build()),
            Err(GgufError::LimitExceeded {
                what: "metadata key length",
                ..
            })
        ));
    }

    #[test]
    fn rejects_oversized_array_claims() {
        let mut v = Vec::new();
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&(1u64 << 40).to_le_bytes());
        let bytes = Builder::new().raw_kv("big", 9, &v).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::LimitExceeded {
                what: "array length",
                ..
            })
        ));

        // Within the count limit but larger than the file.
        let mut v = Vec::new();
        v.extend_from_slice(&10u32.to_le_bytes());
        v.extend_from_slice(&(1u64 << 20).to_le_bytes());
        let bytes = Builder::new().raw_kv("big", 9, &v).build();
        assert!(matches!(check(&bytes), Err(GgufError::Truncated { .. })));
    }

    #[test]
    fn rejects_invalid_utf8_keys_and_values() {
        let mut b = Builder::new();
        put_str(&mut b.kv, "bad.value");
        b.kv.extend_from_slice(&8u32.to_le_bytes());
        b.kv.extend_from_slice(&2u64.to_le_bytes());
        b.kv.extend_from_slice(&[0xC3, 0x28]);
        b.kv_count += 1;
        assert!(matches!(
            check(&b.build()),
            Err(GgufError::InvalidUtf8 { .. })
        ));

        let bytes = Builder::new().kv_u32("has space", 1).build();
        assert!(matches!(check(&bytes), Err(GgufError::InvalidKey { .. })));
    }

    #[test]
    fn rejects_duplicate_keys_and_tensor_names() {
        let bytes = Builder::new().kv_u32("a.b", 1).kv_u32("a.b", 2).build();
        assert!(matches!(check(&bytes), Err(GgufError::DuplicateKey(_))));

        let bytes = Builder::new()
            .tensor("t", &[4], 0, 0)
            .tensor("t", &[4], 0, 32)
            .data_len(64)
            .build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::DuplicateTensorName(_))
        ));
    }

    #[test]
    fn newer_ggml_types_are_sized_exactly() {
        // (type, elements, exact bytes) for the types added up to b12283.
        for (ty, elements, bytes) in [
            (40u32, 128u64, 72u64), // NVFP4: 2 blocks of 36
            (41, 256, 36),          // Q1_0: 2 blocks of 18
            (42, 128, 36),          // Q2_0: 2 blocks of 18
            (43, 100, 100),         // F8_E4M3
        ] {
            let model = |len| {
                Builder::new()
                    .tensor("t", &[elements], ty, 0)
                    .data_len(len)
                    .build()
            };
            let summary = check(&model(bytes)).unwrap_or_else(|e| panic!("type {ty}: {e}"));
            assert_eq!(summary.tensors[0].size_bytes, bytes, "type {ty}");
            // One byte short is refused.
            assert!(check(&model(bytes - 1)).is_err(), "type {ty}");
        }
        // A row that isn't a whole number of blocks, a removed type and an
        // unknown type are refused.
        for (ty, elements) in [(42u32, 100u64), (31, 32), (44, 32)] {
            let bytes = Builder::new()
                .tensor("t", &[elements], ty, 0)
                .data_len(4096)
                .build();
            assert!(check(&bytes).is_err(), "type {ty} x{elements}");
        }
    }

    #[test]
    fn rejects_unknown_types_nested_arrays_and_bad_bools() {
        let bytes = Builder::new().raw_kv("x", 13, &[0; 8]).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::UnknownValueType { value_type: 13, .. })
        ));

        let mut v = Vec::new();
        v.extend_from_slice(&9u32.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        let bytes = Builder::new().raw_kv("x", 9, &v).build();
        assert!(matches!(check(&bytes), Err(GgufError::NestedArray { .. })));

        let bytes = Builder::new().raw_kv("x", 7, &[2]).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::InvalidBool { value: 2, .. })
        ));
    }

    #[test]
    fn requires_a_string_architecture() {
        let mut b = Builder::new();
        b.kv.clear();
        b.kv_count = 0;
        assert!(matches!(
            check(&b.build()),
            Err(GgufError::MissingArchitecture)
        ));

        let mut b = Builder::new();
        b.kv.clear();
        b.kv_count = 0;
        let bytes = b.kv_u32("general.architecture", 1).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::WrongMetadataType { .. })
        ));
    }

    #[test]
    fn rejects_invalid_alignment() {
        for alignment in [0u32, 3, 48, 1 << 30] {
            let bytes = Builder::new()
                .kv_u32("general.alignment", alignment)
                .build();
            assert!(
                matches!(check(&bytes), Err(GgufError::InvalidAlignment(_))),
                "alignment {alignment} accepted"
            );
        }
    }

    #[test]
    fn rejects_malformed_tensors() {
        let bytes = Builder::new().tensor("t", &[], 0, 0).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::InvalidTensorDims { .. })
        ));

        let bytes = Builder::new().tensor("t", &[1, 1, 1, 1, 1], 0, 0).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::InvalidTensorDims { .. })
        ));

        let bytes = Builder::new().tensor("t", &[4, 0], 0, 0).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::ZeroTensorDim { .. })
        ));

        let bytes = Builder::new().tensor("t", &[4], 4, 0).data_len(64).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::UnknownTensorType { ggml_type: 4, .. })
        ));

        let bytes = Builder::new()
            .tensor("t", &[33], 8, 0)
            .data_len(128)
            .build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::MisalignedTensorRow { .. })
        ));

        let bytes = Builder::new().tensor("t", &[4], 0, 8).data_len(64).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::MisalignedTensorOffset { .. })
        ));
    }

    #[test]
    fn rejects_tensors_out_of_bounds_or_overlapping() {
        let bytes = Builder::new().tensor("t", &[16], 0, 0).data_len(32).build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::TensorOutOfBounds { .. })
        ));

        let bytes = Builder::new()
            .tensor("a", &[16], 0, 0)
            .tensor("b", &[16], 0, 32)
            .data_len(128)
            .build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::OverlappingTensors { .. })
        ));
    }

    #[test]
    fn rejects_tensor_size_overflow() {
        let bytes = Builder::new()
            .tensor("t", &[u64::MAX, u64::MAX], 0, 0)
            .data_len(32)
            .build();
        assert!(matches!(
            check(&bytes),
            Err(GgufError::ArithmeticOverflow { .. })
        ));
    }

    #[test]
    fn enforces_header_budget() {
        let limits = GgufLimits {
            max_header_bytes: 64,
            ..GgufLimits::default()
        };
        let result = validate(Cursor::new(valid_model().build()), &limits);
        assert!(matches!(
            result,
            Err(GgufError::LimitExceeded {
                what: "header size",
                ..
            })
        ));
    }

    #[test]
    fn errors_do_not_echo_metadata_values() {
        let bytes = Builder::new()
            .kv_str("secret.value", "hunter2")
            .raw_kv("x", 99, &[])
            .build();
        let message = check(&bytes).unwrap_err().to_string();
        assert!(!message.contains("hunter2"));
    }
}
