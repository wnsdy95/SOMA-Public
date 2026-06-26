//! Resident runtime — PID file + Unix socket + NDJSON control
//! protocol. Discussion 0025 locks the 8 axes shaping this module.
//!
//! Lifecycle:
//!
//! 1. `Resident::start(ResidentConfig)` creates the run dir
//!    (mode 0700), writes a PID file (mode 0600, atomic),
//!    binds the Unix socket (mode 0600), spawns the accept
//!    loop as a tokio task, and returns a `ResidentHandle`.
//! 2. Each accepted connection runs one request/response turn
//!    (§G per-request single-shot), then closes. The supported
//!    control messages are `Hello`, `Status`, `Stop` + an
//!    `Error` response envelope.
//! 3. Graceful shutdown (SIGTERM, SIGINT, or explicit `Stop`
//!    request): broadcast stop signal → accept loop exits →
//!    remove PID file (best effort) → join all in-flight handlers
//!    with a 10 s drain budget → exit.
//!
//! POSIX-only; Windows named-pipe parity is tracked as D56-cand
//! for v2.

#![cfg(unix)]

use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tokio::task::{JoinHandle, JoinSet};

/// Control-plane protocol version. Equality-match only — a mismatch
/// is surfaced as `ControlError::VersionMismatch`. Increment on any
/// wire shape change, even additive.
///
/// Bumped to 2 for the D91 single-cache fix; `ControlRequest::
/// McpFetch` and `ControlResponse::McpFetchOk` were added. Old
/// clients (PROTOCOL=1) now fail Hello with `VersionMismatch`. The
/// `Cmd::McpServe` child is built from the same source so it bumps
/// in lockstep.
pub const PROTOCOL_VERSION: u32 = 2;

/// Drain budget for graceful shutdown (§E). Default 10 s; D105-cand
/// closing made it config-driven through `ResidentConfig::shutdown_
/// drain`. Tests use this as the fallback when building `ResidentConfig`
/// directly without going through `RuntimeConfig`.
pub const SHUTDOWN_DRAIN_DEFAULT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Config + handle
// ---------------------------------------------------------------------------

/// Startup parameters. `run_dir` is typically `~/.soma/run`; tests
/// inject a tempdir. `profile` is the detected hardware profile
/// (`Mini` / `Studio`) — surfaced in Hello + Status responses.
#[derive(Clone)]
pub struct ResidentConfig {
    pub run_dir: PathBuf,
    pub profile: String,
    /// Source of truth for `Status` response counters. Tests pass
    /// `Arc::new(ZeroStats)` for deterministic zeros; production
    /// passes a storage-backed impl. (codex F5 — previous hard-
    /// coded zeros were a drift from discussion 0025 §F.)
    pub stats: Arc<dyn StatsProvider>,
    /// D91 §B — the shared MemoryPack cache that `McpFetch` requests
    /// route through. `None` disables forwarding (tests that don't
    /// exercise MCP, or production scenarios where the resident is
    /// MCP-disabled). The same `Arc` is also wired into `stats` so
    /// `soma status` reports cumulative fetch counters.
    pub mcp_cache: Option<Arc<crate::runtime::mcp_cache::MemoryPackCache>>,
    /// D91 §B — DB path the `McpFetch` handler opens. `None` when
    /// `mcp_cache` is `None`.
    pub mcp_db_path: Option<PathBuf>,
    /// D105-cand — graceful shutdown drain budget. The accept loop
    /// stops accepting then waits up to `shutdown_drain` for in-
    /// flight handlers before forcing exit. Tests use the default
    /// 10 s; production wires `RuntimeConfig::shutdown_drain_secs`.
    pub shutdown_drain: Duration,
}

impl std::fmt::Debug for ResidentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentConfig")
            .field("run_dir", &self.run_dir)
            .field("profile", &self.profile)
            .field("stats", &"<dyn StatsProvider>")
            .field("mcp_cache", &self.mcp_cache.as_ref().map(|_| "<MemoryPackCache>"))
            .field("mcp_db_path", &self.mcp_db_path)
            .field("shutdown_drain", &self.shutdown_drain)
            .finish()
    }
}

