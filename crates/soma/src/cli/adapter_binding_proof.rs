//! `soma adapter-binding-proof` - record observed client binding evidence.
//!
//! This command intentionally separates a checked-in reference contract from
//! evidence that a private editor app actually called SOMA's wrapper.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::cli::AdapterBindingProofArgs;
use crate::context::eval::{
    product_hardening_external_action_safety, ProductHardeningExternalActionSafety,
};
use crate::storage::{
    ClientBindingProofDraft, ClientBindingProofLevel, Storage, StorageError,
    StoredClientBindingProof,
};

const IN_CLIENT_RENDER_EVIDENCE_SCHEMA: &str = "soma.in_client_render_evidence.v1";
const IN_CLIENT_RENDER_EVIDENCE_TRUST_BOUNDARY: &str =
    "observed_in_client_render_is_ui_only_and_never_verifies_promotes_applies_or_acknowledges";
const REVIEW_ACTION_TRUST_BOUNDARY: &str =
    "review_action_uses_verification_storage_gates_and_required_current_control_binding";
const OBSERVED_APP_HOOK_ALLOWED_CLOCK_SKEW_NS: i64 = 1_000_000_000;
const CONTINUE_DEVDATA_COLLECTOR_HOST: &str = "127.0.0.1";
const CONTINUE_DEVDATA_COLLECTOR_PORT: u16 = 8766;
const CONTINUE_DEVDATA_COLLECTOR_PATH: &str = "/continue-devdata";

