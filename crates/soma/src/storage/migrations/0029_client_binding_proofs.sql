-- Observed client binding proof ledger.
--
-- Reference manifests and wrapper smokes do not prove that a private editor app
-- installed or called a hook. This table records the exact proof level and
-- source used for each client binding observation so product/docs surfaces can
-- distinguish reference contracts from real app-hook evidence.

CREATE TABLE IF NOT EXISTS client_binding_proofs (
    id                          INTEGER PRIMARY KEY,
    client                      TEXT NOT NULL,
    proof_level                 TEXT NOT NULL,
    manifest_path               TEXT NOT NULL,
    manifest_status             TEXT NOT NULL,
    evidence_source             TEXT NOT NULL,
    event_jsonl_path            TEXT,
    drain_report_json           TEXT,
    review_render_json          TEXT,
    trust_boundary              TEXT NOT NULL,
    checks_json                 TEXT NOT NULL,
    observed_at_ns              INTEGER NOT NULL,
    created_at_ns               INTEGER NOT NULL,
    CHECK(proof_level IN ('reference_binding', 'observed_event_file', 'observed_app_hook'))
);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_client_level
    ON client_binding_proofs(client, proof_level, observed_at_ns DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_created
    ON client_binding_proofs(created_at_ns DESC, id DESC);
