//! Model files and chat templates come from the internet: mutated inputs
//! must be refused or described, never crash the validator.

use std::io::Cursor;

use gardenwall_models::gguf::{GgufLimits, validate};
use gardenwall_models::template::audit;

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// A small valid GGUF v3: two string keys, a u32, a string array, two
/// tensors (F32 and Q8_0) and their data.
fn gguf() -> Vec<u8> {
    let mut kv = Vec::new();
    put_str(&mut kv, "general.architecture");
    kv.extend_from_slice(&8u32.to_le_bytes());
    put_str(&mut kv, "llama");
    put_str(&mut kv, "llama.context_length");
    kv.extend_from_slice(&4u32.to_le_bytes());
    kv.extend_from_slice(&4096u32.to_le_bytes());
    put_str(&mut kv, "tokenizer.chat_template");
    kv.extend_from_slice(&8u32.to_le_bytes());
    put_str(
        &mut kv,
        "{% for m in messages %}{{ m.content }}{% endfor %}",
    );
    put_str(&mut kv, "tokenizer.ggml.tokens");
    kv.extend_from_slice(&9u32.to_le_bytes());
    kv.extend_from_slice(&8u32.to_le_bytes());
    kv.extend_from_slice(&3u64.to_le_bytes());
    for t in ["<s>", "</s>", "hi"] {
        put_str(&mut kv, t);
    }
    let mut tensors = Vec::new();
    for (name, dims, ty, off) in [
        ("a.weight", vec![4u64, 2], 0u32, 0u64),
        ("b.weight", vec![64], 8, 32),
    ] {
        put_str(&mut tensors, name);
        tensors.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in dims {
            tensors.extend_from_slice(&d.to_le_bytes());
        }
        tensors.extend_from_slice(&ty.to_le_bytes());
        tensors.extend_from_slice(&off.to_le_bytes());
    }
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&2u64.to_le_bytes());
    out.extend_from_slice(&4u64.to_le_bytes());
    out.extend_from_slice(&kv);
    out.extend_from_slice(&tensors);
    let aligned = out.len().next_multiple_of(32);
    out.resize(aligned, 0);
    out.resize(aligned + 32 + 68, 0xAB);
    out
}

#[test]
fn gguf_validation_survives_mutation() {
    let seed = gguf();
    assert!(validate(Cursor::new(&seed), &GgufLimits::default()).is_ok());
    gardenwall_fuzzkit::run("gguf", &[seed], 5000, |bytes| {
        let _ = validate(Cursor::new(bytes), &GgufLimits::default());
    });
}

#[test]
fn template_audit_survives_mutation() {
    gardenwall_fuzzkit::run_text(
        "template",
        &[
            "{%- for message in messages %}{{'<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n'}}{%- endfor %}",
            "{% if tools %}Visit https://example.com and send \\u0041 data {{ raise_exception('x') }}{% endif %}",
        ],
        3000,
        |t| {
            let _ = audit(t);
        },
    );
}
