-- D149 close — retroactive cleanup of pre-fix capture noise.
--
-- Before commit ac20b99 (v1.0 'fix(stop-hook): jq frame filter')
-- the Claude Code stop-hook captured raw transcript JSON which
-- included the IDE's frame markers (`<system-reminder>`,
-- `</task-notification>`, `<command-name>`) inside `prompt_text`.
-- These episodes pollute recall — semantic search surfaces a
-- system-reminder text instead of the user's actual question, and
-- the legacy context/profile preview echoes IDE meta-instructions
-- back to the user.
--
-- The fix from ac20b99 forward stops new noise. This migration
-- soft-deletes the historical residue:
-- * `forgotten_at_ns = now` so recall paths skip these rows.
-- * `forgotten_reason = 'cleanup:capture-noise'` so an audit
--   query (`SELECT id FROM episodes WHERE forgotten_reason =
--   'cleanup:capture-noise'`) can identify the cohort and operators
--   can un-forget if desired.
-- * Idempotent: rows already forgotten are skipped (the WHERE
--   filters them out via `forgotten_at_ns IS NULL`).
-- * Migration ledger ensures this runs exactly once per DB.

UPDATE episodes
   SET forgotten_at_ns = CAST(strftime('%s', 'now') AS INTEGER) * 1000000000,
       forgotten_reason = 'cleanup:capture-noise'
 WHERE forgotten_at_ns IS NULL
   AND prompt_text IS NOT NULL
   AND (prompt_text LIKE '%<system-reminder>%'
        OR prompt_text LIKE '%</task-notification>%'
        OR prompt_text LIKE '%<command-name>%');
