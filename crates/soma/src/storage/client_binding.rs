//! Client binding proof ledger.
//!
//! A reference binding manifest proves only that SOMA's wrapper contract is
//! executable. It does not prove Cursor, Continue, or another private app
//! actually installed or called the hook, or rendered SOMA review UI in-client.
//! This ledger keeps that boundary explicit by recording the proof level with
//! every observation.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Storage, StorageError};

const IN_CLIENT_RENDER_EVIDENCE_SCHEMA: &str = "soma.in_client_render_evidence.v1";
const IN_CLIENT_RENDER_EVIDENCE_TRUST_BOUNDARY: &str =
    "observed_in_client_render_is_ui_only_and_never_verifies_promotes_applies_or_acknowledges";
const OBSERVED_APP_HOOK_CLOCK_SKEW_NS: i64 = 1_000_000_000;
const OBSERVED_APP_HOOK_FUTURE_SKEW_NS: i64 = 5 * 60 * 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientBindingProofLevel {
    ReferenceBinding,
    ObservedEventFile,
    ObservedAppHook,
    ObservedInClientRender,
    ObservedReviewAction,
}

impl ClientBindingProofLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientBindingProofLevel::ReferenceBinding => "reference_binding",
            ClientBindingProofLevel::ObservedEventFile => "observed_event_file",
            ClientBindingProofLevel::ObservedAppHook => "observed_app_hook",
            ClientBindingProofLevel::ObservedInClientRender => "observed_in_client_render",
            ClientBindingProofLevel::ObservedReviewAction => "observed_review_action",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "reference_binding" => Some(ClientBindingProofLevel::ReferenceBinding),
            "observed_event_file" => Some(ClientBindingProofLevel::ObservedEventFile),
            "observed_app_hook" => Some(ClientBindingProofLevel::ObservedAppHook),
            "observed_in_client_render" => Some(ClientBindingProofLevel::ObservedInClientRender),
            "observed_review_action" => Some(ClientBindingProofLevel::ObservedReviewAction),
            _ => None,
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        Self::from_wire(&value).ok_or_else(|| from_sql_error(value, "client binding proof level"))
    }
}

