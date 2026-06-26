//! `soma status` — read resident info (PID, profile, episodes,
//! pending jobs) via the local Unix socket. D0 §B fills the
//! previously-empty stub. Falls back to `"not running"` (exit 0)
//! when the socket file is absent.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::cli::StatusArgs;
use crate::profile;
use crate::runtime::resident::{ControlRequest, ControlResponse, PROTOCOL_VERSION};

#[derive(Debug)]
pub enum StatusError {
    Path(String),
    Connect(String),
    Protocol(String),
    Runtime(String),
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatusError::Path(m) => write!(f, "path: {m}"),
            StatusError::Connect(m) => write!(
                f,
                "connect: {m}. Is the resident running? \
                 Try `launchctl kickstart -k gui/$UID/dev.soma.runtime` (LaunchAgent) \
                 or `soma start` (foreground)."
            ),
            StatusError::Protocol(m) => write!(f, "protocol: {m}"),
            StatusError::Runtime(m) => write!(f, "runtime: {m}"),
        }
    }
}

impl std::error::Error for StatusError {}

pub fn resolve_socket_path() -> Result<PathBuf, StatusError> {
    let home = dirs::home_dir()
        .ok_or_else(|| StatusError::Path("home directory not resolvable".into()))?;
    Ok(home.join(".soma").join("run").join("soma.sock"))
}

/// Render a human-readable status block to stdout. Returns `Ok` on
/// both "live resident" and "no live resident" — the user-visible
/// difference is in the rendered text.
///
/// D107-cand — the timeout reads from `~/.soma/config.toml` via
/// `RuntimeConfig::cli_status_timeout_secs`. Default 3 s when no
/// config file is present.
pub fn run_blocking(args: &StatusArgs) -> Result<(), StatusError> {
    let socket = resolve_socket_path()?;
    if !socket.exists() {
        let detected = profile::detect();
        if args.wants_json_output() {
            print_status_json(status_not_running_value(&socket, &format!("{detected:?}")))?;
        } else {
            println!("soma: status");
            println!("  resident:           not running");
            println!("  profile (detected): {detected:?}");
            println!("  socket (expected):  {}", socket.display());
        }
        return Ok(());
    }
    let home = dirs::home_dir()
        .ok_or_else(|| StatusError::Path("home directory not resolvable".into()))?;
    let cfg = crate::config::Config::load_or_default(&home.join(".soma"));
    let timeout = Duration::from_secs(cfg.runtime.cli_status_timeout_secs);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| StatusError::Runtime(format!("tokio build: {e}")))?;
    let resp = runtime.block_on(send_status(&socket, timeout))?;
    if args.wants_json_output() {
        print_status_json(status_response_value(resp, &socket))?;
    } else {
        print_status(resp, &socket);
    }
    Ok(())
}

/// D129-cand close (R9 audit) — `soma diagnose` adapter.
///
/// Connects + Hello + Status with a default 3s budget, then
/// projects the `StatusOk` payload into a JSON object suitable for
/// the diagnose dump. Returns a structured value either way.
pub async fn send_status_for_diagnose(
    socket: &std::path::Path,
) -> Result<serde_json::Value, StatusError> {
    let resp = tokio::time::timeout(Duration::from_secs(3), send_status_inner(socket))
        .await
        .map_err(|_| StatusError::Protocol("timed out waiting for status".into()))??;
    Ok(serde_json::json!({
        "state": "running",
        "socket": socket.display().to_string(),
        "response": match resp {
            ControlResponse::StatusOk {
                pid,
                profile,
                uptime_ms,
                episodes_total,
                pending_jobs,
                cache_hits,
                cache_misses,
                mlstm_dim,
                mlstm_train_steps,
                mlstm_saved_at_ns,
                degraded_reasons,
            } => serde_json::json!({
                "pid": pid,
                "profile": profile,
                "uptime_ms": uptime_ms,
                "episodes_total": episodes_total,
                "pending_jobs": pending_jobs,
                "cache_hits": cache_hits,
                "cache_misses": cache_misses,
                "mlstm_dim": mlstm_dim,
                "mlstm_train_steps": mlstm_train_steps,
                "mlstm_saved_at_ns": mlstm_saved_at_ns,
                "degraded_reasons": degraded_reasons,
            }),
            other => serde_json::json!({"unexpected": format!("{other:?}")}),
        },
    }))
}

