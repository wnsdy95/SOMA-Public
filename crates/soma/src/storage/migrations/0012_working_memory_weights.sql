-- v1.2 chunk 1.3 (ADR 0008 §D4) — TrainableMLstm Q/K/V weights
-- persistence. Singleton row (CHECK id = 1) so the resident
-- always has at most one set of working-memory projections.
--
-- Separated from working_memory_state (migration 0011) so the
-- two write rates don't contend on the same row — state updates
-- every ingest, weights only at slow_loop train cycles.
--
-- BLOB encoding = little-endian f32, row-major dim×dim.

CREATE TABLE IF NOT EXISTS working_memory_weights (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    dim             INTEGER NOT NULL,
    w_q_blob        BLOB    NOT NULL,
    w_k_blob        BLOB    NOT NULL,
    w_v_blob        BLOB    NOT NULL,
    train_steps     INTEGER NOT NULL DEFAULT 0,
    saved_at_ns     INTEGER NOT NULL
);
