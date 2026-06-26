-- Correction loop persistence for belief candidates.
--
-- A user correction can resolve a contradiction. v20 records that state on
-- the belief_candidates row so every reader, not only ContextEnvelope render,
-- sees the same unresolved/resolved boundary.

ALTER TABLE belief_candidates
    ADD COLUMN resolved_at_ns INTEGER;

ALTER TABLE belief_candidates
    ADD COLUMN resolved_by_correction_episode_id INTEGER
        REFERENCES episodes(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_belief_candidates_unresolved_kind
    ON belief_candidates(kind, resolved_at_ns, created_at_ns);

CREATE INDEX IF NOT EXISTS idx_belief_candidates_resolved_by_correction
    ON belief_candidates(resolved_by_correction_episode_id);