fn status_not_running_value(socket: &std::path::Path, profile_detected: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "soma.status.v1",
        "source": "soma_status",
        "state": "not_running",
        "resident": {
            "running": false,
            "status": "not_running"
        },
        "profile_detected": profile_detected,
        "socket": {
            "path": socket.display().to_string(),
            "exists": false
        },
        "next_commands": [
            ["soma", "start"],
            ["launchctl", "kickstart", "-k", "gui/$UID/dev.soma.runtime"]
        ],
        "trust_boundary": "soma_status_is_read_only: reports resident runtime state only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft"
    })
}

fn status_response_value(resp: ControlResponse, socket: &std::path::Path) -> serde_json::Value {
    match resp {
        ControlResponse::StatusOk {
            pid,
            profile,
            uptime_ms,
            episodes_total,
            pending_jobs,
            cache_hits,
            cache_misses,
            mlstm_dim,
            mlstm_train_steps,
            mlstm_saved_at_ns,
            degraded_reasons,
        } => {
            let cache_total = cache_hits + cache_misses;
            let cache_ratio_percent = if cache_total == 0 {
                None
            } else {
                Some(cache_hits as f64 / cache_total as f64 * 100.0)
            };
            serde_json::json!({
                "schema": "soma.status.v1",
                "source": "soma_status",
                "state": "running",
                "resident": {
                    "running": true,
                    "status": "running",
                    "pid": pid,
                    "profile": profile,
                    "uptime_ms": uptime_ms,
                    "episodes_total": episodes_total,
                    "pending_jobs": pending_jobs,
                    "degraded": !degraded_reasons.is_empty(),
                    "degraded_reasons": degraded_reasons,
                },
                "cache": {
                    "source": "cloud_llm_mcp_resources_read_only",
                    "hits": cache_hits,
                    "misses": cache_misses,
                    "total_fetches": cache_total,
                    "hit_ratio_percent": cache_ratio_percent,
                    "zero_fetches_means": "no MCP resources/read has reached the resident cache yet; capture hooks do not increment this counter"
                },
                "mlstm": {
                    "role": "connected_candidate_selector_diagnostic",
                    "dim": mlstm_dim,
                    "train_steps": mlstm_train_steps,
                    "saved_at_ns": mlstm_saved_at_ns,
                    "saved_at": format_ts_ns(mlstm_saved_at_ns),
                    "status_line": mlstm_status_line(mlstm_dim, mlstm_train_steps, mlstm_saved_at_ns)
                },
                "socket": {
                    "path": socket.display().to_string(),
                    "exists": true
                },
                "next_commands": [
                    ["soma", "clients", "--brief"],
                    ["soma", "diagnose"]
                ],
                "trust_boundary": "soma_status_is_read_only: reports resident runtime state only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft"
            })
        }
        other => serde_json::json!({
            "schema": "soma.status.v1",
            "source": "soma_status",
            "state": "protocol_error",
            "resident": {
                "running": false,
                "status": "protocol_error"
            },
            "socket": {
                "path": socket.display().to_string(),
                "exists": true
            },
            "unexpected_response": format!("{other:?}"),
            "trust_boundary": "soma_status_is_read_only: reports resident runtime state only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft"
        }),
    }
}

fn print_status_json(value: serde_json::Value) -> Result<(), StatusError> {
    let rendered = serde_json::to_string_pretty(&value)
        .map_err(|e| StatusError::Protocol(format!("encode status json: {e}")))?;
    println!("{rendered}");
    Ok(())
}

async fn send_status(
    socket: &std::path::Path,
    timeout: Duration,
) -> Result<ControlResponse, StatusError> {
    tokio::time::timeout(timeout, send_status_inner(socket))
        .await
        .map_err(|_| StatusError::Protocol("timed out waiting for status".into()))?
}

