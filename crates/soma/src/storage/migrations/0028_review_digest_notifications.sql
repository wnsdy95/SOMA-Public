-- Client-side review digest notification state.
--
-- This ledger records that a client rendered or acknowledged a compact review
-- digest notification. It is intentionally outside claim verification and
-- learning proposal state: acknowledging a digest must never create durable
-- promotion trust or apply a proposal.

CREATE TABLE IF NOT EXISTS review_digest_notifications (
    id                      INTEGER PRIMARY KEY,
    client                  TEXT NOT NULL,
    project_scope           TEXT NOT NULL,
    session_scope           TEXT NOT NULL,
    policy                  TEXT NOT NULL,
    batch_key               TEXT NOT NULL,
    digest_signature        TEXT NOT NULL,
    item_count              INTEGER NOT NULL,
    notification_count      INTEGER NOT NULL,
    acknowledged_at_ns      INTEGER NOT NULL,
    cooldown_until_ns       INTEGER NOT NULL,
    ack_count               INTEGER NOT NULL,
    created_at_ns           INTEGER NOT NULL,
    updated_at_ns           INTEGER NOT NULL,
    UNIQUE(client, project_scope, session_scope, policy, batch_key)
);

CREATE INDEX IF NOT EXISTS idx_review_digest_notifications_scope
    ON review_digest_notifications(client, project_scope, session_scope, policy, batch_key);

CREATE INDEX IF NOT EXISTS idx_review_digest_notifications_cooldown
    ON review_digest_notifications(cooldown_until_ns DESC, updated_at_ns DESC);
