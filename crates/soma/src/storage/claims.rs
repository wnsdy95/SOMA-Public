//! Claim provenance and verification ledger.
//!
//! Cloud LLM output is stored as `cloud_draft` claims. A draft claim can remain
//! useful as L2 candidate context, but it cannot become L3/L4 memory unless a
//! user, tool, test, local observation, or correction verifies it.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{EpisodeId, EpisodeSource, LifecycleState, Storage, StorageError, StoredEvidenceRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSourceType {
    CloudDraft,
    UserConfirmed,
    ToolVerified,
    LocalObserved,
    ExplicitCorrection,
}

impl ClaimSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimSourceType::CloudDraft => "cloud_draft",
            ClaimSourceType::UserConfirmed => "user_confirmed",
            ClaimSourceType::ToolVerified => "tool_verified",
            ClaimSourceType::LocalObserved => "local_observed",
            ClaimSourceType::ExplicitCorrection => "explicit_correction",
        }
    }

    pub(crate) fn from_db(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "cloud_draft" => Ok(ClaimSourceType::CloudDraft),
            "user_confirmed" => Ok(ClaimSourceType::UserConfirmed),
            "tool_verified" => Ok(ClaimSourceType::ToolVerified),
            "local_observed" => Ok(ClaimSourceType::LocalObserved),
            "explicit_correction" => Ok(ClaimSourceType::ExplicitCorrection),
            _ => Err(from_sql_error(value, "claim source type")),
        }
    }

    fn is_trusted_source(self) -> bool {
        !matches!(self, ClaimSourceType::CloudDraft)
    }
}

impl fmt::Display for ClaimSourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierType {
    User,
    Test,
    Tool,
    LocalObservation,
    Correction,
}

impl VerifierType {
    pub fn as_str(self) -> &'static str {
        match self {
            VerifierType::User => "user",
            VerifierType::Test => "test",
            VerifierType::Tool => "tool",
            VerifierType::LocalObservation => "local_observation",
            VerifierType::Correction => "correction",
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "user" => Ok(VerifierType::User),
            "test" => Ok(VerifierType::Test),
            "tool" => Ok(VerifierType::Tool),
            "local_observation" => Ok(VerifierType::LocalObservation),
            "correction" => Ok(VerifierType::Correction),
            _ => Err(from_sql_error(value, "verifier type")),
        }
    }
}

impl fmt::Display for VerifierType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationResult {
    Confirmed,
    Contradicted,
    Superseded,
    Inconclusive,
}

impl VerificationResult {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationResult::Confirmed => "confirmed",
            VerificationResult::Contradicted => "contradicted",
            VerificationResult::Superseded => "superseded",
            VerificationResult::Inconclusive => "inconclusive",
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "confirmed" => Ok(VerificationResult::Confirmed),
            "contradicted" => Ok(VerificationResult::Contradicted),
            "superseded" => Ok(VerificationResult::Superseded),
            "inconclusive" => Ok(VerificationResult::Inconclusive),
            _ => Err(from_sql_error(value, "verification result")),
        }
    }
}

impl fmt::Display for VerificationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRecordDraft {
    pub text: String,
    pub source_type: ClaimSourceType,
    pub task_frame_id: Option<i64>,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub confidence: f32,
    pub lifecycle_state: LifecycleState,
}

