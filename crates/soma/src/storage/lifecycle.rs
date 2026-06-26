//! Explicit memory lifecycle rows for the four-stage learning hierarchy.
//!
//! This module intentionally stores system-level latent proxies rather than
//! model-internal neural latents. A proxy is a typed abstraction from an
//! episode that still carries evidence refs, lifecycle state, and a future
//! ContextEnvelope projection target.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{ClaimSourceType, EpisodeId, SensitivityLabel, Storage, StorageError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLayer {
    Working,
    ShortTerm,
    LongTerm,
    Semantic,
}

impl MemoryLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryLayer::Working => "working",
            MemoryLayer::ShortTerm => "short_term",
            MemoryLayer::LongTerm => "long_term",
            MemoryLayer::Semantic => "semantic",
        }
    }
}

impl fmt::Display for MemoryLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Captured,
    Working,
    ShortTermCandidate,
    LongTermMemory,
    SemanticFact,
    Corrected,
    Decayed,
    Forgotten,
}

impl LifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleState::Captured => "captured",
            LifecycleState::Working => "working",
            LifecycleState::ShortTermCandidate => "short_term_candidate",
            LifecycleState::LongTermMemory => "long_term_memory",
            LifecycleState::SemanticFact => "semantic_fact",
            LifecycleState::Corrected => "corrected",
            LifecycleState::Decayed => "decayed",
            LifecycleState::Forgotten => "forgotten",
        }
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredEvidenceRef {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl StoredEvidenceRef {
    pub fn episode(id: EpisodeId) -> Self {
        Self { kind: "episode".to_string(), id: id.to_string(), source: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBackedLatentProxyDraft {
    pub episode_id: EpisodeId,
    pub proxy_type: String,
    pub target: Option<String>,
    pub claim: String,
    pub scope: Option<String>,
    pub confidence: f32,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub expires_at_ns: Option<i64>,
    pub privacy_labels: Vec<SensitivityLabel>,
    pub source_trust: ClaimSourceType,
    pub supersedes_proxy_id: Option<i64>,
}

impl EvidenceBackedLatentProxyDraft {
    pub fn short_term(
        episode_id: EpisodeId,
        proxy_type: impl Into<String>,
        claim: impl Into<String>,
    ) -> Self {
        Self {
            episode_id,
            proxy_type: proxy_type.into(),
            target: None,
            claim: claim.into(),
            scope: None,
            confidence: 0.0,
            evidence_refs: vec![StoredEvidenceRef::episode(episode_id)],
            expires_at_ns: None,
            privacy_labels: vec![SensitivityLabel::ProjectInternal],
            source_trust: ClaimSourceType::LocalObserved,
            supersedes_proxy_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBackedLatentProxy {
    pub id: i64,
    pub episode_id: EpisodeId,
    pub proxy_type: String,
    pub target: Option<String>,
    pub claim: String,
    pub scope: Option<String>,
    pub confidence: f32,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub expires_at_ns: Option<i64>,
    pub privacy_labels: Vec<SensitivityLabel>,
    pub source_trust: ClaimSourceType,
    pub memory_layer: String,
    pub lifecycle_state: String,
    pub promotion_reason: Option<String>,
    pub envelope_section: Option<String>,
    pub supersedes_proxy_id: Option<i64>,
    pub access_count: i64,
    pub last_accessed_at_ns: Option<i64>,
    pub decay_score: f32,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryLifecycleEvent {
    pub id: i64,
    pub proxy_id: i64,
    pub from_layer: Option<String>,
    pub from_state: Option<String>,
    pub to_layer: String,
    pub to_state: String,
    pub transition_reason: String,
    pub evidence_refs: Vec<StoredEvidenceRef>,
    pub envelope_section: Option<String>,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LongTermProxyDecayReport {
    pub kind: &'static str,
    pub dry_run: bool,
    pub cutoff_ns: i64,
    pub max_access_count: i64,
    pub inspected_count: usize,
    pub decayed_proxy_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShortTermProxyPromotionRequest {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub dry_run: bool,
    pub min_confidence: f32,
    pub anomaly_min_confidence: f32,
    pub min_repeated_support: usize,
    pub manual_proxy_ids: Vec<i64>,
    pub reason: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShortTermProxyPromotionReport {
    pub kind: &'static str,
    pub dry_run: bool,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub min_confidence: f32,
    pub anomaly_min_confidence: f32,
    pub min_repeated_support: usize,
    pub manual_proxy_ids: Vec<i64>,
    pub inspected_count: usize,
    pub eligible_count: usize,
    pub promoted_proxy_ids: Vec<i64>,
    pub skipped_cloud_draft_count: usize,
    pub skipped_unsafe_privacy_count: usize,
    pub skipped_low_signal_count: usize,
    pub candidates: Vec<ShortTermProxyPromotionCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShortTermProxyPromotionCandidate {
    pub proxy_id: i64,
    pub rule: String,
    pub reason: String,
    pub proxy_type: String,
    pub target: Option<String>,
    pub claim: String,
    pub confidence: f32,
    pub repeated_support_count: usize,
    pub source_trust: String,
    pub privacy_labels: Vec<String>,
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

impl Storage {
    /// Insert an L2 evidence-backed latent proxy for an episode.
    ///
    /// This is the first implementation step for own-latent-inspired memory:
    /// raw episode text stays as evidence, while lifecycle rules operate on a
    /// typed, auditable abstraction.
    pub fn insert_evidence_latent_proxy(
        &mut self,
        draft: &EvidenceBackedLatentProxyDraft,
    ) -> Result<i64, StorageError> {
        validate_proxy_draft(draft)?;
        let evidence_refs_json = encode_evidence_refs(&draft.evidence_refs)?;
        let privacy_labels_json = encode_privacy_labels(&draft.privacy_labels)?;
        let now_ns = now_ns();
        let id = self.conn.query_row(
            "INSERT INTO evidence_latent_proxies (
                episode_id, proxy_type, target, claim, scope, confidence,
                evidence_refs_json, expires_at_ns, privacy_labels_json, source_trust,
                memory_layer, lifecycle_state,
                promotion_reason, envelope_section, supersedes_proxy_id,
                created_at_ns, updated_at_ns
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10,
                ?11, ?12,
                NULL, NULL, ?13,
                ?14, ?14
             ) RETURNING id",
            rusqlite::params![
                draft.episode_id,
                draft.proxy_type.as_str(),
                draft.target.as_deref(),
                draft.claim.as_str(),
                draft.scope.as_deref(),
                draft.confidence,
                evidence_refs_json,
                draft.expires_at_ns,
                privacy_labels_json,
                draft.source_trust.as_str(),
                MemoryLayer::ShortTerm.as_str(),
                LifecycleState::ShortTermCandidate.as_str(),
                draft.supersedes_proxy_id,
                now_ns
            ],
            |row| row.get::<_, i64>(0),
        )?;
        self.insert_lifecycle_event(
            id,
            None,
            None,
            MemoryLayer::ShortTerm,
            LifecycleState::ShortTermCandidate,
            "extracted_from_episode",
            &draft.evidence_refs,
            Some("short_term_candidates"),
            now_ns,
        )?;
        Ok(id)
    }

    /// Promote an L2 proxy to L3 durable episodic evidence.
    pub fn promote_proxy_to_long_term(
        &mut self,
        proxy_id: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        validate_transition_reason(reason)?;
        let current = self.require_proxy(proxy_id)?;
        if current.source_trust == ClaimSourceType::CloudDraft {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "cloud_draft proxy {proxy_id} requires verification before long_term promotion"
                ),
            });
        }
        if current.memory_layer != MemoryLayer::ShortTerm.as_str()
            || current.lifecycle_state != LifecycleState::ShortTermCandidate.as_str()
        {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "proxy {proxy_id} cannot promote to long_term from {}/{}",
                    current.memory_layer, current.lifecycle_state
                ),
            });
        }
        self.transition_proxy(
            &current,
            MemoryLayer::LongTerm,
            LifecycleState::LongTermMemory,
            reason,
            Some("relevant_memory"),
        )
    }

    /// Promote an L3 proxy to L4 semantic memory. Direct L2 -> L4 promotion is
    /// rejected so anomaly/conflict candidates cannot harden into policy by
    /// accident.
    pub fn promote_proxy_to_semantic(
        &mut self,
        proxy_id: i64,
        reason: &str,
        envelope_section: &str,
    ) -> Result<(), StorageError> {
        validate_transition_reason(reason)?;
        validate_semantic_envelope_section(envelope_section)?;
        let current = self.require_proxy(proxy_id)?;
        if current.memory_layer != MemoryLayer::LongTerm.as_str()
            || current.lifecycle_state != LifecycleState::LongTermMemory.as_str()
        {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "proxy {proxy_id} cannot promote to semantic from {}/{}",
                    current.memory_layer, current.lifecycle_state
                ),
            });
        }
        self.transition_proxy(
            &current,
            MemoryLayer::Semantic,
            LifecycleState::SemanticFact,
            reason,
            Some(envelope_section),
        )
    }

    pub fn evidence_latent_proxy(
        &self,
        proxy_id: i64,
    ) -> Result<Option<EvidenceBackedLatentProxy>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT * FROM evidence_latent_proxies WHERE id = ?1",
                rusqlite::params![proxy_id],
                map_proxy_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn lifecycle_events_for_proxy(
        &self,
        proxy_id: i64,
    ) -> Result<Vec<MemoryLifecycleEvent>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM memory_lifecycle_events
              WHERE proxy_id = ?1
              ORDER BY created_at_ns ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![proxy_id], map_event_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn short_term_candidate_proxies(
        &self,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT * FROM evidence_latent_proxies
              WHERE memory_layer = ?1
                AND lifecycle_state = ?2
                AND (expires_at_ns IS NULL OR expires_at_ns > ?3)
              ORDER BY updated_at_ns DESC, id DESC
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::ShortTerm.as_str(),
                LifecycleState::ShortTermCandidate.as_str(),
                now_ns(),
                limit as i64
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn long_term_proxies_for_envelope_section(
        &self,
        envelope_section: &str,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        validate_long_term_envelope_section(envelope_section)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT * FROM evidence_latent_proxies
              WHERE memory_layer = ?1
                AND lifecycle_state = ?2
                AND envelope_section = ?3
                AND (expires_at_ns IS NULL OR expires_at_ns > ?4)
              ORDER BY decay_score DESC,
                       access_count DESC,
                       updated_at_ns DESC,
                       id DESC
              LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::LongTerm.as_str(),
                LifecycleState::LongTermMemory.as_str(),
                envelope_section,
                now_ns(),
                limit as i64
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn semantic_proxies_for_envelope_section(
        &self,
        envelope_section: &str,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        validate_semantic_envelope_section(envelope_section)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT * FROM evidence_latent_proxies
              WHERE memory_layer = ?1
                AND lifecycle_state = ?2
                AND envelope_section = ?3
              ORDER BY updated_at_ns DESC, id DESC
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::Semantic.as_str(),
                LifecycleState::SemanticFact.as_str(),
                envelope_section,
                limit as i64
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Read-only scan used by latent prediction/evaluation. This returns only
    /// active lifecycle states and honors expiry, but it does not apply
    /// cloud-projection privacy or source-trust gates; callers keep those gates
    /// explicit in their reports.
    pub fn active_evidence_latent_proxies(
        &self,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT * FROM evidence_latent_proxies
              WHERE (
                    (memory_layer = ?1 AND lifecycle_state = ?2)
                 OR (memory_layer = ?3 AND lifecycle_state = ?4)
                 OR (memory_layer = ?5 AND lifecycle_state = ?6)
                )
                AND (expires_at_ns IS NULL OR expires_at_ns > ?7)
              ORDER BY
                CASE memory_layer
                    WHEN 'semantic' THEN 0
                    WHEN 'long_term' THEN 1
                    WHEN 'short_term' THEN 2
                    ELSE 3
                END,
                decay_score DESC,
                access_count DESC,
                updated_at_ns DESC,
                id DESC
              LIMIT ?8",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::ShortTerm.as_str(),
                LifecycleState::ShortTermCandidate.as_str(),
                MemoryLayer::LongTerm.as_str(),
                LifecycleState::LongTermMemory.as_str(),
                MemoryLayer::Semantic.as_str(),
                LifecycleState::SemanticFact.as_str(),
                now_ns(),
                limit as i64
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn promote_short_term_proxies_by_policy(
        &mut self,
        request: &ShortTermProxyPromotionRequest,
    ) -> Result<ShortTermProxyPromotionReport, StorageError> {
        validate_short_term_promotion_request(request)?;
        let proxies = self.short_term_candidate_proxies_scoped(
            request.project.as_deref(),
            request.session_id.as_deref(),
            request.limit,
        )?;
        let repeated_support = repeated_claim_support_counts(&proxies);
        let manual_proxy_ids = request.manual_proxy_ids.iter().copied().collect::<HashSet<_>>();
        let mut report = ShortTermProxyPromotionReport {
            kind: "short_term_proxy_promotion",
            dry_run: request.dry_run,
            project: request.project.clone(),
            session_id: request.session_id.clone(),
            min_confidence: request.min_confidence,
            anomaly_min_confidence: request.anomaly_min_confidence,
            min_repeated_support: request.min_repeated_support,
            manual_proxy_ids: request.manual_proxy_ids.clone(),
            inspected_count: proxies.len(),
            eligible_count: 0,
            promoted_proxy_ids: Vec::new(),
            skipped_cloud_draft_count: 0,
            skipped_unsafe_privacy_count: 0,
            skipped_low_signal_count: 0,
            candidates: Vec::new(),
        };

        for proxy in proxies {
            if proxy.source_trust == ClaimSourceType::CloudDraft {
                report.skipped_cloud_draft_count += 1;
                continue;
            }
            if !cloud_safe_proxy_privacy(&proxy.privacy_labels) {
                report.skipped_unsafe_privacy_count += 1;
                continue;
            }
            let signature = normalized_claim_signature(&proxy.claim);
            let support_count = repeated_support.get(&signature).copied().unwrap_or(1);
            let Some(rule) =
                promotion_rule_for_proxy(&proxy, &manual_proxy_ids, support_count, request)
            else {
                report.skipped_low_signal_count += 1;
                continue;
            };
            let reason = format!("l2_promotion:{}:{}", rule, request.reason.trim());
            if !request.dry_run {
                self.promote_proxy_to_long_term(proxy.id, &reason)?;
                report.promoted_proxy_ids.push(proxy.id);
            }
            report.eligible_count += 1;
            report.candidates.push(ShortTermProxyPromotionCandidate {
                proxy_id: proxy.id,
                rule: rule.to_string(),
                reason,
                proxy_type: proxy.proxy_type,
                target: proxy.target,
                claim: proxy.claim,
                confidence: proxy.confidence,
                repeated_support_count: support_count,
                source_trust: proxy.source_trust.as_str().to_string(),
                privacy_labels: proxy
                    .privacy_labels
                    .into_iter()
                    .map(SensitivityLabel::as_str)
                    .map(str::to_string)
                    .collect(),
                evidence_refs: proxy.evidence_refs,
            });
        }

        Ok(report)
    }

    fn short_term_candidate_proxies_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT p.*
               FROM evidence_latent_proxies p
               LEFT JOIN episodes e ON e.id = p.episode_id
              WHERE p.memory_layer = ?1
                AND p.lifecycle_state = ?2
                AND (p.expires_at_ns IS NULL OR p.expires_at_ns > ?3)
                AND (?4 IS NULL OR e.project = ?4)
                AND (?5 IS NULL OR e.session_id = ?5)
              ORDER BY p.updated_at_ns DESC, p.id DESC
              LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::ShortTerm.as_str(),
                LifecycleState::ShortTermCandidate.as_str(),
                now_ns(),
                project,
                session_id,
                limit as i64,
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn require_proxy(&self, proxy_id: i64) -> Result<EvidenceBackedLatentProxy, StorageError> {
        self.evidence_latent_proxy(proxy_id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("proxy {proxy_id} does not exist"),
        })
    }

    pub fn decay_inactive_long_term_proxies(
        &mut self,
        cutoff_ns: i64,
        max_access_count: i64,
        reason: &str,
        dry_run: bool,
        limit: usize,
    ) -> Result<LongTermProxyDecayReport, StorageError> {
        self.decay_inactive_long_term_proxies_scoped(
            None,
            None,
            cutoff_ns,
            max_access_count,
            reason,
            dry_run,
            limit,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decay_inactive_long_term_proxies_scoped(
        &mut self,
        project: Option<&str>,
        session_id: Option<&str>,
        cutoff_ns: i64,
        max_access_count: i64,
        reason: &str,
        dry_run: bool,
        limit: usize,
    ) -> Result<LongTermProxyDecayReport, StorageError> {
        if cutoff_ns <= 0 {
            return Err(StorageError::Corrupt {
                detail: format!("cutoff_ns must be positive, got {cutoff_ns}"),
            });
        }
        if max_access_count < 0 {
            return Err(StorageError::Corrupt {
                detail: format!("max_access_count must be non-negative, got {max_access_count}"),
            });
        }
        validate_transition_reason(reason)?;
        if limit == 0 {
            return Ok(LongTermProxyDecayReport {
                kind: "long_term_proxy_decay",
                dry_run,
                cutoff_ns,
                max_access_count,
                inspected_count: 0,
                decayed_proxy_ids: Vec::new(),
            });
        }

        let candidates = self.inactive_long_term_proxy_candidates_scoped(
            project,
            session_id,
            cutoff_ns,
            max_access_count,
            limit,
        )?;
        let decayed_proxy_ids = candidates.iter().map(|proxy| proxy.id).collect::<Vec<_>>();
        let inspected_count = candidates.len();
        if !dry_run {
            for proxy in &candidates {
                self.transition_proxy(
                    proxy,
                    MemoryLayer::LongTerm,
                    LifecycleState::Decayed,
                    reason,
                    proxy.envelope_section.as_deref(),
                )?;
            }
        }

        Ok(LongTermProxyDecayReport {
            kind: "long_term_proxy_decay",
            dry_run,
            cutoff_ns,
            max_access_count,
            inspected_count,
            decayed_proxy_ids,
        })
    }

    fn inactive_long_term_proxy_candidates_scoped(
        &self,
        project: Option<&str>,
        session_id: Option<&str>,
        cutoff_ns: i64,
        max_access_count: i64,
        limit: usize,
    ) -> Result<Vec<EvidenceBackedLatentProxy>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.*
               FROM evidence_latent_proxies p
               LEFT JOIN episodes e ON e.id = p.episode_id
              WHERE p.memory_layer = ?1
                AND p.lifecycle_state = ?2
                AND COALESCE(p.last_accessed_at_ns, p.updated_at_ns, p.created_at_ns) < ?3
                AND p.access_count <= ?4
                AND (?5 IS NULL OR e.project = ?5)
                AND (?6 IS NULL OR e.session_id = ?6)
              ORDER BY COALESCE(p.last_accessed_at_ns, p.updated_at_ns, p.created_at_ns) ASC, p.id ASC
              LIMIT ?7",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                MemoryLayer::LongTerm.as_str(),
                LifecycleState::LongTermMemory.as_str(),
                cutoff_ns,
                max_access_count,
                project,
                session_id,
                limit as i64,
            ],
            map_proxy_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn touch_long_term_proxy_accesses<I>(&self, proxy_ids: I) -> Result<(), StorageError>
    where
        I: IntoIterator<Item = i64>,
    {
        let now_ns = now_ns();
        for proxy_id in proxy_ids {
            self.conn.execute(
                "UPDATE evidence_latent_proxies
                    SET access_count = access_count + 1,
                        last_accessed_at_ns = ?1,
                        decay_score = 1.0
                  WHERE id = ?2
                    AND memory_layer = ?3
                    AND lifecycle_state = ?4",
                rusqlite::params![
                    now_ns,
                    proxy_id,
                    MemoryLayer::LongTerm.as_str(),
                    LifecycleState::LongTermMemory.as_str()
                ],
            )?;
        }
        Ok(())
    }

    fn transition_proxy(
        &mut self,
        current: &EvidenceBackedLatentProxy,
        next_layer: MemoryLayer,
        next_state: LifecycleState,
        reason: &str,
        envelope_section: Option<&str>,
    ) -> Result<(), StorageError> {
        let now_ns = now_ns();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE evidence_latent_proxies
                SET memory_layer = ?1,
                    lifecycle_state = ?2,
                    promotion_reason = ?3,
                    envelope_section = ?4,
                    decay_score = CASE WHEN ?2 = 'decayed' THEN 0.0 ELSE decay_score END,
                    updated_at_ns = ?5
              WHERE id = ?6",
            rusqlite::params![
                next_layer.as_str(),
                next_state.as_str(),
                reason,
                envelope_section,
                now_ns,
                current.id
            ],
        )?;
        insert_lifecycle_event_row(
            &tx,
            current.id,
            Some(&current.memory_layer),
            Some(&current.lifecycle_state),
            next_layer,
            next_state,
            reason,
            &current.evidence_refs,
            envelope_section,
            now_ns,
        )?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_lifecycle_event(
        &mut self,
        proxy_id: i64,
        from_layer: Option<&str>,
        from_state: Option<&str>,
        to_layer: MemoryLayer,
        to_state: LifecycleState,
        reason: &str,
        evidence_refs: &[StoredEvidenceRef],
        envelope_section: Option<&str>,
        created_at_ns: i64,
    ) -> Result<(), StorageError> {
        insert_lifecycle_event_row(
            &self.conn,
            proxy_id,
            from_layer,
            from_state,
            to_layer,
            to_state,
            reason,
            evidence_refs,
            envelope_section,
            created_at_ns,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_lifecycle_event_row(
    conn: &rusqlite::Connection,
    proxy_id: i64,
    from_layer: Option<&str>,
    from_state: Option<&str>,
    to_layer: MemoryLayer,
    to_state: LifecycleState,
    reason: &str,
    evidence_refs: &[StoredEvidenceRef],
    envelope_section: Option<&str>,
    created_at_ns: i64,
) -> Result<(), StorageError> {
    validate_transition_reason(reason)?;
    let evidence_refs_json = encode_evidence_refs(evidence_refs)?;
    conn.execute(
        "INSERT INTO memory_lifecycle_events (
            proxy_id, from_layer, from_state, to_layer, to_state,
            transition_reason, evidence_refs_json, envelope_section, created_at_ns
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            proxy_id,
            from_layer,
            from_state,
            to_layer.as_str(),
            to_state.as_str(),
            reason,
            evidence_refs_json,
            envelope_section,
            created_at_ns
        ],
    )?;
    Ok(())
}

fn validate_short_term_promotion_request(
    request: &ShortTermProxyPromotionRequest,
) -> Result<(), StorageError> {
    if !request.min_confidence.is_finite() || !(0.0..=1.0).contains(&request.min_confidence) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "min_confidence must be finite within [0,1], got {}",
                request.min_confidence
            ),
        });
    }
    if !request.anomaly_min_confidence.is_finite()
        || !(0.0..=1.0).contains(&request.anomaly_min_confidence)
    {
        return Err(StorageError::Corrupt {
            detail: format!(
                "anomaly_min_confidence must be finite within [0,1], got {}",
                request.anomaly_min_confidence
            ),
        });
    }
    if request.min_repeated_support < 2 {
        return Err(StorageError::Corrupt {
            detail: "min_repeated_support must be at least 2".to_string(),
        });
    }
    if request.limit == 0 {
        return Err(StorageError::Corrupt { detail: "limit must be greater than 0".to_string() });
    }
    validate_transition_reason(&request.reason)
}

fn repeated_claim_support_counts(proxies: &[EvidenceBackedLatentProxy]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for proxy in proxies {
        *counts.entry(normalized_claim_signature(&proxy.claim)).or_insert(0) += 1;
    }
    counts
}

fn promotion_rule_for_proxy(
    proxy: &EvidenceBackedLatentProxy,
    manual_proxy_ids: &HashSet<i64>,
    repeated_support_count: usize,
    request: &ShortTermProxyPromotionRequest,
) -> Option<&'static str> {
    if manual_proxy_ids.contains(&proxy.id) {
        return Some("manual_pin");
    }
    let proxy_type = proxy.proxy_type.trim().to_ascii_lowercase();
    if durable_proxy_type(&proxy_type) && proxy.confidence >= request.min_confidence {
        return Some("salience");
    }
    if anomaly_value_proxy_type(&proxy_type) && proxy.confidence >= request.anomaly_min_confidence {
        return Some("anomaly_value");
    }
    if repeated_support_count >= request.min_repeated_support {
        return Some("repeated_claim");
    }
    None
}

fn durable_proxy_type(proxy_type: &str) -> bool {
    matches!(
        proxy_type,
        "correction"
            | "policy"
            | "preference"
            | "belief"
            | "implementation_context"
            | "project_fact"
            | "user_policy"
    )
}

fn anomaly_value_proxy_type(proxy_type: &str) -> bool {
    matches!(proxy_type, "anomaly" | "conflict" | "contradiction")
}

fn cloud_safe_proxy_privacy(labels: &[SensitivityLabel]) -> bool {
    !labels.is_empty()
        && labels.iter().all(|label| {
            matches!(label, SensitivityLabel::Public | SensitivityLabel::ProjectInternal)
        })
}

fn normalized_claim_signature(claim: &str) -> String {
    claim
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.trim().is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_proxy_draft(draft: &EvidenceBackedLatentProxyDraft) -> Result<(), StorageError> {
    if draft.proxy_type.trim().is_empty() {
        return Err(StorageError::Corrupt { detail: "proxy_type cannot be empty".to_string() });
    }
    if draft.claim.trim().is_empty() {
        return Err(StorageError::Corrupt { detail: "claim cannot be empty".to_string() });
    }
    if !draft.confidence.is_finite() || !(0.0..=1.0).contains(&draft.confidence) {
        return Err(StorageError::Corrupt {
            detail: format!("confidence must be finite within [0,1], got {}", draft.confidence),
        });
    }
    if draft.evidence_refs.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "evidence-backed proxy requires at least one evidence ref".to_string(),
        });
    }
    if let Some(expires_at_ns) = draft.expires_at_ns {
        if expires_at_ns <= 0 {
            return Err(StorageError::Corrupt {
                detail: format!("expires_at_ns must be positive when set, got {expires_at_ns}"),
            });
        }
    }
    if draft.privacy_labels.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "evidence-backed proxy requires at least one privacy label".to_string(),
        });
    }
    Ok(())
}

