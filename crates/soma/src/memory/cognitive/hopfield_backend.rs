//! `HopfieldBackend` — drop-in alternative to `SemanticIndex`'s HNSW
//! retrieval, backed by `PaperHopfield`.
//!
//! STAGE 3-A per ADR 0006. The existing `SemanticIndex` (HNSW via
//! `instant-distance`) returns the top-k cosine-nearest patterns;
//! a Hopfield backend returns the same shape but the retrieval is
//! the *Ramsauer 2020 update rule* — softmax-weighted across all
//! patterns with multi-head LayerNorm + 1/√d scaling.
//!
//! The two backends are parallel (the context pack builder picks one
//! via `PackConfig::backend`); SOMA's recall callers see the same
//! `Vec<(EpisodeId, f32)>` shape regardless. STAGE 3.1 (trainable
//! Q/K/V/O) lands separately when `cognitive-train` is decided.

use std::sync::{Arc, Mutex};

use crate::memory::cognitive::hopfield::PaperHopfield;
use crate::memory::embed::Embedder;
use crate::memory::semantic::SemanticError;
use crate::storage::{EpisodeId, Storage};

/// Hopfield-backed recall over `episode_vectors` of one model.
///
/// Construct via `open` (reads all vectors for the embedder's
/// `model_id`), then call `recall(query, k)` for retrieval. Like
/// `SemanticIndex`, the v1 backend rebuilds on every `open` —
/// pattern set is small (≤10K) and the rebuild cost is the cosine
/// projection across vectors.
pub struct HopfieldBackend {
    embedder: Arc<dyn Embedder>,
    /// Lookup so the (`pattern_idx` → `EpisodeId`) translation is
    /// deterministic post-rebuild.
    ids: Vec<EpisodeId>,
    hopfield: PaperHopfield,
    /// chunk 4.5 — when present, query embeddings are projected
    /// through `w_q` before `hopfield.recall`. (Stored patterns
    /// were already projected through `w_v` at `open_with` time.)
    /// `None` falls back to identity / chunk 4.1 behavior.
    trained: Option<TrainedProjections>,
}

/// chunk 4.5 — flat row-major `(d_emb, d_emb)` matrices loaded
/// from `hopfield_weights`. Only Q and V are used by the wire;
/// K is reserved for v2 multi-head split.
struct TrainedProjections {
    w_q: Vec<f32>,
    #[allow(dead_code)]
    w_k: Vec<f32>,
    w_v: Vec<f32>,
}

impl HopfieldBackend {
    /// Default β = 8.0 matches the discussion 0037 §D90 retrieval
    /// sharpness for `MemoryPack::semantic` (parity with HNSW path).
    pub const DEFAULT_BETA: f32 = 8.0;
    /// 4 heads over a 384-d embedding gives d_head = 96 — large
    /// enough for the LayerNorm-on-keys to discriminate between
    /// real patterns without flattening the structure.
    pub const DEFAULT_HEADS: usize = 4;

