//! ContextEnvelope why-inspection shared by MCP and CLI surfaces.
//!
//! The cloud tool (`soma_context_why`) and the operator CLI
//! (`soma context why`) must explain the same sections with the same
//! reasons and evidence. Keeping that logic here prevents the bridge
//! contract from drifting by surface.

use serde_json::{json, Value};

use crate::context::envelope::{ContextEnvelope, ContextSection, EvidenceRef};
use crate::storage::{
    ClaimSourceType, Storage, StorageError, StoredClaimRecord, StoredLearningCriticProposal,
    StoredVerificationEvent,
};

const AUDIT_RECORD_LIMIT: usize = 20;

pub const VALID_SECTIONS: &[&str] = &[
    "thread_state",
    "compiler_notes",
    "short_term_candidates",
    "project_experience",
    "relevant_memory",
    "stable_facts",
    "user_policy",
    "open_decisions",
    "corrections",
    "claim_records",
    "learning_critic_proposals",
];

pub fn validate_section(section: Option<&str>) -> Result<(), String> {
    if let Some(section) = section {
        if !VALID_SECTIONS.contains(&section) {
            return Err(format!(
                "unknown section `{section}`; expected {}",
                VALID_SECTIONS.join(", ")
            ));
        }
    }
    Ok(())
}

