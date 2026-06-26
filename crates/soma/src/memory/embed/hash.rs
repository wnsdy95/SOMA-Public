//! Deterministic 384-d hash-trigram embedder. Pure Rust, no system
//! deps, stable across macOS / Linux / CI runners.
//!
//! Algorithm (ported verbatim from WS5 PR 5.2 memory kernel — see
//! `legacy/soma-terminal/src/memory/embed.rs` for the original red
//! test matrix that pinned these invariants):
//!
//! 1. Each byte-trigram `(b0, b1, b2)` is packed into a `u64`, fed
//!    to SplitMix64 with a pinned seed, and contributes to one
//!    bucket chosen by `mix % dim`. Sign comes from the low bit,
//!    magnitude from the upper mantissa bits projected into
//!    `[0.5, 1.0]` so no contribution is ever zero.
//! 2. A length-bias contribution keeps two strings that share all
//!    trigrams (e.g. `"ab"` vs `"ab "` — no trigrams in the first,
//!    one in the second) from collapsing onto the same vector.
//! 3. L2-normalize. Degenerate (empty / zero-norm) inputs fall back
//!    to the canonical basis vector `e_0` so callers never see NaN.
//!
//! Not semantically meaningful — this is the *pipeline-validation*
//! embedder (discussion 0028 §A). Real MiniLM ships behind the
//! `embed-onnx` feature flag.

use super::Embedder;

const SEED: u64 = 0x5A5A_C071_70F0_5EED;
const DIM: usize = 384;
const MODEL_ID: &str = "soma-hash-v1-384d";

/// Deterministic hash-projection embedder. Stateless — all calls
/// resolve through pure functions, and `&self` is `Send + Sync` by
/// construction.
#[derive(Debug, Default, Clone, Copy)]
pub struct HashEmbedder;

impl HashEmbedder {
    pub const fn new() -> Self {
        HashEmbedder
    }
}

impl Embedder for HashEmbedder {
    fn model_id(&self) -> &'static str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let bytes = text.as_bytes();
        let mut buf = vec![0f32; DIM];

        // Byte-level trigram iteration — UTF-8 byte units are
        // platform-independent, no locale collation dependency.
        let n = bytes.len();
        if n == 0 {
            // Empty input → e_0 after l2_normalize's zero-buf guard.
            return canonical_basis();
        }
        if n < 3 {
            // Short input — accumulate single-byte contributions so
            // `"a"` and `"b"` land in different spots.
            for (i, b) in bytes.iter().enumerate() {
                let mix = splitmix64(SEED ^ (*b as u64) ^ (i as u64));
                add_contribution(&mut buf, mix);
            }
        } else {
            for w in bytes.windows(3) {
                let gram = (w[0] as u64) | ((w[1] as u64) << 8) | ((w[2] as u64) << 16);
                let mix = splitmix64(SEED ^ gram);
                add_contribution(&mut buf, mix);
            }
        }

        // Command-length bias.
        let len_mix = splitmix64(SEED ^ 0xB1A5 ^ (bytes.len() as u64));
        add_contribution(&mut buf, len_mix);

        l2_normalize(&mut buf);
        buf
    }
}

fn splitmix64(mut x: u64) -> u64 {
    // Guy Steele's SplitMix64 finalizer constants — pinned so the
    // bit pattern is byte-stable across releases.
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn add_contribution(buf: &mut [f32], mix: u64) {
    let bucket = (mix % (DIM as u64)) as usize;
    let sign = if mix & 1 == 0 { 1.0 } else { -1.0 };
    let mantissa_bits = ((mix >> 1) & 0x00FF_FFFF) as f32 / (1u32 << 24) as f32;
    let magnitude = 0.5 + 0.5 * mantissa_bits;
    buf[bucket] += sign * magnitude;
}

fn l2_normalize(buf: &mut [f32]) {
    let sum_sq: f32 = buf.iter().map(|x| x * x).sum();
    let norm = sum_sq.sqrt();
    if norm.is_finite() && norm > 0.0 {
        for x in buf.iter_mut() {
            *x /= norm;
        }
    } else {
        // Zero-norm or NaN — fall back to e_0.
        for (i, x) in buf.iter_mut().enumerate() {
            *x = if i == 0 { 1.0 } else { 0.0 };
        }
    }
}

fn canonical_basis() -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    v[0] = 1.0;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l2(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn test_hash_embedder_is_deterministic() {
        let e = HashEmbedder::new();
        let a = e.embed("Help me refactor auth middleware");
        let b = e.embed("Help me refactor auth middleware");
        assert_eq!(a, b, "same input must produce byte-identical vector");
    }

    #[test]
    fn test_hash_embedder_returns_normalized_384d() {
        let e = HashEmbedder::new();
        let v = e.embed("hello world");
        assert_eq!(v.len(), 384);
        let norm = l2(&v);
        assert!((norm - 1.0).abs() < 1e-5, "l2 norm should be ~1.0, got {norm}");
    }

    #[test]
    fn test_hash_embedder_different_texts_different_vectors() {
        let e = HashEmbedder::new();
        let a = e.embed("alpha beta gamma");
        let b = e.embed("delta epsilon zeta");
        let c = cosine(&a, &b);
        assert!(c < 0.99, "distinct texts should have cosine < 0.99, got {c}");
    }

    #[test]
    fn test_empty_input_returns_canonical_basis() {
        let e = HashEmbedder::new();
        let v = e.embed("");
        assert_eq!(v[0], 1.0);
        for (i, x) in v.iter().enumerate().skip(1) {
            assert_eq!(*x, 0.0, "e_0 basis expected at idx {i}");
        }
    }

    #[test]
    fn test_model_id_and_dim_accessors() {
        let e = HashEmbedder::new();
        assert_eq!(e.model_id(), "soma-hash-v1-384d");
        assert_eq!(e.dim(), 384);
    }
}
