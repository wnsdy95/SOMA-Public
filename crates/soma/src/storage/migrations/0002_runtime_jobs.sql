-- Migration 0002 — runtime_jobs queue.
--
-- The resident runtime's Fast/Warm/Slow loop scheduler drains this
-- queue. An episode insert enqueues one row per enrichment job
-- (embed / salience / digest / …); the warm loop dequeues with
-- DELETE+RETURNING, runs the job, and — on failure — re-inserts with
-- backoff. This shape is the v1 single-process simplification of
-- legacy/soma-terminal's pending_jobs (WS 3 PR 3.2) + worker pool
-- claim semantics (WS 5 PR 5.1).
--
-- Ownership boundary: discussion 0024 §B migration runner owns
-- schema_version inserts; this file only ships DDL.

CREATE TABLE IF NOT EXISTS runtime_jobs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    episode_id   INTEGER NOT NULL,
    kind         TEXT    NOT NULL,   -- 'embed' | 'salience' | 'digest' | ...
    priority     INTEGER NOT NULL DEFAULT 100,
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    last_error   TEXT,
    created_at   INTEGER NOT NULL,
    next_run_at  INTEGER NOT NULL,
    FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
);

-- Partial index: drained rows (attempts < max_attempts) ordered for
-- next-run pickup. Matches WS 5 PR 5.1 §C1 claim ordering.
CREATE INDEX IF NOT EXISTS idx_runtime_jobs_ready
    ON runtime_jobs(next_run_at, priority, id)
    WHERE attempts < max_attempts;

CREATE INDEX IF NOT EXISTS idx_runtime_jobs_episode
    ON runtime_jobs(episode_id);
