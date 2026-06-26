//! Salience kernel — scalar episode salience plus optional iPC diagnostic.
//!
//! Discussion 0037 §D90. Replaces the v1 stub with a deterministic,
//! frozen-weights, ML-free implementation that takes the *math* of:
//!
//! * Salvatori 2024 (iPC) — optional `Free Energy F = Σ‖ε_l‖²`
//!   diagnostic, kept only if it becomes cited ContextEnvelope
//!   anomaly/conflict context.
//! * Hopfield-attention (Ramsauer 2020) — vectors live on the unit
//!   sphere; cosine distance is the natural error metric.
//! * Premakumar/Graziano 2024 — self-prediction error is an
//!   independent salience axis (`self_relevance`).
//! * MemoryBank/MemMamba — duration-weighted recency drives the
//!   `duration` component.
//!
//! Five components feed a `softmax(weights) · components` reduction
//! into a scalar `free_energy ∈ [0, 1]`. Weights are config knobs
//! (`SalienceWeights`) so an operator can tune the kernel without a
//! code change. Default weights match the discussion lock.

use crate::storage::EpisodeId;

/// Storage / ContextEnvelope wire kind for iPC free-energy anomalies.
pub const IPC_FREE_ENERGY_ANOMALY_KIND: &str = "ipc_free_energy";

/// First-pass iPC anomaly gate. `pc_free_energy` is an unbounded
/// squared-error sum, but the current normalized hash/e5 embeddings
/// usually land near `[0, 1]` for the frozen hierarchy. Keep this
/// conservative and bounded by tests before promoting iPC beyond
/// connected-candidate.
pub const IPC_FREE_ENERGY_ANOMALY_THRESHOLD: f32 = 0.75;

/// Per-component salience score. All components are bounded to
/// `[0, 1]` so the softmax reduction is well-defined regardless of
/// the input embeddings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SalienceScore {
    /// `1 - cos(embed, recent_ema)`. Prediction error against the
    /// running EMA of recent episodes (iPC §3.1).
    pub surprise: f32,
    /// `1 - cos(embed, nearest_neighbor)`. Distinctness from the
    /// closest stored memory (Hopfield §1).
    pub novelty: f32,
    /// `1 - cos(content_emb, context_emb)`. Top-down vs bottom-up
    /// divergence (DBPC §3.3).
    pub contradiction: f32,
    /// `1 - cos(embed, user_profile_centroid)`. Self-prediction
    /// error (Premakumar §3.5).
    pub self_relevance: f32,
    /// `sigmoid((duration_ms - 1000) / 5000)`. Sub-second events
    /// score ~0; multi-second events score ~1 (memory traces are
    /// stronger for engagement events).
    pub duration: f32,
    /// Softmax-weighted reduction of the five components.
    /// Produced inside [`score`] so callers cannot fabricate an
    /// inconsistent value.
    pub free_energy: f32,
    /// Optional iPC total Free Energy `F = Σ ‖ε_l‖²` from the paper-
    /// verified `PaperPC` predictor (Salvatori 2024 iPC). `None`
    /// when the `cognitive` cargo feature is off OR the caller
    /// didn't supply a multi-layer `pc_latents`. Current value is the
    /// frozen-weight identity-init forward pass.
    ///
    /// ADR 0015 boundary: this is not consumed by pinning. When it
    /// crosses `IPC_FREE_ENERGY_ANOMALY_THRESHOLD`, ingest persists a
    /// cited `context_anomalies` row that can become a
    /// `ContextEnvelope.open_decisions` anomaly.
    pub pc_free_energy: Option<f32>,
}

/// Hand-tuned softmax weights. Discussion 0037 default = `[0.30,
/// 0.25, 0.20, 0.15, 0.10]` over `(surprise, novelty, contradiction,
/// self_relevance, duration)`. Sum need not equal 1 — softmax
/// normalizes, so the values express *relative importance*.
#[derive(Debug, Clone, Copy)]
pub struct SalienceWeights {
    pub surprise: f32,
    pub novelty: f32,
    pub contradiction: f32,
    pub self_relevance: f32,
    pub duration: f32,
}

