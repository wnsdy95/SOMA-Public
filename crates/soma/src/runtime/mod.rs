//! Resident runtime orchestrator — Phase 1 scaffold.
//!
//! Filled by subsequent modules:
//! * `scheduler` — Fast / Warm / Slow loop routing.
//! * `resident` — PID file + Unix socket server for CLI → resident.
//! * `mcp` — MCP resource server (`soma://context/...` primary,
//!   `soma://memory-pack/...` developer/debug direct-read only).

pub mod mcp;
pub mod mcp_cache;
pub mod resident;
pub mod scheduler;

// D152 chunk 1.1 (ADR 0012) — local web dashboard GUI for SOMA's
// transparency surface (mLSTM/Hopfield weights, live recall, memory
// state, architecture view). Off by default — see `Cargo.toml`'s
// `dashboard` feature.
#[cfg(feature = "dashboard")]
pub mod dashboard;
