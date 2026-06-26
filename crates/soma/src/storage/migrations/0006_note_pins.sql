-- Migration 0006 — Note Block + access tracking.
--
-- Discussion 0037 §D91 / ADR 0004 §B. The Note Block pattern from
-- MemMamba (`arXiv: 2510.03279`): high-salience episodes are pinned
-- to a separate buffer that survives Ebbinghaus decay regardless of
-- age. This is what lets a 6-month-old "the auth refactor that
-- broke prod" memory still surface in MemoryPack while routine
-- daily commands fade.
--
-- access_count + last_access_ns on episodes implement the Ebbinghaus
-- formula's input (`MemoryBank` AAAI 2024):
--   decay_weight = exp(-λ · Δt_days / (1 + access_count))
--
-- Append-only — never edit this file after landing.

CREATE TABLE IF NOT EXISTS note_pins (
    episode_id      INTEGER PRIMARY KEY REFERENCES episodes(id) ON DELETE CASCADE,
    pinned_at_ns    INTEGER NOT NULL,
    reason          TEXT    NOT NULL,         -- 'salience' | 'manual' | 'system'
    salience_at_pin REAL    NOT NULL          -- D90 free_energy at pin time
);

CREATE INDEX IF NOT EXISTS idx_note_pins_pinned_at ON note_pins(pinned_at_ns DESC);

ALTER TABLE episodes ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE episodes ADD COLUMN last_access_ns INTEGER NOT NULL DEFAULT 0;