impl fmt::Display for ClientBindingProofLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientBindingProofDraft {
    pub client: String,
    pub proof_level: ClientBindingProofLevel,
    pub manifest_path: String,
    pub manifest_status: String,
    pub evidence_source: String,
    pub event_jsonl_path: Option<String>,
    pub installed_config_path: Option<String>,
    pub render_evidence_path: Option<String>,
    pub review_action_report_path: Option<String>,
    pub drain_report_json: Option<Value>,
    pub review_render_json: Option<Value>,
    pub trust_boundary: String,
    pub checks_json: Value,
    pub observed_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredClientBindingProof {
    pub id: i64,
    pub client: String,
    pub proof_level: ClientBindingProofLevel,
    pub manifest_path: String,
    pub manifest_status: String,
    pub evidence_source: String,
    pub event_jsonl_path: Option<String>,
    pub installed_config_path: Option<String>,
    pub render_evidence_path: Option<String>,
    pub review_action_report_path: Option<String>,
    pub drain_report_json: Option<Value>,
    pub review_render_json: Option<Value>,
    pub trust_boundary: String,
    pub checks_json: Value,
    pub observed_at_ns: i64,
    pub created_at_ns: i64,
}

impl Storage {
    pub fn insert_client_binding_proof(
        &mut self,
        draft: &ClientBindingProofDraft,
    ) -> Result<i64, StorageError> {
        validate_client_binding_proof_draft(draft)?;
        let drain_report_json =
            optional_json_to_string(draft.drain_report_json.as_ref(), "drain report")?;
        let review_render_json =
            optional_json_to_string(draft.review_render_json.as_ref(), "review render")?;
        let checks_json = json_to_string(&draft.checks_json, "client binding checks")?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO client_binding_proofs (
                client, proof_level, manifest_path, manifest_status, evidence_source,
                event_jsonl_path, installed_config_path, render_evidence_path,
                review_action_report_path,
                drain_report_json, review_render_json, trust_boundary, checks_json,
                observed_at_ns, created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             RETURNING id",
            rusqlite::params![
                draft.client.trim(),
                draft.proof_level.as_str(),
                draft.manifest_path.trim(),
                draft.manifest_status.trim(),
                draft.evidence_source.trim(),
                draft.event_jsonl_path.as_deref(),
                draft.installed_config_path.as_deref(),
                draft.render_evidence_path.as_deref(),
                draft.review_action_report_path.as_deref(),
                drain_report_json.as_deref(),
                review_render_json.as_deref(),
                draft.trust_boundary.trim(),
                checks_json,
                draft.observed_at_ns,
                now_ns,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn recent_client_binding_proofs(
        &self,
        client: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClientBindingProof>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT *
               FROM client_binding_proofs
              WHERE (?1 IS NULL OR client = ?1)
              ORDER BY observed_at_ns DESC, id DESC
              LIMIT ?2",
        )?;
        let rows =
            stmt.query_map(rusqlite::params![client, limit as i64], map_client_binding_proof_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn client_binding_proof_by_id(
        &self,
        proof_id: i64,
    ) -> Result<Option<StoredClientBindingProof>, StorageError> {
        use rusqlite::OptionalExtension;

        self.conn
            .query_row(
                "SELECT *
                   FROM client_binding_proofs
                  WHERE id = ?1",
                rusqlite::params![proof_id],
                map_client_binding_proof_row,
            )
            .optional()
            .map_err(StorageError::from)
    }
}

fn validate_client_binding_proof_draft(
    draft: &ClientBindingProofDraft,
) -> Result<(), StorageError> {
    for (field, value) in [
        ("client", draft.client.as_str()),
        ("manifest_path", draft.manifest_path.as_str()),
        ("manifest_status", draft.manifest_status.as_str()),
        ("evidence_source", draft.evidence_source.as_str()),
        ("trust_boundary", draft.trust_boundary.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(StorageError::Corrupt {
                detail: format!("client binding proof requires non-empty {field}"),
            });
        }
    }
    match draft.proof_level {
        ClientBindingProofLevel::ReferenceBinding | ClientBindingProofLevel::ObservedEventFile => {}
        ClientBindingProofLevel::ObservedAppHook => {
            if !draft.trust_boundary.contains("observed_app_hook") {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_app_hook proof must carry an observed_app_hook trust boundary"
                            .to_string(),
                });
            }
            if draft.installed_config_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "observed_app_hook proof requires installed_config_path".to_string(),
                });
            }
            validate_observed_app_hook_checks(draft)?;
        }
        ClientBindingProofLevel::ObservedInClientRender => {
            if !draft.trust_boundary.contains("observed_in_client_render") {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof must carry an observed_in_client_render trust boundary".to_string(),
                });
            }
            if draft.installed_config_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires installed_config_path"
                        .to_string(),
                });
            }
            if draft.render_evidence_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render_evidence_path"
                        .to_string(),
                });
            }
            if draft.review_render_json.is_none() {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires review_render_json"
                        .to_string(),
                });
            }
            if bool_at(&draft.checks_json, "/operator_confirmed_in_client_render") != Some(true) {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires operator_confirmed_in_client_render"
                        .to_string(),
                });
            }
            let render_evidence_is_structured = draft
                .checks_json
                .pointer("/render_evidence_scan/valid_structured_render_evidence")
                .and_then(Value::as_bool)
                == Some(true);
            if !render_evidence_is_structured {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof requires valid structured render evidence"
                            .to_string(),
                });
            }
            if string_at(&draft.checks_json, "/render_evidence_scan/schema")
                != Some(IN_CLIENT_RENDER_EVIDENCE_SCHEMA)
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires soma.in_client_render_evidence.v1 schema"
                        .to_string(),
                });
            }
            let render_client = string_at(&draft.checks_json, "/render_evidence_scan/client")
                .map(|value| value.trim().to_ascii_lowercase());
            let expected_client = draft.client.trim().to_ascii_lowercase();
            if render_client.as_deref() != Some(expected_client.as_str()) {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence client matching the binding target"
                        .to_string(),
                });
            }
            if !string_at(&draft.checks_json, "/render_evidence_scan/source")
                .is_some_and(is_allowed_render_evidence_source)
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence source to be manual_operator or client capture"
                        .to_string(),
                });
            }
            let render_observed_at_ns = positive_i64_at(
                &draft.checks_json,
                "/render_evidence_scan/observed_at_ns",
                "observed_in_client_render proof requires positive render evidence observed_at_ns",
            )?;
            if u64_at(&draft.checks_json, "/render_evidence_scan/rendered_surface_count")
                .is_none_or(|value| value == 0)
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires non-empty rendered_surfaces"
                        .to_string(),
                });
            }
            if u64_at(
                &draft.checks_json,
                "/render_evidence_scan/rendered_surface_placeholder_count",
            )
            .unwrap_or(0)
                > 0
            {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof rejects render evidence template placeholders"
                            .to_string(),
                });
            }
            if u64_at(&draft.checks_json, "/render_evidence_scan/raw_tool_output_surface_count")
                .unwrap_or(0)
                > 0
            {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof rejects raw MCP or tool output as render evidence"
                            .to_string(),
                });
            }
            if u64_at(&draft.checks_json, "/render_evidence_scan/visible_surface_count")
                .is_none_or(|value| value == 0)
            {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof requires at least one visible rendered surface"
                            .to_string(),
                });
            }
            let expected_surface_names =
                string_array_at(&draft.checks_json, "/render_evidence_scan/expected_surface_names");
            let rendered_surface_names =
                string_array_at(&draft.checks_json, "/render_evidence_scan/rendered_surface_names");
            let missing_surface_names =
                string_array_at(&draft.checks_json, "/render_evidence_scan/missing_surface_names");
            if !expected_surface_names.is_empty() && rendered_surface_names.is_empty() {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof requires named visible review surfaces"
                            .to_string(),
                });
            }
            if !missing_surface_names.is_empty() {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_in_client_render proof requires rendered_surfaces to include expected review surfaces"
                            .to_string(),
                });
            }
            if string_at(&draft.checks_json, "/render_evidence_scan/trust_boundary")
                != Some(IN_CLIENT_RENDER_EVIDENCE_TRUST_BOUNDARY)
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence trust boundary to remain UI-only"
                        .to_string(),
                });
            }
            let missing_requirements =
                string_array_at(&draft.checks_json, "/render_evidence_scan/missing_requirements");
            if draft
                .checks_json
                .pointer("/render_evidence_scan/missing_requirements")
                .and_then(Value::as_array)
                .is_none()
                || !missing_requirements.is_empty()
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence missing_requirements to be empty"
                        .to_string(),
                });
            }
            let review_render_fingerprint = draft
                .checks_json
                .pointer("/review_render_file_scan/fingerprint")
                .and_then(Value::as_str);
            let render_evidence_review_fingerprint = draft
                .checks_json
                .pointer("/render_evidence_scan/review_render_fingerprint")
                .and_then(Value::as_str);
            if review_render_fingerprint.is_none()
                || review_render_fingerprint != render_evidence_review_fingerprint
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence bound to review_render_file_scan fingerprint"
                        .to_string(),
                });
            }
            let review_workbench_version = draft
                .review_render_json
                .as_ref()
                .and_then(|value| value.pointer("/workbench/version"))
                .and_then(Value::as_str);
            let render_evidence_workbench_version = draft
                .checks_json
                .pointer("/render_evidence_scan/review_workbench_version")
                .and_then(Value::as_str);
            if review_workbench_version.is_none()
                || review_workbench_version != render_evidence_workbench_version
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence bound to review workbench version"
                        .to_string(),
                });
            }
            let review_interaction_contract_version = draft
                .review_render_json
                .as_ref()
                .and_then(|value| value.pointer("/interaction_contract/version"))
                .and_then(Value::as_str);
            let render_evidence_interaction_contract_version = draft
                .checks_json
                .pointer("/render_evidence_scan/review_interaction_contract_version")
                .and_then(Value::as_str);
            if review_interaction_contract_version.is_none()
                || review_interaction_contract_version
                    != render_evidence_interaction_contract_version
            {
                return Err(StorageError::Corrupt {
                    detail: "observed_in_client_render proof requires render evidence bound to review interaction contract version"
                        .to_string(),
                });
            }
            let expected_control_ids = draft
                .review_render_json
                .as_ref()
                .map(review_render_control_ids)
                .unwrap_or_default();
            if !expected_control_ids.is_empty() {
                let rendered_control_ids = string_array_at(
                    &draft.checks_json,
                    "/render_evidence_scan/rendered_control_ids",
                );
                if rendered_control_ids.is_empty() {
                    return Err(StorageError::Corrupt {
                        detail: "observed_in_client_render proof requires render evidence rendered_control_ids when review render has actions"
                            .to_string(),
                    });
                }
                let scan_expected_control_ids = string_array_at(
                    &draft.checks_json,
                    "/render_evidence_scan/expected_control_ids",
                );
                if scan_expected_control_ids != expected_control_ids {
                    return Err(StorageError::Corrupt {
                        detail: "observed_in_client_render proof requires render evidence expected_control_ids bound to review interaction action controls"
                            .to_string(),
                    });
                }
                let missing_control_ids = string_array_at(
                    &draft.checks_json,
                    "/render_evidence_scan/missing_control_ids",
                );
                if !missing_control_ids.is_empty()
                    || expected_control_ids.iter().any(|id| !rendered_control_ids.contains(id))
                {
                    return Err(StorageError::Corrupt {
                        detail: "observed_in_client_render proof requires render evidence rendered_control_ids covering review interaction action controls"
                            .to_string(),
                    });
                }
                let action_surface_rendered_control_ids = string_array_at(
                    &draft.checks_json,
                    "/render_evidence_scan/action_surface_rendered_control_ids",
                );
                let missing_action_surface_control_ids = string_array_at(
                    &draft.checks_json,
                    "/render_evidence_scan/missing_action_surface_control_ids",
                );
                if action_surface_rendered_control_ids.is_empty()
                    || !missing_action_surface_control_ids.is_empty()
                    || expected_control_ids
                        .iter()
                        .any(|id| !action_surface_rendered_control_ids.contains(id))
                {
                    return Err(StorageError::Corrupt {
                        detail: "observed_in_client_render proof requires visible action_buttons surface control ids covering review interaction action controls"
                            .to_string(),
                    });
                }
            }
            validate_observed_in_client_render_installed_config_checks(
                draft,
                render_observed_at_ns,
            )?;
        }
        ClientBindingProofLevel::ObservedReviewAction => {
            if !draft.trust_boundary.contains("observed_review_action") {
                return Err(StorageError::Corrupt {
                    detail:
                        "observed_review_action proof must carry an observed_review_action trust boundary"
                            .to_string(),
                });
            }
            if draft.installed_config_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "observed_review_action proof requires installed_config_path"
                        .to_string(),
                });
            }
            if draft.review_action_report_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "observed_review_action proof requires review_action_report_path"
                        .to_string(),
                });
            }
            validate_observed_review_action_checks(draft)?;
        }
    }
    Ok(())
}

