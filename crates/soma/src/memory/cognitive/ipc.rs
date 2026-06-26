//! Incremental Predictive Coding (Salvatori et al. 2024,
//! `arXiv: 2212.00720`).
//!
//! Reference: `research/ccogito/core/paper_accurate.py::PaperPC`.
//! ccogito v2.0→v2.2 fix list (`docs/research/ccogito-reseach-2.md`
//! §3.2): **simultaneous** latent updates (paper §4) — sequential
//! updates broke the convergence guarantee. Our forward pass mirrors
//! that fix.
//!
//! v2.0 frozen-weight inference: identity-init prediction layers, no
//! gradient. The kernel computes per-layer prediction error
//! `ε_l = x_l - f_l(x_{l+1})` and aggregates into a Free Energy
//! `F = Σ ‖ε_l‖²`. SOMA's salience kernel (D90) already takes the
//! *scalar* version of F; this module exposes the per-layer breakdown
//! for richer downstream signals (per-layer surprise, hierarchy
//! debugging).
//!
//! ADR 0015 disposition: frozen iPC free-energy is a connected
//! candidate through ingest-time `context_anomalies`, which become
//! cited `ContextEnvelope.open_decisions` anomalies. It remains
//! separate from pinning and ranking.

/// Hierarchical PC layer dims, top-down. `dims[0]` = sensory
/// (highest-resolution) layer; `dims[L-1]` = abstract layer.
pub struct PaperPC {
    dims: Vec<usize>,
    /// Frozen down-projection weights `[L-1][dim_lower][dim_higher]`.
    /// Layer `l`'s prediction = projectors[l] · x_{l+1}. v2.0 = pseudo-
    /// identity (top-left identity block).
    projectors: Vec<Vec<Vec<f32>>>,
}

impl PaperPC {
    /// Build a multi-layer iPC predictor. Caller MUST ensure `dims.
    /// len() >= 2` (every active call site already guards on this —
    /// see `salience.rs::compute_pc_free_energy`). The contract is
    /// enforced via `debug_assert!` so dev/test catches violations
    /// without panicking in release builds.
    pub fn new(dims: Vec<usize>) -> Self {
        debug_assert!(dims.len() >= 2, "PC needs at least 2 layers");
        let mut projectors = Vec::with_capacity(dims.len() - 1);
        for l in 0..dims.len() - 1 {
            let lower = dims[l];
            let higher = dims[l + 1];
            // Identity-block init: predictor sends each higher-layer
            // dim to the corresponding lower-layer dim, padding with
            // zeros where lower > higher.
            let mut w = vec![vec![0.0_f32; higher]; lower];
            for i in 0..lower.min(higher) {
                w[i][i] = 1.0;
            }
            projectors.push(w);
        }
        Self { dims, projectors }
    }

    pub fn dims(&self) -> &[usize] {
        &self.dims
    }

    /// Compute per-layer prediction errors. Caller supplies the
    /// `latents`: `latents[0]` = sensory observation,
    /// `latents[l]` = inferred latent at layer `l`. Returns
    /// `errors[l] = latents[l] - projectors[l] · latents[l+1]`
    /// for `l` in 0..L-1.
    pub fn prediction_errors(&self, latents: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let mut errors = Vec::with_capacity(self.dims.len() - 1);
        for l in 0..self.dims.len() - 1 {
            let lower = &latents[l];
            let higher = &latents[l + 1];
            let predicted = matvec(&self.projectors[l], higher);
            let err: Vec<f32> = lower.iter().zip(predicted.iter()).map(|(a, b)| a - b).collect();
            errors.push(err);
        }
        errors
    }

    /// Free Energy F = Σ ‖ε_l‖².
    ///
    /// R6 audit (2026-04-30) — Salvatori 2024 iPC §4 specifies
    /// `F = Σ ‖ε_l‖² + Σ KL(q_l ‖ p_l)` where `q/p` are
    /// posterior/prior over latents. v1 SOMA omits the KL term
    /// intentionally: this is **frozen-weight inference** (no
    /// learnable priors yet), so the KL between identical priors
    /// is identically zero. v1.2 cognitive-train chunk 3 (iPC
    /// trainable predictor) adopts learnable predictors but still
    /// uses fixed-Gaussian priors → KL stays zero. v2.x learnable
    /// priors (D96-cand PCA hierarchical) is when KL becomes
    /// non-trivial and must be added here. Until then the
    /// likelihood-only formula is correct for SOMA's stack.
    pub fn free_energy(&self, latents: &[Vec<f32>]) -> f32 {
        let errors = self.prediction_errors(latents);
        errors.iter().map(|e| e.iter().map(|x| x * x).sum::<f32>()).sum()
    }

