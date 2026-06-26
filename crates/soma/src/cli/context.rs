//! `soma context` handler — render the ContextEnvelope a cloud LLM
//! would receive through MCP.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::adapter_binding_proof::{
    build_client_binding_status_report, proof_session_render_evidence_artifact_path,
    run_proof_session_blocking, AdapterBindingProofContext,
};
use crate::cli::{
    AdapterBindingProofArgs, ContextArgs, ContextLearningProposalMode, ContextMode,
    ContextTaskFramesMode,
};
use crate::context::cloud_prompt::render_cloud_context_artifact;
use crate::context::compiler::{
    attach_local_compiler_note, load_local_compiler_config_from_home,
    resolve_local_compiler_runtime,
};
use crate::context::correction::{record_correction_with_report, CorrectionError, CorrectionInput};
use crate::context::envelope::{
    append_relevant_memory_items, apply_correction_overrides, attach_corrections,
    attach_open_decisions, attach_project_experience, attach_short_term_candidates,
    attach_stable_facts, attach_thread_state, attach_user_policy, build_context_envelope,
    render_json, render_xml, ContextEnvelope, ContextScope,
};
use crate::context::eval::{
    attach_required_client_proof_session_probe,
    attach_required_client_render_evidence_artifact_scan, audit_context_envelope,
    audit_latent_interface_packet, audit_review_backlog, audit_review_control_binding_manifest,
    audit_review_interaction_contract, audit_storage_trust_boundary, audit_task_frame_projection,
    audit_task_frame_retention_hygiene, build_product_hardening_report,
    build_product_hardening_scope_resolution, build_required_client_proof_matrix,
    client_binding_config_root_probe_hint, effective_required_client_names,
    normalize_required_client_names, refresh_required_client_proof_matrix_operator_action,
    ClientBindingHardeningAudit, ClientBindingHardeningClientSnapshot,
    ProductHardeningEvidenceArtifactFailure, ProductHardeningRequirements,
    ProductHardeningScopeResolutionInput,
};
#[cfg(feature = "cognitive")]
use crate::context::eval::{compare_relevant_memory_ranking_corpus, RelevantMemoryRankingCase};
use crate::context::latent_eval::{
    build_storage_latent_eval_cases, build_task_frame_outcome_latent_eval_cases,
    evaluate_latent_predictor, parse_latent_eval_cases_jsonl, LatentProxyEvalInput,
};
use crate::context::latent_predictor::{
    predict_latent_proxies, render_latent_interface_packet, LatentInterfacePacketInput,
    LatentProxyPredictionInput,
};
use crate::context::pack::{build_memory_pack, PackConfig, PackError};
use crate::context::quality::{
    correction_signals_from_storage_scoped, open_decisions_from_storage_scoped_with_corrections,
    project_experience_from_storage, relevant_memory_proxies_from_storage,
    short_term_candidates_from_storage, stable_facts_from_storage,
    user_policy_from_storage_with_corrections, DEFAULT_CORRECTION_LIMIT,
    DEFAULT_OPEN_DECISION_LIMIT, DEFAULT_PROJECT_EXPERIENCE_EVIDENCE_LIMIT,
    DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT, DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
    DEFAULT_STABLE_FACT_LIMIT,
};
use crate::context::review::{
    acknowledge_review_digest, build_review_batch_template, build_review_digest,
    build_review_render_plan, build_review_report, render_review_render_plan_html,
    resolve_verification_targets, ReviewBatchTemplateInput, ReviewDigestAckInput,
    ReviewDigestInput, ReviewRenderInput, ReviewReportInput, VerificationTargetInput,
};
use crate::context::review_action::{
    apply_review_action, apply_review_batch, ReviewAction, ReviewActionError, ReviewActionInput,
    ReviewBatchInput, ReviewTarget,
};
use crate::context::scheduler_control::{
    normalize_scheduler_control_passes, run_scheduler_control, SchedulerControlInput,
    DEFAULT_L2_PROMOTION_REASON, DEFAULT_L3_DECAY_REASON,
};
use crate::context::scope::inferred_project_scope_from_anil;
use crate::context::semantic_learning::{
    SemanticLearningItem, SemanticLearningReport, SEMANTIC_EXACT_GROUP_RULE,
    SEMANTIC_TOKEN_GROUP_RULE,
};
use crate::context::task_frame::{
    build_task_frame, task_frame_thread_state_section, TaskFrameBuildInput,
};
use crate::context::thread_identity::{build_thread_identity_report, ThreadIdentityReportInput};
use crate::storage::{
    task_frame_retention_cutoff_ns, LearningCriticApplyOptions, LearningCriticProposalStatus,
    ShortTermProxyPromotionRequest, Storage, StorageError, StoredEvidenceRef, StoredTaskFrame,
    TaskFrameOutcomeDraft, TaskFrameOutcomeType, TaskFrameRetentionRequest, ThreadIdentityDraft,
    VerificationEventDraft, VerificationResult, VerifierType,
};

#[derive(Debug)]
pub enum ContextError {
    Storage(StorageError),
    Pack(PackError),
    Correction(CorrectionError),
    ReviewAction(ReviewActionError),
    Path(String),
    BadFormat(String),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextError::Storage(e) => write!(f, "storage: {e}"),
            ContextError::Pack(e) => write!(f, "pack: {e}"),
            ContextError::Correction(e) => write!(f, "correction: {e}"),
            ContextError::ReviewAction(e) => write!(f, "review action: {e}"),
            ContextError::Path(m) => write!(f, "path: {m}"),
            ContextError::BadFormat(m) => write!(f, "bad format: {m}"),
        }
    }
}

impl std::error::Error for ContextError {}

impl From<StorageError> for ContextError {
    fn from(e: StorageError) -> Self {
        ContextError::Storage(e)
    }
}

impl From<PackError> for ContextError {
    fn from(e: PackError) -> Self {
        ContextError::Pack(e)
    }
}

impl From<CorrectionError> for ContextError {
    fn from(e: CorrectionError) -> Self {
        ContextError::Correction(e)
    }
}

impl From<ReviewActionError> for ContextError {
    fn from(e: ReviewActionError) -> Self {
        ContextError::ReviewAction(e)
    }
}

#[derive(Debug, Clone)]
pub struct ContextCliContext {
    pub db_path: PathBuf,
}

pub fn run_context(args: &ContextArgs, ctx: &ContextCliContext) -> Result<String, ContextError> {
    match &args.mode {
        ContextMode::Render(render) => run_render(render, ctx),
        ContextMode::Prompt(prompt) => run_prompt(prompt, ctx),
        ContextMode::TaskFrame(task_frame) => run_task_frame(task_frame, ctx),
        ContextMode::TaskFrames(task_frames) => run_task_frames(task_frames, ctx),
        ContextMode::TaskFrameOutcome(outcome) => run_task_frame_outcome(outcome, ctx),
        ContextMode::L3Decay(decay) => run_l3_decay(decay, ctx),
        ContextMode::L2Promote(promote) => run_l2_promote(promote, ctx),
        ContextMode::LatentPredict(predict) => run_latent_predict(predict, ctx),
        ContextMode::LatentPacket(packet) => run_latent_packet(packet, ctx),
        ContextMode::LatentEval(eval) => run_latent_eval(eval, ctx),
        ContextMode::ThreadIdentity(identity) => run_thread_identity(identity, ctx),
        ContextMode::Correct(correct) => run_correct(correct, ctx),
        ContextMode::VerifyClaim(verify) => run_verify_claim(verify, ctx),
        ContextMode::LearningProposals(proposals) => run_learning_proposals(proposals, ctx),
        ContextMode::ReviewQueue(review) => run_review_queue(review, ctx),
        ContextMode::ReviewActions(actions) => run_review_actions(actions, ctx),
        ContextMode::ReviewBatchTemplate(template) => run_review_batch_template(template, ctx),
        ContextMode::ReviewReport(report) => run_review_report(report, ctx),
        ContextMode::ReviewDigest(digest) => run_review_digest(digest, ctx),
        ContextMode::ReviewDigestAck(ack) => run_review_digest_ack(ack, ctx),
        ContextMode::ReviewRender(render) => run_review_render(render, ctx),
        ContextMode::ReviewDrain(drain) => run_review_drain(drain, ctx),
        ContextMode::SchedulerRun(scheduler) => run_scheduler_run(scheduler, ctx),
        ContextMode::SemanticProposals(proposals) => run_semantic_proposals(proposals, ctx),
        ContextMode::OpenDecisionProposals(proposals) => {
            run_open_decision_proposals(proposals, ctx)
        }
        ContextMode::ReviewAction(action) => run_review_action(action, ctx),
        ContextMode::ReviewBatch(batch) => run_review_batch(batch, ctx),
        ContextMode::Audit(audit) => run_audit(audit, ctx),
        ContextMode::TrustAudit(audit) => run_trust_audit(audit, ctx),
        ContextMode::HardeningReport(report) => run_hardening_report(report, ctx),
        ContextMode::Why(why) => run_why(why, ctx),
        #[cfg(feature = "cognitive")]
        ContextMode::CompareRanking(compare) => run_compare_ranking(compare, ctx),
    }
}

fn run_render(
    args: &crate::cli::ContextRenderArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let fmt = OutputFormat::parse(&args.format)?;
    let envelope = build_envelope_for_cli(
        ctx,
        args.query.clone(),
        args.project.clone(),
        args.session_id.clone(),
        args.local_compiler,
        args.local_compiler_endpoint.clone(),
        args.local_compiler_model.clone(),
        args.task_frame_id,
    )?;

    Ok(match fmt {
        OutputFormat::Xml => render_xml(&envelope),
        OutputFormat::Json => render_json(&envelope),
    })
}

fn run_prompt(
    args: &crate::cli::ContextPromptArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let envelope = build_envelope_for_cli(
        ctx,
        args.query.clone(),
        args.project.clone(),
        args.session_id.clone(),
        args.local_compiler,
        args.local_compiler_endpoint.clone(),
        args.local_compiler_model.clone(),
        args.task_frame_id,
    )?;
    let task_frame = match args.task_frame_id {
        Some(task_frame_id) => {
            let storage = Storage::open(&ctx.db_path)?;
            Some(storage.task_frame(task_frame_id)?.ok_or_else(|| StorageError::Corrupt {
                detail: format!("TaskFrame {task_frame_id} not found"),
            })?)
        }
        None => None,
    };
    Ok(render_cloud_context_artifact(&envelope, task_frame.as_ref()))
}

