//! Semantic index — HNSW over `episode_vectors` via
//! `instant-distance`. Discussion 0028 §H + §I.
//!
//! v1 = **rebuild on open**. Every construction reads all vectors
//! for the configured `model_id` from `episode_vectors` and builds
//! an in-memory HNSW (`instant-distance::Hnsw`). Subsequent
//! `index_episode` calls embed the text, `PUT` into
//! `episode_vectors`, and append to the in-memory index.
//!
//! Persistence of the HNSW file itself (reload-without-rebuild) is
//! v1.1 (D68-cand) — the rebuild cost on ≤10K episodes is sub-100ms
//! in practice, acceptable for `soma recall` CLI invocations.
//!
//! Distance metric = cosine via `1.0 - (a · b)` on L2-normalized
//! inputs (Embedder contract guarantees L2 norm).

use std::sync::{Arc, Mutex};

use instant_distance::{Builder as HnswBuilder, HnswMap, Point, Search};

use crate::memory::embed::Embedder;
use crate::storage::{EpisodeId, Storage, StorageError};

/// Errors surfaced by `SemanticIndex` operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum SemanticError {
    Storage(StorageError),
    /// Vector length from the DB didn't match the embedder's
    /// `dim()`. Indicates someone swapped models without migrating
    /// `episode_vectors`, or DB corruption.
    DimMismatch {
        expected: usize,
        got: usize,
    },
}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticError::Storage(e) => write!(f, "storage: {e}"),
            SemanticError::DimMismatch { expected, got } => {
                write!(
                    f,
                    "vector dim mismatch: embedder expects dim={expected}, DB row \
                     has dim={got}. The embedder model changed (e.g. Mini → Studio \
                     upgrade) without re-embedding existing vectors. Wait for the \
                     resident's slow_loop primary-model backfill (~60 min cycle, 64 \
                     episodes/tick) or re-ingest the affected episodes."
                )
            }
        }
    }
}

impl std::error::Error for SemanticError {}

impl From<StorageError> for SemanticError {
    fn from(e: StorageError) -> Self {
        SemanticError::Storage(e)
    }
}

/// A 384-d (or other dim-d) cosine point. Wraps `Vec<f32>` so
/// `instant-distance::Point` is implemented for the exact shape the
/// Embedder produces.
#[derive(Debug, Clone)]
struct CosinePoint(Vec<f32>);

impl Point for CosinePoint {
    fn distance(&self, other: &Self) -> f32 {
        // Both inputs are L2-normalized per the Embedder contract,
        // so cosine distance = 1 - dot. This saves the ||a|| ||b||
        // normalization and is branch-free.
        let dot: f32 = self.0.iter().zip(other.0.iter()).map(|(a, b)| a * b).sum();
        1.0 - dot
    }
}

/// HNSW over one model's vectors. Construct via `open` once per
/// process; mutate via `index_episode`; query via `recall`.
pub struct SemanticIndex {
    embedder: Arc<dyn Embedder>,
    storage: Arc<Mutex<Storage>>,
    index: HnswMap<CosinePoint, EpisodeId>,
}

