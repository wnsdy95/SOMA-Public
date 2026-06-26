//! `soma learning` - one-screen semantic learning review summary.
//!
//! This is a read-only UX layer over the existing learning/review gates. It
//! previews L4 semantic candidates, shows pending review work, and keeps cloud
//! drafts blocked unless independent verification exists.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};

use crate::capture::ai_cli::{resolve_db_path, IngestError};
use crate::cli::LearningStatusArgs;
use crate::context::envelope::{ContextSection, EvidenceRef};
use crate::context::quality::{
    user_policy_from_storage, user_policy_from_storage_with_corrections_session_set,
};
use crate::context::review::{
    build_review_digest, build_review_queue, ReviewActionOption, ReviewDecisionPacket,
    ReviewDigestInput,
};
use crate::context::semantic_learning::{
    propose_semantic_consolidations, SemanticLearningInput, SemanticLearningItem,
};
use crate::memory::beliefs::{BeliefCandidate, BeliefKind};
use crate::storage::{
    ClaimSourceType, EpisodeSource, Storage, StorageError, StoredEpisode, StoredEvidenceRef,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningStatusOutcome {
    pub schema: &'static str,
    pub source: &'static str,
    pub db_path: String,
    pub storage_status: &'static str,
    pub storage_error: Option<String>,
    pub status: String,
    pub operator_next_action_id: &'static str,
    pub operator_next_action_label: &'static str,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub client: String,
    pub limit: usize,
    pub min_support: usize,
    pub candidate_limit: usize,
    pub review_limit: usize,
    pub summary: LearningStatusSummary,
    pub counts: LearningOperatorReviewCounts,
    pub belief_review_summary: LearningBeliefReviewSummary,
    pub target_coverage: Vec<LearningTargetCoverage>,
    pub promotion_matrix: Vec<LearningPromotionMatrixRow>,
    pub review_lanes: Vec<LearningReviewLane>,
    pub review_surface: LearningReviewSurface,
    pub operator_card: LearningOperatorCard,
    pub review_cards: Vec<LearningReviewCard>,
    pub candidates: Vec<LearningCandidateRow>,
    pub cloud_draft_blockers: Vec<LearningCloudDraftBlockerRow>,
    pub policy_items: Vec<LearningPolicyRow>,
    pub belief_items: Vec<LearningBeliefRow>,
    pub review_items: Vec<LearningReviewItemRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dogfood_evidence: Option<LearningDogfoodEvidence>,
    pub next_commands: Vec<Vec<String>>,
    pub recovery_commands: Vec<Vec<String>>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningStatusSummary {
    pub inspected_l3_claim_count: usize,
    pub repeated_group_count: usize,
    pub l4_candidate_count: usize,
    pub review_only_candidate_count: usize,
    pub skipped_untrusted_count: usize,
    pub pending_review_item_count: usize,
    pub ready_proposal_count: usize,
    pub manual_l4_review_count: usize,
    pub cloud_draft_blocked_count: usize,
    pub policy_projection_count: usize,
    pub belief_candidate_count: usize,
    pub belief_group_count: usize,
    pub belief_hidden_duplicate_count: usize,
    pub belief_contradiction_count: usize,
    pub belief_substantive_contradiction_count: usize,
    pub belief_low_value_conflict_count: usize,
    pub belief_low_value_noise_count: usize,
    pub belief_noise_candidate_count: usize,
    pub should_interrupt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningBeliefReviewSummary {
    pub source: &'static str,
    pub status: &'static str,
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
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningTargetCoverage {
    pub target: &'static str,
    pub status: &'static str,
    pub rule: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningPromotionMatrixRow {
    pub source: &'static str,
    pub target: &'static str,
    pub lane: &'static str,
    pub status: &'static str,
    pub candidate_count: usize,
    pub ready_for_manual_l4_review: bool,
    pub context_projection_ready: bool,
    pub blocks_l4_promotion: bool,
    pub projected_context_section: Option<&'static str>,
    pub required_evidence: &'static str,
    pub next_action: String,
    pub primary_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningReviewLane {
    pub lane: &'static str,
    pub priority: u8,
    pub status: &'static str,
    pub count: usize,
    pub next_action: String,
    pub trust_boundary: &'static str,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningReviewSurface {
    pub source: &'static str,
    pub client: String,
    pub primary_surface: &'static str,
    pub primary_reason: String,
    pub render_plan_command: Vec<String>,
    pub report_command: Vec<String>,
    pub queue_command: Vec<String>,
    pub action_plan_command: Vec<String>,
    pub digest_command: Vec<String>,
    pub proof_session_command: Vec<String>,
    pub mcp_tools: Vec<&'static str>,
    pub control_contract: &'static str,
    pub proof_path: &'static str,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningReviewCard {
    pub source: &'static str,
    pub card_id: String,
    pub lane: &'static str,
    pub priority: u8,
    pub target: &'static str,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub primary_action: String,
    pub primary_command: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub blocks_l4_promotion: bool,
    pub projection_path: String,
    pub evidence_rule: &'static str,
    pub accepted_verifier_types: Vec<&'static str>,
    pub forbidden_evidence_sources: Vec<&'static str>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningOperatorCard {
    pub source: &'static str,
    pub status: String,
    pub operator_next_action_id: &'static str,
    pub operator_next_action_label: &'static str,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub review_surface: &'static str,
    pub review_counts: LearningOperatorReviewCounts,
    pub lanes_requiring_review: Vec<String>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningOperatorReviewCounts {
    pub l4_semantic_fact_candidates: usize,
    pub review_only_semantic_candidates: usize,
    pub cloud_draft_blockers: usize,
    pub policy_projections: usize,
    pub belief_candidates: usize,
    pub belief_groups: usize,
    pub belief_hidden_duplicates: usize,
    pub belief_contradictions: usize,
    pub belief_substantive_contradictions: usize,
    pub belief_low_value_conflicts: usize,
    pub belief_low_value_noise: usize,
    pub belief_noise_candidates: usize,
    pub pending_review_items: usize,
    pub ready_to_apply_proposals: usize,
    pub manual_l4_review_items: usize,
    pub should_interrupt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningDogfoodEvidence {
    pub source: &'static str,
    pub path: String,
    pub status: &'static str,
    pub report_status: Option<String>,
    pub semantic_learning_review_status: Option<String>,
    pub summary_pass: Option<u64>,
    pub summary_warn: Option<u64>,
    pub summary_fail: Option<u64>,
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningCandidateRow {
    pub proposal_id: Option<i64>,
    pub target: &'static str,
    pub action: String,
    pub normalized_text: String,
    pub group_rule: String,
    pub support_count: usize,
    pub support_claim_ids: Vec<i64>,
    pub support_claims: Vec<LearningSupportClaimRow>,
    pub durable_promotion_trust: bool,
    pub trusted: bool,
    pub contains_cloud_draft_support: bool,
    pub cloud_draft_support_count: usize,
    pub verified_cloud_draft_support_count: usize,
    pub blocked_cloud_draft_support_count: usize,
    pub unverified_support_count: usize,
    pub support_trust_summary: String,
    pub readiness_verdict: String,
    pub bias_risk: String,
    pub skipped_reason: Option<String>,
    pub review_action: &'static str,
    pub review_rationale: String,
    pub resolution_status: String,
    pub resolution_next_step: String,
    pub review_queue_command: Vec<String>,
    pub verification_template_command: Vec<String>,
    pub resolution_actions: Vec<LearningSemanticResolutionAction>,
    pub resolution_trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearningSemanticResolutionAction {
    pub source: &'static str,
    pub action: String,
    pub control_id: String,
    pub label: String,
    pub requires_evidence: bool,
    pub cli_command: Vec<String>,
    pub mcp_tool: &'static str,
    pub evidence_rule: String,
    pub trust_effect: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningSupportClaimRow {
    pub claim_id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub lifecycle_state: String,
    pub confidence: f32,
    pub evidence_refs: Vec<String>,
    pub promotion_reason: Option<String>,
    pub durable_promotion_trust: bool,
    pub verification_event_count: usize,
    pub trust_status: String,
    pub promotion_trust_basis: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningCloudDraftBlockerRow {
    pub claim_id: i64,
    pub task_frame_id: Option<i64>,
    pub text_preview: String,
    pub lifecycle_state: String,
    pub confidence: f32,
    pub evidence_refs: Vec<String>,
    pub verification_event_count: usize,
    pub durable_promotion_trust: bool,
    pub review_reason: String,
    pub next_action: String,
    pub cli_hint: String,
    pub decision_packet: ReviewDecisionPacket,
    pub action_options: Vec<ReviewActionOption>,
    pub mcp_tools: Vec<String>,
    pub accepted_verifier_types: Vec<&'static str>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningPolicyRow {
    pub text: String,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub confidence: Option<f32>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningBeliefRow {
    pub id: i64,
    pub kind: String,
    pub score: f32,
    pub evidence: Option<String>,
    pub claim_preview: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub triage_status: &'static str,
    pub noise_risk: &'static str,
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
    pub status: &'static str,
    pub promotion_rule: &'static str,
    pub review_action: &'static str,
    pub review_rationale: &'static str,
    pub resolution_action: LearningBeliefResolutionAction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningBeliefResolutionAction {
    pub source: &'static str,
    pub action: String,
    pub label: String,
    pub cli_command: Vec<String>,
    pub mcp_tool: String,
    pub mcp_arguments_template: Value,
    pub evidence_rule: String,
    pub trust_effect: String,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningReviewItemRow {
    pub proposal_id: i64,
    pub target: String,
    pub action: String,
    pub readiness: String,
    pub review_reason: String,
    pub title: String,
    pub next_action: String,
}

#[derive(Debug)]
pub enum LearningStatusError {
    DbPath(IngestError),
    Storage(StorageError),
    Render(serde_json::Error),
}

impl LearningStatusError {
    pub fn exit_code(&self) -> i32 {
        match self {
            LearningStatusError::DbPath(err) => crate::capture::ai_cli::exit_code_for(err),
            LearningStatusError::Storage(_) | LearningStatusError::Render(_) => 2,
        }
    }
}

impl std::fmt::Display for LearningStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LearningStatusError::DbPath(err) => write!(f, "resolve learning status DB path: {err}"),
            LearningStatusError::Storage(err) => write!(f, "read semantic learning status: {err}"),
            LearningStatusError::Render(err) => write!(f, "render learning status: {err}"),
        }
    }
}

impl std::error::Error for LearningStatusError {}

impl From<StorageError> for LearningStatusError {
    fn from(value: StorageError) -> Self {
        LearningStatusError::Storage(value)
    }
}

pub fn run(args: &LearningStatusArgs) -> Result<LearningStatusOutcome, LearningStatusError> {
    let db_path = resolve_db_path(args.db_path.as_deref()).map_err(LearningStatusError::DbPath)?;
    let mut storage = match Storage::open(&db_path) {
        Ok(storage) => storage,
        Err(err) => return Ok(storage_unavailable_outcome(args, &db_path, err.to_string())),
    };
    match run_available(args, &db_path, &mut storage) {
        Ok(outcome) => Ok(outcome),
        Err(LearningStatusError::Storage(err)) => {
            Ok(storage_unavailable_outcome(args, &db_path, err.to_string()))
        }
        Err(err) => Err(err),
    }
}

fn run_available(
    args: &LearningStatusArgs,
    db_path: &std::path::Path,
    storage: &mut Storage,
) -> Result<LearningStatusOutcome, LearningStatusError> {
    let limit = args.limit.max(1);
    let min_support = args.min_support.max(2);
    let candidate_limit = args.candidate_limit.max(1);
    let review_limit = args.review_limit.max(1);
    let semantic = propose_semantic_consolidations(
        storage,
        SemanticLearningInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit,
            min_support,
            dry_run: true,
        },
    )?;
    let queue = build_review_queue(
        storage,
        crate::context::review::ReviewQueueInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit,
        },
    )?;
    let client = args.client.clone().unwrap_or_else(|| "generic".to_string());
    let digest = build_review_digest(
        storage,
        ReviewDigestInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit,
            client: Some(client.clone()),
            include_queue_only: true,
        },
    )?;
    let policy_sections = policy_sections(storage, args)?;
    let belief_candidates = scoped_belief_candidates(storage, args)?;

    let candidates = semantic
        .items
        .iter()
        .filter(|item| item.action != "skip" || item.skipped_reason.is_some())
        .take(candidate_limit)
        .map(|item| candidate_row(storage, item, args))
        .collect::<Result<Vec<_>, StorageError>>()?;
    let cloud_draft_blockers = queue
        .claims
        .iter()
        .filter(|item| item.claim.source_type == ClaimSourceType::CloudDraft)
        .take(review_limit)
        .map(cloud_draft_blocker_row)
        .collect::<Vec<_>>();
    let policy_items =
        policy_sections.iter().take(candidate_limit).map(policy_row).collect::<Vec<_>>();
    let belief_items = group_learning_belief_rows(
        belief_candidates
            .iter()
            .take(review_limit)
            .map(|candidate| belief_row(storage, candidate))
            .collect::<Result<Vec<_>, StorageError>>()?,
    );
    let belief_review_summary = learning_belief_review_summary(&belief_items);
    let belief_group_count = belief_items.len();
    let belief_hidden_duplicate_count = belief_candidates.len().saturating_sub(belief_group_count);
    let belief_contradiction_count =
        belief_candidates.iter().filter(|item| item.kind == BeliefKind::Contradicts).count();
    let review_items = digest
        .items
        .iter()
        .take(review_limit)
        .map(|item| LearningReviewItemRow {
            proposal_id: item.proposal_id,
            target: item.target_lifecycle_state.clone().unwrap_or_else(|| "review_only".into()),
            action: item.action.clone(),
            readiness: item.readiness.clone(),
            review_reason: item.review_reason.clone(),
            title: item.title.clone(),
            next_action: item.recommended_next_action.clone(),
        })
        .collect::<Vec<_>>();
    let semantic_review_only_candidate_count = semantic
        .items
        .iter()
        .filter(|item| semantic_item_requires_review_only_count(item))
        .count()
        .max(review_items.iter().filter(|item| item.target == "review_only").count());

    let summary = LearningStatusSummary {
        inspected_l3_claim_count: semantic.inspected_claim_count,
        repeated_group_count: semantic.repeated_group_count,
        l4_candidate_count: semantic
            .items
            .iter()
            .filter(|item| item.action == "would_propose")
            .count(),
        review_only_candidate_count: semantic_review_only_candidate_count,
        skipped_untrusted_count: semantic.skipped_untrusted_count,
        pending_review_item_count: queue.claim_count + queue.proposal_count,
        ready_proposal_count: queue.ready_proposal_count,
        manual_l4_review_count: queue.manual_review_proposal_count,
        cloud_draft_blocked_count: queue
            .claims
            .iter()
            .filter(|item| item.claim.source_type == ClaimSourceType::CloudDraft)
            .count(),
        policy_projection_count: policy_sections.len(),
        belief_candidate_count: belief_candidates.len(),
        belief_group_count,
        belief_hidden_duplicate_count,
        belief_contradiction_count,
        belief_substantive_contradiction_count: belief_review_summary
            .substantive_contradiction_candidate_count,
        belief_low_value_conflict_count: belief_review_summary.low_value_conflict_candidate_count,
        belief_low_value_noise_count: belief_review_summary.low_value_noise_candidate_count,
        belief_noise_candidate_count: belief_review_summary.noise_candidate_count,
        should_interrupt: digest.should_notify,
    };
    let review_lanes = review_lanes(args, &summary, &client);
    let review_surface = review_surface(args, &client, &summary);
    let promotion_matrix =
        promotion_matrix(&summary, &belief_review_summary, &review_lanes, &review_surface);
    let operator_card = build_operator_card(&summary, &review_lanes, &review_surface);
    let review_cards =
        build_review_cards(args, &candidates, &cloud_draft_blockers, &policy_items, &belief_items);
    let next_commands = next_commands(args, &client);

    Ok(normalize_learning_outcome_commands(LearningStatusOutcome {
        schema: "soma.semantic_learning_review_report.v1",
        source: "soma_learning_read_only_status",
        db_path: db_path.to_string_lossy().into_owned(),
        storage_status: "available",
        storage_error: None,
        status: operator_card.status.clone(),
        operator_next_action_id: operator_card.operator_next_action_id,
        operator_next_action_label: operator_card.operator_next_action_label,
        headline: operator_card.headline.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        project: args.project.clone(),
        session_id: args.session_id.clone(),
        client,
        limit,
        min_support,
        candidate_limit,
        review_limit,
        summary,
        counts: operator_card.review_counts.clone(),
        belief_review_summary,
        target_coverage: target_coverage(),
        promotion_matrix,
        review_lanes,
        review_surface,
        operator_card,
        review_cards,
        candidates,
        cloud_draft_blockers,
        policy_items,
        belief_items,
        review_items,
        dogfood_evidence: learning_dogfood_evidence(args),
        next_commands,
        recovery_commands: Vec::new(),
        trust_boundary: "soma_learning_is_read_only: previews semantic L4 candidates and review blockers; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and requires user/tool/test/local/correction evidence before cloud output can become durable memory",
    }))
}

fn storage_unavailable_outcome(
    args: &LearningStatusArgs,
    db_path: &std::path::Path,
    error: String,
) -> LearningStatusOutcome {
    let client = args.client.clone().unwrap_or_else(|| "generic".to_string());
    let limit = args.limit.max(1);
    let min_support = args.min_support.max(2);
    let candidate_limit = args.candidate_limit.max(1);
    let review_limit = args.review_limit.max(1);
    let summary = LearningStatusSummary {
        inspected_l3_claim_count: 0,
        repeated_group_count: 0,
        l4_candidate_count: 0,
        review_only_candidate_count: 0,
        skipped_untrusted_count: 0,
        pending_review_item_count: 0,
        ready_proposal_count: 0,
        manual_l4_review_count: 0,
        cloud_draft_blocked_count: 0,
        policy_projection_count: 0,
        belief_candidate_count: 0,
        belief_group_count: 0,
        belief_hidden_duplicate_count: 0,
        belief_contradiction_count: 0,
        belief_substantive_contradiction_count: 0,
        belief_low_value_conflict_count: 0,
        belief_low_value_noise_count: 0,
        belief_noise_candidate_count: 0,
        should_interrupt: false,
    };
    let belief_review_summary = empty_learning_belief_review_summary();
    let review_surface = unavailable_review_surface(args, &client);
    let operator_card = unavailable_operator_card(&summary, &review_surface);
    let recovery_commands = learning_storage_recovery_commands();
    let review_lanes = vec![LearningReviewLane {
        lane: "storage",
        priority: 0,
        status: "blocked",
        count: 1,
        next_action: "Restore access to the configured SOMA DB, or run the diagnostic DB command to confirm the CLI surface can render without reading private storage.".to_string(),
        trust_boundary:
            "storage_unavailable_lane_is_read_only: records no proof, creates no verification event, promotes no cloud draft, and applies no proposal",
        command: learning_storage_diagnostic_command(),
    }];
    let promotion_matrix =
        promotion_matrix(&summary, &belief_review_summary, &review_lanes, &review_surface);
    let mut next_command_list = Vec::new();
    next_command_list.extend(recovery_commands.clone());
    next_command_list.extend(next_commands(args, &client));
    normalize_learning_outcome_commands(LearningStatusOutcome {
        schema: "soma.semantic_learning_review_report.v1",
        source: "soma_learning_read_only_status",
        db_path: db_path.to_string_lossy().into_owned(),
        storage_status: "unavailable",
        storage_error: Some(error),
        status: operator_card.status.clone(),
        operator_next_action_id: operator_card.operator_next_action_id,
        operator_next_action_label: operator_card.operator_next_action_label,
        headline: operator_card.headline.clone(),
        primary_next_step: operator_card.primary_next_step.clone(),
        primary_next_command: operator_card.primary_next_command.clone(),
        project: args.project.clone(),
        session_id: args.session_id.clone(),
        client,
        limit,
        min_support,
        candidate_limit,
        review_limit,
        summary,
        counts: operator_card.review_counts.clone(),
        belief_review_summary,
        target_coverage: target_coverage(),
        promotion_matrix,
        review_lanes,
        review_surface,
        operator_card,
        review_cards: Vec::new(),
        candidates: Vec::new(),
        cloud_draft_blockers: Vec::new(),
        policy_items: Vec::new(),
        belief_items: Vec::new(),
        review_items: Vec::new(),
        dogfood_evidence: learning_dogfood_evidence(args),
        next_commands: next_command_list,
        recovery_commands,
        trust_boundary: "soma_learning_storage_unavailable_is_read_only: reports that semantic learning storage could not be read; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and makes no claim that review queues are clear",
    })
}

fn normalize_learning_outcome_commands(
    mut outcome: LearningStatusOutcome,
) -> LearningStatusOutcome {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();

    outcome.primary_next_command = learning_command_with_current_binary_when_path_soma_differs(
        outcome.primary_next_command,
        &binary_identity,
    );
    outcome.operator_card.primary_next_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.operator_card.primary_next_command,
            &binary_identity,
        );
    outcome.review_surface.render_plan_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.render_plan_command,
            &binary_identity,
        );
    outcome.review_surface.report_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.report_command,
            &binary_identity,
        );
    outcome.review_surface.queue_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.queue_command,
            &binary_identity,
        );
    outcome.review_surface.action_plan_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.action_plan_command,
            &binary_identity,
        );
    outcome.review_surface.digest_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.digest_command,
            &binary_identity,
        );
    outcome.review_surface.proof_session_command =
        learning_command_with_current_binary_when_path_soma_differs(
            outcome.review_surface.proof_session_command,
            &binary_identity,
        );
    for lane in &mut outcome.review_lanes {
        lane.command = learning_command_with_current_binary_when_path_soma_differs(
            lane.command.clone(),
            &binary_identity,
        );
    }
    for row in &mut outcome.promotion_matrix {
        row.next_action = learning_cli_hint_with_current_binary_when_path_soma_differs(
            &row.next_action,
            &binary_identity,
        );
        row.primary_command = learning_command_with_current_binary_when_path_soma_differs(
            row.primary_command.clone(),
            &binary_identity,
        );
    }
    for card in &mut outcome.review_cards {
        card.primary_command = learning_command_with_current_binary_when_path_soma_differs(
            card.primary_command.clone(),
            &binary_identity,
        );
    }
    for blocker in &mut outcome.cloud_draft_blockers {
        blocker.cli_hint = learning_cli_hint_with_current_binary_when_path_soma_differs(
            &blocker.cli_hint,
            &binary_identity,
        );
        for action in &mut blocker.action_options {
            action.cli_hint = learning_cli_hint_with_current_binary_when_path_soma_differs(
                &action.cli_hint,
                &binary_identity,
            );
        }
    }
    for belief in &mut outcome.belief_items {
        belief.resolution_action.cli_command =
            learning_command_with_current_binary_when_path_soma_differs(
                belief.resolution_action.cli_command.clone(),
                &binary_identity,
            );
    }
    outcome.next_commands = outcome
        .next_commands
        .into_iter()
        .map(|command| {
            learning_command_with_current_binary_when_path_soma_differs(command, &binary_identity)
        })
        .collect();
    outcome.recovery_commands = outcome
        .recovery_commands
        .into_iter()
        .map(|command| {
            learning_command_with_current_binary_when_path_soma_differs(command, &binary_identity)
        })
        .collect();

    outcome
}

fn learning_command_with_current_binary_when_path_soma_differs(
    command: Vec<String>,
    binary_identity: &crate::cli::binary_identity::BinaryIdentity,
) -> Vec<String> {
    crate::cli::binary_identity::command_with_current_binary_when_path_soma_differs(
        command,
        binary_identity,
    )
}

fn learning_cli_hint_with_current_binary_when_path_soma_differs(
    hint: &str,
    binary_identity: &crate::cli::binary_identity::BinaryIdentity,
) -> String {
    let Some(current_exe) = binary_identity.resolved_soma_bin() else {
        return hint.to_string();
    };
    hint.strip_prefix("soma ")
        .map(|rest| format!("{current_exe} {rest}"))
        .unwrap_or_else(|| hint.to_string())
}

fn learning_dogfood_evidence(args: &LearningStatusArgs) -> Option<LearningDogfoodEvidence> {
    let (path, explicit) = learning_dogfood_report_path(args)?;
    if !explicit && !path.is_file() {
        return None;
    }
    let path_text = path.to_string_lossy().into_owned();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            return Some(LearningDogfoodEvidence {
                source: "soma_learning.dogfood_evidence.v1",
                path: path_text,
                status: "unreadable",
                report_status: None,
                semantic_learning_review_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                error: Some(err.to_string()),
                trust_boundary: "learning_dogfood_evidence_is_read_only: cites optional external dogfood evidence only; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and cannot prove live review queues are clear",
            });
        }
    };
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(err) => {
            return Some(LearningDogfoodEvidence {
                source: "soma_learning.dogfood_evidence.v1",
                path: path_text,
                status: "invalid_json",
                report_status: None,
                semantic_learning_review_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                error: Some(err.to_string()),
                trust_boundary: "learning_dogfood_evidence_is_read_only: cites optional external dogfood evidence only; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and cannot prove live review queues are clear",
            });
        }
    };
    if value.get("schema").and_then(Value::as_str) != Some("soma.client_dogfood_report.v1") {
        return Some(LearningDogfoodEvidence {
            source: "soma_learning.dogfood_evidence.v1",
            path: path_text,
            status: "invalid_schema",
            report_status: value.get("status").and_then(Value::as_str).map(ToOwned::to_owned),
            semantic_learning_review_status: dogfood_objective_status(
                &value,
                "semantic_learning_review",
            ),
            summary_pass: dogfood_summary_count(&value, "pass"),
            summary_warn: dogfood_summary_count(&value, "warn"),
            summary_fail: dogfood_summary_count(&value, "fail"),
            error: Some("expected schema soma.client_dogfood_report.v1".to_string()),
            trust_boundary: "learning_dogfood_evidence_is_read_only: cites optional external dogfood evidence only; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and cannot prove live review queues are clear",
        });
    }
    Some(LearningDogfoodEvidence {
        source: "soma_learning.dogfood_evidence.v1",
        path: path_text,
        status: "valid",
        report_status: value.get("status").and_then(Value::as_str).map(ToOwned::to_owned),
        semantic_learning_review_status: dogfood_objective_status(
            &value,
            "semantic_learning_review",
        ),
        summary_pass: dogfood_summary_count(&value, "pass"),
        summary_warn: dogfood_summary_count(&value, "warn"),
        summary_fail: dogfood_summary_count(&value, "fail"),
        error: None,
        trust_boundary: "learning_dogfood_evidence_is_read_only: cites optional external dogfood evidence only; records no proposal, creates no verification event, applies no proposal, writes no semantic_fact, promotes no cloud draft, and cannot prove live review queues are clear",
    })
}