/// Snapshot for the `Status` control response. Populated by a
/// `StatsProvider` implementation on each `Status` request.
#[derive(Debug, Clone, Default)]
pub struct ResidentStats {
    pub episodes_total: u64,
    pub pending_jobs: u64,
    /// D87 §B — cumulative MemoryPack cache hits since boot.
    pub cache_hits: u64,
    /// D87 §B — cumulative MemoryPack cache misses since boot.
    pub cache_misses: u64,
    /// N3 — TrainableMLstm dimension when persisted (`Some` iff
    /// the slow_loop train cycle has run at least once and saved
    /// non-NaN weights). `None` for fresh DB / cognitive-train
    /// feature off / divergent training that never persisted.
    pub mlstm_dim: Option<u64>,
    /// Cumulative SGD steps since the singleton row was first
    /// written. `0` when `mlstm_dim` is `None`.
    pub mlstm_train_steps: u64,
    /// `working_memory_weights.saved_at_ns` of the last persist.
    /// `0` when no row.
    pub mlstm_saved_at_ns: i64,
    /// D98-cand — if the snapshot encountered any non-fatal failure
    /// (DB lock contention, counters() error, mLSTM read error,
    /// mutex poisoned), each is appended here. `soma status` surfaces
    /// the list as a `degraded:` block so an operator sees the
    /// difference between "0 episodes_total because fresh DB" vs
    /// "0 because counters() failed and was silently zeroed".
    pub degraded_reasons: Vec<String>,
}

/// Read-only adapter from the storage layer into the resident.
/// Callers wire an `Arc<dyn StatsProvider>` into `ResidentConfig`;
/// production passes a `Storage`-backed impl, tests may pass
/// `ZeroStats` for determinism.
pub trait StatsProvider: Send + Sync {
    fn snapshot(&self) -> ResidentStats;
}

/// Trivial impl — always reports zeros. Matches the pre-wiring
/// behaviour of PR 6.2 and keeps the existing 5-test matrix valid.
pub struct ZeroStats;
impl StatsProvider for ZeroStats {
    fn snapshot(&self) -> ResidentStats {
        ResidentStats::default()
    }
}

/// Owner of the accept loop + signal shutdown channel. Callers use
/// this to request an orderly stop or to wait for the loop to exit.
pub struct ResidentHandle {
    shutdown_tx: broadcast::Sender<()>,
    accept_task: Option<JoinHandle<()>>,
    run_dir: PathBuf,
}

impl ResidentHandle {
    /// Request graceful shutdown and wait for the accept loop to
    /// finish. Consumes the handle.
    pub async fn shutdown(mut self) -> io::Result<()> {
        let _ = self.shutdown_tx.send(());
        self.join_in_place().await
    }

