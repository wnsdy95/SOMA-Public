//! Policy-based review drain for safe background/client automation.
//!
//! The drain intentionally performs only the same safe mutation the slow loop is
//! allowed to perform: apply already verified, non-destructive promotion
//! proposals through storage gates. It never verifies cloud drafts and never
//! applies decay/forget proposals.

use serde::Serialize;

use crate::context::review::{
    build_review_action_plan, build_review_queue, ReviewActionPlanInput, ReviewQueueInput,
};
use crate::context::review_apply::{
    apply_ready_learning_proposals, ApplyReadyInput, ApplyReadyReport,
};
use crate::storage::{Storage, StorageError};

pub const REVIEW_DRAIN_POLICY: &str = "verified_non_destructive_promotion_only";
pub const REVIEW_DRAIN_TRUST_BOUNDARY: &str =
    "cloud_drafts_require_external_verification_before_durable_memory";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDrainInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDrainReport {
    pub source: String,
    pub policy: String,
    pub trust_boundary: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
    pub before: ReviewDrainSnapshot,
    pub apply_ready: ApplyReadyReport,
    pub after: ReviewDrainSnapshot,
    pub auto_applied_count: usize,
    pub auto_skipped_count: usize,
    pub manual_action_count_after: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewDrainSnapshot {
    pub claim_count: usize,
    pub proposal_count: usize,
    pub ready_proposal_count: usize,
    pub manual_review_proposal_count: usize,
    pub missing_verification_count: usize,
    pub action_count: usize,
    pub enabled_action_count: usize,
    pub disabled_action_count: usize,
    pub evidence_required_action_count: usize,
    pub destructive_action_count: usize,
    pub safe_auto_apply_action_count: usize,
    pub manual_action_count: usize,
}

pub fn drain_review_queue(
    storage: &mut Storage,
    input: ReviewDrainInput,
) -> Result<ReviewDrainReport, StorageError> {
    let limit = input.limit.max(1);
    let before =
        review_drain_snapshot(storage, input.project.clone(), input.session_id.clone(), limit)?;
    let apply_ready = apply_ready_learning_proposals(
        storage,
        ApplyReadyInput {
            project: input.project.clone(),
            session_id: input.session_id.clone(),
            limit,
            dry_run: input.dry_run,
            include_decay: false,
            include_noop: false,
        },
    )?;
    let after =
        review_drain_snapshot(storage, input.project.clone(), input.session_id.clone(), limit)?;

    Ok(ReviewDrainReport {
        source: "soma_review_drain".to_string(),
        policy: REVIEW_DRAIN_POLICY.to_string(),
        trust_boundary: REVIEW_DRAIN_TRUST_BOUNDARY.to_string(),
        project: input.project,
        session_id: input.session_id,
        limit,
        dry_run: input.dry_run,
        auto_applied_count: apply_ready.applied_count,
        auto_skipped_count: apply_ready.skipped_count,
        manual_action_count_after: after.manual_action_count,
        before,
        apply_ready,
        after,
    })
}

fn review_drain_snapshot(
    storage: &Storage,
    project: Option<String>,
    session_id: Option<String>,
    limit: usize,
) -> Result<ReviewDrainSnapshot, StorageError> {
    let queue = build_review_queue(
        storage,
        ReviewQueueInput { project: project.clone(), session_id: session_id.clone(), limit },
    )?;
    let plan = build_review_action_plan(
        storage,
        ReviewActionPlanInput { project, session_id, limit, include_disabled: true },
    )?;
    let enabled_action_count = plan.actions.iter().filter(|action| action.enabled).count();
    let evidence_required_action_count =
        plan.actions.iter().filter(|action| action.enabled && action.requires_evidence).count();
    let destructive_action_count = plan
        .actions
        .iter()
        .filter(|action| action.enabled && action.requires_destructive_confirmation)
        .count();
    let safe_auto_apply_action_count = plan
        .actions
        .iter()
        .filter(|action| {
            action.enabled
                && action.target_type == "proposal"
                && action.action == "apply"
                && !action.requires_evidence
                && !action.requires_destructive_confirmation
        })
        .count();
    let manual_action_count = enabled_action_count.saturating_sub(safe_auto_apply_action_count);

    Ok(ReviewDrainSnapshot {
        claim_count: queue.claim_count,
        proposal_count: queue.proposal_count,
        ready_proposal_count: queue.ready_proposal_count,
        manual_review_proposal_count: queue.manual_review_proposal_count,
        missing_verification_count: queue.missing_verification_count,
        action_count: plan.action_count,
        enabled_action_count,
        disabled_action_count: plan.disabled_action_count,
        evidence_required_action_count,
        destructive_action_count,
        safe_auto_apply_action_count,
        manual_action_count,
    })
}
