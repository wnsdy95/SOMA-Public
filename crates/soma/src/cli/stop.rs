//! `soma stop` — connect to the resident's Unix control socket and
//! send a `Stop` control message. D0 §B fills what main.rs used to
//! print as `"soma: stop — Phase 1 TODO"`.
//!
//! The wire is NDJSON over Unix domain socket; the
//! `runtime::resident` module owns the protocol shape (discussion
//! 0025 §F + §G). We deliberately reuse the `ControlRequest` /
//! `ControlResponse` enums there instead of hand-rolling JSON so a
//! future protocol bump touches one place.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::runtime::resident::{ControlRequest, ControlResponse, PROTOCOL_VERSION};

/// Failure modes. `NotRunning` is the soft case the dispatcher
/// treats as an exit-zero "nothing to do" — it lines up with how
/// `kill -0` behaves toward an absent process.
#[derive(Debug)]
pub enum StopError {
    Path(String),
    NotRunning,
    Connect(String),
    Protocol(String),
    Runtime(String),
}

impl std::fmt::Display for StopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopError::Path(m) => write!(f, "path: {m}"),
            StopError::NotRunning => write!(f, "resident not running"),
            StopError::Connect(m) => write!(f, "connect: {m}"),
            StopError::Protocol(m) => write!(f, "protocol: {m}"),
            StopError::Runtime(m) => write!(f, "runtime: {m}"),
        }
    }
}

impl std::error::Error for StopError {}

/// Resolve `~/.soma/run/soma.sock`. Tests inject a tempdir-based
/// path through `run_blocking_with_socket` instead.
pub fn resolve_socket_path() -> Result<PathBuf, StopError> {
    let home = dirs::home_dir().ok_or_else(|| {
        StopError::Path("home directory not resolvable; set $HOME or run as a real user".into())
    })?;
    Ok(home.join(".soma").join("run").join("soma.sock"))
}

/// Run a one-shot stop request. Returns once the resident has acked
/// (or refused). Errors are typed so the caller can pick an exit code.
///
/// D106-cand — the timeout reads from `~/.soma/config.toml` via
/// `RuntimeConfig::cli_stop_timeout_secs`. Default 5 s when no
/// config file is present. Tests bypass the config layer through
/// `run_blocking_with_socket` (default 5 s).
pub fn run_blocking() -> Result<(), StopError> {
    let socket = resolve_socket_path()?;
    let home = dirs::home_dir().ok_or_else(|| {
        StopError::Path("home directory not resolvable; set $HOME or run as a real user".into())
    })?;
    let cfg = crate::config::Config::load_or_default(&home.join(".soma"));
    let timeout = Duration::from_secs(cfg.runtime.cli_stop_timeout_secs);
    run_blocking_with_socket_and_timeout(&socket, timeout)
}

/// Same as `run_blocking` but with the socket path injected — used
/// by integration tests. Keeps the historical 5 s timeout default
/// so existing tests don't have to thread `RuntimeConfig` through.
pub fn run_blocking_with_socket(socket: &std::path::Path) -> Result<(), StopError> {
    run_blocking_with_socket_and_timeout(socket, Duration::from_secs(5))
}

fn run_blocking_with_socket_and_timeout(
    socket: &std::path::Path,
    timeout: Duration,
) -> Result<(), StopError> {
    if !socket.exists() {
        return Err(StopError::NotRunning);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| StopError::Runtime(format!("tokio build: {e}")))?;
    runtime.block_on(send_stop(socket, timeout))
}

async fn send_stop(socket: &std::path::Path, timeout: Duration) -> Result<(), StopError> {
    // Sanity-cap the round-trip so a hung resident doesn't leave
    // `soma stop` blocking the LaunchAgent uninstall path forever.
    tokio::time::timeout(timeout, send_stop_inner(socket))
        .await
        .map_err(|_| StopError::Protocol("timed out waiting for resident response".into()))?
}

async fn send_stop_inner(socket: &std::path::Path) -> Result<(), StopError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| StopError::Connect(format!("{}: {e}", socket.display())))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    write_request(&mut writer, &ControlRequest::Hello { protocol_version: PROTOCOL_VERSION })
        .await?;
    // Codex review #4 (2026-04-29) — symmetric with cli/status.rs.
    // Hello reply must be inspected so a PROTOCOL mismatch doesn't
    // produce a confusing "Broken pipe" on the next write.
    match read_response(&mut reader).await? {
        ControlResponse::HelloOk { .. } => {}
        ControlResponse::Error { code, expected, got, .. } => {
            return Err(StopError::Protocol(format!(
                "resident rejected Hello (code={code:?}, expected={expected:?}, got={got:?}). \
                 Restart it with `launchctl kickstart -k gui/$UID/dev.soma.runtime` \
                 (or `kill <pid> && soma start` on non-launchd setups) so the new \
                 binary takes over."
            )));
        }
        other => {
            return Err(StopError::Protocol(format!("unexpected Hello response: {other:?}")));
        }
    }

    write_request(&mut writer, &ControlRequest::Stop).await?;
    match read_response(&mut reader).await? {
        ControlResponse::StopAck => Ok(()),
        other => Err(StopError::Protocol(format!("unexpected response: {other:?}"))),
    }
}

async fn write_request(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    req: &ControlRequest,
) -> Result<(), StopError> {
    let line = serde_json::to_string(req)
        .map_err(|e| StopError::Protocol(format!("encode request: {e}")))?;
    w.write_all(line.as_bytes())
        .await
        .map_err(|e| StopError::Protocol(format!("write request: {e}")))?;
    w.write_all(b"\n").await.map_err(|e| StopError::Protocol(format!("write nl: {e}")))?;
    Ok(())
}

async fn read_response<R>(r: &mut BufReader<R>) -> Result<ControlResponse, StopError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = r.read_line(&mut line).await.map_err(|e| StopError::Protocol(format!("read: {e}")))?;
    if n == 0 {
        return Err(StopError::Protocol("resident closed connection".into()));
    }
    serde_json::from_str(line.trim())
        .map_err(|e| StopError::Protocol(format!("decode response: {e}")))
}

/// Map `StopError` to a process exit code. `NotRunning` is exit 0
/// — the user asked for "stop", and "already stopped" is the
/// requested state.
pub fn exit_code_for(e: &StopError) -> i32 {
    match e {
        StopError::NotRunning => 0,
        StopError::Path(_) => 3,
        StopError::Connect(_) => 4,
        StopError::Protocol(_) | StopError::Runtime(_) => 5,
    }
}
