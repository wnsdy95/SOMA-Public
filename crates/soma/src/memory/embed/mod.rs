//! Embedding backends. Discussion 0028 §A + §E.
//!
//! Public surface:
//!
//! * [`Embedder`] — the trait. Stateless, `Send + Sync`, sync
//!   methods (async callers wrap in `spawn_blocking`).
//! * [`HashEmbedder`] — v1 default. Deterministic 384-d hash-
//!   trigram projection, pure Rust, no system deps. Ported
//!   verbatim from the WS5 PR 5.2 memory kernel under the same
//!   algorithm pins (SplitMix64 + byte-trigrams + length bias +
//!   L2 normalize).
//! * [`mini_l12`] — ONNX-Runtime-backed MiniLM-L12-v2 behind the
//!   `embed-onnx` Cargo feature.
//! * [`e5_large`] — multilingual-e5-large 1024d, Studio profile.
//!
//! Process-wide singletons via [`OnceLock`] — fastembed model loads
//! cost 1-3s per `try_new`. P1-A external-review fix (`docs/code-
//! reviews/2026-04-28-external-review-followup.md`): pre-fix every
//! ingest / recall / slow_loop call instantiated a fresh embedder,
//! reading the ONNX session into RAM each time and busting the OS
//! file cache; post-fix we hold one `Arc<dyn Embedder>` per backend
//! for the process lifetime.

use std::sync::{Arc, OnceLock};

pub mod e5_large;
pub mod mini_l12;

mod hash;

pub use e5_large::{E5Error, E5LargeEmbedder};
pub use hash::HashEmbedder;
pub use mini_l12::{OnnxEmbedder, OnnxError};

/// Process-static primary embedder cache (`select_embedder` result).
/// `OnceLock` — first hot-path caller pays the fastembed init cost,
/// every subsequent caller reuses the same `Arc`.
static PRIMARY_EMBEDDER: OnceLock<Arc<dyn Embedder>> = OnceLock::new();

/// Process-static secondary embedder slot. Plan §D3 — Studio
/// profile stores a 384d MiniLM **boosted secondary** alongside
/// the e5-large 1024d primary, so Mini-origin episodes (which
/// only have 384d) remain comparable on a Mini→Studio upgrade,
/// and Studio-stored episodes keep their 384d row when a future
/// downgrade happens. `None` = no secondary (Mini profile or no
/// embed-onnx feature).
static SECONDARY_EMBEDDER: OnceLock<Option<Arc<dyn Embedder>>> = OnceLock::new();

/// Process-wide default embedder. Cached for the process lifetime
/// — see [`PRIMARY_EMBEDDER`] for the rationale.
///
/// Profile-aware:
/// * **Studio** (RAM ≥ 60 GiB threshold per `profile::detect`):
///   D69 — try `E5LargeEmbedder` (1024d multilingual-e5-large).
/// * **Mini** (default, ≤24GB target): D70 — try `OnnxEmbedder`
///   (384d MiniLM-L12).
/// * Onnx model not downloaded / `embed-onnx` feature off →
///   `HashEmbedder` (deterministic 384d hash projection, v1 default).
///
/// **Test note** — `OnceLock` cannot be reset, so once the first
/// caller in a test process initializes the singleton every
/// subsequent call returns that same backend. Profile-dependent
/// integration tests that need a fresh build must run as their
/// own process (e.g. via `cargo test --test foo` boundary) or
/// inject through a separate seam — the previous docstring
/// claim of a `reset_embedder_cache_for_tests` hook was wrong
/// (Codex 2차 review 2026-04-28).
pub fn select_embedder() -> Arc<dyn Embedder> {
    PRIMARY_EMBEDDER.get_or_init(build_primary_embedder).clone()
}

fn build_primary_embedder() -> Arc<dyn Embedder> {
    #[cfg(feature = "embed-onnx")]
    {
        if matches!(crate::profile::detect(), crate::config::Profile::Studio) {
            match E5LargeEmbedder::new() {
                Ok(e5) => return Arc::new(e5),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "Studio ContextEnvelope evidence ranking model unavailable; \
                         falling back to MiniLM. \
                         Run `soma install --model multilingual-e5-large` to enable."
                    );
                }
            }
        }
        match OnnxEmbedder::new() {
            Ok(onnx) => return Arc::new(onnx),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "Mini ContextEnvelope evidence ranking model unavailable; \
                     falling back to deterministic HashEmbedder. \
                     Run `soma install --model paraphrase-multilingual-MiniLM-L12-v2` to enable."
                );
            }
        }
    }
    Arc::new(HashEmbedder::new())
}

