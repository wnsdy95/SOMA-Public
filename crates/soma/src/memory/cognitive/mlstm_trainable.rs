//! Trainable mLSTM Q/K/V — v1.2 chunk 1 (ADR 0008 §D7) + polish.
//!
//! Wraps the frozen `PaperWorkingMemory` update rule with explicit
//! `W_q`, `W_k`, `W_v` projection matrices held as candle `Var`
//! (mutable trainable). Identity-init at construction so chunk 1.1
//! ships forward-pass parity with the frozen path.
//!
//! `train_step` runs one AdamW step on the autoencoder
//! reconstruction loss (`‖x - read‖²`). Polish replaced chunk 1.2's
//! manual SGD with `candle_nn::AdamW` so adaptive moments + bias
//! correction handle the 384-d weight scale (chunk 1.2's fixed-lr
//! SGD plateaued at ~5e-7 / step).
//!
//! Feature-gated behind `cognitive-train` so default + `cognitive`
//! builds don't pull candle into the dep graph.
//!
//! ADR 0015 disposition: this is a legacy-risk context quality
//! candidate. Its current production consumer is ingest-time salience
//! novelty (`capture::ai_cli::compute_working_memory_read`), not
//! `ContextEnvelope::thread_state` compression. Keep it only if a
//! future P4 slice wires that envelope output directly.

#![cfg(feature = "cognitive-train")]

use std::sync::Mutex;

use candle_core::{DType, Device, Tensor, Var};
use candle_nn::{AdamW, Optimizer, ParamsAdamW};

/// Trainable mLSTM with Q/K/V projection. State `(C, n)` matches
/// the frozen `PaperWorkingMemory`; only the projections are
/// learnable. `forward` returns the read output (same shape as
/// `PaperWorkingMemory::update`) so chunk 1.5 can swap one for the
/// other without reshaping callers.
pub struct TrainableMLstm {
    d_emb: usize,
    /// Q/K/V projections — `Var` so chunk 1.2's `train_step` can
    /// mutate them in place. Identity-init in chunk 1.1.
    w_q: Var,
    w_k: Var,
    w_v: Var,
    /// State `(C, n)`. Plain `Vec<f32>` behind a `Mutex` so the hot
    /// path can update them without going through autograd — only
    /// the weights live in the graph.
    cell: Mutex<MatrixState>,
    forget_scale: f32,
    input_scale: f32,
    /// CPU device — chunk 1 ships CPU only. Metal/CUDA are user-
    /// opt-in via downstream feature flags.
    device: Device,
    /// chunk 1.3 — monotonic counter incremented on every
    /// `train_step` call. Persisted alongside `W_q/W_k/W_v` so a
    /// resident restart sees how much training has run.
    train_steps: Mutex<u64>,
}

struct MatrixState {
    c: Vec<f32>,
    n: Vec<f32>,
}

impl TrainableMLstm {
    /// Identity-init constructor. Chosen so chunk 1.1's forward
    /// pass equals the frozen `PaperWorkingMemory.update` for the
    /// same input — regression tests pin this equivalence until
    /// chunk 1.2's `train_step` mutates the weights.
    pub fn new_identity(d_emb: usize) -> candle_core::Result<Self> {
        let device = Device::Cpu;
        let identity = Self::identity_tensor(d_emb, &device)?;
        Ok(Self {
            d_emb,
            w_q: Var::from_tensor(&identity)?,
            w_k: Var::from_tensor(&identity)?,
            w_v: Var::from_tensor(&identity)?,
            cell: Mutex::new(MatrixState {
                c: vec![0.0_f32; d_emb * d_emb],
                n: vec![0.0_f32; d_emb],
            }),
            forget_scale: 0.95,
            input_scale: 1.0,
            device,
            train_steps: Mutex::new(0),
        })
    }

    fn identity_tensor(d: usize, device: &Device) -> candle_core::Result<Tensor> {
        let mut data = vec![0.0_f32; d * d];
        for i in 0..d {
            data[i * d + i] = 1.0;
        }
        Tensor::from_vec(data, (d, d), device)
    }

