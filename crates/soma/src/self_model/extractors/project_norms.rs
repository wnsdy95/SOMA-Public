//! project_norms extractor — per-project activity snapshot.
//! Discussion 0030 §G (project_state 별도 table 아님 — 여기 kind
//! 로 수용) + 0037 §D93 (magnitude/direction split via DoRA
//! insight, ADR 0004 §D).
//!
//! One fact per distinct `episodes.project` (non-null):
//! key = project name, value =
//! ```json
//! {
//!   "magnitude": {
//!     "episodes": <n>,
//!     "sources": {source_1: count, ...},
//!     "git_branches": [...]
//!   },
//!   "direction_b64": "<base64 of L2-normalized centroid>",
//!   "direction_dim": <d>
//! }
//! ```
//! evidence_ids = all contributing episodes. The `direction` half is
//! the centroid of per-project embeddings — a "preference vector"
//! that lets ContextEnvelope and operator/debug consumers compare
//! project semantics independent of activity volume.

use crate::memory::salience;
use crate::self_model::{SelfExtractor, SelfFact};
use crate::storage::{EpisodeId, Storage, StorageError};

pub struct ProjectNormsExtractor;

#[derive(Default)]
struct Accum {
    episode_count: u64,
    sources: std::collections::HashMap<String, u64>,
    branches: std::collections::BTreeSet<String>,
    evidence: Vec<EpisodeId>,
    centroid: Vec<f32>,    // running EMA — D93 §D direction half
    centroid_count: usize, // sample count, drives EMA α
}

impl SelfExtractor for ProjectNormsExtractor {
    fn kind(&self) -> &'static str {
        "project_norms"
    }

    fn extract(&self, storage: &Storage) -> Result<Vec<SelfFact>, StorageError> {
        let episodes = storage.all_episodes()?;
        // Pull the canonical (`hash-v1`) vectors once so the EMA
        // loop has O(N) lookups instead of N · per-episode reads.
        let model_id = crate::memory::embed::select_embedder().model_id();
        let vectors: std::collections::HashMap<EpisodeId, Vec<f32>> =
            storage.vectors_for_model(model_id).unwrap_or_default().into_iter().collect();

        let mut per_project: std::collections::HashMap<String, Accum> =
            std::collections::HashMap::new();

        for ep in &episodes {
            let Some(project) = ep.project.as_deref() else {
                continue;
            };
            let entry = per_project.entry(project.to_string()).or_default();
            entry.episode_count += 1;
            // D119 — aggregate by the kebab-case wire string so the
            // serialized `sources` map keeps stable JSON keys.
            *entry.sources.entry(ep.source.to_string()).or_default() += 1;
            if let Some(b) = ep.git_branch.as_deref() {
                entry.branches.insert(b.to_string());
            }
            entry.evidence.push(ep.id);
            if let Some(v) = vectors.get(&ep.id) {
                if entry.centroid.is_empty() {
                    entry.centroid = salience::l2_normalize(v);
                } else {
                    let alpha = 1.0 / (entry.centroid_count as f32 + 1.0);
                    entry.centroid = salience::update_centroid(&entry.centroid, v, alpha);
                }
                entry.centroid_count += 1;
            }
        }

        let mut facts = Vec::with_capacity(per_project.len());
        for (project, accum) in per_project {
            use base64::prelude::{Engine, BASE64_STANDARD};
            let sources: serde_json::Map<String, serde_json::Value> =
                accum.sources.into_iter().map(|(k, v)| (k, serde_json::Value::from(v))).collect();
            let branches: Vec<String> = accum.branches.into_iter().collect();
            let direction_dim = accum.centroid.len();
            let direction_b64 = if accum.centroid.is_empty() {
                String::new()
            } else {
                let mut bytes = Vec::with_capacity(direction_dim * 4);
                for v in &accum.centroid {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                BASE64_STANDARD.encode(&bytes)
            };
            facts.push(SelfFact {
                key: project,
                value: serde_json::json!({
                    "magnitude": {
                        "episodes": accum.episode_count,
                        "sources": sources,
                        "git_branches": branches,
                    },
                    "direction_b64": direction_b64,
                    "direction_dim": direction_dim,
                }),
                evidence_ids: accum.evidence,
            });
        }
        facts.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(facts)
    }
}
