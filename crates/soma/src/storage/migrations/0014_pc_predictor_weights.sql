-- v1.2 chunk 3.3 (ADR 0010 §D5) — TrainablePc per-layer predictor
-- weights. Multi-row (one per layer), keyed on layer_idx.

CREATE TABLE IF NOT EXISTS pc_predictor_weights (
    layer_idx       INTEGER PRIMARY KEY CHECK (layer_idx >= 0),
    d_in            INTEGER NOT NULL,
    d_out           INTEGER NOT NULL,
    w_blob          BLOB    NOT NULL,
    train_steps     INTEGER NOT NULL DEFAULT 0,
    saved_at_ns     INTEGER NOT NULL
);