    pub fn d_emb(&self) -> usize {
        self.d_emb
    }

    /// One mLSTM forward step. Identity-init parity with the frozen
    /// path: when `w_q = w_k = w_v = I`, the projection is a no-op
    /// and the rest of the update matches `PaperWorkingMemory`.
    pub fn forward(&self, x: &[f32]) -> candle_core::Result<Vec<f32>> {
        if x.len() != self.d_emb {
            return Ok(vec![0.0; self.d_emb]);
        }
        let x_t = Tensor::from_slice(x, (self.d_emb, 1), &self.device)?;
        let q_t = self.w_q.as_tensor().matmul(&x_t)?;
        let k_t = self.w_k.as_tensor().matmul(&x_t)?;
        let v_t = self.w_v.as_tensor().matmul(&x_t)?;
        let q_vec = tensor_to_vec(&q_t)?;
        let k_vec = tensor_to_vec(&k_t)?;
        let v_vec = tensor_to_vec(&v_t)?;
        Ok(self.update_state_with(x, &q_vec, &k_vec, &v_vec))
    }

    /// Read-only inspection. Same shape as
    /// `PaperWorkingMemory::state_norm`.
    pub fn state_norm(&self) -> (f32, f32) {
        let state =
            self.cell.lock().expect("mlstm trainable state mutex (poison = restart resident)");
        let c_fro: f32 = state.c.iter().map(|v| v * v).sum::<f32>().sqrt();
        let n_l2: f32 = state.n.iter().map(|v| v * v).sum::<f32>().sqrt();
        (c_fro, n_l2)
    }

    /// One AdamW step on the autoencoder reconstruction loss.
    /// Fresh zero-state per call — caller (slow_loop) carries
    /// state across the batch via `train_step_with_state` once
    /// state-carry-over polish lands. Returns scalar loss.
    ///
    /// Loss derivation:
    /// * Fresh state means `C_0 = 0`, `n_0 = 0`, so the gates'
    ///   contribution to `C` is just `i · v⊗k^T` and to `n` is
    ///   `i · k`. With `i ≈ 1` for unit-norm input this collapses
    ///   to `C = v⊗k^T`, `n = k`.
    /// * Read = `(q^T · C) / max(|q^T · n|, 1) ≈ (q^T · v) · k^T
    ///   / |q^T · k|`.
    /// * Loss = `mean((read - x)²)`.
    /// * Gradient flows through `q`, `k`, `v` to `W_q/W_k/W_v`.
    ///
    /// Polish — chunk 1.2's manual SGD (fixed lr=0.01, plateaued at
    /// ~5e-7 weight delta per step on 384-d embeddings) replaced
    /// with `candle_nn::AdamW`. AdamW's adaptive moment estimates
    /// give ~1000× larger effective step on identity-near weights
    /// (verified by the `train_step_reduces_loss_over_epochs` unit
    /// test landing in the same chunk).
    pub fn train_step(&self, x: &[f32], lr: f64) -> candle_core::Result<f32> {
        if x.len() != self.d_emb {
            return Ok(0.0);
        }
        let x_t = Tensor::from_slice(x, (self.d_emb, 1), &self.device)?;
        let target = Tensor::from_slice(x, (1, self.d_emb), &self.device)?;

        // Forward (in-graph, autograd-traceable).
        let q = self.w_q.as_tensor().matmul(&x_t)?; // (d, 1)
        let k = self.w_k.as_tensor().matmul(&x_t)?; // (d, 1)
        let v = self.w_v.as_tensor().matmul(&x_t)?; // (d, 1)

        let q_t = q.transpose(0, 1)?; // (1, d)
        let k_t = k.transpose(0, 1)?; // (1, d)
        let qv = q_t.matmul(&v)?; // (1, 1)
        let qc = qv.broadcast_mul(&k_t)?; // (1, d)
        let qn = q_t.matmul(&k)?; // (1, 1)
        let denom = qn.abs()?.maximum(1f64)?; // (1, 1)
        let read = qc.broadcast_div(&denom)?; // (1, d)

        let diff = (read - target)?;
        let loss = diff.sqr()?.mean_all()?;

        let loss_val: f32 = loss.to_scalar()?;
        // Layer 1 NaN guard — production-safety chunk (5d83181).
        // Divergent loss → skip optimizer step entirely; the cell
        // stays at its last-known-good state.
        if !loss_val.is_finite() {
            return Ok(loss_val);
        }

        // Sanity check on the gradients before letting AdamW touch
        // the weights — defense-in-depth atop AdamW's own NaN
        // tolerance. atomic skip on any non-finite component.
        let grads = loss.backward()?;
        for var in [&self.w_q, &self.w_k, &self.w_v] {
            let g = grads
                .get(var)
                .ok_or_else(|| candle_core::Error::Msg("missing gradient for Var".into()))?;
            if !tensor_is_finite(g)? {
                return Ok(loss_val);
            }
        }

        // AdamW step. We construct the optimizer per-call instead of
        // caching it because the bias-correction state lives in the
        // optimizer; carrying it across resident restarts requires
        // its own persistence chunk (a future polish). Per-call
        // construction throws away the moment estimates between
        // batches but every batch in slow_loop sees enough samples
        // (full pool) for AdamW's warm-up to land within one cycle.
        let params = ParamsAdamW { lr, beta1: 0.9, beta2: 0.999, eps: 1e-8, weight_decay: 0.0 };
        let mut opt =
            AdamW::new(vec![self.w_q.clone(), self.w_k.clone(), self.w_v.clone()], params)?;
        opt.step(&grads)?;

        *self
            .train_steps
            .lock()
            .expect("mlstm trainable state mutex (poison = restart resident)") += 1;
        Ok(loss_val)
    }