#[derive(Debug, Clone)]
pub struct AdapterBindingProofContext {
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingProofOutcome {
    pub proof_id: i64,
    pub client: String,
    pub proof_level: ClientBindingProofLevel,
    pub manifest_status: String,
    pub evidence_source: String,
    pub trust_boundary: String,
    pub checks: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingProofListOutcome {
    pub client: Option<String>,
    pub limit: usize,
    pub proofs: Vec<StoredClientBindingProof>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingProofStatusOutcome {
    pub client: Option<String>,
    pub proof_id: Option<i64>,
    pub limit: usize,
    pub proofs_found: usize,
    pub client_count: usize,
    pub all_latest_artifacts_verified: bool,
    pub trust_boundary: String,
    pub clients: Vec<ClientBindingReadinessStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientBindingReadinessStatus {
    pub client: String,
    pub proof_stage: String,
    pub readiness: String,
    pub ready_for_private_client_claim: bool,
    pub has_reference_binding: bool,
    pub has_observed_event_file: bool,
    pub has_observed_app_hook: bool,
    pub has_observed_in_client_render: bool,
    pub has_observed_review_action: bool,
    pub ready_for_client_operator_loop: bool,
    pub latest_proof_id: Option<i64>,
    pub latest_proof_level: Option<ClientBindingProofLevel>,
    pub latest_observed_at_ns: Option<i64>,
    pub latest_by_level: BTreeMap<String, ClientBindingLatestProofStatus>,
    pub all_latest_artifacts_verified: bool,
    pub artifact_failures: Vec<ClientBindingArtifactFailure>,
    pub coherence_failures: Vec<String>,
    pub non_release_evidence_sources: Vec<ClientBindingNonReleaseEvidenceSource>,
    pub next_steps: Vec<String>,
    pub operator_flow: Vec<ClientBindingOperatorFlowStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingLatestProofStatus {
    pub proof_id: i64,
    pub proof_level: ClientBindingProofLevel,
    pub observed_at_ns: i64,
    pub manifest_status: String,
    pub evidence_source: String,
    pub operator_confirmed_release_grade_evidence: bool,
    pub installed_config_path: Option<String>,
    pub installed_config_fingerprint: Option<String>,
    pub installed_config_binding_nonce: Option<String>,
    pub review_action_control_id: Option<String>,
    pub all_artifacts_verified: bool,
    pub artifact_checks: Vec<EvidenceArtifactCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBindingNonReleaseEvidenceSource {
    pub proof_id: i64,
    pub proof_level: ClientBindingProofLevel,
    pub evidence_source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingArtifactFailure {
    pub proof_id: i64,
    pub proof_level: ClientBindingProofLevel,
    pub kind: String,
    pub path: Option<String>,
    pub status: EvidenceArtifactStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingInstalledConfigCheckOutcome {
    pub client: String,
    pub installed_config_path: String,
    pub proof_level: ClientBindingProofLevel,
    pub eligible_for_observed_app_hook: bool,
    pub missing_requirements: Vec<&'static str>,
    pub trust_boundary: String,
    pub checks: InstalledConfigScan,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingInstalledConfigDiscoveryOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub expected_event_source: String,
    pub config_root: String,
    pub known_private_client_target_relpaths: Vec<String>,
    pub candidates_found: usize,
    pub eligible_candidates: usize,
    pub setup_artifact_eligible_candidates: usize,
    pub private_client_target_eligible_candidates: usize,
    pub eligible_setup_artifact_paths: Vec<String>,
    pub eligible_private_client_target_paths: Vec<String>,
    pub private_client_target_candidate_paths: Vec<String>,
    pub candidates: Vec<InstalledConfigCandidate>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstalledConfigCandidate {
    pub path: String,
    pub exists: bool,
    pub eligible_for_observed_app_hook: bool,
    pub missing_requirements: Vec<&'static str>,
    pub checks: Option<InstalledConfigScan>,
    pub error: Option<String>,
    pub next_commands: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingRealAppProofKitOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub expected_event_source: String,
    pub artifacts: Vec<RealAppProofArtifact>,
    pub proof_readiness: Vec<RealAppProofReadiness>,
    pub commands: Vec<Vec<String>>,
    pub acceptance_gates: Vec<RealAppProofAcceptanceGate>,
    pub unproven_until: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingEvidenceBundleOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub expected_event_source: String,
    pub binding_nonce: String,
    pub generated_binding_nonce: bool,
    pub config_root: String,
    pub event_jsonl_path: Option<String>,
    pub continue_devdata_collector: Option<ContinueDevdataCollectorProbe>,
    pub readiness: AdapterBindingProofStatusOutcome,
    pub installed_config_discovery: AdapterBindingInstalledConfigDiscoveryOutcome,
    pub installed_config_preview: AdapterBindingInstalledConfigRenderOutcome,
    pub real_app_proof_kit: AdapterBindingRealAppProofKitOutcome,
    pub operator_flow: Vec<ClientBindingOperatorFlowStep>,
    pub proof_session: ClientBindingProofSession,
    pub blocking_gaps: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingProofSessionOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub expected_event_source: String,
    pub binding_nonce: String,
    pub generated_binding_nonce: bool,
    pub config_root: String,
    pub proof_storage_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_storage_error: Option<String>,
    pub proof_storage_recovery_commands: Vec<Vec<String>>,
    pub status: String,
    pub release_gate: String,
    pub ready_for_private_client_claim: bool,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub proof_session_next_step_id: Option<String>,
    pub next_operator_step_title: Option<String>,
    pub artifact_failure_count: usize,
    pub artifact_failures: Vec<ClientBindingArtifactFailure>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub app_hook_evidence: ClientBindingAppHookEvidenceSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub operator_card: ClientBindingProofSessionOperatorCard,
    pub next_step_id: Option<String>,
    pub next_command: Option<Vec<String>>,
    pub next_mcp_call: Option<ClientBindingMcpCallTemplate>,
    pub next_operator_step: Option<ClientBindingOperatorFlowStep>,
    pub runbook_schema: String,
    pub pending_proof_levels: Vec<ClientBindingProofLevel>,
    pub blocked_proof_levels: Vec<ClientBindingProofLevel>,
    pub proof_session: ClientBindingProofSession,
    pub blocking_gaps: Vec<String>,
    pub operator_flow: Vec<ClientBindingOperatorFlowStep>,
    pub proofs_found: usize,
    pub ready_client_count: usize,
    pub installed_config_eligible_candidates: usize,
    pub setup_artifact_eligible_candidates: usize,
    pub private_client_target_eligible_candidates: usize,
    pub eligible_setup_artifact_paths: Vec<String>,
    pub eligible_private_client_target_paths: Vec<String>,
    pub private_client_target_candidate_paths: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofSessionOperatorCard {
    pub source: &'static str,
    pub client: String,
    pub status: String,
    pub release_gate: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub proof_session_next_step_id: Option<String>,
    pub next_operator_step_title: Option<String>,
    pub ready_for_private_client_claim: bool,
    pub pending_proof_level_count: usize,
    pub blocked_proof_level_count: usize,
    pub blocking_gap_count: usize,
    pub artifact_failure_count: usize,
    pub artifact_failures: Vec<ClientBindingArtifactFailure>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub app_hook_evidence: ClientBindingAppHookEvidenceSummary,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingAppHookEvidenceSummary {
    pub source: &'static str,
    pub client: String,
    pub event_jsonl_path: Option<String>,
    pub expected_event_source: String,
    pub binding_nonce: String,
    pub status: String,
    pub ready_to_record_now: bool,
    pub blocking_reason_count: usize,
    pub blocking_reasons: Vec<String>,
    pub readiness_probe_command: Option<Vec<String>>,
    pub record_proof_command: Option<Vec<String>>,
    pub continue_devdata_collector: Option<ContinueDevdataCollectorProbe>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientBindingExternalOperatorAction {
    pub source: &'static str,
    pub client: String,
    pub action_id: String,
    pub action_label: String,
    pub proof_session_step_id: String,
    pub action_kind: String,
    pub required_operator_action: String,
    pub requires_operator_confirmation_before_submission: bool,
    pub may_transmit_prompt_to_provider: bool,
    pub suggested_minimal_test_prompt: String,
    pub forbidden_inputs: Vec<String>,
    pub readiness_probe_command: Option<Vec<String>>,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub proof_after_success_step_id: String,
    pub required_observation: String,
    pub why_next_mcp_call_is_null: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofSession {
    pub client: String,
    pub status: String,
    pub release_gate: String,
    pub ready_for_private_client_claim: bool,
    pub completed_proof_levels: Vec<ClientBindingProofLevel>,
    pub pending_proof_levels: Vec<ClientBindingProofLevel>,
    pub ready_to_record_proof_levels: Vec<ClientBindingProofLevel>,
    pub blocked_proof_levels: Vec<ClientBindingProofLevel>,
    pub next_step_id: Option<String>,
    pub next_command: Option<Vec<String>>,
    pub next_mcp_call: Option<ClientBindingMcpCallTemplate>,
    pub next_operator_step: Option<ClientBindingOperatorFlowStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub stages: Vec<ClientBindingProofSessionStage>,
    pub runbook: ClientBindingProofSessionRunbook,
    pub completion_criteria: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofSessionStage {
    pub proof_level: ClientBindingProofLevel,
    pub ledger_status: String,
    pub artifact_status: Option<String>,
    pub ready_to_record_now: bool,
    pub blocking_reasons: Vec<String>,
    pub command: Option<Vec<String>>,
    pub mcp_call: Option<ClientBindingMcpCallTemplate>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofSessionRunbook {
    pub schema: String,
    pub client: String,
    pub status: String,
    pub release_gate: String,
    pub durable_artifact_dir: String,
    pub durable_artifact_dir_write_status: String,
    pub suggested_artifact_paths: Vec<ClientBindingProofArtifactPathSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_fallback_artifact_dir: Option<String>,
    pub workspace_fallback_artifact_paths: Vec<ClientBindingProofArtifactPathSuggestion>,
    pub workspace_fallback_commands: Vec<Vec<String>>,
    pub target_next_step_id: Option<String>,
    pub target_next_operator_step: Option<ClientBindingOperatorFlowStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub progress: ClientBindingProofRunbookProgress,
    pub steps: Vec<ClientBindingProofRunbookStep>,
    pub completion_checks: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofArtifactPathSuggestion {
    pub artifact_kind: String,
    pub path: String,
    pub intent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofRunbookProgress {
    pub total_step_count: usize,
    pub operator_action_step_count: usize,
    pub proof_recording_step_count: usize,
    pub ready_now_step_count: usize,
    pub blocked_proof_step_count: usize,
    pub blocking_reason_count: usize,
    pub completed_proof_level_count: usize,
    pub pending_proof_level_count: usize,
    pub ready_to_record_proof_level_count: usize,
    pub blocked_proof_level_count: usize,
    pub release_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingProofRunbookStep {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub stage: String,
    pub evidence_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_artifact_path: Option<String>,
    pub command: Option<Vec<String>>,
    pub mcp_call: Option<ClientBindingMcpCallTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub requires_operator_action: bool,
    pub records_proof: bool,
    pub ready_now: bool,
    pub blocking_reasons: Vec<String>,
    pub trust_boundary: String,
}

pub fn proof_session_render_evidence_artifact_path(
    proof_session: &ClientBindingProofSession,
) -> Option<String> {
    proof_session
        .runbook
        .steps
        .iter()
        .find(|step| {
            step.id == "capture_in_client_render_evidence"
                || step.evidence_kind == "in_client_render_evidence_packet"
        })
        .and_then(|step| step.suggested_artifact_path.clone())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingOperatorFlowStep {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub command: Option<Vec<String>>,
    pub mcp_call: Option<ClientBindingMcpCallTemplate>,
    pub requires_operator_action: bool,
    pub records_proof: bool,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingMcpCallTemplate {
    pub tool: String,
    pub arguments: Value,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RealAppProofArtifact {
    pub kind: String,
    pub path: String,
    pub provided: bool,
    pub status: String,
    pub scan: Option<Value>,
    pub missing_requirements: Vec<&'static str>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RealAppProofAcceptanceGate {
    pub proof_level: ClientBindingProofLevel,
    pub requires_operator_confirmation: bool,
    pub required_artifacts: Vec<String>,
    pub required_checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RealAppProofReadiness {
    pub proof_level: ClientBindingProofLevel,
    pub status: String,
    pub ready_to_attempt_record: bool,
    pub required_artifacts: Vec<String>,
    pub missing_requirements: Vec<String>,
    pub command: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingInstalledConfigPreparationOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub event_source: String,
    pub binding_nonce: String,
    pub generated_binding_nonce: bool,
    pub lifecycle_environment: Value,
    pub spool_append_environment: Value,
    pub installed_config_snippet: Value,
    pub next_commands: Vec<Vec<String>>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingInstalledConfigRenderOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub event_source: String,
    pub binding_nonce: String,
    pub generated_binding_nonce: bool,
    pub output_path: Option<String>,
    pub wrote_file: bool,
    pub installed_config: Value,
    pub eligible_for_observed_app_hook: Option<bool>,
    pub checks: Option<InstalledConfigScan>,
    pub next_commands: Vec<Vec<String>>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingRenderEvidencePacketOutcome {
    pub client: String,
    pub manifest_path: Option<String>,
    pub review_render_report_path: String,
    pub review_render_fingerprint: String,
    pub output_path: Option<String>,
    pub wrote_file: bool,
    pub render_evidence: Value,
    pub preflight_scan: Option<Value>,
    pub missing_requirements: Vec<String>,
    pub next_commands: Vec<Vec<String>>,
    pub trust_boundary: String,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterBindingProofError {
    Io(String),
    MalformedInput(String),
    Storage(StorageError),
}

impl AdapterBindingProofError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AdapterBindingProofError::MalformedInput(_) => 1,
            AdapterBindingProofError::Storage(_) => 2,
            AdapterBindingProofError::Io(_) => 3,
        }
    }
}

impl std::fmt::Display for AdapterBindingProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterBindingProofError::Io(message) => write!(f, "io: {message}"),
            AdapterBindingProofError::MalformedInput(message) => {
                write!(f, "malformed input: {message}")
            }
            AdapterBindingProofError::Storage(err) => write!(f, "storage: {err}"),
        }
    }
}

impl std::error::Error for AdapterBindingProofError {}

impl From<StorageError> for AdapterBindingProofError {
    fn from(value: StorageError) -> Self {
        AdapterBindingProofError::Storage(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileFingerprintScan {
    path: String,
    byte_len: u64,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EventScan {
    path: String,
    byte_len: u64,
    modified_at_ns: Option<i64>,
    fingerprint: String,
    expected_event_source: Option<String>,
    expected_binding_nonce: Option<String>,
    scanned_lines: usize,
    matching_events: usize,
    matching_turns: usize,
    matching_cloud_outputs: usize,
    matching_private_event_sources: usize,
    matching_private_non_release_manual_events: usize,
    matching_private_non_release_test_events: usize,
    matching_writer_contract_events: usize,
    matching_private_writer_contract_events: usize,
    matching_private_binding_nonces: usize,
    matching_private_non_release_manual_binding_nonces: usize,
    matching_private_non_release_test_binding_nonces: usize,
    matching_events_with_observed_at: usize,
    matching_private_events_with_observed_at: usize,
    max_matching_observed_at_ns: Option<i64>,
    max_matching_private_observed_at_ns: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RenderEvidenceScan {
    path: String,
    byte_len: u64,
    fingerprint: String,
    json_parse_error: Option<String>,
    schema: Option<String>,
    client: Option<String>,
    source: Option<String>,
    observed_at_ns: Option<i64>,
    review_render_fingerprint: Option<String>,
    review_workbench_version: Option<String>,
    review_interaction_contract_version: Option<String>,
    rendered_surface_count: usize,
    rendered_surface_placeholder_count: usize,
    raw_tool_output_surface_count: usize,
    visible_surface_count: usize,
    rendered_surface_names: Vec<String>,
    expected_surface_names: Vec<String>,
    missing_surface_names: Vec<String>,
    rendered_control_ids: Vec<String>,
    action_surface_rendered_control_ids: Vec<String>,
    missing_action_surface_control_ids: Vec<String>,
    expected_control_ids: Vec<String>,
    missing_control_ids: Vec<String>,
    trust_boundary: Option<String>,
    valid_structured_render_evidence: bool,
    missing_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewActionReportScan {
    path: String,
    byte_len: u64,
    modified_at_ns: Option<i64>,
    fingerprint: String,
    json_parse_error: Option<String>,
    target_type: Option<String>,
    target_id: Option<i64>,
    action: Option<String>,
    control_id: Option<String>,
    control_binding_verified: bool,
    verification_result: Option<String>,
    verification_event_count: usize,
    non_cloud_verification_event_count: usize,
    claim_count: usize,
    task_frame_outcome_count: usize,
    durable_promotion_trust: Option<bool>,
    trust_boundary: Option<String>,
    valid_storage_gated_review_action: bool,
    missing_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct InstalledConfigScan {
    pub path: String,
    pub byte_len: u64,
    pub modified_at_ns: Option<i64>,
    pub fingerprint: String,
    pub expected_event_source: Option<String>,
    pub binding_nonce: Option<String>,
    pub references_lifecycle_wrapper: bool,
    pub references_spool_append: bool,
    pub references_spool_drain: bool,
    pub references_review_render: bool,
    pub references_client: bool,
    pub references_event_jsonl_env: bool,
    pub references_private_event_source: bool,
    pub references_binding_nonce: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContinueDevdataCollectorProbe {
    devdata_destination_visible: bool,
    collector_status: String,
    collector_listening: bool,
    collector_host: String,
    collector_port: u16,
    collector_error: Option<String>,
    status_command: Option<Vec<String>>,
    start_command: Option<Vec<String>>,
    trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdapterBindingEvidenceArtifactVerificationOutcome {
    pub client: Option<String>,
    pub proof_id: Option<i64>,
    pub limit: usize,
    pub proofs_found: usize,
    pub all_artifacts_verified: bool,
    pub failing_artifact_count: usize,
    pub repair_actions: Vec<EvidenceArtifactRepairAction>,
    pub recovery_commands: Vec<Vec<String>>,
    pub next_steps: Vec<String>,
    pub trust_boundary: String,
    pub proofs: Vec<ClientBindingEvidenceArtifactVerification>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientBindingEvidenceArtifactVerification {
    pub proof_id: i64,
    pub client: String,
    pub proof_level: ClientBindingProofLevel,
    pub all_artifacts_verified: bool,
    pub artifact_checks: Vec<EvidenceArtifactCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceArtifactRepairAction {
    pub source: &'static str,
    pub proof_id: i64,
    pub client: String,
    pub proof_level: ClientBindingProofLevel,
    pub artifact_kind: String,
    pub status: EvidenceArtifactStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_path: Option<String>,
    pub intent: String,
    pub command: Vec<String>,
    pub records_proof: bool,
    pub requires_operator_action: bool,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceArtifactStatus {
    Verified,
    VerifiedAppendOnlyGrowth,
    MissingExpectedRecord,
    MissingPath,
    MissingFile,
    Changed,
    Unreadable,
}

impl EvidenceArtifactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceArtifactStatus::Verified => "verified",
            EvidenceArtifactStatus::VerifiedAppendOnlyGrowth => "verified_append_only_growth",
            EvidenceArtifactStatus::MissingExpectedRecord => "missing_expected_record",
            EvidenceArtifactStatus::MissingPath => "missing_path",
            EvidenceArtifactStatus::MissingFile => "missing_file",
            EvidenceArtifactStatus::Changed => "changed",
            EvidenceArtifactStatus::Unreadable => "unreadable",
        }
    }
}

fn evidence_artifact_status_is_verified(status: EvidenceArtifactStatus) -> bool {
    matches!(
        status,
        EvidenceArtifactStatus::Verified | EvidenceArtifactStatus::VerifiedAppendOnlyGrowth
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceArtifactCheck {
    pub kind: String,
    pub path: Option<String>,
    pub expected_byte_len: Option<u64>,
    pub actual_byte_len: Option<u64>,
    pub expected_fingerprint: Option<String>,
    pub actual_fingerprint: Option<String>,
    pub status: EvidenceArtifactStatus,
    pub error: Option<String>,
}

pub fn run_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingProofOutcome, AdapterBindingProofError> {
    if args.list {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_list_blocking for --list mode".to_string(),
        ));
    }
    if args.status {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_status_blocking for --status mode".to_string(),
        ));
    }
    if args.verify_evidence_artifacts {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_verify_evidence_artifacts_blocking for --verify-evidence-artifacts mode"
                .to_string(),
        ));
    }
    if args.discover_installed_config {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_discover_installed_config_blocking for --discover-installed-config mode"
                .to_string(),
        ));
    }
    if args.real_app_proof_kit {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_real_app_proof_kit_blocking for --real-app-proof-kit mode".to_string(),
        ));
    }
    if args.prepare_installed_config {
        return Err(AdapterBindingProofError::MalformedInput(
            "use run_prepare_installed_config_blocking for --prepare-installed-config mode"
                .to_string(),
        ));
    }
    let manifest_arg = args.manifest.as_deref().ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "adapter-binding-proof requires --manifest unless --list is used".to_string(),
        )
    })?;
    let manifest_path = canonical_or_raw(manifest_arg);
    let manifest_scan = scan_file_fingerprint(&manifest_path, "manifest")?;
    let manifest = read_json_file(&manifest_path, "manifest")?;
    validate_manifest_shape(&manifest)?;

    let manifest_client = required_str(&manifest, "client")?.to_string();
    let client = args
        .client
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&manifest_client)
        .trim()
        .to_ascii_lowercase();
    if manifest_client.trim().to_ascii_lowercase() != client {
        return Err(AdapterBindingProofError::MalformedInput(format!(
            "manifest client `{manifest_client}` does not match requested client `{client}`"
        )));
    }

    let proof_level = ClientBindingProofLevel::from_wire(&args.proof_level).ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(format!(
            "unknown proof level `{}`; expected reference_binding, observed_event_file, observed_app_hook, observed_in_client_render, or observed_review_action",
            args.proof_level
        ))
    })?;
    if matches!(proof_level, ClientBindingProofLevel::ObservedAppHook)
        && !args.operator_confirm_real_app_invocation
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "observed_app_hook requires --operator-confirm-real-app-invocation".to_string(),
        ));
    }
    if matches!(proof_level, ClientBindingProofLevel::ObservedInClientRender)
        && !args.operator_confirm_in_client_render
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "observed_in_client_render requires --operator-confirm-in-client-render".to_string(),
        ));
    }
    if matches!(proof_level, ClientBindingProofLevel::ObservedReviewAction)
        && !args.operator_confirm_review_action
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "observed_review_action requires --operator-confirm-review-action".to_string(),
        ));
    }

    let manifest_status = required_str(&manifest, "status")?.to_string();
    let manifest_trust_boundary = required_str(&manifest, "trust_boundary")?.to_string();
    let expected_event_source = expected_event_source_from_manifest(&manifest, &client);
    let installed_config_scan = if let Some(path) = args.installed_config.as_deref() {
        Some(scan_installed_config(
            &canonical_or_raw(path),
            &client,
            expected_event_source.as_deref(),
        )?)
    } else {
        None
    };
    require_installed_config_for_level(proof_level, installed_config_scan.as_ref())?;
    let expected_binding_nonce =
        installed_config_scan.as_ref().and_then(|scan| scan.binding_nonce.as_deref());
    let event_scan = if let Some(path) = args.event_jsonl.as_deref() {
        Some(scan_event_jsonl(
            &canonical_or_raw(path),
            &client,
            expected_event_source.as_deref(),
            expected_binding_nonce,
        )?)
    } else {
        None
    };
    require_event_scan_for_level(
        proof_level,
        event_scan.as_ref(),
        expected_event_source.as_deref(),
        expected_binding_nonce,
    )?;
    require_observed_app_hook_temporal_binding(
        proof_level,
        event_scan.as_ref(),
        installed_config_scan.as_ref(),
    )?;

    let drain_report = if let Some(path) = args.drain_report.as_deref() {
        Some(read_json_file(&canonical_or_raw(path), "drain report")?)
    } else {
        None
    };
    validate_drain_report_for_level(proof_level, drain_report.as_ref())?;

    let review_render_file_scan = if let Some(path) = args.review_render_report.as_deref() {
        Some(scan_file_fingerprint(&canonical_or_raw(path), "review render report")?)
    } else {
        None
    };
    let review_render = if let Some(path) = args.review_render_report.as_deref() {
        Some(read_json_file(&canonical_or_raw(path), "review render report")?)
    } else {
        None
    };
    require_review_render_for_level(proof_level, review_render.as_ref())?;
    validate_review_render(review_render.as_ref(), &client)?;

    let mut render_evidence_scan = if let Some(path) = args.render_evidence.as_deref() {
        Some(scan_render_evidence(&canonical_or_raw(path))?)
    } else {
        None
    };
    require_render_evidence_for_level(
        proof_level,
        render_evidence_scan.as_mut(),
        &client,
        review_render_file_scan.as_ref(),
        review_render.as_ref(),
    )?;

    let review_action_report_scan = if let Some(path) = args.review_action_report.as_deref() {
        Some(scan_review_action_report(&canonical_or_raw(path))?)
    } else {
        None
    };
    require_review_action_report_for_level(proof_level, review_action_report_scan.as_ref())?;

    let mut store = Storage::open(&ctx.db_path)?;
    let linked_render_proof =
        if matches!(proof_level, ClientBindingProofLevel::ObservedReviewAction) {
            Some(link_review_action_to_render_proof(
                &store,
                &client,
                installed_config_scan.as_ref(),
                review_action_report_scan.as_ref(),
            )?)
        } else {
            None
        };

    let proof_observed_at_ns = now_ns();
    reject_future_observed_app_hook_events(proof_level, event_scan.as_ref(), proof_observed_at_ns)?;
    let trust_boundary = trust_boundary_for_level(proof_level, &manifest_trust_boundary);
    let checks = json!({
        "manifest_schema": required_str(&manifest, "schema")?,
        "manifest_names_lifecycle_wrapper": manifest.pointer("/lifecycle/wrapper").and_then(Value::as_str) == Some("tools/soma-adapter-lifecycle.sh"),
        "manifest_names_spool_wrapper": manifest.pointer("/spool_drain/wrapper").and_then(Value::as_str) == Some("tools/soma-adapter-spool-watch.sh"),
        "manifest_names_review_wrapper": manifest.pointer("/review_ui/wrapper").and_then(Value::as_str) == Some("tools/soma-review-render.sh"),
        "manifest_refuses_private_install_proof": manifest_trust_boundary.contains("does not prove"),
        "manifest_scan": manifest_scan,
        "expected_private_event_source": expected_event_source,
        "event_scan": event_scan,
        "installed_config_scan": installed_config_scan,
        "render_evidence_scan": render_evidence_scan,
        "review_action_report_scan": review_action_report_scan,
        "linked_render_proof": linked_render_proof,
        "drain_report_present": drain_report.is_some(),
        "review_render_present": review_render.is_some(),
        "review_render_file_scan": review_render_file_scan,
        "proof_observed_at_ns": proof_observed_at_ns,
        "operator_confirmed_real_app_invocation": args.operator_confirm_real_app_invocation,
        "operator_confirmed_in_client_render": args.operator_confirm_in_client_render,
        "operator_confirmed_review_action": args.operator_confirm_review_action,
        "operator_confirmed_release_grade_evidence": args.operator_confirm_release_grade_evidence,
        "release_grade_evidence_source_policy": release_grade_evidence_source_policy(
            &args.evidence_source,
            args.operator_confirm_release_grade_evidence,
        ),
    });

    let draft = ClientBindingProofDraft {
        client: client.clone(),
        proof_level,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        manifest_status: manifest_status.clone(),
        evidence_source: args.evidence_source.trim().to_string(),
        event_jsonl_path: args
            .event_jsonl
            .as_deref()
            .map(|path| canonical_or_raw(path).to_string_lossy().into_owned()),
        installed_config_path: args
            .installed_config
            .as_deref()
            .map(|path| canonical_or_raw(path).to_string_lossy().into_owned()),
        render_evidence_path: args
            .render_evidence
            .as_deref()
            .map(|path| canonical_or_raw(path).to_string_lossy().into_owned()),
        review_action_report_path: args
            .review_action_report
            .as_deref()
            .map(|path| canonical_or_raw(path).to_string_lossy().into_owned()),
        drain_report_json: drain_report,
        review_render_json: review_render,
        trust_boundary: trust_boundary.clone(),
        checks_json: checks.clone(),
        observed_at_ns: proof_observed_at_ns,
    };
    let proof_id = store.insert_client_binding_proof(&draft)?;
    Ok(AdapterBindingProofOutcome {
        proof_id,
        client,
        proof_level,
        manifest_status,
        evidence_source: args.evidence_source.trim().to_string(),
        trust_boundary,
        checks,
    })
}

pub fn run_list_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingProofListOutcome, AdapterBindingProofError> {
    let client = args
        .client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let limit = args.limit.clamp(1, 200);
    let store = Storage::open(&ctx.db_path)?;
    let proofs = store.recent_client_binding_proofs(client.as_deref(), limit)?;
    Ok(AdapterBindingProofListOutcome { client, limit, proofs })
}

pub fn run_status_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingProofStatusOutcome, AdapterBindingProofError> {
    let client = args
        .client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let limit = args.limit.clamp(1, 200);
    let store = Storage::open(&ctx.db_path)?;
    let proofs = if let Some(proof_id) = args.proof_id {
        store.client_binding_proof_by_id(proof_id)?.into_iter().collect()
    } else {
        store.recent_client_binding_proofs(client.as_deref(), limit)?
    };
    let proofs: Vec<_> = proofs
        .into_iter()
        .filter(|proof| client.as_deref().is_none_or(|filter| proof.client == filter))
        .collect();
    Ok(build_client_binding_status_report(client, args.proof_id, limit, &proofs))
}

pub fn build_client_binding_status_report(
    client: Option<String>,
    proof_id: Option<i64>,
    limit: usize,
    proofs: &[StoredClientBindingProof],
) -> AdapterBindingProofStatusOutcome {
    let clients = client_binding_readiness_statuses(proofs);
    let all_latest_artifacts_verified =
        !clients.is_empty() && clients.iter().all(|status| status.all_latest_artifacts_verified);
    AdapterBindingProofStatusOutcome {
        client,
        proof_id,
        limit,
        proofs_found: proofs.len(),
        client_count: clients.len(),
        all_latest_artifacts_verified,
        trust_boundary: "client_binding_status_is_read_only: derives readiness from stored proof rows and artifact replay only; append-only event_jsonl growth is verified by recorded prefix fingerprint, while other artifacts require exact byte length and fingerprint; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation beyond existing ledger evidence".to_string(),
        clients,
    }
}

pub fn run_verify_evidence_artifacts_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingEvidenceArtifactVerificationOutcome, AdapterBindingProofError> {
    let client = args
        .client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let limit = args.limit.clamp(1, 200);
    let store = Storage::open(&ctx.db_path)?;
    let stored = if let Some(proof_id) = args.proof_id {
        store.client_binding_proof_by_id(proof_id)?.into_iter().collect()
    } else {
        store.recent_client_binding_proofs(client.as_deref(), limit)?
    };
    let proofs: Vec<_> = stored.iter().map(verify_proof_artifacts).collect();
    let all_artifacts_verified = !proofs.is_empty()
        && proofs.iter().all(|proof| {
            proof.all_artifacts_verified
                && proof
                    .artifact_checks
                    .iter()
                    .all(|check| evidence_artifact_status_is_verified(check.status))
        });
    let failing_artifact_count = proofs
        .iter()
        .flat_map(|proof| proof.artifact_checks.iter())
        .filter(|check| !evidence_artifact_status_is_verified(check.status))
        .count();
    let repair_actions = evidence_artifact_repair_actions(&proofs);
    let recovery_commands =
        dedup_command_lists(repair_actions.iter().map(|action| action.command.clone()).collect());
    let next_steps = evidence_artifact_replay_next_steps(all_artifacts_verified, &repair_actions);
    Ok(AdapterBindingEvidenceArtifactVerificationOutcome {
        client,
        proof_id: args.proof_id,
        limit,
        proofs_found: proofs.len(),
        all_artifacts_verified,
        failing_artifact_count,
        repair_actions,
        recovery_commands,
        next_steps,
        trust_boundary: "evidence_artifact_replay_is_read_only: compares stored path/byte_length/fingerprint; event_jsonl artifacts may grow append-only when the recorded prefix fingerprint still matches, while other artifacts require exact replay; repair_actions and recovery_commands are read-only handoffs back to proof-session runbooks and record no proof row; it records no verification event, promotes no claim, applies no proposal, and does not prove private app installation beyond the stored proof row".to_string(),
        proofs,
    })
}

fn dedup_command_lists(commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for command in commands {
        if !out.iter().any(|existing| existing == &command) {
            out.push(command);
        }
    }
    out
}

fn evidence_artifact_replay_next_steps(
    all_artifacts_verified: bool,
    repair_actions: &[EvidenceArtifactRepairAction],
) -> Vec<String> {
    if all_artifacts_verified {
        return vec![
            "stored_artifact_replay_passed".to_string(),
            "rerun_client_binding_status_or_hardening_report_if_release_gate_is_still_blocked"
                .to_string(),
        ];
    }
    if repair_actions.is_empty() {
        return vec![
            "inspect_missing_or_changed_artifact_rows_before_trusting_private_client_readiness"
                .to_string(),
            "rerun_soma_adapter_binding_proof_proof_session_for_the_affected_client".to_string(),
        ];
    }
    vec![
        "follow_repair_actions_for_each_missing_or_changed_artifact".to_string(),
        "re_capture_real_private_client_evidence_before_recording_new_release_grade_proof_rows"
            .to_string(),
        "rerun_verify_evidence_artifacts_after_re_recording_fresh_proof_rows".to_string(),
    ]
}

fn evidence_artifact_repair_actions(
    proofs: &[ClientBindingEvidenceArtifactVerification],
) -> Vec<EvidenceArtifactRepairAction> {
    let mut actions = Vec::new();
    for proof in proofs {
        for check in &proof.artifact_checks {
            if evidence_artifact_status_is_verified(check.status) {
                continue;
            }
            actions.push(EvidenceArtifactRepairAction {
                source: "soma.adapter_binding_proof.artifact_replay_repair_action.v1",
                proof_id: proof.proof_id,
                client: proof.client.clone(),
                proof_level: proof.proof_level,
                artifact_kind: check.kind.clone(),
                status: check.status,
                stale_path: check.path.clone(),
                intent: evidence_artifact_repair_intent(
                    proof.proof_level,
                    check.kind.as_str(),
                    check.status,
                )
                .to_string(),
                command: vec![
                    "soma".to_string(),
                    "adapter-binding-proof".to_string(),
                    "--proof-session".to_string(),
                    "--client".to_string(),
                    proof.client.clone(),
                    "--json".to_string(),
                ],
                records_proof: false,
                requires_operator_action: evidence_artifact_repair_requires_operator_action(
                    proof.proof_level,
                    check.kind.as_str(),
                ),
                trust_boundary: "artifact_replay_repair_action_is_read_only: it identifies stale or changed proof artifacts and points back to the proof-session runbook; it records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and cannot replace fresh private-client evidence plus explicit operator confirmation".to_string(),
            });
        }
    }
    actions
}

fn evidence_artifact_repair_requires_operator_action(
    proof_level: ClientBindingProofLevel,
    kind: &str,
) -> bool {
    matches!(
        (proof_level, kind),
        (ClientBindingProofLevel::ObservedAppHook, "event_jsonl")
            | (ClientBindingProofLevel::ObservedAppHook, "installed_config")
            | (ClientBindingProofLevel::ObservedInClientRender, "render_evidence")
            | (ClientBindingProofLevel::ObservedReviewAction, "review_action_report")
    )
}

fn evidence_artifact_repair_intent(
    proof_level: ClientBindingProofLevel,
    kind: &str,
    status: EvidenceArtifactStatus,
) -> &'static str {
    match (proof_level, kind, status) {
        (ClientBindingProofLevel::ObservedInClientRender, "render_evidence", _) => {
            "Re-capture a structured soma.in_client_render_evidence.v1 artifact after the private client visibly renders the current review surface, then re-record observed_in_client_render with explicit operator confirmation."
        }
        (ClientBindingProofLevel::ObservedReviewAction, "review_action_report", _) => {
            "Execute a currently rendered review control with non-cloud user/tool/local/correction evidence, save the storage-gated review-action report, then re-record observed_review_action with explicit operator confirmation."
        }
        (ClientBindingProofLevel::ObservedAppHook, "event_jsonl", _) => {
            "Trigger the real private client hook again, drain the adapter spool, then re-record observed_app_hook only after matching event_source, binding_nonce, writer metadata, temporal binding, and explicit operator confirmation."
        }
        (ClientBindingProofLevel::ObservedAppHook, "installed_config", _) => {
            "Re-check or reinstall the private client binding config before recording app-hook/render/review-action proof against a fresh installed-config artifact."
        }
        (_, "manifest", EvidenceArtifactStatus::Changed) => {
            "Inspect the changed binding manifest and re-record reference or stronger proof only after confirming the current manifest is the intended contract."
        }
        (_, "manifest", _) => {
            "Restore or locate the binding manifest, then rerun the proof-session before claiming client readiness."
        }
        (_, "event_jsonl", EvidenceArtifactStatus::Changed) => {
            "The event JSONL no longer matches the stored proof prefix; inspect the event file and record fresh proof from a valid private-client event if needed."
        }
        (_, "event_jsonl", _) => {
            "Restore or regenerate the event JSONL evidence through the client wrapper before recording any stronger private-client proof."
        }
        (_, "installed_config", _) => {
            "Restore, regenerate, or re-check the installed client binding config before recording proof that depends on it."
        }
        (_, "render_evidence", _) => {
            "Rebuild render evidence from a fresh visible client render before relying on observed_in_client_render proof."
        }
        (_, "review_action_report", _) => {
            "Regenerate a storage-gated review-action report from a visible rendered control before relying on observed_review_action proof."
        }
        _ => {
            "Inspect the stale artifact and follow the proof-session runbook before relying on this proof row."
        }
    }
}

fn client_binding_readiness_statuses(
    proofs: &[StoredClientBindingProof],
) -> Vec<ClientBindingReadinessStatus> {
    let mut by_client: BTreeMap<String, Vec<&StoredClientBindingProof>> = BTreeMap::new();
    for proof in proofs {
        by_client.entry(proof.client.clone()).or_default().push(proof);
    }

    by_client
        .into_iter()
        .map(|(client, mut proofs)| {
            proofs.sort_by(|a, b| {
                b.observed_at_ns.cmp(&a.observed_at_ns).then_with(|| b.id.cmp(&a.id))
            });
            readiness_status_for_client(client, &proofs)
        })
        .collect()
}

fn readiness_status_for_client(
    client: String,
    proofs: &[&StoredClientBindingProof],
) -> ClientBindingReadinessStatus {
    let mut latest_by_level: BTreeMap<String, ClientBindingLatestProofStatus> = BTreeMap::new();
    for proof in proofs {
        let key = proof.proof_level.as_str().to_string();
        latest_by_level.entry(key).or_insert_with(|| latest_proof_status(proof));
    }

    let has_reference_binding =
        latest_by_level.contains_key(ClientBindingProofLevel::ReferenceBinding.as_str());
    let has_observed_event_file =
        latest_by_level.contains_key(ClientBindingProofLevel::ObservedEventFile.as_str());
    let has_observed_app_hook =
        latest_by_level.contains_key(ClientBindingProofLevel::ObservedAppHook.as_str());
    let has_observed_in_client_render =
        latest_by_level.contains_key(ClientBindingProofLevel::ObservedInClientRender.as_str());
    let has_observed_review_action =
        latest_by_level.contains_key(ClientBindingProofLevel::ObservedReviewAction.as_str());
    let latest = proofs.first().copied();
    let artifact_failures = artifact_failures_for_latest_levels(&latest_by_level);
    let all_latest_artifacts_verified = !latest_by_level.is_empty()
        && latest_by_level.values().all(|status| status.all_artifacts_verified);
    let has_release_proof_chain =
        has_observed_app_hook && has_observed_in_client_render && has_observed_review_action;
    let release_artifact_failures: Vec<_> = artifact_failures
        .iter()
        .filter(|failure| release_claim_proof_level(failure.proof_level))
        .cloned()
        .collect();
    let all_release_artifacts_verified = has_release_proof_chain
        && latest_by_level
            .values()
            .filter(|status| release_claim_proof_level(status.proof_level))
            .all(|status| status.all_artifacts_verified);
    let blocking_artifact_failures =
        if has_release_proof_chain { &release_artifact_failures } else { &artifact_failures };
    let coherence_failures = client_binding_coherence_failures(&latest_by_level);
    let non_release_evidence_sources =
        non_release_evidence_sources_for_latest_levels(&latest_by_level);
    let proof_stage = proof_stage(
        has_reference_binding,
        has_observed_event_file,
        has_observed_app_hook,
        has_observed_in_client_render,
        has_observed_review_action,
    );
    let readiness = if !blocking_artifact_failures.is_empty() {
        "artifact_integrity_failed".to_string()
    } else if !coherence_failures.is_empty() {
        "proof_identity_mismatch".to_string()
    } else if has_release_proof_chain && !non_release_evidence_sources.is_empty() {
        "non_release_evidence_source".to_string()
    } else {
        proof_stage.clone()
    };
    let ready_for_client_operator_loop = has_release_proof_chain
        && all_release_artifacts_verified
        && release_artifact_failures.is_empty()
        && coherence_failures.is_empty();
    let ready_for_private_client_claim =
        ready_for_client_operator_loop && non_release_evidence_sources.is_empty();
    let next_steps = client_binding_next_steps(
        has_reference_binding,
        has_observed_event_file,
        has_observed_app_hook,
        has_observed_in_client_render,
        has_observed_review_action,
        !blocking_artifact_failures.is_empty(),
        !coherence_failures.is_empty(),
        !non_release_evidence_sources.is_empty(),
    );
    let operator_flow = client_binding_status_operator_flow(&client, proofs, &latest_by_level);

    ClientBindingReadinessStatus {
        client,
        proof_stage,
        readiness,
        ready_for_private_client_claim,
        has_reference_binding,
        has_observed_event_file,
        has_observed_app_hook,
        has_observed_in_client_render,
        has_observed_review_action,
        ready_for_client_operator_loop,
        latest_proof_id: latest.map(|proof| proof.id),
        latest_proof_level: latest.map(|proof| proof.proof_level),
        latest_observed_at_ns: latest.map(|proof| proof.observed_at_ns),
        latest_by_level,
        all_latest_artifacts_verified,
        artifact_failures,
        coherence_failures,
        non_release_evidence_sources,
        next_steps,
        operator_flow,
    }
}

fn client_binding_status_operator_flow(
    client: &str,
    proofs: &[&StoredClientBindingProof],
    latest_by_level: &BTreeMap<String, ClientBindingLatestProofStatus>,
) -> Vec<ClientBindingOperatorFlowStep> {
    let manifest_path = proofs.first().map(|proof| proof.manifest_path.as_str());
    let binding_nonce = latest_by_level
        .values()
        .find_map(|proof| proof.installed_config_binding_nonce.as_deref())
        .unwrap_or("<binding-nonce>");
    let installed_config_path =
        proofs.iter().find_map(|proof| proof.installed_config_path.as_deref());
    let event_jsonl_path = proofs.iter().find_map(|proof| proof.event_jsonl_path.as_deref());
    let review_render_report_path = proofs
        .iter()
        .find_map(|proof| string_pointer(&proof.checks_json, "/review_render_file_scan/path"));
    let render_evidence_path =
        proofs.iter().find_map(|proof| proof.render_evidence_path.as_deref());
    let review_action_report_path =
        proofs.iter().find_map(|proof| proof.review_action_report_path.as_deref());

    client_binding_operator_flow(
        client,
        manifest_path,
        None,
        binding_nonce,
        None,
        None,
        installed_config_path,
        event_jsonl_path,
        review_render_report_path,
        render_evidence_path,
        review_action_report_path,
    )
}

fn latest_proof_status(proof: &StoredClientBindingProof) -> ClientBindingLatestProofStatus {
    let verification = verify_proof_artifacts(proof);
    ClientBindingLatestProofStatus {
        proof_id: proof.id,
        proof_level: proof.proof_level,
        observed_at_ns: proof.observed_at_ns,
        manifest_status: proof.manifest_status.clone(),
        evidence_source: proof.evidence_source.clone(),
        operator_confirmed_release_grade_evidence: proof
            .checks_json
            .pointer("/operator_confirmed_release_grade_evidence")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        installed_config_path: proof.installed_config_path.clone(),
        installed_config_fingerprint: string_pointer(
            &proof.checks_json,
            "/installed_config_scan/fingerprint",
        )
        .map(ToOwned::to_owned),
        installed_config_binding_nonce: string_pointer(
            &proof.checks_json,
            "/installed_config_scan/binding_nonce",
        )
        .map(ToOwned::to_owned),
        review_action_control_id: string_pointer(
            &proof.checks_json,
            "/review_action_report_scan/control_id",
        )
        .map(ToOwned::to_owned),
        all_artifacts_verified: verification.all_artifacts_verified,
        artifact_checks: verification.artifact_checks,
    }
}

fn client_binding_coherence_failures(
    latest_by_level: &BTreeMap<String, ClientBindingLatestProofStatus>,
) -> Vec<String> {
    let Some(app_hook) = latest_by_level.get(ClientBindingProofLevel::ObservedAppHook.as_str())
    else {
        return Vec::new();
    };
    let Some(render) =
        latest_by_level.get(ClientBindingProofLevel::ObservedInClientRender.as_str())
    else {
        return Vec::new();
    };
    let review_action = latest_by_level.get(ClientBindingProofLevel::ObservedReviewAction.as_str());

    let mut failures = Vec::new();
    match (
        app_hook.installed_config_fingerprint.as_deref(),
        render.installed_config_fingerprint.as_deref(),
    ) {
        (Some(app), Some(rendered)) if app == rendered => {}
        (Some(_), Some(_)) => failures.push("installed_config_fingerprint_mismatch".to_string()),
        _ => failures.push("installed_config_fingerprint_missing_for_readiness".to_string()),
    }
    if let Some(review_action) = review_action {
        match (
            render.installed_config_fingerprint.as_deref(),
            review_action.installed_config_fingerprint.as_deref(),
        ) {
            (Some(rendered), Some(reviewed)) if rendered == reviewed => {}
            (Some(_), Some(_)) => {
                failures.push("review_action_installed_config_fingerprint_mismatch".to_string());
            }
            _ => failures.push(
                "review_action_installed_config_fingerprint_missing_for_readiness".to_string(),
            ),
        }
    }
    match (
        app_hook.installed_config_binding_nonce.as_deref(),
        render.installed_config_binding_nonce.as_deref(),
    ) {
        (Some(app), Some(rendered)) if app == rendered => {}
        (Some(_), Some(_)) => failures.push("installed_config_binding_nonce_mismatch".to_string()),
        _ => failures.push("installed_config_binding_nonce_missing_for_readiness".to_string()),
    }
    if let Some(review_action) = review_action {
        match (
            render.installed_config_binding_nonce.as_deref(),
            review_action.installed_config_binding_nonce.as_deref(),
        ) {
            (Some(rendered), Some(reviewed)) if rendered == reviewed => {}
            (Some(_), Some(_)) => {
                failures.push("review_action_installed_config_binding_nonce_mismatch".to_string());
            }
            _ => failures.push(
                "review_action_installed_config_binding_nonce_missing_for_readiness".to_string(),
            ),
        }
    }
    failures
}

fn artifact_failures_for_latest_levels(
    latest_by_level: &BTreeMap<String, ClientBindingLatestProofStatus>,
) -> Vec<ClientBindingArtifactFailure> {
    latest_by_level
        .values()
        .flat_map(|proof| {
            proof
                .artifact_checks
                .iter()
                .filter(|check| !evidence_artifact_status_is_verified(check.status))
                .map(|check| ClientBindingArtifactFailure {
                    proof_id: proof.proof_id,
                    proof_level: proof.proof_level,
                    kind: check.kind.clone(),
                    path: check.path.clone(),
                    status: check.status,
                    error: check.error.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn non_release_evidence_sources_for_latest_levels(
    latest_by_level: &BTreeMap<String, ClientBindingLatestProofStatus>,
) -> Vec<ClientBindingNonReleaseEvidenceSource> {
    latest_by_level
        .values()
        .filter(|proof| release_claim_proof_level(proof.proof_level))
        .filter_map(|proof| {
            non_release_evidence_source_reason_for_proof(proof).map(|reason| {
                ClientBindingNonReleaseEvidenceSource {
                    proof_id: proof.proof_id,
                    proof_level: proof.proof_level,
                    evidence_source: proof.evidence_source.clone(),
                    reason,
                }
            })
        })
        .collect()
}

fn release_claim_proof_level(proof_level: ClientBindingProofLevel) -> bool {
    matches!(
        proof_level,
        ClientBindingProofLevel::ObservedAppHook
            | ClientBindingProofLevel::ObservedInClientRender
            | ClientBindingProofLevel::ObservedReviewAction
    )
}

fn release_grade_operator_evidence_source(
    client: &str,
    proof_level: ClientBindingProofLevel,
) -> String {
    format!("private_client_operator_observed_{client}_{}", proof_level.as_str())
}

fn release_grade_evidence_source_policy(
    evidence_source: &str,
    operator_confirmed_release_grade_evidence: bool,
) -> Value {
    let source_reason = non_release_evidence_source_reason(evidence_source);
    let non_release_reason = source_reason.clone().or_else(|| {
        if operator_confirmed_release_grade_evidence {
            None
        } else {
            Some(
                "release-grade private-client evidence source was not explicitly operator-confirmed"
                    .to_string(),
            )
        }
    });
    json!({
        "schema": "soma.client_binding_release_evidence_source_policy.v1",
        "evidence_source": evidence_source.trim(),
        "operator_confirmed_release_grade_evidence": operator_confirmed_release_grade_evidence,
        "source_label_passed": source_reason.is_none(),
        "release_grade_evidence_passed": non_release_reason.is_none(),
        "non_release_reason": non_release_reason,
    })
}

fn non_release_evidence_source_reason_for_proof(
    proof: &ClientBindingLatestProofStatus,
) -> Option<String> {
    non_release_evidence_source_reason(&proof.evidence_source).or_else(|| {
        if proof.operator_confirmed_release_grade_evidence {
            None
        } else {
            Some(format!(
                "evidence_source `{}` was not explicitly operator-confirmed as release-grade private-client evidence",
                proof.evidence_source.trim()
            ))
        }
    })
}

fn non_release_evidence_source_reason(evidence_source: &str) -> Option<String> {
    let normalized = evidence_source.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized.is_empty() {
        return Some("evidence_source is empty".to_string());
    }
    let markers = [
        "test",
        "smoke",
        "eval",
        "fixture",
        "synthetic",
        "mock",
        "sample",
        "mcp_server",
        "storage_test",
        "client_integration_eval",
    ];
    markers
        .iter()
        .find(|marker| normalized.contains(**marker))
        .map(|marker| {
            format!(
                "evidence_source `{}` contains non-release marker `{}`",
                evidence_source.trim(),
                marker
            )
        })
        .or_else(|| {
            if normalized.contains("private_client") {
                None
            } else {
                Some(format!(
                    "evidence_source `{}` lacks release-grade `private_client` marker",
                    evidence_source.trim()
                ))
            }
        })
        .or_else(|| {
            let release_grade_markers =
                ["manual", "operator", "observation", "observed", "runtime", "client_capture"];
            if release_grade_markers.iter().any(|marker| normalized.contains(marker)) {
                None
            } else {
                Some(format!(
                "evidence_source `{}` lacks release-grade operator or runtime observation marker",
                evidence_source.trim()
            ))
            }
        })
}

#[allow(clippy::fn_params_excessive_bools)]
fn proof_stage(
    has_reference_binding: bool,
    has_observed_event_file: bool,
    has_observed_app_hook: bool,
    has_observed_in_client_render: bool,
    has_observed_review_action: bool,
) -> String {
    if has_observed_app_hook && has_observed_in_client_render && has_observed_review_action {
        "real_app_hook_in_client_render_and_review_action_observed".to_string()
    } else if has_observed_app_hook && has_observed_in_client_render {
        "real_app_hook_and_in_client_render_observed".to_string()
    } else if has_observed_review_action {
        "review_action_observed_without_complete_client_binding".to_string()
    } else if has_observed_in_client_render {
        "in_client_render_observed_without_app_hook".to_string()
    } else if has_observed_app_hook {
        "real_app_hook_observed".to_string()
    } else if has_observed_event_file {
        "event_file_observed_only".to_string()
    } else if has_reference_binding {
        "reference_binding_only".to_string()
    } else {
        "no_proof_rows".to_string()
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn client_binding_next_steps(
    has_reference_binding: bool,
    has_observed_event_file: bool,
    has_observed_app_hook: bool,
    has_observed_in_client_render: bool,
    has_observed_review_action: bool,
    has_artifact_failures: bool,
    has_coherence_failures: bool,
    has_non_release_evidence_sources: bool,
) -> Vec<String> {
    let mut steps = Vec::new();
    if has_artifact_failures {
        steps.push("refresh_or_replay_changed_evidence_artifacts".to_string());
    }
    if has_coherence_failures {
        steps.push(
            "record_observed_app_hook_and_observed_in_client_render_from_same_installed_config_artifact"
                .to_string(),
        );
    }
    if has_non_release_evidence_sources {
        steps.push(
            "re_record_app_hook_render_and_review_action_with_release_grade_private_client_evidence_source"
                .to_string(),
        );
    }
    if !has_reference_binding && !has_observed_event_file && !has_observed_app_hook {
        steps.push("record_reference_binding_manifest".to_string());
    }
    if !has_observed_event_file && !has_observed_app_hook {
        steps.push("record_observed_event_file_from_wrapper_or_spool".to_string());
    }
    if !has_observed_app_hook {
        steps.push("record_observed_app_hook_with_installed_config_private_event_source_binding_nonce_and_operator_confirmation".to_string());
    }
    if !has_observed_in_client_render {
        steps.push("record_observed_in_client_render_with_review_render_report_render_evidence_and_operator_confirmation".to_string());
    }
    if has_observed_in_client_render && !has_observed_review_action {
        steps.push("record_observed_review_action_with_rendered_control_id_and_storage_gated_review_action_report".to_string());
    }
    steps
}

pub fn run_installed_config_check_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingInstalledConfigCheckOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let installed_config = args.installed_config.as_deref().ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "--check-installed-config requires --installed-config".to_string(),
        )
    })?;
    let installed_config_path = canonical_or_raw(installed_config);
    let checks =
        scan_installed_config(&installed_config_path, &client, Some(&expected_event_source))?;
    let missing_requirements = observed_app_hook_missing_requirements(&checks);
    let eligible_for_observed_app_hook = missing_requirements.is_empty();
    let trust_boundary = if eligible_for_observed_app_hook {
        "installed_config_references_soma_wrapper_path_only: this check supports an observed_app_hook proof but still requires a real event file with matching private event_source and binding_nonce, drain report, and explicit operator confirmation before persistence".to_string()
    } else {
        "installed_config_not_eligible_for_observed_app_hook: missing required wrapper/client/event-spool references; do not persist app-hook proof".to_string()
    };

    Ok(AdapterBindingInstalledConfigCheckOutcome {
        client,
        installed_config_path: installed_config_path.to_string_lossy().into_owned(),
        proof_level: ClientBindingProofLevel::ObservedAppHook,
        eligible_for_observed_app_hook,
        missing_requirements,
        trust_boundary,
        checks,
    })
}

pub fn run_discover_installed_config_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingInstalledConfigDiscoveryOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let config_root = resolve_config_root(args)?;
    let paths = discover_installed_config_paths(args, &client, &config_root);
    let private_client_target_relpaths = private_client_target_installed_config_relpaths(&client);
    let candidates: Vec<_> = paths
        .iter()
        .map(|path| {
            discover_one_installed_config(
                path,
                &client,
                &expected_event_source,
                manifest_path.as_deref(),
            )
        })
        .collect();
    let candidates_found = candidates.iter().filter(|candidate| candidate.exists).count();
    let eligible_candidates =
        candidates.iter().filter(|candidate| candidate.eligible_for_observed_app_hook).count();
    let eligible_setup_artifact_paths = candidates
        .iter()
        .filter(|candidate| candidate.eligible_for_observed_app_hook)
        .filter(|candidate| {
            !is_private_client_target_config_path(&candidate.path, private_client_target_relpaths)
        })
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let eligible_private_client_target_paths = candidates
        .iter()
        .filter(|candidate| candidate.eligible_for_observed_app_hook)
        .filter(|candidate| {
            is_private_client_target_config_path(&candidate.path, private_client_target_relpaths)
        })
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let private_client_target_candidate_paths = candidates
        .iter()
        .filter(|candidate| {
            is_private_client_target_config_path(&candidate.path, private_client_target_relpaths)
        })
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();

    Ok(AdapterBindingInstalledConfigDiscoveryOutcome {
        client,
        manifest_path,
        expected_event_source,
        config_root: config_root.to_string_lossy().into_owned(),
        known_private_client_target_relpaths: private_client_target_relpaths
            .iter()
            .map(|path| (*path).to_string())
            .collect(),
        candidates_found,
        eligible_candidates,
        setup_artifact_eligible_candidates: eligible_setup_artifact_paths.len(),
        private_client_target_eligible_candidates: eligible_private_client_target_paths.len(),
        eligible_setup_artifact_paths,
        eligible_private_client_target_paths,
        private_client_target_candidate_paths,
        candidates,
        trust_boundary: "discover_installed_config_is_read_only: scans likely config paths and applies the installed-config preflight only; it records no proof row, verifies no app invocation, and does not promote cloud drafts".to_string(),
    })
}

fn first_eligible_installed_config_candidate(
    discovery: &AdapterBindingInstalledConfigDiscoveryOutcome,
) -> Option<&InstalledConfigCandidate> {
    let private_client_target_relpaths =
        private_client_target_installed_config_relpaths(&discovery.client);
    discovery
        .candidates
        .iter()
        .find(|candidate| {
            candidate.eligible_for_observed_app_hook
                && is_private_client_target_config_path(
                    &candidate.path,
                    private_client_target_relpaths,
                )
        })
        .or_else(|| {
            discovery.candidates.iter().find(|candidate| candidate.eligible_for_observed_app_hook)
        })
}

pub fn run_real_app_proof_kit_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingRealAppProofKitOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let installed_config_path = args
        .installed_config
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned());
    let event_jsonl_path = args
        .event_jsonl
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned());
    let review_render_report_path = args
        .review_render_report
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned());
    let render_evidence_path = args
        .render_evidence
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned());
    let review_action_report_path = args
        .review_action_report
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned());

    let installed_config_scan = installed_config_path
        .as_deref()
        .map(|path| scan_installed_config(Path::new(path), &client, Some(&expected_event_source)));
    let expected_binding_nonce = installed_config_scan
        .as_ref()
        .and_then(|scan| scan.as_ref().ok())
        .and_then(|scan| scan.binding_nonce.as_deref());
    let event_scan = event_jsonl_path.as_deref().map(|path| {
        scan_event_jsonl(
            Path::new(path),
            &client,
            Some(&expected_event_source),
            expected_binding_nonce,
        )
    });
    let review_render_file_scan = review_render_report_path
        .as_deref()
        .map(|path| scan_file_fingerprint(Path::new(path), "review render report"));
    let review_render_scan = review_render_report_path.as_deref().map(|path| {
        let report = read_json_file(Path::new(path), "review render report")?;
        validate_review_render(Some(&report), &client)?;
        Ok::<Value, AdapterBindingProofError>(report)
    });
    let render_evidence_scan =
        render_evidence_path.as_deref().map(|path| scan_render_evidence(Path::new(path)));
    let review_action_report_scan =
        review_action_report_path.as_deref().map(|path| scan_review_action_report(Path::new(path)));

    let mut artifacts = vec![
        real_app_installed_config_artifact(
            installed_config_path.as_deref(),
            installed_config_scan.as_ref(),
        ),
        real_app_event_jsonl_artifact(
            event_jsonl_path.as_deref(),
            event_scan.as_ref(),
            installed_config_scan.as_ref().and_then(|scan| scan.as_ref().ok()),
        ),
        real_app_review_render_artifact(
            &client,
            review_render_report_path.as_deref(),
            review_render_scan.as_ref(),
        ),
        real_app_render_evidence_artifact(
            render_evidence_path.as_deref(),
            render_evidence_scan.as_ref(),
            &client,
            review_render_file_scan.as_ref(),
            review_render_scan.as_ref(),
        ),
        real_app_review_action_artifact(
            &client,
            review_action_report_path.as_deref(),
            review_action_report_scan.as_ref(),
        ),
    ];
    if args.require_private_target_config_for_app_hook {
        artifacts.insert(
            1,
            real_app_private_client_target_config_artifact(
                &client,
                installed_config_path.as_deref(),
            ),
        );
    }
    let commands = real_app_proof_commands(
        &client,
        manifest_path.as_deref(),
        installed_config_path.as_deref(),
        event_jsonl_path.as_deref(),
        review_render_report_path.as_deref(),
        render_evidence_path.as_deref(),
        review_action_report_path.as_deref(),
    );
    let proof_readiness = real_app_proof_readiness(
        &artifacts,
        &commands,
        args.require_private_target_config_for_app_hook,
    );
    let mut app_hook_required_checks = vec![
        "installed config references lifecycle/spool wrapper, client, private event_source, and binding_nonce".to_string(),
        "event JSONL includes matching private event_source".to_string(),
        "event JSONL includes matching binding_nonce from installed config".to_string(),
        "event JSONL carries soma_adapter_spool_append_v1 writer metadata".to_string(),
        "event observed_at_ns is at or after installed config modified_at".to_string(),
    ];
    if args.require_private_target_config_for_app_hook {
        app_hook_required_checks.insert(
            1,
            "automated proof-session readiness treats only a known private-client target config path as installed; setup artifacts can guide installation but cannot make app-hook proof ready".to_string(),
        );
    }
    let acceptance_gates = vec![
        RealAppProofAcceptanceGate {
            proof_level: ClientBindingProofLevel::ObservedAppHook,
            requires_operator_confirmation: true,
            required_artifacts: vec![
                "installed_config".to_string(),
                "event_jsonl".to_string(),
                "optional_drain_report".to_string(),
            ],
            required_checks: app_hook_required_checks,
        },
        RealAppProofAcceptanceGate {
            proof_level: ClientBindingProofLevel::ObservedInClientRender,
            requires_operator_confirmation: true,
            required_artifacts: vec![
                "review_render_report".to_string(),
                "render_evidence".to_string(),
            ],
            required_checks: vec![
                "review render report matches the target client".to_string(),
                "render evidence uses soma.in_client_render_evidence.v1 and matches the target client".to_string(),
                "render evidence review_render_fingerprint matches the supplied review render report".to_string(),
                "render evidence echoes the review workbench and interaction contract versions from the supplied report".to_string(),
                "render evidence rendered_control_ids cover the current review action control_ids from the supplied report".to_string(),
                "artifact path, byte length, and stable fingerprint are stored only as evidence metadata".to_string(),
            ],
        },
        RealAppProofAcceptanceGate {
            proof_level: ClientBindingProofLevel::ObservedReviewAction,
            requires_operator_confirmation: true,
            required_artifacts: vec![
                "installed_config".to_string(),
                "review_action_report".to_string(),
            ],
            required_checks: vec![
                "review action report was produced by soma_review_action or soma context review-action".to_string(),
                "report carries control_binding_verified=true and a non-empty control_id".to_string(),
                "report records at least one verification event with non-cloud evidence".to_string(),
                "control_id was present globally and inside the visible action_buttons surface in a prior observed_in_client_render proof".to_string(),
                "review action proof uses the same installed config fingerprint and binding_nonce as the linked render proof".to_string(),
            ],
        },
    ];

    Ok(AdapterBindingRealAppProofKitOutcome {
        client,
        manifest_path,
        expected_event_source,
        artifacts,
        proof_readiness,
        commands,
        acceptance_gates,
        unproven_until: vec![
            "the private app actually calls the configured wrapper".to_string(),
            "observed_app_hook is recorded with matching installed-config and event evidence plus operator confirmation".to_string(),
            "observed_in_client_render is recorded with a real app render artifact plus operator confirmation".to_string(),
            "observed_review_action is recorded with a storage-gated review action report whose control_id matched prior render evidence".to_string(),
            "artifact replay remains stable after storage; append-only event_jsonl growth must preserve the recorded prefix fingerprint".to_string(),
        ],
        trust_boundary: "real_app_proof_kit_is_read_only: renders operator steps and optional local preflight scans only; it records no proof row, verifies no claim, promotes no cloud draft, applies no proposal, and does not prove private app invocation by itself".to_string(),
    })
}

