//! `soma adapter-cloud-output` - hook-friendly cloud-output capture.
//!
//! Editor hooks and lightweight watcher scripts should not have to hand-roll
//! MCP JSON-RPC just to record a cloud model response. This command accepts the
//! same semantic payload as `soma_capture_cloud_output`, then reuses the
//! storage-layer critic and proposal gates so cloud text remains `cloud_draft`
//! until verified.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::AdapterCloudOutputArgs;
use crate::context::cloud_prompt::{
    cloud_context_protocol, CloudContextProtocol, CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY,
};
use crate::context::compiler::{
    load_local_compiler_config_from_home, resolve_local_compiler_runtime,
};
use crate::context::critic::{
    capture_cloud_output_claims, learning_critic_proposal_from_capture, select_cloud_output_claims,
    ClaimExtractionSource, CloudOutputCaptureInput, ControlCriticDecision, ControlCriticResult,
    ExtractedCloudClaim, LocalClaimExtractorRuntime, VerificationRequest,
};
use crate::context::task_frame::{build_task_frame, TaskFrameBuildInput};
use crate::memory::local_llm::LocalLlmError;
use crate::storage::{
    LearningCriticAction, LifecycleState, Storage, StorageError, StoredEvidenceRef,
};

#[derive(Debug, Clone)]
pub struct AdapterCloudOutputContext {
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterCloudOutputOutcome {
    pub task_frame_id: i64,
    pub handoff_id: Option<String>,
    pub idempotency_key: String,
    pub replayed: bool,
    pub decision: ControlCriticDecision,
    pub claim_ids: Vec<i64>,
    pub verification_event_ids: Vec<i64>,
    pub verification_requests: Vec<VerificationRequest>,
    pub required_edits: Vec<String>,
    pub proposal_id: Option<i64>,
    pub claim_extraction: ClaimExtractionSource,
    pub protocol: CloudContextProtocol,
    pub trust_boundary: &'static str,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterCloudOutputError {
    MalformedInput(String),
    LocalClaimExtractor(LocalLlmError),
    Storage(StorageError),
}

impl AdapterCloudOutputError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AdapterCloudOutputError::MalformedInput(_) => 1,
            AdapterCloudOutputError::LocalClaimExtractor(_) => 3,
            AdapterCloudOutputError::Storage(_) => 2,
        }
    }
}

impl std::fmt::Display for AdapterCloudOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterCloudOutputError::MalformedInput(message) => {
                write!(f, "malformed input: {message}")
            }
            AdapterCloudOutputError::LocalClaimExtractor(err) => {
                write!(f, "local claim extractor: {err}")
            }
            AdapterCloudOutputError::Storage(err) => write!(f, "storage: {err}"),
        }
    }
}

impl std::error::Error for AdapterCloudOutputError {}

impl From<StorageError> for AdapterCloudOutputError {
    fn from(value: StorageError) -> Self {
        AdapterCloudOutputError::Storage(value)
    }
}

impl From<LocalLlmError> for AdapterCloudOutputError {
    fn from(value: LocalLlmError) -> Self {
        AdapterCloudOutputError::LocalClaimExtractor(value)
    }
}

#[derive(Debug, Deserialize)]
struct AdapterCloudOutputPayload {
    #[serde(default)]
    pub task_frame_id: Option<i64>,
    #[serde(default, alias = "query")]
    pub task_frame_query: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub handoff_id: Option<String>,
    #[serde(default, alias = "cloud_context_contract")]
    pub protocol_contract: Option<String>,
    #[serde(default, alias = "protocol_artifact_version")]
    pub artifact_version: Option<u32>,
    #[serde(default)]
    pub allow_local_private_projection: bool,
    #[serde(default)]
    pub local_private_projection_reason: Option<String>,
    pub output_text: String,
    #[serde(default = "default_decision")]
    pub decision: ControlCriticDecision,
    #[serde(default)]
    pub extracted_claims: Vec<ExtractedCloudClaim>,
    #[serde(default)]
    pub verification_requests: Vec<VerificationRequest>,
    #[serde(default)]
    pub required_edits: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<StoredEvidenceRef>,
    #[serde(default)]
    pub local_claim_extractor: bool,
    #[serde(default)]
    pub local_claim_extractor_endpoint: Option<String>,
    #[serde(default)]
    pub local_claim_extractor_model: Option<String>,
    #[serde(default = "default_enqueue_proposal")]
    pub enqueue_proposal: bool,
    #[serde(default = "default_proposal_action")]
    pub proposal_action: LearningCriticAction,
    #[serde(default)]
    pub proposal_target_lifecycle_state: Option<LifecycleState>,
    #[serde(default)]
    pub proposal_reason: Option<String>,
}

pub fn run_blocking(
    args: &AdapterCloudOutputArgs,
    ctx: &AdapterCloudOutputContext,
) -> Result<AdapterCloudOutputOutcome, AdapterCloudOutputError> {
    let raw = read_json_arg(&args.json)?;
    run_json_str(&raw, ctx)
}

