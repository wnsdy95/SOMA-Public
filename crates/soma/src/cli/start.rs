//! `soma start` — bootstrap the resident runtime and run forever.
//!
//! D0 §B — main.rs previously printed `"soma: start — Phase 1 TODO"`
//! and exited zero, which broke the LaunchAgent we install (it
//! `KeepAlive`'d a no-op binary and `soma stop` had nothing to talk
//! to). This module composes the pieces that already exist:
//!
//! * `runtime::resident::Resident::start` — Unix socket + PID file
//!   control plane (discussion 0025).
//! * `runtime::scheduler::warm_loop::run` — periodic
//!   `self_model::run_all` + cache invalidate (D80).
//! * `tokio::signal::ctrl_c` + a Unix `SIGTERM` handler — orderly
//!   shutdown trigger; the resident's broadcast channel fans out the
//!   stop signal to the warm loop.
//!
//! Everything runs on a single multi-threaded tokio runtime so the
//! LaunchAgent process model stays "one binary, one process tree".

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::profile;
use crate::runtime::mcp_cache::MemoryPackCache;
use crate::runtime::resident::{Resident, ResidentConfig, ResidentStats, StatsProvider};
use crate::runtime::scheduler::slow_loop::{self, SlowLoopConfig};
use crate::runtime::scheduler::warm_loop::{self, WarmLoopConfig};
use crate::storage::Storage;

/// D1 §C — production `StatsProvider` backed by the real `Storage`.
/// Replaces `ZeroStats` so `soma status` reports the live episode +
/// pending-job counters instead of always-zero placeholders.
///
/// D87 §B — also carries an `Arc<MemoryPackCache>` so the same
/// `StatsProvider::snapshot` returns cache hit/miss counters in a
/// single call. Lock-free atomic reads — no extra contention.
struct StorageStats {
    storage: Arc<Mutex<Storage>>,
    cache: Arc<MemoryPackCache>,
}

impl StatsProvider for StorageStats {
    fn snapshot(&self) -> ResidentStats {
        // D98-cand close (2026-04-29) — pre-fix any failure here was
        // silent-zeroed: `counters() failed → (0, 0)` looked identical
        // to a fresh DB. Now every fall-through pushes a string into
        // `degraded_reasons` so `soma status` can render a separate
        // line and the operator sees that the resident is degraded
        // rather than empty.
        let mut degraded_reasons: Vec<String> = Vec::new();
        let cache_hits = self.cache.hits() as u64;
        let cache_misses = self.cache.misses() as u64;
        match self.storage.lock() {
            Ok(g) => {
                let (episodes_total, pending_jobs) = match g.counters() {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(error = %e, "StorageStats::snapshot — counters() failed");
                        degraded_reasons.push(format!("counters() failed: {e}"));
                        (0, 0)
                    }
                };
                let (mlstm_dim, mlstm_train_steps, mlstm_saved_at_ns) = match g
                    .get_working_memory_weights()
                {
                    Ok(Some((dim, _, _, _, steps, ts))) => (Some(dim as u64), steps, ts),
                    Ok(None) => (None, 0, 0),
                    Err(e) => {
                        tracing::warn!(error = %e, "StorageStats::snapshot — mlstm weights read failed");
                        degraded_reasons.push(format!("mlstm weights read failed: {e}"));
                        (None, 0, 0)
                    }
                };
                ResidentStats {
                    episodes_total,
                    pending_jobs,
                    cache_hits,
                    cache_misses,
                    mlstm_dim,
                    mlstm_train_steps,
                    mlstm_saved_at_ns,
                    degraded_reasons,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "StorageStats::snapshot — storage mutex poisoned");
                degraded_reasons.push(format!("storage mutex poisoned: {e}"));
                ResidentStats {
                    cache_hits,
                    cache_misses,
                    degraded_reasons,
                    ..ResidentStats::default()
                }
            }
        }
    }
}

