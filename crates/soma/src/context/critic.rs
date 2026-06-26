//! Cloud-output critic and capture contract.
//!
//! This module is the first local-control-plane gate after a cloud LLM returns
//! text. It does not trust that text as evidence. Every extracted claim enters
//! storage as a `cloud_draft` L2 candidate tied to the TaskFrame that shaped
//! the call.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::cloud_prompt::{
    cloud_context_handoff_version, expected_cloud_context_handoff_id,
    CLOUD_CONTEXT_ARTIFACT_VERSION, CLOUD_CONTEXT_CONTRACT,
    MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION, MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION,
};
use crate::memory::local_llm::{call_ollama, LocalLlmError};
use crate::storage::{
    ClaimRecordDraft, LearningCriticAction, LearningCriticProposalDraft, LifecycleState, Storage,
    StorageError, StoredEvidenceRef, StoredTaskFrame, VerificationEventDraft, VerificationResult,
    VerifierType,
};

const DETERMINISTIC_CLAIM_LIMIT: usize = 12;
pub const LOCAL_CLAIM_EXTRACTOR_SYSTEM_PROMPT: &str = concat!(
    "You are SOMA's private local claim extractor. ",
    "Extract verification candidates from a cloud LLM output. ",
    "Do not verify, promote, summarize, or invent claims. ",
    "Return only a JSON array of strings copied verbatim from the output."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCriticDecision {
    Accept,
    Revise,
    Reject,
}

impl ControlCriticDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ControlCriticDecision::Accept => "accept",
            ControlCriticDecision::Revise => "revise",
            ControlCriticDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedCloudClaim {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

impl ExtractedCloudClaim {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), evidence_refs: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRequest {
    pub claim_text: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptable_verifiers: Vec<VerifierType>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlCriticResult {
    pub task_frame_id: i64,
    pub decision: ControlCriticDecision,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extracted_claims: Vec<ExtractedCloudClaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_requests: Vec<VerificationRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_edits: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

impl ControlCriticResult {
    pub fn baseline_accept(task_frame_id: i64, output_text: impl Into<String>) -> Self {
        Self {
            task_frame_id,
            decision: ControlCriticDecision::Accept,
            extracted_claims: vec![ExtractedCloudClaim::new(output_text)],
            verification_requests: Vec::new(),
            required_edits: Vec::new(),
            evidence_refs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudOutputCaptureInput {
    pub output_text: String,
    pub handoff_id: Option<String>,
    #[serde(default)]
    pub protocol_contract: Option<String>,
    #[serde(default)]
    pub artifact_version: Option<u32>,
    pub critic: ControlCriticResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedCloudOutput {
    pub task_frame_id: i64,
    pub handoff_id: Option<String>,
    pub idempotency_key: String,
    pub replayed: bool,
    pub decision: ControlCriticDecision,
    pub claim_ids: Vec<i64>,
    pub verification_event_ids: Vec<i64>,
    pub verification_requests: Vec<VerificationRequest>,
    pub required_edits: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimExtractionSource {
    Provided,
    Deterministic,
    LocalAssisted,
    WholeOutputFallback,
}

impl ClaimExtractionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimExtractionSource::Provided => "provided",
            ClaimExtractionSource::Deterministic => "deterministic",
            ClaimExtractionSource::LocalAssisted => "local_assisted",
            ClaimExtractionSource::WholeOutputFallback => "whole_output_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalClaimExtractorRuntime<'a> {
    pub endpoint: &'a str,
    pub model: &'a str,
}

pub fn select_cloud_output_claims(
    output_text: &str,
    explicit_claims: Vec<ExtractedCloudClaim>,
    local_runtime: Option<LocalClaimExtractorRuntime<'_>>,
) -> Result<(Vec<ExtractedCloudClaim>, ClaimExtractionSource), LocalLlmError> {
    if !explicit_claims.is_empty() {
        return Ok((explicit_claims, ClaimExtractionSource::Provided));
    }

    let deterministic = deterministic_claims_from_output(output_text);
    if !deterministic.is_empty() {
        return Ok((deterministic, ClaimExtractionSource::Deterministic));
    }

    if let Some(runtime) = local_runtime {
        let assisted =
            extract_local_assisted_cloud_claims(runtime.endpoint, runtime.model, output_text)?;
        if !assisted.is_empty() {
            return Ok((assisted, ClaimExtractionSource::LocalAssisted));
        }
    }

    Ok((Vec::new(), ClaimExtractionSource::WholeOutputFallback))
}

pub fn build_local_claim_extractor_prompt(output_text: &str) -> String {
    let mut out = String::new();
    out.push_str(
        "Task: extract candidate claims from this cloud LLM output for later SOMA verification.\n",
    );
    out.push_str("Rules:\n");
    out.push_str("- Return JSON only: an array of strings.\n");
    out.push_str("- Copy each string verbatim from the output.\n");
    out.push_str("- Do not rewrite, infer, merge, verify, or promote claims.\n");
    out.push_str("- Prefer concrete findings, decisions, requirements, policies, corrections, and invariants.\n");
    out.push_str("- Return [] if there are no concrete claim candidates.\n\n");
    out.push_str("Cloud output:\n");
    out.push_str(output_text);
    out
}

pub fn extract_local_assisted_cloud_claims(
    endpoint: &str,
    model: &str,
    output_text: &str,
) -> Result<Vec<ExtractedCloudClaim>, LocalLlmError> {
    let prompt = build_local_claim_extractor_prompt(output_text);
    let response = call_ollama(endpoint, model, LOCAL_CLAIM_EXTRACTOR_SYSTEM_PROMPT, &prompt)?;
    Ok(extract_local_assisted_cloud_claims_from_text(output_text, &response))
}

pub fn extract_local_assisted_cloud_claims_from_text(
    output_text: &str,
    assistant_text: &str,
) -> Vec<ExtractedCloudClaim> {
    let mut claims = Vec::new();
    for claim in parse_assisted_claim_strings(assistant_text) {
        let Some(cleaned) = clean_candidate_claim(&claim) else {
            continue;
        };
        if !claim_is_anchored_to_output(cleaned, output_text) {
            continue;
        }
        push_unique_claim(&mut claims, cleaned);
        if claims.len() >= DETERMINISTIC_CLAIM_LIMIT {
            break;
        }
    }
    claims.into_iter().map(ExtractedCloudClaim::new).collect()
}

pub fn capture_cloud_output_claims(
    storage: &mut Storage,
    input: &CloudOutputCaptureInput,
) -> Result<CapturedCloudOutput, StorageError> {
    validate_capture_input(input)?;
    let task_frame = require_task_frame(storage, input.critic.task_frame_id)?;
    validate_handoff_id(input.handoff_id.as_deref(), &task_frame)?;
    validate_protocol_echo(input.protocol_contract.as_deref(), input.artifact_version)?;

    let output_ref =
        cloud_output_ref(input.critic.task_frame_id, &input.output_text, input.critic.decision);
    let handoff_ref = input.handoff_id.as_ref().map(|handoff_id| StoredEvidenceRef {
        kind: "cloud_context_handoff".to_string(),
        id: handoff_id.clone(),
        source: Some("soma-cloud-context".to_string()),
    });
    let base_evidence = base_evidence_refs(
        input.critic.task_frame_id,
        &output_ref,
        handoff_ref.as_ref(),
        &input.critic.evidence_refs,
    );
    let replayed_claims =
        storage.cloud_output_claim_records_by_ref(input.critic.task_frame_id, &output_ref.id)?;
    if !replayed_claims.is_empty() {
        let claim_ids = replayed_claims.iter().map(|claim| claim.id).collect::<Vec<_>>();
        let verification_event_ids =
            replayed_cloud_output_verification_event_ids(storage, &claim_ids, &output_ref.id)?;
        return Ok(CapturedCloudOutput {
            task_frame_id: input.critic.task_frame_id,
            handoff_id: input.handoff_id.clone(),
            idempotency_key: output_ref.id,
            replayed: true,
            decision: input.critic.decision,
            claim_ids,
            verification_event_ids,
            verification_requests: input.critic.verification_requests.clone(),
            required_edits: input.critic.required_edits.clone(),
        });
    }
    let claims = normalized_claims(input);

    let mut claim_ids = Vec::with_capacity(claims.len());
    let mut verification_event_ids = Vec::new();
    for claim in claims {
        let mut evidence_refs = base_evidence.clone();
        evidence_refs.extend(claim.evidence_refs);
        evidence_refs = dedupe_evidence_refs(evidence_refs);

        let mut draft = ClaimRecordDraft::cloud_draft(input.critic.task_frame_id, claim.text);
        draft.evidence_refs = evidence_refs;
        let claim_id = storage.insert_claim_record(&draft)?;
        claim_ids.push(claim_id);

        if let Some(result) = automatic_verification_result(input.critic.decision) {
            let event_id = storage.insert_verification_event(&VerificationEventDraft {
                claim_id,
                verifier_type: VerifierType::LocalObservation,
                result,
                evidence_ref: StoredEvidenceRef {
                    kind: "control_critic".to_string(),
                    id: output_ref.id.clone(),
                    source: Some(input.critic.decision.as_str().to_string()),
                },
            })?;
            verification_event_ids.push(event_id);
        }
    }

    Ok(CapturedCloudOutput {
        task_frame_id: input.critic.task_frame_id,
        handoff_id: input.handoff_id.clone(),
        idempotency_key: output_ref.id,
        replayed: false,
        decision: input.critic.decision,
        claim_ids,
        verification_event_ids,
        verification_requests: input.critic.verification_requests.clone(),
        required_edits: input.critic.required_edits.clone(),
    })
}

fn replayed_cloud_output_verification_event_ids(
    storage: &Storage,
    claim_ids: &[i64],
    output_ref_id: &str,
) -> Result<Vec<i64>, StorageError> {
    let mut ids = Vec::new();
    for claim_id in claim_ids {
        for event in storage.verification_events_for_claim(*claim_id)? {
            if event.evidence_ref.kind == "control_critic" && event.evidence_ref.id == output_ref_id
            {
                ids.push(event.id);
            }
        }
    }
    Ok(ids)
}

pub fn learning_critic_proposal_from_capture(
    captured: &CapturedCloudOutput,
    action: LearningCriticAction,
    target_lifecycle_state: Option<LifecycleState>,
    reason: impl Into<String>,
) -> LearningCriticProposalDraft {
    let mut evidence_refs = vec![StoredEvidenceRef {
        kind: "task_frame".to_string(),
        id: captured.task_frame_id.to_string(),
        source: Some("learning_critic".to_string()),
    }];
    evidence_refs.extend(captured.claim_ids.iter().map(|claim_id| StoredEvidenceRef {
        kind: "claim".to_string(),
        id: claim_id.to_string(),
        source: Some("learning_critic".to_string()),
    }));

    LearningCriticProposalDraft {
        task_frame_id: Some(captured.task_frame_id),
        action,
        claim_ids: captured.claim_ids.clone(),
        target_lifecycle_state,
        reason: reason.into(),
        evidence_refs,
    }
}

fn require_task_frame(
    storage: &Storage,
    task_frame_id: i64,
) -> Result<StoredTaskFrame, StorageError> {
    storage.task_frame(task_frame_id)?.ok_or_else(|| StorageError::Corrupt {
        detail: format!("cloud output capture requires existing TaskFrame {task_frame_id}"),
    })
}

fn validate_handoff_id(
    handoff_id: Option<&str>,
    task_frame: &StoredTaskFrame,
) -> Result<(), StorageError> {
    let Some(handoff_id) = handoff_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let expected = expected_cloud_context_handoff_id(task_frame);
    if let Some(version) = cloud_context_handoff_version(handoff_id) {
        if !(MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION
            ..=MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION)
            .contains(&version)
        {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "unsupported cloud context handoff protocol version v{version}; supported range is v{}..=v{}",
                    MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION,
                    MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION
                ),
            });
        }
    }
    if handoff_id != expected {
        return Err(StorageError::Corrupt {
            detail: format!(
                "cloud output handoff_id `{handoff_id}` does not match TaskFrame {} expected `{expected}`",
                task_frame.id
            ),
        });
    }
    Ok(())
}

fn validate_protocol_echo(
    protocol_contract: Option<&str>,
    artifact_version: Option<u32>,
) -> Result<(), StorageError> {
    let contract = protocol_contract.map(str::trim).filter(|value| !value.is_empty());
    if contract.is_none() && artifact_version.is_none() {
        return Ok(());
    }
    let (Some(contract), Some(version)) = (contract, artifact_version) else {
        return Err(StorageError::Corrupt {
            detail:
                "cloud context protocol echo requires both protocol_contract and artifact_version"
                    .to_string(),
        });
    };
    if contract != CLOUD_CONTEXT_CONTRACT {
        return Err(StorageError::Corrupt {
            detail: format!(
                "cloud context protocol_contract `{contract}` does not match expected `{}`",
                CLOUD_CONTEXT_CONTRACT
            ),
        });
    }
    if !(MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION
        ..=MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION)
        .contains(&version)
    {
        return Err(StorageError::Corrupt {
            detail: format!(
                "unsupported cloud context artifact_version v{version}; supported range is v{}..=v{}",
                MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION,
                MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION
            ),
        });
    }
    if version != CLOUD_CONTEXT_ARTIFACT_VERSION {
        return Err(StorageError::Corrupt {
            detail: format!(
                "cloud context artifact_version v{version} does not match current v{}",
                CLOUD_CONTEXT_ARTIFACT_VERSION
            ),
        });
    }
    Ok(())
}

fn validate_capture_input(input: &CloudOutputCaptureInput) -> Result<(), StorageError> {
    if input.output_text.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "cloud output capture requires non-empty output_text".to_string(),
        });
    }
    if input.critic.task_frame_id <= 0 {
        return Err(StorageError::Corrupt {
            detail: "cloud output capture requires positive task_frame_id".to_string(),
        });
    }
    if input.critic.decision == ControlCriticDecision::Revise
        && input.critic.required_edits.iter().all(|edit| edit.trim().is_empty())
    {
        return Err(StorageError::Corrupt {
            detail: "revise critic result requires at least one required_edit".to_string(),
        });
    }
    for claim in &input.critic.extracted_claims {
        if claim.text.trim().is_empty() {
            return Err(StorageError::Corrupt {
                detail: "extracted cloud claim text cannot be empty".to_string(),
            });
        }
    }
    for request in &input.critic.verification_requests {
        if request.claim_text.trim().is_empty() || request.reason.trim().is_empty() {
            return Err(StorageError::Corrupt {
                detail: "verification request requires non-empty claim_text and reason".to_string(),
            });
        }
    }
    Ok(())
}

fn normalized_claims(input: &CloudOutputCaptureInput) -> Vec<ExtractedCloudClaim> {
    if input.critic.extracted_claims.is_empty() {
        let extracted = deterministic_claims_from_output(&input.output_text);
        if extracted.is_empty() {
            vec![ExtractedCloudClaim::new(input.output_text.trim())]
        } else {
            extracted
        }
    } else {
        input
            .critic
            .extracted_claims
            .iter()
            .map(|claim| ExtractedCloudClaim {
                text: claim.text.trim().to_string(),
                evidence_refs: claim.evidence_refs.clone(),
            })
            .collect()
    }
}

pub fn deterministic_claims_from_output(output_text: &str) -> Vec<ExtractedCloudClaim> {
    let mut claims = Vec::new();
    let mut saw_structured_marker = false;

    for claim in markdown_table_claims(output_text) {
        push_unique_claim(&mut claims, &claim);
        saw_structured_marker = true;
        if claims.len() >= DETERMINISTIC_CLAIM_LIMIT {
            return claims.into_iter().map(ExtractedCloudClaim::new).collect();
        }
    }

    for line in output_text.lines() {
        if let Some(claim) = strip_priority_claim_marker(line)
            .or_else(|| strip_labeled_claim_marker(line))
            .or_else(|| strip_checklist_claim_marker(line))
        {
            push_unique_claim(&mut claims, claim);
            saw_structured_marker = true;
            if claims.len() >= DETERMINISTIC_CLAIM_LIMIT {
                return claims.into_iter().map(ExtractedCloudClaim::new).collect();
            }
            continue;
        }

        let Some(claim) = strip_list_claim_marker(line) else {
            continue;
        };
        push_unique_claim(&mut claims, claim);
        if claims.len() >= DETERMINISTIC_CLAIM_LIMIT {
            return claims.into_iter().map(ExtractedCloudClaim::new).collect();
        }
    }
    if saw_structured_marker && !claims.is_empty() {
        return claims.into_iter().map(ExtractedCloudClaim::new).collect();
    }
    if claims.len() >= 2 {
        return claims.into_iter().map(ExtractedCloudClaim::new).collect();
    }

    claims.clear();
    for sentence in split_sentence_claims(output_text) {
        push_unique_claim(&mut claims, sentence);
        if claims.len() >= DETERMINISTIC_CLAIM_LIMIT {
            break;
        }
    }
    if claims.len() >= 2 {
        claims.into_iter().map(ExtractedCloudClaim::new).collect()
    } else {
        Vec::new()
    }
}

fn markdown_table_claims(output_text: &str) -> Vec<String> {
    let lines = output_text.lines().collect::<Vec<_>>();
    let mut claims = Vec::new();
    let mut claim_columns: Option<Vec<usize>> = None;

    for idx in 0..lines.len() {
        let Some(cells) = markdown_table_cells(lines[idx]) else {
            claim_columns = None;
            continue;
        };

        if let Some(columns) = claim_columns.as_ref() {
            for column in columns {
                if let Some(claim) = cells.get(*column).and_then(|cell| clean_candidate_claim(cell))
                {
                    push_unique_claim(&mut claims, claim);
                }
            }
            continue;
        }

        let Some(next) = lines.get(idx + 1).and_then(|line| markdown_table_cells(line)) else {
            continue;
        };
        if !is_markdown_table_separator(&next) {
            continue;
        }
        let columns = cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| is_claim_table_header(cell).then_some(idx))
            .collect::<Vec<_>>();
        if !columns.is_empty() {
            claim_columns = Some(columns);
        }
    }

    claims
}

fn markdown_table_cells(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    if !line.contains('|') {
        return None;
    }
    let cells =
        line.trim_matches('|').split('|').map(|cell| cell.trim().to_string()).collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn is_markdown_table_separator(cells: &[String]) -> bool {
    cells.iter().all(|cell| {
        let compact = cell.trim();
        !compact.is_empty()
            && compact.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
            && compact.chars().any(|ch| ch == '-')
    })
}

fn is_claim_table_header(cell: &str) -> bool {
    matches!(
        normalized_label(cell).as_str(),
        "claim"
            | "claims"
            | "finding"
            | "findings"
            | "decision"
            | "decisions"
            | "risk"
            | "risks"
            | "requirement"
            | "requirements"
            | "invariant"
            | "invariants"
            | "policy"
            | "policies"
            | "correction"
            | "corrections"
            | "fact"
            | "facts"
            | "stablefact"
            | "stablefacts"
    )
}

fn strip_priority_claim_marker(line: &str) -> Option<&str> {
    let line = strip_markdown_prefixes(line.trim());
    let mut chars = line.char_indices();
    let (_, first) = chars.next()?;
    if !matches!(first, 'p' | 'P') {
        return None;
    }
    let mut marker_end = first.len_utf8();
    let mut saw_digit = false;
    for (idx, ch) in chars {
        if ch.is_ascii_digit() {
            saw_digit = true;
            marker_end = idx + ch.len_utf8();
            continue;
        }
        break;
    }
    if !saw_digit {
        return None;
    }
    let rest = line.get(marker_end..)?;
    let rest = strip_leading_claim_separator(rest)?;
    clean_candidate_claim(rest)
}

fn strip_labeled_claim_marker(line: &str) -> Option<&str> {
    let line = strip_markdown_prefixes(line.trim());
    let split_at = line
        .find(':')
        .or_else(|| line.find(" - "))
        .or_else(|| line.find(" \u{2014} "))
        .or_else(|| line.find(" \u{2013} "))?;
    let label = line.get(..split_at)?.trim();
    if label.len() > 32 || !is_claim_line_label(label) {
        return None;
    }
    let rest = line.get(split_at..)?;
    let rest = strip_leading_claim_separator(rest)?;
    clean_candidate_claim(rest)
}

fn strip_checklist_claim_marker(line: &str) -> Option<&str> {
    let line = strip_markdown_prefixes(line.trim());
    let rest = line.strip_prefix("- [").or_else(|| line.strip_prefix("* ["))?;
    let (_, rest) = rest.split_once(']')?;
    clean_candidate_claim(rest)
}

fn strip_list_claim_marker(line: &str) -> Option<&str> {
    let line = strip_markdown_prefixes(line.trim());
    if line.is_empty() || line.starts_with("```") {
        return None;
    }
    for marker in ["- ", "* "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return clean_candidate_claim(rest);
        }
    }

    let marker_end = line
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()?;
    let rest = line.get(marker_end..)?;
    let rest = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))?;
    clean_candidate_claim(rest)
}

