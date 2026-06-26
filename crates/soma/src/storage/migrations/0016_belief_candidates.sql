-- D84 close — belief candidates: typed-relationship overlay on
-- episode_edges. Each row links two episodes by an explicit kind
-- (corroborates / contradicts) with a numeric score (cosine sim
-- at seed time) + short evidence string. Forgotten cascade on
-- episode delete (existing tier convention).
--
-- Phase 7-8 plan calls for an explicit contradiction / corroboration
-- link layer above the cosine-similarity-only `episode_edges` table
-- so slow_loop can surface unreviewed contradictions to the operator
-- (discussions 0024 §K future-work + ADR 0004 §C edge graph).
--
-- Append-only — never edit this file after landing.

CREATE TABLE IF NOT EXISTS belief_candidates (
    id              INTEGER PRIMARY KEY,
    episode_a_id    INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    episode_b_id    INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    score           REAL NOT NULL,
    evidence        TEXT,
    created_at_ns   INTEGER NOT NULL,
    forgotten_at_ns INTEGER,
    UNIQUE(episode_a_id, episode_b_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_belief_candidates_episode_a
    ON belief_candidates(episode_a_id);
CREATE INDEX IF NOT EXISTS idx_belief_candidates_episode_b
    ON belief_candidates(episode_b_id);
