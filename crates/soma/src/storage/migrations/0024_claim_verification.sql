-- Claim provenance and verification ledger.
--
-- Cloud output is useful work product, not durable evidence. Store cloud
-- output as draft claims first, then require explicit verification before a
-- claim can move into long-term or semantic memory.

CREATE TABLE IF NOT EXISTS claim_records (
    id                  INTEGER PRIMARY KEY,
    text                TEXT NOT NULL,
    source_type         TEXT NOT NULL,
    task_frame_id       INTEGER REFERENCES task_frames(id) ON DELETE SET NULL,
    evidence_refs_json  TEXT NOT NULL,
    confidence          REAL NOT NULL DEFAULT 0.0,
    lifecycle_state     TEXT NOT NULL,
    promotion_reason    TEXT,
    created_at_ns       INTEGER NOT NULL,
    updated_at_ns       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_claim_records_task_frame
    ON claim_records(task_frame_id, created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_claim_records_source_state
    ON claim_records(source_type, lifecycle_state, updated_at_ns DESC);

CREATE TABLE IF NOT EXISTS verification_events (
    id                 INTEGER PRIMARY KEY,
    claim_id           INTEGER NOT NULL REFERENCES claim_records(id) ON DELETE CASCADE,
    verifier_type      TEXT NOT NULL,
    result             TEXT NOT NULL,
    evidence_ref_json  TEXT NOT NULL,
    created_at_ns      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_verification_events_claim
    ON verification_events(claim_id, created_at_ns ASC);

CREATE INDEX IF NOT EXISTS idx_verification_events_result
    ON verification_events(result, created_at_ns DESC);