fn strip_markdown_prefixes(mut line: &str) -> &str {
    loop {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('>') {
            line = rest;
            continue;
        }
        return trimmed;
    }
}

fn strip_leading_claim_separator(rest: &str) -> Option<&str> {
    let rest = rest.trim_start();
    for separator in [":", "-", "\u{2014}", "\u{2013}"] {
        if let Some(stripped) = rest.strip_prefix(separator) {
            return Some(stripped.trim_start());
        }
    }
    None
}

fn is_claim_line_label(label: &str) -> bool {
    matches!(
        normalized_label(label).as_str(),
        "claim"
            | "finding"
            | "decision"
            | "risk"
            | "requirement"
            | "invariant"
            | "policy"
            | "correction"
            | "fact"
            | "stablefact"
    )
}

fn normalized_label(label: &str) -> String {
    label.chars().filter(|ch| ch.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn split_sentence_claims(output_text: &str) -> Vec<&str> {
    output_text.split_terminator(['.', '!', '?']).filter_map(clean_candidate_claim).collect()
}

fn clean_candidate_claim(candidate: &str) -> Option<&str> {
    let candidate = candidate
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | '[' | ']' | '*' | '_' | '|'));
    if candidate.len() < 8 || !candidate.chars().any(char::is_alphabetic) {
        return None;
    }
    Some(candidate)
}

