-- Migration 0004 — self_state table.
--
-- Discussion 0030 §A + §G. Narrow row-per-fact schema: one row per
-- (kind, key) pair. Extractors produce `SelfFact` values and
-- `Storage::upsert_self_fact` writes them here with ON CONFLICT
-- upsert so re-running extraction is idempotent.
--
-- `value_json` holds the fact's payload (extractor-defined shape,
-- serialized JSON). `evidence_ids` holds a JSON array of episode
-- IDs that the fact was derived from — the user-facing "why this
-- was inferred" attribution.
--
-- No separate `project_state` table. The narrow schema carries
-- project_norms rows under `kind='project_norms'`.
--
-- Append-only — never edit this file after landing.

CREATE TABLE IF NOT EXISTS self_state (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    kind           TEXT    NOT NULL,
    key            TEXT    NOT NULL,
    value_json     TEXT    NOT NULL,
    evidence_ids   TEXT    NOT NULL,
    computed_at_ns INTEGER NOT NULL,
    UNIQUE (kind, key)
);

CREATE INDEX IF NOT EXISTS idx_self_state_kind ON self_state(kind);
