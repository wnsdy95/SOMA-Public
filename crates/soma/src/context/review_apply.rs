//! Gated batch application for learning critic proposals.
//!
//! This module is intentionally separate from the read-only review queue. It
//! can mutate proposal/claim lifecycle, but only by calling the existing
//! storage-level `apply_learning_critic_proposal` gate for each selected row.

use serde::Serialize;

use crate::context::semantic_learning::{semantic_support_diversity, SEMANTIC_LEARNING_SOURCE};
use crate::storage::{
    LearningCriticAction, LearningCriticApplyOptions, LearningCriticApplyOutcome, LifecycleState,
    Storage, StorageError, StoredClaimRecord, StoredLearningCriticProposal,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReadyInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
    pub include_decay: bool,
    pub include_noop: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApplyReadyReport {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
    pub include_decay: bool,
    pub include_noop: bool,
    pub considered_count: usize,
    pub ready_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub items: Vec<ApplyReadyItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApplyReadyItem {
    pub proposal_id: i64,
    pub action: LearningCriticAction,
    pub target_lifecycle_state: Option<LifecycleState>,
    pub status_before: String,
    pub status_after: Option<String>,
    pub ready: bool,
    pub dry_run: bool,
    pub outcome: Option<LearningCriticApplyOutcome>,
    pub skipped_reason: Option<String>,
    pub missing_verification_claim_ids: Vec<i64>,
    pub proposal_after: Option<StoredLearningCriticProposal>,
}

pub fn apply_ready_learning_proposals(
    storage: &mut Storage,
    input: ApplyReadyInput,
) -> Result<ApplyReadyReport, StorageError> {
    let limit = input.limit.max(1);
    let proposals = storage.open_learning_critic_proposals_scoped(
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;
    let mut items = Vec::with_capacity(proposals.len());

    for proposal in proposals {
        let decision =
            apply_ready_decision(storage, &proposal, input.include_decay, input.include_noop)?;
        let status_before = proposal.status.as_str().to_string();
        if !decision.ready || input.dry_run {
            items.push(ApplyReadyItem {
                proposal_id: proposal.id,
                action: proposal.action,
                target_lifecycle_state: proposal.target_lifecycle_state,
                status_before,
                status_after: Some(proposal.status.as_str().to_string()),
                ready: decision.ready,
                dry_run: input.dry_run,
                outcome: None,
                skipped_reason: decision.skipped_reason,
                missing_verification_claim_ids: decision.missing_verification_claim_ids,
                proposal_after: None,
            });
            continue;
        }

        let outcome = storage.apply_learning_critic_proposal_with_options(
            proposal.id,
            LearningCriticApplyOptions { allow_destructive: input.include_decay },
        )?;
        let proposal_after = storage.learning_critic_proposal(proposal.id)?.ok_or_else(|| {
            StorageError::Corrupt {
                detail: format!("learning critic proposal {} disappeared", proposal.id),
            }
        })?;
        items.push(ApplyReadyItem {
            proposal_id: proposal.id,
            action: proposal.action,
            target_lifecycle_state: proposal.target_lifecycle_state,
            status_before,
            status_after: Some(proposal_after.status.as_str().to_string()),
            ready: true,
            dry_run: false,
            outcome: Some(outcome),
            skipped_reason: None,
            missing_verification_claim_ids: Vec::new(),
            proposal_after: Some(proposal_after),
        });
    }

    let ready_count = items.iter().filter(|item| item.ready).count();
    let applied_count = items.iter().filter(|item| item.outcome.is_some()).count();
    let skipped_count = items.len().saturating_sub(applied_count);
    Ok(ApplyReadyReport {
        project: input.project,
        session_id: input.session_id,
        limit,
        dry_run: input.dry_run,
        include_decay: input.include_decay,
        include_noop: input.include_noop,
        considered_count: items.len(),
        ready_count,
        applied_count,
        skipped_count,
        items,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyReadyDecision {
    ready: bool,
    skipped_reason: Option<String>,
    missing_verification_claim_ids: Vec<i64>,
}

fn apply_ready_decision(
    storage: &Storage,
    proposal: &StoredLearningCriticProposal,
    include_decay: bool,
    include_noop: bool,
) -> Result<ApplyReadyDecision, StorageError> {
    match proposal.action {
        LearningCriticAction::ProposePromotion => {
            if proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact) {
                if let Some(decision) = semantic_auto_apply_diversity_decision(storage, proposal)? {
                    return Ok(decision);
                }
            }
            let mut missing = Vec::new();
            for claim_id in &proposal.claim_ids {
                if !storage.claim_has_durable_promotion_trust(*claim_id)? {
                    missing.push(*claim_id);
                }
            }
            if missing.is_empty() {
                Ok(ready())
            } else {
                Ok(skipped("missing_confirmed_verification", missing))
            }
        }
        LearningCriticAction::Decay => {
            if include_decay {
                Ok(ready())
            } else {
                Ok(skipped("decay_requires_include_decay", Vec::new()))
            }
        }
        LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => {
            if include_noop {
                Ok(ready())
            } else {
                Ok(skipped("noop_requires_include_noop", Vec::new()))
            }
        }
        LearningCriticAction::RequestVerification => {
            Ok(skipped("request_verification_requires_manual_verification", Vec::new()))
        }
    }
}

fn semantic_auto_apply_diversity_decision(
    storage: &Storage,
    proposal: &StoredLearningCriticProposal,
) -> Result<Option<ApplyReadyDecision>, StorageError> {
    let support_claim_ids = semantic_support_claim_ids(proposal);
    if support_claim_ids.is_empty() {
        return Ok(Some(skipped("semantic_support_evidence_required", Vec::new())));
    }

    let mut support_claims = Vec::with_capacity(support_claim_ids.len());
    let mut invalid_claim_ids = Vec::new();
    for claim_id in support_claim_ids {
        match storage.claim_record(claim_id)? {
            Some(claim) if semantic_support_claim_is_active_and_trusted(storage, &claim)? => {
                support_claims.push(claim);
            }
            Some(_) | None => invalid_claim_ids.push(claim_id),
        }
    }
    if !invalid_claim_ids.is_empty() {
        return Ok(Some(skipped(
            "semantic_support_claims_invalidated_before_apply",
            invalid_claim_ids,
        )));
    }

    let diversity = semantic_support_diversity(storage, &support_claims)?;
    if diversity.bias_risk != "low_diverse_support" {
        return Ok(Some(skipped("semantic_support_diversity_requires_manual_review", Vec::new())));
    }
    Ok(None)
}

fn semantic_support_claim_is_active_and_trusted(
    storage: &Storage,
    claim: &StoredClaimRecord,
) -> Result<bool, StorageError> {
    let active_l3_or_l4 = matches!(
        claim.lifecycle_state,
        LifecycleState::LongTermMemory | LifecycleState::SemanticFact
    );
    Ok(active_l3_or_l4 && storage.claim_has_durable_promotion_trust(claim.id)?)
}

fn semantic_support_claim_ids(proposal: &StoredLearningCriticProposal) -> Vec<i64> {
    let mut ids = proposal
        .evidence_refs
        .iter()
        .filter(|evidence| {
            evidence.kind == "claim_record"
                && evidence.source.as_deref() == Some(SEMANTIC_LEARNING_SOURCE)
        })
        .filter_map(|evidence| evidence.id.parse::<i64>().ok())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn ready() -> ApplyReadyDecision {
    ApplyReadyDecision { ready: true, skipped_reason: None, missing_verification_claim_ids: vec![] }
}

fn skipped(reason: &str, missing_verification_claim_ids: Vec<i64>) -> ApplyReadyDecision {
    ApplyReadyDecision {
        ready: false,
        skipped_reason: Some(reason.to_string()),
        missing_verification_claim_ids,
    }
}
