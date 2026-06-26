//! ANIL self-prediction head (Raghu et al. 2020,
//! `arXiv: 1909.09157`).
//!
//! Reference: `research/ccogito/core/paper_accurate.py::PaperSelfModel`.
//! ccogito v2.0→v2.2 fix list (`docs/research/ccogito-reseach-2.md`
//! §6): **encoder freeze** + inner-loop adapts only the
//! task-specific head. v2.0 here implements the *forward pass*
//! (`predict_behavior`) with frozen weights — the meta-train outer
//! loop lands in v2.x.
//!
//! Per Premakumar/Graziano 2024, self-prediction error is an
//! *independent salience axis* — already used in SOMA's D90
//! `self_relevance` via the `user_profile_centroid` EMA. This
//! module exposes the richer ANIL prediction so v2.x callers can
//! score a "would the self-model be surprised by this trace?"

/// ANIL self-model head. Input = behavioral trace tensor
/// `[T × F]` (T timesteps × F features); output = predicted next-
/// step trace `[F]`. v2.0 frozen-weight prediction = exponential-
/// moving-average across the trace dim, projected through an
/// identity head.
pub struct PaperSelfModel {
    feature_dim: usize,
    /// Encoder layer (frozen in ANIL inner loop). Identity-init.
    encoder: Vec<Vec<f32>>, // [feature_dim][feature_dim]
    /// Task head (the only thing inner loop would adapt). Identity.
    head: Vec<Vec<f32>>, // [feature_dim][feature_dim]
    /// EMA weighting decay; smaller = more responsive to recent
    /// timesteps, larger = smoother across the trace.
    ema_alpha: f32,
}

impl PaperSelfModel {
    pub fn new(feature_dim: usize) -> Self {
        let mut encoder = vec![vec![0.0_f32; feature_dim]; feature_dim];
        let mut head = vec![vec![0.0_f32; feature_dim]; feature_dim];
        for i in 0..feature_dim {
            encoder[i][i] = 1.0;
            head[i][i] = 1.0;
        }
        Self { feature_dim, encoder, head, ema_alpha: 0.3 }
    }

    pub fn feature_dim(&self) -> usize {
        self.feature_dim
    }

    /// Predict the next behavioral-trace row from the supplied
    /// trace history. `trace.len() = T` (time), inner vec length =
    /// `feature_dim`. Returns the predicted next-row F-vector.
    pub fn predict_behavior(&self, trace: &[Vec<f32>]) -> Vec<f32> {
        if trace.is_empty() {
            return vec![0.0; self.feature_dim];
        }
        // EMA across the trace history (recent timesteps weighted
        // higher), then push through the encoder + head.
        let mut ema = vec![0.0_f32; self.feature_dim];
        let mut weight_sum = 0.0_f32;
        for (t, row) in trace.iter().enumerate() {
            // Weight = (1-α)^(T-1-t) — exponential recency bias.
            let recency = trace.len() - 1 - t;
            let w = (1.0 - self.ema_alpha).powi(recency as i32);
            for i in 0..self.feature_dim.min(row.len()) {
                ema[i] += row[i] * w;
            }
            weight_sum += w;
        }
        if weight_sum > 0.0 {
            for v in ema.iter_mut() {
                *v /= weight_sum;
            }
        }
        let encoded = matvec(&self.encoder, &ema);
        matvec(&self.head, &encoded)
    }

    /// Self-awareness error (Premakumar 2024 §3.5). Returns the L2
    /// distance between the model's prediction and the actual
    /// observed row. Higher = more surprising self-state.
    pub fn self_awareness_error(&self, trace: &[Vec<f32>], actual: &[f32]) -> f32 {
        let predicted = self.predict_behavior(trace);
        predicted.iter().zip(actual.iter()).map(|(p, a)| (p - a).powi(2)).sum::<f32>().sqrt()
    }
}

fn matvec(m: &[Vec<f32>], v: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0_f32; m.len()];
    for (i, row) in m.iter().enumerate() {
        let mut s = 0.0_f32;
        for (j, x) in v.iter().enumerate() {
            if j < row.len() {
                s += row[j] * x;
            }
        }
        out[i] = s;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_with_empty_trace_returns_zeros() {
        let m = PaperSelfModel::new(8);
        let pred = m.predict_behavior(&[]);
        assert_eq!(pred.len(), 8);
        assert!(pred.iter().all(|x| x.abs() < 1e-9));
    }

    #[test]
    fn predict_constant_trace_returns_constant() {
        let m = PaperSelfModel::new(4);
        let trace = vec![vec![1.0, 0.0, 0.0, 0.0]; 5];
        let pred = m.predict_behavior(&trace);
        assert!((pred[0] - 1.0).abs() < 1e-3, "constant trace → constant prediction");
    }

    #[test]
    fn recency_bias_favors_latest_timestep() {
        // Recency bias means each individual older row is weighted
        // less than the latest — not that *cumulative* old mass is
        // less than one new row. The test pins the per-row weight
        // ordering: 1 old row of pattern A vs 1 new row of pattern B.
        let m = PaperSelfModel::new(4);
        let trace = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 1.0, 0.0, 0.0]];
        let pred = m.predict_behavior(&trace);
        // Latest timestep (pattern B at idx 1) outweighs the older
        // timestep (pattern A at idx 0).
        assert!(pred[1] > pred[0], "recency bias on per-row basis: {pred:?}");
    }

    #[test]
    fn self_awareness_error_zero_for_perfect_match() {
        let m = PaperSelfModel::new(4);
        let trace = vec![vec![1.0, 0.0, 0.0, 0.0]; 3];
        let actual = m.predict_behavior(&trace);
        let err = m.self_awareness_error(&trace, &actual);
        assert!(err < 1e-6);
    }

    #[test]
    fn self_awareness_error_grows_with_mismatch() {
        let m = PaperSelfModel::new(4);
        let trace = vec![vec![1.0, 0.0, 0.0, 0.0]; 3];
        let actual = vec![0.0, 0.0, 0.0, 1.0]; // orthogonal
        let err = m.self_awareness_error(&trace, &actual);
        assert!(err > 0.5, "orthogonal → high error, got {err}");
    }
}