/// Failure modes surfaced to the caller. The CLI dispatcher maps
/// each leg to an exit code identical to other subcommands so
/// `launchctl print` shows a meaningful status.
#[derive(Debug)]
pub enum StartError {
    Path(String),
    Storage(String),
    Resident(String),
    Runtime(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Path(m) => write!(f, "path: {m}"),
            StartError::Storage(m) => write!(f, "storage: {m}"),
            StartError::Resident(m) => write!(f, "resident: {m}"),
            StartError::Runtime(m) => write!(f, "runtime: {m}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Resolve `~/.soma` — the run / log / DB root used by the resident.
/// Tests inject a tempdir via the second-arg constructors below.
pub fn resolve_home_root() -> Result<PathBuf, StartError> {
    let home = dirs::home_dir().ok_or_else(|| {
        StartError::Path("home directory not resolvable; set $HOME or run as a real user".into())
    })?;
    Ok(home.join(".soma"))
}

/// Run the resident until SIGINT / SIGTERM / a `Stop` control
/// message is received. Returns once the accept loop has joined.
///
/// **Blocking.** Builds its own multi-threaded tokio runtime so the
/// caller can be a synchronous `main` function.
pub fn run_blocking() -> Result<(), StartError> {
    let root = resolve_home_root()?;
    let run_dir = root.join("run");
    let db_path = root.join("soma.db");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| StartError::Runtime(format!("tokio build: {e}")))?;

    runtime.block_on(async move {
        let storage = Storage::open(&db_path)
            .map_err(|e| StartError::Storage(format!("open `{}`: {e}", db_path.display())))?;
        let storage = Arc::new(Mutex::new(storage));
        // D86 §B — TTL from config; default = 30 s (mcp_cache::DEFAULT_TTL).
        // D94-cand — load `~/.soma/config.toml` so the user can
        // override profile / TTL / future knobs without rebuilding.
        let cfg_disk = crate::config::Config::load_or_default(&root);
        let cache = Arc::new(MemoryPackCache::from_ttl_secs(cfg_disk.mcp.cache_ttl_secs));

        let cfg = ResidentConfig {
            run_dir,
            profile: format!("{:?}", profile::detect()),
            stats: Arc::new(StorageStats { storage: storage.clone(), cache: cache.clone() }),
            // D91 §B — single cache shared with `Cmd::McpServe`
            // children via the McpFetch forwarding path. cache.clone()
            // here is the SAME Arc the warm_loop invalidates.
            mcp_cache: Some(cache.clone()),
            mcp_db_path: Some(db_path.clone()),
            // D105-cand — graceful shutdown drain budget from config.
            shutdown_drain: std::time::Duration::from_secs(cfg_disk.runtime.shutdown_drain_secs),
        };
        let handle =
            Resident::start(cfg).await.map_err(|e| StartError::Resident(format!("start: {e}")))?;

        let (warm_shutdown_tx, warm_shutdown_rx) = watch::channel(false);
        let warm = tokio::spawn(warm_loop::run(
            storage.clone(),
            cache.clone(),
            warm_shutdown_rx,
            WarmLoopConfig::v1_default(),
        ));

        // D91 §B — slow loop runs alongside warm. Same shutdown
        // cascade so `soma stop` brings both down together.
        let (slow_shutdown_tx, slow_shutdown_rx) = watch::channel(false);
        // D156-C — slow_loop knobs (lambda / merge / cold_tier)
        // 가 [memory] 섹션 에서 옴. v1_default 는 historical
        // hard-coded path, runtime 진입은 config-aware 로 swap.
        let slow_cfg_runtime = match dirs::home_dir() {
            Some(home) => crate::config::Config::load_or_default(&home.join(".soma")),
            None => crate::config::Config::default_v1(),
        };
        let slow = tokio::spawn(slow_loop::run(
            storage.clone(),
            slow_shutdown_rx,
            SlowLoopConfig::from_config(&slow_cfg_runtime),
        ));

        // D1 §B — clone the resident's broadcast Sender so a
        // signal handler can ask the accept loop to drain without
        // consuming the handle. `joined()` then waits for the
        // *same* accept task to exit, regardless of whether the
        // shutdown was triggered by SIGINT/SIGTERM (signal_task
        // fires the broadcast) or by an out-of-band `Stop`
        // request from `soma stop` (the request handler fires it
        // internally). Either way, joined() returns once the
        // accept loop has finalized.
        let resident_shutdown = handle.shutdown_signal();
        let signal_task = tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            let _ = resident_shutdown.send(());
        });

        let join_result = handle.joined().await;
        // The signal task is no longer needed once the accept loop
        // has joined; abort it so an SIGTERM that arrived after
        // joined() returned doesn't outlive the runtime.
        signal_task.abort();

        // Cascade: warm + slow loop break, then await their joins so
        // `self_model::run_all` and slow_loop merges are no longer
        // running when Storage is dropped.
        let _ = warm_shutdown_tx.send(true);
        let _ = slow_shutdown_tx.send(true);
        let _ = warm.await;
        let _ = slow.await;

        join_result.map_err(|e| StartError::Resident(format!("shutdown: {e}")))?;
        Ok::<(), StartError>(())
    })
}

/// `tokio::signal::ctrl_c` covers SIGINT on every platform. SIGTERM
/// is what `launchctl bootout` and `kill` send by default — we
/// register a separate handler so the resident reacts to both.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler install failed; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Map `StartError` to a process exit code. `Path` = 3,
/// `Storage` = 2, `Resident` / `Runtime` = 4 (no other subcommand
/// has reused 4 yet, so it pinpoints "start-time bootstrap" in
/// LaunchAgent stderr logs).
pub fn exit_code_for(e: &StartError) -> i32 {
    match e {
        StartError::Path(_) => 3,
        StartError::Storage(_) => 2,
        StartError::Resident(_) | StartError::Runtime(_) => 4,
    }
}