fn push_unique_claim(claims: &mut Vec<String>, claim: &str) {
    if claims.iter().any(|existing| existing.eq_ignore_ascii_case(claim)) {
        return;
    }
    claims.push(claim.to_string());
}

fn parse_assisted_claim_strings(assistant_text: &str) -> Vec<String> {
    if let Some(json_text) = json_payload_candidate(assistant_text) {
        if let Ok(value) = serde_json::from_str::<Value>(json_text) {
            if let Some(claims) = assisted_claim_strings_from_json(&value) {
                return claims;
            }
        }
    }

    assistant_text
        .lines()
        .filter_map(|line| {
            strip_labeled_claim_marker(line)
                .or_else(|| strip_list_claim_marker(line))
                .map(str::to_string)
        })
        .collect()
}

fn json_payload_candidate(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let fenced = trimmed.strip_prefix("```")?;
    let (_, rest) = fenced.split_once('\n')?;
    let (json, _) = rest.rsplit_once("```")?;
    Some(json.trim())
}

fn assisted_claim_strings_from_json(value: &Value) -> Option<Vec<String>> {
    let array = value.as_array()?;
    let mut claims = Vec::with_capacity(array.len());
    for item in array {
        if let Some(text) = item.as_str() {
            claims.push(text.to_string());
            continue;
        }
        let object = item.as_object()?;
        let text = object.get("text").and_then(Value::as_str)?;
        claims.push(text.to_string());
    }
    Some(claims)
}

