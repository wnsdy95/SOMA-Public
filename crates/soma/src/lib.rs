#![recursion_limit = "256"]

//! SOMA — local context layer for cloud LLMs on Apple Silicon.
//!
//! **v0.2.0-alpha — public API unstable.** Breaking changes may land
//! before the 1.0 release; pin a release tag for production use.
//!
//! ## What it is
//!
//! SOMA captures terminal commands and Claude / Codex CLI/App / Cursor / Continue
//! sessions into a local SQLite database, indexes the captured
//! episodes semantically, and serves a cited ContextEnvelope back to
//! the cloud LLM through the Model Context Protocol so the model can
//! resume work across sessions without the user re-explaining context.
//! The resident daemon, embedder stack, and optional context quality
//! modules all run locally; only cloud LLM inference itself crosses
//! the network.
//!
//! ## Public entry points
//!
//! * [`capture::ai_cli::run_ingest`] — append an episode (terminal
//!   command, Claude prompt/response, tool output, …) to the DB.
//! * [`context::pack::build_memory_pack`] — assemble the recent +
//!   semantic + project + self-state retrieval substrate.
//! * [`context::envelope::build_context_envelope`] — wrap retrieved
//!   local memory into the cloud-LLM-facing ContextEnvelope contract.
//! * [`runtime::resident::Resident`] — POSIX-socket control plane
//!   the resident daemon listens on; `cli::start::run_blocking`
//!   wires the production process tree.
//! * [`memory::embed::select_embedder`] — process-wide embedder
//!   handle (Studio profile prefers e5-large 1024d; Mini prefers
//!   MiniLM-L12 384d; default falls through to a deterministic
//!   hash projection).
//! * [`storage::Storage`] — direct SQLite access for migrations,
//!   episode CRUD, audit pins.
//! * [`SomaError`] — root error taxonomy.
//!
//! ## Cargo features
//!
//! All features are off-by-default to keep the lean binary small:
//!
//! * `pty-capture` — `soma capture --pty` live terminal capture.
//! * `llm-summary` — legacy Anthropic-backed slow-loop narrative synthesis;
//!   not the local compiler bridge.
//! * `embed-onnx` — real ONNX embedder (downloads model on first run).
//! * `cognitive` — optional Hopfield / mLSTM / iPC / ANIL context
//!   quality modules.
//! * `cognitive-train` — trainable Q/K/V via candle for those optional
//!   modules (implies `cognitive`).

pub mod capture;
pub mod cli;
pub mod context;
pub mod error;
pub mod memory;
pub mod runtime;
pub mod self_model;
pub mod storage;

// D110-cand (R5 audit) + R11 audit (2026-05-01) — `config` and
// `profile` are **binary-internal** modules. They are kept `pub` so
// the `soma` binary (`src/main.rs`) and the integration test harness
// can import them, but they are NOT part of the stable public API
// surface that downstream library users should depend on. Field
// additions / removals on `Config`, `RuntimeConfig`, `MemoryConfig`,
// `Profile`, etc. are not considered breaking changes between v1.x
// releases. v1.1+ may downgrade these to `pub(crate)` once the test
// seam is refactored. External consumers should reach for the
// stable surface (`storage::Storage`, `memory::embed::*`,
// `context::pack::*`, `runtime::resident::*`) instead.
pub mod config;
pub mod profile;
// D147 — single source of truth for "what project is the user
// currently in?". Capture, ContextEnvelope scoping, and legacy
// context/profile migration helpers all flow through this resolver.
// Future tweaks (git-remote slug, normalization) should stay here.
pub mod project;
// D155 — cross-module utilities (mutex-poison recovery,
// future shared helpers).
pub mod util;

pub use error::SomaError;
