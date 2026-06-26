//! Storage layer — SQLite source of truth.
//!
//! Discussion 0024 locks the ten axes that shape this module:
//!
//! * §A — single `rusqlite::Connection` wrapped in `Storage`; no
//!   pool. Async callers share via `Arc<Mutex<Storage>>`.
//! * §B — migration runner is a v1-simplified fork of WS 3 PR 3.1
//!   (no legacy 3-way probe).
//! * §C — two migrations at launch: `0001_initial` + `0002_runtime_jobs`.
//! * §D — G6 canonical episode schema.
//! * §E — on-disk path = `~/.soma/soma.db`.
//! * §F — timestamps = INTEGER nanoseconds.
//! * §G — episode ID = SQLite ROWID (i64).
//! * §H — typed `StorageError` with four domain legs.
//! * §I — WAL + synchronous=NORMAL + foreign_keys=ON on disk;
//!   in-memory skips WAL (SQLite rejects).
//! * §J — direct methods, no trait abstraction.

use std::path::Path;

use rusqlite::Connection;

pub mod ann_hnsw;
pub mod audit;
pub mod claims;
pub mod client_binding;
pub mod error;
pub mod learning_critic;
pub mod lifecycle;
pub mod migrations;
pub mod review_notification;
pub mod session;
pub mod task_frame;
pub mod thread_identity;

mod episode;

pub use audit::AuditReason;
pub use claims::{
    ClaimRecordDraft, ClaimSourceType, StoredClaimRecord, StoredVerificationEvent,
    VerificationEventDraft, VerificationResult, VerifierType,
};
pub use client_binding::{
    ClientBindingProofDraft, ClientBindingProofLevel, StoredClientBindingProof,
};
pub use episode::{Episode, EpisodeId, EpisodeSource, EpisodeSourceError, StoredEpisode};
pub use error::StorageError;
pub use learning_critic::{
    LearningCriticAction, LearningCriticApplyOptions, LearningCriticApplyOutcome,
    LearningCriticProposalDraft, LearningCriticProposalStatus, StoredLearningCriticProposal,
};
pub use lifecycle::{
    EvidenceBackedLatentProxy, EvidenceBackedLatentProxyDraft, LifecycleState, MemoryLayer,
    MemoryLifecycleEvent, ShortTermProxyPromotionCandidate, ShortTermProxyPromotionReport,
    ShortTermProxyPromotionRequest, StoredEvidenceRef,
};
pub use review_notification::{ReviewDigestNotificationAckDraft, StoredReviewDigestNotification};
pub use task_frame::{
    secret_like_projection_findings, task_frame_retention_cutoff_ns, SensitivityLabel,
    StoredTaskFrame, StoredTaskFrameOutcome, TaskFrameDocument, TaskFrameDraft,
    TaskFrameOutcomeDraft, TaskFrameOutcomeType, TaskFrameProjection, TaskFrameProjectionPolicy,
    TaskFrameRetentionReport, TaskFrameRetentionRequest, TaskFrameScope,
    DEFAULT_TASK_FRAME_RETENTION_DAYS,
};
pub use thread_identity::{
    StoredThreadIdentity, ThreadIdentityDraft, THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED,
};

/// Read-side shape of a `self_state` row. Storage returns this;
/// downstream context/profile paths parse `value_json` and
/// `evidence_ids_json` with the extractor's value schema.
#[derive(Debug, Clone)]
pub struct SelfStateRow {
    pub kind: String,
    pub key: String,
    pub value_json: String,
    pub evidence_ids_json: String,
    pub computed_at_ns: i64,
}

/// Read-side shape of a `context_anomalies` row. An anomaly is
/// single-episode evidence emitted by an optional quality module,
/// such as iPC free-energy.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextAnomaly {
    pub id: i64,
    pub episode_id: EpisodeId,
    pub kind: String,
    pub score: f32,
    pub evidence: Option<String>,
    pub created_at_ns: i64,
    pub resolved_at_ns: Option<i64>,
    pub resolved_by_correction_episode_id: Option<EpisodeId>,
}

/// Handle around a migrated SQLite connection. Not `Clone`; share
/// across tasks with `Arc<Mutex<Storage>>`.
///
/// D169 actual fix — `db_path` field 가 *Storage 가 어느 db 를
/// open 했는지* 를 carry. `write_persona_artifacts` 의 canonical
/// match invariant 가 이 field 로 verify (test storage 의
/// production HOME 적기 차단).
pub struct Storage {
    conn: Connection,
    db_path: std::path::PathBuf,
}

impl Storage {
    /// Read the on-disk path Storage was opened against. `:memory:`
    /// for `open_in_memory`. D169 의 canonical match invariant 가
    /// 이 read 사용.
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }
}

impl Storage {
    /// Append or replace the vector for `(episode_id, model_id)`.
    /// The `UNIQUE (episode_id, model_id)` constraint from migration
    /// 0003 makes this an upsert — callers that re-embed the same
    /// episode under the same model overwrite the prior row rather
    /// than duplicating.
    ///
    /// Stored vector format = native little-endian `f32[dim]`.
    pub fn put_vector(
        &mut self,
        episode_id: EpisodeId,
        model_id: &str,
        vector: &[f32],
    ) -> Result<(), StorageError> {
        insert_vector_row(&self.conn, episode_id, model_id, vector)
    }

