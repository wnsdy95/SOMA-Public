//! Disposition metadata for optional context quality modules.
//!
//! These values are shown on operator surfaces that expose legacy
//! cognitive-layer weights. The weights remain useful diagnostics, but
//! are not acceptance criteria unless the module affects the
//! cloud-LLM-facing `ContextEnvelope`.

use serde_json::{json, Value};

pub fn context_quality_module_disposition() -> Value {
    json!({
        "kind": "context_quality_module_disposition",
        "module_class": "optional_context_quality_module",
        "source": "docs/decisions/0015-context-quality-module-disposition.md",
        "decision_summary": "The four-module learning stack is not deleted; it is de-cored until a module changes a cloud-LLM-facing ContextEnvelope field.",
        "core_boundary": "ContextEnvelope fields: relevant_memory, thread_state, scope, project_experience, stable_facts, user_policy, open_decisions, corrections, compiler_notes.",
        "acceptance_rule": "Retain a module only when it changes ContextEnvelope ranking, scoping, compression, conflict detection, or evidence selection.",
        "metrics_boundary": "Weight drift, train_steps, norms, and dashboard curves are diagnostics; they are not product acceptance by themselves.",
        "live_adapters": {
            "belief_conflicts": {
                "status": "keep-live",
                "current_effect": "unresolved contradiction rows become cited ContextEnvelope.open_decisions",
                "boundary_field": "open_decisions",
                "proof_gate": "a contradiction candidate must emit a typed ContextEnvelope.open_decisions claim with belief and episode evidence"
            },
            "policy_rows": {
                "status": "keep-live",
                "current_effect": "deterministic policy rows become cited ContextEnvelope.user_policy",
                "boundary_field": "user_policy",
                "proof_gate": "a policy row must carry cited evidence and survive correction decay rules"
            },
            "corrections": {
                "status": "keep-live",
                "current_effect": "correction episodes surface in ContextEnvelope.corrections and suppress matching stale memory/conflicts",
                "boundary_field": "corrections",
                "proof_gate": "a correction episode must alter corrections and suppress matching stale ContextEnvelope claims under test"
            }
        },
        "modules": {
            "mlstm": {
                "status": "connected-candidate",
                "target_boundary_fields": ["thread_state"],
                "envelope_adapter": "mlstm_working_memory_state_thread_selector",
                "current_effect": "persisted working_memory_state selects budgeted ContextEnvelope.thread_state evidence without reranking relevant_memory",
                "measurement": "context_cli::context_render_uses_mlstm_state_for_thread_state_selection",
                "keep_condition": "retain while persisted mLSTM state improves thread_state continuity under a bounded evidence budget",
                "proof_gate": "a persisted mLSTM state must change only ContextEnvelope.thread_state selection while leaving ContextEnvelope.relevant_memory unchanged under test"
            },
            "hopfield": {
                "status": "connected-candidate",
                "target_boundary_fields": ["relevant_memory"],
                "current_effect": "can change ContextEnvelope relevant_memory when the Hopfield backend is selected",
                "measurement": "soma context compare-ranking --corpus <path> with a binary built using --features cognitive",
                "operator_smoke": "tools/context-ranking-corpus-smoke.sh",
                "proof_gate": "a query corpus must show better ContextEnvelope.relevant_memory recall/precision than HNSW before promotion"
            },
            "anil": {
                "status": "connected-candidate",
                "target_boundary_fields": ["scope"],
                "envelope_adapter": "scope_selector_respecting_explicit_filters",
                "current_effect": "high-confidence project_attribution can select default project scope when no explicit project/session filter is provided",
                "measurement": "context_cli::context_render_anil_project_attribution_selects_scope_without_explicit_filter",
                "keep_condition": "retain while ANIL improves default scope selection and never overrides explicit user project/session filters",
                "proof_gate": "ANIL output must change ContextEnvelope scope selection only when explicit user project/session filters are absent"
            },
            "ipc": {
                "status": "connected-candidate",
                "target_boundary_fields": ["open_decisions"],
                "envelope_adapter": "ipc_free_energy_open_decision_adapter",
                "current_effect": "threshold-crossing pc_free_energy rows persist as context_anomalies and become cited ContextEnvelope.open_decisions",
                "measurement": "context_cli::ingest_records_ipc_free_energy_anomaly_for_open_decisions",
                "keep_condition": "retain while iPC anomalies improve short-term anomaly/novelty handling without flooding open_decisions",
                "proof_gate": "iPC output must become a typed, cited ContextEnvelope.open_decisions anomaly claim with context_anomaly and episode evidence"
            }
        },
        "diagnostic_only": {
            "dashboard_status_metrics": {
                "status": "diagnostics-only",
                "current_effect": "surfaces weight rows, train_steps, norms, and dashboard curves",
                "proof_gate": "must not be used as product acceptance unless a live adapter changes a ContextEnvelope field"
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::context_quality_module_disposition;

    #[test]
    fn disposition_names_live_adapters_separately_from_optional_modules() {
        let value = context_quality_module_disposition();

        assert_eq!(value["live_adapters"]["belief_conflicts"]["status"], "keep-live");
        assert_eq!(value["live_adapters"]["belief_conflicts"]["boundary_field"], "open_decisions");
        assert_eq!(value["live_adapters"]["policy_rows"]["boundary_field"], "user_policy");
        assert_eq!(value["live_adapters"]["corrections"]["boundary_field"], "corrections");

        assert_eq!(value["modules"]["mlstm"]["status"], "connected-candidate");
        assert_eq!(value["modules"]["mlstm"]["target_boundary_fields"][0], "thread_state");
        assert_eq!(
            value["modules"]["mlstm"]["envelope_adapter"],
            "mlstm_working_memory_state_thread_selector"
        );
        assert_eq!(value["modules"]["hopfield"]["status"], "connected-candidate");
        assert_eq!(value["modules"]["hopfield"]["target_boundary_fields"][0], "relevant_memory");
        assert!(value["modules"]["hopfield"]["measurement"]
            .as_str()
            .unwrap()
            .contains("compare-ranking --corpus"));
        assert_eq!(
            value["modules"]["hopfield"]["operator_smoke"],
            "tools/context-ranking-corpus-smoke.sh"
        );
        assert_eq!(value["modules"]["anil"]["status"], "connected-candidate");
        assert_eq!(value["modules"]["anil"]["target_boundary_fields"][0], "scope");
        assert_eq!(
            value["modules"]["anil"]["envelope_adapter"],
            "scope_selector_respecting_explicit_filters"
        );
        assert!(value["modules"]["anil"]["proof_gate"]
            .as_str()
            .unwrap()
            .contains("explicit user project/session filters are absent"));
        assert_eq!(value["modules"]["ipc"]["status"], "connected-candidate");
        assert_eq!(value["modules"]["ipc"]["target_boundary_fields"][0], "open_decisions");
        assert_eq!(
            value["modules"]["ipc"]["envelope_adapter"],
            "ipc_free_energy_open_decision_adapter"
        );
        assert!(value["modules"]["ipc"]["proof_gate"]
            .as_str()
            .unwrap()
            .contains("context_anomaly"));
        assert_eq!(
            value["diagnostic_only"]["dashboard_status_metrics"]["status"],
            "diagnostics-only"
        );
    }
}
