//! First-class TaskFrame persistence and cloud projection gates.
//!
//! A TaskFrame is SOMA's pre-cloud-call judgment state. The full local frame is
//! persisted for audit, while the cloud-facing projection is built through
//! sensitivity labels and fails closed when labels are missing or unknown.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{Storage, StorageError, StoredEvidenceRef};

pub const DEFAULT_TASK_FRAME_RETENTION_DAYS: i64 = 30;
const NANOS_PER_DAY: i64 = 86_400_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLabel {
    Public,
    ProjectInternal,
    LocalPrivate,
    Secret,
    NeverSend,
    Unknown,
}

impl SensitivityLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            SensitivityLabel::Public => "public",
            SensitivityLabel::ProjectInternal => "project_internal",
            SensitivityLabel::LocalPrivate => "local_private",
            SensitivityLabel::Secret => "secret",
            SensitivityLabel::NeverSend => "never_send",
            SensitivityLabel::Unknown => "unknown",
        }
    }

    pub fn can_project(self, policy: &TaskFrameProjectionPolicy) -> bool {
        match self {
            SensitivityLabel::Public => true,
            SensitivityLabel::ProjectInternal => policy.allow_project_internal,
            SensitivityLabel::LocalPrivate => policy.allow_local_private,
            SensitivityLabel::Secret | SensitivityLabel::NeverSend | SensitivityLabel::Unknown => {
                false
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFrameProjectionPolicy {
    pub allow_project_internal: bool,
    pub allow_local_private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_reason: Option<String>,
}

impl TaskFrameProjectionPolicy {
    pub fn public_only() -> Self {
        Self { allow_project_internal: false, allow_local_private: false, explicit_reason: None }
    }

    pub fn project_internal() -> Self {
        Self { allow_project_internal: true, allow_local_private: false, explicit_reason: None }
    }

    pub fn local_private_explicit(reason: impl Into<String>) -> Self {
        Self {
            allow_project_internal: true,
            allow_local_private: true,
            explicit_reason: Some(reason.into()),
        }
    }

    pub fn name(&self) -> &'static str {
        if self.allow_local_private {
            "local_private_explicit"
        } else if self.allow_project_internal {
            "project_internal"
        } else {
            "public_only"
        }
    }

    pub fn allowed_sensitivity_labels(&self) -> Vec<&'static str> {
        let mut labels = vec![SensitivityLabel::Public.as_str()];
        if self.allow_project_internal {
            labels.push(SensitivityLabel::ProjectInternal.as_str());
        }
        if self.allow_local_private {
            labels.push(SensitivityLabel::LocalPrivate.as_str());
        }
        labels
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFrameScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskFrameDocument {
    pub goal_state: String,
    pub work_mode: String,
    pub scope: TaskFrameScope,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub direction: Vec<String>,
    #[serde(default)]
    pub avoid: Vec<String>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<StoredEvidenceRef>,
    #[serde(default)]
    pub privacy_labels: BTreeMap<String, SensitivityLabel>,
}

impl TaskFrameDocument {
    pub fn project_cloud_projection(
        &self,
        policy: TaskFrameProjectionPolicy,
    ) -> Result<TaskFrameProjection, StorageError> {
        validate_task_frame_document(self)?;
        let local_full_json = encode_value(self, "task frame")?;
        let mut cloud = Map::new();
        let mut projected_labels = BTreeMap::new();
        let mut blocked_fields = Vec::new();

        for (field, value) in [
            ("goal_state", json!(self.goal_state)),
            ("work_mode", json!(self.work_mode)),
            ("scope", encode_value(&self.scope, "task frame scope")?),
            ("constraints", json!(self.constraints)),
            ("direction", json!(self.direction)),
            ("avoid", json!(self.avoid)),
            ("uncertainty", json!(self.uncertainty)),
            ("evidence_refs", encode_value(&self.evidence_refs, "task frame evidence refs")?),
        ] {
            let label =
                self.privacy_labels.get(field).copied().unwrap_or(SensitivityLabel::Unknown);
            if label.can_project(&policy) {
                cloud.insert(field.to_string(), redact_secret_like_value(value));
                projected_labels.insert(field.to_string(), label);
            } else {
                blocked_fields.push(field.to_string());
            }
        }

        cloud.insert(
            "privacy_labels".to_string(),
            encode_value(&projected_labels, "projected labels")?,
        );

        Ok(TaskFrameProjection {
            local_full_json,
            cloud_redacted_json: Value::Object(cloud),
            blocked_fields,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskFrameProjection {
    pub local_full_json: Value,
    pub cloud_redacted_json: Value,
    pub blocked_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskFrameDraft {
    pub builder_version: String,
    pub frame: TaskFrameDocument,
    pub projection_policy: TaskFrameProjectionPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredTaskFrame {
    pub id: i64,
    pub hash: String,
    pub builder_version: String,
    pub local_full_json: Value,
    pub cloud_redacted_json: Value,
    pub scope: TaskFrameScope,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub work_mode: String,
    pub goal_state: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub privacy_labels: BTreeMap<String, SensitivityLabel>,
    pub projection_policy: TaskFrameProjectionPolicy,
    pub blocked_fields: Vec<String>,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFrameRetentionRequest {
    pub cutoff_ns: i64,
    pub retention_days: i64,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskFrameRetentionReport {
    pub cutoff_ns: i64,
    pub retention_days: i64,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub apply: bool,
    pub eligible_unreferenced_ids: Vec<i64>,
    pub retained_referenced_ids: Vec<i64>,
    pub retained_by_claim_ids: Vec<i64>,
    pub retained_by_proposal_ids: Vec<i64>,
    pub retained_by_outcome_ids: Vec<i64>,
    pub eligible_count: usize,
    pub retained_referenced_count: usize,
    pub deleted_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFrameOutcomeType {
    Accepted,
    Revised,
    Rejected,
    Verified,
    Applied,
    Failed,
    Abandoned,
}

impl TaskFrameOutcomeType {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskFrameOutcomeType::Accepted => "accepted",
            TaskFrameOutcomeType::Revised => "revised",
            TaskFrameOutcomeType::Rejected => "rejected",
            TaskFrameOutcomeType::Verified => "verified",
            TaskFrameOutcomeType::Applied => "applied",
            TaskFrameOutcomeType::Failed => "failed",
            TaskFrameOutcomeType::Abandoned => "abandoned",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StorageError> {
        match value.trim() {
            "accepted" => Ok(TaskFrameOutcomeType::Accepted),
            "revised" => Ok(TaskFrameOutcomeType::Revised),
            "rejected" => Ok(TaskFrameOutcomeType::Rejected),
            "verified" => Ok(TaskFrameOutcomeType::Verified),
            "applied" => Ok(TaskFrameOutcomeType::Applied),
            "failed" => Ok(TaskFrameOutcomeType::Failed),
            "abandoned" => Ok(TaskFrameOutcomeType::Abandoned),
            other => Err(StorageError::Corrupt {
                detail: format!(
                    "TaskFrame outcome_type must be accepted, revised, rejected, verified, applied, failed, or abandoned; got `{other}`"
                ),
            }),
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        Self::parse(&value).map_err(|_| from_sql_error(value, "task frame outcome type"))
    }
}

impl fmt::Display for TaskFrameOutcomeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskFrameOutcomeDraft {
    pub task_frame_id: i64,
    pub outcome_type: TaskFrameOutcomeType,
    pub summary: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub claim_ids: Vec<i64>,
    pub proposal_ids: Vec<i64>,
    pub latent_proxy_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredTaskFrameOutcome {
    pub id: i64,
    pub task_frame_id: i64,
    pub outcome_type: TaskFrameOutcomeType,
    pub summary: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub claim_ids: Vec<i64>,
    pub proposal_ids: Vec<i64>,
    pub latent_proxy_ids: Vec<i64>,
    pub created_at_ns: i64,
}

impl Storage {
    pub fn insert_task_frame(&mut self, draft: &TaskFrameDraft) -> Result<i64, StorageError> {
        validate_task_frame_draft(draft)?;
        let projection = draft.frame.project_cloud_projection(draft.projection_policy.clone())?;
        let local_full_json = json_string(&projection.local_full_json, "task frame local JSON")?;
        let cloud_redacted_json =
            json_string(&projection.cloud_redacted_json, "task frame cloud JSON")?;
        let scope_json = json_string_value(&draft.frame.scope, "task frame scope")?;
        let evidence_refs_json = json_string_value(&draft.frame.evidence_refs, "evidence refs")?;
        let privacy_labels_json = json_string_value(&draft.frame.privacy_labels, "privacy labels")?;
        let policy_json = json_string_value(&draft.projection_policy, "projection policy")?;
        let blocked_fields_json = json_string_value(&projection.blocked_fields, "blocked fields")?;
        let hash = task_frame_hash(&draft.builder_version, &local_full_json, &policy_json);
        let now_ns = now_ns();

        let id = self.conn.query_row(
            "INSERT INTO task_frames (
                hash, builder_version, local_full_json, cloud_redacted_json,
                scope_json, project, session_id, work_mode, goal_state,
                evidence_refs_json, privacy_labels_json, projection_policy_json, blocked_fields_json,
                created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(hash) DO UPDATE SET hash = excluded.hash
             RETURNING id",
            rusqlite::params![
                hash,
                draft.builder_version.as_str(),
                local_full_json,
                cloud_redacted_json,
                scope_json,
                draft.frame.scope.project.as_deref(),
                draft.frame.scope.session_id.as_deref(),
                draft.frame.work_mode.as_str(),
                draft.frame.goal_state.as_str(),
                evidence_refs_json,
                privacy_labels_json,
                policy_json,
                blocked_fields_json,
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn task_frame(&self, task_frame_id: i64) -> Result<Option<StoredTaskFrame>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT * FROM task_frames WHERE id = ?1",
                rusqlite::params![task_frame_id],
                map_task_frame_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn apply_task_frame_retention(
        &mut self,
        request: &TaskFrameRetentionRequest,
    ) -> Result<TaskFrameRetentionReport, StorageError> {
        let project = request.project.as_deref();
        let session_id = request.session_id.as_deref();
        let eligible_unreferenced_ids = query_task_frame_ids(
            &self.conn,
            ELIGIBLE_UNREFERENCED_TASK_FRAMES,
            request,
            project,
            session_id,
        )?;
        let retained_by_claim_ids = query_task_frame_ids(
            &self.conn,
            REFERENCED_BY_CLAIM_TASK_FRAMES,
            request,
            project,
            session_id,
        )?;
        let retained_by_proposal_ids = query_task_frame_ids(
            &self.conn,
            REFERENCED_BY_PROPOSAL_TASK_FRAMES,
            request,
            project,
            session_id,
        )?;
        let retained_by_outcome_ids = query_task_frame_ids(
            &self.conn,
            REFERENCED_BY_OUTCOME_TASK_FRAMES,
            request,
            project,
            session_id,
        )?;
        let mut retained_referenced_ids = retained_by_claim_ids.clone();
        retained_referenced_ids.extend(retained_by_proposal_ids.iter().copied());
        retained_referenced_ids.extend(retained_by_outcome_ids.iter().copied());
        retained_referenced_ids.sort_unstable();
        retained_referenced_ids.dedup();

        let deleted_count = if request.apply {
            self.conn.execute(
                DELETE_ELIGIBLE_UNREFERENCED_TASK_FRAMES,
                rusqlite::params![request.cutoff_ns, project, session_id],
            )?
        } else {
            0
        };

        Ok(TaskFrameRetentionReport {
            cutoff_ns: request.cutoff_ns,
            retention_days: request.retention_days,
            project: request.project.clone(),
            session_id: request.session_id.clone(),
            apply: request.apply,
            eligible_count: eligible_unreferenced_ids.len(),
            retained_referenced_count: retained_referenced_ids.len(),
            eligible_unreferenced_ids,
            retained_referenced_ids,
            retained_by_claim_ids,
            retained_by_proposal_ids,
            retained_by_outcome_ids,
            deleted_count,
        })
    }

    pub fn insert_task_frame_outcome(
        &mut self,
        draft: &TaskFrameOutcomeDraft,
    ) -> Result<i64, StorageError> {
        validate_task_frame_outcome_draft(self, draft)?;
        let evidence_refs_json =
            json_string_value(&draft.evidence_refs, "task frame outcome evidence refs")?;
        let claim_ids_json = json_string_value(&dedup_i64(&draft.claim_ids), "outcome claim ids")?;
        let proposal_ids_json =
            json_string_value(&dedup_i64(&draft.proposal_ids), "outcome proposal ids")?;
        let latent_proxy_ids_json =
            json_string_value(&dedup_i64(&draft.latent_proxy_ids), "outcome latent proxy ids")?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO task_frame_outcomes (
                task_frame_id, outcome_type, summary, evidence_refs_json,
                claim_ids_json, proposal_ids_json, latent_proxy_ids_json,
                created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             RETURNING id",
            rusqlite::params![
                draft.task_frame_id,
                draft.outcome_type.as_str(),
                draft.summary.trim(),
                evidence_refs_json,
                claim_ids_json,
                proposal_ids_json,
                latent_proxy_ids_json,
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn task_frame_outcomes_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        task_frame_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<StoredTaskFrameOutcome>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT o.*
               FROM task_frame_outcomes o
               JOIN task_frames tf ON tf.id = o.task_frame_id
              WHERE (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
                AND (?3 IS NULL OR o.task_frame_id = ?3)
              ORDER BY o.created_at_ns DESC, o.id DESC
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, task_frame_id, limit as i64],
            map_task_frame_outcome_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

pub fn task_frame_retention_cutoff_ns(
    now_ns: i64,
    retention_days: i64,
) -> Result<i64, StorageError> {
    if retention_days < 1 {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame retention_days must be at least 1".to_string(),
        });
    }
    let retention_ns =
        retention_days.checked_mul(NANOS_PER_DAY).ok_or_else(|| StorageError::Corrupt {
            detail: "TaskFrame retention_days overflowed nanosecond range".to_string(),
        })?;
    Ok(now_ns.saturating_sub(retention_ns))
}

pub fn secret_like_projection_findings(value: &Value) -> Vec<String> {
    let mut findings = Vec::new();
    collect_secret_like_findings(value, "$", &mut findings);
    findings
}

const ELIGIBLE_UNREFERENCED_TASK_FRAMES: &str = "
    SELECT tf.id
      FROM task_frames tf
     WHERE tf.created_at_ns < ?1
       AND (?2 IS NULL OR tf.project = ?2)
       AND (?3 IS NULL OR tf.session_id = ?3)
       AND NOT EXISTS (
            SELECT 1 FROM claim_records c WHERE c.task_frame_id = tf.id
       )
       AND NOT EXISTS (
            SELECT 1 FROM learning_critic_proposals p WHERE p.task_frame_id = tf.id
       )
       AND NOT EXISTS (
            SELECT 1 FROM task_frame_outcomes o WHERE o.task_frame_id = tf.id
       )
     ORDER BY tf.created_at_ns ASC, tf.id ASC";

const REFERENCED_BY_CLAIM_TASK_FRAMES: &str = "
    SELECT DISTINCT tf.id
      FROM task_frames tf
      JOIN claim_records c ON c.task_frame_id = tf.id
     WHERE tf.created_at_ns < ?1
       AND (?2 IS NULL OR tf.project = ?2)
       AND (?3 IS NULL OR tf.session_id = ?3)
     ORDER BY tf.created_at_ns ASC, tf.id ASC";

const REFERENCED_BY_PROPOSAL_TASK_FRAMES: &str = "
    SELECT DISTINCT tf.id
      FROM task_frames tf
      JOIN learning_critic_proposals p ON p.task_frame_id = tf.id
     WHERE tf.created_at_ns < ?1
       AND (?2 IS NULL OR tf.project = ?2)
       AND (?3 IS NULL OR tf.session_id = ?3)
     ORDER BY tf.created_at_ns ASC, tf.id ASC";

const REFERENCED_BY_OUTCOME_TASK_FRAMES: &str = "
    SELECT DISTINCT tf.id
      FROM task_frames tf
      JOIN task_frame_outcomes o ON o.task_frame_id = tf.id
     WHERE tf.created_at_ns < ?1
       AND (?2 IS NULL OR tf.project = ?2)
       AND (?3 IS NULL OR tf.session_id = ?3)
     ORDER BY tf.created_at_ns ASC, tf.id ASC";

const DELETE_ELIGIBLE_UNREFERENCED_TASK_FRAMES: &str = "
    DELETE FROM task_frames
     WHERE id IN (
        SELECT tf.id
          FROM task_frames tf
         WHERE tf.created_at_ns < ?1
           AND (?2 IS NULL OR tf.project = ?2)
           AND (?3 IS NULL OR tf.session_id = ?3)
           AND NOT EXISTS (
                SELECT 1 FROM claim_records c WHERE c.task_frame_id = tf.id
           )
           AND NOT EXISTS (
                SELECT 1 FROM learning_critic_proposals p WHERE p.task_frame_id = tf.id
           )
           AND NOT EXISTS (
                SELECT 1 FROM task_frame_outcomes o WHERE o.task_frame_id = tf.id
           )
     )";

fn query_task_frame_ids(
    conn: &rusqlite::Connection,
    sql: &str,
    request: &TaskFrameRetentionRequest,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<Vec<i64>, StorageError> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(rusqlite::params![request.cutoff_ns, project, session_id], |row| {
            row.get::<_, i64>(0)
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn validate_task_frame_draft(draft: &TaskFrameDraft) -> Result<(), StorageError> {
    if draft.builder_version.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame builder_version cannot be empty".to_string(),
        });
    }
    validate_projection_policy(&draft.projection_policy)?;
    validate_task_frame_document(&draft.frame)
}

fn validate_projection_policy(policy: &TaskFrameProjectionPolicy) -> Result<(), StorageError> {
    if policy.allow_local_private
        && policy.explicit_reason.as_deref().is_none_or(|reason| reason.trim().is_empty())
    {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame local_private projection requires an explicit reason".to_string(),
        });
    }
    Ok(())
}

fn validate_task_frame_document(frame: &TaskFrameDocument) -> Result<(), StorageError> {
    if frame.goal_state.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame goal_state cannot be empty".to_string(),
        });
    }
    if frame.work_mode.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame work_mode cannot be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_task_frame_outcome_draft(
    storage: &Storage,
    draft: &TaskFrameOutcomeDraft,
) -> Result<(), StorageError> {
    if draft.summary.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame outcome summary cannot be empty".to_string(),
        });
    }
    if draft.evidence_refs.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "TaskFrame outcome requires at least one evidence ref".to_string(),
        });
    }
    for evidence in &draft.evidence_refs {
        validate_evidence_ref(evidence, "TaskFrame outcome evidence ref")?;
    }
    storage.task_frame(draft.task_frame_id)?.ok_or_else(|| StorageError::Corrupt {
        detail: format!("TaskFrame outcome requires existing TaskFrame {}", draft.task_frame_id),
    })?;
    for claim_id in dedup_i64(&draft.claim_ids) {
        let claim = storage.claim_record(claim_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("TaskFrame outcome references missing claim {claim_id}"),
        })?;
        if let Some(claim_task_frame_id) = claim.task_frame_id {
            if claim_task_frame_id != draft.task_frame_id {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "TaskFrame outcome for {} cannot reference claim {claim_id} from TaskFrame {claim_task_frame_id}",
                        draft.task_frame_id
                    ),
                });
            }
        }
    }
    for proposal_id in dedup_i64(&draft.proposal_ids) {
        let proposal = storage.learning_critic_proposal(proposal_id)?.ok_or_else(|| {
            StorageError::Corrupt {
                detail: format!("TaskFrame outcome references missing proposal {proposal_id}"),
            }
        })?;
        if let Some(proposal_task_frame_id) = proposal.task_frame_id {
            if proposal_task_frame_id != draft.task_frame_id {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "TaskFrame outcome for {} cannot reference proposal {proposal_id} from TaskFrame {proposal_task_frame_id}",
                        draft.task_frame_id
                    ),
                });
            }
        }
    }
    for proxy_id in dedup_i64(&draft.latent_proxy_ids) {
        storage.evidence_latent_proxy(proxy_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("TaskFrame outcome references missing latent proxy {proxy_id}"),
        })?;
    }
    Ok(())
}

fn validate_evidence_ref(evidence: &StoredEvidenceRef, label: &str) -> Result<(), StorageError> {
    if evidence.kind.trim().is_empty() || evidence.id.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: format!("{label} requires non-empty kind and id"),
        });
    }
    Ok(())
}

