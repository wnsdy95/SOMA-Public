//! Optional context quality modules.
//!
//! ADR 0014 reset SOMA's core product to the ContextEnvelope path for
//! cloud LLMs. The research-era 4-layer memory hierarchy remains the
//! internal architecture; this module contains candidate neural
//! implementations for that hierarchy. ADR 0015 records the code-audited
//! disposition: these components are retained only when they improve
//! ContextEnvelope ranking, scoping, compression, conflict detection, or
//! evidence selection. They are not the product identity.
//!
//! ADR 0006 / 0007 still explain where the implementations came from.
//! Each component reproduces the forward-pass math of its source paper
//! with frozen weights; trainable variants live behind `cognitive-train`.
//!
//! Module surface:
//!
//! * `hopfield` — connected candidate for `relevant_memory` ranking.
//! * `mlstm` — connected candidate for budgeted `thread_state`
//!   evidence selection.
//! * `ipc` — connected candidate for cited anomaly `open_decisions`.
//! * `self_model` / `anil_classifier` — connected candidate for
//!   explicit-filter-safe default project scope selection.

#![allow(clippy::needless_range_loop, clippy::doc_lazy_continuation)]

/// v1.2 chunk 2 (ADR 0009) — ANIL classifier head trainable via
/// candle. Same `cognitive-train` feature gate.
pub mod anil_classifier;
pub mod hopfield;
pub mod hopfield_backend;
/// v1.2 chunk 4 (ADR 0011) — trainable Q/K/V Hopfield projections
/// via candle. Same `cognitive-train` feature gate.
pub mod hopfield_trainable;
pub mod ipc;
/// v1.2 chunk 3 (ADR 0010) — multi-layer iPC predictor trainable
/// via candle. Same `cognitive-train` feature gate.
pub mod ipc_trainable;
pub mod mlstm;
/// v1.2 chunk 1 (ADR 0008) — trainable mLSTM Q/K/V via candle.
/// Module body is `#[cfg(feature = "cognitive-train")]` so default +
/// `cognitive` builds skip the dep entirely.
pub mod mlstm_trainable;
pub mod self_model;
