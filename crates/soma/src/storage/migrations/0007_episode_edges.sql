-- Migration 0007 — undirected episode-similarity graph.
--
-- Discussion 0037 §D92 / ADR 0004 §C. HippoRAG-light: at ingest
-- time we record cosine-similar episode pairs as graph edges; at
-- recall time, multi-hop traversal seeded by 1-NN matches surfaces
-- episodes that direct vector search would miss.
--
-- The (src_id < dst_id) CHECK enforces undirected storage: each
-- pair lives in exactly one row. The (src, sim) and (dst, sim)
-- indexes let recall walk neighbors of a seeded node fast.
--
-- Append-only — never edit this file after landing.

CREATE TABLE IF NOT EXISTS episode_edges (
    src_id          INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    dst_id          INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    similarity      REAL    NOT NULL,
    created_at_ns   INTEGER NOT NULL,
    PRIMARY KEY (src_id, dst_id),
    CHECK (src_id < dst_id)
);

CREATE INDEX IF NOT EXISTS idx_episode_edges_src ON episode_edges(src_id);
CREATE INDEX IF NOT EXISTS idx_episode_edges_dst ON episode_edges(dst_id);
CREATE INDEX IF NOT EXISTS idx_episode_edges_sim ON episode_edges(similarity DESC);
