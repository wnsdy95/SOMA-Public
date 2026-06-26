-- Round 2 in-house ultrareview fix — D47-cand close: enforce
-- "one row per (episode_id, kind)" invariant on `runtime_jobs`.
--
-- Pre-fix the schema (migration 0002) had an idx on (state, next_run_at)
-- but no UNIQUE on (episode_id, kind). The capture path's enqueue site
-- (`insert_pending_jobs_tx` validate-then-insert) gated single-call
-- duplicates pre-tx, but cross-call duplicates (e.g. retry after a
-- crashed slow_loop cycle, or a future daemon worker re-enqueuing on
-- transient failure) silently accumulated. Both rows then drained
-- through the dequeue order, breaking the "one job per kind" invariant
-- documented in mig 0002 §goal.
--
-- SQLite ALTER TABLE limitations make adding a UNIQUE constraint to
-- an existing table awkward; the standard remedy is a unique index,
-- which produces equivalent constraint enforcement.
--
-- Migration is safe on populated tables: the UNIQUE index creation
-- will fail loudly if duplicates already exist, surfacing the issue
-- to the operator. In normal v1.x deployment the gate at
-- `insert_pending_jobs_tx` has been preventing duplicates so the
-- index creation is expected to succeed without intervention.

CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_jobs_episode_kind_unique
    ON runtime_jobs (episode_id, kind);
