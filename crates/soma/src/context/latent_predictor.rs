//! Read-only prediction over evidence-backed latent proxies.
//!
//! This is the first Phase-6 bridge from "learn from latents" research into
//! SOMA's product boundary. It predicts which persisted latent proxies are
//! relevant to a query, but it never mutates lifecycle state and never treats
//! cloud drafts as durable evidence.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::storage::{
    ClaimSourceType, EvidenceBackedLatentProxy, LifecycleState, SensitivityLabel, Storage,
    StorageError, StoredEvidenceRef,
};

pub const LATENT_PROXY_PREDICTOR_SOURCE: &str = "soma_latent_proxy_predictor";
pub const LATENT_PROXY_PREDICTOR_RULE: &str =
    "read_only_query_overlap_confidence_lifecycle_baseline_v1";
pub const LATENT_INTERFACE_PACKET_SCHEMA: &str = "soma.latent_interface_packet.v1";
pub const LATENT_INTERFACE_PACKET_SOURCE: &str = "soma_latent_interface_packet";
pub const DEFAULT_LATENT_PREDICTOR_LIMIT: usize = 8;
pub const DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT: usize = 160;
pub const DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE: f32 = 0.35;
pub const OPTIONAL_INFERENCE_FIELD_POLICY_VERSION: &str = "soma.optional_inference_field_policy.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionalInferenceFieldPolicy {
    pub version: &'static str,
    pub mode: &'static str,
    pub allowed_fields: Vec<&'static str>,
    pub allowed_surfaces: Vec<&'static str>,
    pub required_gates: Vec<&'static str>,
    pub forbidden_uses: Vec<&'static str>,
}

