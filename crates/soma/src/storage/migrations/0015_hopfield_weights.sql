-- v1.2 chunk 4.3 (ADR 0011 §D4) — TrainableHopfield Q/K/V weights
-- persistence. Singleton row (CHECK id = 1).

CREATE TABLE IF NOT EXISTS hopfield_weights (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    d_emb           INTEGER NOT NULL,
    num_heads       INTEGER NOT NULL,
    w_q_blob        BLOB    NOT NULL,
    w_k_blob        BLOB    NOT NULL,
    w_v_blob        BLOB    NOT NULL,
    train_steps     INTEGER NOT NULL DEFAULT 0,
    saved_at_ns     INTEGER NOT NULL
);
