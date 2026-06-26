-- D152 chunk 1.3 — legacy-named recall trace table. It originally
-- captured every `soma chat` turn's recall trace; current writers use
-- it for local `soma recall` diagnostics so the dashboard can show
-- which episodes surfaced for a query and what similarities they got.
--
-- One row per local debug recall. The dashboard's `/api/recall/recent`
-- handler reads the last N rows on every poll. SQLite's WAL keeps
-- writer and dashboard reader consistent without an extra channel.
--
-- Schema design notes:
--
-- * `top_k` is a JSON-encoded array of `(episode_id, raw_sim)`
--   tuples. Storing as JSON keeps the migration cost bounded
--   (no second table + join) and reads ergonomically from any
--   client (Rust serde_json or sqlite's json1 functions).
-- * `response_text` is capped at 8 KB at write time so a runaway
--   ollama generation can't bloat this trace table.
-- * No vacuum / retention policy in v1 — the table is small (one
--   row per turn, ~few KB each) and dashboard reads only the
--   most recent N. A future slow_loop pass can prune by
--   `created_at_ns < now - 7 days` (D162-cand).

CREATE TABLE IF NOT EXISTS chat_recall_trace (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ns   INTEGER NOT NULL,
    session_id      TEXT,
    project         TEXT,
    query_text      TEXT NOT NULL,
    pack_count      INTEGER NOT NULL,
    top_k_json      TEXT NOT NULL,
    response_text   TEXT,
    response_chars  INTEGER NOT NULL,
    duration_ms     INTEGER
);

CREATE INDEX IF NOT EXISTS idx_chat_recall_trace_created_at
    ON chat_recall_trace (created_at_ns DESC);