fn run_task_frame(
    args: &crate::cli::ContextTaskFrameArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let mut storage = Storage::open(&ctx.db_path)?;
    let cwd = args
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string()));
    let input = TaskFrameBuildInput {
        query: args.query.clone(),
        project: args.project.clone().or_else(crate::project::current_name),
        session_id: args.session_id.clone(),
        cwd,
        client: args.client.clone(),
        allow_local_private_projection: args.allow_local_private_projection,
        local_private_projection_reason: args.local_private_projection_reason.clone(),
    };
    let draft = build_task_frame(&storage, input)?;
    let id = storage.insert_task_frame(&draft)?;
    let stored = storage.task_frame(id)?.ok_or_else(|| StorageError::Corrupt {
        detail: format!("inserted TaskFrame {id} was not readable"),
    })?;
    let out = serde_json::json!({
        "task_frame_id": stored.id,
        "hash": stored.hash,
        "builder_version": stored.builder_version,
        "created_at_ns": stored.created_at_ns,
        "project": stored.project,
        "session_id": stored.session_id,
        "work_mode": stored.work_mode,
        "goal_state": stored.goal_state,
        "projection_policy": {
            "name": stored.projection_policy.name(),
            "allow_project_internal": stored.projection_policy.allow_project_internal,
            "allow_local_private": stored.projection_policy.allow_local_private,
            "explicit_reason": stored.projection_policy.explicit_reason.clone(),
            "allowed_sensitivity_labels": stored.projection_policy.allowed_sensitivity_labels(),
        },
        "blocked_fields": stored.blocked_fields,
        "cloud_redacted_json": stored.cloud_redacted_json,
        "local_full_json": stored.local_full_json,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_task_frames(
    args: &crate::cli::ContextTaskFramesArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    match &args.mode {
        ContextTaskFramesMode::Retention(retention) => run_task_frame_retention(retention, ctx),
        ContextTaskFramesMode::Outcomes(outcomes) => run_task_frame_outcomes(outcomes, ctx),
    }
}

fn run_task_frame_retention(
    args: &crate::cli::ContextTaskFrameRetentionArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.older_than_days < 1 {
        return Err(ContextError::BadFormat("older_than_days must be at least 1".to_string()));
    }
    let now_ns = now_ns();
    let cutoff_ns = task_frame_retention_cutoff_ns(now_ns, args.older_than_days)?;
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = storage.apply_task_frame_retention(&TaskFrameRetentionRequest {
        cutoff_ns,
        retention_days: args.older_than_days,
        project: args.project.clone(),
        session_id: args.session_id.clone(),
        apply: args.apply,
    })?;
    let out = serde_json::json!({
        "now_ns": now_ns,
        "policy": {
            "retention_days": report.retention_days,
            "cutoff_ns": report.cutoff_ns,
            "default_retention_days": crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS,
            "protects_claim_or_proposal_references": true,
        },
        "report": report,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_task_frame_outcome(
    args: &crate::cli::ContextTaskFrameOutcomeArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let outcome_type = TaskFrameOutcomeType::parse(&args.outcome_type)?;
    let evidence_ref = StoredEvidenceRef {
        kind: args.evidence_kind.clone(),
        id: args.evidence_id.clone(),
        source: args.evidence_source.clone(),
    };
    let mut storage = Storage::open(&ctx.db_path)?;
    let draft = TaskFrameOutcomeDraft {
        task_frame_id: args.task_frame_id,
        outcome_type,
        summary: args.summary.clone(),
        evidence_refs: vec![evidence_ref],
        claim_ids: args.claim_ids.clone(),
        proposal_ids: args.proposal_ids.clone(),
        latent_proxy_ids: args.latent_proxy_ids.clone(),
    };
    let id = storage.insert_task_frame_outcome(&draft)?;
    let outcomes = storage.task_frame_outcomes_scoped(None, None, Some(args.task_frame_id), 100)?;
    let outcome = outcomes.into_iter().find(|outcome| outcome.id == id).ok_or_else(|| {
        StorageError::Corrupt {
            detail: format!("inserted TaskFrame outcome {id} was not readable"),
        }
    })?;
    let out = serde_json::json!({
        "kind": "task_frame_outcome",
        "trust_boundary": "TaskFrame outcome records evaluation evidence only; it creates no claim, verification event, proposal, lifecycle transition, semantic fact, or ContextEnvelope mutation",
        "outcome": outcome,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_task_frame_outcomes(
    args: &crate::cli::ContextTaskFrameOutcomesArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let outcomes = storage.task_frame_outcomes_scoped(
        args.project.as_deref(),
        args.session_id.as_deref(),
        args.task_frame_id,
        args.limit,
    )?;
    let out = serde_json::json!({
        "kind": "task_frame_outcomes",
        "mode": "read_only",
        "trust_boundary": "TaskFrame outcome listing is read-only and records no verification, promotion, proposal, lifecycle, semantic, or ContextEnvelope mutation",
        "project": args.project,
        "session_id": args.session_id,
        "task_frame_id": args.task_frame_id,
        "limit": args.limit,
        "outcome_count": outcomes.len(),
        "outcomes": outcomes,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_l3_decay(
    args: &crate::cli::ContextL3DecayArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.older_than_days < 1 {
        return Err(ContextError::BadFormat("older_than_days must be at least 1".to_string()));
    }
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let (cutoff_ns, cutoff_source) = match args.cutoff_ns {
        Some(cutoff_ns) => (cutoff_ns, "explicit_cutoff_ns"),
        None => {
            (task_frame_retention_cutoff_ns(now_ns(), args.older_than_days)?, "older_than_days")
        }
    };
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = storage.decay_inactive_long_term_proxies(
        cutoff_ns,
        args.max_access_count,
        &args.reason,
        args.dry_run,
        args.limit,
    )?;
    let out = serde_json::json!({
        "kind": "l3_proxy_decay",
        "policy": {
            "older_than_days": args.older_than_days,
            "max_access_count": args.max_access_count,
            "reason": args.reason,
            "dry_run": args.dry_run,
            "limit": args.limit,
            "cutoff_ns": cutoff_ns,
            "cutoff_source": cutoff_source,
            "storage_transition": "long_term_memory -> decayed",
            "evidence_behavior": "original episode/proxy evidence remains stored; ContextEnvelope relevant_memory excludes decayed L3 proxies",
        },
        "report": report,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_l2_promote(
    args: &crate::cli::ContextL2PromoteArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.min_repeated_support < 2 {
        return Err(ContextError::BadFormat("min_repeated_support must be at least 2".to_string()));
    }
    if !args.min_confidence.is_finite() || !(0.0..=1.0).contains(&args.min_confidence) {
        return Err(ContextError::BadFormat(format!(
            "min_confidence must be finite within [0,1], got {}",
            args.min_confidence
        )));
    }
    if !args.anomaly_min_confidence.is_finite()
        || !(0.0..=1.0).contains(&args.anomaly_min_confidence)
    {
        return Err(ContextError::BadFormat(format!(
            "anomaly_min_confidence must be finite within [0,1], got {}",
            args.anomaly_min_confidence
        )));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = storage.promote_short_term_proxies_by_policy(&ShortTermProxyPromotionRequest {
        project: args.project.clone(),
        session_id: args.session_id.clone(),
        dry_run: !args.apply,
        min_confidence: args.min_confidence,
        anomaly_min_confidence: args.anomaly_min_confidence,
        min_repeated_support: args.min_repeated_support,
        manual_proxy_ids: args.manual_proxy_ids.clone(),
        reason: args.reason.clone(),
        limit: args.limit,
    })?;
    let out = serde_json::json!({
        "kind": "l2_proxy_promotion",
        "trust_boundary": "L2 proxy promotion is policy-selected, evidence-backed, dry-run by default, and still blocks cloud_draft or unsafe privacy labels before L3 lifecycle transition",
        "apply": args.apply,
        "report": report,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_latent_predict(
    args: &crate::cli::ContextLatentPredictArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.scan_limit == 0 {
        return Err(ContextError::BadFormat("scan-limit must be greater than 0".to_string()));
    }
    if args.scan_limit < args.limit {
        return Err(ContextError::BadFormat(
            "scan-limit must be greater than or equal to limit".to_string(),
        ));
    }
    if !args.min_confidence.is_finite() || !(0.0..=1.0).contains(&args.min_confidence) {
        return Err(ContextError::BadFormat(
            "min-confidence must be finite within [0,1]".to_string(),
        ));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let report = predict_latent_proxies(
        &storage,
        LatentProxyPredictionInput {
            query: args.query.clone(),
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            scan_limit: args.scan_limit,
            min_confidence: args.min_confidence,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_latent_packet(
    args: &crate::cli::ContextLatentPacketArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.scan_limit == 0 {
        return Err(ContextError::BadFormat("scan-limit must be greater than 0".to_string()));
    }
    if args.scan_limit < args.limit {
        return Err(ContextError::BadFormat(
            "scan-limit must be greater than or equal to limit".to_string(),
        ));
    }
    if !args.min_confidence.is_finite() || !(0.0..=1.0).contains(&args.min_confidence) {
        return Err(ContextError::BadFormat(
            "min-confidence must be finite within [0,1]".to_string(),
        ));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let packet = render_latent_interface_packet(
        &storage,
        LatentInterfacePacketInput {
            query: args.query.clone(),
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            scan_limit: args.scan_limit,
            min_confidence: args.min_confidence,
        },
    )?;
    let text = serde_json::to_string_pretty(&packet).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_latent_eval(
    args: &crate::cli::ContextLatentEvalArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.scan_limit == 0 {
        return Err(ContextError::BadFormat("scan-limit must be greater than 0".to_string()));
    }
    if args.scan_limit < args.limit {
        return Err(ContextError::BadFormat(
            "scan-limit must be greater than or equal to limit".to_string(),
        ));
    }
    if args.case_limit == 0 {
        return Err(ContextError::BadFormat("case-limit must be greater than 0".to_string()));
    }
    if !args.min_confidence.is_finite() || !(0.0..=1.0).contains(&args.min_confidence) {
        return Err(ContextError::BadFormat(
            "min-confidence must be finite within [0,1]".to_string(),
        ));
    }

    let storage = Storage::open(&ctx.db_path)?;
    let (cases, case_source) = if let Some(path) = &args.case_jsonl {
        let jsonl = std::fs::read_to_string(path).map_err(|err| {
            ContextError::Path(format!("failed to read latent eval case JSONL `{path}`: {err}"))
        })?;
        (parse_latent_eval_cases_jsonl(&jsonl)?, format!("jsonl:{path}"))
    } else if args.outcome_cases {
        (
            build_task_frame_outcome_latent_eval_cases(
                &storage,
                args.project.as_deref(),
                args.session_id.as_deref(),
                args.case_limit,
            )?,
            "task_frame_outcome".to_string(),
        )
    } else {
        (
            build_storage_latent_eval_cases(
                &storage,
                args.project.as_deref(),
                args.session_id.as_deref(),
                args.scan_limit,
                args.case_limit,
            )?,
            "storage_active_prediction_eligible_proxy".to_string(),
        )
    };

    let report = evaluate_latent_predictor(
        &storage,
        LatentProxyEvalInput {
            cases,
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            scan_limit: args.scan_limit,
            min_confidence: args.min_confidence,
            case_source,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_thread_identity(
    args: &crate::cli::ContextThreadIdentityArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.join_window_minutes < 1 {
        return Err(ContextError::BadFormat("join_window_minutes must be at least 1".to_string()));
    }
    if args.confirm && args.list_confirmed {
        return Err(ContextError::BadFormat(
            "--confirm and --list-confirmed are mutually exclusive".to_string(),
        ));
    }
    if args.confirm {
        return run_thread_identity_confirm(args, ctx);
    }
    if args.list_confirmed {
        return run_thread_identity_list_confirmed(args, ctx);
    }
    let storage = Storage::open(&ctx.db_path)?;
    let episodes = storage.recent_episodes(args.limit)?;
    let report = build_thread_identity_report(
        &episodes,
        ThreadIdentityReportInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            join_window_minutes: args.join_window_minutes,
        },
    );
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_thread_identity_list_confirmed(
    args: &crate::cli::ContextThreadIdentityArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let storage = Storage::open(&ctx.db_path)?;
    let identities = storage.recent_thread_identities(args.project.as_deref(), args.limit)?;
    let out = serde_json::json!({
        "kind": "context_thread_identity_confirmed_list",
        "status": "read_only",
        "scope": {
            "project": args.project,
            "limit": args.limit,
        },
        "thread_identities": identities,
        "trust_boundary": thread_identity_trust_boundary_json(),
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_thread_identity_confirm(
    args: &crate::cli::ContextThreadIdentityArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let project =
        args.project.as_deref().map(str::trim).filter(|value| !value.is_empty()).ok_or_else(
            || {
                ContextError::BadFormat(
                    "--project is required when confirming thread identity".to_string(),
                )
            },
        )?;
    let sessions = normalize_confirm_sessions(&args.confirm_sessions)?;
    if sessions.len() > 1 && !args.confirm_cross_session {
        return Err(ContextError::BadFormat(
            "confirming more than one session requires --confirm-cross-session".to_string(),
        ));
    }
    let reason = args
        .confirmation_reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ContextError::BadFormat(
                "--confirmation-reason is required when confirming thread identity".to_string(),
            )
        })?;
    let confirmed_by = args.confirmed_by.trim();
    if confirmed_by.is_empty() {
        return Err(ContextError::BadFormat(
            "--confirmed-by must be non-empty when confirming thread identity".to_string(),
        ));
    }

    let mut storage = Storage::open(&ctx.db_path)?;
    for session_id in &sessions {
        if storage.session_has_live_episodes_outside_project(project, session_id)? {
            return Err(ContextError::BadFormat(format!(
                "session `{session_id}` has live episodes outside project `{project}`; resolve ambiguity before confirming a durable thread identity"
            )));
        }
    }
    let episodes = storage.live_episodes_for_thread_identity_sessions(project, &sessions)?;
    let found_sessions: std::collections::BTreeSet<&str> =
        episodes.iter().filter_map(|episode| episode.session_id.as_deref()).collect();
    for session_id in &sessions {
        if !found_sessions.contains(session_id.as_str()) {
            return Err(ContextError::BadFormat(format!(
                "session `{session_id}` has no live episodes in project `{project}`"
            )));
        }
    }
    let evidence_episode_ids: Vec<i64> = episodes.iter().map(|episode| episode.id).collect();
    let thread_key =
        args.thread_key.clone().unwrap_or_else(|| default_thread_key(project, &sessions));
    let stored = storage.confirm_thread_identity(&ThreadIdentityDraft {
        thread_key,
        project: project.to_string(),
        session_ids: sessions,
        evidence_episode_ids,
        confirmed_by: confirmed_by.to_string(),
        confirmation_reason: reason.to_string(),
        allow_cross_session: args.confirm_cross_session,
    })?;
    let out = serde_json::json!({
        "kind": "context_thread_identity_confirmation",
        "status": "operator_confirmed",
        "thread_identity": stored,
        "source": {
            "project": project,
            "session_count": stored.session_ids.len(),
            "evidence_episode_count": stored.evidence_episode_ids.len(),
        },
        "trust_boundary": thread_identity_trust_boundary_json(),
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn normalize_confirm_sessions(values: &[String]) -> Result<Vec<String>, ContextError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut sessions = Vec::new();
    for value in values {
        let session_id = value.trim();
        if session_id.is_empty() {
            return Err(ContextError::BadFormat(
                "--confirm-session values must be non-empty".to_string(),
            ));
        }
        if !seen.insert(session_id.to_string()) {
            return Err(ContextError::BadFormat(format!(
                "duplicate --confirm-session `{session_id}`"
            )));
        }
        sessions.push(session_id.to_string());
    }
    if sessions.is_empty() {
        return Err(ContextError::BadFormat(
            "--confirm requires at least one --confirm-session".to_string(),
        ));
    }
    Ok(sessions)
}

fn default_thread_key(project: &str, sessions: &[String]) -> String {
    let mut text = String::new();
    text.push_str(project);
    for session in sessions {
        text.push('\n');
        text.push_str(session);
    }
    format!("thread:{}:{}", sanitize_key(project), fnv_hash(&text))
}

fn thread_identity_trust_boundary_json() -> serde_json::Value {
    serde_json::json!({
        "operator_confirmation_required": true,
        "persistent_thread_ids_created": true,
        "context_thread_resource_enabled": true,
        "automatic_cross_session_merge_allowed": false,
        "promotion_or_claim_verification_allowed": false,
        "note": "Only operator-confirmed thread identities are exposed as concrete soma://context/thread/<thread_key> resources; this still does not auto-merge future sessions or verify/promote claims.",
    })
}

fn sanitize_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn fnv_hash(text: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn run_audit(
    args: &crate::cli::ContextAuditArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let envelope = build_envelope_for_cli(
        ctx,
        args.query.clone(),
        args.project.clone(),
        args.session_id.clone(),
        args.local_compiler,
        args.local_compiler_endpoint.clone(),
        args.local_compiler_model.clone(),
        args.task_frame_id,
    )?;
    let envelope_audit = audit_context_envelope(&envelope);
    let task_frame_audit = match args.task_frame_id {
        Some(task_frame_id) => {
            let storage = Storage::open(&ctx.db_path)?;
            let task_frame = storage.task_frame(task_frame_id)?.ok_or_else(|| {
                StorageError::Corrupt { detail: format!("TaskFrame {task_frame_id} not found") }
            })?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let task_frame_passed = task_frame_audit.as_ref().is_none_or(|audit| audit.passed());
    let out = serde_json::json!({
        "scope": envelope.scope,
        "assembled_at_ns": envelope.assembled_at_ns,
        "task_frame_id": args.task_frame_id,
        "passed": envelope_audit.passed() && task_frame_passed,
        "envelope": envelope_audit,
        "task_frame": task_frame_audit,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_trust_audit(
    args: &crate::cli::ContextTrustAuditArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let storage = Storage::open(&ctx.db_path)?;
    let audit = audit_storage_trust_boundary(
        &storage,
        args.project.as_deref(),
        args.session_id.as_deref(),
        args.limit,
    )?;
    let text = serde_json::to_string_pretty(&audit).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_hardening_report(
    args: &crate::cli::ContextHardeningReportArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.task_frame_retention_days < 1 {
        return Err(ContextError::BadFormat(
            "task_frame_retention_days must be at least 1".to_string(),
        ));
    }
    let envelope = build_envelope_for_cli(
        ctx,
        args.query.clone(),
        args.project.clone(),
        args.session_id.clone(),
        args.local_compiler,
        args.local_compiler_endpoint.clone(),
        args.local_compiler_model.clone(),
        args.task_frame_id,
    )?;
    let envelope_audit = audit_context_envelope(&envelope);
    let mut storage = Storage::open(&ctx.db_path)?;
    let audit_project = envelope.scope.project.clone();
    let audit_session_id = envelope.scope.session_id.clone();
    let storage_trust = audit_storage_trust_boundary(
        &storage,
        audit_project.as_deref(),
        audit_session_id.as_deref(),
        args.trust_limit,
    )?;
    let review_backlog = audit_review_backlog(
        &storage,
        audit_project.as_deref(),
        audit_session_id.as_deref(),
        args.review_limit,
    )?;
    let review_render_plan = build_review_render_plan(
        &storage,
        ReviewRenderInput {
            project: audit_project.clone(),
            session_id: audit_session_id.clone(),
            limit: args.review_limit,
            client: args.client.clone(),
            include_disabled: false,
        },
    )?;
    let review_interaction = audit_review_interaction_contract(&review_render_plan);
    let review_control_binding = audit_review_control_binding_manifest(&review_render_plan);
    let task_frame_retention = audit_task_frame_retention_hygiene(
        &mut storage,
        audit_project.as_deref(),
        audit_session_id.as_deref(),
        args.task_frame_retention_days,
        now_ns(),
    )?;
    let latent_interface = audit_latent_interface_packet(
        &storage,
        args.query.as_deref(),
        audit_project.as_deref(),
        audit_session_id.as_deref(),
    )?;
    let task_frame_audit = match args.task_frame_id {
        Some(task_frame_id) => {
            let task_frame = storage.task_frame(task_frame_id)?.ok_or_else(|| {
                StorageError::Corrupt { detail: format!("TaskFrame {task_frame_id} not found") }
            })?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let explicit_required_clients =
        normalize_required_client_names(args.required_clients.iter().map(String::as_str));
    let required_clients = effective_required_client_names(
        args.require_client_binding_ready,
        args.client.as_deref(),
        explicit_required_clients,
    );
    let client_binding_required = args.require_client_binding_ready || !required_clients.is_empty();
    let client_binding = if args.skip_client_binding {
        None
    } else {
        Some(client_binding_hardening_audit(
            &storage,
            &ctx.db_path,
            args.client.clone(),
            required_clients.clone(),
            args.client_proof_limit,
            args.client_binding_config_root.as_deref(),
        )?)
    };
    let scope_resolution =
        build_product_hardening_scope_resolution(ProductHardeningScopeResolutionInput {
            scope: &envelope.scope,
            explicit_project: args.project.as_deref(),
            explicit_session_id: args.session_id.as_deref(),
            task_frame_id: args.task_frame_id,
            cwd_project: crate::project::current_name(),
            override_command: hardening_report_scope_override_command(args),
        });
    let report = build_product_hardening_report(
        envelope.scope,
        scope_resolution,
        envelope.assembled_at_ns,
        args.task_frame_id,
        args.client.clone(),
        envelope_audit,
        storage_trust,
        review_backlog,
        review_interaction,
        review_control_binding,
        task_frame_retention,
        latent_interface,
        task_frame_audit,
        client_binding,
        required_clients,
        ProductHardeningRequirements {
            client_binding_ready: client_binding_required,
            review_queue_clear: args.require_review_queue_clear,
            task_frame_retention_clean: args.require_task_frame_retention_clean,
            task_frame_projection: args.require_task_frame_projection,
        },
    );
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn hardening_report_scope_override_command(
    args: &crate::cli::ContextHardeningReportArgs,
) -> Vec<String> {
    let Some(project) = crate::project::current_name() else {
        return Vec::new();
    };
    let project = project.trim();
    if project.is_empty() {
        return Vec::new();
    }

    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "hardening-report".to_string(),
        "--project".to_string(),
        project.to_string(),
    ];
    append_optional_flag_value(&mut command, "--query", args.query.as_deref());
    append_optional_flag_value(&mut command, "--session-id", args.session_id.as_deref());
    append_optional_flag_value(&mut command, "--client", args.client.as_deref());
    for client in &args.required_clients {
        append_optional_flag_value(&mut command, "--required-client", Some(client.as_str()));
    }
    append_optional_flag_value(
        &mut command,
        "--client-binding-config-root",
        args.client_binding_config_root.as_deref(),
    );
    if let Some(task_frame_id) = args.task_frame_id {
        command.push("--task-frame-id".to_string());
        command.push(task_frame_id.to_string());
    }
    if args.trust_limit != 1000 {
        command.push("--trust-limit".to_string());
        command.push(args.trust_limit.to_string());
    }
    if args.review_limit != 1000 {
        command.push("--review-limit".to_string());
        command.push(args.review_limit.to_string());
    }
    if args.client_proof_limit != 20 {
        command.push("--client-proof-limit".to_string());
        command.push(args.client_proof_limit.to_string());
    }
    if args.require_client_binding_ready {
        command.push("--require-client-binding-ready".to_string());
    }
    if args.require_review_queue_clear {
        command.push("--require-review-queue-clear".to_string());
    }
    if args.require_task_frame_retention_clean {
        command.push("--require-task-frame-retention-clean".to_string());
    }
    if args.require_task_frame_projection {
        command.push("--require-task-frame-projection".to_string());
    }
    if args.task_frame_retention_days != crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS {
        command.push("--task-frame-retention-days".to_string());
        command.push(args.task_frame_retention_days.to_string());
    }
    if args.skip_client_binding {
        command.push("--skip-client-binding".to_string());
    }
    if args.json {
        command.push("--json".to_string());
    }
    command
}

fn append_optional_flag_value(command: &mut Vec<String>, flag: &str, value: Option<&str>) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    command.push(flag.to_string());
    command.push(value.to_string());
}

fn client_binding_hardening_audit(
    storage: &Storage,
    db_path: &std::path::Path,
    client: Option<String>,
    required_clients: Vec<String>,
    limit: usize,
    client_binding_config_root: Option<&str>,
) -> Result<ClientBindingHardeningAudit, ContextError> {
    let client = client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let limit = limit.clamp(1, 200);
    let proof_client_filter = if required_clients.is_empty() { client.as_deref() } else { None };
    let proofs = storage.recent_client_binding_proofs(proof_client_filter, limit)?;
    let status = build_client_binding_status_report(client.clone(), None, limit, &proofs);
    let ready_client_count =
        status.clients.iter().filter(|client| client.ready_for_private_client_claim).count();
    let artifact_failure_count =
        status.clients.iter().map(|client| client.artifact_failures.len()).sum();
    let coherence_failure_count =
        status.clients.iter().map(|client| client.coherence_failures.len()).sum();
    let non_release_evidence_source_count: usize =
        status.clients.iter().map(|client| client.non_release_evidence_sources.len()).sum();
    let non_release_proof_levels: Vec<String> = status
        .clients
        .iter()
        .flat_map(|client| {
            client
                .non_release_evidence_sources
                .iter()
                .map(|source| source.proof_level.as_str().to_string())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let primary_readiness = status.clients.first().map(|client| client.readiness.clone());
    let primary_coherence_failures =
        status.clients.first().map(|client| client.coherence_failures.clone()).unwrap_or_default();
    let primary_non_release_evidence_sources = status
        .clients
        .first()
        .map(|client| {
            client
                .non_release_evidence_sources
                .iter()
                .map(|source| {
                    format!(
                        "{}:{}:{}",
                        source.proof_level.as_str(),
                        source.evidence_source,
                        source.reason
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let readiness_values = status.clients.iter().map(|client| client.readiness.clone()).collect();
    let client_snapshots: Vec<ClientBindingHardeningClientSnapshot> = status
        .clients
        .iter()
        .map(|client| ClientBindingHardeningClientSnapshot {
            client: client.client.clone(),
            readiness: client.readiness.clone(),
            ready_for_private_client_claim: client.ready_for_private_client_claim,
            has_observed_app_hook: client.has_observed_app_hook,
            has_observed_in_client_render: client.has_observed_in_client_render,
            has_observed_review_action: client.has_observed_review_action,
            artifact_failure_count: client.artifact_failures.len(),
            artifact_failures: client
                .artifact_failures
                .iter()
                .map(|failure| ProductHardeningEvidenceArtifactFailure {
                    proof_id: failure.proof_id,
                    proof_level: failure.proof_level.as_str().to_string(),
                    kind: failure.kind.clone(),
                    path: failure.path.clone(),
                    status: failure.status.as_str().to_string(),
                    error: failure.error.clone(),
                })
                .collect(),
            coherence_failure_count: client.coherence_failures.len(),
            non_release_evidence_source_count: client.non_release_evidence_sources.len(),
            non_release_proof_levels: client
                .non_release_evidence_sources
                .iter()
                .map(|source| source.proof_level.as_str().to_string())
                .collect(),
        })
        .collect();
    let mut required_client_proof_matrix =
        build_required_client_proof_matrix(client.as_deref(), &required_clients, &client_snapshots);
    let mut missing_required_clients = Vec::new();
    let mut unready_required_clients = Vec::new();
    for required_client in &required_clients {
        match status.clients.iter().find(|status| status.client == *required_client) {
            Some(status) if status.ready_for_private_client_claim => {}
            Some(_) => unready_required_clients.push(required_client.clone()),
            None => missing_required_clients.push(required_client.clone()),
        }
    }
    let required_client_count = required_clients.len();
    let required_ready_client_count =
        required_client_count - missing_required_clients.len() - unready_required_clients.len();
    let (
        mut proof_session_status,
        mut proof_session_release_gate,
        mut proof_session_next_step_id,
        proof_session_target_clients,
    ) = client_binding_hardening_proof_session_summary(
        client.as_deref(),
        &required_clients,
        &missing_required_clients,
        &unready_required_clients,
        &status.clients,
        artifact_failure_count,
        coherence_failure_count,
    );
    let config_root = client_binding_config_root.map(str::trim).filter(|value| !value.is_empty());
    if let Some(config_root) = config_root {
        let mut first_probe = None;
        for row in required_client_proof_matrix.iter_mut().filter(|row| row.proof_session_required)
        {
            let probe = client_binding_hardening_probe_proof_session(
                db_path,
                &row.client,
                config_root,
                limit,
            )?;
            let next_mcp_call = probe.proof_session.next_mcp_call.as_ref();
            attach_required_client_proof_session_probe(
                row,
                probe.proof_session.next_step_id.clone(),
                probe.proof_session.next_command.clone(),
                next_mcp_call.map(|call| call.tool.clone()),
                next_mcp_call.map(|call| call.arguments.clone()),
                next_mcp_call.map(|call| call.trust_boundary.clone()),
            );
            let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
            row.proof_session_cli = format!(
                "{soma_bin} adapter-binding-proof --client {} --proof-session --config-root {}",
                row.client, config_root
            );
            row.proof_session_mcp_arguments
                .insert("config_root".to_string(), config_root.to_string());
            row.config_root_probe_hint =
                Some(client_binding_config_root_probe_hint(Some(&row.client), Some(config_root)));
            attach_required_client_render_evidence_artifact_scan(
                row,
                proof_session_render_evidence_artifact_path(&probe.proof_session),
            );
            refresh_required_client_proof_matrix_operator_action(row);
            if first_probe.is_none() {
                first_probe = Some(probe);
            }
        }
        if let Some(probe) = first_probe {
            proof_session_status = probe.proof_session.status;
            proof_session_release_gate = probe.proof_session.release_gate;
            proof_session_next_step_id = probe.proof_session.next_step_id;
        }
    }
    let proof_session_config_root_probe_hint =
        if required_client_proof_matrix.iter().any(|row| row.proof_session_required) {
            Some(client_binding_config_root_probe_hint(None, config_root))
        } else {
            None
        };
    Ok(ClientBindingHardeningAudit {
        client,
        required_clients,
        required_client_proof_matrix,
        proof_session_source: "soma_client_binding_proof_session".to_string(),
        proof_session_runbook_source: "soma_client_binding_proof_session".to_string(),
        proof_session_runbook_schema: "soma.client_binding_proof_session_runbook.v1".to_string(),
        proof_session_runbook_required: proof_session_release_gate != "pass",
        proof_session_runbook_next_step_id: proof_session_next_step_id.clone(),
        proof_session_status,
        proof_session_release_gate,
        proof_session_next_step_id,
        proof_session_target_clients,
        proof_session_config_root_probe_hint,
        required_client_count,
        required_ready_client_count,
        missing_required_clients,
        unready_required_clients,
        proof_limit: limit,
        proofs_found: status.proofs_found,
        client_count: status.client_count,
        ready_client_count,
        all_latest_artifacts_verified: status.all_latest_artifacts_verified,
        artifact_failure_count,
        coherence_failure_count,
        non_release_evidence_source_count,
        non_release_proof_levels,
        primary_readiness,
        primary_coherence_failures,
        primary_non_release_evidence_sources,
        readiness_values,
    })
}

fn client_binding_hardening_probe_proof_session(
    db_path: &std::path::Path,
    client: &str,
    config_root: &str,
    limit: usize,
) -> Result<crate::cli::adapter_binding_proof::AdapterBindingProofSessionOutcome, ContextError> {
    let args = AdapterBindingProofArgs {
        manifest: None,
        client: Some(client.to_string()),
        list: false,
        status: false,
        check_installed_config: false,
        discover_installed_config: false,
        real_app_proof_kit: false,
        evidence_bundle: false,
        proof_session: true,
        json: true,
        brief: false,
        format: "json".to_string(),
        prepare_installed_config: false,
        render_installed_config: false,
        write_installed_config: None,
        render_render_evidence: false,
        write_render_evidence: None,
        verify_evidence_artifacts: false,
        proof_id: None,
        limit,
        proof_level: "observed_event_file".to_string(),
        evidence_source: "hardening_report_proof_session_probe".to_string(),
        binding_nonce: None,
        config_root: Some(config_root.to_string()),
        artifact_dir: None,
        event_jsonl: None,
        installed_config: None,
        require_private_target_config_for_app_hook: false,
        render_evidence: None,
        review_action_report: None,
        drain_report: None,
        review_render_report: None,
        operator_confirm_real_app_invocation: false,
        operator_confirm_in_client_render: false,
        operator_confirm_review_action: false,
        operator_confirm_release_grade_evidence: false,
        db_path: Some(db_path.to_string_lossy().into_owned()),
    };
    run_proof_session_blocking(
        &args,
        &AdapterBindingProofContext { db_path: db_path.to_path_buf() },
    )
    .map_err(|err| ContextError::BadFormat(format!("client binding proof-session probe: {err}")))
}

fn client_binding_hardening_proof_session_summary(
    requested_client: Option<&str>,
    required_clients: &[String],
    missing_required_clients: &[String],
    unready_required_clients: &[String],
    client_statuses: &[crate::cli::adapter_binding_proof::ClientBindingReadinessStatus],
    artifact_failure_count: usize,
    coherence_failure_count: usize,
) -> (String, String, Option<String>, Vec<String>) {
    let ready = if required_clients.is_empty() {
        client_statuses.iter().any(|status| status.ready_for_private_client_claim)
    } else {
        missing_required_clients.is_empty()
            && unready_required_clients.is_empty()
            && required_clients.iter().all(|required| {
                client_statuses.iter().any(|status| {
                    status.client == *required && status.ready_for_private_client_claim
                })
            })
    };
    let mut target_clients = Vec::new();
    target_clients.extend(missing_required_clients.iter().cloned());
    target_clients.extend(unready_required_clients.iter().cloned());
    if target_clients.is_empty() && !ready {
        if let Some(client) = requested_client {
            target_clients.push(client.to_string());
        } else if let Some(status) = client_statuses.first() {
            target_clients.push(status.client.clone());
        }
    }
    if target_clients.is_empty() && !required_clients.is_empty() {
        target_clients.extend(required_clients.iter().cloned());
    }
    if ready {
        ("ready_for_private_client_claim".to_string(), "pass".to_string(), None, target_clients)
    } else if artifact_failure_count > 0 || coherence_failure_count > 0 {
        (
            "blocked_by_stored_proof_integrity_or_identity".to_string(),
            "fail".to_string(),
            Some("verify_evidence_artifacts_and_status".to_string()),
            target_clients,
        )
    } else {
        (
            "requires_client_binding_proof_session".to_string(),
            "fail".to_string(),
            Some("render_client_binding_proof_session".to_string()),
            target_clients,
        )
    }
}

fn run_why(
    args: &crate::cli::ContextWhyArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    crate::context::why::validate_section(args.section.as_deref())
        .map_err(ContextError::BadFormat)?;
    let envelope = build_envelope_for_cli(
        ctx,
        args.query.clone(),
        args.project.clone(),
        args.session_id.clone(),
        args.local_compiler,
        args.local_compiler_endpoint.clone(),
        args.local_compiler_model.clone(),
        args.task_frame_id,
    )?;
    let storage = Storage::open(&ctx.db_path)?;
    let matches = crate::context::why::why_matches_with_audit(
        &storage,
        &envelope,
        args.section.as_deref(),
        args.contains.as_deref(),
    )?;
    let task_frame_projection = match args.task_frame_id {
        Some(task_frame_id) => {
            let task_frame = storage.task_frame(task_frame_id)?.ok_or_else(|| {
                StorageError::Corrupt { detail: format!("TaskFrame {task_frame_id} not found") }
            })?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let out = serde_json::json!({
        "scope": envelope.scope,
        "assembled_at_ns": envelope.assembled_at_ns,
        "task_frame_id": args.task_frame_id,
        "task_frame_projection": task_frame_projection,
        "section": args.section,
        "contains": args.contains,
        "match_count": matches.len(),
        "matches": matches,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn build_envelope_for_cli(
    ctx: &ContextCliContext,
    query: Option<String>,
    project: Option<String>,
    session_id: Option<String>,
    local_compiler: bool,
    local_compiler_endpoint: Option<String>,
    local_compiler_model: Option<String>,
    task_frame_id: Option<i64>,
) -> Result<ContextEnvelope, ContextError> {
    let task_frame = match task_frame_id {
        Some(task_frame_id) => {
            let storage = Storage::open(&ctx.db_path)?;
            let task_frame = storage.task_frame(task_frame_id)?.ok_or_else(|| {
                StorageError::Corrupt { detail: format!("TaskFrame {task_frame_id} not found") }
            })?;
            validate_task_frame_scope(
                task_frame_id,
                project.as_deref(),
                session_id.as_deref(),
                &task_frame,
            )?;
            Some(task_frame)
        }
        None => None,
    };
    let query = query.or_else(|| task_frame.as_ref().and_then(task_frame_projected_query));
    let project = project.or_else(|| task_frame.as_ref().and_then(|frame| frame.project.clone()));
    let session_id =
        session_id.or_else(|| task_frame.as_ref().and_then(|frame| frame.session_id.clone()));
    let storage = Arc::new(Mutex::new(Storage::open(&ctx.db_path)?));
    let inferred_project = if project.is_none() && session_id.is_none() {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        inferred_project_scope_from_anil(&guard)?
    } else {
        None
    };
    let effective_project = project.clone().or(inferred_project);
    let cfg = PackConfig {
        project_filter: effective_project.clone(),
        session_filter: session_id.clone(),
        ..PackConfig::default()
    };
    let pack = build_memory_pack(storage.clone(), query.as_deref(), cfg)?;
    let relevant_memory_proxies = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        relevant_memory_proxies_from_storage(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT,
        )?
    };
    let corrections = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        correction_signals_from_storage_scoped(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_CORRECTION_LIMIT,
        )?
    };
    let correction_stale_claims =
        corrections.iter().filter_map(|signal| signal.stale_claim.clone()).collect::<Vec<_>>();
    let open_decisions = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        open_decisions_from_storage_scoped_with_corrections(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_OPEN_DECISION_LIMIT,
            &correction_stale_claims,
        )?
    };
    let short_term_candidates = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        short_term_candidates_from_storage(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )?
    };
    let project_experience = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        project_experience_from_storage(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_PROJECT_EXPERIENCE_EVIDENCE_LIMIT,
        )?
    };
    let user_policy = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        user_policy_from_storage_with_corrections(
            &guard,
            effective_project.as_deref(),
            &corrections,
        )?
    };
    let stable_facts = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        stable_facts_from_storage(
            &guard,
            effective_project.as_deref(),
            session_id.as_deref(),
            DEFAULT_STABLE_FACT_LIMIT,
        )?
    };
    let correction_sections = corrections.into_iter().map(|signal| signal.section).collect();
    let scope = match session_id.clone() {
        Some(session_id) => ContextScope::session(session_id, effective_project.clone(), query),
        None => match effective_project.clone() {
            Some(project) => ContextScope::project(project, query),
            None => ContextScope::current(query),
        },
    };
    let mut envelope = build_context_envelope(&pack, scope);
    append_relevant_memory_items(
        &mut envelope,
        relevant_memory_proxies,
        pack.thread_state_selection.as_ref(),
    );
    apply_correction_overrides(&mut envelope, &correction_stale_claims);
    if let Some(task_frame) = task_frame.as_ref() {
        let section = task_frame_thread_state_section(task_frame, envelope.thread_state.as_ref());
        attach_thread_state(&mut envelope, Some(section));
    }
    attach_short_term_candidates(&mut envelope, short_term_candidates);
    attach_project_experience(&mut envelope, project_experience);
    attach_stable_facts(&mut envelope, stable_facts);
    attach_user_policy(&mut envelope, user_policy);
    attach_open_decisions(&mut envelope, open_decisions);
    attach_corrections(&mut envelope, correction_sections);
    if local_compiler {
        let local_config = load_local_compiler_config_from_home();
        let (endpoint, model) = resolve_local_compiler_runtime(
            local_compiler_endpoint.as_deref(),
            local_compiler_model.as_deref(),
            &local_config,
        );
        if let Err(e) = attach_local_compiler_note(&mut envelope, &endpoint, &model) {
            tracing::debug!(
                error = %e,
                "local context compiler unavailable; using deterministic ContextEnvelope fallback"
            );
        }
    }

    Ok(envelope)
}

fn validate_task_frame_scope(
    task_frame_id: i64,
    project: Option<&str>,
    session_id: Option<&str>,
    task_frame: &StoredTaskFrame,
) -> Result<(), ContextError> {
    if let (Some(project), Some(frame_project)) = (project, task_frame.project.as_deref()) {
        if project != frame_project {
            return Err(ContextError::BadFormat(format!(
                "TaskFrame {task_frame_id} project `{frame_project}` does not match requested project `{project}`"
            )));
        }
    }
    if let (Some(session_id), Some(frame_session)) = (session_id, task_frame.session_id.as_deref())
    {
        if session_id != frame_session {
            return Err(ContextError::BadFormat(format!(
                "TaskFrame {task_frame_id} session `{frame_session}` does not match requested session `{session_id}`"
            )));
        }
    }
    Ok(())
}

fn task_frame_projected_query(task_frame: &StoredTaskFrame) -> Option<String> {
    task_frame
        .cloud_redacted_json
        .get("goal_state")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .map(str::to_string)
}

fn run_correct(
    args: &crate::cli::ContextCorrectArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let mut storage = Storage::open(&ctx.db_path)?;
    let project = if args.all_projects {
        None
    } else {
        args.project.clone().or_else(crate::project::current_name)
    };
    let report = record_correction_with_report(
        &mut storage,
        CorrectionInput {
            claim: args.claim.clone(),
            correction: args.correction.clone(),
            project,
            session_id: args.session_id.clone(),
        },
    )?;
    Ok(format!(
        "soma: recorded correction episode #{} (corrected claim_records: {}, resolved contradictions: {})\n",
        report.episode_id,
        report.corrected_claim_ids.len(),
        report.resolved_contradiction_count
    ))
}

fn run_verify_claim(
    args: &crate::cli::ContextVerifyClaimArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let verifier_type = parse_verifier_type(&args.verifier)?;
    let result = parse_verification_result(&args.result)?;
    if args.evidence_kind.trim().is_empty() || args.evidence_id.trim().is_empty() {
        return Err(ContextError::BadFormat(
            "evidence_kind and evidence_id must be non-empty".to_string(),
        ));
    }
    let evidence_ref = StoredEvidenceRef {
        kind: args.evidence_kind.trim().to_string(),
        id: args.evidence_id.trim().to_string(),
        source: args
            .evidence_source
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(str::to_string),
    };

    let mut storage = Storage::open(&ctx.db_path)?;
    let target = verification_target_from_cli(args)?;
    let resolution = resolve_verification_targets(&storage, target, result)?;
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
    let mut claims = Vec::new();
    let mut events = Vec::new();
    for claim_id in &resolution.claim_ids {
        let claim = storage.claim_record(*claim_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("claim {claim_id} disappeared after verification insert"),
        })?;
        claims.push(claim);
        let mut claim_events = storage.verification_events_for_claim(*claim_id)?;
        claim_events.retain(|event| event_ids.contains(&event.id));
        events.extend(claim_events);
    }
    let mut durable_promotion_trust = true;
    for claim_id in resolution.claim_ids.iter().chain(resolution.skipped_claim_ids.iter()) {
        durable_promotion_trust &= storage.claim_has_durable_promotion_trust(*claim_id)?;
    }
    let event = events.first().cloned();
    let claim = claims.first().cloned();
    let out = serde_json::json!({
        "verification_event_id": event_ids.first().copied(),
        "verification_event_ids": event_ids,
        "claim_id": resolution.claim_ids.first().copied(),
        "claim_ids": resolution.claim_ids,
        "skipped_claim_ids": resolution.skipped_claim_ids,
        "verification_target": {
            "type": resolution.target_type,
            "id": resolution.target_id,
        },
        "durable_promotion_trust": durable_promotion_trust,
        "event": event,
        "events": events,
        "claim": claim,
        "claims": claims,
        "proposal": resolution.proposal,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn verification_target_from_cli(
    args: &crate::cli::ContextVerifyClaimArgs,
) -> Result<VerificationTargetInput, ContextError> {
    match (args.claim_id, args.proposal_id) {
        (Some(claim_id), None) => Ok(VerificationTargetInput::Claim(claim_id)),
        (None, Some(proposal_id)) => Ok(VerificationTargetInput::Proposal(proposal_id)),
        (None, None) => {
            Err(ContextError::BadFormat("one of claim_id or proposal_id is required".to_string()))
        }
        (Some(_), Some(_)) => Err(ContextError::BadFormat(
            "claim_id and proposal_id are mutually exclusive".to_string(),
        )),
    }
}

fn run_learning_proposals(
    args: &crate::cli::ContextLearningProposalsArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    match &args.mode {
        ContextLearningProposalMode::List(list) => run_learning_proposals_list(list, ctx),
        ContextLearningProposalMode::Apply(apply) => run_learning_proposals_apply(apply, ctx),
        ContextLearningProposalMode::ApplyReady(apply_ready) => {
            run_learning_proposals_apply_ready(apply_ready, ctx)
        }
        ContextLearningProposalMode::SetStatus(status) => {
            run_learning_proposals_set_status(status, ctx)
        }
    }
}

fn run_learning_proposals_list(
    args: &crate::cli::ContextLearningProposalListArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let status = match args.status.as_deref() {
        Some(status) => Some(parse_learning_proposal_status(status)?),
        None => None,
    };
    let storage = Storage::open(&ctx.db_path)?;
    let proposals = storage.learning_critic_proposals_scoped(
        args.project.as_deref(),
        args.session_id.as_deref(),
        status,
        args.limit,
    )?;
    let out = serde_json::json!({
        "project": args.project.clone(),
        "session_id": args.session_id.clone(),
        "status": status.map(|status| status.as_str()),
        "limit": args.limit,
        "count": proposals.len(),
        "proposals": proposals,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_learning_proposals_apply(
    args: &crate::cli::ContextLearningProposalApplyArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let mut storage = Storage::open(&ctx.db_path)?;
    let outcome = storage.apply_learning_critic_proposal_with_options(
        args.proposal_id,
        LearningCriticApplyOptions { allow_destructive: args.confirm_destructive },
    )?;
    let proposal = storage.learning_critic_proposal(args.proposal_id)?.ok_or_else(|| {
        StorageError::Corrupt {
            detail: format!("learning critic proposal {} disappeared", args.proposal_id),
        }
    })?;
    let out = serde_json::json!({
        "proposal_id": args.proposal_id,
        "outcome": outcome,
        "proposal": proposal,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_learning_proposals_apply_ready(
    args: &crate::cli::ContextLearningProposalApplyReadyArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = crate::context::review_apply::apply_ready_learning_proposals(
        &mut storage,
        crate::context::review_apply::ApplyReadyInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            dry_run: args.dry_run,
            include_decay: args.include_decay,
            include_noop: args.include_noop,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_learning_proposals_set_status(
    args: &crate::cli::ContextLearningProposalSetStatusArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let status = parse_learning_proposal_status(&args.status)?;
    if matches!(status, LearningCriticProposalStatus::Applied) {
        return Err(ContextError::BadFormat(
            "set-status cannot mark a proposal applied; use learning-proposals apply".to_string(),
        ));
    }
    let result = serde_json::json!({
        "review": "cli_set_status",
        "note": args.note.clone(),
    });
    let mut storage = Storage::open(&ctx.db_path)?;
    storage.update_learning_critic_proposal_status(args.proposal_id, status, Some(&result))?;
    let proposal = storage.learning_critic_proposal(args.proposal_id)?.ok_or_else(|| {
        StorageError::Corrupt {
            detail: format!("learning critic proposal {} disappeared", args.proposal_id),
        }
    })?;
    let out = serde_json::json!({
        "proposal_id": args.proposal_id,
        "status": status.as_str(),
        "proposal": proposal,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_review_queue(
    args: &crate::cli::ContextReviewQueueArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let queue = crate::context::review::build_review_queue(
        &storage,
        crate::context::review::ReviewQueueInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
        },
    )?;
    let effective_format = if args.json { "json" } else { args.format.trim() }.to_ascii_lowercase();
    let text = match effective_format.as_str() {
        "json" => serde_json::to_string_pretty(&queue).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => crate::context::review::render_review_queue_markdown(&queue),
        other => {
            return Err(ContextError::BadFormat(format!(
                "unknown review-queue format `{other}`; expected json or markdown"
            )));
        }
    };
    Ok(format!("{text}\n"))
}

fn run_review_actions(
    args: &crate::cli::ContextReviewActionsArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let plan = crate::context::review::build_review_action_plan(
        &storage,
        crate::context::review::ReviewActionPlanInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            include_disabled: args.include_disabled,
        },
    )?;
    let effective_format = if args.json {
        "json"
    } else if args.brief {
        "brief"
    } else {
        args.format.trim()
    }
    .to_ascii_lowercase();
    let text = match effective_format.as_str() {
        "json" => {
            let mut value = serde_json::to_value(&plan).unwrap_or_else(|_| json!({}));
            attach_review_action_report_path_hint(
                &mut value,
                review_action_report_path_hint_from_env(),
            );
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        }
        "brief" => crate::context::review::render_review_action_plan_brief(&plan),
        "markdown" | "md" => crate::context::review::render_review_action_plan_markdown(&plan),
        other => {
            return Err(ContextError::BadFormat(format!(
                "unknown review-actions format `{other}`; expected json, brief, or markdown"
            )));
        }
    };
    Ok(format!("{text}\n"))
}

fn review_action_report_path_hint_from_env() -> Option<String> {
    ["SOMA_REVIEW_ACTION_REPORT", "SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn attach_review_action_report_path_hint(value: &mut Value, path_hint: Option<String>) {
    let Some(path) = path_hint else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.insert(
        "review_action_report_path_hint".to_string(),
        json!({
            "source": "soma.review_action_plan.report_path_hint.v1",
            "path": path,
            "intended_writer": "soma context review-action",
            "required_before_client_binding_proof_level": "observed_review_action",
            "records_client_binding_proof": false,
            "report_still_requires_rendered_control_id": true,
            "report_still_requires_non_cloud_verification": true,
            "trust_boundary": "review_action_report_path_hint_is_read_only: carrying this path in the action plan writes no file, records no client-binding proof row, creates no verification event by itself, promotes no cloud draft, and does not prove the client rendered or executed a review control",
        }),
    );
}

fn run_review_batch_template(
    args: &crate::cli::ContextReviewBatchTemplateArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let action = parse_review_batch_template_action(&args.action)?;
    let target_type = parse_review_batch_template_target_type(&args.target_type)?;
    let verifier_type = args
        .verifier
        .as_deref()
        .map(parse_verifier_type)
        .transpose()?
        .map(|verifier| verifier.as_str().to_string());
    let storage = Storage::open(&ctx.db_path)?;
    let template = build_review_batch_template(
        &storage,
        ReviewBatchTemplateInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            action,
            target_type: Some(target_type),
            verifier_type,
            evidence_kind: args.evidence_kind.clone(),
            evidence_id: args.evidence_id.clone(),
            evidence_source: args.evidence_source.clone(),
        },
    )?;
    let text = serde_json::to_string_pretty(&template).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_review_report(
    args: &crate::cli::ContextReviewReportArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let action = parse_review_batch_template_action(&args.action)?;
    let target_type = parse_review_batch_template_target_type(&args.target_type)?;
    let verifier_type = args
        .verifier
        .as_deref()
        .map(parse_verifier_type)
        .transpose()?
        .map(|verifier| verifier.as_str().to_string());
    let storage = Storage::open(&ctx.db_path)?;
    let report = build_review_report(
        &storage,
        ReviewReportInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            include_disabled: args.include_disabled,
            action,
            target_type: Some(target_type),
            verifier_type,
            evidence_kind: args.evidence_kind.clone(),
            evidence_id: args.evidence_id.clone(),
            evidence_source: args.evidence_source.clone(),
        },
    )?;
    let text = match args.format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => report.operator_markdown,
        other => {
            return Err(ContextError::BadFormat(format!(
                "unknown review-report format `{other}`; expected json or markdown"
            )));
        }
    };
    Ok(format!("{text}\n"))
}

fn run_review_digest(
    args: &crate::cli::ContextReviewDigestArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let digest = build_review_digest(
        &storage,
        ReviewDigestInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            client: args.client.clone(),
            include_queue_only: args.include_queue_only,
        },
    )?;
    let text = match args.format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string_pretty(&digest).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => digest.operator_markdown,
        other => {
            return Err(ContextError::BadFormat(format!(
                "unknown review-digest format `{other}`; expected json or markdown"
            )));
        }
    };
    Ok(format!("{text}\n"))
}

fn run_review_digest_ack(
    args: &crate::cli::ContextReviewDigestAckArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = acknowledge_review_digest(
        &mut storage,
        ReviewDigestAckInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            client: args.client.clone(),
            batch_key: args.batch_key.clone(),
            cooldown_seconds: args.cooldown_seconds,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_review_render(
    args: &crate::cli::ContextReviewRenderArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let storage = Storage::open(&ctx.db_path)?;
    let plan = build_review_render_plan(
        &storage,
        ReviewRenderInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            client: args.client.clone(),
            include_disabled: args.include_disabled,
        },
    )?;
    let effective_format = if args.json { "json" } else { args.format.trim() }.to_ascii_lowercase();
    let text = match effective_format.as_str() {
        "json" => serde_json::to_string_pretty(&plan).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => plan.operator_markdown,
        "html" => render_review_render_plan_html(&plan),
        other => {
            return Err(ContextError::BadFormat(format!(
                "unknown review-render format `{other}`; expected json, markdown, or html"
            )));
        }
    };
    let output = format!("{text}\n");
    if let Some(path) = args.write_report.as_deref() {
        if effective_format != "json" {
            return Err(ContextError::BadFormat(
                "--write-report requires JSON review-render output".to_string(),
            ));
        }
        write_new_context_artifact(path, output.as_bytes())?;
    }
    Ok(output)
}

fn write_new_context_artifact(path: &str, bytes: &[u8]) -> Result<(), ContextError> {
    let path = Path::new(path);
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|err| {
            ContextError::Path(format!(
                "failed to create parent directory `{}`: {err}",
                parent.display()
            ))
        })?;
    }
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|err| {
            ContextError::Path(format!("failed to create report `{}`: {err}", path.display()))
        })?;
    file.write_all(bytes).map_err(|err| {
        ContextError::Path(format!("failed to write report `{}`: {err}", path.display()))
    })?;
    Ok(())
}

fn parse_review_batch_template_action(input: &str) -> Result<String, ContextError> {
    let action = input.trim().to_ascii_lowercase();
    match action.as_str() {
        "confirm" | "contradict" | "supersede" | "inconclusive" => Ok(action),
        other => Err(ContextError::BadFormat(format!(
            "unknown review-batch-template action `{other}`; expected confirm, contradict, supersede, or inconclusive"
        ))),
    }
}

fn parse_review_batch_template_target_type(input: &str) -> Result<String, ContextError> {
    let target_type = input.trim().to_ascii_lowercase();
    match target_type.as_str() {
        "any" | "claim" | "proposal" => Ok(target_type),
        other => Err(ContextError::BadFormat(format!(
            "unknown review-batch-template target_type `{other}`; expected any, claim, or proposal"
        ))),
    }
}

fn run_review_drain(
    args: &crate::cli::ContextReviewDrainArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = crate::context::review_drain::drain_review_queue(
        &mut storage,
        crate::context::review_drain::ReviewDrainInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            dry_run: args.dry_run,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_scheduler_run(
    args: &crate::cli::ContextSchedulerRunArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.semantic_min_support < 2 {
        return Err(ContextError::BadFormat("semantic-min-support must be at least 2".to_string()));
    }
    if !args.l2_promotion_min_confidence.is_finite()
        || !(0.0..=1.0).contains(&args.l2_promotion_min_confidence)
    {
        return Err(ContextError::BadFormat(format!(
            "l2-promotion-min-confidence must be finite within [0,1], got {}",
            args.l2_promotion_min_confidence
        )));
    }
    if !args.l2_promotion_anomaly_min_confidence.is_finite()
        || !(0.0..=1.0).contains(&args.l2_promotion_anomaly_min_confidence)
    {
        return Err(ContextError::BadFormat(format!(
            "l2-promotion-anomaly-min-confidence must be finite within [0,1], got {}",
            args.l2_promotion_anomaly_min_confidence
        )));
    }
    if args.l2_promotion_min_repeated_support < 2 {
        return Err(ContextError::BadFormat(
            "l2-promotion-min-repeated-support must be at least 2".to_string(),
        ));
    }
    if args.l3_decay_older_than_days < 1 {
        return Err(ContextError::BadFormat(
            "l3-decay-older-than-days must be at least 1".to_string(),
        ));
    }
    if args.l3_decay_max_access_count < 0 {
        return Err(ContextError::BadFormat(
            "l3-decay-max-access-count must be non-negative".to_string(),
        ));
    }
    if args.task_frame_retention_days < 1 {
        return Err(ContextError::BadFormat(
            "task-frame-retention-days must be at least 1".to_string(),
        ));
    }
    let passes =
        normalize_scheduler_control_passes(&args.passes).map_err(ContextError::BadFormat)?;
    let l3_decay_cutoff_ns = args
        .l3_decay_cutoff_ns
        .unwrap_or(task_frame_retention_cutoff_ns(now_ns(), args.l3_decay_older_than_days)?);
    let task_frame_retention_cutoff_ns = args
        .task_frame_retention_cutoff_ns
        .unwrap_or(task_frame_retention_cutoff_ns(now_ns(), args.task_frame_retention_days)?);
    let l3_decay_reason = args.l3_decay_reason.trim();
    let l3_decay_reason = if l3_decay_reason.is_empty() {
        DEFAULT_L3_DECAY_REASON.to_string()
    } else {
        l3_decay_reason.to_string()
    };
    let l2_promotion_reason = args.l2_promotion_reason.trim();
    let l2_promotion_reason = if l2_promotion_reason.is_empty() {
        DEFAULT_L2_PROMOTION_REASON.to_string()
    } else {
        l2_promotion_reason.to_string()
    };
    let task_frame_retention_reason = args.task_frame_retention_reason.trim();
    let task_frame_retention_reason = if task_frame_retention_reason.is_empty() {
        crate::context::scheduler_control::DEFAULT_TASK_FRAME_RETENTION_REASON.to_string()
    } else {
        task_frame_retention_reason.to_string()
    };
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = run_scheduler_control(
        &mut storage,
        SchedulerControlInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            semantic_min_support: args.semantic_min_support,
            l2_promotion_min_confidence: args.l2_promotion_min_confidence,
            l2_promotion_anomaly_min_confidence: args.l2_promotion_anomaly_min_confidence,
            l2_promotion_min_repeated_support: args.l2_promotion_min_repeated_support,
            l2_promotion_reason,
            l3_decay_cutoff_ns,
            l3_decay_max_access_count: args.l3_decay_max_access_count,
            l3_decay_reason,
            task_frame_retention_cutoff_ns,
            task_frame_retention_days: args.task_frame_retention_days,
            task_frame_retention_reason,
            dry_run: args.dry_run,
            passes,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_semantic_proposals(
    args: &crate::cli::ContextSemanticProposalsArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.json && args.brief {
        return Err(ContextError::BadFormat(
            "semantic-proposals accepts only one output selector: --json or --brief".to_string(),
        ));
    }
    let requested_format = if args.json {
        "json"
    } else if args.brief {
        "brief"
    } else {
        args.format.as_str()
    };
    if !matches!(requested_format, "json" | "brief") {
        return Err(ContextError::BadFormat(format!(
            "unknown format `{requested_format}`; semantic-proposals currently supports `json` and `brief`"
        )));
    }
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    if args.min_support < 2 {
        return Err(ContextError::BadFormat("min_support must be at least 2".to_string()));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = crate::context::semantic_learning::propose_semantic_consolidations(
        &mut storage,
        crate::context::semantic_learning::SemanticLearningInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            min_support: args.min_support,
            dry_run: args.dry_run,
        },
    )?;
    if requested_format == "brief" {
        return Ok(render_semantic_proposals_brief(&report));
    }
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn render_semantic_proposals_brief(report: &SemanticLearningReport) -> String {
    let mut out = String::new();
    out.push_str("SOMA semantic proposals brief\n");
    out.push_str(&format!("  Status: {} - {}\n", report.status, report.headline));
    out.push_str(&format!(
        "  Scope: project={} session={} dry_run={} limit={} min_support={}\n",
        report.project.as_deref().unwrap_or("all"),
        report.session_id.as_deref().unwrap_or("all"),
        report.dry_run,
        report.limit,
        report.min_support
    ));
    out.push_str(&format!(
        "  Counts: inspected_claims={} l4_candidates={} review_only_candidates={} proposed={} review_proposed={} skipped_untrusted={} skipped_existing_semantic={} skipped_existing_proposal={} skipped_existing_review={}\n",
        report.inspected_claim_count,
        report.l4_candidate_count,
        report.review_only_candidate_count,
        report.proposed_count,
        report.review_proposed_count,
        report.skipped_untrusted_count,
        report.skipped_existing_semantic_count,
        report.skipped_existing_proposal_count,
        report.skipped_existing_review_proposal_count
    ));
    out.push_str(&format!(
        "  Trust boundary: {}\n",
        semantic_proposals_trust_boundary(report.dry_run)
    ));
    if report.items.is_empty() {
        out.push_str("  Candidates: none\n");
        return out;
    }
    out.push_str("  Candidates:\n");
    let max_items = 10;
    for (idx, item) in report.items.iter().take(max_items).enumerate() {
        render_semantic_proposal_item_brief(&mut out, idx + 1, item, report);
    }
    if report.items.len() > max_items {
        out.push_str(&format!(
            "    ... {} more candidate(s) omitted from brief output; use --format json for the full evidence list.\n",
            report.items.len() - max_items
        ));
    }
    out
}

fn semantic_item_is_l4_candidate(item: &SemanticLearningItem) -> bool {
    matches!(item.action.as_str(), "would_propose" | "proposed")
        && matches!(item.group_rule.as_str(), SEMANTIC_EXACT_GROUP_RULE | SEMANTIC_TOKEN_GROUP_RULE)
}

fn semantic_item_is_review_only_candidate(item: &SemanticLearningItem) -> bool {
    matches!(item.action.as_str(), "would_request_verification" | "review_proposed")
        || item.readiness_score.verdict == "review_only_requires_resolution"
}

fn semantic_proposals_trust_boundary(dry_run: bool) -> &'static str {
    if dry_run {
        "read-only dry-run; records no proposal, verification, promotion, correction, or cloud-draft trust"
    } else {
        "may create review proposals only; never applies L4 semantic memory, records verification, or promotes cloud drafts without user/tool/local/correction evidence"
    }
}

fn render_semantic_proposal_item_brief(
    out: &mut String,
    index: usize,
    item: &SemanticLearningItem,
    report: &SemanticLearningReport,
) {
    let lane = if semantic_item_is_l4_candidate(item) {
        "L4-candidate"
    } else if semantic_item_is_review_only_candidate(item) {
        "review-only"
    } else if item.trusted {
        "trusted-skip"
    } else {
        "blocked"
    };
    out.push_str(&format!(
        "    {index}. [{lane}] action={} verdict={} score={}/{} support={} trusted={} bias={}\n",
        item.action,
        item.readiness_score.verdict,
        item.readiness_score.score,
        item.readiness_score.max_score,
        item.support_count,
        item.trusted,
        item.support_diversity.bias_risk
    ));
    out.push_str(&format!(
        "       claim: {}\n",
        semantic_brief_truncate(&item.normalized_text, 180)
    ));
    out.push_str(&format!(
        "       group_rule={} proposal_id={} support_claim_ids={} proposal_claim_ids={}\n",
        item.group_rule,
        item.proposal_id.map_or_else(|| "none".to_string(), |id| id.to_string()),
        semantic_brief_ids(&item.support_claim_ids),
        semantic_brief_ids(&item.proposal_claim_ids)
    ));
    out.push_str(&format!(
        "       diversity: projects={} task_frames={} source_types={} verifier_types={} evidence_sources={}\n",
        semantic_brief_strings(&item.support_diversity.support_projects),
        item.support_diversity.distinct_task_frame_count,
        item.support_diversity.distinct_source_type_count,
        item.support_diversity.distinct_verifier_type_count,
        item.support_diversity.distinct_evidence_source_count
    ));
    out.push_str(&format!(
        "       l4_auto_apply_blocked={} review_required={}\n",
        item.readiness_score.blocks_l4_auto_apply, item.readiness_score.review_required
    ));
    let failing_checks = item
        .readiness_score
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.check_id.as_str())
        .collect::<Vec<_>>();
    if failing_checks.is_empty() {
        out.push_str("       failing_checks: none\n");
    } else {
        out.push_str(&format!("       failing_checks: {}\n", failing_checks.join(",")));
    }
    if semantic_item_needs_verification_guidance(item) {
        if item.proposal_id.is_none() && report.dry_run {
            out.push_str(&format!(
                "       proposal_creation_hint: {}\n",
                semantic_brief_proposal_creation_command(report).join(" ")
            ));
            out.push_str(
                "       proposal_creation_boundary: creates review/proposal rows only; records no verification, applies no proposal, writes no semantic_fact, and promotes no cloud draft\n",
            );
        }
        if let Some(proposal_id) = item.proposal_id {
            out.push_str(&format!(
                "       proposal_review_hint: {}\n",
                semantic_brief_proposal_review_command(report).join(" ")
            ));
            out.push_str(&format!("       proposal_review_scope: proposal_id={proposal_id}\n"));
        }
        out.push_str(&format!(
            "       verification_template: {}\n",
            semantic_brief_verification_command(item).join(" ")
        ));
        out.push_str(&format!(
            "       verification_scope: {}\n",
            semantic_brief_verification_scope(item)
        ));
        out.push_str(
            "       verification_boundary: independent user/tool/test/local_observation/correction evidence only; cloud output and client render text are forbidden evidence\n",
        );
        render_semantic_resolution_plan_brief(out, item);
    }
    let evidence_preview = item
        .evidence_refs
        .iter()
        .take(4)
        .map(|evidence| {
            let source = evidence.source.as_deref().unwrap_or("none");
            format!("{}:{}@{}", evidence.kind, evidence.id, source)
        })
        .collect::<Vec<_>>();
    if evidence_preview.is_empty() {
        out.push_str("       evidence: none\n");
    } else {
        out.push_str(&format!("       evidence: {}\n", evidence_preview.join("; ")));
        if item.evidence_refs.len() > evidence_preview.len() {
            out.push_str(&format!(
                "       evidence_more: {} additional ref(s); use --format json for full refs\n",
                item.evidence_refs.len() - evidence_preview.len()
            ));
        }
    }
    if let Some(reason) = item.skipped_reason.as_deref() {
        out.push_str(&format!("       skipped_reason: {reason}\n"));
    }
}

fn render_semantic_resolution_plan_brief(out: &mut String, item: &SemanticLearningItem) {
    let plan = &item.resolution_plan;
    out.push_str(&format!(
        "       resolution_plan: status={} target={} next={}\n",
        plan.status, plan.target_lifecycle_state, plan.next_step
    ));
    out.push_str(&format!("       resolution_intent: {}\n", plan.intent));
    out.push_str(&format!(
        "       resolution_options: allowed={} blocked={}\n",
        plan.allowed_resolution_actions.join(","),
        plan.blocked_resolution_actions.join(",")
    ));
    out.push_str(&format!(
        "       trusted_evidence: verifiers={} kinds={} forbidden={}\n",
        plan.trusted_verifier_types.join(","),
        plan.trusted_evidence_kinds.join(","),
        plan.forbidden_evidence_kinds.join(",")
    ));
    out.push_str(&format!("       resolution_boundary: {}\n", plan.trust_boundary));
}

fn semantic_brief_proposal_creation_command(report: &SemanticLearningReport) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "semantic-proposals".to_string(),
        "--brief".to_string(),
        "--min-support".to_string(),
        report.min_support.to_string(),
        "--limit".to_string(),
        report.limit.to_string(),
    ];
    if let Some(project) = report.project.as_deref() {
        command.push("--project".to_string());
        command.push(project.to_string());
    }
    if let Some(session_id) = report.session_id.as_deref() {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
    command
}

fn semantic_brief_proposal_review_command(report: &SemanticLearningReport) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "review-queue".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--limit".to_string(),
        "20".to_string(),
    ];
    if let Some(project) = report.project.as_deref() {
        command.push("--project".to_string());
        command.push(project.to_string());
    }
    if let Some(session_id) = report.session_id.as_deref() {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
    command
}

fn semantic_item_needs_verification_guidance(item: &SemanticLearningItem) -> bool {
    semantic_item_is_l4_candidate(item)
        || semantic_item_is_review_only_candidate(item)
        || item.skipped_reason.as_deref() == Some("durable_promotion_trust_required")
}

fn semantic_brief_verification_command(item: &SemanticLearningItem) -> Vec<String> {
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

fn semantic_brief_verification_scope(item: &SemanticLearningItem) -> String {
    let claim_ids = if item.proposal_claim_ids.is_empty() {
        &item.support_claim_ids
    } else {
        &item.proposal_claim_ids
    };
    let claims = semantic_brief_ids(claim_ids);
    if let Some(proposal_id) = item.proposal_id {
        format!("proposal_id={proposal_id} claim_ids={claims}")
    } else if claim_ids.len() > 1 {
        format!(
            "representative_claim={} repeat_or_create_review_proposal_for_claim_ids={claims}",
            claim_ids[0]
        )
    } else {
        format!("claim_ids={claims}")
    }
}

fn semantic_brief_ids(ids: &[i64]) -> String {
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
    }
}

fn semantic_brief_strings(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}

fn semantic_brief_truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn run_open_decision_proposals(
    args: &crate::cli::ContextOpenDecisionProposalsArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.limit == 0 {
        return Err(ContextError::BadFormat("limit must be greater than 0".to_string()));
    }
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = crate::context::open_decision_review::propose_open_decision_reviews(
        &mut storage,
        crate::context::open_decision_review::OpenDecisionProposalInput {
            project: args.project.clone(),
            session_id: args.session_id.clone(),
            limit: args.limit,
            dry_run: args.dry_run,
        },
    )?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_review_action(
    args: &crate::cli::ContextReviewActionArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let target = review_target_from_cli(args)?;
    let action = parse_review_action(&args.action)?;
    let verifier_type = Some(parse_verifier_type(&args.verifier)?);
    let evidence_ref = review_action_evidence_ref(args)?;
    let input = ReviewActionInput {
        target,
        action,
        control_id: args.control_id.clone(),
        verifier_type,
        evidence_ref,
        note: args.note.clone(),
        confirm_destructive: args.confirm_destructive,
    };
    let mut storage = Storage::open(&ctx.db_path)?;
    let report = apply_review_action(&mut storage, input)?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

fn run_review_batch(
    args: &crate::cli::ContextReviewBatchArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    let operations = review_batch_operations_from_cli(args)?;
    let mut storage = Storage::open(&ctx.db_path)?;
    let report =
        apply_review_batch(&mut storage, ReviewBatchInput { operations, dry_run: args.dry_run })?;
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

#[derive(Debug, Deserialize)]
struct ReviewBatchOperationJson {
    claim_id: Option<i64>,
    proposal_id: Option<i64>,
    action: String,
    control_id: Option<String>,
    verifier_type: Option<VerifierType>,
    verifier: Option<String>,
    evidence_ref: Option<StoredEvidenceRef>,
    evidence_kind: Option<String>,
    evidence_id: Option<String>,
    evidence_source: Option<String>,
    note: Option<String>,
    #[serde(default)]
    confirm_destructive: bool,
}

fn review_batch_operations_from_cli(
    args: &crate::cli::ContextReviewBatchArgs,
) -> Result<Vec<ReviewActionInput>, ContextError> {
    let raw = match (&args.operations_json, &args.operations_file) {
        (Some(json), None) => json.clone(),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| {
            ContextError::BadFormat(format!(
                "failed to read operations_file `{}`: {e}",
                path.display()
            ))
        })?,
        (None, None) => {
            return Err(ContextError::BadFormat(
                "one of operations_json or operations_file is required".to_string(),
            ))
        }
        (Some(_), Some(_)) => {
            return Err(ContextError::BadFormat(
                "operations_json and operations_file are mutually exclusive".to_string(),
            ))
        }
    };
    let operations: Vec<ReviewBatchOperationJson> = serde_json::from_str(&raw)
        .map_err(|e| ContextError::BadFormat(format!("operations JSON: {e}")))?;
    operations
        .into_iter()
        .enumerate()
        .map(|(index, op)| review_batch_operation_from_json(index, op))
        .collect()
}

fn review_batch_operation_from_json(
    index: usize,
    op: ReviewBatchOperationJson,
) -> Result<ReviewActionInput, ContextError> {
    let target = match (op.claim_id, op.proposal_id) {
        (Some(claim_id), None) => ReviewTarget::Claim(claim_id),
        (None, Some(proposal_id)) => ReviewTarget::Proposal(proposal_id),
        (None, None) => {
            return Err(ContextError::BadFormat(format!(
                "operations[{index}] requires claim_id or proposal_id"
            )))
        }
        (Some(_), Some(_)) => {
            return Err(ContextError::BadFormat(format!(
                "operations[{index}] claim_id and proposal_id are mutually exclusive"
            )))
        }
    };
    let action = parse_review_action(&op.action)?;
    let verifier_type = match (op.verifier_type, op.verifier.as_deref()) {
        (Some(verifier), _) => Some(verifier),
        (None, Some(verifier)) => Some(parse_verifier_type(verifier)?),
        (None, None) => Some(VerifierType::User),
    };
    let evidence_ref = review_batch_evidence_ref(
        index,
        op.evidence_ref,
        op.evidence_kind,
        op.evidence_id,
        op.evidence_source,
    )?;
    Ok(ReviewActionInput {
        target,
        action,
        control_id: op.control_id,
        verifier_type,
        evidence_ref,
        note: op.note,
        confirm_destructive: op.confirm_destructive,
    })
}

fn review_batch_evidence_ref(
    index: usize,
    evidence_ref: Option<StoredEvidenceRef>,
    evidence_kind: Option<String>,
    evidence_id: Option<String>,
    evidence_source: Option<String>,
) -> Result<Option<StoredEvidenceRef>, ContextError> {
    if evidence_ref.is_some()
        && (evidence_kind.is_some() || evidence_id.is_some() || evidence_source.is_some())
    {
        return Err(ContextError::BadFormat(format!(
            "operations[{index}] must use either evidence_ref or evidence_kind/evidence_id fields"
        )));
    }
    if let Some(evidence_ref) = evidence_ref {
        return Ok(Some(evidence_ref));
    }
    match (evidence_kind, evidence_id) {
        (Some(kind), Some(id)) => {
            if kind.trim().is_empty() || id.trim().is_empty() {
                return Err(ContextError::BadFormat(format!(
                    "operations[{index}] evidence_kind and evidence_id must be non-empty"
                )));
            }
            Ok(Some(StoredEvidenceRef {
                kind: kind.trim().to_string(),
                id: id.trim().to_string(),
                source: evidence_source
                    .as_deref()
                    .map(str::trim)
                    .filter(|source| !source.is_empty())
                    .map(str::to_string),
            }))
        }
        (None, None) => Ok(None),
        _ => Err(ContextError::BadFormat(format!(
            "operations[{index}] evidence_kind and evidence_id must be supplied together"
        ))),
    }
}

fn review_target_from_cli(
    args: &crate::cli::ContextReviewActionArgs,
) -> Result<ReviewTarget, ContextError> {
    match (args.claim_id, args.proposal_id) {
        (Some(claim_id), None) => Ok(ReviewTarget::Claim(claim_id)),
        (None, Some(proposal_id)) => Ok(ReviewTarget::Proposal(proposal_id)),
        (None, None) => {
            Err(ContextError::BadFormat("one of claim_id or proposal_id is required".to_string()))
        }
        (Some(_), Some(_)) => Err(ContextError::BadFormat(
            "claim_id and proposal_id are mutually exclusive".to_string(),
        )),
    }
}

fn review_action_evidence_ref(
    args: &crate::cli::ContextReviewActionArgs,
) -> Result<Option<StoredEvidenceRef>, ContextError> {
    match (args.evidence_kind.as_deref(), args.evidence_id.as_deref()) {
        (Some(kind), Some(id)) => {
            if kind.trim().is_empty() || id.trim().is_empty() {
                return Err(ContextError::BadFormat(
                    "evidence_kind and evidence_id must be non-empty when supplied".to_string(),
                ));
            }
            Ok(Some(StoredEvidenceRef {
                kind: kind.trim().to_string(),
                id: id.trim().to_string(),
                source: args
                    .evidence_source
                    .as_deref()
                    .map(str::trim)
                    .filter(|source| !source.is_empty())
                    .map(str::to_string),
            }))
        }
        (None, None) => Ok(None),
        _ => Err(ContextError::BadFormat(
            "evidence_kind and evidence_id must be supplied together".to_string(),
        )),
    }
}

fn parse_verifier_type(input: &str) -> Result<VerifierType, ContextError> {
    match normalized_token(input).as_str() {
        "user" => Ok(VerifierType::User),
        "test" => Ok(VerifierType::Test),
        "tool" => Ok(VerifierType::Tool),
        "local_observation" => Ok(VerifierType::LocalObservation),
        "correction" => Ok(VerifierType::Correction),
        other => Err(ContextError::BadFormat(format!(
            "unknown verifier `{other}`; expected user, test, tool, local_observation, or correction"
        ))),
    }
}

fn parse_review_action(input: &str) -> Result<ReviewAction, ContextError> {
    match normalized_token(input).as_str() {
        "confirm" | "confirmed" => Ok(ReviewAction::Confirm),
        "contradict" | "contradicted" | "reject_claim" => Ok(ReviewAction::Contradict),
        "supersede" | "superseded" => Ok(ReviewAction::Supersede),
        "inconclusive" => Ok(ReviewAction::Inconclusive),
        "accept" | "accepted" => Ok(ReviewAction::Accept),
        "reject" | "rejected" => Ok(ReviewAction::Reject),
        "wait" | "waiting" | "waiting_verification" => Ok(ReviewAction::Wait),
        "apply" => Ok(ReviewAction::Apply),
        "confirm_and_apply" | "approve_and_apply" => Ok(ReviewAction::ConfirmAndApply),
        other => Err(ContextError::BadFormat(format!(
            "unknown review action `{other}`; expected confirm, contradict, supersede, inconclusive, accept, reject, wait, apply, or confirm_and_apply"
        ))),
    }
}

fn parse_verification_result(input: &str) -> Result<VerificationResult, ContextError> {
    match normalized_token(input).as_str() {
        "confirmed" => Ok(VerificationResult::Confirmed),
        "contradicted" => Ok(VerificationResult::Contradicted),
        "superseded" => Ok(VerificationResult::Superseded),
        "inconclusive" => Ok(VerificationResult::Inconclusive),
        other => Err(ContextError::BadFormat(format!(
            "unknown verification result `{other}`; expected confirmed, contradicted, superseded, or inconclusive"
        ))),
    }
}

fn parse_learning_proposal_status(
    input: &str,
) -> Result<LearningCriticProposalStatus, ContextError> {
    match normalized_token(input).as_str() {
        "queued" => Ok(LearningCriticProposalStatus::Queued),
        "waiting_verification" => Ok(LearningCriticProposalStatus::WaitingVerification),
        "accepted" => Ok(LearningCriticProposalStatus::Accepted),
        "rejected" => Ok(LearningCriticProposalStatus::Rejected),
        "applied" => Ok(LearningCriticProposalStatus::Applied),
        other => Err(ContextError::BadFormat(format!(
            "unknown proposal status `{other}`; expected queued, waiting_verification, accepted, rejected, or applied"
        ))),
    }
}

fn normalized_token(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(feature = "cognitive")]
fn run_compare_ranking(
    args: &crate::cli::ContextCompareRankingArgs,
    ctx: &ContextCliContext,
) -> Result<String, ContextError> {
    if args.semantic_k == 0 {
        return Err(ContextError::BadFormat("semantic_k must be greater than 0".to_string()));
    }

    let storage = Arc::new(Mutex::new(Storage::open(&ctx.db_path)?));
    let cfg = PackConfig {
        semantic_k: args.semantic_k,
        project_filter: args.project.clone(),
        session_filter: args.session_id.clone(),
        ..PackConfig::default()
    };
    let cases = compare_ranking_cases(args)?;
    let comparison = compare_relevant_memory_ranking_corpus(storage, &cases, cfg)?;
    let case_source = match &args.corpus {
        Some(path) => serde_json::json!({"kind": "corpus", "path": path}),
        None => serde_json::json!({"kind": "single_query"}),
    };
    let out = serde_json::json!({
        "kind": "context_relevant_memory_ranking_comparison",
        "scope": {
            "query": args.query,
            "project": args.project,
            "session_id": args.session_id,
        },
        "case_source": case_source,
        "semantic_k": args.semantic_k,
        "backend_status": {
            "baseline": "hnsw",
            "candidate": "hopfield",
            "cognitive_feature_enabled": cfg!(feature = "cognitive"),
            "candidate_effective": if cfg!(feature = "cognitive") { "hopfield" } else { "hnsw-fallback" },
            "note": "diagnostic only; this command does not change the default MCP/ContextEnvelope backend",
        },
        "expected_episode_ids": args.expected_episodes,
        "comparison": comparison,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    Ok(format!("{text}\n"))
}

#[cfg(feature = "cognitive")]
#[derive(Debug, Deserialize)]
struct RankingCorpusCase {
    query: String,
    #[serde(default, alias = "relevant_episode_ids")]
    expected_episode_ids: Vec<i64>,
}

#[cfg(feature = "cognitive")]
fn compare_ranking_cases(
    args: &crate::cli::ContextCompareRankingArgs,
) -> Result<Vec<RelevantMemoryRankingCase>, ContextError> {
    match &args.corpus {
        Some(path) => {
            if args.query.is_some() || !args.expected_episodes.is_empty() {
                return Err(ContextError::BadFormat(
                    "--corpus cannot be combined with --query or --expected-episode".to_string(),
                ));
            }
            read_ranking_corpus(path)
        }
        None => {
            let query = args.query.as_deref().ok_or_else(|| {
                ContextError::BadFormat(
                    "--query is required unless --corpus is provided".to_string(),
                )
            })?;
            if query.trim().is_empty() {
                return Err(ContextError::BadFormat("query cannot be empty".to_string()));
            }
            Ok(vec![RelevantMemoryRankingCase::new(query, args.expected_episodes.clone())])
        }
    }
}

#[cfg(feature = "cognitive")]
fn read_ranking_corpus(path: &Path) -> Result<Vec<RelevantMemoryRankingCase>, ContextError> {
    let body = std::fs::read_to_string(path).map_err(|e| {
        ContextError::Path(format!("read ranking corpus `{}`: {e}", path.display()))
    })?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(ContextError::BadFormat(format!(
            "ranking corpus `{}` is empty",
            path.display()
        )));
    }

    let raw_cases = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<RankingCorpusCase>>(trimmed).map_err(|e| {
            ContextError::BadFormat(format!("parse ranking corpus `{}`: {e}", path.display()))
        })?
    } else {
        let mut raw_cases = Vec::new();
        for (idx, line) in body.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let case = serde_json::from_str::<RankingCorpusCase>(line).map_err(|e| {
                ContextError::BadFormat(format!(
                    "parse ranking corpus `{}` line {}: {e}",
                    path.display(),
                    idx + 1
                ))
            })?;
            raw_cases.push(case);
        }
        raw_cases
    };

    let mut cases = Vec::with_capacity(raw_cases.len());
    for (idx, raw) in raw_cases.into_iter().enumerate() {
        if raw.query.trim().is_empty() {
            return Err(ContextError::BadFormat(format!(
                "ranking corpus `{}` case {} has empty query",
                path.display(),
                idx + 1
            )));
        }
        cases.push(RelevantMemoryRankingCase::new(raw.query, raw.expected_episode_ids));
    }
    if cases.is_empty() {
        return Err(ContextError::BadFormat(format!(
            "ranking corpus `{}` contains no cases",
            path.display()
        )));
    }
    Ok(cases)
}

enum OutputFormat {
    Xml,
    Json,
}

impl OutputFormat {
    fn parse(s: &str) -> Result<Self, ContextError> {
        match s {
            "xml" | "prompt" => Ok(OutputFormat::Xml),
            "json" => Ok(OutputFormat::Json),
            other => Err(ContextError::BadFormat(format!(
                "unknown format `{other}`; expected `xml` or `json`"
            ))),
        }
    }
}

pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, ContextError> {
    crate::capture::ai_cli::resolve_db_path(cli_override).map_err(|e| {
        use crate::capture::ai_cli::IngestError;
        match e {
            IngestError::Path(m) => ContextError::Path(m),
            other => ContextError::Path(other.to_string()),
        }
    })
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

pub fn exit_code_for(e: &ContextError) -> i32 {
    match e {
        ContextError::BadFormat(_) => 1,
        ContextError::ReviewAction(ReviewActionError::Invalid(_)) => 1,
        ContextError::Storage(_)
        | ContextError::Pack(_)
        | ContextError::Correction(_)
        | ContextError::ReviewAction(ReviewActionError::Storage(_)) => 2,
        ContextError::Path(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_action_report_path_hint_is_read_only_metadata() {
        let mut value = json!({
            "schema": "soma.review_action_plan.v1",
            "actions": []
        });

        attach_review_action_report_path_hint(
            &mut value,
            Some("/tmp/soma/review-action.json".to_string()),
        );

        let hint = &value["review_action_report_path_hint"];
        assert_eq!(hint["source"].as_str(), Some("soma.review_action_plan.report_path_hint.v1"));
        assert_eq!(hint["path"].as_str(), Some("/tmp/soma/review-action.json"));
        assert_eq!(hint["records_client_binding_proof"].as_bool(), Some(false));
        assert_eq!(hint["report_still_requires_rendered_control_id"].as_bool(), Some(true));
        assert_eq!(hint["report_still_requires_non_cloud_verification"].as_bool(), Some(true));
        assert!(hint["trust_boundary"]
            .as_str()
            .is_some_and(|value| value.contains("writes no file")));
        assert!(hint["trust_boundary"]
            .as_str()
            .is_some_and(|value| value.contains("records no client-binding proof row")));
    }
}
