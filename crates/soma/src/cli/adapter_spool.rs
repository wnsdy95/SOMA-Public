//! `soma adapter-spool` - checkpointed JSONL watcher/drain contract.
//!
//! Cursor, Continue, or a lightweight wrapper can append normalized events to a
//! JSONL spool without learning SOMA's DB or MCP internals. This command drains
//! new lines since the last checkpoint and forwards each event to the stable
//! adapter capture surfaces.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::capture::ai_cli::{IngestError, IngestOutcome};
use crate::cli::adapter_capture::{
    run_json_str as capture_turn_json, AdapterCaptureContext, AdapterCaptureRunOptions,
};
use crate::cli::adapter_cloud_output::{
    run_json_str as capture_cloud_output_json, AdapterCloudOutputContext, AdapterCloudOutputError,
};
use crate::cli::{AdapterSpoolAppendArgs, AdapterSpoolArgs};

#[derive(Debug, Clone)]
pub struct AdapterSpoolContext {
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterSpoolOutcome {
    pub spool_path: PathBuf,
    pub checkpoint_path: PathBuf,
    pub start_offset: u64,
    pub end_offset: u64,
    pub scanned_lines: usize,
    pub captured_turns: usize,
    pub captured_cloud_outputs: usize,
    pub skipped_empty_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterSpoolAppendOutcome {
    pub spool_path: PathBuf,
    pub kind: String,
    pub appended_bytes: usize,
    pub end_offset: u64,
    pub fsync: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterSpoolError {
    Io(String),
    MalformedInput(String),
    Ingest(IngestError),
    CloudOutput(AdapterCloudOutputError),
}

impl AdapterSpoolError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AdapterSpoolError::MalformedInput(_) => 1,
            AdapterSpoolError::Ingest(_) | AdapterSpoolError::CloudOutput(_) => 2,
            AdapterSpoolError::Io(_) => 3,
        }
    }
}

impl std::fmt::Display for AdapterSpoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterSpoolError::Io(message) => write!(f, "io: {message}"),
            AdapterSpoolError::MalformedInput(message) => write!(f, "malformed input: {message}"),
            AdapterSpoolError::Ingest(err) => write!(f, "adapter capture: {err}"),
            AdapterSpoolError::CloudOutput(err) => write!(f, "adapter cloud output: {err}"),
        }
    }
}

impl std::error::Error for AdapterSpoolError {}

impl From<IngestError> for AdapterSpoolError {
    fn from(value: IngestError) -> Self {
        AdapterSpoolError::Ingest(value)
    }
}

impl From<AdapterCloudOutputError> for AdapterSpoolError {
    fn from(value: AdapterCloudOutputError) -> Self {
        AdapterSpoolError::CloudOutput(value)
    }
}

#[derive(Debug, Deserialize)]
struct AdapterSpoolEvent {
    kind: String,
    payload: Value,
}

pub fn run_append_blocking(
    args: &AdapterSpoolAppendArgs,
) -> Result<AdapterSpoolAppendOutcome, AdapterSpoolError> {
    let raw = read_json_arg(&args.json)?;
    append_json_str(&raw, args)
}

pub fn append_json_str(
    raw: &str,
    args: &AdapterSpoolAppendArgs,
) -> Result<AdapterSpoolAppendOutcome, AdapterSpoolError> {
    let spool_path = PathBuf::from(&args.jsonl);
    let event = build_spool_event_value(raw, args)?;
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("unknown").to_string();
    let mut line = serde_json::to_vec(&event)
        .map_err(|e| AdapterSpoolError::MalformedInput(format!("event encode: {e}")))?;
    line.push(b'\n');
    if let Some(parent) = spool_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AdapterSpoolError::Io(format!("create spool dir `{}`: {e}", parent.display()))
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spool_path)
        .map_err(|e| AdapterSpoolError::Io(format!("open `{}`: {e}", spool_path.display())))?;
    file.write_all(&line)
        .map_err(|e| AdapterSpoolError::Io(format!("append `{}`: {e}", spool_path.display())))?;
    if args.fsync {
        file.sync_all()
            .map_err(|e| AdapterSpoolError::Io(format!("fsync `{}`: {e}", spool_path.display())))?;
    }
    let end_offset = file
        .metadata()
        .map_err(|e| AdapterSpoolError::Io(format!("metadata `{}`: {e}", spool_path.display())))?
        .len();
    Ok(AdapterSpoolAppendOutcome {
        spool_path,
        kind,
        appended_bytes: line.len(),
        end_offset,
        fsync: args.fsync,
    })
}