fn learning_dogfood_report_path(args: &LearningStatusArgs) -> Option<(PathBuf, bool)> {
    if let Some(path) = args.dogfood_report.as_deref().and_then(nonempty) {
        return Some((PathBuf::from(path), true));
    }
    if let Some(value) = env::var_os("SOMA_CLIENT_DOGFOOD_REPORT").filter(|value| !value.is_empty())
    {
        return Some((PathBuf::from(value), true));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some((PathBuf::from(home).join(".soma/reports/client-dogfood-latest.json"), false))
}

fn dogfood_objective_status(value: &Value, objective_name: &str) -> Option<String> {
    value
        .get("objectives")
        .and_then(Value::as_array)?
        .iter()
        .find(|objective| {
            objective.get("objective").and_then(Value::as_str) == Some(objective_name)
        })
        .and_then(|objective| objective.get("status").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn dogfood_summary_count(value: &Value, key: &str) -> Option<u64> {
    value.get("summary").and_then(|summary| summary.get(key)).and_then(Value::as_u64)
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn unavailable_review_surface(args: &LearningStatusArgs, client: &str) -> LearningReviewSurface {
    LearningReviewSurface {
        source: "soma_learning.review_surface.v1",
        client: client.to_string(),
        primary_surface: "unavailable",
        primary_reason: "semantic learning storage could not be read; review status is unknown"
            .to_string(),
        render_plan_command: scoped_client_command(
            args,
            &["soma", "context", "review-render", "--format", "json"],
            client,
        ),
        report_command: scoped_command(
            args,
            &["soma", "context", "review-report", "--format", "json"],
            None,
        ),
        queue_command: scoped_command(
            args,
            &["soma", "context", "review-queue", "--format", "json"],
            None,
        ),
        action_plan_command: scoped_command(
            args,
            &["soma", "context", "review-actions", "--format", "json"],
            None,
        ),
        digest_command: scoped_client_command(
            args,
            &[
                "soma",
                "context",
                "review-digest",
                "--include-queue-only",
                "--format",
                "json",
            ],
            client,
        ),
        proof_session_command: vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--proof-session".to_string(),
            "--json".to_string(),
        ],
        mcp_tools: vec![
            "soma_review_render",
            "soma_review_report",
            "soma_review_actions",
            "soma_client_binding_proof_session",
        ],
        control_contract: "semantic_learning_review_unavailable_until_storage_read_succeeds",
        proof_path: "unavailable_until_storage_read_succeeds",
        trust_boundary:
            "learning_review_surface_unavailable_is_read_only: records no proof, creates no verification event, promotes no cloud draft, and applies no proposal",
    }
}

fn unavailable_operator_card(
    summary: &LearningStatusSummary,
    review_surface: &LearningReviewSurface,
) -> LearningOperatorCard {
    LearningOperatorCard {
        source: "soma_learning.operator_card.v1",
        status: "storage_unavailable".to_string(),
        operator_next_action_id: "restore_learning_storage_access",
        operator_next_action_label: "Restore learning storage access",
        headline: "Semantic learning storage is unreadable; review status is unknown.".to_string(),
        primary_next_step: "Grant SOMA read access to the configured DB, or run the diagnostic DB command to confirm the learning surface can render without private storage.".to_string(),
        primary_next_command: learning_storage_diagnostic_command(),
        review_surface: review_surface.primary_surface,
        review_counts: LearningOperatorReviewCounts {
            l4_semantic_fact_candidates: summary.l4_candidate_count,
            review_only_semantic_candidates: summary.review_only_candidate_count,
            cloud_draft_blockers: summary.cloud_draft_blocked_count,
            policy_projections: summary.policy_projection_count,
            belief_candidates: summary.belief_candidate_count,
            belief_groups: summary.belief_group_count,
            belief_hidden_duplicates: summary.belief_hidden_duplicate_count,
            belief_contradictions: summary.belief_contradiction_count,
            belief_substantive_contradictions: summary.belief_substantive_contradiction_count,
            belief_low_value_conflicts: summary.belief_low_value_conflict_count,
            belief_low_value_noise: summary.belief_low_value_noise_count,
            belief_noise_candidates: summary.belief_noise_candidate_count,
            pending_review_items: summary.pending_review_item_count,
            ready_to_apply_proposals: summary.ready_proposal_count,
            manual_l4_review_items: summary.manual_l4_review_count,
            should_interrupt: false,
        },
        lanes_requiring_review: vec!["storage".to_string()],
        safe_to_claim: vec![
            "The semantic learning CLI surface rendered a read-only degraded report.".to_string(),
        ],
        blocked_claims: vec![
            "Semantic review queues are not clear while storage is unreadable.".to_string(),
            "No L4 fact/policy/belief readiness can be claimed from this degraded report."
                .to_string(),
            "No cloud draft may be promoted while storage is unreadable.".to_string(),
        ],
        trust_boundary:
            "learning_operator_card_storage_unavailable_is_read_only: records no proposal, verification event, proof row, L4 write, or cloud-draft promotion",
    }
}

fn learning_operator_next_action(status: &str) -> (&'static str, &'static str) {
    match status {
        "cloud_draft_blocked" => ("review_cloud_draft_blockers", "Review cloud draft blockers"),
        "l4_review_ready" => ("review_l4_semantic_candidates", "Review L4 semantic candidates"),
        "semantic_review_only_pending" => {
            ("request_semantic_candidate_verification", "Request semantic candidate verification")
        }
        "belief_review_pending" => {
            ("resolve_belief_review_signals", "Resolve belief review signals")
        }
        "review_pending" => ("render_semantic_review_controls", "Render semantic review controls"),
        "clear" => ("no_semantic_review_action_required", "No semantic review action required"),
        _ => ("inspect_semantic_learning_status", "Inspect semantic learning status"),
    }
}

fn learning_storage_diagnostic_db_path() -> String {
    std::env::temp_dir()
        .join(format!("soma-learning-diagnostic-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

fn learning_storage_diagnostic_command() -> Vec<String> {
    vec![
        "soma".to_string(),
        "learning".to_string(),
        "--db-path".to_string(),
        learning_storage_diagnostic_db_path(),
        "--json".to_string(),
    ]
}

fn learning_storage_recovery_commands() -> Vec<Vec<String>> {
    vec![
        learning_storage_diagnostic_command(),
        vec![
            "soma".to_string(),
            "learning".to_string(),
            "--db-path".to_string(),
            "<readable-soma.db>".to_string(),
            "--json".to_string(),
        ],
        vec!["soma".to_string(), "diagnose".to_string()],
    ]
}

pub fn render_json(outcome: &LearningStatusOutcome) -> Result<String, LearningStatusError> {
    serde_json::to_string_pretty(outcome)
        .map(|mut text| {
            text.push('\n');
            text
        })
        .map_err(LearningStatusError::Render)
}

pub fn render_text(outcome: &LearningStatusOutcome) -> String {
    let mut out = String::new();
    out.push_str("SOMA semantic learning review\n");
    out.push_str(&format!(
        "  Status: {} - {}\n",
        outcome.operator_card.status, outcome.operator_card.headline
    ));
    if outcome.storage_status != "available" {
        out.push_str(&format!("  Storage: {}\n", outcome.storage_status));
        if let Some(error) = &outcome.storage_error {
            out.push_str(&format!("    error: {error}\n"));
        }
        for command in &outcome.recovery_commands {
            out.push_str(&format!("    recovery: {}\n", command_line(command)));
        }
    }
    out.push_str(&format!("  Primary next: {}\n", outcome.operator_card.primary_next_step));
    out.push_str(&format!(
        "  Operator action: {} ({})\n",
        outcome.operator_next_action_id, outcome.operator_next_action_label
    ));
    if !outcome.operator_card.primary_next_command.is_empty() {
        out.push_str(&format!(
            "  Primary command: {}\n",
            command_line(&outcome.operator_card.primary_next_command)
        ));
    }
    out.push_str(&format!(
        "  Inspection window: l3_limit={} semantic_candidate_limit={} review_limit={} min_support={} (counts are bounded by this window)\n",
        outcome.limit, outcome.candidate_limit, outcome.review_limit, outcome.min_support
    ));
    out.push_str(&format!(
        "  Card counts: l4={} cloud_draft={} policy={} belief={} groups={} hidden_duplicates={} contradictions={} substantive={} low_value_conflicts={} low_value_noise={}\n",
        outcome.operator_card.review_counts.l4_semantic_fact_candidates,
        outcome.operator_card.review_counts.cloud_draft_blockers,
        outcome.operator_card.review_counts.policy_projections,
        outcome.operator_card.review_counts.belief_candidates,
        outcome.operator_card.review_counts.belief_groups,
        outcome.operator_card.review_counts.belief_hidden_duplicates,
        outcome.operator_card.review_counts.belief_contradictions,
        outcome.operator_card.review_counts.belief_substantive_contradictions,
        outcome.operator_card.review_counts.belief_low_value_conflicts,
        outcome.operator_card.review_counts.belief_low_value_noise
    ));
    out.push_str(&format!(
        "  L3 inspected: {}  L4 candidates: {}  review-only candidates: {}  cloud drafts blocked: {}\n",
        outcome.summary.inspected_l3_claim_count,
        outcome.summary.l4_candidate_count,
        outcome.summary.review_only_candidate_count,
        outcome.summary.cloud_draft_blocked_count
    ));
    out.push_str(&format!(
        "  Pending review: {}  ready apply: {}  manual L4 review: {}  interrupt: {}\n\n",
        outcome.summary.pending_review_item_count,
        outcome.summary.ready_proposal_count,
        outcome.summary.manual_l4_review_count,
        outcome.summary.should_interrupt
    ));
    out.push_str(&format!(
        "  Policy projections: {}  belief candidates: {}  groups: {}  hidden duplicates: {}  contradictions: {}\n",
        outcome.summary.policy_projection_count,
        outcome.summary.belief_candidate_count,
        outcome.summary.belief_group_count,
        outcome.summary.belief_hidden_duplicate_count,
        outcome.summary.belief_contradiction_count
    ));
    out.push_str(&format!(
        "  Belief workload: status={} substantive_groups={} substantive_candidates={} low_value_conflicts={} low_value_noise={} noise_candidates={} primary_group={}\n",
        outcome.belief_review_summary.status,
        outcome.belief_review_summary.substantive_contradiction_group_count,
        outcome.belief_review_summary.substantive_contradiction_candidate_count,
        outcome.belief_review_summary.low_value_conflict_candidate_count,
        outcome.belief_review_summary.low_value_noise_candidate_count,
        outcome.belief_review_summary.noise_candidate_count,
        outcome
            .belief_review_summary
            .primary_group_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));
    out.push_str(&format!("  Belief next: {}\n\n", outcome.belief_review_summary.next_action));

    out.push_str("Review surface:\n");
    out.push_str(&format!(
        "  primary: {} client={} reason={}\n",
        outcome.review_surface.primary_surface,
        outcome.review_surface.client,
        outcome.review_surface.primary_reason
    ));
    out.push_str(&format!(
        "  render: {}\n",
        command_line(&outcome.review_surface.render_plan_command)
    ));
    out.push_str(&format!(
        "  actions: {}\n",
        command_line(&outcome.review_surface.action_plan_command)
    ));
    out.push_str(&format!(
        "  proof: {}\n\n",
        command_line(&outcome.review_surface.proof_session_command)
    ));

    out.push_str("Target coverage:\n");
    for target in &outcome.target_coverage {
        out.push_str(&format!("  {}: {} ({})\n", target.target, target.status, target.note));
    }

    out.push_str("\nPromotion matrix:\n");
    for row in &outcome.promotion_matrix {
        out.push_str(&format!(
            "  {} lane={} status={} count={} manual_l4_review={} context_projection={} blocks_l4={}\n",
            row.target,
            row.lane,
            row.status,
            row.candidate_count,
            row.ready_for_manual_l4_review,
            row.context_projection_ready,
            row.blocks_l4_promotion
        ));
        if let Some(section) = row.projected_context_section {
            out.push_str(&format!("    projects_to: {section}\n"));
        }
        out.push_str(&format!("    evidence: {}\n", row.required_evidence));
        out.push_str(&format!("    next: {}\n", row.next_action));
        if !row.primary_command.is_empty() {
            out.push_str(&format!("    command: {}\n", command_line(&row.primary_command)));
        }
    }

    out.push_str("\nReview lanes:\n");
    for lane in &outcome.review_lanes {
        out.push_str(&format!(
            "  P{} {} count={} status={}\n",
            lane.priority, lane.lane, lane.count, lane.status
        ));
        out.push_str(&format!("    next: {}\n", lane.next_action));
        out.push_str(&format!("    command: {}\n", lane.command.join(" ")));
    }

    out.push_str("\nReview cards:\n");
    if outcome.review_cards.is_empty() {
        out.push_str("  none\n");
    } else {
        for card in &outcome.review_cards {
            out.push_str(&format!(
                "  P{} {} {} status={} blocks_l4={}\n",
                card.priority, card.card_id, card.target, card.status, card.blocks_l4_promotion
            ));
            out.push_str(&format!("    title: {}\n", card.title));
            out.push_str(&format!("    summary: {}\n", card.summary));
            out.push_str(&format!("    projection: {}\n", card.projection_path));
            out.push_str(&format!(
                "    evidence: {} verifiers={} forbidden={}\n",
                card.evidence_rule,
                card.accepted_verifier_types.join(","),
                card.forbidden_evidence_sources.join(",")
            ));
            out.push_str(&format!("    action: {}\n", card.primary_action));
            if !card.primary_command.is_empty() {
                out.push_str(&format!("    command: {}\n", command_line(&card.primary_command)));
            }
        }
    }

    out.push_str("\nCandidate preview:\n");
    if outcome.candidates.is_empty() {
        out.push_str("  none\n");
    } else {
        for candidate in &outcome.candidates {
            out.push_str(&format!(
                "  {} {} support={} durable_trust={} cloud_draft_support={} verified_cloud_draft_support={} blocked_cloud_draft_support={} unverified_support={} verdict={} bias={}\n",
                candidate.target,
                candidate.action,
                candidate.support_count,
                candidate.durable_promotion_trust,
                candidate.cloud_draft_support_count,
                candidate.verified_cloud_draft_support_count,
                candidate.blocked_cloud_draft_support_count,
                candidate.unverified_support_count,
                candidate.readiness_verdict,
                candidate.bias_risk
            ));
            out.push_str(&format!("    support_trust: {}\n", candidate.support_trust_summary));
            out.push_str(&format!("    {}\n", candidate.normalized_text));
            out.push_str(&format!("    action: {}\n", candidate.review_action));
            for support in candidate.support_claims.iter().take(3) {
                out.push_str(&format!(
                    "    support #{} {} {} basis={} durable_trust={} verification_events={}: {}\n",
                    support.claim_id,
                    support.source_type,
                    support.trust_status,
                    support.promotion_trust_basis,
                    support.durable_promotion_trust,
                    support.verification_event_count,
                    support.text_preview
                ));
            }
        }
    }

    out.push_str("\nCloud draft blockers:\n");
    if outcome.cloud_draft_blockers.is_empty() {
        out.push_str("  none\n");
    } else {
        for blocker in &outcome.cloud_draft_blockers {
            out.push_str(&format!(
                "  claim #{} lifecycle={} verified_events={} durable_trust={}\n",
                blocker.claim_id,
                blocker.lifecycle_state,
                blocker.verification_event_count,
                blocker.durable_promotion_trust
            ));
            if let Some(task_frame_id) = blocker.task_frame_id {
                out.push_str(&format!("    task_frame: {task_frame_id}\n"));
            }
            out.push_str(&format!("    {}\n", blocker.text_preview));
            out.push_str(&format!("    next: {}\n", blocker.next_action));
            out.push_str(&format!("    command: {}\n", blocker.cli_hint));
            if let Some(action) = blocker.action_options.iter().find(|action| action.enabled) {
                out.push_str(&format!(
                    "    primary_control: {} action={}\n",
                    action.control_id, action.action
                ));
            }
        }
    }

    out.push_str("\nPolicy projection preview:\n");
    if outcome.policy_items.is_empty() {
        out.push_str("  none\n");
    } else {
        for item in &outcome.policy_items {
            out.push_str(&format!(
                "  status={} confidence={}\n",
                item.status.as_deref().unwrap_or("unknown"),
                item.confidence
                    .map(|value| format!("{:.0}%", value * 100.0))
                    .unwrap_or_else(|| "unknown".to_string())
            ));
            out.push_str(&format!("    {}\n", item.text));
        }
    }

    out.push_str("\nBelief candidate preview:\n");
    if outcome.belief_items.is_empty() {
        out.push_str("  none\n");
    } else {
        for item in &outcome.belief_items {
            out.push_str(&format!(
                "  #{} {} score={:.2} status={} triage={} noise={} priority={} group_size={} duplicates_hidden={}\n",
                item.id,
                item.kind,
                item.score,
                item.status,
                item.triage_status,
                item.noise_risk,
                item.review_priority,
                item.group_candidate_count,
                item.hidden_duplicate_count
            ));
            if let Some(evidence) = &item.evidence {
                out.push_str(&format!("    evidence: {evidence}\n"));
            }
            out.push_str(&format!("    claim: {}\n", item.claim_preview));
            out.push_str(&format!(
                "    scope: project={} session={}\n",
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
                out.push_str(&format!("    grouped_candidates: {ids}\n"));
            }
            out.push_str(&format!("    triage: {}\n", item.triage_reason));
            out.push_str(&format!(
                "    episode {}: {}\n",
                item.episode_a_id, item.episode_a_preview
            ));
            out.push_str(&format!("      outcome: {}\n", item.episode_a_outcome));
            out.push_str(&format!(
                "    episode {}: {}\n",
                item.episode_b_id, item.episode_b_preview
            ));
            out.push_str(&format!("      outcome: {}\n", item.episode_b_outcome));
            out.push_str(&format!("    action: {}\n", item.review_action));
            out.push_str(&format!(
                "    resolution: {} via {}\n",
                item.resolution_action.action,
                command_line(&item.resolution_action.cli_command)
            ));
        }
    }

    out.push_str("\nReview items:\n");
    if outcome.review_items.is_empty() {
        out.push_str("  none\n");
    } else {
        for item in &outcome.review_items {
            out.push_str(&format!(
                "  proposal #{} target={} readiness={}\n",
                item.proposal_id, item.target, item.readiness
            ));
            out.push_str(&format!("    next: {}\n", item.next_action));
        }
    }

    out.push_str("\nUseful next commands:\n");
    for command in &outcome.next_commands {
        out.push_str("  ");
        out.push_str(&command.join(" "));
        out.push('\n');
    }
    out.push_str("\nTrust boundary: read-only status only; no proposal, verification, apply, L4 write, or cloud-draft promotion.\n");
    out
}

pub fn render_brief(outcome: &LearningStatusOutcome) -> String {
    let mut out = String::new();
    out.push_str("SOMA semantic learning brief\n");
    out.push_str(&format!("  status: {}\n", outcome.operator_card.status));
    out.push_str(&format!("  headline: {}\n", outcome.operator_card.headline));
    out.push_str(&format!(
        "  operator_action: {} ({})\n",
        outcome.operator_next_action_id, outcome.operator_next_action_label
    ));
    out.push_str(&format!("  next: {}\n", outcome.operator_card.primary_next_step));
    if !outcome.operator_card.primary_next_command.is_empty() {
        out.push_str(&format!(
            "  command: {}\n",
            command_line(&outcome.operator_card.primary_next_command)
        ));
    }
    out.push_str(&format!(
        "  scope: project={} session={} client={}\n",
        outcome.project.as_deref().unwrap_or("all"),
        outcome.session_id.as_deref().unwrap_or("all"),
        outcome.client
    ));
    out.push_str(&format!(
        "  counts: l4={} review_only={} cloud_draft={} policy={} belief={} pending={} manual_l4={}\n",
        outcome.operator_card.review_counts.l4_semantic_fact_candidates,
        outcome.operator_card.review_counts.review_only_semantic_candidates,
        outcome.operator_card.review_counts.cloud_draft_blockers,
        outcome.operator_card.review_counts.policy_projections,
        outcome.operator_card.review_counts.belief_candidates,
        outcome.operator_card.review_counts.pending_review_items,
        outcome.operator_card.review_counts.manual_l4_review_items,
    ));
    out.push_str(&format!(
        "  review_surface: {} reason={}\n",
        outcome.review_surface.primary_surface, outcome.review_surface.primary_reason
    ));
    out.push_str(&format!(
        "  render: {}\n",
        command_line(&outcome.review_surface.render_plan_command)
    ));
    out.push_str(&format!(
        "  proof: {}\n",
        command_line(&outcome.review_surface.proof_session_command)
    ));
    if let Some(evidence) = &outcome.dogfood_evidence {
        let report_status = evidence.report_status.as_deref().unwrap_or("unknown");
        let semantic_status =
            evidence.semantic_learning_review_status.as_deref().unwrap_or("unknown");
        let pass =
            evidence.summary_pass.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        let warn =
            evidence.summary_warn.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        let fail =
            evidence.summary_fail.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        out.push_str(&format!(
            "  dogfood artifact: status={} report={} semantic_learning_review={} summary=pass={} warn={} fail={} path={}\n",
            evidence.status, report_status, semantic_status, pass, warn, fail, evidence.path
        ));
    }
    brief_list(&mut out, "safe_to_claim", &outcome.operator_card.safe_to_claim);
    brief_list(&mut out, "blocked_claims", &outcome.operator_card.blocked_claims);
    render_brief_semantic_candidates(&mut out, outcome);
    out.push_str("  review_lanes:\n");
    if outcome.review_lanes.is_empty() {
        out.push_str("    - none\n");
    } else {
        for lane in &outcome.review_lanes {
            out.push_str(&format!(
                "    - P{} {} status={} count={} next={}\n",
                lane.priority, lane.lane, lane.status, lane.count, lane.next_action
            ));
            if !lane.command.is_empty() {
                out.push_str(&format!("      command: {}\n", command_line(&lane.command)));
            }
        }
    }
    out.push_str("  review_cards:\n");
    if outcome.review_cards.is_empty() {
        out.push_str("    - none\n");
    } else {
        for card in outcome.review_cards.iter().take(5) {
            out.push_str(&format!(
                "    - {} target={} status={} blocks_l4={} action={}\n",
                card.lane, card.target, card.status, card.blocks_l4_promotion, card.primary_action
            ));
            if !card.primary_command.is_empty() {
                out.push_str(&format!("      command: {}\n", command_line(&card.primary_command)));
            }
        }
        let hidden = outcome.review_cards.len().saturating_sub(5);
        if hidden > 0 {
            out.push_str(&format!("    - ... {hidden} more review card(s); use --json or text\n"));
        }
    }
    out.push_str("  promotion_matrix:\n");
    for row in &outcome.promotion_matrix {
        out.push_str(&format!(
            "    - {} status={} count={} blocks_l4={} evidence={}\n",
            row.target,
            row.status,
            row.candidate_count,
            row.blocks_l4_promotion,
            row.required_evidence
        ));
    }
    out.push_str(&format!("  trust_boundary: {}\n", outcome.operator_card.trust_boundary));
    out
}

fn render_brief_semantic_candidates(out: &mut String, outcome: &LearningStatusOutcome) {
    out.push_str("  semantic_candidates:\n");
    if outcome.candidates.is_empty() {
        out.push_str("    - none\n");
        return;
    }
    for (index, candidate) in outcome.candidates.iter().take(3).enumerate() {
        out.push_str(&format!(
            "    - {}. target={} action={} verdict={} support={} bias={} review_action={}\n",
            index + 1,
            candidate.target,
            candidate.action,
            candidate.readiness_verdict,
            candidate.support_count,
            candidate.bias_risk,
            candidate.review_action
        ));
        out.push_str(&format!(
            "      claim: {}\n",
            truncate_chars(&candidate.normalized_text, 180)
        ));
        out.push_str(&format!("      support_trust: {}\n", candidate.support_trust_summary));
        if !candidate.review_rationale.is_empty() {
            out.push_str(&format!(
                "      rationale: {}\n",
                truncate_chars(&candidate.review_rationale, 180)
            ));
        }
        if let Some(proposal_id) = candidate.proposal_id {
            out.push_str(&format!(
                "      resolution: proposal_id={} status={} next={}\n",
                proposal_id,
                candidate.resolution_status,
                truncate_chars(&candidate.resolution_next_step, 180)
            ));
            out.push_str(&format!(
                "      review_queue: {}\n",
                command_line(&candidate.review_queue_command)
            ));
            out.push_str(&format!(
                "      verification_template: {}\n",
                command_line(&candidate.verification_template_command)
            ));
            for action in candidate.resolution_actions.iter().take(4) {
                out.push_str(&format!(
                    "      resolution_action: {} control_id={} evidence_required={} cli={}\n",
                    action.action,
                    action.control_id,
                    action.requires_evidence,
                    command_line(&action.cli_command)
                ));
            }
            out.push_str(&format!(
                "      resolution_boundary: {}\n",
                candidate.resolution_trust_boundary
            ));
        } else if !candidate.verification_template_command.is_empty() {
            out.push_str(&format!(
                "      verification_template: {}\n",
                command_line(&candidate.verification_template_command)
            ));
        }
        for support in candidate.support_claims.iter().take(2) {
            out.push_str(&format!(
                "      support_claim: claim:{} source={} lifecycle={} trust={} verifications={} text={}\n",
                support.claim_id,
                support.source_type,
                support.lifecycle_state,
                support.trust_status,
                support.verification_event_count,
                support.text_preview
            ));
        }
        let hidden_support = candidate.support_claims.len().saturating_sub(2);
        if hidden_support > 0 {
            out.push_str(&format!(
                "      support_more: {hidden_support} additional claim(s); use --json for full evidence\n"
            ));
        }
    }
    let hidden_candidates = outcome.candidates.len().saturating_sub(3);
    if hidden_candidates > 0 {
        out.push_str(&format!(
            "    - ... {hidden_candidates} more semantic candidate(s); use --json or semantic-proposals --brief\n"
        ));
    }
    out.push_str(
        "    boundary: semantic_candidates_are_read_only; no verification event, proposal apply, semantic_fact write, or cloud-draft promotion is recorded by this brief\n",
    );
}

fn brief_list(out: &mut String, label: &str, values: &[String]) {
    out.push_str(&format!("  {label}:\n"));
    if values.is_empty() {
        out.push_str("    - none\n");
        return;
    }
    for value in values {
        out.push_str(&format!("    - {value}\n"));
    }
}

fn policy_sections(
    storage: &Storage,
    args: &LearningStatusArgs,
) -> Result<Vec<ContextSection>, StorageError> {
    if let Some(session_id) = &args.session_id {
        user_policy_from_storage_with_corrections_session_set(
            storage,
            args.project.as_deref(),
            std::slice::from_ref(session_id),
            &[],
        )
    } else {
        user_policy_from_storage(storage, args.project.as_deref())
    }
}

fn scoped_belief_candidates(
    storage: &Storage,
    args: &LearningStatusArgs,
) -> Result<Vec<BeliefCandidate>, StorageError> {
    let row_limit = args.review_limit.max(1);
    let read_limit = row_limit.saturating_mul(4).max(row_limit);
    let mut rows = Vec::new();
    rows.extend(storage.recent_beliefs_of_kind(BeliefKind::Contradicts, read_limit)?);
    rows.extend(storage.recent_beliefs_of_kind(BeliefKind::Corroborates, read_limit)?);
    let mut scoped = Vec::new();
    for row in rows {
        if belief_candidate_matches_scope(
            storage,
            &row,
            args.project.as_deref(),
            args.session_id.as_deref(),
        )? {
            let triage = belief_candidate_triage(storage, &row)?;
            scoped.push((triage.review_priority, row.created_at_ns, row));
        }
    }
    scoped.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));
    scoped.truncate(row_limit);
    Ok(scoped.into_iter().map(|(_, _, row)| row).collect())
}

fn belief_candidate_matches_scope(
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

fn candidate_row(
    storage: &Storage,
    item: &SemanticLearningItem,
    args: &LearningStatusArgs,
) -> Result<LearningCandidateRow, StorageError> {
    let support_claims = item
        .support_claim_ids
        .iter()
        .copied()
        .map(|claim_id| support_claim_row(storage, claim_id))
        .collect::<Result<Vec<_>, StorageError>>()?;
    let (review_action, review_rationale) = candidate_review_guidance(item);
    let cloud_draft_support_count =
        support_claims.iter().filter(|claim| claim.source_type == "cloud_draft").count();
    let verified_cloud_draft_support_count = support_claims
        .iter()
        .filter(|claim| claim.source_type == "cloud_draft" && claim.durable_promotion_trust)
        .count();
    let blocked_cloud_draft_support_count =
        cloud_draft_support_count.saturating_sub(verified_cloud_draft_support_count);
    let unverified_support_count =
        support_claims.iter().filter(|claim| !claim.durable_promotion_trust).count();
    let support_trust_summary = support_trust_summary(
        cloud_draft_support_count,
        verified_cloud_draft_support_count,
        blocked_cloud_draft_support_count,
        unverified_support_count,
    );
    let review_queue_command = learning_review_queue_command(args);
    let verification_template_command = learning_semantic_verification_template_command(item);
    let resolution_actions = learning_semantic_resolution_actions(item);
    Ok(LearningCandidateRow {
        proposal_id: item.proposal_id,
        target: if item.action == "would_propose" { "semantic_fact" } else { "review_only" },
        action: item.action.clone(),
        normalized_text: item.normalized_text.clone(),
        group_rule: item.group_rule.clone(),
        support_count: item.support_count,
        support_claim_ids: item.support_claim_ids.clone(),
        support_claims,
        durable_promotion_trust: item.trusted,
        trusted: item.trusted,
        contains_cloud_draft_support: cloud_draft_support_count > 0,
        cloud_draft_support_count,
        verified_cloud_draft_support_count,
        blocked_cloud_draft_support_count,
        unverified_support_count,
        support_trust_summary,
        readiness_verdict: item.readiness_score.verdict.clone(),
        bias_risk: item.support_diversity.bias_risk.clone(),
        skipped_reason: item.skipped_reason.clone(),
        review_action,
        review_rationale,
        resolution_status: item.resolution_plan.status.clone(),
        resolution_next_step: item.resolution_plan.next_step.clone(),
        review_queue_command,
        verification_template_command,
        resolution_actions,
        resolution_trust_boundary: item.resolution_plan.trust_boundary.clone(),
    })
}

fn learning_review_queue_command(args: &LearningStatusArgs) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "review-queue".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--limit".to_string(),
        "20".to_string(),
    ];
    if let Some(project) = args.project.as_deref() {
        command.push("--project".to_string());
        command.push(project.to_string());
    }
    if let Some(session_id) = args.session_id.as_deref() {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
    command
}

fn learning_semantic_verification_template_command(item: &SemanticLearningItem) -> Vec<String> {
    let mut command = vec!["soma".to_string(), "context".to_string(), "verify-claim".to_string()];
    if let Some(proposal_id) = item.proposal_id {
        command.push("--proposal-id".to_string());
        command.push(proposal_id.to_string());
    } else if let Some(claim_id) =
        item.proposal_claim_ids.first().or(item.support_claim_ids.first())
    {
        command.push("--claim-id".to_string());
        command.push(claim_id.to_string());
    } else {
        command.push("--claim-id".to_string());
        command.push("CLAIM_ID".to_string());
    }
    command.extend([
        "--verifier".to_string(),
        "TRUSTED_VERIFIER".to_string(),
        "--result".to_string(),
        "VERIFICATION_RESULT".to_string(),
        "--evidence-kind".to_string(),
        "TRUSTED_EVIDENCE_KIND".to_string(),
        "--evidence-id".to_string(),
        "TRUSTED_EVIDENCE_ID".to_string(),
    ]);
    command
}

fn learning_semantic_resolution_actions(
    item: &SemanticLearningItem,
) -> Vec<LearningSemanticResolutionAction> {
    let Some(proposal_id) = item.proposal_id else {
        return Vec::new();
    };
    ["confirm", "accept", "reject", "wait"]
        .into_iter()
        .map(|action| learning_semantic_resolution_action(proposal_id, action))
        .collect()
}

fn learning_semantic_resolution_action(
    proposal_id: i64,
    action: &str,
) -> LearningSemanticResolutionAction {
    let requires_evidence = action != "wait";
    let control_id = format!("proposal:{proposal_id}:{action}");
    let mut cli_command = vec![
        "soma".to_string(),
        "context".to_string(),
        "review-action".to_string(),
        "--proposal-id".to_string(),
        proposal_id.to_string(),
        "--action".to_string(),
        action.to_string(),
        "--control-id".to_string(),
        control_id.clone(),
    ];
    if requires_evidence {
        cli_command.extend([
            "--verifier".to_string(),
            "<user|test|tool|local_observation|correction>".to_string(),
            "--evidence-kind".to_string(),
            "<kind>".to_string(),
            "--evidence-id".to_string(),
            "<id>".to_string(),
        ]);
    }
    let label = match action {
        "confirm" => "Confirm with independent evidence",
        "accept" => "Accept review candidate with evidence",
        "reject" => "Reject review candidate with evidence",
        "wait" => "Wait for more evidence",
        _ => "Review semantic candidate",
    };
    let trust_effect = match action {
        "wait" => "keeps candidate review-only and records no verification evidence",
        "reject" => "records explicit review rejection evidence without creating an L4 semantic_fact",
        _ => "records review resolution evidence only; any L4 semantic_fact still requires the proposal gate",
    };
    LearningSemanticResolutionAction {
        source: "soma_learning.semantic_resolution_action.v1",
        action: action.to_string(),
        control_id,
        label: label.to_string(),
        requires_evidence,
        cli_command,
        mcp_tool: "soma_review_action",
        evidence_rule:
            "use user/tool/test/local_observation/correction evidence; cloud output, assistant draft, client render text, and unverified claims are forbidden"
                .to_string(),
        trust_effect: trust_effect.to_string(),
        trust_boundary:
            "semantic_resolution_action_template_is_read_only: exposes review-action CLI/MCP templates only; records no verification event, applies no proposal, writes no semantic_fact, and promotes no cloud draft",
    }
}

fn semantic_item_requires_review_only_count(item: &SemanticLearningItem) -> bool {
    matches!(item.action.as_str(), "would_request_verification" | "requested_verification")
        || item.skipped_reason.as_deref() == Some("semantic_review_proposal_already_exists")
}

fn support_claim_row(
    storage: &Storage,
    claim_id: i64,
) -> Result<LearningSupportClaimRow, StorageError> {
    let Some(claim) = storage.claim_record(claim_id)? else {
        return Ok(LearningSupportClaimRow {
            claim_id,
            text_preview: "claim unavailable".to_string(),
            source_type: "missing".to_string(),
            lifecycle_state: "missing".to_string(),
            confidence: 0.0,
            evidence_refs: Vec::new(),
            promotion_reason: None,
            durable_promotion_trust: false,
            verification_event_count: 0,
            trust_status: "missing_claim".to_string(),
            promotion_trust_basis: "missing_claim_cannot_support_l3_l4".to_string(),
        });
    };
    let durable_trust = storage.claim_has_durable_promotion_trust(claim.id)?;
    let verification_event_count = storage.verification_events_for_claim(claim.id)?.len();
    let trust_status = if claim.source_type == ClaimSourceType::CloudDraft && durable_trust {
        "independently_verified_cloud_draft"
    } else if claim.source_type == ClaimSourceType::CloudDraft {
        "blocked_cloud_draft"
    } else if durable_trust {
        "trusted_l3_support"
    } else {
        "needs_verification_or_promotion"
    };
    let promotion_trust_basis = if claim.source_type == ClaimSourceType::CloudDraft && durable_trust
    {
        "independent_verification_event_required_and_present"
    } else if claim.source_type == ClaimSourceType::CloudDraft {
        "cloud_draft_without_independent_verification_forbidden"
    } else if durable_trust {
        "non_cloud_or_verified_claim_has_durable_promotion_trust"
    } else {
        "needs_user_tool_test_local_or_correction_verification"
    };
    let compact_text = claim.text.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(LearningSupportClaimRow {
        claim_id: claim.id,
        text_preview: truncate_chars(&compact_text, 180),
        source_type: claim.source_type.to_string(),
        lifecycle_state: claim.lifecycle_state.to_string(),
        confidence: claim.confidence,
        evidence_refs: claim.evidence_refs.iter().map(stored_evidence_ref_label).collect(),
        promotion_reason: claim.promotion_reason,
        durable_promotion_trust: durable_trust,
        verification_event_count,
        trust_status: trust_status.to_string(),
        promotion_trust_basis: promotion_trust_basis.to_string(),
    })
}

fn support_trust_summary(
    cloud_draft_support_count: usize,
    verified_cloud_draft_support_count: usize,
    blocked_cloud_draft_support_count: usize,
    unverified_support_count: usize,
) -> String {
    if cloud_draft_support_count > 0 {
        return format!(
            "cloud_draft_support={} verified_by_independent_events={} blocked_unverified_cloud_draft={} total_unverified_support={}; cloud draft text alone cannot become L3/L4 evidence",
            cloud_draft_support_count,
            verified_cloud_draft_support_count,
            blocked_cloud_draft_support_count,
            unverified_support_count
        );
    }
    if unverified_support_count > 0 {
        return format!(
            "non_cloud_support_needs_more_verification={}; only durable user/tool/test/local/correction evidence can support L3/L4",
            unverified_support_count
        );
    }
    "all support claims have durable promotion trust; review/apply gates still control L4 writes"
        .to_string()
}

fn candidate_review_guidance(item: &SemanticLearningItem) -> (&'static str, String) {
    if item.action == "would_propose" || item.action == "proposed" {
        return (
            "review_l4_semantic_fact_candidate",
            format!(
                "{}; inspect {} trusted support claims and support diversity before apply",
                item.readiness_score.meaning, item.support_count
            ),
        );
    }
    if item.action == "would_request_verification" || item.action == "requested_verification" {
        return (
            "request_verification_before_l4",
            "review-only semantic signal; it cannot become an L4 semantic_fact without later verification"
                .to_string(),
        );
    }
    match item.skipped_reason.as_deref() {
        Some("durable_promotion_trust_required") => (
            "collect_independent_verification",
            "blocked by trust boundary: support claims need user/tool/test/local/correction verification"
                .to_string(),
        ),
        Some("semantic_fact_already_exists") => (
            "no_l4_action_needed",
            "semantic fact already exists; inspect only if the support evidence looks stale"
                .to_string(),
        ),
        Some("semantic_promotion_proposal_already_exists") => (
            "review_existing_l4_proposal",
            "a semantic promotion proposal already exists; use the review queue instead of duplicating it"
                .to_string(),
        ),
        Some("semantic_review_proposal_already_exists") => (
            "review_existing_verification_request",
            "a verification request already exists; resolve that review item before promoting"
                .to_string(),
        ),
        Some(reason) => ("inspect_semantic_candidate", format!("candidate skipped: {reason}")),
        None => (
            "inspect_semantic_candidate",
            "candidate is visible for review but is not ready for L4 promotion".to_string(),
        ),
    }
}

fn cloud_draft_blocker_row(
    item: &crate::context::review::ClaimReviewItem,
) -> LearningCloudDraftBlockerRow {
    let compact_text = item.claim.text.split_whitespace().collect::<Vec<_>>().join(" ");
    LearningCloudDraftBlockerRow {
        claim_id: item.claim.id,
        task_frame_id: item.claim.task_frame_id,
        text_preview: truncate_chars(&compact_text, 180),
        lifecycle_state: item.claim.lifecycle_state.to_string(),
        confidence: item.claim.confidence,
        evidence_refs: item.claim.evidence_refs.iter().map(stored_evidence_ref_label).collect(),
        verification_event_count: item.verification_events.len(),
        durable_promotion_trust: item.durable_promotion_trust,
        review_reason: item.review_reason.clone(),
        next_action: item.recommended_next_action.clone(),
        cli_hint: item.cli_hint.clone(),
        decision_packet: item.decision_packet.clone(),
        action_options: item.action_options.clone(),
        mcp_tools: item.mcp_tools.clone(),
        accepted_verifier_types: vec!["user", "test", "tool", "local_observation", "correction"],
        trust_boundary:
            "cloud_draft_blocker_is_review_only: this row records no verification and cannot promote to L3/L4 until independent non-cloud evidence is recorded",
    }
}

fn policy_row(section: &ContextSection) -> LearningPolicyRow {
    LearningPolicyRow {
        text: section.text.clone(),
        kind: section.kind.clone(),
        status: section.status.clone(),
        confidence: section.confidence,
        evidence_refs: section.evidence.iter().map(evidence_ref_label).collect(),
    }
}

fn build_review_cards(
    args: &LearningStatusArgs,
    candidates: &[LearningCandidateRow],
    cloud_draft_blockers: &[LearningCloudDraftBlockerRow],
    policy_items: &[LearningPolicyRow],
    belief_items: &[LearningBeliefRow],
) -> Vec<LearningReviewCard> {
    let mut cards = Vec::new();
    cards.extend(
        candidates.iter().take(3).map(|candidate| semantic_candidate_card(args, candidate)),
    );
    cards.extend(cloud_draft_blockers.iter().take(3).map(cloud_draft_blocker_card));
    cards.extend(policy_items.iter().take(3).map(|policy| policy_projection_card(args, policy)));
    cards.extend(belief_items.iter().take(3).map(belief_candidate_card));
    cards.sort_by(|left, right| {
        left.priority.cmp(&right.priority).then_with(|| left.card_id.cmp(&right.card_id))
    });
    cards
}

fn review_card_accepted_verifier_types() -> Vec<&'static str> {
    vec!["user", "test", "tool", "local_observation", "correction"]
}

fn review_card_forbidden_evidence_sources() -> Vec<&'static str> {
    vec!["cloud_draft", "cloud_output_text", "review_render_output", "client_binding_status"]
}

fn semantic_candidate_card(
    args: &LearningStatusArgs,
    candidate: &LearningCandidateRow,
) -> LearningReviewCard {
    let blocks_l4 = candidate.target != "semantic_fact" || candidate.action != "would_propose";
    let status = if candidate.target == "semantic_fact" && candidate.action == "would_propose" {
        "ready_for_manual_l4_review"
    } else {
        "review_only_until_verified"
    };
    let claim_preview = truncate_chars(&candidate.normalized_text, 96);
    let title = if candidate.target == "semantic_fact" {
        format!("Semantic fact candidate: {claim_preview}")
    } else {
        format!("Semantic review-only signal: {claim_preview}")
    };
    let support_claims = format_support_claim_ids(&candidate.support_claim_ids);
    LearningReviewCard {
        source: "soma_learning.review_card.v1",
        card_id: format!("semantic:{}:{}", candidate.target, stable_card_key(&candidate.normalized_text)),
        lane: "l4_semantic_fact_candidates",
        priority: if blocks_l4 { 15 } else { 10 },
        target: candidate.target,
        status: status.to_string(),
        title,
        summary: format!(
            "target={} action={} support={} claims={} trust={} verdict={} bias={} rationale={}",
            candidate.target,
            candidate.action,
            candidate.support_count,
            support_claims,
            candidate.support_trust_summary,
            candidate.readiness_verdict,
            candidate.bias_risk,
            truncate_chars(&candidate.review_rationale, 96)
        ),
        primary_action: candidate.review_action.to_string(),
        primary_command: scoped_command(
            args,
            &[
                "soma",
                "context",
                "semantic-proposals",
                "--dry-run",
                "--brief",
                "--min-support",
            ],
            Some(candidate.support_count.max(2).to_string()),
        ),
        evidence_refs: candidate
            .support_claim_ids
            .iter()
            .map(|id| format!("claim:{id}"))
            .collect(),
        blocks_l4_promotion: blocks_l4,
        projection_path: format!(
            "verified_l3_claims -> manual_review -> l4_{} -> ContextEnvelope",
            candidate.target
        ),
        evidence_rule:
            "requires durable_promotion_trust on repeated L3 support claims plus explicit review/apply gates; cloud output cannot verify itself",
        accepted_verifier_types: review_card_accepted_verifier_types(),
        forbidden_evidence_sources: review_card_forbidden_evidence_sources(),
        trust_boundary:
            "semantic_review_card_is_read_only: shows L4 semantic_fact candidate evidence only; L4 writes still require review/apply gates and verified support",
    }
}

fn format_support_claim_ids(ids: &[i64]) -> String {
    if ids.is_empty() {
        return "none".to_string();
    }
    ids.iter().map(|id| format!("claim:{id}")).collect::<Vec<_>>().join(",")
}

fn cloud_draft_blocker_card(blocker: &LearningCloudDraftBlockerRow) -> LearningReviewCard {
    LearningReviewCard {
        source: "soma_learning.review_card.v1",
        card_id: format!("cloud_draft:claim:{}", blocker.claim_id),
        lane: "cloud_draft_blockers",
        priority: 20,
        target: "cloud_draft",
        status: "blocked_until_independent_verification".to_string(),
        title: "Verify cloud draft before durable memory".to_string(),
        summary: format!(
            "claim #{} lifecycle={} verified_events={} {}",
            blocker.claim_id,
            blocker.lifecycle_state,
            blocker.verification_event_count,
            blocker.text_preview
        ),
        primary_action: "record_independent_verification".to_string(),
        primary_command: shell_words(&blocker.cli_hint),
        evidence_refs: blocker.evidence_refs.clone(),
        blocks_l4_promotion: true,
        projection_path:
            "cloud_draft -> review_queue_blocker -> verified_l3_candidate_only_after_independent_evidence -> L4 gates"
                .to_string(),
        evidence_rule:
            "requires independent user/tool/test/local_observation/correction verification before any L3/L4 promotion; cloud draft text is forbidden as evidence",
        accepted_verifier_types: review_card_accepted_verifier_types(),
        forbidden_evidence_sources: review_card_forbidden_evidence_sources(),
        trust_boundary:
            "cloud_draft_review_card_is_read_only: cloud drafts remain draft claims and cannot become L3/L4 until user/tool/test/local/correction verification exists",
    }
}

fn policy_projection_card(
    args: &LearningStatusArgs,
    policy: &LearningPolicyRow,
) -> LearningReviewCard {
    LearningReviewCard {
        source: "soma_learning.review_card.v1",
        card_id: format!("policy:{}", stable_card_key(&policy.text)),
        lane: "policy_projection",
        priority: 30,
        target: "user_policy",
        status: policy
            .status
            .clone()
            .unwrap_or_else(|| "projecting_to_context_envelope".to_string()),
        title: "Inspect projected user policy".to_string(),
        summary: format!(
            "confidence={} {}",
            policy
                .confidence
                .map(|value| format!("{:.0}%", value * 100.0))
                .unwrap_or_else(|| "unknown".to_string()),
            truncate_chars(&policy.text, 140)
        ),
        primary_action: "inspect_or_correct_policy_projection".to_string(),
        primary_command: scoped_command(args, &["soma", "context", "render", "--format", "json"], None),
        evidence_refs: policy.evidence_refs.clone(),
        blocks_l4_promotion: false,
        projection_path: "verified_policy_or_correction_rows -> ContextEnvelope.user_policy"
            .to_string(),
        evidence_rule:
            "policy projection is inspectable, but stale or conflicting policy changes require explicit user/tool/local/correction evidence",
        accepted_verifier_types: review_card_accepted_verifier_types(),
        forbidden_evidence_sources: review_card_forbidden_evidence_sources(),
        trust_boundary:
            "policy_review_card_is_read_only: projects current user_policy evidence for inspection only; stale policy correction requires explicit evidence",
    }
}

fn belief_candidate_card(belief: &LearningBeliefRow) -> LearningReviewCard {
    let blocks_l4 = belief.triage_status == "needs_resolution";
    LearningReviewCard {
        source: "soma_learning.review_card.v1",
        card_id: format!("belief:{}", belief.id),
        lane: "belief_review",
        priority: belief.review_priority,
        target: "belief",
        status: belief.triage_status.to_string(),
        title: if blocks_l4 {
            "Resolve belief contradiction".to_string()
        } else {
            "Inspect belief audit signal".to_string()
        },
        summary: format!(
            "{} score={:.2} noise={} group_size={} {}",
            belief.kind,
            belief.score,
            belief.noise_risk,
            belief.group_candidate_count,
            truncate_chars(&belief.claim_preview, 120)
        ),
        primary_action: belief.resolution_action.action.clone(),
        primary_command: belief.resolution_action.cli_command.clone(),
        evidence_refs: vec![
            format!("episode:{}", belief.episode_a_id),
            format!("episode:{}", belief.episode_b_id),
        ],
        blocks_l4_promotion: blocks_l4,
        projection_path:
            "l2_belief_signal -> correction_or_explicit_review -> possible user_policy/correction/semantic_fact projection"
                .to_string(),
        evidence_rule:
            "belief signals stay L2/review-only until user/tool/local correction or explicit review evidence resolves them; low-value command noise should not become L4",
        accepted_verifier_types: review_card_accepted_verifier_types(),
        forbidden_evidence_sources: review_card_forbidden_evidence_sources(),
        trust_boundary:
            "belief_review_card_is_read_only: belief candidates remain L2/review-only until user/tool/local correction or explicit review evidence resolves them",
    }
}

fn belief_row(
    storage: &Storage,
    candidate: &BeliefCandidate,
) -> Result<LearningBeliefRow, StorageError> {
    let episode_a = storage.get_live_episode(candidate.episode_a_id)?;
    let episode_b = storage.get_live_episode(candidate.episode_b_id)?;
    let triage =
        belief_candidate_triage_from_episodes(candidate, episode_a.as_ref(), episode_b.as_ref());
    let (review_action, review_rationale) =
        belief_review_guidance(candidate.kind, triage.triage_status);
    let resolution_action =
        belief_resolution_action(candidate, &triage, episode_a.as_ref(), episode_b.as_ref());
    let (project, session_id) = belief_shared_scope(episode_a.as_ref(), episode_b.as_ref());
    let claim_preview = belief_claim_hint(episode_a.as_ref(), episode_b.as_ref());
    Ok(LearningBeliefRow {
        id: candidate.id,
        kind: candidate.kind.to_string(),
        score: candidate.score,
        evidence: candidate.evidence.clone(),
        claim_preview,
        project,
        session_id,
        triage_status: triage.triage_status,
        noise_risk: triage.noise_risk,
        review_priority: triage.review_priority,
        triage_reason: triage.triage_reason,
        episode_a_id: candidate.episode_a_id,
        episode_b_id: candidate.episode_b_id,
        episode_a_preview: episode_preview(episode_a.as_ref()),
        episode_b_preview: episode_preview(episode_b.as_ref()),
        episode_a_outcome: episode_outcome(episode_a.as_ref()),
        episode_b_outcome: episode_outcome(episode_b.as_ref()),
        group_candidate_count: 1,
        grouped_candidate_ids: vec![candidate.id],
        hidden_duplicate_count: 0,
        status: "review_only",
        promotion_rule:
            "requires user/tool/local correction or explicit review before L4 semantic promotion",
        review_action,
        review_rationale,
        resolution_action,
    })
}

fn group_learning_belief_rows(items: Vec<LearningBeliefRow>) -> Vec<LearningBeliefRow> {
    let mut groups: Vec<LearningBeliefRow> = Vec::new();
    let mut group_index: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        let key = learning_belief_group_key(&item);
        if let Some(index) = group_index.get(&key).copied() {
            let group = &mut groups[index];
            group.group_candidate_count += 1;
            group.hidden_duplicate_count += 1;
            group.grouped_candidate_ids.push(item.id);
            group.score = group.score.max(item.score);
            continue;
        }
        group_index.insert(key, groups.len());
        groups.push(item);
    }
    groups
}

fn empty_learning_belief_review_summary() -> LearningBeliefReviewSummary {
    LearningBeliefReviewSummary {
        source: "soma_learning.belief_review_summary.v1",
        status: "clear",
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
            "belief_review_summary_is_read_only: derived from unresolved belief candidates only; records no correction, creates no verification event, writes no semantic_fact, and promotes no cloud draft",
    }
}

fn learning_belief_review_summary(items: &[LearningBeliefRow]) -> LearningBeliefReviewSummary {
    let mut summary = empty_learning_belief_review_summary();
    summary.review_group_count = items.len();
    for item in items {
        let candidate_count = item.group_candidate_count.max(1);
        summary.raw_candidate_count += candidate_count;
        summary.hidden_duplicate_count += item.hidden_duplicate_count;
        match item.triage_status {
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
        .map(|item| item.id);

    if summary.substantive_contradiction_group_count > 0 {
        summary.status = "substantive_resolution_required";
        summary.next_action = format!(
            "Resolve {} substantive contradiction group(s) before L4 belief/policy extraction; {} low-value command-noise candidate(s) stay visible as de-prioritized L2 audit evidence.",
            summary.substantive_contradiction_group_count, summary.noise_candidate_count
        );
    } else if summary.noise_candidate_count > 0 {
        summary.status = "noise_triage_only";
        summary.next_action = format!(
            "Inspect {} low-value command-noise candidate(s) only if the command outcome matters; otherwise keep them as L2 audit evidence and avoid L4 promotion.",
            summary.noise_candidate_count
        );
    } else if summary.raw_candidate_count > 0 {
        summary.status = "support_signal_review";
        summary.next_action =
            "Inspect support/context signals as evidence hints; they cannot become L4 facts or policy without independent verification."
                .to_string();
    }

    summary
}

fn learning_belief_group_key(item: &LearningBeliefRow) -> String {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct BeliefTriage {
    triage_status: &'static str,
    noise_risk: &'static str,
    review_priority: u8,
    triage_reason: String,
}

fn belief_candidate_triage(
    storage: &Storage,
    candidate: &BeliefCandidate,
) -> Result<BeliefTriage, StorageError> {
    let episode_a = storage.get_live_episode(candidate.episode_a_id)?;
    let episode_b = storage.get_live_episode(candidate.episode_b_id)?;
    Ok(belief_candidate_triage_from_episodes(candidate, episode_a.as_ref(), episode_b.as_ref()))
}

fn belief_candidate_triage_from_episodes(
    candidate: &BeliefCandidate,
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> BeliefTriage {
    match candidate.kind {
        BeliefKind::Contradicts if low_information_terminal_pair(episode_a, episode_b) => {
            BeliefTriage {
                triage_status: "low_value_conflict",
                noise_risk: "high",
                review_priority: 20,
                triage_reason:
                    "low-information terminal command flapped between outcomes; kept as unresolved L2 evidence but ranked after substantive contradictions"
                        .to_string(),
            }
        }
        BeliefKind::Contradicts => BeliefTriage {
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
            BeliefTriage {
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
                && low_information_terminal_pair(episode_a, episode_b) =>
        {
            BeliefTriage {
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
            BeliefTriage {
                triage_status: "context_signal",
                noise_risk: "medium",
                review_priority: 50,
                triage_reason:
                    "semantic similarity can suggest related evidence, but it needs explicit review before L4 promotion"
                        .to_string(),
            }
        }
        BeliefKind::Corroborates => BeliefTriage {
            triage_status: "review_only_signal",
            noise_risk: "medium",
            review_priority: 60,
            triage_reason:
                "corroboration is visible for operator review and cannot promote without user/tool/local evidence"
                    .to_string(),
        },
    }
}

fn low_information_terminal_pair(
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> bool {
    let Some(episode_a) = episode_a else { return false };
    let Some(episode_b) = episode_b else { return false };
    matches!(&episode_a.source, EpisodeSource::Terminal)
        && matches!(&episode_b.source, EpisodeSource::Terminal)
        && low_information_command(episode_a.command.as_deref())
        && low_information_command(episode_b.command.as_deref())
}

fn low_information_command(command: Option<&str>) -> bool {
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

fn belief_review_guidance(
    kind: BeliefKind,
    triage_status: &'static str,
) -> (&'static str, &'static str) {
    if triage_status == "low_value_conflict" {
        return (
            "inspect_low_value_conflict",
            "low-information command flapping is L2 audit evidence; inspect it only when the command outcome matters",
        );
    }
    match kind {
        BeliefKind::Contradicts => (
            "resolve_or_record_correction",
            "contradictions are L2 unresolved signals until user/tool/local evidence resolves them",
        ),
        BeliefKind::Corroborates => (
            "inspect_before_semantic_promotion",
            "corroborations can support later L4 review but are not facts by themselves",
        ),
    }
}

fn belief_resolution_action(
    candidate: &BeliefCandidate,
    triage: &BeliefTriage,
    episode_a: Option<&StoredEpisode>,
    episode_b: Option<&StoredEpisode>,
) -> LearningBeliefResolutionAction {
    let (project, session_id) = belief_shared_scope(episode_a, episode_b);
    match candidate.kind {
        BeliefKind::Contradicts => {
            let claim_hint = belief_claim_hint(episode_a, episode_b);
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
                    "format": "json",
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
                return LearningBeliefResolutionAction {
                    source: "soma_learning.belief_resolution_action.v1",
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
                        "learning_belief_resolution_action_is_inspection_only: no correction, verification event, semantic_fact write, or cloud-draft promotion",
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
                "correction": "<current truth>",
            });
            if let Some(project) = project {
                mcp_arguments["project"] = json!(project);
            }
            if let Some(session_id) = session_id {
                mcp_arguments["session_id"] = json!(session_id);
            }
            LearningBeliefResolutionAction {
                source: "soma_learning.belief_resolution_action.v1",
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
                    "learning_belief_resolution_action_is_guidance_only: soma learning is read-only and records no correction, verification event, semantic_fact write, or cloud-draft promotion",
            }
        }
        BeliefKind::Corroborates => LearningBeliefResolutionAction {
            source: "soma_learning.belief_resolution_action.v1",
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
                "format": "json",
            }),
            evidence_rule:
                "corroboration is support for review only; L4 promotion still needs repeated verified L3 evidence or explicit correction/policy evidence"
                    .to_string(),
            trust_effect:
                "no mutation; keep as an L2 support signal until a separate verified proposal or correction path uses it"
                    .to_string(),
            trust_boundary:
                "learning_belief_resolution_action_is_inspection_only: no correction, verification event, semantic_fact write, or cloud-draft promotion",
        },
    }
}

fn belief_shared_scope(
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

fn belief_claim_hint(
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
    belief_episode_text(episode_b)
        .or_else(|| belief_episode_text(episode_a))
        .unwrap_or_else(|| "<stale claim or command>".to_string())
}

fn belief_episode_text(episode: Option<&StoredEpisode>) -> Option<String> {
    let episode = episode?;
    let text = episode
        .digest
        .as_deref()
        .or(episode.command.as_deref())
        .or(episode.prompt_text.as_deref())
        .or(episode.response_text.as_deref())?;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!compact.is_empty()).then(|| truncate_chars(&compact, 160))
}

fn episode_preview(episode: Option<&StoredEpisode>) -> String {
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
    let preview = truncate_chars(&compact, 160);
    let source = episode.source.to_string();
    let project = episode.project.as_deref().unwrap_or("unknown-project");
    let session = episode.session_id.as_deref().unwrap_or("unknown-session");
    format!("{preview} [source={source} project={project} session={session}]")
}

fn episode_outcome(episode: Option<&StoredEpisode>) -> String {
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
            parts.push(format!("stdout_preview={}", truncate_chars(&compact, 120)));
        }
    }
    parts.join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn stable_card_key(value: &str) -> String {
    let mut key = String::new();
    let mut previous_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            key.push(ch);
            previous_dash = false;
        } else if !previous_dash && !key.is_empty() {
            key.push('-');
            previous_dash = true;
        }
        if key.len() >= 64 {
            break;
        }
    }
    while key.ends_with('-') {
        key.pop();
    }
    if key.is_empty() {
        "item".to_string()
    } else {
        key
    }
}

fn shell_words(value: &str) -> Vec<String> {
    value.split_whitespace().filter(|part| !part.is_empty()).map(ToString::to_string).collect()
}

fn evidence_ref_label(reference: &EvidenceRef) -> String {
    match &reference.source {
        Some(source) => format!("{}:{} ({source})", reference.kind, reference.id),
        None => format!("{}:{}", reference.kind, reference.id),
    }
}

fn stored_evidence_ref_label(reference: &StoredEvidenceRef) -> String {
    match &reference.source {
        Some(source) => format!("{}:{} ({source})", reference.kind, reference.id),
        None => format!("{}:{}", reference.kind, reference.id),
    }
}

fn target_coverage() -> Vec<LearningTargetCoverage> {
    vec![
        LearningTargetCoverage {
            target: "semantic_fact",
            status: "supported",
            rule: "repeated_verified_l3_claims",
            note: "previewed here; apply still requires review/proposal gates",
        },
        LearningTargetCoverage {
            target: "user_policy",
            status: "supported_projection",
            rule: "deterministic_policy_rows_or_semantic_proxy_user_policy",
            note: "projects into ContextEnvelope.user_policy; distinct from semantic_fact claim consolidation",
        },
        LearningTargetCoverage {
            target: "belief",
            status: "review_only_candidate_graph",
            rule: "belief_candidates_corroborates_contradicts",
            note: "visible as conflict/corroboration evidence; does not become L4 fact or policy without review",
        },
    ]
}

fn promotion_matrix(
    summary: &LearningStatusSummary,
    belief_review_summary: &LearningBeliefReviewSummary,
    lanes: &[LearningReviewLane],
    review_surface: &LearningReviewSurface,
) -> Vec<LearningPromotionMatrixRow> {
    let semantic_lane = lane_by_name(lanes, "l4_semantic_fact_candidates");
    let cloud_lane = lane_by_name(lanes, "cloud_draft_blockers");
    let policy_lane = lane_by_name(lanes, "policy_projection");
    let belief_lane = lane_by_name(lanes, "belief_review");
    let storage_unavailable = lane_by_name(lanes, "storage").is_some();

    vec![
        LearningPromotionMatrixRow {
            source: "soma_learning.promotion_matrix.v1",
            target: "semantic_fact",
            lane: "l4_semantic_fact_candidates",
            status: if storage_unavailable {
                "unknown_storage_unavailable"
            } else if summary.l4_candidate_count > 0 {
                "ready_for_manual_l4_review"
            } else if summary.review_only_candidate_count > 0 {
                "review_only_until_more_verified_support"
            } else {
                "empty"
            },
            candidate_count: summary.l4_candidate_count.max(summary.review_only_candidate_count),
            ready_for_manual_l4_review: summary.l4_candidate_count > 0 && !storage_unavailable,
            context_projection_ready: false,
            blocks_l4_promotion: storage_unavailable || summary.review_only_candidate_count > 0,
            projected_context_section: Some("ContextEnvelope.relevant_memory"),
            required_evidence:
                "repeated durable L3 support claims with user/tool/test/local/correction verification; review/apply gate still required",
            next_action: semantic_lane
                .map(|lane| lane.next_action.clone())
                .unwrap_or_else(|| review_surface.report_command.join(" ")),
            primary_command: semantic_lane
                .map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.report_command.clone()),
            trust_boundary:
                "promotion_matrix_row_is_read_only: summarizes semantic_fact readiness only; records no proposal, creates no verification event, writes no L4 fact, and promotes no cloud draft",
        },
        LearningPromotionMatrixRow {
            source: "soma_learning.promotion_matrix.v1",
            target: "cloud_draft",
            lane: "cloud_draft_blockers",
            status: if storage_unavailable {
                "unknown_storage_unavailable"
            } else if summary.cloud_draft_blocked_count > 0 {
                "blocked_until_independent_verification"
            } else {
                "clear"
            },
            candidate_count: summary.cloud_draft_blocked_count,
            ready_for_manual_l4_review: false,
            context_projection_ready: false,
            blocks_l4_promotion: storage_unavailable || summary.cloud_draft_blocked_count > 0,
            projected_context_section: None,
            required_evidence:
                "independent user/tool/test/local_observation/correction verification; cloud output text and client render status are forbidden evidence",
            next_action: cloud_lane
                .map(|lane| lane.next_action.clone())
                .unwrap_or_else(|| review_surface.render_plan_command.join(" ")),
            primary_command: cloud_lane
                .map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.render_plan_command.clone()),
            trust_boundary:
                "promotion_matrix_cloud_draft_row_is_read_only: cloud drafts remain draft claims until independent evidence exists; this row cannot verify or promote them",
        },
        LearningPromotionMatrixRow {
            source: "soma_learning.promotion_matrix.v1",
            target: "user_policy",
            lane: "policy_projection",
            status: if storage_unavailable {
                "unknown_storage_unavailable"
            } else if summary.policy_projection_count > 0 {
                "projecting_to_context_envelope"
            } else {
                "empty"
            },
            candidate_count: summary.policy_projection_count,
            ready_for_manual_l4_review: false,
            context_projection_ready: summary.policy_projection_count > 0 && !storage_unavailable,
            blocks_l4_promotion: storage_unavailable,
            projected_context_section: Some("ContextEnvelope.user_policy"),
            required_evidence:
                "deterministic policy/correction rows or semantic proxy user_policy evidence; stale policy needs explicit correction evidence",
            next_action: policy_lane
                .map(|lane| lane.next_action.clone())
                .unwrap_or_else(|| review_surface.report_command.join(" ")),
            primary_command: policy_lane
                .map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.report_command.clone()),
            trust_boundary:
                "promotion_matrix_policy_row_is_read_only: mirrors user_policy projection readiness only; records no correction, verification event, or L4 write",
        },
        LearningPromotionMatrixRow {
            source: "soma_learning.promotion_matrix.v1",
            target: "belief",
            lane: "belief_review",
            status: if storage_unavailable {
                "unknown_storage_unavailable"
            } else if belief_review_summary.substantive_contradiction_group_count > 0 {
                "resolution_required_before_l4_extraction"
            } else if belief_review_summary.noise_candidate_count > 0 {
                "audit_only_l2_noise"
            } else if belief_review_summary.raw_candidate_count > 0 {
                "review_only_signal"
            } else {
                "empty"
            },
            candidate_count: summary.belief_candidate_count,
            ready_for_manual_l4_review: false,
            context_projection_ready: false,
            blocks_l4_promotion: storage_unavailable
                || belief_review_summary.substantive_contradiction_group_count > 0,
            projected_context_section: None,
            required_evidence:
                "user/tool/local correction or explicit review evidence; low-value command noise must stay L2 audit evidence",
            next_action: belief_lane
                .map(|lane| lane.next_action.clone())
                .unwrap_or_else(|| review_surface.digest_command.join(" ")),
            primary_command: belief_lane
                .map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.digest_command.clone()),
            trust_boundary:
                "promotion_matrix_belief_row_is_read_only: belief signals remain L2/review-only until explicit evidence resolves them; writes no semantic_fact or policy",
        },
    ]
}

fn build_operator_card(
    summary: &LearningStatusSummary,
    lanes: &[LearningReviewLane],
    review_surface: &LearningReviewSurface,
) -> LearningOperatorCard {
    let lanes_requiring_review = lanes
        .iter()
        .filter(|lane| {
            lane.count > 0
                && lane.status != "empty"
                && lane.status != "clear"
                && lane.status != "audit_only"
        })
        .map(|lane| lane.lane.to_string())
        .collect::<Vec<_>>();

    let mut safe_to_claim = Vec::new();
    if summary.l4_candidate_count > 0 {
        safe_to_claim.push(format!(
            "{} repeated verified L3 group(s) are visible as L4 semantic candidates for review.",
            summary.l4_candidate_count
        ));
    }
    if summary.policy_projection_count > 0 {
        safe_to_claim.push(format!(
            "{} user_policy projection row(s) are visible in the ContextEnvelope preview.",
            summary.policy_projection_count
        ));
    }
    let belief_learning_blocker = summary.belief_substantive_contradiction_count > 0;
    let belief_audit_only = summary.belief_candidate_count > 0 && !belief_learning_blocker;

    if summary.pending_review_item_count == 0
        && summary.cloud_draft_blocked_count == 0
        && summary.l4_candidate_count == 0
        && summary.review_only_candidate_count == 0
        && !belief_learning_blocker
    {
        if belief_audit_only {
            safe_to_claim.push(
                "No blocking L3/L4 semantic learning review work is visible for this scope."
                    .to_string(),
            );
        } else {
            safe_to_claim
                .push("No semantic learning review work is visible for this scope.".to_string());
        }
    }

    let mut blocked_claims = Vec::new();
    if summary.cloud_draft_blocked_count > 0 {
        blocked_claims.push(format!(
            "{} cloud_draft claim(s) cannot become durable memory without user/tool/test/local/correction verification.",
            summary.cloud_draft_blocked_count
        ));
    }
    if summary.review_only_candidate_count > 0 {
        blocked_claims.push(format!(
            "{} semantic candidate group(s) are review-only until additional verification resolves them.",
            summary.review_only_candidate_count
        ));
    }
    if belief_learning_blocker {
        blocked_claims.push(format!(
            "{} substantive belief contradiction candidate(s) must be resolved before L4 belief/policy extraction.",
            summary.belief_substantive_contradiction_count
        ));
    }
    if belief_audit_only {
        safe_to_claim.push(format!(
            "{} belief candidate(s) are isolated as de-prioritized L2 audit evidence, not L4 learning blockers.",
            summary.belief_candidate_count
        ));
    }

    let (status, headline, primary_next_step, primary_next_command) = if summary
        .cloud_draft_blocked_count
        > 0
    {
        (
                "cloud_draft_blocked".to_string(),
                format!(
                    "{} cloud_draft claim(s) need independent verification before L3/L4 learning.",
                    summary.cloud_draft_blocked_count
                ),
                "Render review controls and verify with user/tool/test/local/correction evidence before any durable promotion.".to_string(),
                review_surface.render_plan_command.clone(),
            )
    } else if summary.l4_candidate_count > 0 {
        let lane = lane_by_name(lanes, "l4_semantic_fact_candidates");
        (
            "l4_review_ready".to_string(),
            format!(
                "{} semantic_fact candidate(s) are ready for manual L4 review.",
                summary.l4_candidate_count
            ),
            lane.map(|lane| lane.next_action.clone()).unwrap_or_else(|| {
                "Review repeated verified L3 support before any L4 write.".to_string()
            }),
            lane.map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.report_command.clone()),
        )
    } else if summary.review_only_candidate_count > 0 {
        let lane = lane_by_name(lanes, "l4_semantic_fact_candidates");
        (
            "semantic_review_only_pending".to_string(),
            format!(
                "{} semantic candidate group(s) need verification or correction before L4 learning.",
                summary.review_only_candidate_count
            ),
            lane.map(|lane| lane.next_action.clone()).unwrap_or_else(|| {
                "Request user/tool/test/local/correction verification before any L4 semantic_fact write."
                    .to_string()
            }),
            lane.map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.report_command.clone()),
        )
    } else if belief_learning_blocker {
        let lane = lane_by_name(lanes, "belief_review");
        (
            "belief_review_pending".to_string(),
            format!(
                "{} substantive belief contradiction candidate(s) collapse into {} review group(s) before policy/fact extraction.",
                summary.belief_substantive_contradiction_count,
                summary.belief_group_count
            ),
            lane.map(|lane| lane.next_action.clone()).unwrap_or_else(|| {
                "Resolve substantive contradictions before treating beliefs as stable.".to_string()
            }),
            lane.map(|lane| lane.command.clone())
                .unwrap_or_else(|| review_surface.digest_command.clone()),
        )
    } else if summary.belief_candidate_count > 0 {
        (
            "noise_triage_only".to_string(),
            format!(
                "{} belief candidate(s) remain as L2 audit evidence without blocking L4 learning.",
                summary.belief_candidate_count
            ),
            "Inspect low-value command noise only if a command outcome matters; otherwise keep it out of L4 promotion.".to_string(),
            review_surface.digest_command.clone(),
        )
    } else if summary.pending_review_item_count > 0 || summary.should_interrupt {
        (
            "review_pending".to_string(),
            format!(
                "{} semantic review item(s) are pending in the review queue.",
                summary.pending_review_item_count
            ),
            review_surface.primary_reason.clone(),
            review_surface.render_plan_command.clone(),
        )
    } else {
        (
            "clear".to_string(),
            "No semantic learning review blocker is visible for this scope.".to_string(),
            "Run `soma learning --json` again after new verified evidence or corrections arrive."
                .to_string(),
            vec!["soma".to_string(), "learning".to_string(), "--json".to_string()],
        )
    };
    let (operator_next_action_id, operator_next_action_label) =
        learning_operator_next_action(&status);

    LearningOperatorCard {
        source: "soma_learning.operator_card.v1",
        status,
        operator_next_action_id,
        operator_next_action_label,
        headline,
        primary_next_step,
        primary_next_command,
        review_surface: review_surface.primary_surface,
        review_counts: LearningOperatorReviewCounts {
            l4_semantic_fact_candidates: summary.l4_candidate_count,
            review_only_semantic_candidates: summary.review_only_candidate_count,
            cloud_draft_blockers: summary.cloud_draft_blocked_count,
            policy_projections: summary.policy_projection_count,
            belief_candidates: summary.belief_candidate_count,
            belief_groups: summary.belief_group_count,
            belief_hidden_duplicates: summary.belief_hidden_duplicate_count,
            belief_contradictions: summary.belief_contradiction_count,
            belief_substantive_contradictions: summary.belief_substantive_contradiction_count,
            belief_low_value_conflicts: summary.belief_low_value_conflict_count,
            belief_low_value_noise: summary.belief_low_value_noise_count,
            belief_noise_candidates: summary.belief_noise_candidate_count,
            pending_review_items: summary.pending_review_item_count,
            ready_to_apply_proposals: summary.ready_proposal_count,
            manual_l4_review_items: summary.manual_l4_review_count,
            should_interrupt: summary.should_interrupt,
        },
        lanes_requiring_review,
        safe_to_claim,
        blocked_claims,
        trust_boundary: "read_only_learning_operator_card: summarizes semantic review state only; records no proof, creates no verification event, applies no proposal, writes no semantic_fact, and promotes no cloud draft",
    }
}

