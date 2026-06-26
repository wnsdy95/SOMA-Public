//! Asynchronous learning critic proposal queue.
//!
//! Proposals are audit records, not actions. Inserting a promotion proposal
//! never promotes a claim; the existing verification and lifecycle gates remain
//! the only path into L3/L4 memory.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{LifecycleState, Storage, StorageError, StoredEvidenceRef};

const SEMANTIC_LEARNING_SUPPORT_SOURCE: &str = "soma_semantic_learning";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCriticAction {
    CreateCandidate,
    ProposePromotion,
    Decay,
    RequestVerification,
    Noop,
}

impl LearningCriticAction {
    pub fn as_str(self) -> &'static str {
        match self {
            LearningCriticAction::CreateCandidate => "create_candidate",
            LearningCriticAction::ProposePromotion => "propose_promotion",
            LearningCriticAction::Decay => "decay",
            LearningCriticAction::RequestVerification => "request_verification",
            LearningCriticAction::Noop => "noop",
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "create_candidate" => Ok(LearningCriticAction::CreateCandidate),
            "propose_promotion" => Ok(LearningCriticAction::ProposePromotion),
            "decay" => Ok(LearningCriticAction::Decay),
            "request_verification" => Ok(LearningCriticAction::RequestVerification),
            "noop" => Ok(LearningCriticAction::Noop),
            _ => Err(from_sql_error(value, "learning critic action")),
        }
    }
}

impl fmt::Display for LearningCriticAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCriticProposalStatus {
    Queued,
    WaitingVerification,
    Accepted,
    Rejected,
    Applied,
}

impl LearningCriticProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LearningCriticProposalStatus::Queued => "queued",
            LearningCriticProposalStatus::WaitingVerification => "waiting_verification",
            LearningCriticProposalStatus::Accepted => "accepted",
            LearningCriticProposalStatus::Rejected => "rejected",
            LearningCriticProposalStatus::Applied => "applied",
        }
    }

    fn from_db(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "queued" => Ok(LearningCriticProposalStatus::Queued),
            "waiting_verification" => Ok(LearningCriticProposalStatus::WaitingVerification),
            "accepted" => Ok(LearningCriticProposalStatus::Accepted),
            "rejected" => Ok(LearningCriticProposalStatus::Rejected),
            "applied" => Ok(LearningCriticProposalStatus::Applied),
            _ => Err(from_sql_error(value, "learning critic proposal status")),
        }
    }
}

