-- Add client-rendered review-action proof support to the client binding ledger.
--
-- `observed_review_action` is intentionally stronger than in-client render
-- visibility. It requires an operator-confirmed `soma_review_action` report
-- whose rendered `control_id` was present in a prior in-client render proof.

CREATE TABLE IF NOT EXISTS client_binding_proofs_v35 (
    id                          INTEGER PRIMARY KEY,
    client                      TEXT NOT NULL,
    proof_level                 TEXT NOT NULL,
    manifest_path               TEXT NOT NULL,
    manifest_status             TEXT NOT NULL,
    evidence_source             TEXT NOT NULL,
    event_jsonl_path            TEXT,
    installed_config_path       TEXT,
    render_evidence_path        TEXT,
    review_action_report_path   TEXT,
    drain_report_json           TEXT,
    review_render_json          TEXT,
    trust_boundary              TEXT NOT NULL,
    checks_json                 TEXT NOT NULL,
    observed_at_ns              INTEGER NOT NULL,
    created_at_ns               INTEGER NOT NULL,
    CHECK(proof_level IN (
        'reference_binding',
        'observed_event_file',
        'observed_app_hook',
        'observed_in_client_render',
        'observed_review_action'
    ))
);

INSERT INTO client_binding_proofs_v35 (
    id,
    client,
    proof_level,
    manifest_path,
    manifest_status,
    evidence_source,
    event_jsonl_path,
    installed_config_path,
    render_evidence_path,
    review_action_report_path,
    drain_report_json,
    review_render_json,
    trust_boundary,
    checks_json,
    observed_at_ns,
    created_at_ns
)
SELECT
    id,
    client,
    proof_level,
    manifest_path,
    manifest_status,
    evidence_source,
    event_jsonl_path,
    installed_config_path,
    render_evidence_path,
    NULL,
    drain_report_json,
    review_render_json,
    trust_boundary,
    checks_json,
    observed_at_ns,
    created_at_ns
FROM client_binding_proofs;

DROP TABLE client_binding_proofs;
ALTER TABLE client_binding_proofs_v35 RENAME TO client_binding_proofs;

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_client_level
    ON client_binding_proofs(client, proof_level, observed_at_ns DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_created
    ON client_binding_proofs(created_at_ns DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_installed_config
    ON client_binding_proofs(client, proof_level, installed_config_path);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_render_evidence
    ON client_binding_proofs(client, proof_level, render_evidence_path);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_review_action_report
    ON client_binding_proofs(client, proof_level, review_action_report_path);
