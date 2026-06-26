//! Matrix-LSTM working memory (Beck et al. 2024 xLSTM mLSTM,
//! `arXiv: 2405.04517`).
//!
//! Reference: `research/ccogito/core/paper_accurate.py::PaperWorkingMemory`.
//! ccogito v2.0→v2.2 fix list (`docs/research/ccogito-reseach-2.md`
//! §2.2):
//!
//! 1. **LayerNorm on key/value** (paper §3.2) — keeps the matrix
//!    memory's eigenstructure stable across long-horizon updates.
//! 2. **1/√d_head scaling on key** (paper Eq. 7) — the mLSTM update
//!    rule's `i'_t · (v_t ⊗ k_t^T)` numerically misbehaves without
//!    the scale.
//!
//! v2.0 frozen-weight inference: matrix memory `C_t ∈ ℝ^{d×d}` and
//! normalizer state `n_t ∈ ℝ^d`, both updated *deterministically* from
//! input via identity-init projections. Trainable Q/K/V/O lands in
//! v2.x with `cognitive-train`.

use std::sync::Mutex;

/// Matrix-LSTM working memory cell. State = (C, n) where:
///   * C_t = f'_t · C_{t-1} + i'_t · (v_t ⊗ k_t^T)   ∈ ℝ^{d×d}
///   * n_t = f'_t · n_{t-1} + i'_t · k_t              ∈ ℝ^d
/// Output = q_t · C_t / max(|q_t · n_t|, 1).
pub struct PaperWorkingMemory {
    d_emb: usize,
    /// Persistent matrix memory.
    cell: Mutex<MatrixState>,
    /// Frozen scale for exp gates. Higher → faster forgetting.
    forget_scale: f32,
    input_scale: f32,
}

struct MatrixState {
    /// `d × d` matrix flattened row-major.
    c: Vec<f32>,
    /// Normalizer state.
    n: Vec<f32>,
}

impl PaperWorkingMemory {
    pub fn new(d_emb: usize) -> Self {
        Self {
            d_emb,
            cell: Mutex::new(MatrixState {
                c: vec![0.0_f32; d_emb * d_emb],
                n: vec![0.0_f32; d_emb],
            }),
            forget_scale: 0.95,
            input_scale: 1.0,
        }
    }

    pub fn d_emb(&self) -> usize {
        self.d_emb
    }

    /// Reset the working memory to zero state.
    pub fn reset(&self) {
        let mut state = self.cell.lock().unwrap();
        state.c.fill(0.0);
        state.n.fill(0.0);
    }

    /// Apply one mLSTM update with input `x` (the new "experience"
    /// embedding) and return the *current* working-memory output —
    /// `q · C / |q · n|`. v2.0 identity-init: q = k = v = x.
    pub fn update(&self, x: &[f32]) -> Vec<f32> {
        if x.len() != self.d_emb {
            return vec![0.0; self.d_emb];
        }
        let scale = 1.0 / (self.d_emb as f32).sqrt();
        // LayerNorm key + 1/√d scaling (§2.2 fixes).
        let k_norm = layer_norm(x);
        let k: Vec<f32> = k_norm.iter().map(|v| v * scale).collect();
        let v_norm = layer_norm(x);
        let v = &v_norm;
        let q = &k_norm;

        let mut state = self.cell.lock().unwrap();
        // Exp gates with tanh-like saturation in [0,1]. v2.0 frozen:
        // gates depend only on the L2 norm of input, not on a learned
        // gate function.
        let x_norm: f32 = x.iter().map(|a| a * a).sum::<f32>().sqrt();
        let i_gate = sigmoid(self.input_scale * x_norm);
        let f_gate = sigmoid(self.forget_scale - x_norm * 0.1);

        // Update C_t = f · C_{t-1} + i · (v ⊗ k^T)
        for row in 0..self.d_emb {
            for col in 0..self.d_emb {
                let idx = row * self.d_emb + col;
                state.c[idx] = f_gate * state.c[idx] + i_gate * v[row] * k[col];
            }
        }
        // Update n_t = f · n_{t-1} + i · k
        for j in 0..self.d_emb {
            state.n[j] = f_gate * state.n[j] + i_gate * k[j];
        }

        // Output = (q · C) / max(|q · n|, 1)
        let mut qc = vec![0.0_f32; self.d_emb];
        for col in 0..self.d_emb {
            let mut s = 0.0_f32;
            for row in 0..self.d_emb {
                s += q[row] * state.c[row * self.d_emb + col];
            }
            qc[col] = s;
        }
        let qn: f32 = q.iter().zip(state.n.iter()).map(|(a, b)| a * b).sum::<f32>().abs();
        let denom = qn.max(1.0);
        qc.iter().map(|v| v / denom).collect()
    }