    /// Clone of the broadcast `Sender` used to ask the accept loop
    /// to drain. Exposed so an external signal handler (e.g.
    /// `cli::start::wait_for_shutdown_signal`) can fire shutdown
    /// without consuming the handle — `joined()` then waits for
    /// the same accept task to exit. Discussion 0035 §B / D1
    /// §B (P0 stop semantics fix).
    pub fn shutdown_signal(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Wait for the accept loop to exit without triggering shutdown
    /// ourselves. Used by tests after a client-side `Stop` turned
    /// the resident off remotely. Consumes the handle.
    pub async fn joined(mut self) -> io::Result<()> {
        self.join_in_place().await
    }

    async fn join_in_place(&mut self) -> io::Result<()> {
        if let Some(t) = self.accept_task.take() {
            t.await.map_err(|e| io::Error::other(format!("accept task join: {e}")))?;
        }
        // Best-effort PID file cleanup — accept loop also tries on
        // its own exit. Both paths tolerate the file already being
        // gone.
        let _ = std::fs::remove_file(self.run_dir.join("soma.pid"));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NDJSON wire shapes
// ---------------------------------------------------------------------------

/// Client → server request envelope. `Hello` is required as the
/// first message on every connection (§F handshake).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlRequest {
    Hello {
        protocol_version: u32,
    },
    Status,
    Stop,
    /// D91 §B — `soma mcp-serve` child forwards each MCP method
    /// (initialize / resources/list / resources/read) here so the
    /// resident's single `MemoryPackCache` records the hit/miss.
    /// `params` is the JSON-RPC `params` object, passed through
    /// untouched.
    McpFetch {
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
}

/// Server → client response envelope. One response per request;
/// the connection closes after.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlResponse {
    HelloOk {
        protocol_version: u32,
        pid: u32,
        profile: String,
    },
    StatusOk {
        pid: u32,
        profile: String,
        uptime_ms: u64,
        episodes_total: u64,
        pending_jobs: u64,
        /// D87 §B — additive fields. Defaults to 0 when an older
        /// resident sends a response without them. Bumping the
        /// `PROTOCOL_VERSION` is reserved for shape-breaking
        /// changes; additive fields with defaults are version-stable.
        #[serde(default)]
        cache_hits: u64,
        #[serde(default)]
        cache_misses: u64,
        /// N3 — additive trio for optional mLSTM quality-diagnostic visibility.
        /// `None` (`Option<u64>`) → cognitive-train feature off
        /// or no weights persisted yet. Older clients with the
        /// `#[serde(default)]` get `None` (no breaking change).
        #[serde(default)]
        mlstm_dim: Option<u64>,
        #[serde(default)]
        mlstm_train_steps: u64,
        #[serde(default)]
        mlstm_saved_at_ns: i64,
        /// D98-cand — non-fatal snapshot failures (DB lock contention,
        /// counters() error, mlstm weights read error, storage mutex
        /// poisoned). Empty Vec on a healthy resident; `soma status`
        /// surfaces each as a `degraded:` line.
        #[serde(default)]
        degraded_reasons: Vec<String>,
    },
    StopAck,
    /// D91 §B — successful `McpFetch` response. `result` is the
    /// raw JSON the JSON-RPC client expected (no envelope wrapping
    /// — the standalone fallback path also returns raw `result`
    /// values via the same `dispatch` fn).
    McpFetchOk {
        result: Value,
    },
    Error {
        code: ControlError,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        got: Option<u32>,
    },
}

/// Structured error codes on the `Error` response. Downstream
/// callers match on this rather than parsing the free-form
/// `message`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ControlError {
    VersionMismatch,
    MalformedRequest,
    UnknownRequest,
    InternalError,
    HelloRequired,
    /// D91 §B — `McpFetch` failures: method-not-found, invalid params,
    /// or the resident isn't configured with an `mcp_cache` (e.g.
    /// test harness with `mcp_cache: None`).
    McpDispatch,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Static entry point — holds no state between runs.
pub struct Resident;

impl Resident {
    /// Start the resident. Returns a handle the caller uses to
    /// request shutdown. Errors if the run dir cannot be created,
    /// a live resident is already running (PID file + `kill(pid,0)`
    /// succeeds), or the socket bind fails.
    pub async fn start(cfg: ResidentConfig) -> io::Result<ResidentHandle> {
        create_run_dir(&cfg.run_dir)?;

        let pid_path = cfg.run_dir.join("soma.pid");
        refuse_if_already_running(&pid_path)?;
        write_pid_file(&pid_path, std::process::id())?;

        let sock_path = cfg.run_dir.join("soma.sock");
        // Remove a stale socket left by a crashed previous resident.
        // The PID-file check above already ruled out a live one.
        let _ = std::fs::remove_file(&sock_path);

        let listener = match UnixListener::bind(&sock_path) {
            Ok(listener) => listener,
            Err(e) => {
                let _ = std::fs::remove_file(&pid_path);
                return Err(e);
            }
        };
        match std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&pid_path);
                let _ = std::fs::remove_file(&sock_path);
                return Err(e);
            }
        }

        // Round 3 audit (2026-04-29) — shutdown broadcast capacity
        // raised 4 → 64 so a many-connection resident (Claude Code +
        // Cursor + Continue + multiple `soma status` polls all
        // simultaneously connected) doesn't see a `RecvError::Lagged`
        // on the slowest receiver during graceful shutdown. The
        // channel only carries the empty `()` so the buffer cost is
        // negligible.
        let (shutdown_tx, _) = broadcast::channel::<()>(64);
        let started_at = Instant::now();
        let shared = Arc::new(SharedState {
            pid: std::process::id(),
            profile: cfg.profile.clone(),
            started_at,
            shutdown_tx: shutdown_tx.clone(),
            stats: cfg.stats.clone(),
            mcp_cache: cfg.mcp_cache.clone(),
            mcp_db_path: cfg.mcp_db_path.clone(),
            shutdown_drain: cfg.shutdown_drain,
        });

        let accept_task = tokio::spawn(accept_loop(
            listener,
            shared,
            shutdown_tx.subscribe(),
            cfg.run_dir.clone(),
        ));