pub fn optional_inference_field_policy() -> OptionalInferenceFieldPolicy {
    OptionalInferenceFieldPolicy {
        version: OPTIONAL_INFERENCE_FIELD_POLICY_VERSION,
        mode: "read_only_candidate_fields_only",
        allowed_fields: vec![
            "proxy_relevance_score",
            "proxy_ranking_order",
            "matching_terms",
            "ranking_reasons",
            "abstain_or_fallback_signal",
        ],
        allowed_surfaces: vec![
            "latent_predict.predictions",
            "latent_eval.case_reports",
            "review_only_semantic_candidates",
        ],
        required_gates: vec![
            "deterministic_baseline_parity_passed",
            "zero_cloud_draft_predictions",
            "evidence_backed_proxy_refs_only",
            "privacy_gate_passed",
        ],
        forbidden_uses: vec![
            "verification_event.evidence_ref",
            "claim_record.text",
            "claim_record.source_type",
            "lifecycle_state_transition",
            "semantic_fact_text",
            "user_policy_text",
            "correction_text",
            "task_frame_cloud_projection",
            "context_envelope_mutation_without_evidence",
        ],
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatentProxyPredictionInput {
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub scan_limit: usize,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentProxyPredictionReport {
    pub kind: &'static str,
    pub source: &'static str,
    pub rule: &'static str,
    pub mode: &'static str,
    pub trust_boundary: &'static str,
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub scan_limit: usize,
    pub min_confidence: f32,
    pub inspected_proxy_count: usize,
    pub deterministic_baseline_count: usize,
    pub deterministic_baseline_proxy_ids: Vec<i64>,
    pub predicted_count: usize,
    pub fallback_to_deterministic_projection: bool,
    pub skipped_scope_count: usize,
    pub skipped_privacy_count: usize,
    pub skipped_untrusted_cloud_draft_count: usize,
    pub skipped_missing_evidence_count: usize,
    pub skipped_semantic_evidence_count: usize,
    pub skipped_low_confidence_count: usize,
    pub optional_inference_policy: OptionalInferenceFieldPolicy,
    pub predictions: Vec<LatentProxyPrediction>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentProxyPrediction {
    pub proxy_id: i64,
    pub episode_id: i64,
    pub proxy_type: String,
    pub target: Option<String>,
    pub scope: Option<String>,
    pub claim: String,
    pub memory_layer: String,
    pub lifecycle_state: String,
    pub envelope_section: String,
    pub source_trust: String,
    pub confidence: f32,
    pub score: f32,
    pub matching_terms: Vec<String>,
    pub reasons: Vec<String>,
    pub predicted_action: &'static str,
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatentInterfacePacketInput {
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub scan_limit: usize,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentInterfacePacket {
    pub kind: &'static str,
    pub schema: &'static str,
    pub source: &'static str,
    pub mode: &'static str,
    pub trust_boundary: &'static str,
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub latent_channel: LatentChannelPolicy,
    pub textual_fallback: LatentTextualFallback,
    pub proxy_binding_count: usize,
    pub proxy_bindings: Vec<LatentInterfaceProxyBinding>,
    pub prediction_report: LatentProxyPredictionReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatentChannelPolicy {
    pub requested_channel: &'static str,
    pub current_transport: &'static str,
    pub vector_payload_included: bool,
    pub hidden_state_injection_supported: bool,
    pub reason: &'static str,
    pub acceptance_requirements: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatentTextualFallback {
    pub format: &'static str,
    pub proxy_count: usize,
    pub projection: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentInterfaceProxyBinding {
    pub latent_ref: String,
    pub proxy_id: i64,
    pub envelope_section: String,
    pub lifecycle_state: String,
    pub source_trust: String,
    pub score: f32,
    pub matching_terms: Vec<String>,
    pub ranking_reasons: Vec<String>,
    pub textual_projection: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

pub fn predict_latent_proxies(
    storage: &Storage,
    input: LatentProxyPredictionInput,
) -> Result<LatentProxyPredictionReport, StorageError> {
    let query = input.query.trim().to_string();
    if query.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "latent predictor query must be non-empty".to_string(),
        });
    }
    if input.limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent predictor limit must be greater than 0".to_string(),
        });
    }
    if input.scan_limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent predictor scan_limit must be greater than 0".to_string(),
        });
    }
    if !input.min_confidence.is_finite() || !(0.0..=1.0).contains(&input.min_confidence) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "latent predictor min_confidence must be finite within [0,1], got {}",
                input.min_confidence
            ),
        });
    }

    let proxies = storage.active_evidence_latent_proxies(input.scan_limit)?;
    let query_tokens = tokenize(&query);
    let mut report = LatentProxyPredictionReport {
        kind: "latent_proxy_prediction",
        source: LATENT_PROXY_PREDICTOR_SOURCE,
        rule: LATENT_PROXY_PREDICTOR_RULE,
        mode: "read_only_dry_run",
        trust_boundary: "read_only selector over evidence-backed latent proxies; creates no claim, verification event, proposal, lifecycle transition, or ContextEnvelope mutation; cloud_draft proxies are excluded from prediction",
        query,
        project: input.project.clone(),
        session_id: input.session_id.clone(),
        limit: input.limit,
        scan_limit: input.scan_limit,
        min_confidence: input.min_confidence,
        inspected_proxy_count: proxies.len(),
        deterministic_baseline_count: 0,
        deterministic_baseline_proxy_ids: Vec::new(),
        predicted_count: 0,
        fallback_to_deterministic_projection: true,
        skipped_scope_count: 0,
        skipped_privacy_count: 0,
        skipped_untrusted_cloud_draft_count: 0,
        skipped_missing_evidence_count: 0,
        skipped_semantic_evidence_count: 0,
        skipped_low_confidence_count: 0,
        optional_inference_policy: optional_inference_field_policy(),
        predictions: Vec::new(),
    };

    for proxy in proxies {
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            report.skipped_missing_evidence_count += 1;
            continue;
        };
        if !episode_matches_scope(
            episode.project.as_deref(),
            episode.session_id.as_deref(),
            input.project.as_deref(),
            input.session_id.as_deref(),
        ) {
            report.skipped_scope_count += 1;
            continue;
        }
        if proxy.evidence_refs.is_empty() {
            report.skipped_missing_evidence_count += 1;
            continue;
        }
        if !proxy_cloud_safe(&proxy) {
            report.skipped_privacy_count += 1;
            continue;
        }
        if proxy.source_trust == ClaimSourceType::CloudDraft {
            report.skipped_untrusted_cloud_draft_count += 1;
            continue;
        }
        let Some(envelope_section) = projected_envelope_section(&proxy) else {
            report.skipped_missing_evidence_count += 1;
            continue;
        };
        let envelope_section = envelope_section.to_string();
        if envelope_section == "stable_facts"
            && !semantic_proxy_has_trusted_semantic_claim(storage, &proxy)?
        {
            report.skipped_semantic_evidence_count += 1;
            continue;
        }

        report.deterministic_baseline_count += 1;
        report.deterministic_baseline_proxy_ids.push(proxy.id);
        let (score, matching_terms, mut reasons) =
            score_proxy(&query_tokens, &report.query, &proxy);
        if score < input.min_confidence {
            report.skipped_low_confidence_count += 1;
            continue;
        }
        reasons.push(format!(
            "deterministic_baseline_eligible: {}/{} -> {}",
            proxy.memory_layer, proxy.lifecycle_state, envelope_section
        ));
        reasons.push("prediction_is_read_only_no_lifecycle_promotion".to_string());
        let predicted_action = predicted_action(&proxy.memory_layer, &envelope_section);
        report.predictions.push(LatentProxyPrediction {
            proxy_id: proxy.id,
            episode_id: proxy.episode_id,
            proxy_type: proxy.proxy_type,
            target: proxy.target,
            scope: proxy.scope,
            claim: proxy.claim,
            memory_layer: proxy.memory_layer.clone(),
            lifecycle_state: proxy.lifecycle_state,
            envelope_section,
            source_trust: proxy.source_trust.as_str().to_string(),
            confidence: proxy.confidence.clamp(0.0, 1.0),
            score,
            matching_terms,
            reasons,
            predicted_action,
            evidence_refs: proxy.evidence_refs,
        });
    }

    report.predictions.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.proxy_id.cmp(&b.proxy_id))
    });
    report.predictions.truncate(input.limit);
    report.predicted_count = report.predictions.len();
    report.fallback_to_deterministic_projection = report.predictions.is_empty();
    Ok(report)
}