impl SalienceWeights {
    pub const fn v1_default() -> Self {
        Self {
            surprise: 0.30,
            novelty: 0.25,
            contradiction: 0.20,
            self_relevance: 0.15,
            duration: 0.10,
        }
    }
}

impl Default for SalienceWeights {
    fn default() -> Self {
        Self::v1_default()
    }
}

/// Frozen recall context — what the kernel needs to score one
/// episode. Built once per ingest by `SalienceContext::from_storage`
/// then reused for the score.
pub struct SalienceContext<'a> {
    /// Weighted EMA of recent episode embeddings. `None` means the
    /// store has fewer than 2 episodes — surprise is forced to 1.0
    /// (everything looks novel against an empty history).
    pub recent_ema: Option<&'a [f32]>,
    /// User profile centroid from `self_state`. `None` ⇒
    /// self_relevance is forced to 1.0 (no profile yet).
    pub user_profile_centroid: Option<&'a [f32]>,
    /// HNSW 1-nearest neighbor's embedding. `None` ⇒ novelty is 1.0
    /// (first episode of its kind).
    pub nearest_neighbor: Option<&'a [f32]>,
    /// v1.2 chunk 1.5 (ADR 0008 §D5) — content-addressable
    /// `read = q · C / |q · n|` from `TrainableMLstm`. When `Some`,
    /// novelty is computed against this *learned* representation
    /// instead of the raw cosine top-1; the working memory has had
    /// time (slow_loop train cycles) to consolidate similar
    /// episodes into one center, so its read is a stronger
    /// "what's similar to me" signal than HNSW's nearest neighbor.
    /// `None` ⇒ fallback to `nearest_neighbor` semantics (chunk 1.4
    /// 이전 path, identical for v1.1 sessions).
    ///
    /// ADR 0015 boundary: this signal affects ingest salience only.
    /// It is not currently used by ContextEnvelope thread-state
    /// compression or relevant-memory ranking.
    pub working_memory_read: Option<&'a [f32]>,
    /// Query/response context vector (when ingest pairs them).
    /// `None` ⇒ contradiction = 0 (single-text episode).
    pub context_embed: Option<&'a [f32]>,
    /// Multi-layer latents passed to `PaperPC` for the optional iPC
    /// free-energy diagnostic. `None` ⇒ `pc_free_energy` stays
    /// `None` in the output. Only consulted when the `cognitive`
    /// cargo feature is on.
    ///
    /// ADR 0015 boundary: this currently produces an auxiliary
    /// SalienceScore field only, not an envelope-quality output.
    pub pc_latents: Option<&'a [Vec<f32>]>,
}

/// Score a single episode embedding against the supplied context.
///
/// `embed` is assumed L2-normalized — see `Storage::put_vector` for
/// the invariant. The kernel does *not* renormalize because doing
/// so per call would mask a callers' bug.
pub fn score(
    embed: &[f32],
    duration_ms: i64,
    ctx: &SalienceContext,
    weights: &SalienceWeights,
) -> SalienceScore {
    let surprise = ctx.recent_ema.map(|ema| cosine_distance(embed, ema)).unwrap_or(1.0);
    // Prefer the optional mLSTM read for ingest-time novelty when
    // present; fall back to HNSW's raw nearest neighbor otherwise.
    let novelty = ctx
        .working_memory_read
        .or(ctx.nearest_neighbor)
        .map(|reference| cosine_distance(embed, reference))
        .unwrap_or(1.0);
    let contradiction =
        ctx.context_embed.map(|ctx_embed| cosine_distance(embed, ctx_embed)).unwrap_or(0.0);
    let self_relevance =
        ctx.user_profile_centroid.map(|c| cosine_distance(embed, c)).unwrap_or(1.0);
    let duration = duration_score(duration_ms);

    let components = [surprise, novelty, contradiction, self_relevance, duration];
    let weight_vec = [
        weights.surprise,
        weights.novelty,
        weights.contradiction,
        weights.self_relevance,
        weights.duration,
    ];
    let normalized = softmax(&weight_vec);
    let free_energy: f32 = components.iter().zip(normalized.iter()).map(|(c, w)| c * w).sum();

    let pc_free_energy = compute_pc_free_energy(ctx);

    SalienceScore {
        surprise,
        novelty,
        contradiction,
        self_relevance,
        duration,
        free_energy,
        pc_free_energy,
    }
}