async fn send_status_inner(socket: &std::path::Path) -> Result<ControlResponse, StatusError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| StatusError::Connect(format!("{}: {e}", socket.display())))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let hello = ControlRequest::Hello { protocol_version: PROTOCOL_VERSION };
    write_request(&mut writer, &hello).await?;
    // Codex review #4 (2026-04-29) — pre-fix the Hello response was
    // discarded with `let _ = ...`. When the running resident is on
    // an older PROTOCOL_VERSION the Hello reply is `Error{Version
    // Mismatch}` + connection close; the discard hid that and the
    // next `write_request` produced an unhelpful "Broken pipe (os
    // error 32)". Now we inspect the Hello reply explicitly and
    // surface a directly actionable message including the upgrade
    // command, so the user knows to restart the resident.
    match read_response(&mut reader).await? {
        ControlResponse::HelloOk { .. } => {}
        ControlResponse::Error { code, expected, got, .. } => {
            return Err(StatusError::Protocol(format!(
                "resident rejected Hello (code={code:?}, expected={expected:?}, got={got:?}). \
                 Restart it with `launchctl kickstart -k gui/$UID/dev.soma.runtime` \
                 (or `soma stop && soma start` on non-launchd setups) so the new \
                 binary takes over."
            )));
        }
        other => {
            return Err(StatusError::Protocol(format!("unexpected Hello response: {other:?}")));
        }
    }

    write_request(&mut writer, &ControlRequest::Status).await?;
    read_response(&mut reader).await
}

async fn write_request(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    req: &ControlRequest,
) -> Result<(), StatusError> {
    let line =
        serde_json::to_string(req).map_err(|e| StatusError::Protocol(format!("encode: {e}")))?;
    w.write_all(line.as_bytes()).await.map_err(|e| StatusError::Protocol(format!("write: {e}")))?;
    w.write_all(b"\n").await.map_err(|e| StatusError::Protocol(format!("write nl: {e}")))?;
    Ok(())
}

async fn read_response<R>(r: &mut BufReader<R>) -> Result<ControlResponse, StatusError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n =
        r.read_line(&mut line).await.map_err(|e| StatusError::Protocol(format!("read: {e}")))?;
    if n == 0 {
        return Err(StatusError::Protocol("resident closed connection".into()));
    }
    serde_json::from_str(line.trim()).map_err(|e| StatusError::Protocol(format!("decode: {e}")))
}

fn print_status(resp: ControlResponse, socket: &std::path::Path) {
    match resp {
        ControlResponse::StatusOk {
            pid,
            profile,
            uptime_ms,
            episodes_total,
            pending_jobs,
            cache_hits,
            cache_misses,
            mlstm_dim,
            mlstm_train_steps,
            mlstm_saved_at_ns,
            degraded_reasons,
        } => {
            println!("soma: status");
            println!("  resident:           running (pid {pid})");
            println!("  profile:            {profile}");
            println!("  uptime_ms:          {uptime_ms}");
            println!("  episodes_total:     {episodes_total}");
            println!("  pending_jobs:       {pending_jobs}");
            // D87 §B — MCP cache hit ratio. The cache only sits on
            // the **cloud-LLM MCP read path** (`runtime/mcp.rs::
            // resources_read`) — capture hooks do file I/O, not MCP.
            // So `0 fetches` means no
            // Claude Code / Cursor / Continue session has yet read a
            // SOMA MCP context resource, not that SOMA is idle.
            let total = cache_hits + cache_misses;
            if total == 0 {
                println!(
                    "  cache:              0 fetches (no MCP resources/read yet — \
                     attach `@soma:context/current` in Claude Code to populate)"
                );
            } else {
                let ratio = cache_hits as f32 / total as f32;
                println!(
                    "  cache:              hits={cache_hits} misses={cache_misses} ratio={:.1}% (cloud-LLM MCP fetches only)",
                    ratio * 100.0
                );
            }
            // N3 — optional mLSTM quality-diagnostic visibility.
            // `None` means cognitive-train feature off OR no slow_loop
            // diagnostic cycle has persisted yet (first cycle fires after `delay_first =
            // 5 min` on resident boot, then hourly).
            //
            // D99-cand close (2026-04-29) — pre-fix the single line
            // "not yet trained" 합쳐 (a) feature OFF (b) awaiting
            // first cycle (c) persist failed/corrupt 3 case 가 모두
            // 같은 표현. compile-time feature flag 로 (a) 를
            // 분리, runtime None 은 (b)+(c) 합친 "awaiting first
            // slow-loop cycle" 로.
            println!("{}", mlstm_status_line(mlstm_dim, mlstm_train_steps, mlstm_saved_at_ns));
            println!("  socket:             {}", socket.display());
            // D98-cand close (2026-04-29) — surface non-fatal snapshot
            // failures so operator distinguishes "fresh DB" from
            // "counter read failed and was zeroed". Empty Vec → no
            // line printed (healthy resident).
            // P3-nit fix (in-house ultrareview): the bare "degraded:"
            // header didn't tell the operator whether each reason was
            // a recovered warning or an active operational problem.
            // Tag explicitly with [warn] so the section reads as
            // "non-fatal snapshot fall-through, not a daemon-down
            // signal".
            if !degraded_reasons.is_empty() {
                println!(
                    "  degraded:        (non-fatal snapshot fall-throughs — daemon still serving)"
                );
                for reason in &degraded_reasons {
                    println!("    - [warn] {reason}");
                }
            }
        }
        other => {
            println!("soma: status");
            println!("  resident:           protocol error ({other:?})");
        }
    }
}

