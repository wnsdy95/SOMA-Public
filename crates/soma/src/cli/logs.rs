//! `soma logs tail` — D27 close.
//!
//! Tail the rolling log file written by [`crate::main::init_tracing`]
//! (D128). The rolling daily appender produces files of the shape
//! `<log_dir>/soma.log.YYYY-MM-DD` (one per UTC day; the active day
//! also exists as `soma.log` on systems where `tracing-appender`
//! flushes a symlinked head).
//!
//! Behaviour:
//!
//! * Resolve `~/.soma/log/` (override via `SOMA_LOG_DIR` for tests).
//! * Enumerate every entry whose file name starts with `soma.log`.
//! * If none exist, print a clean `no log file yet` message and
//!   return `Ok(())` — a freshly-installed user has not started the
//!   resident yet, so there is nothing to tail and an error would
//!   misrepresent reality.
//! * Otherwise, pick the lexically last name (date suffixes sort
//!   chronologically, so this is the most-recent rotation), read
//!   the last `lines` lines, and write them to stdout.
//!
//! Errors are typed (`LogsError`) so the CLI dispatcher can pick a
//! distinct exit code per failure mode (cf. `cli::forget` /
//! `cli::recall`).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Default tail length when the operator does not pass `-n`.
pub const DEFAULT_TAIL_LINES: usize = 50;

/// Sentinel printed when the log directory holds no `soma.log*`
/// entries. Asserted verbatim by `tests/logs_cli.rs::test_no_log_yet`
/// so that downstream tooling (support tickets, MCP rendering) can
/// pattern-match on the exact phrase.
pub const NO_LOG_MSG: &str =
    "soma: no log file yet — resident may not have started or has not emitted any tracing events.";

/// `soma logs tail` flags. Public so the CLI dispatcher can build
/// it via clap's derive.
#[derive(Debug, clap::Parser)]
pub struct LogsArgs {
    #[command(subcommand)]
    pub cmd: LogsCmd,
}

/// `soma logs <subcommand>`. `tail` is the only verb today; future
/// verbs (e.g. `soma logs path`) drop in here.
#[derive(Debug, clap::Subcommand)]
pub enum LogsCmd {
    /// Print the last N lines of the rolling log file.
    Tail(TailArgs),
}