pub fn run_evidence_bundle_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingEvidenceBundleOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let config_root = resolve_config_root(args)?;
    let (mut binding_nonce, mut generated_binding_nonce) = resolve_binding_nonce_for_prepare(args)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);

    let mut bundle_args = args.clone();
    bundle_args.client = Some(client.clone());
    bundle_args.binding_nonce = Some(binding_nonce.clone());
    bundle_args.write_installed_config = None;
    bundle_args.render_installed_config = false;
    bundle_args.prepare_installed_config = false;
    bundle_args.discover_installed_config = false;
    bundle_args.real_app_proof_kit = false;
    bundle_args.evidence_bundle = false;
    bundle_args.check_installed_config = false;
    bundle_args.status = false;
    bundle_args.list = false;
    bundle_args.verify_evidence_artifacts = false;
    if bundle_args.event_jsonl.is_none() {
        let default_event_jsonl = config_root.join(".soma").join("adapter").join("events.jsonl");
        if default_event_jsonl.is_file() {
            bundle_args.event_jsonl =
                Some(canonical_or_raw(&default_event_jsonl).to_string_lossy().into_owned());
        }
    }

    let readiness = run_status_blocking(&bundle_args, ctx)?;
    let installed_config_discovery = run_discover_installed_config_blocking(&bundle_args)?;
    let auto_selected_installed_config = bundle_args.installed_config.is_none();
    if auto_selected_installed_config {
        if let Some(candidate) =
            first_eligible_installed_config_candidate(&installed_config_discovery)
        {
            bundle_args.installed_config = Some(candidate.path.clone());
        }
    }
    bundle_args.require_private_target_config_for_app_hook = auto_selected_installed_config;
    if args.binding_nonce.is_none() {
        if let Some(path) = bundle_args.installed_config.as_deref() {
            if let Ok(scan) =
                scan_installed_config(Path::new(path), &client, Some(&expected_event_source))
            {
                if let Some(nonce) = scan.binding_nonce {
                    binding_nonce = nonce;
                    generated_binding_nonce = false;
                    bundle_args.binding_nonce = Some(binding_nonce.clone());
                }
            }
        }
    }
    let config_root_string = config_root.to_string_lossy().into_owned();
    let artifact_dir = args
        .artifact_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            durable_client_artifact_dir_for_root(&client, &binding_nonce, Some(&config_root_string))
        });
    select_existing_durable_artifacts(&mut bundle_args, &artifact_dir);
    let continue_collector_probe = continue_devdata_collector_probe(
        &client,
        &config_root,
        bundle_args.installed_config.as_deref(),
        bundle_args.event_jsonl.as_deref(),
    );
    let installed_config_preview = run_render_installed_config_blocking(&bundle_args)?;
    let real_app_proof_kit = run_real_app_proof_kit_blocking(&bundle_args)?;
    let mut operator_flow = client_binding_operator_flow(
        &client,
        manifest_path.as_deref(),
        Some(&expected_event_source),
        &binding_nonce,
        Some(&artifact_dir),
        Some(&config_root_string),
        bundle_args.installed_config.as_deref(),
        bundle_args.event_jsonl.as_deref(),
        bundle_args.review_render_report.as_deref(),
        bundle_args.render_evidence.as_deref(),
        bundle_args.review_action_report.as_deref(),
    );
    insert_continue_collector_step(
        &mut operator_flow,
        &client,
        bundle_args.installed_config.as_deref(),
        bundle_args.event_jsonl.as_deref(),
        continue_collector_probe.as_ref(),
    );
    let blocking_gaps =
        client_binding_evidence_bundle_gaps(&readiness, &installed_config_discovery);
    let current_private_target_config_ready =
        installed_config_discovery.private_client_target_eligible_candidates > 0;
    let proof_session = client_binding_proof_session(
        &client,
        &artifact_dir,
        &readiness,
        &real_app_proof_kit,
        &operator_flow,
        current_private_target_config_ready,
        continue_collector_probe.as_ref(),
    );

    Ok(AdapterBindingEvidenceBundleOutcome {
        client,
        manifest_path,
        expected_event_source,
        binding_nonce,
        generated_binding_nonce,
        config_root: config_root_string,
        event_jsonl_path: bundle_args.event_jsonl,
        continue_devdata_collector: continue_collector_probe,
        readiness,
        installed_config_discovery,
        installed_config_preview,
        real_app_proof_kit,
        operator_flow,
        proof_session,
        blocking_gaps,
        trust_boundary: "client_binding_evidence_bundle_is_read_only: composes readiness, installed-config discovery, proof-free config preview, and real-app proof-kit guidance only; it records no proof row, verifies no app invocation, records no verification event, promotes no cloud draft, applies no proposal, and does not prove private client installation or in-client rendering by itself".to_string(),
    })
}

pub fn run_proof_session_blocking(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
) -> Result<AdapterBindingProofSessionOutcome, AdapterBindingProofError> {
    if args.client.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none()
        && args.manifest.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none()
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "--proof-session requires --client or --manifest".to_string(),
        ));
    }
    let bundle = match run_evidence_bundle_blocking(args, ctx) {
        Ok(bundle) => bundle,
        Err(AdapterBindingProofError::Storage(err)) => {
            return degraded_proof_session_storage_unavailable(args, ctx, err.to_string());
        }
        Err(err) => return Err(err),
    };
    let ready_client_count = usize::from(bundle.proof_session.ready_for_private_client_claim);
    let proof_session = bundle.proof_session;
    let status = proof_session.status.clone();
    let release_gate = proof_session.release_gate.clone();
    let ready_for_private_client_claim = proof_session.ready_for_private_client_claim;
    let next_step_id = proof_session.next_step_id.clone();
    let next_command = proof_session.next_command.clone();
    let next_mcp_call = proof_session.next_mcp_call.clone();
    let next_operator_step = proof_session.next_operator_step.clone();
    let runbook_schema = proof_session.runbook.schema.clone();
    let pending_proof_levels = proof_session.pending_proof_levels.clone();
    let blocked_proof_levels = proof_session.blocked_proof_levels.clone();
    let artifact_failures = bundle
        .readiness
        .clients
        .iter()
        .find(|status| status.client == bundle.client)
        .map(|status| status.artifact_failures.clone())
        .unwrap_or_default();
    let operator_next_action_id =
        proof_session_operator_next_action_id(&status, next_step_id.as_deref());
    let operator_next_action_label =
        proof_session_operator_next_action_label(&operator_next_action_id, &bundle.client);
    let app_hook_evidence = client_binding_app_hook_evidence_summary(
        &bundle.client,
        bundle.event_jsonl_path.as_deref(),
        &bundle.expected_event_source,
        &bundle.binding_nonce,
        &proof_session,
        &bundle.operator_flow,
        bundle.continue_devdata_collector.as_ref(),
    );
    let operator_card = proof_session_operator_card(
        &bundle.client,
        &status,
        &release_gate,
        &operator_next_action_id,
        &operator_next_action_label,
        next_step_id.as_deref(),
        next_operator_step.as_ref(),
        next_command.as_ref(),
        ready_for_private_client_claim,
        &pending_proof_levels,
        &blocked_proof_levels,
        &bundle.blocking_gaps,
        &artifact_failures,
        &app_hook_evidence,
    );
    let external_action_safety = operator_card.external_action_safety.clone();
    let external_action = operator_card.external_action.clone();
    Ok(AdapterBindingProofSessionOutcome {
        client: bundle.client,
        manifest_path: bundle.manifest_path,
        expected_event_source: bundle.expected_event_source,
        binding_nonce: bundle.binding_nonce,
        generated_binding_nonce: bundle.generated_binding_nonce,
        config_root: bundle.config_root,
        proof_storage_status: "available".to_string(),
        proof_storage_error: None,
        proof_storage_recovery_commands: Vec::new(),
        status,
        release_gate,
        ready_for_private_client_claim,
        operator_next_action_id,
        operator_next_action_label,
        headline: operator_card.headline.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        proof_session_next_step_id: operator_card.proof_session_next_step_id.clone(),
        next_operator_step_title: operator_card.next_operator_step_title.clone(),
        artifact_failure_count: operator_card.artifact_failure_count,
        artifact_failures: operator_card.artifact_failures.clone(),
        safe_to_claim: operator_card.safe_to_claim.clone(),
        blocked_claims: operator_card.blocked_claims.clone(),
        app_hook_evidence,
        external_action_safety,
        external_action,
        operator_card,
        next_step_id,
        next_command,
        next_mcp_call,
        next_operator_step,
        runbook_schema,
        pending_proof_levels,
        blocked_proof_levels,
        proof_session,
        blocking_gaps: bundle.blocking_gaps,
        operator_flow: bundle.operator_flow,
        proofs_found: bundle.readiness.proofs_found,
        ready_client_count,
        installed_config_eligible_candidates: bundle.installed_config_discovery.eligible_candidates,
        setup_artifact_eligible_candidates: bundle
            .installed_config_discovery
            .setup_artifact_eligible_candidates,
        private_client_target_eligible_candidates: bundle
            .installed_config_discovery
            .private_client_target_eligible_candidates,
        eligible_setup_artifact_paths: bundle
            .installed_config_discovery
            .eligible_setup_artifact_paths,
        eligible_private_client_target_paths: bundle
            .installed_config_discovery
            .eligible_private_client_target_paths,
        private_client_target_candidate_paths: bundle
            .installed_config_discovery
            .private_client_target_candidate_paths,
        trust_boundary: "client_binding_proof_session_outcome_is_read_only: composes stored proof readiness and currently supplied artifact readiness through the evidence bundle contract; writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation, rendering, or review-action execution".to_string(),
    })
}

fn degraded_proof_session_storage_unavailable(
    args: &AdapterBindingProofArgs,
    ctx: &AdapterBindingProofContext,
    error: String,
) -> Result<AdapterBindingProofSessionOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let config_root = resolve_config_root(args)?;
    let (binding_nonce, generated_binding_nonce) = resolve_binding_nonce_for_prepare(args)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let config_root = config_root.to_string_lossy().into_owned();
    let status = "proof_storage_unavailable".to_string();
    let release_gate = "fail".to_string();
    let next_step_id = Some("restore_client_binding_proof_storage_access".to_string());
    let recovery_commands = proof_session_storage_recovery_commands(&client);
    let next_command = recovery_commands.first().cloned();
    let next_operator_step = Some(ClientBindingOperatorFlowStep {
        id: "restore_client_binding_proof_storage_access".to_string(),
        title: "Restore client binding proof storage access".to_string(),
        intent: format!(
            "SOMA cannot read `{}` for client-binding proof rows (`{error}`). Use a readable SOMA_DB or diagnostic DB before claiming private {client} app-hook/render/review-action readiness.",
            ctx.db_path.display()
        ),
        command: next_command.clone(),
        mcp_call: None,
        requires_operator_action: false,
        records_proof: false,
        trust_boundary: "proof_session_storage_recovery_step_is_read_only: reports unreadable proof storage and suggests diagnostic commands only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft".to_string(),
    });
    let blocking_reason = format!("proof_storage_unavailable: {error}");
    let stages = required_private_client_proof_levels()
        .iter()
        .map(|proof_level| ClientBindingProofSessionStage {
            proof_level: *proof_level,
            ledger_status: "unknown_storage_unavailable".to_string(),
            artifact_status: None,
            ready_to_record_now: false,
            blocking_reasons: vec![blocking_reason.clone()],
            command: None,
            mcp_call: None,
            trust_boundary: "proof_session_storage_unavailable_stage_is_read_only: storage must be readable before stored proof levels can be inspected or recorded; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft".to_string(),
        })
        .collect::<Vec<_>>();
    let pending_proof_levels = required_private_client_proof_levels().to_vec();
    let blocked_proof_levels = pending_proof_levels.clone();
    let completion_criteria = vec![
        "proof storage is readable from the current client/session".to_string(),
        "observed_app_hook proof can be inspected before any app-hook readiness claim".to_string(),
        "observed_in_client_render proof can be inspected before any in-client review claim"
            .to_string(),
        "observed_review_action proof can be inspected before any review-action claim".to_string(),
    ];
    let operator_flow = vec![next_operator_step.clone().expect("storage recovery step")];
    let runbook = client_binding_proof_session_runbook(
        &client,
        &status,
        &release_gate,
        next_step_id.as_deref(),
        next_operator_step.as_ref(),
        &operator_flow,
        &stages,
        &completion_criteria,
        &durable_client_artifact_dir(&client),
    );
    let proof_session = ClientBindingProofSession {
        client: client.clone(),
        status: status.clone(),
        release_gate: release_gate.clone(),
        ready_for_private_client_claim: false,
        completed_proof_levels: Vec::new(),
        pending_proof_levels: pending_proof_levels.clone(),
        ready_to_record_proof_levels: Vec::new(),
        blocked_proof_levels: blocked_proof_levels.clone(),
        next_step_id: next_step_id.clone(),
        next_command: next_command.clone(),
        next_mcp_call: None,
        next_operator_step: next_operator_step.clone(),
        external_action: None,
        stages,
        runbook,
        completion_criteria,
        trust_boundary: "client_binding_proof_session_storage_unavailable_is_read_only: reports that proof storage could not be read; records no proof row, creates no verification event, installs no hook, applies no proposal, and promotes no cloud draft".to_string(),
    };
    let operator_next_action_id =
        proof_session_operator_next_action_id(&status, next_step_id.as_deref());
    let operator_next_action_label =
        proof_session_operator_next_action_label(&operator_next_action_id, &client);
    let blocking_gaps = vec![blocking_reason];
    let app_hook_evidence = client_binding_app_hook_evidence_summary(
        &client,
        args.event_jsonl.as_deref(),
        &expected_event_source,
        &binding_nonce,
        &proof_session,
        &operator_flow,
        None,
    );
    let operator_card = proof_session_operator_card(
        &client,
        &status,
        &release_gate,
        &operator_next_action_id,
        &operator_next_action_label,
        next_step_id.as_deref(),
        next_operator_step.as_ref(),
        next_command.as_ref(),
        false,
        &pending_proof_levels,
        &blocked_proof_levels,
        &blocking_gaps,
        &[],
        &app_hook_evidence,
    );
    Ok(AdapterBindingProofSessionOutcome {
        client,
        manifest_path,
        expected_event_source,
        binding_nonce,
        generated_binding_nonce,
        config_root,
        proof_storage_status: "unavailable".to_string(),
        proof_storage_error: Some(error),
        proof_storage_recovery_commands: recovery_commands,
        status,
        release_gate,
        ready_for_private_client_claim: false,
        operator_next_action_id,
        operator_next_action_label,
        headline: operator_card.headline.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        proof_session_next_step_id: operator_card.proof_session_next_step_id.clone(),
        next_operator_step_title: operator_card.next_operator_step_title.clone(),
        artifact_failure_count: operator_card.artifact_failure_count,
        artifact_failures: operator_card.artifact_failures.clone(),
        safe_to_claim: operator_card.safe_to_claim.clone(),
        blocked_claims: operator_card.blocked_claims.clone(),
        app_hook_evidence,
        external_action_safety: None,
        external_action: None,
        operator_card,
        next_step_id,
        next_command,
        next_mcp_call: None,
        next_operator_step,
        runbook_schema: proof_session.runbook.schema.clone(),
        pending_proof_levels,
        blocked_proof_levels,
        proof_session,
        blocking_gaps,
        operator_flow,
        proofs_found: 0,
        ready_client_count: 0,
        installed_config_eligible_candidates: 0,
        setup_artifact_eligible_candidates: 0,
        private_client_target_eligible_candidates: 0,
        eligible_setup_artifact_paths: Vec::new(),
        eligible_private_client_target_paths: Vec::new(),
        private_client_target_candidate_paths: Vec::new(),
        trust_boundary: "client_binding_proof_session_storage_unavailable_outcome_is_read_only: composes a degraded proof-session handoff after storage open failed; writes no files, records no proof row, creates no verification event, installs no hook, applies no proposal, and promotes no cloud draft".to_string(),
    })
}

fn proof_session_storage_diagnostic_db_path() -> String {
    std::env::temp_dir()
        .join(format!("soma-client-binding-proof-session-diagnostic-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

fn proof_session_storage_recovery_commands(client: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--proof-session".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--db-path".to_string(),
            proof_session_storage_diagnostic_db_path(),
            "--brief".to_string(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--proof-session".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--db-path".to_string(),
            "<readable-soma.db>".to_string(),
            "--brief".to_string(),
        ],
        vec![
            "soma".to_string(),
            "clients".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--brief".to_string(),
        ],
        vec!["soma".to_string(), "diagnose".to_string()],
    ]
}

pub fn render_proof_session_brief(outcome: &AdapterBindingProofSessionOutcome) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "SOMA client binding proof-session brief");
    let _ = writeln!(out, "Client: {}", outcome.client);
    if let Some(manifest_path) = outcome.manifest_path.as_deref() {
        let _ = writeln!(out, "Manifest: {manifest_path}");
    }
    let _ = writeln!(
        out,
        "Status: {} release_gate={} ready={}",
        outcome.status, outcome.release_gate, outcome.ready_for_private_client_claim
    );
    if outcome.proof_storage_status != "available" {
        let _ = writeln!(out, "Proof storage: {}", outcome.proof_storage_status);
        if let Some(error) = outcome.proof_storage_error.as_deref() {
            let _ = writeln!(out, "Proof storage error: {error}");
        }
        for command in outcome.proof_storage_recovery_commands.iter().take(4) {
            let _ = writeln!(out, "Recovery: {}", command_line(command));
        }
    }
    let _ = writeln!(
        out,
        "Next action: {} ({})",
        outcome.operator_next_action_id, outcome.operator_next_action_label
    );

    match outcome.next_operator_step.as_ref() {
        Some(step) => {
            let step_id = outcome.next_step_id.as_deref().unwrap_or(step.id.as_str());
            let _ = writeln!(out, "Proof step: {step_id} - {}", step.title);
            let _ = writeln!(out, "Why: {}", step.intent);
        }
        None => {
            let step_id = outcome.next_step_id.as_deref().unwrap_or("none");
            let _ = writeln!(out, "Proof step: {step_id}");
            let _ = writeln!(out, "Why: {}", outcome.operator_card.primary_next_step);
        }
    }

    if let Some(command) =
        outcome.next_command.as_ref().filter(|command| !command.is_empty()).or_else(|| {
            (!outcome.operator_card.primary_next_command.is_empty())
                .then_some(&outcome.operator_card.primary_next_command)
        })
    {
        let _ = writeln!(out, "Command: {}", command_line(command));
    }
    if let Some(mcp_call) = outcome.next_mcp_call.as_ref() {
        let _ = writeln!(out, "MCP: {} {}", mcp_call.tool, mcp_call.arguments);
    }

    let app_hook = &outcome.app_hook_evidence;
    let event_jsonl = app_hook.event_jsonl_path.as_deref().unwrap_or("none");
    let _ = writeln!(
        out,
        "App hook evidence: status={} ready={} event_jsonl={} expected_event_source={} binding_nonce={} blockers={}",
        app_hook.status,
        app_hook.ready_to_record_now,
        event_jsonl,
        app_hook.expected_event_source,
        app_hook.binding_nonce,
        app_hook.blocking_reason_count
    );
    if !app_hook.blocking_reasons.is_empty() {
        let _ =
            writeln!(out, "App hook blockers: {}", string_list_or_none(&app_hook.blocking_reasons));
    }
    if let Some(command) = app_hook.readiness_probe_command.as_ref() {
        let _ = writeln!(out, "App hook readiness probe: {}", command_line(command));
    }
    if let Some(collector) = app_hook.continue_devdata_collector.as_ref() {
        let _ = writeln!(
            out,
            "Continue collector: status={} listening={} endpoint={}:{} devdata_visible={}",
            collector.collector_status,
            collector.collector_listening,
            collector.collector_host,
            collector.collector_port,
            collector.devdata_destination_visible
        );
        if let Some(error) = collector.collector_error.as_deref() {
            let _ = writeln!(out, "Continue collector probe error: {error}");
        }
        if let Some(command) = collector.status_command.as_ref() {
            let _ = writeln!(out, "Continue collector status check: {}", command_line(command));
        }
        if let Some(command) = collector.start_command.as_ref() {
            let _ = writeln!(out, "Continue collector start: {}", command_line(command));
        }
    }

    if let Some(external_action) = outcome.external_action.as_ref() {
        let _ = writeln!(
            out,
            "External action: {} ({})",
            external_action.action_label, external_action.action_kind
        );
        let _ = writeln!(
            out,
            "External proof after success: {}",
            external_action.proof_after_success_step_id
        );
        if let Some(step) =
            proof_step_after_external_success(outcome, &external_action.proof_after_success_step_id)
        {
            if let Some(command) = step.command.as_ref().filter(|command| !command.is_empty()) {
                let _ = writeln!(out, "After success proof command: {}", command_line(command));
            }
            if let Some(mcp_call) = step.mcp_call.as_ref() {
                let _ = writeln!(
                    out,
                    "After success proof MCP: {} {}",
                    mcp_call.tool, mcp_call.arguments
                );
            }
            let _ = writeln!(out, "After success proof boundary: {}", step.trust_boundary);
        }
    }
    if let Some(safety) = outcome
        .external_action_safety
        .as_ref()
        .or(outcome.operator_card.external_action_safety.as_ref())
    {
        let _ = writeln!(
            out,
            "External safety: classification={} requires_confirmation={} may_transmit_prompt_to_provider={}",
            safety.classification,
            safety.requires_operator_confirmation_before_submission,
            safety.may_transmit_prompt_to_provider
        );
        let _ = writeln!(out, "Minimal prompt: {}", safety.suggested_minimal_test_prompt);
        let _ =
            writeln!(out, "Forbidden inputs: {}", string_list_or_none(&safety.forbidden_inputs));
    }

    let session = &outcome.proof_session;
    let _ = writeln!(
        out,
        "Proof levels: completed={} pending={} ready_to_record={} blocked={}",
        proof_level_list(&session.completed_proof_levels),
        proof_level_list(&session.pending_proof_levels),
        proof_level_list(&session.ready_to_record_proof_levels),
        proof_level_list(&session.blocked_proof_levels)
    );

    let progress = &session.runbook.progress;
    let _ = writeln!(
        out,
        "Runbook: steps={} operator_actions={} proof_recording={} ready_now={} blocked_proofs={} blocking_reasons={} release_ready={}",
        progress.total_step_count,
        progress.operator_action_step_count,
        progress.proof_recording_step_count,
        progress.ready_now_step_count,
        progress.blocked_proof_step_count,
        progress.blocking_reason_count,
        progress.release_ready
    );
    let _ = writeln!(out, "Durable artifact dir: {}", session.runbook.durable_artifact_dir);
    let _ = writeln!(
        out,
        "Durable artifact dir write: {}",
        session.runbook.durable_artifact_dir_write_status
    );
    for suggestion in &session.runbook.suggested_artifact_paths {
        let _ = writeln!(
            out,
            "Suggested artifact: kind={} path={} intent={}",
            suggestion.artifact_kind, suggestion.path, suggestion.intent
        );
    }
    if let Some(dir) = session.runbook.workspace_fallback_artifact_dir.as_deref() {
        let _ = writeln!(out, "Workspace fallback artifact dir: {dir}");
        for suggestion in &session.runbook.workspace_fallback_artifact_paths {
            let _ = writeln!(
                out,
                "Workspace fallback artifact: kind={} path={} intent={}",
                suggestion.artifact_kind, suggestion.path, suggestion.intent
            );
        }
        for command in &session.runbook.workspace_fallback_commands {
            let _ = writeln!(out, "Workspace fallback command: {}", command_line(command));
        }
    }

    let ready_steps: Vec<_> = session.runbook.steps.iter().filter(|step| step.ready_now).collect();
    if ready_steps.is_empty() {
        let _ = writeln!(out, "Ready now: none");
    } else {
        let _ = writeln!(out, "Ready now:");
        for step in ready_steps.into_iter().take(5) {
            let _ = writeln!(out, "- {}: {}", step.id, step.title);
            let display_artifact_path =
                workspace_fallback_artifact_path_for_step(&session.runbook, step.id.as_str())
                    .or_else(|| step.suggested_artifact_path.clone());
            if let Some(path) = display_artifact_path.as_deref() {
                let _ = writeln!(out, "  artifact: {path}");
            }
            let display_command =
                workspace_fallback_command_for_step(&session.runbook, step.id.as_str())
                    .or_else(|| step.command.clone());
            if let Some(command) = display_command.as_ref().filter(|command| !command.is_empty()) {
                let _ = writeln!(out, "  command: {}", command_line(command));
            }
        }
    }

    let blocked_steps: Vec<_> =
        session.runbook.steps.iter().filter(|step| !step.blocking_reasons.is_empty()).collect();
    if !blocked_steps.is_empty() {
        let _ = writeln!(out, "Blocked steps:");
        for step in blocked_steps.into_iter().take(5) {
            let _ = writeln!(out, "- {}: {}", step.id, string_list_or_none(&step.blocking_reasons));
            if step.records_proof {
                if let Some(command) = step.command.as_ref().filter(|command| !command.is_empty()) {
                    let _ = writeln!(
                        out,
                        "  proof command after evidence is refreshed: {}",
                        command_line(command)
                    );
                }
                if let Some(mcp_call) = step.mcp_call.as_ref() {
                    let _ = writeln!(
                        out,
                        "  proof MCP after evidence is refreshed: {} {}",
                        mcp_call.tool, mcp_call.arguments
                    );
                }
                let _ = writeln!(out, "  proof boundary: {}", step.trust_boundary);
            }
        }
    }

    let _ = writeln!(out, "Blocking gaps: {}", string_list_or_none(&outcome.blocking_gaps));
    let _ = writeln!(
        out,
        "Blocked claims: {}",
        string_list_or_none(&outcome.operator_card.blocked_claims)
    );
    let _ =
        writeln!(out, "Safe claims: {}", string_list_or_none(&outcome.operator_card.safe_to_claim));
    let _ = writeln!(out, "Trust boundary: {}", outcome.trust_boundary);
    out
}

fn proof_step_after_external_success<'a>(
    outcome: &'a AdapterBindingProofSessionOutcome,
    proof_after_success_step_id: &str,
) -> Option<&'a ClientBindingOperatorFlowStep> {
    outcome
        .operator_flow
        .iter()
        .find(|step| step.id == proof_after_success_step_id && step.records_proof)
}

fn proof_level_list(levels: &[ClientBindingProofLevel]) -> String {
    if levels.is_empty() {
        return "none".to_string();
    }
    levels.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

fn string_list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        return "none".to_string();
    }
    values.join("; ")
}

fn command_line(command: &[String]) -> String {
    command.iter().map(|part| shell_quote_arg(part)).collect::<Vec<_>>().join(" ")
}

