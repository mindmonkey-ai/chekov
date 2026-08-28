//! Enough of the GGUF header to size a KV cache honestly.
//!
//! Reads a bounded prefix of a local file with `std::fs` — no seam change, no
//! network. Every length field is `u64` in GGUF v3; a `u32` parser silently
//! corrupts past 4 GiB, so the widths here are not incidental.

use std::io::Read;
use std::path::Path;

use crate::error::ChekovError;

/// Header prefix we are willing to read before giving up.
const MAX_HEADER_BYTES: u64 = 32 * 1024 * 1024;

/// The geometry a KV-cache calculation needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Geometry {
    pub arch: String,
    pub block_count: Option<u32>,
    pub nextn_predict_layers: Option<u32>,
    pub full_attention_interval: Option<u32>,
    pub head_count_kv: Option<u32>,
    pub key_length: Option<u32>,
    pub value_length: Option<u32>,
    pub key_length_mla: Option<u32>,
    /// `tokenizer.chat_template` — what decides the tool-call parser.
    pub chat_template: Option<String>,
}

/// Cursor over the header bytes, refusing every out-of-range read.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.buf.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// GGUF strings are a u64 length followed by that many bytes.
    fn string(&mut self) -> Option<String> {
        let len = usize::try_from(self.u64()?).ok()?;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}

/// A metadata value, reduced to what this module consumes.
enum Value {
    U32(u32),
    Str(String),
    Other,
}

/// Skip or capture one value of `kind`. Returns None on a malformed stream.
fn read_value(c: &mut Cursor, kind: u32) -> Option<Value> {
    match kind {
        0 | 1 => c.take(1).map(|_| Value::Other),
        2 | 3 => c.take(2).map(|_| Value::Other),
        // uint32 and int32 share a width; both are read as u32 because every
        // geometry field this module wants is a small non-negative count.
        4 | 5 => c.u32().map(Value::U32),
        6 => c.take(4).map(|_| Value::Other),
        7 => c.take(1).map(|_| Value::Other),
        8 => c.string().map(Value::Str),
        9 => read_array(c),
        10..=12 => c.take(8).map(|_| Value::Other),
        _ => None,
    }
}

/// `head_count_kv` can be an ARRAY (one entry per layer) on hybrid models, not
/// a scalar. Take the first element rather than erroring.
fn read_array(c: &mut Cursor) -> Option<Value> {
    let elem = c.u32()?;
    let count = c.u64()?;
    let mut first = None;
    for i in 0..count {
        let v = read_value(c, elem)?;
        if i == 0 {
            first = Some(v);
        }
    }
    Some(first.unwrap_or(Value::Other))
}

/// Read the geometry out of a local GGUF file.
pub fn read_geometry(path: &Path) -> Result<Geometry, ChekovError> {
    let file = std::fs::File::open(path)
        .map_err(|e| ChekovError::io(format!("opening {}", path.display()), e))?;
    let mut buf = Vec::new();
    file.take(MAX_HEADER_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| ChekovError::io(format!("reading {}", path.display()), e))?;
    parse_geometry(&buf).ok_or_else(|| ChekovError::ConfigInvalid {
        path: path.to_path_buf(),
        reason: "not a GGUF v2/v3 header, or the metadata block is truncated".to_owned(),
    })
}

/// Pure parser over header bytes.
#[must_use]
pub fn parse_geometry(buf: &[u8]) -> Option<Geometry> {
    let mut c = Cursor::new(buf);
    if c.take(4)? != b"GGUF" {
        return None;
    }
    let _version = c.u32()?;
    let _tensor_count = c.u64()?;
    let kv_count = c.u64()?;
    let mut g = Geometry::default();
    for _ in 0..kv_count {
        let key = c.string()?;
        let kind = c.u32()?;
        let value = read_value(&mut c, kind)?;
        absorb(&mut g, &key, value);
    }
    Some(g)
}