        Ok(ResidentHandle { shutdown_tx, accept_task: Some(accept_task), run_dir: cfg.run_dir })
    }
}

struct SharedState {
    pid: u32,
    profile: String,
    started_at: Instant,
    shutdown_tx: broadcast::Sender<()>,
    stats: Arc<dyn StatsProvider>,
    mcp_cache: Option<Arc<crate::runtime::mcp_cache::MemoryPackCache>>,
    mcp_db_path: Option<PathBuf>,
    shutdown_drain: Duration,
}

async fn accept_loop(
    listener: UnixListener,
    shared: Arc<SharedState>,
    mut shutdown_rx: broadcast::Receiver<()>,
    run_dir: PathBuf,
) {
    // JoinSet auto-removes completed handles so idle connections do
    // not leak (codex F10). We wrap the listener in an Option so the
    // shutdown path can drop it + unlink the socket BEFORE draining
    // in-flight handlers, closing the accept window (codex F1).
    let mut listener = Some(listener);
    let mut in_flight: JoinSet<()> = JoinSet::new();

    loop {
        let accept_fut = async {
            match listener.as_ref() {
                Some(l) => l.accept().await.map(Some),
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            accepted = accept_fut => {
                match accepted {
                    Ok(Some((stream, _addr))) => {
                        let s = shared.clone();
                        in_flight.spawn(handle_connection(stream, s));
                    }
                    Ok(None) => {
                        // Round 3 audit (2026-04-29) — formerly
                        // `unreachable!()`. The accept_fut combinator
                        // is structured so this branch can't fire,
                        // but production code shouldn't panic on a
                        // can't-happen. Treat as an exit condition
                        // identical to a None listener.
                        tracing::warn!("accept_fut yielded Ok(None); exiting accept loop");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed; exiting accept loop");
                        break;
                    }
                }
            }
            // Reap completed handlers so in_flight grows with active
            // load, not cumulative traffic.
            Some(_) = in_flight.join_next(), if !in_flight.is_empty() => {}
            _ = shutdown_rx.recv() => {
                tracing::info!("shutdown signaled; draining");
                break;
            }
        }
    }

    // Close the accept face BEFORE draining so new connections can't
    // sneak in during the drain window (codex F1). Drop the listener
    // and unlink the socket file immediately.
    drop(listener.take());
    let _ = std::fs::remove_file(run_dir.join("soma.sock"));

    // Drain in-flight handlers with the configured budget (D105-cand
    // — was hard-coded SHUTDOWN_DRAIN = 10s). Abort on timeout rather
    // than detaching (codex F1).
    let drain_deadline = Instant::now() + shared.shutdown_drain;
    loop {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            in_flight.shutdown().await;
            break;
        }
        tokio::select! {
            next = in_flight.join_next() => {
                if next.is_none() {
                    break;
                }
            }
            _ = tokio::time::sleep(remaining) => {
                in_flight.shutdown().await;
                break;
            }
        }
    }

    // PID file cleanup — ResidentHandle::join_in_place also attempts.
    let _ = std::fs::remove_file(run_dir.join("soma.pid"));
}