fn lane_by_name<'a>(lanes: &'a [LearningReviewLane], name: &str) -> Option<&'a LearningReviewLane> {
    lanes.iter().find(|lane| lane.lane == name)
}

fn review_lanes(
    args: &LearningStatusArgs,
    summary: &LearningStatusSummary,
    client: &str,
) -> Vec<LearningReviewLane> {
    let mut lanes = vec![
        LearningReviewLane {
            lane: "l4_semantic_fact_candidates",
            priority: 1,
            status: if summary.l4_candidate_count > 0 {
                "ready_for_manual_review"
            } else if summary.review_only_candidate_count > 0 {
                "review_only_until_verified"
            } else {
                "empty"
            },
            count: summary.l4_candidate_count.max(summary.review_only_candidate_count),
            next_action: if summary.l4_candidate_count > 0 {
                "Review repeated verified L3 support, then dry-run apply-ready before any L4 write."
                    .to_string()
            } else if summary.review_only_candidate_count > 0 {
                "Request user/tool/test/local/correction verification or correction before any L4 semantic_fact write."
                    .to_string()
            } else {
                "No repeated verified L3 semantic_fact candidate currently meets the support gate."
                    .to_string()
            },
            trust_boundary:
                "semantic_fact lane is read-only here; L4 writes still require review/apply gates",
            command: scoped_command(
                args,
                &[
                    "soma",
                    "context",
                    "semantic-proposals",
                    "--dry-run",
                    "--brief",
                    "--min-support",
                ],
                Some(args.min_support.max(2).to_string()),
            ),
        },
        LearningReviewLane {
            lane: "cloud_draft_blockers",
            priority: 2,
            status: if summary.cloud_draft_blocked_count > 0 {
                "blocked_until_independent_verification"
            } else {
                "clear"
            },
            count: summary.cloud_draft_blocked_count,
            next_action: if summary.cloud_draft_blocked_count > 0 {
                "Verify with user/tool/test/local/correction evidence; do not use cloud output as evidence for itself."
                    .to_string()
            } else {
                "No unverified cloud_draft claim is currently blocking review in this scope."
                    .to_string()
            },
            trust_boundary:
                "cloud_draft lane cannot promote to L3/L4 without independent non-cloud verification",
            command: scoped_command(args, &["soma", "context", "review-report"], None),
        },
        LearningReviewLane {
            lane: "policy_projection",
            priority: 3,
            status: if summary.policy_projection_count > 0 {
                "projecting_to_context_envelope"
            } else {
                "empty"
            },
            count: summary.policy_projection_count,
            next_action: if summary.policy_projection_count > 0 {
                "Inspect projected user_policy rows and correct stale policy before relying on it."
                    .to_string()
            } else {
                "No policy projection is currently visible in this scope.".to_string()
            },
            trust_boundary:
                "policy lane is a projection preview; corrections still require explicit evidence",
            command: scoped_command(args, &["soma", "context", "render", "--format", "json"], None),
        },
        LearningReviewLane {
            lane: "belief_review",
            priority: 4,
            status: if summary.belief_substantive_contradiction_count > 0 {
                "review_only_signal"
            } else if summary.belief_candidate_count > 0 {
                "audit_only"
            } else {
                "empty"
            },
            count: summary.belief_candidate_count,
            next_action: if summary.belief_substantive_contradiction_count > 0 {
                "Resolve substantive contradictions first; inspect corroborations after triage because low-value command conflicts and pairs are de-prioritized."
                    .to_string()
            } else if summary.belief_candidate_count > 0 {
                "Low-value belief signals are L2 audit evidence; inspect only when the command outcome matters."
                    .to_string()
            } else {
                "No belief candidate currently needs review in this scope.".to_string()
            },
            trust_boundary:
                "belief lane remains L2/review-only until user/tool/local correction or explicit review evidence resolves it",
            command: scoped_client_command(
                args,
                &[
                    "soma",
                    "context",
                    "review-digest",
                    "--include-queue-only",
                    "--format",
                    "json",
                ],
                client,
            ),
        },
    ];
    lanes.sort_by_key(|lane| lane.priority);
    lanes
}