pub fn render_latent_interface_packet(
    storage: &Storage,
    input: LatentInterfacePacketInput,
) -> Result<LatentInterfacePacket, StorageError> {
    let prediction_report = predict_latent_proxies(
        storage,
        LatentProxyPredictionInput {
            query: input.query,
            project: input.project,
            session_id: input.session_id,
            limit: input.limit,
            scan_limit: input.scan_limit,
            min_confidence: input.min_confidence,
        },
    )?;
    let proxy_bindings = prediction_report
        .predictions
        .iter()
        .map(|prediction| LatentInterfaceProxyBinding {
            latent_ref: format!("evidence_latent_proxy:{}", prediction.proxy_id),
            proxy_id: prediction.proxy_id,
            envelope_section: prediction.envelope_section.clone(),
            lifecycle_state: prediction.lifecycle_state.clone(),
            source_trust: prediction.source_trust.clone(),
            score: prediction.score,
            matching_terms: prediction.matching_terms.clone(),
            ranking_reasons: prediction.reasons.clone(),
            textual_projection: format!(
                "{} -> {}: {}",
                prediction.proxy_type, prediction.envelope_section, prediction.claim
            ),
            evidence_refs: prediction.evidence_refs.clone(),
        })
        .collect::<Vec<_>>();
    let textual_fallback = LatentTextualFallback {
        format: "ranked_evidence_backed_proxy_claims_with_citations",
        proxy_count: proxy_bindings.len(),
        projection: render_textual_fallback(&proxy_bindings),
    };
    Ok(LatentInterfacePacket {
        kind: "latent_interface_packet",
        schema: LATENT_INTERFACE_PACKET_SCHEMA,
        source: LATENT_INTERFACE_PACKET_SOURCE,
        mode: "read_only_textual_fallback_packet",
        trust_boundary: "read_only advanced latent interface packet; includes no raw vectors, no hidden-state injection, no uninspectable latent payload, and creates no claim, verification event, proposal, lifecycle transition, or ContextEnvelope mutation",
        query: prediction_report.query.clone(),
        project: prediction_report.project.clone(),
        session_id: prediction_report.session_id.clone(),
        latent_channel: LatentChannelPolicy {
            requested_channel: "future_non_token_state",
            current_transport: "textual_fallback_over_existing_cloud_context_channels",
            vector_payload_included: false,
            hidden_state_injection_supported: false,
            reason: "current production cloud-local protocol accepts inspectable text/JSON artifacts, not provider hidden states or prompt-induced raw vectors",
            acceptance_requirements: vec![
                "provider_api_supports_explicit_non_token_state",
                "latent_payload_is_inspectable_or_cites_evidence_backed_proxy_refs",
                "zero_cloud_draft_proxy_bindings",
                "privacy_gate_passed_for_every_proxy_binding",
                "deterministic_textual_fallback_remains_available",
            ],
        },
        proxy_binding_count: proxy_bindings.len(),
        proxy_bindings,
        textual_fallback,
        prediction_report,
    })
}

