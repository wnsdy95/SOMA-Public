//! `soma capture --pty` — live terminal capture via portable-pty.
//!
//! STAGE 2-A-full per ADR 0004 §F. The shell-init half (always-on
//! post-command hook) covers the common case; this pty wrapper is
//! for users who want OSC 133-grade boundary detection +
//! synchronous TUI awareness (vim / htop / less inside a session).
//!
//! Behaviour:
//!
//! 1. Spawn a child shell (default `$SHELL`, fallback `/bin/sh`)
//!    inside a freshly-allocated pty.
//! 2. Forward parent stdin → child stdin and child stdout/stderr →
//!    parent stdout, while feeding the child's output bytes to the
//!    OSC 133 parser.
//! 3. On every `Osc133Event::PostExec { exit_code }`, emit a
//!    `soma ingest --source terminal` invocation with the buffered
//!    command line (collected between `CommandStart` and `PreExec`).
//!
//! Failure modes (advisory): a missing `$SHELL`, a pty alloc error,
//! a child spawn error → printed + exit 1. We treat the running
//! shell as the user's tool and never return a non-zero status
//! solely because of an ingest failure.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::capture::ai_cli::{run_ingest, IngestContext};
use crate::capture::terminal::{FeedItem, Osc133Event, Osc133Parser};
use crate::cli::IngestArgs;

#[derive(Debug)]
pub enum CaptureError {
    Pty(String),
    Spawn(String),
    Io(std::io::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Pty(m) => write!(f, "pty: {m}"),
            CaptureError::Spawn(m) => write!(f, "spawn: {m}"),
            CaptureError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct CaptureContext {
    pub db_path: PathBuf,
    pub project: Option<String>,
    pub session_id: String,
}

/// Spawn the user's shell inside a pty and pump bytes both ways
/// until the child exits. Returns the child's exit status when the
/// session ends. Blocking — caller is the CLI dispatcher.
pub fn run_pty_capture(ctx: &CaptureContext) -> Result<i32, CaptureError> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: 40, cols: 100, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| CaptureError::Pty(format!("openpty: {e}")))?;

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("SOMA_PTY_CAPTURE", "1");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| CaptureError::Spawn(format!("spawn `{shell}`: {e}")))?;
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(Osc133Parser::new()));
    let cmd_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let in_command = Arc::new(Mutex::new(false));
    // D85 close-out — capture stdout bytes during the command's
    // execution window (PreExec → PostExec) so the ingest path
    // surfaces them as the `stdout` BLOB column. Without this,
    // terminal-source episodes carry exit_code but no output.
    let stdout_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let in_output = Arc::new(Mutex::new(false));
    let ingest_ctx = Arc::new(IngestContext { db_path: ctx.db_path.clone() });
    let ctx_arc = Arc::new(ctx.clone());

    // child → parent stdout + OSC 133 routing.
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| CaptureError::Pty(format!("clone reader: {e}")))?;
    let parser_clone = parser.clone();
    let cmd_buf_clone = cmd_buffer.clone();
    let in_cmd_clone = in_command.clone();
    let stdout_buf_clone = stdout_buffer.clone();
    let in_output_clone = in_output.clone();
    let ingest_ctx_clone = ingest_ctx.clone();
    let ctx_clone = ctx_arc.clone();
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let stdout = std::io::stdout();
        // D85 — guard the stdout buffer at 1 MiB. Full-screen TUI
        // output could blow this (vim re-paints the whole window
        // per keystroke) so cap, flag truncation, and let the
        // ingest path attach a stdout="<truncated>" sentinel
        // instead of holding 100 MB in memory.
        const STDOUT_BUFFER_CAP: usize = 1024 * 1024;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let items = {
                let mut p = parser_clone.lock().unwrap();
                p.feed_classified(&buf[..n])
            };
            let mut passthrough: Vec<u8> = Vec::with_capacity(n);
            for item in items {
                match item {
                    FeedItem::Byte(b) => {
                        passthrough.push(b);
                        if *in_cmd_clone.lock().unwrap() {
                            cmd_buf_clone.lock().unwrap().push(b);
                        } else if *in_output_clone.lock().unwrap() {
                            let mut so = stdout_buf_clone.lock().unwrap();
                            if so.len() < STDOUT_BUFFER_CAP {
                                so.push(b);
                            }
                        }
                    }
                    FeedItem::Event(Osc133Event::CommandStart) => {
                        *in_cmd_clone.lock().unwrap() = true;
                        cmd_buf_clone.lock().unwrap().clear();
                    }
                    FeedItem::Event(Osc133Event::PreExec) => {
                        *in_cmd_clone.lock().unwrap() = false;
                        // D85 — output window opens here. Clear any
                        // residual bytes so the captured stdout is
                        // strictly the command's output, not a tail
                        // of the previous prompt's render.
                        *in_output_clone.lock().unwrap() = true;
                        stdout_buf_clone.lock().unwrap().clear();
                    }
                    FeedItem::Event(Osc133Event::PostExec { exit_code }) => {
                        *in_output_clone.lock().unwrap() = false;
                        let cmd_bytes = std::mem::take(&mut *cmd_buf_clone.lock().unwrap());
                        let cmd_text = String::from_utf8_lossy(&cmd_bytes).trim().to_string();
                        let stdout_bytes = std::mem::take(&mut *stdout_buf_clone.lock().unwrap());
                        if !cmd_text.is_empty() {
                            emit_ingest(
                                &ingest_ctx_clone,
                                &ctx_clone,
                                &cmd_text,
                                exit_code,
                                &stdout_bytes,
                            );
                        }
                    }
                    FeedItem::Event(Osc133Event::PromptStart) => {
                        // Boundary marker — nothing to do at v1.
                    }
                }
            }
            let mut out = stdout.lock();
            let _ = out.write_all(&passthrough);
            let _ = out.flush();
        }
    });

    // parent stdin → child stdin.
    let mut writer =
        pair.master.take_writer().map_err(|e| CaptureError::Pty(format!("take writer: {e}")))?;
    let stdin_thread = thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            let mut handle = stdin.lock();
            let n = match handle.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if writer.write_all(&buf[..n]).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    let exit =
        child.wait().map_err(|e| CaptureError::Spawn(format!("wait: {e}")))?.exit_code() as i32;
    // Best-effort drain — parent stdin thread may still be blocked
    // on read(); a short pause helps preserve last bytes but isn't
    // critical for correctness.
    thread::sleep(Duration::from_millis(50));
    drop(reader_thread);
    drop(stdin_thread);
    Ok(exit)
}