fn dedup_i64(ids: &[i64]) -> Vec<i64> {
    let mut out = ids.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

fn redact_secret_like_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_secret_like_text(&text)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_secret_like_value).collect())
        }
        Value::Object(fields) => Value::Object(
            fields.into_iter().map(|(key, value)| (key, redact_secret_like_value(value))).collect(),
        ),
        other => other,
    }
}

fn redact_secret_like_text(text: &str) -> String {
    if contains_secret_block_marker(text) {
        return "[REDACTED_SECRET_BLOCK]".to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            push_redacted_token(&mut out, &token);
            token.clear();
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    push_redacted_token(&mut out, &token);
    out
}

fn push_redacted_token(out: &mut String, token: &str) {
    if token.is_empty() {
        return;
    }
    if token_is_secret_like(token) {
        out.push_str("[REDACTED_SECRET]");
    } else {
        out.push_str(token);
    }
}

fn collect_secret_like_findings(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if contains_secret_like_text(text) {
                out.push(path.to_string());
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                collect_secret_like_findings(item, &format!("{path}[{idx}]"), out);
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                collect_secret_like_findings(value, &format!("{path}.{key}"), out);
            }
        }
        _ => {}
    }
}

fn contains_secret_like_text(text: &str) -> bool {
    if contains_secret_block_marker(text) {
        return true;
    }
    text.split_whitespace().any(token_is_secret_like)
}

