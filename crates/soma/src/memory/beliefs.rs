//! D84 close — belief candidates seeder + types.
//!
//! Pulls a window of recent episodes from storage, computes pairwise
//! cosine similarity from the primary-model vectors, and seeds rows
//! into `belief_candidates` for pairs that meet the threshold +
//! either share an outcome (corroborates) or diverge (contradicts).
//!
//! v1 algorithm intentionally simple — no LLM, no embedding model
//! retraining. Just structural rules over (cosine, command, exit_code).
//! Slow_loop calls this once per cycle (integration in coordinator
//! follow-up commit; this unit only ships the module + storage CRUD).
//!
//! ## Algorithm (rejected alternatives in commit message)
//!
//! For each pair (a, b) in the window with `a.id < b.id`:
//!
//! 1. Compute `cos = dot(vec_a, vec_b)` (vectors are L2-normalized at
//!    storage so dot = cosine).
//! 2. Skip if `cos < sim_threshold`.
//! 3. Classify:
//!    * **Same non-empty command** + opposite exit-code sign →
//!      `Contradicts`, evidence `"command flapping"`.
//!    * **Same non-empty command** + same exit-code sign →
//!      `Corroborates`, evidence `"command-and-outcome match"`.
//!    * **Both commands empty** (AI-source episodes) + cos ≥ threshold
//!      → `Corroborates`, evidence `"high-cosine-pair"`.
//!    * **Different non-empty commands** + cos ≥ threshold →
//!      `Corroborates`, evidence `"high-cosine-pair"`.
//! 4. Insert via `Storage::insert_belief_candidate` (UNIQUE constraint
//!    deduplicates idempotently — a second seed call on the same
//!    window returns 0 new rows).
//!
//! Exit-code sign convention: `0` is success, any non-zero (positive
//! or negative) is failure. `None` (unknown) is treated as `0` to
//! avoid spurious contradictions on AI-source episodes which never
//! carry an exit code. Pairs where one side has `None` and the other
//! has `Some(0)` therefore corroborate; one `None` + one `Some(7)`
//! contradict iff commands also match. (This keeps AI / terminal
//! crossover from producing false contradictions.)

use crate::storage::{Storage, StorageError};
use std::str::FromStr;

/// Typed kind discriminator persisted as TEXT in `belief_candidates.kind`.
/// Wire shape matches `episodes.source` convention — lowercase ASCII
/// kebab-free single tokens at the SQLite boundary, typed enum in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeliefKind {
    Corroborates,
    Contradicts,
}

impl std::fmt::Display for BeliefKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BeliefKind::Corroborates => "corroborates",
            BeliefKind::Contradicts => "contradicts",
        })
    }
}

/// FromStr error — single leg, since the only rejection is "string
/// outside the canonical {`corroborates`, `contradicts`} set". Carries
/// the offending input verbatim so call-site error messages quote it.
#[derive(Debug)]
pub struct ParseBeliefKindError(String);

impl std::fmt::Display for ParseBeliefKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid belief kind: {}", self.0)
    }
}

impl std::error::Error for ParseBeliefKindError {}

impl FromStr for BeliefKind {
    type Err = ParseBeliefKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "corroborates" => Ok(BeliefKind::Corroborates),
            "contradicts" => Ok(BeliefKind::Contradicts),
            other => Err(ParseBeliefKindError(other.to_string())),
        }
    }
}

/// Read-side row shape — what `Storage::get_belief_candidates_for_episode`
/// + `Storage::recent_contradictions` return.
#[derive(Debug, Clone)]
pub struct BeliefCandidate {
    pub id: i64,
    pub episode_a_id: i64,
    pub episode_b_id: i64,
    pub kind: BeliefKind,
    pub score: f32,
    pub evidence: Option<String>,
    pub created_at_ns: i64,
    pub forgotten_at_ns: Option<i64>,
    pub resolved_at_ns: Option<i64>,
    pub resolved_by_correction_episode_id: Option<i64>,
}