    /// Read the user-profile centroid stored in `self_state` under
    /// `(kind='profile', key='user_centroid')`. Returns `None` when
    /// the row hasn't been primed yet (fresh DB or an `episode_count`
    /// of 0). Discussion 0037 §D90 / ADR 0004 §A.
    pub fn get_user_centroid(&self) -> Result<Option<(Vec<f32>, u64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM self_state WHERE kind='profile' AND key='user_centroid'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(json) = row else {
            return Ok(None);
        };
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| StorageError::Corrupt { detail: format!("centroid JSON: {e}") })?;
        let count = v.get("episode_count").and_then(|n| n.as_u64()).unwrap_or(0);
        let b64 = v.get("centroid_b64").and_then(|s| s.as_str()).unwrap_or("");
        if b64.is_empty() || count == 0 {
            return Ok(None);
        }
        use base64::prelude::{Engine, BASE64_STANDARD};
        let bytes = BASE64_STANDARD
            .decode(b64)
            .map_err(|e| StorageError::Corrupt { detail: format!("centroid b64: {e}") })?;
        if bytes.len() % 4 != 0 {
            return Err(StorageError::Corrupt {
                detail: format!("centroid bytes {}: not divisible by 4", bytes.len()),
            });
        }
        let centroid: Vec<f32> =
            bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        Ok(Some((centroid, count)))
    }

    /// Upsert the user-profile centroid. Encodes the L2-normalized
    /// vector as little-endian f32 bytes wrapped in base64 so it
    /// fits the `value_json` TEXT column without a schema change.
    /// Re-runs are idempotent — UNIQUE (kind, key) UPSERT.
    pub fn update_user_centroid(
        &mut self,
        centroid: &[f32],
        episode_count: u64,
    ) -> Result<(), StorageError> {
        use base64::prelude::{Engine, BASE64_STANDARD};
        let mut bytes = Vec::with_capacity(centroid.len() * 4);
        for v in centroid {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let b64 = BASE64_STANDARD.encode(&bytes);
        let value = serde_json::json!({
            "dim": centroid.len(),
            "centroid_b64": b64,
            "episode_count": episode_count,
        });
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO self_state (kind, key, value_json, evidence_ids, computed_at_ns)
             VALUES ('profile', 'user_centroid', ?1, '[]', ?2)
             ON CONFLICT(kind, key) DO UPDATE SET
                value_json = excluded.value_json,
                computed_at_ns = excluded.computed_at_ns",
            rusqlite::params![value.to_string(), now_ns],
        )?;
        Ok(())
    }

    /// Soft-delete an episode by id. Stamps `forgotten_at_ns` +
    /// `forgotten_reason` and pins the episode with a forgotten audit
    /// note (so a curious operator can later see which episodes were
    /// purged). Returns `true` when the row was modified, `false`
    /// when no episode with that id exists or it was already
    /// forgotten.
    pub fn forget_episode(
        &mut self,
        episode_id: EpisodeId,
        reason: &str,
    ) -> Result<bool, StorageError> {
        // R6 audit (2026-04-30) — pre-fix the UPDATE + INSERT note_pins
        // were two separate statements; a crash between them left the
        // episode forgotten without an audit pin (the bulk paths
        // `forget_by_project` / `forget_before` already wrap in a
        // transaction; the single-id path was the asymmetric outlier).
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        // D157-final — wire format 그대로 유지, typed enum 의
        // `to_wire()` 가 SoT.
        let pin_reason = crate::storage::AuditReason::Forgotten(reason.to_string()).to_wire();
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE episodes
                SET forgotten_at_ns = ?1, forgotten_reason = ?2
              WHERE id = ?3 AND forgotten_at_ns IS NULL",
            rusqlite::params![now_ns, reason, episode_id],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO note_pins (episode_id, reason, salience_at_pin, pinned_at_ns)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(episode_id) DO UPDATE SET
                reason = excluded.reason,
                salience_at_pin = excluded.salience_at_pin,
                pinned_at_ns = excluded.pinned_at_ns",
            rusqlite::params![episode_id, pin_reason, 0.0_f32, now_ns],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Soft-delete every episode matching a project name. Returns
    /// the count of newly-forgotten episodes.
    ///
    /// P1-C external-review fix — bulk path now writes the same
    /// `note_pins.reason='forgotten:<reason>'` audit trail that
    /// `forget_episode` (single) does. Pre-fix the bulk paths
    /// stamped `forgotten_at_ns` only, breaking the audit
    /// invariant migration 0010 documents. Wrapped in a single
    /// transaction so update + audit pins commit atomically; a
    /// crash mid-bulk leaves the DB in a consistent state (either
    /// none forgotten + no pins, or all forgotten + all pinned).
    pub fn forget_by_project(&mut self, project: &str, reason: &str) -> Result<u64, StorageError> {
        // D157-final — wire format 그대로 유지, typed enum 의
        // `to_wire()` 가 SoT.
        let pin_reason = crate::storage::AuditReason::Forgotten(reason.to_string()).to_wire();
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let tx = self.conn.transaction()?;
        let target_ids: Vec<EpisodeId> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM episodes WHERE project = ?1 AND forgotten_at_ns IS NULL",
            )?;
            let rows = stmt.query_map(rusqlite::params![project], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let n = target_ids.len() as u64;
        if n == 0 {
            tx.commit()?;
            return Ok(0);
        }
        for id in &target_ids {
            tx.execute(
                "UPDATE episodes SET forgotten_at_ns = ?1, forgotten_reason = ?2 WHERE id = ?3",
                rusqlite::params![now_ns, reason, id],
            )?;
            tx.execute(
                "INSERT INTO note_pins (episode_id, reason, salience_at_pin, pinned_at_ns)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(episode_id) DO UPDATE SET
                    reason = excluded.reason,
                    salience_at_pin = excluded.salience_at_pin,
                    pinned_at_ns = excluded.pinned_at_ns",
                rusqlite::params![id, pin_reason, 0.0_f32, now_ns],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Soft-delete every episode whose ts_start_ns predates the
    /// supplied threshold. Returns the count of newly-forgotten
    /// episodes.
    ///
    /// P1-C external-review fix — symmetric to `forget_by_project`.
    pub fn forget_before(
        &mut self,
        ts_threshold_ns: i64,
        reason: &str,
    ) -> Result<u64, StorageError> {
        // D157-final — wire format 그대로 유지, typed enum 의
        // `to_wire()` 가 SoT.
        let pin_reason = crate::storage::AuditReason::Forgotten(reason.to_string()).to_wire();
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let tx = self.conn.transaction()?;
        let target_ids: Vec<EpisodeId> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM episodes
                 WHERE ts_start_ns < ?1 AND forgotten_at_ns IS NULL",
            )?;
            let rows =
                stmt.query_map(rusqlite::params![ts_threshold_ns], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let n = target_ids.len() as u64;
        if n == 0 {
            tx.commit()?;
            return Ok(0);
        }
        for id in &target_ids {
            tx.execute(
                "UPDATE episodes SET forgotten_at_ns = ?1, forgotten_reason = ?2 WHERE id = ?3",
                rusqlite::params![now_ns, reason, id],
            )?;
            tx.execute(
                "INSERT INTO note_pins (episode_id, reason, salience_at_pin, pinned_at_ns)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(episode_id) DO UPDATE SET
                    reason = excluded.reason,
                    salience_at_pin = excluded.salience_at_pin,
                    pinned_at_ns = excluded.pinned_at_ns",
                rusqlite::params![id, pin_reason, 0.0_f32, now_ns],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Read `(forgotten_at_ns, forgotten_reason)` for an episode.
    /// Returns `None` when the episode is live (not forgotten).
    pub fn forgotten_status(
        &self,
        episode_id: EpisodeId,
    ) -> Result<Option<(i64, Option<String>)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<(Option<i64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT forgotten_at_ns, forgotten_reason FROM episodes WHERE id = ?1",
                rusqlite::params![episode_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((Some(ts), reason)) => Ok(Some((ts, reason))),
            _ => Ok(None),
        }
    }

    /// Total number of forgotten episodes (audit + diagnostic).
    pub fn forgotten_count(&self) -> Result<u64, StorageError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM episodes WHERE forgotten_at_ns IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// STAGE 3-C — load the persisted PaperWorkingMemory state.
    /// Returns `Some(WorkingMemoryState)` when the singleton row
    /// has been written, `None` for a fresh DB.
    #[allow(clippy::type_complexity)]
    pub fn get_working_memory_state(
        &self,
    ) -> Result<Option<(usize, Vec<f32>, Vec<f32>, i64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, Vec<u8>, Vec<u8>, i64)> = self
            .conn
            .query_row(
                "SELECT dim, c_matrix_blob, n_vector_blob, saved_at_ns
                   FROM working_memory_state WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((dim, c_bytes, n_bytes, ts)) = row else {
            return Ok(None);
        };
        let dim = dim as usize;
        if c_bytes.len() != dim * dim * 4 || n_bytes.len() != dim * 4 {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "working_memory_state shape mismatch: dim={dim}, c={} (expected {}), n={} (expected {})",
                    c_bytes.len(),
                    dim * dim * 4,
                    n_bytes.len(),
                    dim * 4
                ),
            });
        }
        let c: Vec<f32> =
            c_bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let n: Vec<f32> =
            n_bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        Ok(Some((dim, c, n, ts)))
    }

    /// STAGE 3-C — UPSERT the PaperWorkingMemory state into the
    /// singleton row.
    pub fn save_working_memory_state(
        &mut self,
        dim: usize,
        c: &[f32],
        n: &[f32],
    ) -> Result<(), StorageError> {
        if c.len() != dim * dim || n.len() != dim {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_working_memory_state shape: dim={dim} but c={} n={}",
                    c.len(),
                    n.len()
                ),
            });
        }
        let mut c_bytes = Vec::with_capacity(c.len() * 4);
        for v in c {
            c_bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut n_bytes = Vec::with_capacity(n.len() * 4);
        for v in n {
            n_bytes.extend_from_slice(&v.to_le_bytes());
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO working_memory_state (id, dim, c_matrix_blob, n_vector_blob, saved_at_ns)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                dim = excluded.dim,
                c_matrix_blob = excluded.c_matrix_blob,
                n_vector_blob = excluded.n_vector_blob,
                saved_at_ns = excluded.saved_at_ns",
            rusqlite::params![dim as i64, c_bytes, n_bytes, now_ns],
        )?;
        Ok(())
    }

    /// v1.2 chunk 1.3 (ADR 0008 §D4) — load the trainable mLSTM
    /// projection weights. Returns `Some((dim, w_q, w_k, w_v,
    /// train_steps, saved_at_ns))` when the singleton row exists,
    /// `None` for a fresh DB. State is in `working_memory_state`
    /// (migration 0011), separated by write-rate (state per ingest,
    /// weights per slow_loop train cycle).
    #[allow(clippy::type_complexity)]
    pub fn get_working_memory_weights(
        &self,
    ) -> Result<Option<(usize, Vec<f32>, Vec<f32>, Vec<f32>, u64, i64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, Vec<u8>, Vec<u8>, Vec<u8>, i64, i64)> = self
            .conn
            .query_row(
                "SELECT dim, w_q_blob, w_k_blob, w_v_blob, train_steps, saved_at_ns
                   FROM working_memory_weights WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?;
        let Some((dim, q_bytes, k_bytes, v_bytes, steps, ts)) = row else {
            return Ok(None);
        };
        let dim = dim as usize;
        let expected = dim * dim * 4;
        if q_bytes.len() != expected || k_bytes.len() != expected || v_bytes.len() != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "working_memory_weights shape mismatch: dim={dim}, w_q={} w_k={} w_v={} (expected {expected})",
                    q_bytes.len(),
                    k_bytes.len(),
                    v_bytes.len()
                ),
            });
        }
        let decode = |bytes: Vec<u8>| -> Vec<f32> {
            bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        };
        let q = decode(q_bytes);
        let k = decode(k_bytes);
        let v = decode(v_bytes);
        // P1-1 (audit fix) — Layer 3 NaN guard. Layer 2 (save) refuses
        // non-finite weights, but external SQLite corruption could
        // bypass that. Refuse on read so the hot path never sees NaN.
        finite_check_blob("working_memory_weights.w_q", &q)?;
        finite_check_blob("working_memory_weights.w_k", &k)?;
        finite_check_blob("working_memory_weights.w_v", &v)?;
        Ok(Some((dim, q, k, v, steps as u64, ts)))
    }

    /// v1.2 chunk 1.3 — UPSERT the trainable mLSTM weights into
    /// the singleton row. `train_steps` is monotonic — caller
    /// passes the *current* count after the slow_loop train cycle.
    ///
    /// Production-safety NaN guard: any NaN/inf in the weights is
    /// rejected with `StorageError::Corrupt`. A divergent training
    /// batch never poisons the persisted state — the cell stays at
    /// its last-known-good weights, and the caller can log + retry.
    pub fn save_working_memory_weights(
        &mut self,
        dim: usize,
        w_q: &[f32],
        w_k: &[f32],
        w_v: &[f32],
        train_steps: u64,
    ) -> Result<(), StorageError> {
        let expected = dim * dim;
        if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_working_memory_weights shape: dim={dim} but w_q={} w_k={} w_v={}",
                    w_q.len(),
                    w_k.len(),
                    w_v.len()
                ),
            });
        }
        for (name, w) in [("w_q", w_q), ("w_k", w_k), ("w_v", w_v)] {
            if let Some((idx, bad)) = w.iter().enumerate().find(|(_, v)| !v.is_finite()) {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "save_working_memory_weights: {name}[{idx}] = {bad} (non-finite); refusing to persist divergent training state"
                    ),
                });
            }
        }
        let encode = |w: &[f32]| -> Vec<u8> {
            let mut out = Vec::with_capacity(w.len() * 4);
            for v in w {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        };
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO working_memory_weights
                 (id, dim, w_q_blob, w_k_blob, w_v_blob, train_steps, saved_at_ns)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                dim = excluded.dim,
                w_q_blob = excluded.w_q_blob,
                w_k_blob = excluded.w_k_blob,
                w_v_blob = excluded.w_v_blob,
                train_steps = excluded.train_steps,
                saved_at_ns = excluded.saved_at_ns",
            rusqlite::params![
                dim as i64,
                encode(w_q),
                encode(w_k),
                encode(w_v),
                train_steps as i64,
                now_ns
            ],
        )?;
        Ok(())
    }

    /// v1.2 chunk 2.3 (ADR 0009 §D4) — load the ANIL head weights.
    /// Returns `Some((d_emb, w_head_flat, b_head, projects, train_
    /// steps, saved_at_ns))` when persisted. `None` for fresh DB
    /// or `cognitive-train` feature off (the feature gate doesn't
    /// affect storage shape — same row works for both).
    #[allow(clippy::type_complexity)]
    pub fn get_anil_head_weights(
        &self,
    ) -> Result<Option<(usize, Vec<f32>, Vec<f32>, Vec<String>, u64, i64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, i64, Vec<u8>, Vec<u8>, String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT d_emb, num_classes, w_head_blob, b_head_blob, projects_json,
                        train_steps, saved_at_ns
                   FROM anil_head_weights WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((d_emb, num_classes, w_bytes, b_bytes, projects_json, steps, ts)) = row else {
            return Ok(None);
        };
        let d_emb = d_emb as usize;
        let num_classes = num_classes as usize;
        let expected_w = d_emb * num_classes * 4;
        let expected_b = num_classes * 4;
        if w_bytes.len() != expected_w || b_bytes.len() != expected_b {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "anil_head_weights shape mismatch: d_emb={d_emb} K={num_classes}, w={} (expected {expected_w}), b={} (expected {expected_b})",
                    w_bytes.len(),
                    b_bytes.len()
                ),
            });
        }
        let projects: Vec<String> = serde_json::from_str(&projects_json)
            .map_err(|e| StorageError::Corrupt { detail: format!("projects_json: {e}") })?;
        if projects.len() != num_classes {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "anil_head_weights projects.len()={} != num_classes={num_classes}",
                    projects.len()
                ),
            });
        }
        let decode = |bytes: Vec<u8>| -> Vec<f32> {
            bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        };
        let w = decode(w_bytes);
        let b = decode(b_bytes);
        // P1-1 (audit fix) — Layer 3 NaN guard.
        finite_check_blob("anil_head_weights.w_head", &w)?;
        finite_check_blob("anil_head_weights.b_head", &b)?;
        Ok(Some((d_emb, w, b, projects, steps as u64, ts)))
    }

    /// v1.2 chunk 2.3 — UPSERT the ANIL head weights into the
    /// singleton row. Layer 2 NaN guard symmetric to working_memory_
    /// weights — divergent batch refused, last-known-good preserved.
    pub fn save_anil_head_weights(
        &mut self,
        d_emb: usize,
        w_head: &[f32],
        b_head: &[f32],
        projects: &[String],
        train_steps: u64,
    ) -> Result<(), StorageError> {
        let k = projects.len();
        if k == 0 {
            return Err(StorageError::Corrupt {
                detail: "save_anil_head_weights: K = 0 (empty projects)".into(),
            });
        }
        if w_head.len() != k * d_emb || b_head.len() != k {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_anil_head_weights shape: d_emb={d_emb} K={k} but w_head={} b_head={}",
                    w_head.len(),
                    b_head.len()
                ),
            });
        }
        if let Some((idx, bad)) = w_head.iter().enumerate().find(|(_, v)| !v.is_finite()) {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_anil_head_weights: w_head[{idx}] = {bad} (non-finite); refusing divergent training state"
                ),
            });
        }
        if let Some((idx, bad)) = b_head.iter().enumerate().find(|(_, v)| !v.is_finite()) {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_anil_head_weights: b_head[{idx}] = {bad} (non-finite); refusing divergent training state"
                ),
            });
        }

        let encode = |w: &[f32]| -> Vec<u8> {
            let mut out = Vec::with_capacity(w.len() * 4);
            for v in w {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        };
        let projects_json = serde_json::to_string(projects)
            .map_err(|e| StorageError::Corrupt { detail: format!("projects encode: {e}") })?;
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO anil_head_weights
                 (id, d_emb, num_classes, w_head_blob, b_head_blob, projects_json,
                  train_steps, saved_at_ns)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                d_emb = excluded.d_emb,
                num_classes = excluded.num_classes,
                w_head_blob = excluded.w_head_blob,
                b_head_blob = excluded.b_head_blob,
                projects_json = excluded.projects_json,
                train_steps = excluded.train_steps,
                saved_at_ns = excluded.saved_at_ns",
            rusqlite::params![
                d_emb as i64,
                k as i64,
                encode(w_head),
                encode(b_head),
                projects_json,
                train_steps as i64,
                now_ns
            ],
        )?;
        Ok(())
    }

    /// v1.2 chunk 3.3 (ADR 0010 §D5) — load all PC predictor rows
    /// keyed by layer_idx. Returns `(layer_idx, d_in, d_out, w_flat,
    /// train_steps, saved_at_ns)` per layer. Empty when no row.
    #[allow(clippy::type_complexity)]
    pub fn get_pc_predictor_layers(
        &self,
    ) -> Result<Vec<(usize, usize, usize, Vec<f32>, u64, i64)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT layer_idx, d_in, d_out, w_blob, train_steps, saved_at_ns
             FROM pc_predictor_weights ORDER BY layer_idx ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let lid: i64 = r.get(0)?;
            let d_in: i64 = r.get(1)?;
            let d_out: i64 = r.get(2)?;
            let w_bytes: Vec<u8> = r.get(3)?;
            let steps: i64 = r.get(4)?;
            let ts: i64 = r.get(5)?;
            Ok((lid, d_in, d_out, w_bytes, steps, ts))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (lid, d_in, d_out, bytes, steps, ts) = r?;
            let d_in_u = d_in as usize;
            let d_out_u = d_out as usize;
            let expected = d_in_u * d_out_u * 4;
            if bytes.len() != expected {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "pc_predictor_weights layer {lid} shape mismatch: got {} expected {expected}",
                        bytes.len()
                    ),
                });
            }
            let w: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            // P1-1 (audit fix) — Layer 3 NaN guard, per-layer.
            finite_check_blob(&format!("pc_predictor_weights.layer_{lid}"), &w)?;
            out.push((lid as usize, d_in_u, d_out_u, w, steps as u64, ts));
        }
        Ok(out)
    }

    /// v1.2 chunk 3.3 — UPSERT one PC predictor layer. Layer 2 NaN
    /// guard symmetric to working_memory_weights / anil_head_weights.
    pub fn save_pc_predictor_layer(
        &mut self,
        layer_idx: usize,
        d_in: usize,
        d_out: usize,
        w: &[f32],
        train_steps: u64,
    ) -> Result<(), StorageError> {
        let expected = d_in * d_out;
        if w.len() != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_pc_predictor_layer shape: layer={layer_idx} d_in={d_in} d_out={d_out} but w={}",
                    w.len()
                ),
            });
        }
        if let Some((idx, bad)) = w.iter().enumerate().find(|(_, v)| !v.is_finite()) {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_pc_predictor_layer layer {layer_idx}: w[{idx}] = {bad} (non-finite)"
                ),
            });
        }
        let mut bytes = Vec::with_capacity(w.len() * 4);
        for v in w {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO pc_predictor_weights
                 (layer_idx, d_in, d_out, w_blob, train_steps, saved_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(layer_idx) DO UPDATE SET
                d_in = excluded.d_in,
                d_out = excluded.d_out,
                w_blob = excluded.w_blob,
                train_steps = excluded.train_steps,
                saved_at_ns = excluded.saved_at_ns",
            rusqlite::params![
                layer_idx as i64,
                d_in as i64,
                d_out as i64,
                bytes,
                train_steps as i64,
                now_ns
            ],
        )?;
        Ok(())
    }

    /// v1.2 chunk 4.3 (ADR 0011 §D4) — load TrainableHopfield Q/K/V.
    #[allow(clippy::type_complexity)]
    pub fn get_hopfield_weights(
        &self,
    ) -> Result<Option<(usize, usize, Vec<f32>, Vec<f32>, Vec<f32>, u64, i64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, i64, Vec<u8>, Vec<u8>, Vec<u8>, i64, i64)> = self
            .conn
            .query_row(
                "SELECT d_emb, num_heads, w_q_blob, w_k_blob, w_v_blob,
                        train_steps, saved_at_ns
                   FROM hopfield_weights WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((d_emb, num_heads, qb, kb, vb, steps, ts)) = row else {
            return Ok(None);
        };
        let d_emb = d_emb as usize;
        let num_heads = num_heads as usize;
        let expected = d_emb * d_emb * 4;
        if qb.len() != expected || kb.len() != expected || vb.len() != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "hopfield_weights shape mismatch: d_emb={d_emb}, q={} k={} v={} (expected {expected})",
                    qb.len(),
                    kb.len(),
                    vb.len()
                ),
            });
        }
        let decode = |bytes: Vec<u8>| -> Vec<f32> {
            bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        };
        let q = decode(qb);
        let k = decode(kb);
        let v = decode(vb);
        // P1-1 (audit fix) — Layer 3 NaN guard.
        finite_check_blob("hopfield_weights.w_q", &q)?;
        finite_check_blob("hopfield_weights.w_k", &k)?;
        finite_check_blob("hopfield_weights.w_v", &v)?;
        Ok(Some((d_emb, num_heads, q, k, v, steps as u64, ts)))
    }

    /// v1.2 chunk 4.3 — UPSERT trainable Hopfield Q/K/V.
    pub fn save_hopfield_weights(
        &mut self,
        d_emb: usize,
        num_heads: usize,
        w_q: &[f32],
        w_k: &[f32],
        w_v: &[f32],
        train_steps: u64,
    ) -> Result<(), StorageError> {
        let expected = d_emb * d_emb;
        if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "save_hopfield_weights shape: d_emb={d_emb} but q={} k={} v={}",
                    w_q.len(),
                    w_k.len(),
                    w_v.len()
                ),
            });
        }
        for (name, slice) in [("w_q", w_q), ("w_k", w_k), ("w_v", w_v)] {
            if let Some((idx, bad)) = slice.iter().enumerate().find(|(_, v)| !v.is_finite()) {
                return Err(StorageError::Corrupt {
                    detail: format!("save_hopfield_weights: {name}[{idx}] = {bad} (non-finite)"),
                });
            }
        }
        let encode = |w: &[f32]| -> Vec<u8> {
            let mut out = Vec::with_capacity(w.len() * 4);
            for v in w {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        };
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO hopfield_weights
                 (id, d_emb, num_heads, w_q_blob, w_k_blob, w_v_blob,
                  train_steps, saved_at_ns)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                d_emb = excluded.d_emb,
                num_heads = excluded.num_heads,
                w_q_blob = excluded.w_q_blob,
                w_k_blob = excluded.w_k_blob,
                w_v_blob = excluded.w_v_blob,
                train_steps = excluded.train_steps,
                saved_at_ns = excluded.saved_at_ns",
            rusqlite::params![
                d_emb as i64,
                num_heads as i64,
                encode(w_q),
                encode(w_k),
                encode(w_v),
                train_steps as i64,
                now_ns
            ],
        )?;
        Ok(())
    }

    /// Read the narrative paragraph synthesized by slow_loop.
    /// Returns `(paragraph_md, synthesized_at_ns, kind)` where
    /// `kind` is `"rule"` (template-based) or `"llm"` (future v1.2
    /// LLM-assisted). Empty paragraph means the slow_loop hasn't
    /// fired a synthesis yet.
    pub fn get_narrative(&self) -> Result<Option<(String, i64, String)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM self_state WHERE kind='narrative' AND key='paragraph_md'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(json) = row else {
            return Ok(None);
        };
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| StorageError::Corrupt { detail: format!("narrative JSON: {e}") })?;
        let paragraph = v.get("paragraph_md").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let ts = v.get("synthesized_at_ns").and_then(|n| n.as_i64()).unwrap_or(0);
        let kind = v.get("kind").and_then(|s| s.as_str()).unwrap_or("rule").to_string();
        Ok(Some((paragraph, ts, kind)))
    }

    /// Update the narrative paragraph row. UPSERT — `(kind='narrative',
    /// key='paragraph_md')` is the canonical singleton.
    pub fn update_narrative(&mut self, paragraph_md: &str, kind: &str) -> Result<(), StorageError> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let value = serde_json::json!({
            "paragraph_md": paragraph_md,
            "synthesized_at_ns": now_ns,
            "kind": kind,
        });
        self.conn.execute(
            "INSERT INTO self_state (kind, key, value_json, evidence_ids, computed_at_ns)
             VALUES ('narrative', 'paragraph_md', ?1, '[]', ?2)
             ON CONFLICT(kind, key) DO UPDATE SET
                value_json = excluded.value_json,
                computed_at_ns = excluded.computed_at_ns",
            rusqlite::params![value.to_string(), now_ns],
        )?;
        Ok(())
    }

    /// D93 §E — set the compression metadata on an episode. Used by
    /// the slow_loop's repeated-pattern collapse (HEN/Santos 2025
    /// insight). `summary_count` becomes the number of real ingests
    /// the row represents; `summary_signature` is a SHA256 of
    /// `(command, project, exit_code)` so future passes can group by
    /// signature in O(N log N).
    pub fn update_summary_metadata(
        &mut self,
        episode_id: EpisodeId,
        summary_count: u64,
        summary_signature: Option<&str>,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE episodes
                SET summary_count = ?1,
                    summary_signature = ?2
              WHERE id = ?3",
            rusqlite::params![summary_count as i64, summary_signature, episode_id],
        )?;
        Ok(())
    }

    /// Read summary metadata for one episode. Returns
    /// `(summary_count, summary_signature)`.
    pub fn summary_metadata(
        &self,
        episode_id: EpisodeId,
    ) -> Result<Option<(u64, Option<String>)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT summary_count, summary_signature FROM episodes WHERE id = ?1",
                rusqlite::params![episode_id],
                |r| {
                    let count: i64 = r.get(0)?;
                    let sig: Option<String> = r.get(1)?;
                    Ok((count as u64, sig))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Group episodes by their `summary_signature` (skipping NULL).
    /// Returns `(signature, Vec<EpisodeId>)` pairs ordered by ID
    /// within each signature group.
    pub fn episodes_by_signature(&self) -> Result<Vec<(String, Vec<EpisodeId>)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT summary_signature, id FROM episodes
              WHERE summary_signature IS NOT NULL
              ORDER BY summary_signature, id",
        )?;
        let rows = stmt.query_map([], |r| {
            let sig: String = r.get(0)?;
            let id: i64 = r.get(1)?;
            Ok((sig, id))
        })?;

        let mut grouped: std::collections::BTreeMap<String, Vec<EpisodeId>> =
            std::collections::BTreeMap::new();
        for r in rows {
            let (sig, id) = r?;
            grouped.entry(sig).or_default().push(id);
        }
        Ok(grouped.into_iter().collect())
    }

    /// D92 §C — insert an undirected episode-similarity edge.
    /// Caller passes the *unordered* pair; this method canonicalizes
    /// to (min, max) so the schema's `CHECK (src_id < dst_id)`
    /// invariant holds. UPSERT keeps the latest similarity.
    pub fn upsert_edge(
        &mut self,
        a: EpisodeId,
        b: EpisodeId,
        similarity: f32,
    ) -> Result<(), StorageError> {
        if a == b {
            return Ok(());
        }
        // R6 audit (2026-04-30) — guard the cosine similarity range
        // [-1, 1] at insert time. SQLite's CHECK constraint can't be
        // added retroactively (no ALTER TABLE ADD CHECK in SQLite),
        // so the invariant is enforced application-side. NaN / Inf /
        // out-of-range values would otherwise persist silently from
        // a buggy embedder + corrupt the multi-hop graph.
        if !similarity.is_finite() || !(-1.0..=1.0).contains(&similarity) {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "episode_edges similarity {similarity} out of range [-1, 1] \
                     (a={a}, b={b}); rejecting insert"
                ),
            });
        }
        let (src, dst) = if a < b { (a, b) } else { (b, a) };
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO episode_edges (src_id, dst_id, similarity, created_at_ns)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(src_id, dst_id) DO UPDATE SET
                similarity = excluded.similarity,
                created_at_ns = excluded.created_at_ns",
            rusqlite::params![src, dst, similarity, now_ns],
        )?;
        Ok(())
    }

    /// All neighbors (undirected) of an episode, returning
    /// `(other_id, similarity)`. Sorted by similarity DESC.
    pub fn edges_for(
        &self,
        episode_id: EpisodeId,
        limit: usize,
    ) -> Result<Vec<(EpisodeId, f32)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT dst_id AS other, similarity FROM episode_edges WHERE src_id = ?1
             UNION ALL
             SELECT src_id AS other, similarity FROM episode_edges WHERE dst_id = ?1
             ORDER BY similarity DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![episode_id, limit as i64], |r| {
            let other: i64 = r.get(0)?;
            let sim: f32 = r.get(1)?;
            Ok((other, sim))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Total edge count — diagnostic / test helper.
    pub fn edge_count(&self) -> Result<u64, StorageError> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM episode_edges", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// v1.2 chunk 4.4 — pull every `(src_id, dst_id, similarity)`
    /// edge for the trainable Hopfield's contrastive batch. Order
    /// is the natural insertion order; the slow_loop shuffles
    /// before iterating. Returns an empty Vec when no edges exist
    /// (fresh DB / D92 graph not yet populated).
    pub fn all_edges(&self) -> Result<Vec<(EpisodeId, EpisodeId, f32)>, StorageError> {
        let mut stmt = self.conn.prepare("SELECT src_id, dst_id, similarity FROM episode_edges")?;
        let rows = stmt.query_map([], |r| {
            let src: i64 = r.get(0)?;
            let dst: i64 = r.get(1)?;
            let sim: f32 = r.get(2)?;
            Ok((src, dst, sim))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// D91 §B — pin an episode to the Note Block. Idempotent UPSERT
    /// on `episode_id` PK; re-pinning replaces the reason/timestamp.
    pub fn pin_episode(
        &mut self,
        episode_id: EpisodeId,
        reason: &str,
        salience_at_pin: f32,
    ) -> Result<(), StorageError> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO note_pins (episode_id, pinned_at_ns, reason, salience_at_pin)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(episode_id) DO UPDATE SET
                pinned_at_ns = excluded.pinned_at_ns,
                reason = excluded.reason,
                salience_at_pin = excluded.salience_at_pin",
            rusqlite::params![episode_id, now_ns, reason, salience_at_pin],
        )?;
        Ok(())
    }

    /// Remove an episode from the Note Block. Idempotent (`OK` even
    /// if it wasn't pinned).
    pub fn unpin_episode(&mut self, episode_id: EpisodeId) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM note_pins WHERE episode_id = ?1",
            rusqlite::params![episode_id],
        )?;
        Ok(())
    }

    /// D164 close — note-pin activity grouped by local-day. Each
    /// row is `(day_unix_secs, pin_count)`; the day boundary is
    /// computed at SQL time using `strftime('%s', date(...))` so
    /// the bucket aligns with the operator's local timezone.
    /// Returns rows oldest-first across the last `days` days
    /// (default 30 from the dashboard's `/api/memory/timeline`).
    pub fn note_pin_timeline_days(&self, days: u32) -> Result<Vec<(i64, u64)>, StorageError> {
        // SQLite stores `pinned_at_ns` in nanoseconds; convert to
        // unix seconds in-query to feed into `date()`. The cutoff
        // is `now - days * 86400` so the window is rolling.
        let mut stmt = self.conn.prepare(
            "SELECT
                CAST(strftime('%s', date(pinned_at_ns / 1000000000, 'unixepoch', 'localtime')) AS INTEGER) AS day_ts,
                COUNT(*) AS count
             FROM note_pins
             WHERE pinned_at_ns >= ?1
             GROUP BY day_ts
             ORDER BY day_ts ASC",
        )?;
        let cutoff_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0)
            - (i64::from(days) * 86_400 * 1_000_000_000);
        let rows = stmt.query_map(rusqlite::params![cutoff_ns], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Check whether an episode is currently pinned.
    pub fn is_pinned(&self, episode_id: EpisodeId) -> Result<bool, StorageError> {
        use rusqlite::OptionalExtension;
        let pinned: Option<i64> = self
            .conn
            .query_row(
                "SELECT episode_id FROM note_pins WHERE episode_id = ?1",
                rusqlite::params![episode_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(pinned.is_some())
    }

    /// Bulk read all pinned episode IDs (sorted by pinned_at_ns DESC).
    pub fn pinned_episode_ids(&self) -> Result<Vec<EpisodeId>, StorageError> {
        let mut stmt =
            self.conn.prepare("SELECT episode_id FROM note_pins ORDER BY pinned_at_ns DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// D91 §B — increment `(access_count, last_access_ns)` for a
    /// recall hit. Slow-loop and context assembly call this so the
    /// Ebbinghaus formula's "repeated access counteracts age"
    /// invariant holds.
    pub fn touch_episode(&mut self, episode_id: EpisodeId) -> Result<(), StorageError> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "UPDATE episodes
                SET access_count = access_count + 1,
                    last_access_ns = ?1
              WHERE id = ?2",
            rusqlite::params![now_ns, episode_id],
        )?;
        Ok(())
    }

    /// Read `(access_count, last_access_ns, ts_start_ns)` for a
    /// single episode — used by the recall path to compute the
    /// Ebbinghaus decay factor without re-loading the full row.
    pub fn access_metadata(
        &self,
        episode_id: EpisodeId,
    ) -> Result<Option<(u64, i64, i64)>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT access_count, last_access_ns, ts_start_ns
                   FROM episodes WHERE id = ?1",
                rusqlite::params![episode_id],
                |r| {
                    let access: i64 = r.get(0)?;
                    let last: i64 = r.get(1)?;
                    let ts: i64 = r.get(2)?;
                    Ok((access as u64, last, ts))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Upsert a self-model fact. `(kind, key)` is the unique tuple
    /// — re-running the same extractor overwrites prior values with
    /// fresh evidence. Discussion 0030 §A + §H.
    pub fn upsert_self_fact(
        &mut self,
        kind: &str,
        key: &str,
        value_json: &str,
        evidence_ids: &[EpisodeId],
    ) -> Result<(), StorageError> {
        let evidence = serde_json::to_string(evidence_ids).unwrap_or_else(|_| "[]".to_string());
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO self_state (kind, key, value_json, evidence_ids, computed_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(kind, key) DO UPDATE SET
                value_json = excluded.value_json,
                evidence_ids = excluded.evidence_ids,
                computed_at_ns = excluded.computed_at_ns",
            rusqlite::params![kind, key, value_json, evidence, now_ns],
        )?;
        Ok(())
    }

    /// Read all `(kind, key, value_json, evidence_ids,
    /// computed_at_ns)` rows ordered by kind then key. Discussion
    /// 0030 §H — consumers include `soma profile`, ContextEnvelope
    /// quality paths, and legacy debug profile payloads.
    pub fn read_all_self_facts(&self) -> Result<Vec<SelfStateRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, key, value_json, evidence_ids, computed_at_ns
             FROM self_state ORDER BY kind ASC, key ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SelfStateRow {
                kind: r.get(0)?,
                key: r.get(1)?,
                value_json: r.get(2)?,
                evidence_ids_json: r.get(3)?,
                computed_at_ns: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Iterate every `(episode_id, vector)` pair for a given model.
    /// Callers use this to rebuild the HNSW index on open.
    ///
    /// D91 §P2 — verify each row's BLOB length against the
    /// `dim` column. A pre-D91 caller could store a vector whose
    /// `dim * 4 != bytes.len()` (no DB constraint enforces this);
    /// the silent-truncate path of `le_bytes_to_f32_vec` would
    /// then return a shorter vector that the HNSW index would
    /// crash on later. Surface as `Corrupt` instead.
    pub fn vectors_for_model(
        &self,
        model_id: &str,
    ) -> Result<Vec<(EpisodeId, Vec<f32>)>, StorageError> {
        // D-forget — JOIN on episodes so forgotten rows are dropped
        // from the recall / HNSW build path.
        let mut stmt = self.conn.prepare(
            "SELECT v.episode_id, v.dim, v.vector
               FROM episode_vectors v
               JOIN episodes e ON e.id = v.episode_id
              WHERE v.model_id = ?1 AND e.forgotten_at_ns IS NULL",
        )?;
        let rows = stmt.query_map(rusqlite::params![model_id], |r| {
            let id: EpisodeId = r.get(0)?;
            let dim: i64 = r.get(1)?;
            let bytes: Vec<u8> = r.get(2)?;
            Ok((id, dim as usize, bytes))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, dim, bytes) = r?;
            if bytes.len() != dim * 4 {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "episode_vectors row episode_id={id}, model_id={model_id}: dim={dim} but bytes.len()={} (expected {})",
                        bytes.len(),
                        dim * 4
                    ),
                });
            }
            let v = le_bytes_to_f32_vec(&bytes);
            out.push((id, v));
        }
        Ok(out)
    }

    /// `(episodes_total, pending_runtime_jobs)` for resident Status
    /// snapshots. Single-query per counter, no transaction — readers
    /// may observe a mildly inconsistent pair if a writer commits
    /// between the two counts, which is fine for the Status surface
    /// (advisory display, not correctness-bearing).
    pub fn counters(&self) -> Result<(u64, u64), StorageError> {
        let episodes: i64 =
            self.conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))?;
        let jobs: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM runtime_jobs WHERE attempts < max_attempts",
            [],
            |r| r.get(0),
        )?;
        Ok((episodes as u64, jobs as u64))
    }

    // -----------------------------------------------------------------
    // D84 — belief candidates CRUD (migration 0016).
    //
    // Typed-relationship overlay on episode_edges. Storage exposes
    // three methods:
    //   * `insert_belief_candidate` — `INSERT OR IGNORE` semantics so
    //     a re-seed against the same window is idempotent.
    //   * `get_belief_candidates_for_episode` — both directions
    //     (a OR b matches), live rows only (forgotten_at_ns IS NULL).
    //   * `recent_contradictions` — operator-facing slice for the
    //     slow_loop's unresolved contradiction surface.
    //   * `resolve_belief_candidate_with_correction` — persist that
    //     a correction episode resolved a candidate.
    //
    // D46 invariant — INSERTs use `named_params!` and reads use
    // `row.get("col_name")` so column-order drift can't silently
    // misroute the wrong field. (`recent_contradictions` shares
    // `read_belief_row` with the by-episode reader to keep both call
    // sites consistent.)
    // -----------------------------------------------------------------

    /// Insert a belief candidate. The `UNIQUE(episode_a_id,
    /// episode_b_id, kind)` constraint deduplicates idempotently —
    /// returns `Some(rowid)` on actual insert, `None` when the row
    /// already existed. Caller must canonicalize `(episode_a_id <
    /// episode_b_id)` before calling; the storage layer does not
    /// re-order pairs because the seeder needs to choose evidence
    /// based on the original (ts-ordered) pair direction.
    pub fn insert_belief_candidate(
        &mut self,
        episode_a_id: i64,
        episode_b_id: i64,
        kind: crate::memory::beliefs::BeliefKind,
        score: f32,
        evidence: Option<&str>,
    ) -> Result<Option<i64>, StorageError> {
        // Score sanity guard symmetric to `upsert_edge`. Cosine on
        // unit-normalized vectors lives in [-1, 1]; NaN / Inf must
        // not reach SQLite (CHECK constraints can't be added
        // retroactively, so the guard is application-side).
        if !score.is_finite() || !(-1.0_f32..=1.0_f32).contains(&score) {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "belief_candidates score {score} out of range [-1, 1] \
                     (a={episode_a_id}, b={episode_b_id}); rejecting insert"
                ),
            });
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let kind_str = kind.to_string();
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO belief_candidates
                 (episode_a_id, episode_b_id, kind, score, evidence, created_at_ns)
             VALUES
                 (:a, :b, :kind, :score, :evidence, :ts)",
            rusqlite::named_params! {
                ":a": episode_a_id,
                ":b": episode_b_id,
                ":kind": kind_str,
                ":score": score,
                ":evidence": evidence,
                ":ts": now_ns,
            },
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// Read all live belief candidates touching `episode_id` (either
    /// `episode_a_id` or `episode_b_id`). Forgotten rows
    /// (`forgotten_at_ns IS NOT NULL`) are filtered out so the
    /// recall path never resurfaces tombstoned beliefs. Sorted by
    /// `created_at_ns DESC` so callers see the freshest evidence
    /// first.
    pub fn get_belief_candidates_for_episode(
        &self,
        episode_id: i64,
    ) -> Result<Vec<crate::memory::beliefs::BeliefCandidate>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, episode_a_id, episode_b_id, kind, score, evidence,
                    created_at_ns, forgotten_at_ns, resolved_at_ns,
                    resolved_by_correction_episode_id
               FROM belief_candidates
              WHERE (episode_a_id = :id OR episode_b_id = :id)
                AND forgotten_at_ns IS NULL
                AND resolved_at_ns IS NULL
              ORDER BY created_at_ns DESC",
        )?;
        let rows = stmt.query_map(rusqlite::named_params! { ":id": episode_id }, map_belief_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// D164.5 close — read recent belief candidates of a given
    /// kind. `recent_contradictions` 은 wrapper. Generic 한 path
    /// 라 dashboard 의 corroborates panel 도 같이 read 가능.
    pub fn recent_beliefs_of_kind(
        &self,
        kind: crate::memory::beliefs::BeliefKind,
        limit: usize,
    ) -> Result<Vec<crate::memory::beliefs::BeliefCandidate>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let kind_str = kind.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT id, episode_a_id, episode_b_id, kind, score, evidence,
                    created_at_ns, forgotten_at_ns, resolved_at_ns,
                    resolved_by_correction_episode_id
               FROM belief_candidates
              WHERE kind = :kind
                AND forgotten_at_ns IS NULL
                AND resolved_at_ns IS NULL
              ORDER BY created_at_ns DESC
              LIMIT :limit",
        )?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":kind": kind_str,
                ":limit": limit as i64,
            },
            map_belief_row,
        )?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Read the `limit` most-recent live `Contradicts` rows, newest
    /// first. Used by the operator-facing slow_loop surface that
    /// asks "what unreviewed contradictions has SOMA accumulated?"
    /// `limit == 0` returns an empty vec.
    pub fn recent_contradictions(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::memory::beliefs::BeliefCandidate>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // The wire string for the `kind` column comes from
        // `BeliefKind::Display` so any future variant rename can't
        // silently desync this query from the typed enum.
        let kind = crate::memory::beliefs::BeliefKind::Contradicts.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT id, episode_a_id, episode_b_id, kind, score, evidence,
                    created_at_ns, forgotten_at_ns, resolved_at_ns,
                    resolved_by_correction_episode_id
               FROM belief_candidates
              WHERE kind = :kind
                AND forgotten_at_ns IS NULL
                AND resolved_at_ns IS NULL
              ORDER BY created_at_ns DESC
              LIMIT :limit",
        )?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":kind": kind,
                ":limit": limit as i64,
            },
            map_belief_row,
        )?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Upsert a single-episode context anomaly. The `(episode_id,
    /// kind)` uniqueness constraint makes repeated renders/ingests
    /// idempotent while allowing a newer score/evidence string to
    /// refresh an unresolved anomaly.
    pub fn upsert_context_anomaly(
        &mut self,
        episode_id: EpisodeId,
        kind: &str,
        score: f32,
        evidence: Option<&str>,
    ) -> Result<i64, StorageError> {
        if kind.trim().is_empty() {
            return Err(StorageError::Corrupt {
                detail: "context_anomalies kind must not be empty".to_string(),
            });
        }
        if !score.is_finite() || score < 0.0 {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "context_anomalies score {score} out of range [0, +inf) \
                     (episode_id={episode_id}, kind={kind}); rejecting upsert"
                ),
            });
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO context_anomalies
                 (episode_id, kind, score, evidence, created_at_ns)
             VALUES
                 (:episode_id, :kind, :score, :evidence, :created_at_ns)
             ON CONFLICT(episode_id, kind) DO UPDATE SET
                 score = excluded.score,
                 evidence = excluded.evidence,
                 created_at_ns = excluded.created_at_ns,
                 resolved_at_ns = NULL,
                 resolved_by_correction_episode_id = NULL",
            rusqlite::named_params! {
                ":episode_id": episode_id,
                ":kind": kind,
                ":score": score,
                ":evidence": evidence,
                ":created_at_ns": now_ns,
            },
        )?;

        let id = self.conn.query_row(
            "SELECT id FROM context_anomalies
              WHERE episode_id = :episode_id
                AND kind = :kind",
            rusqlite::named_params! {
                ":episode_id": episode_id,
                ":kind": kind,
            },
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Read recent unresolved context anomalies of `kind`, newest
    /// first. Forgotten source episodes are filtered out by joining
    /// to the live `episodes` table.
    pub fn recent_context_anomalies(
        &self,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<ContextAnomaly>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT ca.id, ca.episode_id, ca.kind, ca.score, ca.evidence,
                    ca.created_at_ns, ca.resolved_at_ns,
                    ca.resolved_by_correction_episode_id
               FROM context_anomalies ca
               JOIN episodes e ON e.id = ca.episode_id
              WHERE ca.kind = :kind
                AND ca.resolved_at_ns IS NULL
                AND e.forgotten_at_ns IS NULL
              ORDER BY ca.created_at_ns DESC
              LIMIT :limit",
        )?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":kind": kind,
                ":limit": limit as i64,
            },
            map_context_anomaly_row,
        )?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Read a live belief candidate by id, including resolved rows.
    /// Used by tests and future inspection paths that need to audit
    /// why an open decision disappeared from the default unresolved
    /// views.
    pub fn get_belief_candidate(
        &self,
        belief_candidate_id: i64,
    ) -> Result<Option<crate::memory::beliefs::BeliefCandidate>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT id, episode_a_id, episode_b_id, kind, score, evidence,
                        created_at_ns, forgotten_at_ns, resolved_at_ns,
                        resolved_by_correction_episode_id
                   FROM belief_candidates
                  WHERE id = :id
                    AND forgotten_at_ns IS NULL",
                rusqlite::named_params! { ":id": belief_candidate_id },
                map_belief_row,
            )
            .optional()?;
        Ok(row)
    }

    /// Persist that a correction episode resolved a belief candidate.
    /// Returns `true` when this call transitioned an unresolved row,
    /// `false` when the row was missing, forgotten, or already
    /// resolved.
    pub fn resolve_belief_candidate_with_correction(
        &mut self,
        belief_candidate_id: i64,
        correction_episode_id: i64,
    ) -> Result<bool, StorageError> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let n = self.conn.execute(
            "UPDATE belief_candidates
                SET resolved_at_ns = :resolved_at_ns,
                    resolved_by_correction_episode_id = :correction_episode_id
              WHERE id = :id
                AND forgotten_at_ns IS NULL
                AND resolved_at_ns IS NULL",
            rusqlite::named_params! {
                ":resolved_at_ns": now_ns,
                ":correction_episode_id": correction_episode_id,
                ":id": belief_candidate_id,
            },
        )?;
        Ok(n > 0)
    }
}

