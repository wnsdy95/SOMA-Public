//! Review queue for claim verification and learning proposals.
//!
//! This module is intentionally read-only. It gives operators and MCP clients a
//! single inspection surface for pending trust-boundary work, but all mutation
//! still goes through `verify-claim` and `learning-proposals apply`.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::context::eval::{audit_task_frame_projection, TaskFrameProjectionAudit};
use crate::context::semantic_learning::{
    semantic_readiness_score, semantic_support_diversity, SemanticReadinessScore,
    SemanticSupportDiversity, SEMANTIC_EXACT_GROUP_RULE, SEMANTIC_LATENT_REVIEW_GROUP_RULE,
    SEMANTIC_LATENT_REVIEW_RULE, SEMANTIC_LATENT_REVIEW_SOURCE, SEMANTIC_LEARNING_RULE,
    SEMANTIC_NEGATION_CONFLICT_GROUP_RULE, SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE,
    SEMANTIC_NEGATION_CONFLICT_RULE, SEMANTIC_TOKEN_GROUP_RULE,
};
use crate::memory::beliefs::{BeliefCandidate, BeliefKind};
use crate::storage::{
    EpisodeSource, LearningCriticAction, LearningCriticProposalStatus, LifecycleState,
    ReviewDigestNotificationAckDraft, Storage, StorageError, StoredClaimRecord, StoredEpisode,
    StoredLearningCriticProposal, StoredReviewDigestNotification, StoredVerificationEvent,
    VerificationResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewQueueInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewActionPlanInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub include_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewBatchTemplateInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub action: String,
    pub target_type: Option<String>,
    pub verifier_type: Option<String>,
    pub evidence_kind: Option<String>,
    pub evidence_id: Option<String>,
    pub evidence_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReportInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub include_disabled: bool,
    pub action: String,
    pub target_type: Option<String>,
    pub verifier_type: Option<String>,
    pub evidence_kind: Option<String>,
    pub evidence_id: Option<String>,
    pub evidence_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDigestInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub client: Option<String>,
    pub include_queue_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDigestAckInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub client: Option<String>,
    pub batch_key: Option<String>,
    pub cooldown_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRenderInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub client: Option<String>,
    pub include_disabled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewQueue {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub claim_count: usize,
    pub proposal_count: usize,
    pub ready_proposal_count: usize,
    pub manual_review_proposal_count: usize,
    pub missing_verification_count: usize,
    pub interruption_summary: ReviewInterruptionSummary,
    pub batch_apply_cli_hint: Option<String>,
    pub batch_apply_mcp_tool: Option<String>,
    pub claims: Vec<ClaimReviewItem>,
    pub proposals: Vec<ProposalReviewItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionPlan {
    pub schema: &'static str,
    pub source: String,
    pub status: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub include_disabled: bool,
    pub claim_count: usize,
    pub proposal_count: usize,
    pub semantic_review_resolution_count: usize,
    pub evidence_required_action_count: usize,
    pub action_count: usize,
    pub disabled_action_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_action: Option<ReviewActionPlanPrimaryAction>,
    pub actions: Vec<ReviewActionOption>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionPlanPrimaryAction {
    pub source: &'static str,
    pub action_id: String,
    pub label: String,
    pub target_type: String,
    pub target_id: i64,
    pub control_id: String,
    pub action: String,
    pub requires_evidence: bool,
    pub cli_hint: String,
    pub mcp_tool: String,
    pub mcp_arguments_template: Value,
    pub intent: String,
    pub trust_effect: String,
    pub authorization_boundary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_trust_boundary: Option<String>,
    pub safe_default: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchTemplate {
    pub source: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub action: String,
    pub target_type: String,
    pub operation_count: usize,
    pub excluded_action_count: usize,
    pub requires_evidence_fill: bool,
    pub executable_after_fill: bool,
    pub trust_boundary: String,
    pub batch_tool: String,
    pub cli_hint: String,
    pub ui_hint: ReviewBatchTemplateUiHint,
    pub operations: Vec<ReviewBatchTemplateOperation>,
    pub mcp_arguments_template: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewReport {
    pub source: String,
    pub trust_boundary: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub include_disabled: bool,
    pub queue: ReviewQueue,
    pub action_plan: ReviewActionPlan,
    pub batch_template: ReviewBatchTemplate,
    pub operator_markdown: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigest {
    pub source: String,
    pub trust_boundary: String,
    pub client: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub include_queue_only: bool,
    pub should_notify: bool,
    pub suppressed_by_cooldown: bool,
    pub pending_notification_count: usize,
    pub notification_count: usize,
    pub queue_only_count: usize,
    pub hidden_queue_only_count: usize,
    pub cooldown_remaining_seconds: u64,
    pub digest_signature: String,
    pub notification_state: Option<StoredReviewDigestNotification>,
    pub item_count: usize,
    pub interruption_summary: ReviewInterruptionSummary,
    pub ui_hint: ReviewDigestUiHint,
    pub belief_review: ReviewDigestBeliefReview,
    pub items: Vec<ReviewDigestItem>,
    pub operator_markdown: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestBeliefReview {
    pub source: String,
    pub status: String,
    pub candidate_count: usize,
    pub contradiction_count: usize,
    pub corroboration_count: usize,
    pub group_count: usize,
    pub visible_count: usize,
    pub hidden_count: usize,
    pub hidden_duplicate_count: usize,
    pub workload_summary: ReviewDigestBeliefWorkloadSummary,
    pub next_action: String,
    pub promotion_rule: String,
    pub trust_boundary: String,
    pub items: Vec<ReviewDigestBeliefItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewDigestBeliefWorkloadSummary {
    pub source: String,
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
    pub support_signal_group_count: usize,
    pub support_signal_candidate_count: usize,
    pub context_signal_group_count: usize,
    pub context_signal_candidate_count: usize,
    pub review_only_signal_group_count: usize,
    pub review_only_signal_candidate_count: usize,
    pub noise_group_count: usize,
    pub noise_candidate_count: usize,
    pub primary_group_id: Option<i64>,
    pub next_action: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestBeliefItem {
    pub candidate_id: i64,
    pub kind: String,
    pub score: f32,
    pub evidence: Option<String>,
    pub claim_preview: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub triage_status: String,
    pub noise_risk: String,
    pub review_priority: u8,
    pub triage_reason: String,
    pub episode_a_id: i64,
    pub episode_b_id: i64,
    pub episode_a_preview: String,
    pub episode_b_preview: String,
    pub episode_a_outcome: String,
    pub episode_b_outcome: String,
    pub group_candidate_count: usize,
    pub grouped_candidate_ids: Vec<i64>,
    pub hidden_duplicate_count: usize,
    pub recommended_next_action: String,
    pub resolution_action: ReviewDigestBeliefResolutionAction,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestBeliefResolutionAction {
    pub source: String,
    pub action: String,
    pub label: String,
    pub cli_command: Vec<String>,
    pub mcp_tool: String,
    pub mcp_arguments_template: Value,
    pub evidence_rule: String,
    pub trust_effect: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestUiHint {
    pub client: String,
    pub surface: String,
    pub notification_style: String,
    pub render_as: String,
    pub cadence: String,
    pub cooldown_seconds: u64,
    pub batch_key: String,
    pub primary_tool: String,
    pub queue_tool: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestItem {
    pub proposal_id: i64,
    pub action: String,
    pub target_lifecycle_state: Option<String>,
    pub readiness: String,
    pub digest_card: ReviewDigestItemCard,
    pub decision_packet: ReviewDecisionPacket,
    pub title: String,
    pub body: String,
    pub review_reason: String,
    pub recommended_next_action: String,
    pub interruption_hint: ReviewInterruptionHint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_review: Option<SemanticPromotionReview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_review_card: Option<SemanticReviewCard>,
    pub mcp_tools: Vec<String>,
    pub action_options: Vec<ReviewActionOption>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestItemCard {
    pub source: String,
    pub lane: String,
    pub target: String,
    pub status: String,
    pub blocks_l4_promotion: bool,
    pub projection_path: String,
    pub evidence_rule: String,
    pub accepted_verifier_types: Vec<String>,
    pub forbidden_evidence_sources: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDigestAckReport {
    pub source: String,
    pub trust_boundary: String,
    pub client: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub policy: String,
    pub batch_key: String,
    pub digest_signature: String,
    pub item_count: usize,
    pub pending_notification_count: usize,
    pub cooldown_seconds: u64,
    pub notification_state: StoredReviewDigestNotification,
    pub digest_after_ack: ReviewDigest,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewRenderPlan {
    pub source: String,
    pub trust_boundary: String,
    pub client: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub primary_surface: String,
    pub should_notify: bool,
    pub suppressed_by_cooldown: bool,
    pub action_count: usize,
    pub batch_operation_count: usize,
    pub surfaces: Vec<ReviewRenderSurface>,
    pub mcp_call_order: Vec<ReviewRenderCall>,
    pub wrapper_call_order: Vec<String>,
    pub client_ui: ReviewClientUiModel,
    pub workbench: ReviewWorkbenchModel,
    pub interaction_contract: ReviewInteractionContract,
    pub control_binding_manifest: ReviewControlBindingManifest,
    pub render_evidence_template: ReviewRenderEvidenceTemplate,
    pub digest: ReviewDigest,
    pub action_plan: ReviewActionPlan,
    pub batch_template: ReviewBatchTemplate,
    pub operator_markdown: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewRenderSurface {
    pub name: String,
    pub source: String,
    pub render_as: String,
    pub display: String,
    pub item_count: usize,
    pub client_hint: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewRenderCall {
    pub step: u16,
    pub purpose: String,
    pub tool: String,
    pub arguments: Value,
    pub when: String,
    pub mutation: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewClientUiModel {
    pub client: String,
    pub layout: String,
    pub notification_strategy: String,
    pub binding_status_strategy: String,
    pub action_strategy: String,
    pub batch_strategy: String,
    pub ack_strategy: String,
    pub unsupported_claims: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchModel {
    pub version: String,
    pub source: String,
    pub client: String,
    pub primary_surface: String,
    pub empty_state: String,
    pub safe_default_action: String,
    pub counts: ReviewWorkbenchCounts,
    pub surfaces: Vec<ReviewWorkbenchSurfaceState>,
    pub action_groups: Vec<ReviewWorkbenchActionGroup>,
    pub evidence_policy: ReviewWorkbenchEvidencePolicy,
    pub submission_contract: ReviewWorkbenchSubmissionContract,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchCounts {
    pub pending_claims: usize,
    pub pending_proposals: usize,
    pub pending_notifications: usize,
    pub enabled_actions: usize,
    pub disabled_actions: usize,
    pub evidence_required_actions: usize,
    pub destructive_actions: usize,
    pub batch_operations: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchSurfaceState {
    pub name: String,
    pub display: String,
    pub item_count: usize,
    pub render_as: String,
    pub ack_allowed_after_visible_render: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchActionGroup {
    pub group: String,
    pub order: u16,
    pub button_style: String,
    pub render_as: String,
    pub enabled_count: usize,
    pub disabled_count: usize,
    pub evidence_required_count: usize,
    pub destructive_count: usize,
    pub control_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchEvidencePolicy {
    pub evidence_required_for_actions: Vec<String>,
    pub accepted_verifier_types: Vec<String>,
    pub durable_promotion_verifier_types: Vec<String>,
    pub required_fields: Vec<String>,
    pub forbidden_evidence_sources: Vec<String>,
    pub evidence_form_source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewWorkbenchSubmissionContract {
    pub mutation_tool: String,
    pub batch_tool: String,
    pub dry_run_first_for_batch: bool,
    pub operator_authorization_required: bool,
    pub agent_self_authorization_forbidden: bool,
    pub authorization_boundary: String,
    pub required_pre_submit_checks: Vec<String>,
    pub success_refresh_surfaces: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewInteractionContract {
    pub version: String,
    pub source: String,
    pub client: String,
    pub mutation_boundary: String,
    pub read_only_until: Vec<String>,
    pub empty_state: String,
    pub actions: Vec<ReviewInteractionAction>,
    pub global_guardrails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewControlBindingManifest {
    pub schema: String,
    pub source: String,
    pub client: String,
    pub workbench_version: String,
    pub interaction_contract_version: String,
    pub expected_control_count: usize,
    pub expected_control_ids: Vec<String>,
    pub action_selector: String,
    pub submit_button_selector: String,
    pub argument_template_attribute: String,
    pub evidence_form_selector: String,
    pub required_dom_attributes: Vec<String>,
    pub missing_control_behavior: String,
    pub bindings: Vec<ReviewControlBinding>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewControlBinding {
    pub control_id: String,
    pub target_type: String,
    pub target_id: i64,
    pub action: String,
    pub enabled: bool,
    pub evidence_required: bool,
    pub dom_selector: String,
    pub submit_tool: String,
    pub submit_arguments_template: Value,
    pub pre_submit_checks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewRenderEvidenceTemplate {
    pub schema: String,
    pub source: String,
    pub client: String,
    pub template_kind: String,
    pub expected_workbench_version: String,
    pub expected_interaction_contract_version: String,
    pub expected_surface_names: Vec<String>,
    pub expected_control_ids: Vec<String>,
    pub accepted_sources: Vec<String>,
    pub required_after_visible_render: Vec<String>,
    pub proof_command_template: Vec<String>,
    pub template_json: Value,
    pub trust_boundary: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewInteractionAction {
    pub control_id: String,
    pub target_type: String,
    pub target_id: i64,
    pub action: String,
    pub label: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub group: String,
    pub order: u16,
    pub button_style: String,
    pub icon: String,
    pub evidence_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_result: Option<String>,
    pub accepted_verifier_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref_template: Option<ReviewBatchTemplateEvidenceRef>,
    pub submit_tool: String,
    pub submit_arguments_template: Value,
    pub cli_command_template: String,
    pub operator_authorization_required: bool,
    pub agent_self_authorization_forbidden: bool,
    pub authorization_boundary: String,
    pub pre_submit_checks: Vec<String>,
    pub success_effect: String,
    pub safe_default: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchTemplateOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<i64>,
    pub action: String,
    pub control_id: String,
    pub verifier_type: String,
    pub evidence_ref: ReviewBatchTemplateEvidenceRef,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchTemplateEvidenceRef {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ClaimReviewItem {
    pub claim: StoredClaimRecord,
    pub verification_events: Vec<StoredVerificationEvent>,
    pub durable_promotion_trust: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_frame_projection_audit: Option<TaskFrameProjectionAudit>,
    pub review_reason: String,
    pub recommended_next_action: String,
    pub decision_packet: ReviewDecisionPacket,
    pub cli_hint: String,
    pub mcp_tools: Vec<String>,
    pub action_options: Vec<ReviewActionOption>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProposalReviewItem {
    pub proposal: StoredLearningCriticProposal,
    pub linked_claims: Vec<ProposalClaimReview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_review: Option<SemanticPromotionReview>,
    pub interruption_hint: ReviewInterruptionHint,
    pub missing_verification_claim_ids: Vec<i64>,
    pub readiness: String,
    pub batch_apply_eligible: bool,
    pub review_reason: String,
    pub recommended_next_action: String,
    pub decision_packet: ReviewDecisionPacket,
    pub cli_hint: String,
    pub apply_ready_cli_hint: Option<String>,
    pub mcp_tools: Vec<String>,
    pub action_options: Vec<ReviewActionOption>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDecisionPacket {
    pub target_type: String,
    pub target_id: i64,
    pub priority: String,
    pub status: String,
    pub primary_question: String,
    pub required_evidence: Vec<String>,
    pub blocking_reasons: Vec<String>,
    pub safe_default_action: String,
    pub next_surface: String,
    pub trust_boundary: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionOption {
    pub control_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: i64,
    pub label: String,
    pub intent: String,
    pub trust_effect: String,
    pub requires_evidence: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_template: Option<ReviewVerificationTemplate>,
    pub requires_destructive_confirmation: bool,
    pub operator_authorization_required: bool,
    pub agent_self_authorization_forbidden: bool,
    pub authorization_boundary: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub cli_hint: String,
    pub mcp_tool: String,
    pub mcp_arguments_template: Value,
    pub ui_hint: ReviewActionUiHint,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewVerificationTemplate {
    pub result: String,
    pub accepted_verifier_types: Vec<String>,
    pub durable_promotion_verifier_types: Vec<String>,
    pub evidence_ref_template: ReviewBatchTemplateEvidenceRef,
    pub example_evidence_refs: Vec<ReviewVerificationEvidenceExample>,
    pub operator_checklist: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewVerificationEvidenceExample {
    pub verifier_type: String,
    pub evidence_ref: ReviewBatchTemplateEvidenceRef,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionUiHint {
    pub group: String,
    pub order: u16,
    pub button_style: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<ReviewActionConfirmation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_form: Option<ReviewEvidenceForm>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionConfirmation {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewEvidenceForm {
    pub required: bool,
    pub verifier_type_options: Vec<String>,
    pub result: String,
    pub fields: Vec<ReviewEvidenceField>,
    pub submit_label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewEvidenceField {
    pub name: String,
    pub label: String,
    pub required: bool,
    pub placeholder: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchTemplateUiHint {
    pub form_title: String,
    pub submit_label: String,
    pub dry_run_first: bool,
    pub mutation_tool: String,
    pub allowed_actions: Vec<String>,
    pub target_type: String,
    pub operation_count: usize,
    pub requires_evidence_fill: bool,
    pub evidence_form: ReviewEvidenceForm,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewInterruptionSummary {
    pub policy: String,
    pub should_interrupt: bool,
    pub interrupt_count: usize,
    pub digest_count: usize,
    pub queue_only_count: usize,
    pub next_surface: String,
    pub cadence: String,
    pub cooldown_seconds: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewInterruptionHint {
    pub policy: String,
    pub should_interrupt: bool,
    pub level: String,
    pub surface: String,
    pub cadence: String,
    pub cooldown_seconds: u64,
    pub batch_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProposalClaimReview {
    pub claim_id: i64,
    pub text: Option<String>,
    pub lifecycle_state: Option<String>,
    pub durable_promotion_trust: Option<bool>,
    pub missing: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticPromotionReview {
    pub target_lifecycle_state: String,
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grouping_rule: Option<String>,
    pub representative_claim_ids: Vec<i64>,
    pub support_claim_ids: Vec<i64>,
    pub support_count: usize,
    pub support_evidence_refs: Vec<crate::storage::StoredEvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_diversity: Option<SemanticSupportDiversity>,
    pub readiness_score: SemanticReadinessScore,
    pub required_verification: String,
    pub review_card: SemanticReviewCard,
    pub review_rubric: Vec<SemanticReviewRubricItem>,
    pub review_prompt: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticReviewCard {
    pub source: String,
    pub title: String,
    pub review_state: String,
    pub support_summary: String,
    pub operator_authorization_required: bool,
    pub agent_self_authorization_forbidden: bool,
    pub review_decision_authority: String,
    pub required_resolution_evidence: Vec<String>,
    pub trust_boundary: String,
    pub allowed_actions: Vec<String>,
    pub blocked_actions: Vec<String>,
    pub checklist: Vec<SemanticReviewChecklistItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticReviewChecklistItem {
    pub check_id: String,
    pub status: String,
    pub question: String,
    pub evidence_paths: Vec<String>,
    pub fail_closed_reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticReviewRubricItem {
    pub check_id: String,
    pub question: String,
    pub required_evidence: Vec<String>,
    pub fail_closed_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationTargetInput {
    Claim(i64),
    Proposal(i64),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VerificationTargetResolution {
    pub target_type: String,
    pub target_id: i64,
    pub claim_ids: Vec<i64>,
    pub skipped_claim_ids: Vec<i64>,
    pub proposal: Option<StoredLearningCriticProposal>,
}

pub fn resolve_verification_targets(
    storage: &Storage,
    target: VerificationTargetInput,
    result: VerificationResult,
) -> Result<VerificationTargetResolution, StorageError> {
    match target {
        VerificationTargetInput::Claim(claim_id) => Ok(VerificationTargetResolution {
            target_type: "claim".to_string(),
            target_id: claim_id,
            claim_ids: vec![claim_id],
            skipped_claim_ids: Vec::new(),
            proposal: None,
        }),
        VerificationTargetInput::Proposal(proposal_id) => {
            let proposal = storage.learning_critic_proposal(proposal_id)?.ok_or_else(|| {
                StorageError::Corrupt {
                    detail: format!("learning critic proposal {proposal_id} not found"),
                }
            })?;
            if proposal.claim_ids.is_empty() {
                return Err(StorageError::Corrupt {
                    detail: format!("learning critic proposal {proposal_id} has no claim ids"),
                });
            }
            let mut claim_ids = Vec::new();
            let mut skipped_claim_ids = Vec::new();
            for claim_id in &proposal.claim_ids {
                let already_trusted = storage.claim_has_durable_promotion_trust(*claim_id)?;
                if result == VerificationResult::Confirmed
                    && proposal.action == LearningCriticAction::ProposePromotion
                    && already_trusted
                {
                    skipped_claim_ids.push(*claim_id);
                } else {
                    claim_ids.push(*claim_id);
                }
            }
            Ok(VerificationTargetResolution {
                target_type: "proposal".to_string(),
                target_id: proposal_id,
                claim_ids,
                skipped_claim_ids,
                proposal: Some(proposal),
            })
        }
    }
}

pub fn build_review_queue(
    storage: &Storage,
    input: ReviewQueueInput,
) -> Result<ReviewQueue, StorageError> {
    let limit = input.limit.max(1);
    let claims = storage.unverified_cloud_draft_claim_records_scoped(
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;
    let proposals = storage.open_learning_critic_proposals_scoped(
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;

    let claim_items = claims
        .into_iter()
        .map(|claim| claim_review_item(storage, claim))
        .collect::<Result<Vec<_>, _>>()?;
    let proposal_items = proposals
        .into_iter()
        .map(|proposal| proposal_review_item(storage, proposal))
        .collect::<Result<Vec<_>, _>>()?;
    let ready_proposal_count =
        proposal_items.iter().filter(|item| item.batch_apply_eligible).count();
    let manual_review_proposal_count = proposal_items
        .iter()
        .filter(|item| proposal_requires_manual_review(&item.readiness))
        .count();
    let missing_verification_count =
        claim_items.iter().filter(|item| !item.durable_promotion_trust).count()
            + proposal_items
                .iter()
                .map(|item| item.missing_verification_claim_ids.len())
                .sum::<usize>();
    let interruption_summary = review_interruption_summary(&proposal_items);
    let batch_apply_cli_hint = (ready_proposal_count > 0)
        .then(|| batch_apply_cli_hint(input.project.as_deref(), input.session_id.as_deref()));
    let batch_apply_mcp_tool =
        (ready_proposal_count > 0).then(|| "soma_learning_proposals_apply_ready".to_string());

    Ok(ReviewQueue {
        project: input.project,
        session_id: input.session_id,
        limit,
        claim_count: claim_items.len(),
        proposal_count: proposal_items.len(),
        ready_proposal_count,
        manual_review_proposal_count,
        missing_verification_count,
        interruption_summary,
        batch_apply_cli_hint,
        batch_apply_mcp_tool,
        claims: claim_items,
        proposals: proposal_items,
    })
}

pub fn build_review_action_plan(
    storage: &Storage,
    input: ReviewActionPlanInput,
) -> Result<ReviewActionPlan, StorageError> {
    let queue = build_review_queue(
        storage,
        ReviewQueueInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
        },
    )?;
    let mut all_actions = Vec::new();
    for item in &queue.claims {
        all_actions.extend(item.action_options.iter().cloned());
    }
    for item in &queue.proposals {
        all_actions.extend(item.action_options.iter().cloned());
    }
    let semantic_review_resolution_count = queue
        .proposals
        .iter()
        .filter(|item| item.readiness == "semantic_review_only_candidate_requires_resolution")
        .count();
    let disabled_action_count = all_actions.iter().filter(|action| !action.enabled).count();
    let actions = if input.include_disabled {
        all_actions
    } else {
        all_actions.into_iter().filter(|action| action.enabled).collect()
    };
    let evidence_required_action_count =
        actions.iter().filter(|action| action.requires_evidence).count();
    let status = review_action_plan_status(
        &queue,
        semantic_review_resolution_count,
        actions.len(),
        disabled_action_count,
    );
    let primary_action =
        review_action_plan_primary_action(&actions, semantic_review_resolution_count);
    Ok(ReviewActionPlan {
        schema: "soma.review_action_plan.v1",
        source: "soma_review_queue.action_options".to_string(),
        status,
        project: queue.project,
        session_id: queue.session_id,
        limit: queue.limit,
        include_disabled: input.include_disabled,
        claim_count: queue.claim_count,
        proposal_count: queue.proposal_count,
        semantic_review_resolution_count,
        evidence_required_action_count,
        action_count: actions.len(),
        disabled_action_count,
        primary_action,
        actions,
        trust_boundary:
            "review_action_plan_is_read_only: summarizes enabled review controls and the next operator action only; records no verification event, applies no proposal, writes no semantic_fact, records no client proof, and promotes no cloud draft",
    })
}

fn review_action_plan_status(
    queue: &ReviewQueue,
    semantic_review_resolution_count: usize,
    action_count: usize,
    disabled_action_count: usize,
) -> String {
    if semantic_review_resolution_count > 0 {
        "semantic_review_resolution_pending".to_string()
    } else if queue.claim_count > 0 {
        "claim_verification_pending".to_string()
    } else if queue.proposal_count > 0 {
        "proposal_review_pending".to_string()
    } else if action_count == 0 && disabled_action_count > 0 {
        "blocked_actions_only".to_string()
    } else {
        "clear".to_string()
    }
}

fn review_action_plan_primary_action(
    actions: &[ReviewActionOption],
    semantic_review_resolution_count: usize,
) -> Option<ReviewActionPlanPrimaryAction> {
    let action = if semantic_review_resolution_count > 0 {
        actions.iter().find(|action| {
            action.target_type == "proposal" && action.action == "accept" && action.enabled
        })
    } else {
        actions.iter().find(|action| action.enabled)
    }
    .or_else(|| actions.first())?;
    let safe_default = if action.requires_evidence {
        "collect_independent_user_tool_test_local_or_correction_evidence_before_submit"
    } else if action.action == "wait" {
        "wait_without_mutating_trust"
    } else {
        "inspect_action_before_submit"
    };
    Some(ReviewActionPlanPrimaryAction {
        source: "soma_review_action_plan.primary_action.v1",
        action_id: format!("{}:{}:{}", action.target_type, action.target_id, action.action),
        label: action.label.clone(),
        target_type: action.target_type.clone(),
        target_id: action.target_id,
        control_id: action.control_id.clone(),
        action: action.action.clone(),
        requires_evidence: action.requires_evidence,
        cli_hint: action.cli_hint.clone(),
        mcp_tool: action.mcp_tool.clone(),
        mcp_arguments_template: action.mcp_arguments_template.clone(),
        intent: action.intent.clone(),
        trust_effect: action.trust_effect.clone(),
        authorization_boundary: action.authorization_boundary.clone(),
        verification_trust_boundary: action
            .verification_template
            .as_ref()
            .map(|template| template.trust_boundary.clone()),
        safe_default: safe_default.to_string(),
        trust_boundary:
            "review_action_plan_primary_action_is_read_only: points at one enabled review control only; execution still requires explicit soma_review_action or soma_review_batch with required evidence",
    })
}

pub fn build_review_batch_template(
    storage: &Storage,
    input: ReviewBatchTemplateInput,
) -> Result<ReviewBatchTemplate, StorageError> {
    let normalized_action = input.action.trim().to_ascii_lowercase();
    let normalized_target_type =
        input.target_type.as_deref().unwrap_or("any").trim().to_ascii_lowercase();
    let plan = build_review_action_plan(
        storage,
        ReviewActionPlanInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
            include_disabled: true,
        },
    )?;
    let requires_evidence_fill = input.verifier_type.is_none()
        || input.evidence_kind.is_none()
        || input.evidence_id.is_none();
    let verifier_type = input
        .verifier_type
        .unwrap_or_else(|| "<user|test|tool|local_observation|correction>".to_string());
    let evidence_kind = input.evidence_kind.unwrap_or_else(|| "<kind>".to_string());
    let evidence_id = input.evidence_id.unwrap_or_else(|| "<id>".to_string());

    let mut operations = Vec::new();
    let mut excluded_action_count = 0;
    for action in &plan.actions {
        let target_matches =
            normalized_target_type == "any" || action.target_type == normalized_target_type;
        if is_review_batch_template_action(action)
            && action.action == normalized_action
            && target_matches
        {
            operations.push(review_batch_template_operation(
                action,
                &verifier_type,
                &evidence_kind,
                &evidence_id,
                input.evidence_source.clone(),
            ));
        } else {
            excluded_action_count += 1;
        }
    }

    let operations_value = serde_json::to_value(&operations).unwrap_or_else(|_| json!([]));
    let cli_hint =
        format!("soma context review-batch --dry-run --operations-json '{}'", operations_value);
    let mcp_arguments_template = json!({
        "dry_run": true,
        "operations": operations.clone(),
    });
    let ui_hint = review_batch_template_ui_hint(
        &normalized_action,
        &normalized_target_type,
        operations.len(),
        requires_evidence_fill,
    );

    Ok(ReviewBatchTemplate {
        source: "soma_review_actions.batch_template".to_string(),
        project: plan.project,
        session_id: plan.session_id,
        limit: plan.limit,
        action: normalized_action,
        target_type: normalized_target_type,
        operation_count: operations.len(),
        excluded_action_count,
        requires_evidence_fill,
        executable_after_fill: !requires_evidence_fill,
        trust_boundary:
            "template_is_read_only_and_only_targets_soma_review_batch_verification_events"
                .to_string(),
        batch_tool: "soma_review_batch".to_string(),
        cli_hint,
        ui_hint,
        operations,
        mcp_arguments_template,
    })
}

pub fn build_review_report(
    storage: &Storage,
    input: ReviewReportInput,
) -> Result<ReviewReport, StorageError> {
    let include_disabled = input.include_disabled;
    let queue = build_review_queue(
        storage,
        ReviewQueueInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
        },
    )?;
    let action_plan = build_review_action_plan(
        storage,
        ReviewActionPlanInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
            include_disabled: input.include_disabled,
        },
    )?;
    let batch_template = build_review_batch_template(
        storage,
        ReviewBatchTemplateInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
            action: input.action,
            target_type: input.target_type,
            verifier_type: input.verifier_type,
            evidence_kind: input.evidence_kind,
            evidence_id: input.evidence_id,
            evidence_source: input.evidence_source,
        },
    )?;
    let mut report = ReviewReport {
        source: "soma_review_report".to_string(),
        trust_boundary:
            "review_report_is_read_only_and_never_records_verification_or_applies_proposals"
                .to_string(),
        project: queue.project.clone(),
        session_id: queue.session_id.clone(),
        limit: queue.limit,
        include_disabled,
        queue,
        action_plan,
        batch_template,
        operator_markdown: String::new(),
    };
    report.operator_markdown = render_review_report_markdown(&report);
    Ok(report)
}

pub fn build_review_digest(
    storage: &Storage,
    input: ReviewDigestInput,
) -> Result<ReviewDigest, StorageError> {
    let queue = build_review_queue(
        storage,
        ReviewQueueInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit: input.limit,
        },
    )?;
    let client = normalize_review_digest_client(input.client.as_deref());
    let notification_items = queue
        .proposals
        .iter()
        .filter(|item| item.interruption_hint.should_interrupt)
        .collect::<Vec<_>>();
    let pending_notification_count = notification_items.len();
    let queue_only_count =
        queue.proposals.iter().filter(|item| !item.interruption_hint.should_interrupt).count();
    let digest_signature = review_digest_signature(&notification_items);
    let batch_key = review_digest_batch_key(pending_notification_count);
    let now_ns = now_ns();
    let notification_state = if pending_notification_count > 0 {
        storage.review_digest_notification(
            &client,
            queue.project.as_deref(),
            queue.session_id.as_deref(),
            "l4_semantic_interruption_v1",
            &batch_key,
        )?
    } else {
        None
    };
    let suppressed_by_cooldown = notification_state.as_ref().is_some_and(|state| {
        state.digest_signature == digest_signature && state.cooldown_until_ns > now_ns
    });
    let cooldown_remaining_seconds = notification_state
        .as_ref()
        .filter(|state| suppressed_by_cooldown && state.cooldown_until_ns > now_ns)
        .map(|state| ((state.cooldown_until_ns - now_ns) as u64).div_ceil(1_000_000_000))
        .unwrap_or(0);
    let notification_count = if suppressed_by_cooldown { 0 } else { pending_notification_count };
    let items = queue
        .proposals
        .iter()
        .filter(|item| input.include_queue_only || item.interruption_hint.should_interrupt)
        .map(review_digest_item)
        .collect::<Vec<_>>();
    let belief_review = review_digest_belief_review(storage, &input, input.include_queue_only)?;
    let hidden_queue_only_count = if input.include_queue_only { 0 } else { queue_only_count };
    let should_notify = notification_count > 0;
    let ui_hint = ReviewDigestUiHint {
        client: client.clone(),
        surface: "review_digest".to_string(),
        notification_style: if should_notify {
            "non_blocking_digest"
        } else if suppressed_by_cooldown {
            "suppressed_by_cooldown"
        } else {
            "none"
        }
        .to_string(),
        render_as: "compact_notification_with_expandable_actions".to_string(),
        cadence: queue.interruption_summary.cadence.clone(),
        cooldown_seconds: queue.interruption_summary.cooldown_seconds,
        batch_key: batch_key.clone(),
        primary_tool: "soma_review_digest".to_string(),
        queue_tool: "soma_review_queue".to_string(),
    };
    let mut digest = ReviewDigest {
        source: "soma_review_digest".to_string(),
        trust_boundary:
            "review_digest_is_read_only_and_never_records_verification_or_applies_proposals"
                .to_string(),
        client,
        project: queue.project.clone(),
        session_id: queue.session_id.clone(),
        limit: queue.limit,
        include_queue_only: input.include_queue_only,
        should_notify,
        suppressed_by_cooldown,
        pending_notification_count,
        notification_count,
        queue_only_count,
        hidden_queue_only_count,
        cooldown_remaining_seconds,
        digest_signature,
        notification_state,
        item_count: items.len(),
        interruption_summary: queue.interruption_summary,
        ui_hint,
        belief_review,
        items,
        operator_markdown: String::new(),
    };
    digest.operator_markdown = render_review_digest_markdown(&digest);
    Ok(digest)
}

pub fn acknowledge_review_digest(
    storage: &mut Storage,
    input: ReviewDigestAckInput,
) -> Result<ReviewDigestAckReport, StorageError> {
    let limit = input.limit.max(1);
    let queue = build_review_queue(
        storage,
        ReviewQueueInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit,
        },
    )?;
    let client = normalize_review_digest_client(input.client.as_deref());
    let notification_items = queue
        .proposals
        .iter()
        .filter(|item| item.interruption_hint.should_interrupt)
        .collect::<Vec<_>>();
    let pending_notification_count = notification_items.len();
    let digest_signature = review_digest_signature(&notification_items);
    let batch_key =
        input.batch_key.unwrap_or_else(|| review_digest_batch_key(pending_notification_count));
    let cooldown_seconds =
        input.cooldown_seconds.unwrap_or(queue.interruption_summary.cooldown_seconds);
    let policy = "l4_semantic_interruption_v1".to_string();
    let notification_state =
        storage.upsert_review_digest_notification_ack(&ReviewDigestNotificationAckDraft {
            client: client.clone(),
            project: queue.project.clone(),
            session_id: queue.session_id.clone(),
            policy: policy.clone(),
            batch_key: batch_key.clone(),
            digest_signature: digest_signature.clone(),
            item_count: notification_items.len(),
            notification_count: pending_notification_count,
            cooldown_seconds,
        })?;
    let digest_after_ack = build_review_digest(
        storage,
        ReviewDigestInput {
            project: queue.project.clone(),
            session_id: queue.session_id.clone(),
            limit: queue.limit,
            client: Some(client.clone()),
            include_queue_only: false,
        },
    )?;
    Ok(ReviewDigestAckReport {
        source: "soma_review_digest_ack".to_string(),
        trust_boundary:
            "review_digest_ack_records_notification_state_only_and_never_records_verification_or_applies_proposals"
                .to_string(),
        client,
        project: queue.project,
        session_id: queue.session_id,
        limit: queue.limit,
        policy,
        batch_key,
        digest_signature,
        item_count: notification_items.len(),
        pending_notification_count,
        cooldown_seconds,
        notification_state,
        digest_after_ack,
    })
}

pub fn build_review_render_plan(
    storage: &Storage,
    input: ReviewRenderInput,
) -> Result<ReviewRenderPlan, StorageError> {
    let limit = input.limit.max(1);
    let client = normalize_review_digest_client(input.client.as_deref());
    let digest = build_review_digest(
        storage,
        ReviewDigestInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit,
            client: Some(client.clone()),
            include_queue_only: false,
        },
    )?;
    let action_plan = build_review_action_plan(
        storage,
        ReviewActionPlanInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit,
            include_disabled: input.include_disabled,
        },
    )?;
    let batch_template = build_review_batch_template(
        storage,
        ReviewBatchTemplateInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit,
            action: "confirm".to_string(),
            target_type: Some("any".to_string()),
            verifier_type: None,
            evidence_kind: None,
            evidence_id: None,
            evidence_source: None,
        },
    )?;
    let primary_surface = if digest.should_notify {
        "notification_digest"
    } else if action_plan.action_count > 0 {
        "action_buttons"
    } else {
        "none"
    }
    .to_string();
    let action_count = action_plan.action_count;
    let batch_operation_count = batch_template.operation_count;
    let project = digest.project.clone();
    let session_id = digest.session_id.clone();
    let surfaces = review_render_surfaces(&digest, &action_plan, &batch_template);
    let workbench = review_render_workbench_model(
        &client,
        primary_surface.clone(),
        &digest,
        &action_plan,
        &batch_template,
        &surfaces,
    );
    let interaction_contract = review_render_interaction_contract(&client, &action_plan);
    let control_binding_manifest =
        review_render_control_binding_manifest(&client, &workbench, &interaction_contract);
    let render_evidence_template =
        review_render_evidence_template(&client, &workbench, &interaction_contract);
    let mut plan = ReviewRenderPlan {
        source: "soma_review_render".to_string(),
        trust_boundary:
            "review_render_is_read_only_and_never_records_verification_or_applies_proposals_or_ack"
                .to_string(),
        client: client.clone(),
        project: project.clone(),
        session_id: session_id.clone(),
        limit,
        primary_surface,
        should_notify: digest.should_notify,
        suppressed_by_cooldown: digest.suppressed_by_cooldown,
        action_count,
        batch_operation_count,
        surfaces,
        mcp_call_order: review_render_mcp_call_order(&digest, &action_plan, &batch_template),
        wrapper_call_order: review_render_wrapper_call_order(&client),
        client_ui: review_render_client_ui_model(&client),
        workbench,
        interaction_contract,
        control_binding_manifest,
        render_evidence_template,
        digest,
        action_plan,
        batch_template,
        operator_markdown: String::new(),
    };
    plan.operator_markdown = render_review_render_plan_markdown(&plan);
    Ok(plan)
}

fn review_render_surfaces(
    digest: &ReviewDigest,
    action_plan: &ReviewActionPlan,
    batch_template: &ReviewBatchTemplate,
) -> Vec<ReviewRenderSurface> {
    vec![
        ReviewRenderSurface {
            name: "notification_digest".to_string(),
            source: "soma_review_digest".to_string(),
            render_as: digest.ui_hint.render_as.clone(),
            display: if digest.should_notify {
                "show_non_blocking".to_string()
            } else if digest.suppressed_by_cooldown {
                "suppressed_by_cooldown".to_string()
            } else {
                "hidden".to_string()
            },
            item_count: digest.pending_notification_count,
            client_hint:
                "Render as a compact badge/toast; call ack only after the user-visible digest is rendered."
                    .to_string(),
        },
        ReviewRenderSurface {
            name: "action_buttons".to_string(),
            source: "soma_review_actions".to_string(),
            render_as: "flat_buttons_or_commands".to_string(),
            display: if action_plan.action_count > 0 { "show" } else { "hidden" }.to_string(),
            item_count: action_plan.action_count,
            client_hint:
                "Render enabled action_options directly; disabled actions are preview-only when requested."
                    .to_string(),
        },
        ReviewRenderSurface {
            name: "client_binding_status".to_string(),
            source: "soma_client_binding_proofs".to_string(),
            render_as: "read_only_readiness_badge".to_string(),
            display: "available_on_demand".to_string(),
            item_count: 1,
            client_hint:
                "Render primary_readiness/client_statuses as setup status only; never treat it as verification evidence."
                    .to_string(),
        },
        ReviewRenderSurface {
            name: "verification_batch_form".to_string(),
            source: "soma_review_batch_template".to_string(),
            render_as: "dry_run_first_form".to_string(),
            display: if batch_template.operation_count > 0 {
                "available_after_evidence_fill"
            } else {
                "hidden"
            }
            .to_string(),
            item_count: batch_template.operation_count,
            client_hint:
                "Use the template to prefill soma_review_batch, then require user/tool/local evidence before execution."
                    .to_string(),
        },
        ReviewRenderSurface {
            name: "expanded_review_report".to_string(),
            source: "soma_review_report".to_string(),
            render_as: "expanded_markdown_or_json_panel".to_string(),
            display: if action_plan.claim_count + action_plan.proposal_count > 0 {
                "available_on_demand"
            } else {
                "hidden"
            }
            .to_string(),
            item_count: action_plan.claim_count + action_plan.proposal_count,
            client_hint:
                "Open for operator detail when a compact digest or action row needs evidence context."
                    .to_string(),
        },
    ]
}

fn review_render_workbench_model(
    client: &str,
    primary_surface: String,
    digest: &ReviewDigest,
    action_plan: &ReviewActionPlan,
    batch_template: &ReviewBatchTemplate,
    surfaces: &[ReviewRenderSurface],
) -> ReviewWorkbenchModel {
    let enabled_actions = action_plan.actions.iter().filter(|action| action.enabled).count();
    let evidence_required_actions =
        action_plan.actions.iter().filter(|action| action.requires_evidence).count();
    let destructive_actions = action_plan
        .actions
        .iter()
        .filter(|action| action.requires_destructive_confirmation)
        .count();
    let action_groups = review_workbench_action_groups(&action_plan.actions);

    ReviewWorkbenchModel {
        version: "soma.review_workbench.v1".to_string(),
        source: "soma_review_render.workbench".to_string(),
        client: client.to_string(),
        primary_surface: primary_surface.clone(),
        empty_state: if enabled_actions == 0 {
            "render_no_pending_review_state_and_hide_mutation_controls".to_string()
        } else {
            "render_pending_review_controls_grouped_by_action_group".to_string()
        },
        safe_default_action: if evidence_required_actions > 0 {
            "wait_for_non_cloud_evidence_before_submit".to_string()
        } else if batch_template.operation_count > 0 {
            "run_dry_run_before_batch_apply".to_string()
        } else {
            "read_only_inspection".to_string()
        },
        counts: ReviewWorkbenchCounts {
            pending_claims: action_plan.claim_count,
            pending_proposals: action_plan.proposal_count,
            pending_notifications: digest.pending_notification_count,
            enabled_actions,
            disabled_actions: action_plan.disabled_action_count,
            evidence_required_actions,
            destructive_actions,
            batch_operations: batch_template.operation_count,
        },
        surfaces: surfaces
            .iter()
            .map(|surface| ReviewWorkbenchSurfaceState {
                name: surface.name.clone(),
                display: surface.display.clone(),
                item_count: surface.item_count,
                render_as: surface.render_as.clone(),
                ack_allowed_after_visible_render: surface.name == "notification_digest"
                    && digest.should_notify,
            })
            .collect(),
        action_groups,
        evidence_policy: review_workbench_evidence_policy(action_plan),
        submission_contract: ReviewWorkbenchSubmissionContract {
            mutation_tool: "soma_review_action".to_string(),
            batch_tool: "soma_review_batch".to_string(),
            dry_run_first_for_batch: true,
            operator_authorization_required: true,
            agent_self_authorization_forbidden: true,
            authorization_boundary: review_action_authorization_boundary(),
            required_pre_submit_checks: vec![
                "action_enabled_true".to_string(),
                "control_id_matches_current_enabled_action_option".to_string(),
                "target_id_matches_rendered_review_item".to_string(),
                "operator_authorization_is_explicit".to_string(),
                "agent_self_authorization_is_not_used_as_evidence".to_string(),
                "evidence_source_is_not_cloud_draft".to_string(),
                "verifier_type_is_one_of_template_accepted_verifier_types".to_string(),
            ],
            success_refresh_surfaces: vec![
                "soma_review_actions".to_string(),
                "soma_review_queue".to_string(),
                "soma_context_trust_audit".to_string(),
                "soma_context_task_frames_outcomes".to_string(),
            ],
            trust_boundary:
                "workbench_is_read_only; mutation_requires_soma_review_action_or_soma_review_batch_with_non_cloud_evidence_and_storage_gate_recheck"
                    .to_string(),
        },
    }
}

fn review_workbench_action_groups(
    actions: &[ReviewActionOption],
) -> Vec<ReviewWorkbenchActionGroup> {
    #[derive(Default)]
    struct GroupDraft {
        order: u16,
        button_style: String,
        enabled_count: usize,
        disabled_count: usize,
        evidence_required_count: usize,
        destructive_count: usize,
        control_ids: Vec<String>,
    }

    let mut groups: BTreeMap<String, GroupDraft> = BTreeMap::new();
    for action in actions {
        let group = action.ui_hint.group.clone();
        let entry = groups.entry(group).or_insert_with(|| GroupDraft {
            order: action.ui_hint.order,
            button_style: action.ui_hint.button_style.clone(),
            ..GroupDraft::default()
        });
        entry.order = entry.order.min(action.ui_hint.order);
        if entry.button_style.is_empty() {
            entry.button_style.clone_from(&action.ui_hint.button_style);
        }
        if action.enabled {
            entry.enabled_count += 1;
        } else {
            entry.disabled_count += 1;
        }
        if action.requires_evidence {
            entry.evidence_required_count += 1;
        }
        if action.requires_destructive_confirmation {
            entry.destructive_count += 1;
        }
        entry.control_ids.push(action.control_id.clone());
    }

    groups
        .into_iter()
        .map(|(group, draft)| ReviewWorkbenchActionGroup {
            group,
            order: draft.order,
            button_style: draft.button_style,
            render_as: "button_group_or_command_palette_section".to_string(),
            enabled_count: draft.enabled_count,
            disabled_count: draft.disabled_count,
            evidence_required_count: draft.evidence_required_count,
            destructive_count: draft.destructive_count,
            control_ids: draft.control_ids,
        })
        .collect()
}

fn review_workbench_evidence_policy(
    action_plan: &ReviewActionPlan,
) -> ReviewWorkbenchEvidencePolicy {
    let mut evidence_required_for_actions = action_plan
        .actions
        .iter()
        .filter(|action| action.requires_evidence)
        .map(|action| action.control_id.clone())
        .collect::<Vec<_>>();
    evidence_required_for_actions.sort();
    evidence_required_for_actions.dedup();

    ReviewWorkbenchEvidencePolicy {
        evidence_required_for_actions,
        accepted_verifier_types: review_verifier_type_options(),
        durable_promotion_verifier_types: review_verifier_type_options(),
        required_fields: vec![
            "verifier_type".to_string(),
            "evidence_ref.kind".to_string(),
            "evidence_ref.id".to_string(),
            "evidence_ref.source".to_string(),
        ],
        forbidden_evidence_sources: review_forbidden_evidence_sources(),
        evidence_form_source: "action_options.verification_template".to_string(),
    }
}

fn review_render_mcp_call_order(
    digest: &ReviewDigest,
    action_plan: &ReviewActionPlan,
    batch_template: &ReviewBatchTemplate,
) -> Vec<ReviewRenderCall> {
    let scope = |format: Option<&str>| {
        let mut args = json!({
            "project": digest.project.clone(),
            "session_id": digest.session_id.clone(),
            "limit": digest.limit,
        });
        if let Some(format) = format {
            args["format"] = json!(format);
        }
        args
    };

    vec![
        ReviewRenderCall {
            step: 1,
            purpose: "read compact notification state".to_string(),
            tool: "soma_review_digest".to_string(),
            arguments: {
                let mut args = scope(Some("json"));
                args["client"] = json!(digest.client.clone());
                args["include_queue_only"] = json!(false);
                args
            },
            when: "before rendering review notification surfaces".to_string(),
            mutation: "read_only".to_string(),
            trust_boundary:
                "review_digest_call_is_read_only: records no ack, verification, proposal apply, proof row, or promotion"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 2,
            purpose: "ack rendered digest notification".to_string(),
            tool: "soma_review_digest_ack".to_string(),
            arguments: {
                let mut args = scope(None);
                args["client"] = json!(digest.client.clone());
                args["batch_key"] = json!(digest.ui_hint.batch_key.clone());
                args
            },
            when: "after_client_renders_notification_and_only_if_digest.should_notify_was_true"
                .to_string(),
            mutation: "notification_cooldown_only_after_render".to_string(),
            trust_boundary:
                "review_digest_ack_call_is_notification_cooldown_only: requires visible render first and never verifies claims, promotes drafts, applies proposals, or proves client binding"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 3,
            purpose: "read client binding readiness status".to_string(),
            tool: "soma_client_binding_proofs".to_string(),
            arguments: json!({
                "client": digest.client.clone(),
                "limit": 20,
            }),
            when: "when rendering client setup/readiness status".to_string(),
            mutation: "read_only".to_string(),
            trust_boundary:
                "client_binding_proofs_call_is_read_only: setup status is not claim evidence and cannot verify, promote, apply, or replace observed private-client proof"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 4,
            purpose: "read action buttons and forms".to_string(),
            tool: "soma_review_actions".to_string(),
            arguments: {
                let mut args = scope(Some("json"));
                args["include_disabled"] = json!(action_plan.include_disabled);
                args
            },
            when: "when rendering review controls".to_string(),
            mutation: "read_only".to_string(),
            trust_boundary:
                "review_actions_call_is_read_only: action options are UI controls only and never create verification events or proposal decisions"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 5,
            purpose: "prefill dry-run batch verification form".to_string(),
            tool: "soma_review_batch_template".to_string(),
            arguments: {
                let mut args = scope(None);
                args["action"] = json!(batch_template.action.clone());
                args["target_type"] = json!(batch_template.target_type.clone());
                args
            },
            when: "when offering batch verification controls".to_string(),
            mutation: "read_only".to_string(),
            trust_boundary:
                "review_batch_template_call_is_read_only: renders a dry-run payload template and does not execute verification, promotion, or proposal apply"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 6,
            purpose: "open expanded operator report".to_string(),
            tool: "soma_review_report".to_string(),
            arguments: {
                let mut args = scope(Some("markdown"));
                args["include_disabled"] = json!(action_plan.include_disabled);
                args["action"] = json!(batch_template.action.clone());
                args["target_type"] = json!(batch_template.target_type.clone());
                args
            },
            when: "on demand when a user expands review details".to_string(),
            mutation: "read_only".to_string(),
            trust_boundary:
                "review_report_call_is_read_only: expanded detail is inspection only and cannot serve as durable evidence by itself"
                    .to_string(),
        },
        ReviewRenderCall {
            step: 7,
            purpose: "execute one user-selected review action".to_string(),
            tool: "soma_review_action".to_string(),
            arguments: json!({
                "claim_id": "<claim_id when target_type=claim>",
                "proposal_id": "<proposal_id when target_type=proposal>",
                "action": "<selected action_option.action>",
                "verifier_type": "<user|test|tool|local_observation|correction>",
                "evidence_ref": {
                    "kind": "<evidence kind>",
                    "id": "<evidence id>",
                    "source": "<evidence source>"
                }
            }),
            when: "after explicit user/tool/local verification evidence is available".to_string(),
            mutation: "trusted_review_mutation_only_after_user_or_tool_or_local_evidence"
                .to_string(),
            trust_boundary:
                "review_action_call_is_trusted_mutation_only_after_independent_evidence: storage gates require user, tool, test, local_observation, or correction evidence and reject cloud_draft, review_render_output, and client_binding_status as evidence"
                    .to_string(),
        },
    ]
}

fn review_render_wrapper_call_order(client: &str) -> Vec<String> {
    vec![
        format!(
            "SOMA_REVIEW_CLIENT={client} SOMA_REVIEW_FORMAT=json tools/soma-review-render.sh"
        ),
        format!(
            "soma adapter-binding-proof --status --client {client} --json # read-only readiness"
        ),
        format!(
            "SOMA_REVIEW_CLIENT={client} SOMA_REVIEW_FORMAT=json tools/soma-review-digest.sh"
        ),
        format!(
            "SOMA_REVIEW_CLIENT={client} tools/soma-review-digest-ack.sh # after visible render only"
        ),
        "SOMA_REVIEW_FORMAT=json tools/soma-review-actions.sh".to_string(),
        "tools/soma-review-batch-template.sh # read-only dry-run payload template".to_string(),
        "SOMA_REVIEW_FORMAT=markdown tools/soma-review-report.sh # expanded detail".to_string(),
    ]
}

fn review_render_interaction_contract(
    client: &str,
    action_plan: &ReviewActionPlan,
) -> ReviewInteractionContract {
    ReviewInteractionContract {
        version: "soma.review_interaction_contract.v1".to_string(),
        source: "soma_review_render.interaction_contract".to_string(),
        client: client.to_string(),
        mutation_boundary:
            "rendered_controls_are_read_only_until_soma_review_action_or_soma_review_batch_is_called_with_non_cloud_evidence"
                .to_string(),
        read_only_until: vec![
            "client_has_rendered_the_review_surface_to_the_user".to_string(),
            "operator_or_tool_supplies_independently_inspectable_evidence_ref".to_string(),
            "selected_action_is_enabled_and_storage_gate_rechecks_trust".to_string(),
        ],
        empty_state: if action_plan.action_count == 0 {
            "hide_mutating_controls_and_render_no_pending_review_copy".to_string()
        } else {
            "render_enabled_actions_grouped_by_ui_hint_order".to_string()
        },
        actions: action_plan
            .actions
            .iter()
            .map(review_render_interaction_action)
            .collect(),
        global_guardrails: vec![
            "do_not_submit_cloud_draft_as_evidence".to_string(),
            "do_not_self_authorize_review_mutations_as_the_agent".to_string(),
            "do_not_ack_digest_before_visible_render".to_string(),
            "do_not_treat_client_binding_status_as_claim_verification".to_string(),
            "do_not_promote_l3_or_l4_from_render_plan_output".to_string(),
        ],
    }
}

fn review_render_control_binding_manifest(
    client: &str,
    workbench: &ReviewWorkbenchModel,
    interaction_contract: &ReviewInteractionContract,
) -> ReviewControlBindingManifest {
    let mut expected_control_ids = interaction_contract
        .actions
        .iter()
        .map(|action| action.control_id.clone())
        .collect::<Vec<_>>();
    expected_control_ids.sort();
    expected_control_ids.dedup();
    let bindings = interaction_contract
        .actions
        .iter()
        .map(|action| ReviewControlBinding {
            control_id: action.control_id.clone(),
            target_type: action.target_type.clone(),
            target_id: action.target_id,
            action: action.action.clone(),
            enabled: action.enabled,
            evidence_required: action.evidence_required,
            dom_selector: format!(
                "[data-soma-control-id=\"{}\"]",
                css_attr_selector_escape(&action.control_id)
            ),
            submit_tool: action.submit_tool.clone(),
            submit_arguments_template: action.submit_arguments_template.clone(),
            pre_submit_checks: action.pre_submit_checks.clone(),
        })
        .collect::<Vec<_>>();
    ReviewControlBindingManifest {
        schema: "soma.review_control_binding_manifest.v1".to_string(),
        source: "soma_review_render.control_binding_manifest".to_string(),
        client: client.to_string(),
        workbench_version: workbench.version.clone(),
        interaction_contract_version: interaction_contract.version.clone(),
        expected_control_count: expected_control_ids.len(),
        expected_control_ids,
        action_selector: "[data-soma-review-action=\"true\"]".to_string(),
        submit_button_selector: "[data-soma-submit-control=\"true\"]".to_string(),
        argument_template_attribute: "data-mcp-arguments-template".to_string(),
        evidence_form_selector: "[data-evidence-form=\"required\"]".to_string(),
        required_dom_attributes: vec![
            "data-soma-review-action".to_string(),
            "data-soma-control-id".to_string(),
            "data-submit-tool".to_string(),
            "data-mcp-arguments-template".to_string(),
        ],
        missing_control_behavior:
            "block_submission_and_require_fresh_soma_review_render_before_mutation".to_string(),
        bindings,
        trust_boundary:
            "control_binding_manifest_is_read_only_ui_contract: it records no render proof, verifies no claim, promotes no draft, applies no proposal, and only defines visible control bindings that later render evidence must echo"
                .to_string(),
    }
}

fn review_render_evidence_template(
    client: &str,
    workbench: &ReviewWorkbenchModel,
    interaction_contract: &ReviewInteractionContract,
) -> ReviewRenderEvidenceTemplate {
    let mut expected_control_ids = interaction_contract
        .actions
        .iter()
        .map(|action| action.control_id.clone())
        .collect::<Vec<_>>();
    expected_control_ids.sort();
    expected_control_ids.dedup();
    let mut expected_surface_names = Vec::new();
    if workbench.primary_surface != "none" {
        expected_surface_names.push(workbench.primary_surface.clone());
    }
    if !expected_control_ids.is_empty() {
        expected_surface_names.push("action_buttons".to_string());
    }
    expected_surface_names.sort();
    expected_surface_names.dedup();
    let required_after_visible_render = vec![
        "save_or_capture_the_exact_json_review_render_report_bytes".to_string(),
        "compute_review_render_fingerprint_with_soma_stable_content_fingerprint".to_string(),
        "fill_observed_at_ns_after_the_private_client_visibly_renders_the_surface".to_string(),
        "fill_rendered_surfaces_with_named_visible_client_surfaces_from_expected_surface_names"
            .to_string(),
        "echo_all_expected_control_ids_that_were_bound_to_visible_controls".to_string(),
        "echo_expected_control_ids_inside_the_visible_action_buttons_surface".to_string(),
        "keep_trust_boundary_ui_only_and_do_not_use_this_as_claim_verification".to_string(),
    ];
    let accepted_sources = vec!["manual_operator".to_string(), "client_capture".to_string()];
    let template_json = json!({
        "schema": "soma.in_client_render_evidence.v1",
        "client": client,
        "source": "<manual_operator_or_client_capture>",
        "observed_at_ns": "<positive_unix_epoch_nanoseconds_after_visible_render>",
        "review_render_fingerprint": "<fingerprint_of_saved_review_render_json_report>",
        "review_workbench_version": workbench.version,
        "review_interaction_contract_version": interaction_contract.version,
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
        "trust_boundary": "observed_in_client_render_is_ui_only_and_never_verifies_promotes_applies_or_acknowledges"
    });
    ReviewRenderEvidenceTemplate {
        schema: "soma.in_client_render_evidence.v1".to_string(),
        source: "soma_review_render.render_evidence_template".to_string(),
        client: client.to_string(),
        template_kind: "fill_after_visible_private_client_render".to_string(),
        expected_workbench_version: workbench.version.clone(),
        expected_interaction_contract_version: interaction_contract.version.clone(),
        expected_surface_names,
        expected_control_ids,
        accepted_sources,
        required_after_visible_render,
        proof_command_template: vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--proof-level".to_string(),
            "observed_in_client_render".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--installed-config".to_string(),
            "<installed-client-config>".to_string(),
            "--review-render-report".to_string(),
            "$HOME/.soma/client-evidence/<client>/<run-id>/review-render.json".to_string(),
            "--render-evidence".to_string(),
            "$HOME/.soma/client-evidence/<client>/<run-id>/render-evidence.json".to_string(),
            "--operator-confirm-in-client-render".to_string(),
        ],
        template_json,
        trust_boundary:
            "render_evidence_template_is_read_only_and_not_itself_evidence: placeholders must be filled after visible private-client render; this template records no proof, verifies no claim, promotes no draft, applies no proposal, and acknowledges no digest"
                .to_string(),
    }
}

fn review_render_interaction_action(action: &ReviewActionOption) -> ReviewInteractionAction {
    let evidence_template = action.verification_template.as_ref();
    let mut pre_submit_checks = vec![
        "action_enabled_true".to_string(),
        "control_id_matches_current_enabled_action_option".to_string(),
        "target_id_matches_rendered_review_item".to_string(),
        "user_intent_or_tool_result_is_present_before_mutation".to_string(),
        "operator_authorization_is_explicit".to_string(),
        "agent_self_authorization_is_not_used_as_evidence".to_string(),
    ];
    if action.requires_evidence {
        pre_submit_checks.push("evidence_ref_kind_and_id_are_filled".to_string());
        pre_submit_checks.push("evidence_source_is_not_cloud_draft".to_string());
        pre_submit_checks
            .push("verifier_type_is_one_of_template_accepted_verifier_types".to_string());
    }
    if action.requires_destructive_confirmation {
        pre_submit_checks.push("destructive_confirmation_is_explicit".to_string());
    }

    ReviewInteractionAction {
        control_id: action.control_id.clone(),
        target_type: action.target_type.clone(),
        target_id: action.target_id,
        action: action.action.clone(),
        label: action.label.clone(),
        enabled: action.enabled,
        disabled_reason: action.disabled_reason.clone(),
        group: action.ui_hint.group.clone(),
        order: action.ui_hint.order,
        button_style: action.ui_hint.button_style.clone(),
        icon: action.ui_hint.icon.clone(),
        evidence_required: action.requires_evidence,
        evidence_result: evidence_template.map(|template| template.result.clone()),
        accepted_verifier_types: evidence_template
            .map(|template| template.accepted_verifier_types.clone())
            .unwrap_or_default(),
        evidence_ref_template: evidence_template
            .map(|template| template.evidence_ref_template.clone()),
        submit_tool: action.mcp_tool.clone(),
        submit_arguments_template: action.mcp_arguments_template.clone(),
        cli_command_template: action.cli_hint.clone(),
        operator_authorization_required: action.operator_authorization_required,
        agent_self_authorization_forbidden: action.agent_self_authorization_forbidden,
        authorization_boundary: action.authorization_boundary.clone(),
        pre_submit_checks,
        success_effect: action.trust_effect.clone(),
        safe_default: if action.enabled {
            "wait_until_required_evidence_is_available".to_string()
        } else {
            "render_disabled_or_hide_when_include_disabled_false".to_string()
        },
    }
}

fn review_render_client_ui_model(client: &str) -> ReviewClientUiModel {
    match client {
        "cursor" => ReviewClientUiModel {
            client: client.to_string(),
            layout: "status_bar_badge_plus_command_palette_or_webview".to_string(),
            notification_strategy:
                "show a compact non-blocking badge/toast, then ack only after the badge is rendered"
                    .to_string(),
            binding_status_strategy:
                "show primary_readiness as a setup badge near the status bar; artifact_integrity_failed should prompt proof refresh, not verification"
                    .to_string(),
            action_strategy:
                "render action_options as command-palette buttons with evidence form fields"
                    .to_string(),
            batch_strategy:
                "offer a dry-run-first batch confirmation form for eligible verification actions"
                    .to_string(),
            ack_strategy:
                "call soma_review_digest_ack only after a visible digest render, never on background polling"
                    .to_string(),
            unsupported_claims: vec![
                "private Cursor hook installation is not proven by this render plan".to_string(),
                "rendering a button is not verification evidence".to_string(),
            ],
        },
        "continue" => ReviewClientUiModel {
            client: client.to_string(),
            layout: "sidebar_card_plus_quick_pick".to_string(),
            notification_strategy:
                "show a sidebar card with compact counts and a quick-pick expansion path"
                    .to_string(),
            binding_status_strategy:
                "show client_statuses in the sidebar setup card and keep proof refresh separate from claim review"
                    .to_string(),
            action_strategy:
                "map action_options to quick-pick commands and require evidence fields before mutation"
                    .to_string(),
            batch_strategy:
                "use the batch template as a preview form, then submit soma_review_batch after evidence fill"
                    .to_string(),
            ack_strategy:
                "ack only after the sidebar card is displayed to the user".to_string(),
            unsupported_claims: vec![
                "private Continue hook installation is not proven by this render plan".to_string(),
                "cloud output remains draft until verified".to_string(),
            ],
        },
        "claude-code" => ReviewClientUiModel {
            client: client.to_string(),
            layout: "markdown_panel_plus_mcp_tool_calls".to_string(),
            notification_strategy:
                "render digest markdown or JSON in a non-blocking panel before acking".to_string(),
            binding_status_strategy:
                "include the read-only binding status call in the panel preflight, but never use Claude text as proof"
                    .to_string(),
            action_strategy:
                "show MCP tool-call buttons from action_options and keep evidence explicit"
                    .to_string(),
            batch_strategy:
                "prefer soma_review_batch_template for bulk review, then run dry-run first"
                    .to_string(),
            ack_strategy:
                "ack only after the markdown/JSON review panel has been emitted".to_string(),
            unsupported_claims: vec![
                "MCP read output is not durable memory evidence".to_string(),
                "Claude text is still cloud_draft unless externally verified".to_string(),
            ],
        },
        _ => ReviewClientUiModel {
            client: client.to_string(),
            layout: "json_driven_cards".to_string(),
            notification_strategy:
                "render digest counts as a non-blocking card and ack only after visible render"
                    .to_string(),
            binding_status_strategy:
                "render client binding readiness as a read-only setup card using soma_client_binding_proofs"
                    .to_string(),
            action_strategy:
                "render action_options exactly as JSON-defined controls with evidence forms"
                    .to_string(),
            batch_strategy:
                "offer batch templates as dry-run-first forms".to_string(),
            ack_strategy:
                "never ack on background polling; ack only after a user-visible notification"
                    .to_string(),
            unsupported_claims: vec![
                "client-specific private hook installation is outside this contract".to_string(),
                "read-only review render cannot verify, promote, or apply anything".to_string(),
            ],
        },
    }
}

pub fn render_review_render_plan_markdown(plan: &ReviewRenderPlan) -> String {
    let mut out = String::new();
    out.push_str("# SOMA Review Render Plan\n\n");
    out.push_str(&format!("Source: `{}`\n", plan.source));
    out.push_str(&format!("Trust boundary: {}\n", plan.trust_boundary));
    out.push_str(
        "Mutation path: this render plan is read-only. It never records verification, never applies proposals, and never acknowledges notifications by itself.\n",
    );
    out.push_str(
        "Ack: after render only. Verification/apply: after explicit user/tool/local/correction evidence only.\n\n",
    );
    out.push_str(&format!(
        "Client: {} layout={} primary_surface={}\n",
        plan.client, plan.client_ui.layout, plan.primary_surface
    ));
    out.push_str(&format!(
        "Scope: project={} session={} limit={}\n\n",
        plan.project.as_deref().unwrap_or("*"),
        plan.session_id.as_deref().unwrap_or("*"),
        plan.limit
    ));
    out.push_str(&format!(
        "Counts: should_notify={} suppressed_by_cooldown={} actions={} batch_operations={}\n\n",
        plan.should_notify,
        plan.suppressed_by_cooldown,
        plan.action_count,
        plan.batch_operation_count
    ));

    out.push_str("## Client UI\n\n");
    out.push_str(&format!(
        "- notification: {}\n- binding status: {}\n- actions: {}\n- batch: {}\n- ack: {}\n",
        plan.client_ui.notification_strategy,
        plan.client_ui.binding_status_strategy,
        plan.client_ui.action_strategy,
        plan.client_ui.batch_strategy,
        plan.client_ui.ack_strategy
    ));
    if !plan.client_ui.unsupported_claims.is_empty() {
        out.push_str("- unsupported claims:\n");
        for claim in &plan.client_ui.unsupported_claims {
            out.push_str(&format!("  - {claim}\n"));
        }
    }
    out.push('\n');

    out.push_str("## Review Workbench\n\n");
    out.push_str(&format!(
        "Version: {} source={} primary_surface={} empty_state={} safe_default={}\n",
        plan.workbench.version,
        plan.workbench.source,
        plan.workbench.primary_surface,
        plan.workbench.empty_state,
        plan.workbench.safe_default_action
    ));
    out.push_str(&format!(
        "Counts: pending_claims={} pending_proposals={} notifications={} enabled_actions={} disabled_actions={} evidence_required={} destructive={} batch_operations={}\n",
        plan.workbench.counts.pending_claims,
        plan.workbench.counts.pending_proposals,
        plan.workbench.counts.pending_notifications,
        plan.workbench.counts.enabled_actions,
        plan.workbench.counts.disabled_actions,
        plan.workbench.counts.evidence_required_actions,
        plan.workbench.counts.destructive_actions,
        plan.workbench.counts.batch_operations
    ));
    out.push_str(&format!(
        "Evidence policy: required_fields={} forbidden_sources={} form_source={}\n",
        plan.workbench.evidence_policy.required_fields.join(","),
        plan.workbench.evidence_policy.forbidden_evidence_sources.join(","),
        plan.workbench.evidence_policy.evidence_form_source
    ));
    out.push_str(&format!(
        "Submission: mutation_tool={} batch_tool={} dry_run_first={} operator_authorization_required={} agent_self_authorization_forbidden={} trust_boundary={}\n",
        plan.workbench.submission_contract.mutation_tool,
        plan.workbench.submission_contract.batch_tool,
        plan.workbench.submission_contract.dry_run_first_for_batch,
        plan.workbench.submission_contract.operator_authorization_required,
        plan.workbench.submission_contract.agent_self_authorization_forbidden,
        plan.workbench.submission_contract.trust_boundary
    ));
    out.push_str(&format!(
        "Authorization boundary: {}\n",
        plan.workbench.submission_contract.authorization_boundary
    ));
    if plan.workbench.action_groups.is_empty() {
        out.push_str("- action_groups: none\n");
    } else {
        out.push_str("- action_groups:\n");
        for group in &plan.workbench.action_groups {
            out.push_str(&format!(
                "  - {} order={} style={} enabled={} disabled={} evidence_required={} destructive={} controls={}\n",
                group.group,
                group.order,
                group.button_style,
                group.enabled_count,
                group.disabled_count,
                group.evidence_required_count,
                group.destructive_count,
                group.control_ids.join(",")
            ));
        }
    }
    out.push('\n');

    out.push_str("## Control Binding Manifest\n\n");
    out.push_str(&format!(
        "Schema: {} source={} controls={} selector={} submit_selector={} trust_boundary={}\n",
        plan.control_binding_manifest.schema,
        plan.control_binding_manifest.source,
        plan.control_binding_manifest.expected_control_count,
        plan.control_binding_manifest.action_selector,
        plan.control_binding_manifest.submit_button_selector,
        plan.control_binding_manifest.trust_boundary
    ));
    out.push_str(&format!(
        "Required DOM attributes: {}\n",
        plan.control_binding_manifest.required_dom_attributes.join(",")
    ));
    out.push_str(&format!(
        "Missing control behavior: {}\n",
        plan.control_binding_manifest.missing_control_behavior
    ));
    for binding in &plan.control_binding_manifest.bindings {
        out.push_str(&format!(
            "- control={} selector={} target={}#{} action={} enabled={} evidence_required={} submit_tool={}\n",
            binding.control_id,
            binding.dom_selector,
            binding.target_type,
            binding.target_id,
            binding.action,
            binding.enabled,
            binding.evidence_required,
            binding.submit_tool
        ));
    }
    out.push('\n');

    out.push_str("## Interaction Contract\n\n");
    out.push_str(&format!(
        "Version: {} source={} mutation_boundary={}\n",
        plan.interaction_contract.version,
        plan.interaction_contract.source,
        plan.interaction_contract.mutation_boundary
    ));
    out.push_str(&format!("Empty state: {}\n", plan.interaction_contract.empty_state));
    out.push_str("- read-only until:\n");
    for rule in &plan.interaction_contract.read_only_until {
        out.push_str(&format!("  - {rule}\n"));
    }
    out.push_str("- global guardrails:\n");
    for guardrail in &plan.interaction_contract.global_guardrails {
        out.push_str(&format!("  - {guardrail}\n"));
    }
    if plan.interaction_contract.actions.is_empty() {
        out.push_str("- actions: none\n");
    } else {
        out.push_str("- actions:\n");
        for action in &plan.interaction_contract.actions {
            out.push_str(&format!(
                "  - {} label=\"{}\" tool={} enabled={} evidence_required={} operator_authorization_required={} agent_self_authorization_forbidden={} safe_default={}\n",
                action.control_id,
                action.label,
                action.submit_tool,
                action.enabled,
                action.evidence_required,
                action.operator_authorization_required,
                action.agent_self_authorization_forbidden,
                action.safe_default
            ));
            out.push_str(&format!(
                "    authorization_boundary={}\n",
                action.authorization_boundary
            ));
            if let Some(result) = &action.evidence_result {
                out.push_str(&format!(
                    "    evidence_result={} accepted_verifiers={}\n",
                    result,
                    action.accepted_verifier_types.join(",")
                ));
            }
            if !action.pre_submit_checks.is_empty() {
                out.push_str(&format!(
                    "    pre_submit_checks={}\n",
                    action.pre_submit_checks.join(",")
                ));
            }
        }
    }
    out.push('\n');

    out.push_str("## In-Client Render Evidence Template\n\n");
    out.push_str(&format!(
        "Schema: {} source={} template_kind={} trust_boundary={}\n",
        plan.render_evidence_template.schema,
        plan.render_evidence_template.source,
        plan.render_evidence_template.template_kind,
        plan.render_evidence_template.trust_boundary
    ));
    out.push_str(&format!(
        "Expected: workbench_version={} interaction_contract_version={} controls={}\n",
        plan.render_evidence_template.expected_workbench_version,
        plan.render_evidence_template.expected_interaction_contract_version,
        plan.render_evidence_template.expected_control_ids.join(",")
    ));
    out.push_str("- fill after visible render:\n");
    for requirement in &plan.render_evidence_template.required_after_visible_render {
        out.push_str(&format!("  - {requirement}\n"));
    }
    out.push_str("- template_json:\n");
    out.push_str(&format!(
        "```json\n{}\n```\n\n",
        serde_json::to_string_pretty(&plan.render_evidence_template.template_json)
            .unwrap_or_else(|_| "{}".to_string())
    ));

    out.push_str("## Surfaces\n\n");
    for surface in &plan.surfaces {
        out.push_str(&format!(
            "- {} source={} render_as={} display={} items={}\n",
            surface.name, surface.source, surface.render_as, surface.display, surface.item_count
        ));
        out.push_str(&format!("  hint: {}\n", surface.client_hint));
    }
    out.push('\n');

    out.push_str("## MCP Call Order\n\n");
    for call in &plan.mcp_call_order {
        out.push_str(&format!(
            "{}. {} via `{}` when={} mutation={} trust_boundary={}\n",
            call.step, call.purpose, call.tool, call.when, call.mutation, call.trust_boundary
        ));
        out.push_str(&format!("   args: `{}`\n", call.arguments));
    }
    out.push('\n');

    out.push_str("## Wrapper Call Order\n\n");
    for command in &plan.wrapper_call_order {
        out.push_str(&format!("- `{command}`\n"));
    }
    out
}

pub fn render_review_render_plan_html(plan: &ReviewRenderPlan) -> String {
    let workbench_version = escape_html(&plan.workbench.version);
    let interaction_version = escape_html(&plan.interaction_contract.version);
    let trust_boundary = escape_html(&plan.trust_boundary);
    let mut out = String::new();

    out.push_str("<!doctype html>\n");
    out.push_str("<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>SOMA Review Workbench</title>\n");
    out.push_str("<style>\n");
    out.push_str(
        ":root{color-scheme:light;--bg:#f7f8fb;--panel:#ffffff;--text:#172033;--muted:#5f6b7a;--line:#d9dee8;--blue:#1d4ed8;--green:#047857;--amber:#b45309;--red:#b91c1c;--soft-blue:#eff6ff;--soft-green:#ecfdf5;--soft-amber:#fff7ed;--soft-red:#fef2f2;}\n",
    );
    out.push_str(
        "*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font-family:Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;font-size:14px;line-height:1.45;}main{width:min(1180px,calc(100% - 32px));margin:0 auto;padding:24px 0 40px;}header{display:flex;align-items:flex-start;justify-content:space-between;gap:16px;margin-bottom:16px;}h1,h2,h3,p{margin:0}h1{font-size:26px;font-weight:760;letter-spacing:0}h2{font-size:17px;margin-bottom:10px}h3{font-size:15px}.eyebrow{font-size:12px;color:var(--muted);text-transform:uppercase;font-weight:700}.scope{color:var(--muted);margin-top:6px}.badge{display:inline-flex;align-items:center;min-height:24px;border:1px solid var(--line);border-radius:8px;padding:3px 8px;background:var(--panel);font-weight:650}.badge.warn{border-color:#fed7aa;background:var(--soft-amber);color:var(--amber)}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:10px;margin:14px 0}.metric,.panel,.action,.call,.surface{background:var(--panel);border:1px solid var(--line);border-radius:8px}.metric{padding:12px}.metric span{display:block;color:var(--muted);font-size:12px}.metric strong{display:block;margin-top:3px;font-size:19px}.panel{padding:14px;margin-top:12px}.boundary{border-color:#fed7aa;background:var(--soft-amber)}.policy{border-color:#bfdbfe;background:var(--soft-blue)}.table{width:100%;border-collapse:collapse}.table th,.table td{border-top:1px solid var(--line);padding:8px;text-align:left;vertical-align:top}.table th{font-size:12px;color:var(--muted);font-weight:700}.actions{display:grid;gap:10px}.action{padding:12px}.action-head{display:flex;align-items:flex-start;justify-content:space-between;gap:12px}.action-meta{color:var(--muted);font-size:12px;margin-top:3px}.button{appearance:none;border:1px solid var(--blue);background:var(--blue);color:#fff;border-radius:8px;min-height:34px;padding:7px 10px;font-weight:700;white-space:normal;text-align:center}.button.secondary{border-color:var(--line);background:#fff;color:var(--text)}.button.danger{border-color:var(--red);background:var(--red)}.button.warn{border-color:var(--amber);background:var(--amber)}.button:disabled{border-color:var(--line);background:#eef1f5;color:#7a8492}.chips{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}.chip{border:1px solid var(--line);border-radius:8px;padding:2px 7px;background:#fff;font-size:12px;color:var(--muted)}.chip.ok{border-color:#bbf7d0;background:var(--soft-green);color:var(--green)}.chip.bad{border-color:#fecaca;background:var(--soft-red);color:var(--red)}code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}pre{white-space:pre-wrap;overflow:auto;background:#111827;color:#f9fafb;border-radius:8px;padding:10px}.checks{margin:8px 0 0 18px;padding:0}.checks li{margin:2px 0}.muted{color:var(--muted)}details{margin-top:8px}summary{cursor:pointer;font-weight:700}.split{display:grid;grid-template-columns:minmax(0,1fr) minmax(220px,320px);gap:12px}.empty{padding:16px;border:1px dashed var(--line);border-radius:8px;background:#fff;color:var(--muted)}@media(max-width:760px){main{width:min(100% - 20px,1180px);padding-top:16px}header,.action-head,.split{display:block}.button{width:100%;margin-top:10px}.table{font-size:13px}}\n",
    );
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str(&format!(
        "<main data-soma-review-workbench=\"{}\" data-interaction-contract=\"{}\" data-control-binding-manifest=\"{}\" data-trust-boundary=\"{}\" data-client=\"{}\">\n",
        workbench_version,
        interaction_version,
        escape_html(&plan.control_binding_manifest.schema),
        trust_boundary,
        escape_html(&plan.client)
    ));
    out.push_str("<header>\n<div>\n<p class=\"eyebrow\">SOMA Review Workbench</p>\n");
    out.push_str(&format!(
        "<h1>{}</h1>\n<p class=\"scope\">project={} session={} limit={}</p>\n",
        escape_html(&plan.client_ui.layout),
        escape_html(plan.project.as_deref().unwrap_or("*")),
        escape_html(plan.session_id.as_deref().unwrap_or("*")),
        plan.limit
    ));
    out.push_str("</div>\n");
    out.push_str(&format!(
        "<span class=\"badge warn\">{}</span>\n</header>\n",
        escape_html(&plan.primary_surface)
    ));

    out.push_str("<section class=\"grid\" aria-label=\"Review counts\">\n");
    push_metric(&mut out, "Pending claims", plan.workbench.counts.pending_claims);
    push_metric(&mut out, "Pending proposals", plan.workbench.counts.pending_proposals);
    push_metric(&mut out, "Notifications", plan.workbench.counts.pending_notifications);
    push_metric(&mut out, "Enabled actions", plan.workbench.counts.enabled_actions);
    push_metric(&mut out, "Evidence required", plan.workbench.counts.evidence_required_actions);
    push_metric(&mut out, "Batch operations", plan.workbench.counts.batch_operations);
    out.push_str("</section>\n");

    out.push_str("<section class=\"panel boundary\">\n<h2>Trust Boundary</h2>\n");
    out.push_str(&format!("<p>{}</p>\n", trust_boundary));
    out.push_str(&format!(
        "<p class=\"muted\">mutation_boundary={}</p>\n",
        escape_html(&plan.interaction_contract.mutation_boundary)
    ));
    out.push_str("<ul class=\"checks\">\n");
    for guardrail in &plan.interaction_contract.global_guardrails {
        out.push_str(&format!("<li>{}</li>\n", escape_html(guardrail)));
    }
    out.push_str("</ul>\n</section>\n");

    out.push_str("<section class=\"panel policy\">\n<h2>Evidence Policy</h2>\n");
    out.push_str("<div class=\"chips\">\n");
    for verifier in &plan.workbench.evidence_policy.accepted_verifier_types {
        out.push_str(&format!("<span class=\"chip ok\">{}</span>\n", escape_html(verifier)));
    }
    for source in &plan.workbench.evidence_policy.forbidden_evidence_sources {
        out.push_str(&format!("<span class=\"chip bad\">forbid {}</span>\n", escape_html(source)));
    }
    out.push_str("</div>\n");
    out.push_str("<ul class=\"checks\">\n");
    for check in &plan.workbench.submission_contract.required_pre_submit_checks {
        out.push_str(&format!("<li>{}</li>\n", escape_html(check)));
    }
    out.push_str("</ul>\n</section>\n");

    out.push_str("<section class=\"panel policy\" data-render-evidence-template=\"soma.in_client_render_evidence.v1\">\n<h2>In-Client Render Evidence Template</h2>\n");
    out.push_str(&format!(
        "<p class=\"muted\">source={} trust_boundary={}</p>\n",
        escape_html(&plan.render_evidence_template.source),
        escape_html(&plan.render_evidence_template.trust_boundary)
    ));
    out.push_str("<div class=\"chips\">\n");
    for control_id in &plan.render_evidence_template.expected_control_ids {
        out.push_str(&format!("<span class=\"chip ok\">{}</span>\n", escape_html(control_id)));
    }
    out.push_str("</div>\n<ul class=\"checks\">\n");
    for requirement in &plan.render_evidence_template.required_after_visible_render {
        out.push_str(&format!("<li>{}</li>\n", escape_html(requirement)));
    }
    out.push_str("</ul>\n");
    out.push_str(&format!(
        "<pre data-render-evidence-template-json=\"soma.in_client_render_evidence.v1\">{}</pre>\n",
        escape_html(&plan.render_evidence_template.template_json.to_string())
    ));
    out.push_str("</section>\n");

    out.push_str("<section class=\"panel policy\" data-control-binding-manifest=\"soma.review_control_binding_manifest.v1\">\n<h2>Control Binding Manifest</h2>\n");
    out.push_str(&format!(
        "<p class=\"muted\">source={} action_selector={} submit_selector={} missing_control_behavior={}</p>\n",
        escape_html(&plan.control_binding_manifest.source),
        escape_html(&plan.control_binding_manifest.action_selector),
        escape_html(&plan.control_binding_manifest.submit_button_selector),
        escape_html(&plan.control_binding_manifest.missing_control_behavior)
    ));
    out.push_str("<div class=\"chips\">\n");
    for control_id in &plan.control_binding_manifest.expected_control_ids {
        out.push_str(&format!(
            "<span class=\"chip ok\" data-soma-expected-control-id=\"{}\">{}</span>\n",
            escape_html(control_id),
            escape_html(control_id)
        ));
    }
    out.push_str("</div>\n<ul class=\"checks\">\n");
    for attribute in &plan.control_binding_manifest.required_dom_attributes {
        out.push_str(&format!("<li>{}</li>\n", escape_html(attribute)));
    }
    out.push_str("</ul>\n</section>\n");

    out.push_str("<section class=\"panel\">\n<h2>Surfaces</h2>\n<table class=\"table\">\n");
    out.push_str("<thead><tr><th>Name</th><th>Source</th><th>Display</th><th>Items</th><th>Client hint</th></tr></thead><tbody>\n");
    for surface in &plan.surfaces {
        out.push_str(&format!(
            "<tr data-surface=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            escape_html(&surface.name),
            escape_html(&surface.name),
            escape_html(&surface.source),
            escape_html(&surface.display),
            surface.item_count,
            escape_html(&surface.client_hint)
        ));
    }
    out.push_str("</tbody></table>\n</section>\n");

    out.push_str("<section class=\"panel\">\n<h2>Review Actions</h2>\n");
    if plan.interaction_contract.actions.is_empty() {
        out.push_str(&format!(
            "<div class=\"empty\">{}</div>\n",
            escape_html(&plan.interaction_contract.empty_state)
        ));
    } else {
        out.push_str("<div class=\"actions\">\n");
        for action in &plan.interaction_contract.actions {
            push_html_action(&mut out, action);
        }
        out.push_str("</div>\n");
    }
    out.push_str("</section>\n");

    out.push_str("<section class=\"panel\">\n<h2>Batch Template</h2>\n");
    out.push_str(&format!(
        "<p class=\"muted\">tool={} dry_run_first={} trust_boundary={}</p>\n",
        escape_html(&plan.batch_template.batch_tool),
        plan.workbench.submission_contract.dry_run_first_for_batch,
        escape_html(&plan.batch_template.trust_boundary)
    ));
    out.push_str(&format!(
        "<pre data-batch-template=\"{}\">{}</pre>\n",
        escape_html(&plan.batch_template.batch_tool),
        escape_html(&plan.batch_template.mcp_arguments_template.to_string())
    ));
    out.push_str("</section>\n");

    out.push_str("<section class=\"panel\">\n<h2>MCP Call Order</h2>\n");
    for call in &plan.mcp_call_order {
        out.push_str(&format!(
            "<article class=\"call\" data-step=\"{}\" data-tool=\"{}\" data-mutation=\"{}\" data-trust-boundary=\"{}\"><div class=\"split\"><div><h3>{}. {}</h3><p class=\"muted\">when={} mutation={}</p><p class=\"muted\">trust_boundary={}</p></div><pre>{}</pre></div></article>\n",
            call.step,
            escape_html(&call.tool),
            escape_html(&call.mutation),
            escape_html(&call.trust_boundary),
            call.step,
            escape_html(&call.purpose),
            escape_html(&call.when),
            escape_html(&call.mutation),
            escape_html(&call.trust_boundary),
            escape_html(&call.arguments.to_string())
        ));
    }
    out.push_str("</section>\n");

    out.push_str("<section class=\"panel\">\n<h2>Wrapper Call Order</h2>\n<ul class=\"checks\">\n");
    for command in &plan.wrapper_call_order {
        out.push_str(&format!("<li><code>{}</code></li>\n", escape_html(command)));
    }
    out.push_str("</ul>\n</section>\n");
    out.push_str("</main>\n</body>\n</html>\n");
    out
}

fn push_metric(out: &mut String, label: &str, value: usize) {
    out.push_str(&format!(
        "<article class=\"metric\"><span>{}</span><strong>{}</strong></article>\n",
        escape_html(label),
        value
    ));
}

fn push_html_action(out: &mut String, action: &ReviewInteractionAction) {
    let button_class = review_html_button_class(&action.button_style);
    let disabled = if action.enabled { "" } else { " disabled" };
    out.push_str(&format!(
        "<article class=\"action\" data-soma-review-action=\"true\" data-control-id=\"{}\" data-soma-control-id=\"{}\" data-target-type=\"{}\" data-target-id=\"{}\" data-action=\"{}\" data-enabled=\"{}\" data-submit-tool=\"{}\">\n",
        escape_html(&action.control_id),
        escape_html(&action.control_id),
        escape_html(&action.target_type),
        action.target_id,
        escape_html(&action.action),
        action.enabled,
        escape_html(&action.submit_tool)
    ));
    out.push_str("<div class=\"action-head\">\n<div>\n");
    out.push_str(&format!("<h3>{}</h3>\n", escape_html(&action.label)));
    out.push_str(&format!(
        "<p class=\"action-meta\">{} #{} group={} safe_default={}</p>\n",
        escape_html(&action.target_type),
        action.target_id,
        escape_html(&action.group),
        escape_html(&action.safe_default)
    ));
    if let Some(reason) = &action.disabled_reason {
        out.push_str(&format!(
            "<p class=\"action-meta\">disabled_reason={}</p>\n",
            escape_html(reason)
        ));
    }
    out.push_str("</div>\n");
    out.push_str(&format!(
        "<button type=\"button\" class=\"button {}\"{} data-soma-submit-control=\"true\" data-control-id=\"{}\" data-soma-control-id=\"{}\" data-submit-tool=\"{}\" data-mcp-arguments-template=\"{}\">{}</button>\n",
        button_class,
        disabled,
        escape_html(&action.control_id),
        escape_html(&action.control_id),
        escape_html(&action.submit_tool),
        escape_html(&action.submit_arguments_template.to_string()),
        escape_html(&action.label)
    ));
    out.push_str("</div>\n");
    out.push_str("<div class=\"chips\">\n");
    out.push_str(&format!(
        "<span class=\"chip\">icon {}</span><span class=\"chip\">style {}</span><span class=\"chip\">evidence_required {}</span>\n",
        escape_html(&action.icon),
        escape_html(&action.button_style),
        action.evidence_required
    ));
    for verifier in &action.accepted_verifier_types {
        out.push_str(&format!("<span class=\"chip ok\">{}</span>\n", escape_html(verifier)));
    }
    out.push_str("</div>\n");
    if action.evidence_required {
        out.push_str(
            "<div class=\"panel\" data-evidence-form=\"required\">\n<h3>Evidence Fields</h3>\n",
        );
        if let Some(result) = &action.evidence_result {
            out.push_str(&format!("<p class=\"muted\">result={}</p>\n", escape_html(result)));
        }
        if let Some(template) = &action.evidence_ref_template {
            out.push_str(&format!(
                "<p class=\"muted\">evidence_ref.kind={} evidence_ref.id={} evidence_ref.source={}</p>\n",
                escape_html(&template.kind),
                escape_html(&template.id),
                escape_html(template.source.as_deref().unwrap_or("<source>"))
            ));
        }
        out.push_str("</div>\n");
    }
    out.push_str("<details><summary>Pre-submit checks</summary><ul class=\"checks\">\n");
    for check in &action.pre_submit_checks {
        out.push_str(&format!("<li>{}</li>\n", escape_html(check)));
    }
    out.push_str("</ul></details>\n");
    out.push_str("<details><summary>CLI command template</summary>\n");
    out.push_str(&format!("<pre>{}</pre>\n", escape_html(&action.cli_command_template)));
    out.push_str("</details>\n");
    out.push_str("</article>\n");
}

fn review_html_button_class(button_style: &str) -> &'static str {
    match button_style {
        "destructive" | "danger" => "danger",
        "warning" | "caution" => "warn",
        "secondary" | "neutral" => "secondary",
        _ => "",
    }
}

fn css_attr_selector_escape(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn render_review_queue_markdown(queue: &ReviewQueue) -> String {
    let mut out = String::new();
    out.push_str("# SOMA Review Queue\n\n");
    out.push_str(&format!(
        "Scope: project={} session={} limit={}\n\n",
        queue.project.as_deref().unwrap_or("*"),
        queue.session_id.as_deref().unwrap_or("*"),
        queue.limit
    ));
    out.push_str(&format!(
        "Counts: claims={} proposals={} ready={} manual_review={} missing_verification={}\n\n",
        queue.claim_count,
        queue.proposal_count,
        queue.ready_proposal_count,
        queue.manual_review_proposal_count,
        queue.missing_verification_count
    ));
    render_interruption_summary_markdown(&mut out, &queue.interruption_summary);

    out.push_str("## Claims Needing Verification\n\n");
    if queue.claims.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for item in &queue.claims {
            out.push_str(&format!(
                "- claim #{} [{}] {}\n",
                item.claim.id,
                item.claim.lifecycle_state.as_str(),
                compact_for_markdown(&item.claim.text, 140)
            ));
            out.push_str(&format!("  reason: {}\n", item.review_reason));
            render_decision_packet_markdown(&mut out, &item.decision_packet);
            render_task_frame_projection_summary(
                &mut out,
                item.task_frame_projection_audit.as_ref(),
            );
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            out.push_str(&format!("  cli: `{}`\n", item.cli_hint));
            for option in &item.action_options {
                out.push_str(&format!("  action: {} -> `{}`\n", option.label, option.cli_hint));
            }
        }
        out.push('\n');
    }

    out.push_str("## Learning Proposals\n\n");
    if queue.proposals.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for item in &queue.proposals {
            let target = item
                .proposal
                .target_lifecycle_state
                .map(|state| state.as_str().to_string())
                .unwrap_or_else(|| "none".to_string());
            out.push_str(&format!(
                "- proposal #{} action={} target={} readiness={}\n",
                item.proposal.id,
                item.proposal.action.as_str(),
                target,
                item.readiness
            ));
            out.push_str(&format!("  reason: {}\n", item.review_reason));
            render_decision_packet_markdown(&mut out, &item.decision_packet);
            render_semantic_review_markdown(&mut out, item.semantic_review.as_ref());
            if !item.missing_verification_claim_ids.is_empty() {
                out.push_str(&format!(
                    "  missing claim verification: {}\n",
                    item.missing_verification_claim_ids
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            render_interruption_hint_markdown(&mut out, &item.interruption_hint);
            out.push_str(&format!("  cli: `{}`\n", item.cli_hint));
            for option in &item.action_options {
                let mut suffix = String::new();
                if option.requires_evidence {
                    suffix.push_str(" evidence");
                }
                if option.requires_destructive_confirmation {
                    suffix.push_str(" destructive-confirmation");
                }
                out.push_str(&format!(
                    "  action: {}{} -> `{}`\n",
                    option.label, suffix, option.cli_hint
                ));
            }
            if let Some(hint) = &item.apply_ready_cli_hint {
                out.push_str(&format!("  batch: `{hint}`\n"));
            }
        }
        out.push('\n');
    }

    if let Some(hint) = &queue.batch_apply_cli_hint {
        out.push_str(&format!("Batch apply ready proposals: `{hint}`\n"));
    }
    out
}

pub fn render_review_digest_markdown(digest: &ReviewDigest) -> String {
    let mut out = String::new();
    out.push_str("# SOMA Review Digest\n\n");
    out.push_str(&format!("Source: `{}`\n", digest.source));
    out.push_str(&format!("Trust boundary: {}\n", digest.trust_boundary));
    out.push_str("Mutation path: this digest is read-only. Record verification through `soma context review-action`, `soma context review-batch`, MCP `soma_review_action`, or MCP `soma_review_batch`; apply proposals only through learning-proposal gates.\n\n");
    out.push_str(&format!(
        "Client: {} surface={} style={} render_as={}\n",
        digest.client,
        digest.ui_hint.surface,
        digest.ui_hint.notification_style,
        digest.ui_hint.render_as
    ));
    out.push_str(&format!(
        "Scope: project={} session={} limit={} include_queue_only={}\n\n",
        digest.project.as_deref().unwrap_or("*"),
        digest.session_id.as_deref().unwrap_or("*"),
        digest.limit,
        digest.include_queue_only
    ));
    out.push_str(&format!(
        "Counts: should_notify={} suppressed_by_cooldown={} pending_notifications={} notifications={} queue_only={} hidden_queue_only={} cooldown_remaining_seconds={} items={}\n",
        digest.should_notify,
        digest.suppressed_by_cooldown,
        digest.pending_notification_count,
        digest.notification_count,
        digest.queue_only_count,
        digest.hidden_queue_only_count,
        digest.cooldown_remaining_seconds,
        digest.item_count
    ));
    out.push_str(&format!("Digest signature: `{}`\n\n", digest.digest_signature));
    if let Some(state) = &digest.notification_state {
        out.push_str(&format!(
            "Notification state: ack_count={} acknowledged_at_ns={} cooldown_until_ns={}\n\n",
            state.ack_count, state.acknowledged_at_ns, state.cooldown_until_ns
        ));
    }
    render_interruption_summary_markdown(&mut out, &digest.interruption_summary);

    out.push_str("## Belief Review\n\n");
    out.push_str(&format!(
        "status={} candidates={} groups={} contradictions={} corroborations={} visible={} hidden={} hidden_duplicates={}\n",
        digest.belief_review.status,
        digest.belief_review.candidate_count,
        digest.belief_review.group_count,
        digest.belief_review.contradiction_count,
        digest.belief_review.corroboration_count,
        digest.belief_review.visible_count,
        digest.belief_review.hidden_count,
        digest.belief_review.hidden_duplicate_count
    ));
    out.push_str(&format!(
        "workload: status={} substantive_groups={} substantive_candidates={} low_value_conflicts={} low_value_noise={} noise_candidates={} primary_group={}\n",
        digest.belief_review.workload_summary.status,
        digest
            .belief_review
            .workload_summary
            .substantive_contradiction_group_count,
        digest
            .belief_review
            .workload_summary
            .substantive_contradiction_candidate_count,
        digest
            .belief_review
            .workload_summary
            .low_value_conflict_candidate_count,
        digest
            .belief_review
            .workload_summary
            .low_value_noise_candidate_count,
        digest.belief_review.workload_summary.noise_candidate_count,
        digest
            .belief_review
            .workload_summary
            .primary_group_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));
    out.push_str(&format!(
        "workload_next: {}\n",
        digest.belief_review.workload_summary.next_action
    ));
    out.push_str(&format!("next: {}\n", digest.belief_review.next_action));
    out.push_str(&format!("promotion_rule: {}\n", digest.belief_review.promotion_rule));
    out.push_str(&format!("trust_boundary: {}\n\n", digest.belief_review.trust_boundary));
    if digest.belief_review.items.is_empty() {
        if digest.belief_review.hidden_count > 0 {
            out.push_str(
                "- hidden. Re-run with `--include-queue-only` to include belief review rows.\n\n",
            );
        } else {
            out.push_str("- none\n\n");
        }
    } else {
        for item in &digest.belief_review.items {
            out.push_str(&format!(
                "- belief_candidate #{} kind={} triage={} noise={} priority={} group_size={} duplicates_hidden={}\n",
                item.candidate_id,
                item.kind,
                item.triage_status,
                item.noise_risk,
                item.review_priority,
                item.group_candidate_count,
                item.hidden_duplicate_count
            ));
            out.push_str(&format!("  evidence: {}\n", item.evidence.as_deref().unwrap_or("none")));
            out.push_str(&format!("  claim: {}\n", item.claim_preview));
            out.push_str(&format!(
                "  scope: project={} session={}\n",
                item.project.as_deref().unwrap_or("unknown-project"),
                item.session_id.as_deref().unwrap_or("unknown-session")
            ));
            if item.group_candidate_count > 1 {
                let ids = item
                    .grouped_candidate_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("  grouped_candidates: {ids}\n"));
            }
            out.push_str(&format!("  episode_a: {}\n", item.episode_a_preview));
            out.push_str(&format!("  episode_a_outcome: {}\n", item.episode_a_outcome));
            out.push_str(&format!("  episode_b: {}\n", item.episode_b_preview));
            out.push_str(&format!("  episode_b_outcome: {}\n", item.episode_b_outcome));
            out.push_str(&format!("  reason: {}\n", item.triage_reason));
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            out.push_str(&format!(
                "  resolution: {} via {}\n",
                item.resolution_action.action,
                command_line(&item.resolution_action.cli_command)
            ));
        }
        out.push('\n');
    }

    out.push_str("## Digest Items\n\n");
    if digest.items.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for item in &digest.items {
            let target = item.target_lifecycle_state.as_deref().unwrap_or("none");
            out.push_str(&format!(
                "- proposal #{} action={} target={} readiness={}\n",
                item.proposal_id, item.action, target, item.readiness
            ));
            out.push_str(&format!("  title: {}\n", item.title));
            out.push_str(&format!("  body: {}\n", item.body));
            out.push_str(&format!("  reason: {}\n", item.review_reason));
            out.push_str(&format!(
                "  digest_card: lane={} target={} status={} blocks_l4={} evidence_rule={}\n",
                item.digest_card.lane,
                item.digest_card.target,
                item.digest_card.status,
                item.digest_card.blocks_l4_promotion,
                item.digest_card.evidence_rule
            ));
            render_decision_packet_markdown(&mut out, &item.decision_packet);
            render_semantic_review_markdown(&mut out, item.semantic_review.as_ref());
            render_interruption_hint_markdown(&mut out, &item.interruption_hint);
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            render_report_action_summary(&mut out, &item.action_options);
        }
        out.push('\n');
    }

    if digest.hidden_queue_only_count > 0 {
        out.push_str(&format!(
            "Hidden queue-only proposals: {}. Re-run with `--include-queue-only` or use `soma_review_queue` for the full queue.\n",
            digest.hidden_queue_only_count
        ));
    }
    out
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

pub fn render_review_report_markdown(report: &ReviewReport) -> String {
    let mut out = String::new();
    out.push_str("# SOMA Review Report\n\n");
    out.push_str(&format!("Source: `{}`\n", report.source));
    out.push_str(&format!("Trust boundary: {}\n", report.trust_boundary));
    out.push_str("Mutation path: this report is read-only. Record verification only through `soma context review-action`, `soma context review-batch`, MCP `soma_review_action`, or MCP `soma_review_batch`; apply proposals only through learning-proposal gates.\n\n");
    out.push_str(&format!(
        "Scope: project={} session={} limit={} include_disabled={}\n\n",
        report.project.as_deref().unwrap_or("*"),
        report.session_id.as_deref().unwrap_or("*"),
        report.limit,
        report.include_disabled
    ));
    out.push_str(&format!(
        "Counts: claims={} proposals={} ready={} manual_review={} missing_verification={} actions={} disabled_actions={}\n\n",
        report.queue.claim_count,
        report.queue.proposal_count,
        report.queue.ready_proposal_count,
        report.queue.manual_review_proposal_count,
        report.queue.missing_verification_count,
        report.action_plan.action_count,
        report.action_plan.disabled_action_count
    ));
    render_interruption_summary_markdown(&mut out, &report.queue.interruption_summary);

    out.push_str("## Claims\n\n");
    if report.queue.claims.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for item in &report.queue.claims {
            out.push_str(&format!(
                "- claim #{} [{}] {}\n",
                item.claim.id,
                item.claim.lifecycle_state.as_str(),
                compact_for_markdown(&item.claim.text, 140)
            ));
            out.push_str(&format!("  trust_ready: {}\n", item.durable_promotion_trust));
            out.push_str(&format!("  reason: {}\n", item.review_reason));
            render_decision_packet_markdown(&mut out, &item.decision_packet);
            render_task_frame_projection_summary(
                &mut out,
                item.task_frame_projection_audit.as_ref(),
            );
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            render_report_action_summary(&mut out, &item.action_options);
        }
        out.push('\n');
    }

    out.push_str("## Proposals\n\n");
    if report.queue.proposals.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for item in &report.queue.proposals {
            let target = item
                .proposal
                .target_lifecycle_state
                .map(|state| state.as_str().to_string())
                .unwrap_or_else(|| "none".to_string());
            out.push_str(&format!(
                "- proposal #{} action={} target={} readiness={}\n",
                item.proposal.id,
                item.proposal.action.as_str(),
                target,
                item.readiness
            ));
            out.push_str(&format!("  reason: {}\n", item.review_reason));
            render_decision_packet_markdown(&mut out, &item.decision_packet);
            render_semantic_review_markdown(&mut out, item.semantic_review.as_ref());
            if !item.missing_verification_claim_ids.is_empty() {
                out.push_str(&format!(
                    "  missing claim verification: {}\n",
                    item.missing_verification_claim_ids
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            out.push_str(&format!("  next: {}\n", item.recommended_next_action));
            render_interruption_hint_markdown(&mut out, &item.interruption_hint);
            render_report_action_summary(&mut out, &item.action_options);
            if let Some(hint) = &item.apply_ready_cli_hint {
                out.push_str(&format!("  gated_apply_hint: `{hint}`\n"));
            }
        }
        out.push('\n');
    }

    out.push_str("## Batch Verification Template\n\n");
    out.push_str(&format!(
        "- action={} target_type={} operations={} excluded={} requires_evidence_fill={} executable_after_fill={}\n",
        report.batch_template.action,
        report.batch_template.target_type,
        report.batch_template.operation_count,
        report.batch_template.excluded_action_count,
        report.batch_template.requires_evidence_fill,
        report.batch_template.executable_after_fill
    ));
    out.push_str(&format!("  trust: {}\n", report.batch_template.trust_boundary));
    out.push_str(&format!(
        "  ui: title=\"{}\" submit=\"{}\" dry_run_first={} mutation_tool=`{}`\n",
        report.batch_template.ui_hint.form_title,
        report.batch_template.ui_hint.submit_label,
        report.batch_template.ui_hint.dry_run_first,
        report.batch_template.ui_hint.mutation_tool
    ));
    out.push_str(&format!("  dry_run_cli: `{}`\n", report.batch_template.cli_hint));
    out.push_str(&format!("  mcp_tool: `{}`\n", report.batch_template.batch_tool));
    out.push_str(&format!(
        "  mcp_arguments_template: `{}`\n",
        report.batch_template.mcp_arguments_template
    ));

    if report.include_disabled {
        let disabled: Vec<&ReviewActionOption> =
            report.action_plan.actions.iter().filter(|action| !action.enabled).collect();
        out.push_str("\n## Disabled Controls\n\n");
        if disabled.is_empty() {
            out.push_str("- none\n");
        } else {
            for action in disabled {
                out.push_str(&format!(
                    "- {} `{}` #{} disabled_reason={}\n",
                    action.target_type,
                    action.action,
                    action.target_id,
                    action.disabled_reason.as_deref().unwrap_or("unknown")
                ));
            }
        }
    }

    out
}

fn render_decision_packet_markdown(out: &mut String, packet: &ReviewDecisionPacket) {
    let required = if packet.required_evidence.is_empty() {
        "none".to_string()
    } else {
        packet.required_evidence.join(",")
    };
    let blocking = if packet.blocking_reasons.is_empty() {
        "none".to_string()
    } else {
        packet.blocking_reasons.join(",")
    };
    out.push_str(&format!(
        "  decision_packet: priority={} status={} question={} required_evidence={} blocking={} safe_default={} next_surface={}\n",
        packet.priority,
        packet.status,
        packet.primary_question,
        required,
        blocking,
        packet.safe_default_action,
        packet.next_surface
    ));
}

fn render_task_frame_projection_summary(
    out: &mut String,
    audit: Option<&TaskFrameProjectionAudit>,
) {
    let Some(audit) = audit else {
        return;
    };
    out.push_str(&format!(
        "  task_frame_projection: id={} policy={} passed={}\n",
        audit.task_frame_id, audit.projection_policy, audit.passed
    ));
    if let Some(reason) = &audit.projection_policy_explicit_reason {
        out.push_str(&format!("  local_private_reason: {}\n", compact_for_markdown(reason, 160)));
    }
}

fn render_interruption_summary_markdown(out: &mut String, summary: &ReviewInterruptionSummary) {
    out.push_str(&format!(
        "Interruption policy: policy={} should_interrupt={} surface={} cadence={} cooldown_seconds={} reason={} interrupt_count={} digest_count={} queue_only_count={}\n\n",
        summary.policy,
        summary.should_interrupt,
        summary.next_surface,
        summary.cadence,
        summary.cooldown_seconds,
        summary.reason,
        summary.interrupt_count,
        summary.digest_count,
        summary.queue_only_count
    ));
}

fn render_interruption_hint_markdown(out: &mut String, hint: &ReviewInterruptionHint) {
    out.push_str(&format!(
        "  interruption: level={} surface={} should_interrupt={} cadence={} cooldown_seconds={} batch_key={} reason={}\n",
        hint.level,
        hint.surface,
        hint.should_interrupt,
        hint.cadence,
        hint.cooldown_seconds,
        hint.batch_key,
        hint.reason
    ));
}

pub fn render_review_action_plan_markdown(plan: &ReviewActionPlan) -> String {
    let mut out = String::new();
    out.push_str("# SOMA Review Action Guide\n\n");
    out.push_str(&format!(
        "Scope: project={} session={} limit={} include_disabled={}\n\n",
        plan.project.as_deref().unwrap_or("*"),
        plan.session_id.as_deref().unwrap_or("*"),
        plan.limit,
        plan.include_disabled
    ));
    out.push_str(&format!(
        "Status: {} semantic_review_resolution_count={} evidence_required_actions={}\n\n",
        plan.status, plan.semantic_review_resolution_count, plan.evidence_required_action_count
    ));
    if let Some(primary) = &plan.primary_action {
        out.push_str(&format!(
            "Primary action: {} `{}` #{} control={} evidence_required={} safe_default={}\n",
            primary.target_type,
            primary.action,
            primary.target_id,
            primary.control_id,
            primary.requires_evidence,
            primary.safe_default
        ));
        out.push_str(&format!("Primary command: {}\n", primary.cli_hint));
        out.push_str(&format!("Primary trust effect: {}\n\n", primary.trust_effect));
    }
    out.push_str(&format!(
        "Counts: claims={} proposals={} actions={} disabled={}\n\n",
        plan.claim_count, plan.proposal_count, plan.action_count, plan.disabled_action_count
    ));
    out.push_str(&format!("Trust boundary: this guide is read-only. {}\n\n", plan.trust_boundary));

    let enabled: Vec<&ReviewActionOption> =
        plan.actions.iter().filter(|action| action.enabled).collect();
    out.push_str("## Enabled Actions\n\n");
    if enabled.is_empty() {
        out.push_str("- none\n\n");
    } else {
        for action in enabled {
            render_action_option_markdown(&mut out, action);
        }
        out.push('\n');
    }

    let disabled: Vec<&ReviewActionOption> =
        plan.actions.iter().filter(|action| !action.enabled).collect();
    if plan.include_disabled {
        out.push_str("## Disabled Actions\n\n");
        if disabled.is_empty() {
            out.push_str("- none\n");
        } else {
            for action in disabled {
                render_action_option_markdown(&mut out, action);
            }
        }
    } else if plan.disabled_action_count > 0 {
        out.push_str(&format!(
            "Hidden disabled actions: {}. Re-run with `--include-disabled` or MCP `include_disabled=true` to preview blocked controls.\n",
            plan.disabled_action_count
        ));
    }
    out
}

pub fn render_review_action_plan_brief(plan: &ReviewActionPlan) -> String {
    let mut out = String::new();
    out.push_str("SOMA review actions brief\n");
    out.push_str(&format!(
        "  Status: {} actions={} evidence_required={} disabled={} semantic_resolution={}\n",
        plan.status,
        plan.action_count,
        plan.evidence_required_action_count,
        plan.disabled_action_count,
        plan.semantic_review_resolution_count
    ));
    out.push_str(&format!(
        "  Scope: project={} session={} limit={} include_disabled={}\n",
        plan.project.as_deref().unwrap_or("*"),
        plan.session_id.as_deref().unwrap_or("*"),
        plan.limit,
        plan.include_disabled
    ));
    if let Some(primary) = &plan.primary_action {
        out.push_str(&format!(
            "  Primary: {} {} #{} control={} evidence_required={} safe_default={}\n",
            primary.target_type,
            primary.action,
            primary.target_id,
            primary.control_id,
            primary.requires_evidence,
            primary.safe_default
        ));
        out.push_str(&format!("    command: {}\n", primary.cli_hint));
        out.push_str(&format!("    trust_effect: {}\n", primary.trust_effect));
    } else {
        out.push_str("  Primary: none\n");
    }

    let enabled = plan.actions.iter().filter(|action| action.enabled).collect::<Vec<_>>();
    if enabled.is_empty() {
        out.push_str("  Actions: none\n");
    } else {
        out.push_str("  Actions:\n");
        for action in enabled.iter().take(8) {
            let verifier_summary = action
                .verification_template
                .as_ref()
                .map(|template| template.accepted_verifier_types.join(","))
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!(
                "    - {} {} #{} control={} evidence={} verifiers={} label=\"{}\"\n",
                action.target_type,
                action.action,
                action.target_id,
                action.control_id,
                action.requires_evidence,
                verifier_summary,
                action.label
            ));
            out.push_str(&format!("      cli: {}\n", action.cli_hint));
            if let Some(reason) = action.disabled_reason.as_deref() {
                out.push_str(&format!("      disabled_reason: {reason}\n"));
            }
        }
        if enabled.len() > 8 {
            out.push_str(&format!(
                "    ... {} more enabled action(s); use `--format markdown` or `--json` for the full list.\n",
                enabled.len() - 8
            ));
        }
    }
    out.push_str(
        "  Evidence boundary: user/tool/test/local_observation/correction only; cloud_draft, cloud_output_text, and client render text cannot verify or promote memory.\n",
    );
    out.push_str(&format!("  Trust boundary: {}\n", plan.trust_boundary));
    out
}

fn claim_review_item(
    storage: &Storage,
    claim: StoredClaimRecord,
) -> Result<ClaimReviewItem, StorageError> {
    let verification_events = storage.verification_events_for_claim(claim.id)?;
    let durable_promotion_trust = storage.claim_has_durable_promotion_trust(claim.id)?;
    let decision_packet = claim_decision_packet(&claim, durable_promotion_trust);
    Ok(ClaimReviewItem {
        review_reason: if durable_promotion_trust {
            "verified_claim_ready_for_proposal_apply".to_string()
        } else {
            "cloud_draft_requires_user_tool_test_local_or_correction_verification".to_string()
        },
        recommended_next_action: if durable_promotion_trust {
            "Apply a queued promotion proposal if one references this claim.".to_string()
        } else {
            "Record a confirmed, contradicted, superseded, or inconclusive verification event before any L3/L4 promotion.".to_string()
        },
        decision_packet,
        cli_hint: format!(
            "soma context review-action --claim-id {} --action confirm --verifier <user|test|tool|local_observation|correction> --evidence-kind <kind> --evidence-id <id>",
            claim.id
        ),
        mcp_tools: vec!["soma_review_action".to_string(), "soma_verify_claim".to_string()],
        action_options: claim_action_options(claim.id),
        task_frame_projection_audit: claim_task_frame_projection_audit(storage, &claim)?,
        claim,
        verification_events,
        durable_promotion_trust,
    })
}

fn claim_task_frame_projection_audit(
    storage: &Storage,
    claim: &StoredClaimRecord,
) -> Result<Option<TaskFrameProjectionAudit>, StorageError> {
    let Some(task_frame_id) = claim.task_frame_id else {
        return Ok(None);
    };
    let Some(task_frame) = storage.task_frame(task_frame_id)? else {
        return Ok(None);
    };
    Ok(Some(audit_task_frame_projection(&task_frame)))
}

fn proposal_review_item(
    storage: &Storage,
    proposal: StoredLearningCriticProposal,
) -> Result<ProposalReviewItem, StorageError> {
    let linked_claims = proposal
        .claim_ids
        .iter()
        .map(|claim_id| proposal_claim_review(storage, *claim_id))
        .collect::<Result<Vec<_>, _>>()?;
    let missing_verification_claim_ids =
        promotion_missing_verification_claim_ids(&proposal, &linked_claims);
    let semantic_review = semantic_promotion_review(storage, &proposal, &linked_claims)?;
    let readiness =
        proposal_readiness(&proposal, &missing_verification_claim_ids, semantic_review.as_ref());
    let batch_apply_eligible = readiness == "ready_for_batch_apply";
    let review_reason =
        proposal_review_reason(&proposal, &missing_verification_claim_ids, &readiness);
    let recommended_next_action =
        proposal_recommended_next_action(&proposal, &missing_verification_claim_ids, &readiness);
    let cli_hint = proposal_cli_hint(&proposal, &missing_verification_claim_ids, &readiness);
    let interruption_hint =
        proposal_interruption_hint(&proposal, &readiness, semantic_review.as_ref());
    let apply_ready_cli_hint =
        batch_apply_eligible.then(|| "soma context learning-proposals apply-ready".to_string());
    let action_options =
        proposal_action_options(&proposal, &readiness, &missing_verification_claim_ids);
    let decision_packet = proposal_decision_packet(
        &proposal,
        &readiness,
        &missing_verification_claim_ids,
        semantic_review.as_ref(),
        &interruption_hint,
    );
    Ok(ProposalReviewItem {
        proposal,
        linked_claims,
        semantic_review,
        interruption_hint,
        missing_verification_claim_ids,
        readiness,
        batch_apply_eligible,
        review_reason,
        recommended_next_action,
        decision_packet,
        cli_hint,
        apply_ready_cli_hint,
        mcp_tools: vec![
            "soma_review_action".to_string(),
            "soma_verify_claim".to_string(),
            "soma_learning_proposals_apply".to_string(),
            "soma_learning_proposals_apply_ready".to_string(),
        ],
        action_options,
    })
}

fn claim_decision_packet(
    claim: &StoredClaimRecord,
    durable_promotion_trust: bool,
) -> ReviewDecisionPacket {
    let (priority, status, primary_question, required_evidence, blocking_reasons, safe_default) =
        if durable_promotion_trust {
            (
                "low",
                "verified",
                "Does a queued promotion proposal need this verified claim?",
                Vec::new(),
                Vec::new(),
                "inspect_related_proposals",
            )
        } else {
            (
                "high",
                "needs_verification",
                "Is this cloud draft true enough to become durable evidence?",
                vec![
                    "user/tool/test/local_observation/correction verifier".to_string(),
                    "evidence_ref.kind".to_string(),
                    "evidence_ref.id".to_string(),
                ],
                vec!["cloud_draft_without_durable_promotion_trust".to_string()],
                "wait_or_record_verification",
            )
        };

    ReviewDecisionPacket {
        target_type: "claim".to_string(),
        target_id: claim.id,
        priority: priority.to_string(),
        status: status.to_string(),
        primary_question: primary_question.to_string(),
        required_evidence,
        blocking_reasons,
        safe_default_action: safe_default.to_string(),
        next_surface: "review_action".to_string(),
        trust_boundary: format!(
            "{} claim remains {} until trusted verification changes lifecycle through storage gates",
            claim.source_type.as_str(),
            claim.lifecycle_state.as_str()
        ),
    }
}

fn proposal_decision_packet(
    proposal: &StoredLearningCriticProposal,
    readiness: &str,
    missing_verification_claim_ids: &[i64],
    semantic_review: Option<&SemanticPromotionReview>,
    interruption_hint: &ReviewInterruptionHint,
) -> ReviewDecisionPacket {
    let mut required_evidence = Vec::new();
    let mut blocking_reasons = Vec::new();
    let (priority, status, primary_question, safe_default_action, next_surface) = match readiness {
        "needs_claim_verification" => {
            required_evidence.push("trusted verification for linked claim ids".to_string());
            required_evidence.push("evidence_ref.kind".to_string());
            required_evidence.push("evidence_ref.id".to_string());
            blocking_reasons.push(format!(
                "missing_verification_claim_ids={}",
                join_i64s(missing_verification_claim_ids)
            ));
            (
                "high",
                "blocked_on_claim_verification",
                "Can the linked draft claims be externally verified before this proposal applies?",
                "confirm_or_wait",
                "review_action",
            )
        }
        "semantic_support_diversity_requires_manual_review" => {
            required_evidence.push("inspect semantic_review.support_claim_ids".to_string());
            required_evidence
                .push("inspect semantic_review.support_diversity.bias_risk".to_string());
            required_evidence.push("inspect semantic_review.review_rubric".to_string());
            if let Some(review) = semantic_review {
                if let Some(diversity) = &review.support_diversity {
                    blocking_reasons
                        .push(format!("support_diversity_bias_risk={}", diversity.bias_risk));
                } else {
                    blocking_reasons.push("support_diversity_missing".to_string());
                }
            }
            (
                "high",
                "manual_l4_semantic_review",
                "Should this repeated L3 evidence really become a stable L4 semantic fact?",
                "inspect_support_then_apply_or_reject",
                interruption_hint.surface.as_str(),
            )
        }
        "semantic_review_only_candidate_requires_resolution" => {
            required_evidence.push("inspect semantic_review.support_claim_ids".to_string());
            required_evidence.push("inspect semantic_review.review_rubric".to_string());
            required_evidence.push("record resolution evidence before any L4 proposal".to_string());
            if let Some(review) = semantic_review {
                blocking_reasons.push(format!("semantic_review_rule={}", review.rule));
                if let Some(grouping_rule) = &review.grouping_rule {
                    blocking_reasons.push(format!("semantic_grouping_rule={grouping_rule}"));
                }
            }
            (
                "high",
                "semantic_review_only_verification_required",
                "Do these L3 claims resolve into a safe abstraction, or should they remain unresolved?",
                "inspect_support_then_confirm_reject_or_wait",
                "review_queue",
            )
        }
        "ready_for_batch_apply" => (
            "medium",
            "ready_for_verified_apply",
            "Should this already verified promotion be applied now?",
            "apply_ready_dry_run_first",
            "review_drain_or_apply_ready",
        ),
        "explicit_review_required" => {
            if proposal.action == LearningCriticAction::Decay {
                blocking_reasons
                    .push("destructive_lifecycle_change_requires_confirmation".to_string());
                (
                    "high",
                    "destructive_review_required",
                    "Should this decay/forget proposal change durable memory?",
                    "wait_until_explicit_confirmation",
                    "review_report",
                )
            } else {
                (
                    "medium",
                    "explicit_review_required",
                    "What operator decision should resolve this learning proposal?",
                    "wait_or_review_action",
                    "review_report",
                )
            }
        }
        "closed" => (
            "low",
            "closed",
            "This proposal is closed; no operator action is required.",
            "none",
            "none",
        ),
        _ => (
            "medium",
            readiness,
            "What operator decision should resolve this proposal?",
            "wait_or_review_action",
            "review_report",
        ),
    };

    ReviewDecisionPacket {
        target_type: "proposal".to_string(),
        target_id: proposal.id,
        priority: priority.to_string(),
        status: status.to_string(),
        primary_question: primary_question.to_string(),
        required_evidence,
        blocking_reasons,
        safe_default_action: safe_default_action.to_string(),
        next_surface: next_surface.to_string(),
        trust_boundary: "proposal decisions are read-only until soma_review_action, soma_review_batch, review_drain, or learning_proposals apply re-check storage gates".to_string(),
    }
}

fn claim_action_options(claim_id: i64) -> Vec<ReviewActionOption> {
    [
        (
            "confirm",
            "Confirm",
            "Record trusted evidence that this draft claim is true.",
            "A confirmed user/tool/test/local/correction event can satisfy durable promotion trust.",
        ),
        (
            "contradict",
            "Contradict",
            "Record trusted evidence that this draft claim is false or unsafe.",
            "A contradicted event blocks durable promotion trust and keeps the contradiction inspectable.",
        ),
        (
            "supersede",
            "Supersede",
            "Record trusted evidence that a newer claim replaces this one.",
            "A superseded event prevents stale cloud output from being promoted as current evidence.",
        ),
        (
            "inconclusive",
            "Mark inconclusive",
            "Record that the claim was reviewed but not verified.",
            "An inconclusive event keeps the claim out of L3/L4 promotion.",
        ),
    ]
    .into_iter()
    .map(|(action, label, intent, trust_effect)| {
        review_action_option(ReviewActionOptionInput {
            target_type: "claim",
            target_id: claim_id,
            action,
            label,
            intent,
            trust_effect,
            requires_evidence: true,
            requires_destructive_confirmation: false,
            enabled: true,
            disabled_reason: None,
        })
    })
    .collect()
}

fn proposal_action_options(
    proposal: &StoredLearningCriticProposal,
    readiness: &str,
    missing_verification_claim_ids: &[i64],
) -> Vec<ReviewActionOption> {
    let mut options = Vec::new();
    match proposal.action {
        LearningCriticAction::ProposePromotion if !missing_verification_claim_ids.is_empty() => {
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "confirm_and_apply",
                label: "Confirm and apply",
                intent: "Record one trusted verification event for linked unverified claims, then apply through storage gates.",
                trust_effect: "Cloud draft claims become durable only if the verification event satisfies the source-trust rule.",
                requires_evidence: true,
                requires_destructive_confirmation: false,
                enabled: true,
                disabled_reason: None,
            }));
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "confirm",
                label: "Confirm only",
                intent: "Record verification for linked claims without applying the proposal yet.",
                trust_effect:
                    "The proposal can become apply-ready after durable promotion trust is present.",
                requires_evidence: true,
                requires_destructive_confirmation: false,
                enabled: true,
                disabled_reason: None,
            }));
        }
        LearningCriticAction::ProposePromotion => {
            let manual_semantic_apply =
                readiness == "semantic_support_diversity_requires_manual_review";
            let enabled = readiness == "ready_for_batch_apply" || manual_semantic_apply;
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "apply",
                label: "Apply",
                intent: "Apply this verified promotion proposal through storage-level gates.",
                trust_effect:
                    "No new verification is created; existing durable promotion trust is required.",
                requires_evidence: false,
                requires_destructive_confirmation: false,
                enabled,
                disabled_reason: (!enabled).then(|| "proposal is not apply-ready".to_string()),
            }));
        }
        LearningCriticAction::Decay => {
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "apply",
                label: "Apply destructive change",
                intent: "Apply the reviewed decay/forget proposal.",
                trust_effect: "The claim lifecycle may move to decayed or forgotten.",
                requires_evidence: false,
                requires_destructive_confirmation: true,
                enabled: true,
                disabled_reason: None,
            }));
        }
        LearningCriticAction::RequestVerification => {
            let semantic_review_only =
                readiness == "semantic_review_only_candidate_requires_resolution";
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "confirm",
                label: "Confirm evidence",
                intent: "Record trusted verification for claims referenced by this verification request.",
                trust_effect: "The claim can satisfy durable promotion trust if the verifier source is trusted.",
                requires_evidence: true,
                requires_destructive_confirmation: false,
                enabled: !proposal.claim_ids.is_empty(),
                disabled_reason: proposal
                    .claim_ids
                    .is_empty()
                    .then(|| "proposal does not reference claims".to_string()),
            }));
            if semantic_review_only {
                options.push(review_action_option(ReviewActionOptionInput {
                    target_type: "proposal",
                    target_id: proposal.id,
                    action: "accept",
                    label: "Accept resolution",
                    intent: "Record reviewer evidence that this review-only semantic candidate is resolved without creating an L4 fact.",
                    trust_effect:
                        "The proposal closes as accepted; no verification event, promotion, or semantic fact is created.",
                    requires_evidence: true,
                    requires_destructive_confirmation: false,
                    enabled: true,
                    disabled_reason: None,
                }));
            }
            if !semantic_review_only {
                options.push(review_action_option(ReviewActionOptionInput {
                    target_type: "proposal",
                    target_id: proposal.id,
                    action: "apply",
                    label: "Mark waiting",
                    intent: "Apply the request-verification proposal, leaving it waiting for external verification.",
                    trust_effect: "No claim is promoted; the proposal records that verification is still required.",
                    requires_evidence: false,
                    requires_destructive_confirmation: false,
                    enabled: true,
                    disabled_reason: None,
                }));
            }
        }
        LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => {
            options.push(review_action_option(ReviewActionOptionInput {
                target_type: "proposal",
                target_id: proposal.id,
                action: "apply",
                label: "Close as applied",
                intent: "Close the audit proposal without changing claim lifecycle.",
                trust_effect: "No promotion or semantic fact is created.",
                requires_evidence: false,
                requires_destructive_confirmation: false,
                enabled: true,
                disabled_reason: None,
            }));
        }
    }
    options.push(review_action_option(ReviewActionOptionInput {
        target_type: "proposal",
        target_id: proposal.id,
        action: "reject",
        label: "Reject",
        intent: "Reject this proposal without mutating linked claims.",
        trust_effect: "The proposal closes as rejected; draft claims stay governed by their existing verification state.",
        requires_evidence: readiness == "semantic_review_only_candidate_requires_resolution",
        requires_destructive_confirmation: false,
        enabled: true,
        disabled_reason: None,
    }));
    if !matches!(
        proposal.action,
        LearningCriticAction::CreateCandidate | LearningCriticAction::Noop
    ) {
        options.push(review_action_option(ReviewActionOptionInput {
            target_type: "proposal",
            target_id: proposal.id,
            action: "wait",
            label: "Wait",
            intent: "Keep this proposal open for more verification or local evidence.",
            trust_effect:
                "No claim lifecycle changes; the proposal remains visible in the review queue.",
            requires_evidence: false,
            requires_destructive_confirmation: false,
            enabled: true,
            disabled_reason: None,
        }));
    }
    options
}

fn is_review_batch_template_action(action: &ReviewActionOption) -> bool {
    action.enabled
        && action.requires_evidence
        && !action.requires_destructive_confirmation
        && matches!(action.action.as_str(), "confirm" | "contradict" | "supersede" | "inconclusive")
        && matches!(action.target_type.as_str(), "claim" | "proposal")
}

fn review_batch_template_operation(
    action: &ReviewActionOption,
    verifier_type: &str,
    evidence_kind: &str,
    evidence_id: &str,
    evidence_source: Option<String>,
) -> ReviewBatchTemplateOperation {
    let (claim_id, proposal_id) = match action.target_type.as_str() {
        "claim" => (Some(action.target_id), None),
        "proposal" => (None, Some(action.target_id)),
        other => panic!("unsupported review target type {other}"),
    };
    ReviewBatchTemplateOperation {
        claim_id,
        proposal_id,
        action: action.action.clone(),
        control_id: action.control_id.clone(),
        verifier_type: verifier_type.to_string(),
        evidence_ref: ReviewBatchTemplateEvidenceRef {
            kind: evidence_kind.to_string(),
            id: evidence_id.to_string(),
            source: evidence_source,
        },
    }
}

fn review_batch_template_ui_hint(
    action: &str,
    target_type: &str,
    operation_count: usize,
    requires_evidence_fill: bool,
) -> ReviewBatchTemplateUiHint {
    ReviewBatchTemplateUiHint {
        form_title: format!("Review {operation_count} {target_type} {action} action(s)"),
        submit_label: "Dry-run verification batch".to_string(),
        dry_run_first: true,
        mutation_tool: "soma_review_batch".to_string(),
        allowed_actions: vec![
            "confirm".to_string(),
            "contradict".to_string(),
            "supersede".to_string(),
            "inconclusive".to_string(),
        ],
        target_type: target_type.to_string(),
        operation_count,
        requires_evidence_fill,
        evidence_form: review_evidence_form(action, "Prepare batch"),
    }
}

struct ReviewActionOptionInput<'a> {
    target_type: &'a str,
    target_id: i64,
    action: &'a str,
    label: &'a str,
    intent: &'a str,
    trust_effect: &'a str,
    requires_evidence: bool,
    requires_destructive_confirmation: bool,
    enabled: bool,
    disabled_reason: Option<String>,
}

fn review_action_option(input: ReviewActionOptionInput<'_>) -> ReviewActionOption {
    let control_id = review_action_control_id(input.target_type, input.target_id, input.action);
    let id_flag = match input.target_type {
        "claim" => "--claim-id",
        "proposal" => "--proposal-id",
        other => panic!("unsupported review target type {other}"),
    };
    let id_key = match input.target_type {
        "claim" => "claim_id",
        "proposal" => "proposal_id",
        other => panic!("unsupported review target type {other}"),
    };
    let mut cli_hint = format!(
        "soma context review-action {id_flag} {} --action {} --control-id {}",
        input.target_id, input.action, control_id
    );
    if input.requires_evidence {
        cli_hint.push_str(
            " --verifier <user|test|tool|local_observation|correction> --evidence-kind <kind> --evidence-id <id>",
        );
    }
    if input.requires_destructive_confirmation {
        cli_hint.push_str(" --confirm-destructive");
    }

    let mut mcp_arguments_template = json!({
        id_key: input.target_id,
        "action": input.action,
        "control_id": control_id,
    });
    if input.requires_evidence {
        mcp_arguments_template["verifier_type"] =
            json!("<user|test|tool|local_observation|correction>");
        mcp_arguments_template["evidence_ref"] = json!({
            "kind": "<kind>",
            "id": "<id>",
            "source": "<source>"
        });
    }
    if input.requires_destructive_confirmation {
        mcp_arguments_template["confirm_destructive"] = json!(true);
    }

    let ui_hint = review_action_ui_hint(&input);

    ReviewActionOption {
        control_id,
        action: input.action.to_string(),
        target_type: input.target_type.to_string(),
        target_id: input.target_id,
        label: input.label.to_string(),
        intent: input.intent.to_string(),
        trust_effect: input.trust_effect.to_string(),
        requires_evidence: input.requires_evidence,
        verification_template: review_verification_template(&input),
        requires_destructive_confirmation: input.requires_destructive_confirmation,
        operator_authorization_required: true,
        agent_self_authorization_forbidden: true,
        authorization_boundary: review_action_authorization_boundary(),
        enabled: input.enabled,
        disabled_reason: input.disabled_reason,
        cli_hint,
        mcp_tool: "soma_review_action".to_string(),
        mcp_arguments_template,
        ui_hint,
    }
}

pub fn review_action_control_id(target_type: &str, target_id: i64, action: &str) -> String {
    format!("{target_type}:{target_id}:{action}")
}

fn review_action_authorization_boundary() -> String {
    "review_action_requires_explicit_operator_or_independently_inspectable_tool_test_local_correction_evidence; autonomous agent judgment alone cannot authorize mutation, close review-only semantic candidates, verify claims, promote drafts, or record proof"
        .to_string()
}

fn review_action_ui_hint(input: &ReviewActionOptionInput<'_>) -> ReviewActionUiHint {
    let (group, order, button_style, icon) =
        review_action_ui_classification(input.action, input.requires_destructive_confirmation);
    ReviewActionUiHint {
        group: group.to_string(),
        order,
        button_style: button_style.to_string(),
        icon: icon.to_string(),
        confirmation: review_action_confirmation(input),
        evidence_form: input
            .requires_evidence
            .then(|| review_evidence_form(input.action, input.label)),
    }
}

fn review_verification_template(
    input: &ReviewActionOptionInput<'_>,
) -> Option<ReviewVerificationTemplate> {
    if !input.requires_evidence {
        return None;
    }
    let result = review_action_verification_result(input.action);
    let accepted_verifier_types = review_verifier_type_options();
    let durable_promotion_verifier_types =
        if result == "confirmed" { accepted_verifier_types.clone() } else { Vec::new() };
    Some(ReviewVerificationTemplate {
        result: result.to_string(),
        accepted_verifier_types,
        durable_promotion_verifier_types,
        evidence_ref_template: ReviewBatchTemplateEvidenceRef {
            kind: "<test|tool_output|user_note|local_observation|correction_record>".to_string(),
            id: "<stable-evidence-id>".to_string(),
            source: Some("<client|command|tool|human-reviewer>".to_string()),
        },
        example_evidence_refs: review_verification_evidence_examples(result),
        operator_checklist: review_verification_operator_checklist(result),
        trust_boundary: "verification_template_never_accepts_cloud_draft_as_evidence; mutation_still_requires_soma_review_action_or_soma_review_batch_storage_gates".to_string(),
    })
}

fn review_verifier_type_options() -> Vec<String> {
    ["user", "test", "tool", "local_observation", "correction"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn review_forbidden_evidence_sources() -> Vec<String> {
    ["cloud_draft", "cloud_output_text", "review_render_output", "client_binding_status"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn review_verification_evidence_examples(result: &str) -> Vec<ReviewVerificationEvidenceExample> {
    let base = match result {
        "confirmed" => [
            ("tool", "tool_output", "cargo-test-client-binding-proof"),
            ("test", "test_report", "cargo-test-context-cli"),
            ("user", "user_note", "operator-confirmed-review-note"),
            ("local_observation", "local_observation", "review-render-visible-in-client"),
            ("correction", "correction_record", "user-correction-id"),
        ],
        "contradicted" => [
            ("test", "test_report", "failing-regression-test"),
            ("tool", "tool_output", "tool-result-contradicts-claim"),
            ("user", "user_note", "operator-rejects-claim"),
            ("local_observation", "local_observation", "local-state-contradicts-claim"),
            ("correction", "correction_record", "correction-invalidates-claim"),
        ],
        "superseded" => [
            ("user", "user_note", "newer-user-decision"),
            ("tool", "tool_output", "newer-tool-result"),
            ("test", "test_report", "newer-test-report"),
            ("local_observation", "local_observation", "newer-local-state"),
            ("correction", "correction_record", "replacement-correction-id"),
        ],
        _ => [
            ("user", "user_note", "operator-reviewed-but-inconclusive"),
            ("tool", "tool_output", "tool-output-insufficient"),
            ("test", "test_report", "test-result-inconclusive"),
            ("local_observation", "local_observation", "local-observation-inconclusive"),
            ("correction", "correction_record", "correction-needs-follow-up"),
        ],
    };
    base.into_iter()
        .map(|(verifier_type, kind, id)| ReviewVerificationEvidenceExample {
            verifier_type: verifier_type.to_string(),
            evidence_ref: ReviewBatchTemplateEvidenceRef {
                kind: kind.to_string(),
                id: id.to_string(),
                source: Some("replace-with-actual-evidence-source".to_string()),
            },
        })
        .collect()
}

fn review_verification_operator_checklist(result: &str) -> Vec<String> {
    let mut checklist = vec![
        "choose_verifier_type_from_user_test_tool_local_observation_or_correction".to_string(),
        "evidence_ref_must_point_to_independently_inspectable_user_tool_test_local_or_correction_evidence".to_string(),
        "cloud_draft_is_not_valid_evidence_source".to_string(),
        "leave_source_set_to_the_client_tool_command_or_reviewer_that_produced_the_evidence".to_string(),
    ];
    if result == "confirmed" {
        checklist.push(
            "confirmed_verification_can_unlock_durable_promotion_only_after_storage_gates_recheck_trust"
                .to_string(),
        );
    } else {
        checklist.push("non_confirmed_verification_does_not_unlock_l3_or_l4_promotion".to_string());
    }
    checklist
}

fn review_action_ui_classification(
    action: &str,
    requires_destructive_confirmation: bool,
) -> (&'static str, u16, &'static str, &'static str) {
    if requires_destructive_confirmation {
        return ("destructive", 90, "danger", "trash");
    }
    match action {
        "confirm_and_apply" => ("promotion", 10, "primary", "check-circle"),
        "confirm" => ("verification", 20, "primary", "check"),
        "apply" => ("proposal_decision", 30, "primary", "play"),
        "accept" => ("proposal_decision", 40, "secondary", "thumbs-up"),
        "wait" => ("proposal_decision", 50, "secondary", "clock"),
        "inconclusive" => ("verification", 60, "secondary", "circle-help"),
        "supersede" => ("verification", 70, "warning", "replace"),
        "contradict" => ("verification", 80, "danger", "x-circle"),
        "reject" => ("proposal_decision", 85, "warning", "x"),
        _ => ("proposal_decision", 100, "secondary", "circle"),
    }
}

fn review_action_confirmation(
    input: &ReviewActionOptionInput<'_>,
) -> Option<ReviewActionConfirmation> {
    if input.requires_destructive_confirmation {
        return Some(ReviewActionConfirmation {
            title: "Confirm destructive lifecycle change".to_string(),
            body: "This action may decay or forget remembered evidence. The storage gate still requires explicit destructive confirmation.".to_string(),
            confirm_label: input.label.to_string(),
        });
    }
    (input.action == "confirm_and_apply").then(|| ReviewActionConfirmation {
        title: "Confirm evidence and apply proposal".to_string(),
        body: "This records trusted evidence for linked claims before applying the proposal through storage gates.".to_string(),
        confirm_label: input.label.to_string(),
    })
}

fn review_evidence_form(action: &str, submit_label: &str) -> ReviewEvidenceForm {
    ReviewEvidenceForm {
        required: true,
        verifier_type_options: review_verifier_type_options(),
        result: review_action_verification_result(action).to_string(),
        fields: vec![
            ReviewEvidenceField {
                name: "evidence_ref.kind".to_string(),
                label: "Evidence kind".to_string(),
                required: true,
                placeholder: "test | command | file | user_note".to_string(),
            },
            ReviewEvidenceField {
                name: "evidence_ref.id".to_string(),
                label: "Evidence id".to_string(),
                required: true,
                placeholder: "stable id, command id, file path, or review note id".to_string(),
            },
            ReviewEvidenceField {
                name: "evidence_ref.source".to_string(),
                label: "Evidence source".to_string(),
                required: false,
                placeholder: "client, tool, test command, or local observer".to_string(),
            },
        ],
        submit_label: submit_label.to_string(),
    }
}

fn review_action_verification_result(action: &str) -> &'static str {
    match action {
        "confirm" | "confirm_and_apply" => "confirmed",
        "contradict" => "contradicted",
        "supersede" => "superseded",
        "inconclusive" => "inconclusive",
        _ => "confirmed",
    }
}

fn proposal_claim_review(
    storage: &Storage,
    claim_id: i64,
) -> Result<ProposalClaimReview, StorageError> {
    let Some(claim) = storage.claim_record(claim_id)? else {
        return Ok(ProposalClaimReview {
            claim_id,
            text: None,
            lifecycle_state: None,
            durable_promotion_trust: None,
            missing: true,
        });
    };
    let durable_promotion_trust = storage.claim_has_durable_promotion_trust(claim_id)?;
    Ok(ProposalClaimReview {
        claim_id,
        text: Some(claim.text),
        lifecycle_state: Some(claim.lifecycle_state.as_str().to_string()),
        durable_promotion_trust: Some(durable_promotion_trust),
        missing: false,
    })
}

fn promotion_missing_verification_claim_ids(
    proposal: &StoredLearningCriticProposal,
    linked_claims: &[ProposalClaimReview],
) -> Vec<i64> {
    if proposal.action != LearningCriticAction::ProposePromotion {
        return Vec::new();
    }
    linked_claims
        .iter()
        .filter(|claim| claim.durable_promotion_trust == Some(false) || claim.missing)
        .map(|claim| claim.claim_id)
        .collect()
}

fn proposal_readiness(
    proposal: &StoredLearningCriticProposal,
    missing_verification_claim_ids: &[i64],
    semantic_review: Option<&SemanticPromotionReview>,
) -> String {
    if !matches!(
        proposal.status,
        LearningCriticProposalStatus::Queued
            | LearningCriticProposalStatus::WaitingVerification
            | LearningCriticProposalStatus::Accepted
    ) {
        return "closed".to_string();
    }
    match proposal.action {
        LearningCriticAction::ProposePromotion if !missing_verification_claim_ids.is_empty() => {
            "needs_claim_verification".to_string()
        }
        LearningCriticAction::ProposePromotion
            if proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact)
                && semantic_review_has_limited_support_diversity(semantic_review) =>
        {
            "semantic_support_diversity_requires_manual_review".to_string()
        }
        LearningCriticAction::ProposePromotion => "ready_for_batch_apply".to_string(),
        LearningCriticAction::RequestVerification
            if semantic_review.is_some_and(|review| review.target_lifecycle_state == "none") =>
        {
            "semantic_review_only_candidate_requires_resolution".to_string()
        }
        LearningCriticAction::RequestVerification
        | LearningCriticAction::Decay
        | LearningCriticAction::CreateCandidate
        | LearningCriticAction::Noop => "explicit_review_required".to_string(),
    }
}

fn semantic_review_has_limited_support_diversity(
    semantic_review: Option<&SemanticPromotionReview>,
) -> bool {
    semantic_review
        .and_then(|review| review.support_diversity.as_ref())
        .is_none_or(|diversity| diversity.bias_risk != "low_diverse_support")
}

fn proposal_requires_manual_review(readiness: &str) -> bool {
    matches!(
        readiness,
        "explicit_review_required"
            | "semantic_support_diversity_requires_manual_review"
            | "semantic_review_only_candidate_requires_resolution"
    )
}

fn proposal_review_reason(
    proposal: &StoredLearningCriticProposal,
    missing_verification_claim_ids: &[i64],
    readiness: &str,
) -> String {
    if proposal.action == LearningCriticAction::ProposePromotion
        && !missing_verification_claim_ids.is_empty()
    {
        if proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact) {
            return "semantic_fact_promotion_waits_for_claim_verification".to_string();
        }
        return "proposal_waits_for_claim_verification".to_string();
    }
    if readiness == "semantic_support_diversity_requires_manual_review" {
        return "semantic_fact_support_diversity_requires_manual_review".to_string();
    }
    if readiness == "semantic_review_only_candidate_requires_resolution" {
        return "semantic_review_only_candidate_requires_resolution".to_string();
    }
    match (proposal.action, proposal.status, proposal.target_lifecycle_state) {
        (
            LearningCriticAction::ProposePromotion,
            LearningCriticProposalStatus::Queued
            | LearningCriticProposalStatus::WaitingVerification
            | LearningCriticProposalStatus::Accepted,
            Some(LifecycleState::SemanticFact),
        ) => "semantic_fact_ready_for_gated_apply".to_string(),
        (
            LearningCriticAction::ProposePromotion,
            LearningCriticProposalStatus::Queued
            | LearningCriticProposalStatus::WaitingVerification
            | LearningCriticProposalStatus::Accepted,
            Some(LifecycleState::LongTermMemory),
        ) => "proposal_ready_for_gated_apply".to_string(),
        (LearningCriticAction::RequestVerification, _, _) => {
            "proposal_requests_external_verification".to_string()
        }
        (LearningCriticAction::Decay, _, _) => {
            "decay_proposal_requires_explicit_review".to_string()
        }
        _ => "proposal_open_for_operator_review".to_string(),
    }
}

fn proposal_recommended_next_action(
    proposal: &StoredLearningCriticProposal,
    missing_verification_claim_ids: &[i64],
    readiness: &str,
) -> String {
    if proposal.action == LearningCriticAction::ProposePromotion
        && !missing_verification_claim_ids.is_empty()
    {
        if proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact) {
            return "Record trusted verification for the representative claim, then review whether repeated verified L3 support justifies a stable L4 fact.".to_string();
        }
        return "Record verification events for the listed claims before applying this proposal."
            .to_string();
    }
    if readiness == "semantic_support_diversity_requires_manual_review" {
        return "Review support_diversity and bias_risk manually; apply this single proposal only if the repeated L3 support justifies an L4 semantic fact.".to_string();
    }
    if readiness == "semantic_review_only_candidate_requires_resolution" {
        return "Resolve the semantic candidate with explicit user/tool/local evidence; do not apply it as L4 from this request.".to_string();
    }
    match proposal.action {
        LearningCriticAction::ProposePromotion
            if proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact) =>
        {
            "Apply this semantic proposal only after reviewing the L3 support evidence.".to_string()
        }
        LearningCriticAction::ProposePromotion => {
            "Apply this proposal through apply-ready or the single-proposal gated apply path, or reject it with set-status.".to_string()
        }
        LearningCriticAction::Decay => {
            "Review the decay/forget intent explicitly, then apply this proposal or include decay in an apply-ready batch.".to_string()
        }
        LearningCriticAction::RequestVerification => {
            "Record verification events, then apply or reject the proposal.".to_string()
        }
        LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => {
            "Apply to close the audit record, or reject it with set-status.".to_string()
        }
    }
}

fn review_interruption_summary(proposal_items: &[ProposalReviewItem]) -> ReviewInterruptionSummary {
    let interrupt_count =
        proposal_items.iter().filter(|item| item.interruption_hint.should_interrupt).count();
    let digest_count = proposal_items
        .iter()
        .filter(|item| item.interruption_hint.surface == "review_digest")
        .count();
    let queue_only_count = proposal_items
        .iter()
        .filter(|item| item.interruption_hint.surface == "review_queue")
        .count();
    if interrupt_count > 0 {
        ReviewInterruptionSummary {
            policy: "l4_semantic_interruption_v1".to_string(),
            should_interrupt: true,
            interrupt_count,
            digest_count,
            queue_only_count,
            next_surface: "review_digest".to_string(),
            cadence: "at_most_once_per_session".to_string(),
            cooldown_seconds: 3600,
            reason: "l4_semantic_review_digest_pending".to_string(),
        }
    } else {
        ReviewInterruptionSummary {
            policy: "l4_semantic_interruption_v1".to_string(),
            should_interrupt: false,
            interrupt_count,
            digest_count,
            queue_only_count,
            next_surface: "review_queue".to_string(),
            cadence: "on_demand".to_string(),
            cooldown_seconds: 0,
            reason: "no_interruptible_l4_semantic_proposals".to_string(),
        }
    }
}

fn proposal_interruption_hint(
    proposal: &StoredLearningCriticProposal,
    readiness: &str,
    semantic_review: Option<&SemanticPromotionReview>,
) -> ReviewInterruptionHint {
    if semantic_review.is_some()
        && proposal.action == LearningCriticAction::ProposePromotion
        && proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact)
        && matches!(
            proposal.status,
            LearningCriticProposalStatus::Queued
                | LearningCriticProposalStatus::WaitingVerification
                | LearningCriticProposalStatus::Accepted
        )
    {
        let reason = if readiness == "ready_for_batch_apply" {
            "verified_l4_semantic_fact_ready_for_review"
        } else if readiness == "semantic_support_diversity_requires_manual_review" {
            "l4_semantic_support_diversity_requires_manual_review"
        } else {
            "l4_semantic_fact_needs_trusted_verification"
        };
        return ReviewInterruptionHint {
            policy: "l4_semantic_interruption_v1".to_string(),
            should_interrupt: true,
            level: "non_blocking_digest".to_string(),
            surface: "review_digest".to_string(),
            cadence: "at_most_once_per_session".to_string(),
            cooldown_seconds: 3600,
            batch_key: "l4_semantic_promotion".to_string(),
            reason: reason.to_string(),
        };
    }
    ReviewInterruptionHint {
        policy: "l4_semantic_interruption_v1".to_string(),
        should_interrupt: false,
        level: "queue_only".to_string(),
        surface: "review_queue".to_string(),
        cadence: "on_demand".to_string(),
        cooldown_seconds: 0,
        batch_key: match proposal.action {
            LearningCriticAction::RequestVerification => "verification_request",
            LearningCriticAction::Decay => "destructive_review",
            LearningCriticAction::ProposePromotion => "non_l4_promotion",
            LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => "audit_closure",
        }
        .to_string(),
        reason: "not_high_value_l4_semantic_proposal".to_string(),
    }
}

fn proposal_cli_hint(
    proposal: &StoredLearningCriticProposal,
    missing_verification_claim_ids: &[i64],
    readiness: &str,
) -> String {
    match proposal.action {
        LearningCriticAction::ProposePromotion if !missing_verification_claim_ids.is_empty() => {
            format!(
                "soma context verify-claim --proposal-id {} --verifier <user|test|tool|local_observation|correction> --result confirmed --evidence-kind <kind> --evidence-id <id>; then: soma context learning-proposals apply-ready",
                proposal.id
            )
        }
        LearningCriticAction::ProposePromotion => {
            format!(
                "soma context learning-proposals apply-ready # or: soma context learning-proposals apply --proposal-id {}",
                proposal.id
            )
        }
        LearningCriticAction::Decay => {
            format!(
                "soma context learning-proposals apply-ready --include-decay # or: soma context learning-proposals apply --proposal-id {} --confirm-destructive",
                proposal.id
            )
        }
        LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => {
            format!(
                "soma context learning-proposals apply-ready --include-noop # or: soma context learning-proposals apply --proposal-id {}",
                proposal.id
            )
        }
        LearningCriticAction::RequestVerification
            if readiness == "semantic_review_only_candidate_requires_resolution" =>
        {
            format!(
                "soma context review-action --proposal-id {} --action accept|reject|wait --control-id proposal:{}:<action> --verifier <user|test|tool|local_observation|correction> --evidence-kind <kind> --evidence-id <id>",
                proposal.id, proposal.id
            )
        }
        LearningCriticAction::RequestVerification => format!(
            "soma context verify-claim --claim-id <claim-id> --verifier <user|test|tool|local_observation|correction> --result confirmed --evidence-kind <kind> --evidence-id <id>; then: soma context learning-proposals apply --proposal-id {}",
            proposal.id
        ),
    }
}

fn batch_apply_cli_hint(project: Option<&str>, session_id: Option<&str>) -> String {
    let mut hint = "soma context learning-proposals apply-ready".to_string();
    if let Some(project) = project {
        hint.push_str(" --project ");
        hint.push_str(project);
    }
    if let Some(session_id) = session_id {
        hint.push_str(" --session-id ");
        hint.push_str(session_id);
    }
    hint
}

fn semantic_promotion_review(
    storage: &Storage,
    proposal: &StoredLearningCriticProposal,
    linked_claims: &[ProposalClaimReview],
) -> Result<Option<SemanticPromotionReview>, StorageError> {
    let semantic_kind = semantic_review_kind(proposal);
    let Some(semantic_kind) = semantic_kind else { return Ok(None) };

    let representative_claim_ids =
        linked_claims.iter().filter(|claim| !claim.missing).map(|claim| claim.claim_id).collect();
    let mut support_claim_ids = support_claim_ids_from_evidence(&proposal.evidence_refs);
    if support_claim_ids.is_empty() {
        support_claim_ids = linked_claims
            .iter()
            .filter(|claim| !claim.missing)
            .map(|claim| claim.claim_id)
            .collect();
    }
    let support_claims = support_claim_ids
        .iter()
        .filter_map(|claim_id| storage.claim_record(*claim_id).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    let support_diversity = if support_claims.is_empty() {
        None
    } else {
        Some(semantic_support_diversity(storage, &support_claims)?)
    };
    let (target_lifecycle_state, rule, required_verification, review_prompt) =
        semantic_review_copy(semantic_kind, linked_claims);
    let grouping_rule = semantic_grouping_rule_from_reason(&proposal.reason);
    let trusted = linked_claims
        .iter()
        .filter(|claim| !claim.missing)
        .all(|claim| claim.durable_promotion_trust == Some(true));
    let default_diversity;
    let diversity_for_score = if let Some(diversity) = support_diversity.as_ref() {
        diversity
    } else {
        default_diversity = SemanticSupportDiversity {
            distinct_task_frame_count: 0,
            distinct_project_count: 0,
            distinct_source_type_count: 0,
            distinct_verifier_type_count: 0,
            distinct_evidence_source_count: 0,
            support_projects: Vec::new(),
            single_task_frame_only: false,
            single_project_only: false,
            single_source_type_only: false,
            single_verifier_type_only: false,
            single_evidence_source_only: false,
            bias_risk: "insufficient_support".to_string(),
        };
        &default_diversity
    };
    let readiness_score = semantic_readiness_score(
        grouping_rule.as_deref().unwrap_or("unknown_grouping_rule"),
        trusted,
        support_claim_ids.len(),
        diversity_for_score,
    );
    let review_rubric = semantic_review_rubric(semantic_kind, support_diversity.as_ref());
    let review_card = semantic_review_card(
        semantic_kind,
        &target_lifecycle_state,
        grouping_rule.as_deref(),
        trusted,
        support_claim_ids.len(),
        support_diversity.as_ref(),
        &readiness_score,
        &review_rubric,
    );
    Ok(Some(SemanticPromotionReview {
        target_lifecycle_state,
        rule,
        grouping_rule,
        representative_claim_ids,
        support_count: support_claim_ids.len(),
        support_claim_ids,
        support_evidence_refs: proposal.evidence_refs.clone(),
        support_diversity,
        readiness_score,
        required_verification,
        review_card,
        review_rubric,
        review_prompt,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticReviewKind {
    L4Promotion,
    LatentReviewOnly,
    NegationConflictReviewOnly,
}

fn semantic_review_kind(proposal: &StoredLearningCriticProposal) -> Option<SemanticReviewKind> {
    if proposal.action == LearningCriticAction::ProposePromotion
        && proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact)
    {
        return Some(SemanticReviewKind::L4Promotion);
    }
    if proposal.action == LearningCriticAction::RequestVerification
        && semantic_review_candidate_source(proposal, SEMANTIC_LATENT_REVIEW_SOURCE)
    {
        return Some(SemanticReviewKind::LatentReviewOnly);
    }
    if proposal.action == LearningCriticAction::RequestVerification
        && semantic_review_candidate_source(proposal, SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE)
    {
        return Some(SemanticReviewKind::NegationConflictReviewOnly);
    }
    None
}

fn semantic_review_candidate_source(proposal: &StoredLearningCriticProposal, source: &str) -> bool {
    proposal.evidence_refs.iter().any(|evidence_ref| {
        evidence_ref.kind == "semantic_review_candidate"
            && evidence_ref.source.as_deref() == Some(source)
    })
}

fn semantic_review_copy(
    kind: SemanticReviewKind,
    linked_claims: &[ProposalClaimReview],
) -> (String, String, String, String) {
    match kind {
        SemanticReviewKind::L4Promotion => {
            let required_verification =
                if linked_claims.iter().any(|claim| claim.durable_promotion_trust != Some(true)) {
                    "representative_claim_requires_trusted_verification_before_l4_apply"
                        .to_string()
                } else {
                    "representative_claim_has_durable_trust_apply_still_requires_gated_review"
                        .to_string()
                };
            (
                LifecycleState::SemanticFact.as_str().to_string(),
                SEMANTIC_LEARNING_RULE.to_string(),
                required_verification,
                "Confirm that repeated verified L3 evidence should become a stable L4 semantic fact; reject if the support is stale, too broad, or only a cloud draft."
                    .to_string(),
            )
        }
        SemanticReviewKind::LatentReviewOnly => (
            "none".to_string(),
            SEMANTIC_LATENT_REVIEW_RULE.to_string(),
            "review_only_candidate_requires_explicit_l4_resolution_before_semantic_fact"
                .to_string(),
            "Decide whether these paraphrase-like L3 claims describe the same abstraction; this request has no L4 target and must be rejected or followed by a separate explicit L4 proposal."
                .to_string(),
        ),
        SemanticReviewKind::NegationConflictReviewOnly => (
            "none".to_string(),
            SEMANTIC_NEGATION_CONFLICT_RULE.to_string(),
            "negation_conflict_requires_human_or_tool_resolution_before_l4"
                .to_string(),
            "Resolve the opposing polarity between these L3 claims; this conflict review request has no L4 target and must not be applied as a semantic fact."
                .to_string(),
        ),
    }
}

fn semantic_review_rubric(
    kind: SemanticReviewKind,
    support_diversity: Option<&SemanticSupportDiversity>,
) -> Vec<SemanticReviewRubricItem> {
    match kind {
        SemanticReviewKind::L4Promotion => {
            let mut items = vec![
                rubric_item(
                    "support_claims_are_verified_l3",
                    "Are all support claims verified L3 evidence rather than draft observations?",
                    &[
                        "semantic_review.support_claim_ids",
                        "linked claim durable_promotion_trust=true",
                        "linked claim lifecycle_state=long_term_memory",
                    ],
                    "wait_for_verification_or_reject_l4_promotion",
                ),
                rubric_item(
                    "abstraction_scope_matches_support",
                    "Does the proposed L4 fact state only what the support claims actually repeat?",
                    &[
                        "semantic_review.grouping_rule",
                        "semantic_review.representative_claim_ids",
                        "semantic_review.support_claim_ids",
                    ],
                    "reject_or_split_overbroad_semantic_fact",
                ),
                rubric_item(
                    "support_diversity_sufficient_or_manual_acceptance",
                    "Is support diversity sufficient, or has the reviewer explicitly accepted the limited-context risk?",
                    &[
                        "semantic_review.support_diversity.bias_risk",
                        "semantic_review.support_diversity.distinct_task_frame_count",
                        "semantic_review.support_diversity.distinct_evidence_source_count",
                    ],
                    "manual_review_required_no_auto_l4_apply",
                ),
            ];
            if support_diversity
                .is_none_or(|diversity| diversity.bias_risk != "low_diverse_support")
            {
                items.push(rubric_item(
                    "limited_diversity_bias_risk_visible",
                    "Has the reviewer inspected why the support is limited before applying?",
                    &["decision_packet.blocking_reasons", "semantic_review.support_diversity"],
                    "keep_proposal_pending_until_support_risk_is_resolved",
                ));
            }
            items
        }
        SemanticReviewKind::LatentReviewOnly => vec![
            rubric_item(
                "latent_candidate_same_abstraction",
                "Do the paraphrase-like L3 claims truly describe the same abstraction?",
                &[
                    "semantic_review.support_claim_ids",
                    "semantic_review.grouping_rule",
                    "human_or_tool_resolution_evidence",
                ],
                "keep_as_review_only_candidate",
            ),
            rubric_item(
                "explicit_l4_proposal_required",
                "If the abstraction is valid, should a separate explicit L4 proposal be created?",
                &[
                    "semantic_review.target_lifecycle_state=none",
                    "new proposal reason with cited support",
                ],
                "do_not_apply_review_only_candidate_as_l4",
            ),
        ],
        SemanticReviewKind::NegationConflictReviewOnly => vec![
            rubric_item(
                "resolve_negation_polarity",
                "Which polarity is correct, and what evidence resolves the conflict?",
                &[
                    "semantic_review.support_claim_ids",
                    "tool_or_user_resolution_evidence",
                    "correction target if one claim is stale",
                ],
                "keep_conflict_unresolved_no_l4_projection",
            ),
            rubric_item(
                "correction_or_policy_choice_recorded",
                "Should the outcome become a correction, policy, or no durable fact?",
                &[
                    "review_action evidence_ref",
                    "correction or policy target",
                    "semantic_review.target_lifecycle_state=none",
                ],
                "reject_review_request_or_request_more_evidence",
            ),
        ],
    }
}

fn semantic_review_card(
    kind: SemanticReviewKind,
    target_lifecycle_state: &str,
    grouping_rule: Option<&str>,
    trusted: bool,
    support_count: usize,
    support_diversity: Option<&SemanticSupportDiversity>,
    readiness_score: &SemanticReadinessScore,
    review_rubric: &[SemanticReviewRubricItem],
) -> SemanticReviewCard {
    let review_state =
        semantic_review_card_state(kind, target_lifecycle_state, trusted, readiness_score);
    let support_summary = semantic_review_card_support_summary(
        support_count,
        grouping_rule.unwrap_or("unknown_grouping_rule"),
        support_diversity,
    );
    let allowed_actions = semantic_review_card_allowed_actions(kind, trusted, readiness_score);
    let blocked_actions = semantic_review_card_blocked_actions(kind, trusted, readiness_score);
    let checklist = review_rubric
        .iter()
        .map(|item| SemanticReviewChecklistItem {
            check_id: item.check_id.clone(),
            status: semantic_review_check_status(&item.check_id, kind, trusted, support_diversity),
            question: item.question.clone(),
            evidence_paths: item.required_evidence.clone(),
            fail_closed_reason: item.fail_closed_reason.clone(),
        })
        .collect();
    SemanticReviewCard {
        source: "soma_semantic_review_card_v1".to_string(),
        title: semantic_review_card_title(kind, target_lifecycle_state),
        review_state,
        support_summary,
        operator_authorization_required: true,
        agent_self_authorization_forbidden: true,
        review_decision_authority:
            "explicit_user_tool_test_local_observation_or_correction_evidence_only".to_string(),
        required_resolution_evidence: vec![
            "operator_intent_or_independent_tool_test_local_correction_evidence".to_string(),
            "evidence_ref.kind_and_id_filled".to_string(),
            "evidence_source_not_cloud_draft_or_review_render_output".to_string(),
        ],
        trust_boundary: "semantic_review_card_is_read_only: L4 semantic projection requires verified L3 support plus explicit review/apply storage gates; cloud_draft evidence alone cannot satisfy this card; autonomous agent judgment cannot authorize semantic review resolution".to_string(),
        allowed_actions,
        blocked_actions,
        checklist,
    }
}

fn semantic_review_card_title(kind: SemanticReviewKind, target_lifecycle_state: &str) -> String {
    match kind {
        SemanticReviewKind::L4Promotion => {
            format!("Review candidate L4 {target_lifecycle_state}")
        }
        SemanticReviewKind::LatentReviewOnly => {
            "Resolve latent-similar L3 candidate before any L4 proposal".to_string()
        }
        SemanticReviewKind::NegationConflictReviewOnly => {
            "Resolve semantic negation conflict before any L4 proposal".to_string()
        }
    }
}

fn semantic_review_card_state(
    kind: SemanticReviewKind,
    target_lifecycle_state: &str,
    trusted: bool,
    readiness_score: &SemanticReadinessScore,
) -> String {
    if target_lifecycle_state == "none" {
        return "review_only_resolution_required".to_string();
    }
    if !trusted {
        return "blocked_until_support_verified".to_string();
    }
    if readiness_score.blocks_l4_auto_apply {
        return "manual_l4_review_required".to_string();
    }
    match kind {
        SemanticReviewKind::L4Promotion => "verified_l4_candidate".to_string(),
        SemanticReviewKind::LatentReviewOnly | SemanticReviewKind::NegationConflictReviewOnly => {
            "review_only_resolution_required".to_string()
        }
    }
}

fn semantic_review_card_support_summary(
    support_count: usize,
    grouping_rule: &str,
    support_diversity: Option<&SemanticSupportDiversity>,
) -> String {
    if let Some(diversity) = support_diversity {
        format!(
            "support_claims={} grouping={} task_frames={} projects={} verifier_types={} evidence_sources={} bias_risk={}",
            support_count,
            grouping_rule,
            diversity.distinct_task_frame_count,
            diversity.distinct_project_count,
            diversity.distinct_verifier_type_count,
            diversity.distinct_evidence_source_count,
            diversity.bias_risk
        )
    } else {
        format!(
            "support_claims={} grouping={} bias_risk=insufficient_support",
            support_count, grouping_rule
        )
    }
}

fn semantic_review_card_allowed_actions(
    kind: SemanticReviewKind,
    trusted: bool,
    readiness_score: &SemanticReadinessScore,
) -> Vec<String> {
    match kind {
        SemanticReviewKind::L4Promotion if !trusted => vec![
            "confirm_support_claims".to_string(),
            "reject_l4_candidate".to_string(),
            "wait_for_more_evidence".to_string(),
        ],
        SemanticReviewKind::L4Promotion if readiness_score.blocks_l4_auto_apply => vec![
            "manual_apply_after_support_review".to_string(),
            "reject_l4_candidate".to_string(),
            "wait_for_more_diverse_support".to_string(),
        ],
        SemanticReviewKind::L4Promotion => vec![
            "apply_l4_candidate".to_string(),
            "reject_l4_candidate".to_string(),
            "wait_for_operator_review".to_string(),
        ],
        SemanticReviewKind::LatentReviewOnly | SemanticReviewKind::NegationConflictReviewOnly => {
            vec![
                "accept_resolution_with_evidence".to_string(),
                "reject_review_candidate_with_evidence".to_string(),
                "wait_for_more_evidence".to_string(),
            ]
        }
    }
}

fn semantic_review_card_blocked_actions(
    kind: SemanticReviewKind,
    trusted: bool,
    readiness_score: &SemanticReadinessScore,
) -> Vec<String> {
    match kind {
        SemanticReviewKind::L4Promotion => {
            let mut actions = Vec::new();
            if !trusted {
                actions.push("apply_l4_without_verified_l3_support".to_string());
            }
            if readiness_score.blocks_l4_auto_apply {
                actions.push("batch_apply_without_manual_l4_review".to_string());
            }
            actions
        }
        SemanticReviewKind::LatentReviewOnly | SemanticReviewKind::NegationConflictReviewOnly => {
            vec!["apply_review_only_candidate_as_l4_fact".to_string()]
        }
    }
}

fn semantic_review_check_status(
    check_id: &str,
    kind: SemanticReviewKind,
    trusted: bool,
    support_diversity: Option<&SemanticSupportDiversity>,
) -> String {
    match check_id {
        "support_claims_are_verified_l3" if trusted => "pass".to_string(),
        "support_claims_are_verified_l3" => "blocked".to_string(),
        "support_diversity_sufficient_or_manual_acceptance"
        | "limited_diversity_bias_risk_visible" => {
            if support_diversity
                .is_some_and(|diversity| diversity.bias_risk == "low_diverse_support")
            {
                "pass".to_string()
            } else {
                "manual_review_required".to_string()
            }
        }
        "explicit_l4_proposal_required" => "blocked_for_this_candidate".to_string(),
        "latent_candidate_same_abstraction"
        | "resolve_negation_polarity"
        | "correction_or_policy_choice_recorded" => "needs_reviewer_evidence".to_string(),
        "abstraction_scope_matches_support" => "needs_review".to_string(),
        _ if matches!(
            kind,
            SemanticReviewKind::LatentReviewOnly | SemanticReviewKind::NegationConflictReviewOnly
        ) =>
        {
            "needs_reviewer_evidence".to_string()
        }
        _ => "needs_review".to_string(),
    }
}

fn rubric_item(
    check_id: &str,
    question: &str,
    required_evidence: &[&str],
    fail_closed_reason: &str,
) -> SemanticReviewRubricItem {
    SemanticReviewRubricItem {
        check_id: check_id.to_string(),
        question: question.to_string(),
        required_evidence: required_evidence.iter().map(|value| (*value).to_string()).collect(),
        fail_closed_reason: fail_closed_reason.to_string(),
    }
}

fn support_claim_ids_from_evidence(
    evidence_refs: &[crate::storage::StoredEvidenceRef],
) -> Vec<i64> {
    let mut ids = Vec::new();
    for evidence_ref in evidence_refs {
        if evidence_ref.kind == "claim_record" {
            if let Ok(id) = evidence_ref.id.parse::<i64>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

fn semantic_grouping_rule_from_reason(reason: &str) -> Option<String> {
    if reason.contains(&format!("/{SEMANTIC_EXACT_GROUP_RULE}")) {
        Some(SEMANTIC_EXACT_GROUP_RULE.to_string())
    } else if reason.contains(&format!("/{SEMANTIC_TOKEN_GROUP_RULE}")) {
        Some(SEMANTIC_TOKEN_GROUP_RULE.to_string())
    } else if reason.contains(&format!("/{SEMANTIC_LATENT_REVIEW_GROUP_RULE}")) {
        Some(SEMANTIC_LATENT_REVIEW_GROUP_RULE.to_string())
    } else if reason.contains(&format!("/{SEMANTIC_NEGATION_CONFLICT_GROUP_RULE}")) {
        Some(SEMANTIC_NEGATION_CONFLICT_GROUP_RULE.to_string())
    } else {
        None
    }
}

fn review_digest_item(item: &ProposalReviewItem) -> ReviewDigestItem {
    let target_lifecycle_state =
        item.proposal.target_lifecycle_state.map(|state| state.as_str().to_string());
    let semantic_review = item.semantic_review.clone();
    let semantic_review_card = semantic_review.as_ref().map(|review| review.review_card.clone());
    let digest_card = review_digest_item_card(item, semantic_review.as_ref());
    let title = match target_lifecycle_state.as_deref() {
        Some("semantic_fact") => format!("Review L4 semantic fact proposal #{}", item.proposal.id),
        Some(target) => format!("Review {} proposal #{}", target, item.proposal.id),
        None => format!("Review learning proposal #{}", item.proposal.id),
    };
    let body = if let Some(semantic_review) = &item.semantic_review {
        if semantic_review.target_lifecycle_state == "none" {
            format!(
                "Review-only semantic candidate from {} support claims. Resolve before any L4 proposal.",
                semantic_review.support_count
            )
        } else {
            format!(
                "Candidate L4 semantic_fact from {} support claims. Review evidence before projection.",
                semantic_review.support_count
            )
        }
    } else {
        compact_for_markdown(&item.proposal.reason, 180)
    };
    ReviewDigestItem {
        proposal_id: item.proposal.id,
        action: item.proposal.action.as_str().to_string(),
        target_lifecycle_state,
        readiness: item.readiness.clone(),
        digest_card,
        decision_packet: item.decision_packet.clone(),
        title,
        body,
        review_reason: item.review_reason.clone(),
        recommended_next_action: item.recommended_next_action.clone(),
        interruption_hint: item.interruption_hint.clone(),
        semantic_review,
        semantic_review_card,
        mcp_tools: item.mcp_tools.clone(),
        action_options: item.action_options.clone(),
    }
}

fn review_digest_item_card(
    item: &ProposalReviewItem,
    semantic_review: Option<&SemanticPromotionReview>,
) -> ReviewDigestItemCard {
    let target = item
        .proposal
        .target_lifecycle_state
        .map(|state| state.as_str().to_string())
        .unwrap_or_else(|| "review_only".to_string());
    if let Some(review) = semantic_review {
        let review_only = review.target_lifecycle_state == "none";
        let status = if review_only {
            "review_only_until_verified"
        } else if review.readiness_score.blocks_l4_auto_apply {
            "manual_l4_review_required"
        } else {
            "ready_for_manual_l4_review"
        };
        let projection_path = if review_only {
            "verified_l3_claims -> manual_review -> l4_review_only -> ContextEnvelope"
        } else {
            "verified_l3_claims -> manual_review_apply -> semantic_fact -> ContextEnvelope.stable_facts"
        };
        return ReviewDigestItemCard {
            source: "soma_review_digest.item_card.v1".to_string(),
            lane: "l4_semantic_fact_candidates".to_string(),
            target,
            status: status.to_string(),
            blocks_l4_promotion: review_only || review.readiness_score.blocks_l4_auto_apply,
            projection_path: projection_path.to_string(),
            evidence_rule: "requires durable L3 support claims plus explicit user/tool/test/local/correction review evidence; cloud output cannot verify itself".to_string(),
            accepted_verifier_types: review_verifier_type_options(),
            forbidden_evidence_sources: review_forbidden_evidence_sources(),
            trust_boundary: "review_digest_item_card_is_read_only: summarizes one review item for client UI only; records no verification, applies no proposal, writes no semantic_fact, and promotes no cloud draft".to_string(),
        };
    }
    ReviewDigestItemCard {
        source: "soma_review_digest.item_card.v1".to_string(),
        lane: "proposal_review".to_string(),
        target,
        status: item.readiness.clone(),
        blocks_l4_promotion: true,
        projection_path:
            "proposal_queue -> review_action_or_batch_storage_gate -> verified_claim_or_proposal_state"
                .to_string(),
        evidence_rule: "requires explicit review action with independent user/tool/test/local/correction evidence when the action records trust".to_string(),
        accepted_verifier_types: review_verifier_type_options(),
        forbidden_evidence_sources: review_forbidden_evidence_sources(),
        trust_boundary: "review_digest_item_card_is_read_only: summarizes one review item for client UI only; records no verification, applies no proposal, writes no semantic_fact, and promotes no cloud draft".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewDigestBeliefTriage {
    triage_status: &'static str,
    noise_risk: &'static str,
    review_priority: u8,
    triage_reason: String,
}

fn review_digest_belief_review(
    storage: &Storage,
    input: &ReviewDigestInput,
    include_items: bool,
) -> Result<ReviewDigestBeliefReview, StorageError> {
    let candidates = scoped_review_digest_belief_candidates(storage, input)?;
    let candidate_count = candidates.len();
    let contradiction_count =
        candidates.iter().filter(|candidate| candidate.kind == BeliefKind::Contradicts).count();
    let corroboration_count =
        candidates.iter().filter(|candidate| candidate.kind == BeliefKind::Corroborates).count();
    let grouped_items = candidates
        .iter()
        .map(|candidate| review_digest_belief_item(storage, candidate))
        .collect::<Result<Vec<_>, StorageError>>()
        .map(review_digest_group_belief_items)?;
    let group_count = grouped_items.len();
    let hidden_duplicate_count = candidate_count.saturating_sub(group_count);
    let workload_summary = review_digest_belief_workload_summary(&grouped_items);
    let hidden_count = if include_items { 0 } else { candidate_count };
    let items = if include_items { grouped_items } else { Vec::new() };
    let status = if contradiction_count > 0 {
        "needs_resolution"
    } else if candidate_count > 0 {
        "review_only_signal"
    } else {
        "clear"
    };
    let next_action = if contradiction_count > 0 {
        "Resolve substantive contradictions first; belief candidates remain L2/review-only until user/tool/local correction or explicit review evidence resolves them."
    } else if candidate_count > 0 {
        "Inspect corroborations as support signals; they cannot become L4 facts or policy without independent evidence."
    } else {
        "No unresolved belief candidates are visible for this scope."
    };

    Ok(ReviewDigestBeliefReview {
        source: "soma_review_digest.belief_review.v1".to_string(),
        status: status.to_string(),
        candidate_count,
        contradiction_count,
        corroboration_count,
        group_count,
        visible_count: items.len(),
        hidden_count,
        hidden_duplicate_count,
        workload_summary,
        next_action: next_action.to_string(),
        promotion_rule:
            "belief candidates are L2/review-only signals; L4 fact/policy/belief projection requires user/tool/local correction or explicit review evidence"
                .to_string(),
        trust_boundary:
            "belief_review_digest_is_read_only: projects unresolved belief candidates for review only; records no correction, creates no verification event, writes no semantic_fact, and promotes no cloud draft"
                .to_string(),
        items,
    })
}

fn scoped_review_digest_belief_candidates(
    storage: &Storage,
    input: &ReviewDigestInput,
) -> Result<Vec<BeliefCandidate>, StorageError> {
    let row_limit = input.limit.max(1);
    let read_limit = row_limit.saturating_mul(4).max(row_limit);
    let mut rows = Vec::new();
    rows.extend(storage.recent_beliefs_of_kind(BeliefKind::Contradicts, read_limit)?);
    rows.extend(storage.recent_beliefs_of_kind(BeliefKind::Corroborates, read_limit)?);
    let mut scoped = Vec::new();
    for row in rows {
        if review_digest_belief_matches_scope(
            storage,
            &row,
            input.project.as_deref(),
            input.session_id.as_deref(),
        )? {
            let triage = review_digest_belief_triage(storage, &row)?;
            scoped.push((triage.review_priority, row.created_at_ns, row));
        }
    }
    scoped.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));
    scoped.truncate(row_limit);
    Ok(scoped.into_iter().map(|(_, _, row)| row).collect())
}

fn review_digest_belief_matches_scope(
    storage: &Storage,
    candidate: &BeliefCandidate,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<bool, StorageError> {
    for episode_id in [candidate.episode_a_id, candidate.episode_b_id] {
        let Some(episode) = storage.get_live_episode(episode_id)? else {
            return Ok(false);
        };
        if let Some(project) = project {
            if episode.project.as_deref() != Some(project) {
                return Ok(false);
            }
        }
        if let Some(session_id) = session_id {
            if episode.session_id.as_deref() != Some(session_id) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn review_digest_belief_item(
    storage: &Storage,
    candidate: &BeliefCandidate,
) -> Result<ReviewDigestBeliefItem, StorageError> {
    let episode_a = storage.get_live_episode(candidate.episode_a_id)?;
    let episode_b = storage.get_live_episode(candidate.episode_b_id)?;
    let triage = review_digest_belief_triage_from_episodes(
        candidate,
        episode_a.as_ref(),
        episode_b.as_ref(),
    );
    let recommended_next_action =
        review_digest_belief_guidance(candidate.kind, triage.triage_status);
    let (project, session_id) = review_digest_shared_scope(episode_a.as_ref(), episode_b.as_ref());
    let claim_preview = review_digest_belief_claim_hint(episode_a.as_ref(), episode_b.as_ref());
    let resolution_action = review_digest_belief_resolution_action(
        candidate,
        &triage,
        episode_a.as_ref(),
        episode_b.as_ref(),
    );
    Ok(ReviewDigestBeliefItem {
        candidate_id: candidate.id,
        kind: candidate.kind.to_string(),
        score: candidate.score,
        evidence: candidate.evidence.clone(),
        claim_preview,
        project,
        session_id,
        triage_status: triage.triage_status.to_string(),
        noise_risk: triage.noise_risk.to_string(),
        review_priority: triage.review_priority,
        triage_reason: triage.triage_reason,
        episode_a_id: candidate.episode_a_id,
        episode_b_id: candidate.episode_b_id,
        episode_a_preview: review_digest_episode_preview(episode_a.as_ref()),
        episode_b_preview: review_digest_episode_preview(episode_b.as_ref()),
        episode_a_outcome: review_digest_episode_outcome(episode_a.as_ref()),
        episode_b_outcome: review_digest_episode_outcome(episode_b.as_ref()),
        group_candidate_count: 1,
        grouped_candidate_ids: vec![candidate.id],
        hidden_duplicate_count: 0,
        recommended_next_action: recommended_next_action.to_string(),
        resolution_action,
        trust_boundary:
            "belief_digest_item_is_review_only: this row records no correction, verification, proposal apply, semantic_fact write, or cloud-draft promotion"
                .to_string(),
    })
}

fn review_digest_group_belief_items(
    items: Vec<ReviewDigestBeliefItem>,
) -> Vec<ReviewDigestBeliefItem> {
    let mut groups: Vec<ReviewDigestBeliefItem> = Vec::new();
    let mut group_index: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        let key = review_digest_belief_group_key(&item);
        if let Some(index) = group_index.get(&key).copied() {
            let group = &mut groups[index];
            group.group_candidate_count += 1;
            group.hidden_duplicate_count += 1;
            group.grouped_candidate_ids.push(item.candidate_id);
            group.score = group.score.max(item.score);
            continue;
        }
        group_index.insert(key, groups.len());
        groups.push(item);
    }
    groups
}

fn empty_review_digest_belief_workload_summary() -> ReviewDigestBeliefWorkloadSummary {
    ReviewDigestBeliefWorkloadSummary {
        source: "soma_review_digest.belief_workload_summary.v1".to_string(),
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
        support_signal_group_count: 0,
        support_signal_candidate_count: 0,
        context_signal_group_count: 0,
        context_signal_candidate_count: 0,
        review_only_signal_group_count: 0,
        review_only_signal_candidate_count: 0,
        noise_group_count: 0,
        noise_candidate_count: 0,
        primary_group_id: None,
        next_action: "No unresolved belief candidates are visible for this scope.".to_string(),
        trust_boundary:
            "belief_workload_summary_is_read_only: derived from unresolved belief candidates only; records no correction, creates no verification event, writes no semantic_fact, and promotes no cloud draft"
                .to_string(),
    }
}

fn review_digest_belief_workload_summary(
    items: &[ReviewDigestBeliefItem],
) -> ReviewDigestBeliefWorkloadSummary {
    let mut summary = empty_review_digest_belief_workload_summary();
    summary.review_group_count = items.len();
    for item in items {
        let candidate_count = item.group_candidate_count.max(1);
        summary.raw_candidate_count += candidate_count;
        summary.hidden_duplicate_count += item.hidden_duplicate_count;
        match item.triage_status.as_str() {
            "needs_resolution" => {
                summary.substantive_contradiction_group_count += 1;
                summary.substantive_contradiction_candidate_count += candidate_count;
            }
            "low_value_conflict" => {
                summary.low_value_conflict_group_count += 1;
                summary.low_value_conflict_candidate_count += candidate_count;
            }
            "low_value_noise" => {
                summary.low_value_noise_group_count += 1;
                summary.low_value_noise_candidate_count += candidate_count;
            }
            "support_signal" => {
                summary.support_signal_group_count += 1;
                summary.support_signal_candidate_count += candidate_count;
            }
            "context_signal" => {
                summary.context_signal_group_count += 1;
                summary.context_signal_candidate_count += candidate_count;
            }
            "review_only_signal" => {
                summary.review_only_signal_group_count += 1;
                summary.review_only_signal_candidate_count += candidate_count;
            }
            _ => {}
        }
    }
    summary.noise_group_count =
        summary.low_value_conflict_group_count + summary.low_value_noise_group_count;
    summary.noise_candidate_count =
        summary.low_value_conflict_candidate_count + summary.low_value_noise_candidate_count;
    summary.primary_group_id = items
        .iter()
        .find(|item| item.triage_status == "needs_resolution")
        .or_else(|| items.iter().find(|item| item.triage_status == "low_value_conflict"))
        .or_else(|| items.first())
        .map(|item| item.candidate_id);

    if summary.substantive_contradiction_group_count > 0 {
        summary.status = "substantive_resolution_required".to_string();
        summary.next_action = format!(
            "Resolve {} substantive contradiction group(s) before L4 belief/policy extraction; {} low-value command-noise candidate(s) stay visible as de-prioritized L2 audit evidence.",
            summary.substantive_contradiction_group_count, summary.noise_candidate_count
        );
    } else if summary.noise_candidate_count > 0 {
        summary.status = "noise_triage_only".to_string();
        summary.next_action = format!(
            "Inspect {} low-value command-noise candidate(s) only if the command outcome matters; otherwise keep them as L2 audit evidence and avoid L4 promotion.",
            summary.noise_candidate_count
        );
    } else if summary.raw_candidate_count > 0 {
        summary.status = "support_signal_review".to_string();
        summary.next_action =
            "Inspect support/context signals as evidence hints; they cannot become L4 facts or policy without independent verification."
                .to_string();
    }

    summary
}

fn review_digest_belief_group_key(item: &ReviewDigestBeliefItem) -> String {
    let claim = item.claim_preview.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}|{}|{}|{}|{}",
        item.kind,
        item.triage_status,
        item.project.as_deref().unwrap_or("*"),
        item.session_id.as_deref().unwrap_or("*"),
        claim.to_lowercase()
    )
}

fn review_digest_belief_triage(
    storage: &Storage,
    candidate: &BeliefCandidate,
) -> Result<ReviewDigestBeliefTriage, StorageError> {
    let episode_a = storage.get_live_episode(candidate.episode_a_id)?;
    let episode_b = storage.get_live_episode(candidate.episode_b_id)?;
    Ok(review_digest_belief_triage_from_episodes(candidate, episode_a.as_ref(), episode_b.as_ref()))
}

fn review_digest_belief_resolution_action(
    candidate: &BeliefCandidate,
    triage: &ReviewDigestBeliefTriage,
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> ReviewDigestBeliefResolutionAction {
    let (project, session_id) = review_digest_shared_scope(episode_a, episode_b);
    match candidate.kind {
        BeliefKind::Contradicts => {
            let claim_hint = review_digest_belief_claim_hint(episode_a, episode_b);
            if triage.triage_status == "low_value_conflict" {
                let mut cli_command = vec![
                    "soma".to_string(),
                    "context".to_string(),
                    "review-digest".to_string(),
                    "--include-queue-only".to_string(),
                    "--format".to_string(),
                    "json".to_string(),
                ];
                let mut mcp_arguments = json!({
                    "include_queue_only": true,
                    "format": "json"
                });
                if let Some(project) = project {
                    cli_command.push("--project".to_string());
                    cli_command.push(project.clone());
                    mcp_arguments["project"] = json!(project);
                }
                if let Some(session_id) = session_id {
                    cli_command.push("--session-id".to_string());
                    cli_command.push(session_id.clone());
                    mcp_arguments["session_id"] = json!(session_id);
                }
                return ReviewDigestBeliefResolutionAction {
                    source: "soma_review_digest.belief_resolution_action.v1".to_string(),
                    action: "inspect_low_value_conflict".to_string(),
                    label: "Inspect low-value conflict".to_string(),
                    cli_command,
                    mcp_tool: "soma_review_digest".to_string(),
                    mcp_arguments_template: mcp_arguments,
                    evidence_rule:
                        "low-information terminal conflicts are run-specific L2 audit evidence by default; record a correction only after user/tool/local evidence proves the command outcome should become durable memory"
                            .to_string(),
                    trust_effect:
                        "no mutation; keeps command flapping out of L3/L4 unless a separate verified correction path uses it"
                            .to_string(),
                    trust_boundary:
                        "resolution_action_is_inspection_only: no correction, verification event, semantic_fact write, or cloud-draft promotion"
                            .to_string(),
                };
            }
            let mut cli_command = vec![
                "soma".to_string(),
                "context".to_string(),
                "correct".to_string(),
                "--claim".to_string(),
                claim_hint.clone(),
                "--correction".to_string(),
                "<current truth>".to_string(),
            ];
            if let Some(project) = &project {
                cli_command.push("--project".to_string());
                cli_command.push(project.clone());
            }
            if let Some(session_id) = &session_id {
                cli_command.push("--session-id".to_string());
                cli_command.push(session_id.clone());
            }
            let mut mcp_arguments = json!({
                "claim": claim_hint,
                "correction": "<current truth>"
            });
            if let Some(project) = project {
                mcp_arguments["project"] = json!(project);
            }
            if let Some(session_id) = session_id {
                mcp_arguments["session_id"] = json!(session_id);
            }
            ReviewDigestBeliefResolutionAction {
                source: "soma_review_digest.belief_resolution_action.v1".to_string(),
                action: "record_correction".to_string(),
                label: "Record correction".to_string(),
                cli_command,
                mcp_tool: "soma_record_correction".to_string(),
                mcp_arguments_template: mcp_arguments,
                evidence_rule:
                    "use user-provided current truth or independently inspected local/tool evidence; do not use cloud output as evidence for itself"
                        .to_string(),
                trust_effect:
                    "records a correction episode, may supersede matching claim_records, and resolves matching belief_candidates through resolved_by_correction_episode_id"
                        .to_string(),
                trust_boundary:
                    "resolution_action_is_operator_guidance_only: digest rendering does not record the correction, create verification trust, write semantic_fact, or promote a cloud draft"
                        .to_string(),
            }
        }
        BeliefKind::Corroborates => ReviewDigestBeliefResolutionAction {
            source: "soma_review_digest.belief_resolution_action.v1".to_string(),
            action: "inspect_only".to_string(),
            label: "Inspect support signal".to_string(),
            cli_command: vec![
                "soma".to_string(),
                "context".to_string(),
                "review-digest".to_string(),
                "--include-queue-only".to_string(),
            ],
            mcp_tool: "soma_review_digest".to_string(),
            mcp_arguments_template: json!({
                "include_queue_only": true,
                "format": "json"
            }),
            evidence_rule:
                "corroboration is support for review only; L4 promotion still needs repeated verified L3 evidence or explicit correction/policy evidence"
                    .to_string(),
            trust_effect:
                "no mutation; keep as an L2 support signal until a separate verified proposal or correction path uses it"
                    .to_string(),
            trust_boundary:
                "resolution_action_is_inspection_only: no correction, verification event, semantic_fact write, or cloud-draft promotion"
                    .to_string(),
        },
    }
}

fn review_digest_shared_scope(
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> (Option<String>, Option<String>) {
    let project = match (
        episode_a.and_then(|episode| episode.project.as_deref()),
        episode_b.and_then(|episode| episode.project.as_deref()),
    ) {
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), None) => Some(left.to_string()),
        (None, Some(right)) => Some(right.to_string()),
        _ => None,
    };
    let session_id = match (
        episode_a.and_then(|episode| episode.session_id.as_deref()),
        episode_b.and_then(|episode| episode.session_id.as_deref()),
    ) {
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), None) => Some(left.to_string()),
        (None, Some(right)) => Some(right.to_string()),
        _ => None,
    };
    (project, session_id)
}

fn review_digest_belief_claim_hint(
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> String {
    if let (Some(left), Some(right)) = (
        episode_a.and_then(|episode| episode.command.as_deref()),
        episode_b.and_then(|episode| episode.command.as_deref()),
    ) {
        let left = left.trim();
        let right = right.trim();
        if !left.is_empty() && left == right {
            return left.to_string();
        }
    }
    review_digest_episode_text(episode_b)
        .or_else(|| review_digest_episode_text(episode_a))
        .unwrap_or_else(|| "<stale claim or command>".to_string())
}

fn review_digest_belief_guidance(kind: BeliefKind, triage_status: &str) -> &'static str {
    if triage_status == "low_value_conflict" {
        return "inspect_low_value_conflict";
    }
    match kind {
        BeliefKind::Contradicts => "resolve_or_record_correction",
        BeliefKind::Corroborates => "inspect_before_semantic_promotion",
    }
}

fn review_digest_episode_text(episode: Option<&StoredEpisode>) -> Option<String> {
    let episode = episode?;
    let text = episode
        .digest
        .as_deref()
        .or(episode.command.as_deref())
        .or(episode.prompt_text.as_deref())
        .or(episode.response_text.as_deref())?;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!compact.is_empty()).then(|| compact_for_markdown(&compact, 160))
}

fn review_digest_belief_triage_from_episodes(
    candidate: &BeliefCandidate,
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> ReviewDigestBeliefTriage {
    match candidate.kind {
        BeliefKind::Contradicts if review_digest_low_information_terminal_pair(episode_a, episode_b) => {
            ReviewDigestBeliefTriage {
                triage_status: "low_value_conflict",
                noise_risk: "high",
                review_priority: 20,
                triage_reason:
                    "low-information terminal command flapped between outcomes; kept as unresolved L2 evidence but ranked after substantive contradictions"
                        .to_string(),
            }
        }
        BeliefKind::Contradicts => ReviewDigestBeliefTriage {
            triage_status: "needs_resolution",
            noise_risk: "low",
            review_priority: 1,
            triage_reason:
                "contradiction candidates are unresolved L2 signals and must be resolved before L4 belief/policy extraction"
                    .to_string(),
        },
        BeliefKind::Corroborates
            if candidate.evidence.as_deref() == Some("command-and-outcome match") =>
        {
            ReviewDigestBeliefTriage {
                triage_status: "support_signal",
                noise_risk: "medium",
                review_priority: 30,
                triage_reason:
                    "same command and outcome can support later review, but repeated execution is not an L4 fact by itself"
                        .to_string(),
            }
        }
        BeliefKind::Corroborates
            if candidate.evidence.as_deref() == Some("high-cosine-pair")
                && review_digest_low_information_terminal_pair(episode_a, episode_b) =>
        {
            ReviewDigestBeliefTriage {
                triage_status: "low_value_noise",
                noise_risk: "high",
                review_priority: 90,
                triage_reason:
                    "terminal command-only high-cosine pair with low information value; kept for audit but de-prioritized in review"
                        .to_string(),
            }
        }
        BeliefKind::Corroborates
            if candidate.evidence.as_deref() == Some("high-cosine-pair") =>
        {
            ReviewDigestBeliefTriage {
                triage_status: "context_signal",
                noise_risk: "medium",
                review_priority: 50,
                triage_reason:
                    "semantic similarity can suggest related evidence, but it needs explicit review before L4 promotion"
                        .to_string(),
            }
        }
        BeliefKind::Corroborates => ReviewDigestBeliefTriage {
            triage_status: "review_only_signal",
            noise_risk: "medium",
            review_priority: 60,
            triage_reason:
                "corroboration is visible for operator review and cannot promote without user/tool/local evidence"
                    .to_string(),
        },
    }
}

fn review_digest_low_information_terminal_pair(
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> bool {
    let Some(episode_a) = episode_a else { return false };
    let Some(episode_b) = episode_b else { return false };
    matches!(&episode_a.source, EpisodeSource::Terminal)
        && matches!(&episode_b.source, EpisodeSource::Terminal)
        && review_digest_low_information_command(episode_a.command.as_deref())
        && review_digest_low_information_command(episode_b.command.as_deref())
}

fn review_digest_low_information_command(command: Option<&str>) -> bool {
    let Some(command) = command.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if command.starts_with("eval ") && command.contains("aenv ") {
        return true;
    }
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let Some(first) = tokens.first().copied() else {
        return false;
    };
    match first {
        "cd" | "pwd" | "clear" | "exit" | "history" | "jobs" | "fg" | "bg" | "l" | "ls" | "ll"
        | "la" => true,
        "git" => tokens.get(1).is_some_and(|sub| matches!(*sub, "add" | "init" | "status")),
        "aenv" => true,
        "soma" => {
            tokens.len() <= 2
                && tokens.get(1).is_none_or(|sub| matches!(*sub, "help" | "-h" | "--help"))
        }
        "codex" | "claude" | "claude-code" => {
            tokens.len() <= 2 && tokens.get(1).is_none_or(|sub| sub.starts_with('-'))
        }
        _ => false,
    }
}

fn review_digest_episode_preview(episode: Option<&StoredEpisode>) -> String {
    let Some(episode) = episode else {
        return "episode unavailable".to_string();
    };
    let body = episode
        .digest
        .as_deref()
        .or(episode.command.as_deref())
        .or(episode.prompt_text.as_deref())
        .or(episode.response_text.as_deref())
        .map(str::to_string)
        .or_else(|| {
            episode.stdout.as_ref().map(|stdout| String::from_utf8_lossy(stdout).to_string())
        })
        .unwrap_or_else(|| "no text payload".to_string());
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = compact_for_markdown(&compact, 160);
    let source = episode.source.to_string();
    let project = episode.project.as_deref().unwrap_or("unknown-project");
    let session = episode.session_id.as_deref().unwrap_or("unknown-session");
    format!("{preview} [source={source} project={project} session={session}]")
}

fn review_digest_episode_outcome(episode: Option<&StoredEpisode>) -> String {
    let Some(episode) = episode else {
        return "outcome unavailable".to_string();
    };
    let mut parts = Vec::new();
    if let Some(exit_code) = episode.exit_code {
        let status = if exit_code == 0 { "success" } else { "failure" };
        parts.push(format!("exit_code={exit_code}"));
        parts.push(format!("status={status}"));
    } else if matches!(&episode.source, EpisodeSource::Terminal) {
        parts.push("exit_code=unknown".to_string());
        parts.push("status=unknown".to_string());
    } else {
        parts.push("status=no_process_outcome".to_string());
    }
    if let Some(stdout) = episode.stdout.as_ref() {
        let stdout = String::from_utf8_lossy(stdout);
        let compact = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            parts.push(format!("stdout_preview={}", compact_for_markdown(&compact, 120)));
        }
    }
    parts.join(" ")
}

fn review_digest_signature(items: &[&ProposalReviewItem]) -> String {
    if items.is_empty() {
        return "empty".to_string();
    }
    let mut parts = items
        .iter()
        .map(|item| {
            format!("{}:{}:{}", item.proposal.id, item.proposal.updated_at_ns, item.readiness)
        })
        .collect::<Vec<_>>();
    parts.sort();
    parts.join("|")
}

fn review_digest_batch_key(pending_notification_count: usize) -> String {
    if pending_notification_count > 0 {
        "l4_semantic_promotion".to_string()
    } else {
        "none".to_string()
    }
}

fn normalize_review_digest_client(input: Option<&str>) -> String {
    match input.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => match value.to_ascii_lowercase().replace('_', "-").as_str() {
            "codex-app" | "codexapp" | "codex-desktop" | "codex-desktop-app" => {
                "codex-app".to_string()
            }
            "codex" | "codex-cli" => "codex-cli".to_string(),
            "cursor" => "cursor".to_string(),
            "continue" => "continue".to_string(),
            "claude" | "claude-code" => "claude-code".to_string(),
            "generic" => "generic".to_string(),
            _ => "generic".to_string(),
        },
        None => "generic".to_string(),
    }
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn render_semantic_review_markdown(
    out: &mut String,
    semantic_review: Option<&SemanticPromotionReview>,
) {
    let Some(semantic_review) = semantic_review else {
        return;
    };
    let grouping_rule = semantic_review.grouping_rule.as_deref().unwrap_or("unknown_grouping_rule");
    let representative = join_i64s(&semantic_review.representative_claim_ids);
    let support = join_i64s(&semantic_review.support_claim_ids);
    out.push_str(&format!(
        "  semantic_review: target={} rule={} grouping={} representative_claims={} support_claims={} support_count={}\n",
        semantic_review.target_lifecycle_state,
        semantic_review.rule,
        grouping_rule,
        representative,
        support,
        semantic_review.support_count
    ));
    if let Some(diversity) = &semantic_review.support_diversity {
        out.push_str(&format!(
            "  semantic_support_diversity: task_frames={} projects={} support_projects=[{}] source_types={} verifier_types={} evidence_sources={} single_project_only={} single_evidence_source_only={} risk={}\n",
            diversity.distinct_task_frame_count,
            diversity.distinct_project_count,
            diversity.support_projects.join(","),
            diversity.distinct_source_type_count,
            diversity.distinct_verifier_type_count,
            diversity.distinct_evidence_source_count,
            diversity.single_project_only,
            diversity.single_evidence_source_only,
            diversity.bias_risk
        ));
    }
    out.push_str(&format!(
        "  semantic_readiness_score: score={}/{} verdict={} meaning={}\n",
        semantic_review.readiness_score.score,
        semantic_review.readiness_score.max_score,
        semantic_review.readiness_score.verdict,
        semantic_review.readiness_score.meaning
    ));
    out.push_str(&format!(
        "  semantic_required_verification: {}\n",
        semantic_review.required_verification
    ));
    render_semantic_review_card_markdown(out, &semantic_review.review_card);
    if !semantic_review.review_rubric.is_empty() {
        let checks = semantic_review
            .review_rubric
            .iter()
            .map(|item| item.check_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("  semantic_review_rubric: checks={checks}\n"));
    }
    out.push_str(&format!("  semantic_prompt: {}\n", semantic_review.review_prompt));
}

fn render_semantic_review_card_markdown(out: &mut String, card: &SemanticReviewCard) {
    out.push_str(&format!(
        "  semantic_review_card: state={} title=\"{}\"\n",
        card.review_state,
        compact_for_markdown(&card.title, 120)
    ));
    out.push_str(&format!(
        "  semantic_support_summary: {}\n",
        compact_for_markdown(&card.support_summary, 220)
    ));
    out.push_str(&format!(
        "  semantic_authorization: operator_required={} agent_self_authorization_forbidden={} authority={}\n",
        card.operator_authorization_required,
        card.agent_self_authorization_forbidden,
        card.review_decision_authority
    ));
    if !card.required_resolution_evidence.is_empty() {
        out.push_str(&format!(
            "  semantic_required_resolution_evidence: {}\n",
            card.required_resolution_evidence.join(",")
        ));
    }
    out.push_str(&format!(
        "  semantic_allowed_actions: {}\n",
        if card.allowed_actions.is_empty() {
            "none".to_string()
        } else {
            card.allowed_actions.join(",")
        }
    ));
    out.push_str(&format!(
        "  semantic_blocked_actions: {}\n",
        if card.blocked_actions.is_empty() {
            "none".to_string()
        } else {
            card.blocked_actions.join(",")
        }
    ));
    if !card.checklist.is_empty() {
        let checklist = card
            .checklist
            .iter()
            .map(|item| format!("{}={}", item.check_id, item.status))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("  semantic_checklist: {checklist}\n"));
    }
}

fn join_i64s(values: &[i64]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
    }
}

fn compact_for_markdown(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut out = compact.chars().take(limit.saturating_sub(3)).collect::<String>();
    out.push_str("...");
    out
}

fn render_report_action_summary(out: &mut String, actions: &[ReviewActionOption]) {
    let enabled: Vec<&ReviewActionOption> =
        actions.iter().filter(|action| action.enabled).collect();
    if enabled.is_empty() {
        out.push_str("  enabled_actions: none\n");
        return;
    }
    out.push_str("  enabled_actions:\n");
    for action in enabled {
        let mut requirements = Vec::new();
        if action.requires_evidence {
            requirements.push("evidence");
        }
        if action.requires_destructive_confirmation {
            requirements.push("destructive_confirmation");
        }
        let requirements =
            if requirements.is_empty() { "none".to_string() } else { requirements.join(",") };
        out.push_str(&format!(
            "  - {} group={} style={} requirements={} cli=`{}` mcp_tool=`{}`\n",
            action.action,
            action.ui_hint.group,
            action.ui_hint.button_style,
            requirements,
            action.cli_hint,
            action.mcp_tool
        ));
    }
}

fn render_action_option_markdown(out: &mut String, action: &ReviewActionOption) {
    out.push_str(&format!(
        "- {} `{}` #{}: {}\n",
        action.target_type, action.action, action.target_id, action.label
    ));
    out.push_str(&format!("  intent: {}\n", action.intent));
    out.push_str(&format!("  trust: {}\n", action.trust_effect));
    out.push_str(&format!(
        "  ui: group={} order={} style={} icon={}\n",
        action.ui_hint.group,
        action.ui_hint.order,
        action.ui_hint.button_style,
        action.ui_hint.icon
    ));
    out.push_str(&format!("  evidence_required: {}\n", action.requires_evidence));
    if let Some(form) = &action.ui_hint.evidence_form {
        out.push_str(&format!(
            "  evidence_form: result={} verifier_options={}\n",
            form.result,
            form.verifier_type_options.join(",")
        ));
    }
    if let Some(template) = &action.verification_template {
        out.push_str(&format!(
            "  verification_template: result={} accepted_verifiers={} durable_verifiers={} trust_boundary={}\n",
            template.result,
            template.accepted_verifier_types.join(","),
            template.durable_promotion_verifier_types.join(","),
            template.trust_boundary
        ));
        out.push_str(&format!(
            "  verification_checklist: {}\n",
            template.operator_checklist.join(",")
        ));
    }
    out.push_str(&format!(
        "  destructive_confirmation_required: {}\n",
        action.requires_destructive_confirmation
    ));
    if let Some(confirmation) = &action.ui_hint.confirmation {
        out.push_str(&format!("  confirmation: {}\n", confirmation.title));
    }
    if let Some(reason) = &action.disabled_reason {
        out.push_str(&format!("  disabled_reason: {}\n", reason));
    }
    out.push_str(&format!("  cli: `{}`\n", action.cli_hint));
    out.push_str(&format!("  mcp_tool: `{}`\n", action.mcp_tool));
    out.push_str(&format!("  mcp_arguments_template: `{}`\n", action.mcp_arguments_template));
}