pub fn build_spool_event_value(
    raw: &str,
    args: &AdapterSpoolAppendArgs,
) -> Result<Value, AdapterSpoolError> {
    let kind = canonical_kind(&args.kind)?;
    let payload = serde_json::from_str::<Value>(raw)
        .map_err(|e| AdapterSpoolError::MalformedInput(format!("payload JSON parse: {e}")))?;
    let mut payload = payload_object(payload)?;
    apply_append_defaults(&kind, &mut payload, args);
    validate_payload_for_kind(&kind, &payload)?;
    Ok(serde_json::json!({
        "schema": "soma.adapter_spool_event.v1",
        "kind": kind,
        "writer_contract": "soma_adapter_spool_append_v1",
        "observed_at_ns": now_ns(),
        "payload": Value::Object(payload),
    }))
}

pub fn run_blocking(
    args: &AdapterSpoolArgs,
    ctx: &AdapterSpoolContext,
) -> Result<AdapterSpoolOutcome, AdapterSpoolError> {
    drain_once(args, ctx)
}

fn drain_once(
    args: &AdapterSpoolArgs,
    ctx: &AdapterSpoolContext,
) -> Result<AdapterSpoolOutcome, AdapterSpoolError> {
    let spool_path = PathBuf::from(&args.jsonl);
    let checkpoint_path = args
        .checkpoint
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_checkpoint_path(&spool_path));
    let start_offset = read_checkpoint(&checkpoint_path)?;
    let file = File::open(&spool_path)
        .map_err(|e| AdapterSpoolError::Io(format!("open `{}`: {e}", spool_path.display())))?;
    let file_len = file
        .metadata()
        .map_err(|e| AdapterSpoolError::Io(format!("metadata `{}`: {e}", spool_path.display())))?
        .len();
    let start_offset = start_offset.min(file_len);
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(start_offset))
        .map_err(|e| AdapterSpoolError::Io(format!("seek `{}`: {e}", spool_path.display())))?;

    let mut outcome = AdapterSpoolOutcome {
        spool_path,
        checkpoint_path,
        start_offset,
        end_offset: start_offset,
        scanned_lines: 0,
        captured_turns: 0,
        captured_cloud_outputs: 0,
        skipped_empty_lines: 0,
    };
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|e| {
            AdapterSpoolError::Io(format!("read `{}`: {e}", outcome.spool_path.display()))
        })?;
        if read == 0 {
            break;
        }
        outcome.end_offset = outcome.end_offset.saturating_add(read as u64);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            outcome.skipped_empty_lines += 1;
            continue;
        }
        outcome.scanned_lines += 1;
        process_line(trimmed, args, ctx, &mut outcome)?;
    }
    write_checkpoint(&outcome.checkpoint_path, outcome.end_offset)?;
    Ok(outcome)
}

fn process_line(
    line: &str,
    args: &AdapterSpoolArgs,
    ctx: &AdapterSpoolContext,
    outcome: &mut AdapterSpoolOutcome,
) -> Result<(), AdapterSpoolError> {
    let event = serde_json::from_str::<AdapterSpoolEvent>(line)
        .map_err(|e| AdapterSpoolError::MalformedInput(format!("JSONL event parse: {e}")))?;
    let payload = serde_json::to_string(&event.payload)
        .map_err(|e| AdapterSpoolError::MalformedInput(format!("payload encode: {e}")))?;
    match normalized_kind(&event.kind).as_str() {
        "turn" | "capture_turn" | "adapter_capture" => {
            let capture_ctx = AdapterCaptureContext { db_path: ctx.db_path.clone() };
            let capture = capture_turn_json(
                &payload,
                AdapterCaptureRunOptions {
                    source: args.source.clone(),
                    cwd: args.cwd.clone(),
                    project: args.project.clone(),
                    session_id: args.session_id.clone().or_else(|| env_nonempty("SOMA_SESSION_ID")),
                    git_branch: args.git_branch.clone(),
                },
                &capture_ctx,
            )?;
            match capture {
                IngestOutcome::Stored { .. } => outcome.captured_turns += 1,
            }
        }
        "cloud_output" | "adapter_cloud_output" => {
            let cloud_ctx = AdapterCloudOutputContext { db_path: ctx.db_path.clone() };
            capture_cloud_output_json(&payload, &cloud_ctx)?;
            outcome.captured_cloud_outputs += 1;
        }
        other => {
            return Err(AdapterSpoolError::MalformedInput(format!(
                "unknown spool event kind `{other}`; expected turn or cloud_output"
            )));
        }
    }
    Ok(())
}

fn default_checkpoint_path(spool_path: &Path) -> PathBuf {
    let mut checkpoint = spool_path.to_path_buf();
    checkpoint.set_extension("offset");
    checkpoint
}