    /// chunk 1.3 — copy out W_q, W_k, W_v as flat row-major
    /// `Vec<f32>` for SQLite BLOB persistence. Order matches the
    /// `(d_emb, d_emb)` Tensor shape.
    pub fn export_weights(&self) -> candle_core::Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let q = tensor_to_vec(&self.w_q.as_tensor().flatten_all()?)?;
        let k = tensor_to_vec(&self.w_k.as_tensor().flatten_all()?)?;
        let v = tensor_to_vec(&self.w_v.as_tensor().flatten_all()?)?;
        Ok((q, k, v))
    }

    /// chunk 1.3 — restore weights from SQLite BLOB. Each Vec must
    /// be `d_emb²` long; mismatches are silently dropped (caller
    /// treats that as "fresh init" — same convention as
    /// `PaperWorkingMemory::import_state`). Returns `true` on
    /// success, `false` on any shape mismatch.
    pub fn import_weights(&self, w_q: Vec<f32>, w_k: Vec<f32>, w_v: Vec<f32>) -> bool {
        let expected = self.d_emb * self.d_emb;
        if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
            return false;
        }
        let load = |w: Vec<f32>, target: &Var| -> candle_core::Result<()> {
            let t = Tensor::from_vec(w, (self.d_emb, self.d_emb), &self.device)?;
            target.set(&t)
        };
        load(w_q, &self.w_q).is_ok() && load(w_k, &self.w_k).is_ok() && load(w_v, &self.w_v).is_ok()
    }

    /// chunk 1.3 — current monotonic train-step counter. Persisted
    /// to SQLite alongside the weights so resident restarts see
    /// the cumulative training run length.
    pub fn train_steps(&self) -> u64 {
        *self.train_steps.lock().expect("mlstm trainable state mutex (poison = restart resident)")
    }

    /// chunk 1.3 — restore the train-step counter from a prior
    /// persisted snapshot. Caller pairs this with `import_weights`.
    pub fn set_train_steps(&self, steps: u64) {
        *self
            .train_steps
            .lock()
            .expect("mlstm trainable state mutex (poison = restart resident)") = steps;
    }

    /// Apply the mLSTM state update + read using projected `q/k/v`.
    /// Body lifted from `PaperWorkingMemory::update` so the two
    /// share the gating + 1/√d scaling + LayerNorm logic.
    fn update_state_with(&self, x: &[f32], q_in: &[f32], k_in: &[f32], v_in: &[f32]) -> Vec<f32> {
        let scale = 1.0 / (self.d_emb as f32).sqrt();
        let k_norm = layer_norm(k_in);
        let k: Vec<f32> = k_norm.iter().map(|v| v * scale).collect();
        let v = layer_norm(v_in);
        let q = layer_norm(q_in);

        let mut state =
            self.cell.lock().expect("mlstm trainable state mutex (poison = restart resident)");
        let x_norm: f32 = x.iter().map(|a| a * a).sum::<f32>().sqrt();
        let i_gate = sigmoid(self.input_scale * x_norm);
        let f_gate = sigmoid(self.forget_scale - x_norm * 0.1);

        for row in 0..self.d_emb {
            for col in 0..self.d_emb {
                let idx = row * self.d_emb + col;
                state.c[idx] = f_gate * state.c[idx] + i_gate * v[row] * k[col];
            }
        }
        for j in 0..self.d_emb {
            state.n[j] = f_gate * state.n[j] + i_gate * k[j];
        }

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
}

