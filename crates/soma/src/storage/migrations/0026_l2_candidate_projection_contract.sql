-- L2 candidate projection contract hardening.
--
-- Phase 2 requires short-term candidates to carry expiry, privacy, and
-- source-trust metadata so candidate projection remains fail-closed and cannot
-- be confused with durable evidence.

ALTER TABLE evidence_latent_proxies
    ADD COLUMN expires_at_ns INTEGER;

ALTER TABLE evidence_latent_proxies
    ADD COLUMN privacy_labels_json TEXT NOT NULL DEFAULT '["project_internal"]';

ALTER TABLE evidence_latent_proxies
    ADD COLUMN source_trust TEXT NOT NULL DEFAULT 'local_observed';

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_expiry
    ON evidence_latent_proxies(lifecycle_state, expires_at_ns, updated_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_source_trust
    ON evidence_latent_proxies(source_trust, lifecycle_state, updated_at_ns DESC);
