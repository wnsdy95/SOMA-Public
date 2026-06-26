-- Migration 0003 — semantic layer: episode_vectors table.
--
-- Discussion 0028 §F. Stores one vector per (episode, model) pair.
-- Multi-model coexistence is expected (Mini profile's hash
-- embedder writes one row per episode; Studio's MiniLM-L12 + e5-
-- large dual write is v1.1 territory). The UNIQUE constraint on
-- (episode_id, model_id) keeps identity within a model stable and
-- lets the caller detect re-embed attempts.
--
-- vector BLOB format: native little-endian f32[dim], dim * 4 bytes.
-- Rust callers reinterpret via bytemuck::cast_slice.
--
-- Append-only — never edit this file after landing. Schema changes
-- ship as new numbered migrations (discussion 0024 §B).

CREATE TABLE IF NOT EXISTS episode_vectors (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    episode_id     INTEGER NOT NULL,
    model_id       TEXT    NOT NULL,
    dim            INTEGER NOT NULL,
    vector         BLOB    NOT NULL,
    created_at_ns  INTEGER NOT NULL,
    FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE,
    UNIQUE (episode_id, model_id)
);

CREATE INDEX IF NOT EXISTS idx_episode_vectors_episode ON episode_vectors(episode_id);
CREATE INDEX IF NOT EXISTS idx_episode_vectors_model   ON episode_vectors(model_id);