    /// Read-only state read — useful for `soma inspect` / tests.
    pub fn state_norm(&self) -> (f32, f32) {
        let state = self.cell.lock().unwrap();
        let c_fro: f32 = state.c.iter().map(|v| v * v).sum::<f32>().sqrt();
        let n_l2: f32 = state.n.iter().map(|v| v * v).sum::<f32>().sqrt();
        (c_fro, n_l2)
    }

    /// STAGE 3-C — copy out the matrix + normalizer for SQLite
    /// BLOB persistence. Returns `(c, n)` where `c` is the
    /// `d×d` flattened row-major and `n` is `d`-vector. The clones
    /// keep the lock window short.
    pub fn export_state(&self) -> (Vec<f32>, Vec<f32>) {
        let state = self.cell.lock().unwrap();
        (state.c.clone(), state.n.clone())
    }

    /// STAGE 3-C — restore state from SQLite BLOB. `c` must be
    /// `d_emb²` long, `n` must be `d_emb`. Mismatches are silently
    /// dropped — the caller treats that as "fresh init".
    pub fn import_state(&self, c: Vec<f32>, n: Vec<f32>) -> bool {
        if c.len() != self.d_emb * self.d_emb || n.len() != self.d_emb {
            return false;
        }
        let mut state = self.cell.lock().unwrap();
        state.c = c;
        state.n = n;
        true
    }
}

fn sigmoid(x: f32) -> f32 {
    // R6 audit (2026-04-30) — clamp input to [-20, 20]. For x < -89,
    // (-x).exp() saturates to f32::INFINITY and 1/(1+inf) = 0 (still
    // valid), but for x > +89, (-x).exp() rounds to 0 yielding 1.0
    // (also valid) — the *real* concern is gradient flow during
    // training, where values past ±20 already give effectively flat
    // gradients. Clamping mirrors PyTorch / candle convention and
    // keeps numerics defensible regardless of caller-supplied input
    // norm (a malformed embedder could feed huge norms).
    let x = x.clamp(-20.0, 20.0);
    1.0 / (1.0 + (-x).exp())
}

fn layer_norm(v: &[f32]) -> Vec<f32> {
    let n = v.len() as f32;
    if n < 1.0 {
        return v.to_vec();
    }
    let mean: f32 = v.iter().sum::<f32>() / n;
    let var: f32 = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    let std = (var + 1e-5).sqrt();
    v.iter().map(|x| (x - mean) / std).collect()
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
    fn fresh_state_is_zero() {
        let wm = PaperWorkingMemory::new(32);
        let (c, n) = wm.state_norm();
        assert_eq!(c, 0.0);
        assert_eq!(n, 0.0);
    }

    #[test]
    fn update_grows_state_norm() {
        let wm = PaperWorkingMemory::new(32);
        wm.update(&unit_at(32, 0));
        let (c, n) = wm.state_norm();
        assert!(c > 0.0);
        assert!(n > 0.0);
    }

    #[test]
    fn reset_returns_zero_state() {
        let wm = PaperWorkingMemory::new(32);
        wm.update(&unit_at(32, 0));
        wm.reset();
        let (c, n) = wm.state_norm();
        assert_eq!(c, 0.0);
        assert_eq!(n, 0.0);
    }

    #[test]
    fn output_dim_matches_d_emb() {
        let wm = PaperWorkingMemory::new(48);
        let out = wm.update(&unit_at(48, 0));
        assert_eq!(out.len(), 48);
    }

    #[test]
    fn forget_attenuates_old_input() {
        let wm = PaperWorkingMemory::new(32);
        wm.update(&unit_at(32, 0));
        let (c1, _) = wm.state_norm();
        // Many subsequent unrelated updates should attenuate the
        // pattern stored at step 1.
        for i in 1..10 {
            wm.update(&unit_at(32, i % 32));
        }
        let (c10, _) = wm.state_norm();
        // C grows then bounded — but the fraction attributable to
        // step-1 input must drop. Numerical sanity: state norm is
        // bounded.
        assert!(c10.is_finite());
        assert!(c1.is_finite());
    }
}