fn validate_transition_reason(reason: &str) -> Result<(), StorageError> {
    if reason.trim().is_empty() {
        return Err(StorageError::Corrupt {
            detail: "lifecycle transition requires a non-empty reason".to_string(),
        });
    }
    Ok(())
}

fn validate_long_term_envelope_section(section: &str) -> Result<(), StorageError> {
    match section {
        "relevant_memory" => Ok(()),
        other => Err(StorageError::Corrupt {
            detail: format!(
                "long-term proxy envelope_section must be relevant_memory; got {other}"
            ),
        }),
    }
}

fn validate_semantic_envelope_section(section: &str) -> Result<(), StorageError> {
    match section {
        "stable_facts" | "user_policy" | "corrections" | "open_decisions" => Ok(()),
        other => Err(StorageError::Corrupt {
            detail: format!(
                "semantic proxy envelope_section must be stable_facts, user_policy, corrections, or open_decisions; got {other}"
            ),
        }),
    }
}

fn encode_evidence_refs(evidence_refs: &[StoredEvidenceRef]) -> Result<String, StorageError> {
    serde_json::to_string(evidence_refs)
        .map_err(|e| StorageError::Corrupt { detail: format!("evidence refs encode: {e}") })
}

fn encode_privacy_labels(privacy_labels: &[SensitivityLabel]) -> Result<String, StorageError> {
    serde_json::to_string(privacy_labels)
        .map_err(|e| StorageError::Corrupt { detail: format!("privacy labels encode: {e}") })
}