fn validate_observed_review_action_checks(
    draft: &ClientBindingProofDraft,
) -> Result<(), StorageError> {
    if bool_at(&draft.checks_json, "/operator_confirmed_review_action") != Some(true) {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires operator_confirmed_review_action"
                .to_string(),
        });
    }
    if bool_at(&draft.checks_json, "/installed_config_scan/references_client") != Some(true) {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires installed config client reference"
                .to_string(),
        });
    }
    non_empty_string_at(
        &draft.checks_json,
        "/installed_config_scan/fingerprint",
        "observed_review_action proof requires installed config fingerprint",
    )?;
    non_empty_string_at(
        &draft.checks_json,
        "/installed_config_scan/binding_nonce",
        "observed_review_action proof requires installed config binding_nonce",
    )?;
    if bool_at(&draft.checks_json, "/review_action_report_scan/valid_storage_gated_review_action")
        != Some(true)
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires valid storage-gated review action report"
                    .to_string(),
        });
    }
    let control_id = non_empty_string_at(
        &draft.checks_json,
        "/review_action_report_scan/control_id",
        "observed_review_action proof requires review action control_id",
    )?;
    if bool_at(&draft.checks_json, "/review_action_report_scan/control_binding_verified")
        != Some(true)
    {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires control_binding_verified".to_string(),
        });
    }
    require_positive_u64(
        &draft.checks_json,
        "/review_action_report_scan/verification_event_count",
        "observed_review_action proof requires at least one verification event",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/review_action_report_scan/non_cloud_verification_event_count",
        "observed_review_action proof requires non-cloud verification evidence",
    )?;
    if string_at(&draft.checks_json, "/review_action_report_scan/trust_boundary")
        != Some(
            "review_action_uses_verification_storage_gates_and_required_current_control_binding",
        )
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires review_action storage-gate trust boundary"
                    .to_string(),
        });
    }
    let missing_requirements =
        string_array_at(&draft.checks_json, "/review_action_report_scan/missing_requirements");
    if draft
        .checks_json
        .pointer("/review_action_report_scan/missing_requirements")
        .and_then(Value::as_array)
        .is_none()
        || !missing_requirements.is_empty()
    {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires review action missing_requirements to be empty"
                .to_string(),
        });
    }
    if string_at(&draft.checks_json, "/linked_render_proof/proof_level")
        != Some(ClientBindingProofLevel::ObservedInClientRender.as_str())
    {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires linked observed_in_client_render proof"
                .to_string(),
        });
    }
    if bool_at(&draft.checks_json, "/linked_render_proof/control_id_in_rendered_control_ids")
        != Some(true)
        || bool_at(
            &draft.checks_json,
            "/linked_render_proof/control_id_in_action_surface_rendered_control_ids",
        ) != Some(true)
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires linked render proof to show the same visible control_id"
                    .to_string(),
        });
    }
    if string_at(&draft.checks_json, "/linked_render_proof/control_id") != Some(control_id) {
        return Err(StorageError::Corrupt {
            detail: "observed_review_action proof requires linked render control_id match"
                .to_string(),
        });
    }
    if string_at(&draft.checks_json, "/linked_render_proof/installed_config_fingerprint")
        != string_at(&draft.checks_json, "/installed_config_scan/fingerprint")
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires linked render installed_config fingerprint match"
                    .to_string(),
        });
    }
    if string_at(&draft.checks_json, "/linked_render_proof/installed_config_binding_nonce")
        != string_at(&draft.checks_json, "/installed_config_scan/binding_nonce")
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires linked render installed_config binding_nonce match"
                    .to_string(),
        });
    }
    let report_modified_at_ns = positive_i64_at(
        &draft.checks_json,
        "/review_action_report_scan/modified_at_ns",
        "observed_review_action proof requires review action report modified_at_ns",
    )?;
    let config_modified_at_ns = positive_i64_at(
        &draft.checks_json,
        "/installed_config_scan/modified_at_ns",
        "observed_review_action proof requires installed config modified_at_ns",
    )?;
    if report_modified_at_ns.saturating_add(OBSERVED_APP_HOOK_CLOCK_SKEW_NS) < config_modified_at_ns
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_review_action proof requires review action report temporal binding to installed config"
                    .to_string(),
        });
    }
    Ok(())
}

