//! Modern Hopfield Network update rule (Ramsauer et al. 2020,
//! `arXiv: 2008.02217`).
//!
//! Reference: `research/ccogito/core/paper_accurate.py::PaperHopfield`.
//! ccogito's v2.0→v2.2 fix list (per `docs/research/ccogito-reseach-2.md`
//! §1.4) is baked in:
//!
//! 1. **LayerNorm on keys** (paper §3.2) — without it, head-output
//!    norms drift across writes and retrieval becomes unstable.
//! 2. **1/√d scaling** (paper Eq. 7) — keeps β · q·Kᵀ in a numerically
//!    stable range across head dims.
//! 3. **Weighted-sum retrieval** — output = V · softmax(β · K^T · q),
//!    not top-k indices. This is the *actual* Ramsauer update rule;
//!    top-k cutoff was the v2.0 mistake.
//!
//! v2.0 frozen-weight inference: stored patterns + frozen Q/K/V/output
//! projections (initialized as identity matrices for the v2.0 default,
//! upgradable to learned weights once `cognitive-train` lands). The
//! retrieval is a deterministic function of inputs.

use std::sync::Mutex;

/// Multi-head Hopfield network. Patterns are written via `write` and
/// retrieved via `recall`. Pattern storage = `Vec<Vec<f32>>` where
/// each inner vec is the L2-normalized embedding (Hu 2024 spherical-
/// codes invariant — same as SOMA's storage layer).
pub struct PaperHopfield {
    d_emb: usize,
    num_heads: usize,
    d_head: usize,
    beta: f32,
    /// L2-normalized stored patterns. Each row = one memory.
    patterns: Mutex<Vec<Vec<f32>>>,
    /// Q/K/V projection weights, frozen at construction. v2.0 = identity
    /// (each head sees a slice of the embedding); v2.1 swaps in learned
    /// weights from a candle session.
    w_q: Vec<Vec<Vec<f32>>>, // [num_heads][d_head][d_emb]
    w_k: Vec<Vec<Vec<f32>>>,
    w_v: Vec<Vec<Vec<f32>>>,
    /// Output projection [d_emb][num_heads · d_head]. Identity-init
    /// concatenates head outputs back to d_emb.
    w_o: Vec<Vec<f32>>,
    /// LayerNorm γ + β per head. Identity-init γ=1, β=0.
    ln_gamma: Vec<Vec<f32>>, // [num_heads][d_head]
    ln_beta: Vec<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct RecallHit {
    /// Index into the stored patterns Vec.
    pub pattern_idx: usize,
    /// Softmax weight assigned to this pattern.
    pub weight: f32,
}

impl PaperHopfield {
    /// Construct a frozen-weight Hopfield. v2.0 = identity-init Q/K/V/O
    /// + γ=1, β=0 LayerNorm. `num_heads` divides `d_emb`.
    ///
    /// Panics if `d_emb % num_heads != 0`.
    pub fn new(d_emb: usize, num_heads: usize, beta_init: f32) -> Self {
        assert_eq!(d_emb % num_heads, 0, "d_emb must be divisible by num_heads");
        let d_head = d_emb / num_heads;
        let mut w_q = Vec::with_capacity(num_heads);
        let mut w_k = Vec::with_capacity(num_heads);
        let mut w_v = Vec::with_capacity(num_heads);
        for h in 0..num_heads {
            // Identity slice: head `h` extracts dims `[h*d_head .. (h+1)*d_head]`.
            let mut q_proj = vec![vec![0.0; d_emb]; d_head];
            let mut k_proj = vec![vec![0.0; d_emb]; d_head];
            let mut v_proj = vec![vec![0.0; d_emb]; d_head];
            for i in 0..d_head {
                let src = h * d_head + i;
                q_proj[i][src] = 1.0;
                k_proj[i][src] = 1.0;
                v_proj[i][src] = 1.0;
            }
            w_q.push(q_proj);
            w_k.push(k_proj);
            w_v.push(v_proj);
        }
        // Output projection: identity over the concatenated heads.
        let total_concat = num_heads * d_head;
        let mut w_o = vec![vec![0.0; total_concat]; d_emb];
        for i in 0..d_emb.min(total_concat) {
            w_o[i][i] = 1.0;
        }
        let ln_gamma = vec![vec![1.0_f32; d_head]; num_heads];
        let ln_beta = vec![vec![0.0_f32; d_head]; num_heads];

        Self {
            d_emb,
            num_heads,
            d_head,
            beta: beta_init,
            patterns: Mutex::new(Vec::new()),
            w_q,
            w_k,
            w_v,
            w_o,
            ln_gamma,
            ln_beta,
        }
    }

    pub fn d_emb(&self) -> usize {
        self.d_emb
    }

