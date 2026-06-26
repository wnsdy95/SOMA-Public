//! D152 chunk 1.1 — SOMA dashboard GUI bootstrap (ADR 0012).
//!
//! Shape:
//!
//! * `config.rs` — `DashboardConfig` (port / bind / open) parsed
//!   from CLI args by `cli::serve::ServeArgs`.
//! * `server.rs` — axum 0.7 router + shutdown wiring for the Quality,
//!   Recall, Memory, and Architecture dashboard views.
//!
//! All number / weight / cosine surfaces read SOMA's actual disk +
//! in-process state. Mock 0, placebo 0 (ADR 0012 §A4).

pub mod config;
pub mod memory_state;
pub mod operations;
pub mod recall;
pub mod server;
pub mod training;

pub use config::DashboardConfig;
pub use server::serve;
