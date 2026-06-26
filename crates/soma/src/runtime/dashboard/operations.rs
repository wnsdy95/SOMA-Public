//! Read-only operator status snapshot for the dashboard Operations tab.
//!
//! This endpoint intentionally reuses the existing CLI read models instead of
//! inventing a second readiness contract. It records no proof, creates no
//! verification event, applies no learning proposal, and promotes no draft.

use std::path::Path;

use serde_json::{json, Value};

use crate::cli::client_status;
use crate::cli::client_status::ClientStatusOutcome;
use crate::cli::learning_status;
use crate::cli::learning_status::LearningStatusOutcome;
use crate::cli::projects::ProjectExperienceReport;
use crate::cli::projects::{self, ProjectExperienceContext};
use crate::cli::{ClientStatusArgs, LearningStatusArgs, ProjectExperienceArgs};

pub fn operations_snapshot(db_path: &Path) -> Value {
    let db_path_string = db_path.display().to_string();
    json!({
        "schema": "soma.dashboard.operations_status.v1",
        "source": "soma_dashboard_operations",
        "db_path": db_path_string,
        "clients": clients_status(db_path),
        "projects": projects_status(db_path),
        "learning": learning_status_section(db_path),
        "trust_boundary": "dashboard_operations_status_is_read_only: reuses soma clients/projects/learning read models; records no proof, creates no verification event, applies no learning proposal, writes no semantic_fact, and promotes no cloud draft",
    })
}

fn clients_status(db_path: &Path) -> Value {
    let args = ClientStatusArgs {
        client: Some("all".to_string()),
        project: None,
        command: None,
        db_path: Some(db_path.display().to_string()),
        dogfood_report: None,
        real_cli_dogfood_report: None,
        limit: 200,
        format: "json".to_string(),
        brief: false,
        json: true,
    };
    match client_status::run(&args) {
        Ok(outcome) => compact_clients(&outcome),
        Err(err) => section_error("clients", err),
    }
}

fn projects_status(db_path: &Path) -> Value {
    let args = ProjectExperienceArgs {
        project: None,
        evidence_limit: 5,
        format: "json".to_string(),
        brief: false,
        current_terminal: false,
        require_current_terminal_scope: false,
        json: true,
        db_path: Some(db_path.display().to_string()),
        dogfood_report: None,
    };
    let ctx = ProjectExperienceContext { db_path: db_path.to_path_buf() };
    match projects::build_project_experience_report(&args, &ctx) {
        Ok(report) => compact_projects(&report),
        Err(err) => section_error("projects", err),
    }
}

fn learning_status_section(db_path: &Path) -> Value {
    let args = LearningStatusArgs {
        status_alias: None,
        project: None,
        session_id: None,
        client: Some("dashboard".to_string()),
        limit: 100,
        min_support: 2,
        candidate_limit: 10,
        review_limit: 10,
        db_path: Some(db_path.display().to_string()),
        dogfood_report: None,
        format: "json".to_string(),
        brief: false,
        json: true,
    };
    match learning_status::run(&args) {
        Ok(outcome) => compact_learning(&outcome),
        Err(err) => section_error("learning", err),
    }
}

