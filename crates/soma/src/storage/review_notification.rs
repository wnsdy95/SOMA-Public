use rusqlite::{OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{Storage, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDigestNotificationAckDraft {
    pub client: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub policy: String,
    pub batch_key: String,
    pub digest_signature: String,
    pub item_count: usize,
    pub notification_count: usize,
    pub cooldown_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredReviewDigestNotification {
    pub id: i64,
    pub client: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub policy: String,
    pub batch_key: String,
    pub digest_signature: String,
    pub item_count: usize,
    pub notification_count: usize,
    pub acknowledged_at_ns: i64,
    pub cooldown_until_ns: i64,
    pub ack_count: usize,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

impl Storage {
    pub fn review_digest_notification(
        &self,
        client: &str,
        project: Option<&str>,
        session_id: Option<&str>,
        policy: &str,
        batch_key: &str,
    ) -> Result<Option<StoredReviewDigestNotification>, StorageError> {
        let row = self
            .conn
            .query_row(
                "SELECT *
                   FROM review_digest_notifications
                  WHERE client = ?1
                    AND project_scope = ?2
                    AND session_scope = ?3
                    AND policy = ?4
                    AND batch_key = ?5",
                rusqlite::params![
                    normalize_required(client, "client")?,
                    scope_key(project),
                    scope_key(session_id),
                    normalize_required(policy, "policy")?,
                    normalize_required(batch_key, "batch_key")?,
                ],
                map_review_digest_notification_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn upsert_review_digest_notification_ack(
        &mut self,
        draft: &ReviewDigestNotificationAckDraft,
    ) -> Result<StoredReviewDigestNotification, StorageError> {
        validate_ack_draft(draft)?;
        let now_ns = now_ns();
        let cooldown_until_ns =
            now_ns.saturating_add((draft.cooldown_seconds as i64).saturating_mul(1_000_000_000));
        self.conn.execute(
            "INSERT INTO review_digest_notifications (
                client, project_scope, session_scope, policy, batch_key,
                digest_signature, item_count, notification_count, acknowledged_at_ns,
                cooldown_until_ns, ack_count, created_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?9, ?9)
             ON CONFLICT(client, project_scope, session_scope, policy, batch_key)
             DO UPDATE SET
                digest_signature = excluded.digest_signature,
                item_count = excluded.item_count,
                notification_count = excluded.notification_count,
                acknowledged_at_ns = excluded.acknowledged_at_ns,
                cooldown_until_ns = excluded.cooldown_until_ns,
                ack_count = review_digest_notifications.ack_count + 1,
                updated_at_ns = excluded.updated_at_ns",
            rusqlite::params![
                normalize_required(&draft.client, "client")?,
                scope_key(draft.project.as_deref()),
                scope_key(draft.session_id.as_deref()),
                normalize_required(&draft.policy, "policy")?,
                normalize_required(&draft.batch_key, "batch_key")?,
                normalize_required(&draft.digest_signature, "digest_signature")?,
                draft.item_count as i64,
                draft.notification_count as i64,
                now_ns,
                cooldown_until_ns,
            ],
        )?;
        self.review_digest_notification(
            &draft.client,
            draft.project.as_deref(),
            draft.session_id.as_deref(),
            &draft.policy,
            &draft.batch_key,
        )?
        .ok_or_else(|| StorageError::Corrupt {
            detail: "review digest notification ack upsert did not return a row".to_string(),
        })
    }
}

fn validate_ack_draft(draft: &ReviewDigestNotificationAckDraft) -> Result<(), StorageError> {
    normalize_required(&draft.client, "client")?;
    normalize_required(&draft.policy, "policy")?;
    normalize_required(&draft.batch_key, "batch_key")?;
    normalize_required(&draft.digest_signature, "digest_signature")?;
    Ok(())
}

fn normalize_required<'a>(value: &'a str, field: &str) -> Result<&'a str, StorageError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(StorageError::Corrupt { detail: format!("{field} must not be empty") });
    }
    Ok(trimmed)
}

fn scope_key(value: Option<&str>) -> String {
    value.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("*").to_string()
}

fn scope_value(value: String) -> Option<String> {
    if value == "*" {
        None
    } else {
        Some(value)
    }
}

fn map_review_digest_notification_row(
    row: &Row<'_>,
) -> rusqlite::Result<StoredReviewDigestNotification> {
    let project_scope: String = row.get("project_scope")?;
    let session_scope: String = row.get("session_scope")?;
    let item_count: i64 = row.get("item_count")?;
    let notification_count: i64 = row.get("notification_count")?;
    let ack_count: i64 = row.get("ack_count")?;
    Ok(StoredReviewDigestNotification {
        id: row.get("id")?,
        client: row.get("client")?,
        project: scope_value(project_scope),
        session_id: scope_value(session_scope),
        policy: row.get("policy")?,
        batch_key: row.get("batch_key")?,
        digest_signature: row.get("digest_signature")?,
        item_count: item_count.max(0) as usize,
        notification_count: notification_count.max(0) as usize,
        acknowledged_at_ns: row.get("acknowledged_at_ns")?,
        cooldown_until_ns: row.get("cooldown_until_ns")?,
        ack_count: ack_count.max(0) as usize,
        created_at_ns: row.get("created_at_ns")?,
        updated_at_ns: row.get("updated_at_ns")?,
    })
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