impl ClaimRecordDraft {
    pub fn cloud_draft(task_frame_id: i64, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source_type: ClaimSourceType::CloudDraft,
            task_frame_id: Some(task_frame_id),
            evidence_refs: vec![StoredEvidenceRef {
                kind: "task_frame".to_string(),
                id: task_frame_id.to_string(),
                source: Some("cloud_draft_context".to_string()),
            }],
            confidence: 0.0,
            lifecycle_state: LifecycleState::ShortTermCandidate,
        }
    }

    pub fn trusted(
        source_type: ClaimSourceType,
        text: impl Into<String>,
        evidence_refs: Vec<StoredEvidenceRef>,
    ) -> Self {
        Self {
            text: text.into(),
            source_type,
            task_frame_id: None,
            evidence_refs,
            confidence: 1.0,
            lifecycle_state: LifecycleState::Captured,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredClaimRecord {
    pub id: i64,
    pub text: String,
    pub source_type: ClaimSourceType,
    pub task_frame_id: Option<i64>,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub confidence: f32,
    pub lifecycle_state: LifecycleState,
    pub promotion_reason: Option<String>,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerificationEventDraft {
    pub claim_id: i64,
    pub verifier_type: VerifierType,
    pub result: VerificationResult,
    pub evidence_ref: StoredEvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredVerificationEvent {
    pub id: i64,
    pub claim_id: i64,
    pub verifier_type: VerifierType,
    pub result: VerificationResult,
    pub evidence_ref: StoredEvidenceRef,
    pub created_at_ns: i64,
}

impl Storage {
    pub fn insert_claim_record(&mut self, draft: &ClaimRecordDraft) -> Result<i64, StorageError> {
        validate_claim_record_draft(draft)?;
        let evidence_refs_json = encode_evidence_refs(&draft.evidence_refs)?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO claim_records (
                text, source_type, task_frame_id, evidence_refs_json, confidence,
                lifecycle_state, promotion_reason, created_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7)
             RETURNING id",
            rusqlite::params![
                draft.text.trim(),
                draft.source_type.as_str(),
                draft.task_frame_id,
                evidence_refs_json,
                draft.confidence,
                draft.lifecycle_state.as_str(),
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn claim_record(&self, claim_id: i64) -> Result<Option<StoredClaimRecord>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT * FROM claim_records WHERE id = ?1",
                rusqlite::params![claim_id],
                map_claim_record_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn cloud_output_claim_records_by_ref(
        &self,
        task_frame_id: i64,
        cloud_output_ref_id: &str,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT *
               FROM claim_records
              WHERE task_frame_id = ?1
                AND source_type = 'cloud_draft'
              ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_frame_id], map_claim_record_row)?;
        let mut out = Vec::new();
        for row in rows {
            let claim = row?;
            if claim.evidence_refs.iter().any(|evidence_ref| {
                evidence_ref.kind == "cloud_output" && evidence_ref.id == cloud_output_ref_id
            }) {
                out.push(claim);
            }
        }
        Ok(out)
    }

    pub fn recent_claim_records_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.*
               FROM claim_records c
               LEFT JOIN task_frames tf ON tf.id = c.task_frame_id
              WHERE (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
              ORDER BY c.created_at_ns DESC, c.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_claim_record_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn active_claim_records_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.*
               FROM claim_records c
               LEFT JOIN task_frames tf ON tf.id = c.task_frame_id
              WHERE c.lifecycle_state IN (
                    'captured',
                    'working',
                    'short_term_candidate',
                    'long_term_memory',
                    'semantic_fact'
                )
                AND (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
              ORDER BY c.updated_at_ns DESC, c.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_claim_record_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn unverified_cloud_draft_claim_records_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.*
               FROM claim_records c
               LEFT JOIN task_frames tf ON tf.id = c.task_frame_id
              WHERE c.source_type = 'cloud_draft'
                AND c.lifecycle_state NOT IN (
                    'long_term_memory', 'semantic_fact', 'corrected', 'decayed', 'forgotten'
                )
                AND (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
                AND NOT EXISTS (
                    SELECT 1
                      FROM verification_events ve
                     WHERE ve.claim_id = c.id
                       AND ve.result = 'confirmed'
                       AND ve.verifier_type IN (
                           'user', 'test', 'tool', 'local_observation', 'correction'
                       )
                )
              ORDER BY c.updated_at_ns DESC, c.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_claim_record_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn semantic_claim_records_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.*
               FROM claim_records c
               LEFT JOIN task_frames tf ON tf.id = c.task_frame_id
              WHERE c.lifecycle_state = 'semantic_fact'
                AND (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
              ORDER BY c.updated_at_ns DESC, c.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_claim_record_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn long_term_claim_records_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredClaimRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.*
               FROM claim_records c
               LEFT JOIN task_frames tf ON tf.id = c.task_frame_id
              WHERE c.lifecycle_state = 'long_term_memory'
                AND (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
              ORDER BY c.updated_at_ns DESC, c.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_claim_record_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn insert_verification_event(
        &mut self,
        draft: &VerificationEventDraft,
    ) -> Result<i64, StorageError> {
        validate_verification_event_draft(self, draft)?;
        self.require_claim_record(draft.claim_id)?;
        let evidence_ref_json = encode_evidence_ref(&draft.evidence_ref)?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO verification_events (
                claim_id, verifier_type, result, evidence_ref_json, created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             RETURNING id",
            rusqlite::params![
                draft.claim_id,
                draft.verifier_type.as_str(),
                draft.result.as_str(),
                evidence_ref_json,
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        if matches!(draft.result, VerificationResult::Contradicted | VerificationResult::Superseded)
        {
            self.mark_claim_corrected(
                draft.claim_id,
                &format!("verification_{}", draft.result.as_str()),
            )?;
        }
        Ok(id)
    }

    pub fn verification_events_for_claim(
        &self,
        claim_id: i64,
    ) -> Result<Vec<StoredVerificationEvent>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM verification_events
              WHERE claim_id = ?1
              ORDER BY created_at_ns ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![claim_id], map_verification_event_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn promote_claim_to_long_term(
        &mut self,
        claim_id: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        self.promote_claim_to_state(claim_id, LifecycleState::LongTermMemory, reason)
    }

    pub fn promote_claim_to_semantic(
        &mut self,
        claim_id: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        let current = self.require_claim_record(claim_id)?;
        if current.lifecycle_state != LifecycleState::LongTermMemory {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "claim {claim_id} cannot promote to semantic from {}",
                    current.lifecycle_state
                ),
            });
        }
        self.promote_claim_to_state(claim_id, LifecycleState::SemanticFact, reason)
    }

    pub fn claim_has_durable_promotion_trust(&self, claim_id: i64) -> Result<bool, StorageError> {
        let claim = self.require_claim_record(claim_id)?;
        self.claim_has_promotion_trust(&claim)
    }

    pub fn mark_claim_corrected(
        &mut self,
        claim_id: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        validate_transition_reason(reason)?;
        let claim = self.require_claim_record(claim_id)?;
        if !self.claim_has_correcting_verification(claim.id)? {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "claim {claim_id} requires contradicted or superseded verification before corrected transition"
                ),
            });
        }
        let now_ns = now_ns();
        self.conn.execute(
            "UPDATE claim_records
                SET lifecycle_state = ?1,
                    promotion_reason = ?2,
                    updated_at_ns = ?3
              WHERE id = ?4",
            rusqlite::params![LifecycleState::Corrected.as_str(), reason, now_ns, claim_id],
        )?;
        Ok(())
    }

    fn promote_claim_to_state(
        &mut self,
        claim_id: i64,
        next_state: LifecycleState,
        reason: &str,
    ) -> Result<(), StorageError> {
        validate_transition_reason(reason)?;
        match next_state {
            LifecycleState::LongTermMemory | LifecycleState::SemanticFact => {}
            other => {
                return Err(StorageError::Corrupt {
                    detail: format!("unsupported claim promotion target: {other}"),
                });
            }
        }
        let claim = self.require_claim_record(claim_id)?;
        if !self.claim_has_promotion_trust(&claim)? {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "claim {claim_id} from {} requires confirmed verification before L3/L4 promotion",
                    claim.source_type
                ),
            });
        }
        let now_ns = now_ns();
        self.conn.execute(
            "UPDATE claim_records
                SET lifecycle_state = ?1,
                    promotion_reason = ?2,
                    updated_at_ns = ?3
              WHERE id = ?4",
            rusqlite::params![next_state.as_str(), reason, now_ns, claim_id],
        )?;
        Ok(())
    }

    fn require_claim_record(&self, claim_id: i64) -> Result<StoredClaimRecord, StorageError> {
        self.claim_record(claim_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("claim {claim_id} does not exist"),
        })
    }

    fn claim_has_promotion_trust(&self, claim: &StoredClaimRecord) -> Result<bool, StorageError> {
        if claim.source_type.is_trusted_source() {
            return Ok(true);
        }
        Ok(self.verification_events_for_claim(claim.id)?.iter().any(|event| {
            matches!(event.result, VerificationResult::Confirmed)
                && matches!(
                    event.verifier_type,
                    VerifierType::User
                        | VerifierType::Test
                        | VerifierType::Tool
                        | VerifierType::LocalObservation
                        | VerifierType::Correction
                )
        }))
    }

    fn claim_has_correcting_verification(&self, claim_id: i64) -> Result<bool, StorageError> {
        Ok(self.verification_events_for_claim(claim_id)?.iter().any(|event| {
            matches!(
                event.result,
                VerificationResult::Contradicted | VerificationResult::Superseded
            ) && matches!(
                event.verifier_type,
                VerifierType::User
                    | VerifierType::Test
                    | VerifierType::Tool
                    | VerifierType::LocalObservation
                    | VerifierType::Correction
            )
        }))
    }
}

fn validate_claim_record_draft(draft: &ClaimRecordDraft) -> Result<(), StorageError> {
    if draft.text.trim().is_empty() {
        return Err(StorageError::Corrupt { detail: "claim text cannot be empty".to_string() });
    }
    if !draft.confidence.is_finite() || !(0.0..=1.0).contains(&draft.confidence) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "claim confidence must be finite within [0,1], got {}",
                draft.confidence
            ),
        });
    }
    if draft.evidence_refs.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "claim requires at least one evidence ref".to_string(),
        });
    }
    if draft.source_type == ClaimSourceType::CloudDraft && draft.task_frame_id.is_none() {
        return Err(StorageError::Corrupt {
            detail: "cloud_draft claim requires task_frame_id".to_string(),
        });
    }
    if draft.source_type == ClaimSourceType::CloudDraft
        && draft.lifecycle_state != LifecycleState::ShortTermCandidate
    {
        return Err(StorageError::Corrupt {
            detail: "cloud_draft claim must be inserted as short_term_candidate".to_string(),
        });
    }
    Ok(())
}

