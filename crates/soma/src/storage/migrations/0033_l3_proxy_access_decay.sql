-- L3 proxy access/decay lifecycle metadata.
--
-- Long-term episodic proxies should not be a static promoted bucket. Projection
-- records access evidence, and stale low-access L3 rows can transition to
-- decayed without losing the original episode evidence or lifecycle events.

ALTER TABLE evidence_latent_proxies
    ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE evidence_latent_proxies
    ADD COLUMN last_accessed_at_ns INTEGER;

ALTER TABLE evidence_latent_proxies
    ADD COLUMN decay_score REAL NOT NULL DEFAULT 1.0;

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_l3_access
    ON evidence_latent_proxies(
        memory_layer,
        lifecycle_state,
        envelope_section,
        decay_score DESC,
        access_count DESC,
        updated_at_ns DESC
    );

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_decay_candidates
    ON evidence_latent_proxies(
        memory_layer,
        lifecycle_state,
        last_accessed_at_ns,
        updated_at_ns,
        access_count
    );