impl fmt::Display for LearningCriticProposalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningCriticProposalDraft {
    pub task_frame_id: Option<i64>,
    pub action: LearningCriticAction,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claim_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_lifecycle_state: Option<LifecycleState>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredLearningCriticProposal {
    pub id: i64,
    pub task_frame_id: Option<i64>,
    pub action: LearningCriticAction,
    pub claim_ids: Vec<i64>,
    pub target_lifecycle_state: Option<LifecycleState>,
    pub reason: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub status: LearningCriticProposalStatus,
    pub result_json: Option<Value>,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCriticApplyOutcome {
    Applied,
    WaitingVerification,
    Rejected,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LearningCriticApplyOptions {
    pub allow_destructive: bool,
}

impl Storage {
    pub fn insert_learning_critic_proposal(
        &mut self,
        draft: &LearningCriticProposalDraft,
    ) -> Result<i64, StorageError> {
        validate_learning_critic_proposal(self, draft)?;
        let claim_ids_json = encode_json(&draft.claim_ids, "learning critic claim ids")?;
        let evidence_refs_json =
            encode_json(&draft.evidence_refs, "learning critic evidence refs")?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO learning_critic_proposals (
                task_frame_id, action, claim_ids_json, target_lifecycle_state,
                reason, evidence_refs_json, status, result_json,
                created_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?8)
             RETURNING id",
            rusqlite::params![
                draft.task_frame_id,
                draft.action.as_str(),
                claim_ids_json,
                draft.target_lifecycle_state.map(LifecycleState::as_str),
                draft.reason.trim(),
                evidence_refs_json,
                LearningCriticProposalStatus::Queued.as_str(),
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn equivalent_learning_critic_proposal(
        &self,
        draft: &LearningCriticProposalDraft,
    ) -> Result<Option<StoredLearningCriticProposal>, StorageError> {
        validate_learning_critic_proposal(self, draft)?;
        let claim_ids_json = encode_json(&draft.claim_ids, "learning critic claim ids")?;
        let evidence_refs_json =
            encode_json(&draft.evidence_refs, "learning critic evidence refs")?;
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT *
                   FROM learning_critic_proposals
                  WHERE (?1 IS NULL AND task_frame_id IS NULL OR task_frame_id = ?1)
                    AND action = ?2
                    AND claim_ids_json = ?3
                    AND (
                        (?4 IS NULL AND target_lifecycle_state IS NULL)
                        OR target_lifecycle_state = ?4
                    )
                    AND reason = ?5
                    AND evidence_refs_json = ?6
                  ORDER BY id ASC
                  LIMIT 1",
                rusqlite::params![
                    draft.task_frame_id,
                    draft.action.as_str(),
                    claim_ids_json,
                    draft.target_lifecycle_state.map(LifecycleState::as_str),
                    draft.reason.trim(),
                    evidence_refs_json,
                ],
                map_learning_critic_proposal_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn learning_critic_proposal(
        &self,
        proposal_id: i64,
    ) -> Result<Option<StoredLearningCriticProposal>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT * FROM learning_critic_proposals WHERE id = ?1",
                rusqlite::params![proposal_id],
                map_learning_critic_proposal_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn queued_learning_critic_proposals(
        &self,
        limit: usize,
    ) -> Result<Vec<StoredLearningCriticProposal>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM learning_critic_proposals
              WHERE status = 'queued'
              ORDER BY created_at_ns ASC, id ASC
              LIMIT ?1",
        )?;
        let rows =
            stmt.query_map(rusqlite::params![limit as i64], map_learning_critic_proposal_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn recent_learning_critic_proposals_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredLearningCriticProposal>, StorageError> {
        self.learning_critic_proposals_scoped(project, session_id, None, limit)
    }

    pub fn learning_critic_proposals_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        status: Option<LearningCriticProposalStatus>,
        limit: usize,
    ) -> Result<Vec<StoredLearningCriticProposal>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.*
               FROM learning_critic_proposals p
               LEFT JOIN task_frames tf ON tf.id = p.task_frame_id
              WHERE (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
                AND (?3 IS NULL OR p.status = ?3)
              ORDER BY p.created_at_ns DESC, p.id DESC
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                project,
                session_id,
                status.map(|status| status.as_str()),
                limit as i64
            ],
            map_learning_critic_proposal_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn open_learning_critic_proposals_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredLearningCriticProposal>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.*
               FROM learning_critic_proposals p
               LEFT JOIN task_frames tf ON tf.id = p.task_frame_id
              WHERE (?1 IS NULL OR tf.project = ?1)
                AND (?2 IS NULL OR tf.session_id = ?2)
                AND p.status IN ('queued', 'waiting_verification', 'accepted')
              ORDER BY
                CASE p.status
                    WHEN 'waiting_verification' THEN 0
                    WHEN 'queued' THEN 1
                    WHEN 'accepted' THEN 2
                    ELSE 3
                END ASC,
                p.created_at_ns ASC,
                p.id ASC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project, session_id, limit as i64],
            map_learning_critic_proposal_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn update_learning_critic_proposal_status(
        &mut self,
        proposal_id: i64,
        status: LearningCriticProposalStatus,
        result_json: Option<&Value>,
    ) -> Result<(), StorageError> {
        self.require_learning_critic_proposal(proposal_id)?;
        let result_json = match result_json {
            Some(value) => Some(encode_json(value, "learning critic result")?),
            None => None,
        };
        let now_ns = now_ns();
        self.conn.execute(
            "UPDATE learning_critic_proposals
                SET status = ?1,
                    result_json = ?2,
                    updated_at_ns = ?3
              WHERE id = ?4",
            rusqlite::params![status.as_str(), result_json, now_ns, proposal_id],
        )?;
        Ok(())
    }

    pub fn apply_learning_critic_proposal(
        &mut self,
        proposal_id: i64,
    ) -> Result<LearningCriticApplyOutcome, StorageError> {
        self.apply_learning_critic_proposal_with_options(
            proposal_id,
            LearningCriticApplyOptions::default(),
        )
    }

    pub fn apply_learning_critic_proposal_with_options(
        &mut self,
        proposal_id: i64,
        options: LearningCriticApplyOptions,
    ) -> Result<LearningCriticApplyOutcome, StorageError> {
        let proposal = self.require_learning_critic_proposal(proposal_id)?;
        match proposal.status {
            LearningCriticProposalStatus::Applied => {
                return Ok(LearningCriticApplyOutcome::Applied)
            }
            LearningCriticProposalStatus::Rejected => {
                return Ok(LearningCriticApplyOutcome::Rejected);
            }
            LearningCriticProposalStatus::Queued
            | LearningCriticProposalStatus::WaitingVerification
            | LearningCriticProposalStatus::Accepted => {}
        }

        match proposal.action {
            LearningCriticAction::ProposePromotion => self.apply_promotion_proposal(&proposal),
            LearningCriticAction::Decay => {
                if !options.allow_destructive {
                    return Err(StorageError::Corrupt {
                        detail: "destructive decay/forget proposal requires explicit destructive confirmation".to_string(),
                    });
                }
                self.apply_decay_proposal(&proposal)
            }
            LearningCriticAction::RequestVerification => {
                self.mark_learning_critic_waiting_verification(
                    proposal.id,
                    &proposal.claim_ids,
                    "verification_requested",
                )?;
                Ok(LearningCriticApplyOutcome::WaitingVerification)
            }
            LearningCriticAction::CreateCandidate | LearningCriticAction::Noop => {
                self.mark_learning_critic_applied(
                    proposal.id,
                    &proposal.claim_ids,
                    json!({
                        "outcome": "applied",
                        "action": proposal.action.as_str(),
                        "note": "proposal recorded; no lifecycle mutation required"
                    }),
                )?;
                Ok(LearningCriticApplyOutcome::Applied)
            }
        }
    }

    fn apply_promotion_proposal(
        &mut self,
        proposal: &StoredLearningCriticProposal,
    ) -> Result<LearningCriticApplyOutcome, StorageError> {
        let Some(target) = proposal.target_lifecycle_state else {
            return Err(StorageError::Corrupt {
                detail: format!("proposal {} missing promotion target", proposal.id),
            });
        };
        if target == LifecycleState::SemanticFact {
            if let Some(invalid_support_claim_ids) =
                self.invalid_semantic_learning_support_claim_ids(proposal)?
            {
                self.update_learning_critic_proposal_status(
                    proposal.id,
                    LearningCriticProposalStatus::Rejected,
                    Some(&json!({
                        "outcome": "rejected",
                        "reason": "semantic_support_claims_invalidated_before_apply",
                        "claim_ids": proposal.claim_ids,
                        "invalid_support_claim_ids": invalid_support_claim_ids,
                    })),
                )?;
                return Ok(LearningCriticApplyOutcome::Rejected);
            }
        }
        let mut missing_trust = Vec::new();
        for claim_id in &proposal.claim_ids {
            if !self.claim_has_durable_promotion_trust(*claim_id)? {
                missing_trust.push(*claim_id);
            }
        }
        if !missing_trust.is_empty() {
            self.mark_learning_critic_waiting_verification(
                proposal.id,
                &missing_trust,
                "confirmed_verification_required",
            )?;
            return Ok(LearningCriticApplyOutcome::WaitingVerification);
        }

        for claim_id in &proposal.claim_ids {
            let claim = self.claim_record(*claim_id)?.ok_or_else(|| StorageError::Corrupt {
                detail: format!("proposal {} references missing claim {claim_id}", proposal.id),
            })?;
            match target {
                LifecycleState::LongTermMemory => {
                    if !matches!(
                        claim.lifecycle_state,
                        LifecycleState::LongTermMemory | LifecycleState::SemanticFact
                    ) {
                        self.promote_claim_to_long_term(*claim_id, &proposal.reason)?;
                    }
                }
                LifecycleState::SemanticFact => {
                    if claim.lifecycle_state != LifecycleState::SemanticFact {
                        if claim.lifecycle_state != LifecycleState::LongTermMemory {
                            self.promote_claim_to_long_term(*claim_id, &proposal.reason)?;
                        }
                        self.promote_claim_to_semantic(*claim_id, &proposal.reason)?;
                    }
                }
                other => {
                    return Err(StorageError::Corrupt {
                        detail: format!("unsupported promotion apply target: {other}"),
                    });
                }
            }
        }

        self.mark_learning_critic_applied(
            proposal.id,
            &proposal.claim_ids,
            json!({
                "outcome": "applied",
                "action": proposal.action.as_str(),
                "target_lifecycle_state": target.as_str(),
                "claim_ids": proposal.claim_ids,
            }),
        )?;
        Ok(LearningCriticApplyOutcome::Applied)
    }

    fn invalid_semantic_learning_support_claim_ids(
        &self,
        proposal: &StoredLearningCriticProposal,
    ) -> Result<Option<Vec<i64>>, StorageError> {
        let support_claim_ids = semantic_learning_support_claim_ids(proposal);
        if support_claim_ids.is_empty() {
            return Ok(None);
        }

        let mut invalid = Vec::new();
        for claim_id in support_claim_ids {
            let Some(claim) = self.claim_record(claim_id)? else {
                invalid.push(claim_id);
                continue;
            };
            let active_l3_or_l4 = matches!(
                claim.lifecycle_state,
                LifecycleState::LongTermMemory | LifecycleState::SemanticFact
            );
            if !active_l3_or_l4 || !self.claim_has_durable_promotion_trust(claim_id)? {
                invalid.push(claim_id);
            }
        }
        Ok((!invalid.is_empty()).then_some(invalid))
    }

    fn apply_decay_proposal(
        &mut self,
        proposal: &StoredLearningCriticProposal,
    ) -> Result<LearningCriticApplyOutcome, StorageError> {
        let Some(target) = proposal.target_lifecycle_state else {
            return Err(StorageError::Corrupt {
                detail: format!("proposal {} missing decay target", proposal.id),
            });
        };
        if !matches!(target, LifecycleState::Decayed | LifecycleState::Forgotten) {
            return Err(StorageError::Corrupt {
                detail: format!("unsupported decay apply target: {target}"),
            });
        }
        let now_ns = now_ns();
        for claim_id in &proposal.claim_ids {
            self.conn.execute(
                "UPDATE claim_records
                    SET lifecycle_state = ?1,
                        promotion_reason = ?2,
                        updated_at_ns = ?3
                  WHERE id = ?4",
                rusqlite::params![target.as_str(), proposal.reason.as_str(), now_ns, claim_id],
            )?;
        }
        self.mark_learning_critic_applied(
            proposal.id,
            &proposal.claim_ids,
            json!({
                "outcome": "applied",
                "action": proposal.action.as_str(),
                "target_lifecycle_state": target.as_str(),
                "claim_ids": proposal.claim_ids,
            }),
        )?;
        Ok(LearningCriticApplyOutcome::Applied)
    }

    fn mark_learning_critic_waiting_verification(
        &mut self,
        proposal_id: i64,
        claim_ids: &[i64],
        reason: &str,
    ) -> Result<(), StorageError> {
        self.update_learning_critic_proposal_status(
            proposal_id,
            LearningCriticProposalStatus::WaitingVerification,
            Some(&json!({
                "outcome": "waiting_verification",
                "reason": reason,
                "claim_ids": claim_ids,
            })),
        )
    }

    fn mark_learning_critic_applied(
        &mut self,
        proposal_id: i64,
        claim_ids: &[i64],
        result: Value,
    ) -> Result<(), StorageError> {
        let mut value = result;
        if let Some(obj) = value.as_object_mut() {
            obj.entry("claim_ids".to_string()).or_insert_with(|| json!(claim_ids));
        }
        self.update_learning_critic_proposal_status(
            proposal_id,
            LearningCriticProposalStatus::Applied,
            Some(&value),
        )
    }

    fn require_learning_critic_proposal(
        &self,
        proposal_id: i64,
    ) -> Result<StoredLearningCriticProposal, StorageError> {
        self.learning_critic_proposal(proposal_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("learning critic proposal {proposal_id} does not exist"),
        })
    }
}

fn semantic_learning_support_claim_ids(proposal: &StoredLearningCriticProposal) -> Vec<i64> {
    let mut ids = proposal
        .evidence_refs
        .iter()
        .filter(|evidence| {
            evidence.kind == "claim_record"
                && evidence.source.as_deref() == Some(SEMANTIC_LEARNING_SUPPORT_SOURCE)
        })
        .filter_map(|evidence| evidence.id.parse::<i64>().ok())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn validate_learning_critic_proposal(
    storage: &Storage,
    draft: &LearningCriticProposalDraft,
) -> Result<(), StorageError> {
    if draft.reason.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "learning critic proposal requires a non-empty reason".to_string(),
        });
    }
    if draft.evidence_refs.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "learning critic proposal requires at least one evidence ref".to_string(),
        });
    }
    if let Some(task_frame_id) = draft.task_frame_id {
        if storage.task_frame(task_frame_id)?.is_none() {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "learning critic proposal requires existing TaskFrame {task_frame_id}"
                ),
            });
        }
    }
    for claim_id in &draft.claim_ids {
        if storage.claim_record(*claim_id)?.is_none() {
            return Err(StorageError::Corrupt {
                detail: format!("learning critic proposal references missing claim {claim_id}"),
            });
        }
    }

    match draft.action {
        LearningCriticAction::CreateCandidate => {
            if draft.target_lifecycle_state != Some(LifecycleState::ShortTermCandidate) {
                return Err(StorageError::Corrupt {
                    detail: "create_candidate proposal targets short_term_candidate".to_string(),
                });
            }
        }
        LearningCriticAction::ProposePromotion => {
            if draft.claim_ids.is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "propose_promotion requires at least one claim id".to_string(),
                });
            }
            if !matches!(
                draft.target_lifecycle_state,
                Some(LifecycleState::LongTermMemory | LifecycleState::SemanticFact)
            ) {
                return Err(StorageError::Corrupt {
                    detail: "propose_promotion targets long_term_memory or semantic_fact"
                        .to_string(),
                });
            }
        }
        LearningCriticAction::Decay => {
            if draft.claim_ids.is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "decay proposal requires at least one claim id".to_string(),
                });
            }
            if !matches!(
                draft.target_lifecycle_state,
                Some(LifecycleState::Decayed | LifecycleState::Forgotten)
            ) {
                return Err(StorageError::Corrupt {
                    detail: "decay proposal targets decayed or forgotten".to_string(),
                });
            }
        }
        LearningCriticAction::RequestVerification => {
            if draft.claim_ids.is_empty() {
                return Err(StorageError::Corrupt {
                    detail: "request_verification requires at least one claim id".to_string(),
                });
            }
            if draft.target_lifecycle_state.is_some() {
                return Err(StorageError::Corrupt {
                    detail: "request_verification does not target a lifecycle state".to_string(),
                });
            }
        }
        LearningCriticAction::Noop => {
            if draft.target_lifecycle_state.is_some() {
                return Err(StorageError::Corrupt {
                    detail: "noop does not target a lifecycle state".to_string(),
                });
            }
        }
    }
    Ok(())
}

