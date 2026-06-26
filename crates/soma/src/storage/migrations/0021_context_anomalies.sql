-- Context anomaly rows for optional quality-module signals.
--
-- iPC free-energy is single-episode evidence, not a two-episode belief
-- candidate. Keep it in a dedicated table so anomaly evidence can become
-- cited ContextEnvelope.open_decisions without overloading note_pins or
-- belief_candidates.

CREATE TABLE IF NOT EXISTS context_anomalies (
    id                                INTEGER PRIMARY KEY,
    episode_id                        INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    kind                              TEXT NOT NULL,
    score                             REAL NOT NULL,
    evidence                          TEXT,
    created_at_ns                     INTEGER NOT NULL,
    resolved_at_ns                    INTEGER,
    resolved_by_correction_episode_id INTEGER REFERENCES episodes(id) ON DELETE SET NULL,
    UNIQUE(episode_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_context_anomalies_episode
    ON context_anomalies(episode_id);

CREATE INDEX IF NOT EXISTS idx_context_anomalies_unresolved_kind
    ON context_anomalies(kind, resolved_at_ns, created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_context_anomalies_resolved_by_correction
    ON context_anomalies(resolved_by_correction_episode_id);