fn shell_quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(&byte))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[allow(clippy::too_many_arguments)]
fn proof_session_operator_card(
    client: &str,
    status: &str,
    release_gate: &str,
    operator_next_action_id: &str,
    operator_next_action_label: &str,
    next_step_id: Option<&str>,
    next_operator_step: Option<&ClientBindingOperatorFlowStep>,
    next_command: Option<&Vec<String>>,
    ready_for_private_client_claim: bool,
    pending_proof_levels: &[ClientBindingProofLevel],
    blocked_proof_levels: &[ClientBindingProofLevel],
    blocking_gaps: &[String],
    artifact_failures: &[ClientBindingArtifactFailure],
    app_hook_evidence: &ClientBindingAppHookEvidenceSummary,
) -> ClientBindingProofSessionOperatorCard {
    let display_name = private_client_display_name(client);
    let headline = match operator_next_action_id {
        "client_binding_release_gate_passed" => {
            format!("{display_name} private-client release proof is ready.")
        }
        "inspect_render_evidence_packet_for_artifact_repair" => {
            format!("{display_name} render evidence needs fresh private-client UI observation.")
        }
        "refresh_invalid_client_binding_artifacts" => {
            format!("{display_name} proof artifacts changed or failed replay.")
        }
        "restore_client_binding_proof_storage_access" => {
            format!("{display_name} proof storage is unreadable.")
        }
        "record_release_grade_private_client_proof" => {
            format!("{display_name} proof exists, but release-grade evidence is still required.")
        }
        "write_or_install_private_client_binding_config" => {
            format!("Install the {display_name} SOMA binding config before app-hook proof.")
        }
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            format!("Trigger a real {display_name} private app hook.")
        }
        "check_continue_devdata_collector_status" => {
            "Check Continue dev-data collector status.".to_string()
        }
        "record_observed_app_hook_from_real_event_after_operator_confirmation" => {
            format!("Record {display_name} app-hook proof after real-event confirmation.")
        }
        "render_client_review_surface" => {
            format!("Render the {display_name} review surface before UI proof.")
        }
        "capture_in_client_render_evidence" => {
            format!("Capture structured {display_name} in-client render evidence.")
        }
        "record_observed_in_client_render_after_operator_confirmation" => {
            format!("Record {display_name} in-client render proof after confirmation.")
        }
        "execute_rendered_review_control" => {
            format!("Execute a rendered {display_name} review control.")
        }
        "record_observed_review_action_after_operator_confirmation" => {
            format!("Record {display_name} review-action proof after confirmation.")
        }
        _ => format!("Continue the {display_name} proof session."),
    };
    let primary_next_step = match operator_next_action_id {
        "client_binding_release_gate_passed" => {
            "Rerun product hardening with client-binding readiness required before claiming release."
                .to_string()
        }
        "inspect_render_evidence_packet_for_artifact_repair" => {
            format!(
                "Inspect or regenerate the proof-free render evidence packet, replace visible-render placeholders from the real {display_name} UI, then record observed_in_client_render only with explicit operator confirmation."
            )
        }
        "refresh_invalid_client_binding_artifacts" => {
            "Replay stored proof artifacts, inspect changed or missing files, then re-record stale proof rows from fresh real-client evidence."
                .to_string()
        }
        "restore_client_binding_proof_storage_access" => {
            "Restore access to a readable SOMA_DB before claiming private-client proof readiness."
                .to_string()
        }
        _ => next_operator_step
            .map(|step| step.intent.clone())
            .unwrap_or_else(|| "Inspect the proof session status and follow the next proof step.".to_string()),
    };
    let primary_next_command = match operator_next_action_id {
        "client_binding_release_gate_passed" => {
            vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--status".to_string(),
                "--client".to_string(),
                client.to_string(),
                "--json".to_string(),
            ]
        }
        "inspect_render_evidence_packet_for_artifact_repair" => {
            next_command.cloned().unwrap_or_else(|| {
                vec![
                    "soma".to_string(),
                    "adapter-binding-proof".to_string(),
                    "--client".to_string(),
                    client.to_string(),
                    "--proof-session".to_string(),
                    "--brief".to_string(),
                ]
            })
        }
        "refresh_invalid_client_binding_artifacts" => {
            vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--verify-evidence-artifacts".to_string(),
                "--client".to_string(),
                client.to_string(),
                "--json".to_string(),
            ]
        }
        _ => next_command.cloned().unwrap_or_else(|| {
            vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--proof-session".to_string(),
                "--client".to_string(),
                client.to_string(),
                "--json".to_string(),
            ]
        }),
    };
    let external_action_safety =
        product_hardening_external_action_safety(client, operator_next_action_id);
    let external_action = client_binding_external_operator_action(
        client,
        operator_next_action_id,
        operator_next_action_label,
        next_step_id.unwrap_or_default(),
        next_command,
    );

    let mut safe_to_claim = Vec::new();
    let mut blocked_claims = Vec::new();
    if ready_for_private_client_claim {
        safe_to_claim.push(
            "Release-gate proof rows replay cleanly for app-hook, in-client-render, and review-action evidence."
                .to_string(),
        );
    } else {
        blocked_claims.push(
            "Private-client readiness is not claimable until app-hook, render, and review-action proof all pass with release-grade evidence."
                .to_string(),
        );
    }
    if release_gate == "pass" {
        safe_to_claim.push("The proof-session release gate is pass.".to_string());
    } else {
        blocked_claims.push("The proof-session release gate is fail.".to_string());
    }
    if !pending_proof_levels.is_empty() {
        blocked_claims.push(format!(
            "Pending proof levels: {}.",
            pending_proof_levels.iter().map(|level| level.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if !blocked_proof_levels.is_empty() {
        blocked_claims.push(format!(
            "Blocked proof levels: {}.",
            blocked_proof_levels.iter().map(|level| level.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    blocked_claims.extend(blocking_gaps.iter().map(|gap| format!("Blocking gap: {gap}.")));
    for failure in artifact_failures.iter().take(5) {
        let path = failure.path.as_deref().unwrap_or("<unknown>");
        blocked_claims.push(format!(
            "Artifact replay failed for proof {} {} `{}` at `{}` with status {}.",
            failure.proof_id,
            failure.proof_level.as_str(),
            failure.kind,
            path,
            failure.status.as_str()
        ));
    }

    ClientBindingProofSessionOperatorCard {
        source: "soma_client_binding_proof_session.operator_card.v1",
        client: client.to_string(),
        status: status.to_string(),
        release_gate: release_gate.to_string(),
        operator_next_action_id: operator_next_action_id.to_string(),
        operator_next_action_label: operator_next_action_label.to_string(),
        headline,
        primary_next_step,
        primary_next_command,
        external_action_safety,
        external_action,
        proof_session_next_step_id: next_step_id.map(str::to_string),
        next_operator_step_title: next_operator_step.map(|step| step.title.clone()),
        ready_for_private_client_claim,
        pending_proof_level_count: pending_proof_levels.len(),
        blocked_proof_level_count: blocked_proof_levels.len(),
        blocking_gap_count: blocking_gaps.len(),
        artifact_failure_count: artifact_failures.len(),
        artifact_failures: artifact_failures.iter().take(5).cloned().collect(),
        safe_to_claim,
        blocked_claims,
        app_hook_evidence: app_hook_evidence.clone(),
        trust_boundary:
            "client_binding_proof_session_operator_card_is_read_only: translates proof-session status into one operator action only; records no proof row, creates no verification event, installs no hook, applies no proposal, and promotes no cloud draft",
    }
}

fn client_binding_app_hook_evidence_summary(
    client: &str,
    event_jsonl_path: Option<&str>,
    expected_event_source: &str,
    binding_nonce: &str,
    proof_session: &ClientBindingProofSession,
    operator_flow: &[ClientBindingOperatorFlowStep],
    continue_devdata_collector: Option<&ContinueDevdataCollectorProbe>,
) -> ClientBindingAppHookEvidenceSummary {
    let app_hook_stage = proof_session
        .stages
        .iter()
        .find(|stage| stage.proof_level == ClientBindingProofLevel::ObservedAppHook);
    let blocking_reasons =
        app_hook_stage.map(|stage| stage.blocking_reasons.clone()).unwrap_or_else(|| {
            vec!["observed_app_hook: no proof-session stage verdict was available".to_string()]
        });
    let ready_to_record_now = app_hook_stage.is_some_and(|stage| stage.ready_to_record_now);
    let status = app_hook_stage.map_or_else(
        || "stage_missing".to_string(),
        |stage| {
            if stage.ledger_status == "stored_verified" {
                "stored_verified".to_string()
            } else if stage.ready_to_record_now {
                "ready_to_record".to_string()
            } else {
                stage.artifact_status.clone().unwrap_or_else(|| stage.ledger_status.clone())
            }
        },
    );
    let readiness_probe_command = operator_flow
        .iter()
        .find(|step| step.id == "trigger_private_client_hook")
        .and_then(|step| step.command.clone());
    let record_proof_command = operator_flow
        .iter()
        .find(|step| step.id == "record_observed_app_hook")
        .and_then(|step| step.command.clone())
        .or_else(|| app_hook_stage.and_then(|stage| stage.command.clone()));

    ClientBindingAppHookEvidenceSummary {
        source: "soma_client_binding_app_hook_evidence_summary.v1",
        client: client.to_string(),
        event_jsonl_path: event_jsonl_path.map(str::to_string),
        expected_event_source: expected_event_source.to_string(),
        binding_nonce: binding_nonce.to_string(),
        status,
        ready_to_record_now,
        blocking_reason_count: blocking_reasons.len(),
        blocking_reasons,
        readiness_probe_command,
        record_proof_command,
        continue_devdata_collector: continue_devdata_collector.cloned(),
        trust_boundary: "app_hook_evidence_summary_is_read_only: mirrors event-jsonl and proof-session blockers only; records no proof row, creates no verification event, executes no hook, applies no proposal, and promotes no cloud draft",
    }
}

fn proof_session_operator_next_action_id(status: &str, next_step_id: Option<&str>) -> String {
    match status {
        "proof_storage_unavailable" => {
            return "restore_client_binding_proof_storage_access".to_string();
        }
        "ready_for_private_client_claim" => {
            return "client_binding_release_gate_passed".to_string()
        }
        "blocked_by_stored_proof_integrity_or_identity" => {
            if next_step_id == Some("capture_in_client_render_evidence") {
                return "capture_in_client_render_evidence".to_string();
            }
            return "refresh_invalid_client_binding_artifacts".to_string();
        }
        "blocked_by_non_release_evidence_source" => {
            return "record_release_grade_private_client_proof".to_string();
        }
        _ => {}
    }

    match next_step_id {
        Some("render_or_write_installed_config" | "install_or_merge_private_client_config") => {
            "write_or_install_private_client_binding_config".to_string()
        }
        Some("trigger_private_client_hook") => {
            "trigger_real_private_client_hook_to_write_private_spool_event".to_string()
        }
        Some("start_continue_devdata_collector_before_real_hook") => {
            "start_continue_devdata_collector_before_real_hook".to_string()
        }
        Some("check_continue_devdata_collector_status") => {
            "check_continue_devdata_collector_status".to_string()
        }
        Some("record_observed_app_hook") => {
            "record_observed_app_hook_from_real_event_after_operator_confirmation".to_string()
        }
        Some("render_review_surface") => "render_client_review_surface".to_string(),
        Some("capture_in_client_render_evidence") => {
            "capture_in_client_render_evidence".to_string()
        }
        Some("record_observed_in_client_render") => {
            "record_observed_in_client_render_after_operator_confirmation".to_string()
        }
        Some("execute_rendered_review_control") => "execute_rendered_review_control".to_string(),
        Some("record_observed_review_action") => {
            "record_observed_review_action_after_operator_confirmation".to_string()
        }
        Some("verify_evidence_artifacts_and_status") => {
            "verify_client_binding_evidence_artifacts".to_string()
        }
        Some(next) => format!("continue_proof_session_{next}"),
        None => "inspect_private_client_readiness".to_string(),
    }
}

fn proof_session_operator_next_action_label(action_id: &str, client: &str) -> String {
    let display_name = private_client_display_name(client);
    match action_id {
        "client_binding_release_gate_passed" => "Release gate passed".to_string(),
        "inspect_render_evidence_packet_for_artifact_repair" => {
            "Inspect render evidence packet".to_string()
        }
        "refresh_invalid_client_binding_artifacts" => {
            "Refresh invalid binding artifacts".to_string()
        }
        "restore_client_binding_proof_storage_access" => "Restore proof storage access".to_string(),
        "record_release_grade_private_client_proof" => {
            "Record release-grade private-client proof".to_string()
        }
        "write_or_install_private_client_binding_config" => {
            format!("Install {display_name} binding config")
        }
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            format!("Trigger real {display_name} hook")
        }
        "start_continue_devdata_collector_before_real_hook" => {
            "Start Continue dev-data collector".to_string()
        }
        "check_continue_devdata_collector_status" => "Check Continue collector status".to_string(),
        "record_observed_app_hook_from_real_event_after_operator_confirmation" => {
            "Record observed app hook after confirmation".to_string()
        }
        "render_client_review_surface" => "Render client review surface".to_string(),
        "capture_in_client_render_evidence" => "Capture in-client render evidence".to_string(),
        "record_observed_in_client_render_after_operator_confirmation" => {
            "Record in-client render proof after confirmation".to_string()
        }
        "execute_rendered_review_control" => "Execute rendered review control".to_string(),
        "record_observed_review_action_after_operator_confirmation" => {
            "Record review-action proof after confirmation".to_string()
        }
        "verify_client_binding_evidence_artifacts" => {
            "Verify client binding evidence artifacts".to_string()
        }
        _ => "Continue proof session".to_string(),
    }
}

fn real_app_installed_config_artifact(
    path: Option<&str>,
    scan: Option<&Result<InstalledConfigScan, AdapterBindingProofError>>,
) -> RealAppProofArtifact {
    match (path, scan) {
        (Some(path), Some(Ok(scan))) => {
            let missing_requirements = observed_app_hook_missing_requirements(scan);
            let status = if missing_requirements.is_empty() { "eligible" } else { "not_eligible" };
            RealAppProofArtifact {
                kind: "installed_config".to_string(),
                path: path.to_string(),
                provided: true,
                status: status.to_string(),
                scan: Some(json!(scan)),
                missing_requirements,
                error: None,
            }
        }
        (Some(path), Some(Err(err))) => RealAppProofArtifact {
            kind: "installed_config".to_string(),
            path: path.to_string(),
            provided: true,
            status: "unreadable".to_string(),
            scan: None,
            missing_requirements: Vec::new(),
            error: Some(err.to_string()),
        },
        _ => RealAppProofArtifact {
            kind: "installed_config".to_string(),
            path: "<client-hook-config>".to_string(),
            provided: false,
            status: "not_provided".to_string(),
            scan: None,
            missing_requirements: vec!["required before observed_app_hook proof can be recorded"],
            error: None,
        },
    }
}

fn real_app_private_client_target_config_artifact(
    client: &str,
    path: Option<&str>,
) -> RealAppProofArtifact {
    let relpaths = private_client_target_installed_config_relpaths(client);
    match path {
        Some(path) if is_private_client_target_config_path(path, relpaths) => {
            RealAppProofArtifact {
                kind: "private_client_target_config".to_string(),
                path: path.to_string(),
                provided: true,
                status: "present".to_string(),
                scan: None,
                missing_requirements: Vec::new(),
                error: None,
            }
        }
        Some(path) => RealAppProofArtifact {
            kind: "private_client_target_config".to_string(),
            path: path.to_string(),
            provided: true,
            status: "setup_artifact_only".to_string(),
            scan: None,
            missing_requirements: vec!["private_client_target_config_not_discovered"],
            error: None,
        },
        None => RealAppProofArtifact {
            kind: "private_client_target_config".to_string(),
            path: String::new(),
            provided: false,
            status: "missing".to_string(),
            scan: None,
            missing_requirements: vec!["private_client_target_config_not_discovered"],
            error: None,
        },
    }
}

fn real_app_event_jsonl_artifact(
    path: Option<&str>,
    scan: Option<&Result<EventScan, AdapterBindingProofError>>,
    installed_config_scan: Option<&InstalledConfigScan>,
) -> RealAppProofArtifact {
    match (path, scan) {
        (Some(path), Some(Ok(scan))) => {
            let has_private_event_requirements = scan.matching_private_event_sources > 0
                && scan.matching_private_writer_contract_events > 0
                && scan.matching_private_binding_nonces > 0
                && scan.matching_private_events_with_observed_at > 0;
            let missing_requirements =
                real_app_event_missing_requirements(scan, installed_config_scan);
            let status = if missing_requirements.is_empty() {
                "matching_private_event"
            } else if has_private_event_requirements {
                "stale_private_event"
            } else {
                "missing_private_event_requirements"
            };
            RealAppProofArtifact {
                kind: "event_jsonl".to_string(),
                path: path.to_string(),
                provided: true,
                status: status.to_string(),
                scan: Some(json!(scan)),
                missing_requirements,
                error: None,
            }
        }
        (Some(path), Some(Err(err))) => RealAppProofArtifact {
            kind: "event_jsonl".to_string(),
            path: path.to_string(),
            provided: true,
            status: "unreadable".to_string(),
            scan: None,
            missing_requirements: Vec::new(),
            error: Some(err.to_string()),
        },
        _ => RealAppProofArtifact {
            kind: "event_jsonl".to_string(),
            path: "$HOME/.soma/adapter/events.jsonl".to_string(),
            provided: false,
            status: "not_provided".to_string(),
            scan: None,
            missing_requirements: vec!["required before observed_app_hook proof can be recorded"],
            error: None,
        },
    }
}

fn real_app_review_render_artifact(
    client: &str,
    path: Option<&str>,
    scan: Option<&Result<Value, AdapterBindingProofError>>,
) -> RealAppProofArtifact {
    match (path, scan) {
        (Some(path), Some(Ok(report))) => RealAppProofArtifact {
            kind: "review_render_report".to_string(),
            path: path.to_string(),
            provided: true,
            status: "valid".to_string(),
            scan: Some(report.clone()),
            missing_requirements: Vec::new(),
            error: None,
        },
        (Some(path), Some(Err(err))) => RealAppProofArtifact {
            kind: "review_render_report".to_string(),
            path: path.to_string(),
            provided: true,
            status: "invalid".to_string(),
            scan: None,
            missing_requirements: Vec::new(),
            error: Some(err.to_string()),
        },
        _ => RealAppProofArtifact {
            kind: "review_render_report".to_string(),
            path: durable_client_artifact_path(client, "review-render.json"),
            provided: false,
            status: "not_provided".to_string(),
            scan: None,
            missing_requirements: vec![
                "required before observed_in_client_render proof can be recorded",
            ],
            error: None,
        },
    }
}

fn real_app_render_evidence_artifact(
    path: Option<&str>,
    scan: Option<&Result<RenderEvidenceScan, AdapterBindingProofError>>,
    client: &str,
    review_render_file_scan: Option<&Result<FileFingerprintScan, AdapterBindingProofError>>,
    review_render_scan: Option<&Result<Value, AdapterBindingProofError>>,
) -> RealAppProofArtifact {
    match (path, scan) {
        (Some(path), Some(Ok(scan))) => {
            let mut scan = scan.clone();
            let expected_review_render_fingerprint = review_render_file_scan
                .and_then(|scan| scan.as_ref().ok())
                .map(|scan| scan.fingerprint.as_str());
            let expected_workbench_version = review_render_scan
                .and_then(|scan| scan.as_ref().ok())
                .and_then(review_render_workbench_version);
            let expected_interaction_contract_version = review_render_scan
                .and_then(|scan| scan.as_ref().ok())
                .and_then(review_render_interaction_contract_version);
            let expected_control_ids = review_render_scan
                .and_then(|scan| scan.as_ref().ok())
                .map(review_render_control_ids)
                .unwrap_or_default();
            let expected_surface_names = review_render_scan
                .and_then(|scan| scan.as_ref().ok())
                .map(|report| review_render_required_surface_names(report, &expected_control_ids))
                .unwrap_or_default();
            let missing_requirements = annotate_render_evidence_scan(
                &mut scan,
                client,
                expected_review_render_fingerprint,
                expected_workbench_version,
                expected_interaction_contract_version,
                &expected_surface_names,
                &expected_control_ids,
            );
            let status =
                if missing_requirements.is_empty() { "valid_structured" } else { "not_eligible" };
            RealAppProofArtifact {
                kind: "render_evidence".to_string(),
                path: path.to_string(),
                provided: true,
                status: status.to_string(),
                scan: Some(json!(scan)),
                missing_requirements,
                error: None,
            }
        }
        (Some(path), Some(Err(err))) => RealAppProofArtifact {
            kind: "render_evidence".to_string(),
            path: path.to_string(),
            provided: true,
            status: "unreadable".to_string(),
            scan: None,
            missing_requirements: Vec::new(),
            error: Some(err.to_string()),
        },
        _ => RealAppProofArtifact {
            kind: "render_evidence".to_string(),
            path: durable_client_artifact_path(client, "render-evidence.json"),
            provided: false,
            status: "not_provided".to_string(),
            scan: None,
            missing_requirements: vec![
                "required before observed_in_client_render proof can be recorded",
            ],
            error: None,
        },
    }
}

fn real_app_review_action_artifact(
    client: &str,
    path: Option<&str>,
    scan: Option<&Result<ReviewActionReportScan, AdapterBindingProofError>>,
) -> RealAppProofArtifact {
    match (path, scan) {
        (Some(path), Some(Ok(scan))) => {
            let status = if scan.valid_storage_gated_review_action {
                "valid_storage_gated"
            } else {
                "not_eligible"
            };
            RealAppProofArtifact {
                kind: "review_action_report".to_string(),
                path: path.to_string(),
                provided: true,
                status: status.to_string(),
                scan: Some(json!(scan)),
                missing_requirements: review_action_report_missing_requirements(scan),
                error: None,
            }
        }
        (Some(path), Some(Err(err))) => RealAppProofArtifact {
            kind: "review_action_report".to_string(),
            path: path.to_string(),
            provided: true,
            status: "unreadable".to_string(),
            scan: None,
            missing_requirements: Vec::new(),
            error: Some(err.to_string()),
        },
        _ => RealAppProofArtifact {
            kind: "review_action_report".to_string(),
            path: durable_client_artifact_path(client, "review-action.json"),
            provided: false,
            status: "not_provided".to_string(),
            scan: None,
            missing_requirements: vec![
                "required before observed_review_action proof can be recorded",
            ],
            error: None,
        },
    }
}

fn real_app_event_missing_requirements(
    scan: &EventScan,
    installed_config_scan: Option<&InstalledConfigScan>,
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if scan.matching_private_event_sources == 0 {
        missing.push("event JSONL must include the expected private event_source");
    }
    if scan.expected_binding_nonce.is_some() && scan.matching_private_binding_nonces == 0 {
        missing.push("event JSONL must include the installed config binding_nonce");
    }
    if scan.matching_private_writer_contract_events == 0 {
        missing.push("event JSONL must carry soma_adapter_spool_append_v1 writer metadata");
    }
    if scan.matching_private_events_with_observed_at == 0 {
        missing.push("event JSONL must include observed_at_ns on the matching private event");
    }
    if let Some(installed_config_scan) = installed_config_scan {
        match installed_config_scan.modified_at_ns {
            Some(config_modified_at) => {
                if let Some(event_observed_at) = scan.max_matching_private_observed_at_ns {
                    if event_observed_at.saturating_add(OBSERVED_APP_HOOK_ALLOWED_CLOCK_SKEW_NS)
                        < config_modified_at
                    {
                        missing.push(
                            "event JSONL matching private event observed_at_ns must be at or after installed config modified_at",
                        );
                    }
                }
                if let Some(event_modified_at) = scan.modified_at_ns {
                    if event_modified_at.saturating_add(OBSERVED_APP_HOOK_ALLOWED_CLOCK_SKEW_NS)
                        < config_modified_at
                    {
                        missing.push(
                            "event JSONL file modified_at must be at or after installed config modified_at",
                        );
                    }
                }
            }
            None => {
                missing.push(
                    "installed config modified_at required before app-hook proof can be recorded",
                );
            }
        }
    }
    missing
}

fn real_app_proof_readiness(
    artifacts: &[RealAppProofArtifact],
    commands: &[Vec<String>],
    require_private_target_config_for_app_hook: bool,
) -> Vec<RealAppProofReadiness> {
    let mut observed_app_hook_requirements =
        vec![("installed_config", "eligible"), ("event_jsonl", "matching_private_event")];
    if require_private_target_config_for_app_hook {
        observed_app_hook_requirements.insert(1, ("private_client_target_config", "present"));
    }
    vec![
        readiness_for_proof_level(
            ClientBindingProofLevel::ObservedAppHook,
            artifacts,
            commands,
            &observed_app_hook_requirements,
            "observed_app_hook_recording_requires_real_private_event_artifacts_and_operator_confirmation",
        ),
        readiness_for_proof_level(
            ClientBindingProofLevel::ObservedInClientRender,
            artifacts,
            commands,
            &[
                ("review_render_report", "valid"),
                ("render_evidence", "valid_structured"),
            ],
            "observed_in_client_render_recording_requires_visible_client_render_artifacts_and_operator_confirmation",
        ),
        readiness_for_proof_level(
            ClientBindingProofLevel::ObservedReviewAction,
            artifacts,
            commands,
            &[
                ("installed_config", "eligible"),
                ("review_action_report", "valid_storage_gated"),
            ],
            "observed_review_action_recording_requires_storage_gated_review_action_artifact_and_operator_confirmation",
        ),
    ]
}

fn readiness_for_proof_level(
    proof_level: ClientBindingProofLevel,
    artifacts: &[RealAppProofArtifact],
    commands: &[Vec<String>],
    requirements: &[(&str, &str)],
    trust_boundary: &str,
) -> RealAppProofReadiness {
    let mut missing_requirements = Vec::new();
    for (kind, expected_status) in requirements {
        match artifacts.iter().find(|artifact| artifact.kind == *kind) {
            Some(artifact) if artifact.status == *expected_status => {}
            Some(artifact) => {
                if artifact.missing_requirements.is_empty() {
                    missing_requirements.push(format!(
                        "{kind}: expected status `{expected_status}`, got `{}`",
                        artifact.status
                    ));
                } else {
                    missing_requirements.extend(
                        artifact
                            .missing_requirements
                            .iter()
                            .map(|missing| format!("{kind}: {missing}")),
                    );
                }
                if let Some(error) = artifact.error.as_deref() {
                    missing_requirements.push(format!("{kind}: {error}"));
                }
            }
            None => {
                missing_requirements
                    .push(format!("{kind}: required artifact not present in proof kit"));
            }
        }
    }
    let command = command_for_proof_level(commands, proof_level).unwrap_or_default();
    let ready_to_attempt_record = missing_requirements.is_empty();
    RealAppProofReadiness {
        proof_level,
        status: if ready_to_attempt_record {
            "ready_to_attempt_record".to_string()
        } else {
            "blocked_by_missing_or_invalid_artifacts".to_string()
        },
        ready_to_attempt_record,
        required_artifacts: requirements.iter().map(|(kind, _)| (*kind).to_string()).collect(),
        missing_requirements,
        command,
        trust_boundary: trust_boundary.to_string(),
    }
}

fn command_for_proof_level(
    commands: &[Vec<String>],
    proof_level: ClientBindingProofLevel,
) -> Option<Vec<String>> {
    commands
        .iter()
        .find(|command| {
            command
                .windows(2)
                .any(|pair| pair[0] == "--proof-level" && pair[1] == proof_level.as_str())
        })
        .cloned()
}

fn client_binding_proof_session(
    client: &str,
    artifact_dir: &str,
    readiness: &AdapterBindingProofStatusOutcome,
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
    operator_flow: &[ClientBindingOperatorFlowStep],
    current_private_target_config_ready: bool,
    continue_collector_probe: Option<&ContinueDevdataCollectorProbe>,
) -> ClientBindingProofSession {
    let status = readiness.clients.iter().find(|status| status.client == client);
    let stages = required_private_client_proof_levels()
        .iter()
        .map(|proof_level| proof_session_stage(*proof_level, status, proof_kit, operator_flow))
        .collect::<Vec<_>>();
    let completed_proof_levels = stages
        .iter()
        .filter(|stage| stage.ledger_status == "stored_verified")
        .map(|stage| stage.proof_level)
        .collect::<Vec<_>>();
    let pending_proof_levels = stages
        .iter()
        .filter(|stage| stage.ledger_status != "stored_verified")
        .map(|stage| stage.proof_level)
        .collect::<Vec<_>>();
    let ready_to_record_proof_levels = stages
        .iter()
        .filter(|stage| stage.ready_to_record_now)
        .map(|stage| stage.proof_level)
        .collect::<Vec<_>>();
    let blocked_proof_levels = stages
        .iter()
        .filter(|stage| stage.ledger_status != "stored_verified" && !stage.ready_to_record_now)
        .map(|stage| stage.proof_level)
        .collect::<Vec<_>>();
    let stored_ready_for_private_client_claim =
        status.is_some_and(|status| status.ready_for_private_client_claim);
    let missing_current_private_target_config =
        stored_ready_for_private_client_claim && !current_private_target_config_ready;
    let ready_for_private_client_claim =
        stored_ready_for_private_client_claim && current_private_target_config_ready;
    let next_stage = stages.iter().find(|stage| stage.ledger_status != "stored_verified");
    let next_step_id = if missing_current_private_target_config {
        Some("install_or_merge_private_client_config".to_string())
    } else {
        next_stage.map(|stage| {
            let step_id =
                proof_session_next_step_id(stage.proof_level, stage.ready_to_record_now, proof_kit);
            proof_session_next_step_id_for_client(client, &step_id, continue_collector_probe)
        })
    };
    let next_operator_step = next_step_id
        .as_deref()
        .and_then(|id| operator_flow.iter().find(|step| step.id == id).cloned());
    let initial_next_command = if missing_current_private_target_config {
        next_operator_step.as_ref().and_then(|step| step.command.clone())
    } else {
        next_stage.and_then(|stage| {
            if stage.ready_to_record_now {
                stage.command.clone()
            } else {
                next_operator_step.as_ref().and_then(|step| step.command.clone())
            }
        })
    };
    let next_mcp_call = next_operator_step.as_ref().and_then(|step| step.mcp_call.clone());
    let status = if ready_for_private_client_claim {
        "ready_for_private_client_claim"
    } else if missing_current_private_target_config {
        "blocked_by_missing_private_target_config"
    } else if !ready_to_record_proof_levels.is_empty() {
        "proof_artifacts_ready_for_operator_recording"
    } else if stages
        .iter()
        .any(|stage| stage.ledger_status == "stored_but_non_release_evidence_source")
    {
        "blocked_by_non_release_evidence_source"
    } else if stages.iter().any(|stage| stage.ledger_status.starts_with("stored_but_")) {
        "blocked_by_stored_proof_integrity_or_identity"
    } else {
        "blocked_by_missing_or_invalid_artifacts"
    };
    let operator_next_action_id =
        proof_session_operator_next_action_id(status, next_step_id.as_deref());
    let operator_next_action_label =
        proof_session_operator_next_action_label(&operator_next_action_id, client);
    let completion_criteria = vec![
        "current known private-client target config is discoverable, eligible, and carries the same binding nonce as the proof chain".to_string(),
        "observed_app_hook proof recorded from a real private client event with matching event_source, binding_nonce, writer metadata, temporal binding, and operator confirmation".to_string(),
        "observed_in_client_render proof recorded from structured UI-only render evidence bound to the review-render report, workbench version, interaction contract, visible surfaces, and installed config".to_string(),
        "observed_review_action proof recorded from a rendered control_id that produced a storage-gated review-action report with non-cloud verification evidence".to_string(),
        "latest stored evidence artifacts replay successfully; append-only event_jsonl growth must preserve the recorded prefix fingerprint and app-hook/render/review-action proofs share one installed-config identity".to_string(),
    ];
    let runbook = client_binding_proof_session_runbook(
        client,
        status,
        if ready_for_private_client_claim { "pass" } else { "fail" },
        next_step_id.as_deref(),
        next_operator_step.as_ref(),
        operator_flow,
        &stages,
        &completion_criteria,
        artifact_dir,
    );
    let next_command = proof_session_next_command_with_workspace_fallback(
        next_step_id.as_deref(),
        initial_next_command,
        &runbook,
    );
    let external_action = client_binding_external_operator_action(
        client,
        &operator_next_action_id,
        &operator_next_action_label,
        next_step_id.as_deref().unwrap_or_default(),
        next_command.as_ref(),
    );

    ClientBindingProofSession {
        client: client.to_string(),
        status: status.to_string(),
        release_gate: if ready_for_private_client_claim { "pass" } else { "fail" }.to_string(),
        ready_for_private_client_claim,
        completed_proof_levels,
        pending_proof_levels,
        ready_to_record_proof_levels,
        blocked_proof_levels,
        next_step_id,
        next_command,
        next_mcp_call,
        next_operator_step,
        external_action,
        stages,
        runbook,
        completion_criteria,
        trust_boundary: "client_binding_proof_session_is_read_only: summarizes stored proof readiness and currently provided artifact readiness only; it records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation or in-client rendering by itself".to_string(),
    }
}

fn client_binding_proof_session_runbook(
    client: &str,
    status: &str,
    release_gate: &str,
    target_next_step_id: Option<&str>,
    target_next_operator_step: Option<&ClientBindingOperatorFlowStep>,
    operator_flow: &[ClientBindingOperatorFlowStep],
    stages: &[ClientBindingProofSessionStage],
    completion_criteria: &[String],
    artifact_dir: &str,
) -> ClientBindingProofSessionRunbook {
    let mut completion_checks = completion_criteria.to_vec();
    completion_checks.push(
        "rerun `soma adapter-binding-proof --proof-session --json` or MCP `soma_client_binding_proof_session` until release_gate is pass".to_string(),
    );
    completion_checks.push(format!(
        "save review-render, render-evidence, and review-action report artifacts under `{}` so proof replay survives process restarts and tmp cleanup",
        artifact_dir
    ));
    completion_checks.push(
        "rerun product hardening with client-binding readiness required before claiming private-client release".to_string(),
    );
    let steps = operator_flow
        .iter()
        .map(|step| {
            client_binding_runbook_step(client, artifact_dir, step, target_next_step_id, stages)
        })
        .collect::<Vec<_>>();
    let progress = client_binding_runbook_progress(&steps, stages, release_gate);
    let external_action_safety =
        runbook_step_external_action_safety(client, target_next_step_id.unwrap_or_default());
    let external_action = target_next_operator_step.and_then(|step| {
        let action_id = proof_session_operator_next_action_id(status, target_next_step_id);
        let action_label = proof_session_operator_next_action_label(&action_id, client);
        client_binding_external_operator_action(
            client,
            &action_id,
            &action_label,
            step.id.as_str(),
            step.command.as_ref(),
        )
    });
    let durable_artifact_dir_write_status = artifact_dir_write_status(artifact_dir);
    let workspace_fallback_artifact_dir =
        if artifact_dir_write_status_allows_new_files(&durable_artifact_dir_write_status) {
            None
        } else {
            workspace_fallback_artifact_dir_for_client(client, artifact_dir)
        };
    let workspace_fallback_artifact_paths = workspace_fallback_artifact_dir
        .as_deref()
        .map(client_binding_artifact_path_suggestions)
        .unwrap_or_default();
    let workspace_fallback_commands = workspace_fallback_artifact_dir
        .as_deref()
        .map(|dir| client_binding_workspace_fallback_commands(client, dir))
        .unwrap_or_default();

    ClientBindingProofSessionRunbook {
        schema: "soma.client_binding_proof_session_runbook.v1".to_string(),
        client: client.to_string(),
        status: status.to_string(),
        release_gate: release_gate.to_string(),
        durable_artifact_dir: artifact_dir.to_string(),
        durable_artifact_dir_write_status,
        suggested_artifact_paths: client_binding_artifact_path_suggestions(artifact_dir),
        workspace_fallback_artifact_dir,
        workspace_fallback_artifact_paths,
        workspace_fallback_commands,
        target_next_step_id: target_next_step_id.map(str::to_string),
        target_next_operator_step: target_next_operator_step.cloned(),
        external_action_safety,
        external_action,
        progress,
        steps,
        completion_checks,
        trust_boundary: "client_binding_proof_session_runbook_is_read_only: it exports the operator proof sequence and current blockers only; it writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation, rendering, or review-action execution".to_string(),
    }
}

fn client_binding_runbook_progress(
    steps: &[ClientBindingProofRunbookStep],
    stages: &[ClientBindingProofSessionStage],
    release_gate: &str,
) -> ClientBindingProofRunbookProgress {
    ClientBindingProofRunbookProgress {
        total_step_count: steps.len(),
        operator_action_step_count: steps
            .iter()
            .filter(|step| step.requires_operator_action)
            .count(),
        proof_recording_step_count: steps.iter().filter(|step| step.records_proof).count(),
        ready_now_step_count: steps.iter().filter(|step| step.ready_now).count(),
        blocked_proof_step_count: steps
            .iter()
            .filter(|step| {
                step.records_proof && !step.ready_now && !step.blocking_reasons.is_empty()
            })
            .count(),
        blocking_reason_count: steps.iter().map(|step| step.blocking_reasons.len()).sum(),
        completed_proof_level_count: stages
            .iter()
            .filter(|stage| stage.ledger_status == "stored_verified")
            .count(),
        pending_proof_level_count: stages
            .iter()
            .filter(|stage| stage.ledger_status != "stored_verified")
            .count(),
        ready_to_record_proof_level_count: stages
            .iter()
            .filter(|stage| stage.ready_to_record_now)
            .count(),
        blocked_proof_level_count: stages
            .iter()
            .filter(|stage| stage.ledger_status != "stored_verified" && !stage.ready_to_record_now)
            .count(),
        release_ready: release_gate == "pass",
    }
}

fn client_binding_runbook_step(
    client: &str,
    artifact_dir: &str,
    step: &ClientBindingOperatorFlowStep,
    target_next_step_id: Option<&str>,
    stages: &[ClientBindingProofSessionStage],
) -> ClientBindingProofRunbookStep {
    let proof_level = runbook_step_proof_level(&step.id);
    let stage = runbook_step_stage(&step.id).to_string();
    let evidence_kind = runbook_step_evidence_kind(&step.id).to_string();
    let matching_stage = proof_level
        .and_then(|proof_level| stages.iter().find(|stage| stage.proof_level == proof_level));
    let is_target_step = target_next_step_id.is_some_and(|id| id == step.id);
    let ready_now = is_target_step
        || matching_stage.is_some_and(|stage| stage.ready_to_record_now && step.records_proof);
    let blocking_reasons = if step.records_proof {
        matching_stage
            .filter(|stage| stage.ledger_status != "stored_verified" && !stage.ready_to_record_now)
            .map(|stage| stage.blocking_reasons.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    ClientBindingProofRunbookStep {
        id: step.id.clone(),
        title: step.title.clone(),
        intent: step.intent.clone(),
        stage,
        evidence_kind,
        suggested_artifact_path: suggested_artifact_path_for_step(artifact_dir, &step.id),
        command: step.command.clone(),
        mcp_call: step.mcp_call.clone(),
        external_action_safety: runbook_step_external_action_safety(client, &step.id),
        external_action: client_binding_external_operator_action(
            client,
            "trigger_real_private_client_hook_to_write_private_spool_event",
            &proof_session_operator_next_action_label(
                "trigger_real_private_client_hook_to_write_private_spool_event",
                client,
            ),
            &step.id,
            step.command.as_ref(),
        ),
        requires_operator_action: step.requires_operator_action,
        records_proof: step.records_proof,
        ready_now,
        blocking_reasons,
        trust_boundary: step.trust_boundary.clone(),
    }
}

fn client_binding_external_operator_action(
    client: &str,
    action_id: &str,
    action_label: &str,
    proof_session_step_id: &str,
    readiness_probe_command: Option<&Vec<String>>,
) -> Option<ClientBindingExternalOperatorAction> {
    if proof_session_step_id != "trigger_private_client_hook"
        || action_id != "trigger_real_private_client_hook_to_write_private_spool_event"
    {
        return None;
    }
    let safety = product_hardening_external_action_safety(client, action_id)?;
    let display_name = private_client_display_name(client);
    Some(ClientBindingExternalOperatorAction {
        source: "soma_client_binding_proof_session.external_operator_action.v1",
        client: client.to_string(),
        action_id: action_id.to_string(),
        action_label: action_label.to_string(),
        proof_session_step_id: proof_session_step_id.to_string(),
        action_kind: "real_private_client_action".to_string(),
        required_operator_action: format!(
            "Run one real {display_name} chat/action from the installed private-client binding, using only the minimal hook-ping prompt unless the operator explicitly approves broader context."
        ),
        requires_operator_confirmation_before_submission: safety
            .requires_operator_confirmation_before_submission,
        may_transmit_prompt_to_provider: safety.may_transmit_prompt_to_provider,
        suggested_minimal_test_prompt: safety.suggested_minimal_test_prompt,
        forbidden_inputs: safety.forbidden_inputs,
        readiness_probe_command: readiness_probe_command.cloned(),
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        proof_after_success_step_id: "record_observed_app_hook".to_string(),
        required_observation: format!(
            "A fresh private `{client}` lifecycle event with the expected event_source, binding_nonce, writer metadata, and observed_at_ns must appear in the adapter JSONL before any observed_app_hook proof is recordable."
        ),
        why_next_mcp_call_is_null: "A real private-client hook cannot be substituted by an MCP call; MCP can only inspect readiness or record proof after independent private-client evidence exists.".to_string(),
        trust_boundary: "client_binding_external_operator_action_is_read_only: describes the required external private-client action and privacy boundary only; it executes no client action, submits no prompt, records no proof row, creates no verification event, applies no proposal, and promotes no cloud draft".to_string(),
    })
}

fn runbook_step_external_action_safety(
    client: &str,
    step_id: &str,
) -> Option<ProductHardeningExternalActionSafety> {
    if step_id == "trigger_private_client_hook" {
        product_hardening_external_action_safety(
            client,
            "trigger_real_private_client_hook_to_write_private_spool_event",
        )
    } else {
        None
    }
}

fn runbook_step_proof_level(step_id: &str) -> Option<ClientBindingProofLevel> {
    match step_id {
        "check_continue_devdata_collector_status"
        | "start_continue_devdata_collector_before_real_hook"
        | "trigger_private_client_hook"
        | "drain_adapter_spool"
        | "record_observed_app_hook" => Some(ClientBindingProofLevel::ObservedAppHook),
        "render_review_surface"
        | "capture_in_client_render_evidence"
        | "record_observed_in_client_render" => {
            Some(ClientBindingProofLevel::ObservedInClientRender)
        }
        "execute_rendered_review_control" | "record_observed_review_action" => {
            Some(ClientBindingProofLevel::ObservedReviewAction)
        }
        _ => None,
    }
}

fn runbook_step_stage(step_id: &str) -> &'static str {
    match step_id {
        "record_reference_binding_manifest"
        | "render_or_write_installed_config"
        | "install_or_merge_private_client_config"
        | "check_installed_config" => "setup",
        "check_continue_devdata_collector_status"
        | "trigger_private_client_hook"
        | "start_continue_devdata_collector_before_real_hook"
        | "drain_adapter_spool"
        | "record_observed_event_file"
        | "record_observed_app_hook" => "app_hook",
        "render_review_surface"
        | "capture_in_client_render_evidence"
        | "record_observed_in_client_render" => "review_render",
        "execute_rendered_review_control" | "record_observed_review_action" => "review_action",
        "verify_evidence_artifacts_and_status" => "verification",
        _ => "unknown",
    }
}

fn runbook_step_evidence_kind(step_id: &str) -> &'static str {
    match step_id {
        "record_reference_binding_manifest" => "binding_manifest",
        "render_or_write_installed_config" => "installed_config_preview",
        "install_or_merge_private_client_config" => "operator_installed_config",
        "check_installed_config" => "installed_config_scan",
        "check_continue_devdata_collector_status" => "continue_devdata_collector_status",
        "start_continue_devdata_collector_before_real_hook" => "continue_devdata_collector",
        "trigger_private_client_hook" => "private_client_lifecycle_event",
        "drain_adapter_spool" => "adapter_spool_drain_report",
        "record_observed_event_file" => "observed_event_file_proof",
        "record_observed_app_hook" => "observed_app_hook_proof",
        "render_review_surface" => "review_render_report",
        "capture_in_client_render_evidence" => "in_client_render_evidence_packet",
        "record_observed_in_client_render" => "observed_in_client_render_proof",
        "execute_rendered_review_control" => "review_action_report",
        "record_observed_review_action" => "observed_review_action_proof",
        "verify_evidence_artifacts_and_status" => "proof_status_and_artifact_replay",
        _ => "unknown",
    }
}

fn required_private_client_proof_levels() -> [ClientBindingProofLevel; 3] {
    [
        ClientBindingProofLevel::ObservedAppHook,
        ClientBindingProofLevel::ObservedInClientRender,
        ClientBindingProofLevel::ObservedReviewAction,
    ]
}

fn proof_session_stage(
    proof_level: ClientBindingProofLevel,
    client_status: Option<&ClientBindingReadinessStatus>,
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
    operator_flow: &[ClientBindingOperatorFlowStep],
) -> ClientBindingProofSessionStage {
    let proof_key = proof_level.as_str();
    let stored = client_status.and_then(|status| status.latest_by_level.get(proof_key));
    let artifact_readiness =
        proof_kit.proof_readiness.iter().find(|readiness| readiness.proof_level == proof_level);
    let artifact_status = artifact_readiness.map(|readiness| readiness.status.clone());
    let mut command = artifact_readiness
        .map(|readiness| readiness.command.clone())
        .filter(|command| !command.is_empty());
    let mut blocking_reasons =
        artifact_readiness.map(|readiness| readiness.missing_requirements.clone()).unwrap_or_else(
            || vec![format!("{proof_key}: no proof readiness verdict was available in proof kit")],
        );
    let ledger_status = match (client_status, stored) {
        (Some(status), Some(latest)) => {
            let artifact_failure =
                status.artifact_failures.iter().any(|failure| failure.proof_level == proof_level);
            let non_release_source = status
                .non_release_evidence_sources
                .iter()
                .find(|source| source.proof_level == proof_level);
            let stage_coherence_failures =
                stage_coherence_failures(proof_level, &status.coherence_failures);
            if artifact_failure || !latest.all_artifacts_verified {
                blocking_reasons.push(format!("{proof_key}: stored proof artifact replay failed"));
                "stored_but_artifact_integrity_failed"
            } else if !stage_coherence_failures.is_empty() {
                blocking_reasons.extend(stage_coherence_failures);
                "stored_but_identity_mismatch"
            } else if let Some(source) = non_release_source {
                blocking_reasons.push(format!("{proof_key}: {}", source.reason));
                "stored_but_non_release_evidence_source"
            } else {
                blocking_reasons.clear();
                "stored_verified"
            }
        }
        _ => "missing",
    };
    let ready_to_record_now = matches!(
        ledger_status,
        "missing" | "stored_but_artifact_integrity_failed" | "stored_but_identity_mismatch"
    ) && artifact_readiness
        .is_some_and(|readiness| readiness.ready_to_attempt_record);
    if matches!(proof_level, ClientBindingProofLevel::ObservedAppHook) && ready_to_record_now {
        if let Some(wrapper_command) = operator_flow
            .iter()
            .find(|step| step.id == "record_observed_app_hook")
            .and_then(|step| step.command.clone())
        {
            command = Some(wrapper_command);
        }
    }

    ClientBindingProofSessionStage {
        proof_level,
        ledger_status: ledger_status.to_string(),
        artifact_status,
        ready_to_record_now,
        blocking_reasons,
        command,
        mcp_call: proof_session_stage_mcp_call(proof_level, operator_flow),
        trust_boundary: proof_session_stage_trust_boundary(proof_level).to_string(),
    }
}

fn stage_coherence_failures(
    proof_level: ClientBindingProofLevel,
    failures: &[String],
) -> Vec<String> {
    match proof_level {
        ClientBindingProofLevel::ObservedInClientRender => failures
            .iter()
            .filter(|failure| !failure.starts_with("review_action_"))
            .cloned()
            .collect(),
        ClientBindingProofLevel::ObservedReviewAction => failures
            .iter()
            .filter(|failure| failure.starts_with("review_action_"))
            .cloned()
            .collect(),
        ClientBindingProofLevel::ReferenceBinding
        | ClientBindingProofLevel::ObservedEventFile
        | ClientBindingProofLevel::ObservedAppHook => Vec::new(),
    }
}

fn proof_session_stage_mcp_call(
    proof_level: ClientBindingProofLevel,
    operator_flow: &[ClientBindingOperatorFlowStep],
) -> Option<ClientBindingMcpCallTemplate> {
    let record_step_id = match proof_level {
        ClientBindingProofLevel::ObservedAppHook => "record_observed_app_hook",
        ClientBindingProofLevel::ObservedInClientRender => "record_observed_in_client_render",
        ClientBindingProofLevel::ObservedReviewAction => "record_observed_review_action",
        ClientBindingProofLevel::ReferenceBinding | ClientBindingProofLevel::ObservedEventFile => {
            return None
        }
    };
    operator_flow
        .iter()
        .find(|step| step.id == record_step_id)
        .and_then(|step| step.mcp_call.clone())
}

fn proof_session_next_step_id(
    proof_level: ClientBindingProofLevel,
    ready_to_record_now: bool,
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
) -> String {
    let step_id = match (proof_level, ready_to_record_now) {
        (ClientBindingProofLevel::ObservedAppHook, true) => "record_observed_app_hook",
        (ClientBindingProofLevel::ObservedAppHook, false) => {
            proof_session_next_app_hook_step_id(proof_kit)
        }
        (ClientBindingProofLevel::ObservedInClientRender, true) => {
            "record_observed_in_client_render"
        }
        (ClientBindingProofLevel::ObservedInClientRender, false) => {
            proof_session_next_render_step_id(proof_kit)
        }
        (ClientBindingProofLevel::ObservedReviewAction, true) => "record_observed_review_action",
        (ClientBindingProofLevel::ObservedReviewAction, false) => "execute_rendered_review_control",
        _ => "inspect_client_binding_status",
    };
    step_id.to_string()
}

fn proof_session_next_step_id_for_client(
    client: &str,
    step_id: &str,
    continue_collector_probe: Option<&ContinueDevdataCollectorProbe>,
) -> String {
    if client == "continue"
        && step_id == "trigger_private_client_hook"
        && continue_collector_probe.is_some_and(|probe| {
            probe.devdata_destination_visible
                && matches!(probe.collector_status.as_str(), "probe_blocked" | "probe_unavailable")
        })
    {
        return "check_continue_devdata_collector_status".to_string();
    }
    if client == "continue"
        && step_id == "trigger_private_client_hook"
        && continue_collector_probe.is_some_and(|probe| {
            probe.devdata_destination_visible && probe.collector_status == "not_listening"
        })
    {
        return "start_continue_devdata_collector_before_real_hook".to_string();
    }
    step_id.to_string()
}

fn proof_session_next_app_hook_step_id(
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
) -> &'static str {
    match proof_kit_artifact(proof_kit, "installed_config") {
        Some(artifact) if artifact.status == "eligible" => "trigger_private_client_hook",
        Some(artifact) if artifact.provided => "check_installed_config",
        _ => "render_or_write_installed_config",
    }
}

fn proof_session_next_render_step_id(
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
) -> &'static str {
    if !proof_kit_artifact_has_status(proof_kit, "review_render_report", "valid") {
        return "render_review_surface";
    }
    "capture_in_client_render_evidence"
}

fn proof_kit_artifact<'a>(
    proof_kit: &'a AdapterBindingRealAppProofKitOutcome,
    kind: &str,
) -> Option<&'a RealAppProofArtifact> {
    proof_kit.artifacts.iter().find(|artifact| artifact.kind == kind)
}

fn proof_kit_artifact_has_status(
    proof_kit: &AdapterBindingRealAppProofKitOutcome,
    kind: &str,
    status: &str,
) -> bool {
    proof_kit_artifact(proof_kit, kind).is_some_and(|artifact| artifact.status == status)
}

fn proof_session_stage_trust_boundary(proof_level: ClientBindingProofLevel) -> &'static str {
    match proof_level {
        ClientBindingProofLevel::ObservedAppHook => {
            "observed_app_hook_stage_requires_real_private_event_evidence_and_operator_confirmation"
        }
        ClientBindingProofLevel::ObservedInClientRender => {
            "observed_in_client_render_stage_is_ui_only_and_never_verifies_promotes_applies_or_acknowledges"
        }
        ClientBindingProofLevel::ObservedReviewAction => {
            "observed_review_action_stage_requires_rendered_control_id_storage_gated_report_non_cloud_verification_and_operator_confirmation"
        }
        ClientBindingProofLevel::ReferenceBinding | ClientBindingProofLevel::ObservedEventFile => {
            "setup_stage_does_not_prove_private_client_operator_loop"
        }
    }
}

fn durable_client_artifact_dir(client: &str) -> String {
    durable_client_artifact_dir_for_run(client, "<run-id>")
}

fn durable_client_artifact_dir_for_run(client: &str, run_id: &str) -> String {
    let run_id = sanitize_artifact_run_id(run_id);
    if let Some(home) = dirs::home_dir() {
        return home
            .join(".soma")
            .join("client-evidence")
            .join(client)
            .join(run_id)
            .to_string_lossy()
            .into_owned();
    }
    format!("$HOME/.soma/client-evidence/{client}/{run_id}")
}

fn durable_client_artifact_dir_for_root(client: &str, run_id: &str, root: Option<&str>) -> String {
    let run_id = sanitize_artifact_run_id(run_id);
    let Some(root) = root.filter(|value| !value.trim().is_empty()) else {
        return durable_client_artifact_dir_for_run(client, &run_id);
    };
    Path::new(root)
        .join(".soma")
        .join("client-evidence")
        .join(client)
        .join(run_id)
        .to_string_lossy()
        .into_owned()
}

fn durable_client_artifact_path(client: &str, filename: &str) -> String {
    durable_client_artifact_path_in_dir(&durable_client_artifact_dir(client), filename)
}

fn durable_client_artifact_path_in_dir(dir: &str, filename: &str) -> String {
    format!("{dir}/{filename}")
}

fn select_existing_durable_artifacts(args: &mut AdapterBindingProofArgs, artifact_dir: &str) {
    if args.review_render_report.is_none() {
        args.review_render_report =
            existing_durable_artifact_path(artifact_dir, "review-render.json");
    }
    if args.render_evidence.is_none() {
        args.render_evidence = existing_durable_artifact_path(artifact_dir, "render-evidence.json");
    }
    if args.review_action_report.is_none() {
        args.review_action_report =
            existing_durable_artifact_path(artifact_dir, "review-action.json");
    }
}

fn existing_durable_artifact_path(artifact_dir: &str, filename: &str) -> Option<String> {
    let path = Path::new(artifact_dir).join(filename);
    path.is_file().then(|| canonical_or_raw(&path).to_string_lossy().into_owned())
}

fn sanitize_artifact_run_id(run_id: &str) -> String {
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        return "<run-id>".to_string();
    }
    trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '<' | '>') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn client_binding_artifact_path_suggestions(
    dir: &str,
) -> Vec<ClientBindingProofArtifactPathSuggestion> {
    vec![
        ClientBindingProofArtifactPathSuggestion {
            artifact_kind: "review_render_report".to_string(),
            path: durable_client_artifact_path_in_dir(dir, "review-render.json"),
            intent: "Read-only `soma context review-render` report that visible render evidence must bind to.".to_string(),
        },
        ClientBindingProofArtifactPathSuggestion {
            artifact_kind: "render_evidence".to_string(),
            path: durable_client_artifact_path_in_dir(dir, "render-evidence.json"),
            intent: "Filled soma.in_client_render_evidence.v1 captured after visible private-client UI rendering.".to_string(),
        },
        ClientBindingProofArtifactPathSuggestion {
            artifact_kind: "review_action_report".to_string(),
            path: durable_client_artifact_path_in_dir(dir, "review-action.json"),
            intent: "Storage-gated `soma context review-action` report from one rendered control.".to_string(),
        },
    ]
}

