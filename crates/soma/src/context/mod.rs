//! ContextEnvelope assembly and rendering.
//!
//! The legacy `MemoryPack` shape remains an internal retrieval substrate for
//! recent/semantic recall, but the cloud-LLM-facing contract is the cited
//! `ContextEnvelope`: thread state, ranked relevant memory, policy, decisions,
//! corrections, and optional compiler notes.

pub mod cloud_prompt;
pub mod compiler;
pub mod correction;
pub mod critic;
pub mod disposition;
pub mod envelope;
pub mod eval;
pub mod explain;
pub mod latent_eval;
pub mod latent_predictor;
pub(crate) mod matching;
pub mod open_decision_review;
pub mod pack;
pub mod quality;
pub mod review;
pub mod review_action;
pub mod review_apply;
pub mod review_drain;
pub mod scheduler_control;
pub(crate) mod scope;
pub mod semantic_learning;
pub mod task_frame;
pub mod thread_identity;
pub mod why;
