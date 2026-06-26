-- First-class TaskFrame persistence for the local control plane.
--
-- A TaskFrame records SOMA's pre-cloud-call judgment state. Keep the full
-- local form separate from the cloud-redacted projection so local reasoning can
-- be audited without leaking local-only context to cloud clients.

CREATE TABLE IF NOT EXISTS task_frames (
    id                   INTEGER PRIMARY KEY,
    hash                 TEXT NOT NULL UNIQUE,
    builder_version      TEXT NOT NULL,
    local_full_json      TEXT NOT NULL,
    cloud_redacted_json  TEXT NOT NULL,
    scope_json           TEXT NOT NULL,
    project              TEXT,
    session_id           TEXT,
    work_mode            TEXT NOT NULL,
    goal_state           TEXT NOT NULL,
    evidence_refs_json   TEXT NOT NULL,
    privacy_labels_json  TEXT NOT NULL,
    blocked_fields_json  TEXT NOT NULL,
    created_at_ns        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_task_frames_created
    ON task_frames(created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_task_frames_project_created
    ON task_frames(project, created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_task_frames_session_created
    ON task_frames(session_id, created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_task_frames_work_mode_created
    ON task_frames(work_mode, created_at_ns DESC);
