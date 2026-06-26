-- Migration 0010 — soft-delete column for `soma forget`.
--
-- Discussion 0034 §A-half close-out follow-up. `soma forget` does
-- *not* DELETE rows — that would obliterate evidence_ids that
-- point back at episodes from self_state, breaking the orthogonal-
-- fact invariant (ADR 0004 §D). Instead each forgotten episode
-- gets `forgotten_at_ns` stamped + every recall path filters
-- non-NULL forgotten_at_ns out.
--
-- The audit trail = note_pins reason='forgotten:<reason>' +
-- forgotten_at_ns timestamp.
--
-- Append-only — never edit this file after landing.

ALTER TABLE episodes ADD COLUMN forgotten_at_ns INTEGER;
ALTER TABLE episodes ADD COLUMN forgotten_reason TEXT;

CREATE INDEX IF NOT EXISTS idx_episodes_forgotten
    ON episodes(forgotten_at_ns)
    WHERE forgotten_at_ns IS NOT NULL;