/// Invoke PaperPC's free_energy when the `cognitive` feature is on
/// AND the caller supplied multi-layer `pc_latents`.
/// Returns `None` otherwise so SalienceScore.pc_free_energy stays
/// `None` and downstream consumers can detect the augmentation
/// status.
/// ADR 0015 boundary: downstream consumers do not use this for
/// `forgetting::should_pin`, `ContextEnvelope`, or `open_decisions`
/// today.
#[cfg(feature = "cognitive")]
fn compute_pc_free_energy(ctx: &SalienceContext<'_>) -> Option<f32> {
    let latents = ctx.pc_latents?;
    if latents.len() < 2 {
        return None;
    }
    let dims: Vec<usize> = latents.iter().map(|l| l.len()).collect();
    let pc = crate::memory::cognitive::ipc::PaperPC::new(dims);
    Some(pc.free_energy(latents))
}

#[cfg(not(feature = "cognitive"))]
fn compute_pc_free_energy(_ctx: &SalienceContext<'_>) -> Option<f32> {
    None
}

/// Update the EMA of the user profile centroid. v1 uses a simple
/// fixed-α formula: `new = (1 − α) · old + α · sample`. `α = 0.1`
/// gives a half-life around 7 episodes so the centroid follows the
/// user's recent focus without losing stability.
pub fn update_centroid(old: &[f32], sample: &[f32], alpha: f32) -> Vec<f32> {
    if old.is_empty() {
        return l2_normalize(sample);
    }
    if old.len() != sample.len() {
        return l2_normalize(sample);
    }
    let merged: Vec<f32> =
        old.iter().zip(sample.iter()).map(|(o, s)| (1.0 - alpha) * o + alpha * s).collect();
    l2_normalize(&merged)
}

/// `1 - cos(a, b)` clamped to `[0, 1]`. Returns 1.0 (max distance)
/// when either vector is zero-length (a degenerate case the
/// salience kernel treats as "fully novel").
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    // Vectors are L2-normalized at storage; dot is the cosine.
    (1.0 - dot).clamp(0.0, 1.0)
}

/// Smooth duration → [0, 1] sigmoid centered on ~1 second.
fn duration_score(duration_ms: i64) -> f32 {
    let x = (duration_ms as f32 - 1000.0) / 5000.0;
    1.0 / (1.0 + (-x).exp())
}

