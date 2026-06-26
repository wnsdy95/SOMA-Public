-- v1.2 chunk 2.3 (ADR 0009 §D4) — ANIL classifier head weights
-- + project mapping. Singleton row (CHECK id = 1).
--
-- K (num_classes) is not fixed — chunk 2.4's slow_loop grows the
-- mapping by appending a row to projects_json + zero-init row to
-- the BLOBs whenever a new project shows up.
--
-- BLOB encoding = little-endian f32. w_head is row-major
-- (num_classes × d_emb). b_head is (num_classes,).

CREATE TABLE IF NOT EXISTS anil_head_weights (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    d_emb           INTEGER NOT NULL,
    num_classes     INTEGER NOT NULL,
    w_head_blob     BLOB    NOT NULL,
    b_head_blob     BLOB    NOT NULL,
    projects_json   TEXT    NOT NULL,
    train_steps     INTEGER NOT NULL DEFAULT 0,
    saved_at_ns     INTEGER NOT NULL
);
