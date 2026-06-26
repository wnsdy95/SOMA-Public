//! Read-only evaluation reports for the latent proxy predictor.
//!
//! The predictor is intentionally not a promotion path. This module makes that
//! boundary testable by scoring expected proxy hits against the active
//! deterministic baseline without inserting claims, verification events,
//! proposals, lifecycle events, or ContextEnvelope mutations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::context::latent_predictor::{
    optional_inference_field_policy, predict_latent_proxies, LatentProxyPredictionInput,
    OptionalInferenceFieldPolicy, LATENT_PROXY_PREDICTOR_RULE, LATENT_PROXY_PREDICTOR_SOURCE,
};
use crate::storage::{ClaimSourceType, SensitivityLabel, Storage, StorageError};

pub const LATENT_PROXY_EVAL_SOURCE: &str = "soma_latent_proxy_eval";
pub const DEFAULT_LATENT_EVAL_CASE_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct LatentProxyEvalInput {
    pub cases: Vec<LatentProxyEvalCase>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub scan_limit: usize,
    pub min_confidence: f32,
    pub case_source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatentProxyEvalCase {
    pub id: String,
    pub description: Option<String>,
    pub source: String,
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub expected_proxy_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentProxyEvalReport {
    pub kind: &'static str,
    pub source: &'static str,
    pub predictor_source: &'static str,
    pub predictor_rule: &'static str,
    pub mode: &'static str,
    pub trust_boundary: &'static str,
    pub case_source: String,
    pub case_count: usize,
    pub scored_case_count: usize,
    pub prediction_hit_count: usize,
    pub prediction_hit_rate: f32,
    pub deterministic_baseline_hit_count: usize,
    pub deterministic_baseline_hit_rate: f32,
    pub deterministic_baseline_parity_passed: bool,
    pub fallback_count: usize,
    pub cloud_draft_prediction_count: usize,
    pub optional_inference_fields_admissible: bool,
    pub optional_inference_policy: OptionalInferenceFieldPolicy,
    pub limit: usize,
    pub scan_limit: usize,
    pub min_confidence: f32,
    pub cases: Vec<LatentProxyEvalCaseReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatentProxyEvalCaseReport {
    pub id: String,
    pub description: Option<String>,
    pub source: String,
    pub query: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub expected_proxy_ids: Vec<i64>,
    pub predicted_proxy_ids: Vec<i64>,
    pub deterministic_baseline_proxy_ids: Vec<i64>,
    pub prediction_hit: bool,
    pub deterministic_baseline_hit: bool,
    pub fallback_to_deterministic_projection: bool,
    pub predicted_count: usize,
    pub top_prediction_score: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct RawLatentProxyEvalCase {
    id: Option<String>,
    description: Option<String>,
    source: Option<String>,
    query: String,
    project: Option<String>,
    session_id: Option<String>,
    expected_proxy_id: Option<i64>,
    expected_proxy_ids: Option<Vec<i64>>,
}

pub fn parse_latent_eval_cases_jsonl(
    jsonl: &str,
) -> Result<Vec<LatentProxyEvalCase>, StorageError> {
    let mut cases = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let raw: RawLatentProxyEvalCase =
            serde_json::from_str(trimmed).map_err(|err| StorageError::Corrupt {
                detail: format!("latent eval JSONL line {} is invalid: {err}", idx + 1),
            })?;
        let mut expected_proxy_ids = raw.expected_proxy_ids.unwrap_or_default();
        if let Some(expected_proxy_id) = raw.expected_proxy_id {
            expected_proxy_ids.push(expected_proxy_id);
        }
        expected_proxy_ids.sort_unstable();
        expected_proxy_ids.dedup();
        let id = raw.id.unwrap_or_else(|| format!("jsonl-line-{}", idx + 1));
        cases.push(validate_eval_case(LatentProxyEvalCase {
            id,
            description: raw.description,
            source: raw.source.unwrap_or_else(|| "jsonl".to_string()),
            query: raw.query,
            project: raw.project,
            session_id: raw.session_id,
            expected_proxy_ids,
        })?);
    }
    Ok(cases)
}

pub fn build_storage_latent_eval_cases(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    scan_limit: usize,
    case_limit: usize,
) -> Result<Vec<LatentProxyEvalCase>, StorageError> {
    if scan_limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent eval scan_limit must be greater than 0".to_string(),
        });
    }
    if case_limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent eval case_limit must be greater than 0".to_string(),
        });
    }

    let mut cases = Vec::new();
    for proxy in storage.active_evidence_latent_proxies(scan_limit)? {
        if cases.len() >= case_limit {
            break;
        }
        if proxy.evidence_refs.is_empty()
            || proxy.source_trust == ClaimSourceType::CloudDraft
            || !proxy_cloud_safe_for_eval(&proxy.privacy_labels)
        {
            continue;
        }
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            continue;
        };
        if !matches_scope(
            episode.project.as_deref(),
            episode.session_id.as_deref(),
            project,
            session_id,
        ) {
            continue;
        }
        let case_project = episode.project.clone();
        let case_session_id = episode.session_id.clone();
        let eligibility = predict_latent_proxies(
            storage,
            LatentProxyPredictionInput {
                query: proxy.claim.clone(),
                project: case_project.clone(),
                session_id: case_session_id.clone(),
                limit: 1,
                scan_limit,
                min_confidence: 0.0,
            },
        )?;
        if !eligibility.deterministic_baseline_proxy_ids.contains(&proxy.id) {
            continue;
        }
        cases.push(LatentProxyEvalCase {
            id: format!("storage-proxy-{}", proxy.id),
            description: Some(format!(
                "Storage-derived active evidence proxy {} should be recoverable from its claim.",
                proxy.id
            )),
            source: "storage_active_prediction_eligible_proxy".to_string(),
            query: proxy.claim,
            project: case_project,
            session_id: case_session_id,
            expected_proxy_ids: vec![proxy.id],
        });
    }
    Ok(cases)
}