/// Numerically stable softmax.
///
/// D135-cand close (R9 audit, 2026-04-30) — production callers
/// (salience kernel) pass 5-component vectors. Stack-allocate up
/// to `SMALL_CAPACITY=8` to avoid heap churn in the hot path.
/// Larger inputs fall through to the heap path so the API stays
/// general-purpose.
fn softmax(values: &[f32]) -> Vec<f32> {
    const SMALL_CAPACITY: usize = 8;
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let n = values.len();

    if n <= SMALL_CAPACITY {
        let mut buf: [f32; SMALL_CAPACITY] = [0.0; SMALL_CAPACITY];
        for i in 0..n {
            buf[i] = (values[i] - max).exp();
        }
        let sum: f32 = buf[..n].iter().sum();
        if sum == 0.0 {
            return vec![1.0 / n as f32; n];
        }
        return buf[..n].iter().map(|e| e / sum).collect();
    }

    let exps: Vec<f32> = values.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum == 0.0 {
        return vec![1.0 / n as f32; n];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

/// L2-normalize a vector. Zero-length input returns the canonical
/// basis (`[1, 0, 0, ...]`) — the same degenerate-input contract
/// `HashEmbedder::embed` uses.
pub fn l2_normalize(v: &[f32]) -> Vec<f32> {
    if v.is_empty() {
        return Vec::new();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-9 {
        let mut out = vec![0.0; v.len()];
        out[0] = 1.0;
        return out;
    }
    v.iter().map(|x| x / norm).collect()
}

/// Test whether a vector is approximately L2-unit-norm.
pub fn is_unit_normalized(v: &[f32]) -> bool {
    if v.is_empty() {
        return true;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (norm - 1.0).abs() < 1e-3
}

/// Softmax-weighted retrieval over `(EpisodeId, similarity)` pairs.
/// Returns episodes whose cumulative weight reaches `mass_threshold`
/// (default = 0.95), capped at `max_take`. Ramsauer 2020 §1: the
/// retrieval is a weighted sum, not a top-k selection.
///
/// Output triple = `(id, raw_sim, softmax_weight)`. `raw_sim` is the
/// caller's input similarity preserved verbatim (so callers like
/// `pack.rs` can surface the raw cosine on `PackItem.similarity` per
/// D137 contract); `softmax_weight = softmax(β·sim)` steers only the
/// cumulative-mass truncation and is internal to retrieval.
pub fn softmax_weighted_recall(
    raw_hits: &[(EpisodeId, f32)],
    beta: f32,
    mass_threshold: f32,
    max_take: usize,
) -> Vec<(EpisodeId, f32, f32)> {
    if raw_hits.is_empty() || max_take == 0 {
        return Vec::new();
    }
    // Sort similarities (already sorted by SemanticIndex but be
    // defensive — softmax is invariant to order, but cumulative-mass
    // truncation depends on sorted order).
    let mut sorted = raw_hits.to_vec();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let weights: Vec<f32> = sorted.iter().map(|(_, sim)| beta * sim).collect();
    let normalized = softmax(&weights);

    let mut out: Vec<(EpisodeId, f32, f32)> = Vec::with_capacity(max_take.min(sorted.len()));
    let mut cumulative = 0.0;
    for ((id, original_sim), w) in sorted.iter().zip(normalized.iter()) {
        out.push((*id, *original_sim, *w));
        cumulative += w;
        if cumulative >= mass_threshold || out.len() >= max_take {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_x() -> Vec<f32> {
        let mut v = vec![0.0; 384];
        v[0] = 1.0;
        v
    }

    fn unit_y() -> Vec<f32> {
        let mut v = vec![0.0; 384];
        v[1] = 1.0;
        v
    }

    #[test]
    fn surprise_zero_for_identical_embed() {
        let v = unit_x();
        let ctx = SalienceContext {
            recent_ema: Some(&v),
            user_profile_centroid: None,
            nearest_neighbor: None,
            context_embed: None,
            pc_latents: None,
            working_memory_read: None,
        };
        let s = score(&v, 1000, &ctx, &SalienceWeights::v1_default());
        assert!(s.surprise.abs() < 1e-3, "identical embed → surprise 0, got {}", s.surprise);
    }

    #[test]
    fn novelty_one_for_orthogonal_neighbor() {
        let v = unit_x();
        let nn = unit_y();
        let ctx = SalienceContext {
            recent_ema: None,
            user_profile_centroid: None,
            nearest_neighbor: Some(&nn),
            context_embed: None,
            pc_latents: None,
            working_memory_read: None,
        };
        let s = score(&v, 1000, &ctx, &SalienceWeights::v1_default());
        assert!((s.novelty - 1.0).abs() < 1e-3, "orthogonal NN → novelty 1, got {}", s.novelty);
    }

    #[test]
    fn contradiction_zero_when_context_matches_content() {
        let v = unit_x();
        let ctx = SalienceContext {
            recent_ema: None,
            user_profile_centroid: None,
            nearest_neighbor: None,
            context_embed: Some(&v),
            pc_latents: None,
            working_memory_read: None,
        };
        let s = score(&v, 1000, &ctx, &SalienceWeights::v1_default());
        assert!(s.contradiction.abs() < 1e-3);
    }

    #[test]
    fn self_relevance_zero_for_centroid_match() {
        let v = unit_x();
        let ctx = SalienceContext {
            recent_ema: None,
            user_profile_centroid: Some(&v),
            nearest_neighbor: None,
            context_embed: None,
            pc_latents: None,
            working_memory_read: None,
        };
        let s = score(&v, 1000, &ctx, &SalienceWeights::v1_default());
        assert!(s.self_relevance.abs() < 1e-3);
    }

    #[test]
    fn free_energy_in_unit_range() {
        let v = unit_x();
        let nn = unit_y();
        let ctx = SalienceContext {
            recent_ema: None,
            user_profile_centroid: None,
            nearest_neighbor: Some(&nn),
            context_embed: None,
            pc_latents: None,
            working_memory_read: None,
        };
        let s = score(&v, 0, &ctx, &SalienceWeights::v1_default());
        assert!(
            (0.0..=1.0).contains(&s.free_energy),
            "free_energy out of range: {}",
            s.free_energy
        );
    }

    /// ADR 0015 boundary: `working_memory_read` is an ingest-time
    /// salience signal. It overrides `nearest_neighbor` for the
    /// novelty axis only; it does not compile ContextEnvelope
    /// `thread_state` or rank `relevant_memory` today.
    #[test]
    fn working_memory_read_is_salience_novelty_only() {
        let v = unit_x();
        let nn = unit_x(); // would give novelty = 0
        let wm = unit_y(); // would give novelty = 1

        let ctx_nn = SalienceContext {
            recent_ema: None,
            user_profile_centroid: None,
            nearest_neighbor: Some(&nn),
            context_embed: None,
            pc_latents: None,
            working_memory_read: None,
        };
        let s_nn = score(&v, 0, &ctx_nn, &SalienceWeights::v1_default());

        let ctx_wm = SalienceContext {
            recent_ema: None,
            user_profile_centroid: None,
            nearest_neighbor: Some(&nn),
            context_embed: None,
            pc_latents: None,
            working_memory_read: Some(&wm),
        };
        let s_wm = score(&v, 0, &ctx_wm, &SalienceWeights::v1_default());

        assert!(s_nn.novelty.abs() < 1e-3, "nn==v → novelty ≈ 0");
        assert!((s_wm.novelty - 1.0).abs() < 1e-3, "wm⊥v → novelty ≈ 1");
        assert_ne!(s_nn.novelty, s_wm.novelty, "wm_read changes salience novelty only");
    }

    #[test]
    fn duration_score_monotonic() {
        let a = duration_score(0);
        let b = duration_score(1000);
        let c = duration_score(10_000);
        assert!(a < b && b < c, "duration sigmoid not monotonic: {} {} {}", a, b, c);
    }

    #[test]
    fn softmax_normalizes_to_one() {
        let s = softmax(&[1.0, 2.0, 3.0]);
        let total: f32 = s.iter().sum();
        assert!((total - 1.0).abs() < 1e-5);
        // Largest input → largest output.
        assert!(s[2] > s[1] && s[1] > s[0]);
    }

    #[test]
    fn softmax_weighted_recall_picks_top_by_mass() {
        let hits = vec![(1i64, 0.95), (2i64, 0.50), (3i64, 0.10)];
        let result = softmax_weighted_recall(&hits, 4.0, 0.95, 5);
        assert!(!result.is_empty());
        // softmax(β·sim) is monotonic in sim → sorted DESC by both.
        for w in result.windows(2) {
            assert!(
                w[0].2 >= w[1].2,
                "softmax weights must be non-increasing: {} >= {}",
                w[0].2,
                w[1].2
            );
        }
        assert_eq!(result[0].0, 1);
        assert!((result[0].1 - 0.95).abs() < 1e-6, "raw_sim preserved, got {}", result[0].1);
        assert!(result[0].2 > 0.5, "top weight should dominate, got {}", result[0].2);
        for (_, raw, weight) in &result {
            assert!(*raw >= 0.0 && *raw <= 1.0, "raw_sim out of [0,1]: {raw}");
            assert!(*weight >= 0.0 && *weight <= 1.0, "softmax weight out of [0,1]: {weight}");
        }
        let total_weight: f32 = result.iter().map(|(_, _, w)| w).sum();
        assert!(total_weight <= 1.0 + 1e-6, "Σ softmax weight ≤ 1, got {total_weight}");
    }

    #[test]
    fn softmax_weighted_recall_respects_max_take() {
        // 20 identical raw_sim → uniform softmax (1/20) → cumulative
        // hits 3·(1/20)=0.15 < 0.95 mass cap, so max_take is the
        // limiter (not the mass threshold).
        let hits = vec![(1i64, 0.5); 20];
        let result = softmax_weighted_recall(&hits, 1.0, 0.95, 3);
        assert_eq!(result.len(), 3);
        for (_, raw, weight) in &result {
            assert!((raw - 0.5).abs() < 1e-6, "raw_sim preserved at 0.5, got {raw}");
            assert!(*weight >= 0.0 && *weight <= 1.0, "softmax weight out of [0,1]: {weight}");
        }
        let total: f32 = result.iter().map(|(_, _, w)| w).sum();
        assert!(total <= 1.0 + 1e-6, "Σ softmax weight ≤ 1, got {total}");
    }

    #[test]
    fn l2_normalize_unit_input_is_idempotent() {
        let v = unit_x();
        let n = l2_normalize(&v);
        for (a, b) in v.iter().zip(n.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn l2_normalize_scales_arbitrary_vector() {
        let v = vec![3.0, 4.0]; // norm = 5
        let n = l2_normalize(&v);
        assert!((n[0] - 0.6).abs() < 1e-5);
        assert!((n[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn is_unit_normalized_recognizes_unit_and_rejects_others() {
        assert!(is_unit_normalized(&unit_x()));
        assert!(!is_unit_normalized(&[3.0_f32, 4.0]));
        assert!(is_unit_normalized(&[]));
    }

    /// R8 audit (2026-04-30) — invariant property: any non-zero
    /// `l2_normalize` output must satisfy `is_unit_normalized` (norm
    /// ≈ 1.0). Pseudo-random sweep over a few input shapes catches
    /// any drift in the normalization implementation. Zero-vector
    /// edge case is exempt (norm = 0 is an absorbing state).
    #[test]
    fn l2_normalize_output_is_unit_norm_property() {
        let mut state: u64 = 0xC0FFEE;
        for trial in 0..32 {
            // Splitmix64 to fill a small vector with reproducible
            // pseudo-random f32 values.
            let dim = 1 + (trial % 7) as usize; // 1..=7
            let mut v = Vec::with_capacity(dim);
            for _ in 0..dim {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^= z >> 31;
                let f = ((z >> 32) as i32 as f32) / 1.0e7_f32;
                v.push(f);
            }
            let n = l2_normalize(&v);
            // Skip degenerate zero-vector trials (the splitmix never
            // produces an exact zero vector but be defensive).
            let norm: f32 = n.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm < 1e-6 {
                continue;
            }
            assert!(
                is_unit_normalized(&n),
                "trial {trial}: l2_normalize output violates unit norm (norm={norm})"
            );
        }
    }

    #[test]
    fn update_centroid_blends_old_and_new() {
        let old = unit_x();
        let new = unit_y();
        let blended = update_centroid(&old, &new, 0.1);
        // After one update with α=0.1, centroid is mostly `old` but
        // tilted toward `new`. Both first and second components
        // non-zero.
        assert!(blended[0] > 0.5, "centroid keeps old direction: {}", blended[0]);
        assert!(blended[1] > 0.0, "centroid takes some new direction: {}", blended[1]);
        assert!(is_unit_normalized(&blended));
    }
}