fn validate_observed_in_client_render_installed_config_checks(
    draft: &ClientBindingProofDraft,
    render_observed_at_ns: i64,
) -> Result<(), StorageError> {
    if bool_at(&draft.checks_json, "/installed_config_scan/references_review_render") != Some(true)
    {
        return Err(StorageError::Corrupt {
            detail: "observed_in_client_render proof requires installed config review-render wrapper reference"
                .to_string(),
        });
    }
    if bool_at(&draft.checks_json, "/installed_config_scan/references_client") != Some(true) {
        return Err(StorageError::Corrupt {
            detail: "observed_in_client_render proof requires installed config client reference"
                .to_string(),
        });
    }
    let config_modified_at_ns = positive_i64_at(
        &draft.checks_json,
        "/installed_config_scan/modified_at_ns",
        "observed_in_client_render proof requires installed config modified_at_ns",
    )?;
    if render_observed_at_ns.saturating_add(OBSERVED_APP_HOOK_CLOCK_SKEW_NS) < config_modified_at_ns
    {
        return Err(StorageError::Corrupt {
            detail: "observed_in_client_render proof requires render evidence temporal binding to installed config modified_at_ns"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_observed_app_hook_checks(draft: &ClientBindingProofDraft) -> Result<(), StorageError> {
    if draft.event_jsonl_path.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires event_jsonl_path".to_string(),
        });
    }
    if bool_at(&draft.checks_json, "/operator_confirmed_real_app_invocation") != Some(true) {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires operator_confirmed_real_app_invocation"
                .to_string(),
        });
    }
    let Some(drain_report) = draft.drain_report_json.as_ref() else {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires drain_report_json".to_string(),
        });
    };
    let captured_turns = u64_at(drain_report, "/captured_turns").unwrap_or(0);
    let captured_cloud_outputs = u64_at(drain_report, "/captured_cloud_outputs").unwrap_or(0);
    if captured_turns + captured_cloud_outputs == 0 {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires drain report to capture at least one event"
                .to_string(),
        });
    }

    let expected_event_source = non_empty_string_at(
        &draft.checks_json,
        "/expected_private_event_source",
        "observed_app_hook proof requires expected_private_event_source",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/event_scan/matching_events",
        "observed_app_hook proof requires matching event_scan events",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/event_scan/matching_private_event_sources",
        "observed_app_hook proof requires matching private event_source",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/event_scan/matching_private_writer_contract_events",
        "observed_app_hook proof requires soma_adapter_spool_append_v1 writer metadata",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/event_scan/matching_private_binding_nonces",
        "observed_app_hook proof requires matching private binding_nonce",
    )?;
    require_positive_u64(
        &draft.checks_json,
        "/event_scan/matching_private_events_with_observed_at",
        "observed_app_hook proof requires matching private event observed_at_ns",
    )?;
    if string_at(&draft.checks_json, "/event_scan/expected_event_source")
        != Some(expected_event_source)
    {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires event_scan expected_event_source to match expected_private_event_source"
                .to_string(),
        });
    }

    let references_lifecycle_or_spool_append =
        bool_at(&draft.checks_json, "/installed_config_scan/references_lifecycle_wrapper")
            == Some(true)
            || bool_at(&draft.checks_json, "/installed_config_scan/references_spool_append")
                == Some(true);
    if !references_lifecycle_or_spool_append {
        return Err(StorageError::Corrupt {
            detail:
                "observed_app_hook proof requires installed config lifecycle or spool append wrapper reference"
                    .to_string(),
        });
    }
    for (pointer, detail) in [
        (
            "/installed_config_scan/references_client",
            "observed_app_hook proof requires installed config client reference",
        ),
        (
            "/installed_config_scan/references_event_jsonl_env",
            "observed_app_hook proof requires installed config event jsonl env reference",
        ),
        (
            "/installed_config_scan/references_private_event_source",
            "observed_app_hook proof requires installed config private event source reference",
        ),
        (
            "/installed_config_scan/references_binding_nonce",
            "observed_app_hook proof requires installed config binding nonce reference",
        ),
    ] {
        if bool_at(&draft.checks_json, pointer) != Some(true) {
            return Err(StorageError::Corrupt { detail: detail.to_string() });
        }
    }
    if string_at(&draft.checks_json, "/installed_config_scan/expected_event_source")
        != Some(expected_event_source)
    {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires installed config expected_event_source to match expected_private_event_source"
                .to_string(),
        });
    }
    let binding_nonce = non_empty_string_at(
        &draft.checks_json,
        "/installed_config_scan/binding_nonce",
        "observed_app_hook proof requires installed config binding_nonce",
    )?;
    if string_at(&draft.checks_json, "/event_scan/expected_binding_nonce") != Some(binding_nonce) {
        return Err(StorageError::Corrupt {
            detail:
                "observed_app_hook proof requires event_scan expected_binding_nonce to match installed config binding_nonce"
                    .to_string(),
        });
    }

    let config_modified_at_ns = positive_i64_at(
        &draft.checks_json,
        "/installed_config_scan/modified_at_ns",
        "observed_app_hook proof requires installed config modified_at_ns",
    )?;
    let event_observed_at_ns = positive_i64_at(
        &draft.checks_json,
        "/event_scan/max_matching_private_observed_at_ns",
        "observed_app_hook proof requires matching private event observed_at_ns",
    )?;
    if event_observed_at_ns.saturating_add(OBSERVED_APP_HOOK_CLOCK_SKEW_NS) < config_modified_at_ns
    {
        return Err(StorageError::Corrupt {
            detail: "observed_app_hook proof requires app-hook event temporal binding to installed config modified_at_ns"
                .to_string(),
        });
    }
    if let Some(event_modified_at_ns) = i64_at(&draft.checks_json, "/event_scan/modified_at_ns") {
        if event_modified_at_ns.saturating_add(OBSERVED_APP_HOOK_CLOCK_SKEW_NS)
            < config_modified_at_ns
        {
            return Err(StorageError::Corrupt {
                detail: "observed_app_hook proof requires event file temporal binding to installed config modified_at_ns"
                    .to_string(),
            });
        }
    }
    let proof_observed_at_ns = positive_i64_at(
        &draft.checks_json,
        "/proof_observed_at_ns",
        "observed_app_hook proof requires proof_observed_at_ns",
    )?;
    if event_observed_at_ns > proof_observed_at_ns.saturating_add(OBSERVED_APP_HOOK_FUTURE_SKEW_NS)
    {
        return Err(StorageError::Corrupt {
            detail:
                "observed_app_hook proof rejects matching private event observed_at_ns too far in the future"
                    .to_string(),
        });
    }

    Ok(())
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn bool_at(value: &Value, pointer: &str) -> Option<bool> {
    value.pointer(pointer).and_then(Value::as_bool)
}