async fn handle_connection(stream: UnixStream, shared: Arc<SharedState>) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // §G per-request single-shot — one Hello, then one request,
    // then we close. The first read line MUST be Hello.
    let hello_line = match read_line(&mut reader).await {
        Ok(Some(l)) => l,
        _ => return,
    };

    let hello_req = match serde_json::from_str::<ControlRequest>(&hello_line) {
        Ok(r) => r,
        Err(e) => {
            let _ = write_response(
                &mut writer,
                &ControlResponse::Error {
                    code: ControlError::MalformedRequest,
                    message: format!("JSON parse on Hello: {e}"),
                    expected: None,
                    got: None,
                },
            )
            .await;
            return;
        }
    };

    let client_version = match hello_req {
        ControlRequest::Hello { protocol_version } => protocol_version,
        _ => {
            let _ = write_response(
                &mut writer,
                &ControlResponse::Error {
                    code: ControlError::HelloRequired,
                    message: "first message must be Hello".into(),
                    expected: None,
                    got: None,
                },
            )
            .await;
            return;
        }
    };

    if client_version != PROTOCOL_VERSION {
        let _ = write_response(
            &mut writer,
            &ControlResponse::Error {
                code: ControlError::VersionMismatch,
                message: format!(
                    "expected protocol_version={PROTOCOL_VERSION}, got {client_version}"
                ),
                expected: Some(PROTOCOL_VERSION),
                got: Some(client_version),
            },
        )
        .await;
        return;
    }

    // Hello accepted → respond with HelloOk.
    let hello_resp = ControlResponse::HelloOk {
        protocol_version: PROTOCOL_VERSION,
        pid: shared.pid,
        profile: shared.profile.clone(),
    };
    if write_response(&mut writer, &hello_resp).await.is_err() {
        return;
    }

    // Expect exactly one subsequent request; silent EOF ends the
    // connection (the client completed a `hello`-only probe).
    let second_line = match read_line(&mut reader).await {
        Ok(Some(l)) => l,
        _ => return,
    };

    let req = match serde_json::from_str::<ControlRequest>(&second_line) {
        Ok(r) => r,
        Err(e) => {
            let _ = write_response(
                &mut writer,
                &ControlResponse::Error {
                    code: ControlError::MalformedRequest,
                    message: format!("JSON parse on request: {e}"),
                    expected: None,
                    got: None,
                },
            )
            .await;
            return;
        }
    };

    let resp = match req {
        ControlRequest::Hello { .. } => ControlResponse::Error {
            code: ControlError::UnknownRequest,
            message: "Hello already completed on this connection".into(),
            expected: None,
            got: None,
        },
        ControlRequest::Status => {
            let uptime_ms = shared.started_at.elapsed().as_millis() as u64;
            // codex F5 — query the injected StatsProvider so
            // `episodes_total` / `pending_jobs` reflect real storage,
            // not hard-coded zeros.
            let snap = shared.stats.snapshot();
            ControlResponse::StatusOk {
                pid: shared.pid,
                profile: shared.profile.clone(),
                uptime_ms,
                episodes_total: snap.episodes_total,
                pending_jobs: snap.pending_jobs,
                cache_hits: snap.cache_hits,
                cache_misses: snap.cache_misses,
                mlstm_dim: snap.mlstm_dim,
                mlstm_train_steps: snap.mlstm_train_steps,
                mlstm_saved_at_ns: snap.mlstm_saved_at_ns,
                degraded_reasons: snap.degraded_reasons,
            }
        }
        ControlRequest::Stop => {
            let _ = shared.shutdown_tx.send(());
            ControlResponse::StopAck
        }
        ControlRequest::McpFetch { method, params } => {
            // D91 §B — route through the resident's single cache.
            // No mcp_cache injected (e.g. ZeroStats test harness)
            // → return McpDispatch error so the child can fall
            // back to standalone, instead of the request silently
            // succeeding against an empty cache that's never read.
            match (shared.mcp_cache.as_ref(), shared.mcp_db_path.as_ref()) {
                (Some(cache), Some(db)) => {
                    use crate::runtime::mcp::{dispatch, DispatchOutcome};
                    match dispatch(&method, params.as_ref(), db, cache) {
                        DispatchOutcome::Ok(v) => ControlResponse::McpFetchOk { result: v },
                        DispatchOutcome::InvalidParams(msg) => ControlResponse::Error {
                            code: ControlError::McpDispatch,
                            message: format!("invalid params: {msg}"),
                            expected: None,
                            got: None,
                        },
                        DispatchOutcome::MethodNotFound => ControlResponse::Error {
                            code: ControlError::McpDispatch,
                            message: format!("method not found: {method}"),
                            expected: None,
                            got: None,
                        },
                    }
                }
                _ => ControlResponse::Error {
                    code: ControlError::McpDispatch,
                    message: "resident has no mcp_cache configured".into(),
                    expected: None,
                    got: None,
                },
            }
        }
    };

    let _ = write_response(&mut writer, &resp).await;
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Single-shot client for the resident control plane. One client
/// instance corresponds to one accepted connection on the server
/// side (§G). Both halves live behind the same `Mutex` so
/// (write, read) pairs are atomic at the logical-request boundary.
pub struct ResidentClient {
    io: Mutex<ClientIo>,
}

struct ClientIo {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl ResidentClient {
    /// Open a connection to the resident at `socket`. Does not yet
    /// perform the Hello handshake — call `hello()` next.
    pub async fn connect(socket: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(socket).await?;
        let (r, w) = stream.into_split();
        Ok(Self { io: Mutex::new(ClientIo { reader: BufReader::new(r), writer: w }) })
    }

