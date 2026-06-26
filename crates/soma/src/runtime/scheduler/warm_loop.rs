//! Warm loop — periodic `self_model::run_all` + MCP cache
//! invalidate. Discussion 0033.
//!
//! Cycle pseudocode:
//!
//! ```text
//! sleep(delay_first)
//! loop {
//!     select {
//!         interval.tick() => {
//!             count = storage.episode_count()
//!             if count == last_count { skip }
//!             else {
//!                 run_all(storage)
//!                 cache.invalidate_all()
//!                 last_count = count
//!             }
//!         }
//!         shutdown_rx.changed() => break
//!     }
//! }
//! ```
//!
//! The episode-delta gate (`count == last_count`) avoids burning
//! CPU on idle workspaces (§A1). Test harness uses
//! `tokio::time::pause` + `advance` for deterministic timing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use crate::runtime::mcp_cache::MemoryPackCache;
use crate::self_model;
use crate::storage::Storage;

/// Tunable knobs for the warm loop. v1.1 hard-codes
/// `WarmLoopConfig::v1_default()`; config-knob exposure is
/// D89-cand v1.2.
#[derive(Debug, Clone, Copy)]
pub struct WarmLoopConfig {
    pub interval: Duration,
    pub delay_first: Duration,
}

impl WarmLoopConfig {
    /// 60 s interval + 30 s first-fire delay (discussion 0033 §B + §E).
    pub const fn v1_default() -> Self {
        Self { interval: Duration::from_secs(60), delay_first: Duration::from_secs(30) }
    }
}

impl Default for WarmLoopConfig {
    fn default() -> Self {
        Self::v1_default()
    }
}

/// Run the warm loop until `shutdown_rx` flips to `true`. Returns
/// the number of cycles that actually ran `run_all` (skipped cycles
/// — zero-delta or error — don't count).
///
/// **Shutdown-signal invariant** (D127 lock). `tokio::sync::watch`
/// is *lossy* — a sender that flips `false → true → false` between
/// two poll moments can hide both edges from a slow subscriber, but
/// `borrow()` always observes the *current* value. The exit
/// condition `changed.is_err() || *shutdown_rx.borrow()` is the
/// correct shape because `is_err()` catches a sender drop (resident
/// shutting down without sending an explicit `true`) while
/// `*borrow()` catches the current state regardless of how many
/// transitions we missed (a `true` borrow now still means "stop"
/// because we never unset shutdown after setting it). Future
/// refactors must preserve both clauses or the warm loop will
/// either hang on sender-drop or miss late-arriving shutdowns.
pub async fn run(
    storage: Arc<Mutex<Storage>>,
    cache: Arc<MemoryPackCache>,
    mut shutdown_rx: watch::Receiver<bool>,
    cfg: WarmLoopConfig,
) -> usize {
    // First-fire delay — race-avoidance with initial MCP fetches.
    // D1 §B — race the sleep against the shutdown signal so a
    // `soma stop` issued during the first 30 s does not have to
    // wait for the sleep to finish before the warm loop exits.
    tokio::select! {
        _ = tokio::time::sleep(cfg.delay_first) => {}
        changed = shutdown_rx.changed() => {
            if changed.is_err() || *shutdown_rx.borrow() {
                return 0;
            }
        }
    }

    let mut interval = tokio::time::interval(cfg.interval);
    // Skip the immediate-fire that `tokio::time::interval` does on
    // first poll; we already paid `delay_first`.
    interval.tick().await;

    let mut last_count: Option<u64> = None;
    let mut ran = 0usize;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if try_run_cycle(&storage, &cache, &mut last_count) {
                    ran += 1;
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return ran;
                }
            }
        }
    }
}

