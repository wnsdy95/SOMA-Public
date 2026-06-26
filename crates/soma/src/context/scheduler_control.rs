//! Operator-controlled scheduler subpasses for trust-boundary work.
//!
//! This module exposes the slow-loop's trust-sensitive review/learning passes as
//! an explicit control surface. It deliberately does not run the whole resident
//! slow loop: merge/training/maintenance remain resident concerns, while these
//! passes already have dry-run and storage-gate semantics suitable for CLI/MCP
//! operators and client integrations.

use serde::Serialize;
use serde_json::{json, Value};

use crate::context::open_decision_review::{
    propose_open_decision_reviews, OpenDecisionProposalInput,
};
use crate::context::review_drain::{drain_review_queue, ReviewDrainInput};
use crate::context::semantic_learning::{propose_semantic_consolidations, SemanticLearningInput};
use crate::storage::{
    ShortTermProxyPromotionRequest, Storage, StorageError, TaskFrameRetentionRequest,
    DEFAULT_TASK_FRAME_RETENTION_DAYS,
};

pub const SCHEDULER_CONTROL_SOURCE: &str = "soma_scheduler_control";
pub const SCHEDULER_CONTROL_POLICY: &str =
    "explicit_scheduler_control_uses_existing_review_and_learning_gates";
pub const SCHEDULER_CONTROL_TRUST_BOUNDARY: &str =
    "scheduler_control_never_creates_verification_events_and_never_bypasses_review_gates";
pub const DEFAULT_L3_DECAY_OLDER_THAN_DAYS: i64 = 90;
pub const DEFAULT_L3_DECAY_MAX_ACCESS_COUNT: i64 = 0;
pub const DEFAULT_L3_DECAY_REASON: &str = "scheduler-control stale low-access L3 proxy";
pub const DEFAULT_L2_PROMOTION_MIN_CONFIDENCE: f32 = 0.90;
pub const DEFAULT_L2_PROMOTION_ANOMALY_MIN_CONFIDENCE: f32 = 0.85;
pub const DEFAULT_L2_PROMOTION_MIN_REPEATED_SUPPORT: usize = 2;
pub const DEFAULT_L2_PROMOTION_REASON: &str = "scheduler-control L2 proxy promotion";
pub const DEFAULT_TASK_FRAME_RETENTION_REASON: &str =
    "scheduler-control unreferenced TaskFrame retention cleanup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerControlPass {
    OpenDecisionProposals,
    SemanticProposals,
    ReviewDrain,
    L2Promote,
    L3Decay,
    TaskFrameRetention,
}

