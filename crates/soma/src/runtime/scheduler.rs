//! Resident-side schedulers. Discussion 0033 §I.
//!
//! v1.1 schedulers:
//!
//! * **warm_loop** — periodic `self_model::run_all` + cache
//!   invalidate (D80).
//! * **slow_loop** — Sleep Replay analog (D91). Hourly: similar-
//!   episode merge + low-decay cold-tier demotion + project_state
//!   EMA refresh.
//!
//! Phase 1's plan referenced Fast / Warm / Slow loops; v1 ran
//! with none. The warm + slow loops cover the second and third
//! tiers; the Fast loop is the synchronous ingest hot path itself.

pub mod slow_loop;
pub mod warm_loop;