pub fn why_matches(
    envelope: &ContextEnvelope,
    section_filter: Option<&str>,
    contains: Option<&str>,
) -> Vec<Value> {
    let needle = contains.map(|s| s.to_lowercase());
    let mut out = Vec::new();

    if section_selected(section_filter, "thread_state") {
        if let Some(section) = &envelope.thread_state {
            push_why_section(
                &mut out,
                "thread_state",
                &section.text,
                "deterministic evidence-backed summary of ranked local context",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "compiler_notes") {
        for section in &envelope.compiler_notes {
            push_why_section(
                &mut out,
                "compiler_notes",
                &section.text,
                "optional local LLM compiler note admitted only because it cited local evidence",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "short_term_candidates") {
        for section in &envelope.short_term_candidates {
            push_why_section(
                &mut out,
                "short_term_candidates",
                &section.text,
                "L2 short-term candidate signal; use as cautionary context, not durable fact",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "project_experience") {
        for section in &envelope.project_experience {
            push_why_section(
                &mut out,
                "project_experience",
                &section.text,
                "scoped project/session provenance summarized from local episode metadata only",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "relevant_memory") {
        for item in &envelope.relevant_memory {
            push_why_section(
                &mut out,
                "relevant_memory",
                &item.text,
                &item.reason,
                &item.evidence,
                needle.as_deref(),
                json!({
                    "rank": item.rank,
                    "layer": item.layer.clone(),
                    "layer_rank": item.layer_rank,
                    "similarity": item.similarity,
                    "project": item.project.clone(),
                    "session_id": item.session_id.clone(),
                }),
            );
        }
    }

    if section_selected(section_filter, "user_policy") {
        for section in &envelope.user_policy {
            let reason = if section.status.as_deref() == Some("corrected") {
                "policy matched a user correction; confidence decayed and correction evidence attached"
            } else {
                "policy extracted from cited local evidence"
            };
            push_why_section(
                &mut out,
                "user_policy",
                &section.text,
                reason,
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "stable_facts") {
        for section in &envelope.stable_facts {
            push_why_section(
                &mut out,
                "stable_facts",
                &section.text,
                "L4 semantic fact promoted only after durable verification evidence",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "open_decisions") {
        for section in &envelope.open_decisions {
            push_why_section(
                &mut out,
                "open_decisions",
                &section.text,
                "unresolved contradiction or decision candidate with cited evidence",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    if section_selected(section_filter, "corrections") {
        for section in &envelope.corrections {
            push_why_section(
                &mut out,
                "corrections",
                &section.text,
                "user correction recorded as local evidence",
                &section.evidence,
                needle.as_deref(),
                section_metadata(section),
            );
        }
    }

    out
}

pub fn why_matches_with_audit(
    storage: &Storage,
    envelope: &ContextEnvelope,
    section_filter: Option<&str>,
    contains: Option<&str>,
) -> Result<Vec<Value>, StorageError> {
    let needle = contains.map(|s| s.to_lowercase());
    let mut out = why_matches(envelope, section_filter, contains);
    let project = envelope.scope.project.as_deref();
    let session_id = envelope.scope.session_id.as_deref();

    if audit_section_selected(section_filter, "claim_records") {
        for claim in storage.recent_claim_records_scoped(project, session_id, AUDIT_RECORD_LIMIT)? {
            let events = storage.verification_events_for_claim(claim.id)?;
            let durable_trust = storage.claim_has_durable_promotion_trust(claim.id)?;
            push_audit_section(
                &mut out,
                "claim_records",
                &claim.text,
                claim_record_reason(&claim),
                json!(claim.evidence_refs),
                needle.as_deref(),
                claim_record_metadata(&claim, &events, durable_trust),
            );
        }
    }

    if audit_section_selected(section_filter, "learning_critic_proposals") {
        for proposal in storage.recent_learning_critic_proposals_scoped(
            project,
            session_id,
            AUDIT_RECORD_LIMIT,
        )? {
            push_audit_section(
                &mut out,
                "learning_critic_proposals",
                &proposal.reason,
                learning_critic_proposal_reason(&proposal),
                json!(proposal.evidence_refs),
                needle.as_deref(),
                learning_critic_proposal_metadata(&proposal),
            );
        }
    }

    Ok(out)
}

fn section_selected(filter: Option<&str>, section: &str) -> bool {
    filter.is_none_or(|f| f == section)
}

fn audit_section_selected(filter: Option<&str>, section: &str) -> bool {
    filter.is_some_and(|f| f == section)
}

fn section_metadata(section: &ContextSection) -> Value {
    json!({
        "kind": section.kind.clone(),
        "status": section.status.clone(),
        "confidence": section.confidence,
    })
}

fn push_why_section(
    out: &mut Vec<Value>,
    section: &str,
    text: &str,
    reason: &str,
    evidence: &[EvidenceRef],
    needle: Option<&str>,
    metadata: Value,
) {
    if let Some(needle) = needle {
        if !text.to_lowercase().contains(needle) {
            return;
        }
    }
    out.push(json!({
        "section": section,
        "text": text,
        "reason": reason,
        "evidence": evidence,
        "metadata": metadata,
    }));
}

fn claim_record_reason(claim: &StoredClaimRecord) -> &'static str {
    match claim.source_type {
        ClaimSourceType::CloudDraft => {
            "cloud draft claim retained for audit/L2 context only; confirmed user, tool, test, local, or correction evidence is required before L3/L4 promotion"
        }
        ClaimSourceType::UserConfirmed
        | ClaimSourceType::ToolVerified
        | ClaimSourceType::LocalObserved
        | ClaimSourceType::ExplicitCorrection => {
            "trusted-source claim record; durable promotion is still tied to cited evidence and lifecycle rules"
        }
    }
}

fn claim_record_metadata(
    claim: &StoredClaimRecord,
    events: &[StoredVerificationEvent],
    durable_trust: bool,
) -> Value {
    json!({
        "id": claim.id,
        "source_type": claim.source_type.as_str(),
        "task_frame_id": claim.task_frame_id,
        "confidence": claim.confidence,
        "lifecycle_state": claim.lifecycle_state.as_str(),
        "promotion_reason": claim.promotion_reason.clone(),
        "durable_promotion_trust": durable_trust,
        "verification_count": events.len(),
        "verification_events": events.iter().map(verification_event_metadata).collect::<Vec<_>>(),
        "created_at_ns": claim.created_at_ns,
        "updated_at_ns": claim.updated_at_ns,
    })
}

fn verification_event_metadata(event: &StoredVerificationEvent) -> Value {
    json!({
        "id": event.id,
        "claim_id": event.claim_id,
        "verifier_type": event.verifier_type.as_str(),
        "result": event.result.as_str(),
        "evidence_ref": event.evidence_ref.clone(),
        "created_at_ns": event.created_at_ns,
    })
}

fn learning_critic_proposal_reason(proposal: &StoredLearningCriticProposal) -> &'static str {
    match proposal.action.as_str() {
        "propose_promotion" => {
            "learning critic promotion proposal; applying it must re-check claim verification trust before any L3/L4 lifecycle mutation"
        }
        "request_verification" => {
            "learning critic verification request; this records needed review but does not itself verify a claim"
        }
        "decay" => {
            "learning critic decay proposal; applying it records a lifecycle decay/forget transition with cited evidence"
        }
        "create_candidate" => {
            "learning critic candidate proposal; this can create or preserve L2 candidate context without durable promotion"
        }
        _ => "learning critic audit proposal; proposal records are not verification events",
    }
}

fn learning_critic_proposal_metadata(proposal: &StoredLearningCriticProposal) -> Value {
    json!({
        "id": proposal.id,
        "task_frame_id": proposal.task_frame_id,
        "action": proposal.action.as_str(),
        "claim_ids": proposal.claim_ids.clone(),
        "target_lifecycle_state": proposal.target_lifecycle_state.map(|state| state.as_str()),
        "status": proposal.status.as_str(),
        "result": proposal.result_json.clone(),
        "created_at_ns": proposal.created_at_ns,
        "updated_at_ns": proposal.updated_at_ns,
    })
}

fn push_audit_section(
    out: &mut Vec<Value>,
    section: &str,
    text: &str,
    reason: &str,
    evidence: Value,
    needle: Option<&str>,
    metadata: Value,
) {
    if let Some(needle) = needle {
        if !text.to_lowercase().contains(needle) {
            return;
        }
    }
    out.push(json!({
        "section": section,
        "text": text,
        "reason": reason,
        "evidence": evidence,
        "metadata": metadata,
    }));
}
