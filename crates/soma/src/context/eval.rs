//! ContextEnvelope quality evaluation helpers.
//!
//! These helpers are intentionally small and diagnostic. They do not change the
//! default MCP path; they make optional context quality modules measurable
//! against the envelope fields they claim to improve.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::context::envelope::{
    build_context_envelope, ContextEnvelope, ContextScope, ContextSection, EvidenceRef,
};
use crate::context::latent_predictor::{
    render_latent_interface_packet, LatentInterfacePacketInput, DEFAULT_LATENT_PREDICTOR_LIMIT,
    DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE, DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
    LATENT_INTERFACE_PACKET_SCHEMA,
};
use crate::context::pack::{build_memory_pack, BackendKind, PackConfig, PackError};
use crate::context::review::{build_review_queue, ReviewQueueInput, ReviewRenderPlan};
use crate::storage::{
    secret_like_projection_findings, task_frame_retention_cutoff_ns, ClaimSourceType, EpisodeId,
    LearningCriticAction, LearningCriticProposalStatus, LifecycleState, SensitivityLabel, Storage,
    StorageError, StoredTaskFrame, TaskFrameRetentionRequest,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextEnvelopeAudit {
    pub section_count: usize,
    pub evidence_backed_count: usize,
    pub top_level_evidence_count: usize,
    pub relevant_memory_layer_values: Vec<String>,
    pub legacy_memory_tier_projection_layers: Vec<String>,
    pub memory_tier_compatibility_passed: bool,
    pub missing_section_evidence: Vec<String>,
    pub missing_top_level_evidence: Vec<String>,
    pub stable_fact_count: usize,
    pub stable_facts_with_claim_evidence: usize,
    pub stable_facts_missing_claim_evidence: Vec<String>,
    pub passed: bool,
}

impl ContextEnvelopeAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

/// Audit the cloud-facing ContextEnvelope evidence contract.
///
/// This keeps the "learning = state transition + evidence projection" rule
/// measurable at the exact boundary a cloud LLM consumes. Every projected
/// section/item must cite evidence that is also present in the top-level
/// `evidence` ledger, and L4 `stable_facts` must be backed by a claim record.
pub fn audit_context_envelope(envelope: &ContextEnvelope) -> ContextEnvelopeAudit {
    let top_level_evidence: BTreeSet<String> = envelope.evidence.iter().map(evidence_tag).collect();
    let mut audit = ContextEnvelopeAudit {
        section_count: 0,
        evidence_backed_count: 0,
        top_level_evidence_count: top_level_evidence.len(),
        relevant_memory_layer_values: envelope
            .relevant_memory
            .iter()
            .map(|item| item.layer.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        legacy_memory_tier_projection_layers: Vec::new(),
        memory_tier_compatibility_passed: false,
        missing_section_evidence: Vec::new(),
        missing_top_level_evidence: Vec::new(),
        stable_fact_count: envelope.stable_facts.len(),
        stable_facts_with_claim_evidence: 0,
        stable_facts_missing_claim_evidence: Vec::new(),
        passed: false,
    };

    if let Some(section) = &envelope.thread_state {
        audit_context_section("thread_state", section, &top_level_evidence, &mut audit);
    }
    audit_context_sections(
        "compiler_notes",
        &envelope.compiler_notes,
        &top_level_evidence,
        &mut audit,
    );
    audit_context_sections(
        "short_term_candidates",
        &envelope.short_term_candidates,
        &top_level_evidence,
        &mut audit,
    );
    audit_context_sections(
        "project_experience",
        &envelope.project_experience,
        &top_level_evidence,
        &mut audit,
    );
    for (idx, item) in envelope.relevant_memory.iter().enumerate() {
        let label = format!("relevant_memory[{idx}]");
        audit_evidence_refs(&label, &item.evidence, &top_level_evidence, &mut audit);
        if is_legacy_memory_tier_projection_layer(&item.layer) {
            audit.legacy_memory_tier_projection_layers.push(format!("{label}:{}", item.layer));
        }
    }
    audit_context_sections("stable_facts", &envelope.stable_facts, &top_level_evidence, &mut audit);
    for (idx, section) in envelope.stable_facts.iter().enumerate() {
        if section.evidence.iter().any(|evidence| evidence.kind == "claim") {
            audit.stable_facts_with_claim_evidence += 1;
        } else {
            audit.stable_facts_missing_claim_evidence.push(format!("stable_facts[{idx}]"));
        }
    }
    audit_context_sections("user_policy", &envelope.user_policy, &top_level_evidence, &mut audit);
    audit_context_sections(
        "open_decisions",
        &envelope.open_decisions,
        &top_level_evidence,
        &mut audit,
    );
    audit_context_sections("corrections", &envelope.corrections, &top_level_evidence, &mut audit);

    audit.memory_tier_compatibility_passed = audit.legacy_memory_tier_projection_layers.is_empty();
    audit.passed = audit.missing_section_evidence.is_empty()
        && audit.missing_top_level_evidence.is_empty()
        && audit.stable_facts_missing_claim_evidence.is_empty()
        && audit.memory_tier_compatibility_passed;
    audit
}

fn is_legacy_memory_tier_projection_layer(layer: &str) -> bool {
    matches!(layer, "short" | "mid" | "long" | "archive")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskFrameProjectionAudit {
    pub task_frame_id: i64,
    pub projection_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_policy_explicit_reason: Option<String>,
    pub allowed_sensitivity_labels: Vec<String>,
    pub projected_field_count: usize,
    pub blocked_field_count: usize,
    pub invalid_cloud_projection: bool,
    pub has_privacy_labels: bool,
    pub leaked_blocked_fields: Vec<String>,
    pub missing_projected_privacy_labels: Vec<String>,
    pub missing_canonical_privacy_labels: Vec<String>,
    pub mismatched_projected_privacy_labels: Vec<String>,
    pub unsafe_projected_labels: Vec<String>,
    pub disallowed_projected_labels: Vec<String>,
    pub secret_like_projected_values: Vec<String>,
    pub passed: bool,
}

impl TaskFrameProjectionAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

/// Audit a persisted TaskFrame cloud projection.
///
/// This is the privacy half of the local-control-plane boundary: blocked
/// fields must not appear in `cloud_redacted_json`, every projected field must
/// carry a privacy label, and unsafe labels (`unknown`, `secret`, `never_send`)
/// must not be projected or relabeled into something safer.
pub fn audit_task_frame_projection(task_frame: &StoredTaskFrame) -> TaskFrameProjectionAudit {
    let mut audit = TaskFrameProjectionAudit {
        task_frame_id: task_frame.id,
        projection_policy: task_frame.projection_policy.name().to_string(),
        projection_policy_explicit_reason: task_frame.projection_policy.explicit_reason.clone(),
        allowed_sensitivity_labels: task_frame
            .projection_policy
            .allowed_sensitivity_labels()
            .into_iter()
            .map(str::to_string)
            .collect(),
        projected_field_count: 0,
        blocked_field_count: task_frame.blocked_fields.len(),
        invalid_cloud_projection: false,
        has_privacy_labels: false,
        leaked_blocked_fields: Vec::new(),
        missing_projected_privacy_labels: Vec::new(),
        missing_canonical_privacy_labels: Vec::new(),
        mismatched_projected_privacy_labels: Vec::new(),
        unsafe_projected_labels: Vec::new(),
        disallowed_projected_labels: Vec::new(),
        secret_like_projected_values: secret_like_projection_findings(
            &task_frame.cloud_redacted_json,
        ),
        passed: false,
    };

    let Some(cloud) = task_frame.cloud_redacted_json.as_object() else {
        audit.invalid_cloud_projection = true;
        return audit;
    };
    let projected_labels = cloud.get("privacy_labels").and_then(Value::as_object);
    audit.has_privacy_labels = projected_labels.is_some();

    for field in &task_frame.blocked_fields {
        if cloud.contains_key(field) {
            audit.leaked_blocked_fields.push(field.clone());
        }
        if projected_labels.is_some_and(|labels| labels.contains_key(field)) {
            audit.leaked_blocked_fields.push(format!("{field}:privacy_label"));
        }
    }

    for field in cloud.keys().filter(|field| field.as_str() != "privacy_labels") {
        audit.projected_field_count += 1;
        let projected_label =
            projected_labels.and_then(|labels| labels.get(field)).and_then(parse_sensitivity_label);
        let canonical_label = task_frame.privacy_labels.get(field).copied();

        match projected_label {
            Some(label) if unsafe_for_cloud_projection(label) => {
                audit.unsafe_projected_labels.push(format!("{field}:projected:{}", label.as_str()));
            }
            Some(label) if !label.can_project(&task_frame.projection_policy) => {
                audit
                    .disallowed_projected_labels
                    .push(format!("{field}:projected:{}", label.as_str()));
            }
            Some(_) => {}
            None => audit.missing_projected_privacy_labels.push(field.clone()),
        }

        match canonical_label {
            Some(label) if unsafe_for_cloud_projection(label) => {
                audit.unsafe_projected_labels.push(format!("{field}:canonical:{}", label.as_str()));
            }
            Some(label) if !label.can_project(&task_frame.projection_policy) => {
                audit
                    .disallowed_projected_labels
                    .push(format!("{field}:canonical:{}", label.as_str()));
            }
            Some(_) => {}
            None => audit.missing_canonical_privacy_labels.push(field.clone()),
        }

        if let (Some(projected), Some(canonical)) = (projected_label, canonical_label) {
            if projected != canonical {
                audit.mismatched_projected_privacy_labels.push(format!(
                    "{field}:projected={} canonical={}",
                    projected.as_str(),
                    canonical.as_str()
                ));
            }
        }
    }

    audit.passed = !audit.invalid_cloud_projection
        && audit.has_privacy_labels
        && audit.leaked_blocked_fields.is_empty()
        && audit.missing_projected_privacy_labels.is_empty()
        && audit.missing_canonical_privacy_labels.is_empty()
        && audit.mismatched_projected_privacy_labels.is_empty()
        && audit.unsafe_projected_labels.is_empty()
        && audit.disallowed_projected_labels.is_empty()
        && audit.secret_like_projected_values.is_empty();
    audit
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustBoundaryStorageAudit {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub checked_claim_count: usize,
    pub checked_proposal_count: usize,
    pub unverified_cloud_draft_count: usize,
    pub promoted_cloud_draft_count: usize,
    pub promoted_cloud_draft_without_trust_claim_ids: Vec<i64>,
    pub semantic_fact_count: usize,
    pub semantic_fact_without_trust_claim_ids: Vec<i64>,
    pub semantic_fact_missing_promotion_reason_claim_ids: Vec<i64>,
    pub applied_promotion_proposal_count: usize,
    pub applied_promotion_proposals_missing_trust: Vec<TrustBoundaryProposalViolation>,
    pub passed: bool,
}

impl TrustBoundaryStorageAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustBoundaryProposalViolation {
    pub proposal_id: i64,
    pub missing_trust_claim_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBindingHardeningAudit {
    pub client: Option<String>,
    pub required_clients: Vec<String>,
    pub required_client_proof_matrix: Vec<RequiredClientProofMatrixRow>,
    pub proof_session_source: String,
    pub proof_session_runbook_source: String,
    pub proof_session_runbook_schema: String,
    pub proof_session_runbook_required: bool,
    pub proof_session_runbook_next_step_id: Option<String>,
    pub proof_session_status: String,
    pub proof_session_release_gate: String,
    pub proof_session_next_step_id: Option<String>,
    pub proof_session_target_clients: Vec<String>,
    pub proof_session_config_root_probe_hint: Option<ClientBindingConfigRootProbeHint>,
    pub required_client_count: usize,
    pub required_ready_client_count: usize,
    pub missing_required_clients: Vec<String>,
    pub unready_required_clients: Vec<String>,
    pub proof_limit: usize,
    pub proofs_found: usize,
    pub client_count: usize,
    pub ready_client_count: usize,
    pub all_latest_artifacts_verified: bool,
    pub artifact_failure_count: usize,
    pub coherence_failure_count: usize,
    pub non_release_evidence_source_count: usize,
    pub non_release_proof_levels: Vec<String>,
    pub primary_readiness: Option<String>,
    pub primary_coherence_failures: Vec<String>,
    pub primary_non_release_evidence_sources: Vec<String>,
    pub readiness_values: Vec<String>,
}

impl ClientBindingHardeningAudit {
    pub fn has_artifact_failure(&self) -> bool {
        !self.all_latest_artifacts_verified || self.artifact_failure_count > 0
    }

    pub fn required_scope_artifact_failure_count(&self) -> usize {
        if self.required_clients.is_empty() {
            self.artifact_failure_count
        } else {
            self.required_client_proof_matrix
                .iter()
                .filter(|row| row.required_by_release)
                .map(|row| row.artifact_failure_count)
                .sum()
        }
    }

    pub fn required_scope_coherence_failure_count(&self) -> usize {
        if self.required_clients.is_empty() {
            self.coherence_failure_count
        } else {
            self.required_client_proof_matrix
                .iter()
                .filter(|row| row.required_by_release)
                .map(|row| row.coherence_failure_count)
                .sum()
        }
    }

    pub fn required_scope_has_artifact_or_identity_failure(&self) -> bool {
        if self.required_clients.is_empty() {
            self.has_artifact_failure() || self.coherence_failure_count > 0
        } else {
            self.required_client_proof_matrix.iter().any(|row| {
                row.required_by_release
                    && (row.artifact_failure_count > 0 || row.coherence_failure_count > 0)
            })
        }
    }

    pub fn has_ready_client(&self) -> bool {
        self.ready_client_count > 0
    }

    pub fn required_clients_ready(&self) -> bool {
        self.required_clients.is_empty()
            || (self.missing_required_clients.is_empty()
                && self.unready_required_clients.is_empty()
                && self.required_ready_client_count == self.required_client_count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientBindingHardeningClientSnapshot {
    pub client: String,
    pub readiness: String,
    pub ready_for_private_client_claim: bool,
    pub has_observed_app_hook: bool,
    pub has_observed_in_client_render: bool,
    pub has_observed_review_action: bool,
    pub artifact_failure_count: usize,
    pub artifact_failures: Vec<ProductHardeningEvidenceArtifactFailure>,
    pub coherence_failure_count: usize,
    pub non_release_evidence_source_count: usize,
    pub non_release_proof_levels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductHardeningEvidenceArtifactFailure {
    pub proof_id: i64,
    pub proof_level: String,
    pub kind: String,
    pub path: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductHardeningArtifactReplayRepairAction {
    pub source: &'static str,
    pub proof_id: i64,
    pub client: String,
    pub proof_level: String,
    pub artifact_kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_path: Option<String>,
    pub intent: String,
    pub command: Vec<String>,
    pub records_proof: bool,
    pub requires_operator_action: bool,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequiredClientProofMatrixRow {
    pub client: String,
    pub required_by_release: bool,
    pub status: String,
    pub release_gate: String,
    pub readiness: Option<String>,
    pub ready_for_private_client_claim: bool,
    pub completed_proof_levels: Vec<String>,
    pub missing_proof_levels: Vec<String>,
    pub non_release_proof_levels: Vec<String>,
    pub artifact_failure_count: usize,
    pub coherence_failure_count: usize,
    pub artifact_replay_failures: Vec<ProductHardeningEvidenceArtifactFailure>,
    pub artifact_replay_repair_actions: Vec<ProductHardeningArtifactReplayRepairAction>,
    pub artifact_replay_recovery_commands: Vec<Vec<String>>,
    pub next_step_id: Option<String>,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub proof_session_required: bool,
    pub proof_session_cli: String,
    pub proof_session_mcp_tool: String,
    pub proof_session_mcp_arguments: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_trust_boundary: Option<String>,
    pub config_root_probe_hint: Option<ClientBindingConfigRootProbeHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_evidence_artifact_scan: Option<ProductHardeningRenderEvidenceArtifactScan>,
    pub runbook_schema: String,
    pub external_proof_requirements: Vec<ClientBindingExternalProofRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_action: Option<ProductHardeningExternalOperatorAction>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProductHardeningRenderEvidenceArtifactScan {
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
pub struct ProductHardeningExternalActionSafety {
    pub source: String,
    pub classification: String,
    pub requires_operator_confirmation_before_submission: bool,
    pub may_transmit_prompt_to_provider: bool,
    pub suggested_minimal_test_prompt: String,
    pub forbidden_inputs: Vec<String>,
    pub reason: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProductHardeningExternalOperatorAction {
    pub source: String,
    pub client: String,
    pub action_id: String,
    pub action_label: String,
    pub action_kind: String,
    pub proof_session_step_id: String,
    pub required_operator_action: String,
    pub requires_operator_confirmation_before_submission: bool,
    pub may_transmit_prompt_to_provider: bool,
    pub suggested_minimal_test_prompt: String,
    pub forbidden_inputs: Vec<String>,
    pub proof_session_cli: String,
    pub readiness_probe_command: Vec<String>,
    pub proof_after_success_step_id: String,
    pub required_observation: Vec<String>,
    pub records_proof: bool,
    pub creates_verification_event: bool,
    pub promotes_cloud_draft: bool,
    pub why_next_mcp_call_is_null: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBindingConfigRootProbeHint {
    pub status: String,
    pub config_root: Option<String>,
    pub cli_flag: String,
    pub hardening_report_cli: String,
    pub proof_session_cli: String,
    pub mcp_argument: String,
    pub reason: String,
    pub trust_boundary: String,
}

pub fn client_binding_config_root_probe_hint(
    client: Option<&str>,
    config_root: Option<&str>,
) -> ClientBindingConfigRootProbeHint {
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    let client = client.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("<client>");
    let config_root = config_root.map(str::trim).filter(|value| !value.is_empty());
    let config_root_arg = config_root.unwrap_or("<config-root>");
    let required_client_arg = format!("--required-client {client}");
    let status = if config_root.is_some() { "probed" } else { "not_probed" };
    let reason = if config_root.is_some() {
        "installed client config artifacts were replay-probed from this config root; proof rows remain unchanged unless external evidence is recorded"
    } else {
        "without client_binding_config_root, hardening only inspects stored proof rows and cannot discover installed config artifacts or advance the proof-session blocker"
    };

    ClientBindingConfigRootProbeHint {
        status: status.to_string(),
        config_root: config_root.map(str::to_string),
        cli_flag: "--client-binding-config-root <config-root>".to_string(),
        hardening_report_cli: format!(
            "{soma_bin} context hardening-report --require-client-binding-ready {required_client_arg} --client-binding-config-root {config_root_arg}"
        ),
        proof_session_cli: format!(
            "{soma_bin} adapter-binding-proof --client {client} --proof-session --config-root {config_root_arg}"
        ),
        mcp_argument: "client_binding_config_root".to_string(),
        reason: reason.to_string(),
        trust_boundary: "config_root_probe_is_read_only: discovers and replays installed client binding artifacts only to refine the operator next step; it records no proof row, creates no verification event, promotes no cloud draft, and does not claim real private-client behavior without external evidence".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientBindingExternalProofRequirement {
    pub proof_level: String,
    pub evidence_source: String,
    pub evidence_source_replacement_required: bool,
    pub release_grade_evidence_source_template: String,
    pub required_external_evidence: Vec<String>,
    pub record_mcp_tool: String,
    pub record_mcp_arguments: BTreeMap<String, Value>,
    pub operator_confirmation_key: Option<String>,
    pub release_grade_confirmation_key: String,
    pub release_evidence_source_policy_schema: String,
    pub trust_boundary: String,
}

pub fn build_required_client_proof_matrix(
    requested_client: Option<&str>,
    required_clients: &[String],
    client_snapshots: &[ClientBindingHardeningClientSnapshot],
) -> Vec<RequiredClientProofMatrixRow> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for client in required_clients {
        if seen.insert(client.clone()) {
            targets.push(client.clone());
        }
    }
    if targets.is_empty() {
        if let Some(client) = requested_client.map(str::trim).filter(|client| !client.is_empty()) {
            let client = client.to_ascii_lowercase();
            if seen.insert(client.clone()) {
                targets.push(client);
            }
        }
    }
    if targets.is_empty() {
        for snapshot in client_snapshots {
            if seen.insert(snapshot.client.clone()) {
                targets.push(snapshot.client.clone());
            }
        }
    }

    targets
        .into_iter()
        .map(|client| {
            let snapshot = client_snapshots.iter().find(|snapshot| snapshot.client == client);
            required_client_proof_matrix_row(&client, required_clients, snapshot)
        })
        .collect()
}

fn required_client_proof_matrix_row(
    client: &str,
    required_clients: &[String],
    snapshot: Option<&ClientBindingHardeningClientSnapshot>,
) -> RequiredClientProofMatrixRow {
    let required_by_release = required_clients.iter().any(|required| required == client);
    let mut completed_proof_levels = Vec::new();
    let mut missing_proof_levels = Vec::new();
    for (proof_level, present) in [
        ("observed_app_hook", snapshot.is_some_and(|snapshot| snapshot.has_observed_app_hook)),
        (
            "observed_in_client_render",
            snapshot.is_some_and(|snapshot| snapshot.has_observed_in_client_render),
        ),
        (
            "observed_review_action",
            snapshot.is_some_and(|snapshot| snapshot.has_observed_review_action),
        ),
    ] {
        if present {
            completed_proof_levels.push(proof_level.to_string());
        } else {
            missing_proof_levels.push(proof_level.to_string());
        }
    }

    let artifact_failure_count = snapshot.map_or(0, |snapshot| snapshot.artifact_failure_count);
    let artifact_replay_failures =
        snapshot.map_or_else(Vec::new, |snapshot| snapshot.artifact_failures.clone());
    let coherence_failure_count = snapshot.map_or(0, |snapshot| snapshot.coherence_failure_count);
    let non_release_proof_levels =
        snapshot.map_or_else(Vec::new, |snapshot| snapshot.non_release_proof_levels.clone());
    let ready_for_private_client_claim =
        snapshot.is_some_and(|snapshot| snapshot.ready_for_private_client_claim);
    let status = if snapshot.is_none() {
        "missing"
    } else if ready_for_private_client_claim {
        "ready"
    } else if artifact_failure_count > 0 || coherence_failure_count > 0 {
        "blocked_by_artifact_or_identity"
    } else if snapshot.is_some_and(|snapshot| snapshot.non_release_evidence_source_count > 0) {
        "blocked_by_non_release_evidence_source"
    } else {
        "unready"
    };
    let next_step_id = match status {
        "ready" => None,
        "blocked_by_artifact_or_identity" => Some("verify_evidence_artifacts_and_status"),
        "blocked_by_non_release_evidence_source" => {
            Some("record_release_grade_private_client_proof")
        }
        _ => Some("render_client_binding_proof_session"),
    }
    .map(str::to_string);
    let operator_next_action_id =
        required_client_operator_next_action_id(status, next_step_id.as_deref());
    let operator_next_action_label =
        required_client_operator_next_action_label(&operator_next_action_id, client);
    let proof_session_required = status != "ready";
    let external_requirement_levels: Vec<String> =
        if artifact_failure_count > 0 || coherence_failure_count > 0 {
            vec![
                "observed_app_hook".to_string(),
                "observed_in_client_render".to_string(),
                "observed_review_action".to_string(),
            ]
        } else if !non_release_proof_levels.is_empty() {
            non_release_proof_levels.clone()
        } else {
            missing_proof_levels.clone()
        };
    let external_proof_requirements = external_requirement_levels
        .iter()
        .map(|proof_level| client_binding_external_proof_requirement(client, proof_level))
        .collect();
    let external_action_safety =
        product_hardening_external_action_safety(client, &operator_next_action_id);
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    let proof_session_cli =
        format!("{soma_bin} adapter-binding-proof --client {client} --proof-session");
    let external_action = product_hardening_external_operator_action(
        client,
        &operator_next_action_id,
        &operator_next_action_label,
        next_step_id.as_deref(),
        &proof_session_cli,
        external_action_safety.as_ref(),
    );
    let artifact_replay_repair_actions =
        product_hardening_artifact_replay_repair_actions(client, &artifact_replay_failures);
    let artifact_replay_recovery_commands = dedup_product_hardening_command_lists(
        artifact_replay_repair_actions.iter().map(|action| action.command.clone()).collect(),
    );

    RequiredClientProofMatrixRow {
        client: client.to_string(),
        required_by_release,
        status: status.to_string(),
        release_gate: if ready_for_private_client_claim { "pass" } else { "fail" }.to_string(),
        readiness: snapshot.map(|snapshot| snapshot.readiness.clone()),
        ready_for_private_client_claim,
        completed_proof_levels,
        missing_proof_levels,
        non_release_proof_levels,
        artifact_failure_count,
        coherence_failure_count,
        artifact_replay_failures,
        artifact_replay_repair_actions,
        artifact_replay_recovery_commands,
        next_step_id,
        operator_next_action_id,
        operator_next_action_label,
        proof_session_required,
        proof_session_cli,
        proof_session_mcp_tool: "soma_client_binding_proof_session".to_string(),
        proof_session_mcp_arguments: BTreeMap::from([("client".to_string(), client.to_string())]),
        proof_session_next_command: None,
        proof_session_next_step_id: None,
        proof_session_next_mcp_tool: None,
        proof_session_next_mcp_arguments: None,
        proof_session_next_mcp_trust_boundary: None,
        config_root_probe_hint: if proof_session_required {
            Some(client_binding_config_root_probe_hint(Some(client), None))
        } else {
            None
        },
        render_evidence_artifact_scan: None,
        runbook_schema: "soma.client_binding_proof_session_runbook.v1".to_string(),
        external_proof_requirements,
        external_action_safety,
        external_action,
        trust_boundary: "required_client_proof_matrix_is_read_only: derives per-client readiness from stored proof rows and artifact/coherence status only; artifact replay repair actions are proof-free handoffs back to proof-session runbooks and record no proof row; records no verification event, promotes no cloud draft, applies no proposal, and does not prove private client behavior beyond cited ledger evidence".to_string(),
    }
}

fn dedup_product_hardening_command_lists(commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for command in commands {
        if !out.iter().any(|existing| existing == &command) {
            out.push(command);
        }
    }
    out
}

fn product_hardening_artifact_replay_repair_actions(
    client: &str,
    failures: &[ProductHardeningEvidenceArtifactFailure],
) -> Vec<ProductHardeningArtifactReplayRepairAction> {
    failures
        .iter()
        .map(|failure| ProductHardeningArtifactReplayRepairAction {
            source: "soma_product_hardening.artifact_replay_repair_action.v1",
            proof_id: failure.proof_id,
            client: client.to_string(),
            proof_level: failure.proof_level.clone(),
            artifact_kind: failure.kind.clone(),
            status: failure.status.clone(),
            stale_path: failure.path.clone(),
            intent: product_hardening_artifact_replay_repair_intent(
                failure.proof_level.as_str(),
                failure.kind.as_str(),
                failure.status.as_str(),
            )
            .to_string(),
            command: vec![
                "soma".to_string(),
                "adapter-binding-proof".to_string(),
                "--proof-session".to_string(),
                "--client".to_string(),
                client.to_string(),
                "--json".to_string(),
            ],
            records_proof: false,
            requires_operator_action: product_hardening_artifact_replay_requires_operator_action(
                failure.proof_level.as_str(),
                failure.kind.as_str(),
            ),
            trust_boundary: "product_hardening_artifact_replay_repair_action_is_read_only: it mirrors stale or changed proof artifacts into the release matrix and points back to the proof-session runbook; it records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and cannot replace fresh private-client evidence plus explicit operator confirmation".to_string(),
        })
        .collect()
}

fn product_hardening_artifact_replay_requires_operator_action(
    proof_level: &str,
    kind: &str,
) -> bool {
    matches!(
        (proof_level, kind),
        ("observed_app_hook", "event_jsonl")
            | ("observed_app_hook", "installed_config")
            | ("observed_in_client_render", "render_evidence")
            | ("observed_review_action", "review_action_report")
    )
}

fn product_hardening_artifact_replay_repair_intent(
    proof_level: &str,
    kind: &str,
    status: &str,
) -> &'static str {
    match (proof_level, kind, status) {
        ("observed_in_client_render", "render_evidence", _) => {
            "Re-capture a structured soma.in_client_render_evidence.v1 artifact after the private client visibly renders the current review surface, then re-record observed_in_client_render with explicit operator confirmation."
        }
        ("observed_review_action", "review_action_report", _) => {
            "Execute a currently rendered review control with non-cloud user/tool/local/correction evidence, save the storage-gated review-action report, then re-record observed_review_action with explicit operator confirmation."
        }
        ("observed_app_hook", "event_jsonl", _) => {
            "Trigger the real private client hook again, drain the adapter spool, then re-record observed_app_hook only after matching event_source, binding_nonce, writer metadata, temporal binding, and explicit operator confirmation."
        }
        ("observed_app_hook", "installed_config", _) => {
            "Re-check or reinstall the private client binding config before recording app-hook/render/review-action proof against a fresh installed-config artifact."
        }
        (_, "manifest", "changed") => {
            "Inspect the changed binding manifest and re-record reference or stronger proof only after confirming the current manifest is the intended contract."
        }
        (_, "manifest", _) => {
            "Restore or locate the binding manifest, then rerun the proof-session before claiming client readiness."
        }
        (_, "event_jsonl", "changed") => {
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

pub fn attach_required_client_render_evidence_artifact_scan(
    row: &mut RequiredClientProofMatrixRow,
    path: Option<String>,
) {
    if !required_client_row_should_scan_render_evidence(row) {
        return;
    }
    row.render_evidence_artifact_scan =
        Some(scan_product_hardening_render_evidence_artifact(&row.client, path));
}

fn required_client_row_should_scan_render_evidence(row: &RequiredClientProofMatrixRow) -> bool {
    row.artifact_failure_count > 0
        || matches!(
            row.next_step_id.as_deref(),
            Some("capture_in_client_render_evidence")
                | Some("record_observed_in_client_render")
                | Some("execute_rendered_review_control")
                | Some("record_observed_review_action")
        )
}

pub fn scan_product_hardening_render_evidence_artifact(
    client: &str,
    path: Option<String>,
) -> ProductHardeningRenderEvidenceArtifactScan {
    let Some(path) = path else {
        return product_hardening_render_evidence_scan_with_missing(
            None,
            "missing_path",
            0,
            vec!["render_evidence_path_required"],
        );
    };
    let artifact_path = Path::new(&path);
    if !artifact_path.exists() {
        return product_hardening_render_evidence_scan_with_missing(
            Some(path),
            "missing_file",
            0,
            vec!["render_evidence_file_required"],
        );
    }
    let raw = match fs::read_to_string(artifact_path) {
        Ok(raw) => raw,
        Err(_) => {
            return product_hardening_render_evidence_scan_with_missing(
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
            return product_hardening_render_evidence_scan_with_missing(
                Some(path),
                "invalid_json",
                0,
                vec!["render_evidence_must_be_json"],
            );
        }
    };
    let placeholder_count = product_hardening_value_template_placeholder_count(&value);
    let mut missing = product_hardening_render_evidence_missing_requirements(client, &value);
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
    product_hardening_render_evidence_scan_with_missing(
        Some(path),
        status,
        placeholder_count,
        missing,
    )
}

fn product_hardening_render_evidence_scan_with_missing(
    path: Option<String>,
    status: &'static str,
    placeholder_count: usize,
    missing_requirements: Vec<impl Into<String>>,
) -> ProductHardeningRenderEvidenceArtifactScan {
    ProductHardeningRenderEvidenceArtifactScan {
        source: "soma_product_hardening.render_evidence_artifact_scan.v1",
        path,
        status,
        placeholder_count,
        missing_requirements: missing_requirements.into_iter().map(Into::into).collect(),
        proof_free_local_materialization_only: true,
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        trust_boundary:
            "product_hardening_render_evidence_artifact_scan_is_read_only: inspects proof-free in-client render evidence artifacts for placeholders and missing local observation fields only; records no proof row, creates no verification event, promotes no cloud draft, and cannot replace explicit operator-confirmed private-client UI evidence",
    }
}

fn product_hardening_render_evidence_missing_requirements(
    client: &str,
    value: &Value,
) -> Vec<String> {
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
    if !value
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(product_hardening_allowed_render_evidence_source)
    {
        missing.push("source_must_be_manual_operator_or_client_capture".to_string());
    }
    if !product_hardening_positive_observed_at_ns(value.get("observed_at_ns")) {
        missing.push("observed_at_ns_must_be_positive".to_string());
    }
    if !product_hardening_concrete_string_field(value, "review_render_fingerprint") {
        missing.push("review_render_fingerprint_must_be_concrete".to_string());
    }
    let surfaces = value.get("rendered_surfaces").and_then(Value::as_array);
    if surfaces.is_none_or(Vec::is_empty) {
        missing.push("rendered_surfaces_must_be_non_empty".to_string());
        return missing;
    }
    let surfaces = surfaces.expect("checked above");
    if surfaces.iter().any(product_hardening_value_contains_template_placeholder) {
        missing.push("rendered_surfaces_must_not_contain_template_placeholders".to_string());
    }
    if !surfaces.iter().any(product_hardening_surface_is_visible) {
        missing.push("rendered_surfaces_must_include_visible_surface".to_string());
    }
    if !surfaces.iter().any(|surface| product_hardening_concrete_string_field(surface, "kind")) {
        missing.push("rendered_surfaces_must_include_concrete_kind".to_string());
    }
    if !surfaces.iter().any(|surface| product_hardening_concrete_string_field(surface, "title")) {
        missing.push("rendered_surfaces_must_include_visible_title".to_string());
    }
    missing
}

fn product_hardening_positive_observed_at_ns(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => {
            number.as_i64().is_some_and(|value| value > 0)
                || number.as_u64().is_some_and(|value| value > 0)
        }
        Some(Value::String(text)) => text.trim().parse::<u64>().is_ok_and(|value| value > 0),
        _ => false,
    }
}

fn product_hardening_concrete_string_field(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|text| !text.is_empty() && !product_hardening_is_template_placeholder(text))
}

fn product_hardening_surface_is_visible(surface: &Value) -> bool {
    surface.get("visible").and_then(Value::as_bool) == Some(true)
}

fn product_hardening_value_template_placeholder_count(value: &Value) -> usize {
    match value {
        Value::String(text) => usize::from(product_hardening_is_template_placeholder(text)),
        Value::Array(values) => {
            values.iter().map(product_hardening_value_template_placeholder_count).sum()
        }
        Value::Object(map) => {
            map.values().map(product_hardening_value_template_placeholder_count).sum()
        }
        _ => 0,
    }
}

fn product_hardening_value_contains_template_placeholder(value: &Value) -> bool {
    product_hardening_value_template_placeholder_count(value) > 0
}

fn product_hardening_is_template_placeholder(text: &str) -> bool {
    let text = text.trim();
    text.len() > 2 && text.starts_with('<') && text.ends_with('>')
}

fn product_hardening_allowed_render_evidence_source(source: &str) -> bool {
    matches!(
        source.trim(),
        "manual_operator"
            | "client_capture"
            | "client_ui_capture"
            | "client_dom_capture"
            | "screenshot_ocr"
    )
}

pub fn refresh_required_client_proof_matrix_operator_action(
    row: &mut RequiredClientProofMatrixRow,
) {
    row.operator_next_action_id =
        required_client_operator_next_action_id(&row.status, row.next_step_id.as_deref());
    row.operator_next_action_label =
        required_client_operator_next_action_label(&row.operator_next_action_id, &row.client);
    row.external_action_safety =
        product_hardening_external_action_safety(&row.client, &row.operator_next_action_id);
    row.external_action = product_hardening_external_operator_action(
        &row.client,
        &row.operator_next_action_id,
        &row.operator_next_action_label,
        row.next_step_id.as_deref(),
        &row.proof_session_cli,
        row.external_action_safety.as_ref(),
    );
}

pub fn attach_required_client_proof_session_probe(
    row: &mut RequiredClientProofMatrixRow,
    next_step_id: Option<String>,
    next_command: Option<Vec<String>>,
    next_mcp_tool: Option<String>,
    next_mcp_arguments: Option<Value>,
    next_mcp_trust_boundary: Option<String>,
) {
    row.next_step_id.clone_from(&next_step_id);
    row.proof_session_next_command = next_command;
    row.proof_session_next_step_id = next_step_id;
    row.proof_session_next_mcp_tool = next_mcp_tool;
    row.proof_session_next_mcp_arguments = next_mcp_arguments;
    row.proof_session_next_mcp_trust_boundary = next_mcp_trust_boundary;
}

fn required_client_operator_next_action_id(status: &str, next_step_id: Option<&str>) -> String {
    match status {
        "ready" => return "client_binding_release_gate_passed".to_string(),
        "blocked_by_artifact_or_identity" => {
            return match next_step_id {
                Some("render_review_surface") => {
                    "regenerate_review_render_report_for_artifact_repair".to_string()
                }
                Some("capture_in_client_render_evidence") => {
                    "capture_fresh_render_evidence_for_artifact_repair".to_string()
                }
                Some("record_observed_in_client_render") => {
                    "rerecord_in_client_render_proof_for_artifact_repair".to_string()
                }
                Some("execute_rendered_review_control") => {
                    "execute_rendered_review_control_for_artifact_repair".to_string()
                }
                Some("record_observed_review_action") => {
                    "rerecord_review_action_proof_for_artifact_repair".to_string()
                }
                _ => "refresh_invalid_client_binding_artifacts".to_string(),
            };
        }
        "blocked_by_non_release_evidence_source" => {
            return "record_release_grade_private_client_proof".to_string();
        }
        _ => {}
    }

    match next_step_id {
        Some("render_client_binding_proof_session") => {
            "render_client_binding_proof_session".to_string()
        }
        Some("render_or_write_installed_config" | "install_or_merge_private_client_config") => {
            "write_or_install_private_client_binding_config".to_string()
        }
        Some("trigger_private_client_hook") => {
            "trigger_real_private_client_hook_to_write_private_spool_event".to_string()
        }
        Some("start_continue_devdata_collector_before_real_hook") => {
            "start_continue_devdata_collector_before_real_hook".to_string()
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
        Some("record_release_grade_private_client_proof") => {
            "record_release_grade_private_client_proof".to_string()
        }
        Some(next) => format!("continue_proof_session_{next}"),
        None => "inspect_private_client_readiness".to_string(),
    }
}

fn required_client_operator_next_action_label(action_id: &str, client: &str) -> String {
    let display_name = client_binding_display_name(client);
    match action_id {
        "client_binding_release_gate_passed" => "Release gate passed".to_string(),
        "refresh_invalid_client_binding_artifacts" => {
            "Refresh invalid binding artifacts".to_string()
        }
        "regenerate_review_render_report_for_artifact_repair" => {
            "Regenerate review-render artifact".to_string()
        }
        "capture_fresh_render_evidence_for_artifact_repair" => {
            "Capture fresh render evidence".to_string()
        }
        "rerecord_in_client_render_proof_for_artifact_repair" => {
            "Re-record in-client render proof".to_string()
        }
        "execute_rendered_review_control_for_artifact_repair" => {
            "Execute rendered review control".to_string()
        }
        "rerecord_review_action_proof_for_artifact_repair" => {
            "Re-record review-action proof".to_string()
        }
        "record_release_grade_private_client_proof" => {
            "Record release-grade private-client proof".to_string()
        }
        "render_client_binding_proof_session" => "Render client binding proof session".to_string(),
        "write_or_install_private_client_binding_config" => {
            format!("Install {display_name} binding config")
        }
        "trigger_real_private_client_hook_to_write_private_spool_event" => {
            format!("Trigger real {display_name} hook")
        }
        "start_continue_devdata_collector_before_real_hook" => {
            "Start Continue dev-data collector".to_string()
        }
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

pub fn product_hardening_external_action_safety(
    client: &str,
    action_id: &str,
) -> Option<ProductHardeningExternalActionSafety> {
    if action_id != "trigger_real_private_client_hook_to_write_private_spool_event" {
        return None;
    }
    let display_name = client_binding_display_name(client);
    let suggested_minimal_test_prompt = match client {
        "continue" => "SOMA hook ping. Reply with exactly: SOMA_CONTINUE_HOOK_OK",
        "codex-app" => "SOMA hook ping. Reply with exactly: SOMA_CODEX_APP_HOOK_OK",
        _ => "SOMA hook ping. Reply with exactly: SOMA_PRIVATE_HOOK_OK",
    };
    Some(ProductHardeningExternalActionSafety {
        source: "soma_product_hardening.external_action_safety.v1".to_string(),
        classification: "real_private_client_action_may_send_prompt_to_provider".to_string(),
        requires_operator_confirmation_before_submission: true,
        may_transmit_prompt_to_provider: true,
        suggested_minimal_test_prompt: suggested_minimal_test_prompt.to_string(),
        forbidden_inputs: vec![
            "secrets".to_string(),
            "API keys".to_string(),
            "credentials".to_string(),
            "private customer data".to_string(),
            "proprietary source snippets".to_string(),
            "large workspace context".to_string(),
        ],
        reason: format!(
            "A real {display_name} action is required before hardening can pass private-client proof, but submitting chat/edit text may be sent through the configured client provider. Use only the minimal hook-ping prompt unless the operator explicitly approves broader context."
        ),
        trust_boundary: "product_hardening_external_action_safety_is_read_only: documents the operator privacy boundary for the next real client action only; it records no proof row, creates no verification event, submits no prompt, executes no command, and cannot substitute for observed app-hook/render/review-action evidence".to_string(),
    })
}

fn product_hardening_external_operator_action(
    client: &str,
    action_id: &str,
    action_label: &str,
    proof_session_step_id: Option<&str>,
    proof_session_cli: &str,
    safety: Option<&ProductHardeningExternalActionSafety>,
) -> Option<ProductHardeningExternalOperatorAction> {
    if action_id != "trigger_real_private_client_hook_to_write_private_spool_event" {
        return None;
    }
    let safety = safety?;
    let step_id = proof_session_step_id.unwrap_or("trigger_private_client_hook");
    let display_name = client_binding_display_name(client);
    Some(ProductHardeningExternalOperatorAction {
        source: "soma_product_hardening.external_action.v1".to_string(),
        client: client.to_string(),
        action_id: action_id.to_string(),
        action_label: action_label.to_string(),
        action_kind: "real_private_client_action".to_string(),
        proof_session_step_id: step_id.to_string(),
        required_operator_action: format!(
            "Open {display_name}, submit only the minimal SOMA hook-ping prompt after operator confirmation, then wait for a fresh private lifecycle event before recording any proof."
        ),
        requires_operator_confirmation_before_submission:
            safety.requires_operator_confirmation_before_submission,
        may_transmit_prompt_to_provider: safety.may_transmit_prompt_to_provider,
        suggested_minimal_test_prompt: safety.suggested_minimal_test_prompt.clone(),
        forbidden_inputs: safety.forbidden_inputs.clone(),
        proof_session_cli: proof_session_cli.to_string(),
        readiness_probe_command: vec![
            "tools/soma-client-hook-readiness.sh".to_string(),
            "--client".to_string(),
            client.to_string(),
        ],
        proof_after_success_step_id: "record_observed_app_hook".to_string(),
        required_observation: vec![
            format!("fresh `{client}_private_lifecycle_hook` adapter event"),
            "matching binding nonce from the active proof session".to_string(),
            "writer metadata from the private client adapter".to_string(),
            "observed_at_ns newer than the installed binding artifact".to_string(),
        ],
        records_proof: false,
        creates_verification_event: false,
        promotes_cloud_draft: false,
        why_next_mcp_call_is_null: "A real private-client hook must be triggered inside the user's client; MCP can only render the proof session or record evidence after the private lifecycle event exists, so no MCP call can substitute for this action.".to_string(),
        trust_boundary: "product_hardening_external_action_is_read_only: describes the required real private-client operator action only; it submits no prompt, records no proof row, creates no verification event, promotes no cloud draft, and cannot substitute for user/tool/local observed evidence".to_string(),
    })
}

fn client_binding_display_name(client: &str) -> String {
    match client {
        "codex-app" => "Codex app".to_string(),
        "codex-cli" => "Codex CLI".to_string(),
        "claude-code" => "Claude Code".to_string(),
        "cursor" => "Cursor".to_string(),
        "continue" => "Continue".to_string(),
        other => other.to_string(),
    }
}

fn client_binding_external_proof_requirement(
    client: &str,
    proof_level: &str,
) -> ClientBindingExternalProofRequirement {
    let mut record_mcp_arguments = product_control_record_proof_mcp_arguments(client, proof_level);
    record_mcp_arguments.insert(
        "evidence_source".to_string(),
        Value::String("required_client_proof_matrix_external_requirement".to_string()),
    );
    let (required_external_evidence, operator_confirmation_key, trust_boundary) = match proof_level {
        "observed_app_hook" => (
            vec![
                "installed client config replayed through soma-adapter-lifecycle".to_string(),
                "private adapter event JSONL containing the target client, private event_source, binding_nonce, writer metadata, and event timestamp".to_string(),
                "adapter drain report proving the real client hook produced captured turns or cloud-output capture".to_string(),
                "explicit operator confirmation that the event came from real app invocation".to_string(),
                "explicit release-grade confirmation that the evidence is real private-client operator/runtime observation".to_string(),
            ],
            Some("operator_confirm_real_app_invocation".to_string()),
            "external_app_hook_requirement_is_not_evidence_by_itself: readiness requires real private-client artifacts and operator confirmation recorded through soma_client_binding_record_proof",
        ),
        "observed_in_client_render" => (
            vec![
                "installed client config identity for the target client".to_string(),
                "saved soma_review_render report for the target client".to_string(),
                "filled soma.in_client_render_evidence.v1 packet from the private client UI".to_string(),
                "review-render report fingerprint, workbench version, interaction contract version, and rendered control_id coverage".to_string(),
                "explicit operator confirmation that the review surface was visible in the target client".to_string(),
                "explicit release-grade confirmation that the evidence is real private-client operator/runtime observation".to_string(),
            ],
            Some("operator_confirm_in_client_render".to_string()),
            "external_in_client_render_requirement_is_not_evidence_by_itself: readiness requires structured render evidence from the private client and operator confirmation recorded through soma_client_binding_record_proof",
        ),
        "observed_review_action" => (
            vec![
                "installed client config identity for the target client".to_string(),
                "saved soma_review_action report produced from a rendered control_id".to_string(),
                "control binding verification tying the action to the previously rendered review surface".to_string(),
                "non-cloud verification evidence created by the review action".to_string(),
                "explicit operator confirmation that the review action was executed from the private client".to_string(),
                "explicit release-grade confirmation that the evidence is real private-client operator/runtime observation".to_string(),
            ],
            Some("operator_confirm_review_action".to_string()),
            "external_review_action_requirement_is_not_evidence_by_itself: readiness requires storage-gated review-action output with non-cloud verification evidence and operator confirmation recorded through soma_client_binding_record_proof",
        ),
        _ => (
            vec!["proof level must be rendered through the client binding proof session".to_string()],
            None,
            "external_proof_requirement_is_not_evidence_by_itself: render proof_session before recording proof",
        ),
    };

    ClientBindingExternalProofRequirement {
        proof_level: proof_level.to_string(),
        evidence_source: "required_client_proof_matrix_external_requirement".to_string(),
        evidence_source_replacement_required: true,
        release_grade_evidence_source_template: release_grade_evidence_source_template(
            client,
            proof_level,
        ),
        required_external_evidence,
        record_mcp_tool: "soma_client_binding_record_proof".to_string(),
        record_mcp_arguments,
        operator_confirmation_key,
        release_grade_confirmation_key: "operator_confirm_release_grade_evidence".to_string(),
        release_evidence_source_policy_schema:
            "soma.client_binding_release_evidence_source_policy.v1".to_string(),
        trust_boundary: trust_boundary.to_string(),
    }
}

fn release_grade_evidence_source_template(client: &str, proof_level: &str) -> String {
    format!("private_client_operator_observed_{client}_{proof_level}")
}

pub fn normalize_required_client_names<'a>(
    clients: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut normalized = BTreeSet::new();
    for client in clients {
        let client = client.trim().to_ascii_lowercase();
        if !client.is_empty() {
            normalized.insert(client);
        }
    }
    normalized.into_iter().collect()
}

pub const DEFAULT_REQUIRED_PRIVATE_CLIENTS: &[&str] = &["codex-app", "cursor", "continue"];

pub fn effective_required_client_names(
    require_client_binding_ready: bool,
    client: Option<&str>,
    explicit_required_clients: Vec<String>,
) -> Vec<String> {
    if !explicit_required_clients.is_empty() {
        return explicit_required_clients;
    }
    if !require_client_binding_ready {
        return Vec::new();
    }
    if let Some(client) = client {
        let client = normalize_required_client_names(std::iter::once(client));
        if !client.is_empty() {
            return client;
        }
    }
    normalize_required_client_names(DEFAULT_REQUIRED_PRIVATE_CLIENTS.iter().copied())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewBacklogHardeningAudit {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub claim_count: usize,
    pub cloud_draft_blocked_count: usize,
    pub proposal_count: usize,
    pub ready_proposal_count: usize,
    pub manual_review_proposal_count: usize,
    pub missing_verification_count: usize,
    pub semantic_support_diversity_manual_review_count: usize,
    pub explicit_review_proposal_count: usize,
    pub semantic_review_pending_count: usize,
    pub semantic_review_status: String,
    pub semantic_review_primary_surface: String,
    pub semantic_review_next_step: String,
    pub semantic_review_learning_cli_hint: String,
    pub semantic_review_render_cli_hint: String,
    pub semantic_review_report_cli_hint: String,
    pub semantic_review_actions_cli_hint: String,
    pub semantic_review_mcp_tools: Vec<String>,
    pub semantic_review_control_contract: String,
    pub semantic_review_trust_boundary: String,
    pub pending_review_count: usize,
    pub interruption_should_interrupt: bool,
    pub interruption_reason: String,
    pub next_surface: String,
    pub passed: bool,
}

impl ReviewBacklogHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewInteractionHardeningAudit {
    pub client: String,
    pub source: String,
    pub version: String,
    pub action_count: usize,
    pub enabled_action_count: usize,
    pub evidence_action_count: usize,
    pub submit_tool_action_count: usize,
    pub passed_checks: Vec<String>,
    pub failed_checks: Vec<String>,
    pub passed: bool,
}

impl ReviewInteractionHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewControlBindingHardeningAudit {
    pub client: String,
    pub source: String,
    pub schema: String,
    pub expected_control_count: usize,
    pub binding_count: usize,
    pub required_dom_attribute_count: usize,
    pub passed_checks: Vec<String>,
    pub failed_checks: Vec<String>,
    pub passed: bool,
}

impl ReviewControlBindingHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskFrameRetentionHardeningAudit {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub now_ns: i64,
    pub cutoff_ns: i64,
    pub retention_days: i64,
    pub apply: bool,
    pub status: String,
    pub cleanup_allowed: bool,
    pub inspect_cli_hint: String,
    pub cleanup_cli_hint: String,
    pub eligible_count: usize,
    pub eligible_unreferenced_ids_sample: Vec<i64>,
    pub protected_referenced_ids_sample: Vec<i64>,
    pub retained_referenced_count: usize,
    pub retained_by_claim_count: usize,
    pub retained_by_proposal_count: usize,
    pub retained_by_outcome_count: usize,
    pub deleted_count: usize,
    pub passed: bool,
}

impl TaskFrameRetentionHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct LatentInterfaceHardeningAudit {
    pub query: String,
    pub schema: String,
    pub mode: String,
    pub proxy_binding_count: usize,
    pub textual_fallback_format: String,
    pub textual_fallback_non_empty: bool,
    pub vector_payload_included: bool,
    pub hidden_state_injection_supported: bool,
    pub skipped_untrusted_cloud_draft_count: usize,
    pub passed: bool,
}

impl LatentInterfaceHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckedInEvalReportsHardeningAudit {
    pub source: String,
    pub required_report_count: usize,
    pub report_count: usize,
    pub passed_report_count: usize,
    pub failed_report_count: usize,
    pub total_case_count: usize,
    pub reports: Vec<CheckedInEvalReportStatus>,
    pub passed: bool,
    pub trust_boundary: String,
}

impl CheckedInEvalReportsHardeningAudit {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckedInEvalReportStatus {
    pub suite: String,
    pub report_path: String,
    pub kind: Option<String>,
    pub case_count: usize,
    pub passed_count: Option<usize>,
    pub failed_count: Option<usize>,
    pub required_outcome: String,
    pub required_outcome_passed: bool,
    pub passed: bool,
    pub failure_reason: Option<String>,
}

pub fn audit_checked_in_eval_reports() -> CheckedInEvalReportsHardeningAudit {
    let reports = vec![
        audit_summary_eval_report(
            "context_trust_loop",
            "docs/evals/context-trust-loop-report.json",
            include_str!("../../../../docs/evals/context-trust-loop-report.json"),
            "context_trust_loop_eval",
            "trust_boundary_passed",
        ),
        audit_summary_eval_report(
            "semantic_learning_quality",
            "docs/evals/semantic-learning-quality-report.json",
            include_str!("../../../../docs/evals/semantic-learning-quality-report.json"),
            "semantic_learning_quality_eval",
            "semantic_learning_quality_passed",
        ),
        audit_summary_eval_report(
            "client_integration",
            "docs/evals/client-integration-report.json",
            include_str!("../../../../docs/evals/client-integration-report.json"),
            "client_integration_eval",
            "client_integration_contract_passed",
        ),
        audit_summary_eval_report(
            "latent_proxy_eval",
            "docs/evals/latent-proxy-eval-report.json",
            include_str!("../../../../docs/evals/latent-proxy-eval-report.json"),
            "latent_proxy_eval_hardening",
            "latent_proxy_eval_passed",
        ),
        audit_ranking_eval_report(
            "context_ranking_dogfood",
            "docs/evals/context-ranking-dogfood-report.json",
            include_str!("../../../../docs/evals/context-ranking-dogfood-report.json"),
        ),
    ];
    let required_report_count = 5;
    let passed_report_count = reports.iter().filter(|report| report.passed).count();
    let failed_report_count = reports.len().saturating_sub(passed_report_count);
    let total_case_count = reports.iter().map(|report| report.case_count).sum();
    CheckedInEvalReportsHardeningAudit {
        source: "soma_checked_in_eval_reports_v1".to_string(),
        required_report_count,
        report_count: reports.len(),
        passed_report_count,
        failed_report_count,
        total_case_count,
        passed: reports.len() == required_report_count && failed_report_count == 0,
        reports,
        trust_boundary: "checked_in_eval_reports_are_release_evidence_only: parses bundled docs/evals reports, records no proof row, creates no verification event, promotes no memory, and does not execute eval scripts".to_string(),
    }
}

fn audit_summary_eval_report(
    suite: &str,
    report_path: &str,
    body: &str,
    expected_kind: &str,
    required_summary_flag: &str,
) -> CheckedInEvalReportStatus {
    let required_outcome = format!("summary.{required_summary_flag}=true");
    let report = match serde_json::from_str::<Value>(body) {
        Ok(report) => report,
        Err(err) => {
            return failed_eval_report(
                suite,
                report_path,
                required_outcome,
                format!("report JSON parse failed: {err}"),
            );
        }
    };
    let kind = report.get("kind").and_then(Value::as_str).map(str::to_string);
    let summary = report.get("summary").and_then(Value::as_object);
    let case_count =
        summary.and_then(|summary| summary.get("case_count")).and_then(Value::as_u64).unwrap_or(0)
            as usize;
    let passed_count = summary
        .and_then(|summary| summary.get("passed_count"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let failed_count = summary
        .and_then(|summary| summary.get("failed_count"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let required_outcome_passed =
        summary.and_then(|summary| summary.get(required_summary_flag)).and_then(Value::as_bool)
            == Some(true);
    let all_cases_passed = report.get("cases").and_then(Value::as_array).is_some_and(|cases| {
        !cases.is_empty()
            && cases.iter().all(|case| case.get("passed").and_then(Value::as_bool) == Some(true))
    });
    let passed = kind.as_deref() == Some(expected_kind)
        && case_count > 0
        && passed_count == Some(case_count)
        && failed_count == Some(0)
        && required_outcome_passed
        && all_cases_passed;
    CheckedInEvalReportStatus {
        suite: suite.to_string(),
        report_path: report_path.to_string(),
        kind,
        case_count,
        passed_count,
        failed_count,
        required_outcome,
        required_outcome_passed,
        passed,
        failure_reason: (!passed).then(|| {
            format!(
                "expected kind={expected_kind}, case_count>0, passed_count=case_count, failed_count=0, all cases passed, and summary.{required_summary_flag}=true"
            )
        }),
    }
}

fn audit_ranking_eval_report(
    suite: &str,
    report_path: &str,
    body: &str,
) -> CheckedInEvalReportStatus {
    let required_outcome = "comparison.hopfield_promotable_by_recall_precision=false".to_string();
    let report = match serde_json::from_str::<Value>(body) {
        Ok(report) => report,
        Err(err) => {
            return failed_eval_report(
                suite,
                report_path,
                required_outcome,
                format!("report JSON parse failed: {err}"),
            );
        }
    };
    let kind = report.get("kind").and_then(Value::as_str).map(str::to_string);
    let comparison = report.get("comparison").and_then(Value::as_object);
    let case_count = comparison
        .and_then(|comparison| comparison.get("case_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let scored_case_count = comparison
        .and_then(|comparison| comparison.get("scored_case_count"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let promotable = comparison
        .and_then(|comparison| comparison.get("hopfield_promotable_by_recall_precision"))
        .and_then(Value::as_bool);
    let required_outcome_passed = promotable == Some(false);
    let passed = kind.as_deref() == Some("context_relevant_memory_ranking_comparison")
        && case_count > 0
        && scored_case_count == Some(case_count)
        && required_outcome_passed;
    CheckedInEvalReportStatus {
        suite: suite.to_string(),
        report_path: report_path.to_string(),
        kind,
        case_count,
        passed_count: scored_case_count,
        failed_count: scored_case_count.map(|scored| case_count.saturating_sub(scored)),
        required_outcome,
        required_outcome_passed,
        passed,
        failure_reason: (!passed).then(|| {
            "expected ranking report kind, scored_case_count=case_count, and Hopfield candidate not promotable by recall/precision".to_string()
        }),
    }
}

fn failed_eval_report(
    suite: &str,
    report_path: &str,
    required_outcome: String,
    failure_reason: String,
) -> CheckedInEvalReportStatus {
    CheckedInEvalReportStatus {
        suite: suite.to_string(),
        report_path: report_path.to_string(),
        kind: None,
        case_count: 0,
        passed_count: None,
        failed_count: None,
        required_outcome,
        required_outcome_passed: false,
        passed: false,
        failure_reason: Some(failure_reason),
    }
}

pub fn audit_latent_interface_packet(
    storage: &Storage,
    query: Option<&str>,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<LatentInterfaceHardeningAudit, StorageError> {
    let query = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("product hardening latent interface release audit")
        .to_string();
    let packet = render_latent_interface_packet(
        storage,
        LatentInterfacePacketInput {
            query: query.clone(),
            project: project.map(str::to_string),
            session_id: session_id.map(str::to_string),
            limit: DEFAULT_LATENT_PREDICTOR_LIMIT,
            scan_limit: DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
            min_confidence: DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE,
        },
    )?;
    let textual_fallback_non_empty = !packet.textual_fallback.projection.trim().is_empty();
    let passed = packet.schema == LATENT_INTERFACE_PACKET_SCHEMA
        && !packet.latent_channel.vector_payload_included
        && !packet.latent_channel.hidden_state_injection_supported
        && textual_fallback_non_empty
        && packet
            .proxy_bindings
            .iter()
            .all(|binding| binding.source_trust != ClaimSourceType::CloudDraft.as_str());
    Ok(LatentInterfaceHardeningAudit {
        query,
        schema: packet.schema.to_string(),
        mode: packet.mode.to_string(),
        proxy_binding_count: packet.proxy_binding_count,
        textual_fallback_format: packet.textual_fallback.format.to_string(),
        textual_fallback_non_empty,
        vector_payload_included: packet.latent_channel.vector_payload_included,
        hidden_state_injection_supported: packet.latent_channel.hidden_state_injection_supported,
        skipped_untrusted_cloud_draft_count: packet
            .prediction_report
            .skipped_untrusted_cloud_draft_count,
        passed,
    })
}

pub fn audit_task_frame_retention_hygiene(
    storage: &mut Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    retention_days: i64,
    now_ns: i64,
) -> Result<TaskFrameRetentionHardeningAudit, StorageError> {
    let cutoff_ns = task_frame_retention_cutoff_ns(now_ns, retention_days)?;
    let report = storage.apply_task_frame_retention(&TaskFrameRetentionRequest {
        cutoff_ns,
        retention_days,
        project: project.map(str::to_string),
        session_id: session_id.map(str::to_string),
        apply: false,
    })?;
    let eligible_unreferenced_ids_sample =
        report.eligible_unreferenced_ids.iter().take(20).copied().collect();
    let protected_referenced_ids_sample =
        report.retained_referenced_ids.iter().take(20).copied().collect();
    let status = if report.deleted_count > 0 {
        "unexpected_mutation_in_hardening_audit"
    } else if report.eligible_count > 0 {
        "stale_unreferenced_task_frames_found"
    } else {
        "clean"
    }
    .to_string();
    let inspect_cli_hint =
        task_frame_retention_cli_hint(project, session_id, report.retention_days, false);
    let cleanup_cli_hint =
        task_frame_retention_cli_hint(project, session_id, report.retention_days, true);
    Ok(TaskFrameRetentionHardeningAudit {
        project: report.project,
        session_id: report.session_id,
        now_ns,
        cutoff_ns: report.cutoff_ns,
        retention_days: report.retention_days,
        apply: report.apply,
        status,
        cleanup_allowed: report.eligible_count > 0 && report.deleted_count == 0,
        inspect_cli_hint,
        cleanup_cli_hint,
        eligible_count: report.eligible_count,
        eligible_unreferenced_ids_sample,
        protected_referenced_ids_sample,
        retained_referenced_count: report.retained_referenced_count,
        retained_by_claim_count: report.retained_by_claim_ids.len(),
        retained_by_proposal_count: report.retained_by_proposal_ids.len(),
        retained_by_outcome_count: report.retained_by_outcome_ids.len(),
        deleted_count: report.deleted_count,
        passed: report.eligible_count == 0 && report.deleted_count == 0,
    })
}

fn task_frame_retention_cli_hint(
    project: Option<&str>,
    session_id: Option<&str>,
    retention_days: i64,
    apply: bool,
) -> String {
    let mut hint =
        format!("soma context task-frames retention --older-than-days {}", retention_days);
    if let Some(project) = project {
        hint.push_str(" --project ");
        hint.push_str(project);
    }
    if let Some(session_id) = session_id {
        hint.push_str(" --session-id ");
        hint.push_str(session_id);
    }
    if apply {
        hint.push_str(" --apply");
    }
    hint
}

pub fn audit_review_interaction_contract(
    plan: &ReviewRenderPlan,
) -> ReviewInteractionHardeningAudit {
    let contract = &plan.interaction_contract;
    let workbench = &plan.workbench;
    let enabled_actions: Vec<_> = contract.actions.iter().filter(|action| action.enabled).collect();
    let evidence_actions: Vec<_> =
        contract.actions.iter().filter(|action| action.evidence_required).collect();
    let submit_tool_action_count =
        enabled_actions.iter().filter(|action| action.submit_tool == "soma_review_action").count();
    let contract_has_read_only_boundary = contract.mutation_boundary.contains("read_only_until")
        || contract.mutation_boundary.contains("read_only");
    let has_cloud_draft_guardrail = contract
        .global_guardrails
        .iter()
        .any(|guardrail| guardrail == "do_not_submit_cloud_draft_as_evidence");
    let has_no_promotion_guardrail = contract
        .global_guardrails
        .iter()
        .any(|guardrail| guardrail == "do_not_promote_l3_or_l4_from_render_plan_output");
    let all_enabled_actions_have_submit_tool = enabled_actions.iter().all(|action| {
        action.submit_tool == "soma_review_action"
            && !action.control_id.trim().is_empty()
            && action.pre_submit_checks.iter().any(|check| check == "action_enabled_true")
    });
    let all_enabled_actions_have_control_binding_precheck = enabled_actions.iter().all(|action| {
        action
            .pre_submit_checks
            .iter()
            .any(|check| check == "control_id_matches_current_enabled_action_option")
    });
    let all_enabled_actions_have_control_id_template_binding =
        enabled_actions.iter().all(|action| {
            action
                .submit_arguments_template
                .get("control_id")
                .and_then(Value::as_str)
                .is_some_and(|control_id| control_id == action.control_id)
        });
    let evidence_actions_have_cloud_draft_precheck = evidence_actions.iter().all(|action| {
        action.pre_submit_checks.iter().any(|check| check == "evidence_source_is_not_cloud_draft")
    });
    let evidence_actions_have_verifier_options = evidence_actions.iter().all(|action| {
        ["user", "test", "tool", "local_observation", "correction"]
            .iter()
            .any(|verifier| action.accepted_verifier_types.iter().any(|value| value == verifier))
    });
    let workbench_counts_match_actions = workbench.counts.enabled_actions == enabled_actions.len()
        && workbench.counts.evidence_required_actions == evidence_actions.len()
        && workbench.counts.batch_operations == plan.batch_operation_count;
    let workbench_forbids_untrusted_evidence =
        ["cloud_draft", "review_render_output", "client_binding_status"].iter().all(|source| {
            workbench.evidence_policy.forbidden_evidence_sources.iter().any(|value| value == source)
        });
    let workbench_submission_has_prechecks = [
        "action_enabled_true",
        "control_id_matches_current_enabled_action_option",
        "evidence_source_is_not_cloud_draft",
        "verifier_type_is_one_of_template_accepted_verifier_types",
    ]
    .iter()
    .all(|check| {
        workbench.submission_contract.required_pre_submit_checks.iter().any(|value| value == check)
    });
    let workbench_is_read_only =
        workbench.submission_contract.trust_boundary.contains("workbench_is_read_only")
            && workbench.submission_contract.mutation_tool == "soma_review_action"
            && workbench.submission_contract.batch_tool == "soma_review_batch";
    let mut passed_checks = Vec::new();
    let mut failed_checks = Vec::new();
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "version_v1",
        contract.version == "soma.review_interaction_contract.v1",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "source_review_render_interaction_contract",
        contract.source == "soma_review_render.interaction_contract",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "read_only_boundary",
        contract_has_read_only_boundary,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "cloud_draft_guardrail",
        has_cloud_draft_guardrail,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "no_promotion_guardrail",
        has_no_promotion_guardrail,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "enabled_actions_submit_tool",
        all_enabled_actions_have_submit_tool,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "enabled_actions_control_binding_precheck",
        all_enabled_actions_have_control_binding_precheck,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "enabled_actions_control_id_template_binding",
        all_enabled_actions_have_control_id_template_binding,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "evidence_actions_cloud_draft_precheck",
        evidence_actions_have_cloud_draft_precheck,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "evidence_actions_verifier_options",
        evidence_actions_have_verifier_options,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_version_v1",
        workbench.version == "soma.review_workbench.v1"
            && workbench.source == "soma_review_render.workbench",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_counts_match_actions",
        workbench_counts_match_actions,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_forbids_untrusted_evidence",
        workbench_forbids_untrusted_evidence,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_submission_prechecks",
        workbench_submission_has_prechecks,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_read_only_submission_boundary",
        workbench_is_read_only,
    );
    let passed = failed_checks.is_empty();

    ReviewInteractionHardeningAudit {
        client: contract.client.clone(),
        source: contract.source.clone(),
        version: contract.version.clone(),
        action_count: contract.actions.len(),
        enabled_action_count: enabled_actions.len(),
        evidence_action_count: evidence_actions.len(),
        submit_tool_action_count,
        passed_checks,
        failed_checks,
        passed,
    }
}

pub fn audit_review_control_binding_manifest(
    plan: &ReviewRenderPlan,
) -> ReviewControlBindingHardeningAudit {
    let manifest = &plan.control_binding_manifest;
    let contract = &plan.interaction_contract;
    let workbench = &plan.workbench;
    let action_control_ids: BTreeSet<String> =
        contract.actions.iter().map(|action| action.control_id.clone()).collect();
    let manifest_expected_ids: BTreeSet<String> =
        manifest.expected_control_ids.iter().cloned().collect();
    let binding_control_ids: BTreeSet<String> =
        manifest.bindings.iter().map(|binding| binding.control_id.clone()).collect();
    let required_dom_attributes = [
        "data-soma-review-action",
        "data-soma-control-id",
        "data-submit-tool",
        "data-mcp-arguments-template",
    ];
    let bindings_match_actions = manifest.bindings.len() == contract.actions.len()
        && contract.actions.iter().all(|action| {
            manifest.bindings.iter().any(|binding| {
                binding.control_id == action.control_id
                    && binding.target_type == action.target_type
                    && binding.target_id == action.target_id
                    && binding.action == action.action
                    && binding.enabled == action.enabled
                    && binding.evidence_required == action.evidence_required
            })
        });
    let bindings_have_submit_tool =
        manifest.bindings.iter().all(|binding| binding.submit_tool == "soma_review_action");
    let bindings_have_template_control_id = manifest.bindings.iter().all(|binding| {
        binding
            .submit_arguments_template
            .get("control_id")
            .and_then(Value::as_str)
            .is_some_and(|control_id| control_id == binding.control_id)
    });
    let bindings_have_dom_selector = manifest.bindings.iter().all(|binding| {
        binding.dom_selector.starts_with("[data-soma-control-id=")
            && binding.dom_selector.contains(&binding.control_id)
    });
    let bindings_have_pre_submit_checks = manifest.bindings.iter().all(|binding| {
        ["action_enabled_true", "control_id_matches_current_enabled_action_option"]
            .iter()
            .all(|required| binding.pre_submit_checks.iter().any(|check| check == required))
    });
    let mut passed_checks = Vec::new();
    let mut failed_checks = Vec::new();
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "schema_v1",
        manifest.schema == "soma.review_control_binding_manifest.v1",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "source_review_render_control_binding_manifest",
        manifest.source == "soma_review_render.control_binding_manifest",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "client_matches_interaction_contract",
        manifest.client == contract.client,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "workbench_version_matches",
        manifest.workbench_version == workbench.version,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "interaction_contract_version_matches",
        manifest.interaction_contract_version == contract.version,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "expected_control_ids_match_actions",
        manifest.expected_control_count == action_control_ids.len()
            && manifest_expected_ids == action_control_ids,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_unique_and_cover_actions",
        binding_control_ids.len() == manifest.bindings.len()
            && binding_control_ids == action_control_ids,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_match_interaction_actions",
        bindings_match_actions,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_submit_to_review_action",
        bindings_have_submit_tool,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_template_control_id",
        bindings_have_template_control_id,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_dom_selector",
        bindings_have_dom_selector,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "bindings_pre_submit_checks",
        bindings_have_pre_submit_checks,
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "required_dom_attributes",
        required_dom_attributes.iter().all(|attribute| {
            manifest.required_dom_attributes.iter().any(|value| value == attribute)
        }),
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "selectors_define_review_controls",
        manifest.action_selector == "[data-soma-review-action=\"true\"]"
            && manifest.submit_button_selector == "[data-soma-submit-control=\"true\"]"
            && manifest.argument_template_attribute == "data-mcp-arguments-template"
            && manifest.evidence_form_selector == "[data-evidence-form=\"required\"]",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "missing_control_blocks_submission",
        manifest.missing_control_behavior
            == "block_submission_and_require_fresh_soma_review_render_before_mutation",
    );
    push_review_interaction_check(
        &mut passed_checks,
        &mut failed_checks,
        "read_only_trust_boundary",
        manifest.trust_boundary.contains("read_only")
            && manifest.trust_boundary.contains("records no render proof"),
    );
    let passed = failed_checks.is_empty();

    ReviewControlBindingHardeningAudit {
        client: manifest.client.clone(),
        source: manifest.source.clone(),
        schema: manifest.schema.clone(),
        expected_control_count: manifest.expected_control_count,
        binding_count: manifest.bindings.len(),
        required_dom_attribute_count: manifest.required_dom_attributes.len(),
        passed_checks,
        failed_checks,
        passed,
    }
}

fn push_review_interaction_check(
    passed_checks: &mut Vec<String>,
    failed_checks: &mut Vec<String>,
    name: &str,
    passed: bool,
) {
    if passed {
        passed_checks.push(name.to_string());
    } else {
        failed_checks.push(name.to_string());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductHardeningGate {
    pub name: String,
    pub status: String,
    pub blocking: bool,
    pub summary: String,
    pub evidence_refs: Vec<String>,
    pub recommended_actions: Vec<String>,
}

impl ProductHardeningGate {
    fn pass(
        name: &str,
        summary: String,
        evidence_refs: Vec<String>,
        recommended_actions: Vec<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            status: "pass".to_string(),
            blocking: false,
            summary,
            evidence_refs,
            recommended_actions,
        }
    }

    fn warn(
        name: &str,
        summary: String,
        evidence_refs: Vec<String>,
        recommended_actions: Vec<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            status: "warn".to_string(),
            blocking: false,
            summary,
            evidence_refs,
            recommended_actions,
        }
    }

    fn fail(
        name: &str,
        summary: String,
        evidence_refs: Vec<String>,
        recommended_actions: Vec<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            status: "fail".to_string(),
            blocking: true,
            summary,
            evidence_refs,
            recommended_actions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductObjectiveCoverage {
    pub area: String,
    pub status: String,
    pub blocking: bool,
    pub summary: String,
    pub evidence_refs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_next_commands: Vec<Vec<String>>,
}

impl ProductObjectiveCoverage {
    fn pass(
        area: impl Into<String>,
        summary: impl Into<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self {
            area: area.into(),
            status: "pass".to_string(),
            blocking: false,
            summary: summary.into(),
            evidence_refs,
            operator_next_commands: Vec::new(),
        }
    }

    fn warn(
        area: impl Into<String>,
        summary: impl Into<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self {
            area: area.into(),
            status: "warn".to_string(),
            blocking: false,
            summary: summary.into(),
            evidence_refs,
            operator_next_commands: Vec::new(),
        }
    }

    fn fail(
        area: impl Into<String>,
        summary: impl Into<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self {
            area: area.into(),
            status: "fail".to_string(),
            blocking: true,
            summary: summary.into(),
            evidence_refs,
            operator_next_commands: Vec::new(),
        }
    }

    fn with_operator_next_commands(mut self, commands: Vec<Vec<String>>) -> Self {
        self.operator_next_commands = commands;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductControlPlan {
    pub source: String,
    pub policy: String,
    pub trust_boundary: String,
    pub ready: bool,
    pub step_count: usize,
    pub blocking_step_count: usize,
    pub operator_evidence_step_count: usize,
    pub steps: Vec<ProductControlStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductControlStep {
    pub priority: usize,
    pub gate: String,
    pub gate_status: String,
    pub blocking: bool,
    pub action_kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub target_clients: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_cli: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_mcp_call: Option<ProductControlMcpCall>,
    pub requires_operator_evidence: bool,
    pub mutates_when_executed: bool,
    pub execution_boundary: String,
    pub safety_note: String,
    pub evidence_refs: Vec<String>,
    pub preflight_checks: Vec<ProductControlCheck>,
    pub followup_verification: Vec<ProductControlCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductControlMcpCall {
    pub tool: String,
    pub arguments: BTreeMap<String, Value>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductControlCheck {
    pub check_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_call: Option<ProductControlMcpCall>,
    pub expected: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductHardeningOperatorCard {
    pub source: String,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub headline: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_client: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_session_next_mcp_call: Option<ProductControlMcpCall>,
    pub primary_action_kind: String,
    pub primary_next_step: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_next_cli: Option<String>,
    pub primary_next_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_mcp_call: Option<ProductControlMcpCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_external_action_safety: Option<ProductHardeningExternalActionSafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_external_action: Option<ProductHardeningExternalOperatorAction>,
    pub gate_counts: BTreeMap<String, usize>,
    pub objective_coverage_counts: BTreeMap<String, usize>,
    pub control_plan_ready: bool,
    pub blocking_step_count: usize,
    pub operator_evidence_step_count: usize,
    pub gates_requiring_attention: Vec<String>,
    pub objectives_requiring_attention: Vec<String>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductHardeningScopeResolution {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd_project: Option<String>,
    pub explicit_scope_recommended: bool,
    pub override_command: Vec<String>,
    pub reason: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone)]
pub struct ProductHardeningScopeResolutionInput<'a> {
    pub scope: &'a ContextScope,
    pub explicit_project: Option<&'a str>,
    pub explicit_session_id: Option<&'a str>,
    pub task_frame_id: Option<i64>,
    pub cwd_project: Option<String>,
    pub override_command: Vec<String>,
}

pub fn build_product_hardening_scope_resolution(
    input: ProductHardeningScopeResolutionInput<'_>,
) -> ProductHardeningScopeResolution {
    let explicit_project = input.explicit_project.and_then(non_empty_str);
    let explicit_session_id = input.explicit_session_id.and_then(non_empty_str);
    let source = if explicit_project.is_some() && explicit_session_id.is_some() {
        "explicit_project_session"
    } else if explicit_project.is_some() {
        "explicit_project"
    } else if explicit_session_id.is_some() {
        "explicit_session"
    } else if input.task_frame_id.is_some() {
        "task_frame"
    } else if input.scope.project.as_deref().and_then(non_empty_str).is_some() {
        "anil_inferred_project"
    } else {
        "current"
    };

    let project = input.scope.project.clone();
    let session_id = input.scope.session_id.clone();
    let cwd_project = input.cwd_project.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let selected_project = project.as_deref().and_then(non_empty_str);
    let cwd_project_ref = cwd_project.as_deref().and_then(non_empty_str);
    let project_mismatch = matches!((selected_project, cwd_project_ref), (Some(selected), Some(cwd)) if selected != cwd);
    let explicit_scope_recommended = source == "anil_inferred_project" && project_mismatch;
    let override_command =
        if explicit_scope_recommended { input.override_command } else { Vec::new() };
    let reason = match source {
        "explicit_project_session" => "operator supplied --project and --session-id",
        "explicit_project" => "operator supplied --project",
        "explicit_session" => "operator supplied --session-id",
        "task_frame" => "TaskFrame supplied or selected the project/session scope",
        "anil_inferred_project" => {
            "ANIL project_attribution selected the scope because no explicit project, session, or TaskFrame filter was supplied"
        }
        _ => "no explicit or inferred project/session scope was available",
    };

    ProductHardeningScopeResolution {
        source: source.to_string(),
        project,
        session_id,
        cwd_project,
        explicit_scope_recommended,
        override_command,
        reason: reason.to_string(),
        trust_boundary: "read_only_scope_explanation: reports how the audited ContextEnvelope scope was selected; records no proof row, changes no session, and promotes no memory"
            .to_string(),
    }
}

fn non_empty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProductHardeningReport {
    pub scope: ContextScope,
    pub scope_resolution: ProductHardeningScopeResolution,
    pub assembled_at_ns: i64,
    pub task_frame_id: Option<i64>,
    pub client: Option<String>,
    pub required_clients: Vec<String>,
    pub client_binding_required: bool,
    pub review_queue_clear_required: bool,
    pub task_frame_retention_clean_required: bool,
    pub task_frame_projection_required: bool,
    pub status: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub operator_card: ProductHardeningOperatorCard,
    pub passed: bool,
    pub gate_count: usize,
    pub passed_gate_count: usize,
    pub warning_gate_count: usize,
    pub failed_gate_count: usize,
    pub objective_coverage_total_count: usize,
    pub objective_coverage_pass_count: usize,
    pub objective_coverage_warning_count: usize,
    pub objective_coverage_fail_count: usize,
    pub objective_coverage: Vec<ProductObjectiveCoverage>,
    pub gates: Vec<ProductHardeningGate>,
    pub control_plan: ProductControlPlan,
    pub recommended_actions: Vec<String>,
    pub envelope: ContextEnvelopeAudit,
    pub storage_trust: TrustBoundaryStorageAudit,
    pub review_backlog: ReviewBacklogHardeningAudit,
    pub review_interaction: ReviewInteractionHardeningAudit,
    pub review_control_binding: ReviewControlBindingHardeningAudit,
    pub task_frame_retention: TaskFrameRetentionHardeningAudit,
    pub latent_interface: LatentInterfaceHardeningAudit,
    pub checked_in_eval_reports: CheckedInEvalReportsHardeningAudit,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_frame: Option<TaskFrameProjectionAudit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_binding: Option<ClientBindingHardeningAudit>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProductHardeningRequirements {
    pub client_binding_ready: bool,
    pub review_queue_clear: bool,
    pub task_frame_retention_clean: bool,
    pub task_frame_projection: bool,
}

/// Build a read-only product hardening report from the existing trust gates.
///
/// The report deliberately composes already-audited boundaries instead of
/// adding a new promotion path. `fail` gates are release blockers. `warn`
/// gates mean the report did not see enough optional evidence, such as a
/// client app-hook/render proof, but they do not mutate trust or block local
/// storage correctness.
pub fn build_product_hardening_report(
    scope: ContextScope,
    scope_resolution: ProductHardeningScopeResolution,
    assembled_at_ns: i64,
    task_frame_id: Option<i64>,
    client: Option<String>,
    envelope: ContextEnvelopeAudit,
    storage_trust: TrustBoundaryStorageAudit,
    review_backlog: ReviewBacklogHardeningAudit,
    review_interaction: ReviewInteractionHardeningAudit,
    review_control_binding: ReviewControlBindingHardeningAudit,
    task_frame_retention: TaskFrameRetentionHardeningAudit,
    latent_interface: LatentInterfaceHardeningAudit,
    task_frame: Option<TaskFrameProjectionAudit>,
    client_binding: Option<ClientBindingHardeningAudit>,
    required_clients: Vec<String>,
    requirements: ProductHardeningRequirements,
) -> ProductHardeningReport {
    let mut gates = Vec::new();
    let client_binding_required = requirements.client_binding_ready;
    let review_queue_clear_required = requirements.review_queue_clear;
    let task_frame_retention_clean_required = requirements.task_frame_retention_clean;
    let task_frame_projection_required = requirements.task_frame_projection;

    if envelope.passed() {
        gates.push(ProductHardeningGate::pass(
            "context_envelope_evidence",
            format!(
                "ContextEnvelope sections cite top-level evidence ({} sections, {} top-level evidence refs).",
                envelope.section_count, envelope.top_level_evidence_count
            ),
            vec!["audit_context_envelope".to_string()],
            vec![
                "Keep soma_context_audit or `soma context audit` in the release gate before relying on cloud-facing context.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "context_envelope_evidence",
            format!(
                "ContextEnvelope evidence contract failed: {} section refs missing, {} top-level refs missing, and {} legacy memory_tier projection layer(s) found.",
                envelope.missing_section_evidence.len(),
                envelope.missing_top_level_evidence.len(),
                envelope.legacy_memory_tier_projection_layers.len()
            ),
            vec!["audit_context_envelope".to_string()],
            vec![
                "Inspect missing evidence with `soma context why` and fix uncited ContextEnvelope sections before release.".to_string(),
                "Rerun `soma context audit` after the projection/evidence fix.".to_string(),
            ],
        ));
    }

    if envelope.memory_tier_compatibility_passed {
        gates.push(ProductHardeningGate::pass(
            "memory_lifecycle_source_of_truth",
            format!(
                "ContextEnvelope relevant_memory uses lifecycle projection layers {:?}; legacy episodes.memory_tier remains compatibility metadata only.",
                envelope.relevant_memory_layer_values
            ),
            vec![
                "ContextEnvelope.relevant_memory.layer".to_string(),
                "evidence_latent_proxies".to_string(),
                "claim_records".to_string(),
                "verification_events".to_string(),
            ],
            vec![
                "Keep lifecycle promotion/decay decisions in evidence_latent_proxies, claim_records, verification_events, and review proposals rather than episodes.memory_tier.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "memory_lifecycle_source_of_truth",
            format!(
                "Legacy episodes.memory_tier labels leaked into ContextEnvelope relevant_memory.layer: {:?}.",
                envelope.legacy_memory_tier_projection_layers
            ),
            vec![
                "ContextEnvelope.relevant_memory.layer".to_string(),
                "episodes.memory_tier".to_string(),
                "evidence_latent_proxies".to_string(),
            ],
            vec![
                "Map cloud-facing relevant_memory layers to projection sources such as `recent`, `semantic`, or `long_term_proxy` instead of legacy tier names.".to_string(),
                "Treat episodes.memory_tier as read compatibility metadata, not as the four-stage lifecycle source of truth.".to_string(),
                "Rerun `soma context hardening-report` after the projection layer mapping is repaired.".to_string(),
            ],
        ));
    }

    if storage_trust.passed() {
        gates.push(ProductHardeningGate::pass(
            "storage_trust_boundary",
            format!(
                "Claim/proposal storage trust gates passed across {} claims and {} proposals.",
                storage_trust.checked_claim_count, storage_trust.checked_proposal_count
            ),
            vec!["audit_storage_trust_boundary".to_string()],
            vec![
                "Keep `soma context trust-audit` in the release gate after review or learning proposal batches.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "storage_trust_boundary",
            format!(
                "Storage trust boundary failed: {} promoted cloud drafts, {} untrusted semantic facts, {} applied proposal violations.",
                storage_trust.promoted_cloud_draft_without_trust_claim_ids.len(),
                storage_trust.semantic_fact_without_trust_claim_ids.len(),
                storage_trust.applied_promotion_proposals_missing_trust.len()
            ),
            vec![
                "claim_records".to_string(),
                "verification_events".to_string(),
                "learning_critic_proposals".to_string(),
            ],
            vec![
                "Use `soma context review-report` to inspect unverified cloud drafts and open proposals.".to_string(),
                "Verify, reject, or correct the affected claims through review actions, then rerun `soma context trust-audit`.".to_string(),
            ],
        ));
    }

    if review_backlog.passed() {
        gates.push(ProductHardeningGate::pass(
            "review_backlog_clear",
            format!(
                "Review queue is clear: semantic_review_status={} and no unverified cloud drafts, open learning proposals, or pending manual semantic review items were found.",
                review_backlog.semantic_review_status
            ),
            vec!["soma_review_queue".to_string(), "soma learning --json".to_string()],
            vec![
                "Keep `soma context review-queue` or MCP `soma_review_queue` in release checks before claiming the local control plane is caught up.".to_string(),
            ],
        ));
    } else {
        let summary = format!(
            "Review queue has {} pending item(s): {} cloud_draft blocker(s), {} open proposal(s), {} manual semantic-support review item(s); semantic_review_status={} primary_surface={}.",
            review_backlog.pending_review_count,
            review_backlog.cloud_draft_blocked_count,
            review_backlog.proposal_count,
            review_backlog.semantic_support_diversity_manual_review_count,
            review_backlog.semantic_review_status,
            review_backlog.semantic_review_primary_surface
        );
        let evidence_refs = vec![
            "soma learning --json".to_string(),
            "soma_review_render".to_string(),
            "soma_review_queue".to_string(),
            "soma_review_report".to_string(),
            "soma_review_digest".to_string(),
            "soma_review_drain".to_string(),
        ];
        let recommended_actions = vec![
            format!(
                "Render `{}` to inspect semantic learning status, cloud_draft blockers, L4 candidates, and review-only belief signals.",
                review_backlog.semantic_review_learning_cli_hint
            ),
            format!(
                "Render `{}` or MCP `soma_review_render` so the client can show evidence-gated review controls before release.",
                review_backlog.semantic_review_render_cli_hint
            ),
            format!(
                "Render `{}` or MCP `soma_review_report` so the operator can inspect every pending claim/proposal before release.",
                review_backlog.semantic_review_report_cli_hint
            ),
            "Record user/tool/test/local/correction verification for unverified cloud drafts through `soma context review-action` or MCP `soma_review_action`.".to_string(),
            "For L4 semantic proposals with `semantic_support_diversity_requires_manual_review`, inspect support diversity before using single-proposal apply.".to_string(),
            "Run `soma context review-drain --dry-run` before any safe background drain, then rerun hardening.".to_string(),
        ];
        if review_queue_clear_required {
            gates.push(ProductHardeningGate::fail(
                "review_backlog_clear",
                summary,
                evidence_refs,
                recommended_actions,
            ));
        } else {
            gates.push(ProductHardeningGate::warn(
                "review_backlog_clear",
                summary,
                evidence_refs,
                recommended_actions,
            ));
        }
    }

    if review_interaction.passed() {
        gates.push(ProductHardeningGate::pass(
            "review_interaction_contract",
            format!(
                "Review render interaction contract passed for client `{}` ({} action(s), {} evidence action(s)).",
                review_interaction.client,
                review_interaction.action_count,
                review_interaction.evidence_action_count
            ),
            vec![
                "soma_review_render".to_string(),
                "soma_review_actions".to_string(),
                "soma_review_action".to_string(),
            ],
            vec![
                "Keep `soma_review_render` or MCP `soma_review_render` in client release checks so review controls remain evidence-gated.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "review_interaction_contract",
            format!(
                "Review render interaction contract failed for client `{}`: failed checks [{}].",
                review_interaction.client,
                review_interaction.failed_checks.join(", ")
            ),
            vec![
                "soma_review_render.interaction_contract".to_string(),
                "soma_review_action".to_string(),
            ],
            vec![
                "Fix `soma_review_render` so every enabled action has a stable control id, `soma_review_action` submit template, and `action_enabled_true` pre-submit check.".to_string(),
                "For evidence-taking actions, require `evidence_source_is_not_cloud_draft` and accepted verifier types before release.".to_string(),
                "Rerun `soma context hardening-report` after the interaction contract is repaired.".to_string(),
            ],
        ));
    }

    if review_control_binding.passed() {
        gates.push(ProductHardeningGate::pass(
            "review_control_binding_manifest",
            format!(
                "Review control binding manifest passed for client `{}` ({} expected control(s), {} binding(s)).",
                review_control_binding.client,
                review_control_binding.expected_control_count,
                review_control_binding.binding_count
            ),
            vec![
                "soma_review_render.control_binding_manifest".to_string(),
                "soma_review_action".to_string(),
                "soma.in_client_render_evidence.v1".to_string(),
            ],
            vec![
                "Keep the control binding manifest in private-client release checks so visible controls, DOM attributes, and submit templates stay in sync.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "review_control_binding_manifest",
            format!(
                "Review control binding manifest failed for client `{}`: failed checks [{}].",
                review_control_binding.client,
                review_control_binding.failed_checks.join(", ")
            ),
            vec![
                "soma_review_render.control_binding_manifest".to_string(),
                "soma_review_action".to_string(),
                "soma.in_client_render_evidence.v1".to_string(),
            ],
            vec![
                "Fix `soma_review_render` so every interaction action has one matching control binding, stable `data-soma-control-id`, and a `soma_review_action` submit template.".to_string(),
                "Require clients to block submission and request a fresh render plan when a listed control id is missing from the DOM.".to_string(),
                "Rerun `soma context hardening-report` after the control binding manifest is repaired.".to_string(),
            ],
        ));
    }

    if task_frame_retention.passed() {
        gates.push(ProductHardeningGate::pass(
            "task_frame_retention_hygiene",
            format!(
                "TaskFrame retention dry-run is clean for {} day policy: no unreferenced stale TaskFrames are eligible for pruning.",
                task_frame_retention.retention_days
            ),
            vec![
                "task_frames".to_string(),
                "soma context task-frames retention".to_string(),
            ],
            vec![
                format!(
                    "Keep `{}` in product hardening checks so pre-cloud judgment records do not accumulate without review.",
                    task_frame_retention.inspect_cli_hint
                ),
            ],
        ));
    } else {
        let summary = format!(
            "TaskFrame retention dry-run found {} unreferenced stale TaskFrame(s) older than {} day(s); sample ids {:?}; {} referenced stale TaskFrame(s) are protected from cleanup.",
            task_frame_retention.eligible_count,
            task_frame_retention.retention_days,
            task_frame_retention.eligible_unreferenced_ids_sample,
            task_frame_retention.retained_referenced_count
        );
        let evidence_refs =
            vec!["task_frames".to_string(), "soma context task-frames retention".to_string()];
        let recommended_actions = vec![
            format!(
                "Run `{}` to inspect stale unreferenced TaskFrames before release.",
                task_frame_retention.inspect_cli_hint
            ),
            format!(
                "After confirming no referenced claim/proposal/outcome rows depend on the frames, run `{}` to prune only eligible unreferenced TaskFrames.",
                task_frame_retention.cleanup_cli_hint
            ),
            "Rerun `soma context hardening-report` after retention cleanup.".to_string(),
        ];
        if task_frame_retention_clean_required {
            gates.push(ProductHardeningGate::fail(
                "task_frame_retention_hygiene",
                summary,
                evidence_refs,
                recommended_actions,
            ));
        } else {
            gates.push(ProductHardeningGate::warn(
                "task_frame_retention_hygiene",
                summary,
                evidence_refs,
                recommended_actions,
            ));
        }
    }

    match &task_frame {
        Some(audit) if audit.passed() => gates.push(ProductHardeningGate::pass(
            "task_frame_projection",
            format!(
                "TaskFrame {} cloud projection passed privacy/redaction policy `{}`.",
                audit.task_frame_id, audit.projection_policy
            ),
            vec!["audit_task_frame_projection".to_string()],
            vec![
                "Use this TaskFrame id for the cloud call and rerun hardening after cloud output is reviewed.".to_string(),
            ],
        )),
        Some(audit) => gates.push(ProductHardeningGate::fail(
            "task_frame_projection",
            format!(
                "TaskFrame {} cloud projection failed privacy/redaction checks.",
                audit.task_frame_id
            ),
            vec![
                "task_frames.local_full_json".to_string(),
                "task_frames.cloud_redacted_json".to_string(),
            ],
            vec![
                "Regenerate or edit the TaskFrame projection so blocked, unsafe, or secret-like fields are removed before cloud use.".to_string(),
                "Rerun `soma context audit --task-frame-id <id>` before sending the TaskFrame to a cloud model.".to_string(),
            ],
        )),
        None => {
            let summary = "No TaskFrame id supplied, so TaskFrame cloud projection privacy was not checked in this report.".to_string();
            let evidence_refs = vec!["soma context audit --task-frame-id".to_string()];
            let recommended_actions = vec![
                "Create or select a TaskFrame and rerun hardening with `--task-frame-id` before a cloud-facing release check.".to_string(),
            ];
            if task_frame_projection_required {
                gates.push(ProductHardeningGate::fail(
                    "task_frame_projection",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            } else {
                gates.push(ProductHardeningGate::warn(
                    "task_frame_projection",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            }
        }
    }

    if latent_interface.passed() {
        gates.push(ProductHardeningGate::pass(
            "latent_interface_packet",
            format!(
                "Latent interface packet `{}` is read-only, uses textual fallback `{}`, includes {} proxy binding(s), and carries no vector or hidden-state payload.",
                latent_interface.schema,
                latent_interface.textual_fallback_format,
                latent_interface.proxy_binding_count
            ),
            vec![
                "soma_latent_packet".to_string(),
                "soma context latent-packet".to_string(),
                "soma.latent_interface_packet.v1".to_string(),
            ],
            vec![
                "Keep `soma_latent_packet` or `soma context latent-packet` in client release checks before claiming advanced latent-channel readiness.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "latent_interface_packet",
            format!(
                "Latent interface packet contract failed: schema={}, vector_payload_included={}, hidden_state_injection_supported={}, textual_fallback_non_empty={}.",
                latent_interface.schema,
                latent_interface.vector_payload_included,
                latent_interface.hidden_state_injection_supported,
                latent_interface.textual_fallback_non_empty
            ),
            vec![
                "soma_latent_packet".to_string(),
                "soma context latent-packet".to_string(),
                "soma.latent_interface_packet.v1".to_string(),
            ],
            vec![
                "Fix the latent packet renderer so current production packets remain inspectable, text-fallback capable, and vector-free until an explicit provider latent channel exists.".to_string(),
                "Rerun `soma context hardening-report` after the latent-interface contract is repaired.".to_string(),
            ],
        ));
    }

    let checked_in_eval_reports = audit_checked_in_eval_reports();
    if checked_in_eval_reports.passed() {
        gates.push(ProductHardeningGate::pass(
            "checked_in_eval_reports",
            format!(
                "{} checked-in eval report(s) passed across {} case(s).",
                checked_in_eval_reports.report_count, checked_in_eval_reports.total_case_count
            ),
            vec![
                "docs/evals/context-trust-loop-report.json".to_string(),
                "docs/evals/semantic-learning-quality-report.json".to_string(),
                "docs/evals/client-integration-report.json".to_string(),
                "docs/evals/latent-proxy-eval-report.json".to_string(),
                "docs/evals/context-ranking-dogfood-report.json".to_string(),
                "tools/smoke-all.sh".to_string(),
            ],
            vec![
                "Keep `tools/smoke-all.sh` and each eval `--check-docs-report` command in release checks so checked-in eval evidence stays fresh.".to_string(),
            ],
        ));
    } else {
        gates.push(ProductHardeningGate::fail(
            "checked_in_eval_reports",
            format!(
                "{} checked-in eval report(s) failed hardening audit.",
                checked_in_eval_reports.failed_report_count
            ),
            vec![
                "docs/evals/context-trust-loop-report.json".to_string(),
                "docs/evals/semantic-learning-quality-report.json".to_string(),
                "docs/evals/client-integration-report.json".to_string(),
                "docs/evals/latent-proxy-eval-report.json".to_string(),
                "docs/evals/context-ranking-dogfood-report.json".to_string(),
                "tools/smoke-all.sh".to_string(),
            ],
            vec![
                "Run `tools/context-trust-loop-eval.sh --check-docs-report` and regenerate the report if stale.".to_string(),
                "Run `tools/semantic-learning-quality-eval.sh --check-docs-report` and regenerate the report if stale.".to_string(),
                "Run `tools/client-integration-eval.sh --check-docs-report` and regenerate the report if stale.".to_string(),
                "Run `tools/latent-proxy-eval.sh --check-docs-report` and regenerate the report if stale.".to_string(),
                "Run `tools/context-ranking-dogfood-report.sh --check-docs-report` or the ranking dogfood report generator used by the repo before release.".to_string(),
            ],
        ));
    }

    match &client_binding {
        Some(audit) if audit.proofs_found == 0 || audit.client_count == 0 => {
            let summary = if audit.required_clients.is_empty() {
                "No client binding proof rows were found; app-hook/render/review-action readiness is unproven."
                    .to_string()
            } else {
                format!(
                    "No client binding proof rows were found for required client(s) {:?}; app-hook/render/review-action readiness is unproven.",
                    audit.required_clients
                )
            };
            let evidence_refs = vec![
                "soma adapter-binding-proof --proof-session --json".to_string(),
                "soma_client_binding_proof_session".to_string(),
            ];
            let recommended_actions = vec![
                "Render the compact read-only proof session with MCP `soma_client_binding_proof_session` or `soma adapter-binding-proof --proof-session --json`.".to_string(),
                "Render the fuller evidence bundle with MCP `soma_client_binding_evidence_bundle` or `soma adapter-binding-proof --evidence-bundle` if setup needs config preview or proof-kit details.".to_string(),
                "Render a proof-free setup plan with MCP `soma_client_binding_install_plan` or `soma adapter-binding-proof --render-installed-config` if the session shows no eligible installed config.".to_string(),
                "Install or merge the generated client config, run the real client hook, then record `observed_app_hook` evidence with matching private event_source and binding_nonce.".to_string(),
                "Record `observed_in_client_render` only after collecting structured `soma.in_client_render_evidence.v1` evidence bound to the target client, review-render report fingerprint, review workbench version, and review interaction contract version.".to_string(),
                "Record `observed_review_action` only after a rendered control_id produces a storage-gated review-action report with non-cloud verification evidence.".to_string(),
            ];
            if client_binding_required {
                gates.push(ProductHardeningGate::fail(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            } else {
                gates.push(ProductHardeningGate::warn(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            }
        }
        Some(audit) if audit.required_scope_has_artifact_or_identity_failure() => {
            let artifact_failure_count = audit.required_scope_artifact_failure_count();
            let coherence_failure_count = audit.required_scope_coherence_failure_count();
            let summary = if audit.required_clients.is_empty() {
                format!(
                    "Client binding artifact or identity integrity failed for {} proof rows ({} artifact failure(s), {} coherence failure(s)).",
                    audit.proofs_found, artifact_failure_count, coherence_failure_count
                )
            } else {
                format!(
                    "Required client binding artifact or identity integrity failed for {:?} ({} artifact failure(s), {} coherence failure(s)).",
                    audit.required_clients, artifact_failure_count, coherence_failure_count
                )
            };
            gates.push(ProductHardeningGate::fail(
                "client_binding_readiness",
                summary,
                vec![
                    "client_binding_proofs".to_string(),
                    "soma adapter-binding-proof --status --json".to_string(),
                ],
                vec![
                    "Run `soma adapter-binding-proof --status --verify-evidence-artifacts --json` to identify changed or missing evidence artifacts.".to_string(),
                    "Render `soma adapter-binding-proof --proof-session --json` after fixing artifacts so the proof_session names the next safe recording step.".to_string(),
                    "Re-record stale client binding proof rows from fresh real-client evidence before claiming readiness.".to_string(),
                ],
            ));
        }
        Some(audit) if !audit.has_ready_client() => {
            let summary = format!(
                "Client binding proof rows exist, but no client is ready for a private-client claim yet ({:?}).",
                audit.readiness_values
            );
            let evidence_refs = vec![
                "soma adapter-binding-proof --status --json".to_string(),
                "soma adapter-binding-proof --proof-session --json".to_string(),
            ];
            let recommended_actions = vec![
                "Run `soma adapter-binding-proof --proof-session --json` so proof_session shows the next app-hook, render, or review-action proof step for this client.".to_string(),
                "Use MCP `soma_client_binding_proof_session` to show the current proof session in the client setup UI.".to_string(),
            ];
            if client_binding_required {
                gates.push(ProductHardeningGate::fail(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            } else {
                gates.push(ProductHardeningGate::warn(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            }
        }
        Some(audit) if !audit.required_clients_ready() => {
            let summary = format!(
                "Required client binding readiness is incomplete: required={:?}, missing={:?}, unready={:?}.",
                audit.required_clients, audit.missing_required_clients, audit.unready_required_clients
            );
            let evidence_refs = vec![
                "soma adapter-binding-proof --status --json".to_string(),
                "soma adapter-binding-proof --proof-session --json".to_string(),
            ];
            let recommended_actions = vec![
                "Collect app-hook, in-client-render, and review-action proof for every required client before claiming all-client readiness.".to_string(),
                "Use MCP `soma_client_binding_proof_session` once per missing or unready client to render the next operator step without recording proof.".to_string(),
            ];
            if client_binding_required {
                gates.push(ProductHardeningGate::fail(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            } else {
                gates.push(ProductHardeningGate::warn(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            }
        }
        Some(audit) => gates.push(ProductHardeningGate::pass(
            "client_binding_readiness",
            if audit.required_clients.is_empty() {
                format!(
                    "{} client binding status row(s) are ready; primary readiness is {:?}.",
                    audit.ready_client_count, audit.primary_readiness
                )
            } else {
                format!(
                    "All required client binding status rows are ready ({}/{}): {:?}.",
                    audit.required_ready_client_count,
                    audit.required_client_count,
                    audit.required_clients
                )
            },
            vec![
                "client_binding_proofs".to_string(),
                "soma_client_binding_proofs".to_string(),
            ],
            vec![
                "Treat private-client readiness as proven only for the cited ledger rows and keep artifact replay in release checks.".to_string(),
            ],
        )),
        None => {
            let summary = if client_binding_required {
                "Client binding readiness was required but skipped; private app-hook/render/review-action readiness is unproven in this report.".to_string()
            } else {
                "Client binding readiness was not requested; private app-hook/render/review-action readiness is unproven in this report.".to_string()
            };
            let evidence_refs = vec![
                "soma_client_binding_proofs".to_string(),
                "soma_client_binding_proof_session".to_string(),
            ];
            let recommended_actions = vec![
                "Rerun hardening with `--client <client>` when the release depends on a private client integration.".to_string(),
                "Use `--skip-client-binding` only for releases that do not claim private app-hook, in-client render, or review-action readiness.".to_string(),
            ];
            if client_binding_required {
                gates.push(ProductHardeningGate::fail(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            } else {
                gates.push(ProductHardeningGate::warn(
                    "client_binding_readiness",
                    summary,
                    evidence_refs,
                    recommended_actions,
                ));
            }
        }
    }

    let recommended_actions = gates
        .iter()
        .filter(|gate| gate.status != "pass")
        .flat_map(|gate| gate.recommended_actions.iter().cloned())
        .collect();
    let control_plan = build_product_control_plan(
        &gates,
        client.as_deref(),
        client_binding.as_ref(),
        task_frame_id,
        task_frame_projection_required,
    );
    let gate_count = gates.len();
    let failed_gate_count = gates.iter().filter(|gate| gate.status == "fail").count();
    let warning_gate_count = gates.iter().filter(|gate| gate.status == "warn").count();
    let passed_gate_count = gates.iter().filter(|gate| gate.status == "pass").count();
    let passed = failed_gate_count == 0;
    let status = if failed_gate_count > 0 {
        "fail"
    } else if warning_gate_count > 0 {
        "pass_with_warnings"
    } else {
        "pass"
    }
    .to_string();
    let objective_coverage = build_product_objective_coverage(
        &envelope,
        &storage_trust,
        &review_backlog,
        &review_interaction,
        &review_control_binding,
        &task_frame_retention,
        &latent_interface,
        &checked_in_eval_reports,
        task_frame.as_ref(),
        client_binding.as_ref(),
        requirements,
        failed_gate_count,
        warning_gate_count,
    );
    let objective_coverage_total_count = objective_coverage.len();
    let objective_coverage_pass_count =
        objective_coverage.iter().filter(|item| item.status == "pass").count();
    let objective_coverage_warning_count =
        objective_coverage.iter().filter(|item| item.status == "warn").count();
    let objective_coverage_fail_count =
        objective_coverage.iter().filter(|item| item.status == "fail").count();
    let operator_card = build_product_hardening_operator_card(
        &status,
        &gates,
        &objective_coverage,
        &control_plan,
        client_binding.as_ref(),
    );
    let operator_next_action_id = operator_card.operator_next_action_id.clone();
    let operator_next_action_label = operator_card.operator_next_action_label.clone();

    ProductHardeningReport {
        scope,
        scope_resolution,
        assembled_at_ns,
        task_frame_id,
        client,
        required_clients,
        client_binding_required,
        review_queue_clear_required,
        task_frame_retention_clean_required,
        task_frame_projection_required,
        status,
        operator_next_action_id,
        operator_next_action_label,
        operator_card,
        passed,
        gate_count,
        passed_gate_count,
        warning_gate_count,
        failed_gate_count,
        objective_coverage_total_count,
        objective_coverage_pass_count,
        objective_coverage_warning_count,
        objective_coverage_fail_count,
        objective_coverage,
        gates,
        control_plan,
        recommended_actions,
        envelope,
        storage_trust,
        review_backlog,
        review_interaction,
        review_control_binding,
        task_frame_retention,
        latent_interface,
        checked_in_eval_reports,
        task_frame,
        client_binding,
        trust_boundary: "product_hardening_report_is_read_only: composes existing ContextEnvelope, TaskFrame, claim/proposal, review queue, retention hygiene, checked-in eval report, and client binding audits; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, deletes no TaskFrame, acknowledges no notification, executes no eval script, and proves no private client behavior beyond cited ledger evidence".to_string(),
    }
}

fn build_product_hardening_operator_card(
    status: &str,
    gates: &[ProductHardeningGate],
    objective_coverage: &[ProductObjectiveCoverage],
    control_plan: &ProductControlPlan,
    client_binding: Option<&ClientBindingHardeningAudit>,
) -> ProductHardeningOperatorCard {
    let failed_gate_count = gates.iter().filter(|gate| gate.status == "fail").count();
    let warning_gate_count = gates.iter().filter(|gate| gate.status == "warn").count();
    let passed_gate_count = gates.iter().filter(|gate| gate.status == "pass").count();
    let objective_fail_count =
        objective_coverage.iter().filter(|item| item.status == "fail").count();
    let objective_warning_count =
        objective_coverage.iter().filter(|item| item.status == "warn").count();
    let objective_pass_count =
        objective_coverage.iter().filter(|item| item.status == "pass").count();
    let primary_step =
        control_plan.steps.iter().find(|step| step.blocking).or_else(|| control_plan.steps.first());
    let primary_client_binding_row = primary_step
        .filter(|step| step.gate == "client_binding_readiness")
        .and_then(|step| product_hardening_primary_client_binding_row(client_binding, step));
    let proof_session_next_step_id =
        primary_client_binding_row.and_then(|row| row.next_step_id.clone());
    let proof_session_next_mcp_call =
        primary_client_binding_row.and_then(product_hardening_client_binding_next_mcp_call);
    let proof_session_next_mcp_tool =
        proof_session_next_mcp_call.as_ref().map(|call| call.tool.clone());
    let (operator_next_action_id, operator_next_action_label) =
        if let Some(row) = primary_client_binding_row {
            (row.operator_next_action_id.clone(), row.operator_next_action_label.clone())
        } else {
            primary_step.map_or_else(
                || ("release_gate_passed".to_string(), "Release gate passed".to_string()),
                |step| (step.action_kind.clone(), step.title.clone()),
            )
        };
    let primary_next_command = primary_client_binding_row
        .and_then(|row| row.proof_session_next_command.clone())
        .or_else(|| {
            primary_client_binding_row
                .map(|row| product_hardening_operator_command(&row.proof_session_cli))
        })
        .or_else(|| {
            primary_step
                .and_then(|step| step.primary_cli.as_deref())
                .map(product_hardening_operator_command)
        })
        .unwrap_or_else(|| {
            vec!["soma".to_string(), "context".to_string(), "hardening-report".to_string()]
        });
    let primary_next_cli = Some(primary_next_command.join(" "));
    let primary_next_step = if let Some(row) = primary_client_binding_row {
        product_hardening_client_binding_next_step(row)
    } else {
        primary_step.map_or_else(
                || {
                    "All product hardening gates are passing for this report scope; keep this report in release checks before making readiness claims."
                        .to_string()
                },
                |step| {
            format!(
                "{}: {}",
                step.title.trim_end_matches('.'),
                step.safety_note
            )
                },
            )
    };
    let primary_external_action_safety =
        primary_client_binding_row.and_then(|row| row.external_action_safety.clone());
    let primary_external_action =
        primary_client_binding_row.and_then(|row| row.external_action.clone());
    let headline = if failed_gate_count > 0 {
        format!("{failed_gate_count} blocking product hardening gate(s) need operator action.")
    } else if warning_gate_count > 0 {
        format!("{warning_gate_count} product hardening warning gate(s) need follow-up before stronger claims.")
    } else {
        "All product hardening gates are passing.".to_string()
    };
    let gates_requiring_attention = gates
        .iter()
        .filter(|gate| gate.status != "pass")
        .map(|gate| gate.name.clone())
        .collect::<Vec<_>>();
    let objectives_requiring_attention = objective_coverage
        .iter()
        .filter(|item| item.status != "pass")
        .map(|item| item.area.clone())
        .collect::<Vec<_>>();
    let mut safe_to_claim = Vec::new();
    let mut blocked_claims = Vec::new();
    if failed_gate_count == 0 {
        safe_to_claim.push(
            "No blocking product hardening gate is visible for this report scope.".to_string(),
        );
        if warning_gate_count == 0 {
            safe_to_claim.push(
                "All objective coverage areas are backed by passing hardening evidence."
                    .to_string(),
            );
        } else {
            blocked_claims.push(
                "Warning areas remain caveated; do not claim those checks are fully proven until their gates pass."
                    .to_string(),
            );
        }
    } else {
        safe_to_claim
            .push("Passing gates remain safe only within their cited evidence refs.".to_string());
        blocked_claims.push(format!(
            "{failed_gate_count} blocking gate(s) prevent release-grade product hardening claims."
        ));
        if objective_fail_count > 0 {
            blocked_claims.push(format!(
                "{objective_fail_count} objective coverage area(s) are still failing."
            ));
        }
    }

    ProductHardeningOperatorCard {
        source: "soma_product_hardening.operator_card.v1".to_string(),
        status: status.to_string(),
        operator_next_action_id,
        operator_next_action_label,
        headline,
        primary_gate: primary_step.map(|step| step.gate.clone()),
        primary_client: primary_client_binding_row.map(|row| row.client.clone()),
        proof_session_next_step_id,
        proof_session_next_mcp_tool,
        proof_session_next_mcp_call,
        primary_action_kind: primary_client_binding_row
            .map(|row| row.operator_next_action_id.clone())
            .or_else(|| primary_step.map(|step| step.action_kind.clone()))
            .unwrap_or_else(|| "release_gate_passed".to_string()),
        primary_next_step,
        primary_next_cli,
        primary_next_command,
        primary_mcp_tool: primary_client_binding_row
            .map(|row| row.proof_session_mcp_tool.clone())
            .or_else(|| primary_step.and_then(|step| step.primary_mcp_tool.clone())),
        primary_mcp_call: primary_client_binding_row
            .map(product_hardening_client_binding_mcp_call)
            .or_else(|| primary_step.and_then(|step| step.primary_mcp_call.clone())),
        primary_external_action_safety,
        primary_external_action,
        gate_counts: BTreeMap::from([
            ("total".to_string(), gates.len()),
            ("pass".to_string(), passed_gate_count),
            ("warn".to_string(), warning_gate_count),
            ("fail".to_string(), failed_gate_count),
        ]),
        objective_coverage_counts: BTreeMap::from([
            ("total".to_string(), objective_coverage.len()),
            ("pass".to_string(), objective_pass_count),
            ("warn".to_string(), objective_warning_count),
            ("fail".to_string(), objective_fail_count),
        ]),
        control_plan_ready: control_plan.ready,
        blocking_step_count: control_plan.blocking_step_count,
        operator_evidence_step_count: control_plan.operator_evidence_step_count,
        gates_requiring_attention,
        objectives_requiring_attention,
        safe_to_claim,
        blocked_claims,
        trust_boundary: "product_hardening_operator_card_is_read_only: summarizes gates, objective coverage, and plan-only control steps; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and executes no command".to_string(),
    }
}

fn product_hardening_operator_command(cli: &str) -> Vec<String> {
    cli.split_whitespace().map(str::to_string).collect()
}

fn product_hardening_primary_client_binding_row<'a>(
    client_binding: Option<&'a ClientBindingHardeningAudit>,
    step: &ProductControlStep,
) -> Option<&'a RequiredClientProofMatrixRow> {
    let audit = client_binding?;
    for target_client in &step.target_clients {
        if let Some(row) = audit
            .required_client_proof_matrix
            .iter()
            .find(|row| row.client == *target_client && row.proof_session_required)
        {
            return Some(row);
        }
    }
    audit
        .required_client_proof_matrix
        .iter()
        .find(|row| row.proof_session_required && row.release_gate == "fail")
}

fn product_hardening_proof_session_step_label(next_step_id: &str) -> String {
    match next_step_id {
        "render_client_binding_proof_session" => "Render client binding proof session",
        "render_or_write_installed_config" => "Render/write installed client config",
        "install_or_merge_private_client_config" => "Install or merge private client config",
        "trigger_private_client_hook" => "Trigger private client hook",
        "record_observed_app_hook" => "Record observed app-hook proof",
        "render_review_surface" => "Render review surface",
        "capture_in_client_render_evidence" => "Capture in-client render evidence",
        "record_observed_in_client_render" => "Record observed in-client render proof",
        "execute_rendered_review_control" => "Execute rendered review control",
        "record_observed_review_action" => "Record observed review-action proof",
        "verify_evidence_artifacts_and_status" => "Verify evidence artifacts and status",
        "record_release_grade_private_client_proof" => "Record release-grade private-client proof",
        _ => "Continue client proof session",
    }
    .to_string()
}

fn product_hardening_client_binding_next_step(row: &RequiredClientProofMatrixRow) -> String {
    let next_step_id = row.next_step_id.as_deref().unwrap_or("client_binding_release_gate_passed");
    let label = product_hardening_proof_session_step_label(next_step_id);
    let missing = if row.missing_proof_levels.is_empty() {
        "no missing proof levels are currently reported".to_string()
    } else {
        format!("missing proof levels: {}", row.missing_proof_levels.join(", "))
    };
    let config_root_note = row
        .config_root_probe_hint
        .as_ref()
        .and_then(|hint| hint.config_root.as_ref())
        .map(|root| format!("; installed config discovery was already probed from `{root}`"))
        .unwrap_or_default();
    if row.status == "blocked_by_artifact_or_identity" {
        let replay_note = if row.artifact_failure_count > 0 && row.coherence_failure_count > 0 {
            format!(
                "{} artifact replay failure(s) and {} identity/coherence failure(s) are blocking stored proof reuse",
                row.artifact_failure_count, row.coherence_failure_count
            )
        } else if row.artifact_failure_count > 0 {
            format!(
                "{} artifact replay failure(s) are blocking stored proof reuse",
                row.artifact_failure_count
            )
        } else if row.coherence_failure_count > 0 {
            format!(
                "{} identity/coherence failure(s) are blocking stored proof reuse",
                row.coherence_failure_count
            )
        } else {
            "stored proof identity or artifact replay is blocking proof reuse".to_string()
        };
        return match next_step_id {
            "render_review_surface" => format!(
                "{replay_note}; regenerate a fresh read-only `{}` review-render report ({label}), render it in the real client UI, then continue with structured render evidence. {missing}{config_root_note}.",
                row.client
            ),
            "capture_in_client_render_evidence" => format!(
                "{replay_note}; fill a fresh `soma.in_client_render_evidence.v1` packet from the visible `{}` UI ({label}) before re-recording observed_in_client_render. {missing}{config_root_note}.",
                row.client
            ),
            "record_observed_in_client_render" => format!(
                "{replay_note}; re-record observed_in_client_render for `{}` only after the fresh render evidence passes storage gates and explicit operator confirmation is present ({label}). {missing}{config_root_note}.",
                row.client
            ),
            "execute_rendered_review_control" => format!(
                "{replay_note}; execute one rendered `{}` review control and save a storage-gated review-action report with non-cloud verification evidence ({label}). {missing}{config_root_note}.",
                row.client
            ),
            "record_observed_review_action" => format!(
                "{replay_note}; re-record observed_review_action for `{}` only after the fresh review-action report is present and explicitly confirmed ({label}). {missing}{config_root_note}.",
                row.client
            ),
            _ => format!(
                "{replay_note}; inspect and refresh the invalid `{}` binding artifacts through the proof-session runbook ({label}). {missing}{config_root_note}.",
                row.client
            ),
        };
    }
    match next_step_id {
        "trigger_private_client_hook" => format!(
            "Trigger a real `{}` private app hook before recording proof ({label}); {missing}{config_root_note}. Use the proof-session command for the exact runbook and keep this report read-only.",
            row.client
        ),
        "render_or_write_installed_config" | "install_or_merge_private_client_config" => format!(
            "Render or install the proof-free `{}` client binding config before any app-hook proof ({label}); {missing}{config_root_note}.",
            row.client
        ),
        "render_client_binding_proof_session" => format!(
            "Render the read-only `{}` proof session to discover the concrete setup or proof blocker ({label}); {missing}{config_root_note}.",
            row.client
        ),
        _ => format!(
            "Continue the `{}` proof session at `{next_step_id}` ({label}); {missing}{config_root_note}.",
            row.client
        ),
    }
}

fn product_hardening_client_binding_mcp_call(
    row: &RequiredClientProofMatrixRow,
) -> ProductControlMcpCall {
    ProductControlMcpCall {
        tool: row.proof_session_mcp_tool.clone(),
        arguments: row
            .proof_session_mcp_arguments
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
        trust_boundary:
            "operator_card_primary_mcp_call_is_template_only_and_not_executed_by_hardening_report"
                .to_string(),
    }
}

fn product_hardening_client_binding_next_mcp_call(
    row: &RequiredClientProofMatrixRow,
) -> Option<ProductControlMcpCall> {
    let tool = row.proof_session_next_mcp_tool.clone()?;
    let arguments = match row.proof_session_next_mcp_arguments.as_ref() {
        Some(Value::Object(arguments)) => {
            arguments.iter().map(|(key, value)| (key.clone(), value.clone())).collect()
        }
        Some(value) => BTreeMap::from([("value".to_string(), value.clone())]),
        None => BTreeMap::new(),
    };
    Some(ProductControlMcpCall {
        tool,
        arguments,
        trust_boundary: row
            .proof_session_next_mcp_trust_boundary
            .clone()
            .unwrap_or_else(|| {
                "operator_card_proof_session_next_mcp_call_is_template_only_and_not_executed_by_hardening_report"
                    .to_string()
            }),
    })
}

fn build_product_control_plan(
    gates: &[ProductHardeningGate],
    client: Option<&str>,
    client_binding: Option<&ClientBindingHardeningAudit>,
    task_frame_id: Option<i64>,
    task_frame_projection_required: bool,
) -> ProductControlPlan {
    let client_binding_target_clients =
        client_binding_control_target_clients(client_binding, client);
    let mut pending: Vec<(usize, &ProductHardeningGate)> =
        gates.iter().enumerate().filter(|(_, gate)| gate.status != "pass").collect();
    pending.sort_by_key(|(idx, gate)| (if gate.status == "fail" { 0 } else { 1 }, *idx));
    let steps: Vec<ProductControlStep> = pending
        .into_iter()
        .enumerate()
        .map(|(priority, (_, gate))| {
            build_product_control_step(
                priority + 1,
                gate,
                client,
                &client_binding_target_clients,
                task_frame_id,
                task_frame_projection_required,
            )
        })
        .collect();
    let blocking_step_count = steps.iter().filter(|step| step.blocking).count();
    let operator_evidence_step_count =
        steps.iter().filter(|step| step.requires_operator_evidence).count();
    ProductControlPlan {
        source: "soma_product_hardening_control_plan".to_string(),
        policy: "non_pass_release_gates_become_plan_only_operator_steps".to_string(),
        trust_boundary: "control_plan_is_read_only_and_never_executes_steps_or_records_evidence"
            .to_string(),
        ready: steps.is_empty(),
        step_count: steps.len(),
        blocking_step_count,
        operator_evidence_step_count,
        steps,
    }
}

fn client_binding_control_target_clients(
    client_binding: Option<&ClientBindingHardeningAudit>,
    client: Option<&str>,
) -> Vec<String> {
    let mut clients = BTreeSet::new();
    if let Some(audit) = client_binding {
        clients.extend(audit.proof_session_target_clients.iter().cloned());
        if clients.is_empty() {
            clients.extend(audit.missing_required_clients.iter().cloned());
            clients.extend(audit.unready_required_clients.iter().cloned());
        }
    }
    if clients.is_empty() {
        if let Some(client) = client {
            clients.insert(client.to_string());
        }
    }
    clients.into_iter().collect()
}

fn build_product_control_step(
    priority: usize,
    gate: &ProductHardeningGate,
    client: Option<&str>,
    client_binding_target_clients: &[String],
    task_frame_id: Option<i64>,
    task_frame_projection_required: bool,
) -> ProductControlStep {
    let target_clients = if gate.name == "client_binding_readiness" {
        client_binding_target_clients.to_vec()
    } else {
        Vec::new()
    };
    let client_arg = target_clients
        .first()
        .cloned()
        .or_else(|| client.map(str::to_string))
        .unwrap_or_else(|| "<client>".to_string());
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    let target_clients_note = if target_clients.len() > 1 {
        format!(" Repeat this step for required clients: {}.", target_clients.join(", "))
    } else {
        String::new()
    };
    let task_frame_arg =
        task_frame_id.map(|id| id.to_string()).unwrap_or_else(|| "<task_frame_id>".to_string());
    let task_frame_projection_required_flag =
        if task_frame_projection_required { " --require-task-frame-projection" } else { "" };
    let (
        action_kind,
        title,
        primary_cli,
        primary_mcp_tool,
        requires_operator_evidence,
        safety_note,
        preflight_checks,
        followup_verification,
    ) = match gate.name.as_str() {
            "context_envelope_evidence" => (
                "inspect_context_evidence",
                "Repair uncited ContextEnvelope projection before cloud use",
                Some("soma context audit".to_string()),
                Some("soma_context_audit"),
                false,
                "Inspect-only step; fix projection/evidence code, then rerun hardening.",
                vec![control_check(
                    "capture_current_audit_failure",
                    "Capture the current ContextEnvelope audit failure before editing projection code",
                    Some("soma context audit"),
                    Some("soma_context_audit"),
                    "audit output identifies missing section or top-level evidence refs",
                )],
                vec![
                    control_check(
                        "rerun_context_audit",
                        "Rerun the ContextEnvelope audit",
                        Some("soma context audit"),
                        Some("soma_context_audit"),
                        "passed=true and missing evidence arrays are empty",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "context_envelope_evidence gate status becomes pass",
                    ),
                ],
            ),
            "storage_trust_boundary" => (
                "inspect_storage_trust",
                "Review trust-boundary violations before any lifecycle transition",
                Some("soma context trust-audit".to_string()),
                Some("soma_trust_boundary_audit"),
                true,
                "Do not promote or consolidate affected claims until independent verification exists.",
                vec![control_check(
                    "inspect_trust_violations",
                    "Inspect trust-boundary violations",
                    Some("soma context trust-audit"),
                    Some("soma_trust_boundary_audit"),
                    "violating claim/proposal ids are visible before any review action",
                )],
                vec![
                    control_check(
                        "rerun_trust_audit",
                        "Rerun trust-boundary audit after review actions",
                        Some("soma context trust-audit"),
                        Some("soma_trust_boundary_audit"),
                        "passed=true with no promoted cloud drafts or untrusted semantic facts",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "storage_trust_boundary gate status becomes pass",
                    ),
                ],
            ),
            "review_backlog_clear" => (
                "review_pending_work",
                "Render semantic review controls for operator verification",
                Some(format!("soma context review-render --client {client_arg} --format json")),
                Some("soma_review_render"),
                true,
                "The render plan is read-only; any verification must go through control-bound review actions with independent non-cloud evidence.",
                vec![
                    control_check(
                        "render_learning_status",
                        "Render semantic learning review status",
                        Some("soma learning --json"),
                        None,
                        "semantic_review_status, cloud_draft blockers, L4 candidates, and review-only belief signals are visible",
                    ),
                    control_check_with_mcp_arguments(
                        "render_semantic_review_controls",
                        "Render client semantic review controls",
                        Some(format!("soma context review-render --client {client_arg} --format json")),
                        Some("soma_review_render"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        "cloud_draft blocker or semantic review action controls are visible before any mutation",
                    ),
                    control_check(
                        "render_review_report",
                        "Render pending review work read-only",
                        Some("soma context review-report --format json"),
                        Some("soma_review_report"),
                        "pending claims/proposals and decision packets are visible",
                    ),
                    control_check(
                        "render_review_actions",
                        "Render review action templates read-only",
                        Some("soma context review-actions --format json"),
                        Some("soma_review_actions"),
                        "enabled actions require user/tool/test/local/correction evidence and reject cloud_draft as evidence",
                    ),
                    control_check(
                        "preflight_safe_drain",
                        "Preview safe drain before any batch apply",
                        Some("soma context review-drain --dry-run"),
                        Some("soma_review_drain"),
                        "dry-run shows only verified non-destructive promotions are eligible",
                    ),
                ],
                vec![
                    control_check(
                        "rerun_review_queue",
                        "Rerun review queue after explicit review actions",
                        Some("soma context review-queue --format json"),
                        Some("soma_review_queue"),
                        "pending_review_count is zero when release requires a clear queue",
                    ),
                    control_check(
                        "rerun_learning_status",
                        "Rerun semantic learning status after review actions",
                        Some("soma learning --json"),
                        None,
                        "semantic_review_status is clear before claiming L3/L4 learning is caught up",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report --require-review-queue-clear"),
                        Some("soma_product_hardening_report"),
                        "review_backlog_clear gate status becomes pass",
                    ),
                ],
            ),
            "review_interaction_contract" => (
                "repair_review_controls",
                "Repair review controls before accepting operator actions",
                Some(format!("soma context review-render --client {client_arg}")),
                Some("soma_review_render"),
                false,
                "Do not expose mutating review actions until rendered control ids and templates pass.",
                vec![control_check(
                    "render_client_review_contract",
                    "Render the client review interaction contract",
                    Some(format!("soma context review-render --client {client_arg}")),
                    Some("soma_review_render"),
                    "every enabled action has a stable control_id and submit template",
                )],
                vec![
                    control_check(
                        "rerun_review_render",
                        "Rerun review render",
                        Some(format!("soma context review-render --client {client_arg}")),
                        Some("soma_review_render"),
                        "interaction_contract.passed=true",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "review_interaction_contract gate status becomes pass",
                    ),
                ],
            ),
            "review_control_binding_manifest" => (
                "repair_review_control_bindings",
                "Repair visible review control bindings before accepting operator actions",
                Some(format!("soma context review-render --client {client_arg} --format json")),
                Some("soma_review_render"),
                false,
                "Do not submit review actions from a client until every visible control id matches the manifest and submit template.",
                vec![control_check(
                    "render_control_binding_manifest",
                    "Render the client control binding manifest",
                    Some(format!("soma context review-render --client {client_arg} --format json")),
                    Some("soma_review_render"),
                    "control_binding_manifest.expected_control_ids match interaction_contract.actions control ids",
                )],
                vec![
                    control_check(
                        "rerun_review_render",
                        "Rerun review render",
                        Some(format!("soma context review-render --client {client_arg} --format json")),
                        Some("soma_review_render"),
                        "control_binding_manifest bindings match actions and required DOM attributes",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "review_control_binding_manifest gate status becomes pass",
                    ),
                ],
            ),
            "task_frame_retention_hygiene" => (
                "inspect_task_frame_retention",
                "Inspect stale unreferenced TaskFrames before cleanup",
                Some("soma context task-frames retention --dry-run".to_string()),
                None,
                true,
                "Cleanup is explicit and must prune only old unreferenced TaskFrames.",
                vec![control_check(
                    "inspect_retention_candidates",
                    "Inspect stale unreferenced TaskFrame candidates",
                    Some("soma context task-frames retention --dry-run"),
                    None,
                    "only unreferenced stale TaskFrames are listed as cleanup candidates",
                )],
                vec![
                    control_check(
                        "rerun_retention_dry_run",
                        "Rerun TaskFrame retention dry-run",
                        Some("soma context task-frames retention --dry-run"),
                        None,
                        "eligible_count is zero or only intentionally retained frames remain",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "task_frame_retention_hygiene gate status becomes pass",
                    ),
                ],
            ),
            "task_frame_projection" => (
                "audit_task_frame_projection",
                "Audit TaskFrame privacy projection before cloud handoff",
                Some(format!("soma context audit --task-frame-id {task_frame_arg}")),
                Some("soma_context_audit"),
                false,
                "Do not send TaskFrame content to cloud until redaction/projection passes.",
                vec![control_check(
                    "audit_task_frame_projection",
                    "Audit the exact TaskFrame projection intended for cloud handoff",
                    Some(format!("soma context audit --task-frame-id {task_frame_arg}")),
                    Some("soma_context_audit"),
                    "projection has no blocked labels or secret-like values",
                )],
                vec![control_check(
                    "rerun_product_hardening_with_task_frame",
                    "Rerun product hardening with the same TaskFrame id",
                    Some(format!(
                        "soma context hardening-report --task-frame-id {task_frame_arg}{task_frame_projection_required_flag}"
                    )),
                    Some("soma_product_hardening_report"),
                    "task_frame_projection gate status becomes pass",
                )],
            ),
            "latent_interface_packet" => (
                "audit_latent_interface",
                "Keep latent interface packets inspectable and vector-free",
                Some("soma context latent-packet".to_string()),
                Some("soma_latent_packet"),
                false,
                "Current cloud channels may receive textual fallback only, not raw vectors or hidden state.",
                vec![control_check(
                    "render_latent_packet",
                    "Render the latent interface packet",
                    Some("soma context latent-packet"),
                    Some("soma_latent_packet"),
                    "packet is schema v1, textual fallback is present, and raw vector/hidden-state fields are absent",
                )],
                vec![control_check(
                    "rerun_product_hardening",
                    "Rerun product hardening",
                    Some("soma context hardening-report"),
                    Some("soma_product_hardening_report"),
                    "latent_interface_packet gate status becomes pass",
                )],
            ),
            "checked_in_eval_reports" => (
                "refresh_checked_in_evals",
                "Refresh checked-in eval reports before release",
                Some("tools/smoke-all.sh".to_string()),
                None,
                false,
                "Eval refresh checks are external release evidence; the hardening report only reads bundled reports.",
                vec![
                    control_check(
                        "check_trust_loop_eval_report",
                        "Check the trust-loop eval report",
                        Some("tools/context-trust-loop-eval.sh --check-docs-report"),
                        None,
                        "context-trust-loop report matches current generated output",
                    ),
                    control_check(
                        "check_semantic_learning_eval_report",
                        "Check the semantic-learning quality report",
                        Some("tools/semantic-learning-quality-eval.sh --check-docs-report"),
                        None,
                        "semantic-learning quality report matches current generated output",
                    ),
                    control_check(
                        "check_client_integration_eval_report",
                        "Check the client-integration report",
                        Some("tools/client-integration-eval.sh --check-docs-report"),
                        None,
                        "client-integration report matches current generated output",
                    ),
                ],
                vec![
                    control_check(
                        "rerun_smoke_all",
                        "Rerun the full release smoke",
                        Some("tools/smoke-all.sh"),
                        None,
                        "smoke-all reports all eval report checks green",
                    ),
                    control_check(
                        "rerun_product_hardening",
                        "Rerun product hardening",
                        Some("soma context hardening-report"),
                        Some("soma_product_hardening_report"),
                        "checked_in_eval_reports gate status becomes pass",
                    ),
                ],
            ),
            "client_binding_readiness" => (
                "collect_client_binding_proof",
                "Collect real client proof session",
                Some(format!(
                    "{soma_bin} adapter-binding-proof --client {client_arg} --proof-session"
                )),
                Some("soma_client_binding_proof_session"),
                true,
                "Only operator-confirmed real app-hook, in-client render, and review-action evidence can prove private-client readiness.",
                vec![
                    control_check_with_mcp_arguments(
                        "render_client_binding_proof_session",
                        "Render the client binding proof session",
                        Some(format!(
                            "{soma_bin} adapter-binding-proof --client {client_arg} --proof-session"
                        )),
                        Some("soma_client_binding_proof_session"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        format!(
                            "proof_session exposes release_gate, next_step_id, pending proof levels, and records no proof row.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "verify_client_binding_proof_session_runbook",
                        "Verify the proof-session operator runbook contract",
                        Some(format!(
                            "{soma_bin} adapter-binding-proof --client {client_arg} --proof-session"
                        )),
                        Some("soma_client_binding_proof_session"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        format!(
                            "proof_session.runbook.schema is soma.client_binding_proof_session_runbook.v1 and runbook.target_next_step_id matches the active proof-session blocker.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "render_proof_free_install_plan",
                        "Render proof-free installed client config",
                        Some(format!(
                            "{soma_bin} adapter-binding-proof --client {client_arg} --render-installed-config"
                        )),
                        Some("soma_client_binding_install_plan"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        format!(
                            "rendered config records no proof row and includes private event_source plus binding_nonce.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "render_in_client_evidence_packet",
                        "Render the in-client render evidence packet template",
                        Some(product_control_render_evidence_packet_cli(client_arg.as_str())),
                        Some("soma_client_render_evidence_packet"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        format!(
                            "template is proof-free until filled with real in-client evidence.{target_clients_note}"
                        ),
                    ),
                ],
                vec![
                    control_check_with_mcp_arguments(
                        "record_app_hook_proof",
                        "Record observed app-hook proof only from real client evidence",
                        Some(product_control_record_proof_cli(
                            client_arg.as_str(),
                            "observed_app_hook",
                        )),
                        Some("soma_client_binding_record_proof"),
                        product_control_record_proof_mcp_arguments(
                            client_arg.as_str(),
                            "observed_app_hook",
                        ),
                        format!(
                            "proof row is operator-confirmed and bound to installed config, private event_source, binding_nonce, writer metadata, and event time.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "record_in_client_render_proof",
                        "Record observed in-client render proof only from structured render evidence",
                        Some(product_control_record_proof_cli(
                            client_arg.as_str(),
                            "observed_in_client_render",
                        )),
                        Some("soma_client_binding_record_proof"),
                        product_control_record_proof_mcp_arguments(
                            client_arg.as_str(),
                            "observed_in_client_render",
                        ),
                        format!(
                            "proof row is operator-confirmed and bound to review-render fingerprint, workbench version, interaction version, and rendered control ids.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "record_review_action_proof",
                        "Record observed review-action proof only from rendered control evidence",
                        Some(product_control_record_proof_cli(
                            client_arg.as_str(),
                            "observed_review_action",
                        )),
                        Some("soma_client_binding_record_proof"),
                        product_control_record_proof_mcp_arguments(
                            client_arg.as_str(),
                            "observed_review_action",
                        ),
                        format!(
                            "proof row is operator-confirmed and bound to a prior rendered control_id plus storage-gated non-cloud review-action report.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "rerun_client_binding_status",
                        "Rerun client binding readiness status",
                        Some(format!(
                            "{soma_bin} adapter-binding-proof --client {client_arg} --status"
                        )),
                        Some("soma_client_binding_proofs"),
                        product_control_client_mcp_arguments(client_arg.as_str()),
                        format!(
                            "ready_for_private_client_claim=true with artifact replay clean.{target_clients_note}"
                        ),
                    ),
                    control_check_with_mcp_arguments(
                        "rerun_strict_product_hardening",
                        "Rerun strict product hardening",
                        Some(format!(
                            "soma context hardening-report --client {client_arg} --require-client-binding-ready"
                        )),
                        Some("soma_product_hardening_report"),
                        product_control_strict_hardening_mcp_arguments(client_arg.as_str()),
                        format!(
                            "client_binding_readiness gate status becomes pass.{target_clients_note}"
                        ),
                    ),
                ],
            ),
            _ => (
                "inspect_release_gate",
                "Inspect non-passing product hardening gate",
                gate.recommended_actions.first().cloned(),
                None,
                gate.blocking,
                "Follow the gate recommendation, then rerun product hardening.",
                vec![control_check(
                    "inspect_gate_recommendation",
                    "Inspect the gate recommendation",
                    gate.recommended_actions.first().cloned(),
                    None,
                    "operator can see the failing gate and cited evidence refs",
                )],
                vec![control_check(
                    "rerun_product_hardening",
                    "Rerun product hardening",
                    Some("soma context hardening-report"),
                    Some("soma_product_hardening_report"),
                    "gate status becomes pass or the remaining gap is still explicit",
                )],
            ),
        };
    ProductControlStep {
        priority,
        gate: gate.name.clone(),
        gate_status: gate.status.clone(),
        blocking: gate.blocking,
        action_kind: action_kind.to_string(),
        title: title.to_string(),
        target_clients,
        primary_cli,
        primary_mcp_tool: primary_mcp_tool.map(str::to_string),
        primary_mcp_call: product_control_primary_mcp_call(
            primary_mcp_tool,
            gate.name.as_str(),
            client_arg.as_str(),
            task_frame_id,
        ),
        requires_operator_evidence,
        mutates_when_executed: false,
        execution_boundary: "plan_only_no_execution_from_hardening_report".to_string(),
        safety_note: safety_note.to_string(),
        evidence_refs: gate.evidence_refs.clone(),
        preflight_checks,
        followup_verification,
    }
}

fn product_control_primary_mcp_call(
    primary_mcp_tool: Option<&str>,
    gate_name: &str,
    client_arg: &str,
    task_frame_id: Option<i64>,
) -> Option<ProductControlMcpCall> {
    let tool = primary_mcp_tool?;
    let mut arguments = BTreeMap::new();
    match tool {
        "soma_client_binding_proof_session" => {
            arguments.insert("client".to_string(), Value::String(client_arg.to_string()));
        }
        "soma_review_render" => {
            arguments.insert("client".to_string(), Value::String(client_arg.to_string()));
        }
        "soma_context_audit" if gate_name == "task_frame_projection" => {
            if let Some(task_frame_id) = task_frame_id {
                arguments
                    .insert("task_frame_id".to_string(), Value::String(task_frame_id.to_string()));
            }
        }
        _ => {}
    }
    Some(ProductControlMcpCall {
        tool: tool.to_string(),
        arguments,
        trust_boundary:
            "control_plan_primary_mcp_call_is_template_only_and_not_executed_by_hardening_report"
                .to_string(),
    })
}

fn product_control_client_mcp_arguments(client_arg: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("client".to_string(), Value::String(client_arg.to_string()))])
}

fn product_control_strict_hardening_mcp_arguments(client_arg: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("client".to_string(), Value::String(client_arg.to_string())),
        ("require_client_binding_ready".to_string(), Value::Bool(true)),
    ])
}

fn product_control_render_evidence_packet_cli(client_arg: &str) -> String {
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    format!(
        "{soma_bin} adapter-binding-proof --client {client_arg} \
         --manifest <checked-in-or-installed-client-binding-manifest> \
         --render-render-evidence \
         --review-render-report <saved-review-render-report-json> \
         --write-render-evidence <filled-in-client-render-evidence-json>"
    )
}

fn product_control_record_proof_cli(client_arg: &str, proof_level: &str) -> String {
    let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
    let evidence_source = release_grade_evidence_source_template(client_arg, proof_level);
    let common = format!(
        "{soma_bin} adapter-binding-proof --client {client_arg} \
         --manifest <checked-in-or-installed-client-binding-manifest> \
         --proof-level {proof_level} \
         --evidence-source {evidence_source}"
    );
    match proof_level {
        "observed_app_hook" => format!(
            "{common} \
             --installed-config <installed-client-config-path> \
             --event-jsonl <private-adapter-event-jsonl> \
             --drain-report <adapter-drain-report-json> \
             --operator-confirm-real-app-invocation \
             --operator-confirm-release-grade-evidence"
        ),
        "observed_in_client_render" => format!(
            "{common} \
             --installed-config <installed-client-config-path> \
             --review-render-report <saved-review-render-report-json> \
             --render-evidence <filled-in-client-render-evidence-json> \
             --operator-confirm-in-client-render \
             --operator-confirm-release-grade-evidence"
        ),
        "observed_review_action" => format!(
            "{common} \
             --installed-config <installed-client-config-path> \
             --review-action-report <saved-review-action-report-json> \
             --operator-confirm-review-action \
             --operator-confirm-release-grade-evidence"
        ),
        _ => common,
    }
}

fn product_control_record_proof_mcp_arguments(
    client_arg: &str,
    proof_level: &str,
) -> BTreeMap<String, Value> {
    let mut arguments = BTreeMap::from([
        ("client".to_string(), Value::String(client_arg.to_string())),
        (
            "manifest".to_string(),
            Value::String("<checked-in-or-installed-client-binding-manifest>".to_string()),
        ),
        ("proof_level".to_string(), Value::String(proof_level.to_string())),
        (
            "evidence_source".to_string(),
            Value::String(release_grade_evidence_source_template(client_arg, proof_level)),
        ),
    ]);
    match proof_level {
        "observed_app_hook" => {
            arguments
                .insert("operator_confirm_release_grade_evidence".to_string(), Value::Bool(true));
            arguments.insert(
                "installed_config".to_string(),
                Value::String("<installed-client-config-path>".to_string()),
            );
            arguments.insert(
                "event_jsonl".to_string(),
                Value::String("<private-adapter-event-jsonl>".to_string()),
            );
            arguments.insert(
                "drain_report".to_string(),
                Value::String("<adapter-drain-report-json>".to_string()),
            );
            arguments.insert("operator_confirm_real_app_invocation".to_string(), Value::Bool(true));
        }
        "observed_in_client_render" => {
            arguments
                .insert("operator_confirm_release_grade_evidence".to_string(), Value::Bool(true));
            arguments.insert(
                "installed_config".to_string(),
                Value::String("<installed-client-config-path>".to_string()),
            );
            arguments.insert(
                "review_render_report".to_string(),
                Value::String("<saved-review-render-report-json>".to_string()),
            );
            arguments.insert(
                "render_evidence".to_string(),
                Value::String("<filled-in-client-render-evidence-json>".to_string()),
            );
            arguments.insert("operator_confirm_in_client_render".to_string(), Value::Bool(true));
        }
        "observed_review_action" => {
            arguments
                .insert("operator_confirm_release_grade_evidence".to_string(), Value::Bool(true));
            arguments.insert(
                "installed_config".to_string(),
                Value::String("<installed-client-config-path>".to_string()),
            );
            arguments.insert(
                "review_action_report".to_string(),
                Value::String("<saved-review-action-report-json>".to_string()),
            );
            arguments.insert("operator_confirm_review_action".to_string(), Value::Bool(true));
        }
        _ => {}
    }
    arguments
}

fn product_control_check_mcp_call(
    mcp_tool: Option<&str>,
    arguments: BTreeMap<String, Value>,
) -> Option<ProductControlMcpCall> {
    mcp_tool.map(|tool| ProductControlMcpCall {
        tool: tool.to_string(),
        arguments,
        trust_boundary:
            "control_plan_check_mcp_call_is_template_only_and_not_executed_by_hardening_report"
                .to_string(),
    })
}

fn control_check(
    check_id: &str,
    title: &str,
    cli: Option<impl Into<String>>,
    mcp_tool: Option<&str>,
    expected: impl Into<String>,
) -> ProductControlCheck {
    control_check_with_mcp_arguments(check_id, title, cli, mcp_tool, BTreeMap::new(), expected)
}

fn control_check_with_mcp_arguments(
    check_id: &str,
    title: &str,
    cli: Option<impl Into<String>>,
    mcp_tool: Option<&str>,
    mcp_arguments: BTreeMap<String, Value>,
    expected: impl Into<String>,
) -> ProductControlCheck {
    ProductControlCheck {
        check_id: check_id.to_string(),
        title: title.to_string(),
        cli: cli.map(Into::into),
        mcp_tool: mcp_tool.map(str::to_string),
        mcp_call: product_control_check_mcp_call(mcp_tool, mcp_arguments),
        expected: expected.into(),
    }
}

pub fn audit_review_backlog(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<ReviewBacklogHardeningAudit, StorageError> {
    let limit = limit.max(1);
    let queue = build_review_queue(
        storage,
        ReviewQueueInput {
            project: project.map(str::to_string),
            session_id: session_id.map(str::to_string),
            limit,
        },
    )?;
    let semantic_support_diversity_manual_review_count = queue
        .proposals
        .iter()
        .filter(|item| item.readiness == "semantic_support_diversity_requires_manual_review")
        .count();
    let explicit_review_proposal_count =
        queue.proposals.iter().filter(|item| item.readiness == "explicit_review_required").count();
    let pending_review_count = queue.claim_count + queue.proposal_count;
    let cloud_draft_blocked_count = queue.claim_count;
    let semantic_review_pending_count = pending_review_count;
    let semantic_review_status = if cloud_draft_blocked_count > 0 {
        "blocked_cloud_draft_verification"
    } else if semantic_support_diversity_manual_review_count > 0 {
        "pending_semantic_review"
    } else if explicit_review_proposal_count > 0 {
        "review_only_beliefs"
    } else if queue.proposal_count > 0 || queue.interruption_summary.should_interrupt {
        "pending_semantic_review"
    } else {
        "clear"
    }
    .to_string();
    let semantic_review_primary_surface = if cloud_draft_blocked_count > 0 {
        "review_render"
    } else if queue.interruption_summary.should_interrupt {
        "review_digest"
    } else if pending_review_count > 0 {
        "review_report"
    } else {
        "none"
    }
    .to_string();
    let semantic_review_next_step = match semantic_review_status.as_str() {
        "blocked_cloud_draft_verification" => {
            "Render review controls and record user/tool/test/local/correction verification before any L3/L4 promotion.".to_string()
        }
        "pending_semantic_review" => {
            "Inspect semantic proposals, support diversity, and decision packets before applying or draining review work.".to_string()
        }
        "review_only_beliefs" => {
            "Resolve review-only belief/conflict signals with reviewer evidence before treating them as policy or fact.".to_string()
        }
        _ => "No pending semantic learning review work is visible for this scope.".to_string(),
    };
    let semantic_review_learning_cli_hint =
        scoped_hardening_cli_hint("soma learning --json", project, session_id);
    let semantic_review_render_cli_hint =
        scoped_hardening_cli_hint("soma context review-render --format json", project, session_id);
    let semantic_review_report_cli_hint =
        scoped_hardening_cli_hint("soma context review-report --format json", project, session_id);
    let semantic_review_actions_cli_hint =
        scoped_hardening_cli_hint("soma context review-actions --format json", project, session_id);
    Ok(ReviewBacklogHardeningAudit {
        project: queue.project,
        session_id: queue.session_id,
        limit: queue.limit,
        claim_count: queue.claim_count,
        cloud_draft_blocked_count,
        proposal_count: queue.proposal_count,
        ready_proposal_count: queue.ready_proposal_count,
        manual_review_proposal_count: queue.manual_review_proposal_count,
        missing_verification_count: queue.missing_verification_count,
        semantic_support_diversity_manual_review_count,
        explicit_review_proposal_count,
        semantic_review_pending_count,
        semantic_review_status,
        semantic_review_primary_surface,
        semantic_review_next_step,
        semantic_review_learning_cli_hint,
        semantic_review_render_cli_hint,
        semantic_review_report_cli_hint,
        semantic_review_actions_cli_hint,
        semantic_review_mcp_tools: vec![
            "soma_review_render".to_string(),
            "soma_review_report".to_string(),
            "soma_review_queue".to_string(),
            "soma_review_actions".to_string(),
            "soma_review_action".to_string(),
        ],
        semantic_review_control_contract:
            "semantic_review_hardening_requires_rendered_review_controls_before_client_mutation"
                .to_string(),
        semantic_review_trust_boundary:
            "semantic_review_hardening_is_read_only: mirrors review backlog and learning status only; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and never treats cloud output as durable evidence without independent user/tool/test/local/correction verification"
                .to_string(),
        pending_review_count,
        interruption_should_interrupt: queue.interruption_summary.should_interrupt,
        interruption_reason: queue.interruption_summary.reason,
        next_surface: queue.interruption_summary.next_surface,
        passed: pending_review_count == 0,
    })
}

fn scoped_hardening_cli_hint(
    command: &str,
    project: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let mut hint = command.to_string();
    if let Some(project) = project.filter(|value| !value.trim().is_empty()) {
        hint.push_str(" --project ");
        hint.push_str(project);
    }
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        hint.push_str(" --session-id ");
        hint.push_str(session_id);
    }
    hint
}

#[allow(clippy::too_many_arguments)]
fn build_product_objective_coverage(
    envelope: &ContextEnvelopeAudit,
    storage_trust: &TrustBoundaryStorageAudit,
    review_backlog: &ReviewBacklogHardeningAudit,
    review_interaction: &ReviewInteractionHardeningAudit,
    review_control_binding: &ReviewControlBindingHardeningAudit,
    task_frame_retention: &TaskFrameRetentionHardeningAudit,
    latent_interface: &LatentInterfaceHardeningAudit,
    checked_in_eval_reports: &CheckedInEvalReportsHardeningAudit,
    task_frame: Option<&TaskFrameProjectionAudit>,
    client_binding: Option<&ClientBindingHardeningAudit>,
    requirements: ProductHardeningRequirements,
    failed_gate_count: usize,
    warning_gate_count: usize,
) -> Vec<ProductObjectiveCoverage> {
    vec![
        memory_substrate_coverage(
            envelope,
            storage_trust,
            task_frame_retention,
            requirements.task_frame_retention_clean,
        ),
        trust_boundary_coverage(storage_trust, task_frame, requirements.task_frame_projection),
        local_control_plane_coverage(
            review_backlog,
            review_interaction,
            review_control_binding,
            requirements.review_queue_clear,
        ),
        cloud_local_protocol_coverage(
            task_frame,
            review_interaction,
            review_control_binding,
            latent_interface,
            requirements.task_frame_projection,
        ),
        review_verification_ux_coverage(review_backlog, review_interaction, review_control_binding),
        semantic_learning_coverage(storage_trust, review_backlog, latent_interface),
        evaluation_coverage(
            failed_gate_count,
            warning_gate_count,
            latent_interface,
            checked_in_eval_reports,
        ),
        client_integration_coverage(client_binding, requirements.client_binding_ready),
        product_hardening_coverage(failed_gate_count, warning_gate_count),
    ]
}

fn memory_substrate_coverage(
    envelope: &ContextEnvelopeAudit,
    storage_trust: &TrustBoundaryStorageAudit,
    task_frame_retention: &TaskFrameRetentionHardeningAudit,
    task_frame_retention_clean_required: bool,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "ContextEnvelope.evidence".to_string(),
        "ContextEnvelope.relevant_memory.layer".to_string(),
        "evidence_latent_proxies".to_string(),
        "claim_records".to_string(),
        "verification_events".to_string(),
        "task_frames".to_string(),
        "soma context task-frames retention".to_string(),
    ];
    if !envelope.passed() || !storage_trust.passed() {
        return ProductObjectiveCoverage::fail(
            "memory_substrate",
            format!(
                "Memory substrate has blocking evidence/trust violations: envelope_passed={}, storage_trust_passed={}, memory_tier_compatibility_passed={}.",
                envelope.passed(),
                storage_trust.passed(),
                envelope.memory_tier_compatibility_passed
            ),
            evidence_refs,
        );
    }
    if !task_frame_retention.passed() {
        let summary = format!(
            "Memory substrate is evidence-safe but has {} stale unreferenced TaskFrame retention candidate(s).",
            task_frame_retention.eligible_count
        );
        if task_frame_retention_clean_required {
            return ProductObjectiveCoverage::fail("memory_substrate", summary, evidence_refs);
        }
        return ProductObjectiveCoverage::warn("memory_substrate", summary, evidence_refs);
    }
    ProductObjectiveCoverage::pass(
        "memory_substrate",
        format!(
            "Memory substrate evidence, lifecycle projection layers, claim trust, and TaskFrame retention checks passed ({} top-level evidence refs, {} relevant_memory layer value(s), {} checked claims).",
            envelope.top_level_evidence_count,
            envelope.relevant_memory_layer_values.len(),
            storage_trust.checked_claim_count
        ),
        evidence_refs,
    )
}

fn trust_boundary_coverage(
    storage_trust: &TrustBoundaryStorageAudit,
    task_frame: Option<&TaskFrameProjectionAudit>,
    task_frame_projection_required: bool,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "claim_records".to_string(),
        "verification_events".to_string(),
        "learning_critic_proposals".to_string(),
        "TaskFrame.cloud_redacted_json".to_string(),
    ];
    if !storage_trust.passed() {
        return ProductObjectiveCoverage::fail(
            "trust_boundary",
            format!(
                "Trust boundary failed: {} promoted cloud draft claim(s), {} untrusted semantic fact(s), {} applied proposal violation(s).",
                storage_trust.promoted_cloud_draft_without_trust_claim_ids.len(),
                storage_trust.semantic_fact_without_trust_claim_ids.len(),
                storage_trust.applied_promotion_proposals_missing_trust.len()
            ),
            evidence_refs,
        );
    }
    match task_frame {
        Some(audit) if audit.passed() => ProductObjectiveCoverage::pass(
            "trust_boundary",
            "Storage trust and TaskFrame projection privacy checks passed.",
            evidence_refs,
        ),
        Some(audit) => ProductObjectiveCoverage::fail(
            "trust_boundary",
            format!(
                "Storage trust passed, but TaskFrame {} projection privacy failed.",
                audit.task_frame_id
            ),
            evidence_refs,
        ),
        None => {
            let summary =
                "Storage trust passed, but no TaskFrame id was supplied for cloud projection privacy audit.";
            if task_frame_projection_required {
                ProductObjectiveCoverage::fail("trust_boundary", summary, evidence_refs)
            } else {
                ProductObjectiveCoverage::warn("trust_boundary", summary, evidence_refs)
            }
        }
    }
}

fn local_control_plane_coverage(
    review_backlog: &ReviewBacklogHardeningAudit,
    review_interaction: &ReviewInteractionHardeningAudit,
    review_control_binding: &ReviewControlBindingHardeningAudit,
    review_queue_clear_required: bool,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma_review_queue".to_string(),
        "soma_review_report".to_string(),
        "soma_review_render".to_string(),
        "soma_review_render.control_binding_manifest".to_string(),
        "soma_scheduler_run".to_string(),
    ];
    if !review_interaction.passed() {
        return ProductObjectiveCoverage::fail(
            "local_control_plane",
            format!(
                "Local control plane review interaction contract failed: [{}].",
                review_interaction.failed_checks.join(", ")
            ),
            evidence_refs,
        );
    }
    if !review_control_binding.passed() {
        return ProductObjectiveCoverage::fail(
            "local_control_plane",
            format!(
                "Local control plane review control binding manifest failed: [{}].",
                review_control_binding.failed_checks.join(", ")
            ),
            evidence_refs,
        );
    }
    if !review_backlog.passed() {
        let summary = format!(
            "Local control plane has {} pending review item(s) across claims/proposals/manual semantic review.",
            review_backlog.pending_review_count
        );
        if review_queue_clear_required {
            return ProductObjectiveCoverage::fail("local_control_plane", summary, evidence_refs);
        }
        return ProductObjectiveCoverage::warn("local_control_plane", summary, evidence_refs);
    }
    ProductObjectiveCoverage::pass(
        "local_control_plane",
        "Review queue, render contract, control binding manifest, and gated scheduler control checks are release-ready.",
        evidence_refs,
    )
}

fn cloud_local_protocol_coverage(
    task_frame: Option<&TaskFrameProjectionAudit>,
    review_interaction: &ReviewInteractionHardeningAudit,
    review_control_binding: &ReviewControlBindingHardeningAudit,
    latent_interface: &LatentInterfaceHardeningAudit,
    task_frame_projection_required: bool,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "TaskFrame.cloud_redacted_json".to_string(),
        "soma context prompt".to_string(),
        "soma_capture_cloud_output".to_string(),
        "soma_review_render".to_string(),
        "soma_review_render.control_binding_manifest".to_string(),
        "soma_latent_packet".to_string(),
    ];
    if !review_interaction.passed() {
        return ProductObjectiveCoverage::fail(
            "cloud_local_protocol",
            "Cloud-local protocol cannot be release-ready while review controls fail their interaction contract.",
            evidence_refs,
        );
    }
    if !review_control_binding.passed() {
        return ProductObjectiveCoverage::fail(
            "cloud_local_protocol",
            "Cloud-local protocol cannot be release-ready while review control bindings fail the visible-control manifest.",
            evidence_refs,
        );
    }
    if !latent_interface.passed() {
        return ProductObjectiveCoverage::fail(
            "cloud_local_protocol",
            "Cloud-local protocol cannot be release-ready while the advanced latent-interface packet lacks an inspectable vector-free textual fallback.",
            evidence_refs,
        );
    }
    match task_frame {
        Some(audit) if audit.passed() => ProductObjectiveCoverage::pass(
            "cloud_local_protocol",
            format!(
                "Cloud-local protocol has a checked TaskFrame projection ({}), gated review handoff, and vector-free latent packet fallback.",
                audit.task_frame_id
            ),
            evidence_refs,
        ),
        Some(audit) => ProductObjectiveCoverage::fail(
            "cloud_local_protocol",
            format!("TaskFrame {} cloud projection failed before cloud handoff.", audit.task_frame_id),
            evidence_refs,
        ),
        None => {
            let summary =
                "No TaskFrame id was supplied, so this report cannot prove cloud-local handoff privacy for a concrete call.";
            if task_frame_projection_required {
                ProductObjectiveCoverage::fail("cloud_local_protocol", summary, evidence_refs)
            } else {
                ProductObjectiveCoverage::warn("cloud_local_protocol", summary, evidence_refs)
            }
        }
    }
}

fn review_verification_ux_coverage(
    review_backlog: &ReviewBacklogHardeningAudit,
    review_interaction: &ReviewInteractionHardeningAudit,
    review_control_binding: &ReviewControlBindingHardeningAudit,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma_review_report".to_string(),
        "soma_review_digest".to_string(),
        "soma_review_action".to_string(),
        "soma_review_render.control_binding_manifest".to_string(),
        "soma_verify_claim".to_string(),
    ];
    if !review_interaction.passed() {
        return ProductObjectiveCoverage::fail(
            "review_verification_ux",
            format!(
                "Review/verification UX contract failed for client `{}`.",
                review_interaction.client
            ),
            evidence_refs,
        );
    }
    if !review_control_binding.passed() {
        return ProductObjectiveCoverage::fail(
            "review_verification_ux",
            format!(
                "Review/verification UX control binding manifest failed for client `{}`.",
                review_control_binding.client
            ),
            evidence_refs,
        );
    }
    if review_backlog.pending_review_count > 0 {
        return ProductObjectiveCoverage::warn(
            "review_verification_ux",
            format!(
                "Review/verification UX is usable, with {} pending item(s) still requiring operator evidence.",
                review_backlog.pending_review_count
            ),
            evidence_refs,
        );
    }
    ProductObjectiveCoverage::pass(
        "review_verification_ux",
        format!(
            "Review/verification UX contract and control binding manifest passed for client `{}` with no pending review backlog.",
            review_interaction.client
        ),
        evidence_refs,
    )
}

fn semantic_learning_coverage(
    storage_trust: &TrustBoundaryStorageAudit,
    review_backlog: &ReviewBacklogHardeningAudit,
    latent_interface: &LatentInterfaceHardeningAudit,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma learning --json".to_string(),
        "soma_semantic_proposals".to_string(),
        "semantic_fact claim_records".to_string(),
        "soma_review_render".to_string(),
        "soma_review_action".to_string(),
        "correction verification_events".to_string(),
        "soma_latent_packet".to_string(),
    ];
    if !storage_trust.passed() {
        return ProductObjectiveCoverage::fail(
            "semantic_learning",
            "Semantic learning cannot be release-ready while storage trust-boundary audit fails.",
            evidence_refs,
        );
    }
    if !latent_interface.passed() {
        return ProductObjectiveCoverage::fail(
            "semantic_learning",
            "Semantic learning cannot be release-ready while latent interface packets can bypass inspectable evidence-backed proxy refs.",
            evidence_refs,
        );
    }
    if review_backlog.cloud_draft_blocked_count > 0 {
        return ProductObjectiveCoverage::warn(
            "semantic_learning",
            format!(
                "{} cloud_draft claim(s) still require independent user/tool/test/local/correction verification before L3/L4 learning can advance.",
                review_backlog.cloud_draft_blocked_count
            ),
            evidence_refs,
        );
    }
    if review_backlog.semantic_support_diversity_manual_review_count > 0 {
        return ProductObjectiveCoverage::warn(
            "semantic_learning",
            format!(
                "{} semantic L4 proposal(s) still require manual support-diversity review.",
                review_backlog.semantic_support_diversity_manual_review_count
            ),
            evidence_refs,
        );
    }
    if review_backlog.semantic_review_status != "clear" {
        return ProductObjectiveCoverage::warn(
            "semantic_learning",
            format!(
                "Semantic learning review is not clear yet: status={}, pending={} item(s).",
                review_backlog.semantic_review_status, review_backlog.semantic_review_pending_count
            ),
            evidence_refs,
        );
    }
    ProductObjectiveCoverage::pass(
        "semantic_learning",
        format!(
            "Semantic learning trust checks passed across {} semantic fact claim(s); pending manual L4 review is clear.",
            storage_trust.semantic_fact_count
        ),
        evidence_refs,
    )
}

fn evaluation_coverage(
    failed_gate_count: usize,
    warning_gate_count: usize,
    latent_interface: &LatentInterfaceHardeningAudit,
    checked_in_eval_reports: &CheckedInEvalReportsHardeningAudit,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma context audit".to_string(),
        "soma context trust-audit".to_string(),
        "soma context hardening-report".to_string(),
        "soma context latent-packet".to_string(),
        "docs/evals/context-trust-loop-report.json".to_string(),
        "docs/evals/semantic-learning-quality-report.json".to_string(),
        "docs/evals/client-integration-report.json".to_string(),
        "docs/evals/context-ranking-dogfood-report.json".to_string(),
        "tools/smoke-all.sh".to_string(),
    ];
    if !latent_interface.passed() {
        return ProductObjectiveCoverage::fail(
            "evaluation",
            "Runtime hardening evaluation failed the latent-interface packet contract.",
            evidence_refs,
        );
    }
    if !checked_in_eval_reports.passed() {
        return ProductObjectiveCoverage::fail(
            "evaluation",
            format!(
                "Checked-in eval report hardening failed for {} of {} required report(s).",
                checked_in_eval_reports.failed_report_count,
                checked_in_eval_reports.required_report_count
            ),
            evidence_refs,
        );
    }
    if failed_gate_count > 0 {
        ProductObjectiveCoverage::fail(
            "evaluation",
            format!(
                "Runtime hardening evaluation has {} blocking gate failure(s).",
                failed_gate_count
            ),
            evidence_refs,
        )
    } else if warning_gate_count > 0 {
        ProductObjectiveCoverage::warn(
            "evaluation",
            format!(
                "Runtime hardening evaluation has no blocking failures, but {} warning gate(s) still need operator evidence or cleanup.",
                warning_gate_count
            ),
            evidence_refs,
        )
    } else {
        ProductObjectiveCoverage::pass(
            "evaluation",
            format!(
                "Runtime hardening gates and {} checked-in eval report(s) passed across {} case(s); keep smoke-all as release evidence.",
                checked_in_eval_reports.report_count,
                checked_in_eval_reports.total_case_count
            ),
            evidence_refs,
        )
    }
}

fn client_integration_coverage(
    client_binding: Option<&ClientBindingHardeningAudit>,
    client_binding_required: bool,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma_client_binding_proof_session".to_string(),
        "soma.client_binding_proof_session_runbook.v1".to_string(),
        "soma_client_binding_install_plan".to_string(),
        "client_binding_proofs".to_string(),
        "soma adapter-binding-proof --status --json".to_string(),
        "tools/client-binding-smoke.sh".to_string(),
    ];
    match client_binding {
        Some(audit) if audit.proofs_found == 0 || audit.client_count == 0 => {
            let operator_next_commands = client_integration_operator_next_commands(audit);
            let summary = if audit.required_clients.is_empty() {
                "No client binding proof rows were found; app-hook/render readiness is unproven."
                    .to_string()
            } else {
                format!(
                    "No client binding proof rows were found for required client(s) {:?}; app-hook/render readiness is unproven.",
                    audit.required_clients
                )
            };
            if client_binding_required {
                ProductObjectiveCoverage::fail("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            } else {
                ProductObjectiveCoverage::warn("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            }
        }
        Some(audit) if audit.required_scope_has_artifact_or_identity_failure() => {
            let artifact_failure_count = audit.required_scope_artifact_failure_count();
            let coherence_failure_count = audit.required_scope_coherence_failure_count();
            let summary = if audit.required_clients.is_empty() {
                format!(
                    "Client integration has {} changed or missing proof artifact(s) and {} proof identity coherence failure(s).",
                    artifact_failure_count, coherence_failure_count
                )
            } else {
                format!(
                    "Client integration has required-client proof integrity failures for {:?}: {} artifact failure(s), {} identity coherence failure(s).",
                    audit.required_clients, artifact_failure_count, coherence_failure_count
                )
            };
            ProductObjectiveCoverage::fail("client_integration", summary, evidence_refs)
                .with_operator_next_commands(client_integration_operator_next_commands(audit))
        }
        Some(audit) if !audit.required_clients_ready() => {
            let operator_next_commands = client_integration_operator_next_commands(audit);
            let summary = format!(
                "Client integration is not ready for all required clients: required={:?}, missing={:?}, unready={:?}. Open each listed proof-session command before recording app-hook/render/review-action proof.",
                audit.required_clients, audit.missing_required_clients, audit.unready_required_clients
            );
            if client_binding_required {
                ProductObjectiveCoverage::fail("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            } else {
                ProductObjectiveCoverage::warn("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            }
        }
        Some(audit) if audit.has_ready_client() => ProductObjectiveCoverage::pass(
            "client_integration",
            if audit.required_clients.is_empty() {
                format!(
                    "{} client binding row(s) are ready for private-client claims.",
                    audit.ready_client_count
                )
            } else {
                format!(
                    "All required client binding rows are ready for private-client claims ({}/{}).",
                    audit.required_ready_client_count, audit.required_client_count
                )
            },
            evidence_refs,
        ),
        Some(audit) => {
            let operator_next_commands = client_integration_operator_next_commands(audit);
            let summary = format!(
                "Client binding proof exists but is not ready yet ({:?}).",
                audit.readiness_values
            );
            if client_binding_required {
                ProductObjectiveCoverage::fail("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            } else {
                ProductObjectiveCoverage::warn("client_integration", summary, evidence_refs)
                    .with_operator_next_commands(operator_next_commands)
            }
        }
        None => {
            let summary =
                "Client binding readiness was skipped or no client binding audit is available."
                    .to_string();
            if client_binding_required {
                ProductObjectiveCoverage::fail("client_integration", summary, evidence_refs)
            } else {
                ProductObjectiveCoverage::warn("client_integration", summary, evidence_refs)
            }
        }
    }
}

fn client_integration_operator_next_commands(
    audit: &ClientBindingHardeningAudit,
) -> Vec<Vec<String>> {
    audit
        .required_client_proof_matrix
        .iter()
        .filter(|row| row.proof_session_required)
        .map(|row| product_hardening_operator_command(&row.proof_session_cli))
        .filter(|command| !command.is_empty())
        .collect()
}

fn product_hardening_coverage(
    failed_gate_count: usize,
    warning_gate_count: usize,
) -> ProductObjectiveCoverage {
    let evidence_refs = vec![
        "soma_product_hardening_report".to_string(),
        "soma context hardening-report".to_string(),
        "recommended_actions".to_string(),
    ];
    if failed_gate_count > 0 {
        ProductObjectiveCoverage::fail(
            "product_hardening",
            format!("Product hardening has {} blocking gate failure(s).", failed_gate_count),
            evidence_refs,
        )
    } else if warning_gate_count > 0 {
        ProductObjectiveCoverage::warn(
            "product_hardening",
            format!(
                "Product hardening is safe but incomplete: {} warning gate(s) remain.",
                warning_gate_count
            ),
            evidence_refs,
        )
    } else {
        ProductObjectiveCoverage::pass(
            "product_hardening",
            "Product hardening gates are all passing with no recommended follow-up actions.",
            evidence_refs,
        )
    }
}

/// Audit persisted claim/proposal state for trust-boundary regressions.
///
/// This is the storage-wide companion to `audit_context_envelope`: instead of
/// checking one cloud-facing projection, it verifies that recently inspected
/// persisted rows still obey the core boundary rule: cloud output may be useful
/// as draft evidence, but it must not become L3/L4 memory unless trusted
/// verification exists in `verification_events`.
pub fn audit_storage_trust_boundary(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<TrustBoundaryStorageAudit, StorageError> {
    let limit = limit.max(1);
    let claims = storage.recent_claim_records_scoped(project, session_id, limit)?;
    let proposals = storage.learning_critic_proposals_scoped(project, session_id, None, limit)?;
    let unverified_cloud_drafts =
        storage.unverified_cloud_draft_claim_records_scoped(project, session_id, limit)?;

    let mut promoted_cloud_draft_count = 0;
    let mut promoted_cloud_draft_without_trust_claim_ids = Vec::new();
    let mut semantic_fact_count = 0;
    let mut semantic_fact_without_trust_claim_ids = Vec::new();
    let mut semantic_fact_missing_promotion_reason_claim_ids = Vec::new();

    for claim in &claims {
        let has_trust = storage.claim_has_durable_promotion_trust(claim.id)?;
        if claim.source_type == ClaimSourceType::CloudDraft
            && matches!(
                claim.lifecycle_state,
                LifecycleState::LongTermMemory | LifecycleState::SemanticFact
            )
        {
            promoted_cloud_draft_count += 1;
            if !has_trust {
                promoted_cloud_draft_without_trust_claim_ids.push(claim.id);
            }
        }

        if claim.lifecycle_state == LifecycleState::SemanticFact {
            semantic_fact_count += 1;
            if !has_trust {
                semantic_fact_without_trust_claim_ids.push(claim.id);
            }
            if claim.promotion_reason.as_deref().is_none_or(str::is_empty) {
                semantic_fact_missing_promotion_reason_claim_ids.push(claim.id);
            }
        }
    }

    let mut applied_promotion_proposal_count = 0;
    let mut applied_promotion_proposals_missing_trust = Vec::new();
    for proposal in &proposals {
        if proposal.status != LearningCriticProposalStatus::Applied
            || proposal.action != LearningCriticAction::ProposePromotion
            || !matches!(
                proposal.target_lifecycle_state,
                Some(LifecycleState::LongTermMemory | LifecycleState::SemanticFact)
            )
        {
            continue;
        }
        applied_promotion_proposal_count += 1;
        let mut missing_trust_claim_ids = Vec::new();
        for claim_id in &proposal.claim_ids {
            if !storage.claim_has_durable_promotion_trust(*claim_id)? {
                missing_trust_claim_ids.push(*claim_id);
            }
        }
        if !missing_trust_claim_ids.is_empty() {
            applied_promotion_proposals_missing_trust.push(TrustBoundaryProposalViolation {
                proposal_id: proposal.id,
                missing_trust_claim_ids,
            });
        }
    }

    let passed = promoted_cloud_draft_without_trust_claim_ids.is_empty()
        && semantic_fact_without_trust_claim_ids.is_empty()
        && semantic_fact_missing_promotion_reason_claim_ids.is_empty()
        && applied_promotion_proposals_missing_trust.is_empty();

    Ok(TrustBoundaryStorageAudit {
        project: project.map(str::to_string),
        session_id: session_id.map(str::to_string),
        limit,
        checked_claim_count: claims.len(),
        checked_proposal_count: proposals.len(),
        unverified_cloud_draft_count: unverified_cloud_drafts.len(),
        promoted_cloud_draft_count,
        promoted_cloud_draft_without_trust_claim_ids,
        semantic_fact_count,
        semantic_fact_without_trust_claim_ids,
        semantic_fact_missing_promotion_reason_claim_ids,
        applied_promotion_proposal_count,
        applied_promotion_proposals_missing_trust,
        passed,
    })
}

fn audit_context_sections(
    section_name: &str,
    sections: &[ContextSection],
    top_level_evidence: &BTreeSet<String>,
    audit: &mut ContextEnvelopeAudit,
) {
    for (idx, section) in sections.iter().enumerate() {
        audit_context_section(
            &format!("{section_name}[{idx}]"),
            section,
            top_level_evidence,
            audit,
        );
    }
}

fn audit_context_section(
    label: &str,
    section: &ContextSection,
    top_level_evidence: &BTreeSet<String>,
    audit: &mut ContextEnvelopeAudit,
) {
    audit_evidence_refs(label, &section.evidence, top_level_evidence, audit);
}

fn audit_evidence_refs(
    label: &str,
    evidence: &[EvidenceRef],
    top_level_evidence: &BTreeSet<String>,
    audit: &mut ContextEnvelopeAudit,
) {
    audit.section_count += 1;
    if evidence.is_empty() {
        audit.missing_section_evidence.push(label.to_string());
        return;
    }
    audit.evidence_backed_count += 1;
    for evidence_ref in evidence {
        let tag = evidence_tag(evidence_ref);
        if !top_level_evidence.contains(&tag) {
            audit.missing_top_level_evidence.push(format!("{label}:{tag}"));
        }
    }
}

fn evidence_tag(evidence: &EvidenceRef) -> String {
    format!("{}:{}", evidence.kind, evidence.id)
}

fn parse_sensitivity_label(value: &Value) -> Option<SensitivityLabel> {
    serde_json::from_value(value.clone()).ok()
}

fn unsafe_for_cloud_projection(label: SensitivityLabel) -> bool {
    matches!(
        label,
        SensitivityLabel::Secret | SensitivityLabel::NeverSend | SensitivityLabel::Unknown
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelevantMemoryRankingComparison {
    pub query: String,
    pub compared_at_k: usize,
    pub hnsw_episode_ids: Vec<EpisodeId>,
    pub hopfield_episode_ids: Vec<EpisodeId>,
    pub overlap_at_k: usize,
}

impl RelevantMemoryRankingComparison {
    pub fn rankings_differ(&self) -> bool {
        self.hnsw_episode_ids != self.hopfield_episode_ids
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelevantMemoryRankingCase {
    pub query: String,
    pub relevant_episode_ids: Vec<EpisodeId>,
}

impl RelevantMemoryRankingCase {
    pub fn new(query: impl Into<String>, relevant_episode_ids: Vec<EpisodeId>) -> Self {
        Self { query: query.into(), relevant_episode_ids }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelevantMemoryRankingCorpusComparison {
    pub case_count: usize,
    pub scored_case_count: usize,
    pub overlap_case_count: usize,
    pub mean_overlap_ratio_at_k: f32,
    pub rankings_differ_count: usize,
    pub hnsw_better_average_precision_case_count: usize,
    pub hopfield_better_average_precision_case_count: usize,
    pub equal_average_precision_case_count: usize,
    pub relevant_episode_count: usize,
    pub hnsw_result_count_at_k: usize,
    pub hopfield_result_count_at_k: usize,
    pub hnsw_relevant_hits_at_k: usize,
    pub hopfield_relevant_hits_at_k: usize,
    pub hnsw_recall_at_k: f32,
    pub hopfield_recall_at_k: f32,
    pub hnsw_precision_at_k: f32,
    pub hopfield_precision_at_k: f32,
    pub hnsw_mean_reciprocal_rank_at_k: f32,
    pub hopfield_mean_reciprocal_rank_at_k: f32,
    pub hnsw_mean_average_precision_at_k: f32,
    pub hopfield_mean_average_precision_at_k: f32,
    pub hopfield_promotable_by_recall_precision: bool,
    pub cases: Vec<RelevantMemoryRankingComparison>,
}

/// Compare the primary HNSW retrieval path with the opt-in Hopfield backend at
/// the ContextEnvelope boundary.
///
/// ADR 0015 classifies Hopfield as a connected candidate because this is the
/// one cognitive backend that can already change
/// `ContextEnvelope::relevant_memory`: `build_memory_pack` swaps the semantic
/// backend, then the envelope inherits those semantic hits. This helper fixes
/// that contract in code so future P4 work can measure quality instead of
/// relying on training progress or dashboard metrics.
pub fn compare_relevant_memory_rankings(
    storage: Arc<Mutex<Storage>>,
    query: &str,
    base_cfg: PackConfig,
) -> Result<RelevantMemoryRankingComparison, PackError> {
    let mut hnsw_cfg = base_cfg.clone();
    hnsw_cfg.backend = BackendKind::Hnsw;
    let mut hopfield_cfg = base_cfg;
    hopfield_cfg.backend = BackendKind::Hopfield;

    let scope = scope_from_cfg(query, &hnsw_cfg);
    let hnsw_pack = build_memory_pack(storage.clone(), Some(query), hnsw_cfg)?;
    let hnsw_envelope = build_context_envelope(&hnsw_pack, scope.clone());

    let hopfield_pack = build_memory_pack(storage, Some(query), hopfield_cfg)?;
    let hopfield_envelope = build_context_envelope(&hopfield_pack, scope);

    let hnsw_episode_ids = semantic_episode_ids(&hnsw_envelope);
    let hopfield_episode_ids = semantic_episode_ids(&hopfield_envelope);
    let compared_at_k = hnsw_episode_ids.len().min(hopfield_episode_ids.len());
    let hnsw_top: HashSet<EpisodeId> =
        hnsw_episode_ids.iter().take(compared_at_k).copied().collect();
    let overlap_at_k =
        hopfield_episode_ids.iter().take(compared_at_k).filter(|id| hnsw_top.contains(id)).count();

    Ok(RelevantMemoryRankingComparison {
        query: query.to_string(),
        compared_at_k,
        hnsw_episode_ids,
        hopfield_episode_ids,
        overlap_at_k,
    })
}

/// Compare HNSW vs Hopfield across a small corpus of query → expected episode
/// ids cases.
///
/// This is still diagnostic: it does not choose the production backend. Its
/// job is to make the ADR 0015 Hopfield keep condition concrete by measuring
/// the `ContextEnvelope.relevant_memory` field across more than one scoped
/// query.
pub fn compare_relevant_memory_ranking_corpus(
    storage: Arc<Mutex<Storage>>,
    cases: &[RelevantMemoryRankingCase],
    base_cfg: PackConfig,
) -> Result<RelevantMemoryRankingCorpusComparison, PackError> {
    let mut comparisons = Vec::with_capacity(cases.len());
    let mut overlap_case_count = 0_usize;
    let mut overlap_ratio_total = 0.0_f32;
    let mut rankings_differ_count = 0_usize;
    let mut relevant_episode_count = 0_usize;
    let mut hnsw_result_count_at_k = 0_usize;
    let mut hopfield_result_count_at_k = 0_usize;
    let mut hnsw_relevant_hits_at_k = 0_usize;
    let mut hopfield_relevant_hits_at_k = 0_usize;
    let mut hnsw_mrr_total = 0.0_f32;
    let mut hopfield_mrr_total = 0.0_f32;
    let mut hnsw_ap_total = 0.0_f32;
    let mut hopfield_ap_total = 0.0_f32;
    let mut scored_case_count = 0_usize;
    let mut hnsw_better_average_precision_case_count = 0_usize;
    let mut hopfield_better_average_precision_case_count = 0_usize;
    let mut equal_average_precision_case_count = 0_usize;

    for case in cases {
        let comparison =
            compare_relevant_memory_rankings(storage.clone(), &case.query, base_cfg.clone())?;
        if comparison.compared_at_k > 0 {
            overlap_case_count += 1;
            overlap_ratio_total += comparison.overlap_at_k as f32 / comparison.compared_at_k as f32;
        }
        if comparison.rankings_differ() {
            rankings_differ_count += 1;
        }
        hnsw_result_count_at_k += comparison.hnsw_episode_ids.len();
        hopfield_result_count_at_k += comparison.hopfield_episode_ids.len();

        let expected: HashSet<EpisodeId> = case.relevant_episode_ids.iter().copied().collect();
        if !expected.is_empty() {
            scored_case_count += 1;
            relevant_episode_count += expected.len();
            hnsw_relevant_hits_at_k +=
                comparison.hnsw_episode_ids.iter().filter(|id| expected.contains(id)).count();
            hopfield_relevant_hits_at_k +=
                comparison.hopfield_episode_ids.iter().filter(|id| expected.contains(id)).count();
            let hnsw_ap = average_precision_at_k(&comparison.hnsw_episode_ids, &expected);
            let hopfield_ap = average_precision_at_k(&comparison.hopfield_episode_ids, &expected);
            hnsw_ap_total += hnsw_ap;
            hopfield_ap_total += hopfield_ap;
            hnsw_mrr_total += reciprocal_rank_at_k(&comparison.hnsw_episode_ids, &expected);
            hopfield_mrr_total += reciprocal_rank_at_k(&comparison.hopfield_episode_ids, &expected);
            if hopfield_ap > hnsw_ap + f32::EPSILON {
                hopfield_better_average_precision_case_count += 1;
            } else if hnsw_ap > hopfield_ap + f32::EPSILON {
                hnsw_better_average_precision_case_count += 1;
            } else {
                equal_average_precision_case_count += 1;
            }
        }
        comparisons.push(comparison);
    }

    let mean_overlap_ratio_at_k =
        if overlap_case_count == 0 { 0.0 } else { overlap_ratio_total / overlap_case_count as f32 };
    let hnsw_recall_at_k = recall(hnsw_relevant_hits_at_k, relevant_episode_count);
    let hopfield_recall_at_k = recall(hopfield_relevant_hits_at_k, relevant_episode_count);
    let hnsw_precision_at_k = precision(hnsw_relevant_hits_at_k, hnsw_result_count_at_k);
    let hopfield_precision_at_k =
        precision(hopfield_relevant_hits_at_k, hopfield_result_count_at_k);
    let hnsw_mean_reciprocal_rank_at_k = mean(hnsw_mrr_total, scored_case_count);
    let hopfield_mean_reciprocal_rank_at_k = mean(hopfield_mrr_total, scored_case_count);
    let hnsw_mean_average_precision_at_k = mean(hnsw_ap_total, scored_case_count);
    let hopfield_mean_average_precision_at_k = mean(hopfield_ap_total, scored_case_count);
    let hopfield_promotable_by_recall_precision =
        hopfield_recall_at_k > hnsw_recall_at_k && hopfield_precision_at_k > hnsw_precision_at_k;

    Ok(RelevantMemoryRankingCorpusComparison {
        case_count: cases.len(),
        scored_case_count,
        overlap_case_count,
        mean_overlap_ratio_at_k,
        rankings_differ_count,
        hnsw_better_average_precision_case_count,
        hopfield_better_average_precision_case_count,
        equal_average_precision_case_count,
        relevant_episode_count,
        hnsw_result_count_at_k,
        hopfield_result_count_at_k,
        hnsw_relevant_hits_at_k,
        hopfield_relevant_hits_at_k,
        hnsw_recall_at_k,
        hopfield_recall_at_k,
        hnsw_precision_at_k,
        hopfield_precision_at_k,
        hnsw_mean_reciprocal_rank_at_k,
        hopfield_mean_reciprocal_rank_at_k,
        hnsw_mean_average_precision_at_k,
        hopfield_mean_average_precision_at_k,
        hopfield_promotable_by_recall_precision,
        cases: comparisons,
    })
}

fn recall(hits: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        hits as f32 / total as f32
    }
}

fn precision(hits: usize, retrieved: usize) -> f32 {
    if retrieved == 0 {
        0.0
    } else {
        hits as f32 / retrieved as f32
    }
}

fn mean(total: f32, count: usize) -> f32 {
    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

fn reciprocal_rank_at_k(ranked_ids: &[EpisodeId], expected: &HashSet<EpisodeId>) -> f32 {
    ranked_ids
        .iter()
        .position(|id| expected.contains(id))
        .map(|idx| 1.0 / (idx + 1) as f32)
        .unwrap_or(0.0)
}

fn average_precision_at_k(ranked_ids: &[EpisodeId], expected: &HashSet<EpisodeId>) -> f32 {
    if expected.is_empty() {
        return 0.0;
    }
    let mut hits = 0_usize;
    let mut precision_sum = 0.0_f32;
    for (idx, id) in ranked_ids.iter().enumerate() {
        if expected.contains(id) {
            hits += 1;
            precision_sum += hits as f32 / (idx + 1) as f32;
        }
    }
    precision_sum / expected.len() as f32
}

fn scope_from_cfg(query: &str, cfg: &PackConfig) -> ContextScope {
    match cfg.project_filter.clone() {
        Some(project) => ContextScope::project(project, Some(query.to_string())),
        None => ContextScope::current(Some(query.to_string())),
    }
}

fn semantic_episode_ids(envelope: &ContextEnvelope) -> Vec<EpisodeId> {
    envelope
        .relevant_memory
        .iter()
        .filter(|item| item.layer == "semantic")
        .filter_map(|item| {
            item.evidence
                .iter()
                .find(|evidence| evidence.kind == "episode")
                .and_then(|evidence| evidence.id.parse::<EpisodeId>().ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::envelope::{ContextItem, CONTEXT_ENVELOPE_VERSION};

    #[test]
    fn context_envelope_audit_accepts_cited_claim_backed_stable_facts() {
        let episode = evidence("episode", "42", Some("terminal"));
        let claim = evidence("claim", "7", Some("tool_verified"));
        let envelope = envelope_with_stable_fact(episode.clone(), claim.clone());

        let audit = audit_context_envelope(&envelope);

        assert!(audit.passed());
        assert_eq!(audit.stable_fact_count, 1);
        assert_eq!(audit.stable_facts_with_claim_evidence, 1);
        assert_eq!(audit.relevant_memory_layer_values, vec!["semantic"]);
        assert!(audit.memory_tier_compatibility_passed);
        assert_eq!(audit.legacy_memory_tier_projection_layers, Vec::<String>::new());
        assert_eq!(audit.missing_section_evidence, Vec::<String>::new());
        assert_eq!(audit.missing_top_level_evidence, Vec::<String>::new());
    }

    #[test]
    fn context_envelope_audit_rejects_legacy_memory_tier_projection_layers() {
        let episode = evidence("episode", "42", Some("terminal"));
        let claim = evidence("claim", "7", Some("tool_verified"));
        let mut envelope = envelope_with_stable_fact(episode, claim);
        envelope.relevant_memory[0].layer = "long".to_string();

        let audit = audit_context_envelope(&envelope);

        assert!(!audit.passed());
        assert_eq!(audit.relevant_memory_layer_values, vec!["long"]);
        assert!(!audit.memory_tier_compatibility_passed);
        assert_eq!(audit.legacy_memory_tier_projection_layers, vec!["relevant_memory[0]:long"]);
        assert_eq!(audit.missing_section_evidence, Vec::<String>::new());
        assert_eq!(audit.missing_top_level_evidence, Vec::<String>::new());
    }

    #[test]
    fn context_envelope_audit_rejects_evidence_less_and_uncited_projection() {
        let episode = evidence("episode", "42", Some("terminal"));
        let claim = evidence("claim", "7", Some("tool_verified"));
        let mut envelope = envelope_with_stable_fact(episode, claim);
        envelope.user_policy.push(ContextSection::typed(
            "Prefer local-first verification.".to_string(),
            vec![evidence("episode", "99", Some("user"))],
            "user_policy",
            "active",
            Some(0.9),
        ));
        envelope.open_decisions.push(ContextSection::typed(
            "Unresolved: whether client capture should run for Cursor.".to_string(),
            Vec::new(),
            "open_decision",
            "open",
            None,
        ));
        envelope.stable_facts[0].evidence = vec![evidence("episode", "42", Some("terminal"))];

        let audit = audit_context_envelope(&envelope);

        assert!(!audit.passed());
        assert_eq!(audit.stable_facts_missing_claim_evidence, vec!["stable_facts[0]"]);
        assert_eq!(audit.missing_section_evidence, vec!["open_decisions[0]"]);
        assert_eq!(audit.missing_top_level_evidence, vec!["user_policy[0]:episode:99"]);
    }

    #[test]
    fn artifact_blocked_client_matrix_preserves_probe_next_step() {
        let snapshot = ClientBindingHardeningClientSnapshot {
            client: "cursor".to_string(),
            readiness: "artifact_integrity_failed".to_string(),
            ready_for_private_client_claim: false,
            has_observed_app_hook: true,
            has_observed_in_client_render: true,
            has_observed_review_action: false,
            artifact_failure_count: 2,
            artifact_failures: Vec::new(),
            coherence_failure_count: 0,
            non_release_evidence_source_count: 0,
            non_release_proof_levels: Vec::new(),
        };
        let mut rows = build_required_client_proof_matrix(
            None,
            &["cursor".to_string()],
            std::slice::from_ref(&snapshot),
        );
        let row = rows.first_mut().expect("matrix row");
        assert_eq!(row.operator_next_action_id, "refresh_invalid_client_binding_artifacts");

        row.next_step_id = Some("capture_in_client_render_evidence".to_string());
        refresh_required_client_proof_matrix_operator_action(row);

        assert_eq!(
            row.operator_next_action_id,
            "capture_fresh_render_evidence_for_artifact_repair"
        );
        assert_eq!(row.operator_next_action_label, "Capture fresh render evidence");
        let next_step = product_hardening_client_binding_next_step(row);
        assert!(next_step.contains("artifact replay failure"), "{next_step}");
        assert!(next_step.contains("soma.in_client_render_evidence.v1"), "{next_step}");
        assert!(next_step.contains("visible `cursor` UI"), "{next_step}");
    }

    #[test]
    fn required_client_proof_matrix_mirrors_probed_next_mcp_call() {
        let snapshot = ClientBindingHardeningClientSnapshot {
            client: "cursor".to_string(),
            readiness: "artifact_integrity_failed".to_string(),
            ready_for_private_client_claim: false,
            has_observed_app_hook: true,
            has_observed_in_client_render: true,
            has_observed_review_action: false,
            artifact_failure_count: 1,
            artifact_failures: Vec::new(),
            coherence_failure_count: 0,
            non_release_evidence_source_count: 0,
            non_release_proof_levels: Vec::new(),
        };
        let mut rows = build_required_client_proof_matrix(
            None,
            &["cursor".to_string()],
            std::slice::from_ref(&snapshot),
        );
        let row = rows.first_mut().expect("matrix row");

        attach_required_client_proof_session_probe(
            row,
            Some("capture_in_client_render_evidence".to_string()),
            Some(vec![
                "tools/soma-client-render-proof-prep.sh".to_string(),
                "--client".to_string(),
                "cursor".to_string(),
                "--artifact-dir".to_string(),
                "/tmp/cursor-artifacts".to_string(),
            ]),
            Some("soma_client_render_evidence_packet".to_string()),
            Some(serde_json::json!({
                "client": "cursor",
                "operator_confirm_in_client_render": true,
                "review_render_report": "/tmp/review-render.json"
            })),
            Some("next_mcp_call_is_template_only".to_string()),
        );
        refresh_required_client_proof_matrix_operator_action(row);

        assert_eq!(
            row.proof_session_next_step_id.as_deref(),
            Some("capture_in_client_render_evidence")
        );
        assert!(row.proof_session_next_command.as_ref().is_some_and(|command| {
            command.iter().any(|part| part == "tools/soma-client-render-proof-prep.sh")
                && command.iter().any(|part| part == "--artifact-dir")
        }));
        assert_eq!(
            row.proof_session_next_mcp_tool.as_deref(),
            Some("soma_client_render_evidence_packet")
        );
        assert_eq!(
            row.proof_session_next_mcp_arguments
                .as_ref()
                .and_then(|arguments| arguments["operator_confirm_in_client_render"].as_bool()),
            Some(true)
        );
        assert_eq!(
            row.proof_session_next_mcp_trust_boundary.as_deref(),
            Some("next_mcp_call_is_template_only")
        );
        assert_eq!(
            row.operator_next_action_id,
            "capture_fresh_render_evidence_for_artifact_repair"
        );
    }

    #[test]
    fn hardening_operator_card_mirrors_primary_proof_session_next_mcp_call() {
        let mut rows = build_required_client_proof_matrix(None, &["cursor".to_string()], &[]);
        let row = rows.first_mut().expect("matrix row");
        attach_required_client_proof_session_probe(
            row,
            Some("capture_in_client_render_evidence".to_string()),
            Some(vec![
                "tools/soma-client-render-proof-prep.sh".to_string(),
                "--client".to_string(),
                "cursor".to_string(),
                "--artifact-dir".to_string(),
                "/tmp/cursor-artifacts".to_string(),
            ]),
            Some("soma_client_render_evidence_packet".to_string()),
            Some(serde_json::json!({
                "client": "cursor",
                "operator_confirm_in_client_render": true,
                "review_render_report": "/tmp/review-render.json"
            })),
            Some("mcp_render_evidence_packet_template_only".to_string()),
        );
        refresh_required_client_proof_matrix_operator_action(row);
        let audit = ClientBindingHardeningAudit {
            client: Some("cursor".to_string()),
            required_clients: vec!["cursor".to_string()],
            required_client_proof_matrix: rows,
            proof_session_source: "soma_client_binding_proof_session".to_string(),
            proof_session_runbook_source: "soma_client_binding_proof_session".to_string(),
            proof_session_runbook_schema: "soma.client_binding_proof_session_runbook.v1"
                .to_string(),
            proof_session_runbook_required: true,
            proof_session_runbook_next_step_id: Some("capture_in_client_render_evidence".into()),
            proof_session_status: "blocked_by_stored_proof_integrity_or_identity".to_string(),
            proof_session_release_gate: "fail".to_string(),
            proof_session_next_step_id: Some("capture_in_client_render_evidence".into()),
            proof_session_target_clients: vec!["cursor".to_string()],
            proof_session_config_root_probe_hint: None,
            required_client_count: 1,
            required_ready_client_count: 0,
            missing_required_clients: Vec::new(),
            unready_required_clients: vec!["cursor".to_string()],
            proof_limit: 5,
            proofs_found: 0,
            client_count: 1,
            ready_client_count: 0,
            all_latest_artifacts_verified: false,
            artifact_failure_count: 1,
            coherence_failure_count: 0,
            non_release_evidence_source_count: 0,
            non_release_proof_levels: Vec::new(),
            primary_readiness: Some("artifact_integrity_failed".to_string()),
            primary_coherence_failures: Vec::new(),
            primary_non_release_evidence_sources: Vec::new(),
            readiness_values: vec!["artifact_integrity_failed".to_string()],
        };
        let control_plan = ProductControlPlan {
            source: "test".to_string(),
            policy: "test".to_string(),
            trust_boundary: "test".to_string(),
            ready: false,
            step_count: 1,
            blocking_step_count: 1,
            operator_evidence_step_count: 1,
            steps: vec![ProductControlStep {
                priority: 0,
                gate: "client_binding_readiness".to_string(),
                gate_status: "fail".to_string(),
                blocking: true,
                action_kind: "verify_client_binding_proof_session_runbook".to_string(),
                title: "Verify client binding".to_string(),
                target_clients: vec!["cursor".to_string()],
                primary_cli: None,
                primary_mcp_tool: None,
                primary_mcp_call: None,
                requires_operator_evidence: true,
                mutates_when_executed: false,
                execution_boundary: "test".to_string(),
                safety_note: "test".to_string(),
                evidence_refs: Vec::new(),
                preflight_checks: Vec::new(),
                followup_verification: Vec::new(),
            }],
        };

        let card =
            build_product_hardening_operator_card("fail", &[], &[], &control_plan, Some(&audit));

        assert!(card
            .primary_next_command
            .iter()
            .any(|part| part == "tools/soma-client-render-proof-prep.sh"));
        assert!(card.primary_next_command.iter().any(|part| part == "--artifact-dir"));
        assert!(card
            .primary_next_cli
            .as_deref()
            .is_some_and(|cli| cli.contains("soma-client-render-proof-prep.sh")));
        assert_eq!(
            card.proof_session_next_mcp_tool.as_deref(),
            Some("soma_client_render_evidence_packet")
        );
        let call = card.proof_session_next_mcp_call.as_ref().expect("next MCP call");
        assert_eq!(call.tool, "soma_client_render_evidence_packet");
        assert_eq!(call.arguments["client"].as_str(), Some("cursor"));
        assert_eq!(call.arguments["operator_confirm_in_client_render"].as_bool(), Some(true));
        assert_eq!(call.trust_boundary, "mcp_render_evidence_packet_template_only");
    }

    fn envelope_with_stable_fact(episode: EvidenceRef, claim: EvidenceRef) -> ContextEnvelope {
        ContextEnvelope {
            version: CONTEXT_ENVELOPE_VERSION,
            assembled_at_ns: 0,
            scope: ContextScope::project("SOMA".to_string(), Some("trust boundary".to_string())),
            thread_state: Some(ContextSection::typed(
                "SOMA compiled 1 local episode.".to_string(),
                vec![episode.clone()],
                "thread_state",
                "compiled",
                None,
            )),
            compiler_notes: Vec::new(),
            short_term_candidates: Vec::new(),
            project_experience: Vec::new(),
            relevant_memory: vec![ContextItem {
                text: "Cloud output is only draft evidence until verified.".to_string(),
                evidence: vec![episode.clone()],
                reason: "semantic similarity 0.91".to_string(),
                rank: 1,
                layer: "semantic".to_string(),
                layer_rank: 1,
                similarity: Some(0.91),
                project: Some("SOMA".to_string()),
                session_id: Some("sess".to_string()),
            }],
            stable_facts: vec![ContextSection::typed(
                "Cloud drafts require local/tool/user verification before promotion.".to_string(),
                vec![claim.clone(), episode.clone()],
                "stable_fact",
                "active",
                Some(1.0),
            )],
            user_policy: Vec::new(),
            open_decisions: Vec::new(),
            corrections: Vec::new(),
            evidence: vec![episode, claim],
        }
    }

    fn evidence(kind: &str, id: &str, source: Option<&str>) -> EvidenceRef {
        EvidenceRef {
            kind: kind.to_string(),
            id: id.to_string(),
            source: source.map(str::to_string),
        }
    }
}
