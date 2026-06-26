//! Mutating review actions for the review queue.
//!
//! This module is the small orchestration layer above the read-only queue. It
//! never promotes cloud drafts directly; it records trusted verification events
//! and applies proposals only through the existing storage-level gates.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::context::review::{
    build_review_queue, resolve_verification_targets, review_action_control_id, ReviewQueueInput,
    VerificationTargetInput,
};
use crate::context::semantic_learning::{
    SEMANTIC_LATENT_REVIEW_SOURCE, SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE,
};
use crate::storage::{
    LearningCriticApplyOptions, LearningCriticApplyOutcome, LearningCriticProposalStatus, Storage,
    StorageError, StoredClaimRecord, StoredEvidenceRef, StoredLearningCriticProposal,
    StoredVerificationEvent, TaskFrameOutcomeDraft, TaskFrameOutcomeType, VerificationEventDraft,
    VerificationResult, VerifierType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Confirm,
    Contradict,
    Supersede,
    Inconclusive,
    Accept,
    Reject,
    Wait,
    Apply,
    ConfirmAndApply,
}

impl ReviewAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewAction::Confirm => "confirm",
            ReviewAction::Contradict => "contradict",
            ReviewAction::Supersede => "supersede",
            ReviewAction::Inconclusive => "inconclusive",
            ReviewAction::Accept => "accept",
            ReviewAction::Reject => "reject",
            ReviewAction::Wait => "wait",
            ReviewAction::Apply => "apply",
            ReviewAction::ConfirmAndApply => "confirm_and_apply",
        }
    }

    fn verification_result(self) -> Option<VerificationResult> {
        match self {
            ReviewAction::Confirm | ReviewAction::ConfirmAndApply => {
                Some(VerificationResult::Confirmed)
            }
            ReviewAction::Contradict => Some(VerificationResult::Contradicted),
            ReviewAction::Supersede => Some(VerificationResult::Superseded),
            ReviewAction::Inconclusive => Some(VerificationResult::Inconclusive),
            ReviewAction::Accept
            | ReviewAction::Reject
            | ReviewAction::Wait
            | ReviewAction::Apply => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTarget {
    Claim(i64),
    Proposal(i64),
}

impl ReviewTarget {
    pub fn kind(self) -> &'static str {
        match self {
            ReviewTarget::Claim(_) => "claim",
            ReviewTarget::Proposal(_) => "proposal",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            ReviewTarget::Claim(id) | ReviewTarget::Proposal(id) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewActionInput {
    pub target: ReviewTarget,
    pub action: ReviewAction,
    pub control_id: Option<String>,
    pub verifier_type: Option<VerifierType>,
    pub evidence_ref: Option<StoredEvidenceRef>,
    pub note: Option<String>,
    pub confirm_destructive: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewActionReport {
    pub target_type: String,
    pub target_id: i64,
    pub action: String,
    pub control_id: Option<String>,
    pub control_binding_verified: bool,
    pub verification_result: Option<VerificationResult>,
    pub verification_event_ids: Vec<i64>,
    pub claim_ids: Vec<i64>,
    pub skipped_claim_ids: Vec<i64>,
    pub task_frame_outcome_ids: Vec<i64>,
    pub durable_promotion_trust: Option<bool>,
    pub claims: Vec<StoredClaimRecord>,
    pub verification_events: Vec<StoredVerificationEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_review_resolution: Option<SemanticReviewResolution>,
    pub proposal_before: Option<StoredLearningCriticProposal>,
    pub proposal_after: Option<StoredLearningCriticProposal>,
    pub apply_outcome: Option<LearningCriticApplyOutcome>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticReviewResolution {
    pub source: String,
    pub proposal_id: i64,
    pub action: String,
    pub resolution_kind: String,
    pub verifier_type: String,
    pub evidence_ref: StoredEvidenceRef,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewBatchInput {
    pub operations: Vec<ReviewActionInput>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchReport {
    pub source: String,
    pub dry_run: bool,
    pub requested_count: usize,
    pub applied_count: usize,
    pub failed_count: usize,
    pub operations: Vec<ReviewBatchOperationReport>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewBatchOperationReport {
    pub index: usize,
    pub target_type: String,
    pub target_id: i64,
    pub action: String,
    pub control_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub report: Option<ReviewActionReport>,
}

pub const REVIEW_BATCH_MAX_OPERATIONS: usize = 50;
const REVIEW_CONTROL_BINDING_LIMIT: usize = 10_000;

#[derive(Debug)]
pub enum ReviewActionError {
    Storage(StorageError),
    Invalid(String),
}

impl std::fmt::Display for ReviewActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewActionError::Storage(err) => write!(f, "storage: {err}"),
            ReviewActionError::Invalid(message) => write!(f, "invalid review action: {message}"),
        }
    }
}

impl std::error::Error for ReviewActionError {}

impl From<StorageError> for ReviewActionError {
    fn from(value: StorageError) -> Self {
        ReviewActionError::Storage(value)
    }
}

pub fn apply_review_action(
    storage: &mut Storage,
    input: ReviewActionInput,
) -> Result<ReviewActionReport, ReviewActionError> {
    validate_review_control_binding(storage, &input, true)?;
    match input.target {
        ReviewTarget::Claim(_) => apply_claim_action(storage, input),
        ReviewTarget::Proposal(_) => apply_proposal_action(storage, input),
    }
}

pub fn apply_review_batch(
    storage: &mut Storage,
    input: ReviewBatchInput,
) -> Result<ReviewBatchReport, ReviewActionError> {
    if input.operations.is_empty() {
        return Err(ReviewActionError::Invalid(
            "review batch requires at least one operation".to_string(),
        ));
    }
    if input.operations.len() > REVIEW_BATCH_MAX_OPERATIONS {
        return Err(ReviewActionError::Invalid(format!(
            "review batch accepts at most {REVIEW_BATCH_MAX_OPERATIONS} operations"
        )));
    }

    let mut preflight = Vec::with_capacity(input.operations.len());
    let mut failed_count = 0;
    for (index, operation) in input.operations.iter().enumerate() {
        match validate_review_batch_operation(storage, operation) {
            Ok(()) => {
                preflight.push(batch_operation_status(index, operation, "would_apply", None, None));
            }
            Err(err) => {
                failed_count += 1;
                preflight.push(batch_operation_status(
                    index,
                    operation,
                    "invalid",
                    Some(err.to_string()),
                    None,
                ));
            }
        }
    }

    if input.dry_run || failed_count > 0 {
        return Ok(ReviewBatchReport {
            source: "soma_review_batch".to_string(),
            dry_run: input.dry_run,
            requested_count: input.operations.len(),
            applied_count: 0,
            failed_count,
            operations: preflight,
            trust_boundary: "review_batch_is_verification_only_and_never_applies_proposals"
                .to_string(),
        });
    }

    let mut operations = Vec::with_capacity(input.operations.len());
    let mut applied_count = 0;
    let mut failed_count = 0;
    let mut stop_error: Option<String> = None;
    for (index, operation) in input.operations.into_iter().enumerate() {
        if let Some(error) = &stop_error {
            operations.push(batch_operation_status(
                index,
                &operation,
                "skipped_after_error",
                Some(error.clone()),
                None,
            ));
            continue;
        }
        match apply_review_batch_operation(storage, operation.clone()) {
            Ok(report) => {
                applied_count += 1;
                operations.push(batch_operation_status(
                    index,
                    &operation,
                    "applied",
                    None,
                    Some(report),
                ));
            }
            Err(err) => {
                failed_count += 1;
                let error = err.to_string();
                stop_error = Some(error.clone());
                operations.push(batch_operation_status(
                    index,
                    &operation,
                    "failed",
                    Some(error),
                    None,
                ));
            }
        }
    }

    Ok(ReviewBatchReport {
        source: "soma_review_batch".to_string(),
        dry_run: false,
        requested_count: operations.len(),
        applied_count,
        failed_count,
        operations,
        trust_boundary: "review_batch_is_verification_only_and_never_applies_proposals".to_string(),
    })
}

fn apply_review_batch_operation(
    storage: &mut Storage,
    input: ReviewActionInput,
) -> Result<ReviewActionReport, ReviewActionError> {
    match input.target {
        ReviewTarget::Claim(_) => apply_claim_action(storage, input),
        ReviewTarget::Proposal(_) => apply_proposal_action(storage, input),
    }
}

fn apply_claim_action(
    storage: &mut Storage,
    input: ReviewActionInput,
) -> Result<ReviewActionReport, ReviewActionError> {
    if input.action == ReviewAction::ConfirmAndApply {
        return Err(ReviewActionError::Invalid(
            "confirm_and_apply is only valid for proposal targets".to_string(),
        ));
    }
    let Some(result) = input.action.verification_result() else {
        return Err(ReviewActionError::Invalid(
            "claim targets accept confirm, contradict, supersede, or inconclusive".to_string(),
        ));
    };
    let (verifier_type, evidence_ref) = verifier_and_evidence(&input)?;
    let target = VerificationTargetInput::Claim(input.target.id());
    let verification =
        insert_verification_events(storage, target, result, verifier_type, evidence_ref)?;
    report_from_verification(storage, input, Some(result), verification, None, None)
}

fn apply_proposal_action(
    storage: &mut Storage,
    input: ReviewActionInput,
) -> Result<ReviewActionReport, ReviewActionError> {
    let proposal_id = input.target.id();
    let proposal_before = storage.learning_critic_proposal(proposal_id)?;
    match input.action {
        ReviewAction::Confirm
        | ReviewAction::Contradict
        | ReviewAction::Supersede
        | ReviewAction::Inconclusive => {
            let result = input.action.verification_result().expect("verification action");
            let (verifier_type, evidence_ref) = verifier_and_evidence(&input)?;
            let verification = insert_verification_events(
                storage,
                VerificationTargetInput::Proposal(proposal_id),
                result,
                verifier_type,
                evidence_ref,
            )?;
            Ok(report_from_verification(
                storage,
                input,
                Some(result),
                verification,
                proposal_before,
                None,
            )?)
        }
        ReviewAction::ConfirmAndApply => {
            let (verifier_type, evidence_ref) = verifier_and_evidence(&input)?;
            let verification = insert_verification_events(
                storage,
                VerificationTargetInput::Proposal(proposal_id),
                VerificationResult::Confirmed,
                verifier_type,
                evidence_ref,
            )?;
            let outcome = storage.apply_learning_critic_proposal_with_options(
                proposal_id,
                LearningCriticApplyOptions { allow_destructive: input.confirm_destructive },
            )?;
            Ok(report_from_verification(
                storage,
                input,
                Some(VerificationResult::Confirmed),
                verification,
                proposal_before,
                Some(outcome),
            )?)
        }
        ReviewAction::Accept | ReviewAction::Reject | ReviewAction::Wait => {
            let semantic_review_resolution =
                semantic_review_resolution_for_status_action(&input, proposal_before.as_ref())?;
            let status = match input.action {
                ReviewAction::Accept => LearningCriticProposalStatus::Accepted,
                ReviewAction::Reject => LearningCriticProposalStatus::Rejected,
                ReviewAction::Wait => LearningCriticProposalStatus::WaitingVerification,
                _ => unreachable!("status action checked above"),
            };
            storage.update_learning_critic_proposal_status(
                proposal_id,
                status,
                Some(&proposal_status_result_json(&input, semantic_review_resolution.as_ref())),
            )?;
            Ok(empty_report(storage, input, proposal_before, None, semantic_review_resolution)?)
        }
        ReviewAction::Apply => {
            let outcome = storage.apply_learning_critic_proposal_with_options(
                proposal_id,
                LearningCriticApplyOptions { allow_destructive: input.confirm_destructive },
            )?;
            Ok(empty_report(storage, input, proposal_before, Some(outcome), None)?)
        }
    }
}

fn proposal_status_result_json(
    input: &ReviewActionInput,
    semantic_review_resolution: Option<&SemanticReviewResolution>,
) -> Value {
    let mut result = json!({
        "review": "review_action",
        "action": input.action.as_str(),
        "note": input.note,
    });
    if let Some(resolution) = semantic_review_resolution {
        result["semantic_review_resolution"] = serde_json::to_value(resolution)
            .unwrap_or_else(|_| json!({"error": "semantic_review_resolution_encode_failed"}));
    }
    result
}

fn semantic_review_resolution_for_status_action(
    input: &ReviewActionInput,
    proposal: Option<&StoredLearningCriticProposal>,
) -> Result<Option<SemanticReviewResolution>, ReviewActionError> {
    let Some(proposal) = proposal else { return Ok(None) };
    if !is_semantic_review_only_proposal(proposal) {
        return Ok(None);
    }
    match input.action {
        ReviewAction::Accept | ReviewAction::Reject => {
            let (verifier_type, evidence_ref) = verifier_and_evidence(input)?;
            Ok(Some(SemanticReviewResolution {
                source: "soma_semantic_review_resolution_v1".to_string(),
                proposal_id: proposal.id,
                action: input.action.as_str().to_string(),
                resolution_kind: match input.action {
                    ReviewAction::Accept => "accept_review_only_candidate".to_string(),
                    ReviewAction::Reject => "reject_review_only_candidate".to_string(),
                    _ => unreachable!("status action checked above"),
                },
                verifier_type: verifier_type.as_str().to_string(),
                evidence_ref,
                trust_boundary:
                    "semantic_review_resolution_is_proposal_audit_only: records reviewer evidence in proposal result_json, creates no verification_event, writes no L4 semantic_fact, and promotes no cloud draft"
                        .to_string(),
            }))
        }
        ReviewAction::Wait => Ok(None),
        _ => Ok(None),
    }
}

fn is_semantic_review_only_proposal(proposal: &StoredLearningCriticProposal) -> bool {
    proposal.action == crate::storage::LearningCriticAction::RequestVerification
        && proposal.target_lifecycle_state.is_none()
        && proposal.evidence_refs.iter().any(|evidence_ref| {
            evidence_ref.kind == "semantic_review_candidate"
                && matches!(
                    evidence_ref.source.as_deref(),
                    Some(SEMANTIC_LATENT_REVIEW_SOURCE | SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE)
                )
        })
}

fn validate_review_batch_operation(
    storage: &Storage,
    input: &ReviewActionInput,
) -> Result<(), ReviewActionError> {
    validate_review_control_binding(storage, input, false)?;
    match input.action {
        ReviewAction::Confirm
        | ReviewAction::Contradict
        | ReviewAction::Supersede
        | ReviewAction::Inconclusive => {}
        ReviewAction::Accept
        | ReviewAction::Reject
        | ReviewAction::Wait
        | ReviewAction::Apply
        | ReviewAction::ConfirmAndApply => {
            return Err(ReviewActionError::Invalid(
                "review batch is verification-only; use review-action or review-drain for proposal apply/status actions"
                    .to_string(),
            ));
        }
    }
    let result = input.action.verification_result().ok_or_else(|| {
        ReviewActionError::Invalid("review batch operation must record verification".to_string())
    })?;
    verifier_and_evidence(input)?;
    match input.target {
        ReviewTarget::Claim(claim_id) => {
            if storage.claim_record(claim_id)?.is_none() {
                return Err(ReviewActionError::Invalid(format!("claim {claim_id} not found")));
            }
        }
        ReviewTarget::Proposal(proposal_id) => {
            resolve_verification_targets(
                storage,
                VerificationTargetInput::Proposal(proposal_id),
                result,
            )?;
        }
    }
    Ok(())
}

fn validate_review_control_binding(
    storage: &Storage,
    input: &ReviewActionInput,
    required: bool,
) -> Result<(), ReviewActionError> {
    let Some(control_id) =
        input.control_id.as_deref().map(str::trim).filter(|value| !value.is_empty())
    else {
        if required {
            return Err(ReviewActionError::Invalid(
                "review action requires control_id from a currently enabled review action option"
                    .to_string(),
            ));
        }
        return Ok(());
    };
    let expected =
        review_action_control_id(input.target.kind(), input.target.id(), input.action.as_str());
    if control_id != expected {
        return Err(ReviewActionError::Invalid(format!(
            "review control_id `{control_id}` does not match target/action `{expected}`"
        )));
    }
    let queue = build_review_queue(
        storage,
        ReviewQueueInput { project: None, session_id: None, limit: REVIEW_CONTROL_BINDING_LIMIT },
    )?;
    let enabled = queue
        .claims
        .iter()
        .flat_map(|item| item.action_options.iter())
        .chain(queue.proposals.iter().flat_map(|item| item.action_options.iter()))
        .any(|action| {
            action.enabled
                && action.control_id == control_id
                && action.target_type == input.target.kind()
                && action.target_id == input.target.id()
                && action.action == input.action.as_str()
        });
    if enabled {
        Ok(())
    } else {
        Err(ReviewActionError::Invalid(format!(
            "review control_id `{control_id}` is not currently enabled in the review queue"
        )))
    }
}

fn batch_operation_status(
    index: usize,
    input: &ReviewActionInput,
    status: &str,
    error: Option<String>,
    report: Option<ReviewActionReport>,
) -> ReviewBatchOperationReport {
    ReviewBatchOperationReport {
        index,
        target_type: input.target.kind().to_string(),
        target_id: input.target.id(),
        action: input.action.as_str().to_string(),
        control_id: input.control_id.clone(),
        status: status.to_string(),
        error,
        report,
    }
}

#[derive(Debug, Clone)]
struct VerificationInsertOutcome {
    event_ids: Vec<i64>,
    claim_ids: Vec<i64>,
    skipped_claim_ids: Vec<i64>,
}

fn insert_verification_events(
    storage: &mut Storage,
    target: VerificationTargetInput,
    result: VerificationResult,
    verifier_type: VerifierType,
    evidence_ref: StoredEvidenceRef,
) -> Result<VerificationInsertOutcome, ReviewActionError> {
    let resolution = resolve_verification_targets(storage, target, result)?;
    let mut event_ids = Vec::new();
    for claim_id in &resolution.claim_ids {
        let event_id = storage.insert_verification_event(&VerificationEventDraft {
            claim_id: *claim_id,
            verifier_type,
            result,
            evidence_ref: evidence_ref.clone(),
        })?;
        event_ids.push(event_id);
    }
    Ok(VerificationInsertOutcome {
        event_ids,
        claim_ids: resolution.claim_ids,
        skipped_claim_ids: resolution.skipped_claim_ids,
    })
}

fn verifier_and_evidence(
    input: &ReviewActionInput,
) -> Result<(VerifierType, StoredEvidenceRef), ReviewActionError> {
    let verifier_type = input.verifier_type.ok_or_else(|| {
        ReviewActionError::Invalid(
            "verification actions require verifier_type and evidence_ref".to_string(),
        )
    })?;
    let evidence_ref = input.evidence_ref.clone().ok_or_else(|| {
        ReviewActionError::Invalid(
            "verification actions require verifier_type and evidence_ref".to_string(),
        )
    })?;
    if evidence_ref.kind.trim().is_empty() || evidence_ref.id.trim().is_empty() {
        return Err(ReviewActionError::Invalid(
            "evidence_ref.kind and evidence_ref.id must be non-empty".to_string(),
        ));
    }
    Ok((verifier_type, evidence_ref))
}

fn report_from_verification(
    storage: &mut Storage,
    input: ReviewActionInput,
    result: Option<VerificationResult>,
    verification: VerificationInsertOutcome,
    proposal_before: Option<StoredLearningCriticProposal>,
    apply_outcome: Option<LearningCriticApplyOutcome>,
) -> Result<ReviewActionReport, ReviewActionError> {
    let mut claims = Vec::new();
    let mut events = Vec::new();
    for claim_id in &verification.claim_ids {
        if let Some(claim) = storage.claim_record(*claim_id)? {
            claims.push(claim);
        }
        let mut claim_events = storage.verification_events_for_claim(*claim_id)?;
        claim_events.retain(|event| verification.event_ids.contains(&event.id));
        events.extend(claim_events);
    }
    let mut durable_promotion_trust = true;
    for claim_id in verification.claim_ids.iter().chain(verification.skipped_claim_ids.iter()) {
        durable_promotion_trust &= storage.claim_has_durable_promotion_trust(*claim_id)?;
    }
    let task_frame_outcome_ids = record_task_frame_outcomes_for_review(
        storage,
        &input,
        result,
        &claims,
        &events,
        proposal_before.as_ref(),
        apply_outcome,
    )?;
    Ok(ReviewActionReport {
        target_type: input.target.kind().to_string(),
        target_id: input.target.id(),
        action: input.action.as_str().to_string(),
        control_id: input.control_id.clone(),
        control_binding_verified: input
            .control_id
            .as_deref()
            .is_some_and(|control_id| !control_id.trim().is_empty()),
        verification_result: result,
        verification_event_ids: verification.event_ids,
        claim_ids: verification.claim_ids,
        skipped_claim_ids: verification.skipped_claim_ids,
        task_frame_outcome_ids,
        durable_promotion_trust: Some(durable_promotion_trust),
        claims,
        verification_events: events,
        semantic_review_resolution: None,
        proposal_before,
        proposal_after: proposal_after(storage, input.target)?,
        apply_outcome,
        trust_boundary:
            "review_action_uses_verification_storage_gates_and_required_current_control_binding"
                .to_string(),
    })
}

fn empty_report(
    storage: &Storage,
    input: ReviewActionInput,
    proposal_before: Option<StoredLearningCriticProposal>,
    apply_outcome: Option<LearningCriticApplyOutcome>,
    semantic_review_resolution: Option<SemanticReviewResolution>,
) -> Result<ReviewActionReport, ReviewActionError> {
    Ok(ReviewActionReport {
        target_type: input.target.kind().to_string(),
        target_id: input.target.id(),
        action: input.action.as_str().to_string(),
        control_id: input.control_id.clone(),
        control_binding_verified: input
            .control_id
            .as_deref()
            .is_some_and(|control_id| !control_id.trim().is_empty()),
        verification_result: None,
        verification_event_ids: Vec::new(),
        claim_ids: Vec::new(),
        skipped_claim_ids: Vec::new(),
        task_frame_outcome_ids: Vec::new(),
        durable_promotion_trust: None,
        claims: Vec::new(),
        verification_events: Vec::new(),
        semantic_review_resolution,
        proposal_before,
        proposal_after: proposal_after(storage, input.target)?,
        apply_outcome,
        trust_boundary:
            "review_action_uses_verification_storage_gates_and_required_current_control_binding"
                .to_string(),
    })
}

fn proposal_after(
    storage: &Storage,
    target: ReviewTarget,
) -> Result<Option<StoredLearningCriticProposal>, ReviewActionError> {
    match target {
        ReviewTarget::Claim(_) => Ok(None),
        ReviewTarget::Proposal(proposal_id) => Ok(storage.learning_critic_proposal(proposal_id)?),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_task_frame_outcomes_for_review(
    storage: &mut Storage,
    input: &ReviewActionInput,
    result: Option<VerificationResult>,
    claims: &[StoredClaimRecord],
    events: &[StoredVerificationEvent],
    proposal_before: Option<&StoredLearningCriticProposal>,
    apply_outcome: Option<LearningCriticApplyOutcome>,
) -> Result<Vec<i64>, ReviewActionError> {
    let Some(result) = result else {
        return Ok(Vec::new());
    };
    if events.is_empty() {
        return Ok(Vec::new());
    }

    let outcome_type = review_outcome_type(result, apply_outcome);
    let mut by_task_frame: BTreeMap<i64, TaskFrameOutcomeBucket> = BTreeMap::new();
    for claim in claims {
        if let Some(task_frame_id) = claim.task_frame_id {
            let bucket = by_task_frame.entry(task_frame_id).or_default();
            bucket.claim_ids.push(claim.id);
        }
    }
    if let Some(proposal) = proposal_before {
        if let Some(task_frame_id) = proposal.task_frame_id {
            let bucket = by_task_frame.entry(task_frame_id).or_default();
            bucket.proposal_ids.push(proposal.id);
        }
    }

    let mut outcome_ids = Vec::new();
    for (task_frame_id, mut bucket) in by_task_frame {
        bucket.claim_ids.sort_unstable();
        bucket.claim_ids.dedup();
        bucket.proposal_ids.sort_unstable();
        bucket.proposal_ids.dedup();
        let claim_ids_for_task = bucket.claim_ids.clone();
        let evidence_refs = outcome_evidence_refs_for_claims(events, &claim_ids_for_task);
        if evidence_refs.is_empty() {
            continue;
        }
        let outcome_id = storage.insert_task_frame_outcome(&TaskFrameOutcomeDraft {
            task_frame_id,
            outcome_type,
            summary: review_outcome_summary(
                input,
                result,
                outcome_type,
                claim_ids_for_task.len(),
                bucket.proposal_ids.len(),
                apply_outcome,
            ),
            evidence_refs,
            claim_ids: claim_ids_for_task,
            proposal_ids: bucket.proposal_ids,
            latent_proxy_ids: Vec::new(),
        })?;
        outcome_ids.push(outcome_id);
    }
    Ok(outcome_ids)
}

#[derive(Default)]
struct TaskFrameOutcomeBucket {
    claim_ids: Vec<i64>,
    proposal_ids: Vec<i64>,
}

fn review_outcome_type(
    result: VerificationResult,
    apply_outcome: Option<LearningCriticApplyOutcome>,
) -> TaskFrameOutcomeType {
    match (result, apply_outcome) {
        (VerificationResult::Confirmed, Some(LearningCriticApplyOutcome::Applied)) => {
            TaskFrameOutcomeType::Applied
        }
        (VerificationResult::Confirmed, _) => TaskFrameOutcomeType::Verified,
        (VerificationResult::Contradicted, _) => TaskFrameOutcomeType::Rejected,
        (VerificationResult::Superseded, _) => TaskFrameOutcomeType::Revised,
        (VerificationResult::Inconclusive, _) => TaskFrameOutcomeType::Failed,
    }
}

fn outcome_evidence_refs_for_claims(
    events: &[StoredVerificationEvent],
    claim_ids: &[i64],
) -> Vec<StoredEvidenceRef> {
    let mut evidence_refs = Vec::new();
    for event in events {
        if !claim_ids.contains(&event.claim_id) {
            continue;
        }
        if !evidence_refs.contains(&event.evidence_ref) {
            evidence_refs.push(event.evidence_ref.clone());
        }
    }
    evidence_refs
}

fn review_outcome_summary(
    input: &ReviewActionInput,
    result: VerificationResult,
    outcome_type: TaskFrameOutcomeType,
    claim_count: usize,
    proposal_count: usize,
    apply_outcome: Option<LearningCriticApplyOutcome>,
) -> String {
    let apply =
        apply_outcome.map(|outcome| format!(", apply_outcome={outcome:?}")).unwrap_or_default();
    format!(
        "review_action `{}` recorded `{}` as TaskFrame outcome `{}` for {claim_count} claim(s) and {proposal_count} proposal(s){apply}",
        input.action.as_str(),
        result.as_str(),
        outcome_type.as_str(),
    )
}