/// Keys are namespaced by architecture (`qwen3moe.block_count`), so match on
/// the suffix rather than hard-coding every architecture's prefix.
fn absorb(g: &mut Geometry, key: &str, value: Value) {
    if let Value::Str(s) = value {
        match key {
            "general.architecture" => g.arch = s,
            "tokenizer.chat_template" => g.chat_template = Some(s),
            _ => {}
        }
        return;
    }
    let Value::U32(v) = value else {
        return;
    };
    let suffix = key.rsplit('.').next().unwrap_or(key);
    match (key, suffix) {
        (_, "block_count") => g.block_count = Some(v),
        (_, "nextn_predict_layers") => g.nextn_predict_layers = Some(v),
        (_, "full_attention_interval") => g.full_attention_interval = Some(v),
        (k, _) if k.ends_with("attention.head_count_kv") => g.head_count_kv = Some(v),
        (k, _) if k.ends_with("attention.key_length_mla") => g.key_length_mla = Some(v),
        (k, _) if k.ends_with("attention.key_length") => g.key_length = Some(v),
        (k, _) if k.ends_with("attention.value_length") => g.value_length = Some(v),
        _ => {}
    }
}

/// Layers that actually hold a KV cache.
///
/// NOT `block_count`. An MTP block is subtracted, then a hybrid model caches
/// one layer in every `full_attention_interval`. Using `block_count` directly
/// over-estimates `ornith-1.5-35b-a3b` by 4x, which refuses configurations
/// that fit.
#[must_use]
pub fn kv_layers(g: &Geometry) -> Option<u32> {
    let n_layer = g
        .block_count?
        .saturating_sub(g.nextn_predict_layers.unwrap_or(0));
    Some(match g.full_attention_interval {
        Some(i) if i > 0 => n_layer / i,
        _ => n_layer,
    })
}

/// KV cache bytes at `ctx`, in exact integer arithmetic.
///
/// `q8_0` is 34 bytes per 32 elements — 17/16, not 1/2. Over a 262144-token
/// cache that 6.25% is hundreds of MiB.
#[must_use]
pub fn kv_bytes(g: &Geometry, ctx: u32, q8_cache: bool) -> Option<u64> {
    let layers = u64::from(kv_layers(g)?);
    let heads = u64::from(g.head_count_kv?);
    let ek = u64::from(g.key_length?) * heads;
    // MLA allocates K only.
    let ev = if g.key_length_mla.is_some() {
        0
    } else {
        u64::from(g.value_length?) * heads
    };
    let cells = u64::from(pad256(ctx));
    let elems = layers * (ek + ev) * cells;
    Some(if q8_cache { elems * 17 / 16 } else { elems * 2 })
}

/// llama.cpp pads the context up to a multiple of 256.
#[must_use]
pub const fn pad256(ctx: u32) -> u32 {
    ctx.div_ceil(256) * 256
}

#[cfg(test)]
mod tests {
    use super::{Geometry, kv_bytes, kv_layers, pad256, parse_geometry};

    /// Build a minimal GGUF header with the given u32 metadata keys.
    fn header(arch: &str, keys: &[(&str, u32)]) -> Vec<u8> {
        let mut b = Vec::from(*b"GGUF");
        b.extend(3_u32.to_le_bytes()); // version
        b.extend(0_u64.to_le_bytes()); // tensor_count
        b.extend((keys.len() as u64 + 1).to_le_bytes());
        // general.architecture (string, type 8)
        b.extend((b"general.architecture".len() as u64).to_le_bytes());
        b.extend(b"general.architecture");
        b.extend(8_u32.to_le_bytes());
        b.extend((arch.len() as u64).to_le_bytes());
        b.extend(arch.as_bytes());
        for (k, v) in keys {
            b.extend((k.len() as u64).to_le_bytes());
            b.extend(k.as_bytes());
            b.extend(4_u32.to_le_bytes()); // uint32
            b.extend(v.to_le_bytes());
        }
        b
    }