fn artifact_dir_write_status(dir: &str) -> String {
    if dir.contains("$HOME") || dir.contains("<run-id>") {
        return "not_checked_template".to_string();
    }
    let path = Path::new(dir);
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            let exact = candidate == path;
            if !exact && !candidate.is_dir() {
                return "parent_not_writable".to_string();
            }
            let writable = path_is_writable_without_mutation(candidate);
            return match (exact, writable) {
                (true, true) => "writable",
                (true, false) => "not_writable",
                (false, true) => "parent_writable",
                (false, false) => "parent_not_writable",
            }
            .to_string();
        }
        current = candidate.parent();
    }
    "parent_missing".to_string()
}

#[cfg(unix)]
fn path_is_writable_without_mutation(path: &Path) -> bool {
    Command::new("/bin/test").arg("-w").arg(path).status().is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn path_is_writable_without_mutation(path: &Path) -> bool {
    fs::metadata(path).map(|metadata| !metadata.permissions().readonly()).unwrap_or(false)
}

fn artifact_dir_write_status_allows_new_files(status: &str) -> bool {
    matches!(status, "writable" | "parent_writable" | "not_checked_template")
}

fn workspace_fallback_artifact_dir_for_client(client: &str, artifact_dir: &str) -> Option<String> {
    let run_id = Path::new(artifact_dir)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "<run-id>".to_string());
    let cwd = env::current_dir().ok()?;
    Some(
        cwd.join(".soma")
            .join("client-evidence")
            .join(client)
            .join(run_id)
            .to_string_lossy()
            .into_owned(),
    )
}

fn client_binding_workspace_fallback_commands(
    client: &str,
    artifact_dir: &str,
) -> Vec<Vec<String>> {
    let manifest = default_client_binding_manifest(client).unwrap_or("<client-binding-manifest>");
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    vec![
        vec![
            soma_bin.clone(),
            "adapter-binding-proof".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--proof-session".to_string(),
            "--brief".to_string(),
            "--artifact-dir".to_string(),
            artifact_dir.to_string(),
        ],
        vec![
            "tools/soma-client-render-proof-prep.sh".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--soma-bin".to_string(),
            soma_bin,
            "--manifest".to_string(),
            manifest.to_string(),
            "--artifact-dir".to_string(),
            artifact_dir.to_string(),
        ],
    ]
}

fn proof_session_next_command_with_workspace_fallback(
    next_step_id: Option<&str>,
    command: Option<Vec<String>>,
    runbook: &ClientBindingProofSessionRunbook,
) -> Option<Vec<String>> {
    if next_step_id != Some("capture_in_client_render_evidence") {
        return command;
    }
    workspace_fallback_command_for_step(runbook, "capture_in_client_render_evidence").or(command)
}

fn workspace_fallback_command_for_step(
    runbook: &ClientBindingProofSessionRunbook,
    step_id: &str,
) -> Option<Vec<String>> {
    runbook.workspace_fallback_artifact_dir.as_ref()?;
    match step_id {
        "capture_in_client_render_evidence" => runbook
            .workspace_fallback_commands
            .iter()
            .find(|command| {
                command.iter().any(|part| part == "tools/soma-client-render-proof-prep.sh")
            })
            .cloned(),
        _ => None,
    }
}

fn workspace_fallback_artifact_path_for_step(
    runbook: &ClientBindingProofSessionRunbook,
    step_id: &str,
) -> Option<String> {
    runbook.workspace_fallback_artifact_dir.as_ref()?;
    let artifact_kind = match step_id {
        "render_review_surface" => "review_render_report",
        "capture_in_client_render_evidence" | "record_observed_in_client_render" => {
            "render_evidence"
        }
        "execute_rendered_review_control" | "record_observed_review_action" => {
            "review_action_report"
        }
        _ => return None,
    };
    runbook
        .workspace_fallback_artifact_paths
        .iter()
        .find(|suggestion| suggestion.artifact_kind == artifact_kind)
        .map(|suggestion| suggestion.path.clone())
}

fn suggested_artifact_path_for_step(dir: &str, step_id: &str) -> Option<String> {
    match step_id {
        "render_review_surface" => {
            Some(durable_client_artifact_path_in_dir(dir, "review-render.json"))
        }
        "capture_in_client_render_evidence" | "record_observed_in_client_render" => {
            Some(durable_client_artifact_path_in_dir(dir, "render-evidence.json"))
        }
        "execute_rendered_review_control" | "record_observed_review_action" => {
            Some(durable_client_artifact_path_in_dir(dir, "review-action.json"))
        }
        _ => None,
    }
}

