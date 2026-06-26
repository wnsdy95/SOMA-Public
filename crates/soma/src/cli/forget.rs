//! `soma forget` — soft-delete episodes by id / project / time.
//!
//! Discussion 0034 §A-half close-out follow-up. Pre-D86 the verb
//! was an exit-7 stub (D92 P2 fix). This module wires the
//! `Storage::forget_*` API into the CLI so users can purge
//! sensitive content without breaking foreign keys.
//!
//! Soft-delete semantics — episodes get `forgotten_at_ns` stamped
//! plus `note_pins.reason='forgotten:<reason>'` audit. The recall
//! path (HNSW, softmax, decay) filters them out automatically.
//! `soma inspect` can still surface them with `--include-forgotten`
//! for forensic use.

use std::path::PathBuf;

use crate::cli::ForgetArgs;
use crate::storage::{Storage, StorageError};

#[derive(Debug, Clone)]
pub struct ForgetContext {
    pub db_path: PathBuf,
}

#[derive(Debug)]
pub enum ForgetError {
    Path(String),
    Storage(StorageError),
    BadInput(String),
}

impl std::fmt::Display for ForgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgetError::Path(m) => write!(f, "path: {m}"),
            ForgetError::Storage(e) => write!(f, "storage: {e}"),
            ForgetError::BadInput(m) => write!(f, "bad input: {m}"),
        }
    }
}

impl std::error::Error for ForgetError {}

impl From<StorageError> for ForgetError {
    fn from(e: StorageError) -> Self {
        ForgetError::Storage(e)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ForgetOutcome {
    pub forgotten_count: u64,
}

/// Run one `soma forget` invocation. Either `--episode <id>`,
/// `--project <name>`, or `--before <ts>` must be supplied
/// (mutually exclusive). The `--reason` text is recorded in the
/// audit pin.
pub fn run_forget(args: &ForgetArgs, ctx: &ForgetContext) -> Result<ForgetOutcome, ForgetError> {
    let mut store = Storage::open(&ctx.db_path)?;
    let reason = args.reason.as_deref().unwrap_or("user-request");

    let modes = [args.episode.is_some(), args.project.is_some(), args.before.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if modes != 1 {
        return Err(ForgetError::BadInput(
            "exactly one of --episode / --project / --before is required".into(),
        ));
    }

    let n = if let Some(id) = args.episode {
        if store.forget_episode(id, reason)? {
            1
        } else {
            0
        }
    } else if let Some(project) = args.project.as_deref() {
        store.forget_by_project(project, reason)?
    } else if let Some(ts_str) = args.before.as_deref() {
        let ts_ns = parse_ts_to_ns(ts_str)?;
        store.forget_before(ts_ns, reason)?
    } else {
        unreachable!("mutex check above");
    };

    Ok(ForgetOutcome { forgotten_count: n })
}

/// Accept either a unix-epoch ns integer or an ISO-8601 timestamp
/// (`YYYY-MM-DDThh:mm:ssZ`). v1 keeps the parser tight — no chrono
/// dep — and rejects richer forms with a clear error.
fn parse_ts_to_ns(s: &str) -> Result<i64, ForgetError> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(n);
    }
    let bytes = s.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err(ForgetError::BadInput(format!(
            "--before: expected unix-ns integer or `YYYY-MM-DDThh:mm:ssZ`, got `{s}`"
        )));
    }
    let parse = |rng: std::ops::Range<usize>, label: &str| -> Result<i64, ForgetError> {
        std::str::from_utf8(&bytes[rng])
            .ok()
            .and_then(|x| x.parse::<i64>().ok())
            .ok_or_else(|| ForgetError::BadInput(format!("--before {label} parse fail")))
    };
    let year = parse(0..4, "year")?;
    let month = parse(5..7, "month")?;
    let day = parse(8..10, "day")?;
    let hour = parse(11..13, "hour")?;
    let min = parse(14..16, "minute")?;
    let sec = parse(17..19, "second")?;

    // Howard Hinnant's date algorithm — days since 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;
    let secs = days_since_epoch * 86400 + hour * 3600 + min * 60 + sec;
    Ok(secs * 1_000_000_000)
}

pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, ForgetError> {
    if let Some(p) = cli_override {
        return Ok(PathBuf::from(p));
    }
    if let Ok(env) = std::env::var("SOMA_DB") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let home =
        dirs::home_dir().ok_or_else(|| ForgetError::Path("home dir not resolvable".into()))?;
    Ok(home.join(".soma").join("soma.db"))
}

pub fn exit_code_for(e: &ForgetError) -> i32 {
    match e {
        ForgetError::BadInput(_) => 1,
        ForgetError::Storage(_) => 2,
        ForgetError::Path(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ns_integer() {
        assert_eq!(parse_ts_to_ns("1700000000000000000").unwrap(), 1_700_000_000_000_000_000);
    }

    #[test]
    fn parse_iso_8601() {
        let ns = parse_ts_to_ns("2024-01-01T00:00:00Z").unwrap();
        assert_eq!(ns, 1_704_067_200_000_000_000);
    }

    #[test]
    fn parse_bad_format_rejected() {
        assert!(parse_ts_to_ns("yesterday").is_err());
        assert!(parse_ts_to_ns("2024-01-01").is_err());
        assert!(parse_ts_to_ns("2024-01-01 00:00:00").is_err());
    }
}