    /// Build a fresh Hopfield from every vector for the embedder's
    /// `model_id`. Mirrors `SemanticIndex::open`.
    pub fn open(
        storage: Arc<Mutex<Storage>>,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, SemanticError> {
        Self::open_with(storage, embedder, Self::DEFAULT_HEADS, Self::DEFAULT_BETA)
    }

    /// `open` + tunable `(num_heads, beta)`. `d_emb` MUST be
    /// divisible by `num_heads`; if not, `PaperHopfield::new`
    /// panics.
    ///
    /// chunk 4.5 wire — when `cognitive-train` is on AND the
    /// trainable Hopfield weights row exists in storage, every
    /// stored pattern is pre-projected through `W_v` before
    /// `hopfield.write`. Recall queries are projected through
    /// `W_q` before retrieval. The frozen attention rule then
    /// operates on the trained representation. When weights are
    /// absent the backend falls back to identity (chunk 4.1
    /// pre-train semantics).
    pub fn open_with(
        storage: Arc<Mutex<Storage>>,
        embedder: Arc<dyn Embedder>,
        num_heads: usize,
        beta: f32,
    ) -> Result<Self, SemanticError> {
        let dim = embedder.dim();
        let model_id = embedder.model_id();
        let rows = {
            let guard = crate::util::mutex::lock_or_recover(&storage);
            guard.vectors_for_model(model_id)?
        };
        let trained = load_trained_projections(&storage, dim);
        let hopfield = PaperHopfield::new(dim, num_heads, beta);
        let mut ids = Vec::with_capacity(rows.len());
        for (id, vec) in rows {
            if vec.len() != dim {
                return Err(SemanticError::DimMismatch { expected: dim, got: vec.len() });
            }
            let projected = match &trained {
                Some(w) => matmul_row(&w.w_v, &vec, dim),
                None => vec,
            };
            hopfield.write(&projected);
            ids.push(id);
        }
        Ok(Self { embedder, ids, hopfield, trained })
    }

    /// Number of patterns in the underlying Hopfield. Diagnostic.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Top-k retrieval — same shape as `SemanticIndex::recall`.
    /// Returns `(EpisodeId, weight)` ordered by the Hopfield head-
    /// averaged softmax weight (DESC). Weight semantics:
    ///
    /// * Sum of weights across ALL patterns ≈ 1.0 (softmax invariant).
    /// * Truncated to `k` so the caller still gets a bounded list.
    pub fn recall(&self, query: &str, k: usize) -> Result<Vec<(EpisodeId, f32)>, SemanticError> {
        if self.ids.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        // D138 — query-side prefix on the recall path. Mirrors
        // `SemanticIndex::recall` so the two retrieval backends route
        // through the same e5 prefix policy.
        let qv = self.embedder.embed_query(query);
        let dim = self.embedder.dim();
        if qv.len() != dim {
            return Err(SemanticError::DimMismatch { expected: dim, got: qv.len() });
        }
        // chunk 4.5 — apply trained Q projection if available so the
        // query lives in the same space as the pre-projected stored
        // patterns. Identity passthrough when `trained = None`.
        let projected_q = match &self.trained {
            Some(t) => matmul_row(&t.w_q, &qv, dim),
            None => qv,
        };
        let (_output, hits) = self.hopfield.recall(&projected_q);
        Ok(hits
            .into_iter()
            .take(k)
            .filter_map(|h| self.ids.get(h.pattern_idx).map(|id| (*id, h.weight)))
            .collect())
    }

    /// Embedder model id — diagnostic, mirrors `SemanticIndex`.
    pub fn model_id(&self) -> &'static str {
        self.embedder.model_id()
    }
}

/// chunk 4.5 — pull `hopfield_weights` row when `cognitive-train`
/// is on AND the row exists with the expected `dim`. Returns
/// `None` on any mismatch / absence — caller treats as identity.
fn load_trained_projections(
    storage: &Arc<Mutex<Storage>>,
    dim: usize,
) -> Option<TrainedProjections> {
    // Round 1 in-house ultrareview fix: pre-fix `.ok().flatten()?` chain
    // collapsed "row absent (cold start)" and "DB I/O error" into a
    // single None — operator hitting a transient storage failure saw
    // silent identity-init fallback with no log. Now log explicitly
    // and still fall through (identity is correct fallback for both
    // cases; only the diagnostic surface differs).
    let row = match crate::util::mutex::lock_or_recover(storage).get_hopfield_weights() {
        Ok(opt) => opt?,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "hopfield_backend: get_hopfield_weights failed; falling back to identity-init"
            );
            return None;
        }
    };
    let (stored_dim, _heads, w_q, w_k, w_v, _steps, _ts) = row;
    if stored_dim != dim {
        return None;
    }
    let expected = dim * dim;
    if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
        return None;
    }
    // Defense in depth — Storage's NaN guard should have stopped
    // this before persistence, but the recall path is hot.
    if w_q.iter().chain(w_k.iter()).chain(w_v.iter()).any(|v| !v.is_finite()) {
        return None;
    }
    Some(TrainedProjections { w_q, w_k, w_v })
}

