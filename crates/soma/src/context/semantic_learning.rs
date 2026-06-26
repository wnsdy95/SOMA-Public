//! Deterministic semantic consolidation from verified L3 evidence.
//!
//! This pass does not promote claims directly. It proposes L4
//! `semantic_fact` promotion only when a repeated pattern is already present in
//! long-term memory and every supporting claim has durable promotion trust.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::storage::{
    LearningCriticAction, LearningCriticProposalDraft, LearningCriticProposalStatus,
    LifecycleState, Storage, StorageError, StoredClaimRecord, StoredEvidenceRef,
    StoredLearningCriticProposal, VerificationResult,
};

pub const SEMANTIC_LEARNING_SOURCE: &str = "soma_semantic_learning";
pub const SEMANTIC_LEARNING_RULE: &str = "repeated_verified_l3_claims";
pub const SEMANTIC_EXACT_GROUP_RULE: &str = "exact_normalized_text";
pub const SEMANTIC_TOKEN_GROUP_RULE: &str = "conservative_token_signature";
pub const SEMANTIC_LATENT_REVIEW_SOURCE: &str = "soma_semantic_latent_review";
pub const SEMANTIC_LATENT_REVIEW_RULE: &str = "latent_paraphrase_review_candidate";
pub const SEMANTIC_LATENT_REVIEW_GROUP_RULE: &str = "token_overlap_review_candidate";
pub const SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE: &str = "soma_semantic_conflict_review";
pub const SEMANTIC_NEGATION_CONFLICT_RULE: &str = "semantic_negation_conflict_review_candidate";
pub const SEMANTIC_NEGATION_CONFLICT_GROUP_RULE: &str = "negation_stripped_token_signature";
pub const SEMANTIC_LEARNING_REPORT_SCHEMA: &str = "soma.semantic_proposals_report.v1";
const SEMANTIC_REVIEW_CANDIDATE_KIND: &str = "semantic_review_candidate";
const DEFAULT_PROPOSAL_SCAN_LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticLearningInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub min_support: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticLearningReport {
    pub schema: String,
    pub source: String,
    pub rule: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub min_support: usize,
    pub dry_run: bool,
    pub status: String,
    pub headline: String,
    pub primary_lane: String,
    pub primary_next_action_id: String,
    pub primary_next_action_label: String,
    pub operator_next_action_id: String,
    pub operator_next_action_label: String,
    pub primary_next_command: Vec<String>,
    pub next_commands: Vec<Vec<String>>,
    pub operator_card: SemanticLearningOperatorCard,
    pub review_lanes: Vec<SemanticReviewLane>,
    pub trust_boundary: String,
    pub inspected_claim_count: usize,
    pub repeated_group_count: usize,
    pub l4_candidate_count: usize,
    pub review_only_candidate_count: usize,
    pub blocked_untrusted_count: usize,
    pub already_handled_count: usize,
    pub proposed_count: usize,
    pub skipped_existing_semantic_count: usize,
    pub skipped_existing_proposal_count: usize,
    pub skipped_existing_review_proposal_count: usize,
    pub skipped_untrusted_count: usize,
    pub review_candidate_count: usize,
    pub review_proposed_count: usize,
    pub items: Vec<SemanticLearningItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticLearningOperatorCard {
    pub source: String,
    pub status: String,
    pub headline: String,
    pub primary_lane: String,
    pub primary_next_action_id: String,
    pub primary_next_action_label: String,
    pub l4_candidate_count: usize,
    pub review_only_candidate_count: usize,
    pub blocked_untrusted_count: usize,
    pub already_handled_count: usize,
    pub records_verification: bool,
    pub writes_semantic_fact: bool,
    pub promotes_cloud_draft: bool,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticReviewLane {
    pub source: String,
    pub id: String,
    pub label: String,
    pub count: usize,
    pub item_indexes: Vec<usize>,
    pub next_action_id: String,
    pub next_action_label: String,
    pub records_verification: bool,
    pub writes_semantic_fact: bool,
    pub promotes_cloud_draft: bool,
    pub blocked_resolution_actions: Vec<String>,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticLearningItem {
    pub normalized_text: String,
    pub group_key: String,
    pub group_rule: String,
    pub support_claim_ids: Vec<i64>,
    pub proposal_claim_ids: Vec<i64>,
    pub support_count: usize,
    pub trusted: bool,
    pub action: String,
    pub proposal_id: Option<i64>,
    pub skipped_reason: Option<String>,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub support_diversity: SemanticSupportDiversity,
    pub readiness_score: SemanticReadinessScore,
    pub resolution_plan: SemanticResolutionPlan,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticResolutionPlan {
    pub source: String,
    pub status: String,
    pub target_lifecycle_state: String,
    pub intent: String,
    pub allowed_resolution_actions: Vec<String>,
    pub blocked_resolution_actions: Vec<String>,
    pub trusted_verifier_types: Vec<String>,
    pub trusted_evidence_kinds: Vec<String>,
    pub forbidden_evidence_kinds: Vec<String>,
    pub next_step: String,
    pub trust_boundary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct SemanticSupportDiversity {
    pub distinct_task_frame_count: usize,
    pub distinct_project_count: usize,
    pub distinct_source_type_count: usize,
    pub distinct_verifier_type_count: usize,
    pub distinct_evidence_source_count: usize,
    pub support_projects: Vec<String>,
    pub single_task_frame_only: bool,
    pub single_project_only: bool,
    pub single_source_type_only: bool,
    pub single_verifier_type_only: bool,
    pub single_evidence_source_only: bool,
    pub bias_risk: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticReadinessScore {
    pub source: String,
    pub version: String,
    pub score: u8,
    pub max_score: u8,
    pub verdict: String,
    pub meaning: String,
    pub review_required: bool,
    pub blocks_l4_auto_apply: bool,
    pub checks: Vec<SemanticReadinessCheck>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticReadinessCheck {
    pub check_id: String,
    pub passed: bool,
    pub score: u8,
    pub max_score: u8,
    pub evidence_path: String,
    pub note: String,
}

pub fn propose_semantic_consolidations(
    storage: &mut Storage,
    input: SemanticLearningInput,
) -> Result<SemanticLearningReport, StorageError> {
    let limit = input.limit.max(1);
    let min_support = input.min_support.max(2);
    let claims = storage.long_term_claim_records_scoped(
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;
    let inspected_claim_count = claims.len();
    let semantic_group_keys = existing_semantic_group_keys(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;
    let proposal_group_keys = existing_semantic_proposal_group_keys(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
    )?;
    let review_proposal_group_keys = existing_semantic_review_proposal_group_keys(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
    )?;

    let mut groups: HashMap<String, SemanticCandidateGroup> = HashMap::new();
    for claim in &claims {
        let keys = semantic_group_keys_for_text(&claim.text);
        for key in keys {
            groups
                .entry(key.group_key.clone())
                .or_insert_with(|| SemanticCandidateGroup {
                    normalized_text: key.normalized_text.clone(),
                    group_key: key.group_key,
                    group_rule: key.group_rule,
                    claims: Vec::new(),
                })
                .claims
                .push(claim.clone());
        }
    }

    let mut repeated_group_count = 0;
    let mut proposed_count = 0;
    let mut skipped_existing_semantic_count = 0;
    let mut skipped_existing_proposal_count = 0;
    let mut skipped_existing_review_proposal_count = 0;
    let mut skipped_untrusted_count = 0;
    let mut review_candidate_count = 0;
    let mut review_proposed_count = 0;
    let mut items = Vec::new();

    let mut sorted_groups: Vec<SemanticCandidateGroup> =
        groups.into_values().filter(|group| group.claims.len() >= min_support).collect();
    sorted_groups.sort_by(|a, b| {
        b.claims
            .len()
            .cmp(&a.claims.len())
            .then_with(|| group_rule_rank(&a.group_rule).cmp(&group_rule_rank(&b.group_rule)))
            .then_with(|| a.normalized_text.cmp(&b.normalized_text))
    });

    let mut seen_support_sets = HashSet::new();
    let mut consumed_claim_ids = HashSet::new();
    for SemanticCandidateGroup { normalized_text, group_key, group_rule, claims } in sorted_groups {
        let support_set_key = support_set_key(&claims);
        if !seen_support_sets.insert(support_set_key) {
            continue;
        }
        let claim_ids = claims.iter().map(|claim| claim.id).collect::<Vec<_>>();
        if claim_ids.iter().any(|claim_id| consumed_claim_ids.contains(claim_id)) {
            continue;
        }
        if semantic_group_has_mixed_negation_polarity(&claims) {
            continue;
        }
        repeated_group_count += 1;
        let proposal_claim_ids = claim_ids.iter().copied().take(1).collect::<Vec<_>>();
        let evidence_refs = semantic_evidence_refs(storage, &claims)?;
        let support_diversity = semantic_support_diversity(storage, &claims)?;
        let trusted = claims
            .iter()
            .map(|claim| storage.claim_has_durable_promotion_trust(claim.id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|trusted| trusted);

        if semantic_group_keys.contains(&group_key) {
            skipped_existing_semantic_count += 1;
            consumed_claim_ids.extend(claim_ids.iter().copied());
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                None,
                Some("semantic_fact_already_exists"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids,
                proposal_claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: None,
                skipped_reason: Some("semantic_fact_already_exists".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }
        if proposal_group_keys.contains(&group_key) {
            skipped_existing_proposal_count += 1;
            consumed_claim_ids.extend(claim_ids.iter().copied());
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                None,
                Some("semantic_promotion_proposal_already_exists"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids,
                proposal_claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: None,
                skipped_reason: Some("semantic_promotion_proposal_already_exists".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }
        if !trusted {
            skipped_untrusted_count += 1;
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                None,
                Some("durable_promotion_trust_required"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids,
                proposal_claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: None,
                skipped_reason: Some("durable_promotion_trust_required".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }

        let proposal_id = if input.dry_run {
            None
        } else {
            let draft = LearningCriticProposalDraft {
                task_frame_id: claims.iter().find_map(|claim| claim.task_frame_id),
                action: LearningCriticAction::ProposePromotion,
                claim_ids: proposal_claim_ids.clone(),
                target_lifecycle_state: Some(LifecycleState::SemanticFact),
                reason: format!(
                    "Semantic consolidation via {SEMANTIC_LEARNING_RULE}/{group_rule}: {} verified L3 claims repeat `{}`",
                    claim_ids.len(),
                    normalized_text
                ),
                evidence_refs: evidence_refs.clone(),
            };
            Some(storage.insert_learning_critic_proposal(&draft)?)
        };
        proposed_count += 1;
        consumed_claim_ids.extend(claim_ids.iter().copied());
        let readiness_score =
            semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
        let action = if input.dry_run { "would_propose" } else { "proposed" };
        let resolution_plan = semantic_resolution_plan(action, proposal_id, None, &readiness_score);
        items.push(SemanticLearningItem {
            normalized_text,
            group_key,
            group_rule,
            support_count: claim_ids.len(),
            support_claim_ids: claim_ids,
            proposal_claim_ids,
            trusted,
            action: action.to_string(),
            proposal_id,
            skipped_reason: None,
            evidence_refs,
            support_diversity,
            readiness_score,
            resolution_plan,
        });
    }

    let conflict_groups =
        negation_conflict_candidate_groups(&claims, &consumed_claim_ids, min_support);
    let mut review_consumed_claim_ids = HashSet::new();
    for SemanticCandidateGroup { normalized_text, group_key, group_rule, claims } in conflict_groups
    {
        let claim_ids = claims.iter().map(|claim| claim.id).collect::<Vec<_>>();
        if claim_ids.iter().any(|claim_id| review_consumed_claim_ids.contains(claim_id)) {
            continue;
        }
        review_candidate_count += 1;
        let mut evidence_refs = semantic_evidence_refs(storage, &claims)?;
        let support_diversity = semantic_support_diversity(storage, &claims)?;
        let mut seen = evidence_refs
            .iter()
            .map(|evidence_ref| {
                (evidence_ref.kind.clone(), evidence_ref.id.clone(), evidence_ref.source.clone())
            })
            .collect::<HashSet<_>>();
        push_ref(
            &mut evidence_refs,
            &mut seen,
            StoredEvidenceRef {
                kind: SEMANTIC_REVIEW_CANDIDATE_KIND.to_string(),
                id: group_key.clone(),
                source: Some(SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE.to_string()),
            },
        );
        let trusted = claims
            .iter()
            .map(|claim| storage.claim_has_durable_promotion_trust(claim.id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|trusted| trusted);

        if let Some(existing_proposal_id) = review_proposal_group_keys.get(&group_key).copied() {
            skipped_existing_review_proposal_count += 1;
            review_consumed_claim_ids.extend(claim_ids.iter().copied());
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                Some(existing_proposal_id),
                Some("semantic_review_proposal_already_exists"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids.clone(),
                proposal_claim_ids: claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: Some(existing_proposal_id),
                skipped_reason: Some("semantic_review_proposal_already_exists".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }
        if !trusted {
            skipped_untrusted_count += 1;
            review_consumed_claim_ids.extend(claim_ids.iter().copied());
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                None,
                Some("durable_promotion_trust_required"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids.clone(),
                proposal_claim_ids: claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: None,
                skipped_reason: Some("durable_promotion_trust_required".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }

        let proposal_claim_ids = claim_ids.clone();
        let proposal_id = if input.dry_run {
            None
        } else {
            let draft = LearningCriticProposalDraft {
                task_frame_id: claims.iter().find_map(|claim| claim.task_frame_id),
                action: LearningCriticAction::RequestVerification,
                claim_ids: proposal_claim_ids.clone(),
                target_lifecycle_state: None,
                reason: format!(
                    "Negation/conflict semantic review via {SEMANTIC_NEGATION_CONFLICT_RULE}/{group_rule}: {} verified L3 claims share a negation-stripped signature but disagree in polarity; reviewer resolution is required before any L4 proposal",
                    proposal_claim_ids.len()
                ),
                evidence_refs: evidence_refs.clone(),
            };
            Some(storage.insert_learning_critic_proposal(&draft)?)
        };
        review_proposed_count += 1;
        review_consumed_claim_ids.extend(claim_ids.iter().copied());
        let readiness_score =
            semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
        let action = if input.dry_run { "would_request_verification" } else { "review_proposed" };
        let resolution_plan = semantic_resolution_plan(action, proposal_id, None, &readiness_score);
        items.push(SemanticLearningItem {
            normalized_text,
            group_key,
            group_rule,
            support_count: claim_ids.len(),
            support_claim_ids: claim_ids,
            proposal_claim_ids,
            trusted,
            action: action.to_string(),
            proposal_id,
            skipped_reason: None,
            evidence_refs,
            support_diversity,
            readiness_score,
            resolution_plan,
        });
    }

    let mut review_excluded_claim_ids = consumed_claim_ids.clone();
    review_excluded_claim_ids.extend(review_consumed_claim_ids.iter().copied());
    let latent_groups =
        latent_review_candidate_groups(&claims, &review_excluded_claim_ids, min_support);
    let mut latent_consumed_claim_ids = HashSet::new();
    for SemanticCandidateGroup { normalized_text, group_key, group_rule, claims } in latent_groups {
        let claim_ids = claims.iter().map(|claim| claim.id).collect::<Vec<_>>();
        if claim_ids.iter().any(|claim_id| latent_consumed_claim_ids.contains(claim_id)) {
            continue;
        }
        review_candidate_count += 1;
        let mut evidence_refs = semantic_evidence_refs(storage, &claims)?;
        let support_diversity = semantic_support_diversity(storage, &claims)?;
        let mut seen = evidence_refs
            .iter()
            .map(|evidence_ref| {
                (evidence_ref.kind.clone(), evidence_ref.id.clone(), evidence_ref.source.clone())
            })
            .collect::<HashSet<_>>();
        push_ref(
            &mut evidence_refs,
            &mut seen,
            StoredEvidenceRef {
                kind: SEMANTIC_REVIEW_CANDIDATE_KIND.to_string(),
                id: group_key.clone(),
                source: Some(SEMANTIC_LATENT_REVIEW_SOURCE.to_string()),
            },
        );
        let trusted = claims
            .iter()
            .map(|claim| storage.claim_has_durable_promotion_trust(claim.id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|trusted| trusted);

        if let Some(existing_proposal_id) = review_proposal_group_keys.get(&group_key).copied() {
            skipped_existing_review_proposal_count += 1;
            latent_consumed_claim_ids.extend(claim_ids.iter().copied());
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                Some(existing_proposal_id),
                Some("semantic_review_proposal_already_exists"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids.clone(),
                proposal_claim_ids: claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: Some(existing_proposal_id),
                skipped_reason: Some("semantic_review_proposal_already_exists".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }
        if !trusted {
            skipped_untrusted_count += 1;
            let readiness_score =
                semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
            let resolution_plan = semantic_resolution_plan(
                "skip",
                None,
                Some("durable_promotion_trust_required"),
                &readiness_score,
            );
            items.push(SemanticLearningItem {
                normalized_text,
                group_key,
                group_rule,
                support_count: claim_ids.len(),
                support_claim_ids: claim_ids.clone(),
                proposal_claim_ids: claim_ids,
                trusted,
                action: "skip".to_string(),
                proposal_id: None,
                skipped_reason: Some("durable_promotion_trust_required".to_string()),
                evidence_refs,
                support_diversity,
                readiness_score,
                resolution_plan,
            });
            continue;
        }

        let proposal_claim_ids = claim_ids.clone();
        let proposal_id = if input.dry_run {
            None
        } else {
            let draft = LearningCriticProposalDraft {
                task_frame_id: claims.iter().find_map(|claim| claim.task_frame_id),
                action: LearningCriticAction::RequestVerification,
                claim_ids: proposal_claim_ids.clone(),
                target_lifecycle_state: None,
                reason: format!(
                    "Latent/paraphrase semantic review via {SEMANTIC_LATENT_REVIEW_RULE}/{group_rule}: {} verified L3 claims may describe the same abstraction; reviewer confirmation is required before any L4 proposal",
                    proposal_claim_ids.len()
                ),
                evidence_refs: evidence_refs.clone(),
            };
            Some(storage.insert_learning_critic_proposal(&draft)?)
        };
        review_proposed_count += 1;
        latent_consumed_claim_ids.extend(claim_ids.iter().copied());
        let readiness_score =
            semantic_readiness_score(&group_rule, trusted, claim_ids.len(), &support_diversity);
        let action = if input.dry_run { "would_request_verification" } else { "review_proposed" };
        let resolution_plan = semantic_resolution_plan(action, proposal_id, None, &readiness_score);
        items.push(SemanticLearningItem {
            normalized_text,
            group_key,
            group_rule,
            support_count: claim_ids.len(),
            support_claim_ids: claim_ids,
            proposal_claim_ids,
            trusted,
            action: action.to_string(),
            proposal_id,
            skipped_reason: None,
            evidence_refs,
            support_diversity,
            readiness_score,
            resolution_plan,
        });
    }

    let review_lanes = semantic_review_lanes(&items);
    let operator_card = semantic_operator_card(&items, &review_lanes, input.dry_run);
    let (primary_next_command, next_commands) =
        semantic_report_commands(&input, &operator_card, &items, limit, min_support);
    Ok(SemanticLearningReport {
        schema: SEMANTIC_LEARNING_REPORT_SCHEMA.to_string(),
        source: SEMANTIC_LEARNING_SOURCE.to_string(),
        rule: SEMANTIC_LEARNING_RULE.to_string(),
        project: input.project.clone(),
        session_id: input.session_id.clone(),
        limit,
        min_support,
        dry_run: input.dry_run,
        status: operator_card.status.clone(),
        headline: operator_card.headline.clone(),
        primary_lane: operator_card.primary_lane.clone(),
        primary_next_action_id: operator_card.primary_next_action_id.clone(),
        primary_next_action_label: operator_card.primary_next_action_label.clone(),
        operator_next_action_id: operator_card.primary_next_action_id.clone(),
        operator_next_action_label: operator_card.primary_next_action_label.clone(),
        primary_next_command,
        next_commands,
        l4_candidate_count: operator_card.l4_candidate_count,
        review_only_candidate_count: operator_card.review_only_candidate_count,
        blocked_untrusted_count: operator_card.blocked_untrusted_count,
        already_handled_count: operator_card.already_handled_count,
        operator_card,
        review_lanes,
        trust_boundary: semantic_learning_trust_boundary(input.dry_run).to_string(),
        inspected_claim_count,
        repeated_group_count,
        proposed_count,
        skipped_existing_semantic_count,
        skipped_existing_proposal_count,
        skipped_existing_review_proposal_count,
        skipped_untrusted_count,
        review_candidate_count,
        review_proposed_count,
        items,
    })
}

fn semantic_report_commands(
    input: &SemanticLearningInput,
    operator_card: &SemanticLearningOperatorCard,
    items: &[SemanticLearningItem],
    limit: usize,
    min_support: usize,
) -> (Vec<String>, Vec<Vec<String>>) {
    let create_or_propose = semantic_proposal_creation_command(input, limit, min_support);
    let dry_run = semantic_proposal_dry_run_command(input, limit, min_support);
    let review_queue = semantic_scoped_command(
        input,
        &["soma", "context", "review-queue", "--format", "json", "--limit", "20"],
    );
    let review_report =
        semantic_scoped_command(input, &["soma", "context", "review-report", "--format", "json"]);
    let review_actions =
        semantic_scoped_command(input, &["soma", "context", "review-actions", "--format", "json"]);
    let verification_template = semantic_verification_command_template();
    let has_open_review_row = input.dry_run
        && (items.iter().any(|item| item.proposal_id.is_some())
            || items.iter().any(|item| {
                item.skipped_reason.as_deref() == Some("semantic_review_proposal_already_exists")
                    || item.skipped_reason.as_deref() == Some("semantic_proposal_already_exists")
            }));
    let needs_proposal_row = input.dry_run
        && items.iter().any(|item| {
            item.proposal_id.is_none()
                && matches!(item.action.as_str(), "would_propose" | "would_request_verification")
                && item.skipped_reason.is_none()
        });
    let primary = match operator_card.primary_lane.as_str() {
        "manual_l4_review" if needs_proposal_row => create_or_propose.clone(),
        "manual_l4_review" => review_queue.clone(),
        "review_only_resolution" if needs_proposal_row && !has_open_review_row => {
            create_or_propose.clone()
        }
        "review_only_resolution" => review_actions.clone(),
        "blocked_untrusted_support" => verification_template.clone(),
        "already_handled" => review_report.clone(),
        _ if !items.is_empty() => dry_run.clone(),
        _ => vec!["soma".to_string(), "learning".to_string(), "--json".to_string()],
    };

    let mut next_commands = Vec::new();
    push_semantic_command_once(&mut next_commands, primary.clone());
    if needs_proposal_row {
        push_semantic_command_once(&mut next_commands, create_or_propose);
    }
    if has_open_review_row || operator_card.review_only_candidate_count > 0 {
        push_semantic_command_once(&mut next_commands, review_actions);
    }
    if operator_card.l4_candidate_count > 0 || has_open_review_row {
        push_semantic_command_once(&mut next_commands, review_queue);
    }
    push_semantic_command_once(&mut next_commands, review_report);
    if operator_card.blocked_untrusted_count > 0 {
        push_semantic_command_once(&mut next_commands, verification_template);
    }
    push_semantic_command_once(&mut next_commands, dry_run);
    (primary, next_commands)
}

fn semantic_proposal_creation_command(
    input: &SemanticLearningInput,
    limit: usize,
    min_support: usize,
) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "semantic-proposals".to_string(),
        "--brief".to_string(),
        "--min-support".to_string(),
        min_support.to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ];
    append_semantic_scope(input, &mut command);
    command
}

fn semantic_proposal_dry_run_command(
    input: &SemanticLearningInput,
    limit: usize,
    min_support: usize,
) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "semantic-proposals".to_string(),
        "--dry-run".to_string(),
        "--brief".to_string(),
        "--min-support".to_string(),
        min_support.to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ];
    append_semantic_scope(input, &mut command);
    command
}

fn semantic_scoped_command(input: &SemanticLearningInput, parts: &[&str]) -> Vec<String> {
    let mut command = parts.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
    append_semantic_scope(input, &mut command);
    command
}

fn append_semantic_scope(input: &SemanticLearningInput, command: &mut Vec<String>) {
    if let Some(project) = input.project.as_deref() {
        command.push("--project".to_string());
        command.push(project.to_string());
    }
    if let Some(session_id) = input.session_id.as_deref() {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
}

fn semantic_verification_command_template() -> Vec<String> {
    vec![
        "soma".to_string(),
        "context".to_string(),
        "verify-claim".to_string(),
        "--claim-id".to_string(),
        "CLAIM_ID".to_string(),
        "--verifier".to_string(),
        "TRUSTED_VERIFIER".to_string(),
        "--result".to_string(),
        "VERIFICATION_RESULT".to_string(),
        "--evidence-kind".to_string(),
        "TRUSTED_EVIDENCE_KIND".to_string(),
        "--evidence-id".to_string(),
        "TRUSTED_EVIDENCE_ID".to_string(),
    ]
}

fn push_semantic_command_once(commands: &mut Vec<Vec<String>>, command: Vec<String>) {
    if !commands.iter().any(|existing| existing == &command) {
        commands.push(command);
    }
}

fn semantic_operator_card(
    items: &[SemanticLearningItem],
    lanes: &[SemanticReviewLane],
    dry_run: bool,
) -> SemanticLearningOperatorCard {
    let l4_candidate_count = semantic_l4_candidate_indexes(items).len();
    let review_only_candidate_count = semantic_review_only_indexes(items).len();
    let blocked_untrusted_count = semantic_blocked_untrusted_indexes(items).len();
    let already_handled_count = semantic_already_handled_indexes(items).len();
    let (status, headline, primary_lane, action_id, action_label) = if blocked_untrusted_count > 0 {
        (
            "blocked_untrusted_support",
            format!(
                "{blocked_untrusted_count} candidate(s) need independent verification and L3 trust before semantic learning."
            ),
            "blocked_untrusted_support",
            "record_independent_verification_and_promote_l3",
            "Record independent verification and L3 trust",
        )
    } else if review_only_candidate_count > 0 {
        (
            "semantic_review_only_pending",
            format!(
                "{review_only_candidate_count} review-only candidate(s) need user/tool/local/correction resolution before L4 learning."
            ),
            "review_only_resolution",
            "resolve_semantic_review_candidate_with_independent_evidence",
            "Resolve review-only semantic candidate",
        )
    } else if l4_candidate_count > 0 {
        (
            "manual_l4_review_pending",
            format!(
                "{l4_candidate_count} L4 semantic candidate(s) need manual review before semantic memory changes."
            ),
            "manual_l4_review",
            if dry_run {
                "create_l4_semantic_proposal_for_review"
            } else {
                "inspect_l4_semantic_proposal_review_gate"
            },
            if dry_run {
                "Create L4 proposal for review"
            } else {
                "Inspect L4 proposal review gate"
            },
        )
    } else if items.is_empty() {
        (
            "clear",
            "No repeated verified L3 semantic candidates were found.".to_string(),
            "none",
            "no_semantic_review_action",
            "No semantic review action",
        )
    } else {
        (
            "no_new_l4_candidates",
            "No new semantic promotion candidate is ready.".to_string(),
            "already_handled",
            "inspect_existing_semantic_or_review_row",
            "Inspect existing semantic or review row",
        )
    };
    let primary_lane = if lanes.iter().any(|lane| lane.id == primary_lane) {
        primary_lane.to_string()
    } else {
        "none".to_string()
    };
    let mut safe_to_claim = vec![
        "semantic-proposals never applies L4 semantic memory directly".to_string(),
        "cloud drafts and client render text are forbidden semantic evidence".to_string(),
    ];
    if dry_run {
        safe_to_claim.push("dry-run records no proposal rows".to_string());
    } else {
        safe_to_claim.push("non-dry-run may create review/proposal rows only".to_string());
    }
    SemanticLearningOperatorCard {
        source: "soma_semantic_operator_card.v1".to_string(),
        status: status.to_string(),
        headline,
        primary_lane,
        primary_next_action_id: action_id.to_string(),
        primary_next_action_label: action_label.to_string(),
        l4_candidate_count,
        review_only_candidate_count,
        blocked_untrusted_count,
        already_handled_count,
        records_verification: false,
        writes_semantic_fact: false,
        promotes_cloud_draft: false,
        safe_to_claim,
        blocked_claims: vec![
            "No L4 semantic_fact/policy/belief write occurs without review apply gates.".to_string(),
            "Cloud output, assistant drafts, and raw client render text cannot verify semantic learning.".to_string(),
        ],
        trust_boundary: semantic_learning_trust_boundary(dry_run).to_string(),
    }
}

fn semantic_review_lanes(items: &[SemanticLearningItem]) -> Vec<SemanticReviewLane> {
    vec![
        semantic_review_lane(
            "manual_l4_review",
            "Manual L4 Review",
            semantic_l4_candidate_indexes(items),
            "inspect_l4_candidate_support",
            "Inspect repeated verified L3 support",
            vec![
                "auto_apply_without_review",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
        ),
        semantic_review_lane(
            "review_only_resolution",
            "Review-Only Resolution",
            semantic_review_only_indexes(items),
            "resolve_with_independent_evidence",
            "Accept, reject, or wait with independent evidence",
            vec![
                "apply_review_only_candidate_as_l4_fact",
                "confirm_and_apply_review_only_candidate",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
        ),
        semantic_review_lane(
            "blocked_untrusted_support",
            "Blocked Untrusted Support",
            semantic_blocked_untrusted_indexes(items),
            "record_independent_verification_and_promote_l3",
            "Record independent verification and L3 trust",
            vec![
                "create_l4_proposal",
                "apply_semantic_fact",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
        ),
        semantic_review_lane(
            "already_handled",
            "Already Handled",
            semantic_already_handled_indexes(items),
            "inspect_existing_semantic_or_review_row",
            "Inspect existing semantic or review row",
            vec!["create_duplicate_proposal", "use_cloud_output_as_evidence"],
        ),
    ]
}

fn semantic_review_lane(
    id: &str,
    label: &str,
    item_indexes: Vec<usize>,
    next_action_id: &str,
    next_action_label: &str,
    blocked_resolution_actions: Vec<&str>,
) -> SemanticReviewLane {
    SemanticReviewLane {
        source: "soma_semantic_review_lane.v1".to_string(),
        id: id.to_string(),
        label: label.to_string(),
        count: item_indexes.len(),
        item_indexes,
        next_action_id: next_action_id.to_string(),
        next_action_label: next_action_label.to_string(),
        records_verification: false,
        writes_semantic_fact: false,
        promotes_cloud_draft: false,
        blocked_resolution_actions: blocked_resolution_actions.into_iter().map(str::to_string).collect(),
        trust_boundary: "semantic_review_lane_is_read_only: lane summaries route operator attention only; they record no verification, apply no proposal, write no semantic_fact, and promote no cloud draft".to_string(),
    }
}

fn semantic_l4_candidate_indexes(items: &[SemanticLearningItem]) -> Vec<usize> {
    semantic_item_indexes(items, semantic_item_is_l4_candidate)
}

fn semantic_review_only_indexes(items: &[SemanticLearningItem]) -> Vec<usize> {
    semantic_item_indexes(items, semantic_item_is_review_only_candidate)
}

fn semantic_blocked_untrusted_indexes(items: &[SemanticLearningItem]) -> Vec<usize> {
    semantic_item_indexes(items, semantic_item_is_blocked_untrusted)
}

fn semantic_already_handled_indexes(items: &[SemanticLearningItem]) -> Vec<usize> {
    semantic_item_indexes(items, semantic_item_is_already_handled)
}

fn semantic_item_indexes(
    items: &[SemanticLearningItem],
    predicate: fn(&SemanticLearningItem) -> bool,
) -> Vec<usize> {
    items.iter().enumerate().filter_map(|(index, item)| predicate(item).then_some(index)).collect()
}

fn semantic_item_is_l4_candidate(item: &SemanticLearningItem) -> bool {
    matches!(item.action.as_str(), "would_propose" | "proposed")
        && matches!(item.group_rule.as_str(), SEMANTIC_EXACT_GROUP_RULE | SEMANTIC_TOKEN_GROUP_RULE)
}

fn semantic_item_is_review_only_candidate(item: &SemanticLearningItem) -> bool {
    matches!(item.action.as_str(), "would_request_verification" | "review_proposed")
        || item.readiness_score.verdict == "review_only_requires_resolution"
}

fn semantic_item_is_blocked_untrusted(item: &SemanticLearningItem) -> bool {
    item.skipped_reason.as_deref() == Some("durable_promotion_trust_required") || !item.trusted
}

fn semantic_item_is_already_handled(item: &SemanticLearningItem) -> bool {
    !semantic_item_is_l4_candidate(item)
        && !semantic_item_is_review_only_candidate(item)
        && !semantic_item_is_blocked_untrusted(item)
        && (item.skipped_reason.as_deref().is_some_and(|reason| reason.contains("already_exists"))
            || matches!(item.action.as_str(), "skip"))
}

fn semantic_learning_trust_boundary(dry_run: bool) -> &'static str {
    if dry_run {
        "semantic_proposals_dry_run_is_read_only: records no proposal, verification, promotion, correction, L4 semantic write, or cloud-draft trust"
    } else {
        "semantic_proposals_may_create_review_rows_only: never applies L4 semantic memory, records verification, or promotes cloud drafts without user/tool/local/correction evidence"
    }
}

fn semantic_resolution_plan(
    action: &str,
    proposal_id: Option<i64>,
    skipped_reason: Option<&str>,
    readiness_score: &SemanticReadinessScore,
) -> SemanticResolutionPlan {
    struct PlanParts {
        status: &'static str,
        target_lifecycle_state: &'static str,
        intent: &'static str,
        allowed_resolution_actions: Vec<&'static str>,
        blocked_resolution_actions: Vec<&'static str>,
        next_step: &'static str,
    }

    let review_only = matches!(action, "would_request_verification" | "review_proposed")
        || readiness_score.verdict == "review_only_requires_resolution";
    let l4_candidate = matches!(action, "would_propose" | "proposed");
    let untrusted = skipped_reason == Some("durable_promotion_trust_required");
    let existing = skipped_reason.is_some() && !untrusted;
    let parts = if review_only {
        PlanParts {
            status: "review_only_resolution_required",
            target_lifecycle_state: "none",
            intent: "Resolve the review-only semantic candidate as audit metadata before any separate L4 proposal can exist.",
            allowed_resolution_actions: vec![
                "accept_with_independent_evidence",
                "reject_with_reason",
                "wait_for_more_evidence",
            ],
            blocked_resolution_actions: vec![
                "apply_review_only_candidate_as_l4_fact",
                "confirm_and_apply_review_only_candidate",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
            next_step: if proposal_id.is_some()
                || skipped_reason == Some("semantic_review_proposal_already_exists")
            {
                "Open the existing review proposal, inspect support, then choose accept, reject, or wait with independent evidence."
            } else {
                "Create the review proposal by rerunning semantic-proposals without --dry-run, then verify or resolve with independent evidence."
            },
        }
    } else if l4_candidate {
        PlanParts {
            status: "manual_l4_review_required",
            target_lifecycle_state: "semantic_fact",
            intent: "Inspect repeated verified L3 support before allowing any L4 semantic_fact proposal to be applied.",
            allowed_resolution_actions: vec![
                "create_or_open_l4_proposal",
                "inspect_support_evidence",
                "apply_after_review_gate_passes",
            ],
            blocked_resolution_actions: vec![
                "auto_apply_without_review",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
            next_step: if proposal_id.is_some() {
                "Open the L4 proposal in review-queue and apply only after the semantic review gate passes."
            } else {
                "Create the L4 proposal by rerunning semantic-proposals without --dry-run, then inspect review-queue before apply."
            },
        }
    } else if untrusted {
        PlanParts {
            status: "blocked_until_trusted_l3_support",
            target_lifecycle_state: "none",
            intent: "The candidate lacks durable L3 promotion trust and cannot be used for semantic learning yet.",
            allowed_resolution_actions: vec![
                "record_independent_verification",
                "promote_verified_claims_to_l3",
            ],
            blocked_resolution_actions: vec![
                "create_l4_proposal",
                "apply_semantic_fact",
                "use_cloud_output_as_evidence",
                "use_client_render_text_as_evidence",
            ],
            next_step: "Record independent verification and durable L3 promotion trust before rerunning semantic-proposals.",
        }
    } else if existing {
        PlanParts {
            status: "already_handled",
            target_lifecycle_state: "none",
            intent: "A semantic fact or review proposal already covers this candidate.",
            allowed_resolution_actions: vec!["inspect_existing_semantic_or_review_row"],
            blocked_resolution_actions: vec!["create_duplicate_proposal", "use_cloud_output_as_evidence"],
            next_step: "Inspect the existing semantic fact or review proposal instead of creating a duplicate.",
        }
    } else {
        PlanParts {
            status: "not_applicable",
            target_lifecycle_state: "none",
            intent: "No semantic resolution action is available for this item.",
            allowed_resolution_actions: vec!["inspect_evidence"],
            blocked_resolution_actions: vec!["apply_semantic_fact", "use_cloud_output_as_evidence"],
            next_step: "Inspect evidence and rerun semantic-proposals after stronger trusted support exists.",
        }
    };

    SemanticResolutionPlan {
        source: "soma_semantic_resolution_plan.v1".to_string(),
        status: parts.status.to_string(),
        target_lifecycle_state: parts.target_lifecycle_state.to_string(),
        intent: parts.intent.to_string(),
        allowed_resolution_actions: parts
            .allowed_resolution_actions
            .into_iter()
            .map(str::to_string)
            .collect(),
        blocked_resolution_actions: parts
            .blocked_resolution_actions
            .into_iter()
            .map(str::to_string)
            .collect(),
        trusted_verifier_types: vec![
            "user".to_string(),
            "tool".to_string(),
            "test".to_string(),
            "local_observation".to_string(),
            "correction".to_string(),
        ],
        trusted_evidence_kinds: vec![
            "user_correction".to_string(),
            "test_result".to_string(),
            "tool_output".to_string(),
            "local_observation".to_string(),
            "source_document".to_string(),
        ],
        forbidden_evidence_kinds: vec![
            "cloud_output".to_string(),
            "assistant_draft".to_string(),
            "client_render_text".to_string(),
            "unverified_claim".to_string(),
        ],
        next_step: parts.next_step.to_string(),
        trust_boundary:
            "semantic_resolution_plan_is_read_only: explains allowed review paths only; records no verification, applies no proposal, writes no semantic_fact, and cannot promote cloud output or client render text"
                .to_string(),
    }
}

#[derive(Debug, Clone)]
struct SemanticCandidateGroup {
    normalized_text: String,
    group_key: String,
    group_rule: String,
    claims: Vec<StoredClaimRecord>,
}

#[derive(Debug, Clone)]
struct SemanticGroupKey {
    normalized_text: String,
    group_key: String,
    group_rule: String,
}

fn existing_semantic_group_keys(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<HashSet<String>, StorageError> {
    Ok(storage
        .semantic_claim_records_scoped(project, session_id, limit.saturating_mul(10).max(limit))?
        .into_iter()
        .flat_map(|claim| {
            semantic_group_keys_for_text(&claim.text).into_iter().map(|key| key.group_key)
        })
        .collect())
}

fn existing_semantic_proposal_group_keys(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<HashSet<String>, StorageError> {
    let proposals = storage.learning_critic_proposals_scoped(
        project,
        session_id,
        None,
        DEFAULT_PROPOSAL_SCAN_LIMIT,
    )?;
    Ok(proposals
        .into_iter()
        .filter(is_semantic_promotion_proposal)
        .flat_map(|proposal| proposal.claim_ids.into_iter())
        .filter_map(|claim_id| storage.claim_record(claim_id).ok().flatten())
        .flat_map(|claim| {
            semantic_group_keys_for_text(&claim.text).into_iter().map(|key| key.group_key)
        })
        .collect())
}

fn existing_semantic_review_proposal_group_keys(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<HashMap<String, i64>, StorageError> {
    let proposals = storage.learning_critic_proposals_scoped(
        project,
        session_id,
        None,
        DEFAULT_PROPOSAL_SCAN_LIMIT,
    )?;
    let mut group_keys = HashMap::new();
    for proposal in proposals.into_iter().filter(|proposal| {
        proposal.action == LearningCriticAction::RequestVerification
            && proposal.status != LearningCriticProposalStatus::Rejected
    }) {
        for evidence_ref in proposal.evidence_refs.into_iter().filter(|evidence_ref| {
            evidence_ref.kind == SEMANTIC_REVIEW_CANDIDATE_KIND
                && (evidence_ref.source.as_deref() == Some(SEMANTIC_LATENT_REVIEW_SOURCE)
                    || evidence_ref.source.as_deref()
                        == Some(SEMANTIC_NEGATION_CONFLICT_REVIEW_SOURCE))
        }) {
            group_keys.entry(evidence_ref.id).or_insert(proposal.id);
        }
    }
    Ok(group_keys)
}

fn is_semantic_promotion_proposal(proposal: &StoredLearningCriticProposal) -> bool {
    proposal.action == LearningCriticAction::ProposePromotion
        && proposal.target_lifecycle_state == Some(LifecycleState::SemanticFact)
        && proposal.status != LearningCriticProposalStatus::Applied
}

fn semantic_evidence_refs(
    storage: &Storage,
    claims: &[StoredClaimRecord],
) -> Result<Vec<StoredEvidenceRef>, StorageError> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for claim in claims {
        push_ref(
            &mut refs,
            &mut seen,
            StoredEvidenceRef {
                kind: "claim_record".to_string(),
                id: claim.id.to_string(),
                source: Some(SEMANTIC_LEARNING_SOURCE.to_string()),
            },
        );
        for event in storage.verification_events_for_claim(claim.id)? {
            if event.result == VerificationResult::Confirmed {
                push_ref(
                    &mut refs,
                    &mut seen,
                    StoredEvidenceRef {
                        kind: "verification_event".to_string(),
                        id: event.id.to_string(),
                        source: Some(event.verifier_type.as_str().to_string()),
                    },
                );
                push_ref(&mut refs, &mut seen, event.evidence_ref);
            }
        }
        for evidence_ref in &claim.evidence_refs {
            push_ref(&mut refs, &mut seen, evidence_ref.clone());
        }
    }
    Ok(refs)
}

pub fn semantic_support_diversity(
    storage: &Storage,
    claims: &[StoredClaimRecord],
) -> Result<SemanticSupportDiversity, StorageError> {
    let support_count = claims.len();
    let mut task_frames = HashSet::new();
    let mut projects = BTreeSet::new();
    let mut source_types = HashSet::new();
    let mut verifier_types = HashSet::new();
    let mut evidence_sources = HashSet::new();

    for claim in claims {
        if let Some(task_frame_id) = claim.task_frame_id {
            task_frames.insert(task_frame_id);
            if let Some(task_frame) = storage.task_frame(task_frame_id)? {
                if let Some(project) =
                    task_frame.project.as_deref().filter(|value| !value.is_empty())
                {
                    projects.insert(project.to_string());
                }
            }
        }
        source_types.insert(claim.source_type.as_str().to_string());
        for evidence_ref in &claim.evidence_refs {
            if let Some(source) = evidence_ref.source.as_deref().filter(|source| !source.is_empty())
            {
                evidence_sources.insert(source.to_string());
            }
        }
        for event in storage.verification_events_for_claim(claim.id)? {
            if event.result == VerificationResult::Confirmed {
                verifier_types.insert(event.verifier_type.as_str().to_string());
                if let Some(source) =
                    event.evidence_ref.source.as_deref().filter(|source| !source.is_empty())
                {
                    evidence_sources.insert(source.to_string());
                }
            }
        }
    }

    let single_task_frame_only = support_count > 1 && task_frames.len() <= 1;
    let single_project_only = support_count > 1 && projects.len() <= 1;
    let single_source_type_only = support_count > 1 && source_types.len() <= 1;
    let single_verifier_type_only = support_count > 1 && verifier_types.len() <= 1;
    let single_evidence_source_only = support_count > 1 && evidence_sources.len() <= 1;
    let support_projects = projects.iter().cloned().collect::<Vec<_>>();
    let bias_risk = semantic_support_bias_risk(
        support_count,
        &task_frames,
        &source_types,
        &verifier_types,
        &evidence_sources,
    );

    Ok(SemanticSupportDiversity {
        distinct_task_frame_count: task_frames.len(),
        distinct_project_count: support_projects.len(),
        distinct_source_type_count: source_types.len(),
        distinct_verifier_type_count: verifier_types.len(),
        distinct_evidence_source_count: evidence_sources.len(),
        support_projects,
        single_task_frame_only,
        single_project_only,
        single_source_type_only,
        single_verifier_type_only,
        single_evidence_source_only,
        bias_risk,
    })
}

fn semantic_support_bias_risk(
    support_count: usize,
    task_frames: &HashSet<i64>,
    source_types: &HashSet<String>,
    verifier_types: &HashSet<String>,
    evidence_sources: &HashSet<String>,
) -> String {
    if support_count < 2 {
        return "insufficient_support".to_string();
    }
    if task_frames.len() <= 1 && source_types.len() <= 1 && verifier_types.len() <= 1 {
        return "high_single_context".to_string();
    }
    if evidence_sources.len() <= 1 && (task_frames.len() <= 1 || source_types.len() <= 1) {
        return "high_single_context".to_string();
    }
    if task_frames.len() <= 1
        || source_types.len() <= 1
        || verifier_types.len() <= 1
        || evidence_sources.len() <= 1
    {
        return "medium_limited_diversity".to_string();
    }
    "low_diverse_support".to_string()
}

pub fn semantic_readiness_score(
    group_rule: &str,
    trusted: bool,
    support_count: usize,
    support_diversity: &SemanticSupportDiversity,
) -> SemanticReadinessScore {
    let support_passed = support_count >= 2;
    let l4_target_rule =
        matches!(group_rule, SEMANTIC_EXACT_GROUP_RULE | SEMANTIC_TOKEN_GROUP_RULE);
    let conflict_free = group_rule != SEMANTIC_NEGATION_CONFLICT_GROUP_RULE;
    let diversity_points = match support_diversity.bias_risk.as_str() {
        "low_diverse_support" => 20,
        "medium_limited_diversity" => 10,
        "high_single_context" => 5,
        _ => 0,
    };
    let checks = vec![
        SemanticReadinessCheck {
            check_id: "repeated_support_present".to_string(),
            passed: support_passed,
            score: if support_passed { 20 } else { 0 },
            max_score: 20,
            evidence_path: "semantic_item.support_claim_ids".to_string(),
            note: format!("{support_count} support claim(s) are attached"),
        },
        SemanticReadinessCheck {
            check_id: "durable_promotion_trust".to_string(),
            passed: trusted,
            score: if trusted { 30 } else { 0 },
            max_score: 30,
            evidence_path: "claim_records.verification_events".to_string(),
            note: if trusted {
                "all support claims have durable L3/L4 promotion trust".to_string()
            } else {
                "untrusted support cannot become L4 semantic memory".to_string()
            },
        },
        SemanticReadinessCheck {
            check_id: "l4_target_grouping_rule".to_string(),
            passed: l4_target_rule,
            score: if l4_target_rule { 20 } else { 0 },
            max_score: 20,
            evidence_path: "semantic_item.group_rule".to_string(),
            note: if l4_target_rule {
                format!("{group_rule} may propose L4 review")
            } else {
                format!("{group_rule} is review-only and has no L4 target")
            },
        },
        SemanticReadinessCheck {
            check_id: "support_diversity".to_string(),
            passed: support_diversity.bias_risk == "low_diverse_support",
            score: diversity_points,
            max_score: 20,
            evidence_path: "semantic_item.support_diversity".to_string(),
            note: format!(
                "bias_risk={} projects={:?} single_project_only={} single_evidence_source_only={}",
                support_diversity.bias_risk,
                support_diversity.support_projects,
                support_diversity.single_project_only,
                support_diversity.single_evidence_source_only
            ),
        },
        SemanticReadinessCheck {
            check_id: "conflict_free_l4_target".to_string(),
            passed: conflict_free,
            score: if conflict_free { 10 } else { 0 },
            max_score: 10,
            evidence_path: "semantic_item.group_rule".to_string(),
            note: if conflict_free {
                "no negation-conflict grouping rule detected".to_string()
            } else {
                "negation conflict must be resolved before any L4 proposal".to_string()
            },
        },
    ];
    let score = checks.iter().map(|check| check.score).sum();
    let review_required = !trusted || !l4_target_rule || !conflict_free || diversity_points < 20;
    let blocks_l4_auto_apply =
        !trusted || !l4_target_rule || !conflict_free || diversity_points < 20;
    let verdict = if !trusted {
        "blocked_untrusted_support"
    } else if !l4_target_rule || !conflict_free {
        "review_only_requires_resolution"
    } else if diversity_points < 20 {
        "manual_l4_review_required"
    } else {
        "ready_for_l4_review"
    };
    SemanticReadinessScore {
        source: "soma_semantic_readiness_v1".to_string(),
        version: "1".to_string(),
        score,
        max_score: 100,
        verdict: verdict.to_string(),
        meaning: "evidence_readiness_not_truth_probability".to_string(),
        review_required,
        blocks_l4_auto_apply,
        checks,
    }
}

fn push_ref(
    refs: &mut Vec<StoredEvidenceRef>,
    seen: &mut HashSet<(String, String, Option<String>)>,
    evidence_ref: StoredEvidenceRef,
) {
    let key = (evidence_ref.kind.clone(), evidence_ref.id.clone(), evidence_ref.source.clone());
    if seen.insert(key) {
        refs.push(evidence_ref);
    }
}

fn normalize_claim_text(text: &str) -> Option<String> {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '.' || c == '!' || c == '?' || c == ';' || c == ':')
        .to_ascii_lowercase();
    (normalized.len() >= 24).then_some(normalized)
}

fn semantic_group_keys_for_text(text: &str) -> Vec<SemanticGroupKey> {
    let Some(normalized_text) = normalize_claim_text(text) else {
        return Vec::new();
    };
    let mut keys = vec![SemanticGroupKey {
        normalized_text: normalized_text.clone(),
        group_key: format!("{SEMANTIC_EXACT_GROUP_RULE}:{normalized_text}"),
        group_rule: SEMANTIC_EXACT_GROUP_RULE.to_string(),
    }];
    if let Some(signature) = conservative_token_signature(&normalized_text) {
        keys.push(SemanticGroupKey {
            normalized_text,
            group_key: format!("{SEMANTIC_TOKEN_GROUP_RULE}:{signature}"),
            group_rule: SEMANTIC_TOKEN_GROUP_RULE.to_string(),
        });
    }
    keys
}

fn semantic_group_has_mixed_negation_polarity(claims: &[StoredClaimRecord]) -> bool {
    let mut has_affirmed = false;
    let mut has_negated = false;
    for claim in claims {
        if let Some(normalized_text) = normalize_claim_text(&claim.text) {
            match semantic_negation_polarity(&normalized_text) {
                SemanticNegationPolarity::Affirmed => has_affirmed = true,
                SemanticNegationPolarity::Negated => has_negated = true,
            }
        }
    }
    has_affirmed && has_negated
}

fn negation_conflict_candidate_groups(
    claims: &[StoredClaimRecord],
    consumed_claim_ids: &HashSet<i64>,
    min_support: usize,
) -> Vec<SemanticCandidateGroup> {
    let mut grouped: HashMap<String, Vec<(StoredClaimRecord, String, SemanticNegationPolarity)>> =
        HashMap::new();
    for claim in claims {
        if consumed_claim_ids.contains(&claim.id) {
            continue;
        }
        let Some(normalized_text) = normalize_claim_text(&claim.text) else {
            continue;
        };
        let Some(signature) = negation_stripped_token_signature(&normalized_text) else {
            continue;
        };
        grouped.entry(signature).or_default().push((
            claim.clone(),
            normalized_text.clone(),
            semantic_negation_polarity(&normalized_text),
        ));
    }

    let mut groups = Vec::new();
    for (signature, entries) in grouped {
        if entries.len() < min_support {
            continue;
        }
        let has_affirmed =
            entries.iter().any(|(_, _, polarity)| *polarity == SemanticNegationPolarity::Affirmed);
        let has_negated =
            entries.iter().any(|(_, _, polarity)| *polarity == SemanticNegationPolarity::Negated);
        if !(has_affirmed && has_negated) {
            continue;
        }
        let mut claims = entries.iter().map(|(claim, _, _)| claim.clone()).collect::<Vec<_>>();
        claims.sort_by_key(|claim| claim.id);
        let normalized_text = entries
            .iter()
            .map(|(_, normalized_text, _)| normalized_text.clone())
            .min()
            .unwrap_or_default();
        groups.push(SemanticCandidateGroup {
            normalized_text,
            group_key: format!("{SEMANTIC_NEGATION_CONFLICT_GROUP_RULE}:{signature}"),
            group_rule: SEMANTIC_NEGATION_CONFLICT_GROUP_RULE.to_string(),
            claims,
        });
    }
    groups.sort_by(|a, b| {
        b.claims.len().cmp(&a.claims.len()).then_with(|| a.normalized_text.cmp(&b.normalized_text))
    });
    groups
}

fn latent_review_candidate_groups(
    claims: &[StoredClaimRecord],
    consumed_claim_ids: &HashSet<i64>,
    min_support: usize,
) -> Vec<SemanticCandidateGroup> {
    let candidates = claims
        .iter()
        .filter(|claim| !consumed_claim_ids.contains(&claim.id))
        .filter_map(|claim| {
            let normalized_text = normalize_claim_text(&claim.text)?;
            let tokens = review_candidate_tokens(&normalized_text)?;
            Some((claim.clone(), normalized_text, tokens))
        })
        .collect::<Vec<_>>();

    let mut groups = Vec::new();
    let mut seen_support_sets = HashSet::new();
    for (anchor_index, (anchor_claim, anchor_text, anchor_tokens)) in candidates.iter().enumerate()
    {
        let mut group_claims = vec![anchor_claim.clone()];
        let mut shared_anchor_tokens = anchor_tokens.clone();
        for (candidate_index, (candidate_claim, _candidate_text, candidate_tokens)) in
            candidates.iter().enumerate()
        {
            if candidate_index == anchor_index {
                continue;
            }
            if latent_token_overlap_candidate(anchor_tokens, candidate_tokens) {
                group_claims.push(candidate_claim.clone());
                shared_anchor_tokens = shared_tokens(&shared_anchor_tokens, candidate_tokens);
            }
        }
        if group_claims.len() < min_support {
            continue;
        }
        let support_set = support_set_key(&group_claims);
        if !seen_support_sets.insert(support_set.clone()) {
            continue;
        }
        let token_key = if shared_anchor_tokens.len() >= 4 {
            shared_anchor_tokens.join(" ")
        } else {
            support_set
        };
        groups.push(SemanticCandidateGroup {
            normalized_text: anchor_text.clone(),
            group_key: format!("{SEMANTIC_LATENT_REVIEW_GROUP_RULE}:{token_key}"),
            group_rule: SEMANTIC_LATENT_REVIEW_GROUP_RULE.to_string(),
            claims: group_claims,
        });
    }
    groups.sort_by(|a, b| {
        b.claims.len().cmp(&a.claims.len()).then_with(|| a.normalized_text.cmp(&b.normalized_text))
    });
    groups
}

fn review_candidate_tokens(normalized_text: &str) -> Option<Vec<String>> {
    let mut tokens = normalized_text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter_map(signature_token)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    (tokens.len() >= 5).then_some(tokens)
}

fn latent_token_overlap_candidate(left: &[String], right: &[String]) -> bool {
    if left == right {
        return false;
    }
    let shared_count = shared_token_count(left, right);
    if shared_count < 4 {
        return false;
    }
    let min_len = left.len().min(right.len()).max(1);
    let max_len = left.len().max(right.len()).max(1);
    let overlap_min = shared_count as f32 / min_len as f32;
    let overlap_max = shared_count as f32 / max_len as f32;
    overlap_min >= 0.55 && overlap_max >= 0.40
}

fn shared_token_count(left: &[String], right: &[String]) -> usize {
    let right = right.iter().collect::<HashSet<_>>();
    left.iter().filter(|token| right.contains(token)).count()
}

fn shared_tokens(left: &[String], right: &[String]) -> Vec<String> {
    let right = right.iter().collect::<HashSet<_>>();
    left.iter().filter(|token| right.contains(token)).cloned().collect()
}

fn conservative_token_signature(normalized_text: &str) -> Option<String> {
    let mut tokens = normalized_text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter_map(signature_token)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    (tokens.len() >= 5).then(|| tokens.join(" "))
}

fn negation_stripped_token_signature(normalized_text: &str) -> Option<String> {
    let mut tokens = semantic_word_tokens(normalized_text)
        .into_iter()
        .filter(|token| !is_semantic_negation_token(token))
        .filter_map(|token| signature_token(&token))
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    (tokens.len() >= 5).then(|| tokens.join(" "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticNegationPolarity {
    Affirmed,
    Negated,
}

fn semantic_negation_polarity(normalized_text: &str) -> SemanticNegationPolarity {
    if semantic_word_tokens(normalized_text).iter().any(|token| is_semantic_negation_token(token)) {
        SemanticNegationPolarity::Negated
    } else {
        SemanticNegationPolarity::Affirmed
    }
}

fn semantic_word_tokens(normalized_text: &str) -> Vec<String> {
    let expanded = normalized_text
        .replace("can't", "cannot")
        .replace("won't", "will not")
        .replace("n't", " not");
    expanded
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn is_semantic_negation_token(token: &str) -> bool {
    matches!(
        token,
        "not"
            | "never"
            | "without"
            | "cannot"
            | "cant"
            | "wont"
            | "dont"
            | "doesnt"
            | "isnt"
            | "arent"
            | "wasnt"
            | "werent"
            | "shouldnt"
            | "couldnt"
            | "wouldnt"
            | "mustnt"
            | "no"
            | "none"
            | "nor"
    )
}

fn signature_token(token: &str) -> Option<String> {
    let token = token.trim();
    if token.len() < 3 || is_signature_stopword(token) {
        return None;
    }
    Some(light_singular_stem(token))
}

fn light_singular_stem(token: &str) -> String {
    if token.len() > 4 && token.ends_with("ies") {
        format!("{}y", &token[..token.len() - 3])
    } else if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

fn is_signature_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "are"
            | "was"
            | "were"
            | "with"
            | "from"
            | "that"
            | "this"
            | "then"
            | "than"
            | "into"
            | "onto"
            | "when"
            | "where"
            | "while"
            | "before"
            | "after"
            | "through"
            | "only"
            | "same"
            | "claim"
            | "claims"
            | "evidence"
    )
}

fn support_set_key(claims: &[StoredClaimRecord]) -> String {
    let mut ids = claims.iter().map(|claim| claim.id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.into_iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
}

fn group_rule_rank(rule: &str) -> u8 {
    match rule {
        SEMANTIC_EXACT_GROUP_RULE => 0,
        SEMANTIC_TOKEN_GROUP_RULE => 1,
        _ => 2,
    }
}
