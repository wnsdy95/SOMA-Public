-- Asynchronous learning critic proposal queue.
--
-- The sync critic captures cloud output as draft claims. The async learning
-- critic may later propose lifecycle changes, decay, or verification requests,
-- but a proposal is not itself verification and must not mutate durable memory.

CREATE TABLE IF NOT EXISTS learning_critic_proposals (
    id                      INTEGER PRIMARY KEY,
    task_frame_id           INTEGER REFERENCES task_frames(id) ON DELETE SET NULL,
    action                  TEXT NOT NULL,
    claim_ids_json          TEXT NOT NULL,
    target_lifecycle_state  TEXT,
    reason                  TEXT NOT NULL,
    evidence_refs_json      TEXT NOT NULL,
    status                  TEXT NOT NULL,
    result_json             TEXT,
    created_at_ns           INTEGER NOT NULL,
    updated_at_ns           INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_learning_critic_proposals_status
    ON learning_critic_proposals(status, created_at_ns ASC, id ASC);

CREATE INDEX IF NOT EXISTS idx_learning_critic_proposals_task_frame
    ON learning_critic_proposals(task_frame_id, created_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_learning_critic_proposals_action
    ON learning_critic_proposals(action, status, created_at_ns DESC);
