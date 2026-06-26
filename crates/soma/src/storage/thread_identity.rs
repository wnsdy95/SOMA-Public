//! Operator-confirmed thread identities.
//!
//! This ledger is deliberately narrower than a thread-scoped ContextEnvelope
//! resource. It stores explicit operator confirmations only; callers must not
//! treat these rows as automatic cross-session merge permission.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{EpisodeId, Storage, StorageError, StoredEpisode};

pub const THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED: &str = "operator_confirmed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadIdentityDraft {
    pub thread_key: String,
    pub project: String,
    pub session_ids: Vec<String>,
    pub evidence_episode_ids: Vec<EpisodeId>,
    pub confirmed_by: String,
    pub confirmation_reason: String,
    pub allow_cross_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredThreadIdentity {
    pub id: i64,
    pub thread_key: String,
    pub project: String,
    pub status: String,
    pub session_ids: Vec<String>,
    pub evidence_episode_ids: Vec<EpisodeId>,
    pub confirmed_by: String,
    pub confirmation_reason: String,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

impl Storage {
    pub fn live_episodes_for_thread_identity_sessions(
        &self,
        project: &str,
        session_ids: &[String],
    ) -> Result<Vec<StoredEpisode>, StorageError> {
        let sessions: BTreeSet<&str> = session_ids.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        for episode in self.all_episodes()? {
            if episode.project.as_deref() == Some(project)
                && episode
                    .session_id
                    .as_deref()
                    .is_some_and(|session_id| sessions.contains(session_id))
            {
                out.push(episode);
            }
        }
        out.sort_by_key(|episode| (episode.ts_start_ns, episode.id));
        Ok(out)
    }

    pub fn session_has_live_episodes_outside_project(
        &self,
        project: &str,
        session_id: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.all_episodes()?.into_iter().any(|episode| {
            episode.session_id.as_deref() == Some(session_id)
                && episode.project.as_deref() != Some(project)
        }))
    }

    pub fn confirm_thread_identity(
        &mut self,
        draft: &ThreadIdentityDraft,
    ) -> Result<StoredThreadIdentity, StorageError> {
        validate_thread_identity_draft(draft)?;
        let session_ids_json = encode_json(&draft.session_ids, "thread identity session ids")?;
        let evidence_episode_ids_json =
            encode_json(&draft.evidence_episode_ids, "thread identity evidence episode ids")?;
        let now_ns = now_ns();
        let tx = self.conn.transaction()?;
        let id = tx.query_row(
            "INSERT INTO thread_identities (
                thread_key, project, status, session_ids_json,
                evidence_episode_ids_json, confirmed_by, confirmation_reason,
                created_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             RETURNING id",
            rusqlite::params![
                draft.thread_key.trim(),
                draft.project.trim(),
                THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED,
                session_ids_json,
                evidence_episode_ids_json,
                draft.confirmed_by.trim(),
                draft.confirmation_reason.trim(),
                now_ns,
                now_ns,
            ],
            |row| row.get::<_, i64>(0),
        )?;

        for episode_id in &draft.evidence_episode_ids {
            let (session_id, source): (String, String) = tx.query_row(
                "SELECT session_id, source
                   FROM episodes
                  WHERE id = ?1
                    AND forgotten_at_ns IS NULL
                    AND project = ?2",
                rusqlite::params![episode_id, draft.project.trim()],
                |row| {
                    let session_id: Option<String> = row.get("session_id")?;
                    let source: String = row.get("source")?;
                    Ok((session_id.unwrap_or_default(), source))
                },
            )?;
            if !draft.session_ids.iter().any(|expected| expected == &session_id) {
                return Err(StorageError::Corrupt {
                    detail: format!(
                        "thread identity evidence episode {episode_id} session `{session_id}` is not in confirmed sessions"
                    ),
                });
            }
            tx.execute(
                "INSERT INTO thread_identity_members (
                    thread_identity_id, session_id, episode_id, source, added_at_ns
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, session_id, episode_id, source, now_ns],
            )?;
        }
        tx.commit()?;

        self.thread_identity_by_id(id)?.ok_or_else(|| StorageError::Corrupt {
            detail: format!("inserted thread identity {id} was not readable"),
        })
    }

    pub fn thread_identity_by_id(
        &self,
        identity_id: i64,
    ) -> Result<Option<StoredThreadIdentity>, StorageError> {
        use rusqlite::OptionalExtension;

        self.conn
            .query_row(
                "SELECT *
                   FROM thread_identities
                  WHERE id = ?1",
                rusqlite::params![identity_id],
                map_thread_identity_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn thread_identity_by_key(
        &self,
        thread_key: &str,
    ) -> Result<Option<StoredThreadIdentity>, StorageError> {
        use rusqlite::OptionalExtension;

        self.conn
            .query_row(
                "SELECT *
                   FROM thread_identities
                  WHERE thread_key = ?1",
                rusqlite::params![thread_key],
                map_thread_identity_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn recent_thread_identities(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredThreadIdentity>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT *
               FROM thread_identities
              WHERE (?1 IS NULL OR project = ?1)
              ORDER BY updated_at_ns DESC, id DESC
              LIMIT ?2",
        )?;
        let rows =
            stmt.query_map(rusqlite::params![project, limit as i64], map_thread_identity_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn validate_thread_identity_draft(draft: &ThreadIdentityDraft) -> Result<(), StorageError> {
    for (field, value) in [
        ("thread_key", draft.thread_key.as_str()),
        ("project", draft.project.as_str()),
        ("confirmed_by", draft.confirmed_by.as_str()),
        ("confirmation_reason", draft.confirmation_reason.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(StorageError::Corrupt {
                detail: format!("thread identity confirmation requires non-empty {field}"),
            });
        }
    }
    if draft.session_ids.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "thread identity confirmation requires at least one session".to_string(),
        });
    }
    if draft.evidence_episode_ids.is_empty() {
        return Err(StorageError::Corrupt {
            detail: "thread identity confirmation requires evidence episodes".to_string(),
        });
    }
    let sessions: BTreeSet<&str> = draft.session_ids.iter().map(|value| value.trim()).collect();
    if sessions.len() != draft.session_ids.len() || sessions.iter().any(|value| value.is_empty()) {
        return Err(StorageError::Corrupt {
            detail: "thread identity confirmation requires unique non-empty sessions".to_string(),
        });
    }
    let evidence: BTreeSet<EpisodeId> = draft.evidence_episode_ids.iter().copied().collect();
    if evidence.len() != draft.evidence_episode_ids.len() {
        return Err(StorageError::Corrupt {
            detail: "thread identity confirmation requires unique evidence episodes".to_string(),
        });
    }
    if draft.session_ids.len() > 1 && !draft.allow_cross_session {
        return Err(StorageError::Corrupt {
            detail: "multi-session thread identity confirmation requires allow_cross_session=true"
                .to_string(),
        });
    }
    Ok(())
}

fn map_thread_identity_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredThreadIdentity> {
    let session_ids_json: String = row.get("session_ids_json")?;
    let evidence_episode_ids_json: String = row.get("evidence_episode_ids_json")?;
    Ok(StoredThreadIdentity {
        id: row.get("id")?,
        thread_key: row.get("thread_key")?,
        project: row.get("project")?,
        status: row.get("status")?,
        session_ids: decode_json(&session_ids_json, "thread identity session ids")?,
        evidence_episode_ids: decode_json(
            &evidence_episode_ids_json,
            "thread identity evidence episode ids",
        )?,
        confirmed_by: row.get("confirmed_by")?,
        confirmation_reason: row.get("confirmation_reason")?,
        created_at_ns: row.get("created_at_ns")?,
        updated_at_ns: row.get("updated_at_ns")?,
    })
}

fn encode_json<T: Serialize>(value: &T, label: &str) -> Result<String, StorageError> {
    serde_json::to_string(value)
        .map_err(|err| StorageError::Corrupt { detail: format!("{label} JSON encode: {err}") })
}

fn decode_json<T: for<'de> Deserialize<'de>>(raw: &str, label: &str) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{label} JSON decode: {err}"),
            )),
        )
    })
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
