//! `soma adapter-lifecycle` - normalize client lifecycle events.
//!
//! Cursor, Continue, and other clients tend to expose lifecycle hooks with
//! app-specific field names. This command is a narrow adapter: it translates one
//! hook event into SOMA's existing adapter-spool `{kind,payload}` contract, then
//! optionally appends that event to the checkpointed JSONL spool. It does not
//! ingest directly and does not create a separate trust path.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cli::adapter_spool::{
    append_json_str, build_spool_event_value, AdapterSpoolAppendOutcome,
};
use crate::cli::{AdapterLifecycleArgs, AdapterSpoolAppendArgs};

pub const ADAPTER_LIFECYCLE_SOURCE: &str = "soma_adapter_lifecycle";
pub const ADAPTER_LIFECYCLE_CONTRACT: &str =
    "client_lifecycle_events_normalize_to_adapter_spool_without_direct_promotion";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterLifecycleOutcome {
    pub source: &'static str,
    pub contract: &'static str,
    pub client: String,
    pub lifecycle_event: String,
    pub normalized_kind: String,
    pub emitted_event: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append: Option<AdapterSpoolAppendOutcome>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterLifecycleError {
    Io(String),
    MalformedInput(String),
    Spool(crate::cli::adapter_spool::AdapterSpoolError),
}

impl AdapterLifecycleError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AdapterLifecycleError::MalformedInput(_) => 1,
            AdapterLifecycleError::Spool(err) => err.exit_code(),
            AdapterLifecycleError::Io(_) => 3,
        }
    }
}

impl std::fmt::Display for AdapterLifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterLifecycleError::Io(message) => write!(f, "io: {message}"),
            AdapterLifecycleError::MalformedInput(message) => {
                write!(f, "malformed input: {message}")
            }
            AdapterLifecycleError::Spool(err) => write!(f, "adapter spool: {err}"),
        }
    }
}

impl std::error::Error for AdapterLifecycleError {}