/// Synchronous body of one warm-loop tick. Returns `true` iff
/// `run_all` actually ran. Episode-delta gate + log-and-skip
/// error policy here.
fn try_run_cycle(
    storage: &Arc<Mutex<Storage>>,
    cache: &Arc<MemoryPackCache>,
    last_count: &mut Option<u64>,
) -> bool {
    let current_count = match crate::util::mutex::lock_or_recover(storage).counters() {
        Ok((episodes, _jobs)) => episodes,
        Err(e) => {
            tracing::warn!(error = %e, "warm-loop: counters() failed; skipping cycle");
            return false;
        }
    };

    if Some(current_count) == *last_count {
        // §A1 episode-delta gate — idle workspace zero CPU.
        return false;
    }

    // Round 2 in-house ultrareview fix: invalidate the cache BEFORE
    // running run_all so any reader that arrives during the run picks
    // up a fresh build keyed off the new self_state. Pre-fix, invalidate
    // came after run_all completed: a Claude Code session polling MCP
    // every 10s could land a request between the run_all start and the
    // post-run invalidate, get a stale cache hit (with old self_state),
    // and that stale entry would survive the next 30s TTL window.
    cache.invalidate_all();
    let result = {
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        self_model::run_all(&mut guard)
    };

    match result {
        Ok(n_facts) => {
            tracing::info!(
                facts = n_facts,
                episodes = current_count,
                "warm-loop: self_model::run_all"
            );
            *last_count = Some(current_count);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "warm-loop: run_all failed; skipping cycle");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Episode;

    fn term_episode(ts: i64, cmd: &str) -> Episode {
        use crate::storage::EpisodeSource;
        Episode {
            ts_start_ns: ts,
            ts_end_ns: ts,
            duration_ms: 0,
            source: EpisodeSource::Terminal,
            session_id: None,
            prompt_text: None,
            response_text: None,
            command: Some(cmd.into()),
            stdout: None,
            exit_code: Some(0),
            cwd: None,
            git_branch: Some("main".into()),
            project: Some("p".into()),
            digest: None,
        }
    }

    fn seed(storage: &Arc<Mutex<Storage>>, n: usize) {
        let mut s = storage.lock().unwrap();
        for i in 0..n {
            let ep =
                term_episode(1_700_000_000_000_000_000 + i as i64 * 1_000_000_000, "cargo test");
            s.append_episode(&ep).unwrap();
        }
    }

    fn fresh(
    ) -> (Arc<Mutex<Storage>>, Arc<MemoryPackCache>, watch::Sender<bool>, watch::Receiver<bool>)
    {
        let storage = Arc::new(Mutex::new(Storage::open_in_memory().unwrap()));
        let cache = Arc::new(MemoryPackCache::with_default_ttl());
        let (tx, rx) = watch::channel(false);
        (storage, cache, tx, rx)
    }

    fn snap_has_rows(storage: &Arc<Mutex<Storage>>) -> bool {
        let s = storage.lock().unwrap();
        let snap = self_model::read_snapshot(&s).unwrap();
        !snap.is_empty()
    }

    #[tokio::test(start_paused = true)]
    async fn test_warm_loop_runs_after_delay_first() {
        let (storage, cache, tx, rx) = fresh();
        seed(&storage, 3);

        let cfg = WarmLoopConfig {
            interval: Duration::from_secs(60),
            delay_first: Duration::from_secs(30),
        };
        let task = tokio::spawn(run(storage.clone(), cache.clone(), rx, cfg));

        // Push past delay_first + 1 interval tick.
        tokio::time::sleep(Duration::from_secs(91)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let _ = tx.send(true);
        let ran = task.await.unwrap();
        assert!(ran >= 1, "at least one cycle must have run");
        assert!(snap_has_rows(&storage), "self_state must be populated");
    }

    #[tokio::test(start_paused = true)]
    async fn test_warm_loop_skips_on_zero_episode_delta() {
        let (storage, cache, tx, rx) = fresh();
        seed(&storage, 2);

        let cfg = WarmLoopConfig {
            interval: Duration::from_secs(60),
            delay_first: Duration::from_secs(30),
        };
        let task = tokio::spawn(run(storage.clone(), cache.clone(), rx, cfg));

        // First tick @ 91 s → run.
        tokio::time::sleep(Duration::from_secs(91)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        // Second tick @ 60 s later, no new episodes → skip.
        tokio::time::sleep(Duration::from_secs(61)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let _ = tx.send(true);
        let ran = task.await.unwrap();
        assert_eq!(ran, 1, "only first cycle runs; second is skipped on zero-delta");
    }

    #[tokio::test(start_paused = true)]
    async fn test_warm_loop_invalidates_cache_after_run() {
        use crate::runtime::mcp_cache::{CacheKey, CacheStatus};
        let (storage, cache, tx, rx) = fresh();
        seed(&storage, 1);

        // Pre-populate cache so we can observe invalidation.
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };
        cache
            .get_or_build(key.clone(), || {
                Ok(crate::context::pack::MemoryPack {
                    version: crate::context::pack::MEMORY_PACK_VERSION,
                    assembled_at_ns: 0,
                    query: None,
                    recent: vec![],
                    semantic: vec![],
                    thread_state_selection: None,
                    project_state: serde_json::json!({}),
                    self_state: serde_json::json!({}),
                })
            })
            .unwrap();
        assert_eq!(cache.builder_calls(), 1);

        let task = tokio::spawn(run(
            storage.clone(),
            cache.clone(),
            rx,
            WarmLoopConfig {
                interval: Duration::from_secs(60),
                delay_first: Duration::from_secs(30),
            },
        ));
        tokio::time::sleep(Duration::from_secs(91)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let _ = tx.send(true);
        task.await.unwrap();

        // Same key now → miss (cache was invalidated).
        let (_, status) = cache
            .get_or_build(key, || {
                Ok(crate::context::pack::MemoryPack {
                    version: crate::context::pack::MEMORY_PACK_VERSION,
                    assembled_at_ns: 0,
                    query: None,
                    recent: vec![],
                    semantic: vec![],
                    thread_state_selection: None,
                    project_state: serde_json::json!({}),
                    self_state: serde_json::json!({}),
                })
            })
            .unwrap();
        assert!(matches!(status, CacheStatus::Miss));
        assert_eq!(cache.builder_calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_warm_loop_shutdown_signal_terminates_task() {
        let (storage, cache, tx, rx) = fresh();
        let task = tokio::spawn(run(storage, cache, rx, WarmLoopConfig::v1_default()));
        // Advance a bit so the loop is past the delay_first sleep.
        tokio::time::sleep(Duration::from_secs(31)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let _ = tx.send(true);

        let ran = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("task must finish within 2 sec of simulated time")
            .unwrap();
        // No episodes seeded → ran is 0 or 1 depending on if the
        // first tick fired before shutdown — both are valid.
        assert!(ran <= 1);
    }
}
