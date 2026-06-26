//! Forgetting kernel — Ebbinghaus decay + Note Block + similarity
//! merge (ADR 0004 §B / discussion 0037 §D91).
//!
//! Frozen-weights, ML-free. The two papers this distills:
//!
//! * MemoryBank (AAAI 2024): `decay = exp(-λ · Δt / (1 +
//!   access_count))`. Repeated access counteracts age — a memory
//!   you've revisited is harder to forget.
//! * MemMamba (`arXiv: 2510.03279`): a Note Block survives the
//!   normal decay path entirely. SOMA implements that via the
//!   `note_pins` table — pinned episodes are never decayed.
//!
//! The kernel is split between this module (pure functions) and
//! `runtime::scheduler::slow_loop` (periodic side-effects).

use crate::memory::salience::SalienceScore;

/// Ebbinghaus decay factor in `[0, 1]`. Returns 1.0 when `now_ns ≤
/// ts_start_ns` (newly created — full strength) or when `lambda ≤
/// 0` (decay disabled). Higher `access_count` → slower decay
/// (the formula's `1 + access_count` denominator).
pub fn decay_weight(now_ns: i64, ts_start_ns: i64, access_count: u64, lambda: f32) -> f32 {
    if lambda <= 0.0 {
        return 1.0;
    }
    if now_ns <= ts_start_ns {
        return 1.0;
    }
    let dt_seconds = (now_ns - ts_start_ns) as f32 / 1e9;
    let dt_days = dt_seconds / 86_400.0;
    let exponent = -lambda * dt_days / (1.0 + access_count as f32);
    exponent.exp().clamp(0.0, 1.0)
}

/// Should this episode be pinned to the Note Block? Compares the
/// D90 `free_energy` against the configured threshold.
pub fn should_pin(score: &SalienceScore, pin_threshold: f32) -> bool {
    score.free_energy >= pin_threshold
}

/// Default Ebbinghaus λ — half-life ~14 days for an episode with
/// zero accesses. `decay_weight(14 days, 0 access, λ=0.05) = e^{-0.7}
/// ≈ 0.50`.
pub const DEFAULT_LAMBDA: f32 = 0.05;

/// Default pin threshold. SalienceScore.free_energy in [0, 1]; pin
/// at 0.7 means roughly the top 10-15% of episodes by salience.
pub const DEFAULT_PIN_THRESHOLD: f32 = 0.70;

/// Default similarity for slow-loop merge. Cosine > 0.95 means the
/// two episodes are near-duplicates — same topic, different
/// timestamp.
pub const DEFAULT_MERGE_SIMILARITY: f32 = 0.95;

/// Default cold-tier threshold. After Ebbinghaus, decay_weight <
/// 0.05 means the episode is effectively forgotten by the recall
/// path — demote to cold to keep hot tier compact.
pub const DEFAULT_COLD_TIER_THRESHOLD: f32 = 0.05;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::salience::SalienceScore;

    fn score_with_fe(fe: f32) -> SalienceScore {
        SalienceScore {
            surprise: 0.5,
            novelty: 0.5,
            contradiction: 0.0,
            self_relevance: 0.5,
            duration: 0.5,
            free_energy: fe,
            pc_free_energy: None,
        }
    }

    const NS_PER_DAY: i64 = 86_400 * 1_000_000_000;

    #[test]
    fn decay_weight_one_at_zero_age() {
        let now = 1_700_000_000_000_000_000;
        assert!((decay_weight(now, now, 0, DEFAULT_LAMBDA) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decay_decreases_with_time() {
        let now = 1_700_000_000_000_000_000;
        let day = NS_PER_DAY;
        let w1 = decay_weight(now, now - day, 0, DEFAULT_LAMBDA);
        let w7 = decay_weight(now, now - 7 * day, 0, DEFAULT_LAMBDA);
        let w30 = decay_weight(now, now - 30 * day, 0, DEFAULT_LAMBDA);
        assert!(1.0 > w1, "1 day already decayed");
        assert!(w1 > w7, "7 days decay further");
        assert!(w7 > w30, "30 days decay further");
        assert!(w30 > 0.0, "decay still positive at 30 days");
    }

    #[test]
    fn decay_increases_with_access_count() {
        let now = 1_700_000_000_000_000_000;
        let ts = now - 14 * NS_PER_DAY;
        let w0 = decay_weight(now, ts, 0, DEFAULT_LAMBDA);
        let w5 = decay_weight(now, ts, 5, DEFAULT_LAMBDA);
        let w20 = decay_weight(now, ts, 20, DEFAULT_LAMBDA);
        assert!(w20 > w5, "high access slows decay");
        assert!(w5 > w0, "any access slows decay");
    }

    #[test]
    fn decay_disabled_when_lambda_zero() {
        let now = 1_700_000_000_000_000_000;
        let w = decay_weight(now, now - 1000 * NS_PER_DAY, 0, 0.0);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn decay_clamps_to_zero_one_range() {
        let now = 1_700_000_000_000_000_000;
        let w = decay_weight(now, now - 100_000 * NS_PER_DAY, 0, 1.0);
        assert!((0.0..=1.0).contains(&w));
    }

    #[test]
    fn should_pin_threshold_boundary() {
        let s_high = score_with_fe(0.85);
        let s_mid = score_with_fe(0.70);
        let s_low = score_with_fe(0.30);
        assert!(should_pin(&s_high, DEFAULT_PIN_THRESHOLD));
        assert!(should_pin(&s_mid, DEFAULT_PIN_THRESHOLD)); // boundary inclusive
        assert!(!should_pin(&s_low, DEFAULT_PIN_THRESHOLD));
    }

    #[test]
    fn should_pin_ignores_pc_free_energy_diagnostic() {
        let mut score = score_with_fe(DEFAULT_PIN_THRESHOLD - 0.01);
        score.pc_free_energy = Some(10.0);

        assert!(
            !should_pin(&score, DEFAULT_PIN_THRESHOLD),
            "ADR 0015 boundary: iPC pc_free_energy is not a pinning or ContextEnvelope signal"
        );
    }
}