fn review_surface(
    args: &LearningStatusArgs,
    client: &str,
    summary: &LearningStatusSummary,
) -> LearningReviewSurface {
    let (primary_surface, primary_reason) = if summary.cloud_draft_blocked_count > 0 {
        (
            "review_render",
            "render cloud_draft blocker action controls before any verification or L3/L4 promotion",
        )
    } else if summary.review_only_candidate_count > 0 && summary.l4_candidate_count == 0 {
        (
            "semantic_proposals",
            "inspect semantic review-only candidate evidence through semantic-proposals dry-run before L4 promotion",
        )
    } else if summary.should_interrupt {
        (
            "review_digest",
            "show interruptible semantic review digest before expanding to the full workbench",
        )
    } else if summary.belief_substantive_contradiction_count > 0 {
        (
            "review_digest",
            "show compact belief review digest before expanding to the full workbench",
        )
    } else {
        ("review_report", "inspect semantic learning and verification work on demand")
    };

    LearningReviewSurface {
        source: "soma_learning.review_surface",
        client: client.to_string(),
        primary_surface,
        primary_reason: primary_reason.to_string(),
        render_plan_command: scoped_client_command(
            args,
            &["soma", "context", "review-render", "--format", "json"],
            client,
        ),
        report_command: scoped_command(args, &["soma", "context", "review-report", "--format", "json"], None),
        queue_command: scoped_command(args, &["soma", "context", "review-queue", "--format", "json"], None),
        action_plan_command: scoped_command(args, &["soma", "context", "review-actions", "--format", "json"], None),
        digest_command: scoped_client_command(
            args,
            &["soma", "context", "review-digest", "--include-queue-only", "--format", "json"],
            client,
        ),
        proof_session_command: vec![
            "soma".to_string(),
            "adapter-binding-proof".to_string(),
            "--client".to_string(),
            client.to_string(),
            "--proof-session".to_string(),
            "--json".to_string(),
        ],
        mcp_tools: vec![
            "soma_review_digest",
            "soma_review_digest_ack",
            "soma_review_render",
            "soma_review_report",
            "soma_review_queue",
            "soma_review_actions",
            "soma_review_action",
            "soma_client_binding_proof_session",
            "soma_client_binding_record_proof",
        ],
        control_contract:
            "review_surface_requires_soma_review_render_control_binding_manifest_before_client_mutation",
        proof_path:
            "private_client_readiness_requires_observed_in_client_render_then_observed_review_action_with_storage_gated_non_cloud_verification",
        trust_boundary:
            "learning_review_surface_is_read_only: commands and MCP tools are next-step guidance only; this surface records no proof, creates no verification event, promotes no cloud draft, and applies no proposal until the target review-action/proof command is explicitly executed with independent evidence",
    }
}