/// D152 chunk 1.3 — legacy-named `chat_recall_trace` row shape.
/// Current writers use it for local debug recall/dashboard
/// inspection only; cloud LLM clients use MCP ContextEnvelope
/// resources/tools instead. Wire format matches the dashboard's
/// `/api/recall/recent` JSON shape so the frontend can treat this as
/// a typed diagnostic contract.
#[derive(Debug, Clone)]
pub struct ChatRecallTrace {
    pub id: i64,
    pub created_at_ns: i64,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub query_text: String,
    pub pack_count: i64,
    /// Top-k JSON: array of `{ "episode_id": i64, "raw_sim": f32 }`.
    pub top_k_json: String,
    pub response_text: Option<String>,
    pub response_chars: i64,
    pub duration_ms: Option<i64>,
}

impl Storage {
    /// Append a legacy-named local debug recall trace. Caller-supplied
    /// `created_at_ns` keeps the test path deterministic; production
    /// passes `SystemTime::now()` via the `now_ns()` helper. Returns
    /// the new row id.
    #[allow(clippy::too_many_arguments)]
    pub fn append_chat_recall_trace(
        &mut self,
        created_at_ns: i64,
        session_id: Option<&str>,
        project: Option<&str>,
        query_text: &str,
        pack_count: i64,
        top_k_json: &str,
        response_text: Option<&str>,
        response_chars: i64,
        duration_ms: Option<i64>,
    ) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO chat_recall_trace
                 (created_at_ns, session_id, project, query_text,
                  pack_count, top_k_json, response_text, response_chars,
                  duration_ms)
             VALUES
                 (:ts, :sid, :proj, :q, :pc, :topk, :resp, :rc, :dur)",
            rusqlite::named_params! {
                ":ts": created_at_ns,
                ":sid": session_id,
                ":proj": project,
                ":q": query_text,
                ":pc": pack_count,
                ":topk": top_k_json,
                ":resp": response_text,
                ":rc": response_chars,
                ":dur": duration_ms,
            },
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// D162 close — prune chat_recall_trace rows older than
    /// `cutoff_ns`. Returns the number of rows deleted. Idempotent
    /// — empty result on already-clean table. slow_loop calls this
    /// each cycle with `now - 30 days`.
    pub fn prune_chat_recall_trace_before(
        &mut self,
        cutoff_ns: i64,
    ) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "DELETE FROM chat_recall_trace WHERE created_at_ns < ?1",
            rusqlite::params![cutoff_ns],
        )?;
        Ok(n)
    }

    /// Read the `limit` most-recent local debug recall traces, newest
    /// first. `limit == 0` returns an empty vec. The dashboard's View
    /// 2 reads with limit=10 typically.
    pub fn recent_chat_recall_traces(
        &self,
        limit: usize,
    ) -> Result<Vec<ChatRecallTrace>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at_ns, session_id, project, query_text,
                    pack_count, top_k_json, response_text, response_chars,
                    duration_ms
               FROM chat_recall_trace
              ORDER BY created_at_ns DESC
              LIMIT :limit",
        )?;
        let rows = stmt.query_map(rusqlite::named_params! { ":limit": limit as i64 }, |r| {
            Ok(ChatRecallTrace {
                id: r.get("id")?,
                created_at_ns: r.get("created_at_ns")?,
                session_id: r.get("session_id")?,
                project: r.get("project")?,
                query_text: r.get("query_text")?,
                pack_count: r.get("pack_count")?,
                top_k_json: r.get("top_k_json")?,
                response_text: r.get("response_text")?,
                response_chars: r.get("response_chars")?,
                duration_ms: r.get("duration_ms")?,
            })
        })?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