pub fn run_json_str(
    raw: &str,
    ctx: &AdapterCloudOutputContext,
) -> Result<AdapterCloudOutputOutcome, AdapterCloudOutputError> {
    let payload = serde_json::from_str::<AdapterCloudOutputPayload>(raw)
        .map_err(|e| AdapterCloudOutputError::MalformedInput(format!("JSON parse: {e}")))?;
    let mut storage = Storage::open(&ctx.db_path)?;
    let task_frame_id = resolve_task_frame_id(&mut storage, &payload)?;
    let (extracted_claims, claim_extraction) = resolve_claim_extraction(&payload)?;
    let input = CloudOutputCaptureInput {
        output_text: payload.output_text,
        handoff_id: payload.handoff_id.clone(),
        protocol_contract: payload.protocol_contract.clone(),
        artifact_version: payload.artifact_version,
        critic: ControlCriticResult {
            task_frame_id,
            decision: payload.decision,
            extracted_claims,
            verification_requests: payload.verification_requests,
            required_edits: payload.required_edits,
            evidence_refs: payload.evidence_refs,
        },
    };

    let captured = capture_cloud_output_claims(&mut storage, &input)?;
    let proposal_id = if payload.enqueue_proposal {
        let draft = learning_critic_proposal_from_capture(
            &captured,
            payload.proposal_action,
            payload.proposal_target_lifecycle_state,
            payload.proposal_reason.unwrap_or_else(|| {
                "Cloud output captured by adapter; external verification required before durable promotion"
                    .to_string()
            }),
        );
        if let Some(existing) = storage.equivalent_learning_critic_proposal(&draft)? {
            Some(existing.id)
        } else {
            Some(storage.insert_learning_critic_proposal(&draft)?)
        }
    } else {
        None
    };

    Ok(AdapterCloudOutputOutcome {
        task_frame_id: captured.task_frame_id,
        handoff_id: captured.handoff_id,
        idempotency_key: captured.idempotency_key,
        replayed: captured.replayed,
        decision: captured.decision,
        claim_ids: captured.claim_ids,
        verification_event_ids: captured.verification_event_ids,
        verification_requests: captured.verification_requests,
        required_edits: captured.required_edits,
        proposal_id,
        claim_extraction,
        protocol: cloud_context_protocol(),
        trust_boundary: CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY,
    })
}

fn resolve_claim_extraction(
    payload: &AdapterCloudOutputPayload,
) -> Result<(Vec<ExtractedCloudClaim>, ClaimExtractionSource), AdapterCloudOutputError> {
    let runtime = if payload.local_claim_extractor {
        let local_config = load_local_compiler_config_from_home();
        let (endpoint, model) = resolve_local_compiler_runtime(
            payload.local_claim_extractor_endpoint.as_deref(),
            payload.local_claim_extractor_model.as_deref(),
            &local_config,
        );
        Some((endpoint, model))
    } else {
        None
    };
    let runtime_ref = runtime.as_ref().map(|(endpoint, model)| LocalClaimExtractorRuntime {
        endpoint: endpoint.as_str(),
        model: model.as_str(),
    });
    select_cloud_output_claims(&payload.output_text, payload.extracted_claims.clone(), runtime_ref)
        .map_err(Into::into)
}

fn resolve_task_frame_id(
    storage: &mut Storage,
    payload: &AdapterCloudOutputPayload,
) -> Result<i64, AdapterCloudOutputError> {
    if let Some(task_frame_id) = payload.task_frame_id {
        return Ok(task_frame_id);
    }

    let query = payload
        .task_frame_query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| {
            AdapterCloudOutputError::MalformedInput(
                "`task_frame_id` or non-empty `task_frame_query` is required".to_string(),
            )
        })?;
    let cwd = payload
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string()));
    let draft = build_task_frame(
        storage,
        TaskFrameBuildInput {
            query: Some(query.to_string()),
            project: payload.project.clone().or_else(crate::project::current_name),
            session_id: payload.session_id.clone(),
            cwd,
            client: payload.client.clone().or_else(|| Some("adapter-cloud-output".to_string())),
            allow_local_private_projection: payload.allow_local_private_projection,
            local_private_projection_reason: payload.local_private_projection_reason.clone(),
        },
    )?;
    storage.insert_task_frame(&draft).map_err(Into::into)
}

fn read_json_arg(path: &str) -> Result<String, AdapterCloudOutputError> {
    if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| AdapterCloudOutputError::MalformedInput(format!("stdin read: {e}")))?;
        return Ok(buf);
    }
    std::fs::read_to_string(path)
        .map_err(|e| AdapterCloudOutputError::MalformedInput(format!("read `{path}`: {e}")))
}

fn default_decision() -> ControlCriticDecision {
    ControlCriticDecision::Accept
}

fn default_enqueue_proposal() -> bool {
    true
}

fn default_proposal_action() -> LearningCriticAction {
    LearningCriticAction::RequestVerification
}