/// D69 close (2026-05-01) — dual-store-on-ingest secondary backend.
/// Returns `Some(MiniLM)` when on Studio with both ONNX models on
/// disk; `None` everywhere else. The hot ingest path calls this
/// after the primary store and writes a parallel secondary vector
/// row when present. Recall still reads only the primary model_id
/// (HNSW indexes are per-model) — the secondary is purely
/// cross-profile backfill freight.
pub fn select_secondary_embedder() -> Option<Arc<dyn Embedder>> {
    SECONDARY_EMBEDDER.get_or_init(build_secondary_embedder).clone()
}

#[cfg_attr(not(feature = "embed-onnx"), allow(unused_mut))]
fn build_secondary_embedder() -> Option<Arc<dyn Embedder>> {
    #[cfg(feature = "embed-onnx")]
    {
        if matches!(crate::profile::detect(), crate::config::Profile::Studio) {
            // Studio: secondary = MiniLM 384d (so Mini-origin episodes
            // stay symmetric in 384d-space). If the model isn't on
            // disk, silently skip — the primary 1024d still works.
            match OnnxEmbedder::new() {
                Ok(o) => return Some(Arc::new(o)),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "Studio secondary ContextEnvelope evidence ranking store unavailable; \
                         dual-store skipped this run. \
                         Run `soma install --model paraphrase-multilingual-MiniLM-L12-v2` to enable."
                    );
                }
            }
        }
    }
    None
}

/// All active embedders (primary first, secondary if any). Used by
/// `soma backfill` and slow_loop's backfill helper to drive a
/// model_id-keyed write per active backend.
pub fn select_active_embedders() -> Vec<Arc<dyn Embedder>> {
    let mut v = vec![select_embedder()];
    if let Some(sec) = select_secondary_embedder() {
        v.push(sec);
    }
    v
}

/// Text → dense vector. All impls return **L2-normalized**
/// vectors so `SemanticIndex` can use plain cosine similarity
/// without per-call renormalization.
pub trait Embedder: Send + Sync {
    /// Stable identifier written to `episode_vectors.model_id`.
    /// Must be unique across backends — a rename is a new
    /// identity. Collisions produce a `SemanticError` at index
    /// build time (different vectors under the same model tag
    /// poison HNSW neighbor quality).
    fn model_id(&self) -> &'static str;

    /// Output dimensionality. Callers validate against
    /// `episode_vectors.dim` before insertion.
    fn dim(&self) -> usize;

    /// Embed a single string. Returns L2-normalized `dim()` floats.
    /// Empty input returns `e_0` (`[1.0, 0.0, …]`) rather than
    /// panicking — the degenerate-input contract from the WS5 PR
    /// 5.2 kernel.
    fn embed(&self, text: &str) -> Vec<f32>;

    /// D138 — passage-side embed for the asymmetric ingest path.
    ///
    /// e5 family models are trained with `passage: ` / `query: `
    /// prefix asymmetry: the corpus side gets `passage: `, the
    /// retrieval side gets `query: `. Symmetric backends (Hash,
    /// MiniLM) ignore the asymmetry — the default impl delegates to
    /// [`Self::embed`] so their behavior is unchanged. Only
    /// [`E5LargeEmbedder`] overrides this method to add the prefix.
    ///
    /// Call sites: `capture::ai_cli::run_ingest`,
    /// `runtime::scheduler::slow_loop::backfill_one_model`,
    /// `memory::semantic::SemanticIndex::index_episode`.
    fn embed_passage(&self, text: &str) -> Vec<f32> {
        self.embed(text)
    }

    /// D138 — query-side embed for the asymmetric recall path.
    ///
    /// See [`Self::embed_passage`] for the rationale. Symmetric
    /// backends use the default impl (delegate to [`Self::embed`]),
    /// so non-e5 backends preserve their pre-D138 cosine behavior.
    ///
    /// Call sites: `memory::semantic::SemanticIndex::recall`,
    /// `memory::cognitive::hopfield_backend::HopfieldBackend::recall`.
    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.embed(text)
    }
}