fn mlstm_status_line(
    mlstm_dim: Option<u64>,
    mlstm_train_steps: u64,
    mlstm_saved_at_ns: i64,
) -> String {
    match mlstm_dim {
        Some(dim) => {
            let saved = format_ts_ns(mlstm_saved_at_ns);
            format!(
                "  mlstm:              connected-candidate selector; diagnostic weights dim={dim} train_steps={mlstm_train_steps} saved={saved} (weight drift is diagnostic)"
            )
        }
        None => mlstm_absent_status_line(),
    }
}

#[cfg(feature = "cognitive-train")]
fn mlstm_absent_status_line() -> String {
    "  mlstm:              connected-candidate selector uses working_memory_state; no diagnostic weights row yet"
        .into()
}

#[cfg(not(feature = "cognitive-train"))]
fn mlstm_absent_status_line() -> String {
    "  mlstm:              connected-candidate selector uses working_memory_state; cognitive-train weights off".into()
}

pub fn exit_code_for(e: &StatusError) -> i32 {
    match e {
        StatusError::Path(_) => 3,
        StatusError::Connect(_) => 4,
        StatusError::Protocol(_) | StatusError::Runtime(_) => 5,
    }
}

/// Render `ts_ns` (unix nanoseconds) as a short ISO-8601-ish UTC
/// stamp `YYYY-MM-DDTHH:MM:SSZ`. `0` → "never" so the mlstm line
/// is unambiguous when the cell hasn't yet persisted. Howard
/// Hinnant's civil-from-days algorithm — same fn used by `soma
/// forget --before` parsing for symmetry.
fn format_ts_ns(ts_ns: i64) -> String {
    if ts_ns <= 0 {
        return "never".into();
    }
    let secs = ts_ns / 1_000_000_000;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let h = (sod / 3600) as u32;
    let m = ((sod % 3600) / 60) as u32;
    let s = (sod % 60) as u32;

    // days since 1970-01-01 → YYYY-MM-DD via Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m_civil = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y_civil = if m_civil <= 2 { y + 1 } else { y };

    format!("{y_civil:04}-{m_civil:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::mlstm_status_line;

    #[test]
    fn mlstm_status_does_not_recommend_cognitive_train_as_core_path() {
        let line = mlstm_status_line(None, 0, 0);

        assert!(line.contains("connected-candidate selector"), "{line}");
        assert!(line.contains("working_memory_state"), "{line}");
        assert!(
            line.contains("no diagnostic weights row yet")
                || line.contains("cognitive-train weights off"),
            "{line}"
        );
        assert!(!line.contains("rebuild"), "{line}");
        assert!(!line.contains("--features cognitive-train"), "{line}");
    }

    #[test]
    fn mlstm_status_keeps_existing_weight_snapshot_shape() {
        let line = mlstm_status_line(Some(8), 42, 1_700_000_000_000_000_000);

        assert!(line.contains("connected-candidate selector"), "{line}");
        assert!(line.contains("diagnostic weights"), "{line}");
        assert!(line.contains("dim=8"), "{line}");
        assert!(line.contains("train_steps=42"), "{line}");
        assert!(line.contains("saved=2023-11-14T22:13:20Z"), "{line}");
        assert!(line.contains("weight drift is diagnostic"), "{line}");
    }
}