    /// Send a `Hello { protocol_version }` and return the server's
    /// response. Surfaces `Error { code: VersionMismatch, .. }`
    /// without interpretation — callers decide how to react.
    pub async fn hello(&self, version: u32) -> io::Result<ControlResponse> {
        self.request_raw(&ControlRequest::Hello { protocol_version: version }).await
    }

    /// Send a non-Hello request. Caller must have completed Hello
    /// on this connection, per §G.
    pub async fn request(&self, req: &ControlRequest) -> io::Result<ControlResponse> {
        self.request_raw(req).await
    }

    async fn request_raw(&self, req: &ControlRequest) -> io::Result<ControlResponse> {
        let mut line =
            serde_json::to_string(req).map_err(|e| io::Error::other(format!("serialize: {e}")))?;
        line.push('\n');

        let mut io = self.io.lock().await;
        io.writer.write_all(line.as_bytes()).await?;
        io.writer.flush().await?;

        let mut buf = String::new();
        let n = io.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "resident closed connection before responding",
            ));
        }
        let resp: ControlResponse = serde_json::from_str(buf.trim_end_matches('\n'))
            .map_err(|e| io::Error::other(format!("response parse: {e}")))?;
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// IO helpers
// ---------------------------------------------------------------------------

async fn read_line(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> io::Result<Option<String>> {
    let mut buf = String::new();
    let n = reader.read_line(&mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(buf.trim_end_matches('\n').to_string()))
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &ControlResponse,
) -> io::Result<()> {
    let mut line = serde_json::to_string(resp)
        .map_err(|e| io::Error::other(format!("serialize response: {e}")))?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Filesystem setup
// ---------------------------------------------------------------------------

fn create_run_dir(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    // `create` with recursive skips permission for existing ancestors.
    // Re-chmod the leaf to guarantee 0700 even if it pre-existed.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn refuse_if_already_running(pid_path: &Path) -> io::Result<()> {
    let contents = match std::fs::read_to_string(pid_path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let trimmed = contents.trim();
    let pid: u32 = match trimmed.parse() {
        Ok(0) => {
            // `kill(0, 0)` targets our own process group, not a
            // single PID — interpreting a zero PID as "alive" would
            // falsely refuse every start. Treat 0 as corrupt
            // (codex F9).
            tracing::warn!(path = %pid_path.display(), "PID file holds 0; treating as corrupt");
            return Ok(());
        }
        Ok(n) => n,
        Err(_) => {
            tracing::warn!(path = %pid_path.display(), "corrupt PID file; overwriting");
            return Ok(());
        }
    };
    // `kill(pid, 0)` returns 0 if the process exists; ESRCH if not;
    // EPERM if owned by another user.
    //
    // SAFETY: `libc::kill` is FFI but takes only POD args (`pid_t` +
    // signal number). Passing signal 0 means "check if process is
    // alive" — no process state is modified. The only side effect is
    // setting `errno`. Kernel handles invalid `pid_t` values
    // (negative, zero) by returning -1 with an errno; no UB.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("resident already running at PID {pid}"),
        ));
    }
    // Enumerate errno explicitly (codex F9): ESRCH = truly stale,
    // EPERM = different user, anything else = unexpected OS error
    // that we must surface rather than overwrite.
    let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    match errno {
        libc::ESRCH => Ok(()),
        libc::EPERM => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("PID {pid} exists but belongs to another user"),
        )),
        other => Err(io::Error::other(format!("unexpected errno {other} while probing PID {pid}"))),
    }
}

fn write_pid_file(path: &Path, pid: u32) -> io::Result<()> {
    // Unique per-process tmp name (codex F2). The old shared
    // `soma.pid.tmp` could be unlinked by a racing `Resident::start`
    // leaving the other write surfaced as a rename of its own
    // already-clobbered file. `soma.pid.tmp.<pid>` is inherently
    // per-process, and `create_new(true)` additionally refuses to
    // clobber a pre-existing tmp — we never overwrite files we
    // didn't just create.
    let tmp_name =
        format!("{}.tmp.{pid}", path.file_name().and_then(|s| s.to_str()).unwrap_or("soma.pid"));
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f =
            std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&tmp)?;
        use std::io::Write;
        writeln!(f, "{pid}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