fn validate_verification_event_draft(
    storage: &Storage,
    draft: &VerificationEventDraft,
) -> Result<(), StorageError> {
    let kind = draft.evidence_ref.kind.trim();
    let id = draft.evidence_ref.id.trim();
    if kind.is_empty() || id.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "verification event requires non-empty evidence ref kind and id".to_string(),
        });
    }
    if is_forbidden_verification_evidence_kind(kind)
        || draft
            .evidence_ref
            .source
            .as_deref()
            .is_some_and(is_forbidden_verification_evidence_source)
    {
        return Err(StorageError::Corrupt {
            detail: format!(
                "verification event evidence `{kind}:{id}` is not independent verification evidence; cloud/model/protocol/AI-client artifacts must stay draft or audit references"
            ),
        });
    }
    if !is_allowed_verification_evidence_kind(draft.verifier_type, draft.result, kind) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "verification event evidence kind `{kind}` is not allowed for verifier {} and result {}; expected user/tool/test/local/correction evidence",
                draft.verifier_type,
                draft.result
            ),
        });
    }
    if kind.eq_ignore_ascii_case("episode") {
        validate_episode_verification_evidence(storage, draft)?;
    }
    Ok(())
}

fn validate_episode_verification_evidence(
    storage: &Storage,
    draft: &VerificationEventDraft,
) -> Result<(), StorageError> {
    let episode_id =
        draft.evidence_ref.id.trim().parse::<EpisodeId>().map_err(|e| StorageError::Corrupt {
            detail: format!(
                "verification event episode evidence id `{}` is not an episode id: {e}",
                draft.evidence_ref.id
            ),
        })?;
    let episode = storage.get_live_episode(episode_id)?.ok_or_else(|| StorageError::Corrupt {
        detail: format!(
            "verification event episode evidence `{episode_id}` does not exist or is forgotten"
        ),
    })?;
    if is_ai_client_episode_source(&episode.source)
        || is_forbidden_verification_evidence_source(&episode.source.to_string())
    {
        return Err(StorageError::Corrupt {
            detail: format!(
                "verification event episode evidence `{episode_id}` has AI/client source `{}` and is not independent verification evidence",
                episode.source
            ),
        });
    }
    Ok(())
}