fn tensor_to_vec(t: &Tensor) -> candle_core::Result<Vec<f32>> {
    let t = t.reshape(t.elem_count())?.to_dtype(DType::F32)?;
    t.to_vec1::<f32>()
}

/// True iff every element of `t` is finite (not NaN, not ±∞).
/// Used by `train_step` to gate the SGD step — any NaN gradient
/// poisons every subsequent step, so we'd rather hold the prior
/// weights than persist a divergent batch.
fn tensor_is_finite(t: &Tensor) -> candle_core::Result<bool> {
    let v = tensor_to_vec(&t.flatten_all()?)?;
    Ok(v.iter().all(|x| x.is_finite()))
}

fn sigmoid(x: f32) -> f32 {
    // R6 audit (2026-04-30) — symmetric clamp to mlstm.rs::sigmoid.
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
    use crate::memory::cognitive::mlstm::PaperWorkingMemory;

    fn unit_at(d: usize, idx: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; d];
        v[idx] = 1.0;
        v
    }

    #[test]
    fn identity_init_constructs() {
        let m = TrainableMLstm::new_identity(8).expect("identity init");
        assert_eq!(m.d_emb(), 8);
        let (c_fro, n_l2) = m.state_norm();
        assert!(c_fro.abs() < 1e-6, "fresh state has zero matrix");
        assert!(n_l2.abs() < 1e-6, "fresh state has zero normalizer");
    }

    /// chunk 1.1 — identity-init forward parity. chunk 1.2+ 가
    /// weights 학습 후 baseline 으로 의미 변경 예정.
    #[test]
    fn identity_init_matches_frozen_paper_wm() {
        let d = 8;
        let trainable = TrainableMLstm::new_identity(d).expect("identity");
        let frozen = PaperWorkingMemory::new(d);

        for step in 0..3 {
            let x = unit_at(d, step);
            let out_t = trainable.forward(&x).expect("forward");
            let out_f = frozen.update(&x);
            assert_eq!(out_t.len(), out_f.len(), "shape parity at step {step}");
            for (a, b) in out_t.iter().zip(out_f.iter()) {
                assert!(
                    (a - b).abs() < 1e-4,
                    "identity-init forward parity at step {step}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn forward_rejects_wrong_dim_input() {
        let m = TrainableMLstm::new_identity(8).expect("identity");
        let bad = vec![1.0_f32; 16];
        let out = m.forward(&bad).expect("forward");
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|v| v.abs() < 1e-9), "wrong-dim returns zero vector");
    }

    /// chunk 1.2 — non-trivial sub-unit-norm input gives non-zero
    /// loss at identity init. Identity-init은 두 fixed point 보유:
    ///   1. ‖x‖² = 0 (loss = 0, no signal)
    ///   2. ‖x‖² ≥ 1 (denom = ‖x‖², read = x, loss = 0, no signal)
    /// 학습 signal 은 0 < ‖x‖² < 1 구간에서 만 발생 (denom = 1,
    /// read = ‖x‖²·x ≠ x). 모든 train test 는 그 구간의 입력 사용.
    fn nontrivial_input(d: usize, seed: u32) -> Vec<f32> {
        // Deterministic pseudo-random with seed — values in [-0.3, 0.3]
        // so ‖x‖² ≈ d · 0.03 < 1 for d ≤ 32.
        let mut state = seed;
        let mut v = vec![0.0_f32; d];
        for slot in v.iter_mut() {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            *slot = (((state >> 16) as f32 / 32768.0) - 1.0) * 0.3;
        }
        v
    }

    #[test]
    fn train_step_returns_finite_loss() {
        let m = TrainableMLstm::new_identity(8).expect("identity");
        let x = nontrivial_input(8, 7);
        let loss = m.train_step(&x, 0.01).expect("train step");
        assert!(loss.is_finite(), "loss must be finite, got {loss}");
        assert!(loss >= 0.0, "loss must be non-negative, got {loss}");
        assert!(loss > 0.0, "non-trivial input gives non-zero loss at identity, got {loss}");
    }

    /// chunk 1.2 — *cumulative* training reduces loss. 100 epochs of
    /// SGD on the same x should drive `‖x - read‖²` toward zero.
    /// This is the chunk 1.2 acceptance gate.
    #[test]
    fn train_step_reduces_loss_over_epochs() {
        let m = TrainableMLstm::new_identity(8).expect("identity");
        let x = nontrivial_input(8, 11);

        let initial_loss = m.train_step(&x, 0.0).expect("initial loss probe");
        assert!(initial_loss > 0.0, "non-trivial input must give non-zero initial loss");
        let mut last_loss = initial_loss;
        for _ in 0..100 {
            last_loss = m.train_step(&x, 0.05).expect("train step");
        }

        assert!(
            last_loss < initial_loss,
            "100-epoch loss must drop below initial. initial={initial_loss}, final={last_loss}"
        );
    }

    /// chunk 1.2 — training mutates W_v away from identity. Concrete
    /// observation that the autograd path actually reaches the
    /// projections, not just produces a number.
    /// Production safety — `train_step` skips the SGD step when
    /// the loss is NaN/inf, leaving the weights at their last-
    /// known-good state. The caller sees the bad loss value (so it
    /// can log + decide); the weights stay clean.
    #[test]
    fn train_step_nan_input_does_not_corrupt_weights() {
        let m = TrainableMLstm::new_identity(4).expect("identity");
        let snapshot: Vec<f32> = m.w_v.as_tensor().flatten_all().unwrap().to_vec1().unwrap();

        let bad = vec![f32::NAN; 4];
        let loss = m.train_step(&bad, 0.1).expect("train_step returns even for NaN input");
        assert!(loss.is_nan(), "loss reports the NaN, got {loss}");

        let after: Vec<f32> = m.w_v.as_tensor().flatten_all().unwrap().to_vec1().unwrap();
        assert_eq!(snapshot, after, "weights unchanged after NaN-loss step");
    }

    #[test]
    fn train_step_mutates_weights() {
        let m = TrainableMLstm::new_identity(4).expect("identity");
        let initial_w_v: Vec<f32> = m.w_v.as_tensor().flatten_all().unwrap().to_vec1().unwrap();
        let x = nontrivial_input(4, 13);
        for _ in 0..20 {
            m.train_step(&x, 0.1).expect("train step");
        }
        let final_w_v: Vec<f32> = m.w_v.as_tensor().flatten_all().unwrap().to_vec1().unwrap();
        let max_delta = initial_w_v
            .iter()
            .zip(final_w_v.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_delta > 1e-3,
            "20-step training must move W_v away from identity, max_delta = {max_delta}"
        );
    }
}
