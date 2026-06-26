-- Evidence-backed latent proxy and lifecycle transition foundation.
--
-- The four-stage learning hierarchy is not represented by `memory_tier`
-- alone. Keep raw episodes append-only, then attach typed, auditable
-- abstractions and transition events to them. These rows are system-level
-- latent proxies, not model-internal neural latents.

CREATE TABLE IF NOT EXISTS evidence_latent_proxies (
    id                  INTEGER PRIMARY KEY,
    episode_id          INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    proxy_type          TEXT NOT NULL,
    target              TEXT,
    claim               TEXT NOT NULL,
    scope               TEXT,
    confidence          REAL NOT NULL DEFAULT 0.0,
    evidence_refs_json  TEXT NOT NULL,
    memory_layer        TEXT NOT NULL,
    lifecycle_state     TEXT NOT NULL,
    promotion_reason    TEXT,
    envelope_section    TEXT,
    supersedes_proxy_id INTEGER REFERENCES evidence_latent_proxies(id) ON DELETE SET NULL,
    created_at_ns       INTEGER NOT NULL,
    updated_at_ns       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_episode
    ON evidence_latent_proxies(episode_id);

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_layer_state
    ON evidence_latent_proxies(memory_layer, lifecycle_state, updated_at_ns DESC);

CREATE INDEX IF NOT EXISTS idx_evidence_latent_proxies_type_scope
    ON evidence_latent_proxies(proxy_type, scope, updated_at_ns DESC);

CREATE TABLE IF NOT EXISTS memory_lifecycle_events (
    id                 INTEGER PRIMARY KEY,
    proxy_id           INTEGER NOT NULL REFERENCES evidence_latent_proxies(id) ON DELETE CASCADE,
    from_layer         TEXT,
    from_state         TEXT,
    to_layer           TEXT NOT NULL,
    to_state           TEXT NOT NULL,
    transition_reason  TEXT NOT NULL,
    evidence_refs_json TEXT NOT NULL,
    envelope_section   TEXT,
    created_at_ns      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_lifecycle_events_proxy
    ON memory_lifecycle_events(proxy_id, created_at_ns ASC);

CREATE INDEX IF NOT EXISTS idx_memory_lifecycle_events_state
    ON memory_lifecycle_events(to_layer, to_state, created_at_ns DESC);