fn real_app_proof_commands(
    client: &str,
    manifest_path: Option<&str>,
    installed_config_path: Option<&str>,
    event_jsonl_path: Option<&str>,
    review_render_report_path: Option<&str>,
    render_evidence_path: Option<&str>,
    review_action_report_path: Option<&str>,
) -> Vec<Vec<String>> {
    let manifest = manifest_path
        .or_else(|| default_client_binding_manifest(client))
        .unwrap_or("<client-binding-manifest>");
    let installed_config = installed_config_path
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{}-hook-config>", client));
    let event_jsonl = event_jsonl_path.unwrap_or("$HOME/.soma/adapter/events.jsonl").to_string();
    let review_render_report = review_render_report_path
        .map(str::to_string)
        .unwrap_or_else(|| durable_client_artifact_path(client, "review-render.json"));
    let render_evidence = render_evidence_path
        .map(str::to_string)
        .unwrap_or_else(|| durable_client_artifact_path(client, "render-evidence.json"));
    let review_action_report = review_action_report_path
        .map(str::to_string)
        .unwrap_or_else(|| durable_client_artifact_path(client, "review-action.json"));
    let app_hook_evidence_source =
        release_grade_operator_evidence_source(client, ClientBindingProofLevel::ObservedAppHook);
    let render_evidence_source = release_grade_operator_evidence_source(
        client,
        ClientBindingProofLevel::ObservedInClientRender,
    );
    let review_action_evidence_source = release_grade_operator_evidence_source(
        client,
        ClientBindingProofLevel::ObservedReviewAction,
    );

    commands_with_resolved_soma_binary(vec![
        with_binding_target(
            client,
            manifest_path,
            vec!["soma", "adapter-binding-proof", "--real-app-proof-kit"],
        ),
        with_binding_target(
            client,
            manifest_path,
            vec!["soma", "adapter-binding-proof", "--discover-installed-config"],
        ),
        with_binding_target(
            client,
            manifest_path,
            vec!["soma", "adapter-binding-proof", "--prepare-installed-config"],
        ),
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--check-installed-config".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--installed-config".to_string(),
            installed_config.clone(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--render-render-evidence".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--review-render-report".to_string(),
            review_render_report.clone(),
            "--write-render-evidence".to_string(),
            render_evidence.clone(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_app_hook".to_string(),
            "--event-jsonl".to_string(),
            event_jsonl,
            "--installed-config".to_string(),
            installed_config.clone(),
            "--evidence-source".to_string(),
            app_hook_evidence_source,
            "--operator-confirm-real-app-invocation".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_in_client_render".to_string(),
            "--installed-config".to_string(),
            installed_config.clone(),
            "--review-render-report".to_string(),
            review_render_report,
            "--render-evidence".to_string(),
            render_evidence,
            "--evidence-source".to_string(),
            render_evidence_source,
            "--operator-confirm-in-client-render".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_review_action".to_string(),
            "--installed-config".to_string(),
            installed_config,
            "--review-action-report".to_string(),
            review_action_report,
            "--evidence-source".to_string(),
            review_action_evidence_source,
            "--operator-confirm-review-action".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ],
        with_binding_target(
            client,
            manifest_path,
            vec!["soma", "adapter-binding-proof", "--verify-evidence-artifacts"],
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
fn client_binding_record_proof_mcp_call(
    client: &str,
    manifest: &str,
    proof_level: ClientBindingProofLevel,
    evidence_source: &str,
    event_jsonl: Option<&str>,
    installed_config: Option<&str>,
    review_render_report: Option<&str>,
    render_evidence: Option<&str>,
    review_action_report: Option<&str>,
    operator_confirmation_key: Option<&str>,
) -> ClientBindingMcpCallTemplate {
    let mut arguments = serde_json::Map::new();
    arguments.insert("manifest".to_string(), json!(manifest_path_for_mcp_arg(manifest)));
    arguments.insert("client".to_string(), json!(client));
    arguments.insert("proof_level".to_string(), json!(proof_level.as_str()));
    arguments.insert("evidence_source".to_string(), json!(evidence_source));
    if let Some(path) = event_jsonl {
        arguments.insert("event_jsonl".to_string(), json!(path));
    }
    if let Some(path) = installed_config {
        arguments.insert("installed_config".to_string(), json!(path));
    }
    if let Some(path) = review_render_report {
        arguments.insert("review_render_report".to_string(), json!(path));
    }
    if let Some(path) = render_evidence {
        arguments.insert("render_evidence".to_string(), json!(path));
    }
    if let Some(path) = review_action_report {
        arguments.insert("review_action_report".to_string(), json!(path));
    }
    if let Some(key) = operator_confirmation_key {
        arguments.insert(key.to_string(), json!(true));
    }
    if release_claim_proof_level(proof_level) {
        arguments.insert("operator_confirm_release_grade_evidence".to_string(), json!(true));
    }

    ClientBindingMcpCallTemplate {
        tool: "soma_client_binding_record_proof".to_string(),
        arguments: Value::Object(arguments),
        trust_boundary: "mcp_record_proof_template_only: calling this template records exactly one client-binding proof row through soma_client_binding_record_proof; it creates no claim verification event, promotes no cloud draft, applies no proposal, and stronger proof levels still require real evidence artifacts plus explicit operator and release-grade private-client evidence confirmation".to_string(),
    }
}

fn client_render_evidence_packet_mcp_call(
    client: &str,
    manifest: &str,
    review_render_report: &str,
) -> ClientBindingMcpCallTemplate {
    ClientBindingMcpCallTemplate {
        tool: "soma_client_render_evidence_packet".to_string(),
        arguments: json!({
            "client": client,
            "manifest": manifest_path_for_mcp_arg(manifest),
            "review_render_report": review_render_report,
        }),
        trust_boundary: "mcp_render_evidence_packet_template_only: calling this template materializes a proof-free soma.in_client_render_evidence.v1 packet from a saved review-render report; it writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and cannot prove in-client rendering until visible observations are filled and observed_in_client_render is recorded with explicit operator confirmation".to_string(),
    }
}

fn manifest_path_for_mcp_arg(manifest: &str) -> String {
    let manifest = manifest.trim();
    if manifest.is_empty()
        || manifest.starts_with('<')
        || manifest.contains("$HOME")
        || manifest.contains("${HOME}")
    {
        return manifest.to_string();
    }
    let path = Path::new(manifest);
    if path.is_absolute() || path.exists() {
        return canonical_or_raw(path).to_string_lossy().into_owned();
    }
    if let Some(source_path) = source_tree_manifest_path(manifest) {
        return canonical_or_raw(source_path).to_string_lossy().into_owned();
    }
    manifest.to_string()
}

fn source_tree_manifest_path(relative_manifest: &str) -> Option<PathBuf> {
    let relative = Path::new(relative_manifest);
    if relative.is_absolute() {
        return None;
    }
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.parent().and_then(Path::parent).unwrap_or(crate_dir);
    let candidate = repo_root.join(relative);
    candidate.exists().then_some(candidate)
}

fn observed_app_hook_record_wrapper_command(
    client: &str,
    manifest: &str,
    config_root: Option<&str>,
    event_jsonl: &str,
    expected_event_source: &str,
    binding_nonce: &str,
    evidence_source: &str,
) -> Vec<String> {
    let mut command = vec![
        "env".to_string(),
        format!(
            "SOMA_BIN={}",
            crate::cli::binary_identity::resolved_soma_bin_for_operator_command()
        ),
        format!("SOMA_CLIENT_BINDING_CLIENT={client}"),
        format!("SOMA_CLIENT_BINDING_MANIFEST={manifest}"),
    ];
    if let Some(config_root) = config_root.filter(|value| !value.trim().is_empty()) {
        command.push(format!("SOMA_CLIENT_BINDING_CONFIG_ROOT={config_root}"));
    }
    command.extend([
        format!("SOMA_CLIENT_BINDING_EVENT_JSONL={event_jsonl}"),
        format!("SOMA_CLIENT_BINDING_EVENT_SOURCE={expected_event_source}"),
        format!("SOMA_CLIENT_BINDING_NONCE={binding_nonce}"),
        format!("SOMA_CLIENT_BINDING_APP_HOOK_EVIDENCE_SOURCE={evidence_source}"),
        "SOMA_CONFIRM_REAL_CLIENT_HOOK=1".to_string(),
        "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1".to_string(),
        "tools/soma-client-record-app-hook-proof.sh".to_string(),
    ]);
    command
}

fn client_binding_operator_flow(
    client: &str,
    manifest_path: Option<&str>,
    expected_event_source: Option<&str>,
    binding_nonce: &str,
    artifact_dir: Option<&str>,
    config_root: Option<&str>,
    installed_config_path: Option<&str>,
    event_jsonl_path: Option<&str>,
    review_render_report_path: Option<&str>,
    render_evidence_path: Option<&str>,
    review_action_report_path: Option<&str>,
) -> Vec<ClientBindingOperatorFlowStep> {
    let manifest = manifest_path
        .or_else(|| default_client_binding_manifest(client))
        .unwrap_or("<client-binding-manifest>");
    let installed_config = installed_config_path
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{}-hook-config>", client));
    let expected_event_source = expected_event_source
        .map(str::to_string)
        .unwrap_or_else(|| default_private_event_source(client));
    let hook_trigger_title = private_client_hook_trigger_title(client);
    let hook_trigger_intent = private_client_hook_trigger_intent(client, &expected_event_source);
    let event_jsonl = event_jsonl_path.unwrap_or("$HOME/.soma/adapter/events.jsonl").to_string();
    let artifact_dir = artifact_dir
        .map(str::to_string)
        .unwrap_or_else(|| durable_client_artifact_dir_for_run(client, binding_nonce));
    let review_render_report = review_render_report_path.map(str::to_string).unwrap_or_else(|| {
        durable_client_artifact_path_in_dir(&artifact_dir, "review-render.json")
    });
    let render_evidence = render_evidence_path.map(str::to_string).unwrap_or_else(|| {
        durable_client_artifact_path_in_dir(&artifact_dir, "render-evidence.json")
    });
    let review_action_report = review_action_report_path.map(str::to_string).unwrap_or_else(|| {
        durable_client_artifact_path_in_dir(&artifact_dir, "review-action.json")
    });
    let app_hook_evidence_source =
        release_grade_operator_evidence_source(client, ClientBindingProofLevel::ObservedAppHook);
    let render_evidence_source = release_grade_operator_evidence_source(
        client,
        ClientBindingProofLevel::ObservedInClientRender,
    );
    let review_action_evidence_source = release_grade_operator_evidence_source(
        client,
        ClientBindingProofLevel::ObservedReviewAction,
    );
    let hook_readiness_command = vec![
        "env".to_string(),
        format!("SOMA_CLIENT_BINDING_CLIENT={client}"),
        format!("SOMA_CLIENT_BINDING_MANIFEST={manifest}"),
        format!("SOMA_CLIENT_BINDING_EVENT_JSONL={event_jsonl}"),
        format!("SOMA_CLIENT_BINDING_EVENT_SOURCE={expected_event_source}"),
        format!("SOMA_CLIENT_BINDING_NONCE={binding_nonce}"),
        "tools/soma-client-hook-readiness.sh".to_string(),
    ];
    let app_hook_record_command = observed_app_hook_record_wrapper_command(
        client,
        manifest,
        config_root,
        &event_jsonl,
        &expected_event_source,
        binding_nonce,
        &app_hook_evidence_source,
    );
    let review_action_plan_command = vec![
        "env".to_string(),
        format!("SOMA_REVIEW_CLIENT={client}"),
        "SOMA_REVIEW_FORMAT=json".to_string(),
        format!("SOMA_REVIEW_ACTION_REPORT={review_action_report}"),
        format!("SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT={review_action_report}"),
        "tools/soma-review-actions.sh".to_string(),
    ];
    let reference_binding_mcp_call = client_binding_record_proof_mcp_call(
        client,
        manifest,
        ClientBindingProofLevel::ReferenceBinding,
        "operator_flow_reference_binding",
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let observed_event_file_mcp_call = client_binding_record_proof_mcp_call(
        client,
        manifest,
        ClientBindingProofLevel::ObservedEventFile,
        "operator_flow_observed_event_file",
        Some(event_jsonl.as_str()),
        None,
        None,
        None,
        None,
        None,
    );
    let observed_app_hook_mcp_call = client_binding_record_proof_mcp_call(
        client,
        manifest,
        ClientBindingProofLevel::ObservedAppHook,
        &app_hook_evidence_source,
        Some(event_jsonl.as_str()),
        Some(installed_config.as_str()),
        None,
        None,
        None,
        Some("operator_confirm_real_app_invocation"),
    );
    let observed_in_client_render_mcp_call = client_binding_record_proof_mcp_call(
        client,
        manifest,
        ClientBindingProofLevel::ObservedInClientRender,
        &render_evidence_source,
        None,
        Some(installed_config.as_str()),
        Some(review_render_report.as_str()),
        Some(render_evidence.as_str()),
        None,
        Some("operator_confirm_in_client_render"),
    );
    let observed_review_action_mcp_call = client_binding_record_proof_mcp_call(
        client,
        manifest,
        ClientBindingProofLevel::ObservedReviewAction,
        &review_action_evidence_source,
        None,
        Some(installed_config.as_str()),
        None,
        None,
        Some(review_action_report.as_str()),
        Some("operator_confirm_review_action"),
    );
    let render_evidence_packet_mcp_call =
        client_render_evidence_packet_mcp_call(client, manifest, &review_render_report);
    let render_proof_prep_command = vec![
        "tools/soma-client-render-proof-prep.sh".to_string(),
        "--client".to_string(),
        client.to_string(),
        "--soma-bin".to_string(),
        crate::cli::binary_identity::resolved_soma_bin_for_operator_command(),
        "--manifest".to_string(),
        manifest.to_string(),
        "--artifact-dir".to_string(),
        artifact_dir.clone(),
    ];

    operator_flow_with_resolved_soma_binary(vec![
        ClientBindingOperatorFlowStep {
            id: "record_reference_binding_manifest".to_string(),
            title: "Record reference binding manifest".to_string(),
            intent: "Persist only the checked-in client binding contract; this proves the reference wrapper shape, not private app installation.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--proof-level".to_string(),
                "reference_binding".to_string(),
            ]),
            mcp_call: Some(reference_binding_mcp_call),
            requires_operator_action: false,
            records_proof: true,
            trust_boundary: "reference_binding_records_contract_only_and_does_not_prove_private_app_invocation".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "render_or_write_installed_config".to_string(),
            title: "Render proof-free installed client config".to_string(),
            intent: "Create the client hook config artifact with a per-install binding nonce; this is setup guidance only.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--render-installed-config".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--binding-nonce".to_string(),
                binding_nonce.to_string(),
            ]),
            mcp_call: None,
            requires_operator_action: false,
            records_proof: false,
            trust_boundary: "config_render_records_no_proof_and_does_not_prove_private_install".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "install_or_merge_private_client_config".to_string(),
            title: "Install or merge config in the private client".to_string(),
            intent: "Operator/client must place the rendered config where the private app will actually call SOMA wrappers.".to_string(),
            command: None,
            mcp_call: None,
            requires_operator_action: true,
            records_proof: false,
            trust_boundary: "human_or_client_install_step_is_required_before_app_hook_evidence_exists".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "check_installed_config".to_string(),
            title: "Preflight installed config".to_string(),
            intent: "Confirm the installed config references the lifecycle/spool wrapper, private event_source, JSONL path, and binding_nonce.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--check-installed-config".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--installed-config".to_string(),
                installed_config.clone(),
            ]),
            mcp_call: None,
            requires_operator_action: false,
            records_proof: false,
            trust_boundary: "installed_config_preflight_is_read_only_and_not_app_invocation_proof".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "trigger_private_client_hook".to_string(),
            title: hook_trigger_title,
            intent: hook_trigger_intent,
            command: Some(hook_readiness_command),
            mcp_call: None,
            requires_operator_action: true,
            records_proof: false,
            trust_boundary: "only_the_private_client_invocation_can_create_app_hook_evidence; readiness_probe_records_no_proof_and_only_explains_event_jsonl_state".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "drain_adapter_spool".to_string(),
            title: "Drain adapter spool".to_string(),
            intent: "Forward the private event JSONL into SOMA capture surfaces and produce a drain report.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-spool".to_string(),
                "--jsonl".to_string(),
                event_jsonl.clone(),
                "--checkpoint".to_string(),
                "$HOME/.soma/adapter/events.offset".to_string(),
            ]),
            mcp_call: None,
            requires_operator_action: false,
            records_proof: false,
            trust_boundary: "spool_drain_captures_events_but_does_not_record_binding_proof_by_itself".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "record_observed_event_file".to_string(),
            title: "Record observed event-file proof".to_string(),
            intent: "Persist proof that a wrapper-compatible event file exists; this still does not prove the private app installed or called the hook.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--proof-level".to_string(),
                "observed_event_file".to_string(),
                "--event-jsonl".to_string(),
                event_jsonl.clone(),
            ]),
            mcp_call: Some(observed_event_file_mcp_call),
            requires_operator_action: false,
            records_proof: true,
            trust_boundary: "observed_event_file_is_wrapper_event_evidence_only_not_private_app_invocation_proof".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "record_observed_app_hook".to_string(),
            title: "Record observed app-hook proof".to_string(),
            intent: "Persist app-hook proof only after the event came from the real private app and operator confirmation is explicit.".to_string(),
            command: Some(app_hook_record_command),
            mcp_call: Some(observed_app_hook_mcp_call),
            requires_operator_action: true,
            records_proof: true,
            trust_boundary: "observed_app_hook_requires_real_private_event_evidence_operator_confirmation_and_release_grade_confirmation".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "render_review_surface".to_string(),
            title: "Render review surface in the client".to_string(),
            intent: "Generate the read-only review-render report that the private client should visibly render.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "context".to_string(),
                "review-render".to_string(),
                "--client".to_string(),
                client.to_string(),
                "--format".to_string(),
                "json".to_string(),
                "--write-report".to_string(),
                review_render_report.clone(),
            ]),
            mcp_call: None,
            requires_operator_action: false,
            records_proof: false,
            trust_boundary: "review_render_is_read_only_and_never_verifies_promotes_applies_or_acknowledges".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "capture_in_client_render_evidence".to_string(),
            title: "Capture in-client render evidence".to_string(),
            intent: "Prepare proof-free review-render JSON/Markdown/HTML and a render evidence template, then fill structured soma.in_client_render_evidence.v1 evidence only after the review UI is visibly rendered in the private client.".to_string(),
            command: Some(render_proof_prep_command),
            mcp_call: Some(render_evidence_packet_mcp_call),
            requires_operator_action: true,
            records_proof: false,
            trust_boundary: "render_proof_prep_records_no_proof_and_render_evidence_is_ui_visibility_evidence_only_not_verification_or_promotion".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "record_observed_in_client_render".to_string(),
            title: "Record observed in-client render proof".to_string(),
            intent: "Persist UI-only render proof with structured evidence bound to the review-render report and installed config.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--proof-level".to_string(),
                "observed_in_client_render".to_string(),
                "--installed-config".to_string(),
                installed_config.clone(),
                "--review-render-report".to_string(),
                review_render_report,
                "--render-evidence".to_string(),
                render_evidence,
                "--evidence-source".to_string(),
                render_evidence_source,
                "--operator-confirm-in-client-render".to_string(),
                "--operator-confirm-release-grade-evidence".to_string(),
            ]),
            mcp_call: Some(observed_in_client_render_mcp_call),
            requires_operator_action: true,
            records_proof: true,
            trust_boundary: "observed_in_client_render_is_ui_only_release_grade_confirmed_and_never_verifies_promotes_applies_or_acknowledges".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "execute_rendered_review_control".to_string(),
            title: "Execute rendered review control in the client".to_string(),
            intent: "Operator/client must render the read-only action plan, activate one rendered review control_id, and save the resulting soma_review_action report before review-action proof can be recorded.".to_string(),
            command: Some(review_action_plan_command),
            mcp_call: None,
            requires_operator_action: true,
            records_proof: false,
            trust_boundary: "review_action_plan_probe_records_no_proof; executing_a_rendered_review_control_may_create_review_action_evidence_but_does_not_record_binding_proof_by_itself".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "record_observed_review_action".to_string(),
            title: "Record observed review-action loop proof".to_string(),
            intent: "Persist proof only after a rendered control_id from the private client produced a soma_review_action report with storage-gated non-cloud verification evidence.".to_string(),
            command: Some(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--manifest".to_string(),
                manifest.to_string(),
                "--proof-level".to_string(),
                "observed_review_action".to_string(),
                "--installed-config".to_string(),
                installed_config,
                "--review-action-report".to_string(),
                review_action_report,
                "--evidence-source".to_string(),
                review_action_evidence_source,
                "--operator-confirm-review-action".to_string(),
                "--operator-confirm-release-grade-evidence".to_string(),
            ]),
            mcp_call: Some(observed_review_action_mcp_call),
            requires_operator_action: true,
            records_proof: true,
            trust_boundary: "observed_review_action_requires_rendered_control_id_storage_gated_review_action_report_operator_confirmation_and_release_grade_confirmation".to_string(),
        },
        ClientBindingOperatorFlowStep {
            id: "verify_evidence_artifacts_and_status".to_string(),
            title: "Replay evidence artifacts and inspect readiness".to_string(),
            intent: "Confirm stored artifact paths still match their recorded byte lengths/fingerprints and readiness derives from coherent proof rows.".to_string(),
            command: Some(with_binding_target(
                client,
                manifest_path,
                vec!["soma", "adapter-binding-proof", "--status", "--json"],
            )),
            mcp_call: None,
            requires_operator_action: false,
            records_proof: false,
            trust_boundary: "status_and_artifact_replay_are_read_only_integrity_checks_not_new_evidence".to_string(),
        },
    ])
}

fn insert_continue_collector_step(
    operator_flow: &mut Vec<ClientBindingOperatorFlowStep>,
    client: &str,
    installed_config_path: Option<&str>,
    event_jsonl_path: Option<&str>,
    probe: Option<&ContinueDevdataCollectorProbe>,
) {
    if client != "continue" || !probe.is_some_and(|probe| probe.devdata_destination_visible) {
        return;
    }
    let event_jsonl = event_jsonl_path.unwrap_or("$HOME/.soma/adapter/events.jsonl");
    let binding_config = installed_config_path.unwrap_or("<continue-hook-config>");
    let Some(probe) = probe else {
        return;
    };
    let (id, title, intent, command, trust_boundary) = if matches!(
        probe.collector_status.as_str(),
        "probe_blocked" | "probe_unavailable"
    ) {
        (
                "check_continue_devdata_collector_status",
                "Check Continue dev-data collector status",
                "SOMA could not prove whether the local Continue dev-data collector is listening from this execution context; run the managed status command from the operator shell, avoid starting duplicate collectors, then trigger a real Continue extension chat/edit/review action only after the collector is proven listening.".to_string(),
                continue_devdata_collector_managed_status_command(event_jsonl, binding_config),
                "continue_devdata_collector_status_step_is_operator_guidance_only: checking collector status records no client-binding proof row, creates no verification event, promotes no cloud draft, and cannot satisfy observed_app_hook without a later real Continue event",
            )
    } else if !probe.collector_listening {
        (
                "start_continue_devdata_collector_before_real_hook",
                "Start Continue dev-data collector",
                "Start the local Continue dev-data collector before triggering a real Continue extension chat/edit/review action (not Cursor Agent/Composer); the collector only appends private lifecycle events and records no proof by itself.".to_string(),
                continue_devdata_collector_managed_start_command(event_jsonl, binding_config),
                "continue_devdata_collector_step_is_operator_guidance_only: starting the collector records no client-binding proof row, creates no verification event, promotes no cloud draft, and cannot satisfy observed_app_hook without a later real Continue event",
            )
    } else {
        return;
    };
    let step = ClientBindingOperatorFlowStep {
        id: id.to_string(),
        title: title.to_string(),
        intent,
        command: Some(command),
        mcp_call: None,
        requires_operator_action: true,
        records_proof: false,
        trust_boundary: trust_boundary.to_string(),
    };
    let insert_at = operator_flow
        .iter()
        .position(|existing| existing.id == "trigger_private_client_hook")
        .unwrap_or(operator_flow.len());
    operator_flow.insert(insert_at, step);
}

fn continue_devdata_collector_command(event_jsonl: &str, binding_config: &str) -> Vec<String> {
    let (host, port) = continue_devdata_collector_endpoint();
    vec![
        "tools/soma-continue-devdata-collector.py".to_string(),
        "--host".to_string(),
        host,
        "--port".to_string(),
        port.to_string(),
        "--soma-bin".to_string(),
        crate::cli::binary_identity::resolved_soma_bin_for_operator_command(),
        "--jsonl".to_string(),
        event_jsonl.to_string(),
        "--binding-config".to_string(),
        binding_config.to_string(),
    ]
}

fn continue_devdata_collector_managed_start_command(
    event_jsonl: &str,
    binding_config: &str,
) -> Vec<String> {
    let mut command = vec!["tools/soma-continue-devdata-start.sh".to_string(), "start".to_string()];
    let direct = continue_devdata_collector_command(event_jsonl, binding_config);
    command.extend(direct.into_iter().skip(1));
    command
}

fn continue_devdata_collector_managed_status_command(
    event_jsonl: &str,
    binding_config: &str,
) -> Vec<String> {
    let mut command =
        vec!["tools/soma-continue-devdata-start.sh".to_string(), "status".to_string()];
    let direct = continue_devdata_collector_command(event_jsonl, binding_config);
    command.extend(direct.into_iter().skip(1));
    command
}

fn continue_devdata_collector_probe(
    client: &str,
    config_root: &Path,
    installed_config_path: Option<&str>,
    event_jsonl_path: Option<&str>,
) -> Option<ContinueDevdataCollectorProbe> {
    if client != "continue" {
        return None;
    }
    let devdata_destination_visible = continue_devdata_destination_visible(config_root);
    let (collector_status, collector_listening, collector_host, collector_port, collector_error) =
        continue_devdata_collector_observation();
    let status_command = devdata_destination_visible.then(|| {
        continue_devdata_collector_managed_status_command(
            event_jsonl_path.unwrap_or("$HOME/.soma/adapter/events.jsonl"),
            installed_config_path.unwrap_or("<continue-hook-config>"),
        )
    });
    let start_command =
        (devdata_destination_visible && collector_status == "not_listening").then(|| {
            continue_devdata_collector_managed_start_command(
                event_jsonl_path.unwrap_or("$HOME/.soma/adapter/events.jsonl"),
                installed_config_path.unwrap_or("<continue-hook-config>"),
            )
        });
    Some(ContinueDevdataCollectorProbe {
        devdata_destination_visible,
        collector_status,
        collector_listening,
        collector_host,
        collector_port,
        collector_error,
        status_command,
        start_command,
        trust_boundary: "continue_devdata_collector_probe_is_read_only: reports local config visibility and TCP collector probe only; records no proof row, creates no verification event, executes no client action, and promotes no cloud draft",
    })
}

fn continue_devdata_destination_visible(config_root: &Path) -> bool {
    [
        ".continue/config.yaml",
        ".continue/config.yml",
        ".continue/config.json",
        ".continue/config.ts",
    ]
    .iter()
    .map(|relative| config_root.join(relative))
    .any(|path| {
        fs::read_to_string(path).is_ok_and(|text| {
            text.contains(CONTINUE_DEVDATA_COLLECTOR_PATH)
                || text.contains(&format!(
                    "{}:{}",
                    CONTINUE_DEVDATA_COLLECTOR_HOST, CONTINUE_DEVDATA_COLLECTOR_PORT
                ))
        })
    })
}

fn continue_devdata_collector_endpoint() -> (String, u16) {
    let host = std::env::var("SOMA_CONTINUE_DEVDATA_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CONTINUE_DEVDATA_COLLECTOR_HOST.to_string());
    let port = std::env::var("SOMA_CONTINUE_DEVDATA_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(CONTINUE_DEVDATA_COLLECTOR_PORT);
    (host, port)
}

fn continue_devdata_collector_observation() -> (String, bool, String, u16, Option<String>) {
    let (host, port) = continue_devdata_collector_endpoint();
    if let Ok(status) = std::env::var("SOMA_CONTINUE_DEVDATA_COLLECTOR_STATUS") {
        match status.trim().to_ascii_lowercase().as_str() {
            "listening" => return ("listening".to_string(), true, host, port, None),
            "not_listening" | "not-listening" | "missing" | "stopped" => {
                return ("not_listening".to_string(), false, host, port, None);
            }
            "probe_blocked" | "probe-blocked" => {
                return (
                    "probe_blocked".to_string(),
                    false,
                    host,
                    port,
                    Some("collector TCP probe blocked by execution context".to_string()),
                );
            }
            "probe_unavailable" | "probe-unavailable" => {
                return (
                    "probe_unavailable".to_string(),
                    false,
                    host,
                    port,
                    Some("collector TCP probe unavailable in execution context".to_string()),
                );
            }
            _ => {}
        }
    }
    let Ok(mut addrs) = (host.as_str(), port).to_socket_addrs() else {
        return (
            "probe_unavailable".to_string(),
            false,
            host,
            port,
            Some("collector address did not resolve".to_string()),
        );
    };
    let Some(addr) = addrs.next() else {
        return (
            "probe_unavailable".to_string(),
            false,
            host,
            port,
            Some("no collector socket address resolved".to_string()),
        );
    };
    match TcpStream::connect_timeout(&addr, Duration::from_millis(150)) {
        Ok(_) => ("listening".to_string(), true, host, port, None),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            ("probe_blocked".to_string(), false, host, port, Some(err.to_string()))
        }
        Err(err) => ("not_listening".to_string(), false, host, port, Some(err.to_string())),
    }
}

fn private_client_hook_trigger_title(client: &str) -> String {
    format!("Trigger the {} hook", private_client_display_name(client))
}

fn private_client_display_name(client: &str) -> String {
    match client {
        "codex-app" => "Codex app".to_string(),
        "cursor" => "Cursor".to_string(),
        "continue" => "Continue".to_string(),
        "claude-code" => "Claude Code".to_string(),
        _ => client.to_string(),
    }
}

fn private_client_hook_trigger_intent(client: &str, expected_event_source: &str) -> String {
    match client {
        "codex-app" => format!(
            "Quit or restart the stale Codex app process first if `soma clients` reports restart_recommended, then reopen Codex app and run a real turn that should call the configured wrapper and append a private {expected_event_source} event; rerun the read-only readiness probe until the matching event_source and binding_nonce are observed. `open -a Codex` is only a reopen hint and does not force a stale running process to reload the notify config."
        ),
        "continue" => format!(
            "Reload Continue or its host editor if needed, then run a real Continue extension chat/edit/review action (not Cursor Agent/Composer) that should call the configured wrapper and append a private {expected_event_source} event; rerun the read-only readiness probe until the matching event_source and binding_nonce are observed."
        ),
        "cursor" => format!(
            "Run a real Cursor action from the installed Cursor binding so the configured wrapper appends a private {expected_event_source} event; rerun the read-only readiness probe until the matching event_source and binding_nonce are observed."
        ),
        _ => format!(
            "Run a real {} action that should call the configured wrapper and append a private {expected_event_source} event; rerun the read-only readiness probe until the matching event_source and binding_nonce are observed.",
            private_client_display_name(client)
        ),
    }
}

fn client_binding_evidence_bundle_gaps(
    readiness: &AdapterBindingProofStatusOutcome,
    discovery: &AdapterBindingInstalledConfigDiscoveryOutcome,
) -> Vec<String> {
    let mut gaps = Vec::new();
    let primary = readiness.clients.first();
    if primary.is_none_or(|client| !client.ready_for_private_client_claim) {
        gaps.push("ready_for_private_client_claim_not_yet_proven".to_string());
    }
    if discovery.eligible_candidates == 0 {
        gaps.push("no_eligible_installed_config_candidate_found".to_string());
    }
    if discovery.private_client_target_eligible_candidates == 0 {
        gaps.push("private_client_target_config_not_discovered".to_string());
    }
    if primary.is_none_or(|client| !client.has_observed_app_hook) {
        gaps.push("observed_app_hook_proof_missing".to_string());
    }
    if primary.is_none_or(|client| !client.has_observed_in_client_render) {
        gaps.push("observed_in_client_render_proof_missing".to_string());
    }
    if primary.is_none_or(|client| !client.has_observed_review_action) {
        gaps.push("observed_review_action_proof_missing".to_string());
    }
    if primary.is_some_and(|client| !client.artifact_failures.is_empty()) {
        gaps.push("stored_evidence_artifact_replay_has_failures".to_string());
    }
    if primary.is_some_and(|client| !client.coherence_failures.is_empty()) {
        gaps.push(
            "app_hook_render_and_review_action_proofs_do_not_share_installed_config_identity"
                .to_string(),
        );
    }
    gaps
}

fn with_binding_target(
    client: &str,
    manifest_path: Option<&str>,
    prefix: Vec<&str>,
) -> Vec<String> {
    let mut command: Vec<String> = prefix.into_iter().map(str::to_string).collect();
    if let Some(manifest) = manifest_path {
        command.push("--manifest".to_string());
        command.push(manifest.to_string());
    } else {
        command.push("--client".to_string());
        command.push(client.to_string());
    }
    command
}

fn commands_with_resolved_soma_binary(commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();
    commands
        .into_iter()
        .map(|command| {
            crate::cli::binary_identity::command_with_current_binary_when_path_soma_differs(
                command,
                &binary_identity,
            )
        })
        .collect()
}

fn operator_flow_with_resolved_soma_binary(
    mut steps: Vec<ClientBindingOperatorFlowStep>,
) -> Vec<ClientBindingOperatorFlowStep> {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();
    for step in &mut steps {
        if let Some(command) = step.command.take() {
            step.command = Some(
                crate::cli::binary_identity::command_with_current_binary_when_path_soma_differs(
                    command,
                    &binary_identity,
                ),
            );
        }
    }
    steps
}

pub fn run_prepare_installed_config_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingInstalledConfigPreparationOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let (binding_nonce, generated_binding_nonce) = resolve_binding_nonce_for_prepare(args)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let event = default_lifecycle_event(&client);
    let lifecycle_environment = json!({
        "SOMA_ADAPTER_LIFECYCLE_CLIENT": client,
        "SOMA_ADAPTER_LIFECYCLE_EVENT": event,
        "SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE": expected_event_source,
        "SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE": binding_nonce,
        "SOMA_ADAPTER_LIFECYCLE_JSONL": "$HOME/.soma/adapter/events.jsonl",
        "SOMA_ADAPTER_LIFECYCLE_PROJECT": "$SOMA_PROJECT",
        "SOMA_ADAPTER_LIFECYCLE_SESSION": "$SOMA_SESSION_ID",
        "SOMA_ADAPTER_LIFECYCLE_CWD": "$PWD",
    });
    let spool_append_environment = json!({
        "SOMA_ADAPTER_BINDING_NONCE": binding_nonce,
    });
    let installed_config_snippet = json!({
        "client": client,
        "hook": "tools/soma-adapter-lifecycle.sh",
        "env": lifecycle_environment,
    });
    let manifest_arg = manifest_path.as_deref().unwrap_or("<client-binding-manifest>");
    let installed_config_arg = format!("<{}-hook-config>", client);
    let next_commands = commands_with_resolved_soma_binary(vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--check-installed-config".to_string(),
            "--manifest".to_string(),
            manifest_arg.to_string(),
            "--installed-config".to_string(),
            installed_config_arg.clone(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest_arg.to_string(),
            "--proof-level".to_string(),
            "observed_app_hook".to_string(),
            "--event-jsonl".to_string(),
            "$HOME/.soma/adapter/events.jsonl".to_string(),
            "--installed-config".to_string(),
            installed_config_arg,
            "--operator-confirm-real-app-invocation".to_string(),
        ],
    ]);

    Ok(AdapterBindingInstalledConfigPreparationOutcome {
        client,
        manifest_path,
        event_source: expected_event_source,
        binding_nonce,
        generated_binding_nonce,
        lifecycle_environment,
        spool_append_environment,
        installed_config_snippet,
        next_commands,
        trust_boundary: "prepare_installed_config_is_dry_run: generates an install-specific binding nonce and config snippet only; it records no proof row, verifies no app invocation, and does not promote cloud drafts".to_string(),
    })
}

pub fn run_render_installed_config_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingInstalledConfigRenderOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let expected_event_source = resolve_expected_event_source_for_check(args, &client)?;
    let (binding_nonce, generated_binding_nonce) = resolve_binding_nonce_for_prepare(args)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let installed_config = render_installed_config_artifact(
        &client,
        &expected_event_source,
        &binding_nonce,
        manifest_path.as_deref(),
    );
    let output_path = args
        .write_installed_config
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut checks = None;
    let mut eligible_for_observed_app_hook = None;
    if let Some(path) = output_path.as_deref() {
        let path = PathBuf::from(path);
        if path.exists() {
            return Err(AdapterBindingProofError::MalformedInput(format!(
                "installed config output `{}` already exists; refusing to overwrite",
                path.display()
            )));
        }
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| {
                AdapterBindingProofError::Io(format!(
                    "create installed config parent `{}`: {e}",
                    parent.display()
                ))
            })?;
        }
        let mut text = serde_json::to_string_pretty(&installed_config).map_err(|e| {
            AdapterBindingProofError::MalformedInput(format!(
                "render installed config artifact: {e}"
            ))
        })?;
        text.push('\n');
        fs::write(&path, text).map_err(|e| {
            AdapterBindingProofError::Io(format!(
                "write installed config `{}`: {e}",
                path.display()
            ))
        })?;
        let scan = scan_installed_config(&path, &client, Some(&expected_event_source))?;
        eligible_for_observed_app_hook =
            Some(observed_app_hook_missing_requirements(&scan).is_empty());
        checks = Some(scan);
    }

    let installed_config_arg = output_path
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{}-hook-config>", client));
    let manifest_arg = manifest_path.as_deref().unwrap_or("<client-binding-manifest>");
    let next_commands = commands_with_resolved_soma_binary(vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--check-installed-config".to_string(),
            "--manifest".to_string(),
            manifest_arg.to_string(),
            "--installed-config".to_string(),
            installed_config_arg.clone(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest_arg.to_string(),
            "--proof-level".to_string(),
            "observed_app_hook".to_string(),
            "--event-jsonl".to_string(),
            "$HOME/.soma/adapter/events.jsonl".to_string(),
            "--installed-config".to_string(),
            installed_config_arg,
            "--operator-confirm-real-app-invocation".to_string(),
        ],
    ]);

    Ok(AdapterBindingInstalledConfigRenderOutcome {
        client,
        manifest_path,
        event_source: expected_event_source,
        binding_nonce,
        generated_binding_nonce,
        output_path,
        wrote_file: checks.is_some(),
        installed_config,
        eligible_for_observed_app_hook,
        checks,
        next_commands,
        trust_boundary: "render_installed_config_is_config_only: renders or writes an operator-selected client hook config artifact; it records no proof row, verifies no app invocation, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation by itself".to_string(),
    })
}

pub fn run_render_evidence_packet_blocking(
    args: &AdapterBindingProofArgs,
) -> Result<AdapterBindingRenderEvidencePacketOutcome, AdapterBindingProofError> {
    let client = resolve_client_for_check(args)?;
    let manifest_path = resolved_manifest_path_for_client(args, &client);
    let review_render_report = args.review_render_report.as_deref().ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "--render-render-evidence requires --review-render-report".to_string(),
        )
    })?;
    let review_render_report_path = canonical_or_raw(review_render_report);
    let review_render_file_scan =
        scan_file_fingerprint(&review_render_report_path, "review render report")?;
    let review_render = read_json_file(&review_render_report_path, "review render report")?;
    validate_review_render(Some(&review_render), &client)?;
    let render_evidence = render_in_client_render_evidence_packet(
        &client,
        &review_render_file_scan.fingerprint,
        &review_render,
    )?;

    let output_path = args
        .write_render_evidence
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut preflight_scan = None;
    let mut missing_requirements = vec![
        "source_must_be_manual_operator_or_client_capture".to_string(),
        "observed_at_ns_must_be_positive".to_string(),
        "rendered_surfaces_must_not_contain_template_placeholders".to_string(),
        "rendered_surfaces_must_include_visible_surface".to_string(),
    ];
    if let Some(path) = output_path.as_deref() {
        let path = PathBuf::from(path);
        if path.exists() {
            return Err(AdapterBindingProofError::MalformedInput(format!(
                "render evidence output `{}` already exists; refusing to overwrite",
                path.display()
            )));
        }
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| {
                AdapterBindingProofError::Io(format!(
                    "create render evidence parent `{}`: {e}",
                    parent.display()
                ))
            })?;
        }
        let mut text = serde_json::to_string_pretty(&render_evidence).map_err(|e| {
            AdapterBindingProofError::MalformedInput(format!("render evidence packet: {e}"))
        })?;
        text.push('\n');
        fs::write(&path, text).map_err(|e| {
            AdapterBindingProofError::Io(format!("write render evidence `{}`: {e}", path.display()))
        })?;
        let mut scan = scan_render_evidence(&path)?;
        let expected_control_ids = review_render_control_ids(&review_render);
        let expected_surface_names =
            review_render_required_surface_names(&review_render, &expected_control_ids);
        let missing = annotate_render_evidence_scan(
            &mut scan,
            &client,
            Some(&review_render_file_scan.fingerprint),
            review_render_workbench_version(&review_render),
            review_render_interaction_contract_version(&review_render),
            &expected_surface_names,
            &expected_control_ids,
        );
        missing_requirements = missing.iter().map(|value| (*value).to_string()).collect();
        preflight_scan = Some(json!(scan));
    }

    let manifest_arg = manifest_path.as_deref().unwrap_or("<client-binding-manifest>");
    let render_evidence_arg = output_path
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| durable_client_artifact_path(&client, "render-evidence.json"));
    let next_commands = commands_with_resolved_soma_binary(vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest_arg.to_string(),
            "--proof-level".to_string(),
            "observed_in_client_render".to_string(),
            "--installed-config".to_string(),
            format!("<{}-hook-config>", client),
            "--review-render-report".to_string(),
            review_render_report_path.to_string_lossy().into_owned(),
            "--render-evidence".to_string(),
            render_evidence_arg,
            "--operator-confirm-in-client-render".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ],
        with_binding_target(
            &client,
            manifest_path.as_deref(),
            vec!["soma", "adapter-binding-proof", "--real-app-proof-kit"],
        ),
    ]);

    Ok(AdapterBindingRenderEvidencePacketOutcome {
        client,
        manifest_path,
        review_render_report_path: review_render_report_path.to_string_lossy().into_owned(),
        review_render_fingerprint: review_render_file_scan.fingerprint,
        output_path,
        wrote_file: preflight_scan.is_some(),
        render_evidence,
        preflight_scan,
        missing_requirements,
        next_commands,
        trust_boundary: "render_render_evidence_is_template_only: fills non-observational bindings from a review-render report but leaves visible-render observations as placeholders; it records no proof row, verifies no claim, promotes no cloud draft, applies no proposal, and cannot prove in-client rendering without a filled evidence file plus explicit operator confirmation".to_string(),
    })
}