    /// Store a pattern. Caller-supplied vectors are L2-normalized so
    /// the spherical-codes capacity bound (Hu 2024) holds.
    pub fn write(&self, pattern: &[f32]) {
        if pattern.len() != self.d_emb {
            return;
        }
        let mut p = self.patterns.lock().unwrap_or_else(|p| p.into_inner());
        p.push(l2_normalize(pattern));
    }

    /// Bulk write — equivalent to N `write` calls but takes the lock
    /// once.
    pub fn write_many(&self, patterns: &[Vec<f32>]) {
        let mut p = self.patterns.lock().unwrap_or_else(|p| p.into_inner());
        for pat in patterns {
            if pat.len() == self.d_emb {
                p.push(l2_normalize(pat));
            }
        }
    }

    /// Recall against the stored patterns. Returns `(weighted_output,
    /// per_pattern_weights)` in two parts:
    ///
    /// 1. The Hopfield update output — V · softmax(β · K^T · q),
    ///    aggregated across heads + projected back to d_emb. This is
    ///    the "associated memory" the network has converged onto.
    /// 2. The per-pattern weights so callers can rank or threshold
    ///    explicitly (the context pack semantic section uses these
    ///    for the "Why these items" attribution).
    pub fn recall(&self, query: &[f32]) -> (Vec<f32>, Vec<RecallHit>) {
        if query.len() != self.d_emb {
            return (vec![0.0; self.d_emb], Vec::new());
        }
        let patterns = self.patterns.lock().unwrap_or_else(|p| p.into_inner());
        if patterns.is_empty() {
            return (vec![0.0; self.d_emb], Vec::new());
        }

        let n = patterns.len();
        // §1.4 fix #2 — `1/sqrt(d_head)` matches the transformer
        // attention scale convention.
        //
        // R6 audit (2026-04-30) — combining `scale = 1/sqrt(d_head)`
        // with a constant `beta = 8.0` (DEFAULT_BETA) yields an
        // *effective temperature* of `8 / sqrt(d_head)`, which
        // varies with the head dim. Ramsauer 2020 §3 Eq. 8 prescribes
        // `β = sqrt(d_head)` for exponential-storage capacity bounds.
        // SOMA's empirical β=8.0 was tuned for `d_head=64` (effective
        // temperature 1.0). For other head dims the retrieval
        // sharpness drifts. Changing the formula invalidates already-
        // persisted Hopfield weights, so this is a documentation-only
        // note for v1; v1.x considers either (a) `beta = sqrt(d_head)`
        // (Ramsauer-pure) or (b) lifting the effective temperature
        // into config so operators can re-tune empirically without a
        // weight-format break.
        let scale = 1.0 / (self.d_head as f32).sqrt();

        // Aggregated weights per pattern (averaged across heads).
        let mut total_weights = vec![0.0_f32; n];
        let mut concat_output = vec![0.0_f32; self.num_heads * self.d_head];

        for h in 0..self.num_heads {
            let q_h = matvec(&self.w_q[h], query);
            // Project + LayerNorm-on-keys (§1.4 fix #1).
            let k_rows: Vec<Vec<f32>> = patterns
                .iter()
                .map(|p| layer_norm(&matvec(&self.w_k[h], p), &self.ln_gamma[h], &self.ln_beta[h]))
                .collect();
            let v_rows: Vec<Vec<f32>> = patterns.iter().map(|p| matvec(&self.w_v[h], p)).collect();

            // Score = β · q · k_i / √d
            let mut scores = vec![0.0_f32; n];
            for (i, k) in k_rows.iter().enumerate() {
                scores[i] = self.beta * dot(&q_h, k) * scale;
            }
            let weights = softmax(&scores);

            // Aggregate per-pattern weights across heads.
            for i in 0..n {
                total_weights[i] += weights[i];
            }
            // Head output = V^T · weights (weighted-sum retrieval, §1.4 fix #3).
            let mut head_out = vec![0.0_f32; self.d_head];
            for i in 0..n {
                for d in 0..self.d_head {
                    head_out[d] += v_rows[i][d] * weights[i];
                }
            }
            for d in 0..self.d_head {
                concat_output[h * self.d_head + d] = head_out[d];
            }
        }

        // Average per-pattern weights across heads.
        for w in total_weights.iter_mut() {
            *w /= self.num_heads as f32;
        }

        // Output projection back to d_emb.
        let output = matvec(&self.w_o, &concat_output);

        let mut hits: Vec<RecallHit> = total_weights
            .iter()
            .enumerate()
            .map(|(i, w)| RecallHit { pattern_idx: i, weight: *w })
            .collect();
        hits.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));

        (output, hits)
    }
}