pub fn build_task_frame_outcome_latent_eval_cases(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    case_limit: usize,
) -> Result<Vec<LatentProxyEvalCase>, StorageError> {
    if case_limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent eval case_limit must be greater than 0".to_string(),
        });
    }

    let mut cases = Vec::new();
    for outcome in storage.task_frame_outcomes_scoped(project, session_id, None, case_limit * 4)? {
        if cases.len() >= case_limit {
            break;
        }
        if outcome.latent_proxy_ids.is_empty() {
            continue;
        }
        let Some(task_frame) = storage.task_frame(outcome.task_frame_id)? else {
            continue;
        };
        cases.push(LatentProxyEvalCase {
            id: format!("task-frame-outcome-{}", outcome.id),
            description: Some(format!(
                "TaskFrame outcome {} marked latent proxy ids as useful evidence.",
                outcome.outcome_type
            )),
            source: "task_frame_outcome".to_string(),
            query: format!("{} {}", task_frame.goal_state, outcome.summary),
            project: task_frame.project,
            session_id: task_frame.session_id,
            expected_proxy_ids: outcome.latent_proxy_ids,
        });
    }
    Ok(cases)
}

pub fn evaluate_latent_predictor(
    storage: &Storage,
    input: LatentProxyEvalInput,
) -> Result<LatentProxyEvalReport, StorageError> {
    validate_eval_input(&input)?;
    let mut cases = Vec::new();
    let mut prediction_hit_count = 0;
    let mut deterministic_baseline_hit_count = 0;
    let mut fallback_count = 0;
    let mut cloud_draft_prediction_count = 0;

    for case in input.cases {
        let case = validate_eval_case(case)?;
        let project = case.project.clone().or_else(|| input.project.clone());
        let session_id = case.session_id.clone().or_else(|| input.session_id.clone());
        let report = predict_latent_proxies(
            storage,
            LatentProxyPredictionInput {
                query: case.query.clone(),
                project: project.clone(),
                session_id: session_id.clone(),
                limit: input.limit,
                scan_limit: input.scan_limit,
                min_confidence: input.min_confidence,
            },
        )?;
        let expected = case.expected_proxy_ids.iter().copied().collect::<BTreeSet<_>>();
        let predicted_proxy_ids =
            report.predictions.iter().map(|prediction| prediction.proxy_id).collect::<Vec<_>>();
        let deterministic_baseline_proxy_ids = report
            .deterministic_baseline_proxy_ids
            .iter()
            .copied()
            .take(input.limit)
            .collect::<Vec<_>>();
        let prediction_hit = predicted_proxy_ids.iter().any(|id| expected.contains(id));
        let deterministic_baseline_hit =
            deterministic_baseline_proxy_ids.iter().any(|id| expected.contains(id));
        if prediction_hit {
            prediction_hit_count += 1;
        }
        if deterministic_baseline_hit {
            deterministic_baseline_hit_count += 1;
        }
        if report.fallback_to_deterministic_projection {
            fallback_count += 1;
        }
        cloud_draft_prediction_count += report
            .predictions
            .iter()
            .filter(|prediction| prediction.source_trust == ClaimSourceType::CloudDraft.as_str())
            .count();

        cases.push(LatentProxyEvalCaseReport {
            id: case.id,
            description: case.description,
            source: case.source,
            query: case.query,
            project,
            session_id,
            expected_proxy_ids: case.expected_proxy_ids,
            predicted_proxy_ids,
            deterministic_baseline_proxy_ids,
            prediction_hit,
            deterministic_baseline_hit,
            fallback_to_deterministic_projection: report.fallback_to_deterministic_projection,
            predicted_count: report.predicted_count,
            top_prediction_score: report.predictions.first().map(|prediction| prediction.score),
        });
    }

    let scored_case_count = cases.len();
    let prediction_hit_rate = ratio(prediction_hit_count, scored_case_count);
    let deterministic_baseline_hit_rate =
        ratio(deterministic_baseline_hit_count, scored_case_count);
    let deterministic_baseline_parity_passed = scored_case_count > 0
        && prediction_hit_rate >= deterministic_baseline_hit_rate
        && cloud_draft_prediction_count == 0;
    Ok(LatentProxyEvalReport {
        kind: "latent_proxy_eval",
        source: LATENT_PROXY_EVAL_SOURCE,
        predictor_source: LATENT_PROXY_PREDICTOR_SOURCE,
        predictor_rule: LATENT_PROXY_PREDICTOR_RULE,
        mode: "read_only_eval",
        trust_boundary: "read-only evaluation over evidence-backed latent proxy predictions; creates no claim, verification event, proposal, lifecycle transition, or ContextEnvelope mutation; cloud_draft predictions are a failure signal, never accepted evidence",
        case_source: input.case_source,
        case_count: scored_case_count,
        scored_case_count,
        prediction_hit_count,
        prediction_hit_rate,
        deterministic_baseline_hit_count,
        deterministic_baseline_hit_rate,
        deterministic_baseline_parity_passed,
        fallback_count,
        cloud_draft_prediction_count,
        optional_inference_fields_admissible: deterministic_baseline_parity_passed,
        optional_inference_policy: optional_inference_field_policy(),
        limit: input.limit,
        scan_limit: input.scan_limit,
        min_confidence: input.min_confidence,
        cases,
    })
}