fn render_in_client_render_evidence_packet(
    client: &str,
    review_render_fingerprint: &str,
    review_render: &Value,
) -> Result<Value, AdapterBindingProofError> {
    let review_workbench_version =
        review_render_workbench_version(review_render).ok_or_else(|| {
            AdapterBindingProofError::MalformedInput(
                "review-render report missing workbench.version".to_string(),
            )
        })?;
    let review_interaction_contract_version =
        review_render_interaction_contract_version(review_render).ok_or_else(|| {
            AdapterBindingProofError::MalformedInput(
                "review-render report missing interaction_contract.version".to_string(),
            )
        })?;
    let expected_control_ids = review_render_control_ids(review_render);
    let expected_surface_names =
        review_render_required_surface_names(review_render, &expected_control_ids);
    Ok(json!({
        "schema": IN_CLIENT_RENDER_EVIDENCE_SCHEMA,
        "client": client,
        "source": "<manual_operator_or_client_capture>",
        "observed_at_ns": "<positive_unix_epoch_nanoseconds_after_visible_render>",
        "review_render_fingerprint": review_render_fingerprint,
        "review_workbench_version": review_workbench_version,
        "review_interaction_contract_version": review_interaction_contract_version,
        "expected_surface_names": expected_surface_names,
        "rendered_surfaces": [
            {
                "name": expected_surface_names
                    .first()
                    .map(String::as_str)
                    .unwrap_or("<visible_review_surface_name>"),
                "kind": "<client_surface_kind>",
                "title": "<visible_surface_title>",
                "visible": "<true_after_visible_render>",
                "rendered_control_ids": expected_control_ids
            }
        ],
        "rendered_control_ids": expected_control_ids,
        "trust_boundary": IN_CLIENT_RENDER_EVIDENCE_TRUST_BOUNDARY,
        "capture_note": "fill source, observed_at_ns, and rendered_surfaces only after the private client visibly renders this review surface; this packet is not proof until recorded with operator confirmation"
    }))
}

fn render_installed_config_artifact(
    client: &str,
    event_source: &str,
    binding_nonce: &str,
    manifest_path: Option<&str>,
) -> Value {
    let lifecycle_event = default_lifecycle_event(client);
    json!({
        "schema": "soma.installed_client_binding.v1",
        "client": client,
        "status": "operator_install_artifact",
        "trust_boundary": "This config artifact does not prove the private app installed or called the hook; record observed_app_hook only after real event evidence and operator confirmation.",
        "manifest_path": manifest_path,
        "lifecycle_hook": {
            "wrapper": "tools/soma-adapter-lifecycle.sh",
            "client": client,
            "event": lifecycle_event,
            "env": {
                "SOMA_ADAPTER_LIFECYCLE_CLIENT": client,
                "SOMA_ADAPTER_LIFECYCLE_EVENT": lifecycle_event,
                "SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE": event_source,
                "SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE": binding_nonce,
                "SOMA_ADAPTER_LIFECYCLE_JSONL": "$HOME/.soma/adapter/events.jsonl",
                "SOMA_ADAPTER_LIFECYCLE_PROJECT": "$SOMA_PROJECT",
                "SOMA_ADAPTER_LIFECYCLE_SESSION": "$SOMA_SESSION_ID",
                "SOMA_ADAPTER_LIFECYCLE_CWD": "$PWD"
            }
        },
        "spool_append": {
            "wrapper": "tools/soma-adapter-spool-append.sh",
            "env": {
                "SOMA_ADAPTER_BINDING_NONCE": binding_nonce,
                "SOMA_ADAPTER_SPOOL_JSONL": "$HOME/.soma/adapter/events.jsonl"
            }
        },
        "spool_drain": {
            "wrapper": "tools/soma-adapter-spool-watch.sh",
            "jsonl": "$HOME/.soma/adapter/events.jsonl"
        },
        "review_ui": {
            "wrapper": "tools/soma-review-render.sh",
            "client": client
        },
        "scope_contract": {
            "source": "soma.installed_client_binding.scope_contract.v1",
            "persona_store": true,
            "project_is_provenance_metadata": true,
            "project_creates_separate_store": false,
            "runtime_scope_env": [
                "SOMA_SESSION_ID",
                "SOMA_CLIENT",
                "SOMA_PROJECT",
                "PWD"
            ],
            "trust_boundary": "scope_env_only: project/session/cwd values are runtime provenance for capture and do not prove historical isolation, create a per-project store, or verify a private app invocation"
        },
        "proof": {
            "event_source": event_source,
            "binding_nonce": binding_nonce,
            "required_next_proof_level": "observed_app_hook",
            "required_operator_confirmation": "--operator-confirm-real-app-invocation"
        }
    })
}

fn discover_one_installed_config(
    path: &Path,
    client: &str,
    expected_event_source: &str,
    manifest_path: Option<&str>,
) -> InstalledConfigCandidate {
    if !path.exists() {
        return InstalledConfigCandidate {
            path: path.to_string_lossy().into_owned(),
            exists: false,
            eligible_for_observed_app_hook: false,
            missing_requirements: Vec::new(),
            checks: None,
            error: None,
            next_commands: Vec::new(),
        };
    }

    match scan_installed_config(path, client, Some(expected_event_source)) {
        Ok(checks) => {
            let missing_requirements = observed_app_hook_missing_requirements(&checks);
            let eligible_for_observed_app_hook = missing_requirements.is_empty();
            let next_commands = eligible_for_observed_app_hook
                .then(|| discovered_config_next_commands(path, manifest_path))
                .unwrap_or_default();
            InstalledConfigCandidate {
                path: path.to_string_lossy().into_owned(),
                exists: true,
                eligible_for_observed_app_hook,
                missing_requirements,
                checks: Some(checks),
                error: None,
                next_commands,
            }
        }
        Err(err) => InstalledConfigCandidate {
            path: path.to_string_lossy().into_owned(),
            exists: true,
            eligible_for_observed_app_hook: false,
            missing_requirements: Vec::new(),
            checks: None,
            error: Some(err.to_string()),
            next_commands: Vec::new(),
        },
    }
}

fn discovered_config_next_commands(path: &Path, manifest_path: Option<&str>) -> Vec<Vec<String>> {
    let installed_config = path.to_string_lossy().into_owned();
    let manifest = manifest_path.unwrap_or("<client-binding-manifest>").to_string();
    vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--check-installed-config".to_string(),
            "--manifest".to_string(),
            manifest.clone(),
            "--installed-config".to_string(),
            installed_config.clone(),
        ],
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest,
            "--proof-level".to_string(),
            "observed_app_hook".to_string(),
            "--event-jsonl".to_string(),
            "$HOME/.soma/adapter/events.jsonl".to_string(),
            "--installed-config".to_string(),
            installed_config,
            "--operator-confirm-real-app-invocation".to_string(),
        ],
    ]
}

fn default_client_binding_manifest(client: &str) -> Option<&'static str> {
    match client {
        "codex-app" => Some("tools/client-bindings/codex-app-soma-binding.json.example"),
        "cursor" => Some("tools/client-bindings/cursor-soma-binding.json.example"),
        "continue" => Some("tools/client-bindings/continue-soma-binding.json.example"),
        _ => None,
    }
}

fn resolved_manifest_path_for_client(
    args: &AdapterBindingProofArgs,
    client: &str,
) -> Option<String> {
    args.manifest
        .as_deref()
        .map(|path| canonical_or_raw(path).to_string_lossy().into_owned())
        .or_else(|| default_client_binding_manifest(client).map(str::to_string))
}

fn resolve_config_root(
    args: &AdapterBindingProofArgs,
) -> Result<PathBuf, AdapterBindingProofError> {
    if let Some(root) = args.config_root.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(root));
    }
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir).ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "--discover-installed-config requires HOME or --config-root".to_string(),
        )
    })
}

fn discover_installed_config_paths(
    args: &AdapterBindingProofArgs,
    client: &str,
    config_root: &Path,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) =
        args.installed_config.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        paths.push(PathBuf::from(path));
    }
    for relative in default_installed_config_relpaths(client) {
        paths.push(config_root.join(relative));
    }
    dedup_paths(paths)
}

fn default_installed_config_relpaths(client: &str) -> &'static [&'static str] {
    match client {
        "cursor" => &[
            ".soma/client-bindings/cursor-installed-binding.json",
            ".cursor/soma-installed-binding.json",
        ],
        "continue" => &[
            ".soma/client-bindings/continue-installed-binding.json",
            ".continue/soma-installed-binding.json",
        ],
        "codex-app" => &[
            ".soma/client-bindings/codex-app-installed-binding.json",
            ".codex/soma-installed-binding.json",
        ],
        "claude-code" => &[
            ".soma/client-bindings/claude-code-installed-binding.json",
            ".claude/soma-installed-binding.json",
        ],
        _ => &[],
    }
}

fn private_client_target_installed_config_relpaths(client: &str) -> &'static [&'static str] {
    match client {
        "cursor" => &[".cursor/soma-installed-binding.json"],
        "continue" => &[".continue/soma-installed-binding.json"],
        "codex-app" => &[".codex/soma-installed-binding.json"],
        "claude-code" => &[".claude/soma-installed-binding.json"],
        _ => &[],
    }
}

fn is_private_client_target_config_path(path: &str, relpaths: &[&str]) -> bool {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_matches('/');
    relpaths
        .iter()
        .any(|relpath| normalized == *relpath || normalized.ends_with(&format!("/{relpath}")))
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn verify_proof_artifacts(
    proof: &StoredClientBindingProof,
) -> ClientBindingEvidenceArtifactVerification {
    let artifact_checks: Vec<_> = [
        verify_artifact_reference(
            "manifest",
            Some(proof.manifest_path.as_str()),
            proof.checks_json.get("manifest_scan"),
        ),
        verify_artifact_reference(
            "event_jsonl",
            proof.event_jsonl_path.as_deref(),
            proof.checks_json.get("event_scan"),
        ),
        verify_artifact_reference(
            "installed_config",
            proof.installed_config_path.as_deref(),
            proof.checks_json.get("installed_config_scan"),
        ),
        verify_artifact_reference(
            "render_evidence",
            proof.render_evidence_path.as_deref(),
            proof.checks_json.get("render_evidence_scan"),
        ),
        verify_artifact_reference(
            "review_action_report",
            proof.review_action_report_path.as_deref(),
            proof.checks_json.get("review_action_report_scan"),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    let all_artifacts_verified = !artifact_checks.is_empty()
        && artifact_checks.iter().all(|check| evidence_artifact_status_is_verified(check.status));

    ClientBindingEvidenceArtifactVerification {
        proof_id: proof.id,
        client: proof.client.clone(),
        proof_level: proof.proof_level,
        all_artifacts_verified,
        artifact_checks,
    }
}

fn verify_artifact_reference(
    kind: &str,
    row_path: Option<&str>,
    scan: Option<&Value>,
) -> Option<EvidenceArtifactCheck> {
    let scan = scan.filter(|value| !value.is_null());
    let row_path = row_path.map(str::trim).filter(|value| !value.is_empty());
    if scan.is_none() && row_path.is_none() {
        return None;
    }

    let path = scan
        .and_then(|value| value.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(row_path)
        .map(ToOwned::to_owned);
    let expected_byte_len = scan.and_then(|value| value.get("byte_len")).and_then(Value::as_u64);
    let expected_fingerprint = scan
        .and_then(|value| value.get("fingerprint"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let Some(path_value) = path.clone() else {
        return Some(EvidenceArtifactCheck {
            kind: kind.to_string(),
            path,
            expected_byte_len,
            actual_byte_len: None,
            expected_fingerprint,
            actual_fingerprint: None,
            status: EvidenceArtifactStatus::MissingPath,
            error: Some("stored proof row has no artifact path".to_string()),
        });
    };

    if expected_byte_len.is_none() || expected_fingerprint.is_none() {
        return Some(EvidenceArtifactCheck {
            kind: kind.to_string(),
            path,
            expected_byte_len,
            actual_byte_len: None,
            expected_fingerprint,
            actual_fingerprint: None,
            status: EvidenceArtifactStatus::MissingExpectedRecord,
            error: Some(
                "stored proof row lacks byte length or fingerprint for this artifact".to_string(),
            ),
        });
    }

    match fs::read(&path_value) {
        Ok(bytes) => {
            let actual_byte_len = bytes.len() as u64;
            let actual_fingerprint = stable_content_fingerprint(&bytes);
            let status = artifact_replay_status(
                kind,
                &bytes,
                actual_byte_len,
                actual_fingerprint.as_str(),
                expected_byte_len,
                expected_fingerprint.as_deref(),
            );
            Some(EvidenceArtifactCheck {
                kind: kind.to_string(),
                path,
                expected_byte_len,
                actual_byte_len: Some(actual_byte_len),
                expected_fingerprint,
                actual_fingerprint: Some(actual_fingerprint),
                status,
                error: None,
            })
        }
        Err(err) => {
            let status = if err.kind() == std::io::ErrorKind::NotFound {
                EvidenceArtifactStatus::MissingFile
            } else {
                EvidenceArtifactStatus::Unreadable
            };
            Some(EvidenceArtifactCheck {
                kind: kind.to_string(),
                path,
                expected_byte_len,
                actual_byte_len: None,
                expected_fingerprint,
                actual_fingerprint: None,
                status,
                error: Some(err.to_string()),
            })
        }
    }
}

fn artifact_replay_status(
    kind: &str,
    bytes: &[u8],
    actual_byte_len: u64,
    actual_fingerprint: &str,
    expected_byte_len: Option<u64>,
    expected_fingerprint: Option<&str>,
) -> EvidenceArtifactStatus {
    if Some(actual_byte_len) == expected_byte_len
        && Some(actual_fingerprint) == expected_fingerprint
    {
        return EvidenceArtifactStatus::Verified;
    }

    if kind == "event_jsonl" {
        if let (Some(expected_len), Some(expected_fp)) = (expected_byte_len, expected_fingerprint) {
            if actual_byte_len > expected_len {
                if let Ok(prefix_len) = usize::try_from(expected_len) {
                    if prefix_len <= bytes.len()
                        && stable_content_fingerprint(&bytes[..prefix_len]) == expected_fp
                    {
                        return EvidenceArtifactStatus::VerifiedAppendOnlyGrowth;
                    }
                }
            }
        }
    }

    EvidenceArtifactStatus::Changed
}

fn scan_file_fingerprint(
    path: &Path,
    label: &str,
) -> Result<FileFingerprintScan, AdapterBindingProofError> {
    let bytes = fs::read(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read {label} `{}`: {e}", path.display()))
    })?;
    Ok(FileFingerprintScan {
        path: path.to_string_lossy().into_owned(),
        byte_len: bytes.len() as u64,
        fingerprint: stable_content_fingerprint(&bytes),
    })
}

fn scan_installed_config(
    path: &Path,
    client: &str,
    expected_event_source: Option<&str>,
) -> Result<InstalledConfigScan, AdapterBindingProofError> {
    let metadata = fs::metadata(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("metadata installed config `{}`: {e}", path.display()))
    })?;
    let bytes = fs::read(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read installed config `{}`: {e}", path.display()))
    })?;
    let byte_len = bytes.len() as u64;
    let fingerprint = stable_content_fingerprint(&bytes);
    let raw = String::from_utf8(bytes).map_err(|e| {
        AdapterBindingProofError::MalformedInput(format!(
            "installed config `{}` must be utf-8: {e}",
            path.display()
        ))
    })?;
    let lower = raw.to_ascii_lowercase();
    let binding_nonce = extract_binding_nonce_from_text(&raw);
    Ok(InstalledConfigScan {
        path: path.to_string_lossy().into_owned(),
        byte_len,
        modified_at_ns: modified_at_ns(&metadata),
        fingerprint,
        expected_event_source: expected_event_source.map(ToOwned::to_owned),
        binding_nonce: binding_nonce.clone(),
        references_lifecycle_wrapper: lower.contains("soma-adapter-lifecycle.sh")
            || lower.contains("adapter-lifecycle"),
        references_spool_append: lower.contains("soma-adapter-spool-append.sh")
            || lower.contains("adapter-spool-append"),
        references_spool_drain: lower.contains("soma-adapter-spool-watch.sh")
            || lower.contains("adapter-spool"),
        references_review_render: lower.contains("soma-review-render.sh")
            || lower.contains("review-render"),
        references_client: lower.contains(&client.to_ascii_lowercase()),
        references_event_jsonl_env: lower.contains("soma_adapter_lifecycle_jsonl")
            || lower.contains("soma_adapter_spool_jsonl")
            || lower.contains("adapter-spool.jsonl")
            || lower.contains("adapter/events.jsonl"),
        references_private_event_source: expected_event_source
            .map(|source| lower.contains(&source.to_ascii_lowercase()))
            .unwrap_or(false),
        references_binding_nonce: binding_nonce.is_some(),
    })
}

fn scan_render_evidence(path: &Path) -> Result<RenderEvidenceScan, AdapterBindingProofError> {
    let metadata = fs::metadata(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read render evidence `{}`: {e}", path.display()))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(AdapterBindingProofError::MalformedInput(format!(
            "render evidence `{}` must be a non-empty file",
            path.display()
        )));
    }
    let bytes = fs::read(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read render evidence `{}`: {e}", path.display()))
    })?;
    let byte_len = bytes.len() as u64;
    let fingerprint = stable_content_fingerprint(&bytes);
    let raw = String::from_utf8(bytes).map_err(|e| {
        AdapterBindingProofError::MalformedInput(format!(
            "render evidence `{}` must be utf-8 structured JSON: {e}",
            path.display()
        ))
    })?;
    let (parsed_value, json_parse_error) = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => (Some(value), None),
        Err(err) => (None, Some(err.to_string())),
    };
    let value = parsed_value.as_ref();
    let rendered_surfaces =
        value.and_then(|value| value.get("rendered_surfaces")).and_then(Value::as_array);
    let rendered_surface_count = rendered_surfaces
        .map(Vec::len)
        .or_else(|| {
            value
                .and_then(|value| value.get("rendered_surface_count"))
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
        })
        .unwrap_or(0);
    let rendered_surface_placeholder_count = rendered_surfaces
        .map(|surfaces| {
            surfaces.iter().filter(|surface| value_contains_placeholder(surface)).count()
        })
        .unwrap_or(0);
    let raw_tool_output_surface_count = rendered_surfaces
        .map(|surfaces| {
            surfaces.iter().filter(|surface| surface_is_raw_tool_output(surface)).count()
        })
        .unwrap_or(0);
    let visible_surface_count = rendered_surfaces
        .map(|surfaces| surfaces.iter().filter(|surface| surface_is_visible(surface)).count())
        .unwrap_or(0);
    let rendered_surface_names = rendered_surface_names(rendered_surfaces);
    let action_surface_rendered_control_ids =
        rendered_surface_control_ids(rendered_surfaces, "action_buttons");
    let mut rendered_control_ids = value
        .and_then(|value| value.get("rendered_control_ids"))
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default();
    rendered_control_ids.sort();
    rendered_control_ids.dedup();
    Ok(RenderEvidenceScan {
        path: path.to_string_lossy().into_owned(),
        byte_len,
        fingerprint,
        json_parse_error,
        schema: value
            .and_then(|value| value.get("schema"))
            .and_then(Value::as_str)
            .map(str::to_string),
        client: value
            .and_then(|value| value.get("client"))
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase()),
        source: value
            .and_then(|value| value.get("source"))
            .and_then(Value::as_str)
            .map(str::to_string),
        observed_at_ns: value.and_then(|value| value.get("observed_at_ns")).and_then(Value::as_i64),
        review_render_fingerprint: value
            .and_then(|value| value.get("review_render_fingerprint"))
            .and_then(Value::as_str)
            .map(str::to_string),
        review_workbench_version: value
            .and_then(|value| value.get("review_workbench_version"))
            .and_then(Value::as_str)
            .map(str::to_string),
        review_interaction_contract_version: value
            .and_then(|value| value.get("review_interaction_contract_version"))
            .and_then(Value::as_str)
            .map(str::to_string),
        rendered_surface_count,
        rendered_surface_placeholder_count,
        raw_tool_output_surface_count,
        visible_surface_count,
        rendered_surface_names,
        expected_surface_names: Vec::new(),
        missing_surface_names: Vec::new(),
        rendered_control_ids,
        action_surface_rendered_control_ids,
        missing_action_surface_control_ids: Vec::new(),
        expected_control_ids: Vec::new(),
        missing_control_ids: Vec::new(),
        trust_boundary: value
            .and_then(|value| value.get("trust_boundary"))
            .and_then(Value::as_str)
            .map(str::to_string),
        valid_structured_render_evidence: false,
        missing_requirements: Vec::new(),
    })
}

fn scan_review_action_report(
    path: &Path,
) -> Result<ReviewActionReportScan, AdapterBindingProofError> {
    let metadata = fs::metadata(path).map_err(|e| {
        AdapterBindingProofError::Io(format!(
            "metadata review action report `{}`: {e}",
            path.display()
        ))
    })?;
    let bytes = fs::read(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read review action report `{}`: {e}", path.display()))
    })?;
    let byte_len = bytes.len() as u64;
    let fingerprint = stable_content_fingerprint(&bytes);
    let parsed = serde_json::from_slice::<Value>(&bytes);
    let (value, json_parse_error) = match parsed {
        Ok(value) => (Some(value), None),
        Err(err) => (None, Some(err.to_string())),
    };
    let mut scan = ReviewActionReportScan {
        path: path.to_string_lossy().into_owned(),
        byte_len,
        modified_at_ns: modified_at_ns(&metadata),
        fingerprint,
        json_parse_error,
        target_type: value
            .as_ref()
            .and_then(|value| value.get("target_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        target_id: value.as_ref().and_then(|value| value.get("target_id")).and_then(Value::as_i64),
        action: value
            .as_ref()
            .and_then(|value| value.get("action"))
            .and_then(Value::as_str)
            .map(str::to_string),
        control_id: value
            .as_ref()
            .and_then(|value| value.get("control_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        control_binding_verified: value
            .as_ref()
            .and_then(|value| value.get("control_binding_verified"))
            .and_then(Value::as_bool)
            == Some(true),
        verification_result: value
            .as_ref()
            .and_then(|value| value.get("verification_result"))
            .and_then(Value::as_str)
            .map(str::to_string),
        verification_event_count: value
            .as_ref()
            .and_then(|value| value.get("verification_event_ids"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        non_cloud_verification_event_count: value
            .as_ref()
            .and_then(|value| value.get("verification_events"))
            .and_then(Value::as_array)
            .map(|events| {
                events.iter().filter(|event| verification_event_is_non_cloud(event)).count()
            })
            .unwrap_or(0),
        claim_count: value
            .as_ref()
            .and_then(|value| value.get("claim_ids"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        task_frame_outcome_count: value
            .as_ref()
            .and_then(|value| value.get("task_frame_outcome_ids"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        durable_promotion_trust: value
            .as_ref()
            .and_then(|value| value.get("durable_promotion_trust"))
            .and_then(Value::as_bool),
        trust_boundary: value
            .as_ref()
            .and_then(|value| value.get("trust_boundary"))
            .and_then(Value::as_str)
            .map(str::to_string),
        valid_storage_gated_review_action: false,
        missing_requirements: Vec::new(),
    };
    let missing = review_action_report_missing_requirements(&scan);
    scan.valid_storage_gated_review_action = missing.is_empty();
    scan.missing_requirements = missing.iter().map(|value| (*value).to_string()).collect();
    Ok(scan)
}

fn verification_event_is_non_cloud(event: &Value) -> bool {
    let verifier_type = event.get("verifier_type").and_then(Value::as_str).unwrap_or("");
    let evidence_kind = event.pointer("/evidence_ref/kind").and_then(Value::as_str).unwrap_or("");
    !matches!(verifier_type, "cloud_model" | "cloud_draft" | "llm")
        && !matches!(evidence_kind, "cloud_output" | "cloud_draft" | "llm_output")
}

fn review_action_report_missing_requirements(scan: &ReviewActionReportScan) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if scan.json_parse_error.is_some() {
        missing.push("review_action_report_must_be_json");
    }
    if scan.target_type.as_deref().is_none_or(str::is_empty) {
        missing.push("target_type_required");
    }
    if scan.target_id.is_none_or(|value| value <= 0) {
        missing.push("target_id_must_be_positive");
    }
    if scan.action.as_deref().is_none_or(str::is_empty) {
        missing.push("action_required");
    }
    if scan.control_id.as_deref().is_none_or(str::is_empty) {
        missing.push("control_id_required");
    }
    if !scan.control_binding_verified {
        missing.push("control_binding_verified_required");
    }
    if scan.verification_result.is_none() {
        missing.push("verification_result_required");
    }
    if scan.verification_event_count == 0 {
        missing.push("verification_event_ids_required");
    }
    if scan.non_cloud_verification_event_count == 0 {
        missing.push("non_cloud_verification_event_required");
    }
    if scan.claim_count == 0 {
        missing.push("claim_ids_required");
    }
    if scan.trust_boundary.as_deref() != Some(REVIEW_ACTION_TRUST_BOUNDARY) {
        missing.push("review_action_storage_gate_trust_boundary_required");
    }
    missing
}

fn annotate_render_evidence_scan(
    scan: &mut RenderEvidenceScan,
    client: &str,
    expected_review_render_fingerprint: Option<&str>,
    expected_workbench_version: Option<&str>,
    expected_interaction_contract_version: Option<&str>,
    expected_surface_names: &[String],
    expected_control_ids: &[String],
) -> Vec<&'static str> {
    scan.expected_surface_names = expected_surface_names.to_vec();
    scan.missing_surface_names = expected_surface_names
        .iter()
        .filter(|surface_name| !scan.rendered_surface_names.contains(surface_name))
        .cloned()
        .collect();
    scan.expected_control_ids = expected_control_ids.to_vec();
    scan.missing_control_ids = expected_control_ids
        .iter()
        .filter(|control_id| !scan.rendered_control_ids.contains(control_id))
        .cloned()
        .collect();
    scan.missing_action_surface_control_ids = expected_control_ids
        .iter()
        .filter(|control_id| !scan.action_surface_rendered_control_ids.contains(control_id))
        .cloned()
        .collect();
    let missing = render_evidence_missing_requirements(
        scan,
        client,
        expected_review_render_fingerprint,
        expected_workbench_version,
        expected_interaction_contract_version,
        expected_surface_names,
        expected_control_ids,
    );
    scan.valid_structured_render_evidence = missing.is_empty();
    scan.missing_requirements = missing.iter().map(|value| (*value).to_string()).collect();
    missing
}

fn render_evidence_missing_requirements(
    scan: &RenderEvidenceScan,
    client: &str,
    expected_review_render_fingerprint: Option<&str>,
    expected_workbench_version: Option<&str>,
    expected_interaction_contract_version: Option<&str>,
    expected_surface_names: &[String],
    expected_control_ids: &[String],
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if scan.json_parse_error.is_some() {
        missing.push("render_evidence_must_be_json");
    }
    if scan.schema.as_deref() != Some(IN_CLIENT_RENDER_EVIDENCE_SCHEMA) {
        missing.push("schema_must_be_soma_in_client_render_evidence_v1");
    }
    if scan.client.as_deref() != Some(client) {
        missing.push("client_must_match_binding_target");
    }
    if !scan.source.as_deref().is_some_and(is_allowed_render_evidence_source) {
        missing.push("source_must_be_manual_operator_or_client_capture");
    }
    if scan.observed_at_ns.is_none_or(|value| value <= 0) {
        missing.push("observed_at_ns_must_be_positive");
    }
    match expected_review_render_fingerprint {
        Some(expected) if scan.review_render_fingerprint.as_deref() == Some(expected) => {}
        Some(_) => missing.push("review_render_fingerprint_must_match_report"),
        None => missing.push("review_render_report_fingerprint_required"),
    }
    match expected_workbench_version {
        Some(expected) if scan.review_workbench_version.as_deref() == Some(expected) => {}
        Some(_) => missing.push("review_workbench_version_must_match_report"),
        None => missing.push("review_render_workbench_version_required"),
    }
    match expected_interaction_contract_version {
        Some(expected) if scan.review_interaction_contract_version.as_deref() == Some(expected) => {
        }
        Some(_) => missing.push("review_interaction_contract_version_must_match_report"),
        None => missing.push("review_render_interaction_contract_version_required"),
    }
    if scan.rendered_surface_count == 0 {
        missing.push("rendered_surfaces_must_be_non_empty");
    }
    if scan.rendered_surface_placeholder_count > 0 {
        missing.push("rendered_surfaces_must_not_contain_template_placeholders");
    }
    if scan.raw_tool_output_surface_count > 0 {
        missing.push("rendered_surfaces_must_not_be_raw_mcp_or_tool_output");
    }
    if scan.visible_surface_count == 0 {
        missing.push("rendered_surfaces_must_include_visible_surface");
    }
    if !expected_surface_names.is_empty() && scan.rendered_surface_names.is_empty() {
        missing.push("rendered_surfaces_must_name_visible_review_surfaces");
    }
    if !scan.missing_surface_names.is_empty() {
        missing.push("rendered_surfaces_must_include_expected_review_surfaces");
    }
    if !expected_control_ids.is_empty() && scan.rendered_control_ids.is_empty() {
        missing.push("rendered_control_ids_must_be_non_empty_when_report_has_actions");
    }
    if !scan.missing_control_ids.is_empty() {
        missing.push("rendered_control_ids_must_cover_report_actions");
    }
    if !expected_control_ids.is_empty() && scan.action_surface_rendered_control_ids.is_empty() {
        missing.push("action_buttons_surface_must_echo_rendered_control_ids");
    }
    if !scan.missing_action_surface_control_ids.is_empty() {
        missing.push("action_buttons_surface_control_ids_must_cover_report_actions");
    }
    if scan.trust_boundary.as_deref() != Some(IN_CLIENT_RENDER_EVIDENCE_TRUST_BOUNDARY) {
        missing.push("trust_boundary_must_keep_render_ui_only");
    }
    missing
}

fn is_allowed_render_evidence_source(source: &str) -> bool {
    matches!(
        source.trim(),
        "manual_operator"
            | "client_capture"
            | "client_ui_capture"
            | "client_dom_capture"
            | "screenshot_ocr"
    )
}

fn value_contains_placeholder(value: &Value) -> bool {
    match value {
        Value::String(raw) => {
            let trimmed = raw.trim();
            trimmed.len() >= 2 && trimmed.starts_with('<') && trimmed.ends_with('>')
        }
        Value::Array(values) => values.iter().any(value_contains_placeholder),
        Value::Object(map) => map.values().any(value_contains_placeholder),
        _ => false,
    }
}

fn surface_is_raw_tool_output(surface: &Value) -> bool {
    ["kind", "title", "name", "source", "render_as"].iter().any(|field| {
        surface.get(*field).and_then(Value::as_str).is_some_and(raw_tool_output_marker)
    })
}

fn raw_tool_output_marker(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    normalized.contains("mcp_tool")
        || normalized.contains("tool_output")
        || normalized.contains("review_render_output")
        || normalized.contains("client_binding_status")
        || normalized.contains("json_dump")
}

fn rendered_surface_names(surfaces: Option<&Vec<Value>>) -> Vec<String> {
    let mut names = surfaces
        .into_iter()
        .flatten()
        .filter_map(|surface| surface.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn rendered_surface_control_ids(surfaces: Option<&Vec<Value>>, surface_name: &str) -> Vec<String> {
    let mut ids = surfaces
        .into_iter()
        .flatten()
        .filter(|surface| {
            surface.get("name").and_then(Value::as_str).is_some_and(|name| name == surface_name)
        })
        .flat_map(|surface| {
            surface
                .get("rendered_control_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn surface_is_visible(surface: &Value) -> bool {
    surface.get("visible").and_then(Value::as_bool) == Some(true)
}

fn extract_binding_nonce_from_text(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| extract_binding_nonce_from_json(&value))
        .or_else(|| extract_binding_nonce_from_lines(raw))
}

fn extract_binding_nonce_from_json(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let normalized = key.trim().to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "soma_adapter_lifecycle_binding_nonce"
                        | "soma_adapter_binding_nonce"
                        | "binding_nonce"
                ) {
                    if let Some(nonce) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                        return Some(nonce.to_string());
                    }
                }
            }
            map.values().find_map(extract_binding_nonce_from_json)
        }
        Value::Array(values) => values.iter().find_map(extract_binding_nonce_from_json),
        _ => None,
    }
}

fn extract_binding_nonce_from_lines(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("soma_adapter_lifecycle_binding_nonce")
            || lower.contains("soma_adapter_binding_nonce")
            || lower.contains("binding_nonce")
        {
            let value = line
                .split(['=', ':'])
                .nth(1)
                .unwrap_or("")
                .trim()
                .trim_matches(',')
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn require_installed_config_for_level(
    proof_level: ClientBindingProofLevel,
    installed_config_scan: Option<&InstalledConfigScan>,
) -> Result<(), AdapterBindingProofError> {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding | ClientBindingProofLevel::ObservedEventFile => {
            Ok(())
        }
        ClientBindingProofLevel::ObservedAppHook => {
            let Some(scan) = installed_config_scan else {
                return Err(AdapterBindingProofError::MalformedInput(
                    "observed_app_hook requires --installed-config".to_string(),
                ));
            };
            let missing = observed_app_hook_missing_requirements(scan);
            if missing.contains(&"lifecycle_or_spool_append_wrapper_reference") {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference soma-adapter-lifecycle.sh, adapter-lifecycle, soma-adapter-spool-append.sh, or adapter-spool-append".to_string(),
                ));
            }
            if missing.contains(&"target_client_reference") {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference the target client name".to_string(),
                ));
            }
            if missing.contains(&"adapter_jsonl_spool_reference") {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference the adapter JSONL spool path or env var"
                        .to_string(),
                ));
            }
            if missing.contains(&"private_event_source_reference") {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference the expected private event source".to_string(),
                ));
            }
            if missing.contains(&"binding_nonce_reference") {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must include SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE or binding_nonce".to_string(),
                ));
            }
            Ok(())
        }
        ClientBindingProofLevel::ObservedInClientRender
        | ClientBindingProofLevel::ObservedReviewAction => {
            let Some(scan) = installed_config_scan else {
                return Err(AdapterBindingProofError::MalformedInput(format!(
                    "{} requires --installed-config",
                    proof_level.as_str()
                )));
            };
            if !scan.references_review_render {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference soma-review-render.sh or review-render"
                        .to_string(),
                ));
            }
            if !scan.references_client {
                return Err(AdapterBindingProofError::MalformedInput(
                    "installed config must reference the target client name".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn require_render_evidence_for_level(
    proof_level: ClientBindingProofLevel,
    render_evidence_scan: Option<&mut RenderEvidenceScan>,
    client: &str,
    review_render_file_scan: Option<&FileFingerprintScan>,
    review_render: Option<&Value>,
) -> Result<(), AdapterBindingProofError> {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding
        | ClientBindingProofLevel::ObservedEventFile
        | ClientBindingProofLevel::ObservedAppHook
        | ClientBindingProofLevel::ObservedReviewAction => Ok(()),
        ClientBindingProofLevel::ObservedInClientRender => {
            let Some(scan) = render_evidence_scan else {
                return Err(AdapterBindingProofError::MalformedInput(
                    "observed_in_client_render requires --render-evidence".to_string(),
                ));
            };
            let expected_review_render_fingerprint =
                review_render_file_scan.map(|scan| scan.fingerprint.as_str());
            let expected_control_ids =
                review_render.map(review_render_control_ids).unwrap_or_default();
            let expected_surface_names = review_render
                .map(|report| review_render_required_surface_names(report, &expected_control_ids))
                .unwrap_or_default();
            let missing = annotate_render_evidence_scan(
                scan,
                client,
                expected_review_render_fingerprint,
                review_render.and_then(review_render_workbench_version),
                review_render.and_then(review_render_interaction_contract_version),
                &expected_surface_names,
                &expected_control_ids,
            );
            if !missing.is_empty() {
                return Err(AdapterBindingProofError::MalformedInput(format!(
                    "observed_in_client_render requires structured render evidence; missing: {}",
                    missing.join(", ")
                )));
            }
            Ok(())
        }
    }
}

fn require_review_action_report_for_level(
    proof_level: ClientBindingProofLevel,
    review_action_report_scan: Option<&ReviewActionReportScan>,
) -> Result<(), AdapterBindingProofError> {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding
        | ClientBindingProofLevel::ObservedEventFile
        | ClientBindingProofLevel::ObservedAppHook
        | ClientBindingProofLevel::ObservedInClientRender => Ok(()),
        ClientBindingProofLevel::ObservedReviewAction => {
            let Some(scan) = review_action_report_scan else {
                return Err(AdapterBindingProofError::MalformedInput(
                    "observed_review_action requires --review-action-report".to_string(),
                ));
            };
            if !scan.missing_requirements.is_empty() {
                return Err(AdapterBindingProofError::MalformedInput(format!(
                    "observed_review_action requires storage-gated review action report; missing: {}",
                    scan.missing_requirements.join(", ")
                )));
            }
            Ok(())
        }
    }
}

fn link_review_action_to_render_proof(
    store: &Storage,
    client: &str,
    installed_config_scan: Option<&InstalledConfigScan>,
    review_action_report_scan: Option<&ReviewActionReportScan>,
) -> Result<Value, AdapterBindingProofError> {
    let scan = review_action_report_scan.ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "observed_review_action requires --review-action-report".to_string(),
        )
    })?;
    let control_id = scan.control_id.as_deref().ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "observed_review_action requires review action control_id".to_string(),
        )
    })?;
    let installed_config = installed_config_scan.ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "observed_review_action requires --installed-config".to_string(),
        )
    })?;
    let proofs = store.recent_client_binding_proofs(Some(client), 200)?;
    let render = proofs
        .iter()
        .find(|proof| proof.proof_level == ClientBindingProofLevel::ObservedInClientRender)
        .ok_or_else(|| {
            AdapterBindingProofError::MalformedInput(
                "observed_review_action requires an existing observed_in_client_render proof"
                    .to_string(),
            )
        })?;
    let rendered_control_ids =
        string_vec_pointer(&render.checks_json, "/render_evidence_scan/rendered_control_ids");
    let action_surface_rendered_control_ids = string_vec_pointer(
        &render.checks_json,
        "/render_evidence_scan/action_surface_rendered_control_ids",
    );
    let render_installed_config_fingerprint =
        string_pointer(&render.checks_json, "/installed_config_scan/fingerprint")
            .map(str::to_string);
    let render_installed_config_binding_nonce =
        string_pointer(&render.checks_json, "/installed_config_scan/binding_nonce")
            .map(str::to_string);
    let control_id_in_rendered_control_ids =
        rendered_control_ids.iter().any(|rendered| rendered == control_id);
    let control_id_in_action_surface_rendered_control_ids =
        action_surface_rendered_control_ids.iter().any(|rendered| rendered == control_id);
    let installed_config_fingerprint_matches = render_installed_config_fingerprint.as_deref()
        == Some(installed_config.fingerprint.as_str());
    let installed_config_binding_nonce_matches = render_installed_config_binding_nonce.as_deref()
        == installed_config.binding_nonce.as_deref();
    if !control_id_in_rendered_control_ids || !control_id_in_action_surface_rendered_control_ids {
        return Err(AdapterBindingProofError::MalformedInput(format!(
            "observed_review_action control_id `{control_id}` was not present in the latest observed_in_client_render proof"
        )));
    }
    if !installed_config_fingerprint_matches || !installed_config_binding_nonce_matches {
        return Err(AdapterBindingProofError::MalformedInput(
            "observed_review_action requires the linked render proof to use the same installed config fingerprint and binding_nonce"
                .to_string(),
        ));
    }
    Ok(json!({
        "proof_id": render.id,
        "proof_level": render.proof_level.as_str(),
        "observed_at_ns": render.observed_at_ns,
        "control_id": control_id,
        "rendered_control_ids": rendered_control_ids,
        "action_surface_rendered_control_ids": action_surface_rendered_control_ids,
        "control_id_in_rendered_control_ids": control_id_in_rendered_control_ids,
        "control_id_in_action_surface_rendered_control_ids": control_id_in_action_surface_rendered_control_ids,
        "installed_config_fingerprint": render_installed_config_fingerprint,
        "installed_config_binding_nonce": render_installed_config_binding_nonce,
        "installed_config_fingerprint_matches": installed_config_fingerprint_matches,
        "installed_config_binding_nonce_matches": installed_config_binding_nonce_matches,
    }))
}