/// Seed belief candidates from a window of recent episodes.
///
/// Returns the number of NEW rows inserted (UNIQUE constraint
/// deduplicates; a second call on the same window returns 0).
///
/// `window` caps how many recent episodes participate; `sim_threshold`
/// is the cosine cutoff below which pairs are silently skipped.
/// Failure modes propagate as `StorageError` — vector dim mismatch
/// (logged + skipped, not an error) is the only soft case; storage
/// I/O errors abort the seed pass.
pub fn seed_belief_candidates(
    storage: &mut Storage,
    window: usize,
    sim_threshold: f32,
) -> Result<usize, StorageError> {
    if window == 0 {
        return Ok(0);
    }

    // 1. Pull the most recent `window` episodes; we need id, command,
    //    exit_code. recent_episodes already filters forgotten.
    let episodes = storage.recent_episodes(window)?;
    if episodes.len() < 2 {
        return Ok(0);
    }

    // 2. Pull primary-model vectors for the active embedder. Storage
    //    L2-normalizes at write so dot = cosine.
    let model_id = crate::memory::embed::select_embedder().model_id();
    let vec_rows = storage.vectors_for_model(model_id)?;
    // Index by episode id for O(1) lookup during pair iteration.
    let mut vector_by_id: std::collections::HashMap<i64, Vec<f32>> =
        std::collections::HashMap::with_capacity(vec_rows.len());
    for (id, v) in vec_rows {
        vector_by_id.insert(id, v);
    }

    // 3. Compose `(id, command, exit_code, &vec)` joined-tuples for
    //    the window. Episodes without a vector (rare — terminal
    //    captures with empty payload skip embedding) are dropped from
    //    the seed pass; they can't contribute to similarity scoring.
    let candidates: Vec<(i64, &str, Option<i32>, &Vec<f32>)> = episodes
        .iter()
        .filter_map(|ep| {
            let vec = vector_by_id.get(&ep.id)?;
            // Treat None / empty command as "empty" — AI-source rows
            // never carry a command. The classification rules below
            // group both empty halves together as the "AI/AI" pair
            // case.
            let cmd = ep.command.as_deref().unwrap_or("");
            Some((ep.id, cmd, ep.exit_code, vec))
        })
        .collect();

    if candidates.len() < 2 {
        return Ok(0);
    }

    let mut new_rows: usize = 0;

    // 4. For each pair (a, b) where a.id < b.id, classify + insert.
    //    Outer loop keys off `candidates[i]`; inner runs from i+1.
    //    Canonicalize (a_id, b_id) so a_id < b_id — keeps the UNIQUE
    //    constraint cooperating with caller-symmetric pair input.
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            let (id_x, cmd_x, exit_x, vec_x) = candidates[i];
            let (id_y, cmd_y, exit_y, vec_y) = candidates[j];

            // Skip if vector dim mismatched (corrupt write). The
            // salience kernel emits warnings on shape mismatch
            // elsewhere; a quiet skip here avoids duplicate log spam.
            if vec_x.len() != vec_y.len() || vec_x.is_empty() {
                continue;
            }

            let cos = dot(vec_x, vec_y);
            if cos < sim_threshold {
                continue;
            }

            // Canonicalize ordering so UNIQUE(a_id, b_id, kind) is
            // direction-stable regardless of insertion order.
            let (a_id, a_cmd, a_exit, b_id, b_cmd, b_exit) = if id_x < id_y {
                (id_x, cmd_x, exit_x, id_y, cmd_y, exit_y)
            } else {
                (id_y, cmd_y, exit_y, id_x, cmd_x, exit_x)
            };

            let (kind, evidence) = classify_pair(a_cmd, a_exit, b_cmd, b_exit);

            // None on the insert = UNIQUE collision (a previous
            // slow_loop pass already seeded this pair); idempotent skip.
            if storage.insert_belief_candidate(a_id, b_id, kind, cos, Some(evidence))?.is_some() {
                new_rows += 1;
            }
        }
    }

    tracing::info!(
        new_rows,
        candidates = candidates.len(),
        sim_threshold,
        "seeded belief candidates"
    );

    Ok(new_rows)
}

