//! `soma backfill` — operator-triggered re-embed of all episodes
//! under the *current* primary embedder.
//!
//! D70 close (2026-04-30 dogfooding) — when the operator changes
//! the primary backend (e.g. installs `embed-onnx` after running
//! on `HashEmbedder`), the slow_loop's automatic backfill drains
//! at `BACKFILL_CAP = 64` episodes/cycle (≈ hourly). On a 1k-
//! episode DB that's ≈ 16 hours of recall blindness. This verb
//! pulls the same work onto the foreground process so a model
//! upgrade is visible immediately.
//!
//! Reuses `slow_loop::run_backfill_primary_model` so the indexing
//! contract (D90 §A — same `episode_index_text` join order) is
//! shared with the resident path. Loops until the helper returns 0
//! (no episodes left to write).

use std::sync::{Arc, Mutex};

use crate::storage::{Storage, StorageError};

/// Typed failure legs. Mirrors the structure used by other CLI
/// verbs so the dispatcher can map to a stable exit code.
#[derive(Debug)]
pub enum BackfillError {
    /// DB path resolution failure.
    Path(String),
    /// SQLite open or write failure.
    Storage(StorageError),
}

impl std::fmt::Display for BackfillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackfillError::Path(m) => write!(f, "path: {m}"),
            BackfillError::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

impl std::error::Error for BackfillError {}

impl From<StorageError> for BackfillError {
    fn from(e: StorageError) -> Self {
        BackfillError::Storage(e)
    }
}

/// Drive the backfill loop on the calling thread. Prints progress
/// to stdout (one line per cycle) so the operator sees it move.
pub fn run_blocking() -> Result<(), BackfillError> {
    let db_path = crate::capture::ai_cli::resolve_db_path(None)
        .map_err(|e| BackfillError::Path(e.to_string()))?;
    let storage = Arc::new(Mutex::new(Storage::open(&db_path)?));

    let primary = crate::memory::embed::select_embedder();
    println!("soma: backfill — re-embedding episodes under primary backend");
    println!("  model_id: {}", primary.model_id());
    println!("  dim:      {}", primary.dim());
    println!("  db:       {}", db_path.display());

    // Loop until the slow_loop helper returns 0. Each call drains
    // `BACKFILL_CAP = 64` episodes; the cap intentionally lives in
    // slow_loop.rs so the resident's idle behavior stays bounded.
    let mut total = 0_usize;
    let mut cycle = 0_usize;
    loop {
        let n = crate::runtime::scheduler::slow_loop::run_backfill_primary_model(&storage);
        if n == 0 {
            break;
        }
        cycle += 1;
        total += n;
        println!("  cycle {cycle:>3}: +{n} episodes (cumulative: {total})");
    }

    if total == 0 {
        println!("soma: backfill — every episode already has a primary-model vector (no-op)");
    } else {
        println!("soma: backfill complete — {total} episodes re-embedded over {cycle} cycle(s)");
    }
    Ok(())
}

/// Map `BackfillError` to the §D exit-code taxonomy (re-using the
/// same numbers other verbs picked).
pub fn exit_code_for(err: &BackfillError) -> i32 {
    match err {
        BackfillError::Path(_) => 3,
        BackfillError::Storage(_) => 2,
    }
}