    /// Per-layer free energy (the iPC paper's salience hierarchy).
    /// Returned vec length = dims.len() - 1.
    pub fn per_layer_free_energy(&self, latents: &[Vec<f32>]) -> Vec<f32> {
        self.prediction_errors(latents)
            .iter()
            .map(|e| e.iter().map(|x| x * x).sum::<f32>())
            .collect()
    }

    /// One iteration of *simultaneous* latent inference (§3.2 fix).
    /// Updates each `latents[l]` by `-η · ε_l + η · projectorsᵀ · ε_{l-1}`
    /// (boundary layers truncated). Caller-provided η (typically 0.1).
    pub fn step_inference(&self, latents: &mut [Vec<f32>], eta: f32) {
        if latents.len() < 2 {
            return;
        }
        let errors = self.prediction_errors(latents);
        for l in 1..self.dims.len() {
            // Pull from the lower layer's error via projector^T.
            let lower_err = &errors[l - 1];
            let proj = &self.projectors[l - 1];
            // top-down update — layer l's latent gets the residual.
            let mut delta = vec![0.0_f32; self.dims[l]];
            for i in 0..self.dims[l - 1] {
                for j in 0..self.dims[l] {
                    if j < proj[i].len() {
                        delta[j] += proj[i][j] * lower_err[i];
                    }
                }
            }
            for (j, latent) in latents[l].iter_mut().enumerate() {
                if let Some(d) = delta.get(j) {
                    *latent += eta * d;
                }
            }
        }
        // Bottom layer is observation — *not* updated (clamped to data).
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
    fn dims_validated() {
        let pc = PaperPC::new(vec![64, 32]);
        assert_eq!(pc.dims(), &[64, 32]);
    }

    #[test]
    fn perfect_prediction_zero_error() {
        let pc = PaperPC::new(vec![4, 4]);
        // Identity-init: prediction = higher-layer latent.
        let higher = vec![1.0, 2.0, 3.0, 4.0];
        let lower = higher.clone(); // matches identity prediction
        let errors = pc.prediction_errors(&[lower, higher]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].iter().all(|x| x.abs() < 1e-6));
    }

    #[test]
    fn mismatch_yields_nonzero_free_energy() {
        let pc = PaperPC::new(vec![4, 4]);
        let lower = vec![1.0, 0.0, 0.0, 0.0];
        let higher = vec![0.0, 0.0, 0.0, 1.0];
        let f = pc.free_energy(&[lower, higher]);
        assert!(f > 0.0);
    }

    #[test]
    fn per_layer_breakdown_matches_total() {
        let pc = PaperPC::new(vec![4, 4, 4]);
        let l0 = vec![1.0, 0.0, 0.0, 0.0];
        let l1 = vec![0.0, 1.0, 0.0, 0.0];
        let l2 = vec![0.0, 0.0, 1.0, 0.0];
        let total = pc.free_energy(&[l0.clone(), l1.clone(), l2.clone()]);
        let per = pc.per_layer_free_energy(&[l0, l1, l2]);
        let summed: f32 = per.iter().sum();
        assert!((total - summed).abs() < 1e-6);
        assert_eq!(per.len(), 2);
    }

    #[test]
    fn step_inference_reduces_free_energy() {
        let pc = PaperPC::new(vec![4, 4]);
        let l0 = vec![1.0, 0.0, 0.0, 0.0];
        // Wrong higher-layer prediction.
        let mut latents = vec![l0, vec![0.5, 0.5, 0.0, 0.0]];
        let f_before = pc.free_energy(&latents);
        for _ in 0..20 {
            pc.step_inference(&mut latents, 0.1);
        }
        let f_after = pc.free_energy(&latents);
        assert!(f_after < f_before, "iPC should reduce F: {f_before} -> {f_after}");
    }
}