/// Decide `(BeliefKind, evidence)` for a pair whose cosine has already
/// cleared the threshold. Pure function — exposed `pub(crate)` for
/// unit-test reach without round-tripping through SQLite.
pub(crate) fn classify_pair(
    a_cmd: &str,
    a_exit: Option<i32>,
    b_cmd: &str,
    b_exit: Option<i32>,
) -> (BeliefKind, &'static str) {
    let a_empty = a_cmd.is_empty();
    let b_empty = b_cmd.is_empty();

    // Both commands empty (AI-source pair) — high cosine alone is the
    // signal. Cannot contradict: AI conversations may agree or diverge
    // semantically, but exit-code sign is meaningless for them.
    if a_empty && b_empty {
        return (BeliefKind::Corroborates, "high-cosine-pair");
    }

    // One side empty, one side commanded — different "shapes" of
    // episode. Treat as high-cosine corroboration; we lack the symmetric
    // outcome signal needed to claim contradiction.
    if a_empty != b_empty {
        return (BeliefKind::Corroborates, "high-cosine-pair");
    }

    // Both non-empty. Compare command strings exactly.
    if a_cmd == b_cmd {
        // Same command — outcome sign is the discriminator. None is
        // treated as 0 (success) so AI/terminal crossover (rare in
        // this branch since both have non-empty commands) does not
        // produce false contradictions.
        let a_sign = exit_sign(a_exit);
        let b_sign = exit_sign(b_exit);
        if a_sign == b_sign {
            (BeliefKind::Corroborates, "command-and-outcome match")
        } else {
            (BeliefKind::Contradicts, "command flapping")
        }
    } else {
        // Different commands but cosine cleared the threshold — likely
        // the same intent expressed differently (e.g. `cargo build`
        // vs `cargo build --release`). Surface as corroboration; the
        // operator can downgrade if they disagree.
        (BeliefKind::Corroborates, "high-cosine-pair")
    }
}

/// `exit_code` → sign: 0 (or None) = success, anything else = failure.
/// Returns `0_i32` for success, `1_i32` for failure so the signs
/// compare with `==` rather than wrestling with `i32::signum`'s
/// `-1 / 0 / 1` codomain (a `-7` exit code and `+7` exit code both
/// mean "crashed", and we treat them the same).
fn exit_sign(exit: Option<i32>) -> i32 {
    match exit {
        None => 0,
        Some(0) => 0,
        Some(_) => 1,
    }
}

/// Cosine = dot product on unit-normalized vectors. The salience
/// module's `is_unit_normalized` guard runs at storage write time;
/// here we trust that invariant and skip the renormalize cost.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "dot: dim mismatch (caller must check)");
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_same_command_same_sign_corroborates() {
        let (k, ev) = classify_pair("cargo build", Some(0), "cargo build", Some(0));
        assert_eq!(k, BeliefKind::Corroborates);
        assert_eq!(ev, "command-and-outcome match");

        let (k, ev) = classify_pair("cargo build", Some(7), "cargo build", Some(2));
        assert_eq!(k, BeliefKind::Corroborates, "two failures = corroborate");
        assert_eq!(ev, "command-and-outcome match");
    }

    #[test]
    fn classify_same_command_opposite_sign_contradicts() {
        let (k, ev) = classify_pair("cargo build", Some(0), "cargo build", Some(7));
        assert_eq!(k, BeliefKind::Contradicts);
        assert_eq!(ev, "command flapping");

        let (k, ev) = classify_pair("cargo build", Some(7), "cargo build", Some(0));
        assert_eq!(k, BeliefKind::Contradicts);
        assert_eq!(ev, "command flapping");
    }

    #[test]
    fn classify_different_commands_corroborates_high_cosine() {
        let (k, ev) = classify_pair("cargo build", Some(0), "cargo test", Some(0));
        assert_eq!(k, BeliefKind::Corroborates);
        assert_eq!(ev, "high-cosine-pair");
    }

    #[test]
    fn classify_both_empty_corroborates_high_cosine() {
        let (k, ev) = classify_pair("", None, "", None);
        assert_eq!(k, BeliefKind::Corroborates);
        assert_eq!(ev, "high-cosine-pair");
    }

    #[test]
    fn classify_one_empty_corroborates_high_cosine() {
        let (k, ev) = classify_pair("cargo build", Some(0), "", None);
        assert_eq!(k, BeliefKind::Corroborates);
        assert_eq!(ev, "high-cosine-pair");
    }

    #[test]
    fn exit_sign_treats_none_as_success() {
        assert_eq!(exit_sign(None), 0);
        assert_eq!(exit_sign(Some(0)), 0);
        assert_eq!(exit_sign(Some(1)), 1);
        assert_eq!(exit_sign(Some(-7)), 1);
    }

    #[test]
    fn belief_kind_display_is_kebab_lower() {
        assert_eq!(BeliefKind::Corroborates.to_string(), "corroborates");
        assert_eq!(BeliefKind::Contradicts.to_string(), "contradicts");
    }

    #[test]
    fn belief_kind_from_str_canonical_inputs() {
        assert_eq!(BeliefKind::from_str("corroborates").unwrap(), BeliefKind::Corroborates);
        assert_eq!(BeliefKind::from_str("contradicts").unwrap(), BeliefKind::Contradicts);
        assert!(BeliefKind::from_str("unknown").is_err());
    }
}