fn i64_at(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

fn u64_at(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn is_allowed_render_evidence_source(source: &str) -> bool {
    matches!(
        source.trim(),
        "manual_operator"
            | "client_capture"
            | "client_ui_capture"
            | "client_dom_capture"
            | "screenshot_ocr"
    )
}

fn non_empty_string_at<'a>(
    value: &'a Value,
    pointer: &str,
    detail: &'static str,
) -> Result<&'a str, StorageError> {
    string_at(value, pointer)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| StorageError::Corrupt { detail: detail.to_string() })
}

fn require_positive_u64(
    value: &Value,
    pointer: &str,
    detail: &'static str,
) -> Result<(), StorageError> {
    if u64_at(value, pointer).is_none_or(|count| count == 0) {
        return Err(StorageError::Corrupt { detail: detail.to_string() });
    }
    Ok(())
}

fn positive_i64_at(
    value: &Value,
    pointer: &str,
    detail: &'static str,
) -> Result<i64, StorageError> {
    i64_at(value, pointer)
        .filter(|value| *value > 0)
        .ok_or_else(|| StorageError::Corrupt { detail: detail.to_string() })
}

fn review_render_control_ids(review_render: &Value) -> BTreeSet<String> {
    review_render
        .pointer("/interaction_contract/actions")
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(|action| action.get("control_id").and_then(Value::as_str))
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn string_array_at(value: &Value, pointer: &str) -> BTreeSet<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn optional_json_to_string(
    value: Option<&Value>,
    label: &str,
) -> Result<Option<String>, StorageError> {
    value.map(|value| json_to_string(value, label)).transpose()
}

fn json_to_string(value: &Value, label: &str) -> Result<String, StorageError> {
    serde_json::to_string(value)
        .map_err(|e| StorageError::Corrupt { detail: format!("{label} JSON encode: {e}") })
}

fn json_from_db(value: Option<String>, label: &str) -> rusqlite::Result<Option<Value>> {
    value
        .map(|raw| {
            serde_json::from_str::<Value>(&raw)
                .map_err(|err| from_sql_error(format!("{label}: {err}"), "JSON value"))
        })
        .transpose()
}

fn map_client_binding_proof_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredClientBindingProof> {
    Ok(StoredClientBindingProof {
        id: row.get("id")?,
        client: row.get("client")?,
        proof_level: ClientBindingProofLevel::from_db(row.get("proof_level")?)?,
        manifest_path: row.get("manifest_path")?,
        manifest_status: row.get("manifest_status")?,
        evidence_source: row.get("evidence_source")?,
        event_jsonl_path: row.get("event_jsonl_path")?,
        installed_config_path: row.get("installed_config_path")?,
        render_evidence_path: row.get("render_evidence_path")?,
        review_action_report_path: row.get("review_action_report_path")?,
        drain_report_json: json_from_db(row.get("drain_report_json")?, "drain_report_json")?,
        review_render_json: json_from_db(row.get("review_render_json")?, "review_render_json")?,
        trust_boundary: row.get("trust_boundary")?,
        checks_json: serde_json::from_str::<Value>(&row.get::<_, String>("checks_json")?)
            .map_err(|err| from_sql_error(format!("checks_json: {err}"), "JSON value"))?,
        observed_at_ns: row.get("observed_at_ns")?,
        created_at_ns: row.get("created_at_ns")?,
    })
}

fn from_sql_error(value: String, expected: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown {expected}: {value}"),
        )),
    )
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