fn decode_evidence_refs(json: String) -> rusqlite::Result<Vec<StoredEvidenceRef>> {
    serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn decode_privacy_labels(json: String) -> rusqlite::Result<Vec<SensitivityLabel>> {
    serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn map_proxy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceBackedLatentProxy> {
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    let privacy_labels_json: String = row.get("privacy_labels_json")?;
    let source_trust: String = row.get("source_trust")?;
    let confidence: f64 = row.get("confidence")?;
    Ok(EvidenceBackedLatentProxy {
        id: row.get("id")?,
        episode_id: row.get("episode_id")?,
        proxy_type: row.get("proxy_type")?,
        target: row.get("target")?,
        claim: row.get("claim")?,
        scope: row.get("scope")?,
        confidence: confidence as f32,
        evidence_refs: decode_evidence_refs(evidence_refs_json)?,
        expires_at_ns: row.get("expires_at_ns")?,
        privacy_labels: decode_privacy_labels(privacy_labels_json)?,
        source_trust: ClaimSourceType::from_db(source_trust)?,
        memory_layer: row.get("memory_layer")?,
        lifecycle_state: row.get("lifecycle_state")?,
        promotion_reason: row.get("promotion_reason")?,
        envelope_section: row.get("envelope_section")?,
        supersedes_proxy_id: row.get("supersedes_proxy_id")?,
        access_count: row.get("access_count")?,
        last_accessed_at_ns: row.get("last_accessed_at_ns")?,
        decay_score: row.get::<_, f64>("decay_score")? as f32,
        created_at_ns: row.get("created_at_ns")?,
        updated_at_ns: row.get("updated_at_ns")?,
    })
}

fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryLifecycleEvent> {
    let evidence_refs_json: String = row.get("evidence_refs_json")?;
    Ok(MemoryLifecycleEvent {
        id: row.get("id")?,
        proxy_id: row.get("proxy_id")?,
        from_layer: row.get("from_layer")?,
        from_state: row.get("from_state")?,
        to_layer: row.get("to_layer")?,
        to_state: row.get("to_state")?,
        transition_reason: row.get("transition_reason")?,
        evidence_refs: decode_evidence_refs(evidence_refs_json)?,
        envelope_section: row.get("envelope_section")?,
        created_at_ns: row.get("created_at_ns")?,
    })
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