fn render_textual_fallback(bindings: &[LatentInterfaceProxyBinding]) -> String {
    if bindings.is_empty() {
        return "No eligible evidence-backed latent proxies passed query, privacy, source-trust, and confidence gates; use deterministic ContextEnvelope projection.".to_string();
    }
    bindings
        .iter()
        .enumerate()
        .map(|(idx, binding)| {
            let evidence = binding
                .evidence_refs
                .iter()
                .map(|ev| format!("{}:{}", ev.kind, ev.id))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}. {} [latent_ref={}, score={:.3}, evidence={}]",
                idx + 1,
                binding.textual_projection,
                binding.latent_ref,
                binding.score,
                evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn episode_matches_scope(
    episode_project: Option<&str>,
    episode_session_id: Option<&str>,
    project: Option<&str>,
    session_id: Option<&str>,
) -> bool {
    if let Some(project) = project {
        if episode_project != Some(project) {
            return false;
        }
    }
    if let Some(session_id) = session_id {
        if episode_session_id != Some(session_id) {
            return false;
        }
    }
    true
}

fn proxy_cloud_safe(proxy: &EvidenceBackedLatentProxy) -> bool {
    !proxy.privacy_labels.is_empty()
        && proxy.privacy_labels.iter().all(|label| {
            matches!(label, SensitivityLabel::Public | SensitivityLabel::ProjectInternal)
        })
}

fn projected_envelope_section(proxy: &EvidenceBackedLatentProxy) -> Option<&str> {
    match (proxy.memory_layer.as_str(), proxy.lifecycle_state.as_str()) {
        ("short_term", "short_term_candidate") => Some("short_term_candidates"),
        ("long_term", "long_term_memory") => Some("relevant_memory"),
        ("semantic", "semantic_fact") => proxy.envelope_section.as_deref(),
        _ => None,
    }
}

fn semantic_proxy_has_trusted_semantic_claim(
    storage: &Storage,
    proxy: &EvidenceBackedLatentProxy,
) -> Result<bool, StorageError> {
    for ev in proxy.evidence_refs.iter().filter(|ev| ev.kind == "claim") {
        let Ok(claim_id) = ev.id.parse::<i64>() else {
            continue;
        };
        let Some(claim) = storage.claim_record(claim_id)? else {
            continue;
        };
        if claim.lifecycle_state == LifecycleState::SemanticFact
            && storage.claim_has_durable_promotion_trust(claim.id)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn score_proxy(
    query_tokens: &[String],
    query: &str,
    proxy: &EvidenceBackedLatentProxy,
) -> (f32, Vec<String>, Vec<String>) {
    let text = proxy_text(proxy);
    let doc_tokens = tokenize(&text);
    let doc_set = doc_tokens.iter().cloned().collect::<BTreeSet<_>>();
    let mut matching_terms =
        query_tokens.iter().filter(|token| doc_set.contains(*token)).cloned().collect::<Vec<_>>();
    matching_terms.sort();
    matching_terms.dedup();

    let overlap = if query_tokens.is_empty() {
        0.0
    } else {
        matching_terms.len() as f32 / query_tokens.len() as f32
    };
    let confidence = proxy.confidence.clamp(0.0, 1.0);
    let lifecycle_prior = match proxy.memory_layer.as_str() {
        "semantic" => 1.0,
        "long_term" => 0.85,
        "short_term" => 0.65,
        _ => 0.4,
    };
    let access_bonus = ((proxy.access_count.max(0) + 1) as f32).ln() / 10.0_f32.ln();
    let access_bonus = access_bonus.clamp(0.0, 1.0);
    let mut score = 0.55 * overlap
        + 0.25 * confidence
        + 0.10 * lifecycle_prior
        + 0.05 * proxy.decay_score.clamp(0.0, 1.0)
        + 0.05 * access_bonus;

    let query_lower = query.trim().to_lowercase();
    if !query_lower.is_empty() && text.to_lowercase().contains(&query_lower) {
        score += 0.15;
    }
    score = score.clamp(0.0, 1.0);

    let mut reasons = Vec::new();
    if !matching_terms.is_empty() {
        reasons.push(format!("query_overlap_terms: {}", matching_terms.join(", ")));
    }
    reasons.push(format!("proxy_confidence: {:.3}", confidence));
    reasons.push(format!("lifecycle_prior: {:.2}", lifecycle_prior));
    if proxy.access_count > 0 {
        reasons.push(format!("access_count: {}", proxy.access_count));
    }
    (round_score(score), matching_terms, reasons)
}

fn proxy_text(proxy: &EvidenceBackedLatentProxy) -> String {
    let mut text = String::new();
    text.push_str(&proxy.claim);
    text.push(' ');
    text.push_str(&proxy.proxy_type);
    if let Some(target) = &proxy.target {
        text.push(' ');
        text.push_str(target);
    }
    if let Some(scope) = &proxy.scope {
        text.push(' ');
        text.push_str(scope);
    }
    if let Some(section) = &proxy.envelope_section {
        text.push(' ');
        text.push_str(section);
    }
    text
}

fn predicted_action(memory_layer: &str, envelope_section: &str) -> &'static str {
    match memory_layer {
        "short_term" => "surface_as_short_term_candidate_review_only",
        "long_term" => "eligible_for_relevant_memory_selection",
        "semantic" => match envelope_section {
            "open_decisions" => "eligible_for_open_decision_projection",
            "user_policy" => "eligible_for_user_policy_projection",
            "corrections" => "eligible_for_correction_projection",
            _ => "eligible_for_semantic_projection",
        },
        _ => "inspect_only",
    }
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            push_token(&mut out, &mut current);
        }
    }
    if !current.is_empty() {
        push_token(&mut out, &mut current);
    }
    out.sort();
    out.dedup();
    out
}

fn push_token(out: &mut Vec<String>, current: &mut String) {
    if current.chars().count() >= 2 {
        out.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn round_score(score: f32) -> f32 {
    (score * 1000.0).round() / 1000.0
}
