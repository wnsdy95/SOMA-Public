//! `soma adapter-capture` - native editor-adapter write entrypoint.
//!
//! This is a thin wrapper around the existing `soma ingest` pipeline. It reads
//! one normalized adapter JSON payload, fills missing local metadata, then
//! persists through `capture::ai_cli` so validation, vector writes, salience,
//! and recall behavior stay identical to normal ingest.

use std::path::{Path, PathBuf};

use crate::capture::ai_cli::{
    run_adapter_capture_json, AdapterCaptureDefaults, IngestContext, IngestError, IngestOutcome,
};
use crate::cli::AdapterCaptureArgs;

#[derive(Debug, Clone)]
pub struct AdapterCaptureContext {
    pub db_path: PathBuf,
}

pub fn run_blocking(
    args: &AdapterCaptureArgs,
    ctx: &AdapterCaptureContext,
) -> Result<IngestOutcome, IngestError> {
    let raw = read_json_arg(&args.json)?;
    run_json_str(
        &raw,
        AdapterCaptureRunOptions {
            source: args.source.clone(),
            cwd: args.cwd.clone(),
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            git_branch: args.git_branch.clone(),
        },
        ctx,
    )
}

#[derive(Debug, Clone, Default)]
pub struct AdapterCaptureRunOptions {
    pub source: Option<String>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub git_branch: Option<String>,
}

pub fn run_json_str(
    raw: &str,
    options: AdapterCaptureRunOptions,
    ctx: &AdapterCaptureContext,
) -> Result<IngestOutcome, IngestError> {
    let cwd = options.cwd.clone().or_else(current_cwd_string);
    let project = options.project.clone().or_else(|| {
        env_nonempty("SOMA_PROJECT").or_else(|| {
            cwd.as_deref().and_then(|p| crate::project::name_from_path(Some(Path::new(p))))
        })
    });
    let session_id = options.session_id.clone().or_else(|| env_nonempty("SOMA_SESSION_ID"));
    let git_branch = options.git_branch.clone().or_else(|| current_git_branch(cwd.as_deref()));
    let defaults = AdapterCaptureDefaults { cwd, project, session_id, git_branch };
    let ingest_ctx = IngestContext { db_path: ctx.db_path.clone() };
    run_adapter_capture_json(raw, options.source.as_deref(), defaults, &ingest_ctx)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn read_json_arg(path: &str) -> Result<String, IngestError> {
    if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| IngestError::MalformedInput(format!("stdin read: {e}")))?;
        return Ok(buf);
    }
    std::fs::read_to_string(path)
        .map_err(|e| IngestError::MalformedInput(format!("read `{path}`: {e}")))
}

fn current_cwd_string() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.display().to_string())
}

fn current_git_branch(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!branch.is_empty()).then_some(branch)
}
