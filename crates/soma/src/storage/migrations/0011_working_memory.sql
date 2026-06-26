-- Migration 0011 — PaperWorkingMemory state persistence.
--
-- STAGE 3-C per ADR 0006. The `cognitive` feature's mLSTM cell
-- (Beck 2024) keeps a (C, n) state — a `d×d` matrix + `d` vector.
-- Without persistence, daemon restart loses the working memory; the
-- "session continuity" promise of SOMA degrades to "fresh start
-- every boot".
--
-- Schema = single-row table (always exactly one row, primary key
-- pinned to 1) holding the serialized state. v1.1 stores it as
-- little-endian f32 BLOBs (same format as `episode_vectors.vector`).
--
-- `dim` is the d_emb the matrix was sized for; loader checks it
-- against the runtime PaperWorkingMemory's d_emb and refuses
-- mismatches (treats as fresh init).
--
-- Append-only — never edit this file after landing.

CREATE TABLE IF NOT EXISTS working_memory_state (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    dim             INTEGER NOT NULL,
    c_matrix_blob   BLOB    NOT NULL,
    n_vector_blob   BLOB    NOT NULL,
    saved_at_ns     INTEGER NOT NULL
);