/// Compute `result = W · x` where `W` is row-major `(dim × dim)`.
/// Used by chunk 4.5 to apply trained Q/V projections without
/// reaching for candle on the hot path (we only need a single
/// matrix-vector product, no autograd).
fn matmul_row(w: &[f32], x: &[f32], dim: usize) -> Vec<f32> {
    if w.len() != dim * dim || x.len() != dim {
        return x.to_vec(); // shape mismatch — identity passthrough
    }
    let mut out = vec![0.0_f32; dim];
    for i in 0..dim {
        let mut acc = 0.0_f32;
        for j in 0..dim {
            acc += w[i * dim + j] * x[j];
        }
        out[i] = acc;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embed::HashEmbedder;
    use crate::storage::Episode;

    fn ep(ts: i64, prompt: &str) -> Episode {
        use crate::storage::EpisodeSource;
        Episode {
            ts_start_ns: ts,
            ts_end_ns: ts,
            duration_ms: 0,
            source: EpisodeSource::ClaudeCode,
            session_id: None,
            prompt_text: Some(prompt.into()),
            response_text: None,
            command: None,
            stdout: None,
            exit_code: None,
            cwd: None,
            git_branch: None,
            project: None,
            digest: None,
        }
    }

    fn seed(prompts: &[&str]) -> Arc<Mutex<Storage>> {
        let mut store = Storage::open_in_memory().unwrap();
        let embedder = HashEmbedder::new();
        for (i, p) in prompts.iter().enumerate() {
            let id = store.append_episode(&ep(i as i64, p)).unwrap();
            let v = embedder.embed(p);
            store.put_vector(id, embedder.model_id(), &v).unwrap();
        }
        Arc::new(Mutex::new(store))
    }

    #[test]
    fn empty_store_returns_empty_recall() {
        let storage = Arc::new(Mutex::new(Storage::open_in_memory().unwrap()));
        let backend = HopfieldBackend::open(storage, Arc::new(HashEmbedder::new())).unwrap();
        assert!(backend.is_empty());
        assert!(backend.recall("anything", 5).unwrap().is_empty());
    }

    #[test]
    fn recall_returns_top_k_in_weight_order() {
        let storage = seed(&["alpha", "beta", "gamma", "delta", "epsilon"]);
        let backend = HopfieldBackend::open(storage, Arc::new(HashEmbedder::new())).unwrap();
        assert_eq!(backend.len(), 5);

        let hits = backend.recall("alpha", 3).unwrap();
        assert_eq!(hits.len(), 3);
        // Weights are DESC across the returned slice.
        for w in hits.windows(2) {
            assert!(w[0].1 >= w[1].1, "weights must be DESC: {} >= {}", w[0].1, w[1].1);
        }
    }

    #[test]
    fn dim_mismatch_surfaces_typed_error() {
        let mut store = Storage::open_in_memory().unwrap();
        let id = store.append_episode(&ep(0, "x")).unwrap();
        // Stuff a wrong-dim vector under HashEmbedder's model_id —
        // put_vector renormalizes and stores whatever len the caller
        // passes; the open path then trips the dim check.
        let bad: Vec<f32> = vec![1.0; 100];
        store.put_vector(id, "soma-hash-v1-384d", &bad).unwrap();

        let storage = Arc::new(Mutex::new(store));
        let result = HopfieldBackend::open(storage, Arc::new(HashEmbedder::new()));
        match result {
            Err(SemanticError::DimMismatch { expected, got }) => {
                assert_eq!(expected, 384);
                assert_eq!(got, 100);
            }
            Ok(_) => panic!("expected DimMismatch, got Ok"),
            Err(SemanticError::Storage(e)) => panic!("expected DimMismatch, got Storage: {e}"),
        }
    }
}