fn map_learning_critic_proposal_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredLearningCriticProposal> {
    let action: String = row.get("action")?;
    let claim_ids_json: String = row.get("claim_ids_json")?;
    let target_lifecycle_state: Option<String> = row.get("target_lifecycle_state")?;
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    let status: String = row.get("status")?;
    let result_json: Option<String> = row.get("result_json")?;

    Ok(StoredLearningCriticProposal {
        id: row.get("id")?,
        task_frame_id: row.get("task_frame_id")?,
        action: LearningCriticAction::from_db(action)?,
        claim_ids: decode_json(claim_ids_json)?,
        target_lifecycle_state: match target_lifecycle_state {
            Some(value) => Some(lifecycle_state_from_db(value)?),
            None => None,
        },
        reason: row.get("reason")?,
        evidence_refs: decode_json(evidence_refs_json)?,
        status: LearningCriticProposalStatus::from_db(status)?,
        result_json: match result_json {
            Some(json) => Some(decode_json(json)?),
            None => None,
        },
        created_at_ns: row.get("created_at_ns")?,
        updated_at_ns: row.get("updated_at_ns")?,
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
        _ => Err(from_sql_error(value, "learning critic lifecycle state")),
    }
}

fn encode_json<T: Serialize>(value: &T, label: &str) -> Result<String, StorageError> {
    serde_json::to_string(value)
        .map_err(|e| StorageError::Corrupt { detail: format!("{label} encode: {e}") })
}

fn decode_json<T: for<'de> Deserialize<'de>>(json: String) -> rusqlite::Result<T> {
    serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
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
