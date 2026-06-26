//! "Why these items" attribution — discussion 0029 §D.
//!
//! Rule-based, deterministic. Given an episode + its similarity
//! score (if semantic) + the query (if any), emit a short string
//! that explains why the entry is in the pack. No LLM, no
//! randomness — the same inputs produce the same output.
//!
//! v1 signals:
//! * semantic similarity value (when `similarity.is_some()`),
//! * same-project (`ep.project` matches the query's `project=...`
//!   prefix — not implemented in v1 because `query` is free-form
//!   text; saved as D78-cand),
//! * same-session (not implemented in v1 — session tracking comes
//!   with the MCP resource context in Phase 5),
//! * recency (applied implicitly: `recent` layer always gets the
//!   "within last N episodes" phrasing).
//!
//! When no signal applies, falls back to `"selected by recency"`.

use crate::storage::StoredEpisode;

/// Assemble the `why` line for one pack entry.
///
/// Signal priority: semantic similarity > recency. Future Phase 5
/// swaps in a richer `ContextAssembler` trait that can inject
/// self-model signals (`"matches your tool use pattern"` etc).
pub fn attribute(_ep: &StoredEpisode, similarity: Option<f32>, _query: Option<&str>) -> String {
    if let Some(s) = similarity {
        return format!("semantic similarity {s:.3}");
    }
    "selected by recency".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EpisodeSource, StoredEpisode};

    fn dummy_episode() -> StoredEpisode {
        StoredEpisode {
            id: 42,
            ts_start_ns: 0,
            ts_end_ns: 0,
            duration_ms: 0,
            source: EpisodeSource::ClaudeCode,
            session_id: None,
            prompt_text: Some("hello".into()),
            response_text: None,
            command: None,
            stdout: None,
            exit_code: None,
            cwd: None,
            git_branch: None,
            project: None,
            memory_tier: "short".into(),
            salience: None,
            digest: None,
        }
    }

    #[test]
    fn test_attribute_includes_similarity_for_semantic_entry() {
        let ep = dummy_episode();
        let w = attribute(&ep, Some(0.876), Some("hello"));
        assert!(w.contains("0.876"), "similarity value must appear in why: {w}");
        assert!(w.contains("similarity"), "attribution must name the `similarity` signal: {w}");
    }

    #[test]
    fn test_attribute_recency_fallback_for_no_similarity() {
        let ep = dummy_episode();
        let w = attribute(&ep, None, None);
        assert_eq!(w, "selected by recency");
    }
}