impl Storage {
    /// Open (or create) the DB at `path`, apply pragmas, run
    /// migrations, return. Creates parent directories as needed.
    ///
    /// Round 3 audit (2026-04-29) — dedicated parent directories are
    /// forced to `0o700` and the DB file to `0o600` after open so
    /// episode content is owner-read-only on shared systems. Pre-fix
    /// the default umask (typically 0o022) left both world-readable.
    /// `secrets.toml` already enforced 0o600; this brings DB +
    /// containing dir to the same posture. Sticky shared temp dirs
    /// such as `/tmp` are accepted without chmod because they are not
    /// SOMA-owned; the DB file itself is still tightened to `0o600`.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                Self::tighten_dir_mode(parent)?;
            }
        }
        let mut conn = Connection::open(path)?;
        Self::tighten_db_mode(path)?;
        apply_pragmas(&conn, PragmaPolicy::OnDisk)?;
        migrations::run_migrations(&mut conn)?;
        Ok(Self { conn, db_path: path.to_path_buf() })
    }

    #[cfg(unix)]
    fn tighten_dir_mode(dir: &Path) -> Result<(), StorageError> {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(dir)?;
        let mode = metadata.permissions().mode();
        if mode & 0o1000 != 0 && mode & 0o002 != 0 {
            return Ok(());
        }
        if mode.trailing_zeros() >= 6 {
            return Ok(());
        }
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms).map_err(StorageError::from)?;
        Ok(())
    }
    #[cfg(not(unix))]
    fn tighten_dir_mode(_: &Path) -> Result<(), StorageError> {
        Ok(())
    }

    #[cfg(unix)]
    fn tighten_db_mode(path: &Path) -> Result<(), StorageError> {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)?;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(StorageError::from)?;
        }
        // SQLite also creates `-wal` / `-shm` files alongside; tighten
        // them too (best-effort — they may not exist before first
        // write).
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            let sidecar = std::path::PathBuf::from(sidecar);
            if sidecar.exists() {
                let _ = std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    fn tighten_db_mode(_: &Path) -> Result<(), StorageError> {
        Ok(())
    }

    /// In-memory DB — test and ephemeral use only. WAL is skipped
    /// because SQLite rejects it for `:memory:`.
    /// D121-cand (R7 audit) — periodic maintenance pragma. Releases
    /// freed pages from soft-deleted episodes (`auto_vacuum =
    /// INCREMENTAL` reserved them) and triggers SQLite's query
    /// planner stat refresh. Slow-loop calls this every cycle;
    /// failure is advisory (emits a `tracing::warn!` only —
    /// vacuum failure isn't fatal).
    pub fn run_maintenance(&self) -> Result<(), StorageError> {
        // `incremental_vacuum(64)` releases up to 64 freed pages per
        // call (≈ 256 KiB at 4 KiB page size). On a fresh DB created
        // with `auto_vacuum = INCREMENTAL` this releases real space;
        // on an existing DB created without it, this is a no-op and
        // `PRAGMA optimize` still runs.
        self.conn.execute_batch("PRAGMA incremental_vacuum(64); PRAGMA optimize;")?;
        Ok(())
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let mut conn = Connection::open_in_memory()?;
        apply_pragmas(&conn, PragmaPolicy::InMemory)?;
        migrations::run_migrations(&mut conn)?;
        Ok(Self { conn, db_path: std::path::PathBuf::from(":memory:") })
    }

    /// Applied versions in monotonic order. Used by T.1 to assert
    /// `open()` replayed the full registry, and by operators to
    /// inspect a DB's migration history.
    pub fn schema_versions(&self) -> Result<Vec<u32>, StorageError> {
        let mut stmt =
            self.conn.prepare("SELECT version FROM schema_version ORDER BY version ASC")?;
        let rows = stmt.query_map([], |r| r.get::<_, u32>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Append an episode, returning the assigned `EpisodeId`.
    ///
    /// `memory_tier` defaults to `'short'` and `salience` starts
    /// `NULL`; both become warm-loop territory (Phase 5).
    pub fn append_episode(&mut self, ep: &Episode) -> Result<EpisodeId, StorageError> {
        insert_episode_row(&self.conn, ep)
    }

    /// **Atomic** episode append + vector insert. The two writes
    /// commit or roll back as one SQLite transaction so a vector
    /// failure (dim mismatch, IO error) does **not** leave a
    /// recall-invisible episode in the DB. Used by `run_ingest` to
    /// preserve the SOMA invariant: every episode that exists has
    /// a default-embedder vector under `model_id` (D1 §A).
    ///
    /// `vector.is_empty()` skips the vector write — terminal /
    /// AI episodes whose payload texts are all empty don't
    /// embed (mirrors `episode_index_text` behaviour).
    pub fn append_episode_with_vector(
        &mut self,
        ep: &Episode,
        model_id: &str,
        vector: &[f32],
    ) -> Result<EpisodeId, StorageError> {
        let tx = self.conn.transaction()?;
        let id = insert_episode_row(&tx, ep)?;
        if !vector.is_empty() {
            insert_vector_row(&tx, id, model_id, vector)?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Look up an episode by ID; `Ok(None)` if not found.
    ///
    /// **Surfaces forgotten episodes** — the by-id contract assumes
    /// the caller wants every column regardless of the soft-delete
    /// state. `soma inspect episode --include-forgotten` is the
    /// canonical caller. For recall / ContextEnvelope paths that
    /// must honour the user's forget intent, use [`get_live_episode`].
    pub fn get_episode(&self, id: EpisodeId) -> Result<Option<StoredEpisode>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row("SELECT * FROM episodes WHERE id = ?1", [id], map_row)
            .optional()?;
        Ok(row)
    }

    /// Look up an episode by ID, returning `Ok(None)` if the
    /// episode has been forgotten. R4 audit (2026-04-29) — D92
    /// multi-hop recall walked `episode_edges` (which has no
    /// `forgotten_at_ns` column) and called `get_episode` on each
    /// hop, surfacing forgotten content via PageRank traversal.
    /// `vectors_for_model` already JOIN-filters; this is the
    /// matching by-id-with-filter path for recall consumers.
    pub fn get_live_episode(&self, id: EpisodeId) -> Result<Option<StoredEpisode>, StorageError> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT * FROM episodes WHERE id = ?1 AND forgotten_at_ns IS NULL",
                [id],
                map_row,
            )
            .optional()?;
        Ok(row)
    }

    /// All episodes in insertion order (`id ASC`). Used by self-
    /// model extractors that walk the full history. v1 episode
    /// volume cap (≤10K) makes a full load tolerable; v1.1 can
    /// swap in a streaming iterator.
    ///
    /// D-forget — forgotten episodes (`forgotten_at_ns IS NOT NULL`)
    /// are excluded so soft-deleted rows never re-surface in
    /// extractors or context assembly. Use `all_episodes_including_
    /// forgotten` for audit / inspect paths.
    pub fn all_episodes(&self) -> Result<Vec<StoredEpisode>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM episodes WHERE forgotten_at_ns IS NULL ORDER BY id ASC")?;
        let rows = stmt.query_map([], map_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Live episodes that have **no** vector for `model_id`, in
    /// `id ASC` order, capped at `limit`. SQL anti-join — the DB
    /// filters server-side, so callers never load already-vectored
    /// rows into memory. Used by `slow_loop::backfill_one_model`
    /// to drain its 64-per-cycle window without first materializing
    /// the entire `episode_vectors` table just to extract ids.
    ///
    /// D-forget — forgotten rows are excluded.
    /// `limit == 0` returns an empty vec.
    pub fn episodes_missing_vector_for(
        &self,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredEpisode>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT * FROM episodes
              WHERE forgotten_at_ns IS NULL
                AND id NOT IN (
                    SELECT episode_id FROM episode_vectors
                     WHERE model_id = ?1
                )
              ORDER BY id ASC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![model_id, limit as i64], map_row)?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Same as `all_episodes` but returns forgotten rows too —
    /// audit / `soma inspect` / forensic operators.
    pub fn all_episodes_including_forgotten(&self) -> Result<Vec<StoredEpisode>, StorageError> {
        let mut stmt = self.conn.prepare("SELECT * FROM episodes ORDER BY id ASC")?;
        let rows = stmt.query_map([], map_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Return the N most recent episodes in `ts_start_ns DESC`
    /// order. `limit == 0` returns an empty vec. Forgotten episodes
    /// are filtered out (D-forget).
    pub fn recent_episodes(&self, limit: usize) -> Result<Vec<StoredEpisode>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM episodes WHERE forgotten_at_ns IS NULL
              ORDER BY ts_start_ns DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], map_row)?;
        let mut out = Vec::with_capacity(limit.min(64));
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Episode + vector insertion helpers — work over `&Connection` so
// `Storage` can compose them inside a transaction handle.
// ---------------------------------------------------------------------------

fn insert_episode_row(
    conn: &rusqlite::Connection,
    ep: &Episode,
) -> Result<EpisodeId, StorageError> {
    let id = conn.query_row(
        "INSERT INTO episodes (
            ts_start_ns, ts_end_ns, duration_ms, source, session_id,
            prompt_text, response_text, command, stdout, exit_code,
            cwd, git_branch, project, digest
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14
        ) RETURNING id",
        rusqlite::params![
            ep.ts_start_ns,
            ep.ts_end_ns,
            ep.duration_ms,
            // D119 — `Episode.source` is a typed `EpisodeSource`
            // enum; the SQLite column stays TEXT (wire-schema
            // invariant). Display produces the kebab-case wire
            // string, so the boundary conversion lives here.
            ep.source.to_string(),
            ep.session_id,
            ep.prompt_text,
            ep.response_text,
            ep.command,
            ep.stdout,
            ep.exit_code,
            ep.cwd,
            ep.git_branch,
            ep.project,
            ep.digest,
        ],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(id)
}

/// P1-1 (audit fix) — Layer 3 NaN guard for the four trainable
/// weight tables. Layer 2 (`save_*` methods) already refuses
/// non-finite inputs, but external SQLite corruption could still
/// surface NaN/inf bytes on read; this defends the hot path
/// against propagating those into `compute_free_energy` /
/// `forward` / `salience::score`. Cost = O(N) linear scan over
/// already-decoded f32 values; matches the existing decode pass.
fn finite_check_blob(name: &str, blob: &[f32]) -> Result<(), StorageError> {
    if let Some((idx, bad)) = blob.iter().enumerate().find(|(_, v)| !v.is_finite()) {
        return Err(StorageError::Corrupt {
            detail: format!(
                "{name}[{idx}] = {bad} (non-finite); refusing to surface corrupt weights"
            ),
        });
    }
    Ok(())
}

fn insert_vector_row(
    conn: &rusqlite::Connection,
    episode_id: EpisodeId,
    model_id: &str,
    vector: &[f32],
) -> Result<(), StorageError> {
    // D90 §A — Hopfield §1 spherical-codes invariant: every stored
    // vector lives on the unit sphere. Auto-renormalize rather than
    // reject so a slightly-off-norm caller (numeric drift) doesn't
    // crash the ingest pipeline. The salience kernel and softmax
    // retrieval rely on this invariant.
    let normalized: Vec<f32> = if crate::memory::salience::is_unit_normalized(vector) {
        vector.to_vec()
    } else {
        crate::memory::salience::l2_normalize(vector)
    };
    let dim = normalized.len();
    let mut buf = Vec::with_capacity(dim * 4);
    for v in &normalized {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO episode_vectors
            (episode_id, model_id, dim, vector, created_at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(episode_id, model_id) DO UPDATE SET
            dim = excluded.dim,
            vector = excluded.vector,
            created_at_ns = excluded.created_at_ns",
        rusqlite::params![episode_id, model_id, dim as i64, buf, now_ns],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// PRAGMA policy (discussion 0024 §I)
// ---------------------------------------------------------------------------

enum PragmaPolicy {
    OnDisk,
    InMemory,
}

fn apply_pragmas(conn: &Connection, policy: PragmaPolicy) -> Result<(), StorageError> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    if matches!(policy, PragmaPolicy::OnDisk) {
        // WAL + NORMAL synchronous: single-writer + non-blocking
        // readers + fsync amortized by the WAL checkpointer. Budget-
        // friendly on SOMA's p95 30 ms fast path. `journal_size_limit`
        // caps the WAL file at 64 MiB so a long-running resident
        // doesn't balloon its journal.
        //
        // D121-cand close (R7 audit, 2026-04-30) — explicit
        // `wal_autocheckpoint = 1000` (matches SQLite's default but
        // documents intent) and `auto_vacuum = INCREMENTAL` so
        // months of soft-deletes don't bloat the page table. Note
        // `auto_vacuum` only applies to FRESH databases — for an
        // existing DB the setting is a no-op until a `VACUUM` runs.
        // The slow_loop's optimize sub-task (R7) issues
        // `incremental_vacuum(N)` periodically to release freed
        // pages.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA journal_size_limit = 67108864;
             PRAGMA wal_autocheckpoint = 1000;
             PRAGMA auto_vacuum = INCREMENTAL;",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Convert a BLOB produced by `put_vector` back into `Vec<f32>`.
/// Invalid lengths (non-multiple of 4) truncate silently — vector
/// corruption this extreme would mean someone wrote to the DB
/// outside the `Storage` path, and ingest-time would have rejected
/// the dim mismatch.
fn le_bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    let n = bytes.len() / 4;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[i * 4..i * 4 + 4]);
        v.push(f32::from_le_bytes(buf));
    }
    v
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEpisode> {
    use std::str::FromStr;
    // D119 — read the TEXT column as `String`, then lift to typed
    // `EpisodeSource`. Legacy / unknown / malformed strings (left
    // over from pre-D119 ingest paths or external writes) degrade
    // to `Other(s)` rather than failing row deserialization — read
    // path is hot and a corrupt-source row should never crash
    // `recall` / `inspect` / extractor scans. The kebab-case regex
    // guard runs only on the *write* side (FromStr at ingest time).
    let source_raw: String = row.get("source")?;
    let source = match EpisodeSource::from_str(&source_raw) {
        Ok(s) => s,
        Err(_) => EpisodeSource::Other(source_raw),
    };
    Ok(StoredEpisode {
        id: row.get("id")?,
        ts_start_ns: row.get("ts_start_ns")?,
        ts_end_ns: row.get("ts_end_ns")?,
        duration_ms: row.get("duration_ms")?,
        source,
        session_id: row.get("session_id")?,
        prompt_text: row.get("prompt_text")?,
        response_text: row.get("response_text")?,
        command: row.get("command")?,
        stdout: row.get("stdout")?,
        exit_code: row.get("exit_code")?,
        cwd: row.get("cwd")?,
        git_branch: row.get("git_branch")?,
        project: row.get("project")?,
        memory_tier: row.get("memory_tier")?,
        salience: row.get("salience")?,
        digest: row.get("digest")?,
    })
}

/// D84 — map a `belief_candidates` row to the typed `BeliefCandidate`
/// shape. Symmetric to `map_row` for episodes — uses named-column
/// `row.get("col_name")` lookups (D46 invariant) and lifts the TEXT
/// `kind` column to the typed `BeliefKind` enum. Unknown/legacy
/// strings would fail `FromStr`; we surface them as
/// `rusqlite::Error::FromSqlConversionFailure` rather than panicking
/// or silently mis-typing — a corrupt `kind` value indicates external
/// tampering and the read path should refuse rather than guess.
fn map_belief_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::memory::beliefs::BeliefCandidate> {
    use std::str::FromStr;
    let kind_raw: String = row.get("kind")?;
    let kind = crate::memory::beliefs::BeliefKind::from_str(&kind_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(crate::memory::beliefs::BeliefCandidate {
        id: row.get("id")?,
        episode_a_id: row.get("episode_a_id")?,
        episode_b_id: row.get("episode_b_id")?,
        kind,
        score: row.get("score")?,
        evidence: row.get("evidence")?,
        created_at_ns: row.get("created_at_ns")?,
        forgotten_at_ns: row.get("forgotten_at_ns")?,
        resolved_at_ns: row.get("resolved_at_ns")?,
        resolved_by_correction_episode_id: row.get("resolved_by_correction_episode_id")?,
    })
}

fn map_context_anomaly_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextAnomaly> {
    Ok(ContextAnomaly {
        id: row.get("id")?,
        episode_id: row.get("episode_id")?,
        kind: row.get("kind")?,
        score: row.get("score")?,
        evidence: row.get("evidence")?,
        created_at_ns: row.get("created_at_ns")?,
        resolved_at_ns: row.get("resolved_at_ns")?,
        resolved_by_correction_episode_id: row.get("resolved_by_correction_episode_id")?,
    })
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

    /// D120-cand (R10 audit, 2026-04-30) — `run_maintenance` must
    /// succeed on a fresh DB (no episodes, `auto_vacuum=INCREMENTAL`
    /// already applied). Pre-fix this would have been a regression
    /// vector if the pragma batch ever required a non-empty
    /// `freelist_count` to succeed.
    #[test]
    fn run_maintenance_succeeds_on_fresh_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let store = Storage::open(&path).expect("open");
        // Fresh DB has zero episodes; both pragmas must still succeed.
        store.run_maintenance().expect("maintenance on fresh DB");
        // Idempotent — calling twice in a row must not error.
        store.run_maintenance().expect("maintenance twice");
    }

    #[cfg(unix)]
    #[test]
    fn storage_open_tightens_dedicated_parent_and_db_modes() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("soma-owned-db-dir");
        let path = parent.join("soma.db");
        let store = Storage::open(&path).expect("open");
        drop(store);

        let parent_mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        let db_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "dedicated DB parent should be 0700");
        assert_eq!(db_mode, 0o600, "DB file should be 0600");
    }

    #[cfg(unix)]
    #[test]
    fn storage_open_accepts_sticky_shared_tmp_parent_without_chmod() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let tmp_parent = std::path::PathBuf::from("/tmp");
        let before = std::fs::metadata(&tmp_parent).expect("/tmp metadata").permissions().mode();
        if before & 0o1000 == 0 || before & 0o002 == 0 {
            return;
        }

        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let path = tmp_parent.join(format!("soma-storage-shared-parent-{suffix}.db"));
        let _ = std::fs::remove_file(&path);

        let store = Storage::open(&path).expect("open DB directly under sticky shared tmp");
        drop(store);

        let after =
            std::fs::metadata(&tmp_parent).expect("/tmp metadata after").permissions().mode();
        let db_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(after & 0o7777, before & 0o7777, "Storage::open must not chmod /tmp");
        assert_eq!(db_mode, 0o600, "DB file should still be 0600 in shared tmp");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