impl From<crate::cli::adapter_spool::AdapterSpoolError> for AdapterLifecycleError {
    fn from(value: crate::cli::adapter_spool::AdapterSpoolError) -> Self {
        AdapterLifecycleError::Spool(value)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AdapterLifecyclePayload {
    pub client: Option<String>,
    pub event: Option<String>,
    pub lifecycle_event: Option<String>,
    pub kind: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub source: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub prompt_text: Option<String>,
    pub response_text: Option<String>,
    pub output_text: Option<String>,
    pub task_frame_id: Option<i64>,
    pub task_frame_query: Option<String>,
}

pub fn run_blocking(
    args: &AdapterLifecycleArgs,
) -> Result<AdapterLifecycleOutcome, AdapterLifecycleError> {
    let raw = read_json_arg(&args.json)?;
    run_json_str(&raw, args)
}

pub fn run_json_str(
    raw: &str,
    args: &AdapterLifecycleArgs,
) -> Result<AdapterLifecycleOutcome, AdapterLifecycleError> {
    validate_output_format(&args.format)?;
    let payload = serde_json::from_str::<AdapterLifecyclePayload>(raw)
        .map_err(|e| AdapterLifecycleError::MalformedInput(format!("JSON parse: {e}")))?;
    let raw_value = serde_json::from_str::<Value>(raw)
        .map_err(|e| AdapterLifecycleError::MalformedInput(format!("JSON parse: {e}")))?;
    let raw_map = payload_object(raw_value)?;

    let env_client = env_nonempty("SOMA_CLIENT");
    let env_lifecycle_client = env_nonempty("SOMA_ADAPTER_LIFECYCLE_CLIENT");
    let env_hook_adapter = env_nonempty("SOMA_ADAPTER_LIFECYCLE_HOOK_ADAPTER");
    let client = first_nonempty([
        args.client.as_deref(),
        args.source.as_deref(),
        env_lifecycle_client.as_deref(),
        env_client.as_deref(),
        payload.client.as_deref(),
        payload.source.as_deref(),
    ])
    .unwrap_or("adapter-lifecycle")
    .to_string();
    let lifecycle_event = first_nonempty([
        args.event.as_deref(),
        payload.event.as_deref(),
        payload.lifecycle_event.as_deref(),
        payload.kind.as_deref(),
        payload.event_type.as_deref(),
    ])
    .map(str::to_string)
    .unwrap_or_else(|| infer_event_name(&payload).to_string());
    let normalized_kind = normalize_lifecycle_kind(&lifecycle_event, &payload)?;
    let normalized_payload = normalize_payload(
        raw_map,
        &payload,
        &client,
        &lifecycle_event,
        &normalized_kind,
        args.event_source.as_deref(),
        args.binding_nonce.as_deref(),
        args.hook_adapter.as_deref().or(env_hook_adapter.as_deref()),
    );
    let payload_json = serde_json::to_string(&Value::Object(normalized_payload))
        .map_err(|e| AdapterLifecycleError::MalformedInput(format!("payload encode: {e}")))?;
    let spool_args = AdapterSpoolAppendArgs {
        jsonl: args.jsonl.clone().unwrap_or_default(),
        kind: normalized_kind.clone(),
        json: "-".to_string(),
        source: args.source.clone().or_else(|| Some(client.clone())),
        cwd: args.cwd.clone().or_else(|| env_nonempty("SOMA_ADAPTER_LIFECYCLE_CWD")),
        project: args
            .project
            .clone()
            .or_else(|| env_nonempty("SOMA_ADAPTER_LIFECYCLE_PROJECT"))
            .or_else(|| env_nonempty("SOMA_PROJECT")),
        session_id: args
            .session_id
            .clone()
            .or_else(|| env_nonempty("SOMA_ADAPTER_LIFECYCLE_SESSION"))
            .or_else(|| env_nonempty("SOMA_SESSION_ID")),
        git_branch: args.git_branch.clone(),
        client: args
            .client
            .clone()
            .or(env_lifecycle_client)
            .or(env_client)
            .or_else(|| Some(client.clone())),
        binding_nonce: args.binding_nonce.clone(),
        fsync: args.fsync,
    };
    let emitted_event = build_spool_event_value(&payload_json, &spool_args)?;
    let append = match args.jsonl.as_deref().filter(|path| !path.trim().is_empty()) {
        Some(_) => Some(append_json_str(&payload_json, &spool_args)?),
        None => None,
    };

    Ok(AdapterLifecycleOutcome {
        source: ADAPTER_LIFECYCLE_SOURCE,
        contract: ADAPTER_LIFECYCLE_CONTRACT,
        client,
        lifecycle_event,
        normalized_kind,
        emitted_event,
        append,
    })
}

fn normalize_payload(
    mut raw_map: Map<String, Value>,
    payload: &AdapterLifecyclePayload,
    client: &str,
    lifecycle_event: &str,
    normalized_kind: &str,
    event_source: Option<&str>,
    binding_nonce: Option<&str>,
    hook_adapter: Option<&str>,
) -> Map<String, Value> {
    for lifecycle_key in ["event", "lifecycle_event", "kind", "type"] {
        raw_map.remove(lifecycle_key);
    }
    insert_string_if_missing(&mut raw_map, "client", Some(client));
    insert_string_if_missing(&mut raw_map, "session_id", payload.thread_id.as_deref());
    insert_string_if_missing(&mut raw_map, "lifecycle_event", Some(lifecycle_event));
    insert_string_if_missing(&mut raw_map, "event_source", event_source);
    insert_string_if_missing(&mut raw_map, "binding_nonce", binding_nonce);
    insert_string_if_missing(&mut raw_map, "hook_adapter", hook_adapter);

    match normalized_kind {
        "turn" => {
            insert_string_if_missing(&mut raw_map, "source", Some(client));
            insert_string_if_missing(&mut raw_map, "response_text", payload.output_text.as_deref());
        }
        "cloud_output" => {
            insert_string_if_missing(&mut raw_map, "client", Some(client));
            insert_string_if_missing(&mut raw_map, "output_text", payload.response_text.as_deref());
            if payload.task_frame_id.is_none() {
                insert_string_if_missing(
                    &mut raw_map,
                    "task_frame_query",
                    payload.prompt_text.as_deref(),
                );
            }
        }
        _ => {}
    }

    raw_map
}

fn normalize_lifecycle_kind(
    lifecycle_event: &str,
    payload: &AdapterLifecyclePayload,
) -> Result<String, AdapterLifecycleError> {
    let normalized = normalize_token(lifecycle_event);
    match normalized.as_str() {
        "turn"
        | "capture_turn"
        | "adapter_capture"
        | "turn_completed"
        | "conversation_turn"
        | "message_completed"
        | "user_assistant_turn" => Ok("turn".to_string()),
        "cloud_output"
        | "adapter_cloud_output"
        | "assistant_output"
        | "assistant_response"
        | "cloud_response"
        | "model_output"
        | "completion" => Ok("cloud_output".to_string()),
        "" | "auto" | "unknown" => infer_kind(payload),
        other => Err(AdapterLifecycleError::MalformedInput(format!(
            "unknown lifecycle event `{other}`; expected turn/turn_completed or cloud_output/assistant_response"
        ))),
    }
}

fn infer_kind(payload: &AdapterLifecyclePayload) -> Result<String, AdapterLifecycleError> {
    if has_text(payload.output_text.as_deref())
        || (has_text(payload.response_text.as_deref())
            && (payload.task_frame_id.is_some() || has_text(payload.task_frame_query.as_deref())))
    {
        return Ok("cloud_output".to_string());
    }
    if has_text(payload.prompt_text.as_deref()) || has_text(payload.response_text.as_deref()) {
        return Ok("turn".to_string());
    }
    Err(AdapterLifecycleError::MalformedInput(
        "cannot infer lifecycle kind; provide event=turn_completed or event=assistant_response"
            .to_string(),
    ))
}

fn infer_event_name(payload: &AdapterLifecyclePayload) -> &'static str {
    if has_text(payload.output_text.as_deref()) {
        "assistant_response"
    } else {
        "turn_completed"
    }
}

fn normalize_token(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn payload_object(payload: Value) -> Result<Map<String, Value>, AdapterLifecycleError> {
    match payload {
        Value::Object(map) => Ok(map),
        _ => Err(AdapterLifecycleError::MalformedInput(
            "lifecycle payload must be a JSON object".to_string(),
        )),
    }
}

fn read_json_arg(path: &str) -> Result<String, AdapterLifecycleError> {
    if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| AdapterLifecycleError::Io(format!("read stdin: {e}")))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| AdapterLifecycleError::Io(format!("read `{path}`: {e}")))
    }
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values.into_iter().flatten().map(str::trim).find(|value| !value.is_empty())
}

fn has_text(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|value| !value.is_empty())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn insert_string_if_missing(payload: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if payload.contains_key(key) {
        return;
    }
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn validate_output_format(format: &str) -> Result<(), AdapterLifecycleError> {
    match format.trim().to_ascii_lowercase().as_str() {
        "event" | "report" => Ok(()),
        other => Err(AdapterLifecycleError::MalformedInput(format!(
            "unknown format `{other}`; expected event or report"
        ))),
    }
}
