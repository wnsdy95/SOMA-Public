//! `soma profile` handler — render evidence-backed context profile rows.
//! Discussion 0030 §E + §H.
//!
//! Default path: read the current snapshot + render as markdown.
//! With `--recompute`, re-run `self_model::run_all` first. JSON
//! format via `--format=json`. Same `--db-path` precedence as
//! `soma ingest` / `soma recall`.
//!
//! The storage table is still named `self_state` for schema continuity,
//! but this CLI is an operator view of context hints,
//! not a first-person persona or assistant identity surface.

use std::path::PathBuf;

use crate::cli::ProfileArgs;
use crate::self_model::{self, SelfSnapshot, SelfSnapshotEntry};
use crate::storage::{Storage, StorageError};

#[derive(Debug)]
pub enum ProfileError {
    Storage(StorageError),
    Path(String),
    BadFormat(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Storage(e) => write!(f, "storage: {e}"),
            ProfileError::Path(m) => write!(f, "path: {m}"),
            ProfileError::BadFormat(m) => write!(f, "bad format: {m}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<StorageError> for ProfileError {
    fn from(e: StorageError) -> Self {
        ProfileError::Storage(e)
    }
}

#[derive(Debug, Clone)]
pub struct ProfileContext {
    pub db_path: PathBuf,
}

pub fn run_profile(args: &ProfileArgs, ctx: &ProfileContext) -> Result<String, ProfileError> {
    let fmt = match args.format.as_str() {
        "markdown" | "md" => OutputFormat::Markdown,
        "json" => OutputFormat::Json,
        other => {
            return Err(ProfileError::BadFormat(format!(
                "unknown format `{other}`; expected `markdown` or `json`"
            )));
        }
    };

    let mut storage = Storage::open(&ctx.db_path)?;
    if args.recompute {
        self_model::run_all(&mut storage)?;
    }
    let snap = self_model::read_snapshot(&storage)?;

    Ok(match fmt {
        OutputFormat::Markdown => render_markdown(&snap),
        OutputFormat::Json => render_json(&snap),
    })
}

enum OutputFormat {
    Markdown,
    Json,
}

fn render_markdown(snap: &SelfSnapshot) -> String {
    let mut out = String::new();
    out.push_str("# Context profile\n\n");
    if snap.is_empty() {
        out.push_str("_No context profile facts computed yet. Run `soma profile --recompute`._\n");
        return out;
    }

    out.push_str(&format!("_Computed at ns: {}_\n\n", snap.computed_at_ns));

    if !snap.tool_use.is_empty() {
        out.push_str("## Tool use\n\n");
        for entry in &snap.tool_use {
            render_entry(&mut out, entry);
        }
    }
    if !snap.exit_success.is_empty() {
        out.push_str("## Exit success\n\n");
        for entry in &snap.exit_success {
            render_entry(&mut out, entry);
        }
    }
    if !snap.project_norms.is_empty() {
        out.push_str("## Project norms\n\n");
        for entry in &snap.project_norms {
            render_entry(&mut out, entry);
        }
    }
    if !snap.other.is_empty() {
        out.push_str("## Other\n\n");
        for entry in &snap.other {
            render_entry(&mut out, entry);
        }
    }
    out
}

fn render_entry(out: &mut String, entry: &SelfSnapshotEntry) {
    let value = serde_json::to_string(&entry.value).unwrap_or_else(|_| "{}".to_string());
    out.push_str(&format!("### {}\n", entry.key));
    out.push_str(&format!("- value: `{value}`\n"));
    out.push_str(&format!("- evidence: {} episodes\n\n", entry.evidence_ids.len()));
}

fn render_json(snap: &SelfSnapshot) -> String {
    serde_json::to_string(snap).unwrap_or_else(|_| "{}".to_string())
}

pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, ProfileError> {
    crate::capture::ai_cli::resolve_db_path(cli_override).map_err(|e| {
        use crate::capture::ai_cli::IngestError;
        match e {
            IngestError::Path(m) => ProfileError::Path(m),
            other => ProfileError::Path(other.to_string()),
        }
    })
}

pub fn exit_code_for(e: &ProfileError) -> i32 {
    match e {
        ProfileError::BadFormat(_) => 1,
        ProfileError::Storage(_) => 2,
        ProfileError::Path(_) => 3,
    }
}