#[derive(Debug, clap::Parser)]
pub struct TailArgs {
    /// Number of trailing lines to print. Defaults to 50.
    #[arg(short = 'n', long, default_value_t = DEFAULT_TAIL_LINES)]
    pub lines: usize,
    /// Override the log directory (otherwise `~/.soma/log`). Mainly
    /// for the `tests/logs_cli.rs` integration matrix.
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum LogsError {
    Path(String),
    Io(std::io::Error),
}

impl std::fmt::Display for LogsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogsError::Path(m) => write!(f, "path: {m}"),
            LogsError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for LogsError {}

impl From<std::io::Error> for LogsError {
    fn from(e: std::io::Error) -> Self {
        LogsError::Io(e)
    }
}

pub fn exit_code_for(e: &LogsError) -> i32 {
    match e {
        LogsError::Path(_) => 3,
        LogsError::Io(_) => 2,
    }
}

/// Resolve the log directory. Layered precedence:
///   1. `--log-dir` flag (wins for tests / forensic dumps).
///   2. `SOMA_LOG_DIR` env (lets the resident hand its own dir to
///      child processes via launchctl `EnvironmentVariables`).
///   3. `~/.soma/log` (production default).
pub fn resolve_log_dir(cli_override: Option<&Path>) -> Result<PathBuf, LogsError> {
    if let Some(p) = cli_override {
        return Ok(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("SOMA_LOG_DIR") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let home =
        dirs::home_dir().ok_or_else(|| LogsError::Path("home directory not resolvable".into()))?;
    Ok(home.join(".soma").join("log"))
}

/// Tail the most-recent rolling log file. Returns `Ok(())` for both
/// the "no log yet" branch and the success branch — both are valid
/// outcomes per spec; only filesystem I/O failures bubble.
pub fn run_blocking(args: &LogsArgs) -> Result<(), LogsError> {
    match &args.cmd {
        LogsCmd::Tail(t) => run_tail(t),
    }
}

fn run_tail(args: &TailArgs) -> Result<(), LogsError> {
    let log_dir = resolve_log_dir(args.log_dir.as_deref())?;
    let target = match pick_latest_log(&log_dir)? {
        Some(p) => p,
        None => {
            println!("{NO_LOG_MSG}");
            return Ok(());
        }
    };
    let lines = tail_lines(&target, args.lines)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Enumerate `soma.log*` entries under `log_dir` and return the
/// lexically last one (which, for the rolling daily appender naming
/// scheme `soma.log.YYYY-MM-DD`, is the most recent UTC day).
///
/// `Ok(None)` when the directory does not exist or holds zero
/// matching files. We treat both as "no log yet" — a missing
/// directory is the v1 first-run state.
pub fn pick_latest_log(log_dir: &Path) -> Result<Option<PathBuf>, LogsError> {
    let entries = match std::fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LogsError::Io(e)),
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        // `soma.log` (head, when present) and `soma.log.YYYY-MM-DD`
        // (rotated). starts_with covers both without resorting to a
        // glob crate.
        if name_str.starts_with("soma.log") {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    Ok(candidates.pop())
}

/// Read the last `n` lines of `path`. Implemented as a single linear
/// pass into a ring buffer — fine for the v1 rolling-daily log
/// volume (≤ a few MB / day even under heavy ingest).
pub fn tail_lines(path: &Path, n: usize) -> Result<Vec<String>, LogsError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut ring: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(n);
    for line in reader.lines() {
        let line = line?;
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    Ok(ring.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_log(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn pick_latest_log_returns_none_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(pick_latest_log(&missing).unwrap().is_none());
    }

    #[test]
    fn pick_latest_log_returns_none_when_dir_empty() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        assert!(pick_latest_log(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn pick_latest_log_picks_latest_dated_suffix() {
        let tmp = TempDir::new().unwrap();
        write_log(tmp.path(), "soma.log.2026-04-28", "old\n");
        write_log(tmp.path(), "soma.log.2026-04-30", "new\n");
        write_log(tmp.path(), "soma.log.2026-04-29", "mid\n");
        let picked = pick_latest_log(tmp.path()).unwrap().unwrap();
        assert!(picked.ends_with("soma.log.2026-04-30"));
    }

    #[test]
    fn pick_latest_log_ignores_unrelated_files() {
        let tmp = TempDir::new().unwrap();
        write_log(tmp.path(), "soma.log.2026-04-30", "yes\n");
        write_log(tmp.path(), "config.toml", "no\n");
        write_log(tmp.path(), "other.log", "no\n");
        let picked = pick_latest_log(tmp.path()).unwrap().unwrap();
        assert!(picked.ends_with("soma.log.2026-04-30"));
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let tmp = TempDir::new().unwrap();
        let path = write_log(tmp.path(), "soma.log.2026-04-30", "a\nb\nc\nd\ne\n");
        let last3 = tail_lines(&path, 3).unwrap();
        assert_eq!(last3, vec!["c".to_string(), "d".to_string(), "e".to_string()]);
    }

    #[test]
    fn tail_lines_returns_all_when_fewer_than_n() {
        let tmp = TempDir::new().unwrap();
        let path = write_log(tmp.path(), "soma.log.2026-04-30", "a\nb\n");
        let lines = tail_lines(&path, 50).unwrap();
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn tail_lines_zero_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let path = write_log(tmp.path(), "soma.log.2026-04-30", "a\nb\n");
        assert!(tail_lines(&path, 0).unwrap().is_empty());
    }

    #[test]
    fn resolve_log_dir_honors_cli_override() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_log_dir(Some(tmp.path())).unwrap();
        assert_eq!(resolved, tmp.path().to_path_buf());
    }
}
