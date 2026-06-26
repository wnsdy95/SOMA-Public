-- Migration 0008 — repeated-episode compression metadata.
--
-- Discussion 0037 §E / ADR 0004 §E. HEN (Kashyap 2024) + Santos
-- 2025 continuous-time compression: instead of storing 50 nearly-
-- identical "cargo test" episodes as 50 SQLite rows, collapse to
-- a single representative + count.
--
-- summary_count tracks how many real ingests this row represents
-- (default 1 = an uncompressed episode). summary_signature is a
-- SHA256 hex of (cmd, project, exit_code) so the slow_loop's
-- compression pass can group candidates fast (indexed).
--
-- Append-only — never edit this file after landing.

ALTER TABLE episodes ADD COLUMN summary_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE episodes ADD COLUMN summary_signature TEXT;

CREATE INDEX IF NOT EXISTS idx_episodes_summary_signature
    ON episodes(summary_signature)
    WHERE summary_signature IS NOT NULL;
