-- Migration 0005 — bootstrap the user_profile_centroid row in
-- self_state.
--
-- Discussion 0037 §D90 / ADR 0004 §A. The centroid is the EMA of
-- recent episode embeddings used as the `self_relevance` axis of
-- the salience kernel. It lives under the canonical
-- (kind='profile', key='user_centroid') tuple — so it inherits
-- self_state's UNIQUE (kind, key) UPSERT semantics (orthogonal-fact
-- principle from O-LoRA insight, ADR 0004 §D).
--
-- value_json shape:
--   {
--     "dim": 384,
--     "centroid_b64": "<base64 little-endian f32[dim]>",
--     "episode_count": 0
--   }
--
-- Empty `centroid_b64` means "not yet primed" — salience kernel
-- treats this as `user_profile_centroid: None` and forces
-- self_relevance to 1.0 (every episode is novel against an empty
-- profile).
--
-- Append-only — never edit this file after landing.

INSERT OR IGNORE INTO self_state (kind, key, value_json, evidence_ids, computed_at_ns)
VALUES (
    'profile',
    'user_centroid',
    json_object('dim', 384, 'centroid_b64', '', 'episode_count', 0),
    '[]',
    0
);
