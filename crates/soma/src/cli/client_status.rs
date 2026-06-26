//! `soma clients` - one-screen client readiness summary.
//!
//! This is a read-only UX layer over existing evidence: MCP config checks and
//! client-binding proof rows. It deliberately records no proof row and creates
//! no claim verification event.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use crate::capture::ai_cli::{resolve_db_path, IngestError};
use crate::cli::adapter_binding_proof::{
    build_client_binding_status_report, run_proof_session_blocking, AdapterBindingProofContext,
    ClientBindingArtifactFailure, ClientBindingExternalOperatorAction,
    ClientBindingProofRunbookStep, ClientBindingProofSession, ClientBindingProofSessionStage,
    ClientBindingReadinessStatus, EvidenceArtifactStatus,
};
use crate::cli::binary_identity::BinaryIdentity;
use crate::cli::learning_status;
use crate::cli::mcp_config::{self, McpClientKind};
use crate::cli::projects::{build_project_experience_report, ProjectExperienceContext};
use crate::cli::{
    AdapterBindingProofArgs, ClientStatusArgs, LearningStatusArgs, McpConfigArgs,
    ProjectExperienceArgs,
};
use crate::context::eval::DEFAULT_REQUIRED_PRIVATE_CLIENTS;
use crate::storage::{ClientBindingProofLevel, Storage, StorageError, StoredEpisode};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientStatusOutcome {
    pub schema: &'static str,
    pub source: &'static str,
    pub db_path: String,
    pub command: String,
    pub client_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<ClientProjectScopeSnapshot>,
    pub proof_storage_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_storage_error: Option<String>,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_client: Option<String>,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub semantic_review: ClientSemanticReviewStatus,
    pub operator_card: ClientOperatorCard,
    pub client_binding: ClientBindingReadinessIndex,
    pub readiness_index: ClientReadinessIndex,
    pub private_app_release_snapshot: ClientPrivateAppReleaseSnapshot,
    pub dogfood_index: ClientDogfoodIndex,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_cli_dogfood_probe: Option<ClientRealCliDogfoodProbeReport>,
    pub summary: ClientStatusSummary,
    pub clients: Vec<ClientStatusRow>,
    pub next_commands: Vec<Vec<String>>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientStatusSummary {
    pub client_count: usize,
    pub mcp_registration_ready_count: usize,
    pub runtime_detected_count: usize,
    pub runtime_missing_count: usize,
    pub explicit_cli_client_count: usize,
    pub explicit_cli_capture_available_count: usize,
    pub explicit_cli_real_capture_observed_count: usize,
    pub explicit_cli_real_capture_blocked_count: usize,
    pub explicit_cli_real_capture_failed_count: usize,
    pub explicit_cli_real_capture_unproven_count: usize,
    pub private_app_client_count: usize,
    pub private_app_capture_ready_count: usize,
    pub private_app_capture_unproven_count: usize,
    pub private_app_installed_config_ready_count: usize,
    pub private_app_target_config_ready_count: usize,
    pub private_app_trigger_hook_next_count: usize,
    pub private_app_hook_trigger_ready_count: usize,
    pub private_app_record_app_hook_next_count: usize,
    pub private_app_app_hook_proven_count: usize,
    pub private_app_in_client_render_proven_count: usize,
    pub private_app_review_action_proven_count: usize,
    pub private_capture_ready_count: usize,
    pub private_capture_unproven_count: usize,
    pub client_binding_rows_seen: usize,
    pub proof_storage_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientOperatorCard {
    pub source: &'static str,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_client: Option<String>,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub binary_identity: BinaryIdentity,
    pub primary_next_command_safety: ClientPrimaryNextCommandSafety,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_artifact_repair_summary: Option<ClientArtifactRepairSummary>,
    pub current_session_safety: ClientCurrentSessionSafety,
    pub mcp_ready_clients: Vec<String>,
    pub runtime_detected_clients: Vec<String>,
    pub runtime_missing_clients: Vec<String>,
    pub runtime_not_cli_detectable_clients: Vec<String>,
    pub runtime_check_commands: Vec<Vec<String>>,
    pub proof_storage_recovery_commands: Vec<Vec<String>>,
    pub private_app_restart_recommended_clients: Vec<String>,
    pub private_app_restart_commands: Vec<ClientPrivateAppRestartCommand>,
    pub continue_extension_config_not_visible_clients: Vec<String>,
    pub private_app_hook_trigger_ready_clients: Vec<String>,
    pub private_app_real_hook_ready_clients: Vec<String>,
    pub private_app_observed_app_hook_recordable_clients: Vec<String>,
    pub private_app_next_actions: Vec<ClientPrivateAppNextAction>,
    pub private_app_collector_start_commands: Vec<ClientPrivateAppCollectorStartCommand>,
    pub private_app_wait_commands: Vec<ClientPrivateAppWaitCommand>,
    pub private_app_hook_integration_templates: Vec<ClientPrivateHookIntegrationTemplate>,
    pub private_app_release_plan: Vec<ClientPrivateAppReleasePlanItem>,
    pub private_app_release_proof_checklist: Vec<ClientPrivateAppReleaseProofChecklist>,
    pub strict_private_client_hardening_required_clients: Vec<String>,
    pub strict_private_client_hardening_command: Vec<String>,
    pub observed_capture_dogfood_clients: Vec<String>,
    pub observed_capture_dogfood_evidence: Vec<ClientObservedCaptureDogfoodEvidence>,
    pub explicit_capture_ready_clients: Vec<String>,
    pub capture_dogfood_matrix: Vec<ClientCaptureDogfoodMatrixItem>,
    pub private_capture_ready_clients: Vec<String>,
    pub blocked_private_clients: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub safe_to_claim: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientReadinessIndex {
    pub source: &'static str,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_client: Option<String>,
    pub semantic_review_status: String,
    pub proof_storage_unavailable: bool,
    pub project_scope_status: Option<String>,
    pub ready_for_project_scoped_capture: Option<bool>,
    pub project_scope_storage_write_required_for_capture: Option<bool>,
    pub project_scope_storage_write_ready: Option<bool>,
    pub project_scope_storage_write_status: Option<String>,
    pub project_scope_active_persona: Option<String>,
    pub project_scope_current_client: Option<String>,
    pub project_scope_current_project: Option<String>,
    pub project_scope_current_session_id: Option<String>,
    pub project_scope_missing_envs: Vec<String>,
    pub scope_activation_commands: Vec<Vec<String>>,
    pub mcp_ready_clients: Vec<String>,
    pub runtime_detected_clients: Vec<String>,
    pub runtime_missing_clients: Vec<String>,
    pub runtime_not_cli_detectable_clients: Vec<String>,
    pub private_app_restart_recommended_clients: Vec<String>,
    pub private_app_restart_commands: Vec<ClientPrivateAppRestartCommand>,
    pub continue_extension_config_not_visible_clients: Vec<String>,
    pub private_app_hook_trigger_ready_clients: Vec<String>,
    pub private_app_real_hook_ready_clients: Vec<String>,
    pub private_app_observed_app_hook_recordable_clients: Vec<String>,
    pub private_app_next_actions: Vec<ClientPrivateAppNextAction>,
    pub private_app_collector_start_commands: Vec<ClientPrivateAppCollectorStartCommand>,
    pub private_app_wait_commands: Vec<ClientPrivateAppWaitCommand>,
    pub private_app_hook_integration_templates: Vec<ClientPrivateHookIntegrationTemplate>,
    pub private_app_release_plan: Vec<ClientPrivateAppReleasePlanItem>,
    pub private_app_release_proof_checklist: Vec<ClientPrivateAppReleaseProofChecklist>,
    pub private_app_release_snapshot: ClientPrivateAppReleaseSnapshot,
    pub strict_private_client_hardening_required_clients: Vec<String>,
    pub strict_private_client_hardening_command: Vec<String>,
    pub observed_capture_dogfood_clients: Vec<String>,
    pub observed_capture_dogfood_evidence: Vec<ClientObservedCaptureDogfoodEvidence>,
    pub explicit_capture_ready_clients: Vec<String>,
    pub capture_dogfood_matrix: Vec<ClientCaptureDogfoodMatrixItem>,
    pub private_capture_ready_clients: Vec<String>,
    pub blocked_private_clients: Vec<String>,
    pub primary_next_command: Vec<String>,
    pub primary_next_command_safety: ClientPrimaryNextCommandSafety,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_artifact_repair_summary: Option<ClientArtifactRepairSummary>,
    pub current_session_safety: ClientCurrentSessionSafety,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBindingReadinessIndex {
    pub source: &'static str,
    pub status: String,
    pub ready: bool,
    pub proof_storage_status: &'static str,
    pub proof_storage_unavailable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_client: Option<String>,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub primary_next_command: Vec<String>,
    pub private_app_next_actions: Vec<ClientPrivateAppNextAction>,
    pub required_client_proof_matrix: Vec<ClientPrivateAppReleaseProofChecklist>,
    pub proof_session_commands: Vec<Vec<String>>,
    pub release_runbook_commands: Vec<Vec<String>>,
    pub release_snapshot: ClientPrivateAppReleaseSnapshot,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientPrimaryNextCommandSafety {
    pub source: &'static str,
    pub classification: &'static str,
    pub run_from_separate_terminal_required: bool,
    pub disrupts_current_client_session: bool,
    pub requires_operator_confirmation: bool,
    pub writes_local_files: bool,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub installs_hook: bool,
    pub promotes_cloud_draft: bool,
    pub reason: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCurrentSessionSafety {
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_client: Option<String>,
    pub detected_surface: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_thread_id: Option<String>,
    pub primary_command_targets_current_session: bool,
    pub primary_command_safe_in_current_session: bool,
    pub recommended_execution_context: String,
    pub reason: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCurrentSessionActionSafety {
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_client: Option<String>,
    pub detected_surface: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_thread_id: Option<String>,
    pub action_targets_current_session: bool,
    pub action_safe_in_current_session: bool,
    pub recommended_execution_context: String,
    pub reason: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppNextAction {
    pub client: String,
    pub goal_status: String,
    pub ready_for_private_client_claim: bool,
    pub artifact_failure_count: usize,
    pub coherence_failure_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_repair_summary: Option<ClientArtifactRepairSummary>,
    pub release_gate_blockers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_release_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_step_id: Option<String>,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub current_session_action_safety: ClientCurrentSessionActionSafety,
    pub restart_recommended: bool,
    pub manual_restart_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quit_hint_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reopen_hint_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_event_observation_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_extension_config_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_extension_config_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_destination_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_collector_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_collector_listening: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_collector_start_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_collector_start_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_devdata_collector_managed_start_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_proof_levels: Vec<&'static str>,
    pub next_step: String,
    pub next_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppExternalActionSafety {
    pub source: &'static str,
    pub classification: &'static str,
    pub requires_operator_confirmation_before_submission: bool,
    pub may_transmit_prompt_to_provider: bool,
    pub suggested_minimal_test_prompt: String,
    pub forbidden_inputs: Vec<&'static str>,
    pub reason: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppWaitCommand {
    pub client: String,
    pub goal_status: String,
    pub operator_next_action_id: String,
    pub restart_recommended: bool,
    pub manual_restart_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quit_hint_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reopen_hint_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_event_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_jsonl_path: Option<String>,
    pub wait_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_wait_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_command: Option<Vec<String>>,
    pub instruction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppCollectorStartCommand {
    pub client: String,
    pub goal_status: String,
    pub operator_next_action_id: String,
    pub collector_status: String,
    pub collector_listening: bool,
    pub devdata_destination_visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_event_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_jsonl_path: Option<String>,
    pub start_command: Vec<String>,
    pub managed_start_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_wait_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_follow_up_wait_command: Option<Vec<String>>,
    pub proof_session_command: Vec<String>,
    pub instruction: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppRestartCommand {
    pub client: String,
    pub goal_status: String,
    pub operator_next_action_id: String,
    pub restart_recommended: bool,
    pub manual_restart_required: bool,
    pub execution_safety: ClientPrivateAppRestartExecutionSafety,
    pub quit_command: Vec<String>,
    pub reopen_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_event_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_jsonl_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_wait_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_follow_up_wait_command: Option<Vec<String>>,
    pub instruction: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppRestartExecutionSafety {
    pub run_from_separate_terminal_required: bool,
    pub disrupts_current_client_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppReleasePlanItem {
    pub client: String,
    pub status: String,
    pub current_stage: String,
    pub ready_for_private_client_claim: bool,
    pub requires_external_client_action: bool,
    pub ready_to_record_now: bool,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_level_statuses: Vec<ClientProofLevelStatus>,
    pub completed_proof_levels: Vec<&'static str>,
    pub missing_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_required_proof_level: Option<String>,
    pub release_gate_blockers: Vec<String>,
    pub next_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppProofRecordingHint {
    pub source: &'static str,
    pub proof_level: String,
    pub command: Vec<String>,
    pub records_proof: bool,
    pub requires_operator_confirmation: bool,
    pub requires_release_grade_confirmation: bool,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppReleaseProofChecklist {
    pub client: String,
    pub status: String,
    pub ready_for_private_client_claim: bool,
    pub required_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_level_statuses: Vec<ClientProofLevelStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_record_proof_levels: Vec<String>,
    pub ready_to_record_now: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_required_proof_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_proof_step_id: Option<String>,
    pub release_gate_blockers: Vec<String>,
    pub completion_criteria: Vec<&'static str>,
    pub proof_session_command: Vec<String>,
    pub release_runbook_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_release_runbook_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_install_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_readiness_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_hook_readiness_command: Option<Vec<String>>,
    pub next_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_recording_after_trusted_evidence: Option<ClientPrivateAppProofRecordingHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateAppReleaseSnapshot {
    pub source: &'static str,
    pub scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_filter: Option<String>,
    pub scope_description: String,
    pub status: String,
    pub ready: bool,
    pub private_app_client_count: usize,
    pub release_ready_count: usize,
    pub release_pending_count: usize,
    pub ready_clients: Vec<String>,
    pub pending_clients: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_pending_client: Option<String>,
    pub operator_status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub primary_release_gate_blockers: Vec<String>,
    pub primary_missing_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_next_required_proof_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_next_proof_step_id: Option<String>,
    pub pending_actions: Vec<ClientPrivateAppReleaseSnapshotAction>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientPrivateAppReleaseSnapshotAction {
    pub client: String,
    pub status: String,
    pub ready_for_private_client_claim: bool,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub release_gate_blockers: Vec<String>,
    pub missing_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_required_proof_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_proof_step_id: Option<String>,
    pub requires_external_client_action: bool,
    pub ready_to_record_now: bool,
    pub has_restart_command: bool,
    pub has_collector_start_command: bool,
    pub has_wait_command: bool,
    pub next_command: Vec<String>,
    pub next_command_safety: ClientPrimaryNextCommandSafety,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientDogfoodIndex {
    pub source: &'static str,
    pub status: &'static str,
    pub objective_count: usize,
    pub pass_count: usize,
    pub warning_count: usize,
    pub fail_count: usize,
    pub evidence_report_flow_status: &'static str,
    pub evidence_report_flow_summary: String,
    pub private_app_release_gate_status: String,
    pub private_app_release_gate_ready: bool,
    pub private_app_release_gate_ready_clients: Vec<String>,
    pub private_app_release_gate_pending_clients: Vec<String>,
    pub private_app_release_gate_summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_report: Option<ClientDogfoodEvidenceReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<ClientProjectScopeSnapshot>,
    pub objectives: Vec<ClientDogfoodObjective>,
    pub primary_next_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientProjectScopeSnapshot {
    pub source: &'static str,
    pub status: &'static str,
    pub active_persona: String,
    pub db_path: String,
    pub project_experience_status: String,
    pub project_provenance_status: String,
    pub session_project_status: String,
    pub cross_project_session_count: usize,
    pub unscoped_episode_count: usize,
    pub current_capture_scope_status: String,
    pub ready_for_project_scoped_capture: bool,
    pub storage_write_required_for_capture: bool,
    pub storage_write_ready: bool,
    pub storage_write_status: String,
    pub missing_scope_envs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_project: Option<String>,
    pub client_choice_required: bool,
    pub suggested_clients: Vec<String>,
    pub suggested_persona_call_commands: Vec<Vec<String>>,
    pub suggested_session_start_commands: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_client: Option<String>,
    pub warnings: Vec<String>,
    pub next_commands: Vec<Vec<String>>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientDogfoodEvidenceReport {
    pub source: &'static str,
    pub path: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_modified_at_unix_ms: Option<u64>,
    pub report_status: Option<String>,
    pub private_app_release_proof_status: Option<String>,
    pub private_app_release_proof_ready: Option<bool>,
    pub private_app_release_proof_ready_clients: Vec<String>,
    pub private_app_release_proof_pending_clients: Vec<String>,
    pub real_private_app_release_status: Option<String>,
    pub real_private_app_release_ready: Option<bool>,
    pub real_private_app_release_ready_clients: Vec<String>,
    pub real_private_app_release_pending_clients: Vec<String>,
    pub real_private_app_release_operator_status: Option<String>,
    pub real_private_app_release_operator_primary_next_step: Option<String>,
    pub real_private_app_release_operator_primary_next_command: Vec<String>,
    pub real_private_app_release_pending_actions: Vec<ClientDogfoodPrivateAppSnapshotAction>,
    pub client_mcp_context_capture_status: Option<String>,
    pub semantic_learning_review_status: Option<String>,
    pub multi_terminal_scope_status: Option<String>,
    pub private_client_proof_session_readiness_status: Option<String>,
    pub summary_pass: Option<u64>,
    pub summary_warn: Option<u64>,
    pub summary_fail: Option<u64>,
    pub current_private_app_snapshot_coherence: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub current_private_app_snapshot_mismatches: Vec<String>,
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientRealCliDogfoodProbeReport {
    pub source: &'static str,
    pub path: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_modified_at_unix_ms: Option<u64>,
    pub report_status: Option<String>,
    pub observed_clients: Vec<String>,
    pub blocked_clients: Vec<String>,
    pub failed_clients: Vec<String>,
    pub attempts: Vec<ClientRealCliDogfoodProbeAttempt>,
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientRealCliDogfoodProbeAttempt {
    pub client: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub observed_local_capture: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonl_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientRealCliDogfoodProbeClientStatus {
    pub source: &'static str,
    pub client: String,
    pub status: String,
    pub raw_status: String,
    pub report_path: String,
    pub report_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_modified_at_unix_ms: Option<u64>,
    pub observed_local_capture: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonl_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientDogfoodPrivateAppSnapshotAction {
    pub client: String,
    pub goal_status: Option<String>,
    pub operator_next_action_id: Option<String>,
    pub operator_next_action_label: Option<String>,
    pub release_gate_blockers: Vec<String>,
    pub missing_proof_levels: Vec<String>,
    pub has_restart_command: bool,
    pub restart_requires_separate_terminal: Option<bool>,
    pub has_collector_start_command: bool,
    pub has_wait_command: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientDogfoodObjective {
    pub objective: &'static str,
    pub status: &'static str,
    pub summary: String,
    pub evidence_refs: Vec<&'static str>,
    pub next_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientObservedCaptureDogfoodEvidence {
    pub source: &'static str,
    pub status: &'static str,
    pub client: String,
    pub episode_id: i64,
    pub evidence_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub captured_at_ns: i64,
    pub preview: String,
    pub recall_command: Vec<String>,
    pub context_why_command: Vec<String>,
    pub private_release_proof: bool,
    pub trust_boundary: &'static str,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCaptureDogfoodMatrixItem {
    pub source: &'static str,
    pub client: String,
    pub capture_model: String,
    pub mcp_registration_ready: bool,
    pub explicit_cli_capture_available: bool,
    pub observed_local_capture: bool,
    pub private_release_proof_ready: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_real_cli_probe_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_real_cli_probe_next_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_real_cli_probe_artifact_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_real_cli_probe_generated_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub next_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientStatusRow {
    pub client: &'static str,
    pub display_name: &'static str,
    pub status: String,
    pub ready: bool,
    pub ready_scope: &'static str,
    pub ready_meaning: &'static str,
    pub mcp_context_ready: bool,
    pub stored_local_capture_observed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_real_cli_capture_observed: Option<bool>,
    pub release_ready: bool,
    pub readiness_summary: String,
    pub target_path_hint: &'static str,
    pub mcp_registration_ready: bool,
    pub mcp_status: String,
    pub runtime_status: String,
    pub runtime_target: String,
    pub runtime_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_launch_probe_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_launch_probe_note: Option<String>,
    pub proof_storage_status: &'static str,
    pub capture_model: &'static str,
    pub goal_status: String,
    pub private_capture_status: String,
    pub ready_for_private_client_claim: bool,
    pub ready_for_client_operator_loop: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_capture_dogfood_evidence: Option<ClientObservedCaptureDogfoodEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_cli_dogfood_probe: Option<ClientRealCliDogfoodProbeClientStatus>,
    pub artifact_failure_count: usize,
    pub coherence_failure_count: usize,
    pub proof_stage: Option<String>,
    pub missing_proof_levels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_level_statuses: Vec<ClientProofLevelStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_release_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_operator_step_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_operator_step_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_operator_step_trust_boundary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_operator_step_requires_operator_action: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_external_action: Option<ClientBindingExternalOperatorAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_event_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_binding_nonce: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_jsonl_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_jsonl_probe_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_event_contract: Option<ClientPrivateEventContract>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_hook_integration_template: Option<ClientPrivateHookIntegrationTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_event_watch_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_event_wait_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_private_hook_readiness_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simple_private_event_wait_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_event_observation: Option<ClientPrivateEventObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_notify_reload_check: Option<ClientCodexNotifyReloadCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_extension_config_check: Option<ClientContinueExtensionConfigCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_session_blocking_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_session_ready_to_record_proof_levels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_session_stage_blockers: Vec<ClientProofSessionStageBlocker>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_session_runbook_steps: Vec<ClientProofSessionRunbookStepSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_ready_now_step_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_blocking_reason_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_config_eligible_candidates: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_config_setup_artifact_eligible_candidates: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_config_private_target_eligible_candidates: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub eligible_setup_artifact_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub eligible_private_client_target_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub private_client_target_candidate_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_repair_summary: Option<ClientArtifactRepairSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_repair_plan: Option<ClientArtifactRepairPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_next_action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_next_action_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_next_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_next_command: Option<Vec<String>>,
    pub next_step: String,
    pub next_commands: Vec<Vec<String>>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientArtifactRepairSummary {
    pub source: &'static str,
    pub status: &'static str,
    pub failure_count: usize,
    pub next_command: Vec<String>,
    pub next_command_safety: ClientPrimaryNextCommandSafety,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_checklist: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_observation_fields: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_recording_preconditions: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_evidence_artifact_scan: Option<ClientRenderEvidenceArtifactScan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_proof_packet_scan: Option<ClientRenderProofPacketScan>,
    pub proof_free_local_materialization_only: bool,
    pub requires_real_private_client_evidence_before_recording: bool,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub forbidden_shortcuts: Vec<&'static str>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientRenderEvidenceArtifactScan {
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub status: &'static str,
    pub placeholder_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_requirements: Vec<String>,
    pub proof_free_local_materialization_only: bool,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientRenderProofPacketScan {
    pub source: &'static str,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_render_json_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_render_markdown_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_render_html_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_evidence_path: Option<String>,
    pub review_render_json_exists: bool,
    pub review_render_markdown_exists: bool,
    pub review_render_html_exists: bool,
    pub render_evidence_exists: bool,
    pub placeholder_count: usize,
    pub next_step: &'static str,
    pub proof_free_local_materialization_only: bool,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientArtifactRepairPlan {
    pub source: &'static str,
    pub status: &'static str,
    pub client: &'static str,
    pub failure_count: usize,
    pub suggested_artifact_dir: String,
    pub suggested_artifact_dir_write_status: String,
    pub suggested_artifact_paths: Vec<ClientArtifactPathSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_fallback_artifact_dir: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspace_fallback_artifact_paths: Vec<ClientArtifactPathSuggestion>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspace_fallback_commands: Vec<Vec<String>>,
    pub failed_artifacts: Vec<ClientArtifactRepairFailure>,
    pub diagnostic_commands: Vec<Vec<String>>,
    pub recovery_steps: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientArtifactPathSuggestion {
    pub artifact_kind: String,
    pub path: String,
    pub intent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientArtifactRepairFailure {
    pub proof_id: i64,
    pub proof_level: String,
    pub artifact_kind: String,
    pub path: Option<String>,
    pub status: String,
    pub recovery_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientProofLevelStatus {
    pub proof_level: &'static str,
    pub review_stage: &'static str,
    pub status: &'static str,
    pub required_for_private_client_claim: bool,
    pub blocks_private_client_claim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientProofSessionStageBlocker {
    pub proof_level: String,
    pub ledger_status: String,
    pub artifact_status: Option<String>,
    pub ready_to_record_now: bool,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientProofSessionRunbookStepSummary {
    pub source: &'static str,
    pub id: String,
    pub title: String,
    pub intent: String,
    pub stage: String,
    pub evidence_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_arguments_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ClientPrivateAppExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_artifact_path: Option<String>,
    pub requires_operator_action: bool,
    pub records_proof: bool,
    pub ready_now: bool,
    pub blocking_reasons: Vec<String>,
    pub proof_step_trust_boundary: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateEventContract {
    pub client: String,
    pub event_source: String,
    pub binding_nonces: Vec<String>,
    pub schema: &'static str,
    pub writer_contract: &'static str,
    pub observed_at_ns: &'static str,
    pub source_boundary: &'static str,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientPrivateHookIntegrationTemplate {
    pub source: &'static str,
    pub client: String,
    pub read_only: bool,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub manual_invocation_policy: &'static str,
    pub wrapper: &'static str,
    pub wrapper_command_template: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub stdin_event_template_json: String,
    pub expected_spool_contract: ClientPrivateEventContract,
    pub operator_next_step: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateEventObservation {
    pub path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub event_count: usize,
    pub invalid_event_count: usize,
    pub matching_private_event_count: usize,
    pub matching_private_binding_nonce_count: usize,
    pub matching_private_non_release_manual_event_count: usize,
    pub matching_private_non_release_manual_binding_nonce_count: usize,
    pub matching_private_non_release_test_event_count: usize,
    pub matching_private_non_release_test_binding_nonce_count: usize,
    pub matching_private_event_seen: bool,
    pub matching_private_binding_nonce_seen: bool,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_event: Option<ClientPrivateEventSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevant_event: Option<ClientPrivateEventSummary>,
    pub latest_spool_mismatches: Vec<&'static str>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPrivateEventSummary {
    pub kind: Option<String>,
    pub schema: Option<String>,
    pub writer_contract: Option<String>,
    pub observed_at_ns: Option<i64>,
    pub client: Option<String>,
    pub event_source: Option<String>,
    pub binding_nonce: Option<String>,
    pub hook_adapter: Option<String>,
    pub manual_invocation_policy: Option<String>,
    pub collector_release_grade_candidate: Option<bool>,
    pub continue_profile_id: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub model_provider: Option<String>,
    pub model_name: Option<String>,
    pub model_title: Option<String>,
    pub has_prompt_text: bool,
    pub has_response_text: bool,
    pub has_output_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCodexNotifyReloadCheck {
    pub source: String,
    pub status: &'static str,
    pub config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_mtime_unix: Option<i64>,
    pub codex_desktop_process_count: usize,
    pub stale_codex_desktop_process_count: usize,
    pub restart_recommended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale_processes: Vec<ClientCodexProcessSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCodexProcessSummary {
    pub pid: i32,
    pub started_at_unix: i64,
    pub started_before_config: bool,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientContinueExtensionConfigCheck {
    pub source: String,
    pub status: &'static str,
    pub candidate_paths: Vec<String>,
    pub config_path: Option<String>,
    pub profile_config_status: &'static str,
    pub profile_config_path: Option<String>,
    pub profile_config_required_fields_present: bool,
    pub profile_config_missing_required_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_config_error: Option<String>,
    pub devdata_destination_status: &'static str,
    pub devdata_destination_visible: bool,
    pub devdata_config_path: Option<String>,
    pub devdata_destination: &'static str,
    pub devdata_install_command: Vec<String>,
    pub devdata_collector_command: Vec<String>,
    pub devdata_collector_status: &'static str,
    pub devdata_collector_listening: bool,
    pub devdata_collector_host: String,
    pub devdata_collector_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devdata_collector_error: Option<String>,
    pub devdata_collector_trust_boundary: &'static str,
    pub extension_installation_status: &'static str,
    pub extension_candidate_roots: Vec<String>,
    pub extension_paths: Vec<String>,
    pub extension_observed: bool,
    pub extension_next_step: String,
    pub recommended_config_path: String,
    pub mcp_config_command: Vec<String>,
    pub merge_required: bool,
    pub next_step: String,
    pub has_model_context_protocol: bool,
    pub has_mcp_servers: bool,
    pub has_soma_server: bool,
    pub restart_or_reload_recommended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueExtensionInstallationObservation {
    status: &'static str,
    candidate_roots: Vec<String>,
    extension_paths: Vec<String>,
    observed: bool,
    next_step: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueDevdataCollectorObservation {
    status: &'static str,
    listening: bool,
    host: String,
    port: u16,
    error: Option<String>,
    trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueDevdataDestinationObservation {
    status: &'static str,
    visible: bool,
    config_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueProfileConfigObservation {
    status: &'static str,
    config_path: Option<String>,
    required_fields_present: bool,
    missing_required_fields: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientProofSessionSummary {
    pub status: Option<String>,
    pub release_gate: Option<String>,
    pub next_step_id: Option<String>,
    pub next_operator_step_title: Option<String>,
    pub next_operator_step_intent: Option<String>,
    pub next_operator_step_trust_boundary: Option<String>,
    pub next_operator_step_requires_operator_action: Option<bool>,
    pub next_command: Option<Vec<String>>,
    pub next_mcp_tool: Option<String>,
    pub next_mcp_arguments: Option<Value>,
    pub external_action: Option<ClientBindingExternalOperatorAction>,
    pub expected_event_source: Option<String>,
    pub binding_nonce: Option<String>,
    pub generated_binding_nonce: Option<bool>,
    pub event_jsonl_path: Option<String>,
    pub event_jsonl_probe_status: Option<String>,
    pub blocking_reasons: Vec<String>,
    pub ready_to_record_proof_levels: Vec<String>,
    pub stage_blockers: Vec<ClientProofSessionStageBlocker>,
    pub runbook_steps: Vec<ClientProofSessionRunbookStepSummary>,
    pub ready_now_step_count: Option<usize>,
    pub blocking_reason_count: Option<usize>,
    pub installed_config_eligible_candidates: Option<usize>,
    pub installed_config_setup_artifact_eligible_candidates: Option<usize>,
    pub installed_config_private_target_eligible_candidates: Option<usize>,
    pub eligible_setup_artifact_paths: Vec<String>,
    pub eligible_private_client_target_paths: Vec<String>,
    pub private_client_target_candidate_paths: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticReviewStatus {
    pub source: &'static str,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub client: String,
    pub scope_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub primary_surface: String,
    pub workload_summary: ClientSemanticWorkloadSummary,
    pub pending_review_item_count: usize,
    pub l4_candidate_count: usize,
    pub review_only_candidate_count: usize,
    pub cloud_draft_blocked_count: usize,
    pub belief_candidate_count: usize,
    pub belief_group_count: usize,
    pub belief_hidden_duplicate_count: usize,
    pub belief_contradiction_count: usize,
    pub belief_substantive_contradiction_count: usize,
    pub belief_low_value_conflict_count: usize,
    pub belief_low_value_noise_count: usize,
    pub belief_noise_candidate_count: usize,
    pub belief_review_summary: ClientBeliefReviewSummary,
    pub should_interrupt: bool,
    pub next_step: String,
    pub workload_command: Vec<String>,
    pub primary_command: Vec<String>,
    pub next_commands: Vec<Vec<String>>,
    pub review_render_command: Vec<String>,
    pub review_digest_command: Vec<String>,
    pub review_report_command: Vec<String>,
    pub review_actions_command: Vec<String>,
    pub proof_session_command: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub semantic_resolution_actions: Vec<ClientSemanticResolutionAction>,
    pub review_cards: Vec<ClientSemanticReviewCard>,
    pub promotion_matrix: Vec<ClientSemanticPromotionMatrixRow>,
    pub review_lanes: Vec<ClientSemanticReviewLane>,
    pub next_mcp_tools: Vec<String>,
    pub control_contract: String,
    pub proof_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticWorkloadSummary {
    pub source: &'static str,
    pub scope_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub review_queue_pending_count: usize,
    pub cloud_draft_blocker_count: usize,
    pub l4_review_candidate_count: usize,
    pub manual_l4_review_count: usize,
    pub review_only_verification_count: usize,
    pub belief_resolution_blocker_count: usize,
    pub l4_promotion_blocking_count: usize,
    pub l2_audit_only_count: usize,
    pub context_projection_ready_count: usize,
    pub durable_learning_blocking_count: usize,
    pub operator_attention_count: usize,
    pub primary_operator_bucket: &'static str,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticReviewCard {
    pub source: &'static str,
    pub card_id: String,
    pub lane: String,
    pub priority: u8,
    pub target: String,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub primary_action: String,
    pub primary_command: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub blocks_l4_promotion: bool,
    pub projection_path: String,
    pub evidence_rule: String,
    pub accepted_verifier_types: Vec<String>,
    pub forbidden_evidence_sources: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticResolutionAction {
    pub source: &'static str,
    pub action: String,
    pub control_id: String,
    pub label: String,
    pub requires_evidence: bool,
    pub cli_command: Vec<String>,
    pub mcp_tool: String,
    pub evidence_rule: String,
    pub trust_effect: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticPromotionMatrixRow {
    pub source: &'static str,
    pub target: String,
    pub lane: String,
    pub status: String,
    pub candidate_count: usize,
    pub ready_for_manual_l4_review: bool,
    pub context_projection_ready: bool,
    pub blocks_l4_promotion: bool,
    pub projected_context_section: Option<String>,
    pub required_evidence: String,
    pub next_action: String,
    pub primary_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticReviewLane {
    pub source: &'static str,
    pub lane: String,
    pub priority: u8,
    pub status: String,
    pub count: usize,
    pub next_action: String,
    pub command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBeliefReviewSummary {
    pub source: &'static str,
    pub status: String,
    pub raw_candidate_count: usize,
    pub review_group_count: usize,
    pub hidden_duplicate_count: usize,
    pub substantive_contradiction_group_count: usize,
    pub substantive_contradiction_candidate_count: usize,
    pub low_value_conflict_group_count: usize,
    pub low_value_conflict_candidate_count: usize,
    pub low_value_noise_group_count: usize,
    pub low_value_noise_candidate_count: usize,
    pub noise_group_count: usize,
    pub noise_candidate_count: usize,
    pub primary_group_id: Option<i64>,
    pub next_action: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug)]
pub enum ClientStatusError {
    DbPath(IngestError),
    InvalidClient(String),
    Storage(StorageError),
    McpConfig(mcp_config::McpConfigError),
    Render(serde_json::Error),
}

impl ClientStatusError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ClientStatusError::DbPath(err) => crate::capture::ai_cli::exit_code_for(err),
            ClientStatusError::InvalidClient(_) => 2,
            ClientStatusError::Storage(_) | ClientStatusError::Render(_) => 2,
            ClientStatusError::McpConfig(err) => err.exit_code(),
        }
    }
}

impl std::fmt::Display for ClientStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientStatusError::DbPath(err) => write!(f, "resolve client status DB path: {err}"),
            ClientStatusError::InvalidClient(client) => write!(
                f,
                "invalid client `{client}`; expected all, claude-code, codex-cli, codex-app, cursor, or continue"
            ),
            ClientStatusError::Storage(err) => write!(f, "read client binding proofs: {err}"),
            ClientStatusError::McpConfig(err) => write!(f, "check MCP client config: {err}"),
            ClientStatusError::Render(err) => write!(f, "render client status: {err}"),
        }
    }
}

impl std::error::Error for ClientStatusError {}

impl From<StorageError> for ClientStatusError {
    fn from(value: StorageError) -> Self {
        ClientStatusError::Storage(value)
    }
}

impl From<mcp_config::McpConfigError> for ClientStatusError {
    fn from(value: mcp_config::McpConfigError) -> Self {
        ClientStatusError::McpConfig(value)
    }
}

fn load_dogfood_evidence_report(path: &str) -> ClientDogfoodEvidenceReport {
    let artifact_modified_at_unix_ms = dogfood_report_modified_at_unix_ms(path);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            return ClientDogfoodEvidenceReport {
                source: "soma_clients.dogfood_evidence_report.v1",
                path: path.to_string(),
                status: "unreadable",
                generated_at: None,
                generated_at_unix_ms: None,
                artifact_modified_at_unix_ms,
                report_status: None,
                private_app_release_proof_status: None,
                private_app_release_proof_ready: None,
                private_app_release_proof_ready_clients: Vec::new(),
                private_app_release_proof_pending_clients: Vec::new(),
                real_private_app_release_status: None,
                real_private_app_release_ready: None,
                real_private_app_release_ready_clients: Vec::new(),
                real_private_app_release_pending_clients: Vec::new(),
                real_private_app_release_operator_status: None,
                real_private_app_release_operator_primary_next_step: None,
                real_private_app_release_operator_primary_next_command: Vec::new(),
                real_private_app_release_pending_actions: Vec::new(),
                client_mcp_context_capture_status: None,
                semantic_learning_review_status: None,
                multi_terminal_scope_status: None,
                private_client_proof_session_readiness_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                current_private_app_snapshot_coherence: "not_checked",
                current_private_app_snapshot_mismatches: Vec::new(),
                error: Some(err.to_string()),
                trust_boundary: "dogfood_evidence_report_is_read_only: unreadable external dogfood evidence records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
            };
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => value,
        Err(err) => {
            return ClientDogfoodEvidenceReport {
                source: "soma_clients.dogfood_evidence_report.v1",
                path: path.to_string(),
                status: "invalid_json",
                generated_at: None,
                generated_at_unix_ms: None,
                artifact_modified_at_unix_ms,
                report_status: None,
                private_app_release_proof_status: None,
                private_app_release_proof_ready: None,
                private_app_release_proof_ready_clients: Vec::new(),
                private_app_release_proof_pending_clients: Vec::new(),
                real_private_app_release_status: None,
                real_private_app_release_ready: None,
                real_private_app_release_ready_clients: Vec::new(),
                real_private_app_release_pending_clients: Vec::new(),
                real_private_app_release_operator_status: None,
                real_private_app_release_operator_primary_next_step: None,
                real_private_app_release_operator_primary_next_command: Vec::new(),
                real_private_app_release_pending_actions: Vec::new(),
                client_mcp_context_capture_status: None,
                semantic_learning_review_status: None,
                multi_terminal_scope_status: None,
                private_client_proof_session_readiness_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                current_private_app_snapshot_coherence: "not_checked",
                current_private_app_snapshot_mismatches: Vec::new(),
                error: Some(err.to_string()),
                trust_boundary: "dogfood_evidence_report_is_read_only: invalid external dogfood evidence records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
            };
        }
    };
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("soma.client_dogfood_report.v1")
    {
        return ClientDogfoodEvidenceReport {
            source: "soma_clients.dogfood_evidence_report.v1",
            path: path.to_string(),
            status: "invalid_schema",
            generated_at: value
                .get("generated_at")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            generated_at_unix_ms: value.get("generated_at_unix_ms").and_then(serde_json::Value::as_u64),
            artifact_modified_at_unix_ms,
            report_status: value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            private_app_release_proof_status: dogfood_private_release_proof_status(&value),
            private_app_release_proof_ready: dogfood_private_release_proof_ready(&value),
            private_app_release_proof_ready_clients: dogfood_private_release_proof_ready_clients(
                &value,
            ),
            private_app_release_proof_pending_clients:
                dogfood_private_release_proof_pending_clients(&value),
            real_private_app_release_status: dogfood_real_private_release_status(&value),
            real_private_app_release_ready: dogfood_real_private_release_ready(&value),
            real_private_app_release_ready_clients: dogfood_real_private_release_ready_clients(
                &value,
            ),
            real_private_app_release_pending_clients:
                dogfood_real_private_release_pending_clients(&value),
            real_private_app_release_operator_status:
                dogfood_real_private_release_operator_status(&value),
            real_private_app_release_operator_primary_next_step:
                dogfood_real_private_release_operator_primary_next_step(&value),
            real_private_app_release_operator_primary_next_command:
                dogfood_real_private_release_operator_primary_next_command(&value),
            real_private_app_release_pending_actions:
                dogfood_real_private_release_pending_actions(&value),
            client_mcp_context_capture_status: dogfood_objective_status(
                &value,
                "client_mcp_context_capture",
            ),
            semantic_learning_review_status: dogfood_objective_status(
                &value,
                "semantic_learning_review",
            ),
            multi_terminal_scope_status: None,
            private_client_proof_session_readiness_status: dogfood_objective_status(
                &value,
                "private_client_proof_session_readiness",
            ),
            summary_pass: summary_count(&value, "pass"),
            summary_warn: summary_count(&value, "warn"),
            summary_fail: summary_count(&value, "fail"),
            current_private_app_snapshot_coherence: "not_checked",
            current_private_app_snapshot_mismatches: Vec::new(),
            error: Some("expected schema soma.client_dogfood_report.v1".to_string()),
            trust_boundary: "dogfood_evidence_report_is_read_only: wrong-schema external dogfood evidence records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
        };
    }
    ClientDogfoodEvidenceReport {
        source: "soma_clients.dogfood_evidence_report.v1",
        path: path.to_string(),
        status: "valid",
        generated_at: value
            .get("generated_at")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        generated_at_unix_ms: value.get("generated_at_unix_ms").and_then(serde_json::Value::as_u64),
        artifact_modified_at_unix_ms,
        report_status: value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        private_app_release_proof_status: dogfood_private_release_proof_status(&value),
        private_app_release_proof_ready: dogfood_private_release_proof_ready(&value),
        private_app_release_proof_ready_clients: dogfood_private_release_proof_ready_clients(
            &value,
        ),
        private_app_release_proof_pending_clients: dogfood_private_release_proof_pending_clients(
            &value,
        ),
        real_private_app_release_status: dogfood_real_private_release_status(&value),
        real_private_app_release_ready: dogfood_real_private_release_ready(&value),
        real_private_app_release_ready_clients: dogfood_real_private_release_ready_clients(&value),
        real_private_app_release_pending_clients: dogfood_real_private_release_pending_clients(
            &value,
        ),
        real_private_app_release_operator_status: dogfood_real_private_release_operator_status(
            &value,
        ),
        real_private_app_release_operator_primary_next_step:
            dogfood_real_private_release_operator_primary_next_step(&value),
        real_private_app_release_operator_primary_next_command:
            dogfood_real_private_release_operator_primary_next_command(&value),
        real_private_app_release_pending_actions: dogfood_real_private_release_pending_actions(
            &value,
        ),
        client_mcp_context_capture_status: dogfood_objective_status(
            &value,
            "client_mcp_context_capture",
        ),
        semantic_learning_review_status: dogfood_objective_status(
            &value,
            "semantic_learning_review",
        ),
        multi_terminal_scope_status: dogfood_objective_status(
            &value,
            "multi_terminal_persona_project_scope",
        ),
        private_client_proof_session_readiness_status: dogfood_objective_status(
            &value,
            "private_client_proof_session_readiness",
        ),
        summary_pass: summary_count(&value, "pass"),
        summary_warn: summary_count(&value, "warn"),
        summary_fail: summary_count(&value, "fail"),
        current_private_app_snapshot_coherence: "not_checked",
        current_private_app_snapshot_mismatches: Vec::new(),
        error: None,
        trust_boundary: "dogfood_evidence_report_is_read_only: accepted external dogfood evidence records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
    }
}

fn summary_count(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get("summary")?.get(key)?.as_u64()
}

fn json_string_vec(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn dogfood_report_modified_at_unix_ms(path: &str) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

fn load_real_cli_dogfood_probe_report(path: &str) -> ClientRealCliDogfoodProbeReport {
    let artifact_modified_at_unix_ms = dogfood_report_modified_at_unix_ms(path);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            return ClientRealCliDogfoodProbeReport {
                source: "soma_clients.real_cli_dogfood_probe.v1",
                path: path.to_string(),
                status: "unreadable",
                generated_at_unix_ms: None,
                artifact_modified_at_unix_ms,
                report_status: None,
                observed_clients: Vec::new(),
                blocked_clients: Vec::new(),
                failed_clients: Vec::new(),
                attempts: Vec::new(),
                error: Some(err.to_string()),
                trust_boundary: "real_cli_dogfood_probe_is_read_only: unreadable real CLI probe artifact records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
            };
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => value,
        Err(err) => {
            return ClientRealCliDogfoodProbeReport {
                source: "soma_clients.real_cli_dogfood_probe.v1",
                path: path.to_string(),
                status: "invalid_json",
                generated_at_unix_ms: None,
                artifact_modified_at_unix_ms,
                report_status: None,
                observed_clients: Vec::new(),
                blocked_clients: Vec::new(),
                failed_clients: Vec::new(),
                attempts: Vec::new(),
                error: Some(err.to_string()),
                trust_boundary: "real_cli_dogfood_probe_is_read_only: invalid real CLI probe artifact records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
            };
        }
    };
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("soma.real_cli_dogfood_probe.v1")
    {
        return ClientRealCliDogfoodProbeReport {
            source: "soma_clients.real_cli_dogfood_probe.v1",
            path: path.to_string(),
            status: "invalid_schema",
            generated_at_unix_ms: value.get("generated_at_unix_ms").and_then(serde_json::Value::as_u64),
            artifact_modified_at_unix_ms,
            report_status: value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            observed_clients: json_string_vec(&value, "observed_clients"),
            blocked_clients: json_string_vec(&value, "blocked_clients"),
            failed_clients: json_string_vec(&value, "failed_clients"),
            attempts: real_cli_probe_attempts(&value),
            error: Some("expected schema soma.real_cli_dogfood_probe.v1".to_string()),
            trust_boundary: "real_cli_dogfood_probe_is_read_only: wrong-schema real CLI probe artifact records no proof, creates no verification event, installs no hook, and promotes no cloud draft",
        };
    }
    ClientRealCliDogfoodProbeReport {
        source: "soma_clients.real_cli_dogfood_probe.v1",
        path: path.to_string(),
        status: "valid",
        generated_at_unix_ms: value.get("generated_at_unix_ms").and_then(serde_json::Value::as_u64),
        artifact_modified_at_unix_ms,
        report_status: value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        observed_clients: json_string_vec(&value, "observed_clients"),
        blocked_clients: json_string_vec(&value, "blocked_clients"),
        failed_clients: json_string_vec(&value, "failed_clients"),
        attempts: real_cli_probe_attempts(&value),
        error: None,
        trust_boundary: "real_cli_dogfood_probe_is_read_only: accepted real CLI probe artifact is observational only; it records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
    }
}

fn real_cli_probe_attempts(value: &serde_json::Value) -> Vec<ClientRealCliDogfoodProbeAttempt> {
    value
        .get("attempts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attempt| {
            Some(ClientRealCliDogfoodProbeAttempt {
                client: attempt.get("client")?.as_str()?.to_string(),
                status: attempt
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                exit_code: attempt.get("exit_code").and_then(serde_json::Value::as_i64),
                observed_local_capture: attempt
                    .get("observed_local_capture")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                project: attempt
                    .get("project")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                session_id: attempt
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                marker: attempt
                    .get("marker")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                jsonl_path: attempt
                    .get("jsonl_path")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                stderr_path: attempt
                    .get("stderr_path")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                last_message_path: attempt
                    .get("last_message_path")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                next_action: attempt
                    .get("next_action")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                trust_boundary: attempt
                    .get("trust_boundary")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("real_cli_probe_attempt_is_read_only")
                    .to_string(),
            })
        })
        .collect()
}

fn reconcile_dogfood_evidence_with_current_private_snapshot(
    mut report: ClientDogfoodEvidenceReport,
    current: &ClientPrivateAppReleaseSnapshot,
) -> ClientDogfoodEvidenceReport {
    if report.status != "valid" {
        report.current_private_app_snapshot_coherence = "not_checked";
        return report;
    }
    if current.scope != "all_clients" {
        report.current_private_app_snapshot_coherence = "filtered_current_scope";
        return report;
    }
    if report.real_private_app_release_status.is_none()
        && report.real_private_app_release_ready.is_none()
        && report.real_private_app_release_ready_clients.is_empty()
        && report.real_private_app_release_pending_clients.is_empty()
    {
        report.current_private_app_snapshot_coherence = "missing_real_snapshot";
        return report;
    }

    let mut mismatches = Vec::new();
    if report.real_private_app_release_status.as_deref() != Some(current.status.as_str()) {
        mismatches.push(format!(
            "real_private_app_release_status artifact={} current={}",
            report.real_private_app_release_status.as_deref().unwrap_or("missing"),
            current.status
        ));
    }
    if report.real_private_app_release_ready != Some(current.ready) {
        mismatches.push(format!(
            "real_private_app_release_ready artifact={} current={}",
            report
                .real_private_app_release_ready
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string()),
            current.ready
        ));
    }
    if sorted_string_vec(&report.real_private_app_release_ready_clients)
        != sorted_string_vec(&current.ready_clients)
    {
        mismatches.push(format!(
            "real_private_app_release_ready_clients artifact={} current={}",
            format_private_app_client_list(&sorted_string_vec(
                &report.real_private_app_release_ready_clients
            )),
            format_private_app_client_list(&sorted_string_vec(&current.ready_clients))
        ));
    }
    if sorted_string_vec(&report.real_private_app_release_pending_clients)
        != sorted_string_vec(&current.pending_clients)
    {
        mismatches.push(format!(
            "real_private_app_release_pending_clients artifact={} current={}",
            format_private_app_client_list(&sorted_string_vec(
                &report.real_private_app_release_pending_clients
            )),
            format_private_app_client_list(&sorted_string_vec(&current.pending_clients))
        ));
    }

    report.current_private_app_snapshot_coherence =
        if mismatches.is_empty() { "coherent" } else { "stale" };
    report.current_private_app_snapshot_mismatches = mismatches;
    report
}

fn sorted_string_vec(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values
}

fn dogfood_private_release_proof_status(value: &serde_json::Value) -> Option<String> {
    value
        .get("private_app_release_proof")
        .and_then(|release| release.get("status"))
        .or_else(|| value.get("release_private_app_proof_status"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn dogfood_private_release_proof_ready(value: &serde_json::Value) -> Option<bool> {
    value
        .get("private_app_release_proof")
        .and_then(|release| release.get("ready"))
        .or_else(|| value.get("release_private_app_proof_ready"))
        .and_then(serde_json::Value::as_bool)
}

fn dogfood_private_release_proof_ready_clients(value: &serde_json::Value) -> Vec<String> {
    json_string_array(
        value
            .get("private_app_release_proof")
            .and_then(|release| release.get("ready_clients"))
            .or_else(|| value.get("release_private_app_proof_ready_clients")),
    )
}

fn dogfood_private_release_proof_pending_clients(value: &serde_json::Value) -> Vec<String> {
    json_string_array(
        value
            .get("private_app_release_proof")
            .and_then(|release| release.get("pending_clients"))
            .or_else(|| value.get("release_private_app_proof_pending_clients")),
    )
}

fn dogfood_real_private_release_status(value: &serde_json::Value) -> Option<String> {
    value
        .get("real_private_app_release_status")
        .or_else(|| {
            value
                .get("real_private_app_release_snapshot")
                .and_then(|snapshot| snapshot.get("status"))
        })
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn dogfood_real_private_release_ready(value: &serde_json::Value) -> Option<bool> {
    value
        .get("real_private_app_release_ready")
        .or_else(|| {
            value
                .get("real_private_app_release_snapshot")
                .and_then(|snapshot| snapshot.get("ready"))
        })
        .and_then(serde_json::Value::as_bool)
}

fn dogfood_real_private_release_ready_clients(value: &serde_json::Value) -> Vec<String> {
    json_string_array(value.get("real_private_app_release_ready_clients").or_else(|| {
        value
            .get("real_private_app_release_snapshot")
            .and_then(|snapshot| snapshot.get("ready_clients"))
    }))
}

fn dogfood_real_private_release_pending_clients(value: &serde_json::Value) -> Vec<String> {
    json_string_array(value.get("real_private_app_release_pending_clients").or_else(|| {
        value
            .get("real_private_app_release_snapshot")
            .and_then(|snapshot| snapshot.get("pending_clients"))
    }))
}

fn dogfood_real_private_release_operator_status(value: &serde_json::Value) -> Option<String> {
    value
        .get("real_private_app_release_operator_status")
        .or_else(|| {
            value
                .get("real_private_app_release_snapshot")
                .and_then(|snapshot| snapshot.get("operator_status"))
        })
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn dogfood_real_private_release_operator_primary_next_step(
    value: &serde_json::Value,
) -> Option<String> {
    value
        .get("real_private_app_release_operator_primary_next_step")
        .or_else(|| {
            value
                .get("real_private_app_release_snapshot")
                .and_then(|snapshot| snapshot.get("operator_primary_next_step"))
        })
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn dogfood_real_private_release_operator_primary_next_command(
    value: &serde_json::Value,
) -> Vec<String> {
    json_string_array(value.get("real_private_app_release_operator_primary_next_command").or_else(
        || {
            value
                .get("real_private_app_release_snapshot")
                .and_then(|snapshot| snapshot.get("operator_primary_next_command"))
        },
    ))
}

fn dogfood_real_private_release_pending_actions(
    value: &serde_json::Value,
) -> Vec<ClientDogfoodPrivateAppSnapshotAction> {
    if let Some(top_level_actions) =
        value.get("real_private_app_release_pending_actions").and_then(serde_json::Value::as_array)
    {
        let parsed_actions: Vec<_> =
            top_level_actions.iter().filter_map(dogfood_pending_action_from_value).collect();
        if !parsed_actions.is_empty() {
            return parsed_actions;
        }
    }
    let Some(snapshot) = value.get("real_private_app_release_snapshot") else {
        return Vec::new();
    };
    let restart_commands_by_client =
        json_object_by_client(snapshot.get("operator_private_app_restart_commands"));
    let collector_start_commands_by_client =
        json_object_by_client(snapshot.get("operator_private_app_collector_start_commands"));
    let wait_commands_by_client =
        json_object_by_client(snapshot.get("operator_private_app_wait_commands"));
    let Some(rows) = snapshot.get("clients").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let client = row.get("client")?.as_str()?.to_string();
            let ready_for_private_client_claim = row
                .get("ready_for_private_client_claim")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if ready_for_private_client_claim {
                return None;
            }
            let restart_command = row
                .get("restart_command")
                .filter(|value| value.is_object())
                .or_else(|| restart_commands_by_client.get(&client).copied());
            let collector_start_command = row
                .get("collector_start_command")
                .filter(|value| value.is_object())
                .or_else(|| collector_start_commands_by_client.get(&client).copied());
            let wait_command = row
                .get("wait_command_card")
                .filter(|value| value.is_object())
                .or_else(|| wait_commands_by_client.get(&client).copied());
            let operator_next_action_id = row
                .get("operator_next_action_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    restart_command
                        .and_then(|command| command.get("operator_next_action_id"))
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| {
                    collector_start_command
                        .and_then(|command| command.get("operator_next_action_id"))
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| {
                    wait_command
                        .and_then(|command| command.get("operator_next_action_id"))
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| dogfood_infer_private_app_action_id(row));
            let operator_next_action_label = row
                .get("operator_next_action_label")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    operator_next_action_id
                        .map(|action_id| dogfood_private_app_action_label(action_id, &client))
                });
            let external_action_safety = operator_next_action_id
                .and_then(|action_id| dogfood_external_action_safety_for(&client, action_id));
            Some(ClientDogfoodPrivateAppSnapshotAction {
                client,
                goal_status: row
                    .get("goal_status")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                operator_next_action_id: operator_next_action_id.map(ToOwned::to_owned),
                operator_next_action_label,
                release_gate_blockers: json_string_array(row.get("release_gate_blockers")),
                missing_proof_levels: json_string_array(row.get("missing_proof_levels")),
                has_restart_command: restart_command.is_some(),
                restart_requires_separate_terminal: restart_command.and_then(|command| {
                    command
                        .get("execution_safety")
                        .and_then(|safety| safety.get("run_from_separate_terminal_required"))
                        .and_then(serde_json::Value::as_bool)
                }),
                has_collector_start_command: collector_start_command.is_some(),
                has_wait_command: wait_command.is_some(),
                external_action_safety,
                trust_boundary:
                    "dogfood_real_private_app_snapshot_action_is_read_only: mirrors operator guidance from an external dogfood artifact but records no proof row, creates no verification event, installs no hook, and cannot satisfy private client release gates",
            })
        })
        .collect()
}

fn dogfood_pending_action_from_value(
    action: &serde_json::Value,
) -> Option<ClientDogfoodPrivateAppSnapshotAction> {
    let client = action.get("client")?.as_str()?.to_string();
    let operator_next_action_id = action
        .get("operator_next_action_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let external_action_safety = operator_next_action_id
        .as_deref()
        .and_then(|action_id| dogfood_external_action_safety_for(&client, action_id));
    Some(ClientDogfoodPrivateAppSnapshotAction {
        client,
        goal_status: action
            .get("goal_status")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        operator_next_action_id,
        operator_next_action_label: action
            .get("operator_next_action_label")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        release_gate_blockers: json_string_array(action.get("release_gate_blockers")),
        missing_proof_levels: json_string_array(action.get("missing_proof_levels")),
        has_restart_command: action
            .get("has_restart_command")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        restart_requires_separate_terminal: action
            .get("restart_requires_separate_terminal")
            .and_then(serde_json::Value::as_bool),
        has_collector_start_command: action
            .get("has_collector_start_command")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        has_wait_command: action
            .get("has_wait_command")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        external_action_safety,
        trust_boundary:
            "dogfood_real_private_app_snapshot_action_is_read_only: mirrors operator guidance from an external dogfood artifact but records no proof row, creates no verification event, installs no hook, and cannot satisfy private client release gates",
    })
}

fn dogfood_external_action_safety_for(
    client: &str,
    action_id: &str,
) -> Option<ClientPrivateAppExternalActionSafety> {
    let client_kind = McpClientKind::parse_slug(client)?;
    client_private_app_external_action_safety_for(client, client_kind.display_name(), action_id)
}

fn dogfood_infer_private_app_action_id(row: &serde_json::Value) -> Option<&'static str> {
    match (
        row.get("goal_status").and_then(serde_json::Value::as_str),
        row.get("proof_session_next_step_id").and_then(serde_json::Value::as_str),
    ) {
        (Some("private_app_trigger_hook_required"), _)
        | (_, Some("trigger_private_client_hook")) => {
            Some("trigger_real_private_client_hook_to_write_private_spool_event")
        }
        (Some("private_app_release_grade_proof_ready"), _) => {
            Some("client_binding_release_gate_passed")
        }
        _ => None,
    }
}

fn json_object_by_client(
    value: Option<&serde_json::Value>,
) -> BTreeMap<String, &serde_json::Value> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let client = item.get("client")?.as_str()?;
                    if item.is_object() {
                        Some((client.to_string(), item))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn dogfood_private_app_action_label(action_id: &str, client: &str) -> String {
    match action_id {
        "client_binding_release_gate_passed" => "Release gate passed".to_string(),
        "restart_or_reopen_codex_app_before_real_hook" => "Quit/reopen Codex app".to_string(),
        "start_continue_devdata_collector_before_real_hook" => {
            "Start Continue dev-data collector".to_string()
        }
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            format!("Trigger real {client} hook")
        }
        "merge_continue_mcp_config_before_real_hook" => "Merge Continue MCP config".to_string(),
        "install_or_enable_continue_extension_before_real_hook" => {
            "Install or enable Continue extension".to_string()
        }
        other => other.replace('_', " "),
    }
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|clients| {
            clients.iter().filter_map(serde_json::Value::as_str).map(ToOwned::to_owned).collect()
        })
        .unwrap_or_default()
}

fn dogfood_objective_status(value: &serde_json::Value, objective: &str) -> Option<String> {
    value
        .get("objectives")?
        .as_array()?
        .iter()
        .find(|row| row.get("objective").and_then(serde_json::Value::as_str) == Some(objective))?
        .get("status")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn default_dogfood_report_path() -> Option<(PathBuf, bool)> {
    if let Some(value) = env::var_os("SOMA_CLIENT_DOGFOOD_REPORT").filter(|value| !value.is_empty())
    {
        return Some((PathBuf::from(value), true));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some((PathBuf::from(home).join(".soma/reports/client-dogfood-latest.json"), false))
}

fn default_real_cli_dogfood_report_path() -> Option<(PathBuf, bool)> {
    if let Some(value) =
        env::var_os("SOMA_REAL_CLI_DOGFOOD_REPORT").filter(|value| !value.is_empty())
    {
        return Some((PathBuf::from(value), true));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some((PathBuf::from(home).join(".soma/reports/real-cli-dogfood-latest.json"), false))
}

fn resolve_dogfood_evidence_report(args: &ClientStatusArgs) -> Option<ClientDogfoodEvidenceReport> {
    if let Some(path) = args.dogfood_report.as_deref() {
        return Some(load_dogfood_evidence_report(path));
    }
    let (path, explicit_env) = default_dogfood_report_path()?;
    if explicit_env || path.is_file() {
        return Some(load_dogfood_evidence_report(&path.to_string_lossy()));
    }
    None
}

fn resolve_real_cli_dogfood_probe_report(
    args: &ClientStatusArgs,
) -> Option<ClientRealCliDogfoodProbeReport> {
    if let Some(path) = args.real_cli_dogfood_report.as_deref() {
        return Some(load_real_cli_dogfood_probe_report(path));
    }
    let (path, explicit_env) = default_real_cli_dogfood_report_path()?;
    if explicit_env || path.is_file() {
        return Some(load_real_cli_dogfood_probe_report(&path.to_string_lossy()));
    }
    None
}

fn resolve_requested_client(
    value: Option<&str>,
) -> Result<Option<McpClientKind>, ClientStatusError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let normalized = value.to_ascii_lowercase().replace('_', "-");
    if normalized == "all" {
        return Ok(None);
    }
    McpClientKind::parse_slug(&normalized)
        .map(Some)
        .ok_or_else(|| ClientStatusError::InvalidClient(value.to_string()))
}

fn build_observed_capture_dogfood_evidence(
    db_path: &Path,
    project_filter: Option<&str>,
    requested_client: Option<McpClientKind>,
    limit: usize,
) -> BTreeMap<String, ClientObservedCaptureDogfoodEvidence> {
    let scan_limit = limit.clamp(1, 500);
    let episodes = match Storage::open(db_path).and_then(|store| store.recent_episodes(scan_limit))
    {
        Ok(episodes) => episodes,
        Err(err) => {
            tracing::warn!(
                db_path = %db_path.display(),
                error = %err,
                "observed capture dogfood evidence unavailable; continuing without local capture episode hints"
            );
            return BTreeMap::new();
        }
    };
    observed_capture_dogfood_from_episodes(&episodes, project_filter, requested_client)
}

fn observed_capture_dogfood_from_episodes(
    episodes: &[StoredEpisode],
    project_filter: Option<&str>,
    requested_client: Option<McpClientKind>,
) -> BTreeMap<String, ClientObservedCaptureDogfoodEvidence> {
    let mut by_client = BTreeMap::new();
    for episode in episodes {
        let source = episode.source.to_string();
        let Some(client) = McpClientKind::parse_slug(&source) else {
            continue;
        };
        if requested_client.is_some_and(|requested| requested != client) {
            continue;
        }
        if let Some(project) = project_filter {
            if episode.project.as_deref() != Some(project) {
                continue;
            }
        }
        by_client
            .entry(source.clone())
            .or_insert_with(|| observed_capture_dogfood_evidence_for_episode(client, episode));
    }
    by_client
}

fn observed_capture_dogfood_evidence_for_episode(
    client: McpClientKind,
    episode: &StoredEpisode,
) -> ClientObservedCaptureDogfoodEvidence {
    let preview = observed_capture_preview(episode);
    ClientObservedCaptureDogfoodEvidence {
        source: "soma_clients.observed_capture_dogfood_evidence.v1",
        status: "observed_local_capture_episode",
        client: client.as_str().to_string(),
        episode_id: episode.id,
        evidence_ref: format!("episode:{}", episode.id),
        project: episode.project.clone(),
        session_id: episode.session_id.clone(),
        cwd: episode.cwd.clone(),
        git_branch: episode.git_branch.clone(),
        captured_at_ns: episode.ts_start_ns,
        recall_command: observed_capture_recall_command(episode, &preview),
        context_why_command: observed_capture_context_why_command(episode, &preview),
        preview,
        private_release_proof: false,
        trust_boundary:
            "observed_capture_dogfood_evidence_is_read_only: cites stored local capture episodes only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for release-grade private app-hook/render/review-action proof",
    }
}

fn observed_capture_preview(episode: &StoredEpisode) -> String {
    let candidate = episode
        .prompt_text
        .as_deref()
        .or(episode.response_text.as_deref())
        .or(episode.command.as_deref())
        .or(episode.digest.as_deref())
        .or_else(|| episode.stdout.as_ref().and_then(|stdout| std::str::from_utf8(stdout).ok()))
        .unwrap_or("");
    let compact = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        format!("episode #{} from {}", episode.id, episode.source)
    } else {
        compact.chars().take(160).collect()
    }
}

fn observed_capture_recall_command(episode: &StoredEpisode, preview: &str) -> Vec<String> {
    let mut command = vec!["soma".to_string(), "recall".to_string()];
    push_optional_flag_value(&mut command, "--project", episode.project.as_deref());
    push_optional_flag_value(&mut command, "--session-id", episode.session_id.as_deref());
    command.extend(["--query".to_string(), preview.to_string()]);
    command
}

fn observed_capture_context_why_command(episode: &StoredEpisode, preview: &str) -> Vec<String> {
    let mut command = vec!["soma".to_string(), "context".to_string(), "why".to_string()];
    push_optional_flag_value(&mut command, "--project", episode.project.as_deref());
    push_optional_flag_value(&mut command, "--session-id", episode.session_id.as_deref());
    command.extend(["--query".to_string(), preview.to_string()]);
    command
}

fn push_optional_flag_value(command: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        command.push(flag.to_string());
        command.push(value.to_string());
    }
}

fn attach_observed_capture_dogfood(
    row: &mut ClientStatusRow,
    evidence: ClientObservedCaptureDogfoodEvidence,
) {
    row.safe_to_claim.push(format!(
        "Stored local capture dogfood evidence exists for {} as {}; private release proof remains separate.",
        evidence.client, evidence.evidence_ref
    ));
    row.observed_capture_dogfood_evidence = Some(evidence);
    refresh_client_row_readiness_contract(row);
}

pub fn run(args: &ClientStatusArgs) -> Result<ClientStatusOutcome, ClientStatusError> {
    let requested_client = resolve_requested_client(args.client.as_deref())?;
    let db_path = resolve_db_path(args.db_path.as_deref()).map_err(ClientStatusError::DbPath)?;
    let limit = args.limit.clamp(1, 500);
    let (proof_storage_status, proof_storage_error, proofs, binding_by_client) =
        match Storage::open(&db_path)
            .and_then(|store| store.recent_client_binding_proofs(None, limit))
        {
            Ok(proofs) => {
                let binding_status = build_client_binding_status_report(None, None, limit, &proofs);
                let binding_by_client: BTreeMap<_, _> = binding_status
                    .clients
                    .into_iter()
                    .map(|status| (status.client.clone(), status))
                    .collect();
                ("available", None, proofs, binding_by_client)
            }
            Err(err) => {
                tracing::warn!(
                    db_path = %db_path.display(),
                    error = %err,
                    "client binding proof storage unavailable; rendering MCP readiness without proof rows"
                );
                ("unavailable", Some(err.to_string()), Vec::new(), BTreeMap::new())
            }
        };

    let mcp_args = McpConfigArgs {
        client: None,
        all: true,
        command: args.command.clone(),
        check: true,
        hook_plan: false,
        brief: false,
        json: true,
        format: "json".to_string(),
    };
    let mcp_outcome = match mcp_config::run(&mcp_args)? {
        mcp_config::McpConfigRunOutcome::Aggregate(outcome) => outcome,
        mcp_config::McpConfigRunOutcome::Single(_) => unreachable!("--all yields aggregate"),
    };
    let command = mcp_outcome.command.clone();
    let semantic_review = build_semantic_review_status(
        args,
        &db_path,
        requested_client.map(|client| client.as_str()),
    );

    let proof_session_by_client = if proof_storage_status == "available" {
        build_private_app_proof_session_summaries(&db_path, requested_client, limit)
    } else {
        BTreeMap::new()
    };
    let observed_capture_dogfood = build_observed_capture_dogfood_evidence(
        &db_path,
        args.project.as_deref(),
        requested_client,
        limit,
    );

    let mut rows = Vec::new();
    for client in McpClientKind::all() {
        if requested_client.is_some_and(|filter| filter != *client) {
            continue;
        }
        let mcp = mcp_outcome
            .clients
            .iter()
            .find(|outcome| outcome.client == client.as_str())
            .and_then(|outcome| outcome.check.as_ref())
            .expect("aggregate MCP check includes every client");
        let binding = binding_by_client.get(client.as_str());
        let proof_session = proof_session_by_client.get(client.as_str());
        let mut row = row_for_client(*client, mcp, binding, proof_session, proof_storage_status);
        if let Some(evidence) = observed_capture_dogfood.get(client.as_str()).cloned() {
            attach_observed_capture_dogfood(&mut row, evidence);
        }
        rows.push(row);
    }

    let dogfood_evidence = resolve_dogfood_evidence_report(args);
    let real_cli_dogfood_probe = resolve_real_cli_dogfood_probe_report(args);
    attach_real_cli_dogfood_probe(&mut rows, real_cli_dogfood_probe.as_ref());
    let explicit_cli_client_count =
        rows.iter().filter(|row| row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL).count();
    let private_app_client_count =
        rows.iter().filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL).count();
    let summary = ClientStatusSummary {
        client_count: rows.len(),
        mcp_registration_ready_count: rows.iter().filter(|row| row.mcp_registration_ready).count(),
        runtime_detected_count: rows.iter().filter(|row| row.runtime_status == "detected").count(),
        runtime_missing_count: rows.iter().filter(|row| row.runtime_status == "missing").count(),
        explicit_cli_client_count,
        explicit_cli_capture_available_count: rows
            .iter()
            .filter(|row| row.goal_status == "explicit_cli_capture_available")
            .count(),
        explicit_cli_real_capture_observed_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL
                    && row.latest_real_cli_capture_observed == Some(true)
            })
            .count(),
        explicit_cli_real_capture_blocked_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL
                    && real_cli_probe_blocked_for_summary(row)
            })
            .count(),
        explicit_cli_real_capture_failed_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL
                    && real_cli_probe_failed_for_summary(row)
            })
            .count(),
        explicit_cli_real_capture_unproven_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL
                    && row.latest_real_cli_capture_observed != Some(true)
            })
            .count(),
        private_app_client_count,
        private_app_capture_ready_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL && row.ready_for_private_client_claim
            })
            .count(),
        private_app_capture_unproven_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && !row.ready_for_private_client_claim
            })
            .count(),
        private_app_installed_config_ready_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && row.installed_config_eligible_candidates.unwrap_or_default() > 0
            })
            .count(),
        private_app_target_config_ready_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && row.installed_config_private_target_eligible_candidates.unwrap_or_default()
                        > 0
            })
            .count(),
        private_app_trigger_hook_next_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && row.proof_session_next_step_id.as_deref()
                        == Some("trigger_private_client_hook")
            })
            .count(),
        private_app_hook_trigger_ready_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && private_app_operator_next_action_id(row)
                        == "trigger_real_private_client_hook_to_write_private_spool_event"
            })
            .count(),
        private_app_record_app_hook_next_count: rows
            .iter()
            .filter(|row| {
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                    && row.proof_session_next_step_id.as_deref() == Some("record_observed_app_hook")
            })
            .count(),
        private_app_app_hook_proven_count: proof_level_proven_count(
            &rows,
            ClientBindingProofLevel::ObservedAppHook.as_str(),
        ),
        private_app_in_client_render_proven_count: proof_level_proven_count(
            &rows,
            ClientBindingProofLevel::ObservedInClientRender.as_str(),
        ),
        private_app_review_action_proven_count: proof_level_proven_count(
            &rows,
            ClientBindingProofLevel::ObservedReviewAction.as_str(),
        ),
        private_capture_ready_count: rows
            .iter()
            .filter(|row| row.ready_for_private_client_claim)
            .count(),
        private_capture_unproven_count: rows
            .iter()
            .filter(|row| !row.ready_for_private_client_claim)
            .count(),
        client_binding_rows_seen: proofs.len(),
        proof_storage_unavailable: proof_storage_status != "available",
    };

    let project_scope = build_client_project_scope_snapshot(&db_path, args.project.as_deref());
    let operator_card = build_operator_card(
        &summary,
        &semantic_review,
        &rows,
        args.project.as_deref(),
        real_cli_dogfood_probe.as_ref(),
    );
    let mut next_commands = vec![
        vec!["tools/client-dogfood-report.sh".to_string()],
        vec![
            "tools/real-cli-dogfood-probe.sh".to_string(),
            "--client".to_string(),
            "all".to_string(),
            "--project".to_string(),
            args.project.clone().unwrap_or_else(|| "SOMA".to_string()),
        ],
        strict_private_client_hardening_command(args.project.as_deref()),
        vec![
            "soma".to_string(),
            "mcp-config".to_string(),
            "--all".to_string(),
            "--check".to_string(),
        ],
    ];
    push_next_command_once(&mut next_commands, operator_card.primary_next_command.clone());
    for command in &operator_card.private_app_restart_commands {
        push_next_command_once(&mut next_commands, command.quit_command.clone());
        push_next_command_once(&mut next_commands, command.reopen_command.clone());
    }
    for command in &operator_card.private_app_collector_start_commands {
        push_next_command_once(&mut next_commands, command.start_command.clone());
    }
    for action in &operator_card.private_app_next_actions {
        if let Some(command) = &action.quit_hint_command {
            push_next_command_once(&mut next_commands, command.clone());
        }
        if let Some(command) = &action.reopen_hint_command {
            push_next_command_once(&mut next_commands, command.clone());
        }
    }
    for command in pending_private_app_release_runbook_commands(&operator_card) {
        push_next_command_once(&mut next_commands, command);
    }
    if semantic_review_blocks_learning(&semantic_review) {
        push_next_command_once(
            &mut next_commands,
            semantic_review_primary_command(&semantic_review),
        );
    }
    let next_commands = next_commands
        .into_iter()
        .map(|command| {
            command_with_current_binary_when_path_soma_differs(
                command,
                &operator_card.binary_identity,
            )
        })
        .collect::<Vec<_>>();
    let private_app_release_snapshot = build_private_app_release_snapshot(
        &operator_card,
        requested_client.map(|client| client.as_str().to_string()),
    );
    let client_binding = build_client_binding_readiness_index(
        &summary,
        &operator_card,
        &private_app_release_snapshot,
        proof_storage_status,
    );
    let dogfood_evidence = dogfood_evidence.map(|report| {
        reconcile_dogfood_evidence_with_current_private_snapshot(
            report,
            &private_app_release_snapshot,
        )
    });
    let readiness_index = build_readiness_index(
        &summary,
        &semantic_review,
        &operator_card,
        &private_app_release_snapshot,
        project_scope.as_ref(),
    );
    let dogfood_index = build_dogfood_index(
        &summary,
        &semantic_review,
        &operator_card,
        &private_app_release_snapshot,
        dogfood_evidence,
        project_scope.clone(),
    );

    Ok(ClientStatusOutcome {
        schema: "soma.client_readiness_report.v1",
        source: "soma_clients_read_only_status",
        db_path: db_path.to_string_lossy().into_owned(),
        command,
        client_filter: requested_client.map(|client| client.as_str().to_string()),
        project_filter: args.project.clone(),
        project_scope,
        proof_storage_status,
        proof_storage_error,
        status: operator_card.status.clone(),
        operator_next_action_id: operator_card.operator_next_action_id.clone(),
        operator_next_action_label: operator_card.operator_next_action_label.clone(),
        primary_client: operator_card.primary_client.clone(),
        headline: operator_card.headline.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        semantic_review,
        operator_card,
        client_binding,
        readiness_index,
        private_app_release_snapshot,
        dogfood_index,
        real_cli_dogfood_probe,
        summary,
        clients: rows,
        next_commands,
        trust_boundary: "soma_clients_is_read_only: combines MCP config checks and stored client-binding proof status; it records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and does not prove private app capture beyond existing release-grade proof rows",
    })
}

fn build_dogfood_index(
    summary: &ClientStatusSummary,
    semantic_review: &ClientSemanticReviewStatus,
    operator_card: &ClientOperatorCard,
    private_app_release_snapshot: &ClientPrivateAppReleaseSnapshot,
    evidence_report: Option<ClientDogfoodEvidenceReport>,
    project_scope: Option<ClientProjectScopeSnapshot>,
) -> ClientDogfoodIndex {
    let unobserved_capture_clients = operator_card
        .capture_dogfood_matrix
        .iter()
        .filter(|item| !item.observed_local_capture)
        .map(|item| item.client.clone())
        .collect::<Vec<_>>();
    let internal_client_mcp_status = if summary.mcp_registration_ready_count < summary.client_count
    {
        "fail"
    } else if summary.explicit_cli_capture_available_count < summary.explicit_cli_client_count
        || !unobserved_capture_clients.is_empty()
    {
        "warn"
    } else {
        "pass"
    };
    let client_mcp_status = combine_with_dogfood_evidence(
        internal_client_mcp_status,
        evidence_report.as_ref(),
        |evidence| evidence.client_mcp_context_capture_status.as_deref(),
    );
    let private_app_status = if summary.proof_storage_unavailable {
        "fail"
    } else if summary.private_app_client_count == 0 {
        "warn"
    } else if summary.private_app_capture_ready_count == summary.private_app_client_count {
        "pass"
    } else {
        "fail"
    };
    let internal_semantic_status = match semantic_review.status.as_str() {
        "clear" | "noise_triage_only" => "pass",
        "blocked_cloud_draft_verification" | "unavailable" => "fail",
        _ => "warn",
    };
    let semantic_status = combine_with_dogfood_evidence(
        internal_semantic_status,
        evidence_report.as_ref(),
        |evidence| evidence.semantic_learning_review_status.as_deref(),
    );
    let (
        scope_status,
        scope_summary,
        scope_evidence_refs,
        scope_next_command,
        scope_trust_boundary,
    ) = dogfood_scope_objective_from_evidence(evidence_report.as_ref(), project_scope.as_ref());
    let mut client_mcp_summary = format!(
        "MCP registration ready for {}/{} client(s); explicit CLI capture available for {}/{} CLI client(s).",
        summary.mcp_registration_ready_count,
        summary.client_count,
        summary.explicit_cli_capture_available_count,
        summary.explicit_cli_client_count
    );
    client_mcp_summary.push_str(&dogfood_objective_note(
        evidence_report.as_ref(),
        "client_mcp_context_capture",
        evidence_report
            .as_ref()
            .and_then(|evidence| evidence.client_mcp_context_capture_status.as_deref()),
    ));
    if !operator_card.observed_capture_dogfood_clients.is_empty() {
        client_mcp_summary.push_str(&format!(
            " Live stored capture evidence observed for {}; this does not satisfy private app-hook/render/review-action release proof.",
            operator_card.observed_capture_dogfood_clients.join(",")
        ));
    }
    if !unobserved_capture_clients.is_empty() {
        client_mcp_summary.push_str(&format!(
            " Live stored capture evidence is still missing for {}; configured MCP/capture readiness is not the same as actual dogfood capture.",
            unobserved_capture_clients.join(",")
        ));
    }
    let mut private_app_summary = format!(
        "Private app proof ready for {}/{} app client(s); proof levels: app_hook={}/{} render={}/{} review_action={}/{}.",
        summary.private_app_capture_ready_count,
        summary.private_app_client_count,
        summary.private_app_app_hook_proven_count,
        summary.private_app_client_count,
        summary.private_app_in_client_render_proven_count,
        summary.private_app_client_count,
        summary.private_app_review_action_proven_count,
        summary.private_app_client_count
    );
    private_app_summary.push_str(&current_private_app_proof_ledger_note(operator_card));
    private_app_summary.push_str(&dogfood_private_proof_session_note(evidence_report.as_ref()));
    let mut semantic_summary = format!(
        "Semantic review status is `{}` with {} cloud draft blocker(s), {} L4 candidate(s), {} belief candidate(s), {} belief group(s), {} hidden duplicate(s), {} contradiction signal(s), {} substantive contradiction candidate(s), and {} low-value command-noise candidate(s).",
        semantic_review.status,
        semantic_review.cloud_draft_blocked_count,
        semantic_review.l4_candidate_count,
        semantic_review.belief_candidate_count,
        semantic_review.belief_group_count,
        semantic_review.belief_hidden_duplicate_count,
        semantic_review.belief_contradiction_count,
        semantic_review.belief_substantive_contradiction_count,
        semantic_review.belief_noise_candidate_count
    );
    semantic_summary.push_str(&dogfood_objective_note(
        evidence_report.as_ref(),
        "semantic_learning_review",
        evidence_report
            .as_ref()
            .and_then(|evidence| evidence.semantic_learning_review_status.as_deref()),
    ));
    let objectives = vec![
        ClientDogfoodObjective {
            objective: "client_mcp_context_capture",
            status: client_mcp_status,
            summary: client_mcp_summary,
            evidence_refs: vec![
                "soma mcp-config --all --check",
                "soma clients.summary",
                "episodes.source scoped local capture evidence",
                "tools/client-dogfood-report.sh explicit MCP capture matrix",
                "soma.client_dogfood_report.v1",
            ],
            next_command: vec!["tools/client-dogfood-report.sh".to_string()],
            trust_boundary:
                "dogfood_client_mcp_context_capture_is_read_only: summarizes generated MCP config, explicit CLI capture readiness, and optional dogfood evidence; records no proof row, installs no hook, and proves no private app capture",
        },
        ClientDogfoodObjective {
            objective: "private_app_binding_proof",
            status: private_app_status,
            summary: private_app_summary,
            evidence_refs: vec![
                "client_binding_proofs",
                "soma adapter-binding-proof --proof-session --json",
                "soma_client_binding_proof_session",
                "soma.client_dogfood_report.v1 private_client_proof_session_readiness",
                "soma.client_dogfood_report.v1 real_private_app_release_snapshot",
            ],
            next_command: operator_card.primary_next_command.clone(),
            trust_boundary:
                "dogfood_private_app_binding_proof_is_read_only: derives private app readiness from stored release-grade proof rows and artifact replay only; optional dogfood proof-session readiness can guide setup but cannot prove app-hook/render/review-action behavior, records no proof row, and does not prove private client behavior beyond cited proof evidence",
        },
        ClientDogfoodObjective {
            objective: "multi_terminal_persona_project_scope",
            status: scope_status,
            summary: scope_summary,
            evidence_refs: scope_evidence_refs,
            next_command: scope_next_command,
            trust_boundary: scope_trust_boundary,
        },
        ClientDogfoodObjective {
            objective: "semantic_learning_review",
            status: semantic_status,
            summary: semantic_summary,
            evidence_refs: vec![
                "soma learning --json",
                "soma context review-render --format json",
                "soma clients.semantic_review",
                "soma.client_dogfood_report.v1 semantic_learning_review",
            ],
            next_command: semantic_review_primary_command(semantic_review),
            trust_boundary:
                "dogfood_semantic_learning_review_is_read_only: mirrors learning/review status and optional dogfood evidence only; records no verification event, applies no proposal, writes no L4 fact, and promotes no cloud draft",
        },
    ];
    let pass_count = objectives.iter().filter(|objective| objective.status == "pass").count();
    let warning_count = objectives.iter().filter(|objective| objective.status == "warn").count();
    let fail_count = objectives.iter().filter(|objective| objective.status == "fail").count();
    let status = if fail_count > 0 {
        "fail"
    } else if warning_count > 0 {
        "warn"
    } else {
        "pass"
    };
    let primary_next_command = objectives
        .iter()
        .find(|objective| objective.status == "fail")
        .or_else(|| objectives.iter().find(|objective| objective.status == "warn"))
        .map(|objective| objective.next_command.clone())
        .unwrap_or_default();
    let evidence_report_flow_status = dogfood_evidence_report_flow_status(evidence_report.as_ref());
    let evidence_report_flow_summary =
        dogfood_evidence_report_flow_summary(evidence_report.as_ref());
    let private_app_release_gate_summary =
        dogfood_private_app_release_gate_summary(private_app_release_snapshot);
    ClientDogfoodIndex {
        source: "soma_clients.dogfood_index.v1",
        status,
        objective_count: objectives.len(),
        pass_count,
        warning_count,
        fail_count,
        evidence_report_flow_status,
        evidence_report_flow_summary,
        private_app_release_gate_status: private_app_release_snapshot.status.clone(),
        private_app_release_gate_ready: private_app_release_snapshot.ready,
        private_app_release_gate_ready_clients: private_app_release_snapshot.ready_clients.clone(),
        private_app_release_gate_pending_clients: private_app_release_snapshot.pending_clients.clone(),
        private_app_release_gate_summary,
        evidence_report,
        project_scope,
        objectives,
        primary_next_command,
        trust_boundary:
            "read_only_dogfood_index: maps the active SOMA dogfood objective to existing readiness, proof, optional dogfood JSON evidence, scope, and semantic-review evidence; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and never treats external dogfood guidance as private-client proof",
    }
}

fn dogfood_evidence_report_flow_status(
    evidence: Option<&ClientDogfoodEvidenceReport>,
) -> &'static str {
    let Some(evidence) = evidence else {
        return "missing";
    };
    if evidence.status != "valid" {
        return "invalid";
    }
    if evidence.report_status.as_deref() != Some("ready") {
        return match evidence.report_status.as_deref() {
            Some("fail" | "failed") => "fail",
            Some("warn" | "warning") => "warn",
            Some(_) => "pending",
            None => "pending",
        };
    }
    if evidence.summary_fail.unwrap_or_default() > 0 {
        "fail"
    } else if evidence.summary_warn.unwrap_or_default() > 0 {
        "warn"
    } else {
        "ready"
    }
}

fn dogfood_evidence_report_flow_summary(evidence: Option<&ClientDogfoodEvidenceReport>) -> String {
    let Some(evidence) = evidence else {
        return "No dogfood artifact is loaded; run `tools/client-dogfood-report.sh` to refresh the operator-flow evidence."
            .to_string();
    };
    if evidence.status != "valid" {
        return format!(
            "Dogfood artifact `{}` is `{}` and cannot be used as operator-flow evidence: {}.",
            evidence.path,
            evidence.status,
            evidence.error.as_deref().unwrap_or("unknown error")
        );
    }
    format!(
        "Dogfood artifact `{}` is {}; report_status={} summary=pass={} warn={} fail={}. This operator-flow result is separate from release-grade private app proof.",
        evidence.path,
        dogfood_evidence_report_flow_status(Some(evidence)),
        evidence.report_status.as_deref().unwrap_or("unknown"),
        format_optional_count(evidence.summary_pass),
        format_optional_count(evidence.summary_warn),
        format_optional_count(evidence.summary_fail),
    )
}

fn dogfood_private_app_release_gate_summary(snapshot: &ClientPrivateAppReleaseSnapshot) -> String {
    format!(
        "Current private app release gate is `{}`: ready={}, ready_clients={}, pending_clients={}. This gate is derived from stored release-grade proof rows and remains separate from the operator dogfood artifact.",
        snapshot.status,
        snapshot.ready,
        format_private_app_client_list(&snapshot.ready_clients),
        format_private_app_client_list(&snapshot.pending_clients)
    )
}

fn format_optional_count(count: Option<u64>) -> String {
    count.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string())
}

fn combine_with_dogfood_evidence(
    internal_status: &'static str,
    evidence: Option<&ClientDogfoodEvidenceReport>,
    objective_status: impl FnOnce(&ClientDogfoodEvidenceReport) -> Option<&str>,
) -> &'static str {
    let Some(evidence) = evidence else {
        return internal_status;
    };
    if evidence.status != "valid" {
        return "fail";
    }
    let external_status = match objective_status(evidence) {
        Some("pass") => "pass",
        Some("warn") | Some("not_run") => "warn",
        Some("fail") => "fail",
        Some(_) => "warn",
        None => "fail",
    };
    stricter_status(internal_status, external_status)
}

fn stricter_status(left: &'static str, right: &'static str) -> &'static str {
    if left == "fail" || right == "fail" {
        "fail"
    } else if left == "warn" || right == "warn" {
        "warn"
    } else {
        "pass"
    }
}

fn dogfood_objective_note(
    evidence: Option<&ClientDogfoodEvidenceReport>,
    objective: &str,
    objective_status: Option<&str>,
) -> String {
    let Some(evidence) = evidence else {
        return String::new();
    };
    if evidence.status != "valid" {
        return format!(
            " Dogfood artifact `{}` is `{}` and cannot verify `{objective}`.",
            evidence.path, evidence.status
        );
    }
    match objective_status {
        Some(status) => {
            format!(" Dogfood artifact `{}` reports `{}` for `{objective}`.", evidence.path, status)
        }
        None => format!(
            " Dogfood artifact `{}` is valid but does not contain `{objective}`.",
            evidence.path
        ),
    }
}

fn current_private_app_proof_ledger_note(operator_card: &ClientOperatorCard) -> String {
    format!(
        " Current proof ledger ready clients: {}; pending clients: {}.",
        format_private_app_client_list(&operator_card.private_capture_ready_clients),
        format_private_app_client_list(&operator_card.blocked_private_clients)
    )
}

fn format_private_app_client_list(clients: &[String]) -> String {
    if clients.is_empty() {
        "none".to_string()
    } else {
        clients.join(",")
    }
}

fn dogfood_private_proof_session_note(evidence: Option<&ClientDogfoodEvidenceReport>) -> String {
    let Some(evidence) = evidence else {
        return String::new();
    };
    if evidence.status != "valid" {
        return format!(
            " Dogfood artifact `{}` is `{}` and cannot verify proof-session readiness.",
            evidence.path, evidence.status
        );
    }
    let release_note = match evidence.private_app_release_proof_status.as_deref() {
        Some(status) => {
            let ready = evidence
                .private_app_release_proof_ready
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let ready_clients = if evidence.private_app_release_proof_ready_clients.is_empty() {
                "none".to_string()
            } else {
                evidence.private_app_release_proof_ready_clients.join(",")
            };
            let pending_clients = if evidence.private_app_release_proof_pending_clients.is_empty()
            {
                "none listed".to_string()
            } else {
                evidence.private_app_release_proof_pending_clients.join(",")
            };
            format!(
                " Dogfood proof-free release-flow status is `{status}` with ready={ready}, proof_free_ready_clients={ready_clients}, and proof_free_pending_clients={pending_clients}; this list comes from the dogfood artifact and is not the current proof ledger's ready/pending client list."
            )
        }
        None => {
            " Dogfood artifact does not contain `private_app_release_proof`; private app release proof remains derived only from stored proof rows.".to_string()
        }
    };
    let real_snapshot_note = match evidence.real_private_app_release_status.as_deref() {
        Some(status) => {
            let ready = evidence
                .real_private_app_release_ready
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let ready_clients = if evidence.real_private_app_release_ready_clients.is_empty() {
                "none".to_string()
            } else {
                evidence.real_private_app_release_ready_clients.join(",")
            };
            let pending_clients = if evidence.real_private_app_release_pending_clients.is_empty() {
                "none listed".to_string()
            } else {
                evidence.real_private_app_release_pending_clients.join(",")
            };
            format!(
                " Dogfood real-home release snapshot status is `{status}` with ready={ready}, real_ready_clients={ready_clients}, and real_pending_clients={pending_clients}; this snapshot is a read-only replay of `soma clients` under the user's real HOME and records no proof."
            )
        }
        None => {
            " Dogfood artifact does not contain `real_private_app_release_snapshot`; current proof ledger status remains the only live release-readiness source.".to_string()
        }
    };
    let coherence_note = dogfood_private_snapshot_coherence_note(evidence);
    let pending_action_note = dogfood_private_pending_action_note(evidence);
    let proof_session_note = match evidence.private_client_proof_session_readiness_status.as_deref() {
        Some(status) => format!(
            " Dogfood artifact `{}` reports `{}` for proof-session readiness, but private app readiness still requires release-grade observed_app_hook, observed_in_client_render, and observed_review_action proof rows.",
            evidence.path, status
        ),
        None => format!(
            " Dogfood artifact `{}` is valid but does not contain `private_client_proof_session_readiness`; private app readiness still requires release-grade proof rows.",
            evidence.path
        ),
    };
    format!(
        "{proof_session_note}{release_note}{real_snapshot_note}{coherence_note}{pending_action_note}"
    )
}

fn dogfood_private_snapshot_coherence_note(evidence: &ClientDogfoodEvidenceReport) -> String {
    match evidence.current_private_app_snapshot_coherence {
        "coherent" => {
            " Dogfood real-home release snapshot is coherent with the current proof ledger."
                .to_string()
        }
        "stale" => {
            let mismatches = if evidence.current_private_app_snapshot_mismatches.is_empty() {
                "unknown mismatch".to_string()
            } else {
                evidence.current_private_app_snapshot_mismatches.join("; ")
            };
            format!(
                " Dogfood real-home release snapshot is stale relative to the current proof ledger: {mismatches}. Refresh with `tools/client-dogfood-report.sh` before citing it as current dogfood evidence."
            )
        }
        "missing_real_snapshot" => {
            " Dogfood artifact has no real-home release snapshot to compare with the current proof ledger."
                .to_string()
        }
        "filtered_current_scope" => {
            " Dogfood real-home release snapshot is not compared because the current `soma clients` report is filtered to one client."
                .to_string()
        }
        _ => String::new(),
    }
}

fn dogfood_private_pending_action_note(evidence: &ClientDogfoodEvidenceReport) -> String {
    if evidence.real_private_app_release_pending_actions.is_empty() {
        return String::new();
    }
    let actions = evidence
        .real_private_app_release_pending_actions
        .iter()
        .map(|action| {
            let label = action
                .operator_next_action_label
                .as_deref()
                .or(action.operator_next_action_id.as_deref())
                .unwrap_or("unknown action");
            let mut markers = Vec::new();
            if action.has_restart_command {
                let safety = action
                    .restart_requires_separate_terminal
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                markers.push(format!("restart_command separate_terminal={safety}"));
            }
            if action.has_collector_start_command {
                markers.push("collector_start_command".to_string());
            }
            if action.has_wait_command {
                markers.push("wait_command".to_string());
            }
            let marker_text = if markers.is_empty() {
                String::new()
            } else {
                format!(" ({})", markers.join(", "))
            };
            format!("{}={label}{marker_text}", action.client)
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(" Dogfood real-home pending private app actions: {actions}.")
}

fn dogfood_scope_objective_from_evidence(
    evidence: Option<&ClientDogfoodEvidenceReport>,
    project_scope: Option<&ClientProjectScopeSnapshot>,
) -> (&'static str, String, Vec<&'static str>, Vec<String>, &'static str) {
    let base_refs = vec![
        "tools/client-dogfood-report.sh multi-terminal persona/project isolation",
        "soma projects --json",
        "soma projects.current_terminal_scope",
        "soma call <persona>",
        "soma session start --project",
    ];
    let (dogfood_status, mut summary, mut evidence_refs, trust_boundary) =
        match evidence {
            None => (
                "warn",
                "Scope isolation needs a refreshed external dogfood run; run `tools/client-dogfood-report.sh` to refresh ~/.soma/reports/client-dogfood-latest.json, or pass `--dogfood-report <path>` to replay a specific machine-readable dogfood artifact in this readiness surface.".to_string(),
                base_refs,
                "dogfood_scope_isolation_index_is_read_only: points to external dogfood evidence plus live project-scope status and records no session, persona, project, or proof mutation",
            ),
            Some(evidence) if evidence.status != "valid" => (
                "fail",
                format!(
                    "Scope isolation dogfood evidence artifact `{}` could not be accepted: {}.",
                    evidence.path,
                    evidence
                        .error
                        .as_deref()
                        .unwrap_or("invalid soma.client_dogfood_report.v1 artifact")
                ),
                vec![
                    "tools/client-dogfood-report.sh --json-out",
                    "soma.client_dogfood_report.v1",
                    "soma projects --json",
                    "soma call <persona>",
                    "soma session start --project",
                ],
                "dogfood_scope_isolation_index_with_artifact_is_read_only: invalid external dogfood evidence and live project-scope status are reported but never create proof rows, sessions, personas, projects, verification events, or cloud-draft promotions",
            ),
            Some(evidence) => match evidence.multi_terminal_scope_status.as_deref() {
                Some("pass") => (
                    "pass",
                    format!(
                        "Scope isolation is verified by dogfood evidence `{}`: multi-terminal persona/project objective passed.",
                        evidence.path
                    ),
                    vec![
                        "tools/client-dogfood-report.sh --json-out",
                        "soma.client_dogfood_report.v1",
                        "multi_terminal_persona_project_scope",
                        "soma projects --json",
                        "soma call <persona>",
                        "soma session start --project",
                    ],
                    "dogfood_scope_isolation_index_with_artifact_is_read_only: cites external dogfood evidence and live project-scope status only; records no session, persona, project, proof row, verification event, or cloud-draft promotion",
                ),
                Some("warn") => (
                    "warn",
                    format!(
                        "Scope isolation dogfood evidence `{}` is present but reports warning status for the multi-terminal persona/project objective.",
                        evidence.path
                    ),
                    vec![
                        "tools/client-dogfood-report.sh --json-out",
                        "soma.client_dogfood_report.v1",
                        "multi_terminal_persona_project_scope",
                        "soma projects --json",
                    ],
                    "dogfood_scope_isolation_index_with_artifact_is_read_only: warning external dogfood evidence and live project-scope status are surfaced without mutating proof or memory state",
                ),
                Some("fail") => (
                    "fail",
                    format!(
                        "Scope isolation dogfood evidence `{}` reports a failed multi-terminal persona/project objective.",
                        evidence.path
                    ),
                    vec![
                        "tools/client-dogfood-report.sh --json-out",
                        "soma.client_dogfood_report.v1",
                        "multi_terminal_persona_project_scope",
                        "soma projects --json",
                    ],
                    "dogfood_scope_isolation_index_with_artifact_is_read_only: failed external dogfood evidence and live project-scope status are surfaced without mutating proof or memory state",
                ),
                _ => (
                    "fail",
                    format!(
                        "Scope isolation dogfood evidence `{}` is valid but does not contain a multi_terminal_persona_project_scope objective.",
                        evidence.path
                    ),
                    vec![
                        "tools/client-dogfood-report.sh --json-out",
                        "soma.client_dogfood_report.v1",
                        "multi_terminal_persona_project_scope",
                        "soma projects --json",
                    ],
                    "dogfood_scope_isolation_index_with_artifact_is_read_only: incomplete external dogfood evidence and live project-scope status are surfaced without mutating proof or memory state",
                ),
            },
        };
    let mut next_command = vec!["tools/client-dogfood-report.sh".to_string()];
    if let Some(project_scope) = project_scope {
        let note = format!(
            " Live project scope reports `{}`: current_capture_scope={}, project_experience_status={}, session_project_status={}, unscoped_episodes={}.",
            project_scope.status,
            project_scope.current_capture_scope_status,
            project_scope.project_experience_status,
            project_scope.session_project_status,
            project_scope.unscoped_episode_count
        );
        summary.push_str(&note);
        if !project_scope.warnings.is_empty() {
            summary.push_str(" Live scope warnings: ");
            summary.push_str(&project_scope.warnings.join("; "));
            summary.push('.');
        }
        evidence_refs.extend(["soma projects --json", "soma projects.current_terminal_scope"]);
        evidence_refs.sort_unstable();
        evidence_refs.dedup();
        if project_scope.status != "pass" {
            if let Some(command) = project_scope.next_commands.first() {
                next_command.clone_from(command);
            }
        }
        return (
            worst_status(dogfood_status, project_scope.status),
            summary,
            evidence_refs,
            next_command,
            trust_boundary,
        );
    }
    (dogfood_status, summary, evidence_refs, next_command, trust_boundary)
}

fn build_client_project_scope_snapshot(
    db_path: &Path,
    project_filter: Option<&str>,
) -> Option<ClientProjectScopeSnapshot> {
    let args = ProjectExperienceArgs {
        project: project_filter.map(ToOwned::to_owned),
        evidence_limit: 5,
        format: "json".to_string(),
        brief: false,
        current_terminal: false,
        require_current_terminal_scope: false,
        json: true,
        db_path: Some(db_path.to_string_lossy().into_owned()),
        dogfood_report: None,
    };
    let ctx = ProjectExperienceContext { db_path: db_path.to_path_buf() };
    let report = build_project_experience_report(&args, &ctx).ok()?;
    let current = &report.current_terminal_scope;
    let status = client_project_scope_status(&report);
    let mut warnings = report.scope_warnings.clone();
    warnings.extend(current.warnings.clone());
    warnings.sort();
    warnings.dedup();
    Some(ClientProjectScopeSnapshot {
        source: "soma_clients.project_scope_snapshot.v1",
        status,
        active_persona: report.active_persona,
        db_path: report.db_path,
        project_experience_status: report.status.to_string(),
        project_provenance_status: report.scope_integrity.project_provenance_status.to_string(),
        session_project_status: report.scope_integrity.session_project_status.to_string(),
        cross_project_session_count: report.scope_integrity.cross_project_session_count,
        unscoped_episode_count: report.unscoped_episode_count,
        current_capture_scope_status: current.capture_scope_status.to_string(),
        ready_for_project_scoped_capture: current.ready_for_project_scoped_capture,
        storage_write_required_for_capture: report.scope_contract.storage_write_required_for_capture,
        storage_write_ready: current.storage_write_ready,
        storage_write_status: current.storage_write_status.to_string(),
        missing_scope_envs: current
            .missing_scope_envs
            .iter()
            .map(|env| (*env).to_string())
            .collect(),
        suggested_project: current.suggested_project.clone(),
        client_choice_required: current.client_choice_required,
        suggested_clients: current.suggested_clients.clone(),
        suggested_persona_call_commands: current.suggested_persona_call_commands.clone(),
        suggested_session_start_commands: current.suggested_session_start_commands.clone(),
        current_project: current.project.clone(),
        current_session_id: current.session_id.clone(),
        current_client: current.client.clone(),
        warnings,
        next_commands: if !current.suggested_persona_call_commands.is_empty() {
            current.suggested_persona_call_commands.clone()
        } else if current.suggested_session_start_commands.is_empty() {
            current.next_commands.clone()
        } else {
            current.suggested_session_start_commands.clone()
        },
        trust_boundary: "soma_clients_project_scope_snapshot_is_read_only: mirrors soma projects current-terminal and provenance status only; records no session, persona, project, proof row, verification event, or cloud-draft promotion",
    })
}

fn client_project_scope_status(
    report: &crate::cli::projects::ProjectExperienceReport,
) -> &'static str {
    let current = &report.current_terminal_scope;
    if report.storage_status != "available" {
        return "fail";
    }
    if current.capture_scope_status == "project_scoped_capture_storage_not_writable" {
        return "fail";
    }
    if matches!(current.capture_scope_status, "persona_db_mismatch" | "project_filter_mismatch") {
        return "fail";
    }
    if !current.ready_for_project_scoped_capture
        || report.status == "scope_review_required"
        || report.status == "project_provenance_incomplete"
        || report.scope_integrity.project_provenance_status != "complete"
        || report.scope_integrity.session_project_status != "single_project_sessions"
    {
        return "warn";
    }
    "pass"
}

fn worst_status(left: &'static str, right: &'static str) -> &'static str {
    if left == "fail" || right == "fail" {
        "fail"
    } else if left == "warn" || right == "warn" {
        "warn"
    } else {
        "pass"
    }
}

fn build_readiness_index(
    summary: &ClientStatusSummary,
    semantic_review: &ClientSemanticReviewStatus,
    operator_card: &ClientOperatorCard,
    private_app_release_snapshot: &ClientPrivateAppReleaseSnapshot,
    project_scope: Option<&ClientProjectScopeSnapshot>,
) -> ClientReadinessIndex {
    ClientReadinessIndex {
        source: "soma_clients.readiness_index.v1",
        status: operator_card.status.clone(),
        operator_next_action_id: operator_card.operator_next_action_id.clone(),
        operator_next_action_label: operator_card.operator_next_action_label.clone(),
        primary_client: operator_card.primary_client.clone(),
        semantic_review_status: semantic_review.status.clone(),
        proof_storage_unavailable: summary.proof_storage_unavailable,
        project_scope_status: project_scope.map(|scope| scope.status.to_string()),
        ready_for_project_scoped_capture: project_scope
            .map(|scope| scope.ready_for_project_scoped_capture),
        project_scope_storage_write_required_for_capture: project_scope
            .map(|scope| scope.storage_write_required_for_capture),
        project_scope_storage_write_ready: project_scope.map(|scope| scope.storage_write_ready),
        project_scope_storage_write_status: project_scope
            .map(|scope| scope.storage_write_status.clone()),
        project_scope_active_persona: project_scope.map(|scope| scope.active_persona.clone()),
        project_scope_current_client: project_scope.and_then(|scope| scope.current_client.clone()),
        project_scope_current_project: project_scope
            .and_then(|scope| scope.current_project.clone()),
        project_scope_current_session_id: project_scope
            .and_then(|scope| scope.current_session_id.clone()),
        project_scope_missing_envs: project_scope
            .map(|scope| scope.missing_scope_envs.clone())
            .unwrap_or_default(),
        scope_activation_commands: project_scope
            .map(|scope| scope.next_commands.clone())
            .unwrap_or_default(),
        mcp_ready_clients: operator_card.mcp_ready_clients.clone(),
        runtime_detected_clients: operator_card.runtime_detected_clients.clone(),
        runtime_missing_clients: operator_card.runtime_missing_clients.clone(),
        runtime_not_cli_detectable_clients: operator_card.runtime_not_cli_detectable_clients.clone(),
        private_app_restart_recommended_clients: operator_card
            .private_app_restart_recommended_clients
            .clone(),
        private_app_restart_commands: operator_card.private_app_restart_commands.clone(),
        continue_extension_config_not_visible_clients: operator_card
            .continue_extension_config_not_visible_clients
            .clone(),
        private_app_hook_trigger_ready_clients: operator_card
            .private_app_hook_trigger_ready_clients
            .clone(),
        private_app_real_hook_ready_clients: operator_card.private_app_real_hook_ready_clients.clone(),
        private_app_observed_app_hook_recordable_clients: operator_card
            .private_app_observed_app_hook_recordable_clients
            .clone(),
        private_app_next_actions: operator_card.private_app_next_actions.clone(),
        private_app_collector_start_commands: operator_card
            .private_app_collector_start_commands
            .clone(),
        private_app_wait_commands: operator_card.private_app_wait_commands.clone(),
        private_app_hook_integration_templates: operator_card
            .private_app_hook_integration_templates
            .clone(),
        private_app_release_plan: operator_card.private_app_release_plan.clone(),
        private_app_release_proof_checklist: operator_card
            .private_app_release_proof_checklist
            .clone(),
        private_app_release_snapshot: private_app_release_snapshot.clone(),
        strict_private_client_hardening_required_clients: operator_card
            .strict_private_client_hardening_required_clients
            .clone(),
        strict_private_client_hardening_command: operator_card
            .strict_private_client_hardening_command
            .clone(),
        observed_capture_dogfood_clients: operator_card.observed_capture_dogfood_clients.clone(),
        observed_capture_dogfood_evidence: operator_card
            .observed_capture_dogfood_evidence
            .clone(),
        explicit_capture_ready_clients: operator_card.explicit_capture_ready_clients.clone(),
        capture_dogfood_matrix: operator_card.capture_dogfood_matrix.clone(),
        private_capture_ready_clients: operator_card.private_capture_ready_clients.clone(),
        blocked_private_clients: operator_card.blocked_private_clients.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        primary_next_command_safety: operator_card.primary_next_command_safety.clone(),
        primary_external_action_safety: operator_card.primary_external_action_safety.clone(),
        primary_artifact_repair_summary: operator_card.primary_artifact_repair_summary.clone(),
        current_session_safety: operator_card.current_session_safety.clone(),
        trust_boundary: "read_only_readiness_index: top-level machine index derived from operator_card, summary, semantic_review, private_app_release_snapshot, and read-only project_scope only; records no proof, installs no hook, creates no verification event, creates no session/persona/project state, and promotes no cloud draft",
    }
}

fn build_client_binding_readiness_index(
    summary: &ClientStatusSummary,
    operator_card: &ClientOperatorCard,
    private_app_release_snapshot: &ClientPrivateAppReleaseSnapshot,
    proof_storage_status: &'static str,
) -> ClientBindingReadinessIndex {
    let proof_session_commands = operator_card
        .private_app_release_proof_checklist
        .iter()
        .map(|checklist| checklist.proof_session_command.clone())
        .collect::<Vec<_>>();
    let release_runbook_commands = operator_card
        .private_app_release_proof_checklist
        .iter()
        .map(|checklist| checklist.release_runbook_command.clone())
        .collect::<Vec<_>>();

    ClientBindingReadinessIndex {
        source: "soma_clients.client_binding_readiness_index.v1",
        status: operator_card.status.clone(),
        ready: private_app_release_snapshot.ready,
        proof_storage_status,
        proof_storage_unavailable: summary.proof_storage_unavailable,
        primary_client: operator_card.primary_client.clone(),
        operator_next_action_id: operator_card.operator_next_action_id.clone(),
        operator_next_action_label: operator_card.operator_next_action_label.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        private_app_next_actions: operator_card.private_app_next_actions.clone(),
        required_client_proof_matrix: operator_card.private_app_release_proof_checklist.clone(),
        proof_session_commands,
        release_runbook_commands,
        release_snapshot: private_app_release_snapshot.clone(),
        trust_boundary: "client_binding_readiness_index_is_read_only: mirrors existing private app proof checklist, next actions, and release snapshot for client UI/API consumers; records no proof row, creates no verification event, installs no hook, writes no client config, and promotes no cloud draft",
    }
}

fn strict_private_client_hardening_required_clients() -> Vec<String> {
    DEFAULT_REQUIRED_PRIVATE_CLIENTS.iter().map(|client| (*client).to_string()).collect()
}

fn strict_private_client_hardening_command(project_filter: Option<&str>) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "hardening-report".to_string(),
        "--json".to_string(),
        "--require-client-binding-ready".to_string(),
    ];
    if let Some(project) =
        project_filter.map(ToOwned::to_owned).or_else(crate::project::current_name)
    {
        command.push("--project".to_string());
        command.push(project);
    }
    let config_root = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("$HOME"))
        .to_string_lossy()
        .into_owned();
    command.push("--client-binding-config-root".to_string());
    command.push(config_root);
    command
}

pub fn render_json(outcome: &ClientStatusOutcome) -> Result<String, ClientStatusError> {
    serde_json::to_string_pretty(outcome)
        .map(|mut text| {
            text.push('\n');
            text
        })
        .map_err(ClientStatusError::Render)
}

fn render_external_action_safety(
    out: &mut String,
    indent: &str,
    safety: &ClientPrivateAppExternalActionSafety,
) {
    let forbidden_inputs = external_action_forbidden_inputs(safety);
    out.push_str(&format!(
        "{indent}external action safety: class={} operator_confirmation_before_submission={} may_transmit_prompt_to_provider={}\n",
        safety.classification,
        safety.requires_operator_confirmation_before_submission,
        safety.may_transmit_prompt_to_provider
    ));
    out.push_str(&format!(
        "{indent}minimal test prompt: {}\n",
        safety.suggested_minimal_test_prompt
    ));
    out.push_str(&format!("{indent}forbidden inputs: {forbidden_inputs}\n"));
    out.push_str(&format!("{indent}safety reason: {}\n", safety.reason));
}

fn external_action_forbidden_inputs(safety: &ClientPrivateAppExternalActionSafety) -> String {
    if safety.forbidden_inputs.is_empty() {
        "none".to_string()
    } else {
        safety.forbidden_inputs.join(",")
    }
}

fn primary_external_action_safety(
    outcome: &ClientStatusOutcome,
) -> Option<&ClientPrivateAppExternalActionSafety> {
    if let Some(safety) = outcome.operator_card.primary_external_action_safety.as_ref() {
        return Some(safety);
    }
    outcome
        .operator_card
        .private_app_next_actions
        .iter()
        .find(|action| {
            action.operator_next_action_id == outcome.operator_card.operator_next_action_id
                && action.external_action_safety.is_some()
        })
        .and_then(|action| action.external_action_safety.as_ref())
}

fn render_primary_external_action_safety(
    out: &mut String,
    safety: &ClientPrivateAppExternalActionSafety,
) {
    out.push_str(&format!(
        "  Primary external action safety: class={} operator_confirmation_before_submission={} may_transmit_prompt_to_provider={}\n",
        safety.classification,
        safety.requires_operator_confirmation_before_submission,
        safety.may_transmit_prompt_to_provider
    ));
    out.push_str(&format!("    minimal test prompt: {}\n", safety.suggested_minimal_test_prompt));
    out.push_str(&format!("    forbidden inputs: {}\n", external_action_forbidden_inputs(safety)));
    out.push_str(&format!("    safety reason: {}\n", safety.reason));
}

fn next_command_annotation(
    outcome: &ClientStatusOutcome,
    command: &[String],
) -> Option<&'static str> {
    if outcome.operator_card.primary_next_command == command
        && (outcome.operator_card.primary_next_command_safety.run_from_separate_terminal_required
            || outcome.operator_card.primary_next_command_safety.disrupts_current_client_session)
    {
        return Some("run from separate terminal; may close current client session");
    }
    for restart in &outcome.operator_card.private_app_restart_commands {
        if restart.quit_command == command {
            return Some("run from separate terminal; may close current client session");
        }
        if restart.reopen_command == command {
            return Some(
                "reopen hint after quit; does not force a stale running process to reload",
            );
        }
    }
    None
}

fn private_app_proof_requirement(level: &str) -> &'static str {
    match level {
        "observed_app_hook" => {
            "real private hook event with matching event_source and binding_nonce"
        }
        "observed_in_client_render" => {
            "structured in-client render evidence bound to review-render output"
        }
        "observed_review_action" => {
            "rendered control action report with trusted non-cloud verification evidence"
        }
        _ => "release-grade client-binding evidence",
    }
}

fn private_app_proof_ladder_status(
    checklist: &ClientPrivateAppReleaseProofChecklist,
    level: &str,
) -> &'static str {
    if let Some(status) =
        checklist.proof_level_statuses.iter().find(|status| status.proof_level == level)
    {
        match status.status {
            "recorded" => return "done",
            "artifact_invalid" => return "artifact_invalid",
            "missing" => {}
            other => return other,
        }
    }
    if !checklist.missing_proof_levels.contains(&level) {
        "done"
    } else if checklist.ready_to_record_proof_levels.iter().any(|ready| ready == level) {
        "ready_to_record"
    } else if checklist.next_required_proof_level.as_deref() == Some(level) {
        "next"
    } else {
        "pending"
    }
}

fn render_private_app_proof_ladder(
    out: &mut String,
    checklist: &ClientPrivateAppReleaseProofChecklist,
) {
    if checklist.ready_for_private_client_claim {
        return;
    }
    out.push_str("      pending proof ladder:\n");
    for level in private_app_required_proof_levels() {
        out.push_str(&format!(
            "        {level}: {} - {}\n",
            private_app_proof_ladder_status(checklist, level),
            private_app_proof_requirement(level)
        ));
    }
}

fn format_private_app_proof_level_statuses(statuses: &[ClientProofLevelStatus]) -> String {
    if statuses.is_empty() {
        return "none".to_string();
    }
    statuses
        .iter()
        .map(|status| format!("{}={}", status.proof_level, status.status))
        .collect::<Vec<_>>()
        .join(",")
}

fn private_app_recording_command(client: &str, proof_level: &str) -> Option<Vec<String>> {
    let mut command = vec!["env".to_string(), format!("SOMA_CLIENT_BINDING_CLIENT={client}")];
    match proof_level {
        "observed_app_hook" => {
            command.extend([
                "SOMA_CONFIRM_REAL_CLIENT_HOOK=1".to_string(),
                "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1".to_string(),
                "tools/soma-client-record-app-hook-proof.sh".to_string(),
            ]);
            Some(command)
        }
        "observed_in_client_render" => {
            let artifact_dir = format!("$HOME/.soma/client-evidence/{client}/<run-id>");
            command.extend([
                "SOMA_CONFIRM_IN_CLIENT_RENDER=1".to_string(),
                "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1".to_string(),
                format!(
                    "SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT={artifact_dir}/review-render.json"
                ),
                format!("SOMA_CLIENT_BINDING_RENDER_EVIDENCE={artifact_dir}/render-evidence.json"),
                "tools/soma-client-record-render-proof.sh".to_string(),
            ]);
            Some(command)
        }
        "observed_review_action" => {
            let artifact_dir = format!("$HOME/.soma/client-evidence/{client}/<run-id>");
            command.extend([
                "SOMA_CONFIRM_REVIEW_ACTION=1".to_string(),
                "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1".to_string(),
                format!(
                    "SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT={artifact_dir}/review-action.json"
                ),
                "tools/soma-client-record-review-action-proof.sh".to_string(),
            ]);
            Some(command)
        }
        _ => None,
    }
}

fn private_app_recording_hint(
    client: &str,
    proof_level: Option<&str>,
) -> Option<ClientPrivateAppProofRecordingHint> {
    let proof_level = proof_level?;
    let command = private_app_recording_command(client, proof_level)?;
    Some(ClientPrivateAppProofRecordingHint {
        source: "soma_clients.private_app_proof_recording_hint.v1",
        proof_level: proof_level.to_string(),
        command,
        records_proof: true,
        requires_operator_confirmation: true,
        requires_release_grade_confirmation: true,
        trust_boundary: "private_app_proof_recording_hint_is_guidance_only: the displayed command may record a client-binding proof row only after trusted non-cloud evidence exists and the operator supplies explicit confirmation flags; rendering this hint records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
    })
}

fn render_private_app_recording_hint(
    out: &mut String,
    checklist: &ClientPrivateAppReleaseProofChecklist,
) {
    let Some(hint) = &checklist.next_recording_after_trusted_evidence else {
        return;
    };
    out.push_str(&format!(
        "      record {} after trusted evidence: {}\n",
        hint.proof_level,
        hint.command.join(" ")
    ));
}

fn render_private_app_proof_session_checkpoint(out: &mut String, outcome: &ClientStatusOutcome) {
    let pending_rows = outcome
        .clients
        .iter()
        .filter(|row| {
            row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                && !row.ready_for_private_client_claim
                && (row.proof_session_next_step_id.is_some()
                    || !row.proof_session_blocking_reasons.is_empty()
                    || !row.proof_session_runbook_steps.is_empty())
        })
        .collect::<Vec<_>>();
    if pending_rows.is_empty() {
        return;
    }

    out.push_str("  Private app proof-session checkpoint:\n");
    for row in pending_rows {
        let next_step_id = row.proof_session_next_step_id.as_deref().unwrap_or("none");
        let next_runbook_step =
            row.proof_session_runbook_steps.iter().find(|step| step.id == next_step_id);
        let ready_now_step_count = row.proof_session_ready_now_step_count.unwrap_or_else(|| {
            row.proof_session_runbook_steps.iter().filter(|step| step.ready_now).count()
        });
        let blocking_reason_count = row
            .proof_session_blocking_reason_count
            .unwrap_or(row.proof_session_blocking_reasons.len());
        let requires_operator_action = row
            .proof_session_next_operator_step_requires_operator_action
            .or_else(|| next_runbook_step.map(|step| step.requires_operator_action))
            .unwrap_or(false);
        let ready_to_record = if row.proof_session_ready_to_record_proof_levels.is_empty() {
            "none".to_string()
        } else {
            row.proof_session_ready_to_record_proof_levels.join(",")
        };

        out.push_str(&format!(
            "    {}: status={} release_gate={} next_step={} ready_now_steps={} blocking_reasons={} requires_operator_action={} ready_to_record={}\n",
            row.client,
            row.proof_session_status.as_deref().unwrap_or("unknown"),
            row.proof_session_release_gate.as_deref().unwrap_or("unknown"),
            next_step_id,
            ready_now_step_count,
            blocking_reason_count,
            requires_operator_action,
            ready_to_record
        ));
        if let Some(title) = row.proof_session_next_operator_step_title.as_deref() {
            out.push_str(&format!("      next title: {title}\n"));
        }
        if let Some(intent) = row.proof_session_next_operator_step_intent.as_deref() {
            out.push_str(&format!("      next intent: {intent}\n"));
        }
        if let Some(command) = &row.proof_session_next_command {
            out.push_str(&format!("      next command: {}\n", command.join(" ")));
        }
        if let Some(tool) = &row.proof_session_next_mcp_tool {
            let arguments = row
                .proof_session_next_mcp_arguments
                .as_ref()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "{}".to_string());
            out.push_str(&format!("      next MCP: {tool} {arguments}\n"));
        }
        if let Some(safety) =
            next_runbook_step.and_then(|step| step.external_action_safety.as_ref())
        {
            out.push_str(&format!(
                "      external action safety: class={} operator_confirmation={} may_send_prompt_to_provider={} minimal_prompt=\"{}\"\n",
                safety.classification,
                safety.requires_operator_confirmation_before_submission,
                safety.may_transmit_prompt_to_provider,
                safety.suggested_minimal_test_prompt
            ));
            out.push_str(&format!(
                "      forbidden inputs: {}\n",
                safety.forbidden_inputs.join(", ")
            ));
        }
        if !row.proof_session_blocking_reasons.is_empty() {
            out.push_str(&format!(
                "      blockers: {}\n",
                row.proof_session_blocking_reasons.join("; ")
            ));
        }
    }
}

fn render_project_scope_checkpoint(out: &mut String, outcome: &ClientStatusOutcome) {
    let Some(scope) = outcome.project_scope.as_ref() else {
        return;
    };

    out.push_str("  Project scope checkpoint:\n");
    out.push_str(&format!(
        "    status={} active_persona={} ready_for_project_scoped_capture={} storage_write_status={} storage_write_ready={} current_capture_scope={} project_experience={} session_project={} unscoped_episodes={} cross_project_sessions={}\n",
        scope.status,
        scope.active_persona,
        scope.ready_for_project_scoped_capture,
        scope.storage_write_status,
        scope.storage_write_ready,
        scope.current_capture_scope_status,
        scope.project_experience_status,
        scope.session_project_status,
        scope.unscoped_episode_count,
        scope.cross_project_session_count
    ));
    out.push_str(&format!(
        "    current client={} project={} session={}\n",
        scope.current_client.as_deref().unwrap_or("unset"),
        scope.current_project.as_deref().unwrap_or("unset"),
        scope.current_session_id.as_deref().unwrap_or("unset")
    ));
    if !scope.missing_scope_envs.is_empty() {
        out.push_str(&format!("    missing scope envs: {}\n", scope.missing_scope_envs.join(",")));
    }
    if let Some(project) = scope.suggested_project.as_deref() {
        let clients = if scope.suggested_clients.is_empty() {
            "none".to_string()
        } else {
            scope.suggested_clients.join(",")
        };
        out.push_str(&format!(
            "    suggested project={} client_choice_required={} suggested_clients={}\n",
            project, scope.client_choice_required, clients
        ));
    }
    if !scope.suggested_persona_call_commands.is_empty() {
        out.push_str("    scope call commands:\n");
        for command in &scope.suggested_persona_call_commands {
            out.push_str(&format!("      {}\n", command.join(" ")));
        }
    }
    if !scope.suggested_session_start_commands.is_empty() {
        out.push_str("    scope start commands:\n");
        for command in &scope.suggested_session_start_commands {
            out.push_str(&format!("      {}\n", command.join(" ")));
        }
    } else if scope.suggested_persona_call_commands.is_empty() && !scope.next_commands.is_empty() {
        out.push_str("    scope commands:\n");
        for command in scope.next_commands.iter().take(3) {
            out.push_str(&format!("      {}\n", command.join(" ")));
        }
    }
    if !scope.warnings.is_empty() {
        out.push_str("    scope warnings:\n");
        for warning in scope.warnings.iter().take(5) {
            out.push_str(&format!("      {warning}\n"));
        }
    }
}

fn render_semantic_learning_checkpoint(out: &mut String, semantic: &ClientSemanticReviewStatus) {
    if semantic.error.is_some() && semantic.promotion_matrix.is_empty() {
        return;
    }

    out.push_str("  Semantic learning checkpoint:\n");
    out.push_str(&format!(
        "    status={} surface={} should_interrupt={} pending_review_items={} l4_candidates={} review_only_candidates={} cloud_draft_blockers={} belief_candidates={}\n",
        semantic.status,
        semantic.primary_surface,
        semantic.should_interrupt,
        semantic.pending_review_item_count,
        semantic.l4_candidate_count,
        semantic.review_only_candidate_count,
        semantic.cloud_draft_blocked_count,
        semantic.belief_candidate_count
    ));
    if !semantic.review_lanes.is_empty() {
        out.push_str("    review lanes:\n");
        for lane in &semantic.review_lanes {
            out.push_str(&format!(
                "      P{} {}: status={} count={} next={}\n",
                lane.priority, lane.lane, lane.status, lane.count, lane.next_action
            ));
            if !lane.command.is_empty() {
                out.push_str(&format!("        command: {}\n", lane.command.join(" ")));
            }
        }
    }
    if !semantic.promotion_matrix.is_empty() {
        out.push_str("    promotion lanes:\n");
        for lane in &semantic.promotion_matrix {
            out.push_str(&format!(
                "      {}: status={} candidates={} manual_l4={} context_projection={} blocks_l4={} projected={}\n",
                lane.target,
                lane.status,
                lane.candidate_count,
                lane.ready_for_manual_l4_review,
                lane.context_projection_ready,
                lane.blocks_l4_promotion,
                lane.projected_context_section.as_deref().unwrap_or("none")
            ));
            if lane.blocks_l4_promotion
                || lane.ready_for_manual_l4_review
                || lane.target == "cloud_draft"
            {
                out.push_str(&format!("        required evidence: {}\n", lane.required_evidence));
                if !lane.primary_command.is_empty() {
                    out.push_str(&format!("        command: {}\n", lane.primary_command.join(" ")));
                }
            }
        }
    }
    if !semantic.review_cards.is_empty() {
        let blocking_l4 =
            semantic.review_cards.iter().filter(|card| card.blocks_l4_promotion).count();
        let cloud_draft_cards =
            semantic.review_cards.iter().filter(|card| card.target == "cloud_draft").count();
        let belief_cards =
            semantic.review_cards.iter().filter(|card| card.target == "belief").count();
        out.push_str(&format!(
            "    review_cards={} blocking_l4={} cloud_draft_cards={} belief_cards={}\n",
            semantic.review_cards.len(),
            blocking_l4,
            cloud_draft_cards,
            belief_cards
        ));
    }
}

fn render_dogfood_artifact_checkpoint(out: &mut String, outcome: &ClientStatusOutcome) {
    let Some(report) = outcome.dogfood_index.evidence_report.as_ref() else {
        out.push_str(
            "  Dogfood artifact: missing latest report; run tools/client-dogfood-report.sh\n",
        );
        return;
    };
    let report_status = report.report_status.as_deref().unwrap_or("unknown");
    let pass = report.summary_pass.map_or_else(|| "unknown".to_string(), |value| value.to_string());
    let warn = report.summary_warn.map_or_else(|| "unknown".to_string(), |value| value.to_string());
    let fail = report.summary_fail.map_or_else(|| "unknown".to_string(), |value| value.to_string());
    out.push_str(&format!(
        "  Dogfood artifact: status={} report={} private_snapshot_coherence={} summary=pass={} warn={} fail={} path={}\n",
        report.status,
        report_status,
        report.current_private_app_snapshot_coherence,
        pass,
        warn,
        fail,
        report.path
    ));
    if let Some(generated_at) = report.generated_at.as_deref() {
        out.push_str(&format!("    artifact generated_at: {generated_at}\n"));
    }
    if let Some(generated_at_unix_ms) = report.generated_at_unix_ms {
        out.push_str(&format!("    artifact generated_at_unix_ms: {generated_at_unix_ms}\n"));
    }
    if let Some(modified_at_unix_ms) = report.artifact_modified_at_unix_ms {
        out.push_str(&format!("    artifact mtime_unix_ms: {modified_at_unix_ms}\n"));
    }
    for mismatch in report.current_private_app_snapshot_mismatches.iter().take(4) {
        out.push_str(&format!("    artifact/current mismatch: {mismatch}\n"));
    }
    if let Some(scope_status) = report.multi_terminal_scope_status.as_deref() {
        out.push_str(&format!(
            "    artifact scope objective: multi_terminal_persona_project_scope={scope_status}\n"
        ));
    }
    if let Some(status) = report.private_app_release_proof_status.as_deref() {
        let ready = report.private_app_release_proof_ready.unwrap_or(false);
        out.push_str(&format!(
            "    artifact private app release proof: status={} ready={} ready_clients={} pending_clients={}\n",
            status,
            ready,
            format_private_app_client_list(&report.private_app_release_proof_ready_clients),
            format_private_app_client_list(&report.private_app_release_proof_pending_clients)
        ));
    }
    if let Some(status) = report.real_private_app_release_operator_status.as_deref() {
        out.push_str(&format!("    artifact real-home operator: status={status}\n"));
        if let Some(action) = report.real_private_app_release_pending_actions.first() {
            let blockers = if action.release_gate_blockers.is_empty() {
                "none".to_string()
            } else {
                action.release_gate_blockers.join(",")
            };
            out.push_str(&format!(
                "      artifact next action: client={} action={} ({}) blockers={}\n",
                action.client,
                action.operator_next_action_id.as_deref().unwrap_or("unknown"),
                action.operator_next_action_label.as_deref().unwrap_or("unknown"),
                blockers
            ));
            if let Some(safety) = &action.external_action_safety {
                render_external_action_safety(out, "      ", safety);
            }
        }
        let current_command = dogfood_artifact_current_primary_command(report, &outcome.clients)
            .or_else(|| {
                dogfood_current_private_app_binding_command(report, &outcome.dogfood_index)
            });
        let current_step = dogfood_artifact_current_primary_step(report, &outcome.clients);
        let artifact_step = report.real_private_app_release_operator_primary_next_step.as_deref();
        let current_command_text = current_command.map(|command| command.join(" "));
        let artifact_command_text =
            (!report.real_private_app_release_operator_primary_next_command.is_empty())
                .then(|| report.real_private_app_release_operator_primary_next_command.join(" "));
        let mut rendered_step = current_step.map(ToOwned::to_owned);
        if rendered_step.is_none() {
            rendered_step = artifact_step.map(ToOwned::to_owned);
            if let (Some(step), Some(current), Some(artifact)) = (
                rendered_step.as_mut(),
                current_command_text.as_deref(),
                artifact_command_text.as_deref(),
            ) {
                if current != artifact && !artifact.is_empty() && step.contains(artifact) {
                    *step = step.replace(artifact, current);
                }
            }
        }
        if let Some(step) = rendered_step.as_deref() {
            out.push_str(&format!("      next: {step}\n"));
        }
        if let (Some(rendered), Some(artifact)) = (rendered_step.as_deref(), artifact_step) {
            if rendered != artifact {
                out.push_str(&format!("      artifact source next: {artifact}\n"));
            }
        }
        if let Some(command) = current_command.or_else(|| {
            (!report.real_private_app_release_operator_primary_next_command.is_empty())
                .then_some(report.real_private_app_release_operator_primary_next_command.as_slice())
        }) {
            out.push_str(&format!("      command: {}\n", command.join(" ")));
        }
        if let Some(current) = current_command {
            let artifact = report.real_private_app_release_operator_primary_next_command.as_slice();
            if !artifact.is_empty() && current != artifact {
                out.push_str(&format!("      artifact source command: {}\n", artifact.join(" ")));
            }
        }
        for action in report.real_private_app_release_pending_actions.iter().take(3) {
            let current_release_action =
                dogfood_current_release_snapshot_action_for_client(action, outcome);
            let display_action_id = current_release_action
                .map(|item| item.operator_next_action_id.as_str())
                .or(action.operator_next_action_id.as_deref())
                .unwrap_or("unknown");
            let display_action_label = current_release_action
                .map(|item| item.operator_next_action_label.as_str())
                .or(action.operator_next_action_label.as_deref())
                .unwrap_or("unknown");
            let display_blockers = current_release_action
                .map(|item| item.release_gate_blockers.as_slice())
                .unwrap_or(action.release_gate_blockers.as_slice());
            let blockers = if display_blockers.is_empty() {
                "none".to_string()
            } else {
                display_blockers.join(",")
            };
            let display_has_wait_command = current_release_action
                .map_or(action.has_wait_command, |item| item.has_wait_command);
            out.push_str(&format!(
                "      pending {}: action={} ({}) restart={} collector_start={} wait={} blockers={}\n",
                action.client,
                display_action_id,
                display_action_label,
                action.has_restart_command,
                action.has_collector_start_command,
                display_has_wait_command,
                blockers
            ));
            if Some(display_action_id) != action.operator_next_action_id.as_deref() {
                out.push_str(&format!(
                    "        artifact source action={} ({})\n",
                    action.operator_next_action_id.as_deref().unwrap_or("unknown"),
                    action.operator_next_action_label.as_deref().unwrap_or("unknown")
                ));
            }
        }
    }
    if let Some(error) = report.error.as_deref() {
        out.push_str(&format!("    artifact error: {error}\n"));
    }
}

fn dogfood_artifact_current_primary_row<'a>(
    report: &ClientDogfoodEvidenceReport,
    rows: &'a [ClientStatusRow],
) -> Option<&'a ClientStatusRow> {
    let action = report.real_private_app_release_pending_actions.first()?;
    let action_id = action.operator_next_action_id.as_deref()?;
    rows.iter().find(|row| {
        row.client == action.client && row.operator_next_action_id.as_deref() == Some(action_id)
    })
}

fn dogfood_current_release_snapshot_action_for_client<'a>(
    action: &ClientDogfoodPrivateAppSnapshotAction,
    outcome: &'a ClientStatusOutcome,
) -> Option<&'a ClientPrivateAppReleaseSnapshotAction> {
    outcome
        .private_app_release_snapshot
        .pending_actions
        .iter()
        .find(|item| item.client == action.client)
}

fn dogfood_artifact_current_primary_step<'a>(
    report: &ClientDogfoodEvidenceReport,
    rows: &'a [ClientStatusRow],
) -> Option<&'a str> {
    dogfood_artifact_current_primary_row(report, rows)
        .and_then(|row| row.operator_next_step.as_deref())
        .filter(|step| !step.trim().is_empty())
}

fn dogfood_artifact_current_primary_command<'a>(
    report: &ClientDogfoodEvidenceReport,
    rows: &'a [ClientStatusRow],
) -> Option<&'a [String]> {
    dogfood_artifact_current_primary_row(report, rows)
        .and_then(|row| row.operator_next_command.as_deref())
        .filter(|command| !command.is_empty())
}

fn dogfood_current_private_app_binding_command<'a>(
    report: &ClientDogfoodEvidenceReport,
    index: &'a ClientDogfoodIndex,
) -> Option<&'a [String]> {
    let action_client = report.real_private_app_release_pending_actions.first()?.client.as_str();
    index
        .objectives
        .iter()
        .find(|objective| objective.objective == "private_app_binding_proof")
        .map(|objective| objective.next_command.as_slice())
        .filter(|command| !command.is_empty())
        .filter(|command| command_client_arg(command) == Some(action_client))
}

fn command_client_arg(command: &[String]) -> Option<&str> {
    command.windows(2).find_map(|pair| {
        (pair.first().map(String::as_str) == Some("--client")).then(|| pair[1].as_str())
    })
}

fn render_dogfood_objective_rows_brief(out: &mut String, index: &ClientDogfoodIndex) {
    for objective in &index.objectives {
        out.push_str(&format!(
            "    {}: {} next={}\n",
            objective.objective,
            objective.status,
            dogfood_objective_next_command_brief(objective)
        ));
        if objective.status != "pass" {
            out.push_str(&format!(
                "      summary: {}\n",
                compact_dogfood_objective_summary(&objective.summary, 180)
            ));
        }
    }
}

fn dogfood_objective_next_command_brief(objective: &ClientDogfoodObjective) -> String {
    if objective.next_command.is_empty() {
        "none".to_string()
    } else {
        objective.next_command.join(" ")
    }
}

fn compact_dogfood_objective_summary(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn render_observed_capture_dogfood_brief(out: &mut String, card: &ClientOperatorCard) {
    if card.observed_capture_dogfood_clients.is_empty() {
        return;
    }
    out.push_str(&format!(
        "  Observed capture dogfood: clients={} private_release_proof=false\n",
        card.observed_capture_dogfood_clients.join(",")
    ));
    for evidence in card.observed_capture_dogfood_evidence.iter().take(5) {
        out.push_str(&format!(
            "    {}: {} project={} session={} preview={}\n",
            evidence.client,
            evidence.evidence_ref,
            evidence.project.as_deref().unwrap_or("unset"),
            evidence.session_id.as_deref().unwrap_or("unset"),
            compact_dogfood_objective_summary(&evidence.preview, 96)
        ));
    }
    out.push_str(
        "    boundary: stored capture evidence is useful dogfood, but does not replace private app-hook/render/review-action proof.\n",
    );
}

fn render_capture_dogfood_matrix_brief(out: &mut String, card: &ClientOperatorCard) {
    if card.capture_dogfood_matrix.is_empty() {
        return;
    }
    let observed_count =
        card.capture_dogfood_matrix.iter().filter(|item| item.observed_local_capture).count();
    let unobserved = card
        .capture_dogfood_matrix
        .iter()
        .filter(|item| !item.observed_local_capture)
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "  Capture dogfood matrix: observed={}/{} unobserved={}\n",
        observed_count,
        card.capture_dogfood_matrix.len(),
        unobserved.len()
    ));
    for item in unobserved.iter().take(5) {
        out.push_str(&format!(
            "    {}: status={} next={}\n",
            item.client,
            item.status,
            item.next_command.join(" ")
        ));
    }
    if unobserved.len() > 5 {
        out.push_str(&format!(
            "    ... {} more unobserved client(s); use --json for full capture_dogfood_matrix\n",
            unobserved.len() - 5
        ));
    }
    out.push_str(
        "    boundary: configured/ready client paths are not the same as stored local capture evidence.\n",
    );
}

fn render_real_cli_dogfood_probe_brief(
    out: &mut String,
    report: Option<&ClientRealCliDogfoodProbeReport>,
) {
    let Some(report) = report else {
        return;
    };
    out.push_str(&format!(
        "  Real CLI dogfood probe: status={} report={} observed={} blocked={} failed={}\n",
        report.report_status.as_deref().unwrap_or(report.status),
        report.path,
        format_private_app_client_list(&report.observed_clients),
        format_private_app_client_list(&report.blocked_clients),
        format_private_app_client_list(&report.failed_clients)
    ));
    for attempt in report.attempts.iter().take(4) {
        out.push_str(&format!(
            "    {}: status={} observed={} next={}\n",
            attempt.client,
            attempt.status,
            attempt.observed_local_capture,
            attempt.next_action.as_deref().unwrap_or("inspect probe artifact")
        ));
    }
    out.push_str(
        "    boundary: real CLI probe output is observational; it records no proof row, creates no verification event, and promotes no cloud draft.\n",
    );
}

pub fn render_brief(outcome: &ClientStatusOutcome) -> String {
    let mut out = String::new();
    out.push_str("SOMA client readiness brief\n");
    out.push_str(&format!(
        "  Status: {} - {}\n",
        outcome.operator_card.status, outcome.operator_card.headline
    ));
    out.push_str(&format!(
        "  Next action: {} ({})\n",
        outcome.operator_card.operator_next_action_id,
        outcome.operator_card.operator_next_action_label
    ));
    if !outcome.operator_card.primary_next_step.is_empty() {
        out.push_str(&format!("  Why: {}\n", outcome.operator_card.primary_next_step));
    }
    if !outcome.operator_card.primary_next_command.is_empty() {
        out.push_str(&format!(
            "  Command: {}\n",
            outcome.operator_card.primary_next_command.join(" ")
        ));
        let binary = &outcome.operator_card.binary_identity;
        out.push_str(&format!(
            "  Binary: status={} current={} path_soma={} same_fingerprint={}\n",
            binary.status,
            binary.current_exe.as_deref().unwrap_or("unknown"),
            binary.path_soma.as_deref().unwrap_or("not_found"),
            binary.same_fingerprint
        ));
        let safety = &outcome.operator_card.primary_next_command_safety;
        out.push_str(&format!(
            "  Command safety: class={} separate_terminal={} current_session_disruptive={} records_proof={}\n",
            safety.classification,
            safety.run_from_separate_terminal_required,
            safety.disrupts_current_client_session,
            safety.records_proof
        ));
    }
    if let Some(safety) = primary_external_action_safety(outcome) {
        out.push_str(&format!(
            "  External action: class={} confirmation_before_submission={} may_send_prompt_to_provider={}\n",
            safety.classification,
            safety.requires_operator_confirmation_before_submission,
            safety.may_transmit_prompt_to_provider
        ));
        out.push_str(&format!("    minimal prompt: {}\n", safety.suggested_minimal_test_prompt));
        out.push_str(&format!(
            "    forbidden inputs: {}\n",
            external_action_forbidden_inputs(safety)
        ));
        out.push_str(
            "    probe boundary: the displayed command only waits for evidence after the real client action; it does not submit the prompt or create hook evidence.\n",
        );
    }

    let guard = &outcome.operator_card.current_session_safety;
    out.push_str(&format!(
        "  Current session: detected_client={} surface={} safe={} recommended={}\n",
        guard.detected_client.as_deref().unwrap_or("unknown"),
        guard.detected_surface,
        guard.primary_command_safe_in_current_session,
        guard.recommended_execution_context
    ));
    if outcome.summary.explicit_cli_client_count > 0 {
        out.push_str(&format!(
            "  Explicit CLI real capture: observed={}/{} blocked={} failed={} unproven={} configured_available={}\n",
            outcome.summary.explicit_cli_real_capture_observed_count,
            outcome.summary.explicit_cli_client_count,
            outcome.summary.explicit_cli_real_capture_blocked_count,
            outcome.summary.explicit_cli_real_capture_failed_count,
            outcome.summary.explicit_cli_real_capture_unproven_count,
            outcome.summary.explicit_cli_capture_available_count
        ));
    }
    render_observed_capture_dogfood_brief(&mut out, &outcome.operator_card);
    render_capture_dogfood_matrix_brief(&mut out, &outcome.operator_card);
    render_real_cli_dogfood_probe_brief(&mut out, outcome.real_cli_dogfood_probe.as_ref());

    let release = &outcome.private_app_release_snapshot;
    out.push_str(&format!(
        "  Private apps: status={} ready={} ready_clients={} pending_clients={} primary_pending={}\n",
        release.status,
        release.ready,
        format_private_app_client_list(&release.ready_clients),
        format_private_app_client_list(&release.pending_clients),
        release.primary_pending_client.as_deref().unwrap_or("none")
    ));
    if !release.primary_release_gate_blockers.is_empty() {
        out.push_str(&format!(
            "    blockers: {}\n",
            release.primary_release_gate_blockers.join(",")
        ));
    }
    if !release.primary_missing_proof_levels.is_empty() {
        out.push_str(&format!(
            "    missing proof: {}\n",
            release.primary_missing_proof_levels.join(",")
        ));
    }
    if let Some(step) = release.primary_next_proof_step_id.as_deref() {
        out.push_str(&format!("    proof step: {step}\n"));
    }
    render_client_binding_matrix_brief(&mut out, &outcome.client_binding);

    for action in outcome
        .operator_card
        .private_app_next_actions
        .iter()
        .filter(|action| !action.ready_for_private_client_claim)
        .take(3)
    {
        out.push_str(&format!(
            "  {}: status={} next={} action={} event={} safe_in_current_session={}\n",
            action.client,
            action.goal_status,
            action.proof_session_next_step_id.as_deref().unwrap_or("none"),
            action.operator_next_action_id,
            action.private_event_observation_status.as_deref().unwrap_or("unknown"),
            action.current_session_action_safety.action_safe_in_current_session
        ));
        if let Some(wait) = outcome
            .operator_card
            .private_app_wait_commands
            .iter()
            .find(|wait| wait.client == action.client)
        {
            let command = wait.simple_wait_command.as_ref().unwrap_or(&wait.wait_command);
            out.push_str(&format!("    wait: {}\n", command.join(" ")));
        }
        if let Some(row) = outcome.clients.iter().find(|row| row.client == action.client) {
            render_runtime_launch_probe_lines(&mut out, row, "    ");
            out.push_str(&format!(
                "    proof session brief: {}\n",
                private_app_proof_session_brief_command(row).join(" ")
            ));
            render_artifact_repair_plan_brief(
                &mut out,
                row.artifact_repair_plan.as_ref(),
                row.artifact_repair_summary.as_ref(),
            );
        } else {
            out.push_str(&format!(
                "    proof session brief: {}\n",
                private_app_proof_session_brief_command_for_client(&action.client).join(" ")
            ));
        }
        if let Some(safety) = &action.external_action_safety {
            out.push_str(&format!(
                "    external action requires confirmation={} may_send_prompt_to_provider={} minimal_prompt=\"{}\"\n",
                safety.requires_operator_confirmation_before_submission,
                safety.may_transmit_prompt_to_provider,
                safety.suggested_minimal_test_prompt
            ));
            out.push_str(
                "    probe boundary: the wait command observes evidence only; the real client action must happen separately after confirmation.\n",
            );
        }
        if let Some(step) = brief_after_success_proof_step(outcome, action) {
            out.push_str(&format!("    after success proof: {}\n", step.id));
            if let Some(command) = &step.command {
                out.push_str(&format!("    after success proof command: {}\n", command.join(" ")));
            }
            if let Some(tool) = &step.mcp_tool {
                out.push_str(&format!(
                    "    after success proof MCP: {} {}\n",
                    tool,
                    step.mcp_arguments_json.as_deref().unwrap_or("{}")
                ));
            }
            out.push_str(&format!(
                "    after success proof boundary: {}\n",
                step.proof_step_trust_boundary
            ));
        }
        if let Some(observation) = brief_private_event_observation(outcome, action) {
            if let Some(line) = brief_private_event_observation_line(&action.client, observation) {
                out.push_str(&format!("    {line}\n"));
            }
        }
        if action.client == "continue" {
            out.push_str(
                "    UI target: Continue extension chat/edit/review; Cursor Agent/Composer does not satisfy Continue proof.\n",
            );
        }
    }

    if let Some(scope) = outcome.project_scope.as_ref() {
        out.push_str(&format!(
            "  Project scope: status={} persona={} ready_for_scoped_capture={} storage_write_status={} storage_write_ready={} current_client={} current_project={} current_session={}\n",
            scope.status,
            scope.active_persona,
            scope.ready_for_project_scoped_capture,
            scope.storage_write_status,
            scope.storage_write_ready,
            scope.current_client.as_deref().unwrap_or("unset"),
            scope.current_project.as_deref().unwrap_or("unset"),
            scope.current_session_id.as_deref().unwrap_or("unset")
        ));
        if !scope.missing_scope_envs.is_empty() {
            out.push_str(&format!("    missing env: {}\n", scope.missing_scope_envs.join(",")));
        }
        for command in &scope.suggested_persona_call_commands {
            out.push_str(&format!("    scope call: {}\n", command.join(" ")));
        }
        for command in scope
            .next_commands
            .iter()
            .filter(|command| !scope.suggested_persona_call_commands.contains(command))
            .take(3)
        {
            out.push_str(&format!("    scope command: {}\n", command.join(" ")));
        }
    }

    render_dogfood_artifact_checkpoint(&mut out, outcome);
    out.push_str(&format!(
        "  Dogfood objective index: status={} pass={} warn={} fail={}\n",
        outcome.dogfood_index.status,
        outcome.dogfood_index.pass_count,
        outcome.dogfood_index.warning_count,
        outcome.dogfood_index.fail_count
    ));
    render_dogfood_objective_rows_brief(&mut out, &outcome.dogfood_index);
    out.push_str(&format!(
        "  Semantic review: status={} cloud_draft_blockers={} l4_candidates={} review_only={} belief_candidates={}\n",
        outcome.semantic_review.status,
        outcome.semantic_review.cloud_draft_blocked_count,
        outcome.semantic_review.l4_candidate_count,
        outcome.semantic_review.review_only_candidate_count,
        outcome.semantic_review.belief_candidate_count
    ));
    let semantic_workload = &outcome.semantic_review.workload_summary;
    out.push_str(&format!(
        "    workload: scope={} project={} queue_pending={} durable_blocking={} cloud_draft={} l4_ready={} manual_l4={} l4_blocking={} l2_audit_only={} operator_attention={}\n",
        semantic_workload.scope_source,
        semantic_workload.project.as_deref().unwrap_or("all"),
        semantic_workload.review_queue_pending_count,
        semantic_workload.durable_learning_blocking_count,
        semantic_workload.cloud_draft_blocker_count,
        semantic_workload.l4_review_candidate_count,
        semantic_workload.manual_l4_review_count,
        semantic_workload.l4_promotion_blocking_count,
        semantic_workload.l2_audit_only_count,
        semantic_workload.operator_attention_count
    ));
    if !outcome.semantic_review.review_lanes.is_empty() {
        out.push_str("    semantic lanes:\n");
        for lane in &outcome.semantic_review.review_lanes {
            out.push_str(&format!(
                "      P{} {} status={} count={}\n",
                lane.priority, lane.lane, lane.status, lane.count
            ));
            if !lane.command.is_empty() {
                out.push_str(&format!("        command: {}\n", lane.command.join(" ")));
            }
        }
    }
    if !outcome.semantic_review.semantic_resolution_actions.is_empty() {
        out.push_str("    semantic resolution actions:\n");
        for action in outcome.semantic_review.semantic_resolution_actions.iter().take(4) {
            out.push_str(&format!(
                "      {} control_id={} evidence_required={} cli={}\n",
                action.action,
                action.control_id,
                action.requires_evidence,
                action.cli_command.join(" ")
            ));
        }
    }
    if outcome.semantic_review.status != "clear" {
        out.push_str(&format!("    next: {}\n", outcome.semantic_review.next_step));
        out.push_str(&format!(
            "    review workload: {}\n",
            outcome.semantic_review.workload_command.join(" ")
        ));
        let review_label = if outcome.semantic_review.primary_surface == "semantic_proposals" {
            "candidate drilldown"
        } else {
            "review"
        };
        out.push_str(&format!(
            "    {review_label}: {}\n",
            semantic_review_primary_command(&outcome.semantic_review).join(" ")
        ));
    }
    out.push_str(
        "  Trust boundary: read-only status only; no proof, verification, promotion, hook install, or provider prompt submission.\n",
    );
    out
}

fn render_client_binding_matrix_brief(out: &mut String, index: &ClientBindingReadinessIndex) {
    if index.required_client_proof_matrix.is_empty() {
        return;
    }
    out.push_str(&format!(
        "  Client binding proof matrix: ready={} proof_storage={} rows={}\n",
        index.ready,
        index.proof_storage_status,
        index.required_client_proof_matrix.len()
    ));
    for checklist in &index.required_client_proof_matrix {
        let missing = if checklist.missing_proof_levels.is_empty() {
            "none".to_string()
        } else {
            checklist.missing_proof_levels.join(",")
        };
        out.push_str(&format!(
            "    binding {}: status={} next_step={} next_required={} missing={} blockers={}\n",
            checklist.client,
            checklist.status,
            checklist.next_proof_step_id.as_deref().unwrap_or("none"),
            checklist.next_required_proof_level.as_deref().unwrap_or("none"),
            missing,
            format_private_app_client_list(&checklist.release_gate_blockers)
        ));
        if !checklist.next_command.is_empty() {
            out.push_str(&format!("      next_command: {}\n", checklist.next_command.join(" ")));
        }
    }
}

fn render_artifact_repair_plan_brief(
    out: &mut String,
    plan: Option<&ClientArtifactRepairPlan>,
    summary: Option<&ClientArtifactRepairSummary>,
) {
    let Some(plan) = plan else {
        return;
    };
    out.push_str(&format!(
        "    artifact repair: status={} failures={} boundary={}\n",
        plan.status, plan.failure_count, plan.trust_boundary
    ));
    out.push_str(&format!("      durable artifact dir: {}\n", plan.suggested_artifact_dir));
    out.push_str(&format!(
        "      durable artifact dir write: {}\n",
        plan.suggested_artifact_dir_write_status
    ));
    for suggestion in plan.suggested_artifact_paths.iter().take(4) {
        out.push_str(&format!(
            "      suggested artifact: kind={} path={} intent={}\n",
            suggestion.artifact_kind, suggestion.path, suggestion.intent
        ));
    }
    if let Some(dir) = plan.workspace_fallback_artifact_dir.as_deref() {
        out.push_str(&format!("      workspace fallback artifact dir: {dir}\n"));
        for suggestion in plan.workspace_fallback_artifact_paths.iter().take(4) {
            out.push_str(&format!(
                "      workspace fallback artifact: kind={} path={} intent={}\n",
                suggestion.artifact_kind, suggestion.path, suggestion.intent
            ));
        }
        for command in plan.workspace_fallback_commands.iter().take(2) {
            out.push_str(&format!("      workspace fallback: {}\n", command.join(" ")));
        }
    }
    for failure in plan.failed_artifacts.iter().take(3) {
        out.push_str(&format!(
            "      failed artifact: proof_id={} level={} kind={} status={} path={}\n",
            failure.proof_id,
            failure.proof_level,
            failure.artifact_kind,
            failure.status,
            failure.path.as_deref().unwrap_or("none")
        ));
        out.push_str(&format!("        recover: {}\n", failure.recovery_action));
    }
    for command in plan.diagnostic_commands.iter().take(3) {
        out.push_str(&format!("      diagnose: {}\n", command.join(" ")));
    }
    render_artifact_repair_summary_guidance(out, summary, "      ");
    for step in plan.recovery_steps.iter().take(8) {
        out.push_str(&format!("      step: {step}\n"));
    }
    for claim in plan.blocked_claims.iter().take(2) {
        out.push_str(&format!("      blocked: {claim}\n"));
    }
}

fn render_artifact_repair_summary_guidance(
    out: &mut String,
    summary: Option<&ClientArtifactRepairSummary>,
    indent: &str,
) {
    let Some(summary) = summary else {
        return;
    };
    for item in summary.operator_checklist.iter().take(4) {
        out.push_str(&format!("{indent}check: {item}\n"));
    }
    for field in summary.required_observation_fields.iter().take(8) {
        out.push_str(&format!("{indent}required observation: {field}\n"));
    }
    for precondition in summary.proof_recording_preconditions.iter().take(8) {
        out.push_str(&format!("{indent}proof precondition: {precondition}\n"));
    }
    if let Some(scan) = summary.render_evidence_artifact_scan.as_ref() {
        out.push_str(&format!(
            "{indent}render evidence scan: status={} path={} placeholders={} records_proof={} promotes_cloud_draft={}\n",
            scan.status,
            scan.path.as_deref().unwrap_or("none"),
            scan.placeholder_count,
            scan.records_proof,
            scan.promotes_cloud_draft
        ));
        for requirement in scan.missing_requirements.iter().take(8) {
            out.push_str(&format!("{indent}render evidence missing: {requirement}\n"));
        }
    }
    if let Some(packet) = summary.render_proof_packet_scan.as_ref() {
        out.push_str(&format!(
            "{indent}render packet: status={} artifact_dir={} evidence={} placeholders={} records_proof={} promotes_cloud_draft={}\n",
            packet.status,
            packet.artifact_dir.as_deref().unwrap_or("none"),
            packet.render_evidence_path.as_deref().unwrap_or("none"),
            packet.placeholder_count,
            packet.records_proof,
            packet.promotes_cloud_draft
        ));
        out.push_str(&format!(
            "{indent}render packet view: markdown={} html={} json={}\n",
            packet.review_render_markdown_path.as_deref().unwrap_or("none"),
            packet.review_render_html_path.as_deref().unwrap_or("none"),
            packet.review_render_json_path.as_deref().unwrap_or("none")
        ));
        out.push_str(&format!("{indent}render packet next: {}\n", packet.next_step));
    }
    for shortcut in summary.forbidden_shortcuts.iter().take(4) {
        out.push_str(&format!("{indent}forbidden shortcut: {shortcut}\n"));
    }
}

fn brief_after_success_proof_step<'a>(
    outcome: &'a ClientStatusOutcome,
    action: &ClientPrivateAppNextAction,
) -> Option<&'a ClientProofSessionRunbookStepSummary> {
    let external_action = action.external_action.as_ref()?;
    let step_id = external_action.proof_after_success_step_id.as_str();
    outcome.clients.iter().find(|row| row.client == action.client).and_then(|row| {
        row.proof_session_runbook_steps.iter().find(|step| step.id == step_id && step.records_proof)
    })
}

fn brief_private_event_observation<'a>(
    outcome: &'a ClientStatusOutcome,
    action: &ClientPrivateAppNextAction,
) -> Option<&'a ClientPrivateEventObservation> {
    outcome
        .clients
        .iter()
        .find(|row| row.client == action.client)
        .and_then(|row| row.private_event_observation.as_ref())
}

fn brief_private_event_observation_line(
    expected_client: &str,
    observation: &ClientPrivateEventObservation,
) -> Option<String> {
    let event = observation.relevant_event.as_ref()?;
    let event_client = event.client.as_deref().unwrap_or("unknown");
    let event_source = event.event_source.as_deref().unwrap_or("unknown");
    let binding_nonce = event.binding_nonce.as_deref().unwrap_or("unknown");
    let hook_adapter = event.hook_adapter.as_deref().unwrap_or("unknown");
    let mismatches = if observation.latest_spool_mismatches.is_empty() {
        "none".to_string()
    } else {
        observation.latest_spool_mismatches.join(",")
    };
    let prefix = if observation.status == "matching_private_binding_nonce_seen"
        && observation.latest_spool_mismatches.is_empty()
    {
        "recent event"
    } else {
        "recent event mismatch"
    };
    let proof_note = if prefix == "recent event mismatch" {
        format!("; not proof for {expected_client}")
    } else {
        String::new()
    };
    Some(format!(
        "{prefix}: client={event_client} event_source={event_source} binding_nonce={binding_nonce} hook_adapter={hook_adapter} status={} mismatches={mismatches}{proof_note}",
        observation.status
    ))
}

pub fn render_text(outcome: &ClientStatusOutcome) -> String {
    let mut out = String::new();
    out.push_str("SOMA client readiness\n");
    out.push_str(&format!(
        "  Status: {} - {}\n",
        outcome.operator_card.status, outcome.operator_card.headline
    ));
    out.push_str(&format!(
        "  Operator action: {} ({})\n",
        outcome.operator_card.operator_next_action_id,
        outcome.operator_card.operator_next_action_label
    ));
    out.push_str(&format!("  Primary next: {}\n", outcome.operator_card.primary_next_step));
    if !outcome.operator_card.primary_next_command.is_empty() {
        out.push_str(&format!(
            "  Primary command: {}\n",
            outcome.operator_card.primary_next_command.join(" ")
        ));
        let safety = &outcome.operator_card.primary_next_command_safety;
        out.push_str(&format!(
            "  Primary command safety: class={} separate_terminal_required={} disrupts_current_client_session={} operator_confirmation={} writes_local_files={} records_proof={}\n",
            safety.classification,
            safety.run_from_separate_terminal_required,
            safety.disrupts_current_client_session,
            safety.requires_operator_confirmation,
            safety.writes_local_files,
            safety.records_proof
        ));
        if safety.run_from_separate_terminal_required
            || safety.disrupts_current_client_session
            || safety.records_proof
            || safety.writes_local_files
        {
            out.push_str(&format!("    safety reason: {}\n", safety.reason));
        }
    }
    if let Some(safety) = primary_external_action_safety(outcome) {
        render_primary_external_action_safety(&mut out, safety);
    }
    let current_session_safety = &outcome.operator_card.current_session_safety;
    out.push_str(&format!(
        "  Current session guard: detected_client={} surface={} targets_current_session={} safe_in_current_session={} recommended_execution={}\n",
        current_session_safety.detected_client.as_deref().unwrap_or("unknown"),
        current_session_safety.detected_surface,
        current_session_safety.primary_command_targets_current_session,
        current_session_safety.primary_command_safe_in_current_session,
        current_session_safety.recommended_execution_context
    ));
    if !current_session_safety.primary_command_safe_in_current_session {
        out.push_str(&format!("    current-session reason: {}\n", current_session_safety.reason));
    }
    out.push_str(&format!(
        "  Strict hardening: {}\n",
        outcome.operator_card.strict_private_client_hardening_command.join(" ")
    ));
    out.push_str(
        "    TaskFrame projection hardening: add --task-frame-id <id> --require-task-frame-projection for cloud-facing release checks\n",
    );
    out.push_str(&format!(
        "  MCP ready: {}/{}  explicit CLI capture: {}/{}  private app proof ready: {}/{}  binding proofs seen: {}\n",
        outcome.summary.mcp_registration_ready_count,
        outcome.summary.client_count,
        outcome.summary.explicit_cli_capture_available_count,
        outcome.summary.explicit_cli_client_count,
        outcome.summary.private_app_capture_ready_count,
        outcome.summary.private_app_client_count,
        outcome.summary.client_binding_rows_seen
    ));
    render_observed_capture_dogfood_brief(&mut out, &outcome.operator_card);
    render_capture_dogfood_matrix_brief(&mut out, &outcome.operator_card);
    render_real_cli_dogfood_probe_brief(&mut out, outcome.real_cli_dogfood_probe.as_ref());
    let release = &outcome.private_app_release_snapshot;
    out.push_str(&format!(
        "  Private app release snapshot: scope={} client_filter={} status={} ready={} ready_clients={} pending_clients={} primary_pending={}\n",
        release.scope,
        release.client_filter.as_deref().unwrap_or("all"),
        release.status,
        release.ready,
        format_private_app_client_list(&release.ready_clients),
        format_private_app_client_list(&release.pending_clients),
        release.primary_pending_client.as_deref().unwrap_or("none")
    ));
    if !release.primary_release_gate_blockers.is_empty() {
        out.push_str(&format!(
            "    primary blockers: {} next_action={} ({})\n",
            release.primary_release_gate_blockers.join(","),
            release.operator_next_action_id,
            release.operator_next_action_label
        ));
    }
    render_private_app_proof_session_checkpoint(&mut out, outcome);
    render_project_scope_checkpoint(&mut out, outcome);
    render_dogfood_artifact_checkpoint(&mut out, outcome);
    out.push_str(&format!(
        "  Dogfood objective index: status={} pass={} warn={} fail={}\n",
        outcome.dogfood_index.status,
        outcome.dogfood_index.pass_count,
        outcome.dogfood_index.warning_count,
        outcome.dogfood_index.fail_count
    ));
    for objective in &outcome.dogfood_index.objectives {
        out.push_str(&format!(
            "    {}: {} - {}\n",
            objective.objective, objective.status, objective.summary
        ));
    }
    if !outcome.operator_card.runtime_missing_clients.is_empty() {
        out.push_str(&format!(
            "  Runtime missing: {}\n",
            outcome.operator_card.runtime_missing_clients.join(", ")
        ));
    }
    if !outcome.operator_card.runtime_not_cli_detectable_clients.is_empty() {
        out.push_str(&format!(
            "  Runtime manual check: {}\n",
            outcome.operator_card.runtime_not_cli_detectable_clients.join(", ")
        ));
    }
    if !outcome.operator_card.private_app_restart_recommended_clients.is_empty() {
        out.push_str(&format!(
            "  Private app restart recommended: {}\n",
            outcome.operator_card.private_app_restart_recommended_clients.join(", ")
        ));
    }
    if !outcome.operator_card.private_app_restart_commands.is_empty() {
        out.push_str("  Private app restart commands:\n");
        for command in &outcome.operator_card.private_app_restart_commands {
            out.push_str(&format!(
                "    {}: action={} restart={} manual_restart_required={} separate_terminal_required={} disrupts_current_client_session={} event_source={} nonce={} event_jsonl={}\n",
                command.client,
                command.operator_next_action_id,
                command.restart_recommended,
                command.manual_restart_required,
                command.execution_safety.run_from_separate_terminal_required,
                command.execution_safety.disrupts_current_client_session,
                command.expected_event_source.as_deref().unwrap_or("unknown"),
                command.binding_nonce.as_deref().unwrap_or("unknown"),
                command.event_jsonl_path.as_deref().unwrap_or("unknown")
            ));
            out.push_str(&format!("      quit: {}\n", command.quit_command.join(" ")));
            out.push_str(&format!("      reopen: {}\n", command.reopen_command.join(" ")));
            if let Some(wait) = &command.follow_up_wait_command {
                out.push_str(&format!("      follow-up wait: {}\n", wait.join(" ")));
            }
            if let Some(wait) = &command.simple_follow_up_wait_command {
                out.push_str(&format!("      simple follow-up wait: {}\n", wait.join(" ")));
            }
            out.push_str(&format!("      after restart: {}\n", command.instruction));
        }
    }
    if !outcome.operator_card.continue_extension_config_not_visible_clients.is_empty() {
        out.push_str(&format!(
            "  Continue config attention: {}\n",
            outcome.operator_card.continue_extension_config_not_visible_clients.join(", ")
        ));
    }
    if !outcome.operator_card.private_app_hook_trigger_ready_clients.is_empty() {
        out.push_str(&format!(
            "  Private app hook trigger ready (not proof): {}\n",
            outcome.operator_card.private_app_hook_trigger_ready_clients.join(", ")
        ));
    }
    if !outcome.operator_card.private_app_observed_app_hook_recordable_clients.is_empty() {
        out.push_str(&format!(
            "  Observed app-hook proof recordable: {}\n",
            outcome.operator_card.private_app_observed_app_hook_recordable_clients.join(", ")
        ));
    }
    if !outcome.operator_card.private_app_next_actions.is_empty() {
        out.push_str("  Private app action matrix:\n");
        for action in &outcome.operator_card.private_app_next_actions {
            let next = action.proof_session_next_step_id.as_deref().unwrap_or("none");
            let event = action.private_event_observation_status.as_deref().unwrap_or("unknown");
            let continue_config =
                action.continue_extension_config_status.as_deref().unwrap_or("n/a");
            let continue_collector =
                action.continue_devdata_collector_status.as_deref().unwrap_or("n/a");
            let blockers = if action.release_gate_blockers.is_empty() {
                "none".to_string()
            } else {
                action.release_gate_blockers.join(",")
            };
            out.push_str(&format!(
                "    {}: status={} ready={} next={} operator_action={} ({}) blockers={} restart={} manual_restart_required={} action_targets_current_session={} action_safe_in_current_session={} action_recommended_execution={} quit_hint={} reopen_hint={} event={} continue_config={} continue_collector={} collector_listening={} collector_start_required={}\n",
                action.client,
                action.goal_status,
                action.ready_for_private_client_claim,
                next,
                action.operator_next_action_id,
                action.operator_next_action_label,
                blockers,
                action.restart_recommended,
                action.manual_restart_required,
                action.current_session_action_safety.action_targets_current_session,
                action.current_session_action_safety.action_safe_in_current_session,
                action.current_session_action_safety.recommended_execution_context,
                action
                    .quit_hint_command
                    .as_ref()
                    .map(|command| command.join(" "))
                    .unwrap_or_else(|| "none".to_string()),
                action
                    .reopen_hint_command
                    .as_ref()
                    .map(|command| command.join(" "))
                    .unwrap_or_else(|| "none".to_string()),
                event,
                continue_config,
                continue_collector,
                action
                    .continue_devdata_collector_listening
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                action
                    .continue_devdata_collector_start_required
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            ));
            if let Some(command) = &action.continue_devdata_collector_start_command {
                out.push_str(&format!("      continue collector start: {}\n", command.join(" ")));
            }
            if let Some(command) = &action.continue_devdata_collector_managed_start_command {
                out.push_str(&format!(
                    "      continue collector managed start: {}\n",
                    command.join(" ")
                ));
            }
            if let Some(safety) = &action.external_action_safety {
                render_external_action_safety(&mut out, "      ", safety);
            }
        }
    }
    if !outcome.operator_card.private_app_collector_start_commands.is_empty() {
        out.push_str("  Private app collector start commands:\n");
        for command in &outcome.operator_card.private_app_collector_start_commands {
            out.push_str(&format!(
                "    {}: action={} status={} listening={} devdata_destination_visible={} event_source={} nonce={} event_jsonl={}\n",
                command.client,
                command.operator_next_action_id,
                command.collector_status,
                command.collector_listening,
                command.devdata_destination_visible,
                command.expected_event_source.as_deref().unwrap_or("unknown"),
                command.binding_nonce.as_deref().unwrap_or("unknown"),
                command.event_jsonl_path.as_deref().unwrap_or("unknown")
            ));
            out.push_str(&format!("      start: {}\n", command.start_command.join(" ")));
            out.push_str(&format!(
                "      managed start: {}\n",
                command.managed_start_command.join(" ")
            ));
            if let Some(wait) = &command.follow_up_wait_command {
                out.push_str(&format!("      follow-up wait: {}\n", wait.join(" ")));
            }
            if let Some(wait) = &command.simple_follow_up_wait_command {
                out.push_str(&format!("      simple follow-up wait: {}\n", wait.join(" ")));
            }
            out.push_str(&format!("      after start: {}\n", command.instruction));
        }
    }
    if !outcome.operator_card.private_app_wait_commands.is_empty() {
        out.push_str("  Private app hook wait commands:\n");
        for wait in &outcome.operator_card.private_app_wait_commands {
            out.push_str(&format!(
                "    {}: action={} restart={} manual_restart_required={} quit_hint={} reopen_hint={} event_source={} nonce={} event_jsonl={}\n",
                wait.client,
                wait.operator_next_action_id,
                wait.restart_recommended,
                wait.manual_restart_required,
                wait.quit_hint_command
                    .as_ref()
                    .map(|command| command.join(" "))
                    .unwrap_or_else(|| "none".to_string()),
                wait
                    .reopen_hint_command
                    .as_ref()
                    .map(|command| command.join(" "))
                    .unwrap_or_else(|| "none".to_string()),
                wait.expected_event_source.as_deref().unwrap_or("unknown"),
                wait.binding_nonce.as_deref().unwrap_or("unknown"),
                wait.event_jsonl_path.as_deref().unwrap_or("unknown")
            ));
            out.push_str(&format!("      wait: {}\n", wait.wait_command.join(" ")));
            if let Some(command) = &wait.simple_wait_command {
                out.push_str(&format!("      simple wait: {}\n", command.join(" ")));
            }
            if let Some(command) = &wait.watch_command {
                out.push_str(&format!("      watch: {}\n", command.join(" ")));
            }
            if let Some(safety) = &wait.external_action_safety {
                render_external_action_safety(&mut out, "      ", safety);
            }
        }
    }
    if !outcome.operator_card.private_app_hook_integration_templates.is_empty() {
        out.push_str("  Private app hook integration templates:\n");
        for template in &outcome.operator_card.private_app_hook_integration_templates {
            out.push_str(&format!(
                "    {}: wrapper={} event_source={} nonce={} policy={}\n",
                template.client,
                template.wrapper,
                template
                    .environment
                    .get("SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE")
                    .map(String::as_str)
                    .unwrap_or("unknown"),
                template
                    .environment
                    .get("SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE")
                    .map(String::as_str)
                    .unwrap_or("unknown"),
                template.manual_invocation_policy
            ));
            out.push_str(&format!(
                "      command template: {}\n",
                template.wrapper_command_template.join(" ")
            ));
            out.push_str(&format!(
                "      stdin template: {}\n",
                template.stdin_event_template_json
            ));
        }
    }
    if !outcome.operator_card.private_app_release_plan.is_empty() {
        out.push_str("  Private app release plan:\n");
        for item in &outcome.operator_card.private_app_release_plan {
            let completed = if item.completed_proof_levels.is_empty() {
                "none".to_string()
            } else {
                item.completed_proof_levels.join(",")
            };
            let missing = if item.missing_proof_levels.is_empty() {
                "none".to_string()
            } else {
                item.missing_proof_levels.join(",")
            };
            let blockers = if item.release_gate_blockers.is_empty() {
                "none".to_string()
            } else {
                item.release_gate_blockers.join(",")
            };
            let proof_status = format_private_app_proof_level_statuses(&item.proof_level_statuses);
            out.push_str(&format!(
                "    {}: status={} stage={} external_action={} recordable={} next_action={} ({}) proof_status={} completed={} missing={} next_level={} blockers={} next={}\n",
                item.client,
                item.status,
                item.current_stage,
                item.requires_external_client_action,
                item.ready_to_record_now,
                item.operator_next_action_id,
                item.operator_next_action_label,
                proof_status,
                completed,
                missing,
                item.next_required_proof_level.as_deref().unwrap_or("none"),
                blockers,
                item.next_command.join(" ")
            ));
            if let Some(safety) = &item.external_action_safety {
                render_external_action_safety(&mut out, "      ", safety);
            }
        }
    }
    if !outcome.operator_card.private_app_release_proof_checklist.is_empty() {
        out.push_str("  Private app release proof checklist:\n");
        for checklist in &outcome.operator_card.private_app_release_proof_checklist {
            let missing = if checklist.missing_proof_levels.is_empty() {
                "none".to_string()
            } else {
                checklist.missing_proof_levels.join(",")
            };
            let blockers = if checklist.release_gate_blockers.is_empty() {
                "none".to_string()
            } else {
                checklist.release_gate_blockers.join(",")
            };
            let ready_to_record = if checklist.ready_to_record_proof_levels.is_empty() {
                "none".to_string()
            } else {
                checklist.ready_to_record_proof_levels.join(",")
            };
            let next_required_proof_level =
                checklist.next_required_proof_level.as_deref().unwrap_or("none");
            let next_proof_step_id = checklist.next_proof_step_id.as_deref().unwrap_or("none");
            out.push_str(&format!(
                "    {}: status={} ready={} ready_to_record={} next_level={} next_step={} missing={} blockers={} proof_session={}\n",
                checklist.client,
                checklist.status,
                checklist.ready_for_private_client_claim,
                ready_to_record,
                next_required_proof_level,
                next_proof_step_id,
                missing,
                blockers,
                checklist.proof_session_command.join(" ")
            ));
            out.push_str(&format!(
                "      release runbook: {}\n",
                checklist.release_runbook_command.join(" ")
            ));
            if let Some(command) = &checklist.simple_release_runbook_command {
                out.push_str(&format!("      simple release runbook: {}\n", command.join(" ")));
            }
            if let Some(command) = &checklist.target_install_command {
                out.push_str(&format!("      target install: {}\n", command.join(" ")));
            }
            if let Some(command) = &checklist.hook_readiness_command {
                out.push_str(&format!("      hook readiness: {}\n", command.join(" ")));
            }
            if let Some(command) = &checklist.simple_hook_readiness_command {
                out.push_str(&format!("      simple hook readiness: {}\n", command.join(" ")));
            }
            if !checklist.next_command.is_empty() {
                out.push_str("      next command: ");
                out.push_str(&checklist.next_command.join(" "));
                if let Some(note) = next_command_annotation(outcome, &checklist.next_command) {
                    out.push_str("  # ");
                    out.push_str(note);
                }
                out.push('\n');
            }
            render_private_app_proof_ladder(&mut out, checklist);
            render_private_app_recording_hint(&mut out, checklist);
        }
    }
    if outcome.proof_storage_status != "available" {
        out.push_str(&format!("  Proof storage: {}\n", outcome.proof_storage_status));
        if let Some(error) = &outcome.proof_storage_error {
            out.push_str(&format!("    error: {error}\n"));
        }
        for command in &outcome.operator_card.proof_storage_recovery_commands {
            out.push_str(&format!("    recovery: {}\n", command.join(" ")));
        }
    }
    if outcome.summary.private_app_client_count > 0 {
        out.push_str(&format!(
            "  Private app next: trigger_hook={} hook_trigger_ready={} record_app_hook={}\n",
            outcome.summary.private_app_trigger_hook_next_count,
            outcome.summary.private_app_hook_trigger_ready_count,
            outcome.summary.private_app_record_app_hook_next_count
        ));
        out.push_str(&format!(
            "  Private proof levels: app_hook={}/{} render={}/{} review_action={}/{}\n",
            outcome.summary.private_app_app_hook_proven_count,
            outcome.summary.private_app_client_count,
            outcome.summary.private_app_in_client_render_proven_count,
            outcome.summary.private_app_client_count,
            outcome.summary.private_app_review_action_proven_count,
            outcome.summary.private_app_client_count
        ));
    }
    render_semantic_learning_checkpoint(&mut out, &outcome.semantic_review);
    out.push_str(&format!(
        "  Semantic review: status={} client={} surface={} blocked_cloud_drafts={} l4_candidates={} review_only_candidates={} belief_candidates={} belief_groups={} hidden_duplicates={} contradiction_signals={} substantive={} low_value_conflicts={} low_value_noise={}\n",
        outcome.semantic_review.status,
        outcome.semantic_review.client,
        outcome.semantic_review.primary_surface,
        outcome.semantic_review.cloud_draft_blocked_count,
        outcome.semantic_review.l4_candidate_count,
        outcome.semantic_review.review_only_candidate_count,
        outcome.semantic_review.belief_candidate_count,
        outcome.semantic_review.belief_group_count,
        outcome.semantic_review.belief_hidden_duplicate_count,
        outcome.semantic_review.belief_contradiction_count,
        outcome.semantic_review.belief_substantive_contradiction_count,
        outcome.semantic_review.belief_low_value_conflict_count,
        outcome.semantic_review.belief_low_value_noise_count
    ));
    let semantic_workload = &outcome.semantic_review.workload_summary;
    out.push_str(&format!(
        "  Semantic workload: scope={} project={} queue_pending={} durable_blocking={} cloud_draft={} l4_ready={} manual_l4={} l4_blocking={} l2_audit_only={} operator_attention={} bucket={}\n",
        semantic_workload.scope_source,
        semantic_workload.project.as_deref().unwrap_or("all"),
        semantic_workload.review_queue_pending_count,
        semantic_workload.durable_learning_blocking_count,
        semantic_workload.cloud_draft_blocker_count,
        semantic_workload.l4_review_candidate_count,
        semantic_workload.manual_l4_review_count,
        semantic_workload.l4_promotion_blocking_count,
        semantic_workload.l2_audit_only_count,
        semantic_workload.operator_attention_count,
        semantic_workload.primary_operator_bucket
    ));
    out.push_str(&format!(
        "  Semantic belief workload: status={} substantive_groups={} substantive_candidates={} noise_candidates={} primary_group={}\n",
        outcome.semantic_review.belief_review_summary.status,
        outcome
            .semantic_review
            .belief_review_summary
            .substantive_contradiction_group_count,
        outcome
            .semantic_review
            .belief_review_summary
            .substantive_contradiction_candidate_count,
        outcome.semantic_review.belief_review_summary.noise_candidate_count,
        outcome
            .semantic_review
            .belief_review_summary
            .primary_group_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));
    if !outcome.semantic_review.review_lanes.is_empty() {
        out.push_str("    semantic lanes:\n");
        for lane in &outcome.semantic_review.review_lanes {
            out.push_str(&format!(
                "      P{} {} status={} count={}\n",
                lane.priority, lane.lane, lane.status, lane.count
            ));
            if !lane.command.is_empty() {
                out.push_str(&format!("        command: {}\n", lane.command.join(" ")));
            }
        }
    }
    if !outcome.semantic_review.review_cards.is_empty() {
        out.push_str("    semantic cards:\n");
        for card in outcome.semantic_review.review_cards.iter().take(3) {
            out.push_str(&format!(
                "      P{} {} target={} status={} blocks_l4={}\n",
                card.priority, card.card_id, card.target, card.status, card.blocks_l4_promotion
            ));
        }
    }
    if let Some(error) = &outcome.semantic_review.error {
        out.push_str(&format!("    semantic review error: {error}\n"));
    } else if outcome.semantic_review.status == "noise_triage_only" {
        out.push_str(&format!("    L2 audit note: {}\n", outcome.semantic_review.next_step));
        out.push_str(&format!(
            "    review digest: {}\n",
            outcome.semantic_review.review_digest_command.join(" ")
        ));
    } else if outcome.semantic_review.status != "clear" {
        out.push_str(&format!("    next review surface: {}\n", outcome.semantic_review.next_step));
        out.push_str(&format!(
            "    review workload: {}\n",
            outcome.semantic_review.workload_command.join(" ")
        ));
        let review_command_label =
            if outcome.semantic_review.primary_surface == "semantic_proposals" {
                "candidate drilldown command"
            } else {
                "primary review command"
            };
        out.push_str(&format!(
            "    {review_command_label}: {}\n",
            semantic_review_primary_command(&outcome.semantic_review).join(" ")
        ));
        if outcome.semantic_review.status == "review_only_beliefs" {
            out.push_str(&format!(
                "    review digest: {}\n",
                outcome.semantic_review.review_digest_command.join(" ")
            ));
        }
        out.push_str(&format!(
            "    review render: {}\n",
            outcome.semantic_review.review_render_command.join(" ")
        ));
    }
    out.push('\n');
    for row in &outcome.clients {
        out.push_str(&format!(
            "  {:<12} mcp={:<5} runtime={:<18} model={} status={}\n",
            row.display_name,
            if row.mcp_registration_ready { "ready" } else { "block" },
            row.runtime_status,
            row.capture_model,
            row.goal_status
        ));
        if row.proof_storage_status != "available" {
            out.push_str(&format!("    proof storage: {}\n", row.proof_storage_status));
        }
        render_runtime_launch_probe_lines(&mut out, row, "    ");
        if !row.missing_proof_levels.is_empty() {
            out.push_str(&format!("    missing proof: {}\n", row.missing_proof_levels.join(", ")));
        }
        if row.capture_model == PRIVATE_APP_CAPTURE_MODEL {
            if !row.proof_level_statuses.is_empty() {
                let statuses = row
                    .proof_level_statuses
                    .iter()
                    .map(|proof| format!("{}={}", proof.proof_level, proof.status))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("    proof levels: {statuses}\n"));
            }
            if row.artifact_failure_count > 0 || row.coherence_failure_count > 0 {
                out.push_str(&format!(
                    "    artifact integrity: failures={} coherence_failures={}\n",
                    row.artifact_failure_count, row.coherence_failure_count
                ));
            }
            if let Some(summary) = &row.artifact_repair_summary {
                out.push_str(&format!(
                    "    artifact repair summary: status={} proof_free_materialization={} requires_real_private_client_evidence={} records_proof={} creates_verification_event={} promotes_cloud_draft={}\n",
                    summary.status,
                    summary.proof_free_local_materialization_only,
                    summary.requires_real_private_client_evidence_before_recording,
                    summary.records_proof,
                    summary.creates_verification_event,
                    summary.promotes_cloud_draft
                ));
                out.push_str(&format!(
                    "    artifact repair next: {}\n",
                    summary.next_command.join(" ")
                ));
                render_artifact_repair_summary_guidance(&mut out, Some(summary), "      ");
            }
            if let Some(status) = &row.proof_session_status {
                out.push_str(&format!(
                    "    proof session: status={} gate={} installed_configs={} target_configs={} next={}\n",
                    status,
                    row.proof_session_release_gate.as_deref().unwrap_or("unknown"),
                    row.installed_config_eligible_candidates.unwrap_or_default(),
                    row.installed_config_private_target_eligible_candidates
                        .unwrap_or_default(),
                    row.proof_session_next_step_id.as_deref().unwrap_or("none")
                ));
            }
            if !row.eligible_setup_artifact_paths.is_empty()
                || !row.eligible_private_client_target_paths.is_empty()
                || !row.private_client_target_candidate_paths.is_empty()
            {
                out.push_str(&format!(
                    "    install visibility: setup_artifacts={} target_configs={} target_candidates={}\n",
                    row.eligible_setup_artifact_paths.len(),
                    row.eligible_private_client_target_paths.len(),
                    row.private_client_target_candidate_paths.len()
                ));
            }
            if let Some(command) = private_target_config_install_command(row) {
                out.push_str(&format!(
                    "    target install: {}  # proof-free, non-overwriting config write\n",
                    command.join(" ")
                ));
            }
            if let (Some(event_source), Some(binding_nonce)) =
                (&row.expected_event_source, &row.binding_nonce)
            {
                out.push_str(&format!(
                    "    hook evidence: event_source={} binding_nonce={}\n",
                    event_source, binding_nonce
                ));
            }
            if let Some(path) = &row.event_jsonl_path {
                out.push_str(&format!(
                    "    event probe: status={} path={}\n",
                    row.event_jsonl_probe_status.as_deref().unwrap_or("unknown"),
                    path
                ));
            }
            if let Some(command) = &row.private_event_watch_command {
                out.push_str(&format!(
                    "    event watch: {}  # waits for the required private event contract\n",
                    command.join(" ")
                ));
            }
            if let Some(command) = &row.private_event_wait_command {
                out.push_str(&format!(
                    "    event wait: {}  # bounded read-only wait for the required private event\n",
                    command.join(" ")
                ));
            }
            if let Some(command) = &row.simple_private_event_wait_command {
                out.push_str(&format!(
                    "    simple event wait: {}  # bounded read-only wait, flag-based form\n",
                    command.join(" ")
                ));
            }
            if let Some(observation) = &row.private_event_observation {
                let mismatch_text = if observation.latest_spool_mismatches.is_empty() {
                    "none".to_string()
                } else {
                    observation.latest_spool_mismatches.join(",")
                };
                out.push_str(&format!(
                    "    event observation: status={} events={} matching_private={} matching_nonce={} manual_non_release={} mismatches={}\n",
                    observation.status,
                    observation.event_count,
                    observation.matching_private_event_count,
                    observation.matching_private_binding_nonce_count,
                    observation.matching_private_non_release_manual_event_count,
                    mismatch_text
                ));
                if let Some(event) = &observation.relevant_event {
                    out.push_str(&format!(
                        "    relevant event: client={} event_source={} binding_nonce={}\n",
                        event.client.as_deref().unwrap_or("unknown"),
                        event.event_source.as_deref().unwrap_or("unknown"),
                        event.binding_nonce.as_deref().unwrap_or("unknown")
                    ));
                }
            }
            if let Some(check) = &row.codex_notify_reload_check {
                out.push_str(&format!(
                    "    notify reload: status={} restart_recommended={} stale_processes={} config_mtime={}\n",
                    check.status,
                    check.restart_recommended,
                    check.stale_codex_desktop_process_count,
                    check
                        .config_mtime_unix
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                ));
            }
            if let Some(check) = &row.continue_extension_config_check {
                out.push_str(&format!(
                    "    continue config: status={} visible={} has_mcpServers={} has_modelContextProtocol={} has_soma_server={} config={} recommended={}\n",
                    check.status,
                    !continue_extension_config_not_visible(check),
                    check.has_mcp_servers,
                    check.has_model_context_protocol,
                    check.has_soma_server,
                    check.config_path.as_deref().unwrap_or("none"),
                    check.recommended_config_path
                ));
                out.push_str(&format!(
                    "    continue profile: status={} required_fields_present={} missing_fields={} config={}\n",
                    check.profile_config_status,
                    check.profile_config_required_fields_present,
                    if check.profile_config_missing_required_fields.is_empty() {
                        "none".to_string()
                    } else {
                        check.profile_config_missing_required_fields.join(",")
                    },
                    check.profile_config_path.as_deref().unwrap_or("none")
                ));
                out.push_str(&format!(
                    "    continue devdata: status={} visible={} collector={} endpoint={}:{} destination={} config={}\n",
                    check.devdata_destination_status,
                    check.devdata_destination_visible,
                    check.devdata_collector_status,
                    check.devdata_collector_host,
                    check.devdata_collector_port,
                    check.devdata_destination,
                    check.devdata_config_path.as_deref().unwrap_or("none")
                ));
                if !check.devdata_destination_visible {
                    out.push_str(&format!(
                        "    continue devdata install: {}  # inspect/write local Continue data destination\n",
                        check.devdata_install_command.join(" ")
                    ));
                }
                if continue_profile_config_invalid(check) {
                    out.push_str(&format!(
                        "    continue profile repair: {}  # inspect/write required Continue name/version fields\n",
                        check.devdata_install_command.join(" ")
                    ));
                }
                if continue_devdata_collector_start_needed(check) {
                    let command = continue_devdata_collector_command_for_row(row)
                        .unwrap_or_else(|| check.devdata_collector_command.clone());
                    out.push_str(&format!(
                        "    continue devdata collector: {}  # start local collector before the real Continue action\n",
                        command.join(" ")
                    ));
                } else if continue_devdata_collector_probe_blocked(check) {
                    let command = continue_devdata_collector_status_command_for_row(row)
                        .unwrap_or_else(|| check.devdata_collector_command.clone());
                    out.push_str(&format!(
                        "    continue devdata collector status: {}  # probe was blocked here; check from the operator shell before starting duplicates\n",
                        command.join(" ")
                    ));
                }
                if check.merge_required {
                    out.push_str(&format!(
                        "    continue config write: {}  # write MCP server JSON to {}\n",
                        check.mcp_config_command.join(" "),
                        check.recommended_config_path
                    ));
                }
                out.push_str(&format!("    continue config next: {}\n", check.next_step));
                out.push_str(&format!(
                    "    continue extension: status={} observed={} paths={}\n",
                    check.extension_installation_status,
                    check.extension_observed,
                    if check.extension_paths.is_empty() {
                        "none".to_string()
                    } else {
                        check.extension_paths.join(",")
                    }
                ));
                out.push_str(&format!(
                    "    continue extension next: {}\n",
                    check.extension_next_step
                ));
            }
            if let Some(command) = private_hook_readiness_command(row) {
                out.push_str(&format!(
                    "    hook readiness: {}  # latest spool mismatch summary, proof-free\n",
                    command.join(" ")
                ));
            }
            if let Some(command) = &row.simple_private_hook_readiness_command {
                out.push_str(&format!(
                    "    simple hook readiness: {}  # flag-based readiness probe, proof-free\n",
                    command.join(" ")
                ));
            }
            if let Some(intent) = &row.proof_session_next_operator_step_intent {
                out.push_str(&format!("    next intent: {intent}\n"));
            }
            out.push_str(&format!(
                "    proof session brief: {}  # human-readable release gate and next step\n",
                private_app_proof_session_brief_command(row).join(" ")
            ));
            if let Some(command) = &row.proof_session_next_command {
                out.push_str(&format!("    next proof command: {}\n", command.join(" ")));
            }
            if let Some(tool) = &row.proof_session_next_mcp_tool {
                let arguments = row
                    .proof_session_next_mcp_arguments
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                out.push_str(&format!("    next proof MCP: {tool} {arguments}\n"));
            }
            if let Some(action) = &row.proof_session_external_action {
                out.push_str(&format!(
                    "    external action: kind={} proof_step={} records_proof={} prompt=\"{}\"\n",
                    action.action_kind,
                    action.proof_session_step_id,
                    action.records_proof,
                    action.suggested_minimal_test_prompt
                ));
                out.push_str(&format!(
                    "    external action boundary: {}\n",
                    action.why_next_mcp_call_is_null
                ));
            }
            if !row.proof_session_blocking_reasons.is_empty() {
                out.push_str(&format!(
                    "    blockers: {}\n",
                    row.proof_session_blocking_reasons.join("; ")
                ));
            }
            if !row.proof_session_stage_blockers.is_empty() {
                let blockers = row
                    .proof_session_stage_blockers
                    .iter()
                    .map(stage_blocker_text)
                    .collect::<Vec<_>>()
                    .join(" | ");
                out.push_str(&format!("    proof stage blockers: {blockers}\n"));
            }
            let ready_runbook_steps = row
                .proof_session_runbook_steps
                .iter()
                .filter(|step| step.ready_now)
                .map(|step| {
                    format!(
                        "{}:{}:{}",
                        step.id,
                        if step.records_proof { "records-proof" } else { "read-only" },
                        step.stage
                    )
                })
                .collect::<Vec<_>>();
            if !ready_runbook_steps.is_empty() {
                out.push_str(&format!(
                    "    proof runbook ready steps: {}\n",
                    ready_runbook_steps.join(", ")
                ));
            }
            if let Some(error) = &row.proof_session_error {
                out.push_str(&format!("    proof session error: {error}\n"));
            }
        }
        let next_step = if row.capture_model == PRIVATE_APP_CAPTURE_MODEL {
            private_app_operator_next_step(row)
        } else {
            row.next_step.clone()
        };
        out.push_str(&format!("    next: {next_step}\n"));
    }
    out.push('\n');
    out.push_str("Useful next commands:\n");
    for command in &outcome.next_commands {
        out.push_str("  ");
        out.push_str(&command.join(" "));
        if let Some(note) = next_command_annotation(outcome, command) {
            out.push_str("  # ");
            out.push_str(note);
        }
        out.push('\n');
    }
    out.push_str("\nTrust boundary: read-only status only; no proof, verification, promotion, or hook install.\n");
    out
}

fn private_hook_readiness_command(row: &ClientStatusRow) -> Option<&[String]> {
    if row.capture_model != PRIVATE_APP_CAPTURE_MODEL {
        return None;
    }
    row.next_commands
        .iter()
        .find(|command| command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh"))
        .map(Vec::as_slice)
}

fn private_target_config_install_command(row: &ClientStatusRow) -> Option<&[String]> {
    if row.capture_model != PRIVATE_APP_CAPTURE_MODEL {
        return None;
    }
    row.next_commands
        .iter()
        .find(|command| {
            command.iter().any(|part| part == "--render-installed-config")
                && command.iter().any(|part| part == "--write-installed-config")
        })
        .map(Vec::as_slice)
}

fn render_runtime_launch_probe_lines(out: &mut String, row: &ClientStatusRow, indent: &str) {
    if let Some(command) = &row.runtime_launch_probe_command {
        out.push_str(&format!("{indent}runtime launch probe: {}\n", command.join(" ")));
    }
    if let Some(note) = &row.runtime_launch_probe_note {
        out.push_str(&format!("{indent}runtime launch note: {note}\n"));
    }
}

fn continue_mcp_config_render_command(row: &ClientStatusRow) -> Option<&[String]> {
    if row.client != "continue" {
        return None;
    }
    row.next_commands
        .iter()
        .find(|command| {
            command.iter().any(|part| part == "mcp-config")
                && command.iter().any(|part| part == "--client")
                && command.iter().any(|part| part == "continue")
                && !command.iter().any(|part| part == "--check")
        })
        .map(Vec::as_slice)
}

fn continue_devdata_install_command_for_row(row: &ClientStatusRow) -> Option<&[String]> {
    if row.client != "continue" {
        return None;
    }
    let check = row.continue_extension_config_check.as_ref()?;
    (!check.devdata_destination_visible).then_some(check.devdata_install_command.as_slice())
}

fn continue_profile_config_repair_command_for_row(row: &ClientStatusRow) -> Option<&[String]> {
    if row.client != "continue" {
        return None;
    }
    let check = row.continue_extension_config_check.as_ref()?;
    continue_profile_config_invalid(check).then_some(check.devdata_install_command.as_slice())
}

fn continue_devdata_collector_command_for_row(row: &ClientStatusRow) -> Option<Vec<String>> {
    if row.client != "continue" {
        return None;
    }
    let check = row.continue_extension_config_check.as_ref()?;
    if !continue_devdata_collector_start_needed(check) {
        return None;
    }
    let mut command = check.devdata_collector_command.clone();
    if let Some(event_jsonl) = row.event_jsonl_path.as_deref() {
        command.extend(["--jsonl".to_string(), event_jsonl.to_string()]);
    }
    if let Some(binding_config) = row.eligible_private_client_target_paths.first() {
        command.extend(["--binding-config".to_string(), binding_config.clone()]);
    }
    Some(command)
}

fn continue_devdata_collector_status_command_for_row(row: &ClientStatusRow) -> Option<Vec<String>> {
    if row.client != "continue" {
        return None;
    }
    let check = row.continue_extension_config_check.as_ref()?;
    if !check.devdata_destination_visible {
        return None;
    }
    let mut command = check.devdata_collector_command.clone();
    if let Some(event_jsonl) = row.event_jsonl_path.as_deref() {
        command.extend(["--jsonl".to_string(), event_jsonl.to_string()]);
    }
    if let Some(binding_config) = row.eligible_private_client_target_paths.first() {
        command.extend(["--binding-config".to_string(), binding_config.clone()]);
    }
    let mut managed =
        vec!["tools/soma-continue-devdata-start.sh".to_string(), "status".to_string()];
    managed.extend(command.into_iter().skip(1));
    Some(managed)
}

fn continue_devdata_collector_managed_start_command_for_row(
    row: &ClientStatusRow,
) -> Option<Vec<String>> {
    let direct = continue_devdata_collector_command_for_row(row)?;
    let mut command = vec!["tools/soma-continue-devdata-start.sh".to_string(), "start".to_string()];
    command.extend(direct.into_iter().skip(1));
    Some(command)
}

fn continue_devdata_collector_start_needed(check: &ClientContinueExtensionConfigCheck) -> bool {
    check.devdata_destination_visible && check.devdata_collector_status == "not_listening"
}

fn continue_devdata_collector_probe_blocked(check: &ClientContinueExtensionConfigCheck) -> bool {
    check.devdata_destination_visible
        && matches!(check.devdata_collector_status, "probe_blocked" | "probe_unavailable")
}

fn codex_app_reopen_command_for_row(row: &ClientStatusRow) -> Option<Vec<String>> {
    if codex_app_manual_restart_required(row) {
        Some(vec!["open".to_string(), "-a".to_string(), "Codex".to_string()])
    } else {
        None
    }
}

fn codex_app_quit_command_for_row(row: &ClientStatusRow) -> Option<Vec<String>> {
    if codex_app_manual_restart_required(row) {
        Some(vec!["osascript".to_string(), "-e".to_string(), "quit app \"Codex\"".to_string()])
    } else {
        None
    }
}

fn codex_app_manual_restart_required(row: &ClientStatusRow) -> bool {
    row.client == "codex-app"
        && row.codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended)
}

fn semantic_review_primary_command(semantic_review: &ClientSemanticReviewStatus) -> Vec<String> {
    if !semantic_review.primary_command.is_empty() {
        semantic_review.primary_command.clone()
    } else {
        semantic_review_primary_command_from_parts(
            &semantic_review.status,
            &semantic_review.primary_surface,
            &semantic_review.promotion_matrix,
            &semantic_review.review_render_command,
            &semantic_review.review_digest_command,
            &semantic_review.review_report_command,
        )
    }
}

fn semantic_review_primary_client(semantic_review: &ClientSemanticReviewStatus) -> Option<String> {
    if semantic_review.client == "generic" {
        None
    } else {
        Some(semantic_review.client.clone())
    }
}

fn semantic_review_primary_command_from_parts(
    status: &str,
    primary_surface: &str,
    promotion_matrix: &[ClientSemanticPromotionMatrixRow],
    review_render_command: &[String],
    review_digest_command: &[String],
    review_report_command: &[String],
) -> Vec<String> {
    if status == "semantic_review_only_pending" || primary_surface == "semantic_proposals" {
        semantic_review_lane_primary_command_from_matrix(promotion_matrix, "semantic_fact")
            .unwrap_or_else(|| review_report_command.to_vec())
    } else if matches!(status, "review_only_beliefs" | "noise_triage_only")
        || primary_surface == "review_digest"
    {
        review_digest_command.to_vec()
    } else if primary_surface == "review_report" {
        review_report_command.to_vec()
    } else {
        review_render_command.to_vec()
    }
}

fn semantic_review_next_commands_from_parts(
    status: &str,
    primary_surface: &str,
    primary_command: &[String],
    review_actions_command: &[String],
    review_digest_command: &[String],
    review_report_command: &[String],
    proof_session_command: &[String],
) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    push_next_command_once(&mut commands, primary_command.to_vec());
    if primary_surface == "semantic_proposals" {
        push_next_command_once(&mut commands, review_actions_command.to_vec());
        push_next_command_once(&mut commands, review_report_command.to_vec());
        return commands;
    }
    match status {
        "blocked_cloud_draft_verification" | "pending_semantic_review" => {
            push_next_command_once(&mut commands, review_actions_command.to_vec());
            push_next_command_once(&mut commands, proof_session_command.to_vec());
        }
        "semantic_review_only_pending" => {
            push_next_command_once(&mut commands, review_report_command.to_vec());
            push_next_command_once(&mut commands, review_actions_command.to_vec());
        }
        "review_only_beliefs" | "noise_triage_only" => {
            push_next_command_once(&mut commands, review_digest_command.to_vec());
        }
        "unavailable" => {
            push_next_command_once(&mut commands, review_report_command.to_vec());
        }
        _ => {}
    }
    commands
}

fn semantic_review_lane_primary_command_from_matrix(
    promotion_matrix: &[ClientSemanticPromotionMatrixRow],
    target: &str,
) -> Option<Vec<String>> {
    promotion_matrix
        .iter()
        .find(|lane| lane.target == target && !lane.primary_command.is_empty())
        .map(|lane| lane.primary_command.clone())
}

fn semantic_review_operator_next_action_id(semantic_review: &ClientSemanticReviewStatus) -> String {
    semantic_review_operator_next_action_id_for_status(&semantic_review.status)
}

fn semantic_review_operator_next_action_id_for_status(status: &str) -> String {
    match status {
        "blocked_cloud_draft_verification" => "verify_cloud_draft_with_independent_evidence",
        "pending_semantic_review" => "review_semantic_learning_candidates",
        "semantic_review_only_pending" => "request_semantic_candidate_verification",
        "review_only_beliefs" => "resolve_belief_review_signals",
        "noise_triage_only" => "inspect_semantic_learning_status",
        "unavailable" => "restore_semantic_learning_storage_access",
        _ => "inspect_semantic_learning_status",
    }
    .to_string()
}

fn semantic_review_operator_next_action_label(
    semantic_review: &ClientSemanticReviewStatus,
) -> String {
    semantic_review_operator_next_action_label_for_status(&semantic_review.status)
}

fn semantic_review_operator_next_action_label_for_status(status: &str) -> String {
    match status {
        "blocked_cloud_draft_verification" => "Verify cloud draft",
        "pending_semantic_review" => "Review semantic learning candidates",
        "semantic_review_only_pending" => "Request semantic candidate verification",
        "review_only_beliefs" => "Resolve belief review signals",
        "noise_triage_only" => "Inspect semantic learning status",
        "unavailable" => "Restore semantic storage access",
        _ => "Inspect semantic learning status",
    }
    .to_string()
}

fn semantic_review_blocks_learning(semantic_review: &ClientSemanticReviewStatus) -> bool {
    matches!(
        semantic_review.status.as_str(),
        "blocked_cloud_draft_verification"
            | "pending_semantic_review"
            | "semantic_review_only_pending"
            | "review_only_beliefs"
    )
}

fn stage_blocker_text(blocker: &ClientProofSessionStageBlocker) -> String {
    let reasons = blocker
        .blocking_reasons
        .iter()
        .map(|reason| compact_blocking_reason(reason))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}: {}", blocker.proof_level, reasons)
}

fn compact_blocking_reason(reason: &str) -> String {
    reason
        .strip_suffix(" before observed_app_hook proof can be recorded")
        .or_else(|| reason.strip_suffix(" before observed_in_client_render proof can be recorded"))
        .or_else(|| reason.strip_suffix(" before observed_review_action proof can be recorded"))
        .unwrap_or(reason)
        .to_string()
}

const EXPLICIT_CLI_CAPTURE_MODEL: &str = "explicit_cli_mcp_context_capture";
const PRIVATE_APP_CAPTURE_MODEL: &str = "private_app_hook_render_review_action";
const CLIENT_SEMANTIC_REVIEW_SURFACE_LIMIT: usize = 20;
const PRIVATE_EVENT_OBSERVATION_TRUST_BOUNDARY: &str =
    "private_event_observation_is_read_only: scans the adapter event JSONL to explain latest private-hook mismatch state; records no proof row, creates no verification event, promotes no cloud draft, and cannot substitute for real observed_app_hook evidence";
const CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY: &str =
    "codex_notify_reload_check_is_read_only: compares local Codex app process start time with notify config mtime only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for a real Codex app hook event";
const CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY: &str =
    "continue_extension_config_check_is_read_only: inspects local Continue extension config visibility only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for a real Continue hook event";
const CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY: &str =
    "continue_devdata_collector_probe_is_read_only: probes only whether the configured local Continue dev-data collector TCP endpoint accepts a connection; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for a real Continue hook event";
const CONTINUE_DEVDATA_DESTINATION: &str = "http://127.0.0.1:8766/continue-devdata";
const CONTINUE_DEVDATA_DEFAULT_HOST: &str = "127.0.0.1";
const CONTINUE_DEVDATA_DEFAULT_PORT: u16 = 8766;

fn proof_storage_diagnostic_db_path() -> String {
    std::env::temp_dir()
        .join(format!("soma-client-readiness-diagnostic-{}.db", std::process::id()))
        .display()
        .to_string()
}

fn proof_storage_diagnostic_command() -> Vec<String> {
    vec![
        "soma".to_string(),
        "clients".to_string(),
        "--db-path".to_string(),
        proof_storage_diagnostic_db_path(),
        "--json".to_string(),
    ]
}

fn build_operator_card(
    summary: &ClientStatusSummary,
    semantic_review: &ClientSemanticReviewStatus,
    rows: &[ClientStatusRow],
    project_filter: Option<&str>,
    real_cli_probe: Option<&ClientRealCliDogfoodProbeReport>,
) -> ClientOperatorCard {
    let mcp_ready_clients = rows
        .iter()
        .filter(|row| row.mcp_registration_ready)
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let runtime_detected_clients = rows
        .iter()
        .filter(|row| row.runtime_status == "detected")
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let runtime_missing_rows =
        rows.iter().filter(|row| row.runtime_status == "missing").collect::<Vec<_>>();
    let runtime_missing_clients =
        runtime_missing_rows.iter().map(|row| row.client.to_string()).collect::<Vec<_>>();
    let runtime_not_cli_detectable_clients = rows
        .iter()
        .filter(|row| row.runtime_status == "not_cli_detectable")
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let runtime_check_commands = runtime_missing_rows
        .iter()
        .map(|row| vec!["which".to_string(), row.runtime_target.clone()])
        .collect::<Vec<_>>();
    let mut proof_storage_recovery_commands = if summary.proof_storage_unavailable {
        vec![
            proof_storage_diagnostic_command(),
            vec![
                "soma".to_string(),
                "clients".to_string(),
                "--db-path".to_string(),
                "<readable-soma.db>".to_string(),
                "--json".to_string(),
            ],
            vec!["soma".to_string(), "diagnose".to_string()],
        ]
    } else {
        Vec::new()
    };
    let (binary_identity, binary_identity_errors) =
        crate::cli::binary_identity::collect_binary_identity();
    for command in &mut proof_storage_recovery_commands {
        *command =
            command_with_current_binary_when_path_soma_differs(command.clone(), &binary_identity);
    }
    let private_app_restart_recommended_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| {
            row.codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended)
        })
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let private_app_restart_commands = build_private_app_restart_commands(rows);
    let continue_profile_config_invalid_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| continue_profile_config_invalid_for_row(row))
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let continue_extension_config_not_visible_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| {
            row.continue_extension_config_check
                .as_ref()
                .is_some_and(continue_extension_config_not_visible)
        })
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let continue_extension_not_observed_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| {
            row.continue_extension_config_check
                .as_ref()
                .is_some_and(|check| !check.extension_observed)
        })
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let mut private_app_next_actions = build_private_app_next_actions(rows);
    for action in &mut private_app_next_actions {
        action.next_command = command_with_current_binary_when_path_soma_differs(
            action.next_command.clone(),
            &binary_identity,
        );
        if let Some(command) = &mut action.continue_devdata_collector_start_command {
            *command = command_with_current_binary_when_path_soma_differs(
                command.clone(),
                &binary_identity,
            );
        }
    }
    let private_app_hook_trigger_ready_clients = private_app_next_actions
        .iter()
        .filter(|action| {
            action.operator_next_action_id
                == "trigger_real_private_client_hook_to_write_private_spool_event"
        })
        .map(|action| action.client.clone())
        .collect::<Vec<_>>();
    let private_app_real_hook_ready_clients = private_app_hook_trigger_ready_clients.clone();
    let private_app_observed_app_hook_recordable_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| {
            row.proof_session_ready_to_record_proof_levels
                .iter()
                .any(|level| level == ClientBindingProofLevel::ObservedAppHook.as_str())
        })
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let private_app_collector_start_commands = build_private_app_collector_start_commands(rows);
    let private_app_wait_commands = build_private_app_wait_commands(rows);
    let private_app_hook_integration_templates = build_private_app_hook_integration_templates(rows);
    let mut private_app_release_plan = build_private_app_release_plan(rows);
    for item in &mut private_app_release_plan {
        item.next_command = command_with_current_binary_when_path_soma_differs(
            item.next_command.clone(),
            &binary_identity,
        );
    }
    let mut private_app_release_proof_checklist = build_private_app_release_proof_checklist(rows);
    for checklist in &mut private_app_release_proof_checklist {
        checklist.proof_session_command = command_with_current_binary_when_path_soma_differs(
            checklist.proof_session_command.clone(),
            &binary_identity,
        );
        checklist.release_runbook_command = command_with_current_binary_when_path_soma_differs(
            checklist.release_runbook_command.clone(),
            &binary_identity,
        );
        if let Some(command) = &mut checklist.simple_release_runbook_command {
            *command = command_with_current_binary_when_path_soma_differs(
                command.clone(),
                &binary_identity,
            );
        }
        if let Some(command) = &mut checklist.target_install_command {
            *command = command_with_current_binary_when_path_soma_differs(
                command.clone(),
                &binary_identity,
            );
        }
        if let Some(command) = &mut checklist.hook_readiness_command {
            *command = command_with_current_binary_when_path_soma_differs(
                command.clone(),
                &binary_identity,
            );
        }
        if let Some(command) = &mut checklist.simple_hook_readiness_command {
            *command = command_with_current_binary_when_path_soma_differs(
                command.clone(),
                &binary_identity,
            );
        }
        checklist.next_command = command_with_current_binary_when_path_soma_differs(
            checklist.next_command.clone(),
            &binary_identity,
        );
        if let Some(hint) = &mut checklist.next_recording_after_trusted_evidence {
            hint.command = command_with_current_binary_when_path_soma_differs(
                hint.command.clone(),
                &binary_identity,
            );
        }
    }
    let mut observed_capture_dogfood_evidence = rows
        .iter()
        .filter_map(|row| row.observed_capture_dogfood_evidence.clone())
        .collect::<Vec<_>>();
    for evidence in &mut observed_capture_dogfood_evidence {
        evidence.recall_command = command_with_current_binary_when_path_soma_differs(
            evidence.recall_command.clone(),
            &binary_identity,
        );
        evidence.context_why_command = command_with_current_binary_when_path_soma_differs(
            evidence.context_why_command.clone(),
            &binary_identity,
        );
    }
    let observed_capture_dogfood_clients = observed_capture_dogfood_evidence
        .iter()
        .map(|evidence| evidence.client.clone())
        .collect::<Vec<_>>();
    let explicit_capture_ready_clients = rows
        .iter()
        .filter(|row| row.goal_status == "explicit_cli_capture_available")
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let mut capture_dogfood_matrix = build_capture_dogfood_matrix(rows, real_cli_probe);
    for item in &mut capture_dogfood_matrix {
        item.next_command = command_with_current_binary_when_path_soma_differs(
            item.next_command.clone(),
            &binary_identity,
        );
    }
    let unobserved_capture_clients = capture_dogfood_matrix
        .iter()
        .filter(|item| !item.observed_local_capture)
        .map(|item| item.client.clone())
        .collect::<Vec<_>>();
    let private_capture_ready_clients = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| row.ready_for_private_client_claim)
        .map(|row| row.client.to_string())
        .collect::<Vec<_>>();
    let blocked_private_rows = rows
        .iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| !row.ready_for_private_client_claim)
        .collect::<Vec<_>>();
    let blocked_private_clients =
        blocked_private_rows.iter().map(|row| row.client.to_string()).collect::<Vec<_>>();

    let mut safe_to_claim = Vec::new();
    if summary.mcp_registration_ready_count == summary.client_count && summary.client_count > 0 {
        safe_to_claim.push(
            "All supported client MCP registration configs are shaped for soma mcp-serve."
                .to_string(),
        );
    } else if summary.mcp_registration_ready_count > 0 {
        safe_to_claim.push(format!(
            "{} supported client MCP registration config(s) are shaped for soma mcp-serve.",
            summary.mcp_registration_ready_count
        ));
    }
    if !explicit_capture_ready_clients.is_empty() {
        safe_to_claim.push(format!(
            "Explicit CLI MCP/context/capture can be dogfooded for {}.",
            explicit_capture_ready_clients.join(", ")
        ));
    }
    if !observed_capture_dogfood_clients.is_empty() {
        safe_to_claim.push(format!(
            "Stored local capture dogfood evidence exists for {}; this is not release-grade private app proof.",
            observed_capture_dogfood_clients.join(", ")
        ));
    }
    if !private_capture_ready_clients.is_empty() {
        safe_to_claim.push(format!(
            "Release-grade private-client capture proof is ready for {}.",
            private_capture_ready_clients.join(", ")
        ));
    }

    let semantic_blocked = semantic_review_blocks_learning(semantic_review);

    let (
        status,
        operator_next_action_id,
        operator_next_action_label,
        primary_client,
        headline,
        primary_next_step,
        primary_next_command,
    ) = if summary.proof_storage_unavailable {
        let diagnostic_command = command_with_current_binary_when_path_soma_differs(
            proof_storage_diagnostic_command(),
            &binary_identity,
        );
        (
            "proof_storage_unavailable".to_string(),
            "restore_client_binding_proof_storage_access".to_string(),
            "Restore proof storage access".to_string(),
            None,
            "MCP checks are visible, but private-client proof storage is unreadable.".to_string(),
            format!(
                "Grant SOMA read access to the configured DB for real proof rows, or rerun `{}` for an immediate read-only MCP/runtime diagnostic.",
                diagnostic_command.join(" ")
            ),
            diagnostic_command,
        )
    } else if let Some(row) = primary_blocked_private_row(&blocked_private_rows) {
        let primary_command = command_with_current_binary_when_path_soma_differs(
            primary_private_app_command(row),
            &binary_identity,
        );
        let primary_next_step =
            private_app_operator_next_step_with_command(row, Some(&primary_command));
        (
            "private_app_proof_pending".to_string(),
            private_app_operator_next_action_id(row),
            private_app_operator_next_action_label(row),
            Some(row.client.to_string()),
            format!(
                "{} private app client(s) still need app-hook/render/review-action proof.",
                blocked_private_rows.len()
            ),
            primary_next_step,
            primary_command,
        )
    } else if let Some(item) = primary_real_cli_probe_blocker(&capture_dogfood_matrix) {
        let probe_status = real_cli_probe_blocking_status(item).unwrap_or(item.status.as_str());
        (
            probe_status.to_string(),
            real_cli_probe_operator_next_action_id(probe_status).to_string(),
            real_cli_probe_operator_next_action_label(probe_status).to_string(),
            Some(item.client.clone()),
            format!("Real CLI dogfood capture is blocked for {}.", item.client),
            item.last_real_cli_probe_next_action.clone().unwrap_or_else(|| {
                "Inspect the real CLI dogfood probe artifact and rerun after fixing client access."
                    .to_string()
            }),
            item.next_command.clone(),
        )
    } else if semantic_blocked {
        (
            semantic_review.status.clone(),
            semantic_review_operator_next_action_id(semantic_review),
            semantic_review_operator_next_action_label(semantic_review),
            semantic_review_primary_client(semantic_review),
            "Semantic learning review needs attention before durable learning claims.".to_string(),
            semantic_review.next_step.clone(),
            semantic_review_primary_command(semantic_review),
        )
    } else if summary.runtime_missing_count > 0 {
        let command = command_with_current_binary_when_path_soma_differs(
            vec!["soma".to_string(), "clients".to_string(), "--json".to_string()],
            &binary_identity,
        );
        (
            "runtime_attention_required".to_string(),
            "install_or_expose_missing_client_runtime".to_string(),
            "Install or expose missing client runtime".to_string(),
            runtime_missing_clients.first().cloned(),
            "Private-client proof is ready, but one or more CLI runtimes are not detected."
                .to_string(),
            format!(
                "Install or expose missing client runtimes on PATH, then rerun `{}`.",
                command.join(" ")
            ),
            command,
        )
    } else {
        (
            "ready".to_string(),
            "run_full_client_dogfood_report".to_string(),
            "Run full client dogfood report".to_string(),
            None,
            "SOMA client readiness checks are clear for the current proof ledger.".to_string(),
            "Run the dogfood report when you want a full client integration sweep.".to_string(),
            vec!["tools/client-dogfood-report.sh".to_string()],
        )
    };

    let mut blocked_claims = Vec::new();
    if summary.proof_storage_unavailable {
        blocked_claims.push("Private app-hook/render/review-action readiness cannot be claimed while proof storage is unreadable.".to_string());
    }
    if semantic_blocked {
        blocked_claims.push(
            "Durable semantic learning cannot rely on pending review or unverified cloud drafts."
                .to_string(),
        );
    }
    if !blocked_private_clients.is_empty() {
        blocked_claims.push(format!(
            "Automatic private capture / in-client review readiness is unproven for {}.",
            blocked_private_clients.join(", ")
        ));
    }
    if !private_app_restart_recommended_clients.is_empty() {
        blocked_claims.push(format!(
            "Private app hook evidence may require restarting or reopening {} because the app was already running before its notify config changed.",
            private_app_restart_recommended_clients.join(", ")
        ));
    }
    if !continue_profile_config_invalid_clients.is_empty() {
        blocked_claims.push(format!(
            "Continue profile config.yaml/config.yml is rejected for {}; repair required top-level name/version fields before hook-trigger readiness.",
            continue_profile_config_invalid_clients.join(", ")
        ));
    }
    if !continue_extension_config_not_visible_clients.is_empty() {
        blocked_claims.push(format!(
            "Continue extension/profile config is not ready for SOMA MCP for {}.",
            continue_extension_config_not_visible_clients.join(", ")
        ));
    }
    if !continue_extension_not_observed_clients.is_empty() {
        blocked_claims.push(format!(
            "Continue extension installation is not locally observable for {}; config visibility alone cannot prove a real private hook path.",
            continue_extension_not_observed_clients.join(", ")
        ));
    }
    if !runtime_missing_clients.is_empty() {
        blocked_claims.push(format!(
            "Client runtime detection is missing for {}.",
            runtime_missing_clients.join(", ")
        ));
    }
    if !unobserved_capture_clients.is_empty() {
        blocked_claims.push(format!(
            "Stored local capture dogfood evidence is not yet observed for {}; MCP/config readiness alone does not prove actual capture.",
            unobserved_capture_clients.join(", ")
        ));
    }
    let probe_blockers = capture_dogfood_matrix
        .iter()
        .filter_map(|item| {
            real_cli_probe_blocking_status(item).map(|status| format!("{}={status}", item.client))
        })
        .collect::<Vec<_>>();
    if !probe_blockers.is_empty() {
        blocked_claims.push(format!(
            "Latest real CLI dogfood probe reports {}; latest real-client capture remains unobserved until the real client host completes SOMA capture in that probe.",
            probe_blockers.join(", ")
        ));
    }
    let primary_next_command =
        command_with_current_binary_when_path_soma_differs(primary_next_command, &binary_identity);
    let primary_next_command_safety =
        primary_next_command_safety(&operator_next_action_id, &primary_next_command);
    if binary_identity.differs_from_path_soma() {
        let current = binary_identity.current_exe.as_deref().unwrap_or("<current-exe>");
        let path_soma = binary_identity.path_soma.as_deref().unwrap_or("<path-soma>");
        blocked_claims.push(format!(
            "`soma` on PATH ({path_soma}) differs from the running binary ({current}); reinstall or run the displayed command with the intended binary before judging current CLI/MCP readiness."
        ));
    }
    if !binary_identity_errors.is_empty() {
        blocked_claims.push(format!(
            "Binary identity diagnostic was partial: {}.",
            binary_identity_errors.join("; ")
        ));
    }
    let primary_external_action_safety = private_app_next_actions
        .iter()
        .find(|action| {
            action.operator_next_action_id == operator_next_action_id
                && action.external_action_safety.is_some()
        })
        .and_then(|action| action.external_action_safety.clone());
    let primary_artifact_repair_summary = private_app_next_actions
        .iter()
        .find(|action| {
            action.artifact_repair_summary.is_some()
                && (primary_client.as_ref().is_some_and(|client| action.client == *client)
                    || action.operator_next_action_id == operator_next_action_id)
        })
        .and_then(|action| action.artifact_repair_summary.clone());
    let current_session_safety =
        current_session_safety(&operator_next_action_id, &primary_next_command);
    let strict_private_client_hardening_command =
        command_with_current_binary_when_path_soma_differs(
            strict_private_client_hardening_command(project_filter),
            &binary_identity,
        );

    ClientOperatorCard {
        source: "soma_clients.operator_card.v1",
        status,
        operator_next_action_id,
        operator_next_action_label,
        primary_client,
        headline,
        primary_next_step,
        primary_next_command,
        binary_identity,
        primary_next_command_safety,
        primary_external_action_safety,
        primary_artifact_repair_summary,
        current_session_safety,
        mcp_ready_clients,
        runtime_detected_clients,
        runtime_missing_clients,
        runtime_not_cli_detectable_clients,
        runtime_check_commands,
        proof_storage_recovery_commands,
        private_app_restart_recommended_clients,
        private_app_restart_commands,
        continue_extension_config_not_visible_clients,
        private_app_hook_trigger_ready_clients,
        private_app_real_hook_ready_clients,
        private_app_observed_app_hook_recordable_clients,
        private_app_next_actions,
        private_app_collector_start_commands,
        private_app_wait_commands,
        private_app_hook_integration_templates,
        private_app_release_plan,
        private_app_release_proof_checklist,
        strict_private_client_hardening_required_clients:
            strict_private_client_hardening_required_clients(),
        strict_private_client_hardening_command,
        observed_capture_dogfood_clients,
        observed_capture_dogfood_evidence,
        explicit_capture_ready_clients,
        capture_dogfood_matrix,
        private_capture_ready_clients,
        blocked_private_clients,
        blocked_claims,
        safe_to_claim,
        trust_boundary: "read_only_operator_card: summarizes existing MCP checks, proof rows, and semantic review status; records no proof, installs no hook, creates no verification event, and promotes no cloud draft",
    }
}

fn command_with_current_binary_when_path_soma_differs(
    command: Vec<String>,
    binary_identity: &BinaryIdentity,
) -> Vec<String> {
    crate::cli::binary_identity::command_with_current_binary_when_path_soma_differs(
        command,
        binary_identity,
    )
}

fn command_text_with_current_binary_when_path_soma_differs(command: Vec<String>) -> String {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();
    command_with_current_binary_when_path_soma_differs(command, &binary_identity).join(" ")
}

fn command_text_with_current_binary_for_args(command: &str, args: &[&str]) -> String {
    let mut parts = vec!["soma".to_string()];
    parts.extend(command.split_whitespace().map(ToOwned::to_owned));
    parts.extend(args.iter().map(|part| (*part).to_string()));
    command_text_with_current_binary_when_path_soma_differs(parts)
}

fn soma_clients_command_text(args: &[&str]) -> String {
    command_text_with_current_binary_for_args("clients", args)
}

fn soma_mcp_config_command_text(args: &[&str]) -> String {
    command_text_with_current_binary_for_args("mcp-config", args)
}

fn adapter_binding_proof_command_text(args: &[&str]) -> String {
    command_text_with_current_binary_for_args("adapter-binding-proof", args)
}

fn adapter_binding_proof_client_command_text(client: &str, args: &[&str]) -> String {
    let mut parts = vec!["--client", client];
    parts.extend_from_slice(args);
    adapter_binding_proof_command_text(&parts)
}

fn primary_real_cli_probe_blocker(
    matrix: &[ClientCaptureDogfoodMatrixItem],
) -> Option<&ClientCaptureDogfoodMatrixItem> {
    matrix.iter().find(|item| real_cli_probe_blocking_status(item).is_some())
}

fn real_cli_probe_blocking_status(item: &ClientCaptureDogfoodMatrixItem) -> Option<&'static str> {
    if let Some(raw_status) = item.last_real_cli_probe_status.as_deref() {
        match raw_status {
            "mcp_write_approval_required" => return Some("real_cli_mcp_write_approval_required"),
            "auth_blocked" => return Some("real_cli_auth_blocked"),
            "host_permission_blocked" => return Some("real_cli_host_permission_blocked"),
            "runtime_missing" => return Some("real_cli_runtime_missing"),
            "cli_invocation_failed" => return Some("real_cli_probe_failed"),
            "mcp_tool_visible_but_not_executed" => {
                return Some("real_cli_mcp_tool_visible_not_executed");
            }
            _ => {}
        }
    }
    match item.status.as_str() {
        "real_cli_mcp_write_approval_required"
        | "real_cli_auth_blocked"
        | "real_cli_host_permission_blocked"
        | "real_cli_runtime_missing"
        | "real_cli_probe_failed"
        | "real_cli_mcp_tool_visible_not_executed"
        | "real_cli_capture_not_observed" => Some(match item.status.as_str() {
            "real_cli_mcp_write_approval_required" => "real_cli_mcp_write_approval_required",
            "real_cli_auth_blocked" => "real_cli_auth_blocked",
            "real_cli_host_permission_blocked" => "real_cli_host_permission_blocked",
            "real_cli_runtime_missing" => "real_cli_runtime_missing",
            "real_cli_probe_failed" => "real_cli_probe_failed",
            "real_cli_mcp_tool_visible_not_executed" => "real_cli_mcp_tool_visible_not_executed",
            _ => "real_cli_capture_not_observed",
        }),
        _ => None,
    }
}

fn real_cli_probe_operator_next_action_id(status: &str) -> &'static str {
    match status {
        "real_cli_auth_blocked" => "configure_cli_auth_for_real_dogfood",
        "real_cli_mcp_write_approval_required" => "approve_real_cli_mcp_capture_write",
        "real_cli_host_permission_blocked" => "rerun_real_cli_probe_from_normal_terminal",
        "real_cli_runtime_missing" => "install_or_expose_missing_client_runtime",
        _ => "rerun_real_cli_dogfood_probe",
    }
}

fn real_cli_probe_operator_next_action_label(status: &str) -> &'static str {
    match status {
        "real_cli_auth_blocked" => "Configure CLI auth",
        "real_cli_mcp_write_approval_required" => "Approve real CLI MCP capture write",
        "real_cli_host_permission_blocked" => "Run probe from normal terminal",
        "real_cli_runtime_missing" => "Install or expose missing client runtime",
        _ => "Rerun real CLI dogfood probe",
    }
}

fn real_cli_probe_blocked_for_summary(row: &ClientStatusRow) -> bool {
    row.real_cli_dogfood_probe.as_ref().is_some_and(|probe| {
        matches!(
            probe.status.as_str(),
            "real_cli_auth_blocked"
                | "real_cli_mcp_write_approval_required"
                | "real_cli_host_permission_blocked"
        )
    })
}

fn real_cli_probe_failed_for_summary(row: &ClientStatusRow) -> bool {
    row.real_cli_dogfood_probe.as_ref().is_some_and(|probe| {
        matches!(
            probe.status.as_str(),
            "real_cli_runtime_missing"
                | "real_cli_probe_failed"
                | "real_cli_mcp_tool_visible_not_executed"
                | "real_cli_capture_not_observed"
                | "real_cli_probe_unknown"
        )
    })
}

fn build_capture_dogfood_matrix(
    rows: &[ClientStatusRow],
    real_cli_probe: Option<&ClientRealCliDogfoodProbeReport>,
) -> Vec<ClientCaptureDogfoodMatrixItem> {
    rows.iter()
        .map(|row| {
            let observed = row.observed_capture_dogfood_evidence.as_ref();
            let probe_attempt = real_cli_probe_attempt_for(real_cli_probe, row.client);
            let explicit_cli_capture_available = row.goal_status == "explicit_cli_capture_available";
            let private_release_proof_ready =
                row.capture_model == PRIVATE_APP_CAPTURE_MODEL && row.ready_for_private_client_claim;
            let observed_local_capture = observed.is_some();
            let status = if observed_local_capture {
                "observed_local_capture"
            } else if let Some(attempt) = probe_attempt {
                real_cli_probe_matrix_status(attempt, explicit_cli_capture_available)
            } else if explicit_cli_capture_available {
                "explicit_cli_capture_ready_unobserved"
            } else if private_release_proof_ready {
                "private_release_proof_ready_unobserved"
            } else if row.mcp_registration_ready {
                "mcp_ready_unobserved"
            } else {
                "not_ready"
            };
            ClientCaptureDogfoodMatrixItem {
                source: "soma_clients.capture_dogfood_matrix.v1",
                client: row.client.to_string(),
                capture_model: row.capture_model.to_string(),
                mcp_registration_ready: row.mcp_registration_ready,
                explicit_cli_capture_available,
                observed_local_capture,
                private_release_proof_ready,
                status: status.to_string(),
                last_real_cli_probe_status: probe_attempt.map(|attempt| attempt.status.clone()),
                last_real_cli_probe_next_action: probe_attempt
                    .and_then(|attempt| attempt.next_action.clone()),
                last_real_cli_probe_artifact_path: real_cli_probe.map(|probe| probe.path.clone()),
                last_real_cli_probe_generated_at_unix_ms: real_cli_probe
                    .and_then(|probe| probe.generated_at_unix_ms),
                evidence_ref: observed.map(|evidence| evidence.evidence_ref.clone()),
                project: observed.and_then(|evidence| evidence.project.clone()),
                session_id: observed.and_then(|evidence| evidence.session_id.clone()),
                next_command: capture_dogfood_matrix_next_command(row, observed),
                trust_boundary:
                    "capture_dogfood_matrix_is_read_only: separates configured/ready client paths from stored local capture evidence; records no proof row, creates no verification event, installs no hook, and cannot substitute for private app-hook/render/review-action proof",
            }
        })
        .collect()
}

fn attach_real_cli_dogfood_probe(
    rows: &mut [ClientStatusRow],
    real_cli_probe: Option<&ClientRealCliDogfoodProbeReport>,
) {
    let Some(real_cli_probe) = real_cli_probe else {
        return;
    };
    for row in rows {
        let Some(attempt) = real_cli_probe_attempt_for(Some(real_cli_probe), row.client) else {
            continue;
        };
        let explicit_cli_capture_available = row.goal_status == "explicit_cli_capture_available";
        let status = real_cli_probe_matrix_status(attempt, explicit_cli_capture_available);
        row.real_cli_dogfood_probe = Some(ClientRealCliDogfoodProbeClientStatus {
            source: "soma_clients.real_cli_dogfood_probe_client_status.v1",
            client: attempt.client.clone(),
            status: status.to_string(),
            raw_status: attempt.status.clone(),
            report_path: real_cli_probe.path.clone(),
            report_status: real_cli_probe.report_status.clone(),
            generated_at_unix_ms: real_cli_probe.generated_at_unix_ms,
            artifact_modified_at_unix_ms: real_cli_probe.artifact_modified_at_unix_ms,
            observed_local_capture: attempt.observed_local_capture,
            project: attempt.project.clone(),
            session_id: attempt.session_id.clone(),
            marker: attempt.marker.clone(),
            jsonl_path: attempt.jsonl_path.clone(),
            stderr_path: attempt.stderr_path.clone(),
            last_message_path: attempt.last_message_path.clone(),
            next_action: attempt.next_action.clone(),
            trust_boundary:
                "real_cli_dogfood_probe_client_status_is_read_only: mirrors the latest observational real CLI probe for this client; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
        });
        if attempt.observed_local_capture {
            row.safe_to_claim.push(format!(
                "Latest real CLI dogfood probe observed local capture for {}.",
                row.client
            ));
        } else {
            let next = attempt.next_action.as_deref().unwrap_or(
                "inspect the real CLI dogfood probe artifact and rerun after fixing client access",
            );
            row.blocked_claims.push(format!(
                "Latest real CLI dogfood probe for {} is `{}`; {next}.",
                row.client, attempt.status
            ));
        }
        refresh_client_status_aliases(row);
    }
}

fn real_cli_probe_attempt_for<'a>(
    report: Option<&'a ClientRealCliDogfoodProbeReport>,
    client: &str,
) -> Option<&'a ClientRealCliDogfoodProbeAttempt> {
    report?.attempts.iter().find(|attempt| attempt.client == client)
}

fn real_cli_probe_matrix_status(
    attempt: &ClientRealCliDogfoodProbeAttempt,
    explicit_cli_capture_available: bool,
) -> &'static str {
    match attempt.status.as_str() {
        "capture_observed" if attempt.observed_local_capture => {
            "real_cli_capture_observed_outside_recent_scan"
        }
        "mcp_write_approval_required" => "real_cli_mcp_write_approval_required",
        "auth_blocked" => "real_cli_auth_blocked",
        "host_permission_blocked" => "real_cli_host_permission_blocked",
        "runtime_missing" => "real_cli_runtime_missing",
        "cli_invocation_failed" => "real_cli_probe_failed",
        "mcp_tool_visible_but_not_executed" => "real_cli_mcp_tool_visible_not_executed",
        "mcp_capture_not_observed" | "mcp_call_completed" => "real_cli_capture_not_observed",
        _ if explicit_cli_capture_available => "explicit_cli_capture_ready_unobserved",
        _ => "real_cli_probe_unknown",
    }
}

fn capture_dogfood_matrix_next_command(
    row: &ClientStatusRow,
    observed: Option<&ClientObservedCaptureDogfoodEvidence>,
) -> Vec<String> {
    if let Some(evidence) = observed {
        return evidence.recall_command.clone();
    }
    if row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL {
        return vec![
            "tools/real-cli-dogfood-probe.sh".to_string(),
            "--client".to_string(),
            row.client.to_string(),
        ];
    }
    if row.capture_model == PRIVATE_APP_CAPTURE_MODEL {
        return vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--client".to_string(),
            row.client.to_string(),
            "--proof-session".to_string(),
            "--brief".to_string(),
        ];
    }
    vec!["soma".to_string(), "clients".to_string(), "--json".to_string()]
}

fn build_private_app_restart_commands(
    rows: &[ClientStatusRow],
) -> Vec<ClientPrivateAppRestartCommand> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter_map(|row| {
            let operator_next_action_id = private_app_operator_next_action_id(row);
            if operator_next_action_id != "restart_or_reopen_codex_app_before_real_hook"
                || !codex_app_manual_restart_required(row)
            {
                return None;
            }
            Some(ClientPrivateAppRestartCommand {
                client: row.client.to_string(),
                goal_status: row.goal_status.clone(),
                operator_next_action_id,
                restart_recommended: row
                    .codex_notify_reload_check
                    .as_ref()
                    .is_some_and(|check| check.restart_recommended),
                manual_restart_required: true,
                execution_safety: ClientPrivateAppRestartExecutionSafety {
                    run_from_separate_terminal_required: true,
                    disrupts_current_client_session: true,
                },
                quit_command: codex_app_quit_command_for_row(row)?,
                reopen_command: codex_app_reopen_command_for_row(row)?,
                expected_event_source: row.expected_event_source.clone(),
                binding_nonce: row.binding_nonce.clone(),
                event_jsonl_path: row.event_jsonl_path.clone(),
                follow_up_wait_command: row.private_event_wait_command.clone(),
                simple_follow_up_wait_command: row.simple_private_event_wait_command.clone(),
                instruction: private_app_restart_instruction(row),
                trust_boundary: "private_app_restart_command_is_read_only: exposes the bounded operator quit/reopen hints for reloading a stale private client config only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for observed_app_hook evidence",
            })
        })
        .collect()
}

fn private_app_restart_instruction(row: &ClientStatusRow) -> String {
    format!(
        "Run the quit/reopen commands from a separate terminal because they can close the current {} session, complete a real {} turn after reopening, then use the follow-up wait/proof-session command before recording observed_app_hook proof.",
        row.display_name, row.display_name
    )
}

fn build_private_app_wait_commands(rows: &[ClientStatusRow]) -> Vec<ClientPrivateAppWaitCommand> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| !row.ready_for_private_client_claim)
        .filter(|row| row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook"))
        .filter(|row| row.installed_config_private_target_eligible_candidates.unwrap_or_default() > 0)
        .filter_map(|row| {
            let wait_command = row.private_event_wait_command.clone()?;
            let operator_next_action_id = private_app_operator_next_action_id(row);
            if !private_app_wait_command_actionable(&operator_next_action_id) {
                return None;
            }
            let manual_restart_required = codex_app_manual_restart_required(row);
            Some(ClientPrivateAppWaitCommand {
                client: row.client.to_string(),
                goal_status: row.goal_status.clone(),
                operator_next_action_id,
                restart_recommended: row
                    .codex_notify_reload_check
                    .as_ref()
                    .is_some_and(|check| check.restart_recommended),
                manual_restart_required,
                quit_hint_command: manual_restart_required
                    .then(|| codex_app_quit_command_for_row(row))
                    .flatten(),
                reopen_hint_command: manual_restart_required
                    .then(|| codex_app_reopen_command_for_row(row))
                    .flatten(),
                expected_event_source: row.expected_event_source.clone(),
                binding_nonce: row.binding_nonce.clone(),
                event_jsonl_path: row.event_jsonl_path.clone(),
                wait_command,
                simple_wait_command: row.simple_private_event_wait_command.clone(),
                watch_command: row.private_event_watch_command.clone(),
                instruction: private_app_wait_instruction(row),
                external_action_safety: private_app_external_action_safety(row),
                external_action: row.proof_session_external_action.clone(),
                trust_boundary: "private_app_wait_command_is_read_only: exposes the bounded operator wait/watch commands for a real private client hook only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for observed_app_hook evidence",
            })
        })
        .collect()
}

fn private_app_wait_command_actionable(operator_next_action_id: &str) -> bool {
    matches!(
        operator_next_action_id,
        "trigger_real_private_client_hook_to_write_private_spool_event"
            | "restart_or_reopen_codex_app_before_real_hook"
    )
}

fn private_app_wait_instruction(row: &ClientStatusRow) -> String {
    if row.codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended) {
        return "Quit or restart the stale Codex app process, reopen it, complete a real turn, then rerun the wait command until the matching private event appears.".to_string();
    }
    if row.continue_extension_config_check.is_some() {
        return "Reload Continue, complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the wait command until the matching private event appears.".to_string();
    }
    format!(
        "Complete a real {} action, then rerun the wait command until the matching private event appears.",
        row.display_name
    )
}

fn continue_devdata_collector_start_required(
    row: &ClientStatusRow,
    operator_next_action_id: &str,
) -> bool {
    row.client == "continue"
        && operator_next_action_id == "start_continue_devdata_collector_before_real_hook"
        && row
            .continue_extension_config_check
            .as_ref()
            .is_some_and(continue_devdata_collector_start_needed)
}

fn build_private_app_collector_start_commands(
    rows: &[ClientStatusRow],
) -> Vec<ClientPrivateAppCollectorStartCommand> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter_map(|row| {
            let operator_next_action_id = private_app_operator_next_action_id(row);
            if !continue_devdata_collector_start_required(row, &operator_next_action_id) {
                return None;
            }
            let check = row.continue_extension_config_check.as_ref()?;
            Some(ClientPrivateAppCollectorStartCommand {
                client: row.client.to_string(),
                goal_status: row.goal_status.clone(),
                operator_next_action_id,
                collector_status: check.devdata_collector_status.to_string(),
                collector_listening: check.devdata_collector_listening,
                devdata_destination_visible: check.devdata_destination_visible,
                expected_event_source: row.expected_event_source.clone(),
                binding_nonce: row.binding_nonce.clone(),
                event_jsonl_path: row.event_jsonl_path.clone(),
                start_command: continue_devdata_collector_command_for_row(row)?,
                managed_start_command: continue_devdata_collector_managed_start_command_for_row(row)?,
                follow_up_wait_command: row.private_event_wait_command.clone(),
                simple_follow_up_wait_command: row.simple_private_event_wait_command.clone(),
                proof_session_command: private_app_proof_session_command(row),
                instruction: private_app_collector_start_instruction(row),
                trust_boundary: "private_app_collector_start_command_is_read_only: exposes the bounded operator command for starting the local private-client event collector only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for observed_app_hook evidence",
            })
        })
        .collect()
}

fn private_app_collector_start_instruction(row: &ClientStatusRow) -> String {
    if row.client == "continue" {
        return "Start the local Continue collector in a separate terminal, reload Continue, complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then use the follow-up wait/proof-session command before recording observed_app_hook proof."
            .to_string();
    }
    format!(
        "Start the local {} collector in a separate terminal, reload {}, complete a real {} turn, then use the follow-up wait/proof-session command before recording observed_app_hook proof.",
        row.display_name, row.display_name, row.display_name
    )
}

fn build_private_app_hook_integration_templates(
    rows: &[ClientStatusRow],
) -> Vec<ClientPrivateHookIntegrationTemplate> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .filter(|row| !row.ready_for_private_client_claim)
        .filter_map(|row| row.private_hook_integration_template.clone())
        .collect()
}

fn build_private_app_next_actions(rows: &[ClientStatusRow]) -> Vec<ClientPrivateAppNextAction> {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .map(|row| {
            let operator_next_action_id = private_app_operator_next_action_id(row);
            let manual_restart_required = codex_app_manual_restart_required(row);
            let continue_collector_start_required =
                continue_devdata_collector_start_required(row, &operator_next_action_id);
            let next_command = command_with_current_binary_when_path_soma_differs(
                primary_private_app_command(row),
                &binary_identity,
            );
            let current_session_action_safety =
                current_session_action_safety(&operator_next_action_id, &next_command);
            ClientPrivateAppNextAction {
                client: row.client.to_string(),
                goal_status: row.goal_status.clone(),
                ready_for_private_client_claim: row.ready_for_private_client_claim,
                artifact_failure_count: row.artifact_failure_count,
                coherence_failure_count: row.coherence_failure_count,
                artifact_repair_summary: row.artifact_repair_summary.clone(),
                release_gate_blockers: private_app_release_gate_blockers(row),
                proof_session_status: row.proof_session_status.clone(),
                proof_session_release_gate: row.proof_session_release_gate.clone(),
                proof_session_next_step_id: row.proof_session_next_step_id.clone(),
                operator_next_action_id: operator_next_action_id.clone(),
                operator_next_action_label: private_app_operator_next_action_label(row),
                current_session_action_safety,
                restart_recommended: row
                    .codex_notify_reload_check
                    .as_ref()
                    .is_some_and(|check| check.restart_recommended),
                manual_restart_required,
                quit_hint_command: manual_restart_required
                    .then(|| codex_app_quit_command_for_row(row))
                    .flatten(),
                reopen_hint_command: manual_restart_required
                    .then(|| codex_app_reopen_command_for_row(row))
                    .flatten(),
                private_event_observation_status: row
                    .private_event_observation
                    .as_ref()
                    .map(|observation| observation.status.to_string()),
                continue_extension_config_status: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|check| check.status.to_string()),
                continue_extension_config_visible: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|check| !continue_extension_config_not_visible(check)),
                continue_devdata_destination_visible: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|check| check.devdata_destination_visible),
                continue_devdata_collector_status: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|check| check.devdata_collector_status.to_string()),
                continue_devdata_collector_listening: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|check| check.devdata_collector_listening),
                continue_devdata_collector_start_required: row
                    .continue_extension_config_check
                    .as_ref()
                    .map(|_| continue_collector_start_required),
                continue_devdata_collector_start_command: continue_collector_start_required
                    .then(|| continue_devdata_collector_command_for_row(row))
                    .flatten(),
                continue_devdata_collector_managed_start_command: continue_collector_start_required
                    .then(|| continue_devdata_collector_managed_start_command_for_row(row))
                    .flatten(),
                external_action_safety: private_app_external_action_safety(row),
                external_action: row.proof_session_external_action.clone(),
                missing_proof_levels: row.missing_proof_levels.clone(),
                next_step: private_app_operator_next_step_with_command(row, Some(&next_command)),
                next_command,
                trust_boundary: "private_app_next_action_is_read_only: summarizes one private app client row, artifact repair hints, and current proof-session guidance only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
            }
        })
        .collect()
}

fn build_private_app_release_plan(
    rows: &[ClientStatusRow],
) -> Vec<ClientPrivateAppReleasePlanItem> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .map(|row| {
            let completed_proof_levels = row
                .proof_level_statuses
                .iter()
                .filter(|status| status.status == "recorded")
                .map(|status| status.proof_level)
                .collect::<Vec<_>>();
            ClientPrivateAppReleasePlanItem {
                client: row.client.to_string(),
                status: private_app_release_status(row).to_string(),
                current_stage: private_app_release_current_stage(row),
                ready_for_private_client_claim: row.ready_for_private_client_claim,
                requires_external_client_action: private_app_requires_external_client_action(row),
                ready_to_record_now: !row.proof_session_ready_to_record_proof_levels.is_empty(),
                operator_next_action_id: private_app_operator_next_action_id(row),
                operator_next_action_label: private_app_operator_next_action_label(row),
                proof_level_statuses: row.proof_level_statuses.clone(),
                completed_proof_levels,
                missing_proof_levels: row.missing_proof_levels.clone(),
                next_required_proof_level: private_app_next_required_proof_level(row),
                release_gate_blockers: private_app_release_gate_blockers(row),
                next_command: primary_private_app_command(row),
                external_action_safety: private_app_external_action_safety(row),
                external_action: row.proof_session_external_action.clone(),
                trust_boundary: "private_app_release_plan_is_read_only: compresses current private-client proof progress and next operator action only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
            }
        })
        .collect()
}

fn private_app_external_action_safety(
    row: &ClientStatusRow,
) -> Option<ClientPrivateAppExternalActionSafety> {
    let action_id = private_app_operator_next_action_id(row);
    client_private_app_external_action_safety_for(row.client, row.display_name, &action_id)
}

fn client_private_app_external_action_safety_for(
    client: &str,
    display_name: &str,
    action_id: &str,
) -> Option<ClientPrivateAppExternalActionSafety> {
    let involves_real_turn = matches!(
        action_id,
        "restart_or_reopen_codex_app_before_real_hook"
            | "start_continue_devdata_collector_before_real_hook"
            | "trigger_real_private_client_hook_to_write_private_spool_event"
    );
    if !involves_real_turn {
        return None;
    }

    let suggested_minimal_test_prompt = match client {
        "continue" => "SOMA hook ping. Reply with exactly: SOMA_CONTINUE_HOOK_OK",
        "codex-app" => "SOMA hook ping. Reply with exactly: SOMA_CODEX_APP_HOOK_OK",
        _ => "SOMA hook ping. Reply with exactly: SOMA_PRIVATE_HOOK_OK",
    };
    Some(ClientPrivateAppExternalActionSafety {
        source: "soma_clients.private_app_external_action_safety.v1",
        classification: "real_private_client_action_may_send_prompt_to_provider",
        requires_operator_confirmation_before_submission: true,
        may_transmit_prompt_to_provider: true,
        suggested_minimal_test_prompt: suggested_minimal_test_prompt.to_string(),
        forbidden_inputs: vec![
            "secrets",
            "API keys",
            "credentials",
            "private customer data",
            "proprietary source snippets",
            "large workspace context",
        ],
        reason: format!(
            "A real {} action is required to prove the private hook path, but submitting chat/edit text may be sent through the configured client provider. Use only the minimal hook-ping prompt unless the operator explicitly approves broader context.",
            display_name
        ),
        trust_boundary: "private_app_external_action_safety_is_read_only: documents the operator privacy boundary for the next real client action only; it records no proof row, creates no verification event, submits no prompt, and cannot substitute for observed app-hook/render/review-action evidence",
    })
}

fn build_private_app_release_proof_checklist(
    rows: &[ClientStatusRow],
) -> Vec<ClientPrivateAppReleaseProofChecklist> {
    rows.iter()
        .filter(|row| row.capture_model == PRIVATE_APP_CAPTURE_MODEL)
        .map(|row| {
            let blockers = private_app_release_gate_blockers(row);
            let next_required_proof_level = private_app_next_required_proof_level(row);
            ClientPrivateAppReleaseProofChecklist {
                client: row.client.to_string(),
                status: private_app_release_status(row).to_string(),
                ready_for_private_client_claim: row.ready_for_private_client_claim,
                required_proof_levels: private_app_required_proof_levels(),
                proof_level_statuses: row.proof_level_statuses.clone(),
                missing_proof_levels: row.missing_proof_levels.clone(),
                ready_to_record_proof_levels: row
                    .proof_session_ready_to_record_proof_levels
                    .clone(),
                ready_to_record_now: !row.proof_session_ready_to_record_proof_levels.is_empty(),
                next_required_proof_level: next_required_proof_level.clone(),
                next_proof_step_id: row.proof_session_next_step_id.clone(),
                release_gate_blockers: blockers,
                completion_criteria: private_app_release_completion_criteria(),
                proof_session_command: private_app_proof_session_command(row),
                release_runbook_command: private_app_release_runbook_command(row),
                simple_release_runbook_command: Some(private_app_release_runbook_cli_command(row)),
                target_install_command: private_target_config_install_command(row)
                    .map(ToOwned::to_owned),
                hook_readiness_command: private_hook_readiness_command(row).map(ToOwned::to_owned),
                simple_hook_readiness_command: row.simple_private_hook_readiness_command.clone(),
                next_command: primary_private_app_command(row),
                next_recording_after_trusted_evidence: private_app_recording_hint(
                    row.client,
                    next_required_proof_level.as_deref(),
                ),
                external_action: row.proof_session_external_action.clone(),
                trust_boundary: "private_app_release_proof_checklist_is_read_only: exposes the concrete release proof criteria and current operator commands for one private app client; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
            }
        })
        .collect()
}

fn build_private_app_release_snapshot(
    operator_card: &ClientOperatorCard,
    client_filter: Option<String>,
) -> ClientPrivateAppReleaseSnapshot {
    let scope = if client_filter.is_some() { "filtered_client" } else { "all_clients" };
    let scope_description = client_filter.as_ref().map_or_else(
        || {
            "Full supported-client readiness scope; equivalent to omitting --client or passing --client all."
                .to_string()
        },
        |client| {
            format!(
                "Filtered readiness scope for `{client}`; counts and pending clients are not the global private-app release state."
            )
        },
    );
    let private_app_client_count = operator_card.private_app_release_plan.len();
    let ready_clients = operator_card.private_capture_ready_clients.clone();
    let pending_clients = operator_card.blocked_private_clients.clone();
    let release_ready_count = ready_clients.len();
    let release_pending_count = pending_clients.len();
    let ready = private_app_client_count > 0 && release_pending_count == 0;
    let status = if private_app_client_count == 0 {
        "not_applicable"
    } else if operator_card.status == "proof_storage_unavailable" {
        "proof_storage_unavailable"
    } else if ready {
        "ready"
    } else if release_ready_count > 0 {
        "partial"
    } else {
        "pending"
    }
    .to_string();

    let primary_pending = operator_card
        .private_app_release_plan
        .iter()
        .find(|item| !item.ready_for_private_client_claim);
    let primary_pending_client = primary_pending.map(|item| item.client.clone());
    let primary_release_gate_blockers =
        primary_pending.map(|item| item.release_gate_blockers.clone()).unwrap_or_default();
    let primary_missing_proof_levels =
        primary_pending.map(|item| item.missing_proof_levels.clone()).unwrap_or_default();
    let primary_next_required_proof_level =
        primary_pending.and_then(|item| item.next_required_proof_level.clone());
    let primary_next_proof_step_id = primary_pending.and_then(|item| {
        operator_card
            .private_app_release_proof_checklist
            .iter()
            .find(|checklist| checklist.client == item.client)
            .and_then(|checklist| checklist.next_proof_step_id.clone())
    });

    let pending_actions = operator_card
        .private_app_release_plan
        .iter()
        .filter(|item| !item.ready_for_private_client_claim)
        .map(|item| {
            let next_proof_step_id = operator_card
                .private_app_release_proof_checklist
                .iter()
                .find(|checklist| checklist.client == item.client)
                .and_then(|checklist| checklist.next_proof_step_id.clone());
            let next_command_safety =
                primary_next_command_safety(&item.operator_next_action_id, &item.next_command);
            ClientPrivateAppReleaseSnapshotAction {
                client: item.client.clone(),
                status: item.status.clone(),
                ready_for_private_client_claim: item.ready_for_private_client_claim,
                operator_next_action_id: item.operator_next_action_id.clone(),
                operator_next_action_label: item.operator_next_action_label.clone(),
                release_gate_blockers: item.release_gate_blockers.clone(),
                missing_proof_levels: item.missing_proof_levels.clone(),
                next_required_proof_level: item.next_required_proof_level.clone(),
                next_proof_step_id,
                requires_external_client_action: item.requires_external_client_action,
                ready_to_record_now: item.ready_to_record_now,
                has_restart_command: operator_card
                    .private_app_restart_commands
                    .iter()
                    .any(|command| command.client == item.client),
                has_collector_start_command: operator_card
                    .private_app_collector_start_commands
                    .iter()
                    .any(|command| command.client == item.client),
                has_wait_command: operator_card
                    .private_app_wait_commands
                    .iter()
                    .any(|command| command.client == item.client),
                next_command: item.next_command.clone(),
                next_command_safety,
                external_action_safety: item.external_action_safety.clone(),
                external_action: item.external_action.clone(),
                trust_boundary: "private_app_release_snapshot_action_is_read_only: summarizes one pending private-app release gate from existing operator_card data only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft",
            }
        })
        .collect();

    ClientPrivateAppReleaseSnapshot {
        source: "soma_clients.private_app_release_snapshot.v1",
        scope,
        client_filter,
        scope_description,
        status,
        ready,
        private_app_client_count,
        release_ready_count,
        release_pending_count,
        ready_clients,
        pending_clients,
        primary_pending_client,
        operator_status: operator_card.status.clone(),
        operator_next_action_id: operator_card.operator_next_action_id.clone(),
        operator_next_action_label: operator_card.operator_next_action_label.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        primary_release_gate_blockers,
        primary_missing_proof_levels,
        primary_next_required_proof_level,
        primary_next_proof_step_id,
        pending_actions,
        trust_boundary: "private_app_release_snapshot_is_read_only: top-level scoped release-gate summary derived from the current proof ledger's operator_card only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for observed_app_hook, observed_in_client_render, or observed_review_action proof rows",
    }
}

fn private_app_release_status(row: &ClientStatusRow) -> &'static str {
    if row.ready_for_private_client_claim {
        "ready"
    } else if row.proof_storage_status != "available" {
        "proof_storage_unavailable"
    } else if !row.proof_session_ready_to_record_proof_levels.is_empty() {
        "ready_to_record_proof"
    } else {
        "pending"
    }
}

fn private_app_release_current_stage(row: &ClientStatusRow) -> String {
    if row.ready_for_private_client_claim {
        "release_gate_passed".to_string()
    } else if let Some(step) = &row.proof_session_next_step_id {
        step.clone()
    } else if row.proof_session_error.is_some() {
        "proof_session_error".to_string()
    } else {
        "inspect_readiness".to_string()
    }
}

fn private_app_requires_external_client_action(row: &ClientStatusRow) -> bool {
    if row.ready_for_private_client_claim
        || !row.proof_session_ready_to_record_proof_levels.is_empty()
    {
        return false;
    }
    matches!(
        private_app_operator_next_action_id(row).as_str(),
        "restart_or_reopen_codex_app_before_real_hook"
            | "merge_continue_mcp_config_before_real_hook"
            | "install_or_enable_continue_extension_before_real_hook"
            | "start_continue_devdata_collector_before_real_hook"
            | "trigger_real_private_client_hook_to_write_private_spool_event"
    )
}

fn private_app_required_proof_levels() -> Vec<&'static str> {
    vec!["observed_app_hook", "observed_in_client_render", "observed_review_action"]
}

fn private_app_next_required_proof_level(row: &ClientStatusRow) -> Option<String> {
    if row.ready_for_private_client_claim {
        return None;
    }
    row.proof_session_ready_to_record_proof_levels
        .first()
        .cloned()
        .or_else(|| row.missing_proof_levels.first().map(|level| (*level).to_string()))
        .or_else(|| {
            private_app_next_required_proof_level_for_step(
                row.proof_session_next_step_id.as_deref(),
            )
            .map(str::to_string)
        })
}

fn private_app_next_required_proof_level_for_step(step_id: Option<&str>) -> Option<&'static str> {
    match step_id {
        Some(
            "restart_or_reopen_codex_app_before_real_hook"
            | "install_or_merge_private_client_config"
            | "check_continue_devdata_collector_status"
            | "start_continue_devdata_collector"
            | "trigger_private_client_hook"
            | "record_observed_app_hook",
        ) => Some("observed_app_hook"),
        Some(
            "render_review_surface"
            | "capture_in_client_render_evidence"
            | "record_observed_in_client_render",
        ) => Some("observed_in_client_render"),
        Some("execute_rendered_review_control" | "record_observed_review_action") => {
            Some("observed_review_action")
        }
        _ => None,
    }
}

fn private_app_release_completion_criteria() -> Vec<&'static str> {
    vec![
        "current private-client target config is discoverable and carries the same binding nonce as the proof chain",
        "observed_app_hook proof is recorded from a real private client event with matching event_source, binding_nonce, writer metadata, temporal binding, and operator confirmation",
        "observed_in_client_render proof is recorded from structured UI-only render evidence bound to the review-render report, visible surfaces, interaction contract, and installed config",
        "observed_review_action proof is recorded from a rendered control_id that produced a storage-gated review-action report with non-cloud verification evidence",
        "stored evidence artifacts replay without byte-length or fingerprint changes",
    ]
}

fn private_app_proof_session_command(row: &ClientStatusRow) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "adapter-binding-proof".to_string(),
        "--client".to_string(),
        row.client.to_string(),
        "--proof-session".to_string(),
        "--json".to_string(),
    ];
    if let Some(artifact_dir) = row.artifact_repair_plan.as_ref().and_then(effective_artifact_dir) {
        command.extend(["--artifact-dir".to_string(), artifact_dir]);
    }
    command
}

fn private_app_proof_session_brief_command(row: &ClientStatusRow) -> Vec<String> {
    private_app_proof_session_brief_command_for_client_with_artifact_dir(
        row.client,
        row.artifact_repair_plan.as_ref().and_then(effective_artifact_dir).as_deref(),
    )
}

fn private_app_proof_session_brief_command_for_client(client: &str) -> Vec<String> {
    private_app_proof_session_brief_command_for_client_with_artifact_dir(client, None)
}

fn private_app_proof_session_brief_command_for_client_with_artifact_dir(
    client: &str,
    artifact_dir: Option<&str>,
) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "adapter-binding-proof".to_string(),
        "--client".to_string(),
        client.to_string(),
        "--proof-session".to_string(),
        "--brief".to_string(),
    ];
    if let Some(artifact_dir) = artifact_dir.filter(|value| !value.trim().is_empty()) {
        command.extend(["--artifact-dir".to_string(), artifact_dir.to_string()]);
    }
    command
}

fn private_app_release_runbook_command(row: &ClientStatusRow) -> Vec<String> {
    let mut command =
        private_hook_readiness_command(row).map(ToOwned::to_owned).unwrap_or_else(|| {
            vec![
                "env".to_string(),
                format!("SOMA_CLIENT_BINDING_CLIENT={}", row.client),
                "tools/soma-client-release-proof-runbook.sh".to_string(),
            ]
        });
    let script = "tools/soma-client-release-proof-runbook.sh";
    if let Some(part) =
        command.iter_mut().find(|part| part.as_str() == "tools/soma-client-hook-readiness.sh")
    {
        *part = script.to_string();
    } else if !command.iter().any(|part| part == script) {
        command.push(script.to_string());
    }
    insert_env_part_before_script(
        &mut command,
        "SOMA_CLIENT_RELEASE_PROOF_MODE",
        Some("read_only"),
        script,
    );
    command
}

fn private_app_release_runbook_cli_command(row: &ClientStatusRow) -> Vec<String> {
    let mut command = row.simple_private_hook_readiness_command.clone().unwrap_or_else(|| {
        vec![
            "tools/soma-client-release-proof-runbook.sh".to_string(),
            "--client".to_string(),
            row.client.to_string(),
        ]
    });
    if let Some(script) =
        command.iter_mut().find(|part| part.as_str() == "tools/soma-client-hook-readiness.sh")
    {
        *script = "tools/soma-client-release-proof-runbook.sh".to_string();
    } else if !command.iter().any(|part| part == "tools/soma-client-release-proof-runbook.sh") {
        command.insert(0, "tools/soma-client-release-proof-runbook.sh".to_string());
    }
    if !command.windows(2).any(|pair| pair[0] == "--mode") {
        command.extend(["--mode".to_string(), "read_only".to_string()]);
    }
    command
}

fn pending_private_app_release_runbook_commands(
    operator_card: &ClientOperatorCard,
) -> Vec<Vec<String>> {
    operator_card
        .private_app_release_proof_checklist
        .iter()
        .filter(|checklist| !checklist.ready_for_private_client_claim)
        .map(|checklist| checklist.release_runbook_command.clone())
        .filter(|command| !command.is_empty())
        .collect()
}

fn push_next_command_once(next_commands: &mut Vec<Vec<String>>, command: Vec<String>) {
    if !command.is_empty() && !next_commands.contains(&command) {
        next_commands.push(command);
    }
}

fn primary_next_command_safety(
    operator_next_action_id: &str,
    command: &[String],
) -> ClientPrimaryNextCommandSafety {
    if operator_next_action_id == "restart_or_reopen_codex_app_before_real_hook" {
        return ClientPrimaryNextCommandSafety {
            source: "soma_clients.primary_next_command_safety.v1",
            classification: "disruptive_private_app_restart",
            run_from_separate_terminal_required: true,
            disrupts_current_client_session: true,
            requires_operator_confirmation: true,
            writes_local_files: false,
            records_proof: false,
            creates_verification_event: false,
            installs_hook: false,
            promotes_cloud_draft: false,
            reason: "The primary command can quit the active Codex app process; run it only from a separate terminal after accepting the session-disruption risk."
                .to_string(),
            trust_boundary: "primary_next_command_safety_is_read_only: classifies the displayed operator command only; it never executes the command, records proof, installs hooks, creates verification events, or promotes cloud drafts",
        };
    }

    let records_proof = primary_command_records_client_proof(command);
    let writes_local_files = primary_command_writes_local_files(command);
    let classification = if records_proof {
        "proof_recording_command"
    } else if writes_local_files {
        "local_file_write_command"
    } else {
        "read_only_or_diagnostic_command"
    };
    let reason = if records_proof {
        String::from(
            "The primary command appears to record client proof; require explicit operator confirmation and trusted non-cloud evidence before running it.",
        )
    } else if writes_local_files {
        String::from(
            "The primary command may write a local setup/config/report artifact; inspect the target path before running it.",
        )
    } else {
        String::from(
            "The primary command is treated as a read-only probe, report, or bounded wait by this readiness surface.",
        )
    };

    ClientPrimaryNextCommandSafety {
        source: "soma_clients.primary_next_command_safety.v1",
        classification,
        run_from_separate_terminal_required: false,
        disrupts_current_client_session: false,
        requires_operator_confirmation: records_proof || writes_local_files,
        writes_local_files,
        records_proof,
        creates_verification_event: false,
        installs_hook: false,
        promotes_cloud_draft: false,
        reason,
        trust_boundary: "primary_next_command_safety_is_read_only: classifies the displayed operator command only; it never executes the command, records proof, installs hooks, creates verification events, or promotes cloud drafts",
    }
}

fn current_session_safety(
    operator_next_action_id: &str,
    command: &[String],
) -> ClientCurrentSessionSafety {
    let detected = detect_current_client_session();
    let targets_current_session = detected.client.as_deref().is_some_and(|client| {
        primary_command_targets_client_session(client, operator_next_action_id, command)
    });
    let primary_command_safe_in_current_session = !targets_current_session;
    let recommended_execution_context = if targets_current_session {
        "separate_terminal_or_after_reopening_client"
    } else {
        "current_session_ok"
    }
    .to_string();
    let reason = if targets_current_session {
        match detected.client.as_deref() {
            Some("codex-app") => "The primary command targets the active Codex app process; running it from this Codex app session can terminate the current conversation before follow-up proof capture is visible.".to_string(),
            Some(client) => format!(
                "The primary command targets the detected {client} session; run it from a separate terminal or after switching sessions."
            ),
            None => "The primary command targets the current client session; run it from a separate terminal or after switching sessions.".to_string(),
        }
    } else if detected.client.is_some() {
        "The current client session was detected, and the primary command is not classified as targeting that same session.".to_string()
    } else {
        "No supported interactive client session was detected in the current environment."
            .to_string()
    };

    ClientCurrentSessionSafety {
        source: "soma_clients.current_session_safety.v1",
        detected_client: detected.client,
        detected_surface: detected.surface,
        current_thread_id: detected.thread_id,
        primary_command_targets_current_session: targets_current_session,
        primary_command_safe_in_current_session,
        recommended_execution_context,
        reason,
        trust_boundary: "current_session_safety_is_read_only: inspects local process environment and classifies the displayed primary command only; it never executes commands, records proof, installs hooks, creates verification events, or promotes cloud drafts",
    }
}

fn current_session_action_safety(
    operator_next_action_id: &str,
    command: &[String],
) -> ClientCurrentSessionActionSafety {
    let detected = detect_current_client_session();
    let targets_current_session = detected.client.as_deref().is_some_and(|client| {
        primary_command_targets_client_session(client, operator_next_action_id, command)
    });
    let action_safe_in_current_session = !targets_current_session;
    let recommended_execution_context = if targets_current_session {
        "separate_terminal_or_after_reopening_client"
    } else {
        "current_session_ok"
    }
    .to_string();
    let reason = if targets_current_session {
        match detected.client.as_deref() {
            Some("codex-app") => "This private-app action targets the active Codex app process; running it from this Codex app session can terminate the current conversation before follow-up proof capture is visible.".to_string(),
            Some(client) => format!(
                "This private-app action targets the detected {client} session; run it from a separate terminal or after switching sessions."
            ),
            None => "This private-app action targets the current client session; run it from a separate terminal or after switching sessions.".to_string(),
        }
    } else if detected.client.is_some() {
        "The current client session was detected, and this private-app action is not classified as targeting that same session.".to_string()
    } else {
        "No supported interactive client session was detected in the current environment."
            .to_string()
    };

    ClientCurrentSessionActionSafety {
        source: "soma_clients.current_session_action_safety.v1",
        detected_client: detected.client,
        detected_surface: detected.surface,
        current_thread_id: detected.thread_id,
        action_targets_current_session: targets_current_session,
        action_safe_in_current_session,
        recommended_execution_context,
        reason,
        trust_boundary: "current_session_action_safety_is_read_only: inspects local process environment and classifies one private-app action command only; it never executes commands, records proof, installs hooks, creates verification events, or promotes cloud drafts",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentClientSession {
    client: Option<String>,
    surface: String,
    thread_id: Option<String>,
}

fn detect_current_client_session() -> CurrentClientSession {
    let origin = env::var("CODEX_INTERNAL_ORIGINATOR_OVERRIDE").unwrap_or_default();
    let origin_lower = origin.to_ascii_lowercase();
    let thread_id = non_empty_env("CODEX_THREAD_ID");
    if origin_lower.contains("codex desktop") || origin_lower.contains("codex app") {
        return CurrentClientSession {
            client: Some("codex-app".to_string()),
            surface: "codex_desktop".to_string(),
            thread_id,
        };
    }
    if env::var_os("CODEX_SHELL").is_some() || env::var_os("CODEX_CI").is_some() {
        return CurrentClientSession {
            client: Some("codex-cli".to_string()),
            surface: "codex_shell".to_string(),
            thread_id,
        };
    }
    CurrentClientSession { client: None, surface: "unknown".to_string(), thread_id }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn primary_command_targets_client_session(
    client: &str,
    operator_next_action_id: &str,
    command: &[String],
) -> bool {
    match client {
        "codex-app" => {
            operator_next_action_id == "restart_or_reopen_codex_app_before_real_hook"
                || command.windows(3).any(|parts| {
                    parts[0] == "osascript"
                        && parts[1] == "-e"
                        && parts[2].contains("quit app \"Codex\"")
                })
                || command
                    .windows(3)
                    .any(|parts| parts[0] == "open" && parts[1] == "-a" && parts[2] == "Codex")
        }
        _ => false,
    }
}

fn primary_command_records_client_proof(command: &[String]) -> bool {
    let direct_record_command = command.iter().any(|part| part == "adapter-binding-proof")
        && command.iter().any(|part| part == "--proof-level")
        && command.iter().any(|part| {
            part == "--operator-confirm-release-grade-evidence"
                || part == "--operator-confirm-real-app-invocation"
                || part == "--operator-confirm-in-client-render"
                || part == "--operator-confirm-review-action"
        });
    let app_hook_wrapper_record_command =
        command.iter().any(|part| part == "tools/soma-client-record-app-hook-proof.sh")
            && command.iter().any(|part| part == "SOMA_CONFIRM_REAL_CLIENT_HOOK=1")
            && command.iter().any(|part| part == "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1");

    direct_record_command || app_hook_wrapper_record_command
}

fn primary_command_writes_local_files(command: &[String]) -> bool {
    command.iter().any(|part| {
        matches!(
            part.as_str(),
            "--write-installed-config"
                | "--write-render-evidence"
                | "--write-report"
                | "--json-out"
        )
    })
}

fn artifact_repair_primary_command(row: &ClientStatusRow) -> Option<Vec<String>> {
    let plan = row.artifact_repair_plan.as_ref()?;
    artifact_repair_primary_command_for_plan(row.client, plan)
}

fn artifact_repair_primary_command_for_plan(
    client: &str,
    plan: &ClientArtifactRepairPlan,
) -> Option<Vec<String>> {
    let review_render = effective_artifact_path(plan, "review_render_report")?;
    if !Path::new(&review_render).exists() {
        return Some(review_render_write_command(client, review_render));
    }
    let render_evidence = effective_artifact_path(plan, "render_evidence")?;
    if Path::new(&render_evidence).exists() {
        return Some(private_app_proof_session_brief_command_for_client_with_artifact_dir(
            client,
            effective_artifact_dir(plan).as_deref(),
        ));
    }
    let manifest = client_binding_manifest_for(client)?;
    Some(vec![
        "soma".to_string(),
        "adapter-binding-proof".to_string(),
        "--render-render-evidence".to_string(),
        "--manifest".to_string(),
        manifest.to_string(),
        "--review-render-report".to_string(),
        review_render,
        "--write-render-evidence".to_string(),
        render_evidence,
    ])
}

fn suggested_artifact_path(plan: &ClientArtifactRepairPlan, artifact_kind: &str) -> Option<String> {
    plan.suggested_artifact_paths
        .iter()
        .find(|suggestion| suggestion.artifact_kind == artifact_kind)
        .map(|suggestion| suggestion.path.clone())
}

fn effective_artifact_path(plan: &ClientArtifactRepairPlan, artifact_kind: &str) -> Option<String> {
    if should_use_workspace_artifact_fallback(plan) {
        plan.workspace_fallback_artifact_paths
            .iter()
            .find(|suggestion| suggestion.artifact_kind == artifact_kind)
            .map(|suggestion| suggestion.path.clone())
            .or_else(|| suggested_artifact_path(plan, artifact_kind))
    } else {
        suggested_artifact_path(plan, artifact_kind)
    }
}

fn should_use_workspace_artifact_fallback(plan: &ClientArtifactRepairPlan) -> bool {
    !artifact_dir_write_status_allows_new_files(&plan.suggested_artifact_dir_write_status)
        && !plan.workspace_fallback_artifact_paths.is_empty()
}

fn effective_artifact_dir(plan: &ClientArtifactRepairPlan) -> Option<String> {
    if should_use_workspace_artifact_fallback(plan) {
        plan.workspace_fallback_artifact_dir
            .clone()
            .or_else(|| Some(plan.suggested_artifact_dir.clone()))
    } else {
        Some(plan.suggested_artifact_dir.clone())
    }
}

fn artifact_dir_write_status_allows_new_files(status: &str) -> bool {
    matches!(status, "writable" | "parent_writable" | "not_checked_template")
}

fn review_render_write_command(client: &str, review_render: String) -> Vec<String> {
    vec![
        "soma".to_string(),
        "context".to_string(),
        "review-render".to_string(),
        "--client".to_string(),
        client.to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--write-report".to_string(),
        review_render,
    ]
}

fn client_binding_manifest_for(client: &str) -> Option<&'static str> {
    match client {
        "codex-app" => Some("tools/client-bindings/codex-app-soma-binding.json.example"),
        "cursor" => Some("tools/client-bindings/cursor-soma-binding.json.example"),
        "continue" => Some("tools/client-bindings/continue-soma-binding.json.example"),
        _ => None,
    }
}

fn command_renders_evidence(command: &[String]) -> bool {
    command.iter().any(|part| part == "--render-render-evidence")
}

fn primary_blocked_private_row<'a>(rows: &[&'a ClientStatusRow]) -> Option<&'a ClientStatusRow> {
    rows.iter().copied().min_by_key(|row| private_app_row_priority(row))
}

fn private_app_release_gate_blockers(row: &ClientStatusRow) -> Vec<String> {
    if row.ready_for_private_client_claim {
        return Vec::new();
    }

    let mut blockers = Vec::new();
    if !row.mcp_registration_ready {
        blockers.push("mcp_registration_not_ready".to_string());
    }
    if row.proof_storage_status != "available" {
        blockers.push("proof_storage_unavailable".to_string());
        return blockers;
    }
    if row.private_capture_status == "artifact_integrity_failed" {
        blockers.push("artifact_integrity_failed".to_string());
    }
    if row.private_capture_status == "stored_release_proof_but_private_target_config_missing"
        || row.goal_status == "private_app_target_config_required"
    {
        blockers.push("private_target_config_not_discoverable".to_string());
    }
    for level in &row.missing_proof_levels {
        blockers.push(format!("missing_{level}"));
    }
    if row.proof_session_error.is_some() {
        blockers.push("proof_session_unavailable".to_string());
    }
    match row.proof_session_next_step_id.as_deref() {
        Some("render_or_write_installed_config") => {
            blockers.push("installed_private_client_config_missing".to_string());
        }
        Some("trigger_private_client_hook") => {
            if row.codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended)
            {
                blockers.push("codex_app_restart_required".to_string());
            }
            if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_profile_config_invalid)
            {
                blockers.push("continue_profile_config_invalid".to_string());
            } else if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_extension_config_not_visible)
            {
                blockers.push("continue_mcp_config_not_visible".to_string());
            } else if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(|check| !check.extension_observed)
            {
                blockers.push("continue_extension_not_observed".to_string());
            } else if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_devdata_collector_probe_blocked)
            {
                blockers.push("continue_devdata_collector_probe_blocked".to_string());
            } else if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_devdata_collector_start_needed)
            {
                blockers.push("continue_devdata_collector_not_listening".to_string());
            }
            if private_app_hook_temporal_binding_failed(row) {
                blockers.push("private_hook_temporal_binding_failed".to_string());
            } else {
                blockers.push("real_private_hook_event_missing".to_string());
            }
        }
        Some("start_continue_devdata_collector_before_real_hook") => {
            blockers.push("continue_devdata_collector_not_listening".to_string());
            blockers.push("real_private_hook_event_missing".to_string());
        }
        Some("check_continue_devdata_collector_status") => {
            blockers.push("continue_devdata_collector_probe_blocked".to_string());
            blockers.push("real_private_hook_event_missing".to_string());
        }
        Some("record_observed_app_hook") => {
            blockers.push("observed_app_hook_not_recorded".to_string());
        }
        Some(_) | None => {}
    }
    if blockers.is_empty() {
        blockers.push("private_client_release_gate_not_passed".to_string());
    }
    blockers
}

fn private_app_hook_temporal_binding_failed(row: &ClientStatusRow) -> bool {
    if private_app_hook_event_requirements_missing(row) {
        return false;
    }
    row.proof_session_blocking_reasons
        .iter()
        .any(|reason| is_private_app_hook_temporal_binding_reason(reason))
        || row.proof_session_stage_blockers.iter().any(|stage| {
            stage.proof_level == "observed_app_hook"
                && stage
                    .blocking_reasons
                    .iter()
                    .any(|reason| is_private_app_hook_temporal_binding_reason(reason))
        })
}

fn private_app_hook_event_requirements_missing(row: &ClientStatusRow) -> bool {
    row.proof_session_blocking_reasons
        .iter()
        .any(|reason| is_private_app_hook_event_requirement_missing_reason(reason))
        || row.proof_session_stage_blockers.iter().any(|stage| {
            stage.proof_level == "observed_app_hook"
                && stage
                    .blocking_reasons
                    .iter()
                    .any(|reason| is_private_app_hook_event_requirement_missing_reason(reason))
        })
}

fn private_app_hook_temporal_binding_failed_summary(session: &ClientProofSessionSummary) -> bool {
    if private_app_hook_event_requirements_missing_summary(session) {
        return false;
    }
    session
        .blocking_reasons
        .iter()
        .any(|reason| is_private_app_hook_temporal_binding_reason(reason))
        || session.stage_blockers.iter().any(|stage| {
            stage.proof_level == "observed_app_hook"
                && stage
                    .blocking_reasons
                    .iter()
                    .any(|reason| is_private_app_hook_temporal_binding_reason(reason))
        })
}

fn private_app_hook_event_requirements_missing_summary(
    session: &ClientProofSessionSummary,
) -> bool {
    session
        .blocking_reasons
        .iter()
        .any(|reason| is_private_app_hook_event_requirement_missing_reason(reason))
        || session.stage_blockers.iter().any(|stage| {
            stage.proof_level == "observed_app_hook"
                && stage
                    .blocking_reasons
                    .iter()
                    .any(|reason| is_private_app_hook_event_requirement_missing_reason(reason))
        })
}

fn is_private_app_hook_event_requirement_missing_reason(reason: &str) -> bool {
    reason.contains("event JSONL must include the expected private event_source")
        || reason.contains("event JSONL must include the installed config binding_nonce")
        || reason.contains("event JSONL must carry soma_adapter_spool_append_v1 writer metadata")
        || reason.contains("event JSONL must include observed_at_ns on the matching private event")
}

fn is_private_app_hook_temporal_binding_reason(reason: &str) -> bool {
    reason.contains("installed config modified_at")
        && (reason.contains("observed_at_ns") || reason.contains("file modified_at"))
}

fn private_app_operator_next_action_id(row: &ClientStatusRow) -> String {
    if row.ready_for_private_client_claim {
        return "client_binding_release_gate_passed".to_string();
    }
    if row.private_capture_status == "client_binding_proof_storage_unavailable" {
        return "restore_client_binding_proof_storage_access".to_string();
    }
    if let Some(action_id) = private_app_operator_next_action_id_for_proof_step(row) {
        return action_id;
    }
    if row.private_capture_status == "artifact_integrity_failed" {
        if artifact_repair_primary_command(row)
            .as_ref()
            .is_some_and(|command| command_renders_evidence(command))
        {
            return "materialize_render_evidence_packet_for_artifact_repair".to_string();
        }
        if proof_session_waits_for_render_evidence_capture(row)
            && artifact_repair_effective_path_exists(row, "render_evidence")
        {
            return "capture_in_client_render_evidence_from_real_client_ui".to_string();
        }
        if row.artifact_repair_plan.as_ref().is_some_and(|plan| {
            effective_artifact_path(plan, "review_render_report")
                .is_some_and(|path| Path::new(&path).exists())
        }) {
            return "inspect_render_evidence_packet_for_artifact_repair".to_string();
        }
        return "refresh_invalid_client_binding_artifacts".to_string();
    }
    private_app_operator_next_action_id_for_proof_step(row).unwrap_or_else(|| {
        match row.proof_session_next_step_id.as_deref() {
            Some("install_or_merge_private_client_config" | "render_or_write_installed_config") => {
                "write_or_install_private_client_binding_config".to_string()
            }
            Some(next) => format!("continue_proof_session_{next}"),
            None if row.proof_session_error.is_some() => {
                "inspect_private_client_proof_session_error".to_string()
            }
            None => "inspect_private_client_readiness".to_string(),
        }
    })
}

fn private_app_operator_next_action_id_for_proof_step(row: &ClientStatusRow) -> Option<String> {
    match row.proof_session_next_step_id.as_deref()? {
        "check_continue_devdata_collector_status" => {
            Some("check_continue_devdata_collector_status".to_string())
        }
        "start_continue_devdata_collector_before_real_hook" => {
            Some("start_continue_devdata_collector_before_real_hook".to_string())
        }
        "record_observed_app_hook" => {
            Some("record_observed_app_hook_from_real_event_after_operator_confirmation".to_string())
        }
        "trigger_private_client_hook" => {
            if row.codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended)
            {
                return Some("restart_or_reopen_codex_app_before_real_hook".to_string());
            }
            if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_extension_config_not_visible)
            {
                return Some("merge_continue_mcp_config_before_real_hook".to_string());
            }
            if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(|check| !check.extension_observed)
            {
                return Some("install_or_enable_continue_extension_before_real_hook".to_string());
            }
            if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_devdata_collector_probe_blocked)
            {
                return Some("check_continue_devdata_collector_status".to_string());
            }
            if row
                .continue_extension_config_check
                .as_ref()
                .is_some_and(continue_devdata_collector_start_needed)
            {
                return Some("start_continue_devdata_collector_before_real_hook".to_string());
            }
            Some("trigger_real_private_client_hook_to_write_private_spool_event".to_string())
        }
        _ => None,
    }
}

fn private_app_operator_next_action_label(row: &ClientStatusRow) -> String {
    match private_app_operator_next_action_id(row).as_str() {
        "client_binding_release_gate_passed" => "Release gate passed".to_string(),
        "restore_client_binding_proof_storage_access" => {
            "Restore client proof storage access".to_string()
        }
        "refresh_invalid_client_binding_artifacts" => {
            "Refresh invalid binding artifacts".to_string()
        }
        "materialize_render_evidence_packet_for_artifact_repair" => {
            "Materialize render evidence packet".to_string()
        }
        "capture_in_client_render_evidence_from_real_client_ui" => {
            "Capture render evidence".to_string()
        }
        "inspect_render_evidence_packet_for_artifact_repair" => {
            "Inspect render evidence packet".to_string()
        }
        "record_observed_app_hook_from_real_event_after_operator_confirmation" => {
            "Record observed app hook after confirmation".to_string()
        }
        "restart_or_reopen_codex_app_before_real_hook" => "Quit/reopen Codex app".to_string(),
        "merge_continue_mcp_config_before_real_hook"
            if continue_profile_config_invalid_for_row(row) =>
        {
            "Repair Continue profile config".to_string()
        }
        "merge_continue_mcp_config_before_real_hook" => "Merge Continue MCP config".to_string(),
        "install_or_enable_continue_extension_before_real_hook" => {
            "Install or enable Continue extension".to_string()
        }
        "start_continue_devdata_collector_before_real_hook" => {
            "Start Continue dev-data collector".to_string()
        }
        "check_continue_devdata_collector_status" => "Check Continue collector status".to_string(),
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            format!("Trigger real {} hook", row.display_name)
        }
        "write_or_install_private_client_binding_config" => {
            format!("Install {} binding config", row.display_name)
        }
        "inspect_private_client_proof_session_error" => "Inspect proof-session error".to_string(),
        _ => "Continue proof session".to_string(),
    }
}

fn private_app_operator_next_step(row: &ClientStatusRow) -> String {
    private_app_operator_next_step_with_command(row, None)
}

fn private_app_operator_next_step_with_command(
    row: &ClientStatusRow,
    primary_command: Option<&[String]>,
) -> String {
    let primary_command_text = || {
        primary_command
            .map(|command| command.join(" "))
            .unwrap_or_else(|| primary_private_app_command(row).join(" "))
    };
    match private_app_operator_next_action_id(row).as_str() {
        "materialize_render_evidence_packet_for_artifact_repair" => {
            let command = primary_command_text();
            format!(
                "A review-render report already exists, but visible UI evidence is not proven yet; run `{command}` to materialize the proof-free render evidence packet template, then fill the visible-render fields from the real {} UI before recording observed_in_client_render.",
                row.display_name
            )
        }
        "inspect_render_evidence_packet_for_artifact_repair" => {
            let command = primary_command_text();
            format!(
                "A review-render report and proof-free render-evidence packet template exist, but visible UI evidence is not proven yet; inspect `{command}`, replace the visible-render placeholders from the real {} UI, then record observed_in_client_render only with explicit operator confirmation.",
                row.display_name
            )
        }
        "capture_in_client_render_evidence_from_real_client_ui" => {
            let command = primary_command_text();
            if let Some(packet) = render_proof_packet_scan_for_row(row) {
                if packet.status == "prepared_placeholders_pending" {
                    let review_surface = render_packet_preferred_view_path(&packet);
                    let evidence =
                        packet.render_evidence_path.as_deref().unwrap_or("render-evidence.json");
                    return format!(
                        "The proof-free render packet is already materialized: render `{review_surface}` in the real {} UI, then replace {} placeholder(s) in `{evidence}` only from that visible UI. Rerun `{command}` after filling it so the proof-session can expose the guarded observed_in_client_render recording command. Stored stale artifact rows remain blockers until replacement proof is recorded.",
                        row.display_name, packet.placeholder_count
                    );
                }
                if packet.status == "filled_observation_candidate" {
                    return format!(
                        "The render-evidence packet has a filled local observation candidate; rerun `{command}` so storage gates can validate the review-render binding before exposing the guarded observed_in_client_render recording command. Do not treat the packet as proof until explicit operator-confirmed proof recording succeeds."
                    );
                }
            }
            format!(
                "The proof-session is waiting at `capture_in_client_render_evidence`: run `{command}` to prepare or reuse proof-free review-render and render-evidence artifacts, render the generated Markdown/HTML in the real {} UI, fill the render evidence only from that visible UI, then rerun the proof-session to expose the guarded observed_in_client_render recording command. Stored stale artifact rows remain blockers until replacement proof is recorded.",
                row.display_name
            )
        }
        "restart_or_reopen_codex_app_before_real_hook" => {
            let event_source =
                row.expected_event_source.as_deref().unwrap_or("codex-app_private_lifecycle_hook");
            format!(
                "Quit or restart the stale Codex app process first, reopen Codex app (for example with `open -a Codex`), complete a real Codex app turn, then rerun the hook readiness command until SOMA observes a fresh `{event_source}` event with the installed binding nonce; do not record observed_app_hook before that event passes the temporal binding check. `open -a Codex` alone is only a reopen hint and does not force a stale running process to reload the notify config."
            )
        }
        "trigger_real_private_client_hook_to_write_private_spool_event"
            if row.client == "continue" =>
        {
            let event_source =
                row.expected_event_source.as_deref().unwrap_or("continue_private_lifecycle_hook");
            format!(
                "The Continue dev-data collector is listening; reload Continue or its host editor, complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the hook readiness command until SOMA observes a fresh `{event_source}` event with the installed binding nonce; do not record observed_app_hook before that real hook event is visible."
            )
        }
        "start_continue_devdata_collector_before_real_hook" => {
            "Start the local Continue dev-data collector, reload Continue or its host editor, complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the hook readiness command; do not record observed_app_hook before the real hook event is visible."
                .to_string()
        }
        "check_continue_devdata_collector_status" => {
            "SOMA could not prove the Continue dev-data collector is listening from this execution context; run the managed status command from the operator shell, avoid starting duplicate collectors, then rerun readiness before triggering a real Continue extension action."
                .to_string()
        }
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            let event_source =
                row.expected_event_source.as_deref().unwrap_or("private_lifecycle_hook");
            format!(
                "Complete a real {} action, then rerun the hook readiness command until SOMA observes a fresh `{event_source}` event with the installed binding nonce; do not record observed_app_hook before that real hook event is visible.",
                row.display_name
            )
        }
        _ => row.next_step.clone(),
    }
}

fn private_app_row_priority(row: &ClientStatusRow) -> u8 {
    if row.private_capture_status == "artifact_integrity_failed" {
        return 0;
    }
    match row.proof_session_next_step_id.as_deref() {
        Some("record_observed_app_hook") => 1,
        Some("check_continue_devdata_collector_status") => 2,
        Some("start_continue_devdata_collector_before_real_hook") => 2,
        Some("trigger_private_client_hook") => 2,
        Some("install_or_merge_private_client_config" | "render_or_write_installed_config") => 3,
        Some(_) => 4,
        None if row.proof_session_error.is_some() => 5,
        None => 6,
    }
}

fn primary_private_app_command(row: &ClientStatusRow) -> Vec<String> {
    if row.private_capture_status == "artifact_integrity_failed"
        && private_app_operator_next_action_id_for_proof_step(row).is_none()
    {
        if let Some(command) = artifact_repair_primary_command(row) {
            return command;
        }
        if row.proof_session_next_step_id.as_deref() == Some("capture_in_client_render_evidence") {
            if let Some(command) =
                proof_session_runbook_step_command_for_row(row, "capture_in_client_render_evidence")
            {
                return command;
            }
            if let Some(command) = &row.proof_session_next_command {
                return command.clone();
            }
        }
        if let Some(command) = &row.proof_session_next_command {
            return command.clone();
        }
        return vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--verify-evidence-artifacts".to_string(),
            "--client".to_string(),
            row.client.to_string(),
            "--json".to_string(),
        ];
    }
    if private_app_operator_next_action_id(row) == "restart_or_reopen_codex_app_before_real_hook" {
        if let Some(command) = codex_app_quit_command_for_row(row) {
            return command;
        }
    }
    if matches!(
        row.proof_session_next_step_id.as_deref(),
        Some("install_or_merge_private_client_config" | "trigger_private_client_hook")
    ) {
        if let Some(command) = private_target_config_install_command(row) {
            return command.to_vec();
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook")
        && continue_profile_config_invalid_for_row(row)
    {
        if let Some(command) = continue_profile_config_repair_command_for_row(row) {
            return command.to_vec();
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook")
        && row
            .continue_extension_config_check
            .as_ref()
            .is_some_and(continue_extension_config_not_visible)
    {
        if let Some(command) = continue_mcp_config_render_command(row) {
            return command.to_vec();
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook")
        && private_app_operator_next_action_id(row) == "check_continue_devdata_collector_status"
    {
        if let Some(command) = continue_devdata_collector_status_command_for_row(row) {
            return command;
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("check_continue_devdata_collector_status")
    {
        if let Some(command) = continue_devdata_collector_status_command_for_row(row) {
            return command;
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook")
        && private_app_operator_next_action_id(row)
            == "start_continue_devdata_collector_before_real_hook"
    {
        if let Some(command) = continue_devdata_collector_command_for_row(row) {
            return command;
        }
    }
    if row.proof_session_next_step_id.as_deref()
        == Some("start_continue_devdata_collector_before_real_hook")
    {
        if let Some(command) = continue_devdata_collector_command_for_row(row) {
            return command;
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook")
        && private_app_operator_next_action_id(row)
            == "trigger_real_private_client_hook_to_write_private_spool_event"
    {
        if let Some(command) = continue_devdata_install_command_for_row(row) {
            return command.to_vec();
        }
        if let Some(command) = continue_devdata_collector_command_for_row(row) {
            return command;
        }
    }
    if row.proof_session_next_step_id.as_deref() == Some("trigger_private_client_hook") {
        if let Some(command) = &row.simple_private_event_wait_command {
            return command.clone();
        }
        if let Some(command) = &row.private_event_wait_command {
            return command.clone();
        }
        if let Some(command) = &row.simple_private_hook_readiness_command {
            return command.clone();
        }
        if let Some(command) = row.next_commands.iter().find(|command| {
            command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh")
        }) {
            return command.clone();
        }
    }
    row.proof_session_next_command.clone().unwrap_or_else(|| {
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--client".to_string(),
            row.client.to_string(),
            "--proof-session".to_string(),
            "--json".to_string(),
        ]
    })
}

fn proof_session_waits_for_render_evidence_capture(row: &ClientStatusRow) -> bool {
    row.proof_session_next_step_id.as_deref() == Some("capture_in_client_render_evidence")
}

fn artifact_repair_effective_path_exists(row: &ClientStatusRow, artifact_kind: &str) -> bool {
    row.artifact_repair_plan
        .as_ref()
        .and_then(|plan| effective_artifact_path(plan, artifact_kind))
        .is_some_and(|path| Path::new(&path).exists())
}

fn empty_client_belief_review_summary() -> ClientBeliefReviewSummary {
    ClientBeliefReviewSummary {
        source: "soma_clients.belief_review_summary.v1",
        status: "clear".to_string(),
        raw_candidate_count: 0,
        review_group_count: 0,
        hidden_duplicate_count: 0,
        substantive_contradiction_group_count: 0,
        substantive_contradiction_candidate_count: 0,
        low_value_conflict_group_count: 0,
        low_value_conflict_candidate_count: 0,
        low_value_noise_group_count: 0,
        low_value_noise_candidate_count: 0,
        noise_group_count: 0,
        noise_candidate_count: 0,
        primary_group_id: None,
        next_action: "No unresolved belief candidates are visible for this scope.".to_string(),
        trust_boundary:
            "soma_clients_belief_review_summary_is_read_only: mirrors soma learning belief workload only; records no proof row, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
    }
}

fn client_belief_review_summary(
    summary: &learning_status::LearningBeliefReviewSummary,
) -> ClientBeliefReviewSummary {
    ClientBeliefReviewSummary {
        source: "soma_clients.belief_review_summary.v1",
        status: summary.status.to_string(),
        raw_candidate_count: summary.raw_candidate_count,
        review_group_count: summary.review_group_count,
        hidden_duplicate_count: summary.hidden_duplicate_count,
        substantive_contradiction_group_count: summary.substantive_contradiction_group_count,
        substantive_contradiction_candidate_count: summary.substantive_contradiction_candidate_count,
        low_value_conflict_group_count: summary.low_value_conflict_group_count,
        low_value_conflict_candidate_count: summary.low_value_conflict_candidate_count,
        low_value_noise_group_count: summary.low_value_noise_group_count,
        low_value_noise_candidate_count: summary.low_value_noise_candidate_count,
        noise_group_count: summary.noise_group_count,
        noise_candidate_count: summary.noise_candidate_count,
        primary_group_id: summary.primary_group_id,
        next_action: summary.next_action.clone(),
        trust_boundary:
            "soma_clients_belief_review_summary_is_read_only: mirrors soma learning belief workload only; records no proof row, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
    }
}

fn client_semantic_review_cards(
    cards: &[learning_status::LearningReviewCard],
    binary_identity: &BinaryIdentity,
) -> Vec<ClientSemanticReviewCard> {
    cards
        .iter()
        .map(|card| ClientSemanticReviewCard {
            source: "soma_clients.semantic_review_card.v1",
            card_id: card.card_id.clone(),
            lane: card.lane.to_string(),
            priority: card.priority,
            target: card.target.to_string(),
            status: card.status.clone(),
            title: card.title.clone(),
            summary: card.summary.clone(),
            primary_action: card.primary_action.clone(),
            primary_command: command_with_current_binary_when_path_soma_differs(
                card.primary_command.clone(),
                binary_identity,
            ),
            evidence_refs: card.evidence_refs.clone(),
            blocks_l4_promotion: card.blocks_l4_promotion,
            projection_path: card.projection_path.clone(),
            evidence_rule: card.evidence_rule.to_string(),
            accepted_verifier_types: card
                .accepted_verifier_types
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            forbidden_evidence_sources: card
                .forbidden_evidence_sources
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            trust_boundary:
                "soma_clients_semantic_review_card_is_read_only: mirrors soma learning review_cards only; records no proof row, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
        })
        .collect()
}

fn client_semantic_promotion_matrix(
    rows: &[learning_status::LearningPromotionMatrixRow],
    binary_identity: &BinaryIdentity,
) -> Vec<ClientSemanticPromotionMatrixRow> {
    rows.iter()
        .map(|row| ClientSemanticPromotionMatrixRow {
            source: "soma_clients.semantic_promotion_matrix.v1",
            target: row.target.to_string(),
            lane: row.lane.to_string(),
            status: row.status.to_string(),
            candidate_count: row.candidate_count,
            ready_for_manual_l4_review: row.ready_for_manual_l4_review,
            context_projection_ready: row.context_projection_ready,
            blocks_l4_promotion: row.blocks_l4_promotion,
            projected_context_section: row.projected_context_section.map(ToOwned::to_owned),
            required_evidence: row.required_evidence.to_string(),
            next_action: row.next_action.clone(),
            primary_command: command_with_current_binary_when_path_soma_differs(
                row.primary_command.clone(),
                binary_identity,
            ),
            trust_boundary:
                "soma_clients_semantic_promotion_matrix_is_read_only: mirrors soma learning promotion_matrix only; records no proof row, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
        })
        .collect()
}

fn client_semantic_review_lanes(
    lanes: &[learning_status::LearningReviewLane],
    binary_identity: &BinaryIdentity,
) -> Vec<ClientSemanticReviewLane> {
    lanes
        .iter()
        .map(|lane| ClientSemanticReviewLane {
            source: "soma_clients.semantic_review_lane.v1",
            lane: lane.lane.to_string(),
            priority: lane.priority,
            status: lane.status.to_string(),
            count: lane.count,
            next_action: lane.next_action.clone(),
            command: command_with_current_binary_when_path_soma_differs(
                lane.command.clone(),
                binary_identity,
            ),
            trust_boundary:
                "soma_clients_semantic_review_lane_is_read_only: mirrors soma learning review_lanes only; records no proof row, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
        })
        .collect()
}

fn client_semantic_resolution_actions(
    candidates: &[learning_status::LearningCandidateRow],
    binary_identity: &BinaryIdentity,
) -> Vec<ClientSemanticResolutionAction> {
    candidates
        .iter()
        .flat_map(|candidate| candidate.resolution_actions.iter())
        .map(|action| ClientSemanticResolutionAction {
            source: "soma_clients.semantic_resolution_action.v1",
            action: action.action.clone(),
            control_id: action.control_id.clone(),
            label: action.label.clone(),
            requires_evidence: action.requires_evidence,
            cli_command: command_with_current_binary_when_path_soma_differs(
                action.cli_command.clone(),
                binary_identity,
            ),
            mcp_tool: action.mcp_tool.to_string(),
            evidence_rule: action.evidence_rule.clone(),
            trust_effect: action.trust_effect.clone(),
            trust_boundary:
                "soma_clients_semantic_resolution_action_is_read_only: mirrors soma learning resolution-action templates only; records no proof row, creates no verification event, writes no semantic_fact, applies no proposal, and promotes no cloud draft",
        })
        .collect()
}

fn client_semantic_workload_summary(
    project: Option<&str>,
    status: &str,
    summary: &learning_status::LearningStatusSummary,
    belief_review_summary: &ClientBeliefReviewSummary,
    promotion_matrix: &[ClientSemanticPromotionMatrixRow],
) -> ClientSemanticWorkloadSummary {
    let scope_source = semantic_review_scope_source(project).to_string();
    let belief_resolution_blocker_count = summary.belief_substantive_contradiction_count;
    let l2_audit_only_count =
        summary.belief_candidate_count.saturating_sub(belief_resolution_blocker_count);
    let l4_promotion_blocking_count = summary.cloud_draft_blocked_count
        + summary.review_only_candidate_count
        + summary.manual_l4_review_count
        + belief_resolution_blocker_count;
    let durable_learning_blocking_count = summary.cloud_draft_blocked_count
        + summary.l4_candidate_count
        + summary.manual_l4_review_count
        + summary.review_only_candidate_count
        + belief_resolution_blocker_count
        + usize::from(summary.should_interrupt);
    let named_attention_count = summary.cloud_draft_blocked_count
        + summary.l4_candidate_count
        + summary.review_only_candidate_count
        + belief_resolution_blocker_count;
    ClientSemanticWorkloadSummary {
        source: "soma_clients.semantic_workload_summary.v1",
        scope_source,
        project: project.map(ToOwned::to_owned),
        review_queue_pending_count: summary.pending_review_item_count,
        cloud_draft_blocker_count: summary.cloud_draft_blocked_count,
        l4_review_candidate_count: summary.l4_candidate_count,
        manual_l4_review_count: summary.manual_l4_review_count,
        review_only_verification_count: summary.review_only_candidate_count,
        belief_resolution_blocker_count,
        l4_promotion_blocking_count,
        l2_audit_only_count,
        context_projection_ready_count: promotion_matrix
            .iter()
            .filter(|row| row.context_projection_ready)
            .count(),
        durable_learning_blocking_count,
        operator_attention_count: summary.pending_review_item_count.max(named_attention_count),
        primary_operator_bucket: semantic_workload_primary_bucket(status, belief_review_summary),
        trust_boundary:
            "semantic_workload_summary_is_read_only: separates scoped review queue, durable-learning blockers, cloud-draft blockers, and L2 audit-only belief workload; records no proof row, creates no verification event, writes no L4 memory, and promotes no cloud draft",
    }
}

fn empty_client_semantic_workload_summary(project: Option<&str>) -> ClientSemanticWorkloadSummary {
    ClientSemanticWorkloadSummary {
        source: "soma_clients.semantic_workload_summary.v1",
        scope_source: semantic_review_scope_source(project).to_string(),
        project: project.map(ToOwned::to_owned),
        review_queue_pending_count: 0,
        cloud_draft_blocker_count: 0,
        l4_review_candidate_count: 0,
        manual_l4_review_count: 0,
        review_only_verification_count: 0,
        belief_resolution_blocker_count: 0,
        l4_promotion_blocking_count: 0,
        l2_audit_only_count: 0,
        context_projection_ready_count: 0,
        durable_learning_blocking_count: 0,
        operator_attention_count: 0,
        primary_operator_bucket: "unavailable",
        trust_boundary:
            "semantic_workload_summary_is_read_only: separates scoped review queue, durable-learning blockers, cloud-draft blockers, and L2 audit-only belief workload; records no proof row, creates no verification event, writes no L4 memory, and promotes no cloud draft",
    }
}

fn semantic_review_scope_source(project: Option<&str>) -> &'static str {
    if project.is_some() {
        "explicit_project"
    } else {
        "global_learning_scope"
    }
}

fn semantic_workload_primary_bucket(
    status: &str,
    belief_review_summary: &ClientBeliefReviewSummary,
) -> &'static str {
    match status {
        "blocked_cloud_draft_verification" => "cloud_draft_blocker",
        "pending_semantic_review" => "l4_or_review_queue",
        "semantic_review_only_pending" => "review_only_verification",
        "review_only_beliefs" => "belief_resolution",
        "noise_triage_only" => "l2_audit_only",
        "unavailable" => "unavailable",
        "clear" if belief_review_summary.noise_candidate_count > 0 => "l2_audit_only",
        _ => "clear",
    }
}

fn build_semantic_review_status(
    args: &ClientStatusArgs,
    db_path: &std::path::Path,
    client_filter: Option<&str>,
) -> ClientSemanticReviewStatus {
    let (binary_identity, _binary_identity_errors) =
        crate::cli::binary_identity::collect_binary_identity();
    let client = client_filter.unwrap_or("generic").to_string();
    let learning_args = LearningStatusArgs {
        status_alias: None,
        project: args.project.clone(),
        session_id: None,
        client: Some(client.clone()),
        limit: args.limit.clamp(1, 500),
        min_support: 2,
        candidate_limit: CLIENT_SEMANTIC_REVIEW_SURFACE_LIMIT,
        review_limit: CLIENT_SEMANTIC_REVIEW_SURFACE_LIMIT,
        db_path: Some(db_path.to_string_lossy().into_owned()),
        dogfood_report: args.dogfood_report.clone(),
        format: "json".to_string(),
        brief: false,
        json: true,
    };
    match learning_status::run(&learning_args) {
        Ok(outcome) => {
            let summary = outcome.summary;
            let storage_status = outcome.storage_status;
            let storage_error = outcome.storage_error.clone();
            let belief_review_summary =
                client_belief_review_summary(&outcome.belief_review_summary);
            let review_cards =
                client_semantic_review_cards(&outcome.review_cards, &binary_identity);
            let promotion_matrix =
                client_semantic_promotion_matrix(&outcome.promotion_matrix, &binary_identity);
            let review_lanes =
                client_semantic_review_lanes(&outcome.review_lanes, &binary_identity);
            let semantic_resolution_actions =
                client_semantic_resolution_actions(&outcome.candidates, &binary_identity);
            let review_surface = outcome.review_surface;
            let review_surface_client = review_surface.client.clone();
            let primary_surface = review_surface.primary_surface.to_string();
            let review_render_command = command_with_current_binary_when_path_soma_differs(
                review_surface.render_plan_command,
                &binary_identity,
            );
            let review_digest_command = command_with_current_binary_when_path_soma_differs(
                review_surface.digest_command,
                &binary_identity,
            );
            let review_report_command = command_with_current_binary_when_path_soma_differs(
                review_surface.report_command,
                &binary_identity,
            );
            let review_actions_command = command_with_current_binary_when_path_soma_differs(
                review_surface.action_plan_command,
                &binary_identity,
            );
            let proof_session_command = command_with_current_binary_when_path_soma_differs(
                review_surface.proof_session_command,
                &binary_identity,
            );
            let status =
                client_semantic_status_from_learning_status(storage_status, &outcome.status)
                    .to_string();
            let workload_summary = client_semantic_workload_summary(
                args.project.as_deref(),
                &status,
                &summary,
                &belief_review_summary,
                &promotion_matrix,
            );
            let next_step = match status.as_str() {
                "blocked_cloud_draft_verification" => format!(
                    "Render `{}` so the client can show cloud_draft blocker controls before verification.",
                    review_render_command.join(" ")
                ),
                "pending_semantic_review"
                    if primary_surface == "semantic_proposals" =>
                {
                    format!(
                        "Inspect `{}` to resolve semantic review-only candidates before any L4 semantic_fact write.",
                        promotion_matrix
                            .iter()
                            .find(|lane| lane.target == "semantic_fact" && !lane.primary_command.is_empty())
                            .map(|lane| lane.primary_command.join(" "))
                            .unwrap_or_else(|| review_report_command.join(" "))
                    )
                }
                "pending_semantic_review" => format!(
                    "Render `{}` to inspect semantic L4 candidates and review controls.",
                    review_render_command.join(" ")
                ),
                "semantic_review_only_pending" => format!(
                    "{} semantic review-only candidate(s) need user/tool/test/local/correction verification before L4 learning; inspect `{}`.",
                    summary.review_only_candidate_count,
                    promotion_matrix
                        .iter()
                        .find(|lane| lane.target == "semantic_fact" && !lane.primary_command.is_empty())
                        .map(|lane| lane.primary_command.join(" "))
                        .unwrap_or_else(|| review_report_command.join(" "))
                ),
                "review_only_beliefs" => format!(
                    "Render `{}` to inspect belief review signals without promotion.",
                    review_digest_command.join(" ")
                ),
                "noise_triage_only" => format!(
                    "{} low-value belief candidate(s) remain isolated as L2 command-noise audit evidence; inspect `{}` only if a command outcome matters.",
                    summary.belief_noise_candidate_count,
                    review_digest_command.join(" ")
                ),
                "unavailable" => format!(
                    "Run `{}` after restoring access to the SOMA DB.",
                    command_with_current_binary_when_path_soma_differs(
                        vec![
                            "soma".to_string(),
                            "learning".to_string(),
                            "--client".to_string(),
                            client.clone(),
                            "--json".to_string(),
                        ],
                        &binary_identity,
                    )
                    .join(" ")
                ),
                _ => "No pending semantic learning review work is visible for this scope.".to_string(),
            };
            let workload_command =
                semantic_review_workload_command(&review_surface_client, args.project.as_deref());
            let workload_command = command_with_current_binary_when_path_soma_differs(
                workload_command,
                &binary_identity,
            );
            let primary_command = semantic_review_primary_command_from_parts(
                &status,
                &primary_surface,
                &promotion_matrix,
                &review_render_command,
                &review_digest_command,
                &review_report_command,
            );
            let next_commands = semantic_review_next_commands_from_parts(
                &status,
                &primary_surface,
                &primary_command,
                &review_actions_command,
                &review_digest_command,
                &review_report_command,
                &proof_session_command,
            );
            let operator_next_action_id =
                semantic_review_operator_next_action_id_for_status(&status);
            let operator_next_action_label =
                semantic_review_operator_next_action_label_for_status(&status);
            ClientSemanticReviewStatus {
                source: "soma_clients.semantic_review_status",
                status,
                operator_next_action_id,
                operator_next_action_label,
                client: review_surface_client,
                scope_source: semantic_review_scope_source(args.project.as_deref()).to_string(),
                project: args.project.clone(),
                primary_surface,
                workload_summary,
                pending_review_item_count: summary.pending_review_item_count,
                l4_candidate_count: summary.l4_candidate_count,
                review_only_candidate_count: summary.review_only_candidate_count,
                cloud_draft_blocked_count: summary.cloud_draft_blocked_count,
                belief_candidate_count: summary.belief_candidate_count,
                belief_group_count: summary.belief_group_count,
                belief_hidden_duplicate_count: summary.belief_hidden_duplicate_count,
                belief_contradiction_count: summary.belief_contradiction_count,
                belief_substantive_contradiction_count: summary
                    .belief_substantive_contradiction_count,
                belief_low_value_conflict_count: summary.belief_low_value_conflict_count,
                belief_low_value_noise_count: summary.belief_low_value_noise_count,
                belief_noise_candidate_count: summary.belief_noise_candidate_count,
                belief_review_summary,
                should_interrupt: summary.should_interrupt,
                next_step,
                workload_command,
                primary_command,
                next_commands,
                review_render_command,
                review_digest_command,
                review_report_command,
                review_actions_command,
                proof_session_command,
                semantic_resolution_actions,
                review_cards,
                promotion_matrix,
                review_lanes,
                next_mcp_tools: review_surface
                    .mcp_tools
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                control_contract: review_surface.control_contract.to_string(),
                proof_path: review_surface.proof_path.to_string(),
                error: storage_error,
                trust_boundary:
                    "soma_clients_semantic_review_status_is_read_only: mirrors soma learning review-surface guidance only; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private client rendering",
            }
        }
        Err(err) => {
            let review_render_command = command_with_current_binary_when_path_soma_differs(
                semantic_review_client_command(
                    &["soma", "context", "review-render", "--format", "json"],
                    &client,
                ),
                &binary_identity,
            );
            let review_digest_command = command_with_current_binary_when_path_soma_differs(
                semantic_review_client_command(
                    &[
                        "soma",
                        "context",
                        "review-digest",
                        "--include-queue-only",
                        "--format",
                        "json",
                    ],
                    &client,
                ),
                &binary_identity,
            );
            let review_report_command = command_with_current_binary_when_path_soma_differs(
                semantic_review_command(&["soma", "context", "review-report", "--format", "json"]),
                &binary_identity,
            );
            let review_actions_command = command_with_current_binary_when_path_soma_differs(
                semantic_review_command(&["soma", "context", "review-actions", "--format", "json"]),
                &binary_identity,
            );
            let proof_session_command = command_with_current_binary_when_path_soma_differs(
                vec![
                    "soma".to_string(),
                    "adapter-binding-proof".to_string(),
                    "--client".to_string(),
                    client.clone(),
                    "--proof-session".to_string(),
                    "--json".to_string(),
                ],
                &binary_identity,
            );
            let workload_command =
                semantic_review_workload_command(&client, args.project.as_deref());
            let workload_command = command_with_current_binary_when_path_soma_differs(
                workload_command,
                &binary_identity,
            );
            let primary_command = command_with_current_binary_when_path_soma_differs(
                vec![
                    "soma".to_string(),
                    "learning".to_string(),
                    "--client".to_string(),
                    client.clone(),
                    "--json".to_string(),
                ],
                &binary_identity,
            );
            let mut next_commands = Vec::new();
            push_next_command_once(&mut next_commands, primary_command.clone());
            push_next_command_once(&mut next_commands, review_report_command.clone());
            ClientSemanticReviewStatus {
                source: "soma_clients.semantic_review_status",
                status: "unavailable".to_string(),
                operator_next_action_id: "restore_semantic_learning_storage_access".to_string(),
                operator_next_action_label: "Restore semantic storage access".to_string(),
                client: client.clone(),
                scope_source: semantic_review_scope_source(args.project.as_deref()).to_string(),
                project: args.project.clone(),
                primary_surface: "unavailable".to_string(),
                workload_summary: empty_client_semantic_workload_summary(args.project.as_deref()),
                pending_review_item_count: 0,
                l4_candidate_count: 0,
                review_only_candidate_count: 0,
                cloud_draft_blocked_count: 0,
                belief_candidate_count: 0,
                belief_group_count: 0,
                belief_hidden_duplicate_count: 0,
                belief_contradiction_count: 0,
                belief_substantive_contradiction_count: 0,
                belief_low_value_conflict_count: 0,
                belief_low_value_noise_count: 0,
                belief_noise_candidate_count: 0,
                belief_review_summary: empty_client_belief_review_summary(),
                should_interrupt: false,
                next_step: format!(
                    "Run `{}` after restoring access to the SOMA DB.",
                    primary_command.join(" ")
                ),
                workload_command,
                primary_command,
                next_commands,
                review_render_command,
                review_digest_command,
                review_report_command,
                review_actions_command,
                proof_session_command,
                semantic_resolution_actions: Vec::new(),
                review_cards: Vec::new(),
                promotion_matrix: Vec::new(),
                review_lanes: Vec::new(),
                next_mcp_tools: vec![
                    "soma_review_render".to_string(),
                    "soma_review_report".to_string(),
                    "soma_review_actions".to_string(),
                    "soma_client_binding_proof_session".to_string(),
                ],
                control_contract:
                    "semantic_review_status_unavailable_until_soma_learning_can_read_storage"
                        .to_string(),
                proof_path: "unavailable_until_storage_read_succeeds".to_string(),
                error: Some(err.to_string()),
                trust_boundary:
                    "soma_clients_semantic_review_status_is_read_only: mirrors soma learning review-surface guidance only; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private client rendering",
            }
        }
    }
}

fn client_semantic_status_from_learning_status(
    storage_status: &str,
    learning_status: &str,
) -> &'static str {
    if storage_status != "available" {
        return "unavailable";
    }
    match learning_status {
        "cloud_draft_blocked" => "blocked_cloud_draft_verification",
        "l4_review_ready" | "review_pending" => "pending_semantic_review",
        "semantic_review_only_pending" => "semantic_review_only_pending",
        "belief_review_pending" => "review_only_beliefs",
        "noise_triage_only" => "noise_triage_only",
        "clear" => "clear",
        _ => "pending_semantic_review",
    }
}

fn semantic_review_command(base: &[&str]) -> Vec<String> {
    base.iter().map(|part| (*part).to_string()).collect::<Vec<_>>()
}

fn semantic_review_client_command(base: &[&str], client: &str) -> Vec<String> {
    let mut command = base.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
    command.push("--client".to_string());
    command.push(client.to_string());
    command
}

fn semantic_review_workload_command(client: &str, project: Option<&str>) -> Vec<String> {
    let mut command = semantic_review_client_command(&["soma", "learning", "--brief"], client);
    if let Some(project) = project.filter(|project| !project.trim().is_empty()) {
        command.push("--project".to_string());
        command.push(project.to_string());
    }
    command
}

fn proof_level_proven_count(rows: &[ClientStatusRow], proof_level: &str) -> usize {
    rows.iter()
        .filter(|row| {
            row.capture_model == PRIVATE_APP_CAPTURE_MODEL
                && row
                    .proof_level_statuses
                    .iter()
                    .any(|status| status.proof_level == proof_level && status.status == "recorded")
        })
        .count()
}

fn build_private_app_proof_session_summaries(
    db_path: &std::path::Path,
    requested_client: Option<McpClientKind>,
    limit: usize,
) -> BTreeMap<String, ClientProofSessionSummary> {
    McpClientKind::all()
        .iter()
        .filter(|client| requested_client.is_none_or(|filter| filter == **client))
        .filter(|client| is_private_app_client(**client))
        .map(|client| {
            (client.as_str().to_string(), proof_session_summary_for_client(db_path, *client, limit))
        })
        .collect()
}

fn proof_session_summary_for_client(
    db_path: &std::path::Path,
    client: McpClientKind,
    limit: usize,
) -> ClientProofSessionSummary {
    let default_event_jsonl_path = default_adapter_event_jsonl_path();
    let event_jsonl_probe_status = default_event_jsonl_path
        .as_ref()
        .map(|path| if path.is_file() { "scanned" } else { "not_found" }.to_string())
        .or_else(|| Some("home_unavailable".to_string()));
    let event_jsonl_arg = default_event_jsonl_path
        .as_ref()
        .filter(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned());
    let event_jsonl_path =
        default_event_jsonl_path.as_ref().map(|path| path.to_string_lossy().into_owned());
    let args = AdapterBindingProofArgs {
        manifest: None,
        client: Some(client.as_str().to_string()),
        list: false,
        status: false,
        check_installed_config: false,
        discover_installed_config: false,
        real_app_proof_kit: false,
        evidence_bundle: false,
        proof_session: true,
        json: true,
        brief: false,
        format: "json".to_string(),
        prepare_installed_config: false,
        render_installed_config: false,
        write_installed_config: None,
        render_render_evidence: false,
        write_render_evidence: None,
        verify_evidence_artifacts: false,
        proof_id: None,
        limit,
        proof_level: "observed_event_file".to_string(),
        evidence_source: "clients_status_proof_session_probe".to_string(),
        binding_nonce: None,
        config_root: None,
        artifact_dir: None,
        event_jsonl: event_jsonl_arg,
        installed_config: None,
        require_private_target_config_for_app_hook: false,
        render_evidence: None,
        review_action_report: None,
        drain_report: None,
        review_render_report: None,
        operator_confirm_real_app_invocation: false,
        operator_confirm_in_client_render: false,
        operator_confirm_review_action: false,
        operator_confirm_release_grade_evidence: false,
        db_path: Some(db_path.to_string_lossy().into_owned()),
    };
    let ctx = AdapterBindingProofContext { db_path: db_path.to_path_buf() };
    match run_proof_session_blocking(&args, &ctx) {
        Ok(outcome) => {
            let next_step_id = outcome.proof_session.next_step_id.clone();
            let next_runbook_step = next_step_id.as_deref().and_then(|step_id| {
                outcome.proof_session.runbook.steps.iter().find(|step| step.id == step_id)
            });
            let next_operator_step = outcome.proof_session.next_operator_step.as_ref();
            ClientProofSessionSummary {
                status: Some(outcome.proof_session.status.clone()),
                release_gate: Some(outcome.proof_session.release_gate.clone()),
                next_step_id,
                next_operator_step_title: next_operator_step.map(|step| step.title.clone()),
                next_operator_step_intent: next_operator_step.map(|step| step.intent.clone()),
                next_operator_step_trust_boundary: next_operator_step
                    .map(|step| step.trust_boundary.clone()),
                next_operator_step_requires_operator_action: next_operator_step
                    .map(|step| step.requires_operator_action),
                next_command: outcome.proof_session.next_command.clone(),
                next_mcp_tool: outcome
                    .proof_session
                    .next_mcp_call
                    .as_ref()
                    .map(|call| call.tool.clone()),
                next_mcp_arguments: outcome
                    .proof_session
                    .next_mcp_call
                    .as_ref()
                    .map(|call| call.arguments.clone()),
                external_action: outcome.proof_session.external_action.clone(),
                expected_event_source: Some(outcome.expected_event_source),
                binding_nonce: Some(outcome.binding_nonce),
                generated_binding_nonce: Some(outcome.generated_binding_nonce),
                event_jsonl_path: event_jsonl_path.clone(),
                event_jsonl_probe_status: event_jsonl_probe_status.clone(),
                blocking_reasons: current_proof_stage_blocking_reasons(
                    &outcome.proof_session,
                    next_runbook_step,
                ),
                ready_to_record_proof_levels: outcome
                    .proof_session
                    .ready_to_record_proof_levels
                    .iter()
                    .map(|level| level.as_str().to_string())
                    .collect(),
                stage_blockers: outcome
                    .proof_session
                    .stages
                    .iter()
                    .filter(|stage| !stage.blocking_reasons.is_empty())
                    .map(stage_blocker_summary)
                    .collect(),
                runbook_steps: outcome
                    .proof_session
                    .runbook
                    .steps
                    .iter()
                    .map(|step| proof_session_runbook_step_summary(client, step))
                    .collect(),
                ready_now_step_count: Some(
                    outcome.proof_session.runbook.progress.ready_now_step_count,
                ),
                blocking_reason_count: Some(
                    outcome.proof_session.runbook.progress.blocking_reason_count,
                ),
                installed_config_eligible_candidates: Some(
                    outcome.installed_config_eligible_candidates,
                ),
                installed_config_setup_artifact_eligible_candidates: Some(
                    outcome.setup_artifact_eligible_candidates,
                ),
                installed_config_private_target_eligible_candidates: Some(
                    outcome.private_client_target_eligible_candidates,
                ),
                eligible_setup_artifact_paths: outcome.eligible_setup_artifact_paths,
                eligible_private_client_target_paths: outcome.eligible_private_client_target_paths,
                private_client_target_candidate_paths: outcome
                    .private_client_target_candidate_paths,
                error: None,
            }
        }
        Err(err) => ClientProofSessionSummary {
            status: None,
            release_gate: None,
            next_step_id: None,
            next_operator_step_title: None,
            next_operator_step_intent: None,
            next_operator_step_trust_boundary: None,
            next_operator_step_requires_operator_action: None,
            next_command: None,
            next_mcp_tool: None,
            next_mcp_arguments: None,
            external_action: None,
            expected_event_source: None,
            binding_nonce: None,
            generated_binding_nonce: None,
            event_jsonl_path,
            event_jsonl_probe_status,
            blocking_reasons: Vec::new(),
            ready_to_record_proof_levels: Vec::new(),
            stage_blockers: Vec::new(),
            runbook_steps: Vec::new(),
            ready_now_step_count: None,
            blocking_reason_count: None,
            installed_config_eligible_candidates: None,
            installed_config_setup_artifact_eligible_candidates: None,
            installed_config_private_target_eligible_candidates: None,
            eligible_setup_artifact_paths: Vec::new(),
            eligible_private_client_target_paths: Vec::new(),
            private_client_target_candidate_paths: Vec::new(),
            error: Some(err.to_string()),
        },
    }
}

fn stage_blocker_summary(stage: &ClientBindingProofSessionStage) -> ClientProofSessionStageBlocker {
    ClientProofSessionStageBlocker {
        proof_level: stage.proof_level.as_str().to_string(),
        ledger_status: stage.ledger_status.clone(),
        artifact_status: stage.artifact_status.clone(),
        ready_to_record_now: stage.ready_to_record_now,
        blocking_reasons: stage.blocking_reasons.clone(),
    }
}

fn current_proof_stage_blocking_reasons(
    session: &ClientBindingProofSession,
    next_runbook_step: Option<&ClientBindingProofRunbookStep>,
) -> Vec<String> {
    if let Some(stage) =
        session.stages.iter().find(|stage| stage.ledger_status != "stored_verified")
    {
        if stage.ready_to_record_now {
            return Vec::new();
        }
        if !stage.blocking_reasons.is_empty() {
            return stage.blocking_reasons.clone();
        }
    }

    next_runbook_step.map(|step| step.blocking_reasons.clone()).unwrap_or_default()
}

fn proof_session_runbook_step_summary(
    client: McpClientKind,
    step: &ClientBindingProofRunbookStep,
) -> ClientProofSessionRunbookStepSummary {
    ClientProofSessionRunbookStepSummary {
        source: "soma_clients.private_app_proof_session_runbook_step.v1",
        id: step.id.clone(),
        title: step.title.clone(),
        intent: step.intent.clone(),
        stage: step.stage.clone(),
        evidence_kind: step.evidence_kind.clone(),
        command: step.command.clone(),
        mcp_tool: step.mcp_call.as_ref().map(|call| call.tool.clone()),
        mcp_arguments_json: step
            .mcp_call
            .as_ref()
            .and_then(|call| serde_json::to_string(&call.arguments).ok()),
        external_action_safety: step.external_action_safety.as_ref().and_then(|_| {
            client_private_app_external_action_safety_for(
                client.as_str(),
                client.display_name(),
                "trigger_real_private_client_hook_to_write_private_spool_event",
            )
        }),
        external_action: step.external_action.clone(),
        suggested_artifact_path: step.suggested_artifact_path.clone(),
        requires_operator_action: step.requires_operator_action,
        records_proof: step.records_proof,
        ready_now: step.ready_now,
        blocking_reasons: step.blocking_reasons.clone(),
        proof_step_trust_boundary: step.trust_boundary.clone(),
        trust_boundary:
            "private_app_proof_session_runbook_step_is_read_only: mirrors proof-session runbook controls for client UI only; records no proof row, creates no verification event, installs no hook, promotes no cloud draft, and cannot substitute for app-hook/render/review-action evidence",
    }
}

fn default_adapter_event_jsonl_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .map(|home| home.join(".soma").join("adapter").join("events.jsonl"))
}

fn is_private_app_client(client: McpClientKind) -> bool {
    matches!(client, McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue)
}

fn private_target_config_install_command_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<Vec<String>> {
    if !is_private_app_client(client) {
        return None;
    }
    let proof_session = proof_session?;
    if !matches!(
        proof_session.next_step_id.as_deref(),
        Some("install_or_merge_private_client_config" | "trigger_private_client_hook")
    ) {
        return None;
    }
    if proof_session.installed_config_setup_artifact_eligible_candidates.unwrap_or_default() == 0
        || proof_session.installed_config_private_target_eligible_candidates.unwrap_or_default() > 0
    {
        return None;
    }
    let target_path = proof_session.private_client_target_candidate_paths.first()?.clone();
    let binding_nonce = proof_session.binding_nonce.as_ref()?;
    Some(vec![
        "soma".to_string(),
        "adapter-binding-proof".to_string(),
        "--client".to_string(),
        client.as_str().to_string(),
        "--render-installed-config".to_string(),
        "--binding-nonce".to_string(),
        binding_nonce.clone(),
        "--write-installed-config".to_string(),
        target_path,
        "--json".to_string(),
    ])
}

fn private_hook_readiness_command_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Vec<String> {
    if let Some(summary) = proof_session {
        if let Some(command) = summary.next_command.as_ref().filter(|command| {
            command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh")
        }) {
            let mut command = command.clone();
            enrich_private_hook_readiness_command(&mut command, summary);
            return command;
        }
    }

    let mut command =
        vec!["env".to_string(), format!("SOMA_CLIENT_BINDING_CLIENT={}", client.as_str())];
    if let Some(event_source) =
        proof_session.and_then(|summary| summary.expected_event_source.as_ref())
    {
        command.push(format!("SOMA_CLIENT_BINDING_EVENT_SOURCE={event_source}"));
    }
    if let Some(binding_nonce) = proof_session.and_then(|summary| summary.binding_nonce.as_ref()) {
        command.push(format!("SOMA_CLIENT_BINDING_NONCE={binding_nonce}"));
    }
    if let Some(event_jsonl_path) =
        proof_session.and_then(|summary| summary.event_jsonl_path.as_ref())
    {
        command.push(format!("SOMA_CLIENT_BINDING_EVENT_JSONL={event_jsonl_path}"));
    }
    command.push("tools/soma-client-hook-readiness.sh".to_string());
    command
}

fn private_hook_readiness_cli_command_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
    soma_bin: Option<&str>,
    wait_seconds: Option<u16>,
) -> Vec<String> {
    let mut command = vec![
        "tools/soma-client-hook-readiness.sh".to_string(),
        "--client".to_string(),
        client.as_str().to_string(),
    ];
    if let Some(soma_bin) = soma_bin.filter(|value| !value.trim().is_empty()) {
        command.extend(["--soma-bin".to_string(), soma_bin.to_string()]);
    }
    for (env_key, flag) in [
        ("SOMA_CLIENT_BINDING_MANIFEST", "--manifest"),
        ("SOMA_CLIENT_BINDING_CONFIG_ROOT", "--config-root"),
        ("SOMA_CLIENT_BINDING_PROJECT_ROOT", "--project-root"),
        ("SOMA_CLIENT_BINDING_LOG_ROOT", "--log-root"),
    ] {
        if let Some(value) = proof_session_command_env_value(proof_session, env_key) {
            command.extend([flag.to_string(), value]);
        }
    }
    let event_jsonl_path =
        proof_session.and_then(|summary| summary.event_jsonl_path.clone()).or_else(|| {
            proof_session_command_env_value(proof_session, "SOMA_CLIENT_BINDING_EVENT_JSONL")
        });
    if let Some(event_jsonl_path) = event_jsonl_path {
        command.extend(["--event-jsonl".to_string(), event_jsonl_path]);
    }
    if let Some(wait_seconds) = wait_seconds {
        command.extend(["--wait-seconds".to_string(), wait_seconds.to_string()]);
    }
    command
}

fn proof_session_command_env_value(
    proof_session: Option<&ClientProofSessionSummary>,
    key: &str,
) -> Option<String> {
    let prefix = format!("{key}=");
    proof_session?
        .next_command
        .as_ref()?
        .iter()
        .find_map(|part| part.strip_prefix(&prefix).map(ToOwned::to_owned))
}

fn enrich_private_hook_readiness_command(
    command: &mut Vec<String>,
    proof_session: &ClientProofSessionSummary,
) {
    insert_env_part_before_readiness_script(
        command,
        "SOMA_CLIENT_BINDING_EVENT_SOURCE",
        proof_session.expected_event_source.as_deref(),
    );
    insert_env_part_before_readiness_script(
        command,
        "SOMA_CLIENT_BINDING_NONCE",
        proof_session.binding_nonce.as_deref(),
    );
    insert_env_part_before_readiness_script(
        command,
        "SOMA_CLIENT_BINDING_EVENT_JSONL",
        proof_session.event_jsonl_path.as_deref(),
    );
}

fn insert_env_part_before_readiness_script(
    command: &mut Vec<String>,
    key: &str,
    value: Option<&str>,
) {
    insert_env_part_before_script(command, key, value, "tools/soma-client-hook-readiness.sh");
}

fn insert_env_part_before_script(
    command: &mut Vec<String>,
    key: &str,
    value: Option<&str>,
    script: &str,
) {
    let Some(value) = value else {
        return;
    };
    let prefix = format!("{key}=");
    if command.iter().any(|part| part.starts_with(&prefix)) {
        return;
    }
    let insert_at = command.iter().position(|part| part == script).unwrap_or(command.len());
    command.insert(insert_at, format!("{key}={value}"));
}

fn private_event_wait_command_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<Vec<String>> {
    if !is_private_app_client(client) {
        return None;
    }
    proof_session?.event_jsonl_path.as_ref()?;
    let mut command = private_hook_readiness_command_for(client, proof_session);
    let wait_part = "SOMA_CLIENT_BINDING_WAIT_SECONDS=30".to_string();
    if !command.iter().any(|part| part == &wait_part) {
        let insert_at = command
            .iter()
            .position(|part| part == "tools/soma-client-hook-readiness.sh")
            .unwrap_or(command.len());
        command.insert(insert_at, wait_part);
    }
    Some(command)
}

fn proof_session_next_command_for_row(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<Vec<String>> {
    let next_command = proof_session?.next_command.as_ref()?;
    if next_command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh") {
        return Some(private_hook_readiness_command_for(client, proof_session));
    }
    Some(next_command.clone())
}

fn proof_session_runbook_step_command_for_row(
    row: &ClientStatusRow,
    step_id: &str,
) -> Option<Vec<String>> {
    row.proof_session_runbook_steps
        .iter()
        .find(|step| step.id == step_id)
        .and_then(|step| step.command.clone())
}

fn proof_session_runbook_step_command(
    proof_session: &ClientProofSessionSummary,
    step_id: &str,
) -> Option<Vec<String>> {
    proof_session
        .runbook_steps
        .iter()
        .find(|step| step.id == step_id)
        .and_then(|step| step.command.clone())
}

fn private_event_contract_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<ClientPrivateEventContract> {
    if !is_private_app_client(client) {
        return None;
    }
    let proof_session = proof_session?;
    let event_source = proof_session.expected_event_source.as_ref()?;
    let binding_nonce = proof_session.binding_nonce.as_ref()?;
    Some(ClientPrivateEventContract {
        client: client.as_str().to_string(),
        event_source: event_source.clone(),
        binding_nonces: vec![binding_nonce.clone()],
        schema: "soma.adapter_spool_event.v1",
        writer_contract: "soma_adapter_spool_append_v1",
        observed_at_ns: "required_integer",
        source_boundary: "real_private_client_hook_only",
        trust_boundary:
            "private_event_contract_is_diagnostic_only: it describes the event required before observed_app_hook can be recorded; it records no proof row, creates no verification event, promotes no cloud draft, and cannot substitute for a real private client invocation",
    })
}

fn private_hook_lifecycle_wrapper(client: McpClientKind) -> &'static str {
    match client {
        McpClientKind::CodexApp => "tools/soma-codex-app-capture.sh",
        McpClientKind::Cursor | McpClientKind::Continue => "tools/soma-adapter-lifecycle.sh",
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => "tools/soma-adapter-lifecycle.sh",
    }
}

fn private_hook_lifecycle_event(client: McpClientKind) -> &'static str {
    match client {
        McpClientKind::Cursor => "turn_completed",
        McpClientKind::CodexApp | McpClientKind::Continue => "assistant_response",
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => "turn_completed",
    }
}

fn private_hook_stdin_event_template_json(client: McpClientKind, lifecycle_event: &str) -> String {
    let client_name = client.as_str();
    let value = if lifecycle_event == "assistant_response" {
        serde_json::json!({
            "event": lifecycle_event,
            "client": client_name,
            "project": "<project-name>",
            "session_id": format!("{client_name}-private-hook-session"),
            "cwd": "<project-root>",
            "hook_adapter": "manual_debug_non_release_template",
            "manual_invocation_policy": "non_release_debug_only",
            "prompt_text": "Private client prompt text seen locally.",
            "output_text": "Private client assistant output remains draft evidence until trusted verification.",
            "enqueue_proposal": true,
            "proposal_reason": "Private client integration should expose cloud output only as draft claims."
        })
    } else {
        serde_json::json!({
            "event": lifecycle_event,
            "client": client_name,
            "project": "<project-name>",
            "session_id": format!("{client_name}-private-hook-session"),
            "cwd": "<project-root>",
            "hook_adapter": "manual_debug_non_release_template",
            "manual_invocation_policy": "non_release_debug_only",
            "prompt_text": "Private client prompt text seen locally.",
            "response_text": "Private client turn reaches SOMA through the lifecycle wrapper."
        })
    };
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

fn private_hook_integration_template_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<ClientPrivateHookIntegrationTemplate> {
    if !is_private_app_client(client) {
        return None;
    }
    let proof_session = proof_session?;
    let event_source = proof_session.expected_event_source.as_ref()?;
    let binding_nonce = proof_session.binding_nonce.as_ref()?;
    let event_jsonl_path = proof_session.event_jsonl_path.as_ref()?;
    let expected_spool_contract = private_event_contract_for(client, Some(proof_session))?;
    let lifecycle_event = private_hook_lifecycle_event(client);
    let wrapper = private_hook_lifecycle_wrapper(client);
    let environment = BTreeMap::from([
        ("SOMA_ADAPTER_LIFECYCLE_CLIENT".to_string(), client.as_str().to_string()),
        ("SOMA_ADAPTER_LIFECYCLE_EVENT".to_string(), lifecycle_event.to_string()),
        ("SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE".to_string(), event_source.clone()),
        ("SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE".to_string(), binding_nonce.clone()),
        ("SOMA_ADAPTER_LIFECYCLE_JSONL".to_string(), event_jsonl_path.clone()),
    ]);
    let mut wrapper_command_template = vec!["env".to_string()];
    wrapper_command_template
        .extend(environment.iter().map(|(key, value)| format!("{key}={value}")));
    wrapper_command_template.push(wrapper.to_string());
    Some(ClientPrivateHookIntegrationTemplate {
        source: "soma_clients.private_hook_integration_template.v1",
        client: client.as_str().to_string(),
        read_only: true,
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        manual_invocation_policy: "non_release_debug_only",
        wrapper,
        wrapper_command_template,
        environment,
        stdin_event_template_json: private_hook_stdin_event_template_json(client, lifecycle_event),
        expected_spool_contract,
        operator_next_step: format!(
            "Wire this command into the private client's native lifecycle/hook path, reload that client, perform a real client action, then rerun `{}` or `tools/soma-client-hook-readiness.sh`.",
            soma_clients_command_text(&[])
        ),
        trust_boundary: "private_hook_integration_template_is_guidance_only: renders the wrapper/env/stdin contract a private client should call; records no proof row, creates no verification event, promotes no cloud draft, and manual terminal invocation is non-release debug evidence unless the private client actually invoked the hook and the operator later confirms release-grade evidence",
    })
}

fn private_event_watch_command_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<Vec<String>> {
    if !is_private_app_client(client) {
        return None;
    }
    let event_jsonl_path = proof_session?.event_jsonl_path.as_ref()?;
    Some(vec!["tail".to_string(), "-f".to_string(), event_jsonl_path.clone()])
}

fn private_event_observation_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<ClientPrivateEventObservation> {
    if !is_private_app_client(client) {
        return None;
    }
    let proof_session = proof_session?;
    let event_jsonl_path = proof_session.event_jsonl_path.as_ref()?;
    let expected_event_source = proof_session.expected_event_source.as_ref()?;
    let binding_nonce = proof_session.binding_nonce.as_ref()?;
    let binding_nonces = [binding_nonce.as_str()];
    let path = PathBuf::from(event_jsonl_path);
    if !path.exists() {
        return Some(ClientPrivateEventObservation {
            path: event_jsonl_path.clone(),
            exists: false,
            error: None,
            event_count: 0,
            invalid_event_count: 0,
            matching_private_event_count: 0,
            matching_private_binding_nonce_count: 0,
            matching_private_non_release_manual_event_count: 0,
            matching_private_non_release_manual_binding_nonce_count: 0,
            matching_private_non_release_test_event_count: 0,
            matching_private_non_release_test_binding_nonce_count: 0,
            matching_private_event_seen: false,
            matching_private_binding_nonce_seen: false,
            status: "event_jsonl_missing",
            latest_event: None,
            relevant_event: None,
            latest_spool_mismatches: vec!["event_jsonl_missing"],
            trust_boundary: PRIVATE_EVENT_OBSERVATION_TRUST_BOUNDARY,
        });
    }

    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) => {
            return Some(ClientPrivateEventObservation {
                path: event_jsonl_path.clone(),
                exists: true,
                error: Some(err.to_string()),
                event_count: 0,
                invalid_event_count: 0,
                matching_private_event_count: 0,
                matching_private_binding_nonce_count: 0,
                matching_private_non_release_manual_event_count: 0,
                matching_private_non_release_manual_binding_nonce_count: 0,
                matching_private_non_release_test_event_count: 0,
                matching_private_non_release_test_binding_nonce_count: 0,
                matching_private_event_seen: false,
                matching_private_binding_nonce_seen: false,
                status: "event_jsonl_unreadable",
                latest_event: None,
                relevant_event: None,
                latest_spool_mismatches: vec!["latest_event_unreadable"],
                trust_boundary: PRIVATE_EVENT_OBSERVATION_TRUST_BOUNDARY,
            });
        }
    };

    let mut event_count = 0;
    let mut invalid_event_count = 0;
    let mut matching_private_event_count = 0;
    let mut matching_private_binding_nonce_count = 0;
    let mut matching_private_non_release_manual_event_count = 0;
    let mut matching_private_non_release_manual_binding_nonce_count = 0;
    let mut matching_private_non_release_test_event_count = 0;
    let mut matching_private_non_release_test_binding_nonce_count = 0;
    let mut latest_event = None;
    let mut latest_matching_event = None;
    let mut latest_manual_debug_event = None;
    let mut latest_non_release_test_event = None;
    let mut read_error = None;

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                read_error = Some(err.to_string());
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(value) => value,
            Err(_) => {
                invalid_event_count += 1;
                continue;
            }
        };
        event_count += 1;
        let event = private_event_summary_from_value(&value);
        let private_match = event.schema.as_deref() == Some("soma.adapter_spool_event.v1")
            && event.writer_contract.as_deref() == Some("soma_adapter_spool_append_v1")
            && event
                .client
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(client.as_str()))
            && event.event_source.as_deref() == Some(expected_event_source.as_str())
            && event.observed_at_ns.is_some();
        if private_match {
            let binding_nonce_matches =
                event.binding_nonce.as_deref().is_some_and(|value| binding_nonces.contains(&value));
            if is_non_release_manual_event(&event) {
                matching_private_non_release_manual_event_count += 1;
                latest_manual_debug_event = Some(event.clone());
                if binding_nonce_matches {
                    matching_private_non_release_manual_binding_nonce_count += 1;
                }
            } else if is_non_release_test_event(&event) {
                matching_private_non_release_test_event_count += 1;
                latest_non_release_test_event = Some(event.clone());
                if binding_nonce_matches {
                    matching_private_non_release_test_binding_nonce_count += 1;
                }
            } else {
                matching_private_event_count += 1;
                latest_matching_event = Some(event.clone());
                if binding_nonce_matches {
                    matching_private_binding_nonce_count += 1;
                }
            }
        }
        latest_event = Some(event);
    }

    let relevant_event = latest_matching_event
        .clone()
        .or_else(|| latest_non_release_test_event.clone())
        .or_else(|| latest_manual_debug_event.clone())
        .or_else(|| latest_event.clone());
    let latest_spool_mismatches = private_event_mismatches(
        relevant_event.as_ref(),
        client,
        expected_event_source,
        &binding_nonces,
    );
    let matching_private_event_seen = matching_private_event_count > 0;
    let matching_private_binding_nonce_seen = matching_private_binding_nonce_count > 0;
    let status = private_event_observation_status(
        read_error.as_ref(),
        event_count,
        matching_private_event_count,
        matching_private_binding_nonce_count,
        matching_private_non_release_manual_event_count,
        matching_private_non_release_test_event_count,
    );

    Some(ClientPrivateEventObservation {
        path: event_jsonl_path.clone(),
        exists: true,
        error: read_error,
        event_count,
        invalid_event_count,
        matching_private_event_count,
        matching_private_binding_nonce_count,
        matching_private_non_release_manual_event_count,
        matching_private_non_release_manual_binding_nonce_count,
        matching_private_non_release_test_event_count,
        matching_private_non_release_test_binding_nonce_count,
        matching_private_event_seen,
        matching_private_binding_nonce_seen,
        status,
        latest_event,
        relevant_event,
        latest_spool_mismatches,
        trust_boundary: PRIVATE_EVENT_OBSERVATION_TRUST_BOUNDARY,
    })
}

fn codex_notify_reload_check_for(
    client: McpClientKind,
    proof_session: Option<&ClientProofSessionSummary>,
) -> Option<ClientCodexNotifyReloadCheck> {
    if client != McpClientKind::CodexApp {
        return None;
    }
    if proof_session.and_then(|session| session.next_step_id.as_deref())
        != Some("trigger_private_client_hook")
    {
        return None;
    }
    Some(build_codex_notify_reload_check(codex_notify_config_path()))
}

fn build_codex_notify_reload_check(config_path: Option<PathBuf>) -> ClientCodexNotifyReloadCheck {
    let config_path = match config_path {
        Some(path) => path,
        None => {
            return ClientCodexNotifyReloadCheck {
                source: "home_dir".to_string(),
                status: "config_path_unavailable",
                config_path: "~/.codex/config.toml".to_string(),
                config_mtime_unix: None,
                codex_desktop_process_count: 0,
                stale_codex_desktop_process_count: 0,
                restart_recommended: false,
                stale_processes: Vec::new(),
                error: Some("home directory could not be resolved".to_string()),
                trust_boundary: CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
            };
        }
    };
    let config_path_display = config_path.to_string_lossy().into_owned();
    let config_mtime_unix = match file_mtime_unix(&config_path) {
        Ok(mtime) => mtime,
        Err(err) => {
            return ClientCodexNotifyReloadCheck {
                source: "config_metadata".to_string(),
                status: if config_path.exists() {
                    "config_metadata_unavailable"
                } else {
                    "config_missing"
                },
                config_path: config_path_display,
                config_mtime_unix: None,
                codex_desktop_process_count: 0,
                stale_codex_desktop_process_count: 0,
                restart_recommended: false,
                stale_processes: Vec::new(),
                error: Some(err),
                trust_boundary: CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
            };
        }
    };

    let process_table = match codex_process_table_lines() {
        Ok(table) => table,
        Err(table) => {
            return ClientCodexNotifyReloadCheck {
                source: table.source,
                status: table.status,
                config_path: config_path_display,
                config_mtime_unix: Some(config_mtime_unix),
                codex_desktop_process_count: 0,
                stale_codex_desktop_process_count: 0,
                restart_recommended: false,
                stale_processes: Vec::new(),
                error: table.error,
                trust_boundary: CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
            };
        }
    };
    let processes = codex_desktop_processes_from_lines(&process_table.lines, config_mtime_unix);
    let stale_processes = processes
        .iter()
        .filter(|process| process.started_before_config)
        .cloned()
        .collect::<Vec<_>>();
    let status = if !stale_processes.is_empty() {
        "restart_recommended"
    } else if processes.is_empty() {
        "codex_app_not_running"
    } else {
        "codex_app_started_after_config"
    };

    ClientCodexNotifyReloadCheck {
        source: process_table.source,
        status,
        config_path: config_path_display,
        config_mtime_unix: Some(config_mtime_unix),
        codex_desktop_process_count: processes.len(),
        stale_codex_desktop_process_count: stale_processes.len(),
        restart_recommended: !stale_processes.is_empty(),
        stale_processes: stale_processes.into_iter().take(5).collect(),
        error: None,
        trust_boundary: CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
    }
}

fn continue_extension_config_check_for(
    client: McpClientKind,
    command: &str,
) -> Option<ClientContinueExtensionConfigCheck> {
    if client != McpClientKind::Continue {
        return None;
    }
    Some(build_continue_extension_config_check(
        continue_extension_config_candidate_paths(),
        command,
    ))
}

fn continue_extension_config_candidate_paths() -> Result<Vec<PathBuf>, String> {
    if let Some(path) = std::env::var_os("SOMA_CONTINUE_CONFIG")
        .or_else(|| std::env::var_os("SOMA_CONTINUE_CONFIG_PATH"))
    {
        return Ok(vec![PathBuf::from(path)]);
    }
    let root = std::env::var_os("SOMA_CONTINUE_CONFIG_ROOT")
        .or_else(|| std::env::var_os("SOMA_CLIENT_BINDING_CONFIG_ROOT"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| "home directory could not be resolved".to_string())?;
    let continue_dir = root.join(".continue");
    Ok(vec![
        continue_dir.join("mcpServers/soma.json"),
        continue_dir.join("config.yaml"),
        continue_dir.join("config.yml"),
        continue_dir.join("config.json"),
        continue_dir.join("config.ts"),
    ])
}

fn build_continue_extension_config_check(
    candidate_paths: Result<Vec<PathBuf>, String>,
    command: &str,
) -> ClientContinueExtensionConfigCheck {
    let mcp_config_command = continue_mcp_config_command(command);
    let devdata_install_command = continue_devdata_install_command();
    let devdata_collector_command = continue_devdata_collector_command(command);
    let devdata_collector_observation = continue_devdata_collector_observation();
    let extension_installation = continue_extension_installation_observation();
    let candidate_paths = match candidate_paths {
        Ok(paths) => paths,
        Err(err) => {
            let recommended_config_path = McpClientKind::Continue.target_path_hint().to_string();
            return ClientContinueExtensionConfigCheck {
                source: "continue_config_scan".to_string(),
                status: "config_root_unavailable",
                candidate_paths: Vec::new(),
                config_path: None,
                profile_config_status: "profile_config_not_checked",
                profile_config_path: None,
                profile_config_required_fields_present: false,
                profile_config_missing_required_fields: Vec::new(),
                profile_config_error: None,
                devdata_destination_status: "config_root_unavailable",
                devdata_destination_visible: false,
                devdata_config_path: None,
                devdata_destination: CONTINUE_DEVDATA_DESTINATION,
                devdata_install_command,
                devdata_collector_command,
                devdata_collector_status: devdata_collector_observation.status,
                devdata_collector_listening: devdata_collector_observation.listening,
                devdata_collector_host: devdata_collector_observation.host,
                devdata_collector_port: devdata_collector_observation.port,
                devdata_collector_error: devdata_collector_observation.error,
                devdata_collector_trust_boundary: devdata_collector_observation.trust_boundary,
                extension_installation_status: extension_installation.status,
                extension_candidate_roots: extension_installation.candidate_roots,
                extension_paths: extension_installation.extension_paths,
                extension_observed: extension_installation.observed,
                extension_next_step: extension_installation.next_step,
                recommended_config_path: recommended_config_path.clone(),
                mcp_config_command,
                merge_required: true,
                next_step: continue_extension_next_step(
                    "config_root_unavailable",
                    &recommended_config_path,
                    false,
                    false,
                    false,
                    devdata_collector_observation.status,
                ),
                has_model_context_protocol: false,
                has_mcp_servers: false,
                has_soma_server: false,
                restart_or_reload_recommended: false,
                error: Some(err),
                trust_boundary: CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
            };
        }
    };
    let candidate_path_strings =
        candidate_paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
    let devdata_observation = continue_devdata_destination_observation(&candidate_paths);
    let profile_config_observation = continue_profile_config_observation(&candidate_paths);
    let recommended_config_path = candidate_path_strings
        .first()
        .cloned()
        .unwrap_or_else(|| McpClientKind::Continue.target_path_hint().to_string());
    let mut first_existing_missing: Option<ClientContinueExtensionConfigCheck> = None;
    let mut first_unreadable: Option<ClientContinueExtensionConfigCheck> = None;
    for config_path in candidate_paths.iter().filter(|path| path.exists()) {
        let config_path_string = config_path.display().to_string();
        let text = match fs::read_to_string(config_path) {
            Ok(text) => text,
            Err(err) => {
                if first_unreadable.is_none() {
                    first_unreadable = Some(ClientContinueExtensionConfigCheck {
                        source: "continue_config_scan".to_string(),
                        status: "config_unreadable",
                        candidate_paths: candidate_path_strings.clone(),
                        config_path: Some(config_path_string),
                        profile_config_status: profile_config_observation.status,
                        profile_config_path: profile_config_observation.config_path.clone(),
                        profile_config_required_fields_present: profile_config_observation
                            .required_fields_present,
                        profile_config_missing_required_fields: profile_config_observation
                            .missing_required_fields
                            .clone(),
                        profile_config_error: profile_config_observation.error.clone(),
                        devdata_destination_status: devdata_observation.status,
                        devdata_destination_visible: devdata_observation.visible,
                        devdata_config_path: devdata_observation.config_path.clone(),
                        devdata_destination: CONTINUE_DEVDATA_DESTINATION,
                        devdata_install_command: devdata_install_command.clone(),
                        devdata_collector_command: devdata_collector_command.clone(),
                        devdata_collector_status: devdata_collector_observation.status,
                        devdata_collector_listening: devdata_collector_observation.listening,
                        devdata_collector_host: devdata_collector_observation.host.clone(),
                        devdata_collector_port: devdata_collector_observation.port,
                        devdata_collector_error: devdata_collector_observation.error.clone(),
                        devdata_collector_trust_boundary: devdata_collector_observation
                            .trust_boundary,
                        extension_installation_status: extension_installation.status,
                        extension_candidate_roots: extension_installation.candidate_roots.clone(),
                        extension_paths: extension_installation.extension_paths.clone(),
                        extension_observed: extension_installation.observed,
                        extension_next_step: extension_installation.next_step.clone(),
                        recommended_config_path: recommended_config_path.clone(),
                        mcp_config_command: mcp_config_command.clone(),
                        merge_required: true,
                        next_step: continue_extension_next_step(
                            "config_unreadable",
                            &recommended_config_path,
                            false,
                            devdata_observation.visible,
                            devdata_collector_observation.listening,
                            devdata_collector_observation.status,
                        ),
                        has_model_context_protocol: false,
                        has_mcp_servers: false,
                        has_soma_server: false,
                        restart_or_reload_recommended: false,
                        error: Some(err.to_string()),
                        trust_boundary: CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
                    });
                }
                continue;
            }
        };
        let (has_model_context_protocol, has_mcp_servers, has_soma_server) =
            continue_config_soma_mcp_flags(config_path, &text);
        let base_status = if (has_model_context_protocol || has_mcp_servers) && has_soma_server {
            "config_present_soma_mcp_seen"
        } else {
            "config_present_soma_mcp_missing"
        };
        let status = continue_config_status_with_profile(base_status, &profile_config_observation);
        let extension_observed = extension_installation.observed;
        let check = ClientContinueExtensionConfigCheck {
            source: "continue_config_scan".to_string(),
            status,
            candidate_paths: candidate_path_strings.clone(),
            config_path: Some(config_path_string),
            profile_config_status: profile_config_observation.status,
            profile_config_path: profile_config_observation.config_path.clone(),
            profile_config_required_fields_present: profile_config_observation
                .required_fields_present,
            profile_config_missing_required_fields: profile_config_observation
                .missing_required_fields
                .clone(),
            profile_config_error: profile_config_observation.error.clone(),
            devdata_destination_status: devdata_observation.status,
            devdata_destination_visible: devdata_observation.visible,
            devdata_config_path: devdata_observation.config_path.clone(),
            devdata_destination: CONTINUE_DEVDATA_DESTINATION,
            devdata_install_command: devdata_install_command.clone(),
            devdata_collector_command: devdata_collector_command.clone(),
            devdata_collector_status: devdata_collector_observation.status,
            devdata_collector_listening: devdata_collector_observation.listening,
            devdata_collector_host: devdata_collector_observation.host.clone(),
            devdata_collector_port: devdata_collector_observation.port,
            devdata_collector_error: devdata_collector_observation.error.clone(),
            devdata_collector_trust_boundary: devdata_collector_observation.trust_boundary,
            extension_installation_status: extension_installation.status,
            extension_candidate_roots: extension_installation.candidate_roots.clone(),
            extension_paths: extension_installation.extension_paths.clone(),
            extension_observed: extension_installation.observed,
            extension_next_step: extension_installation.next_step.clone(),
            recommended_config_path: recommended_config_path.clone(),
            mcp_config_command: mcp_config_command.clone(),
            merge_required: status != "config_present_soma_mcp_seen",
            next_step: continue_extension_next_step(
                status,
                &recommended_config_path,
                extension_observed,
                devdata_observation.visible,
                devdata_collector_observation.listening,
                devdata_collector_observation.status,
            ),
            has_model_context_protocol,
            has_mcp_servers,
            has_soma_server,
            restart_or_reload_recommended: status == "config_present_soma_mcp_seen",
            error: None,
            trust_boundary: CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
        };
        if status == "config_present_soma_mcp_seen" {
            return check;
        }
        if first_existing_missing.is_none() {
            first_existing_missing = Some(check);
        }
    }
    if let Some(check) = first_existing_missing {
        return check;
    }
    if let Some(check) = first_unreadable {
        return check;
    }
    ClientContinueExtensionConfigCheck {
        source: "continue_config_scan".to_string(),
        status: "config_missing",
        candidate_paths: candidate_path_strings,
        config_path: None,
        profile_config_status: profile_config_observation.status,
        profile_config_path: profile_config_observation.config_path,
        profile_config_required_fields_present: profile_config_observation.required_fields_present,
        profile_config_missing_required_fields: profile_config_observation.missing_required_fields,
        profile_config_error: profile_config_observation.error,
        devdata_destination_status: devdata_observation.status,
        devdata_destination_visible: devdata_observation.visible,
        devdata_config_path: devdata_observation.config_path,
        devdata_destination: CONTINUE_DEVDATA_DESTINATION,
        devdata_install_command,
        devdata_collector_command,
        devdata_collector_status: devdata_collector_observation.status,
        devdata_collector_listening: devdata_collector_observation.listening,
        devdata_collector_host: devdata_collector_observation.host,
        devdata_collector_port: devdata_collector_observation.port,
        devdata_collector_error: devdata_collector_observation.error,
        devdata_collector_trust_boundary: devdata_collector_observation.trust_boundary,
        extension_installation_status: extension_installation.status,
        extension_candidate_roots: extension_installation.candidate_roots,
        extension_paths: extension_installation.extension_paths,
        extension_observed: extension_installation.observed,
        extension_next_step: extension_installation.next_step,
        recommended_config_path: recommended_config_path.clone(),
        mcp_config_command,
        merge_required: true,
        next_step: continue_extension_next_step(
            "config_missing",
            &recommended_config_path,
            false,
            devdata_observation.visible,
            devdata_collector_observation.listening,
            devdata_collector_observation.status,
        ),
        has_model_context_protocol: false,
        has_mcp_servers: false,
        has_soma_server: false,
        restart_or_reload_recommended: false,
        error: None,
        trust_boundary: CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
    }
}

fn continue_config_soma_mcp_flags(config_path: &Path, text: &str) -> (bool, bool, bool) {
    let lowered = text.to_lowercase();
    let has_model_context_protocol = lowered.contains("modelcontextprotocol");
    let path_has_mcp_servers =
        config_path.to_string_lossy().to_lowercase().contains("/mcpservers/");
    let has_mcp_servers = lowered.contains("mcpservers") || path_has_mcp_servers;
    let file_names_soma = config_path
        .file_stem()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("soma"));
    let has_soma_server = (lowered.contains("soma") || file_names_soma)
        && (lowered.contains("mcp-serve") || has_model_context_protocol || has_mcp_servers);
    (has_model_context_protocol, has_mcp_servers, has_soma_server)
}

fn continue_config_status_with_profile(
    base_status: &'static str,
    profile_config: &ContinueProfileConfigObservation,
) -> &'static str {
    if !continue_profile_config_blocks(profile_config) {
        return base_status;
    }
    match base_status {
        "config_present_soma_mcp_seen" => "config_present_soma_mcp_profile_invalid",
        "config_present_soma_mcp_missing" | "config_missing" => "config_profile_invalid",
        _ => base_status,
    }
}

fn continue_profile_config_blocks(profile_config: &ContinueProfileConfigObservation) -> bool {
    matches!(
        profile_config.status,
        "profile_config_missing_required_fields" | "profile_config_unreadable"
    )
}

fn continue_profile_config_observation(
    candidate_paths: &[PathBuf],
) -> ContinueProfileConfigObservation {
    let profile_paths = candidate_paths
        .iter()
        .filter(|path| {
            path.file_name().and_then(|value| value.to_str()).is_some_and(|name| {
                name.eq_ignore_ascii_case("config.yaml") || name.eq_ignore_ascii_case("config.yml")
            })
        })
        .collect::<Vec<_>>();
    let mut first_unreadable: Option<(String, String)> = None;
    for config_path in profile_paths.into_iter().filter(|path| path.exists()) {
        let config_path_string = config_path.display().to_string();
        let text = match fs::read_to_string(config_path) {
            Ok(text) => text,
            Err(err) => {
                if first_unreadable.is_none() {
                    first_unreadable = Some((config_path_string, err.to_string()));
                }
                continue;
            }
        };
        let missing_required_fields = continue_profile_missing_required_fields(&text);
        if missing_required_fields.is_empty() {
            return ContinueProfileConfigObservation {
                status: "profile_config_required_fields_seen",
                config_path: Some(config_path_string),
                required_fields_present: true,
                missing_required_fields,
                error: None,
            };
        }
        return ContinueProfileConfigObservation {
            status: "profile_config_missing_required_fields",
            config_path: Some(config_path_string),
            required_fields_present: false,
            missing_required_fields,
            error: None,
        };
    }
    if let Some((config_path, err)) = first_unreadable {
        return ContinueProfileConfigObservation {
            status: "profile_config_unreadable",
            config_path: Some(config_path),
            required_fields_present: false,
            missing_required_fields: vec!["name".to_string(), "version".to_string()],
            error: Some(err),
        };
    }
    ContinueProfileConfigObservation {
        status: "profile_config_not_present",
        config_path: None,
        required_fields_present: true,
        missing_required_fields: Vec::new(),
        error: None,
    }
}

fn continue_profile_missing_required_fields(text: &str) -> Vec<String> {
    ["name", "version"]
        .into_iter()
        .filter(|required| !continue_profile_has_top_level_key(text, required))
        .map(str::to_string)
        .collect()
}

fn continue_profile_has_top_level_key(text: &str, required_key: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.len() != line.len() {
            return false;
        }
        let Some((key, _)) = trimmed.split_once(':') else {
            return false;
        };
        key.trim() == required_key
    })
}

fn continue_devdata_destination_observation(
    candidate_paths: &[PathBuf],
) -> ContinueDevdataDestinationObservation {
    let mut first_unreadable: Option<String> = None;
    for config_path in candidate_paths.iter().filter(|path| path.exists()) {
        let config_path_string = config_path.display().to_string();
        let text = match fs::read_to_string(config_path) {
            Ok(text) => text,
            Err(_) => {
                if first_unreadable.is_none() {
                    first_unreadable = Some(config_path_string);
                }
                continue;
            }
        };
        if text.contains(CONTINUE_DEVDATA_DESTINATION)
            || text.contains("SOMA local dev-data bridge")
        {
            return ContinueDevdataDestinationObservation {
                status: "devdata_destination_seen",
                visible: true,
                config_path: Some(config_path_string),
            };
        }
    }
    if let Some(config_path) = first_unreadable {
        return ContinueDevdataDestinationObservation {
            status: "devdata_config_unreadable",
            visible: false,
            config_path: Some(config_path),
        };
    }
    ContinueDevdataDestinationObservation {
        status: "devdata_destination_missing",
        visible: false,
        config_path: None,
    }
}

fn continue_mcp_config_command(command: &str) -> Vec<String> {
    vec![
        "soma".to_string(),
        "mcp-config".to_string(),
        "--client".to_string(),
        "continue".to_string(),
        "--command".to_string(),
        command.to_string(),
    ]
}

fn continue_devdata_install_command() -> Vec<String> {
    vec!["tools/soma-continue-devdata-install.py".to_string(), "--dry-run".to_string()]
}

fn continue_devdata_collector_command(command: &str) -> Vec<String> {
    let (host, port) = continue_devdata_collector_endpoint();
    vec![
        "tools/soma-continue-devdata-collector.py".to_string(),
        "--host".to_string(),
        host,
        "--port".to_string(),
        port.to_string(),
        "--soma-bin".to_string(),
        command.to_string(),
    ]
}

fn continue_devdata_collector_endpoint() -> (String, u16) {
    let host = std::env::var("SOMA_CONTINUE_DEVDATA_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CONTINUE_DEVDATA_DEFAULT_HOST.to_string());
    let port = std::env::var("SOMA_CONTINUE_DEVDATA_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(CONTINUE_DEVDATA_DEFAULT_PORT);
    (host, port)
}

fn continue_devdata_collector_observation() -> ContinueDevdataCollectorObservation {
    let (host, port) = continue_devdata_collector_endpoint();
    if let Ok(status) = std::env::var("SOMA_CONTINUE_DEVDATA_COLLECTOR_STATUS") {
        let normalized = status.trim().to_ascii_lowercase();
        if normalized == "listening" {
            return ContinueDevdataCollectorObservation {
                status: "listening",
                listening: true,
                host,
                port,
                error: None,
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            };
        }
        if normalized == "not_listening" || normalized == "not-listening" {
            return ContinueDevdataCollectorObservation {
                status: "not_listening",
                listening: false,
                host,
                port,
                error: None,
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            };
        }
        if normalized == "probe_blocked" || normalized == "probe-blocked" {
            return ContinueDevdataCollectorObservation {
                status: "probe_blocked",
                listening: false,
                host,
                port,
                error: Some("collector TCP probe blocked by execution context".to_string()),
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            };
        }
        if normalized == "probe_unavailable" || normalized == "probe-unavailable" {
            return ContinueDevdataCollectorObservation {
                status: "probe_unavailable",
                listening: false,
                host,
                port,
                error: Some("collector TCP probe unavailable in execution context".to_string()),
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            };
        }
    }

    let address = format!("{host}:{port}");
    let mut addrs = match address.to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(err) => {
            return ContinueDevdataCollectorObservation {
                status: "probe_unavailable",
                listening: false,
                host,
                port,
                error: Some(err.to_string()),
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            };
        }
    };
    let Some(addr) = addrs.next() else {
        return ContinueDevdataCollectorObservation {
            status: "probe_unavailable",
            listening: false,
            host,
            port,
            error: Some("no socket address resolved".to_string()),
            trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
        };
    };
    match TcpStream::connect_timeout(&addr, Duration::from_millis(150)) {
        Ok(_) => ContinueDevdataCollectorObservation {
            status: "listening",
            listening: true,
            host,
            port,
            error: None,
            trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
        },
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            ContinueDevdataCollectorObservation {
                status: "probe_blocked",
                listening: false,
                host,
                port,
                error: Some(err.to_string()),
                trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
            }
        }
        Err(err) => ContinueDevdataCollectorObservation {
            status: "not_listening",
            listening: false,
            host,
            port,
            error: Some(err.to_string()),
            trust_boundary: CONTINUE_DEVDATA_COLLECTOR_TRUST_BOUNDARY,
        },
    }
}

fn continue_extension_installation_observation() -> ContinueExtensionInstallationObservation {
    let candidate_roots = match continue_extension_candidate_roots() {
        Ok(roots) => roots,
        Err(err) => {
            return ContinueExtensionInstallationObservation {
                status: "extension_root_unavailable",
                candidate_roots: Vec::new(),
                extension_paths: Vec::new(),
                observed: false,
                next_step: format!(
                    "Resolve Continue extension root lookup first ({err}); then install or enable Continue, reload the editor, and run a real turn."
                ),
            };
        }
    };
    let candidate_root_strings =
        candidate_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
    let extension_paths = continue_extension_paths_from_roots(&candidate_roots);
    let extension_path_strings =
        extension_paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
    let observed = !extension_path_strings.is_empty();
    ContinueExtensionInstallationObservation {
        status: if observed { "extension_observed" } else { "extension_not_observed" },
        candidate_roots: candidate_root_strings,
        extension_paths: extension_path_strings,
        observed,
        next_step: if observed {
            "Continue extension installation is locally observable; reload the extension/editor, run a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the hook readiness probe."
                .to_string()
        } else {
            "No local Continue extension install was observed in common VS Code/Cursor extension paths; install or enable Continue, reload the editor, run a real Continue extension turn (not Cursor Agent/Composer), then rerun the hook readiness probe."
                .to_string()
        },
    }
}

fn continue_extension_candidate_roots() -> Result<Vec<PathBuf>, String> {
    if let Some(path) = std::env::var_os("SOMA_CONTINUE_EXTENSION_PATH") {
        return Ok(vec![PathBuf::from(path)]);
    }
    let root = std::env::var_os("SOMA_CONTINUE_CONFIG_ROOT")
        .or_else(|| std::env::var_os("SOMA_CLIENT_BINDING_CONFIG_ROOT"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| "home directory could not be resolved".to_string())?;
    Ok(vec![
        root.join(".vscode/extensions"),
        root.join(".cursor/extensions"),
        root.join("Library/Application Support/Code/User/globalStorage"),
        root.join("Library/Application Support/Cursor/User/globalStorage"),
    ])
}

fn continue_extension_paths_from_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        if is_continue_extension_path(root) {
            paths.push(root.clone());
            continue;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_continue_extension_path(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn is_continue_extension_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lowered = name.to_lowercase();
    lowered.contains("continue")
}

fn continue_extension_next_step(
    status: &str,
    recommended_config_path: &str,
    extension_observed: bool,
    devdata_visible: bool,
    devdata_collector_listening: bool,
    devdata_collector_status: &str,
) -> String {
    let continue_mcp_config_command = soma_mcp_config_command_text(&["--client", "continue"]);
    match status {
        "config_present_soma_mcp_seen"
            if extension_observed && devdata_visible && devdata_collector_listening =>
        {
            "Continue can see SOMA MCP config and the SOMA local dev-data destination, and the collector endpoint is listening; reload Continue, run a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the hook readiness probe.".to_string()
        }
        "config_present_soma_mcp_seen"
            if extension_observed
                && devdata_visible
                && matches!(devdata_collector_status, "probe_blocked" | "probe_unavailable") =>
        {
            "Continue can see SOMA MCP config and the SOMA local dev-data destination, but SOMA could not prove collector listening from this execution context; run the managed collector status command from the operator shell before starting duplicates or triggering a real Continue action.".to_string()
        }
        "config_present_soma_mcp_seen" if extension_observed && devdata_visible => {
            "Continue can see SOMA MCP config and the SOMA local dev-data destination, but the collector endpoint is not listening; start the collector, reload Continue, run a real Continue extension chat/edit/review action (not Cursor Agent/Composer), then rerun the hook readiness probe.".to_string()
        }
        "config_present_soma_mcp_seen" if extension_observed => {
            "Continue can see SOMA MCP config, but the local dev-data destination is not visible; run `tools/soma-continue-devdata-install.py --dry-run`, write it if correct, start the collector, reload Continue, run a real turn, then rerun the hook readiness probe.".to_string()
        }
        "config_present_soma_mcp_seen" => {
            "Continue can see a SOMA MCP server file or mcpServers entry (legacy modelContextProtocol is only accepted for compatibility), but no local Continue extension install was observed; install or enable Continue, reload the editor, run a real turn, then rerun the hook readiness probe.".to_string()
        }
        "config_present_soma_mcp_profile_invalid" => {
            "Continue can see SOMA MCP config, but the local Continue profile config.yaml/config.yml is rejected by Continue because required top-level fields such as name/version are missing or unreadable; run `tools/soma-continue-devdata-install.py --dry-run`, write the repair if correct, reload Continue, then rerun readiness before triggering a real turn.".to_string()
        }
        "config_profile_invalid" => format!(
            "Repair Continue's config.yaml/config.yml top-level name/version fields, write `{continue_mcp_config_command}` output to `{recommended_config_path}` if SOMA MCP is still missing, reload Continue, then rerun readiness."
        ),
        "config_present_soma_mcp_missing" => format!(
            "Write `{continue_mcp_config_command}` output to `{recommended_config_path}`, reload Continue, then complete a real turn before recording observed_app_hook proof."
        ),
        "config_unreadable" => format!(
            "Make `{recommended_config_path}` readable, write `{continue_mcp_config_command}` output there if needed, reload Continue, then rerun readiness."
        ),
        "config_root_unavailable" => {
            "Set HOME or SOMA_CONTINUE_CONFIG_ROOT so SOMA can locate Continue config, then write the generated MCP server JSON and reload Continue.".to_string()
        }
        _ => format!(
            "Create `{recommended_config_path}` from `{continue_mcp_config_command}`, reload Continue, then complete a real turn before recording observed_app_hook proof."
        ),
    }
}

fn continue_extension_config_not_visible(check: &ClientContinueExtensionConfigCheck) -> bool {
    matches!(
        check.status,
        "config_missing"
            | "config_present_soma_mcp_missing"
            | "config_present_soma_mcp_profile_invalid"
            | "config_profile_invalid"
            | "config_unreadable"
            | "config_root_unavailable"
    )
}

fn continue_profile_config_invalid(check: &ClientContinueExtensionConfigCheck) -> bool {
    matches!(check.status, "config_present_soma_mcp_profile_invalid" | "config_profile_invalid")
}

fn continue_profile_config_invalid_for_row(row: &ClientStatusRow) -> bool {
    row.continue_extension_config_check.as_ref().is_some_and(continue_profile_config_invalid)
}

fn codex_notify_config_path() -> Option<PathBuf> {
    std::env::var_os("SOMA_CODEX_NOTIFY_CONFIG")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex/config.toml")))
}

fn file_mtime_unix(path: &Path) -> Result<i64, String> {
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|err| err.to_string())?;
    modified
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|err| err.to_string())
}

struct CodexProcessTableLines {
    source: String,
    status: &'static str,
    lines: Vec<String>,
    error: Option<String>,
}

fn codex_process_table_lines() -> Result<CodexProcessTableLines, CodexProcessTableLines> {
    if let Some(fixture) = std::env::var_os("SOMA_CODEX_NOTIFY_PS_OUTPUT") {
        let fixture = PathBuf::from(fixture);
        if fixture.is_file() {
            return match std::fs::read_to_string(&fixture) {
                Ok(contents) => Ok(CodexProcessTableLines {
                    source: "env_file:SOMA_CODEX_NOTIFY_PS_OUTPUT".to_string(),
                    status: "available",
                    lines: contents.lines().map(ToOwned::to_owned).collect(),
                    error: None,
                }),
                Err(err) => Err(CodexProcessTableLines {
                    source: "env_file:SOMA_CODEX_NOTIFY_PS_OUTPUT".to_string(),
                    status: "unavailable",
                    lines: Vec::new(),
                    error: Some(err.to_string()),
                }),
            };
        }
        return Ok(CodexProcessTableLines {
            source: "env_literal:SOMA_CODEX_NOTIFY_PS_OUTPUT".to_string(),
            status: "available",
            lines: fixture.to_string_lossy().lines().map(ToOwned::to_owned).collect(),
            error: None,
        });
    }
    if std::env::var_os("SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK").is_some_and(|value| value == "1") {
        return Err(CodexProcessTableLines {
            source: "env:SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK".to_string(),
            status: "skipped",
            lines: Vec::new(),
            error: None,
        });
    }
    let output = match Command::new("ps").args(["-axo", "pid,lstart,command"]).output() {
        Ok(output) => output,
        Err(err) => {
            return Err(CodexProcessTableLines {
                source: "ps".to_string(),
                status: "unavailable",
                lines: Vec::new(),
                error: Some(err.to_string()),
            });
        }
    };
    if !output.status.success() {
        return Err(CodexProcessTableLines {
            source: "ps".to_string(),
            status: "unavailable",
            lines: Vec::new(),
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        });
    }
    Ok(CodexProcessTableLines {
        source: "ps".to_string(),
        status: "available",
        lines: String::from_utf8_lossy(&output.stdout).lines().map(ToOwned::to_owned).collect(),
        error: None,
    })
}

fn codex_desktop_processes_from_lines(
    lines: &[String],
    config_mtime_unix: i64,
) -> Vec<ClientCodexProcessSummary> {
    lines
        .iter()
        .filter_map(|line| codex_desktop_process_from_ps_line(line, config_mtime_unix))
        .collect()
}

fn codex_desktop_process_from_ps_line(
    line: &str,
    config_mtime_unix: i64,
) -> Option<ClientCodexProcessSummary> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<i32>().ok()?;
    let weekday = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    let clock = parts.next()?;
    let year = parts.next()?;
    let command = parts.collect::<Vec<_>>().join(" ");
    if command != "/Applications/Codex.app/Contents/MacOS/Codex" {
        return None;
    }
    let started_at = parse_ps_lstart_unix(&format!("{weekday} {month} {day} {clock} {year}"))?;
    Some(ClientCodexProcessSummary {
        pid,
        started_at_unix: started_at,
        started_before_config: started_at < config_mtime_unix,
        command,
    })
}

fn parse_ps_lstart_unix(value: &str) -> Option<i64> {
    parse_ps_lstart_unix_with_bsd_date(value).or_else(|| parse_ps_lstart_unix_estimate(value))
}

#[cfg(target_os = "macos")]
fn parse_ps_lstart_unix_with_bsd_date(value: &str) -> Option<i64> {
    let output = Command::new("date")
        .args(["-j", "-f", "%a %b %d %H:%M:%S %Y", value, "+%s"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse::<i64>().ok()
}

#[cfg(not(target_os = "macos"))]
fn parse_ps_lstart_unix_with_bsd_date(_value: &str) -> Option<i64> {
    None
}

fn parse_ps_lstart_unix_estimate(value: &str) -> Option<i64> {
    let mut parts = value.split_whitespace();
    let _weekday = parts.next()?;
    let month = ps_month_number(parts.next()?)?;
    let day = parts.next()?.parse::<u32>().ok()?;
    let (hour, minute, second) = parse_ps_clock(parts.next()?)?;
    let year = parts.next()?.parse::<i32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    unix_from_ymd_hms(year, month, day, hour, minute, second)
}

fn ps_month_number(value: &str) -> Option<u32> {
    match value {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_ps_clock(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    let second = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hour, minute, second))
}

fn unix_from_ymd_hms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(
        days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i32::from(month <= 2);
    let era = if adjusted_year >= 0 { adjusted_year } else { adjusted_year - 399 } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month as i32 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era * 146_097 + day_of_era - 719_468)
}

fn private_event_summary_from_value(value: &serde_json::Value) -> ClientPrivateEventSummary {
    let payload = value.get("payload").and_then(serde_json::Value::as_object);
    let payload_string = |key: &str| {
        payload
            .and_then(|payload| payload.get(key))
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get(key).and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
    };
    let payload_bool = |key: &str| {
        payload
            .and_then(|payload| payload.get(key))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| value.get(key).and_then(serde_json::Value::as_bool))
    };
    let payload_text_present = |key: &str| {
        payload.and_then(|payload| payload.get(key)).is_some_and(|value| match value {
            serde_json::Value::Null => false,
            serde_json::Value::String(text) => !text.is_empty(),
            _ => true,
        })
    };
    ClientPrivateEventSummary {
        kind: value.get("kind").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        schema: value.get("schema").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        writer_contract: value
            .get("writer_contract")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        observed_at_ns: value.get("observed_at_ns").and_then(serde_json::Value::as_i64),
        client: payload_string("client").or_else(|| payload_string("source")),
        event_source: payload_string("event_source"),
        binding_nonce: payload_string("binding_nonce").or_else(|| {
            value.get("binding_nonce").and_then(serde_json::Value::as_str).map(ToOwned::to_owned)
        }),
        hook_adapter: payload_string("hook_adapter"),
        manual_invocation_policy: payload_string("manual_invocation_policy"),
        collector_release_grade_candidate: payload_bool("collector_release_grade_candidate"),
        continue_profile_id: payload_string("continue_profile_id"),
        session_id: payload_string("session_id"),
        thread_id: payload_string("thread_id"),
        model_provider: payload_string("model_provider"),
        model_name: payload_string("model_name"),
        model_title: payload_string("model_title"),
        has_prompt_text: payload_text_present("prompt_text"),
        has_response_text: payload_text_present("response_text"),
        has_output_text: payload_text_present("output_text"),
    }
}

fn private_event_mismatches(
    relevant_event: Option<&ClientPrivateEventSummary>,
    client: McpClientKind,
    expected_event_source: &str,
    binding_nonces: &[&str],
) -> Vec<&'static str> {
    let Some(event) = relevant_event else {
        return vec!["latest_event_unreadable"];
    };
    let mut mismatches = Vec::new();
    if event.schema.as_deref() != Some("soma.adapter_spool_event.v1") {
        mismatches.push("schema");
    }
    if event.writer_contract.as_deref() != Some("soma_adapter_spool_append_v1") {
        mismatches.push("writer_contract");
    }
    if !event.client.as_deref().is_some_and(|value| value.eq_ignore_ascii_case(client.as_str())) {
        mismatches.push("client");
    }
    if event.event_source.as_deref() != Some(expected_event_source) {
        mismatches.push("event_source");
    }
    if !event.binding_nonce.as_deref().is_some_and(|value| binding_nonces.contains(&value)) {
        mismatches.push("binding_nonce");
    }
    if event.observed_at_ns.is_none() {
        mismatches.push("observed_at_ns");
    }
    if is_non_release_manual_event(event) {
        mismatches.push("manual_debug_non_release_hook_adapter");
    }
    if is_non_release_test_event(event) {
        mismatches.push("dogfood_or_synthetic_test_event");
    }
    mismatches
}

fn private_event_observation_status(
    read_error: Option<&String>,
    event_count: usize,
    matching_private_event_count: usize,
    matching_private_binding_nonce_count: usize,
    matching_non_release_manual_event_count: usize,
    matching_non_release_test_event_count: usize,
) -> &'static str {
    if read_error.is_some() && event_count == 0 {
        "event_jsonl_unreadable"
    } else if matching_private_binding_nonce_count > 0 {
        "matching_private_binding_nonce_seen"
    } else if matching_private_event_count > 0 {
        "matching_private_event_seen_binding_nonce_mismatch"
    } else if matching_non_release_test_event_count > 0 {
        "matching_non_release_test_event_ignored_for_release"
    } else if matching_non_release_manual_event_count > 0 {
        "matching_manual_debug_event_ignored_for_release"
    } else {
        "matching_private_event_missing"
    }
}

fn is_non_release_manual_event(event: &ClientPrivateEventSummary) -> bool {
    is_non_release_manual_marker(event.hook_adapter.as_deref())
        || is_non_release_manual_marker(event.manual_invocation_policy.as_deref())
}

fn is_non_release_test_event(event: &ClientPrivateEventSummary) -> bool {
    if event.collector_release_grade_candidate == Some(false) {
        return true;
    }
    [
        event.continue_profile_id.as_deref(),
        event.session_id.as_deref(),
        event.thread_id.as_deref(),
        event.model_provider.as_deref(),
        event.model_name.as_deref(),
        event.model_title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(contains_non_release_test_marker)
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

fn row_for_client(
    client: McpClientKind,
    mcp: &mcp_config::McpConfigCheckReport,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    proof_storage_status: &'static str,
) -> ClientStatusRow {
    let proof_storage_available = proof_storage_status == "available";
    let capture_model = capture_model(client);
    let missing_proof_levels = missing_proof_levels(client, binding);
    let proof_level_statuses = proof_level_statuses(client, binding);
    let ready_for_private_client_claim =
        private_client_claim_ready(client, binding, proof_session, proof_storage_available);
    let ready_for_client_operator_loop = proof_storage_available
        && binding.is_some_and(|status| status.ready_for_client_operator_loop);
    let private_target_config_missing = private_target_config_missing_for_release(
        client,
        binding,
        proof_session,
        proof_storage_status,
    );
    let private_capture_status =
        private_capture_status(client, binding, proof_session, proof_storage_status);
    let artifact_failure_count = binding.map_or(0, |status| status.artifact_failures.len());
    let coherence_failure_count = binding.map_or(0, |status| status.coherence_failures.len());
    let proof_stage =
        if proof_storage_available { binding.map(client_status_proof_stage) } else { None };
    let continue_extension_config_check = continue_extension_config_check_for(client, &mcp.command);
    let codex_notify_reload_check = codex_notify_reload_check_for(client, proof_session);
    let private_event_wait_command = private_event_wait_command_for(client, proof_session);
    let simple_private_hook_readiness_command = is_private_app_client(client).then(|| {
        private_hook_readiness_cli_command_for(client, proof_session, Some(&mcp.command), None)
    });
    let simple_private_event_wait_command = private_event_wait_command.as_ref().map(|_| {
        private_hook_readiness_cli_command_for(client, proof_session, Some(&mcp.command), Some(30))
    });
    let mut next_commands = vec![vec![
        "soma".to_string(),
        "mcp-config".to_string(),
        "--client".to_string(),
        client.as_str().to_string(),
        "--check".to_string(),
    ]];
    match client {
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            if client == McpClientKind::Continue
                && continue_extension_config_check
                    .as_ref()
                    .is_some_and(continue_profile_config_invalid)
            {
                if let Some(check) = &continue_extension_config_check {
                    next_commands.push(check.devdata_install_command.clone());
                }
            }
            if client == McpClientKind::Continue
                && continue_extension_config_check.as_ref().is_some_and(|check| {
                    continue_extension_config_not_visible(check)
                        && (!continue_profile_config_invalid(check)
                            || check.status == "config_profile_invalid")
                })
            {
                next_commands.push(vec![
                    "soma".to_string(),
                    "mcp-config".to_string(),
                    "--client".to_string(),
                    "continue".to_string(),
                    "--command".to_string(),
                    mcp.command.clone(),
                ]);
            }
            if client == McpClientKind::Continue
                && proof_session.and_then(|session| session.next_step_id.as_deref()).is_some_and(
                    |step| {
                        matches!(
                            step,
                            "trigger_private_client_hook"
                                | "start_continue_devdata_collector_before_real_hook"
                        )
                    },
                )
            {
                if let Some(check) = &continue_extension_config_check {
                    if continue_devdata_collector_start_needed(check) {
                        let mut command = check.devdata_collector_command.clone();
                        if let Some(session) = proof_session {
                            if let Some(event_jsonl) = session.event_jsonl_path.as_deref() {
                                command.extend(["--jsonl".to_string(), event_jsonl.to_string()]);
                            }
                            if let Some(binding_config) =
                                session.eligible_private_client_target_paths.first()
                            {
                                command.extend([
                                    "--binding-config".to_string(),
                                    binding_config.clone(),
                                ]);
                            }
                        }
                        next_commands.push(command);
                    } else if !check.devdata_destination_visible {
                        next_commands.push(check.devdata_install_command.clone());
                    }
                }
            }
            if client == McpClientKind::CodexApp
                && proof_session.and_then(|session| session.next_step_id.as_deref())
                    == Some("trigger_private_client_hook")
                && codex_notify_reload_check.as_ref().is_some_and(|check| check.restart_recommended)
            {
                next_commands.push(vec![
                    "osascript".to_string(),
                    "-e".to_string(),
                    "quit app \"Codex\"".to_string(),
                ]);
                next_commands.push(vec!["open".to_string(), "-a".to_string(), "Codex".to_string()]);
            }
            if binding.is_some_and(|status| status.readiness == "artifact_integrity_failed") {
                next_commands.push(vec![
                    "soma".to_string(),
                    "adapter-binding-proof".to_string(),
                    "--client".to_string(),
                    client.as_str().to_string(),
                    "--status".to_string(),
                    "--json".to_string(),
                ]);
            }
            next_commands.push(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--proof-session".to_string(),
                "--json".to_string(),
            ]);
            if let Some(command) = private_target_config_install_command_for(client, proof_session)
            {
                next_commands.push(command);
            }
            next_commands.push(private_hook_readiness_command_for(client, proof_session));
            if let Some(command) = &private_event_wait_command {
                next_commands.push(command.clone());
            }
            if let Some(command) = &simple_private_hook_readiness_command {
                next_commands.push(command.clone());
            }
            if let Some(command) = &simple_private_event_wait_command {
                next_commands.push(command.clone());
            }
        }
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => {
            next_commands.push(vec![format!(
                "tools/soma-{}.sh",
                if client == McpClientKind::ClaudeCode { "claude-code-cli" } else { "codex-cli" }
            )]);
        }
    }

    let mut safe_to_claim = Vec::new();
    if mcp.readiness.mcp_registration_ready {
        safe_to_claim.push("Generated MCP registration points at soma mcp-serve.".to_string());
    }
    if matches!(client, McpClientKind::ClaudeCode | McpClientKind::CodexCli) {
        safe_to_claim.push(
            "Explicit MCP/adapter capture and managed shell scope can be dogfooded.".to_string(),
        );
    }
    if ready_for_client_operator_loop {
        safe_to_claim.push(
            "The app-hook/render/review-action operator loop has complete proof rows.".to_string(),
        );
    }
    if ready_for_private_client_claim {
        safe_to_claim.push("Release-grade private-client capture claim is ready.".to_string());
    }

    let mut blocked_claims = Vec::new();
    if !mcp.readiness.mcp_registration_ready {
        blocked_claims.push("MCP registration readiness.".to_string());
    }
    if mcp.readiness.client_runtime.required_for_mcp_registration
        && mcp.readiness.client_runtime.status == "missing"
    {
        blocked_claims.push(format!("{} runtime detection.", mcp.readiness.client_runtime.target));
    }
    if !ready_for_private_client_claim {
        blocked_claims.push(
            "Automatic private capture / in-client review readiness is not proven.".to_string(),
        );
    }
    if binding.is_some_and(|status| status.readiness == "artifact_integrity_failed") {
        blocked_claims.push(
            "Stored client-binding proof artifacts changed; refresh or replay evidence before claiming readiness.".to_string(),
        );
    }
    if private_target_config_missing {
        blocked_claims.push(
            "Known private-client target config is not currently discoverable; stored proof rows alone cannot claim current private-client readiness.".to_string(),
        );
    }
    if !proof_storage_available {
        blocked_claims.push("Client binding proof storage is unreadable.".to_string());
    }
    if continue_extension_config_check.as_ref().is_some_and(continue_extension_config_not_visible) {
        blocked_claims
            .push("Continue extension config is not visibly wired to SOMA MCP.".to_string());
    }
    if continue_extension_config_check.as_ref().is_some_and(|check| !check.extension_observed) {
        blocked_claims.push(
            "Continue extension installation was not observed in local editor extension paths."
                .to_string(),
        );
    }
    let artifact_repair_plan = artifact_repair_plan_for_client(client, binding);
    let artifact_repair_summary =
        artifact_repair_plan.as_ref().map(|plan| artifact_repair_summary_for_client(client, plan));
    let next_step = next_step(
        client,
        mcp,
        binding,
        proof_session,
        artifact_repair_plan.as_ref(),
        proof_storage_status,
        continue_extension_config_check.as_ref(),
    );

    let mut row = ClientStatusRow {
        client: client.as_str(),
        display_name: client.display_name(),
        status: String::new(),
        ready: false,
        ready_scope: "uncomputed",
        ready_meaning: "readiness contract has not been computed",
        mcp_context_ready: false,
        stored_local_capture_observed: false,
        latest_real_cli_capture_observed: None,
        release_ready: false,
        readiness_summary: String::new(),
        target_path_hint: client.target_path_hint(),
        mcp_registration_ready: mcp.readiness.mcp_registration_ready,
        mcp_status: mcp.readiness.status.to_string(),
        runtime_status: mcp.readiness.client_runtime.status.to_string(),
        runtime_target: mcp.readiness.client_runtime.target.to_string(),
        runtime_path: mcp.readiness.client_runtime.path.clone(),
        runtime_launch_probe_command: mcp.readiness.client_runtime.launch_probe_command.clone(),
        runtime_launch_probe_note: mcp.readiness.client_runtime.launch_probe_note.clone(),
        proof_storage_status,
        capture_model,
        goal_status: goal_status(client, mcp, binding, proof_session, proof_storage_status),
        private_capture_status,
        ready_for_private_client_claim,
        ready_for_client_operator_loop,
        observed_capture_dogfood_evidence: None,
        real_cli_dogfood_probe: None,
        artifact_failure_count,
        coherence_failure_count,
        proof_stage,
        missing_proof_levels,
        proof_level_statuses,
        proof_session_status: proof_session.and_then(|session| session.status.clone()),
        proof_session_release_gate: proof_session.and_then(|session| session.release_gate.clone()),
        proof_session_next_step_id: proof_session.and_then(|session| session.next_step_id.clone()),
        proof_session_next_operator_step_title: proof_session
            .and_then(|session| session.next_operator_step_title.clone()),
        proof_session_next_operator_step_intent: proof_session
            .and_then(|session| session.next_operator_step_intent.clone()),
        proof_session_next_operator_step_trust_boundary: proof_session
            .and_then(|session| session.next_operator_step_trust_boundary.clone()),
        proof_session_next_operator_step_requires_operator_action: proof_session
            .and_then(|session| session.next_operator_step_requires_operator_action),
        proof_session_next_command: proof_session_next_command_for_row(client, proof_session),
        proof_session_next_mcp_tool: proof_session
            .and_then(|session| session.next_mcp_tool.clone()),
        proof_session_next_mcp_arguments: proof_session
            .and_then(|session| session.next_mcp_arguments.clone()),
        proof_session_external_action: proof_session
            .and_then(|session| session.external_action.clone()),
        expected_event_source: proof_session
            .and_then(|session| session.expected_event_source.clone()),
        binding_nonce: proof_session.and_then(|session| session.binding_nonce.clone()),
        generated_binding_nonce: proof_session.and_then(|session| session.generated_binding_nonce),
        event_jsonl_path: proof_session.and_then(|session| session.event_jsonl_path.clone()),
        event_jsonl_probe_status: proof_session
            .and_then(|session| session.event_jsonl_probe_status.clone()),
        private_event_contract: private_event_contract_for(client, proof_session),
        private_hook_integration_template: private_hook_integration_template_for(
            client,
            proof_session,
        ),
        private_event_watch_command: private_event_watch_command_for(client, proof_session),
        private_event_wait_command,
        simple_private_hook_readiness_command,
        simple_private_event_wait_command,
        private_event_observation: private_event_observation_for(client, proof_session),
        codex_notify_reload_check,
        continue_extension_config_check,
        proof_session_blocking_reasons: proof_session
            .map(|session| session.blocking_reasons.clone())
            .unwrap_or_default(),
        proof_session_ready_to_record_proof_levels: proof_session
            .map(|session| session.ready_to_record_proof_levels.clone())
            .unwrap_or_default(),
        proof_session_stage_blockers: proof_session
            .map(|session| session.stage_blockers.clone())
            .unwrap_or_default(),
        proof_session_runbook_steps: proof_session
            .map(|session| session.runbook_steps.clone())
            .unwrap_or_default(),
        proof_session_ready_now_step_count: proof_session
            .and_then(|session| session.ready_now_step_count),
        proof_session_blocking_reason_count: proof_session
            .and_then(|session| session.blocking_reason_count),
        installed_config_eligible_candidates: proof_session
            .and_then(|session| session.installed_config_eligible_candidates),
        installed_config_setup_artifact_eligible_candidates: proof_session
            .and_then(|session| session.installed_config_setup_artifact_eligible_candidates),
        installed_config_private_target_eligible_candidates: proof_session
            .and_then(|session| session.installed_config_private_target_eligible_candidates),
        eligible_setup_artifact_paths: proof_session
            .map(|session| session.eligible_setup_artifact_paths.clone())
            .unwrap_or_default(),
        eligible_private_client_target_paths: proof_session
            .map(|session| session.eligible_private_client_target_paths.clone())
            .unwrap_or_default(),
        private_client_target_candidate_paths: proof_session
            .map(|session| session.private_client_target_candidate_paths.clone())
            .unwrap_or_default(),
        proof_session_error: proof_session.and_then(|session| session.error.clone()),
        artifact_repair_summary,
        artifact_repair_plan,
        operator_next_action_id: None,
        operator_next_action_label: None,
        operator_next_step: None,
        operator_next_command: None,
        next_step,
        next_commands,
        safe_to_claim,
        blocked_claims,
    };

    attach_private_app_operator_guidance(&mut row);
    refresh_client_status_aliases(&mut row);

    row
}

fn refresh_client_status_aliases(row: &mut ClientStatusRow) {
    row.status = row.goal_status.clone();
    row.ready = client_row_ready(row);
    refresh_client_row_readiness_contract(row);
    if row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL {
        attach_explicit_cli_operator_guidance(row);
    }
    row.readiness_summary = client_row_readiness_summary(row);
}

fn refresh_client_row_readiness_contract(row: &mut ClientStatusRow) {
    row.mcp_context_ready = row.mcp_registration_ready;
    row.stored_local_capture_observed = row.observed_capture_dogfood_evidence.is_some();
    row.latest_real_cli_capture_observed =
        row.real_cli_dogfood_probe.as_ref().map(|probe| probe.observed_local_capture);
    row.release_ready = row.ready_for_private_client_claim;
    if row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL {
        row.ready_scope = "mcp_and_explicit_capture_configured";
        row.ready_meaning =
            "ready=true means MCP registration plus explicit CLI capture path are configured; it does not prove latest real CLI dogfood capture or private release readiness";
    } else if row.ready_for_private_client_claim {
        row.ready_scope = "private_app_release_proof_complete";
        row.ready_meaning =
            "ready=true means app-hook, in-client render, and review-action proof all passed for private-client release";
    } else {
        row.ready_scope = "private_app_release_proof_incomplete";
        row.ready_meaning =
            "ready=false means private app-hook, in-client render, and review-action proof are not all complete";
    }
}

fn client_row_ready(row: &ClientStatusRow) -> bool {
    if row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL {
        return row.mcp_registration_ready && row.goal_status == "explicit_cli_capture_available";
    }
    row.ready_for_private_client_claim
}

fn attach_explicit_cli_operator_guidance(row: &mut ClientStatusRow) {
    let command = real_cli_probe_command(row.client);
    push_next_command_once(&mut row.next_commands, command.clone());
    if let Some(probe) = row.real_cli_dogfood_probe.as_ref() {
        let (action_id, action_label) = if probe.observed_local_capture {
            ("inspect_real_cli_dogfood_capture_evidence", "Inspect real CLI dogfood evidence")
        } else if probe.raw_status == "auth_blocked" {
            ("configure_cli_auth_for_real_dogfood", "Configure CLI auth")
        } else if probe.raw_status == "mcp_write_approval_required" {
            ("approve_real_cli_mcp_capture_write", "Approve real CLI MCP capture write")
        } else if probe.raw_status == "host_permission_blocked" {
            ("rerun_real_cli_probe_from_normal_terminal", "Run probe from normal terminal")
        } else {
            ("rerun_real_cli_dogfood_probe", "Rerun real CLI dogfood probe")
        };
        let next = probe.next_action.as_deref().unwrap_or(
            "inspect the real CLI dogfood probe artifact and rerun after fixing client access",
        );
        row.operator_next_action_id = Some(action_id.to_string());
        row.operator_next_action_label = Some(action_label.to_string());
        row.operator_next_step = Some(format!(
            "Latest real CLI dogfood probe for {} is `{}`; {next}.",
            row.client, probe.raw_status
        ));
        row.operator_next_command = Some(command);
        return;
    }
    row.operator_next_action_id = Some("run_real_cli_dogfood_probe".to_string());
    row.operator_next_action_label = Some("Run real CLI dogfood probe".to_string());
    row.operator_next_step = Some(format!(
        "Run a real {} CLI dogfood probe to verify MCP/context/capture beyond generated config readiness.",
        row.display_name
    ));
    row.operator_next_command = Some(command);
}

fn real_cli_probe_command(client: &str) -> Vec<String> {
    vec!["tools/real-cli-dogfood-probe.sh".to_string(), "--client".to_string(), client.to_string()]
}

fn client_row_readiness_summary(row: &ClientStatusRow) -> String {
    if row.capture_model == EXPLICIT_CLI_CAPTURE_MODEL {
        let base = if row.ready {
            "MCP registration and explicit CLI capture path are configured"
        } else {
            "MCP registration or explicit CLI capture path is not ready"
        };
        if let Some(probe) = row.real_cli_dogfood_probe.as_ref() {
            return format!(
                "{base}; latest real CLI dogfood probe status={} observed_local_capture={}.",
                probe.raw_status, probe.observed_local_capture
            );
        }
        return format!("{base}; real CLI dogfood capture is not yet proven by a latest probe.");
    }
    if row.ready_for_private_client_claim {
        return "Private app hook, in-client render, and review-action proof are complete."
            .to_string();
    }
    if row.ready_for_client_operator_loop {
        return "Private client operator loop is available, but release-grade private capture proof is not complete.".to_string();
    }
    format!(
        "Private app proof is not complete; next status={} proof_stage={}.",
        row.private_capture_status,
        row.proof_stage.as_deref().unwrap_or("unknown")
    )
}

fn attach_private_app_operator_guidance(row: &mut ClientStatusRow) {
    if row.capture_model != PRIVATE_APP_CAPTURE_MODEL {
        return;
    }
    let operator_next_action_id = private_app_operator_next_action_id(row);
    let operator_next_command = primary_private_app_command(row);
    row.operator_next_action_label = Some(private_app_operator_next_action_label(row));
    row.operator_next_step = Some(private_app_operator_next_step(row));
    row.operator_next_command = Some(operator_next_command);
    row.operator_next_action_id = Some(operator_next_action_id);
}

fn artifact_repair_plan_for_client(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
) -> Option<ClientArtifactRepairPlan> {
    let failures = binding
        .filter(|status| status.readiness == "artifact_integrity_failed")
        .map(|status| status.artifact_failures.as_slice())
        .unwrap_or_default();
    if failures.is_empty() {
        return None;
    }
    let artifact_dir = binding
        .and_then(client_binding_artifact_dir)
        .unwrap_or_else(|| durable_client_artifact_dir(client));
    let artifact_dir_write_status = artifact_dir_write_status(&artifact_dir);
    let workspace_fallback_artifact_dir =
        workspace_fallback_artifact_dir_for_client(client, &artifact_dir);
    let use_workspace_fallback =
        !artifact_dir_write_status_allows_new_files(&artifact_dir_write_status);
    let workspace_fallback_artifact_paths = workspace_fallback_artifact_dir
        .as_deref()
        .filter(|_| use_workspace_fallback)
        .map(|dir| suggested_client_artifact_paths(dir, failures))
        .unwrap_or_default();
    let effective_artifact_dir = workspace_fallback_artifact_dir
        .as_deref()
        .filter(|_| use_workspace_fallback)
        .unwrap_or(&artifact_dir)
        .to_string();

    let failed_artifacts = failures
        .iter()
        .take(8)
        .map(|failure| ClientArtifactRepairFailure {
            proof_id: failure.proof_id,
            proof_level: failure.proof_level.as_str().to_string(),
            artifact_kind: failure.kind.clone(),
            path: failure.path.clone(),
            status: evidence_artifact_status_label(failure.status).to_string(),
            recovery_action: artifact_failure_recovery_action(client, failure),
        })
        .collect::<Vec<_>>();
    let mut diagnostic_commands = vec![
        vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--verify-evidence-artifacts".to_string(),
            "--client".to_string(),
            client.as_str().to_string(),
            "--json".to_string(),
        ],
        private_app_proof_session_brief_command_for_client_with_artifact_dir(
            client.as_str(),
            Some(&effective_artifact_dir),
        ),
    ];
    if failed_artifacts.iter().any(|failure| failure.artifact_kind == "render_evidence") {
        diagnostic_commands.push(vec![
            "soma".to_string(),
            "context".to_string(),
            "review-render".to_string(),
            "--client".to_string(),
            client.as_str().to_string(),
            "--format".to_string(),
            "json".to_string(),
            "--write-report".to_string(),
            format!("{effective_artifact_dir}/review-render.json"),
        ]);
    }
    let workspace_fallback_commands = workspace_fallback_artifact_paths
        .iter()
        .find(|suggestion| suggestion.artifact_kind == "review_render_report")
        .map(|suggestion| review_render_write_command(client.as_str(), suggestion.path.clone()))
        .into_iter()
        .collect::<Vec<_>>();
    Some(ClientArtifactRepairPlan {
        source: "soma_clients.artifact_repair_plan.v1",
        status: "artifact_integrity_failed",
        client: client.as_str(),
        failure_count: failures.len(),
        suggested_artifact_dir: artifact_dir.clone(),
        suggested_artifact_dir_write_status: artifact_dir_write_status.clone(),
        suggested_artifact_paths: suggested_client_artifact_paths(&artifact_dir, failures),
        workspace_fallback_artifact_dir,
        workspace_fallback_artifact_paths,
        workspace_fallback_commands,
        failed_artifacts,
        diagnostic_commands,
        recovery_steps: artifact_repair_steps_for_client(
            client,
            failures,
            &effective_artifact_dir,
            &artifact_dir_write_status,
        ),
        blocked_claims: vec![
            "Private-client readiness is not claimable while stored proof artifacts fail replay."
                .to_string(),
            "Do not reuse missing or changed /tmp evidence artifacts as release-grade proof."
                .to_string(),
            "Re-record affected proof levels only after fresh evidence is visible and operator confirmation is explicit."
                .to_string(),
        ],
        trust_boundary:
            "artifact_repair_plan_is_read_only: explains stale or missing proof artifacts and recovery order only; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private-client readiness",
    })
}

fn artifact_repair_summary_for_client(
    client: McpClientKind,
    plan: &ClientArtifactRepairPlan,
) -> ClientArtifactRepairSummary {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();
    let next_command = command_with_current_binary_when_path_soma_differs(
        artifact_repair_primary_command_for_plan(client.as_str(), plan)
            .unwrap_or_else(|| private_app_proof_session_brief_command_for_client(client.as_str())),
        &binary_identity,
    );
    let next_command_safety =
        primary_next_command_safety("artifact_repair_next_command", &next_command);
    let operator_checklist =
        artifact_repair_operator_checklist_for_client(client, plan, &next_command);
    let render_evidence_artifact_scan = scan_render_evidence_artifact_for_repair(client, plan);
    let render_proof_packet_scan = render_evidence_artifact_scan
        .as_ref()
        .map(|scan| scan_render_proof_packet_for_repair(plan, scan));
    ClientArtifactRepairSummary {
        source: "soma_clients.artifact_repair_summary.v1",
        status: plan.status,
        failure_count: plan.failure_count,
        next_command,
        next_command_safety,
        operator_checklist,
        required_observation_fields: artifact_repair_required_observation_fields(plan),
        proof_recording_preconditions: artifact_repair_proof_recording_preconditions(plan),
        render_evidence_artifact_scan,
        render_proof_packet_scan,
        proof_free_local_materialization_only: true,
        requires_real_private_client_evidence_before_recording: true,
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        forbidden_shortcuts: vec![
            "do_not_reuse_missing_or_changed_tmp_artifacts_as_release_grade_proof",
            "do_not_record_observed_in_client_render_from_review_render_output_only",
            "do_not_record_observed_review_action_without_storage_gated_non_cloud_verification",
        ],
        trust_boundary:
            "artifact_repair_summary_is_read_only: exposes the next proof-free repair command and trust boundary for stale evidence artifacts; it records no proof row, creates no verification event, promotes no cloud draft, and cannot replace real private-client UI evidence",
    }
}

fn scan_render_proof_packet_for_repair(
    plan: &ClientArtifactRepairPlan,
    evidence_scan: &ClientRenderEvidenceArtifactScan,
) -> ClientRenderProofPacketScan {
    let render_evidence_path =
        evidence_scan.path.clone().or_else(|| effective_artifact_path(plan, "render_evidence"));
    let artifact_dir =
        render_evidence_path.as_deref().and_then(artifact_parent_dir).or_else(|| {
            effective_artifact_path(plan, "review_render_report")
                .as_deref()
                .and_then(artifact_parent_dir)
        });
    let review_render_json_path = effective_artifact_path(plan, "review_render_report")
        .or_else(|| artifact_dir.as_ref().map(|dir| format!("{dir}/review-render.json")));
    let review_render_markdown_path =
        artifact_dir.as_ref().map(|dir| format!("{dir}/review-render.md"));
    let review_render_html_path =
        artifact_dir.as_ref().map(|dir| format!("{dir}/review-render.html"));
    let render_evidence_exists = render_evidence_path.as_deref().is_some_and(path_exists);
    let review_render_json_exists = review_render_json_path.as_deref().is_some_and(path_exists);
    let review_render_markdown_exists =
        review_render_markdown_path.as_deref().is_some_and(path_exists);
    let review_render_html_exists = review_render_html_path.as_deref().is_some_and(path_exists);
    let any_review_render_exists =
        review_render_json_exists || review_render_markdown_exists || review_render_html_exists;
    let status = if render_evidence_exists
        && any_review_render_exists
        && evidence_scan.status == "filled_observation_candidate"
    {
        "filled_observation_candidate"
    } else if render_evidence_exists
        && any_review_render_exists
        && evidence_scan.status == "template_placeholders_present"
    {
        "prepared_placeholders_pending"
    } else if render_evidence_exists {
        "render_evidence_only"
    } else if any_review_render_exists {
        "review_render_only"
    } else {
        "packet_missing"
    };
    let next_step = match status {
        "prepared_placeholders_pending" => {
            "render review-render.md/html in the real private client UI, then replace render-evidence placeholders from that visible UI before recording proof"
        }
        "filled_observation_candidate" => {
            "rerun proof-session so storage gates can validate the filled observation candidate before recording proof"
        }
        "render_evidence_only" => {
            "regenerate or locate the matching review-render report before using this render-evidence artifact"
        }
        "review_render_only" => {
            "materialize the render-evidence template, then fill it only after visible private-client UI render"
        }
        _ => "run the proof-session render packet preparation command before collecting visible UI evidence",
    };
    ClientRenderProofPacketScan {
        source: "soma_clients.render_proof_packet_scan.v1",
        status,
        artifact_dir,
        review_render_json_path,
        review_render_markdown_path,
        review_render_html_path,
        render_evidence_path,
        review_render_json_exists,
        review_render_markdown_exists,
        review_render_html_exists,
        render_evidence_exists,
        placeholder_count: evidence_scan.placeholder_count,
        next_step,
        proof_free_local_materialization_only: true,
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        trust_boundary:
            "render_proof_packet_scan_is_read_only: inspects local review-render and render-evidence packet files only; records no proof row, creates no verification event, promotes no cloud draft, and cannot replace visible private-client UI evidence plus explicit operator confirmation",
    }
}

fn artifact_parent_dir(path: &str) -> Option<String> {
    Path::new(path).parent().map(|path| path.to_string_lossy().into_owned())
}

fn path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn render_proof_packet_scan_for_row(row: &ClientStatusRow) -> Option<ClientRenderProofPacketScan> {
    if let Some(scan) = row
        .artifact_repair_summary
        .as_ref()
        .and_then(|summary| summary.render_proof_packet_scan.clone())
    {
        return Some(scan);
    }
    let plan = row.artifact_repair_plan.as_ref()?;
    let evidence_scan =
        scan_render_evidence_artifact(row.client, effective_artifact_path(plan, "render_evidence"));
    Some(scan_render_proof_packet_for_repair(plan, &evidence_scan))
}

fn render_packet_preferred_view_path(packet: &ClientRenderProofPacketScan) -> &str {
    packet
        .review_render_markdown_path
        .as_deref()
        .or(packet.review_render_html_path.as_deref())
        .or(packet.review_render_json_path.as_deref())
        .unwrap_or("review-render.md")
}

fn scan_render_evidence_artifact_for_repair(
    client: McpClientKind,
    plan: &ClientArtifactRepairPlan,
) -> Option<ClientRenderEvidenceArtifactScan> {
    if !plan.failed_artifacts.iter().any(|failure| failure.artifact_kind == "render_evidence") {
        return None;
    }
    let path = effective_artifact_path(plan, "render_evidence");
    Some(scan_render_evidence_artifact(client.as_str(), path))
}

fn scan_render_evidence_artifact(
    client: &str,
    path: Option<String>,
) -> ClientRenderEvidenceArtifactScan {
    let Some(path) = path else {
        return render_evidence_scan_with_missing(
            None,
            "missing_path",
            0,
            vec!["render_evidence_path_required"],
        );
    };
    let artifact_path = Path::new(&path);
    if !artifact_path.exists() {
        return render_evidence_scan_with_missing(
            Some(path),
            "missing_file",
            0,
            vec!["render_evidence_file_required"],
        );
    }
    let raw = match fs::read_to_string(artifact_path) {
        Ok(raw) => raw,
        Err(_) => {
            return render_evidence_scan_with_missing(
                Some(path),
                "unreadable",
                0,
                vec!["render_evidence_file_readable"],
            );
        }
    };
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value,
        Err(_) => {
            return render_evidence_scan_with_missing(
                Some(path),
                "invalid_json",
                0,
                vec!["render_evidence_must_be_json"],
            );
        }
    };
    let placeholder_count = value_template_placeholder_count(&value);
    let mut missing = render_evidence_artifact_missing_requirements(client, &value);
    if placeholder_count > 0 && !missing.iter().any(|item| item == "template_placeholders_absent") {
        missing.push("template_placeholders_absent".to_string());
    }
    let status = if placeholder_count > 0 {
        "template_placeholders_present"
    } else if missing.is_empty() {
        "filled_observation_candidate"
    } else {
        "observation_incomplete"
    };
    render_evidence_scan_with_missing(Some(path), status, placeholder_count, missing)
}

fn render_evidence_scan_with_missing(
    path: Option<String>,
    status: &'static str,
    placeholder_count: usize,
    missing_requirements: Vec<impl Into<String>>,
) -> ClientRenderEvidenceArtifactScan {
    ClientRenderEvidenceArtifactScan {
        source: "soma_clients.render_evidence_artifact_scan.v1",
        path,
        status,
        placeholder_count,
        missing_requirements: missing_requirements.into_iter().map(Into::into).collect(),
        proof_free_local_materialization_only: true,
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        trust_boundary:
            "render_evidence_artifact_scan_is_read_only: inspects proof-free in-client render evidence artifacts for placeholders and missing local observation fields only; records no proof row, creates no verification event, promotes no cloud draft, and cannot replace explicit operator-confirmed private-client UI evidence",
    }
}

fn render_evidence_artifact_missing_requirements(client: &str, value: &Value) -> Vec<String> {
    let mut missing = Vec::new();
    if value.get("schema").and_then(Value::as_str) != Some("soma.in_client_render_evidence.v1") {
        missing.push("schema_must_be_soma_in_client_render_evidence_v1".to_string());
    }
    if value
        .get("client")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| *value == client)
        .is_none()
    {
        missing.push("client_must_match_binding_target".to_string());
    }
    if !value.get("source").and_then(Value::as_str).is_some_and(is_allowed_render_evidence_source) {
        missing.push("source_must_be_manual_operator_or_client_capture".to_string());
    }
    if !positive_observed_at_ns(value.get("observed_at_ns")) {
        missing.push("observed_at_ns_must_be_positive".to_string());
    }
    if !concrete_string_field(value, "review_render_fingerprint") {
        missing.push("review_render_fingerprint_must_be_concrete".to_string());
    }
    let surfaces = value.get("rendered_surfaces").and_then(Value::as_array);
    if surfaces.is_none_or(Vec::is_empty) {
        missing.push("rendered_surfaces_must_be_non_empty".to_string());
        return missing;
    }
    let surfaces = surfaces.expect("checked above");
    if surfaces.iter().any(value_contains_template_placeholder) {
        missing.push("rendered_surfaces_must_not_contain_template_placeholders".to_string());
    }
    if surfaces.iter().any(render_evidence_surface_is_raw_tool_output) {
        missing.push("rendered_surfaces_must_not_be_raw_mcp_or_tool_output".to_string());
    }
    if !surfaces.iter().any(surface_is_visible) {
        missing.push("rendered_surfaces_must_include_visible_surface".to_string());
    }
    if !surfaces.iter().any(|surface| concrete_string_field(surface, "kind")) {
        missing.push("rendered_surfaces_must_include_concrete_kind".to_string());
    }
    if !surfaces.iter().any(|surface| concrete_string_field(surface, "title")) {
        missing.push("rendered_surfaces_must_include_visible_title".to_string());
    }
    missing
}

fn positive_observed_at_ns(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => {
            number.as_i64().is_some_and(|value| value > 0)
                || number.as_u64().is_some_and(|value| value > 0)
        }
        Some(Value::String(text)) => text.trim().parse::<u64>().is_ok_and(|value| value > 0),
        _ => false,
    }
}

fn concrete_string_field(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|text| !text.is_empty() && !is_template_placeholder(text))
}

fn surface_is_visible(surface: &Value) -> bool {
    surface.get("visible").and_then(Value::as_bool) == Some(true)
}

fn render_evidence_surface_is_raw_tool_output(surface: &Value) -> bool {
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

fn value_template_placeholder_count(value: &Value) -> usize {
    match value {
        Value::String(text) => usize::from(is_template_placeholder(text)),
        Value::Array(values) => values.iter().map(value_template_placeholder_count).sum(),
        Value::Object(map) => map.values().map(value_template_placeholder_count).sum(),
        _ => 0,
    }
}

fn value_contains_template_placeholder(value: &Value) -> bool {
    value_template_placeholder_count(value) > 0
}

fn is_template_placeholder(text: &str) -> bool {
    let text = text.trim();
    text.len() > 2 && text.starts_with('<') && text.ends_with('>')
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

fn artifact_repair_operator_checklist_for_client(
    client: McpClientKind,
    plan: &ClientArtifactRepairPlan,
    next_command: &[String],
) -> Vec<String> {
    let display = client.display_name();
    let review_render = effective_artifact_path(plan, "review_render_report");
    let render_evidence = effective_artifact_path(plan, "render_evidence");
    let review_action = effective_artifact_path(plan, "review_action_report");
    if next_command.iter().any(|part| part == "review-render") {
        return vec![
            format!(
                "Generate a fresh read-only review-render report for {display}; this is not proof by itself."
            ),
            format!(
                "Render that report in the real {display} UI before filling any in-client render evidence."
            ),
            "Keep cloud output, review-render output, and client binding status out of claim verification evidence.".to_string(),
        ];
    }
    if command_renders_evidence(next_command) {
        return vec![
            format!(
                "Materialize a proof-free render-evidence packet for {display}; placeholders must remain untrusted until the real UI is visible."
            ),
            format!(
                "Replace `source`, `observed_at_ns`, and `rendered_surfaces` only from the visible {display} UI."
            ),
            "Rerun the proof-session before recording so storage gates re-check the filled packet.".to_string(),
        ];
    }
    let mut checklist = Vec::new();
    if let Some(path) = render_evidence {
        checklist.push(format!(
            "Inspect `{path}` and replace every visible-render placeholder from the real {display} UI."
        ));
    } else {
        checklist.push(format!(
            "Create a fresh structured render-evidence packet from the real {display} UI before recording observed_in_client_render."
        ));
    }
    if let Some(path) = review_render {
        checklist.push(format!(
            "Keep the filled render evidence bound to the review-render fingerprint in `{path}`."
        ));
    }
    if let Some(path) = review_action {
        checklist.push(format!(
            "After render proof is valid, save review-action evidence to `{path}` only from a rendered control with non-cloud verification."
        ));
    }
    checklist.push(
        "Record replacement proof only through the proof-session command with explicit operator confirmation and release-grade evidence confirmation."
            .to_string(),
    );
    checklist
}

fn artifact_repair_required_observation_fields(
    plan: &ClientArtifactRepairPlan,
) -> Vec<&'static str> {
    if !plan.failed_artifacts.iter().any(|failure| failure.artifact_kind == "render_evidence") {
        return Vec::new();
    }
    vec![
        "source=manual_operator_or_client_capture",
        "observed_at_ns=positive_unix_epoch_nanoseconds_after_visible_render",
        "rendered_surfaces[].visible=true_from_real_client_ui",
        "rendered_surfaces[].kind=concrete_client_surface_kind",
        "rendered_surfaces[].title=visible_surface_title",
        "rendered_control_ids=current_review_action_control_ids",
    ]
}

fn artifact_repair_proof_recording_preconditions(
    plan: &ClientArtifactRepairPlan,
) -> Vec<&'static str> {
    if plan.failed_artifacts.is_empty() {
        return Vec::new();
    }
    vec![
        "fresh_artifact_paths_exist_and_replay",
        "review_render_fingerprint_matches_render_evidence",
        "review_workbench_and_interaction_contract_versions_match",
        "operator_confirm_in_client_render_or_review_action_is_explicit",
        "operator_confirm_release_grade_evidence_is_explicit",
        "cloud_draft_or_review_render_output_is_not_used_as_verification_evidence",
    ]
}

fn artifact_repair_steps_for_client(
    client: McpClientKind,
    failures: &[ClientBindingArtifactFailure],
    effective_artifact_dir: &str,
    artifact_dir_write_status: &str,
) -> Vec<String> {
    let proof_session_command = command_text_with_current_binary_when_path_soma_differs(
        private_app_proof_session_brief_command_for_client_with_artifact_dir(
            client.as_str(),
            Some(effective_artifact_dir),
        ),
    );
    let verify_evidence_command = adapter_binding_proof_client_command_text(
        client.as_str(),
        &["--verify-evidence-artifacts", "--json"],
    );
    let mut steps = vec![
        format!(
            "Run `{verify_evidence_command}` to inspect the stale proof rows."
        ),
        format!(
            "Run `{proof_session_command}` to follow the current release gate before recording new proof."
        ),
    ];
    if !artifact_dir_write_status_allows_new_files(artifact_dir_write_status) {
        steps.push(format!(
            "The suggested durable artifact directory currently reports `{artifact_dir_write_status}`; use the workspace fallback artifact paths from this report, or fix filesystem access before running the home-directory write command."
        ));
    }
    if failures.iter().any(|failure| failure.kind == "render_evidence") {
        let review_render_command =
            command_text_with_current_binary_when_path_soma_differs(review_render_write_command(
                client.as_str(),
                format!("{effective_artifact_dir}/review-render.json"),
            ));
        steps.push(format!(
            "Regenerate a fresh review-render report with `{review_render_command}`, render it in the real private client, then create a new structured render evidence artifact from that visible UI."
        ));
        steps.push(
            "Re-record `observed_in_client_render` only after the fresh render evidence is bound to the review-render fingerprint and the operator confirms real in-client visibility."
                .to_string(),
        );
        steps.push(format!(
            "Then rerun `{proof_session_command}`; its blocked `record_observed_in_client_render` row prints the exact proof command using the active review-render/render-evidence paths."
        ));
    }
    if failures.iter().any(|failure| failure.kind == "review_action_report") {
        steps.push(
            "Execute one rendered review control in the real private client and save a fresh storage-gated review-action report backed by user/tool/local verification."
                .to_string(),
        );
        steps.push(
            "Re-record `observed_review_action` only after that fresh report is present and the operator confirms release-grade evidence."
                .to_string(),
        );
        steps.push(format!(
            "Then rerun `{proof_session_command}`; its blocked `record_observed_review_action` row prints the exact proof command using the active review-action report path."
        ));
    }
    steps
}

fn suggested_client_artifact_paths(
    dir: &str,
    failures: &[ClientBindingArtifactFailure],
) -> Vec<ClientArtifactPathSuggestion> {
    let mut suggestions = vec![ClientArtifactPathSuggestion {
        artifact_kind: "review_render_report".to_string(),
        path: format!("{dir}/review-render.json"),
        intent: "Read-only `soma context review-render` report that render evidence must bind to."
            .to_string(),
    }];
    if failures.iter().any(|failure| failure.kind == "render_evidence") {
        suggestions.push(ClientArtifactPathSuggestion {
            artifact_kind: "render_evidence".to_string(),
            path: format!("{dir}/render-evidence.json"),
            intent: "Filled soma.in_client_render_evidence.v1 captured after visible private-client UI rendering."
                .to_string(),
        });
    }
    if failures.iter().any(|failure| failure.kind == "review_action_report") {
        suggestions.push(ClientArtifactPathSuggestion {
            artifact_kind: "review_action_report".to_string(),
            path: format!("{dir}/review-action.json"),
            intent: "Storage-gated `soma context review-action` report from one rendered control."
                .to_string(),
        });
    }
    suggestions
}

fn artifact_dir_write_status(dir: &str) -> String {
    if dir.contains("$HOME") || dir.contains("<run-id>") {
        return "not_checked_template".to_string();
    }
    let path = Path::new(dir);
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            let writable = path_is_writable_without_mutation(candidate);
            let exact = candidate == path;
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
    std::fs::metadata(path).map(|metadata| !metadata.permissions().readonly()).unwrap_or(false)
}

fn workspace_fallback_artifact_dir_for_client(
    client: McpClientKind,
    artifact_dir: &str,
) -> Option<String> {
    let run_id = Path::new(artifact_dir)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "<run-id>".to_string());
    let cwd = env::current_dir().ok()?;
    Some(
        cwd.join(".soma")
            .join("client-evidence")
            .join(client.as_str())
            .join(run_id)
            .to_string_lossy()
            .into_owned(),
    )
}

fn durable_client_artifact_dir(client: McpClientKind) -> String {
    format!("$HOME/.soma/client-evidence/{}/<run-id>", client.as_str())
}

fn durable_client_artifact_dir_for_run(client: McpClientKind, run_id: &str) -> String {
    let run_id = sanitize_client_artifact_run_id(run_id);
    if let Some(home) = dirs::home_dir() {
        return home
            .join(".soma")
            .join("client-evidence")
            .join(client.as_str())
            .join(run_id)
            .to_string_lossy()
            .into_owned();
    }
    format!("$HOME/.soma/client-evidence/{}/{run_id}", client.as_str())
}

fn sanitize_client_artifact_run_id(run_id: &str) -> String {
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

fn client_binding_artifact_dir(binding: &ClientBindingReadinessStatus) -> Option<String> {
    let nonce = binding
        .latest_by_level
        .values()
        .find_map(|status| status.installed_config_binding_nonce.as_deref())?;
    let client = McpClientKind::parse_slug(&binding.client)?;
    Some(durable_client_artifact_dir_for_run(client, nonce))
}

fn artifact_failure_recovery_action(
    client: McpClientKind,
    failure: &ClientBindingArtifactFailure,
) -> String {
    match (failure.proof_level, failure.kind.as_str()) {
        (ClientBindingProofLevel::ObservedInClientRender, "render_evidence") => format!(
            "Regenerate review-render for {}, capture fresh structured render evidence from the real client UI, then re-record observed_in_client_render.",
            client.display_name()
        ),
        (ClientBindingProofLevel::ObservedReviewAction, "review_action_report") => format!(
            "Execute a rendered {} review control, save a fresh storage-gated review-action report, then re-record observed_review_action.",
            client.display_name()
        ),
        (_, "manifest") | (_, "installed_config") => {
            "Recreate or reselect the matching manifest/installed config, then rerun proof-session before recording replacement proof.".to_string()
        }
        _ => "Regenerate the missing or changed artifact and rerun proof-session before recording replacement proof.".to_string(),
    }
}

fn evidence_artifact_status_label(status: EvidenceArtifactStatus) -> &'static str {
    match status {
        EvidenceArtifactStatus::Verified => "verified",
        EvidenceArtifactStatus::VerifiedAppendOnlyGrowth => "verified_append_only_growth",
        EvidenceArtifactStatus::MissingExpectedRecord => "missing_expected_record",
        EvidenceArtifactStatus::MissingPath => "missing_path",
        EvidenceArtifactStatus::MissingFile => "missing_file",
        EvidenceArtifactStatus::Changed => "changed",
        EvidenceArtifactStatus::Unreadable => "unreadable",
    }
}

fn capture_model(client: McpClientKind) -> &'static str {
    match client {
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => EXPLICIT_CLI_CAPTURE_MODEL,
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            PRIVATE_APP_CAPTURE_MODEL
        }
    }
}

fn goal_status(
    client: McpClientKind,
    mcp: &mcp_config::McpConfigCheckReport,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    proof_storage_status: &'static str,
) -> String {
    if !mcp.readiness.mcp_registration_ready {
        return "mcp_registration_blocked".to_string();
    }
    match client {
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => {
            if mcp.readiness.client_runtime.required_for_mcp_registration
                && mcp.readiness.client_runtime.status == "missing"
            {
                "explicit_cli_runtime_missing".to_string()
            } else {
                "explicit_cli_capture_available".to_string()
            }
        }
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            if proof_storage_status != "available" {
                "private_app_proof_storage_unavailable".to_string()
            } else if binding.is_some_and(|status| status.readiness == "artifact_integrity_failed")
            {
                "private_app_proof_artifact_integrity_failed".to_string()
            } else if private_target_config_missing_for_release(
                client,
                binding,
                proof_session,
                proof_storage_status,
            ) {
                "private_app_target_config_required".to_string()
            } else if private_client_claim_ready(client, binding, proof_session, true) {
                "private_app_release_grade_proof_ready".to_string()
            } else if proof_session.is_some_and(|session| session.error.is_some()) {
                "private_app_proof_session_unavailable".to_string()
            } else if proof_session.and_then(|session| session.next_step_id.as_deref()).is_some_and(
                |step| {
                    matches!(
                        step,
                        "trigger_private_client_hook"
                            | "start_continue_devdata_collector_before_real_hook"
                    )
                },
            ) {
                "private_app_trigger_hook_required".to_string()
            } else if proof_session.and_then(|session| session.next_step_id.as_deref())
                == Some("render_or_write_installed_config")
            {
                "private_app_installed_config_required".to_string()
            } else {
                "private_app_proof_session_required".to_string()
            }
        }
    }
}

fn private_capture_status(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    proof_storage_status: &'static str,
) -> String {
    if proof_storage_status != "available"
        && matches!(
            client,
            McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue
        )
    {
        return "client_binding_proof_storage_unavailable".to_string();
    }
    if private_target_config_missing_for_release(
        client,
        binding,
        proof_session,
        proof_storage_status,
    ) {
        return "stored_release_proof_but_private_target_config_missing".to_string();
    }
    if let Some(status) = binding {
        return status.readiness.clone();
    }
    match client {
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            "missing_client_binding_proof".to_string()
        }
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => {
            "explicit_capture_available_private_automatic_unproven".to_string()
        }
    }
}

fn private_client_claim_ready(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    proof_storage_available: bool,
) -> bool {
    if !proof_storage_available {
        return false;
    }
    if !is_private_app_client(client) {
        return binding.is_some_and(|status| status.ready_for_private_client_claim);
    }
    binding.is_some_and(|status| status.ready_for_private_client_claim)
        && proof_session.is_some_and(|session| {
            session.status.as_deref() == Some("ready_for_private_client_claim")
                && session.release_gate.as_deref() == Some("pass")
                && session.installed_config_private_target_eligible_candidates.unwrap_or_default()
                    > 0
        })
}

fn private_target_config_missing_for_release(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    proof_storage_status: &'static str,
) -> bool {
    proof_storage_status == "available"
        && is_private_app_client(client)
        && binding.is_some_and(|status| status.ready_for_private_client_claim)
        && proof_session.is_some_and(|session| {
            session.installed_config_private_target_eligible_candidates.unwrap_or_default() == 0
        })
}

fn missing_proof_levels(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
) -> Vec<&'static str> {
    if !matches!(client, McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue)
    {
        return Vec::new();
    }
    let mut missing = Vec::new();
    if !binding.is_some_and(|status| status.has_observed_app_hook) {
        missing.push(ClientBindingProofLevel::ObservedAppHook.as_str());
    }
    if !binding.is_some_and(|status| status.has_observed_in_client_render) {
        missing.push(ClientBindingProofLevel::ObservedInClientRender.as_str());
    }
    if !binding.is_some_and(|status| status.has_observed_review_action) {
        missing.push(ClientBindingProofLevel::ObservedReviewAction.as_str());
    }
    missing
}

fn proof_level_statuses(
    client: McpClientKind,
    binding: Option<&ClientBindingReadinessStatus>,
) -> Vec<ClientProofLevelStatus> {
    if !matches!(client, McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue)
    {
        return Vec::new();
    }
    [
        (
            ClientBindingProofLevel::ObservedAppHook.as_str(),
            "app_hook",
            binding.is_some_and(|status| status.has_observed_app_hook),
        ),
        (
            ClientBindingProofLevel::ObservedInClientRender.as_str(),
            "review_render",
            binding.is_some_and(|status| status.has_observed_in_client_render),
        ),
        (
            ClientBindingProofLevel::ObservedReviewAction.as_str(),
            "review_action",
            binding.is_some_and(|status| status.has_observed_review_action),
        ),
    ]
    .into_iter()
    .map(|(proof_level, review_stage, has_proof)| {
        let status = match (has_proof, binding) {
            (true, Some(binding)) => proof_level_artifact_status(binding, proof_level),
            _ => "missing",
        };
        ClientProofLevelStatus {
            proof_level,
            review_stage,
            status,
            required_for_private_client_claim: true,
            blocks_private_client_claim: status != "recorded",
        }
    })
    .collect()
}

fn client_status_proof_stage(binding: &ClientBindingReadinessStatus) -> String {
    match binding.readiness.as_str() {
        "artifact_integrity_failed" => "stored_proof_artifacts_invalid".to_string(),
        "proof_identity_mismatch" => "stored_proof_identity_mismatch".to_string(),
        "non_release_evidence_source" => "stored_proof_non_release_evidence".to_string(),
        _ => binding.proof_stage.clone(),
    }
}

fn proof_level_artifact_status(
    binding: &ClientBindingReadinessStatus,
    proof_level: &str,
) -> &'static str {
    if let Some(latest) = binding.latest_by_level.get(proof_level) {
        if latest.all_artifacts_verified {
            "recorded"
        } else {
            "artifact_invalid"
        }
    } else if binding.all_latest_artifacts_verified {
        "recorded"
    } else {
        "artifact_invalid"
    }
}

fn next_step(
    client: McpClientKind,
    mcp: &mcp_config::McpConfigCheckReport,
    binding: Option<&ClientBindingReadinessStatus>,
    proof_session: Option<&ClientProofSessionSummary>,
    artifact_repair_plan: Option<&ClientArtifactRepairPlan>,
    proof_storage_status: &'static str,
    continue_extension_config_check: Option<&ClientContinueExtensionConfigCheck>,
) -> String {
    let private_app_client = is_private_app_client(client);
    if let Some(status) = binding {
        if status.readiness == "artifact_integrity_failed" {
            if proof_session.and_then(|session| session.next_step_id.as_deref())
                == Some("capture_in_client_render_evidence")
                && artifact_repair_plan
                    .and_then(|plan| effective_artifact_path(plan, "render_evidence"))
                    .is_some_and(|path| Path::new(&path).exists())
            {
                let command = command_text_with_current_binary_when_path_soma_differs(
                    proof_session
                        .and_then(|session| {
                            proof_session_runbook_step_command(
                                session,
                                "capture_in_client_render_evidence",
                            )
                        })
                        .or_else(|| proof_session.and_then(|session| session.next_command.clone()))
                        .unwrap_or_else(|| {
                            vec![
                                "soma".to_string(),
                                "adapter-binding-proof".to_string(),
                                "--client".to_string(),
                                client.as_str().to_string(),
                                "--proof-session".to_string(),
                                "--json".to_string(),
                            ]
                        }),
                );
                let rerun_proof_session =
                    command_text_with_current_binary_when_path_soma_differs(vec![
                        "soma".to_string(),
                        "adapter-binding-proof".to_string(),
                        "--client".to_string(),
                        client.as_str().to_string(),
                        "--proof-session".to_string(),
                        "--json".to_string(),
                    ]);
                let status_diagnostic =
                    command_text_with_current_binary_when_path_soma_differs(vec![
                        "soma".to_string(),
                        "adapter-binding-proof".to_string(),
                        "--client".to_string(),
                        client.as_str().to_string(),
                        "--status".to_string(),
                        "--json".to_string(),
                    ]);
                return format!(
                    "The proof-session is waiting at `capture_in_client_render_evidence`; run `{command}` to prepare or reuse proof-free review-render and render-evidence artifacts, render the generated Markdown/HTML in the real {} UI, fill the render evidence only from that visible UI, then rerun `{rerun_proof_session}` to expose the guarded observed_in_client_render recording command. Keep `{status_diagnostic}` as the stale-artifact replay diagnostic before claiming readiness.",
                    client.display_name(),
                );
            }
            if let Some(command) = artifact_repair_plan
                .and_then(|plan| artifact_repair_primary_command_for_plan(client.as_str(), plan))
            {
                let renders_evidence = command_renders_evidence(&command);
                let command = command_text_with_current_binary_when_path_soma_differs(command);
                let status_diagnostic =
                    command_text_with_current_binary_when_path_soma_differs(vec![
                        "soma".to_string(),
                        "adapter-binding-proof".to_string(),
                        "--client".to_string(),
                        client.as_str().to_string(),
                        "--status".to_string(),
                        "--json".to_string(),
                    ]);
                if renders_evidence {
                    return format!(
                        "A review-render report already exists, but visible UI evidence is not proven yet; run `{}` to materialize the proof-free render evidence packet template, then fill the visible-render fields from the real {} UI before recording observed_in_client_render. Keep `{status_diagnostic}` as the replay diagnostic before claiming readiness.",
                        command,
                        client.display_name(),
                    );
                }
                return format!(
                    "A review-render report and proof-free render-evidence packet template already exist, but visible UI evidence is not proven yet; inspect `{}` and replace the visible-render placeholders from the real {} UI before recording observed_in_client_render. Keep `{status_diagnostic}` as the replay diagnostic before claiming readiness.",
                    command,
                    client.display_name(),
                );
            }
            if let Some(command) = proof_session.and_then(|session| session.next_command.as_ref()) {
                let command =
                    command_text_with_current_binary_when_path_soma_differs(command.clone());
                let status_diagnostic =
                    command_text_with_current_binary_when_path_soma_differs(vec![
                        "soma".to_string(),
                        "adapter-binding-proof".to_string(),
                        "--client".to_string(),
                        client.as_str().to_string(),
                        "--status".to_string(),
                        "--json".to_string(),
                    ]);
                return format!(
                    "Refresh stale proof artifacts by running `{command}` first; keep `{status_diagnostic}` as the replay diagnostic before claiming readiness.",
                );
            }
            let status_diagnostic = command_text_with_current_binary_when_path_soma_differs(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--status".to_string(),
                "--json".to_string(),
            ]);
            let proof_session = command_text_with_current_binary_when_path_soma_differs(vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--proof-session".to_string(),
                "--json".to_string(),
            ]);
            return format!(
                "Run `{status_diagnostic}` to inspect changed proof artifacts, then rerun `{proof_session}` for the next safe proof step.",
            );
        }
    }
    if proof_storage_status != "available" && private_app_client {
        return format!(
            "Grant SOMA read access to the configured DB for real proof rows, or rerun `{}` for an immediate read-only MCP/runtime diagnostic before claiming private app-hook readiness.",
            command_text_with_current_binary_when_path_soma_differs(proof_storage_diagnostic_command())
        );
    }
    if private_target_config_missing_for_release(
        client,
        binding,
        proof_session,
        proof_storage_status,
    ) {
        let proof_session_command = adapter_binding_proof_client_command_text(
            client.as_str(),
            &["--proof-session", "--json"],
        );
        return format!(
            "Stored app-hook/render/review-action proof rows exist for {}, but no known private-client target config is currently discoverable; install or wire the target config, then rerun `{proof_session_command}` before claiming readiness.",
            client.display_name(),
        );
    }
    if private_client_claim_ready(
        client,
        binding,
        proof_session,
        proof_storage_status == "available",
    ) {
        return format!(
            "Release-grade private-client proof is ready for {}; runtime CLI detection is reported separately.",
            client.display_name()
        );
    }
    if private_app_client {
        if let Some(error) = proof_session.and_then(|session| session.error.as_deref()) {
            let proof_session_command = adapter_binding_proof_client_command_text(
                client.as_str(),
                &["--proof-session", "--json"],
            );
            return format!(
                "Rerun `{proof_session_command}` to inspect proof-session setup; last read-only probe failed: {error}.",
            );
        }
        match proof_session.and_then(|session| session.next_step_id.as_deref()) {
            Some("render_or_write_installed_config") => {
                let proof_session_command = adapter_binding_proof_client_command_text(
                    client.as_str(),
                    &["--proof-session", "--json"],
                );
                let clients_command = soma_clients_command_text(&[]);
                return format!(
                    "Render and install a proof-free {} binding config with `{proof_session_command}`, then rerun `{clients_command}`; runtime CLI detection is reported separately if missing.",
                    client.display_name(),
                );
            }
            Some("trigger_private_client_hook") => {
                let setup_artifacts = proof_session
                    .and_then(|session| session.installed_config_setup_artifact_eligible_candidates)
                    .unwrap_or_default();
                let target_configs = proof_session
                    .and_then(|session| session.installed_config_private_target_eligible_candidates)
                    .unwrap_or_default();
                if setup_artifacts > 0 && target_configs == 0 {
                    return format!(
                        "A proof-free {} setup artifact is eligible, but no known private-client target config was discovered; install or wire that config into the {} target path, then trigger the real private client hook before any observed_app_hook proof is recorded.",
                        client.display_name(),
                        client.display_name()
                    );
                }
                if client == McpClientKind::Continue {
                    if let Some(check) = continue_extension_config_check
                        .filter(|check| continue_profile_config_invalid(check))
                    {
                        if check.status == "config_present_soma_mcp_profile_invalid" {
                            return "Installed Continue binding config and SOMA MCP config are visible, but Continue rejects config.yaml/config.yml because required top-level name/version fields are missing or unreadable; run the Continue dev-data installer dry-run, write the repair if correct, reload Continue, then complete a real turn before any observed_app_hook proof is recorded."
                                .to_string();
                        }
                        return "Installed Continue binding config is eligible, but Continue rejects config.yaml/config.yml because required top-level name/version fields are missing or unreadable; repair the profile config, write the SOMA MCP config if it is still missing, reload Continue, then complete a real turn before any observed_app_hook proof is recorded."
                            .to_string();
                    }
                }
                if client == McpClientKind::Continue
                    && continue_extension_config_check
                        .is_some_and(continue_extension_config_not_visible)
                {
                    return "Installed Continue binding config is eligible, but the Continue extension config is not visibly wired to SOMA; write the generated MCP server JSON to the Continue mcpServers directory, reload Continue, then complete a real Continue extension turn (not Cursor Agent/Composer) before any observed_app_hook proof is recorded."
                        .to_string();
                }
                if client == McpClientKind::Continue
                    && continue_extension_config_check
                        .is_some_and(|check| !check.extension_observed)
                {
                    return "Installed Continue binding config is eligible and SOMA MCP config is visible, but no local Continue extension installation was observed; install or enable Continue in VS Code/Cursor, reload the editor, then complete a real Continue extension turn (not Cursor Agent/Composer) before any observed_app_hook proof is recorded."
                        .to_string();
                }
                if proof_session.is_some_and(private_app_hook_temporal_binding_failed_summary) {
                    return format!(
                        "Installed {} binding config is eligible, but the latest matching private event is older than the current installed config; trigger a fresh real private client hook from {} so the matching event observed_at_ns is after the config modified_at before any observed_app_hook proof is recorded.",
                        client.display_name(),
                        client.display_name()
                    );
                }
                if client == McpClientKind::CodexApp {
                    return format!(
                        "Installed {} binding config is eligible; if Codex app was already running when the notify config was patched, quit or restart the stale Codex app process, reopen it, then complete a real turn so SOMA can observe the app event before any observed_app_hook proof is recorded.",
                        client.display_name()
                    );
                }
                if client == McpClientKind::Continue {
                    if continue_extension_config_check
                        .is_some_and(continue_devdata_collector_probe_blocked)
                    {
                        return "Installed Continue binding config is eligible and Continue MCP/extension/dev-data config are visible, but SOMA could not prove the local dev-data collector status from this execution context; run the managed status command from the operator shell, avoid starting duplicate collectors, then complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer) only after the collector is proven listening."
                            .to_string();
                    }
                    if continue_extension_config_check
                        .is_some_and(continue_devdata_collector_start_needed)
                    {
                        return "Installed Continue binding config is eligible and Continue MCP/extension/dev-data config are visible, but the local dev-data collector is not listening; start the collector, reload Continue, then complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer) so SOMA can observe the app event before any observed_app_hook proof is recorded."
                            .to_string();
                    }
                    return "Installed Continue binding config is eligible and Continue MCP/extension are visible; reload Continue or its host editor if needed, then complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer) so SOMA can observe the app event before any observed_app_hook proof is recorded."
                        .to_string();
                }
                return format!(
                    "Installed {} binding config is eligible; trigger the real private client hook from {} so SOMA can observe the app event before any observed_app_hook proof is recorded.",
                    client.display_name(),
                    client.display_name()
                );
            }
            Some("start_continue_devdata_collector_before_real_hook") => {
                return "Installed Continue binding config is eligible and Continue MCP/extension/dev-data config are visible, but the local dev-data collector is not listening; start the collector, reload Continue, then complete a real Continue extension chat/edit/review action (not Cursor Agent/Composer) so SOMA can observe the app event before any observed_app_hook proof is recorded."
                    .to_string();
            }
            Some("record_observed_app_hook") => {
                let proof_session_command = adapter_binding_proof_client_command_text(
                    client.as_str(),
                    &["--proof-session", "--json"],
                );
                return format!(
                    "Proof artifacts are ready; record observed_app_hook with `{proof_session_command}` guidance, then continue to in-client render and review-action proof.",
                );
            }
            Some(next_step_id) => {
                let title = proof_session
                    .and_then(|session| session.next_operator_step_title.as_deref())
                    .unwrap_or(next_step_id);
                let proof_session_command = adapter_binding_proof_client_command_text(
                    client.as_str(),
                    &["--proof-session", "--json"],
                );
                return format!(
                    "Continue the private proof-session at `{next_step_id}` ({title}) with `{proof_session_command}`; runtime CLI detection is reported separately if missing.",
                );
            }
            None => {}
        }
        if let Some(status) = binding.and_then(|status| status.next_steps.first()) {
            return format!("{status}; runtime CLI detection is reported separately if missing.");
        }
        let proof_session_command = adapter_binding_proof_client_command_text(
            client.as_str(),
            &["--proof-session", "--json"],
        );
        return format!(
            "Run `{proof_session_command}` to collect missing private proof levels: observed_app_hook, observed_in_client_render, and observed_review_action; runtime CLI detection is reported separately if missing."
        );
    }
    if !mcp.readiness.mcp_registration_ready
        || (mcp.readiness.client_runtime.required_for_mcp_registration
            && mcp.readiness.client_runtime.status == "missing")
    {
        return mcp.readiness.next_step.clone();
    }
    if let Some(status) = binding {
        if let Some(step) = status.next_steps.first() {
            return step.clone();
        }
    }
    match client {
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            let proof_session_command = adapter_binding_proof_client_command_text(
                client.as_str(),
                &["--proof-session", "--json"],
            );
            format!(
                "Run `{proof_session_command}` and collect observed_app_hook, observed_in_client_render, and observed_review_action evidence.",
            )
        }
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => {
            "Run `tools/client-dogfood-report.sh` to exercise MCP/context/capture with this CLI surface.".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::adapter_binding_proof::ClientBindingLatestProofStatus;
    use crate::cli::binary_identity::BinaryFileScan;
    use crate::cli::mcp_config::{McpClientReadiness, McpClientReadinessCard, McpClientRuntime};
    use crate::storage::EpisodeSource;
    use tempfile::TempDir;

    fn binary_identity_fixture(status: &str) -> BinaryIdentity {
        BinaryIdentity {
            source: "soma_binary_identity.v1",
            status: status.to_string(),
            current_exe: Some("/tmp/current/soma".to_string()),
            path_soma: Some("/tmp/path/soma".to_string()),
            same_path: false,
            same_fingerprint: status != "path_soma_differs_from_current_exe",
            current_exe_scan: Some(BinaryFileScan {
                path: "/tmp/current/soma".to_string(),
                byte_len: Some(10),
                fingerprint: Some("fnv1a64:current".to_string()),
                error: None,
            }),
            path_soma_scan: Some(BinaryFileScan {
                path: "/tmp/path/soma".to_string(),
                byte_len: Some(9),
                fingerprint: Some("fnv1a64:path".to_string()),
                error: None,
            }),
            trust_boundary: "binary_identity_is_local_diagnostic_only",
        }
    }

    #[test]
    fn primary_command_uses_current_binary_when_path_soma_differs() {
        let identity = binary_identity_fixture("path_soma_differs_from_current_exe");
        let command = command_with_current_binary_when_path_soma_differs(
            vec!["soma".to_string(), "clients".to_string(), "--brief".to_string()],
            &identity,
        );

        assert_eq!(command[0], "/tmp/current/soma");
        assert_eq!(command[1], "clients");
    }

    #[test]
    fn primary_command_keeps_soma_when_path_binary_matches() {
        let identity = binary_identity_fixture("path_soma_matches_current_exe");
        let command = command_with_current_binary_when_path_soma_differs(
            vec!["soma".to_string(), "clients".to_string(), "--brief".to_string()],
            &identity,
        );

        assert_eq!(command[0], "soma");
    }

    fn cursor_mcp_report_with_missing_runtime() -> mcp_config::McpConfigCheckReport {
        mcp_report_with_missing_runtime("cursor", "~/.cursor/mcp.json", "cursor", "Cursor")
    }

    fn continue_mcp_report_with_missing_runtime() -> mcp_config::McpConfigCheckReport {
        mcp_report_with_missing_runtime(
            "continue",
            "~/.continue/mcpServers/soma.json",
            "continue",
            "Continue",
        )
    }

    fn cursor_mcp_report_with_app_bundle_runtime() -> mcp_config::McpConfigCheckReport {
        let mut report =
            mcp_report_with_missing_runtime("cursor", "~/.cursor/mcp.json", "Cursor.app", "Cursor");
        report.readiness.status = "mcp_registration_ready_private_capture_unproven";
        report.readiness.client_runtime = McpClientRuntime {
            target: "Cursor.app",
            status: "detected",
            detection_method: "macos_app_bundle_scan",
            path: Some("/Applications/Cursor.app".to_string()),
            launch_probe_command: Some(vec![
                "/usr/bin/open".to_string(),
                "/Applications/Cursor.app".to_string(),
            ]),
            launch_probe_note: Some(
                "Cursor.app bundle detection proves only that an app bundle exists.".to_string(),
            ),
            required_for_mcp_registration: false,
        };
        report.readiness.next_step =
            "Render proof still requires real Cursor UI evidence.".to_string();
        report
    }

    fn codex_app_mcp_report() -> mcp_config::McpConfigCheckReport {
        mcp_config::McpConfigCheckReport {
            client: "codex-app",
            target_path_hint: "~/.codex/config.toml",
            command: "/tmp/soma".to_string(),
            valid: true,
            checks: Vec::new(),
            readiness: McpClientReadiness {
                status: "mcp_registration_ready_private_capture_unproven",
                mcp_registration_ready: true,
                client_runtime: McpClientRuntime {
                    target: "Codex app settings",
                    status: "not_cli_detectable",
                    detection_method: "settings_surface",
                    path: None,
                    launch_probe_command: None,
                    launch_probe_note: None,
                    required_for_mcp_registration: false,
                },
                private_capture_ready: false,
                private_capture_boundary: "not_proven",
                next_step: "Open Codex app settings and confirm the MCP registration.".to_string(),
                card: McpClientReadinessCard {
                    source: "soma_mcp_config_readiness_card_v1",
                    state: "mcp_registration_ready_private_capture_unproven",
                    headline: "Codex app registration ready".to_string(),
                    safe_to_claim: Vec::new(),
                    blocked_claims: vec!["private capture proof".to_string()],
                    next_cli_commands: Vec::new(),
                    next_mcp_tool: None,
                    trust_boundary: "read_only",
                },
            },
        }
    }

    fn stored_episode_for_source(
        id: i64,
        source: EpisodeSource,
        project: Option<&str>,
        session_id: Option<&str>,
        prompt_text: Option<&str>,
    ) -> StoredEpisode {
        StoredEpisode {
            id,
            ts_start_ns: id * 100,
            ts_end_ns: id * 100 + 10,
            duration_ms: 0,
            source,
            session_id: session_id.map(ToOwned::to_owned),
            prompt_text: prompt_text.map(ToOwned::to_owned),
            response_text: None,
            command: None,
            stdout: None,
            exit_code: None,
            cwd: project.map(|project| format!("/tmp/{project}")),
            git_branch: Some("main".to_string()),
            project: project.map(ToOwned::to_owned),
            memory_tier: "short".to_string(),
            salience: None,
            digest: None,
        }
    }

    fn mcp_report_with_missing_runtime(
        client: &'static str,
        target_path_hint: &'static str,
        target: &'static str,
        display_name: &'static str,
    ) -> mcp_config::McpConfigCheckReport {
        mcp_config::McpConfigCheckReport {
            client,
            target_path_hint,
            command: "/tmp/soma".to_string(),
            valid: true,
            checks: Vec::new(),
            readiness: McpClientReadiness {
                status: "mcp_registration_config_ready_client_runtime_missing",
                mcp_registration_ready: true,
                client_runtime: McpClientRuntime {
                    target,
                    status: "missing",
                    detection_method: "path_lookup",
                    path: None,
                    launch_probe_command: None,
                    launch_probe_note: None,
                    required_for_mcp_registration: true,
                },
                private_capture_ready: false,
                private_capture_boundary: "not_proven",
                next_step: format!(
                    "Install or expose `{target}` on PATH, then rerun this check before registering SOMA."
                ),
                card: McpClientReadinessCard {
                    source: "soma_mcp_config_readiness_card_v1",
                    state: "mcp_registration_config_ready_client_runtime_missing",
                    headline: format!("{display_name} runtime missing"),
                    safe_to_claim: Vec::new(),
                    blocked_claims: vec![format!("{target} runtime detection")],
                    next_cli_commands: vec![vec!["which".to_string(), target.to_string()]],
                    next_mcp_tool: None,
                    trust_boundary: "read_only",
                },
            },
        }
    }

    #[test]
    fn cursor_runtime_launch_probe_surfaces_on_client_status_row() {
        let mcp = cursor_mcp_report_with_app_bundle_runtime();
        let row = row_for_client(McpClientKind::Cursor, &mcp, None, None, "available");

        assert_eq!(row.runtime_status, "detected");
        assert_eq!(row.runtime_target, "Cursor.app");
        assert_eq!(row.runtime_path.as_deref(), Some("/Applications/Cursor.app"));
        assert_eq!(
            row.runtime_launch_probe_command.as_deref(),
            Some(&["/usr/bin/open".to_string(), "/Applications/Cursor.app".to_string()][..])
        );
        assert!(row
            .runtime_launch_probe_note
            .as_deref()
            .is_some_and(|note| note.contains("bundle detection proves only")));
        assert!(!row.ready_for_private_client_claim);
    }

    #[test]
    fn observed_capture_dogfood_evidence_is_project_scoped() {
        let episodes = vec![
            stored_episode_for_source(
                9,
                EpisodeSource::CodexApp,
                Some("SOMA"),
                Some("codex-app-mcp-dogfood"),
                Some("Dogfood SOMA MCP capture from Codex app"),
            ),
            stored_episode_for_source(
                8,
                EpisodeSource::Cursor,
                Some("OTHER"),
                Some("cursor-session"),
                Some("Cursor capture from a different project"),
            ),
            stored_episode_for_source(
                7,
                EpisodeSource::Terminal,
                Some("SOMA"),
                Some("terminal-session"),
                Some("Terminal is not a client MCP target"),
            ),
        ];

        let evidence = observed_capture_dogfood_from_episodes(&episodes, Some("SOMA"), None);

        assert!(evidence.contains_key("codex-app"));
        assert!(!evidence.contains_key("cursor"));
        assert!(!evidence.contains_key("terminal"));
        let codex = evidence.get("codex-app").unwrap();
        assert_eq!(codex.status, "observed_local_capture_episode");
        assert_eq!(codex.evidence_ref, "episode:9");
        assert_eq!(codex.project.as_deref(), Some("SOMA"));
        assert_eq!(codex.session_id.as_deref(), Some("codex-app-mcp-dogfood"));
        assert!(!codex.private_release_proof);
        assert!(codex
            .recall_command
            .windows(2)
            .any(|pair| pair[0] == "--project" && pair[1] == "SOMA"));
        assert!(codex
            .context_why_command
            .windows(2)
            .any(|pair| pair[0] == "--session-id" && pair[1] == "codex-app-mcp-dogfood"));
    }

    #[test]
    fn capture_dogfood_evidence_does_not_satisfy_private_app_release_proof() {
        let mcp = codex_app_mcp_report();
        let mut row = row_for_client(McpClientKind::CodexApp, &mcp, None, None, "available");
        let episode = stored_episode_for_source(
            42,
            EpisodeSource::CodexApp,
            Some("SOMA"),
            Some("codex-app-mcp-dogfood"),
            Some("Dogfood SOMA MCP capture from Codex app"),
        );
        let evidence =
            observed_capture_dogfood_evidence_for_episode(McpClientKind::CodexApp, &episode);

        attach_observed_capture_dogfood(&mut row, evidence);

        assert!(!row.ready_for_private_client_claim);
        assert!(row.observed_capture_dogfood_evidence.is_some());
        assert!(row.safe_to_claim.iter().any(|claim| {
            claim.contains("Stored local capture dogfood evidence exists")
                && claim.contains("private release proof remains separate")
        }));
        assert!(row.blocked_claims.iter().any(|claim| claim.contains("Automatic private capture")));
    }

    fn latest_proof_status_for_test(
        proof_id: i64,
        proof_level: ClientBindingProofLevel,
        all_artifacts_verified: bool,
    ) -> ClientBindingLatestProofStatus {
        ClientBindingLatestProofStatus {
            proof_id,
            proof_level,
            observed_at_ns: 123,
            manifest_status: "reference_binding".to_string(),
            evidence_source: "private_client_operator_observed_cursor_test".to_string(),
            operator_confirmed_release_grade_evidence: true,
            installed_config_path: None,
            installed_config_fingerprint: None,
            installed_config_binding_nonce: Some("soma-bind-test".to_string()),
            review_action_control_id: None,
            all_artifacts_verified,
            artifact_checks: Vec::new(),
        }
    }

    fn cursor_artifact_failed_binding() -> ClientBindingReadinessStatus {
        let mut latest_by_level = BTreeMap::new();
        latest_by_level.insert(
            ClientBindingProofLevel::ObservedAppHook.as_str().to_string(),
            latest_proof_status_for_test(2, ClientBindingProofLevel::ObservedAppHook, true),
        );
        latest_by_level.insert(
            ClientBindingProofLevel::ObservedInClientRender.as_str().to_string(),
            latest_proof_status_for_test(3, ClientBindingProofLevel::ObservedInClientRender, false),
        );
        latest_by_level.insert(
            ClientBindingProofLevel::ObservedReviewAction.as_str().to_string(),
            latest_proof_status_for_test(4, ClientBindingProofLevel::ObservedReviewAction, false),
        );
        ClientBindingReadinessStatus {
            client: "cursor".to_string(),
            proof_stage: "real_app_hook_in_client_render_and_review_action_observed".to_string(),
            readiness: "artifact_integrity_failed".to_string(),
            ready_for_private_client_claim: false,
            has_reference_binding: false,
            has_observed_event_file: false,
            has_observed_app_hook: true,
            has_observed_in_client_render: true,
            has_observed_review_action: true,
            ready_for_client_operator_loop: false,
            latest_proof_id: Some(4),
            latest_proof_level: Some(ClientBindingProofLevel::ObservedReviewAction),
            latest_observed_at_ns: Some(123),
            latest_by_level,
            all_latest_artifacts_verified: false,
            artifact_failures: vec![
                ClientBindingArtifactFailure {
                    proof_id: 3,
                    proof_level: ClientBindingProofLevel::ObservedInClientRender,
                    kind: "render_evidence".to_string(),
                    path: Some("/tmp/cursor-live-render-evidence.json".to_string()),
                    status: EvidenceArtifactStatus::MissingFile,
                    error: Some("No such file or directory".to_string()),
                },
                ClientBindingArtifactFailure {
                    proof_id: 4,
                    proof_level: ClientBindingProofLevel::ObservedReviewAction,
                    kind: "review_action_report".to_string(),
                    path: Some("/tmp/cursor-live-review-action.json".to_string()),
                    status: EvidenceArtifactStatus::MissingFile,
                    error: Some("No such file or directory".to_string()),
                },
            ],
            coherence_failures: Vec::new(),
            non_release_evidence_sources: Vec::new(),
            next_steps: vec!["refresh_or_replay_changed_evidence_artifacts".to_string()],
            operator_flow: Vec::new(),
        }
    }

    fn cursor_ready_binding() -> ClientBindingReadinessStatus {
        ClientBindingReadinessStatus {
            client: "cursor".to_string(),
            proof_stage: "real_app_hook_in_client_render_and_review_action_observed".to_string(),
            readiness: "real_app_hook_in_client_render_and_review_action_observed".to_string(),
            ready_for_private_client_claim: true,
            has_reference_binding: false,
            has_observed_event_file: false,
            has_observed_app_hook: true,
            has_observed_in_client_render: true,
            has_observed_review_action: true,
            ready_for_client_operator_loop: true,
            latest_proof_id: Some(7),
            latest_proof_level: Some(ClientBindingProofLevel::ObservedReviewAction),
            latest_observed_at_ns: Some(123),
            latest_by_level: BTreeMap::<String, ClientBindingLatestProofStatus>::new(),
            all_latest_artifacts_verified: true,
            artifact_failures: Vec::new(),
            coherence_failures: Vec::new(),
            non_release_evidence_sources: Vec::new(),
            next_steps: Vec::new(),
            operator_flow: Vec::new(),
        }
    }

    fn cursor_proof_session(
        status: &str,
        release_gate: &str,
        next_step_id: Option<&str>,
        private_target_configs: usize,
    ) -> ClientProofSessionSummary {
        let runbook_steps = if next_step_id == Some("capture_in_client_render_evidence") {
            vec![ClientProofSessionRunbookStepSummary {
                source: "soma_clients.private_app_proof_session_runbook_step.v1",
                id: "capture_in_client_render_evidence".to_string(),
                title: "Capture in-client render evidence".to_string(),
                intent: "Prepare proof-free render artifacts before visible UI observation."
                    .to_string(),
                stage: "in_client_render".to_string(),
                evidence_kind: "in_client_render_evidence_packet".to_string(),
                command: Some(vec![
                    "tools/soma-client-render-proof-prep.sh".to_string(),
                    "--client".to_string(),
                    "cursor".to_string(),
                    "--soma-bin".to_string(),
                    "soma".to_string(),
                    "--manifest".to_string(),
                    "tools/client-bindings/cursor-soma-binding.json.example".to_string(),
                    "--artifact-dir".to_string(),
                    "/tmp/cursor-artifacts".to_string(),
                ]),
                mcp_tool: Some("soma_client_render_evidence_packet".to_string()),
                mcp_arguments_json: None,
                external_action_safety: None,
                external_action: None,
                suggested_artifact_path: Some("/tmp/cursor-artifacts/render-evidence.json".to_string()),
                requires_operator_action: true,
                records_proof: false,
                ready_now: true,
                blocking_reasons: Vec::new(),
                proof_step_trust_boundary:
                    "render_proof_prep_records_no_proof_and_reuses_existing_artifacts_without_overwrite"
                        .to_string(),
                trust_boundary:
                    "client_proof_session_runbook_step_summary_is_read_only_and_records_no_proof",
            }]
        } else {
            Vec::new()
        };
        ClientProofSessionSummary {
            status: Some(status.to_string()),
            release_gate: Some(release_gate.to_string()),
            next_step_id: next_step_id.map(str::to_string),
            next_operator_step_title: next_step_id.map(|step| match step {
                "install_or_merge_private_client_config" => {
                    "Install or merge config in the private client".to_string()
                }
                _ => step.to_string(),
            }),
            next_operator_step_intent: next_step_id.map(|_| {
                "Operator/client must place the rendered config where the private app will actually call SOMA wrappers.".to_string()
            }),
            next_operator_step_trust_boundary: next_step_id.map(|_| {
                "human_or_client_install_step_is_required_before_app_hook_evidence_exists"
                    .to_string()
            }),
            next_operator_step_requires_operator_action: next_step_id.map(|_| true),
            next_command: None,
            next_mcp_tool: None,
            next_mcp_arguments: None,
            external_action: None,
            expected_event_source: Some("cursor_private_lifecycle_hook".to_string()),
            binding_nonce: Some("soma-bind-test".to_string()),
            generated_binding_nonce: Some(false),
            event_jsonl_path: Some("/tmp/events.jsonl".to_string()),
            event_jsonl_probe_status: Some("scanned".to_string()),
            blocking_reasons: Vec::new(),
            ready_to_record_proof_levels: if next_step_id == Some("record_observed_app_hook") {
                vec!["observed_app_hook".to_string()]
            } else {
                Vec::new()
            },
            stage_blockers: Vec::new(),
            runbook_steps,
            ready_now_step_count: Some(usize::from(next_step_id.is_some())),
            blocking_reason_count: Some(0),
            installed_config_eligible_candidates: Some(1),
            installed_config_setup_artifact_eligible_candidates: Some(usize::from(
                private_target_configs == 0,
            )),
            installed_config_private_target_eligible_candidates: Some(private_target_configs),
            eligible_setup_artifact_paths: if private_target_configs == 0 {
                vec!["/tmp/.soma/client-bindings/cursor-installed-binding.json".to_string()]
            } else {
                Vec::new()
            },
            eligible_private_client_target_paths: if private_target_configs > 0 {
                vec!["/tmp/.cursor/soma-installed-binding.json".to_string()]
            } else {
                Vec::new()
            },
            private_client_target_candidate_paths: vec![
                "/tmp/.cursor/soma-installed-binding.json".to_string(),
            ],
            error: None,
        }
    }

    #[test]
    fn artifact_integrity_failure_next_step_takes_priority_over_runtime_hint() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let mut proof_session = cursor_proof_session(
            "blocked_by_stored_proof_integrity_or_identity",
            "fail",
            Some("render_review_surface"),
            1,
        );
        let repair_command = vec![
            "soma".to_string(),
            "context".to_string(),
            "review-render".to_string(),
            "--client".to_string(),
            "cursor".to_string(),
            "--format".to_string(),
            "json".to_string(),
            "--write-report".to_string(),
            "/tmp/.soma/client-evidence/cursor/soma-bind-test/review-render.json".to_string(),
        ];
        proof_session.next_command = Some(repair_command.clone());
        let row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );

        assert_eq!(row.private_capture_status, "artifact_integrity_failed");
        assert_eq!(row.artifact_failure_count, 2);
        assert_eq!(row.coherence_failure_count, 0);
        assert_eq!(row.proof_stage.as_deref(), Some("stored_proof_artifacts_invalid"));
        assert!(row.next_step.contains("--write-report"), "{}", row.next_step);
        assert!(row.next_step.contains("--status --json"), "{}", row.next_step);
        assert!(!row.next_step.contains("Install or expose `cursor`"), "{}", row.next_step);
        let primary = primary_private_app_command(&row);
        assert_ne!(primary, repair_command);
        assert_eq!(row.operator_next_command.as_ref(), Some(&primary));
        assert!(primary.iter().any(|part| part == "review-render"));
        assert!(primary.windows(2).any(|pair| {
            pair[0] == "--write-report"
                && pair[1]
                    .ends_with("/.soma/client-evidence/cursor/soma-bind-test/review-render.json")
        }));
        let safety =
            primary_next_command_safety(&private_app_operator_next_action_id(&row), &primary);
        assert_eq!(safety.classification, "local_file_write_command");
        assert!(safety.writes_local_files);
        assert!(!safety.records_proof);
        assert!(row.blocked_claims.iter().any(|claim| claim.contains("proof artifacts changed")));
        assert!(row.proof_level_statuses.iter().any(|proof| {
            proof.proof_level == "observed_app_hook"
                && proof.status == "recorded"
                && !proof.blocks_private_client_claim
        }));
        assert!(row.proof_level_statuses.iter().any(|proof| {
            proof.proof_level == "observed_in_client_render"
                && proof.status == "artifact_invalid"
                && proof.blocks_private_client_claim
        }));
        assert!(row.proof_level_statuses.iter().any(|proof| {
            proof.proof_level == "observed_review_action"
                && proof.status == "artifact_invalid"
                && proof.blocks_private_client_claim
        }));
        let checklist = build_private_app_release_proof_checklist(std::slice::from_ref(&row))
            .pop()
            .expect("release proof checklist");
        assert_eq!(
            checklist.next_required_proof_level.as_deref(),
            Some("observed_in_client_render")
        );
        assert!(
            checklist.next_recording_after_trusted_evidence.is_some(),
            "artifact invalid render step should still expose the next recording hint"
        );
        assert!(checklist.proof_level_statuses.iter().any(|proof| {
            proof.proof_level == "observed_in_client_render"
                && proof.status == "artifact_invalid"
                && proof.blocks_private_client_claim
        }));
        assert_eq!(private_app_proof_ladder_status(&checklist, "observed_app_hook"), "done");
        assert_eq!(
            private_app_proof_ladder_status(&checklist, "observed_in_client_render"),
            "artifact_invalid"
        );
        assert_eq!(
            private_app_proof_ladder_status(&checklist, "observed_review_action"),
            "artifact_invalid"
        );
        let plan = build_private_app_release_plan(std::slice::from_ref(&row))
            .pop()
            .expect("release proof plan");
        assert_eq!(plan.next_required_proof_level.as_deref(), Some("observed_in_client_render"));
        assert_eq!(plan.completed_proof_levels, vec!["observed_app_hook"]);
        assert!(plan.proof_level_statuses.iter().any(|proof| {
            proof.proof_level == "observed_in_client_render"
                && proof.status == "artifact_invalid"
                && proof.blocks_private_client_claim
        }));
        assert!(row.next_commands.iter().any(|command| {
            command.iter().any(|part| part == "--status")
                && command.iter().any(|part| part == "--json")
        }));
        let repair_plan = row.artifact_repair_plan.as_ref().expect("artifact repair plan");
        assert_eq!(repair_plan.status, "artifact_integrity_failed");
        assert_eq!(repair_plan.failure_count, 2);
        let repair_summary = row.artifact_repair_summary.as_ref().expect("artifact repair summary");
        assert_eq!(repair_summary.status, "artifact_integrity_failed");
        assert_eq!(repair_summary.failure_count, 2);
        assert!(repair_summary.proof_free_local_materialization_only);
        assert!(repair_summary.requires_real_private_client_evidence_before_recording);
        assert!(!repair_summary.records_proof);
        assert!(!repair_summary.creates_verification_event);
        assert!(!repair_summary.promotes_cloud_draft);
        assert!(repair_summary.next_command.iter().any(|part| part == "review-render"));
        assert_eq!(repair_summary.next_command_safety.classification, "local_file_write_command");
        assert!(repair_summary
            .operator_checklist
            .iter()
            .any(|step| step.contains("Generate a fresh read-only review-render report")));
        assert!(repair_summary
            .required_observation_fields
            .contains(&"observed_at_ns=positive_unix_epoch_nanoseconds_after_visible_render"));
        assert!(repair_summary
            .proof_recording_preconditions
            .contains(&"cloud_draft_or_review_render_output_is_not_used_as_verification_evidence"));
        let render_scan =
            repair_summary.render_evidence_artifact_scan.as_ref().expect("render evidence scan");
        assert_eq!(render_scan.status, "missing_file");
        assert_eq!(render_scan.placeholder_count, 0);
        assert!(render_scan
            .missing_requirements
            .contains(&"render_evidence_file_required".to_string()));
        assert!(!render_scan.records_proof);
        assert!(!render_scan.creates_verification_event);
        assert!(!render_scan.promotes_cloud_draft);
        assert!(repair_summary
            .forbidden_shortcuts
            .iter()
            .any(|shortcut| { shortcut.contains("do_not_record_observed_in_client_render") }));
        let actions = build_private_app_next_actions(std::slice::from_ref(&row));
        let action = actions.first().expect("private app next action");
        assert_eq!(action.artifact_failure_count, 2);
        assert_eq!(action.coherence_failure_count, 0);
        let action_repair_summary = action
            .artifact_repair_summary
            .as_ref()
            .expect("private app action should expose artifact repair summary");
        assert_eq!(action_repair_summary.status, "artifact_integrity_failed");
        assert_eq!(action_repair_summary.failure_count, repair_summary.failure_count);
        assert_eq!(action_repair_summary.next_command, repair_summary.next_command);
        assert!(action_repair_summary.proof_free_local_materialization_only);
        assert!(!action_repair_summary.records_proof);
        assert!(!action_repair_summary.creates_verification_event);
        assert!(!action_repair_summary.promotes_cloud_draft);
        let summary = ClientStatusSummary {
            client_count: 1,
            mcp_registration_ready_count: 1,
            runtime_detected_count: 0,
            runtime_missing_count: 1,
            explicit_cli_client_count: 0,
            explicit_cli_capture_available_count: 0,
            explicit_cli_real_capture_observed_count: 0,
            explicit_cli_real_capture_blocked_count: 0,
            explicit_cli_real_capture_failed_count: 0,
            explicit_cli_real_capture_unproven_count: 0,
            private_app_client_count: 1,
            private_app_capture_ready_count: 0,
            private_app_capture_unproven_count: 1,
            private_app_installed_config_ready_count: 1,
            private_app_target_config_ready_count: 1,
            private_app_trigger_hook_next_count: 0,
            private_app_hook_trigger_ready_count: 0,
            private_app_record_app_hook_next_count: 0,
            private_app_app_hook_proven_count: 1,
            private_app_in_client_render_proven_count: 1,
            private_app_review_action_proven_count: 1,
            private_capture_ready_count: 0,
            private_capture_unproven_count: 1,
            client_binding_rows_seen: 3,
            proof_storage_unavailable: false,
        };
        let semantic_review = ClientSemanticReviewStatus {
            source: "soma_clients.semantic_review_status",
            status: "clear".to_string(),
            operator_next_action_id: "inspect_semantic_learning_status".to_string(),
            operator_next_action_label: "Inspect semantic learning status".to_string(),
            client: "generic".to_string(),
            scope_source: "global_learning_scope".to_string(),
            project: None,
            primary_surface: "clear".to_string(),
            workload_summary: empty_client_semantic_workload_summary(None),
            pending_review_item_count: 0,
            l4_candidate_count: 0,
            review_only_candidate_count: 0,
            cloud_draft_blocked_count: 0,
            belief_candidate_count: 0,
            belief_group_count: 0,
            belief_hidden_duplicate_count: 0,
            belief_contradiction_count: 0,
            belief_substantive_contradiction_count: 0,
            belief_low_value_conflict_count: 0,
            belief_low_value_noise_count: 0,
            belief_noise_candidate_count: 0,
            belief_review_summary: empty_client_belief_review_summary(),
            should_interrupt: false,
            next_step: "No pending semantic learning review work is visible for this scope."
                .to_string(),
            workload_command: vec![
                "soma".to_string(),
                "learning".to_string(),
                "--brief".to_string(),
                "--client".to_string(),
                "generic".to_string(),
            ],
            primary_command: vec!["soma".to_string(), "learning".to_string(), "--json".to_string()],
            next_commands: Vec::new(),
            review_render_command: Vec::new(),
            review_digest_command: Vec::new(),
            review_report_command: Vec::new(),
            review_actions_command: Vec::new(),
            proof_session_command: Vec::new(),
            semantic_resolution_actions: Vec::new(),
            review_cards: Vec::new(),
            promotion_matrix: Vec::new(),
            review_lanes: Vec::new(),
            next_mcp_tools: Vec::new(),
            control_contract: "clear".to_string(),
            proof_path: "clear".to_string(),
            error: None,
            trust_boundary:
                "semantic_review_status_test_fixture_is_read_only: records no proof row",
        };
        let operator_card =
            build_operator_card(&summary, &semantic_review, std::slice::from_ref(&row), None, None);
        assert_eq!(operator_card.primary_client.as_deref(), Some("cursor"));
        assert_eq!(
            operator_card
                .primary_artifact_repair_summary
                .as_ref()
                .map(|summary| summary.failure_count),
            Some(repair_summary.failure_count)
        );
        assert_eq!(
            operator_card
                .primary_artifact_repair_summary
                .as_ref()
                .map(|summary| summary.next_command.clone()),
            Some(repair_summary.next_command.clone())
        );
        let release_snapshot = build_private_app_release_snapshot(&operator_card, None);
        let readiness_index = build_readiness_index(
            &summary,
            &semantic_review,
            &operator_card,
            &release_snapshot,
            None,
        );
        assert_eq!(
            readiness_index.primary_artifact_repair_summary,
            operator_card.primary_artifact_repair_summary
        );
        let mut brief = String::new();
        render_artifact_repair_plan_brief(
            &mut brief,
            row.artifact_repair_plan.as_ref(),
            row.artifact_repair_summary.as_ref(),
        );
        assert!(
            brief.contains("check: Generate a fresh read-only review-render report"),
            "{brief}"
        );
        assert!(
            brief.contains(
                "required observation: observed_at_ns=positive_unix_epoch_nanoseconds_after_visible_render"
            ),
            "{brief}"
        );
        assert!(
            brief.contains(
                "proof precondition: cloud_draft_or_review_render_output_is_not_used_as_verification_evidence"
            ),
            "{brief}"
        );
        assert!(
            brief.contains("forbidden shortcut: do_not_record_observed_in_client_render"),
            "{brief}"
        );
        assert!(
            repair_plan
                .suggested_artifact_dir
                .ends_with("/.soma/client-evidence/cursor/soma-bind-test"),
            "{}",
            repair_plan.suggested_artifact_dir
        );
        assert!(repair_plan.suggested_artifact_paths.iter().any(|suggestion| {
            suggestion.artifact_kind == "render_evidence"
                && suggestion
                    .path
                    .ends_with("/.soma/client-evidence/cursor/soma-bind-test/render-evidence.json")
        }));
        assert!(repair_plan.suggested_artifact_paths.iter().any(|suggestion| {
            suggestion.artifact_kind == "review_action_report"
                && suggestion
                    .path
                    .ends_with("/.soma/client-evidence/cursor/soma-bind-test/review-action.json")
        }));
        assert!(repair_plan.failed_artifacts.iter().any(|failure| {
            failure.proof_level == "observed_in_client_render"
                && failure.artifact_kind == "render_evidence"
                && failure.status == "missing_file"
                && failure.recovery_action.contains("fresh structured render evidence")
        }));
        assert!(repair_plan.failed_artifacts.iter().any(|failure| {
            failure.proof_level == "observed_review_action"
                && failure.artifact_kind == "review_action_report"
                && failure.status == "missing_file"
                && failure.recovery_action.contains("storage-gated review-action report")
        }));
        assert!(repair_plan
            .diagnostic_commands
            .iter()
            .any(|command| command.iter().any(|part| part == "--verify-evidence-artifacts")));
        assert!(repair_plan
            .recovery_steps
            .iter()
            .any(|step| step.contains("Re-record `observed_in_client_render` only after")));
        assert!(repair_plan.recovery_steps.iter().any(|step| {
            step.contains(
                "blocked `record_observed_in_client_render` row prints the exact proof command",
            )
        }));
        assert!(repair_plan.recovery_steps.iter().any(|step| {
            step.contains(
                "blocked `record_observed_review_action` row prints the exact proof command",
            )
        }));
        assert!(repair_plan
            .blocked_claims
            .iter()
            .any(|claim| { claim.contains("Private-client readiness is not claimable") }));
        let primary = primary_private_app_command(&row);
        assert!(primary.iter().any(|part| part == "--write-report"));
        assert!(primary.iter().any(|part| part == "review-render"));
        assert!(primary.windows(2).any(|pair| pair[0] == "--client" && pair[1] == "cursor"));
        assert!(!primary.iter().any(|part| part == "tools/soma-client-hook-readiness.sh"));
    }

    #[test]
    fn latest_real_cli_probe_blocker_is_not_hidden_by_prior_capture_evidence() {
        let item = ClientCaptureDogfoodMatrixItem {
            source: "soma_clients.capture_dogfood_matrix.v1",
            client: "claude-code".to_string(),
            capture_model: EXPLICIT_CLI_CAPTURE_MODEL.to_string(),
            mcp_registration_ready: true,
            explicit_cli_capture_available: true,
            observed_local_capture: true,
            private_release_proof_ready: false,
            status: "observed_local_capture".to_string(),
            last_real_cli_probe_status: Some("auth_blocked".to_string()),
            last_real_cli_probe_next_action: Some(
                "configure an Anthropic API key, then rerun this probe".to_string(),
            ),
            last_real_cli_probe_artifact_path: Some(
                "/Users/example/.soma/reports/real-cli-dogfood-latest.json".to_string(),
            ),
            last_real_cli_probe_generated_at_unix_ms: Some(1_782_395_437_937),
            evidence_ref: Some("episode:1263".to_string()),
            project: Some("feyman-study-project".to_string()),
            session_id: Some("2cb44462-6a26-4267-bb9f-9fa225c9dc03".to_string()),
            next_command: vec![
                "tools/real-cli-dogfood-probe.sh".to_string(),
                "--client".to_string(),
                "claude-code".to_string(),
            ],
            trust_boundary: "test_read_only",
        };

        let matrix = vec![item];
        let blocker = primary_real_cli_probe_blocker(&matrix)
            .expect("latest auth-blocked real CLI probe should remain a blocker");
        let blocking_status = real_cli_probe_blocking_status(blocker).expect("blocking status");
        assert_eq!(blocking_status, "real_cli_auth_blocked");
        assert_eq!(
            real_cli_probe_operator_next_action_id(blocking_status),
            "configure_cli_auth_for_real_dogfood"
        );
        assert_eq!(
            real_cli_probe_operator_next_action_label(blocking_status),
            "Configure CLI auth"
        );
    }

    #[test]
    fn artifact_repair_summary_scans_render_evidence_placeholders() {
        let tmp = TempDir::new().expect("tempdir");
        let render_evidence = tmp.path().join("render-evidence.json");
        fs::write(
            &render_evidence,
            r#"{
  "schema": "soma.in_client_render_evidence.v1",
  "client": "cursor",
  "source": "<manual_operator_or_client_capture>",
  "observed_at_ns": "<positive_unix_epoch_nanoseconds_after_visible_render>",
  "review_render_fingerprint": "fnv1a64:1234",
  "rendered_surfaces": [{
    "kind": "<client_surface_kind>",
    "title": "<visible_surface_title>",
    "visible": "<true_after_visible_render>"
  }]
}"#,
        )
        .expect("write render evidence");
        fs::write(tmp.path().join("review-render.json"), r#"{"schema":"soma.review_render.v1"}"#)
            .expect("write review render json");
        fs::write(tmp.path().join("review-render.md"), "# SOMA review render\n")
            .expect("write review render markdown");
        fs::write(tmp.path().join("review-render.html"), "<html><body>SOMA</body></html>\n")
            .expect("write review render html");
        let plan = ClientArtifactRepairPlan {
            source: "test",
            status: "artifact_integrity_failed",
            client: "cursor",
            failure_count: 1,
            suggested_artifact_dir: tmp.path().to_string_lossy().into_owned(),
            suggested_artifact_dir_write_status: "writable".to_string(),
            suggested_artifact_paths: vec![ClientArtifactPathSuggestion {
                artifact_kind: "render_evidence".to_string(),
                path: render_evidence.to_string_lossy().into_owned(),
                intent: "test render evidence".to_string(),
            }],
            workspace_fallback_artifact_dir: None,
            workspace_fallback_artifact_paths: Vec::new(),
            workspace_fallback_commands: Vec::new(),
            failed_artifacts: vec![ClientArtifactRepairFailure {
                proof_id: 1,
                proof_level: "observed_in_client_render".to_string(),
                artifact_kind: "render_evidence".to_string(),
                path: Some(render_evidence.to_string_lossy().into_owned()),
                status: "missing_file".to_string(),
                recovery_action: "test".to_string(),
            }],
            diagnostic_commands: Vec::new(),
            recovery_steps: Vec::new(),
            blocked_claims: Vec::new(),
            trust_boundary: "test",
        };

        let summary = artifact_repair_summary_for_client(McpClientKind::Cursor, &plan);
        let scan = summary.render_evidence_artifact_scan.as_ref().expect("render evidence scan");
        assert_eq!(scan.status, "template_placeholders_present");
        assert!(scan.placeholder_count >= 4, "{scan:?}");
        assert!(scan
            .missing_requirements
            .contains(&"source_must_be_manual_operator_or_client_capture".to_string()));
        assert!(scan.missing_requirements.contains(&"observed_at_ns_must_be_positive".to_string()));
        assert!(scan
            .missing_requirements
            .contains(&"rendered_surfaces_must_not_contain_template_placeholders".to_string()));
        assert!(scan
            .missing_requirements
            .contains(&"rendered_surfaces_must_include_visible_surface".to_string()));
        assert!(scan.missing_requirements.contains(&"template_placeholders_absent".to_string()));
        assert!(!scan.records_proof);
        assert!(!scan.creates_verification_event);
        assert!(!scan.promotes_cloud_draft);
        let packet = summary.render_proof_packet_scan.as_ref().expect("render proof packet scan");
        assert_eq!(packet.status, "prepared_placeholders_pending");
        assert!(packet.review_render_json_exists);
        assert!(packet.review_render_markdown_exists);
        assert!(packet.review_render_html_exists);
        assert!(packet.render_evidence_exists);
        assert_eq!(packet.placeholder_count, scan.placeholder_count);
        assert!(packet.proof_free_local_materialization_only);
        assert!(!packet.records_proof);
        assert!(!packet.creates_verification_event);
        assert!(!packet.promotes_cloud_draft);
        assert!(packet.next_step.contains("real private client UI"));

        let mut brief = String::new();
        render_artifact_repair_plan_brief(&mut brief, Some(&plan), Some(&summary));
        assert!(
            brief.contains("render evidence scan: status=template_placeholders_present"),
            "{brief}"
        );
        assert!(
            brief.contains("render evidence missing: observed_at_ns_must_be_positive"),
            "{brief}"
        );
        assert!(brief.contains("render packet: status=prepared_placeholders_pending"), "{brief}");
        assert!(brief.contains("render packet view: markdown="), "{brief}");

        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let proof_session = cursor_proof_session(
            "blocked_by_stored_proof_integrity_or_identity",
            "fail",
            Some("capture_in_client_render_evidence"),
            1,
        );
        let mut row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );
        row.artifact_repair_plan = Some(plan);
        row.artifact_repair_summary = Some(summary);
        let step = private_app_operator_next_step(&row);
        assert!(step.contains("render packet is already materialized"), "{step}");
        assert!(step.contains("review-render.md"), "{step}");
        assert!(step.contains("render-evidence.json"), "{step}");
        assert!(step.contains("placeholder(s)"), "{step}");
        assert!(step.contains("real Cursor UI"), "{step}");
    }

    #[test]
    fn artifact_repair_summary_rejects_raw_mcp_tool_output_render_evidence() {
        let tmp = TempDir::new().expect("tempdir");
        let render_evidence = tmp.path().join("render-evidence.json");
        fs::write(
            &render_evidence,
            r#"{
  "schema": "soma.in_client_render_evidence.v1",
  "client": "cursor",
  "source": "client_capture",
  "observed_at_ns": 12345,
  "review_render_fingerprint": "fnv1a64:1234",
  "rendered_surfaces": [{
    "kind": "cursor_mcp_tool_result",
    "name": "action_buttons",
    "title": "SOMA review render visible as raw MCP tool output",
    "visible": true
  }]
}"#,
        )
        .expect("write render evidence");
        let missing = render_evidence_artifact_missing_requirements(
            "cursor",
            &serde_json::from_str(&fs::read_to_string(&render_evidence).unwrap()).unwrap(),
        );

        assert!(
            missing.contains(&"rendered_surfaces_must_not_be_raw_mcp_or_tool_output".to_string())
        );
        assert!(!missing.contains(&"source_must_be_manual_operator_or_client_capture".to_string()));
        assert!(!missing.contains(&"observed_at_ns_must_be_positive".to_string()));
        assert!(!missing.contains(&"rendered_surfaces_must_include_visible_surface".to_string()));
    }

    #[test]
    fn artifact_repair_primary_command_advances_after_review_render_report_exists() {
        let tmp = TempDir::new().expect("tempdir");
        let review_render = tmp.path().join("review-render.json");
        let render_evidence = tmp.path().join("render-evidence.json");
        fs::write(&review_render, "{}\n").expect("write review-render");
        let plan = ClientArtifactRepairPlan {
            source: "test",
            status: "artifact_integrity_failed",
            client: "cursor",
            failure_count: 2,
            suggested_artifact_dir: tmp.path().to_string_lossy().into_owned(),
            suggested_artifact_dir_write_status: "writable".to_string(),
            suggested_artifact_paths: vec![
                ClientArtifactPathSuggestion {
                    artifact_kind: "review_render_report".to_string(),
                    path: review_render.to_string_lossy().into_owned(),
                    intent: "test review render".to_string(),
                },
                ClientArtifactPathSuggestion {
                    artifact_kind: "render_evidence".to_string(),
                    path: render_evidence.to_string_lossy().into_owned(),
                    intent: "test render evidence".to_string(),
                },
            ],
            workspace_fallback_artifact_dir: None,
            workspace_fallback_artifact_paths: Vec::new(),
            workspace_fallback_commands: Vec::new(),
            failed_artifacts: Vec::new(),
            diagnostic_commands: Vec::new(),
            recovery_steps: Vec::new(),
            blocked_claims: Vec::new(),
            trust_boundary: "test",
        };

        let command =
            artifact_repair_primary_command_for_plan("cursor", &plan).expect("primary command");
        assert!(command.iter().any(|part| part == "--render-render-evidence"));
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--review-render-report"
                && pair[1] == review_render.to_string_lossy().as_ref()
        }));
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--write-render-evidence"
                && pair[1] == render_evidence.to_string_lossy().as_ref()
        }));
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--manifest"
                && pair[1] == "tools/client-bindings/cursor-soma-binding.json.example"
        }));
        assert!(!command.iter().any(|part| part == "--write-report"));
        let safety = primary_next_command_safety(
            "materialize_render_evidence_packet_for_artifact_repair",
            &command,
        );
        assert_eq!(safety.classification, "local_file_write_command");
        assert!(safety.writes_local_files);
        assert!(!safety.records_proof);
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let proof_session =
            cursor_proof_session("blocked_by_stored_proof_integrity_or_identity", "fail", None, 1);
        let mut row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );
        row.artifact_repair_plan = Some(plan.clone());
        let operator_step = private_app_operator_next_step(&row);
        assert!(operator_step.contains("visible UI evidence is not proven yet"), "{operator_step}");
        assert!(
            operator_step.contains("proof-free render evidence packet template"),
            "{operator_step}"
        );
        assert!(!operator_step.contains("Fresh review-render"), "{operator_step}");

        fs::write(&render_evidence, "{}\n").expect("write render evidence");
        let command =
            artifact_repair_primary_command_for_plan("cursor", &plan).expect("brief command");
        assert!(command.iter().any(|part| part == "--proof-session"));
        assert!(command.iter().any(|part| part == "--brief"));
        assert!(!command_renders_evidence(&command));
        assert!(!command.iter().any(|part| part == "--write-report"));
        row.artifact_repair_plan = Some(plan);
        let operator_step = private_app_operator_next_step(&row);
        assert!(operator_step.contains("visible UI evidence is not proven yet"), "{operator_step}");
        assert!(
            operator_step.contains("proof-free render-evidence packet template"),
            "{operator_step}"
        );
        assert!(!operator_step.contains("Fresh review-render"), "{operator_step}");
    }

    #[test]
    fn proof_session_next_mcp_arguments_are_mirrored_into_client_row() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let mut proof_session = cursor_proof_session(
            "blocked_by_stored_proof_integrity_or_identity",
            "fail",
            Some("capture_in_client_render_evidence"),
            1,
        );
        proof_session.next_mcp_tool = Some("soma_client_render_evidence_packet".to_string());
        proof_session.next_mcp_arguments = Some(serde_json::json!({
            "client": "cursor",
            "manifest": "tools/client-bindings/cursor-soma-binding.json.example",
            "review_render_report": "/tmp/review-render.json"
        }));

        let row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );

        assert_eq!(
            row.proof_session_next_mcp_tool.as_deref(),
            Some("soma_client_render_evidence_packet")
        );
        assert_eq!(
            row.proof_session_next_mcp_arguments
                .as_ref()
                .and_then(|value| value["review_render_report"].as_str()),
            Some("/tmp/review-render.json")
        );
    }

    #[test]
    fn artifact_integrity_capture_step_surfaces_render_capture_action() {
        let tmp = TempDir::new().expect("tempdir");
        let review_render = tmp.path().join("review-render.json");
        let render_evidence = tmp.path().join("render-evidence.json");
        fs::write(&review_render, "{}\n").expect("write review render");
        fs::write(&render_evidence, "{}\n").expect("write render evidence");

        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let proof_session = cursor_proof_session(
            "blocked_by_stored_proof_integrity_or_identity",
            "fail",
            Some("capture_in_client_render_evidence"),
            1,
        );
        let plan = ClientArtifactRepairPlan {
            source: "test",
            status: "artifact_integrity_failed",
            client: "cursor",
            failure_count: 2,
            suggested_artifact_dir: tmp.path().to_string_lossy().into_owned(),
            suggested_artifact_dir_write_status: "writable".to_string(),
            suggested_artifact_paths: vec![
                ClientArtifactPathSuggestion {
                    artifact_kind: "review_render_report".to_string(),
                    path: review_render.to_string_lossy().into_owned(),
                    intent: "test review render".to_string(),
                },
                ClientArtifactPathSuggestion {
                    artifact_kind: "render_evidence".to_string(),
                    path: render_evidence.to_string_lossy().into_owned(),
                    intent: "test render evidence".to_string(),
                },
            ],
            workspace_fallback_artifact_dir: None,
            workspace_fallback_artifact_paths: Vec::new(),
            workspace_fallback_commands: Vec::new(),
            failed_artifacts: Vec::new(),
            diagnostic_commands: Vec::new(),
            recovery_steps: Vec::new(),
            blocked_claims: Vec::new(),
            trust_boundary: "test",
        };

        let mut row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );
        row.artifact_repair_plan = Some(plan.clone());
        attach_private_app_operator_guidance(&mut row);

        assert_eq!(
            row.operator_next_action_id.as_deref(),
            Some("capture_in_client_render_evidence_from_real_client_ui")
        );
        assert_eq!(row.operator_next_action_label.as_deref(), Some("Capture render evidence"));
        assert!(
            row.operator_next_step
                .as_deref()
                .is_some_and(|step| step.contains("capture_in_client_render_evidence")
                    && step.contains("real Cursor UI")),
            "{:?}",
            row.operator_next_step
        );
        let primary = row.operator_next_command.as_ref().expect("operator command");
        assert!(primary.iter().any(|part| part == "adapter-binding-proof"), "{primary:?}");
        assert!(primary.iter().any(|part| part == "--proof-session"), "{primary:?}");
        assert!(primary.iter().any(|part| part == "--brief"), "{primary:?}");
        assert!(primary.iter().any(|part| part == "--artifact-dir"), "{primary:?}");
        assert!(primary.iter().any(|part| part == &tmp.path().to_string_lossy()), "{primary:?}");
        assert!(!primary.iter().any(|part| part == "--soma-bin"), "{primary:?}");
        assert!(!primary.iter().any(|part| part == "--write-report"), "{primary:?}");
        assert!(!primary.iter().any(|part| part == "--write-render-evidence"), "{primary:?}");

        let step = next_step(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            Some(&plan),
            "available",
            None,
        );
        assert!(step.contains("capture_in_client_render_evidence"), "{step}");
        assert!(step.contains("soma-client-render-proof-prep.sh"), "{step}");
        assert!(step.contains("real Cursor UI"), "{step}");
        assert!(step.contains("--status --json"), "{step}");
    }

    #[test]
    fn artifact_integrity_trigger_hook_step_precedes_render_artifact_repair() {
        let tmp = TempDir::new().expect("tempdir");
        let review_render = tmp.path().join("review-render.json");
        let render_evidence = tmp.path().join("render-evidence.json");
        fs::write(&review_render, "{}\n").expect("write review render");
        fs::write(&render_evidence, "{}\n").expect("write render evidence");

        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let proof_session = cursor_proof_session(
            "blocked_by_stored_proof_integrity_or_identity",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        let plan = ClientArtifactRepairPlan {
            source: "test",
            status: "artifact_integrity_failed",
            client: "cursor",
            failure_count: 2,
            suggested_artifact_dir: tmp.path().to_string_lossy().into_owned(),
            suggested_artifact_dir_write_status: "writable".to_string(),
            suggested_artifact_paths: vec![
                ClientArtifactPathSuggestion {
                    artifact_kind: "review_render_report".to_string(),
                    path: review_render.to_string_lossy().into_owned(),
                    intent: "test review render".to_string(),
                },
                ClientArtifactPathSuggestion {
                    artifact_kind: "render_evidence".to_string(),
                    path: render_evidence.to_string_lossy().into_owned(),
                    intent: "test render evidence".to_string(),
                },
            ],
            workspace_fallback_artifact_dir: None,
            workspace_fallback_artifact_paths: Vec::new(),
            workspace_fallback_commands: Vec::new(),
            failed_artifacts: Vec::new(),
            diagnostic_commands: Vec::new(),
            recovery_steps: Vec::new(),
            blocked_claims: Vec::new(),
            trust_boundary: "test",
        };

        let mut row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );
        row.artifact_repair_plan = Some(plan);
        attach_private_app_operator_guidance(&mut row);

        assert_eq!(
            row.operator_next_action_id.as_deref(),
            Some("trigger_real_private_client_hook_to_write_private_spool_event")
        );
        assert_eq!(row.operator_next_action_label.as_deref(), Some("Trigger real Cursor hook"));
        assert!(
            row.operator_next_step
                .as_deref()
                .is_some_and(|step| step.contains("fresh `cursor_private_lifecycle_hook`")),
            "{:?}",
            row.operator_next_step
        );
        let primary = row.operator_next_command.as_ref().expect("operator command");
        assert!(
            primary.iter().any(|part| part == "tools/soma-client-hook-readiness.sh"),
            "{primary:?}"
        );
        assert!(!primary.iter().any(|part| part == "--write-render-evidence"), "{primary:?}");
        assert!(!primary.iter().any(|part| part == "--render-render-evidence"), "{primary:?}");

        let checklist = build_private_app_release_proof_checklist(std::slice::from_ref(&row))
            .pop()
            .expect("release proof checklist");
        assert_eq!(checklist.next_required_proof_level.as_deref(), Some("observed_app_hook"));
    }

    #[test]
    fn artifact_repair_primary_command_uses_workspace_fallback_when_home_artifacts_unwritable() {
        let tmp = TempDir::new().expect("tempdir");
        let fallback_dir = tmp.path().join(".soma/client-evidence/cursor/soma-bind-test");
        let fallback_review = fallback_dir.join("review-render.json");
        let fallback_render = fallback_dir.join("render-evidence.json");
        let plan = ClientArtifactRepairPlan {
            source: "test",
            status: "artifact_integrity_failed",
            client: "cursor",
            failure_count: 2,
            suggested_artifact_dir: "/Users/example/.soma/client-evidence/cursor/soma-bind-test"
                .to_string(),
            suggested_artifact_dir_write_status: "parent_not_writable".to_string(),
            suggested_artifact_paths: vec![ClientArtifactPathSuggestion {
                artifact_kind: "review_render_report".to_string(),
                path:
                    "/Users/example/.soma/client-evidence/cursor/soma-bind-test/review-render.json"
                        .to_string(),
                intent: "home review render".to_string(),
            }],
            workspace_fallback_artifact_dir: Some(fallback_dir.to_string_lossy().into_owned()),
            workspace_fallback_artifact_paths: vec![
                ClientArtifactPathSuggestion {
                    artifact_kind: "review_render_report".to_string(),
                    path: fallback_review.to_string_lossy().into_owned(),
                    intent: "fallback review render".to_string(),
                },
                ClientArtifactPathSuggestion {
                    artifact_kind: "render_evidence".to_string(),
                    path: fallback_render.to_string_lossy().into_owned(),
                    intent: "fallback render evidence".to_string(),
                },
            ],
            workspace_fallback_commands: Vec::new(),
            failed_artifacts: Vec::new(),
            diagnostic_commands: Vec::new(),
            recovery_steps: Vec::new(),
            blocked_claims: Vec::new(),
            trust_boundary: "test",
        };

        let command =
            artifact_repair_primary_command_for_plan("cursor", &plan).expect("write command");
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--write-report" && pair[1] == fallback_review.to_string_lossy().as_ref()
        }));
        assert!(!command.iter().any(|part| part.contains("/Users/example/.soma/client-evidence")));
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_artifact_failed_binding();
        let proof_session =
            cursor_proof_session("blocked_by_stored_proof_integrity_or_identity", "fail", None, 1);
        let mut row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );
        row.artifact_repair_plan = Some(plan.clone());
        attach_private_app_operator_guidance(&mut row);
        assert_eq!(
            row.operator_next_action_id.as_deref(),
            Some("refresh_invalid_client_binding_artifacts")
        );
        let row_command = row.operator_next_command.as_ref().expect("row operator command");
        assert!(row_command.windows(2).any(|pair| {
            pair[0] == "--write-report" && pair[1] == fallback_review.to_string_lossy().as_ref()
        }));
        assert!(!row_command
            .iter()
            .any(|part| part.contains("/Users/example/.soma/client-evidence")));

        fs::create_dir_all(&fallback_dir).expect("fallback dir");
        fs::write(&fallback_review, "{}\n").expect("fallback review render");
        let command =
            artifact_repair_primary_command_for_plan("cursor", &plan).expect("render template");
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--review-render-report"
                && pair[1] == fallback_review.to_string_lossy().as_ref()
        }));
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--write-render-evidence"
                && pair[1] == fallback_render.to_string_lossy().as_ref()
        }));
        attach_private_app_operator_guidance(&mut row);
        assert_eq!(
            row.operator_next_action_id.as_deref(),
            Some("materialize_render_evidence_packet_for_artifact_repair")
        );
        let row_command = row.operator_next_command.as_ref().expect("row operator command");
        assert!(row_command.windows(2).any(|pair| {
            pair[0] == "--write-render-evidence"
                && pair[1] == fallback_render.to_string_lossy().as_ref()
        }));
        assert!(!row_command
            .iter()
            .any(|part| part.contains("/Users/example/.soma/client-evidence")));

        fs::write(&fallback_render, "{}\n").expect("fallback render evidence");
        let command =
            artifact_repair_primary_command_for_plan("cursor", &plan).expect("proof session");
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--artifact-dir" && pair[1] == fallback_dir.to_string_lossy().as_ref()
        }));
        assert!(!command.iter().any(|part| part.contains("/Users/example/.soma/client-evidence")));
        attach_private_app_operator_guidance(&mut row);
        let row_command = row.operator_next_command.as_ref().expect("row proof session command");
        assert!(row_command.windows(2).any(|pair| {
            pair[0] == "--artifact-dir" && pair[1] == fallback_dir.to_string_lossy().as_ref()
        }));
        assert!(!row_command
            .iter()
            .any(|part| part.contains("/Users/example/.soma/client-evidence")));
    }

    #[test]
    fn dogfood_artifact_command_fallback_matches_snapshot_action_client() {
        let report = ClientDogfoodEvidenceReport {
            source: "soma_clients.dogfood_evidence_report.v1",
            path: "/tmp/client-dogfood.json".to_string(),
            status: "valid",
            generated_at: None,
            generated_at_unix_ms: None,
            artifact_modified_at_unix_ms: None,
            report_status: Some("ready".to_string()),
            private_app_release_proof_status: Some("pending".to_string()),
            private_app_release_proof_ready: Some(false),
            private_app_release_proof_ready_clients: Vec::new(),
            private_app_release_proof_pending_clients: vec!["codex-app".to_string()],
            real_private_app_release_status: Some("pending".to_string()),
            real_private_app_release_ready: Some(false),
            real_private_app_release_ready_clients: Vec::new(),
            real_private_app_release_pending_clients: vec!["codex-app".to_string()],
            real_private_app_release_operator_status: Some("private_app_proof_pending".to_string()),
            real_private_app_release_operator_primary_next_step: None,
            real_private_app_release_operator_primary_next_command: vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--client".to_string(),
                "codex-app".to_string(),
                "--proof-session".to_string(),
                "--brief".to_string(),
            ],
            real_private_app_release_pending_actions: vec![ClientDogfoodPrivateAppSnapshotAction {
                client: "codex-app".to_string(),
                goal_status: Some("private_app_proof_artifact_integrity_failed".to_string()),
                operator_next_action_id: Some(
                    "inspect_render_evidence_packet_for_artifact_repair".to_string(),
                ),
                operator_next_action_label: Some("Inspect render evidence packet".to_string()),
                release_gate_blockers: vec!["artifact_integrity_failed".to_string()],
                missing_proof_levels: Vec::new(),
                has_restart_command: false,
                restart_requires_separate_terminal: None,
                has_collector_start_command: false,
                has_wait_command: false,
                external_action_safety: None,
                trust_boundary: "test",
            }],
            client_mcp_context_capture_status: Some("pass".to_string()),
            semantic_learning_review_status: Some("warn".to_string()),
            multi_terminal_scope_status: Some("pass".to_string()),
            private_client_proof_session_readiness_status: Some("pass".to_string()),
            summary_pass: Some(1),
            summary_warn: Some(0),
            summary_fail: Some(0),
            current_private_app_snapshot_coherence: "filtered_current_scope",
            current_private_app_snapshot_mismatches: Vec::new(),
            error: None,
            trust_boundary: "test",
        };
        let mut index = ClientDogfoodIndex {
            source: "soma_clients.dogfood_index.v1",
            status: "fail",
            objective_count: 1,
            pass_count: 0,
            warning_count: 0,
            fail_count: 1,
            evidence_report_flow_status: "valid",
            evidence_report_flow_summary: "test".to_string(),
            private_app_release_gate_status: "pending".to_string(),
            private_app_release_gate_ready: false,
            private_app_release_gate_ready_clients: Vec::new(),
            private_app_release_gate_pending_clients: vec!["codex-app".to_string()],
            private_app_release_gate_summary: "test".to_string(),
            evidence_report: None,
            project_scope: None,
            objectives: vec![ClientDogfoodObjective {
                objective: "private_app_binding_proof",
                status: "fail",
                summary: "test".to_string(),
                evidence_refs: Vec::new(),
                next_command: vec![
                    "soma".to_string(),
                    "adapter-binding-proof".to_string(),
                    "--client".to_string(),
                    "codex-app".to_string(),
                    "--proof-session".to_string(),
                    "--brief".to_string(),
                    "--artifact-dir".to_string(),
                    "/workspace/.soma/client-evidence/codex-app/soma-bind-test".to_string(),
                ],
                trust_boundary: "test",
            }],
            primary_next_command: Vec::new(),
            trust_boundary: "test",
        };

        let command =
            dogfood_current_private_app_binding_command(&report, &index).expect("matching command");
        assert!(command.windows(2).any(|pair| {
            pair[0] == "--artifact-dir"
                && pair[1] == "/workspace/.soma/client-evidence/codex-app/soma-bind-test"
        }));

        index.objectives[0].next_command[3] = "cursor".to_string();
        assert!(dogfood_current_private_app_binding_command(&report, &index).is_none());
    }

    #[test]
    fn ready_private_client_claim_next_step_takes_priority_over_runtime_hint() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_ready_binding();
        let proof_session = cursor_proof_session("ready_for_private_client_claim", "pass", None, 1);
        let row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );

        assert_eq!(
            row.private_capture_status,
            "real_app_hook_in_client_render_and_review_action_observed"
        );
        assert!(row.ready_for_private_client_claim);
        assert!(row.next_step.contains("private-client proof is ready"), "{}", row.next_step);
        assert!(
            row.next_step.contains("runtime CLI detection is reported separately"),
            "{}",
            row.next_step
        );
        assert!(!row.next_step.contains("Install or expose `cursor`"), "{}", row.next_step);
        assert!(row
            .safe_to_claim
            .iter()
            .any(|claim| claim.contains("Release-grade private-client capture claim is ready")));
        assert!(row.blocked_claims.iter().any(|claim| claim.contains("cursor runtime detection")));
        assert!(row
            .proof_level_statuses
            .iter()
            .all(|proof| { proof.status == "recorded" && !proof.blocks_private_client_claim }));
    }

    #[test]
    fn stored_ready_proofs_without_target_config_do_not_claim_private_client_ready() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let binding = cursor_ready_binding();
        let proof_session = cursor_proof_session(
            "blocked_by_missing_private_target_config",
            "fail",
            Some("install_or_merge_private_client_config"),
            0,
        );
        let row = row_for_client(
            McpClientKind::Cursor,
            &mcp,
            Some(&binding),
            Some(&proof_session),
            "available",
        );

        assert_eq!(
            row.private_capture_status,
            "stored_release_proof_but_private_target_config_missing"
        );
        assert_eq!(row.goal_status, "private_app_target_config_required");
        assert!(!row.ready_for_private_client_claim);
        assert!(row.ready_for_client_operator_loop);
        assert!(
            row.next_step.contains("no known private-client target config"),
            "{}",
            row.next_step
        );
        assert!(row.next_step.contains("--proof-session --json"), "{}", row.next_step);
        assert!(!row
            .safe_to_claim
            .iter()
            .any(|claim| claim.contains("Release-grade private-client capture claim is ready")));
        assert!(row
            .blocked_claims
            .iter()
            .any(|claim| { claim.contains("target config is not currently discoverable") }));
        assert_eq!(row.installed_config_private_target_eligible_candidates, Some(0));
    }

    #[test]
    fn trigger_hook_primary_command_uses_bounded_wait_after_target_config_exists() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        let row =
            row_for_client(McpClientKind::Cursor, &mcp, None, Some(&proof_session), "available");

        assert_eq!(row.goal_status, "private_app_trigger_hook_required");
        assert!(row.private_event_wait_command.as_ref().is_some_and(|command| command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_WAIT_SECONDS=30")));
        assert!(row.simple_private_event_wait_command.as_ref().is_some_and(|command| command
            .windows(2)
            .any(|pair| pair[0] == "--wait-seconds" && pair[1] == "30")));
        let primary = primary_private_app_command(&row);
        assert_eq!(
            primary.first().map(String::as_str),
            Some("tools/soma-client-hook-readiness.sh")
        );
        assert!(primary.windows(2).any(|pair| pair[0] == "--client" && pair[1] == "cursor"));
        assert!(primary
            .windows(2)
            .any(|pair| pair[0] == "--event-jsonl" && pair[1] == "/tmp/events.jsonl"));
        assert!(primary.windows(2).any(|pair| pair[0] == "--wait-seconds" && pair[1] == "30"));
        assert!(!primary.iter().any(|part| part.starts_with("SOMA_CLIENT_BINDING_")));
        assert!(!primary.iter().any(|part| part == "--write-installed-config"));
    }

    #[test]
    fn trigger_hook_temporal_binding_failure_mentions_fresh_event() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let temporal_reason =
            "event_jsonl: event JSONL matching private event observed_at_ns must be at or after installed config modified_at"
                .to_string();
        let mut proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        proof_session.blocking_reasons = vec![temporal_reason.clone()];
        proof_session.stage_blockers = vec![ClientProofSessionStageBlocker {
            proof_level: "observed_app_hook".to_string(),
            ledger_status: "missing".to_string(),
            artifact_status: Some("blocked_by_missing_or_invalid_artifacts".to_string()),
            ready_to_record_now: false,
            blocking_reasons: vec![temporal_reason],
        }];
        let row =
            row_for_client(McpClientKind::Cursor, &mcp, None, Some(&proof_session), "available");

        assert!(row.next_step.contains("latest matching private event is older"));
        assert!(row.next_step.contains("fresh real private client hook"));
        let blockers = private_app_release_gate_blockers(&row);
        assert!(blockers.iter().any(|blocker| blocker == "private_hook_temporal_binding_failed"));
        assert!(!blockers.iter().any(|blocker| blocker == "real_private_hook_event_missing"));
    }

    #[test]
    fn trigger_hook_missing_event_requirements_do_not_claim_stale_event() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let missing_event_source =
            "event_jsonl: event JSONL must include the expected private event_source".to_string();
        let temporal_reason =
            "event_jsonl: event JSONL file modified_at must be at or after installed config modified_at"
                .to_string();
        let mut proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        proof_session.blocking_reasons =
            vec![missing_event_source.clone(), temporal_reason.clone()];
        proof_session.stage_blockers = vec![ClientProofSessionStageBlocker {
            proof_level: "observed_app_hook".to_string(),
            ledger_status: "missing".to_string(),
            artifact_status: Some("blocked_by_missing_or_invalid_artifacts".to_string()),
            ready_to_record_now: false,
            blocking_reasons: vec![missing_event_source, temporal_reason],
        }];
        let row =
            row_for_client(McpClientKind::Cursor, &mcp, None, Some(&proof_session), "available");

        assert!(!row.next_step.contains("latest matching private event is older"));
        assert!(row.next_step.contains("trigger the real private client hook"));
        let blockers = private_app_release_gate_blockers(&row);
        assert!(blockers.iter().any(|blocker| blocker == "real_private_hook_event_missing"));
        assert!(!blockers.iter().any(|blocker| blocker == "private_hook_temporal_binding_failed"));
    }

    #[test]
    fn codex_app_trigger_hook_next_step_mentions_reload_after_notify_patch() {
        let mcp = codex_app_mcp_report();
        let proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        let row =
            row_for_client(McpClientKind::CodexApp, &mcp, None, Some(&proof_session), "available");

        assert_eq!(row.goal_status, "private_app_trigger_hook_required");
        assert!(row.next_step.contains("Installed Codex app binding config is eligible"));
        assert!(
            row.next_step.contains("quit or restart the stale Codex app process"),
            "{}",
            row.next_step
        );
        assert!(row.next_step.contains("complete a real turn"), "{}", row.next_step);
        assert!(
            row.next_step.contains("before any observed_app_hook proof is recorded"),
            "{}",
            row.next_step
        );
        let reload_check =
            row.codex_notify_reload_check.as_ref().expect("codex notify reload check");
        assert!(reload_check.trust_boundary.contains("records no proof row"));
    }

    #[test]
    fn codex_notify_reload_check_flags_stale_desktop_process() {
        let processes = codex_desktop_processes_from_lines(
            &[
                "  111 Thu Jun 18 00:05:14 2026     /Applications/Codex.app/Contents/MacOS/Codex"
                    .to_string(),
                "  222 Sun Jun 21 23:59:59 2026     /bin/zsh -c harmless".to_string(),
            ],
            1_782_049_138,
        );

        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 111);
        assert!(processes[0].started_before_config);
        assert_eq!(processes[0].command, "/Applications/Codex.app/Contents/MacOS/Codex");
    }

    #[test]
    fn missing_private_client_proof_next_step_takes_priority_over_runtime_hint() {
        let mcp = continue_mcp_report_with_missing_runtime();
        let row = row_for_client(McpClientKind::Continue, &mcp, None, None, "available");

        assert_eq!(row.private_capture_status, "missing_client_binding_proof");
        assert!(!row.ready_for_private_client_claim);
        assert!(
            row.next_step.contains("adapter-binding-proof --client continue --proof-session"),
            "{}",
            row.next_step
        );
        assert!(row.next_step.contains("observed_app_hook"), "{}", row.next_step);
        assert!(
            row.next_step.contains("runtime CLI detection is reported separately"),
            "{}",
            row.next_step
        );
        assert!(!row.next_step.contains("Install or expose `continue`"), "{}", row.next_step);
        assert!(row
            .blocked_claims
            .iter()
            .any(|claim| claim.contains("continue runtime detection")));
        assert!(row.blocked_claims.iter().any(|claim| claim.contains("Automatic private capture")));
        assert!(row
            .proof_level_statuses
            .iter()
            .all(|proof| { proof.status == "missing" && proof.blocks_private_client_claim }));
    }

    #[test]
    fn hook_readiness_fallback_keeps_event_contract_context() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        let row =
            row_for_client(McpClientKind::Cursor, &mcp, None, Some(&proof_session), "available");

        let command = row
            .next_commands
            .iter()
            .find(|command| {
                command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh")
            })
            .expect("hook readiness command");
        assert!(command.iter().any(|part| part == "SOMA_CLIENT_BINDING_CLIENT=cursor"));
        assert!(command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_EVENT_SOURCE=cursor_private_lifecycle_hook"));
        assert!(command.iter().any(|part| part == "SOMA_CLIENT_BINDING_NONCE=soma-bind-test"));
        assert!(command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_EVENT_JSONL=/tmp/events.jsonl"));
        assert!(row.private_event_wait_command.as_ref().is_some_and(|command| command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_WAIT_SECONDS=30")));
    }

    #[test]
    fn hook_readiness_next_command_is_enriched_with_event_contract_context() {
        let mcp = continue_mcp_report_with_missing_runtime();
        let mut proof_session = cursor_proof_session(
            "blocked_by_missing_or_invalid_artifacts",
            "fail",
            Some("trigger_private_client_hook"),
            1,
        );
        proof_session.expected_event_source = Some("continue_private_lifecycle_hook".to_string());
        proof_session.binding_nonce = Some("soma-bind-continue".to_string());
        proof_session.next_command = Some(vec![
            "env".to_string(),
            "SOMA_CLIENT_BINDING_CLIENT=continue".to_string(),
            "SOMA_CLIENT_BINDING_MANIFEST=tools/client-bindings/continue-soma-binding.json.example"
                .to_string(),
            "SOMA_CLIENT_BINDING_EVENT_JSONL=/tmp/events.jsonl".to_string(),
            "tools/soma-client-hook-readiness.sh".to_string(),
        ]);

        let row =
            row_for_client(McpClientKind::Continue, &mcp, None, Some(&proof_session), "available");

        let proof_session_next_command =
            row.proof_session_next_command.as_ref().expect("proof session next command");
        assert!(
            proof_session_next_command
                .iter()
                .any(|part| part
                    == "SOMA_CLIENT_BINDING_EVENT_SOURCE=continue_private_lifecycle_hook")
        );
        assert!(proof_session_next_command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_NONCE=soma-bind-continue"));
        let command = row
            .next_commands
            .iter()
            .find(|command| {
                command.iter().any(|part| part == "tools/soma-client-hook-readiness.sh")
            })
            .expect("hook readiness command");
        assert!(
            command
                .iter()
                .any(|part| part
                    == "SOMA_CLIENT_BINDING_EVENT_SOURCE=continue_private_lifecycle_hook")
        );
        assert!(command.iter().any(|part| part == "SOMA_CLIENT_BINDING_NONCE=soma-bind-continue"));
        assert!(command.iter().any(|part| {
            part == "SOMA_CLIENT_BINDING_MANIFEST=tools/client-bindings/continue-soma-binding.json.example"
        }));
        let wait_command =
            row.private_event_wait_command.as_ref().expect("private event wait command");
        assert!(
            wait_command
                .iter()
                .any(|part| part
                    == "SOMA_CLIENT_BINDING_EVENT_SOURCE=continue_private_lifecycle_hook")
        );
        assert!(wait_command
            .iter()
            .any(|part| part == "SOMA_CLIENT_BINDING_NONCE=soma-bind-continue"));
        assert!(wait_command.iter().any(|part| part == "SOMA_CLIENT_BINDING_WAIT_SECONDS=30"));
    }

    #[test]
    fn proof_storage_unavailable_next_step_takes_priority_over_runtime_hint() {
        let mcp = cursor_mcp_report_with_missing_runtime();
        let row = row_for_client(McpClientKind::Cursor, &mcp, None, None, "unavailable");

        assert_eq!(row.private_capture_status, "client_binding_proof_storage_unavailable");
        assert!(row.next_step.contains("Grant SOMA read access"), "{}", row.next_step);
        assert!(!row.next_step.contains("Install or expose `cursor`"), "{}", row.next_step);
        assert!(row
            .blocked_claims
            .iter()
            .any(|claim| claim.contains("proof storage is unreadable")));
    }
}