fn emit_ingest(
    ingest_ctx: &IngestContext,
    capture_ctx: &CaptureContext,
    cmd: &str,
    exit_code: Option<i32>,
    stdout_bytes: &[u8],
) {
    // D85 — write the captured stdout to a temp file so the
    // ingest path slurps it (existing `stdout_file` arg shape;
    // base64 encode + BLOB write happens in ai_cli). Empty buffer
    // → don't bother with the temp file roundtrip.
    let stdout_file = if stdout_bytes.is_empty() {
        None
    } else {
        match write_temp_stdout(stdout_bytes) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "pty stdout temp file write failed (advisory)");
                None
            }
        }
    };
    let args = IngestArgs {
        source: "terminal".into(),
        session: Some(capture_ctx.session_id.clone()),
        prompt: None,
        response: None,
        command: Some(cmd.to_string()),
        stdout_file,
        exit_code,
        cwd: std::env::current_dir().ok().and_then(|p| p.to_str().map(|s| s.to_string())),
        git_branch: None,
        project: capture_ctx.project.clone(),
        digest: None,
        json: None,
        db_path: None,
    };
    if let Err(e) = run_ingest(&args, ingest_ctx) {
        tracing::warn!(error = %e, "pty ingest failed");
    }
}

/// D85 — drop the captured stdout into `~/.soma/run/pty-stdout-
/// <ts_ns>.bin` for the ingest path to slurp. Caller's temp file
/// — we don't try to clean it up because the ingest path's own
/// stdout reader already consumes it (best-effort delete after
/// successful read happens there too).
fn write_temp_stdout(bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// Process-unique counter — `SystemTime::now()` 의 ns 해상도
    /// 가 multi-core 동시 호출 시 충돌 가능 (write_temp_stdout
    /// 의 두 unit test 가 parallel run 시 같은 ts_ns 받아 path
    /// collision → 한쪽 의 read 가 다른쪽 의 remove 본 race).
    /// AtomicU64 counter 가 그 충돌 path 닫음.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("soma-pty-stdout-{pid}-{ts_ns}-{seq}.bin"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub fn exit_code_for(_e: &CaptureError) -> i32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D85 — `write_temp_stdout` round-trips bytes verbatim into a
    /// pid+ts-tagged temp file. The ingest path picks it up via
    /// `IngestArgs::stdout_file`, so this contract is the seam
    /// between the pty thread and the ingest BLOB write.
    #[test]
    fn write_temp_stdout_roundtrips_bytes() {
        let original = b"hello\nworld\n\xff\x00\x42";
        let path = write_temp_stdout(original).expect("write");
        assert!(path.exists(), "temp file written: {path:?}");
        assert!(
            path.file_name().and_then(|n| n.to_str()).unwrap().starts_with("soma-pty-stdout-"),
            "filename has the expected prefix: {path:?}"
        );
        let read_back = std::fs::read(&path).expect("read");
        assert_eq!(read_back, original, "bytes preserved verbatim");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_temp_stdout_handles_empty_input() {
        let path = write_temp_stdout(b"").expect("write");
        assert!(path.exists());
        let read_back = std::fs::read(&path).expect("read");
        assert!(read_back.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
