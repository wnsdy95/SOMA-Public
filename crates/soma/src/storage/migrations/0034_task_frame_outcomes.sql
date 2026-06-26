CREATE TABLE task_frame_outcomes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_frame_id INTEGER NOT NULL REFERENCES task_frames(id) ON DELETE RESTRICT,
    outcome_type TEXT NOT NULL CHECK (
        outcome_type IN (
            'accepted',
            'revised',
            'rejected',
            'verified',
            'applied',
            'failed',
            'abandoned'
        )
    ),
    summary TEXT NOT NULL,
    evidence_refs_json TEXT NOT NULL,
    claim_ids_json TEXT NOT NULL DEFAULT '[]',
    proposal_ids_json TEXT NOT NULL DEFAULT '[]',
    latent_proxy_ids_json TEXT NOT NULL DEFAULT '[]',
    created_at_ns INTEGER NOT NULL
);

CREATE INDEX idx_task_frame_outcomes_task_frame
    ON task_frame_outcomes(task_frame_id, created_at_ns DESC, id DESC);

CREATE INDEX idx_task_frame_outcomes_type
    ON task_frame_outcomes(outcome_type, created_at_ns DESC, id DESC);