fn observed_app_hook_missing_requirements(scan: &InstalledConfigScan) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !(scan.references_lifecycle_wrapper || scan.references_spool_append) {
        missing.push("lifecycle_or_spool_append_wrapper_reference");
    }
    if !scan.references_client {
        missing.push("target_client_reference");
    }
    if !scan.references_event_jsonl_env {
        missing.push("adapter_jsonl_spool_reference");
    }
    if !scan.references_private_event_source {
        missing.push("private_event_source_reference");
    }
    if !scan.references_binding_nonce {
        missing.push("binding_nonce_reference");
    }
    missing
}

fn resolve_client_for_check(
    args: &AdapterBindingProofArgs,
) -> Result<String, AdapterBindingProofError> {
    if let Some(client) = args.client.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(client.to_ascii_lowercase());
    }
    let manifest_path = args.manifest.as_deref().ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "--check-installed-config requires --client or --manifest".to_string(),
        )
    })?;
    let manifest = read_json_file(&canonical_or_raw(manifest_path), "manifest")?;
    validate_manifest_shape(&manifest)?;
    Ok(required_str(&manifest, "client")?.trim().to_ascii_lowercase())
}

fn resolve_expected_event_source_for_check(
    args: &AdapterBindingProofArgs,
    client: &str,
) -> Result<String, AdapterBindingProofError> {
    if let Some(manifest_path) = args.manifest.as_deref() {
        let manifest = read_json_file(&canonical_or_raw(manifest_path), "manifest")?;
        validate_manifest_shape(&manifest)?;
        if let Some(source) = expected_event_source_from_manifest(&manifest, client) {
            return Ok(source);
        }
    }
    Ok(default_private_event_source(client))
}

fn expected_event_source_from_manifest(manifest: &Value, client: &str) -> Option<String> {
    manifest
        .pointer("/lifecycle/event_source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| Some(default_private_event_source(client)))
}

fn default_private_event_source(client: &str) -> String {
    format!("{}_private_lifecycle_hook", client.trim().to_ascii_lowercase())
}

fn default_lifecycle_event(client: &str) -> &'static str {
    match client {
        "continue" | "codex-app" => "assistant_response",
        _ => "turn_completed",
    }
}

fn resolve_binding_nonce_for_prepare(
    args: &AdapterBindingProofArgs,
) -> Result<(String, bool), AdapterBindingProofError> {
    if let Some(value) = args.binding_nonce.as_deref() {
        let nonce = normalize_binding_nonce(value)?;
        return Ok((nonce, false));
    }
    Ok((format!("soma-bind-{}", Uuid::new_v4()), true))
}

fn normalize_binding_nonce(value: &str) -> Result<String, AdapterBindingProofError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AdapterBindingProofError::MalformedInput(
            "--binding-nonce must not be empty".to_string(),
        ));
    }
    if trimmed.len() > 256 {
        return Err(AdapterBindingProofError::MalformedInput(
            "--binding-nonce must be at most 256 bytes".to_string(),
        ));
    }
    if trimmed.chars().any(char::is_whitespace) || trimmed.chars().any(char::is_control) {
        return Err(AdapterBindingProofError::MalformedInput(
            "--binding-nonce must be a single opaque token without whitespace".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_manifest_shape(manifest: &Value) -> Result<(), AdapterBindingProofError> {
    if !manifest.is_object() {
        return Err(AdapterBindingProofError::MalformedInput(
            "manifest must be a JSON object".to_string(),
        ));
    }
    if required_str(manifest, "schema")? != "soma.client_binding.v1" {
        return Err(AdapterBindingProofError::MalformedInput(
            "manifest schema must be soma.client_binding.v1".to_string(),
        ));
    }
    for pointer in [
        "/lifecycle/wrapper",
        "/spool_drain/wrapper",
        "/review_ui/wrapper",
        "/lifecycle/sample_event",
    ] {
        if manifest.pointer(pointer).is_none() {
            return Err(AdapterBindingProofError::MalformedInput(format!(
                "manifest missing `{pointer}`"
            )));
        }
    }
    Ok(())
}

fn scan_event_jsonl(
    path: &Path,
    client: &str,
    expected_event_source: Option<&str>,
    expected_binding_nonce: Option<&str>,
) -> Result<EventScan, AdapterBindingProofError> {
    let metadata = fs::metadata(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("metadata event JSONL `{}`: {e}", path.display()))
    })?;
    let bytes = fs::read(path)
        .map_err(|e| AdapterBindingProofError::Io(format!("read `{}`: {e}", path.display())))?;
    let byte_len = bytes.len() as u64;
    let fingerprint = stable_content_fingerprint(&bytes);
    let raw = String::from_utf8(bytes).map_err(|e| {
        AdapterBindingProofError::MalformedInput(format!(
            "event JSONL `{}` must be utf-8: {e}",
            path.display()
        ))
    })?;
    let mut scan = EventScan {
        path: path.to_string_lossy().into_owned(),
        byte_len,
        modified_at_ns: modified_at_ns(&metadata),
        fingerprint,
        expected_event_source: expected_event_source.map(ToOwned::to_owned),
        expected_binding_nonce: expected_binding_nonce.map(ToOwned::to_owned),
        scanned_lines: 0,
        matching_events: 0,
        matching_turns: 0,
        matching_cloud_outputs: 0,
        matching_private_event_sources: 0,
        matching_private_non_release_manual_events: 0,
        matching_private_non_release_test_events: 0,
        matching_writer_contract_events: 0,
        matching_private_writer_contract_events: 0,
        matching_private_binding_nonces: 0,
        matching_private_non_release_manual_binding_nonces: 0,
        matching_private_non_release_test_binding_nonces: 0,
        matching_events_with_observed_at: 0,
        matching_private_events_with_observed_at: 0,
        max_matching_observed_at_ns: None,
        max_matching_private_observed_at_ns: None,
    };
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        scan.scanned_lines += 1;
        let event: Value = serde_json::from_str(trimmed).map_err(|e| {
            AdapterBindingProofError::MalformedInput(format!(
                "event JSONL line {} parse: {e}",
                idx + 1
            ))
        })?;
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("").replace('-', "_");
        let payload = event.get("payload").unwrap_or(&Value::Null);
        let payload_client = payload
            .get("client")
            .or_else(|| payload.get("source"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if payload_client == client {
            scan.matching_events += 1;
            let writer_contract_matches = event
                .get("writer_contract")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "soma_adapter_spool_append_v1");
            if writer_contract_matches {
                scan.matching_writer_contract_events += 1;
            }
            let observed_at_ns = event.get("observed_at_ns").and_then(Value::as_i64);
            if let Some(value) = observed_at_ns {
                scan.matching_events_with_observed_at += 1;
                scan.max_matching_observed_at_ns =
                    Some(scan.max_matching_observed_at_ns.map_or(value, |max| max.max(value)));
            }
            let private_source_matches = expected_event_source
                .map(|source| {
                    payload
                        .get("event_source")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .is_some_and(|value| value.eq_ignore_ascii_case(source))
                })
                .unwrap_or(false);
            if private_source_matches {
                let private_binding_nonce_matches = expected_binding_nonce
                    .map(|expected| {
                        event_binding_nonce(&event, payload)
                            .is_some_and(|actual| actual == expected)
                    })
                    .unwrap_or(false);
                if is_non_release_manual_event(&event, payload) {
                    scan.matching_private_non_release_manual_events += 1;
                    if private_binding_nonce_matches {
                        scan.matching_private_non_release_manual_binding_nonces += 1;
                    }
                } else if is_non_release_test_event(&event, payload) {
                    scan.matching_private_non_release_test_events += 1;
                    if private_binding_nonce_matches {
                        scan.matching_private_non_release_test_binding_nonces += 1;
                    }
                } else {
                    scan.matching_private_event_sources += 1;
                    if writer_contract_matches {
                        scan.matching_private_writer_contract_events += 1;
                    }
                    if private_binding_nonce_matches {
                        scan.matching_private_binding_nonces += 1;
                    }
                    if let Some(value) = observed_at_ns {
                        scan.matching_private_events_with_observed_at += 1;
                        scan.max_matching_private_observed_at_ns = Some(
                            scan.max_matching_private_observed_at_ns
                                .map_or(value, |max| max.max(value)),
                        );
                    }
                }
            }
            match kind.as_str() {
                "turn" | "capture_turn" | "adapter_capture" => scan.matching_turns += 1,
                "cloud_output" | "adapter_cloud_output" => scan.matching_cloud_outputs += 1,
                _ => {}
            }
        }
    }
    Ok(scan)
}

fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("fnv1a64:{hash:016x}")
}

fn event_binding_nonce<'a>(event: &'a Value, payload: &'a Value) -> Option<&'a str> {
    payload
        .get("binding_nonce")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            event
                .get("binding_nonce")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn event_hook_adapter<'a>(event: &'a Value, payload: &'a Value) -> Option<&'a str> {
    event_payload_string(event, payload, "hook_adapter")
}

fn event_manual_invocation_policy<'a>(event: &'a Value, payload: &'a Value) -> Option<&'a str> {
    event_payload_string(event, payload, "manual_invocation_policy")
}

fn event_payload_bool(event: &Value, payload: &Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(Value::as_bool).or_else(|| event.get(key).and_then(Value::as_bool))
}

fn event_payload_string<'a>(event: &'a Value, payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            event.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
        })
}

fn is_non_release_manual_event(event: &Value, payload: &Value) -> bool {
    is_non_release_manual_marker(event_hook_adapter(event, payload))
        || is_non_release_manual_marker(event_manual_invocation_policy(event, payload))
}

fn is_non_release_test_event(event: &Value, payload: &Value) -> bool {
    if event_payload_bool(event, payload, "collector_release_grade_candidate") == Some(false) {
        return true;
    }
    [
        "continue_profile_id",
        "session_id",
        "thread_id",
        "model_provider",
        "model_name",
        "model_title",
        "prompt_text",
        "response_text",
        "output_text",
    ]
    .iter()
    .any(|key| {
        event_payload_string(event, payload, key).is_some_and(contains_non_release_test_marker)
    })
}

fn is_non_release_manual_marker(value: Option<&str>) -> bool {
    value.map(|value| value.trim().to_ascii_lowercase()).is_some_and(|value| {
        value.contains("manual_debug")
            || value.contains("manual-template")
            || value.contains("manual_template")
            || value.contains("non_release")
            || value.contains("non-release")
    })
}

fn contains_non_release_test_marker(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.contains("dogfood")
        || value.contains("local-dogfood")
        || value.contains("soma-test")
        || value.contains("soma_continue_collector_ok")
}

fn require_event_scan_for_level(
    proof_level: ClientBindingProofLevel,
    event_scan: Option<&EventScan>,
    expected_event_source: Option<&str>,
    expected_binding_nonce: Option<&str>,
) -> Result<(), AdapterBindingProofError> {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding
        | ClientBindingProofLevel::ObservedInClientRender
        | ClientBindingProofLevel::ObservedReviewAction => Ok(()),
        ClientBindingProofLevel::ObservedEventFile | ClientBindingProofLevel::ObservedAppHook => {
            let Some(scan) = event_scan else {
                return Err(AdapterBindingProofError::MalformedInput(format!(
                    "{proof_level} requires --event-jsonl"
                )));
            };
            if scan.matching_events == 0 {
                return Err(AdapterBindingProofError::MalformedInput(format!(
                    "{proof_level} requires at least one event matching the client"
                )));
            }
            if matches!(proof_level, ClientBindingProofLevel::ObservedAppHook) {
                let source = expected_event_source.ok_or_else(|| {
                    AdapterBindingProofError::MalformedInput(
                        "observed_app_hook requires manifest lifecycle.event_source".to_string(),
                    )
                })?;
                if scan.matching_private_event_sources == 0 {
                    if scan.matching_private_non_release_manual_events > 0 {
                        return Err(AdapterBindingProofError::MalformedInput(
                            "observed_app_hook ignores manual_debug/non_release hook_adapter events; trigger the real private app hook before recording release proof".to_string(),
                        ));
                    }
                    if scan.matching_private_non_release_test_events > 0 {
                        return Err(AdapterBindingProofError::MalformedInput(
                            "observed_app_hook ignores dogfood/synthetic test events; trigger the real private app hook before recording release proof".to_string(),
                        ));
                    }
                    return Err(AdapterBindingProofError::MalformedInput(format!(
                        "observed_app_hook requires at least one event payload with event_source `{source}` matching the client"
                    )));
                }
                if scan.matching_private_writer_contract_events == 0 {
                    return Err(AdapterBindingProofError::MalformedInput(
                        "observed_app_hook requires at least one matching private event written by soma_adapter_spool_append_v1".to_string(),
                    ));
                }
                if scan.matching_private_events_with_observed_at == 0 {
                    return Err(AdapterBindingProofError::MalformedInput(
                        "observed_app_hook requires at least one matching private event with observed_at_ns".to_string(),
                    ));
                }
                let nonce = expected_binding_nonce.ok_or_else(|| {
                    AdapterBindingProofError::MalformedInput(
                        "observed_app_hook requires installed config binding nonce".to_string(),
                    )
                })?;
                if scan.matching_private_binding_nonces == 0 {
                    return Err(AdapterBindingProofError::MalformedInput(format!(
                        "observed_app_hook requires at least one matching private event with binding_nonce `{nonce}`"
                    )));
                }
            }
            Ok(())
        }
    }
}

fn require_observed_app_hook_temporal_binding(
    proof_level: ClientBindingProofLevel,
    event_scan: Option<&EventScan>,
    installed_config_scan: Option<&InstalledConfigScan>,
) -> Result<(), AdapterBindingProofError> {
    if !matches!(proof_level, ClientBindingProofLevel::ObservedAppHook) {
        return Ok(());
    }
    let (Some(event_scan), Some(installed_config_scan)) = (event_scan, installed_config_scan)
    else {
        return Ok(());
    };
    let config_modified_at = installed_config_scan.modified_at_ns.ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "observed_app_hook requires installed config modified_at timestamp".to_string(),
        )
    })?;
    let event_observed_at = event_scan.max_matching_private_observed_at_ns.ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(
            "observed_app_hook requires matching private event observed_at_ns".to_string(),
        )
    })?;
    if event_observed_at.saturating_add(OBSERVED_APP_HOOK_ALLOWED_CLOCK_SKEW_NS)
        < config_modified_at
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "observed_app_hook requires matching private event observed_at_ns at or after installed config modified_at".to_string(),
        ));
    }
    if let Some(event_modified_at) = event_scan.modified_at_ns {
        if event_modified_at.saturating_add(OBSERVED_APP_HOOK_ALLOWED_CLOCK_SKEW_NS)
            < config_modified_at
        {
            return Err(AdapterBindingProofError::MalformedInput(
                "observed_app_hook requires event JSONL file modified_at at or after installed config modified_at".to_string(),
            ));
        }
    }
    Ok(())
}

fn reject_future_observed_app_hook_events(
    proof_level: ClientBindingProofLevel,
    event_scan: Option<&EventScan>,
    proof_observed_at_ns: i64,
) -> Result<(), AdapterBindingProofError> {
    if !matches!(proof_level, ClientBindingProofLevel::ObservedAppHook) {
        return Ok(());
    }
    const ALLOWED_FUTURE_SKEW_NS: i64 = 5 * 60 * 1_000_000_000;
    if let Some(observed_at) = event_scan.and_then(|scan| scan.max_matching_private_observed_at_ns)
    {
        if observed_at > proof_observed_at_ns.saturating_add(ALLOWED_FUTURE_SKEW_NS) {
            return Err(AdapterBindingProofError::MalformedInput(
                "observed_app_hook matching private event observed_at_ns is too far in the future"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_drain_report_for_level(
    proof_level: ClientBindingProofLevel,
    drain_report: Option<&Value>,
) -> Result<(), AdapterBindingProofError> {
    let Some(report) = drain_report else {
        if matches!(proof_level, ClientBindingProofLevel::ObservedAppHook) {
            return Err(AdapterBindingProofError::MalformedInput(
                "observed_app_hook requires --drain-report".to_string(),
            ));
        }
        return Ok(());
    };
    let turns = report.get("captured_turns").and_then(Value::as_u64).unwrap_or(0);
    let clouds = report.get("captured_cloud_outputs").and_then(Value::as_u64).unwrap_or(0);
    if turns + clouds == 0 {
        return Err(AdapterBindingProofError::MalformedInput(
            "drain report must capture at least one turn or cloud output".to_string(),
        ));
    }
    Ok(())
}

fn require_review_render_for_level(
    proof_level: ClientBindingProofLevel,
    review_render: Option<&Value>,
) -> Result<(), AdapterBindingProofError> {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding
        | ClientBindingProofLevel::ObservedEventFile
        | ClientBindingProofLevel::ObservedAppHook
        | ClientBindingProofLevel::ObservedReviewAction => Ok(()),
        ClientBindingProofLevel::ObservedInClientRender => {
            if review_render.is_none() {
                return Err(AdapterBindingProofError::MalformedInput(
                    "observed_in_client_render requires --review-render-report".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn validate_review_render(
    review_render: Option<&Value>,
    client: &str,
) -> Result<(), AdapterBindingProofError> {
    let Some(report) = review_render else {
        return Ok(());
    };
    let boundary = report.get("trust_boundary").and_then(Value::as_str).unwrap_or("");
    if boundary
        != "review_render_is_read_only_and_never_records_verification_or_applies_proposals_or_ack"
    {
        return Err(AdapterBindingProofError::MalformedInput(
            "review render report must carry the read-only trust boundary".to_string(),
        ));
    }
    let report_client = report
        .get("client")
        .and_then(Value::as_str)
        .or_else(|| report.pointer("/client_ui/client").and_then(Value::as_str))
        .unwrap_or("");
    if report_client != client {
        return Err(AdapterBindingProofError::MalformedInput(format!(
            "review render client `{report_client}` does not match `{client}`"
        )));
    }
    Ok(())
}

fn review_render_workbench_version(review_render: &Value) -> Option<&str> {
    review_render.pointer("/workbench/version").and_then(Value::as_str)
}

fn review_render_interaction_contract_version(review_render: &Value) -> Option<&str> {
    review_render.pointer("/interaction_contract/version").and_then(Value::as_str)
}

fn review_render_control_ids(review_render: &Value) -> Vec<String> {
    let mut ids = review_render
        .pointer("/interaction_contract/actions")
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(|action| action.get("control_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

fn review_render_required_surface_names(
    review_render: &Value,
    expected_control_ids: &[String],
) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(primary) =
        review_render.get("primary_surface").and_then(Value::as_str).map(normalize_surface_name)
    {
        if !primary.is_empty() && primary != "none" {
            names.push(primary);
        }
    }
    if !expected_control_ids.is_empty() {
        names.push("action_buttons".to_string());
    }
    if names.is_empty() {
        names.extend(
            review_render
                .get("surfaces")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|surface| surface.get("display").and_then(Value::as_str) != Some("hidden"))
                .filter_map(|surface| surface.get("name").and_then(Value::as_str))
                .map(normalize_surface_name)
                .filter(|name| !name.is_empty() && name != "none"),
        );
    }
    names.sort();
    names.dedup();
    names
}

fn normalize_surface_name(name: &str) -> String {
    match name.trim() {
        "review_actions" => "action_buttons".to_string(),
        value => value.to_string(),
    }
}

fn trust_boundary_for_level(
    proof_level: ClientBindingProofLevel,
    manifest_trust_boundary: &str,
) -> String {
    match proof_level {
        ClientBindingProofLevel::ReferenceBinding => format!(
            "reference_binding_only: {manifest_trust_boundary}"
        ),
        ClientBindingProofLevel::ObservedEventFile => format!(
            "observed_event_file_only: {manifest_trust_boundary}; event files alone still do not prove a private app hook"
        ),
        ClientBindingProofLevel::ObservedAppHook => {
        "observed_app_hook: operator confirmed the event evidence came from the private app hook with matching private event_source, binding_nonce, writer metadata, and temporal binding; trust still stops at captured local/tool/user evidence and never promotes cloud drafts directly".to_string()
        }
        ClientBindingProofLevel::ObservedInClientRender => {
            "observed_in_client_render: operator confirmed the read-only review render plan was visible inside the client; this proves UI rendering only and never verifies, promotes, applies, or acknowledges review items".to_string()
        }
        ClientBindingProofLevel::ObservedReviewAction => {
            "observed_review_action: operator confirmed a soma_review_action report from a rendered client control_id that matched prior in-client render evidence; this proves the review UI action loop reached storage verification gates but still does not make cloud drafts durable without those gates".to_string()
        }
    }
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, AdapterBindingProofError> {
    value.get(key).and_then(Value::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| {
        AdapterBindingProofError::MalformedInput(format!("manifest field `{key}` must be a string"))
    })
}

fn string_pointer<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
}

fn string_vec_pointer(value: &Value, pointer: &str) -> Vec<String> {
    let mut out = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn read_json_file(path: &Path, label: &str) -> Result<Value, AdapterBindingProofError> {
    let raw = fs::read_to_string(path).map_err(|e| {
        AdapterBindingProofError::Io(format!("read {label} `{}`: {e}", path.display()))
    })?;
    serde_json::from_str::<Value>(&raw)
        .map_err(|e| AdapterBindingProofError::MalformedInput(format!("{label} JSON parse: {e}")))
}

fn canonical_or_raw(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn modified_at_ns(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn render_evidence_rejects_raw_mcp_tool_output_as_release_render_proof() {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(
            file.path(),
            r#"{
  "schema": "soma.in_client_render_evidence.v1",
  "client": "codex-app",
  "source": "client_capture",
  "observed_at_ns": 12345,
  "review_render_fingerprint": "fnv1a64:test",
  "review_workbench_version": "soma.review_workbench.v1",
  "review_interaction_contract_version": "soma.review_interaction_contract.v1",
  "rendered_control_ids": ["proposal:4:wait"],
  "rendered_surfaces": [{
    "kind": "codex_app_mcp_tool_result",
    "name": "action_buttons",
    "title": "SOMA Review Render Plan visible as raw MCP tool output",
    "visible": true,
    "rendered_control_ids": ["proposal:4:wait"]
  }],
  "trust_boundary": "observed_in_client_render_is_ui_only_and_never_verifies_promotes_applies_or_acknowledges"
}"#,
        )
        .expect("write render evidence");

        let mut scan = scan_render_evidence(file.path()).expect("scan render evidence");
        let expected_surface_names = vec!["action_buttons".to_string()];
        let expected_control_ids = vec!["proposal:4:wait".to_string()];
        let missing = annotate_render_evidence_scan(
            &mut scan,
            "codex-app",
            Some("fnv1a64:test"),
            Some("soma.review_workbench.v1"),
            Some("soma.review_interaction_contract.v1"),
            &expected_surface_names,
            &expected_control_ids,
        );

        assert_eq!(scan.raw_tool_output_surface_count, 1);
        assert!(missing.contains(&"rendered_surfaces_must_not_be_raw_mcp_or_tool_output"));
        assert!(!scan.valid_structured_render_evidence);
    }
}