fn contains_secret_block_marker(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY")
}

fn token_is_secret_like(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch: char| {
        matches!(ch, '"' | '\'' | '`' | ',' | ';' | ':' | ')' | ']' | '}')
    });
    let lower = trimmed.to_ascii_lowercase();
    let upper = trimmed.to_ascii_uppercase();

    lower.starts_with("sk-")
        || lower.starts_with("sk_live_")
        || lower.starts_with("sk_test_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("ghp_")
        || lower.starts_with("gho_")
        || lower.starts_with("ghu_")
        || lower.starts_with("ghs_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("xoxp-")
        || lower.starts_with("xoxa-")
        || lower.starts_with("xoxr-")
        || upper.contains("API_KEY=")
        || upper.contains("OPENAI_API_KEY=")
        || upper.contains("ANTHROPIC_API_KEY=")
        || upper.contains("GITHUB_TOKEN=")
        || upper.contains("ACCESS_TOKEN=")
        || upper.contains("AUTH_TOKEN=")
        || upper.contains("PASSWORD=")
        || upper.contains("PASSWD=")
        || upper.contains("SECRET=")
}

fn map_task_frame_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTaskFrame> {
    let local_full_json: String = row.get("local_full_json")?;
    let cloud_redacted_json: String = row.get("cloud_redacted_json")?;
    let scope_json: String = row.get("scope_json")?;
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    let privacy_labels_json: String = row.get("privacy_labels_json")?;
    let projection_policy_json: String = row.get("projection_policy_json")?;
    let blocked_fields_json: String = row.get("blocked_fields_json")?;

    Ok(StoredTaskFrame {
        id: row.get("id")?,
        hash: row.get("hash")?,
        builder_version: row.get("builder_version")?,
        local_full_json: decode_json_value(local_full_json)?,
        cloud_redacted_json: decode_json_value(cloud_redacted_json)?,
        scope: decode_json(scope_json)?,
        project: row.get("project")?,
        session_id: row.get("session_id")?,
        work_mode: row.get("work_mode")?,
        goal_state: row.get("goal_state")?,
        evidence_refs: decode_json(evidence_refs_json)?,
        privacy_labels: decode_json(privacy_labels_json)?,
        projection_policy: decode_json(projection_policy_json)?,
        blocked_fields: decode_json(blocked_fields_json)?,
        created_at_ns: row.get("created_at_ns")?,
    })
}

fn map_task_frame_outcome_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTaskFrameOutcome> {
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    let claim_ids_json: String = row.get("claim_ids_json")?;
    let proposal_ids_json: String = row.get("proposal_ids_json")?;
    let latent_proxy_ids_json: String = row.get("latent_proxy_ids_json")?;
    Ok(StoredTaskFrameOutcome {
        id: row.get("id")?,
        task_frame_id: row.get("task_frame_id")?,
        outcome_type: TaskFrameOutcomeType::from_db(row.get("outcome_type")?)?,
        summary: row.get("summary")?,
        evidence_refs: decode_json(evidence_refs_json)?,
        claim_ids: decode_json(claim_ids_json)?,
        proposal_ids: decode_json(proposal_ids_json)?,
        latent_proxy_ids: decode_json(latent_proxy_ids_json)?,
        created_at_ns: row.get("created_at_ns")?,
    })
}

fn encode_value<T: Serialize>(value: &T, label: &str) -> Result<Value, StorageError> {
    serde_json::to_value(value)
        .map_err(|e| StorageError::Corrupt { detail: format!("{label} encode: {e}") })
}

fn json_string_value<T: Serialize>(value: &T, label: &str) -> Result<String, StorageError> {
    let value = encode_value(value, label)?;
    json_string(&value, label)
}

fn json_string(value: &Value, label: &str) -> Result<String, StorageError> {
    serde_json::to_string(value)
        .map_err(|e| StorageError::Corrupt { detail: format!("{label} encode: {e}") })
}

fn decode_json<T: for<'de> Deserialize<'de>>(json: String) -> rusqlite::Result<T> {
    serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn decode_json_value(json: String) -> rusqlite::Result<Value> {
    decode_json(json)
}

fn from_sql_error(value: String, label: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(StorageError::Corrupt { detail: format!("unknown {label}: {value}") }),
    )
}

fn task_frame_hash(
    builder_version: &str,
    local_full_json: &str,
    projection_policy_json: &str,
) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for chunk in [builder_version, "\n", local_full_json, "\n", projection_policy_json] {
        for byte in chunk.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("tfv1:{hash:016x}")
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