fn next_commands(args: &LearningStatusArgs, client: &str) -> Vec<Vec<String>> {
    let mut commands = vec![
        scoped_command(
            args,
            &["soma", "context", "semantic-proposals", "--dry-run", "--brief", "--min-support"],
            Some(args.min_support.max(2).to_string()),
        ),
        scoped_client_command(
            args,
            &["soma", "context", "review-digest", "--include-queue-only", "--format", "json"],
            client,
        ),
        scoped_command(args, &["soma", "context", "review-report"], None),
        scoped_command(
            args,
            &["soma", "context", "learning-proposals", "apply-ready", "--dry-run"],
            None,
        ),
    ];
    if args.project.is_none() && args.session_id.is_none() {
        commands.push(vec!["soma".to_string(), "context".to_string(), "trust-audit".to_string()]);
    }
    commands
}

fn scoped_client_command(args: &LearningStatusArgs, base: &[&str], client: &str) -> Vec<String> {
    let mut command = scoped_command(args, base, None);
    command.push("--client".to_string());
    command.push(client.to_string());
    command
}

fn scoped_command(
    args: &LearningStatusArgs,
    base: &[&str],
    trailing: Option<String>,
) -> Vec<String> {
    let mut command = base.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
    if let Some(value) = trailing {
        command.push(value);
    }
    if let Some(project) = &args.project {
        command.push("--project".to_string());
        command.push(project.clone());
    }
    if let Some(session_id) = &args.session_id {
        command.push("--session-id".to_string());
        command.push(session_id.clone());
    }
    command
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped_learning_args() -> LearningStatusArgs {
        LearningStatusArgs {
            status_alias: None,
            project: Some("scope-project".to_string()),
            session_id: Some("scope-session".to_string()),
            client: Some("cursor".to_string()),
            limit: 100,
            min_support: 2,
            candidate_limit: 10,
            review_limit: 10,
            db_path: None,
            dogfood_report: None,
            format: "json".to_string(),
            brief: false,
            json: true,
        }
    }

    fn command_has_pair(command: &[String], flag: &str, value: &str) -> bool {
        command.windows(2).any(|pair| pair[0] == flag && pair[1] == value)
    }

    #[test]
    fn review_card_primary_commands_preserve_explicit_scope() {
        let args = scoped_learning_args();
        let candidate = LearningCandidateRow {
            proposal_id: None,
            target: "semantic_fact",
            action: "would_propose".to_string(),
            normalized_text: "scoped semantic card command stays in the active project".to_string(),
            group_rule: "test".to_string(),
            support_count: 2,
            support_claim_ids: vec![1, 2],
            support_claims: Vec::new(),
            durable_promotion_trust: true,
            trusted: true,
            contains_cloud_draft_support: false,
            cloud_draft_support_count: 0,
            verified_cloud_draft_support_count: 0,
            blocked_cloud_draft_support_count: 0,
            unverified_support_count: 0,
            support_trust_summary: "all support claims have durable promotion trust".to_string(),
            readiness_verdict: "ready_for_manual_l4_review".to_string(),
            bias_risk: "low".to_string(),
            skipped_reason: None,
            review_action: "review_l4_semantic_fact_candidate",
            review_rationale: "test candidate".to_string(),
            resolution_status: "none".to_string(),
            resolution_next_step: "review scoped candidate".to_string(),
            review_queue_command: learning_review_queue_command(&args),
            verification_template_command: vec![
                "soma".to_string(),
                "context".to_string(),
                "verify-claim".to_string(),
                "--claim-id".to_string(),
                "1".to_string(),
            ],
            resolution_actions: Vec::new(),
            resolution_trust_boundary: "test_read_only".to_string(),
        };
        let policy = LearningPolicyRow {
            text: "Prefer scoped review commands when project context is active.".to_string(),
            kind: Some("preference".to_string()),
            status: Some("projecting_to_context_envelope".to_string()),
            confidence: Some(0.9),
            evidence_refs: vec!["episode:1".to_string()],
        };

        let cards = build_review_cards(&args, &[candidate], &[], &[policy], &[]);
        let semantic = cards
            .iter()
            .find(|card| card.lane == "l4_semantic_fact_candidates")
            .expect("semantic card");
        assert!(semantic.primary_command.iter().any(|part| part == "semantic-proposals"));
        assert!(command_has_pair(&semantic.primary_command, "--project", "scope-project"));
        assert!(command_has_pair(&semantic.primary_command, "--session-id", "scope-session"));
        assert_eq!(
            semantic.title,
            "Semantic fact candidate: scoped semantic card command stays in the active project"
        );
        assert!(semantic.summary.contains("target=semantic_fact"), "{}", semantic.summary);
        assert!(semantic.summary.contains("action=would_propose"), "{}", semantic.summary);
        assert!(semantic.summary.contains("claims=claim:1,claim:2"), "{}", semantic.summary);
        assert!(
            semantic.summary.contains("trust=all support claims have durable promotion trust"),
            "{}",
            semantic.summary
        );

        let policy =
            cards.iter().find(|card| card.lane == "policy_projection").expect("policy card");
        assert!(policy.primary_command.iter().any(|part| part == "render"));
        assert!(command_has_pair(&policy.primary_command, "--project", "scope-project"));
        assert!(command_has_pair(&policy.primary_command, "--session-id", "scope-session"));
    }
}