impl SemanticIndex {
    /// Rebuild the in-memory HNSW from `episode_vectors` rows
    /// whose `model_id` matches the embedder's id.
    pub fn open(
        storage: Arc<Mutex<Storage>>,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, SemanticError> {
        let dim = embedder.dim();
        let model_id = embedder.model_id();
        let rows = {
            let guard = crate::util::mutex::lock_or_recover(&storage);
            guard.vectors_for_model(model_id)?
        };

        let mut points = Vec::with_capacity(rows.len());
        let mut ids = Vec::with_capacity(rows.len());
        for (id, vec) in rows {
            if vec.len() != dim {
                return Err(SemanticError::DimMismatch { expected: dim, got: vec.len() });
            }
            points.push(CosinePoint(vec));
            ids.push(id);
        }

        let index = HnswBuilder::default().build(points, ids);
        Ok(Self { embedder, storage, index })
    }

    /// Embed `text`, persist the vector (upsert on `(episode_id,
    /// model_id)`), and rebuild the in-memory index. This
    /// simplified v1 path rebuilds on every insert — `instant-
    /// distance::Hnsw` is immutable post-build, so adding one
    /// point means `build()` again. For ≤10K episode volumes the
    /// rebuild cost is acceptable; v1.1 swaps in an incremental
    /// structure if/when the volume breaks 10K.
    pub fn index_episode(
        &mut self,
        episode_id: EpisodeId,
        text: &str,
    ) -> Result<(), SemanticError> {
        // D138 — passage-side prefix on the stored corpus. Non-e5
        // backends use the default `embed` delegation, so this is a
        // no-op for Hash / MiniLM and only matters for E5LargeEmbedder.
        let vec = self.embedder.embed_passage(text);
        if vec.len() != self.embedder.dim() {
            return Err(SemanticError::DimMismatch {
                expected: self.embedder.dim(),
                got: vec.len(),
            });
        }
        {
            let mut guard = crate::util::mutex::lock_or_recover(&self.storage);
            guard.put_vector(episode_id, self.embedder.model_id(), &vec)?;
        }
        // Rebuild from DB — keeps the index + on-disk state in
        // lock-step with zero extra state to drift.
        let rows = {
            let guard = crate::util::mutex::lock_or_recover(&self.storage);
            guard.vectors_for_model(self.embedder.model_id())?
        };
        let mut points = Vec::with_capacity(rows.len());
        let mut ids = Vec::with_capacity(rows.len());
        let dim = self.embedder.dim();
        for (id, v) in rows {
            if v.len() != dim {
                return Err(SemanticError::DimMismatch { expected: dim, got: v.len() });
            }
            points.push(CosinePoint(v));
            ids.push(id);
        }
        self.index = HnswBuilder::default().build(points, ids);
        Ok(())
    }

    /// Top-k cosine-similar `EpisodeId`s for the query text. Each
    /// result is `(episode_id, similarity)` where similarity is in
    /// `[-1, 1]` and higher is closer. Results are ordered highest
    /// similarity first.
    pub fn recall(&self, query: &str, k: usize) -> Result<Vec<(EpisodeId, f32)>, SemanticError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        // D138 — query-side prefix on retrieval; pairs with
        // `index_episode`'s passage-side prefix so e5 cosines peak in
        // the joint retrieval space. Non-e5 backends are unaffected.
        let qv = self.embedder.embed_query(query);
        if qv.len() != self.embedder.dim() {
            return Err(SemanticError::DimMismatch {
                expected: self.embedder.dim(),
                got: qv.len(),
            });
        }
        let qp = CosinePoint(qv);
        let mut search = Search::default();
        let hits = self.index.search(&qp, &mut search);
        let mut out: Vec<(EpisodeId, f32)> = hits
            .take(k)
            .map(|item| {
                // `distance` is 1 - cosine, so similarity = 1 -
                // distance.
                let sim = 1.0 - item.distance;
                (*item.value, sim)
            })
            .collect();
        // Round 1 in-house ultrareview fix: filter NaN before sort.
        // Embedder contract excludes non-finite vectors, but a corrupt
        // episode_vectors BLOB or a downstream caller violating the
        // contract could produce NaN similarity → partial_cmp returns
        // None → unwrap_or(Equal) means NaN treated as equal to all
        // values, sort order non-deterministic. Drop them defensively.
        out.retain(|(_, sim)| sim.is_finite());
        // `instant-distance` already returns nearest-first, but
        // sort defensively in case internal ordering ever changes.
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    /// Current index size. Diagnostic accessor.
    pub fn len(&self) -> usize {
        // `HnswMap` has no public `len`; the value list mirrors it.
        self.index.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