impl SchedulerControlPass {
    pub const fn as_str(self) -> &'static str {
        match self {
            SchedulerControlPass::OpenDecisionProposals => "open_decision_proposals",
            SchedulerControlPass::SemanticProposals => "semantic_proposals",
            SchedulerControlPass::ReviewDrain => "review_drain",
            SchedulerControlPass::L2Promote => "l2_promote",
            SchedulerControlPass::L3Decay => "l3_decay",
            SchedulerControlPass::TaskFrameRetention => "task_frame_retention",
        }
    }

    fn from_str(input: &str) -> Result<Self, String> {
        match input.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "open_decision" | "open_decision_proposals" | "open_decisions" => {
                Ok(Self::OpenDecisionProposals)
            }
            "semantic" | "semantic_learning" | "semantic_proposals" => Ok(Self::SemanticProposals),
            "drain" | "review_drain" | "safe_review_drain" => Ok(Self::ReviewDrain),
            "l2_promote" | "l2_promotion" | "short_term_promote" => Ok(Self::L2Promote),
            "l3_decay" | "long_term_decay" | "memory_decay" => Ok(Self::L3Decay),
            "task_frame_retention" | "task_frames_retention" | "retention" => {
                Ok(Self::TaskFrameRetention)
            }
            other => Err(format!(
                "unknown scheduler pass `{other}`; expected all, open_decision_proposals, semantic_proposals, review_drain, l2_promote, l3_decay, or task_frame_retention"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerControlInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub semantic_min_support: usize,
    pub l2_promotion_min_confidence: f32,
    pub l2_promotion_anomaly_min_confidence: f32,
    pub l2_promotion_min_repeated_support: usize,
    pub l2_promotion_reason: String,
    pub l3_decay_cutoff_ns: i64,
    pub l3_decay_max_access_count: i64,
    pub l3_decay_reason: String,
    pub task_frame_retention_cutoff_ns: i64,
    pub task_frame_retention_days: i64,
    pub task_frame_retention_reason: String,
    pub dry_run: bool,
    pub passes: Vec<SchedulerControlPass>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SchedulerControlReport {
    pub source: String,
    pub policy: String,
    pub trust_boundary: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub semantic_min_support: usize,
    pub l2_promotion_min_confidence: f32,
    pub l2_promotion_anomaly_min_confidence: f32,
    pub l2_promotion_min_repeated_support: usize,
    pub l2_promotion_reason: String,
    pub l3_decay_cutoff_ns: i64,
    pub l3_decay_max_access_count: i64,
    pub l3_decay_reason: String,
    pub task_frame_retention_cutoff_ns: i64,
    pub task_frame_retention_days: i64,
    pub task_frame_retention_reason: String,
    pub dry_run: bool,
    pub pass_count: usize,
    pub passes: Vec<SchedulerControlPassReport>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SchedulerControlPassReport {
    pub pass: String,
    pub status: String,
    pub mutation_boundary: String,
    pub summary: Value,
    pub report: Value,
}

pub fn normalize_scheduler_control_passes(
    raw_passes: &[String],
) -> Result<Vec<SchedulerControlPass>, String> {
    if raw_passes.is_empty()
        || raw_passes.iter().any(|pass| pass.trim().eq_ignore_ascii_case("all"))
    {
        return Ok(default_scheduler_control_passes());
    }
    let mut passes = Vec::new();
    for raw in raw_passes {
        let pass = SchedulerControlPass::from_str(raw)?;
        if !passes.contains(&pass) {
            passes.push(pass);
        }
    }
    Ok(passes)
}

pub fn default_scheduler_control_passes() -> Vec<SchedulerControlPass> {
    vec![
        SchedulerControlPass::OpenDecisionProposals,
        SchedulerControlPass::SemanticProposals,
        SchedulerControlPass::ReviewDrain,
    ]
}

pub fn run_scheduler_control(
    storage: &mut Storage,
    input: SchedulerControlInput,
) -> Result<SchedulerControlReport, StorageError> {
    let limit = input.limit.max(1);
    let semantic_min_support = input.semantic_min_support.max(2);
    let l2_promotion_min_repeated_support = input.l2_promotion_min_repeated_support.max(2);
    let passes =
        if input.passes.is_empty() { default_scheduler_control_passes() } else { input.passes };
    let mut pass_reports = Vec::with_capacity(passes.len());
    for pass in passes {
        pass_reports.push(run_scheduler_pass(
            storage,
            pass,
            input.project.clone(),
            input.session_id.clone(),
            limit,
            semantic_min_support,
            input.l2_promotion_min_confidence,
            input.l2_promotion_anomaly_min_confidence,
            l2_promotion_min_repeated_support,
            input.l2_promotion_reason.clone(),
            input.l3_decay_cutoff_ns,
            input.l3_decay_max_access_count,
            input.l3_decay_reason.clone(),
            input.task_frame_retention_cutoff_ns,
            input.task_frame_retention_days,
            input.task_frame_retention_reason.clone(),
            input.dry_run,
        )?);
    }

    Ok(SchedulerControlReport {
        source: SCHEDULER_CONTROL_SOURCE.to_string(),
        policy: SCHEDULER_CONTROL_POLICY.to_string(),
        trust_boundary: SCHEDULER_CONTROL_TRUST_BOUNDARY.to_string(),
        project: input.project,
        session_id: input.session_id,
        limit,
        semantic_min_support,
        l2_promotion_min_confidence: input.l2_promotion_min_confidence,
        l2_promotion_anomaly_min_confidence: input.l2_promotion_anomaly_min_confidence,
        l2_promotion_min_repeated_support,
        l2_promotion_reason: input.l2_promotion_reason,
        l3_decay_cutoff_ns: input.l3_decay_cutoff_ns,
        l3_decay_max_access_count: input.l3_decay_max_access_count,
        l3_decay_reason: input.l3_decay_reason,
        task_frame_retention_cutoff_ns: input.task_frame_retention_cutoff_ns,
        task_frame_retention_days: input.task_frame_retention_days,
        task_frame_retention_reason: input.task_frame_retention_reason,
        dry_run: input.dry_run,
        pass_count: pass_reports.len(),
        passes: pass_reports,
    })
}

fn run_scheduler_pass(
    storage: &mut Storage,
    pass: SchedulerControlPass,
    project: Option<String>,
    session_id: Option<String>,
    limit: usize,
    semantic_min_support: usize,
    l2_promotion_min_confidence: f32,
    l2_promotion_anomaly_min_confidence: f32,
    l2_promotion_min_repeated_support: usize,
    l2_promotion_reason: String,
    l3_decay_cutoff_ns: i64,
    l3_decay_max_access_count: i64,
    l3_decay_reason: String,
    task_frame_retention_cutoff_ns: i64,
    task_frame_retention_days: i64,
    task_frame_retention_reason: String,
    dry_run: bool,
) -> Result<SchedulerControlPassReport, StorageError> {
    match pass {
        SchedulerControlPass::OpenDecisionProposals => {
            let report = propose_open_decision_reviews(
                storage,
                OpenDecisionProposalInput { project, session_id, limit, dry_run },
            )?;
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary: "l2_open_decisions_become_request_verification_proposals_only"
                    .to_string(),
                summary: json!({
                    "inspected_signal_count": report.inspected_signal_count,
                    "proposed_count": report.proposed_count,
                    "skipped_existing_proposal_count": report.skipped_existing_proposal_count
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
        SchedulerControlPass::SemanticProposals => {
            let report = propose_semantic_consolidations(
                storage,
                SemanticLearningInput {
                    project,
                    session_id,
                    limit,
                    min_support: semantic_min_support,
                    dry_run,
                },
            )?;
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary:
                    "repeated_verified_l3_claims_become_semantic_promotion_proposals_and_latent_candidates_become_review_only_requests"
                        .to_string(),
                summary: json!({
                    "inspected_claim_count": report.inspected_claim_count,
                    "repeated_group_count": report.repeated_group_count,
                    "proposed_count": report.proposed_count,
                    "review_candidate_count": report.review_candidate_count,
                    "review_proposed_count": report.review_proposed_count,
                    "skipped_existing_semantic_count": report.skipped_existing_semantic_count,
                    "skipped_existing_proposal_count": report.skipped_existing_proposal_count,
                    "skipped_existing_review_proposal_count": report.skipped_existing_review_proposal_count
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
        SchedulerControlPass::ReviewDrain => {
            let report = drain_review_queue(
                storage,
                ReviewDrainInput { project, session_id, limit, dry_run },
            )?;
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary: "verified_non_destructive_promotion_only_through_storage_gates"
                    .to_string(),
                summary: json!({
                    "auto_applied_count": report.auto_applied_count,
                    "auto_skipped_count": report.auto_skipped_count,
                    "manual_action_count_after": report.manual_action_count_after
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
        SchedulerControlPass::L2Promote => {
            let report =
                storage.promote_short_term_proxies_by_policy(&ShortTermProxyPromotionRequest {
                    project,
                    session_id,
                    dry_run,
                    min_confidence: l2_promotion_min_confidence,
                    anomaly_min_confidence: l2_promotion_anomaly_min_confidence,
                    min_repeated_support: l2_promotion_min_repeated_support,
                    manual_proxy_ids: Vec::new(),
                    reason: l2_promotion_reason.clone(),
                    limit,
                })?;
            let promoted_count = report.promoted_proxy_ids.len();
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary:
                    "explicit_l2_promote_pass_only_moves_trusted_cloud_safe_l2_proxies_to_l3"
                        .to_string(),
                summary: json!({
                    "inspected_count": report.inspected_count,
                    "eligible_count": report.eligible_count,
                    "promoted_count": promoted_count,
                    "skipped_cloud_draft_count": report.skipped_cloud_draft_count,
                    "skipped_unsafe_privacy_count": report.skipped_unsafe_privacy_count,
                    "reason": l2_promotion_reason
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
        SchedulerControlPass::L3Decay => {
            let report = storage.decay_inactive_long_term_proxies_scoped(
                project.as_deref(),
                session_id.as_deref(),
                l3_decay_cutoff_ns,
                l3_decay_max_access_count,
                &l3_decay_reason,
                dry_run,
                limit,
            )?;
            let decayed_count = report.decayed_proxy_ids.len();
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary:
                    "explicit_l3_decay_pass_only_soft_decays_stale_low_access_proxy_memory"
                        .to_string(),
                summary: json!({
                    "inspected_count": report.inspected_count,
                    "decayed_count": decayed_count,
                    "cutoff_ns": report.cutoff_ns,
                    "max_access_count": report.max_access_count,
                    "reason": l3_decay_reason
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
        SchedulerControlPass::TaskFrameRetention => {
            let retention_days = if task_frame_retention_days < 1 {
                DEFAULT_TASK_FRAME_RETENTION_DAYS
            } else {
                task_frame_retention_days
            };
            let report = storage.apply_task_frame_retention(&TaskFrameRetentionRequest {
                cutoff_ns: task_frame_retention_cutoff_ns,
                retention_days,
                project,
                session_id,
                apply: !dry_run,
            })?;
            Ok(SchedulerControlPassReport {
                pass: pass.as_str().to_string(),
                status: pass_status(dry_run),
                mutation_boundary:
                    "explicit_task_frame_retention_only_deletes_old_unreferenced_task_frames"
                        .to_string(),
                summary: json!({
                    "eligible_count": report.eligible_count,
                    "retained_referenced_count": report.retained_referenced_count,
                    "deleted_count": report.deleted_count,
                    "cutoff_ns": report.cutoff_ns,
                    "retention_days": report.retention_days,
                    "reason": task_frame_retention_reason
                }),
                report: serde_json::to_value(report).unwrap_or_else(|_| json!({})),
            })
        }
    }
}

fn pass_status(dry_run: bool) -> String {
    if dry_run { "previewed" } else { "executed" }.to_string()
}