    fn ornith() -> Geometry {
        // Geometry verified against a live `llama-cli -v` log for
        // ornith-1.5-35b-a3b: 41 blocks, 1 MTP, interval 4, ek = ev = 512.
        parse_geometry(&header(
            "qwen3moe",
            &[
                ("qwen3moe.block_count", 41),
                ("qwen3moe.nextn_predict_layers", 1),
                ("qwen3moe.full_attention_interval", 4),
                ("qwen3moe.attention.head_count_kv", 4),
                ("qwen3moe.attention.key_length", 128),
                ("qwen3moe.attention.value_length", 128),
            ],
        ))
        .expect("a parsable header")
    }

    #[test]
    fn a_non_gguf_file_is_refused_rather_than_misread() {
        assert_eq!(parse_geometry(b"NOTGGUF...."), None);
        assert_eq!(parse_geometry(b"GGUF"), None, "truncated header");
    }

    #[test]
    fn the_architecture_and_geometry_come_out_of_the_metadata_block() {
        let g = ornith();
        assert_eq!(g.arch, "qwen3moe");
        assert_eq!(g.block_count, Some(41));
        assert_eq!(g.nextn_predict_layers, Some(1));
        assert_eq!(g.full_attention_interval, Some(4));
    }

    #[test]
    fn kv_layers_is_not_the_block_count() {
        // (41 - 1 MTP) / 4 = 10, not 41. Using block_count over-estimates by 4x
        // and refuses configurations that fit.
        assert_eq!(kv_layers(&ornith()), Some(10));
    }

    #[test]
    fn the_ornith_regression_vector_reproduces_to_the_byte() {
        // Spec §4.5: 10 x 2 x 512 x 262144 x 1.0625 = 2,852,126,720 B.
        assert_eq!(
            kv_bytes(&ornith(), 262_144, true),
            Some(2_852_126_720),
            "the worked example must reproduce exactly, not approximately"
        );
    }

    #[test]
    fn q8_cache_is_seventeen_sixteenths_not_a_half() {
        let g = ornith();
        let q8 = kv_bytes(&g, 262_144, true).expect("q8");
        let f16 = kv_bytes(&g, 262_144, false).expect("f16");
        assert_eq!(f16 * 17 / 32, q8, "q8_0 is 34 bytes per 32 elements");
        assert_ne!(q8, f16 / 2, "the common wrong answer");
    }

    #[test]
    fn mla_models_allocate_k_only() {
        let mut g = ornith();
        g.key_length_mla = Some(576);
        let with_mla = kv_bytes(&g, 4096, true).expect("mla");
        g.key_length_mla = None;
        let without = kv_bytes(&g, 4096, true).expect("gqa");
        assert_eq!(with_mla * 2, without, "MLA caches K but not V");
    }

    #[test]
    fn a_head_count_array_takes_its_first_element() {
        // head_count_kv is an ARRAY on some hybrid models; a scalar-only parser
        // either errors or silently misreads.
        let mut b = Vec::from(*b"GGUF");
        b.extend(3_u32.to_le_bytes());
        b.extend(0_u64.to_le_bytes());
        b.extend(1_u64.to_le_bytes());
        let k = "qwen3moe.attention.head_count_kv";
        b.extend((k.len() as u64).to_le_bytes());
        b.extend(k.as_bytes());
        b.extend(9_u32.to_le_bytes()); // array
        b.extend(4_u32.to_le_bytes()); // of uint32
        b.extend(3_u64.to_le_bytes()); // 3 entries
        for v in [8_u32, 8, 8] {
            b.extend(v.to_le_bytes());
        }
        assert_eq!(parse_geometry(&b).expect("parsed").head_count_kv, Some(8));
    }

    #[test]
    fn the_context_is_padded_up_to_a_multiple_of_256() {
        assert_eq!(pad256(262_144), 262_144);
        assert_eq!(
            pad256(100),
            256,
            "GGML_PAD rounds UP despite the log message"
        );
        assert_eq!(pad256(257), 512);
    }
}