fn is_ai_client_episode_source(source: &EpisodeSource) -> bool {
    matches!(
        source,
        EpisodeSource::ClaudeCode
            | EpisodeSource::CodexCli
            | EpisodeSource::CodexApp
            | EpisodeSource::Cursor
            | EpisodeSource::Continue
    )
}

fn validate_transition_reason(reason: &str) -> Result<(), StorageError> {
    if reason.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "claim promotion requires a non-empty reason".to_string(),
        });
    }
    Ok(())
}

fn is_forbidden_verification_evidence_kind(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "cloud_draft"
            | "cloud_output"
            | "cloud_context"
            | "cloud_context_handoff"
            | "cloud_context_artifact"
            | "context_envelope"
            | "compiled_context"
            | "task_frame"
            | "protocol_echo"
            | "handoff_echo"
            | "assistant_response"
            | "model_output"
            | "llm_output"
            | "cloud_llm_output"
            | "prompt"
            | "claim"
            | "claim_record"
            | "learning_critic_proposal"
    )
}

fn is_forbidden_verification_evidence_source(source: &str) -> bool {
    let source = source.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        source.as_str(),
        "cloud_draft"
            | "cloud_output"
            | "cloud_output_capture"
            | "cloud_context"
            | "cloud_context_handoff"
            | "context_envelope"
            | "compiled_context"
            | "task_frame"
            | "protocol_echo"
            | "assistant_response"
            | "model_output"
            | "llm_output"
            | "cloud_llm"
            | "claude_code"
            | "codex_cli"
            | "codex_app"
            | "cursor"
            | "continue"
    ) || [
        "cloud_draft:",
        "cloud_output:",
        "cloud_context:",
        "context_envelope:",
        "compiled_context:",
        "task_frame:",
        "protocol_echo:",
        "assistant_response:",
        "model_output:",
        "llm_output:",
        "cloud_llm:",
        "claude_code:",
        "codex_cli:",
        "codex_app:",
        "cursor:",
        "continue:",
    ]
    .iter()
    .any(|prefix| source.starts_with(prefix))
}