fn matvec(m: &[Vec<f32>], v: &[f32]) -> Vec<f32> {
    let rows = m.len();
    let mut out = vec![0.0_f32; rows];
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

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn softmax(values: &[f32]) -> Vec<f32> {
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = values.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum == 0.0 {
        return vec![1.0 / values.len() as f32; values.len()];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-9 {
        let mut out = vec![0.0; v.len()];
        if !v.is_empty() {
            out[0] = 1.0;
        }
        return out;
    }
    v.iter().map(|x| x / norm).collect()
}

/// Per-vector LayerNorm (zero mean, unit variance) + γ scale + β shift.
fn layer_norm(v: &[f32], gamma: &[f32], beta: &[f32]) -> Vec<f32> {
    let n = v.len() as f32;
    if n < 1.0 {
        return v.to_vec();
    }
    let mean: f32 = v.iter().sum::<f32>() / n;
    let var: f32 = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    let std = (var + 1e-5).sqrt();
    v.iter()
        .enumerate()
        .map(|(i, x)| {
            let g = gamma.get(i).copied().unwrap_or(1.0);
            let b = beta.get(i).copied().unwrap_or(0.0);
            ((x - mean) / std) * g + b
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_at(d: usize, idx: usize) -> Vec<f32> {
        let mut v = vec![0.0; d];
        v[idx] = 1.0;
        v
    }

    #[test]
    fn empty_store_returns_zero_output() {
        let h = PaperHopfield::new(64, 4, 8.0);
        let (out, hits) = h.recall(&unit_at(64, 0));
        assert_eq!(out.len(), 64);
        assert!(out.iter().all(|x| (*x).abs() < 1e-9));
        assert!(hits.is_empty());
    }

    #[test]
    fn single_pattern_dominates_recall() {
        let h = PaperHopfield::new(64, 4, 8.0);
        let p = unit_at(64, 0);
        h.write(&p);
        let (_, hits) = h.recall(&p);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].weight > 0.99);
    }

    #[test]
    fn softmax_weighted_retrieval_picks_closest() {
        // Use richer patterns than degenerate unit vectors — the
        // LayerNorm-on-keys fix flattens unit-vector statistics
        // (same mean / std), so we need patterns whose distinct
        // *shape* survives LayerNorm. Each pattern occupies a
        // different 8-dim block.
        let h = PaperHopfield::new(64, 4, 8.0);
        let mut patterns: Vec<Vec<f32>> = Vec::new();
        for i in 0..8 {
            let mut p = vec![0.0; 64];
            for j in 0..8 {
                p[i * 8 + j] = ((j + 1) as f32) * 0.1;
            }
            patterns.push(p);
        }
        h.write_many(&patterns);

        // Exact-match query → that pattern dominates.
        let (_, hits) = h.recall(&patterns[3]);
        assert_eq!(hits[0].pattern_idx, 3);
        assert!(hits[0].weight > hits[1].weight, "top dominates runner-up");
    }

    #[test]
    fn weights_sum_to_one() {
        let h = PaperHopfield::new(64, 4, 8.0);
        for i in 0..16 {
            h.write(&unit_at(64, i));
        }
        let (_, hits) = h.recall(&unit_at(64, 5));
        let total: f32 = hits.iter().map(|h| h.weight).sum();
        // Average across heads → still ~1.
        assert!((total - 1.0).abs() < 1e-3, "weights sum ~1, got {total}");
    }

    #[test]
    fn beta_controls_sharpness() {
        let mut soft_top = 0.0_f32;
        let mut sharp_top = 0.0_f32;
        for &beta in &[1.0_f32, 32.0] {
            let h = PaperHopfield::new(64, 4, beta);
            for i in 0..8 {
                h.write(&unit_at(64, i));
            }
            let (_, hits) = h.recall(&unit_at(64, 3));
            if beta == 1.0 {
                soft_top = hits[0].weight;
            } else {
                sharp_top = hits[0].weight;
            }
        }
        assert!(sharp_top > soft_top, "high β concentrates mass: {sharp_top} vs {soft_top}");
    }

    #[test]
    fn capacity_under_noise_d_emb_64_n_16() {
        // Numerical sanity: with 16 random orthogonal-ish patterns in
        // d=64 and a noisy query (~0.85 cosine to its target), the
        // top-1 retrieval still picks the right pattern.
        let h = PaperHopfield::new(64, 4, 8.0);
        let mut patterns: Vec<Vec<f32>> = Vec::new();
        for i in 0..16 {
            let mut p = vec![0.0; 64];
            // Each pattern occupies a unique 4-dim block.
            for j in 0..4 {
                p[i * 4 + j] = 1.0;
            }
            patterns.push(p);
        }
        h.write_many(&patterns);

        // Noisy query of pattern 7 — corrupt one of its dims.
        let mut q = patterns[7].clone();
        q[7 * 4 + 3] = 0.7;
        q[63] = 0.3;

        let (_, hits) = h.recall(&q);
        assert_eq!(hits[0].pattern_idx, 7, "noisy query still recalls target");
    }

    #[test]
    fn output_dim_matches_d_emb() {
        let h = PaperHopfield::new(96, 6, 8.0);
        h.write(&unit_at(96, 0));
        let (out, _) = h.recall(&unit_at(96, 0));
        assert_eq!(out.len(), 96);
    }
}