fn read_checkpoint(path: &Path) -> Result<u64, AdapterSpoolError> {
    match fs::read_to_string(path) {
        Ok(raw) => raw.trim().parse::<u64>().map_err(|e| {
            AdapterSpoolError::MalformedInput(format!(
                "checkpoint `{}` is not a byte offset: {e}",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(AdapterSpoolError::Io(format!("read checkpoint `{}`: {e}", path.display()))),
    }
}

fn write_checkpoint(path: &Path, offset: u64) -> Result<(), AdapterSpoolError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AdapterSpoolError::Io(format!("create checkpoint dir `{}`: {e}", parent.display()))
        })?;
    }
    fs::write(path, format!("{offset}\n"))
        .map_err(|e| AdapterSpoolError::Io(format!("write checkpoint `{}`: {e}", path.display())))
}

fn normalized_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace('-', "_")
}

fn canonical_kind(kind: &str) -> Result<String, AdapterSpoolError> {
    match normalized_kind(kind).as_str() {
        "turn" | "capture_turn" | "adapter_capture" => Ok("turn".to_string()),
        "cloud_output" | "adapter_cloud_output" => Ok("cloud_output".to_string()),
        other => Err(AdapterSpoolError::MalformedInput(format!(
            "unknown spool event kind `{other}`; expected turn or cloud_output"
        ))),
    }
}

fn read_json_arg(path: &str) -> Result<String, AdapterSpoolError> {
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| AdapterSpoolError::Io(format!("read stdin: {e}")))?;
        Ok(buf)
    } else {
        fs::read_to_string(path).map_err(|e| AdapterSpoolError::Io(format!("read `{path}`: {e}")))
    }
}

fn payload_object(payload: Value) -> Result<Map<String, Value>, AdapterSpoolError> {
    match payload {
        Value::Object(map) => Ok(map),
        _ => Err(AdapterSpoolError::MalformedInput("payload must be a JSON object".to_string())),
    }
}

fn apply_append_defaults(
    kind: &str,
    payload: &mut Map<String, Value>,
    args: &AdapterSpoolAppendArgs,
) {
    let project = args.project.clone().or_else(|| env_nonempty("SOMA_PROJECT"));
    let session_id = args.session_id.clone().or_else(|| env_nonempty("SOMA_SESSION_ID"));
    let client = args.client.clone().or_else(|| env_nonempty("SOMA_CLIENT"));
    let source = args.source.clone().or_else(|| env_nonempty("SOMA_CLIENT"));
    insert_string_if_missing(payload, "project", project.as_deref());
    insert_string_if_missing(payload, "session_id", session_id.as_deref());
    insert_string_if_missing(payload, "cwd", args.cwd.as_deref());
    insert_string_if_missing(payload, "binding_nonce", args.binding_nonce.as_deref());
    match kind {
        "turn" => {
            insert_string_if_missing(payload, "source", source.as_deref());
            insert_string_if_missing(payload, "git_branch", args.git_branch.as_deref());
        }
        "cloud_output" => {
            insert_string_if_missing(payload, "client", client.as_deref());
        }
        _ => {}
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn insert_string_if_missing(payload: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if !payload.contains_key(key) {
        if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
            payload.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
}

fn validate_payload_for_kind(
    kind: &str,
    payload: &Map<String, Value>,
) -> Result<(), AdapterSpoolError> {
    match kind {
        "turn" => {
            require_nonempty_string(payload, "source")?;
            if !has_nonempty_string(payload, "prompt_text")
                && !has_nonempty_string(payload, "response_text")
            {
                return Err(AdapterSpoolError::MalformedInput(
                    "turn payload requires prompt_text or response_text".to_string(),
                ));
            }
        }
        "cloud_output" => {
            require_nonempty_string(payload, "output_text")?;
            if !payload.contains_key("task_frame_id")
                && !has_nonempty_string(payload, "task_frame_query")
            {
                return Err(AdapterSpoolError::MalformedInput(
                    "cloud_output payload requires task_frame_id or task_frame_query".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn require_nonempty_string(
    payload: &Map<String, Value>,
    key: &str,
) -> Result<(), AdapterSpoolError> {
    if has_nonempty_string(payload, key) {
        Ok(())
    } else {
        Err(AdapterSpoolError::MalformedInput(format!(
            "payload field `{key}` must be a non-empty string"
        )))
    }
}

fn has_nonempty_string(payload: &Map<String, Value>, key: &str) -> bool {
    payload.get(key).and_then(Value::as_str).map(|v| !v.trim().is_empty()).unwrap_or(false)
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_checkpoint_sits_next_to_jsonl_with_offset_extension() {
        let checkpoint = default_checkpoint_path(Path::new("/tmp/soma/adapter/events.jsonl"));
        assert_eq!(checkpoint, PathBuf::from("/tmp/soma/adapter/events.offset"));
    }
}