fn is_allowed_verification_evidence_kind(
    verifier_type: VerifierType,
    result: VerificationResult,
    kind: &str,
) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    matches!(
        (verifier_type, result, kind.as_str()),
        (
            VerifierType::LocalObservation,
            VerificationResult::Contradicted
                | VerificationResult::Superseded
                | VerificationResult::Inconclusive,
            "control_critic",
        ) | (VerifierType::User, _, "user" | "user_note" | "manual_review" | "episode")
            | (
                VerifierType::Test,
                _,
                "test" | "test_report" | "eval" | "ci" | "smoke" | "benchmark",
            )
            | (
                VerifierType::Tool,
                _,
                "tool"
                    | "tool_output"
                    | "command_output"
                    | "test"
                    | "test_report"
                    | "eval"
                    | "ci"
                    | "smoke"
                    | "file"
                    | "file_observation",
            )
            | (
                VerifierType::LocalObservation,
                _,
                "local_observation"
                    | "local_file"
                    | "file"
                    | "file_observation"
                    | "workspace"
                    | "episode",
            )
            | (
                VerifierType::Correction,
                _,
                "correction" | "correction_record" | "user_correction" | "episode",
            )
    )
}

fn map_claim_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredClaimRecord> {
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    let source_type: String = row.get("source_type")?;
    let lifecycle_state: String = row.get("lifecycle_state")?;
    let confidence: f64 = row.get("confidence")?;
    Ok(StoredClaimRecord {
        id: row.get("id")?,
        text: row.get("text")?,
        source_type: ClaimSourceType::from_db(source_type)?,
        task_frame_id: row.get("task_frame_id")?,
        evidence_refs: decode_evidence_refs(evidence_refs_json)?,
        confidence: confidence as f32,
        lifecycle_state: lifecycle_state_from_db(lifecycle_state)?,
        promotion_reason: row.get("promotion_reason")?,
        created_at_ns: row.get("created_at_ns")?,
        updated_at_ns: row.get("updated_at_ns")?,
    })
}

fn map_verification_event_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredVerificationEvent> {
    let verifier_type: String = row.get("verifier_type")?;
    let result: String = row.get("result")?;
    let evidence_ref_json: String = row.get("evidence_ref_json")?;
    Ok(StoredVerificationEvent {
        id: row.get("id")?,
        claim_id: row.get("claim_id")?,
        verifier_type: VerifierType::from_db(verifier_type)?,
        result: VerificationResult::from_db(result)?,
        evidence_ref: decode_evidence_ref(evidence_ref_json)?,
        created_at_ns: row.get("created_at_ns")?,
    })
}

fn lifecycle_state_from_db(value: String) -> rusqlite::Result<LifecycleState> {
    match value.as_str() {
        "captured" => Ok(LifecycleState::Captured),
        "working" => Ok(LifecycleState::Working),
        "short_term_candidate" => Ok(LifecycleState::ShortTermCandidate),
        "long_term_memory" => Ok(LifecycleState::LongTermMemory),
        "semantic_fact" => Ok(LifecycleState::SemanticFact),
        "corrected" => Ok(LifecycleState::Corrected),
        "decayed" => Ok(LifecycleState::Decayed),
        "forgotten" => Ok(LifecycleState::Forgotten),
        _ => Err(from_sql_error(value, "claim lifecycle state")),
    }
}

fn encode_evidence_refs(evidence_refs: &[StoredEvidenceRef]) -> Result<String, StorageError> {
    serde_json::to_string(evidence_refs)
        .map_err(|e| StorageError::Corrupt { detail: format!("evidence refs encode: {e}") })
}

fn encode_evidence_ref(evidence_ref: &StoredEvidenceRef) -> Result<String, StorageError> {
    serde_json::to_string(evidence_ref)
        .map_err(|e| StorageError::Corrupt { detail: format!("evidence ref encode: {e}") })
}

fn decode_evidence_refs(json: String) -> rusqlite::Result<Vec<StoredEvidenceRef>> {
    serde_json::from_str(&json).map_err(json_sql_error)
}

fn decode_evidence_ref(json: String) -> rusqlite::Result<StoredEvidenceRef> {
    serde_json::from_str(&json).map_err(json_sql_error)
}

fn json_sql_error(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

fn from_sql_error(value: String, label: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown {label}: {value}"),
        )),
    )
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
