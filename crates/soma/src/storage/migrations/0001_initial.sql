-- Migration 0001 — initial schema.
--
-- v1 reboot baseline. Owns the `episodes` table with G6 canonical
-- columns per discussion 0024 §D (AI + Terminal episode shapes
-- share the table; prompt_text / response_text / command / stdout
-- all NULLABLE; session_id / digest NEW in v1).
--
-- The runner-owned `schema_version` ledger (discussion 0024 §B /
-- WS 3 PR 3.1 §C2) is created + populated by
-- `storage::migrations::bootstrap_schema_version` +
-- `apply_migration`; migration SQL files never touch it.
--
-- Append-only: never edit this file after landing. Schema changes
-- ship as new numbered migrations.

CREATE TABLE IF NOT EXISTS episodes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Temporal
    ts_start_ns     INTEGER NOT NULL,
    ts_end_ns       INTEGER NOT NULL,
    duration_ms     INTEGER NOT NULL,
    -- Source classification
    source          TEXT    NOT NULL,
    session_id      TEXT,
    -- Capture payload (AI / Terminal share — shape picked by source)
    prompt_text     TEXT,
    response_text   TEXT,
    command         TEXT,
    stdout          BLOB,
    exit_code       INTEGER,
    -- Provenance
    cwd             TEXT,
    git_branch      TEXT,
    project         TEXT,
    -- Memory kernel derived (filled by warm loop)
    memory_tier     TEXT    NOT NULL DEFAULT 'short',
    salience        REAL,
    digest          TEXT
);

CREATE INDEX IF NOT EXISTS idx_episodes_ts      ON episodes(ts_start_ns DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_proj    ON episodes(project, ts_start_ns DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_branch  ON episodes(git_branch, ts_start_ns DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_tier    ON episodes(memory_tier);
CREATE INDEX IF NOT EXISTS idx_episodes_session ON episodes(session_id, ts_start_ns DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_source  ON episodes(source);