fn compact_clients(outcome: &ClientStatusOutcome) -> Value {
    let rows: Vec<Value> = outcome
        .clients
        .iter()
        .map(|row| {
            let real_cli_probe_status =
                row.real_cli_dogfood_probe.as_ref().map(|probe| probe.status.clone());
            let real_cli_probe_next_action =
                row.real_cli_dogfood_probe.as_ref().and_then(|probe| probe.next_action.clone());
            let real_cli_probe_report_path =
                row.real_cli_dogfood_probe.as_ref().map(|probe| probe.report_path.clone());
            let proof_session_blocking_reasons =
                row.proof_session_blocking_reasons.iter().take(3).cloned().collect::<Vec<_>>();
            let artifact_repair_status =
                row.artifact_repair_summary.as_ref().map(|summary| summary.status);
            let artifact_repair_next_command =
                row.artifact_repair_summary.as_ref().map(|summary| summary.next_command.clone());
            let artifact_repair_render_evidence_scan =
                row.artifact_repair_summary.as_ref().and_then(|summary| {
                    summary.render_evidence_artifact_scan.as_ref().map(|scan| {
                        json!({
                            "status": scan.status,
                            "path": scan.path.clone(),
                            "placeholder_count": scan.placeholder_count,
                            "missing_requirements": scan.missing_requirements.clone(),
                            "records_proof": scan.records_proof,
                            "creates_verification_event": scan.creates_verification_event,
                            "promotes_cloud_draft": scan.promotes_cloud_draft,
                            "trust_boundary": scan.trust_boundary,
                        })
                    })
                });
            let artifact_repair_blocked_claims = row
                .artifact_repair_plan
                .as_ref()
                .map(|plan| plan.blocked_claims.iter().take(3).cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let private_event_observation = row.private_event_observation.as_ref();
            let continue_config = row.continue_extension_config_check.as_ref();
            let private_event_summary = private_event_observation.map(|observation| {
                json!({
                    "status": observation.status,
                    "event_count": observation.event_count,
                    "matching_private_event_count": observation.matching_private_event_count,
                    "matching_private_binding_nonce_count": observation
                        .matching_private_binding_nonce_count,
                    "matching_private_non_release_manual_event_count": observation
                        .matching_private_non_release_manual_event_count,
                    "matching_private_non_release_manual_binding_nonce_count": observation
                        .matching_private_non_release_manual_binding_nonce_count,
                    "matching_private_non_release_test_event_count": observation
                        .matching_private_non_release_test_event_count,
                    "matching_private_non_release_test_binding_nonce_count": observation
                        .matching_private_non_release_test_binding_nonce_count,
                    "matching_private_event_seen": observation.matching_private_event_seen,
                    "matching_private_binding_nonce_seen": observation
                        .matching_private_binding_nonce_seen,
                    "latest_spool_mismatches": observation.latest_spool_mismatches,
                    "trust_boundary": observation.trust_boundary,
                })
            });
            let continue_collector_summary = continue_config.map(|check| {
                json!({
                    "config_status": check.status,
                    "profile_config_status": check.profile_config_status,
                    "devdata_destination_status": check.devdata_destination_status,
                    "devdata_destination_visible": check.devdata_destination_visible,
                    "devdata_collector_status": check.devdata_collector_status,
                    "devdata_collector_listening": check.devdata_collector_listening,
                    "devdata_collector_error": check.devdata_collector_error,
                    "extension_installation_status": check.extension_installation_status,
                    "extension_observed": check.extension_observed,
                    "restart_or_reload_recommended": check.restart_or_reload_recommended,
                    "next_step": check.next_step,
                    "trust_boundary": check.trust_boundary,
                })
            });
            json!({
                "client": row.client,
                "display_name": row.display_name,
                "status": row.status,
                "ready": row.ready,
                "ready_scope": row.ready_scope,
                "ready_meaning": row.ready_meaning,
                "mcp_context_ready": row.mcp_context_ready,
                "stored_local_capture_observed": row.stored_local_capture_observed,
                "latest_real_cli_capture_observed": row.latest_real_cli_capture_observed,
                "release_ready": row.release_ready,
                "readiness_summary": row.readiness_summary,
                "operator_next_action_id": row.operator_next_action_id,
                "operator_next_action_label": row.operator_next_action_label,
                "operator_next_command": row.operator_next_command,
                "real_cli_probe_status": real_cli_probe_status,
                "real_cli_probe_next_action": real_cli_probe_next_action,
                "real_cli_probe_report_path": real_cli_probe_report_path,
                "proof_session_status": row.proof_session_status,
                "proof_session_next_step_id": row.proof_session_next_step_id,
                "proof_session_next_operator_step_title": row.proof_session_next_operator_step_title,
                "proof_session_next_operator_step_intent": row
                    .proof_session_next_operator_step_intent,
                "proof_session_next_operator_step_requires_operator_action": row
                    .proof_session_next_operator_step_requires_operator_action,
                "proof_session_next_command": row.proof_session_next_command,
                "proof_session_external_action": row.proof_session_external_action,
                "proof_level_statuses": row.proof_level_statuses.clone(),
                "missing_proof_levels": row.missing_proof_levels.clone(),
                "proof_session_ready_to_record_proof_levels": row
                    .proof_session_ready_to_record_proof_levels,
                "expected_event_source": row.expected_event_source,
                "binding_nonce": row.binding_nonce,
                "generated_binding_nonce": row.generated_binding_nonce,
                "event_jsonl_path": row.event_jsonl_path,
                "event_jsonl_probe_status": row.event_jsonl_probe_status,
                "private_event_observation": private_event_summary,
                "private_event_watch_command": row.private_event_watch_command,
                "private_event_wait_command": row.private_event_wait_command,
                "simple_private_hook_readiness_command": row.simple_private_hook_readiness_command,
                "simple_private_event_wait_command": row.simple_private_event_wait_command,
                "continue_extension_config_status": continue_config.map(|check| check.status),
                "continue_devdata_destination_visible": continue_config
                    .map(|check| check.devdata_destination_visible),
                "continue_devdata_collector_status": continue_config
                    .map(|check| check.devdata_collector_status),
                "continue_devdata_collector_listening": continue_config
                    .map(|check| check.devdata_collector_listening),
                "continue_devdata_collector_error": continue_config
                    .and_then(|check| check.devdata_collector_error.clone()),
                "continue_collector": continue_collector_summary,
                "proof_session_blocking_reason_count": row
                    .proof_session_blocking_reason_count
                    .unwrap_or(row.proof_session_blocking_reasons.len()),
                "proof_session_blocking_reasons": proof_session_blocking_reasons,
                "artifact_failure_count": row.artifact_failure_count,
                "artifact_repair_status": artifact_repair_status,
                "artifact_repair_next_command": artifact_repair_next_command,
                "artifact_repair_render_evidence_scan": artifact_repair_render_evidence_scan,
                "artifact_repair_blocked_claims": artifact_repair_blocked_claims,
                "ready_for_private_client_claim": row.ready_for_private_client_claim,
                "ready_for_client_operator_loop": row.ready_for_client_operator_loop,
                "observed_capture_dogfood": row.observed_capture_dogfood_evidence.is_some(),
                "blocked_claims": row.blocked_claims,
                "safe_to_claim": row.safe_to_claim,
            })
        })
        .collect();
    json!({
        "schema": outcome.schema,
        "source": outcome.source,
        "status": outcome.status,
        "operator_next_action_id": outcome.operator_next_action_id,
        "operator_next_action_label": outcome.operator_next_action_label,
        "headline": outcome.headline,
        "primary_next_step": outcome.primary_next_step,
        "primary_next_command": outcome.primary_next_command,
        "summary": outcome.summary,
        "operator_card": {
            "source": outcome.operator_card.source,
            "status": outcome.operator_card.status,
            "operator_next_action_id": outcome.operator_card.operator_next_action_id,
            "operator_next_action_label": outcome.operator_card.operator_next_action_label,
            "headline": outcome.operator_card.headline,
            "primary_next_step": outcome.operator_card.primary_next_step,
            "primary_next_command": outcome.operator_card.primary_next_command,
            "mcp_ready_clients": outcome.operator_card.mcp_ready_clients,
            "runtime_detected_clients": outcome.operator_card.runtime_detected_clients,
            "runtime_missing_clients": outcome.operator_card.runtime_missing_clients,
            "observed_capture_dogfood_clients": outcome.operator_card.observed_capture_dogfood_clients,
            "explicit_capture_ready_clients": outcome.operator_card.explicit_capture_ready_clients,
            "private_capture_ready_clients": outcome.operator_card.private_capture_ready_clients,
            "blocked_private_clients": outcome.operator_card.blocked_private_clients,
            "blocked_claims": outcome.operator_card.blocked_claims,
            "safe_to_claim": outcome.operator_card.safe_to_claim,
            "trust_boundary": outcome.operator_card.trust_boundary,
        },
        "dogfood_index": {
            "source": outcome.dogfood_index.source,
            "status": outcome.dogfood_index.status,
            "objective_count": outcome.dogfood_index.objective_count,
            "pass_count": outcome.dogfood_index.pass_count,
            "warning_count": outcome.dogfood_index.warning_count,
            "fail_count": outcome.dogfood_index.fail_count,
            "evidence_report_flow_status": outcome.dogfood_index.evidence_report_flow_status,
            "evidence_report_flow_summary": outcome.dogfood_index.evidence_report_flow_summary.clone(),
            "private_app_release_gate_status": outcome.dogfood_index.private_app_release_gate_status.clone(),
            "private_app_release_gate_ready": outcome.dogfood_index.private_app_release_gate_ready,
            "private_app_release_gate_ready_clients": outcome
                .dogfood_index
                .private_app_release_gate_ready_clients
                .clone(),
            "private_app_release_gate_pending_clients": outcome
                .dogfood_index
                .private_app_release_gate_pending_clients
                .clone(),
            "private_app_release_gate_summary": outcome
                .dogfood_index
                .private_app_release_gate_summary
                .clone(),
            "objectives": outcome
                .dogfood_index
                .objectives
                .iter()
                .map(|objective| {
                    json!({
                        "objective": objective.objective,
                        "status": objective.status,
                        "summary": objective.summary,
                        "evidence_refs": objective.evidence_refs,
                        "next_command": objective.next_command,
                        "trust_boundary": objective.trust_boundary,
                    })
                })
                .collect::<Vec<_>>(),
            "primary_next_command": outcome.dogfood_index.primary_next_command.clone(),
            "trust_boundary": outcome.dogfood_index.trust_boundary,
        },
        "private_app_release_snapshot": {
            "source": outcome.private_app_release_snapshot.source,
            "status": outcome.private_app_release_snapshot.status.clone(),
            "ready": outcome.private_app_release_snapshot.ready,
            "ready_clients": outcome.private_app_release_snapshot.ready_clients.clone(),
            "pending_clients": outcome.private_app_release_snapshot.pending_clients.clone(),
            "operator_next_action_id": outcome
                .private_app_release_snapshot
                .operator_next_action_id
                .clone(),
            "operator_next_action_label": outcome
                .private_app_release_snapshot
                .operator_next_action_label
                .clone(),
            "primary_next_step": outcome.private_app_release_snapshot.primary_next_step.clone(),
            "primary_next_command": outcome
                .private_app_release_snapshot
                .primary_next_command
                .clone(),
            "trust_boundary": outcome.private_app_release_snapshot.trust_boundary,
        },
        "clients": rows,
        "trust_boundary": outcome.trust_boundary,
    })
}

fn compact_projects(report: &ProjectExperienceReport) -> Value {
    let activation_command = project_scope_activation_command(report);
    let focus_project = project_focus_summary(report);
    let review_items: Vec<Value> = report
        .scope_review_plan
        .cross_project_session_review_items
        .iter()
        .take(3)
        .map(|item| {
            json!({
                "session_id": item.session_id,
                "projects": item.projects,
                "episode_count": item.episode_count,
                "status": item.status,
                "next_action": item.next_action,
                "context_render_command": item.context_render_command,
                "recall_command": item.recall_command,
                "trust_boundary": item.trust_boundary,
            })
        })
        .collect();
    json!({
        "schema": report.schema,
        "source": report.source,
        "status": report.status,
        "active_persona": report.active_persona,
        "db_path": report.db_path,
        "project_count": report.project_count,
        "scoped_episode_count": report.scoped_episode_count,
        "unscoped_episode_count": report.unscoped_episode_count,
        "current_scope_ready": report.current_terminal_scope.ready_for_project_scoped_capture,
        "missing_scope_envs": report.current_terminal_scope.missing_scope_envs,
        "activation_command": activation_command,
        "focus_project": focus_project,
        "current_terminal_scope": report.current_terminal_scope,
        "scope_contract": report.scope_contract,
        "scope_integrity": report.scope_integrity,
        "scope_review_plan": {
            "source": report.scope_review_plan.source,
            "status": report.scope_review_plan.status,
            "headline": report.scope_review_plan.headline,
            "current_scope_ready": report.scope_review_plan.current_scope_ready,
            "historical_warning_count": report.scope_review_plan.historical_warning_count,
            "cross_project_session_count": report.scope_review_plan.cross_project_session_count,
            "unscoped_episode_count": report.scope_review_plan.unscoped_episode_count,
            "dogfood_scope_status": report.scope_review_plan.dogfood_scope_status,
            "cross_project_session_review_items": review_items,
            "clean_capture_commands": report.scope_review_plan.clean_capture_commands,
            "safe_to_claim": report.scope_review_plan.safe_to_claim,
            "blocked_claims": report.scope_review_plan.blocked_claims,
            "trust_boundary": report.scope_review_plan.trust_boundary,
        },
        "operator_next_action_id": report.operator_next_action_id,
        "operator_next_action_label": report.operator_next_action_label,
        "operator_card": report.operator_card,
        "primary_next_step": report.primary_next_step,
        "primary_next_command": report.primary_next_command,
        "scope_warnings": report.scope_warnings,
        "trust_boundary": report.trust_boundary,
    })
}

fn project_focus_summary(report: &ProjectExperienceReport) -> Option<Value> {
    let wanted = report
        .project_filter
        .as_deref()
        .or(report.current_terminal_scope.project.as_deref())
        .or(report.current_terminal_scope.suggested_project.as_deref());
    let project = wanted
        .and_then(|name| report.projects.iter().find(|project| project.project == name))
        .or_else(|| report.projects.first())?;
    Some(json!({
        "project": project.project,
        "episode_count": project.episode_count,
        "session_count": project.session_count,
        "source_counts": project.source_counts,
        "recent_sessions": project.recent_sessions,
        "evidence_episode_ids": project.evidence_episode_ids,
        "latest_session_id": project.recent_sessions.first(),
        "latest_evidence_episode_id": project.evidence_episode_ids.first(),
    }))
}

fn project_scope_activation_command(report: &ProjectExperienceReport) -> Option<Vec<String>> {
    if report.current_terminal_scope.ready_for_project_scoped_capture {
        return None;
    }
    report
        .current_terminal_scope
        .suggested_persona_call_commands
        .first()
        .or_else(|| report.scope_review_plan.clean_capture_commands.first())
        .cloned()
}

fn compact_learning(outcome: &LearningStatusOutcome) -> Value {
    let review_cards: Vec<Value> = outcome
        .review_cards
        .iter()
        .take(8)
        .map(|card| {
            json!({
                "card_id": card.card_id,
                "lane": card.lane,
                "priority": card.priority,
                "target": card.target,
                "status": card.status,
                "title": card.title,
                "summary": card.summary,
                "primary_action": card.primary_action,
                "primary_command": card.primary_command,
                "evidence_refs": card.evidence_refs,
                "blocks_l4_promotion": card.blocks_l4_promotion,
                "projection_path": card.projection_path,
                "evidence_rule": card.evidence_rule,
                "accepted_verifier_types": card.accepted_verifier_types,
                "forbidden_evidence_sources": card.forbidden_evidence_sources,
                "trust_boundary": card.trust_boundary,
            })
        })
        .collect();
    json!({
        "schema": outcome.schema,
        "source": outcome.source,
        "status": outcome.status,
        "operator_next_action_id": outcome.operator_next_action_id,
        "operator_next_action_label": outcome.operator_next_action_label,
        "headline": outcome.headline,
        "primary_next_step": outcome.primary_next_step,
        "primary_next_command": outcome.primary_next_command,
        "project": outcome.project,
        "session_id": outcome.session_id,
        "client": outcome.client,
        "summary": outcome.summary,
        "belief_review_summary": outcome.belief_review_summary,
        "target_coverage": outcome.target_coverage,
        "promotion_matrix": outcome.promotion_matrix,
        "review_lanes": outcome.review_lanes,
        "review_surface": outcome.review_surface,
        "operator_card": outcome.operator_card,
        "review_card_count": outcome.review_cards.len(),
        "review_cards": review_cards,
        "cloud_draft_blockers": outcome.cloud_draft_blockers,
        "next_commands": outcome.next_commands,
        "trust_boundary": outcome.trust_boundary,
    })
}

fn section_error<E>(section: &'static str, err: E) -> Value
where
    E: std::fmt::Display,
{
    json!({
        "schema": "soma.dashboard.operations_section_error.v1",
        "section": section,
        "status": "error",
        "error": err.to_string(),
    })
}
