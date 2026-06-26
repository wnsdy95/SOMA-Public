-- Operator-confirmed thread identity ledger.
--
-- `soma context thread-identity` starts as a read-only preflight. This ledger
-- records only explicit operator confirmations that a set of captured sessions
-- should be treated as one durable thread identity in future work. Creating a
-- row here does not enable soma://context/thread/<id>, does not auto-merge new
-- sessions, and does not promote or verify memory claims.

CREATE TABLE IF NOT EXISTS thread_identities (
    id                              INTEGER PRIMARY KEY,
    thread_key                      TEXT NOT NULL UNIQUE,
    project                         TEXT NOT NULL,
    status                          TEXT NOT NULL,
    session_ids_json                TEXT NOT NULL,
    evidence_episode_ids_json       TEXT NOT NULL,
    confirmed_by                    TEXT NOT NULL,
    confirmation_reason             TEXT NOT NULL,
    created_at_ns                   INTEGER NOT NULL,
    updated_at_ns                   INTEGER NOT NULL,
    CHECK(status IN ('operator_confirmed', 'disabled'))
);

CREATE TABLE IF NOT EXISTS thread_identity_members (
    thread_identity_id              INTEGER NOT NULL REFERENCES thread_identities(id) ON DELETE CASCADE,
    session_id                      TEXT NOT NULL,
    episode_id                      INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    source                          TEXT NOT NULL,
    added_at_ns                     INTEGER NOT NULL,
    PRIMARY KEY(thread_identity_id, episode_id)
);

CREATE INDEX IF NOT EXISTS idx_thread_identities_project_status
    ON thread_identities(project, status, updated_at_ns DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_thread_identity_members_session
    ON thread_identity_members(session_id, thread_identity_id);

CREATE INDEX IF NOT EXISTS idx_thread_identity_members_episode
    ON thread_identity_members(episode_id);
