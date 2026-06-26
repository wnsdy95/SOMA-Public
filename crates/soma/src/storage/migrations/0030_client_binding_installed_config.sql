-- Installed client config evidence for app-hook binding proofs.
--
-- `observed_app_hook` is stronger than an event-file observation. It requires
-- evidence that the user's client configuration or hook file points at SOMA's
-- lifecycle/spool wrapper. Store the path, not the file contents, to avoid
-- copying editor config secrets into the ledger.

ALTER TABLE client_binding_proofs
    ADD COLUMN installed_config_path TEXT;

CREATE INDEX IF NOT EXISTS idx_client_binding_proofs_installed_config
    ON client_binding_proofs(client, proof_level, installed_config_path);
