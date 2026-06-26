//! Self-model rule extractors. Each emits rows into `self_state`
//! with `evidence_count` + explicit references so
//! `soma explain-why` can attribute every trait to its episodes.
//! Phase 5.

pub mod exit_success;
pub mod project_norms;
pub mod tool_use;