fn claim_is_anchored_to_output(claim: &str, output_text: &str) -> bool {
    let claim = normalize_for_anchor(claim);
    if claim.is_empty() {
        return false;
    }
    normalize_for_anchor(output_text).contains(&claim)
}

fn normalize_for_anchor(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            for lowered in ch.to_lowercase() {
                out.push(lowered);
            }
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    out.trim().to_string()
}

fn base_evidence_refs(
    task_frame_id: i64,
    output_ref: &StoredEvidenceRef,
    handoff_ref: Option<&StoredEvidenceRef>,
    critic_refs: &[StoredEvidenceRef],
) -> Vec<StoredEvidenceRef> {
    let mut refs = vec![
        StoredEvidenceRef {
            kind: "task_frame".to_string(),
            id: task_frame_id.to_string(),
            source: Some("cloud_output_capture".to_string()),
        },
        output_ref.clone(),
    ];
    if let Some(handoff_ref) = handoff_ref {
        refs.push(handoff_ref.clone());
    }
    refs.extend_from_slice(critic_refs);
    dedupe_evidence_refs(refs)
}

fn cloud_output_ref(
    task_frame_id: i64,
    output_text: &str,
    decision: ControlCriticDecision,
) -> StoredEvidenceRef {
    StoredEvidenceRef {
        kind: "cloud_output".to_string(),
        id: format!(
            "cloudout:{}",
            fnv_hash(&format!("{task_frame_id}\n{}\n{output_text}", decision.as_str()))
        ),
        source: Some(decision.as_str().to_string()),
    }
}

fn automatic_verification_result(decision: ControlCriticDecision) -> Option<VerificationResult> {
    match decision {
        ControlCriticDecision::Accept => None,
        ControlCriticDecision::Revise => Some(VerificationResult::Inconclusive),
        ControlCriticDecision::Reject => Some(VerificationResult::Contradicted),
    }
}

fn dedupe_evidence_refs(evidence_refs: Vec<StoredEvidenceRef>) -> Vec<StoredEvidenceRef> {
    let mut out = Vec::new();
    for evidence_ref in evidence_refs {
        if !out.iter().any(|existing: &StoredEvidenceRef| {
            existing.kind == evidence_ref.kind
                && existing.id == evidence_ref.id
                && existing.source == evidence_ref.source
        }) {
            out.push(evidence_ref);
        }
    }
    out
}

fn fnv_hash(text: &str) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;
