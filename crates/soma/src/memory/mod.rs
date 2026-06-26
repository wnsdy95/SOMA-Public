//! Local context modules for capture, retrieval, policy extraction, and
//! optional quality candidates.
//!
//! * `episode` — raw capture source of truth (append-only
//!   SQLite).
//! * `semantic` — profile-aware HNSW vector index.
//! * `embed` — EmbeddingBackend impls (MiniLM-L12 for Mini,
//!   multilingual-e5-large + MiniLM-L12 for Studio).
//! * `salience` — deterministic surprise/novelty/contradiction
//!   kernel (frozen weights in v1).
//! * `memory_item` — MemoryRecord assembly from episode +
//!   enrichment.

pub mod beliefs;
// Legacy CLAUDE.md SOMA-section migration/debug helper.
// markers (`<!-- SOMA-BEGIN -->` / `<!-- SOMA-END -->`) 사이 만
// SOMA 가 own, 외부 content 보존.
pub mod claudemd;
#[cfg(feature = "cognitive")]
pub mod cognitive;
pub mod embed;
pub mod episode;
pub mod forgetting;
pub mod llm;
pub mod local_llm;
pub mod memory_item;
pub mod narrative;
pub mod persona;
// Interpretable policy extractor. Claude Haiku 활용 nat-lang rule
// + evidence cohort. Feeds ContextEnvelope.user_policy; hidden
// migration/debug commands can render markdown from the same self_state rows.
pub mod policy;
pub mod salience;
pub mod secret;
pub mod semantic;