fn validate_eval_input(input: &LatentProxyEvalInput) -> Result<(), StorageError> {
    if input.limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent eval limit must be greater than 0".to_string(),
        });
    }
    if input.scan_limit == 0 {
        return Err(StorageError::Corrupt {
            detail: "latent eval scan_limit must be greater than 0".to_string(),
        });
    }
    if input.scan_limit < input.limit {
        return Err(StorageError::Corrupt {
            detail: "latent eval scan_limit must be greater than or equal to limit".to_string(),
        });
    }
    if !input.min_confidence.is_finite() || !(0.0..=1.0).contains(&input.min_confidence) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "latent eval min_confidence must be finite within [0,1], got {}",
                input.min_confidence
            ),
        });
    }
    Ok(())
}

fn validate_eval_case(case: LatentProxyEvalCase) -> Result<LatentProxyEvalCase, StorageError> {
    if case.id.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "latent eval case id must be non-empty".to_string(),
        });
    }
    if case.query.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: format!("latent eval case `{}` query must be non-empty", case.id),
        });
    }
    if case.expected_proxy_ids.is_empty() {
        return Err(StorageError::Corrupt {
            detail: format!(
                "latent eval case `{}` requires at least one expected proxy id",
                case.id
            ),
        });
    }
    Ok(case)
}

fn matches_scope(
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

fn proxy_cloud_safe_for_eval(labels: &[SensitivityLabel]) -> bool {
    !labels.is_empty()
        && labels.iter().all(|label| {
            matches!(label, SensitivityLabel::Public | SensitivityLabel::ProjectInternal)
        })
}

fn ratio(count: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        ((count as f32 / total as f32) * 1000.0).round() / 1000.0
    }
}
