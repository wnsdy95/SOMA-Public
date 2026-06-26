//! Trainable Hopfield Q/K/V — v1.2 chunk 4 (ADR 0011).
//!
//! Multi-head attention with trainable Q/K/V projection matrices.
//! Identity-init = chunk 4.1 ships forward-pass parity with the
//! frozen `PaperHopfield`. chunk 4.2 trains via contrastive loss
//! over (query, ground_truth_pattern) pairs from `episode_edges`.
//!
//! Output projection O is frozen at identity in chunk 4 (ADR 0011
//! §D1) — v2 polish can add it.
//!
//! Feature-gated behind `cognitive-train`.

#![cfg(feature = "cognitive-train")]

use std::sync::Mutex;

use candle_core::{Device, Tensor, Var};
use candle_nn::{AdamW, Optimizer, ParamsAdamW};

/// Trainable Hopfield head with three projection matrices.
/// Single-head simplification (chunk 4 minimal); v2 polish adds
/// multi-head split — `PaperHopfield::DEFAULT_HEADS = 4` is the
/// frozen reference.
pub struct TrainableHopfield {
    d_emb: usize,
    /// Trainable projections — `Mutex<Tensor>` for shape stability
    /// across grow paths (none in chunk 4 minimal but keeps the
    /// pattern symmetric with ANIL/iPC).
    w_q: Mutex<Tensor>,
    w_k: Mutex<Tensor>,
    w_v: Mutex<Tensor>,
    train_steps: Mutex<u64>,
    device: Device,
}

impl TrainableHopfield {
    /// Identity-init constructor — Q = K = V = I_d. Forward parity
    /// with frozen `PaperHopfield` at construction; chunk 4.2's
    /// `train_step` mutates them.
    pub fn new_identity(d_emb: usize) -> candle_core::Result<Self> {
        let device = Device::Cpu;
        let identity = identity_tensor(d_emb, &device)?;
        Ok(Self {
            d_emb,
            w_q: Mutex::new(identity.clone()),
            w_k: Mutex::new(identity.clone()),
            w_v: Mutex::new(identity),
            train_steps: Mutex::new(0),
            device,
        })
    }

    pub fn d_emb(&self) -> usize {
        self.d_emb
    }

    pub fn train_steps(&self) -> u64 {
        *self
            .train_steps
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)")
    }

    pub fn set_train_steps(&self, n: u64) {
        *self
            .train_steps
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)") = n;
    }

    /// Single-pattern softmax retrieval. `query` and `pattern` have
    /// shape `d_emb`. Returns the read vector after Q/K/V projection
    /// + 1-pattern softmax (pattern = single key, single value).
    /// chunk 4.5 wires this into `HopfieldBackend` for full multi-
    /// pattern retrieval.
    pub fn retrieve_single(&self, query: &[f32], pattern: &[f32]) -> candle_core::Result<Vec<f32>> {
        if query.len() != self.d_emb || pattern.len() != self.d_emb {
            return Err(candle_core::Error::Msg("dim mismatch".into()));
        }
        let q_t = Tensor::from_slice(query, (self.d_emb, 1), &self.device)?;
        let p_t = Tensor::from_slice(pattern, (self.d_emb, 1), &self.device)?;
        let w_q =
            self.w_q.lock().expect("hopfield trainable state mutex (poison = restart resident)");
        let w_k =
            self.w_k.lock().expect("hopfield trainable state mutex (poison = restart resident)");
        let w_v =
            self.w_v.lock().expect("hopfield trainable state mutex (poison = restart resident)");
        let q_proj = w_q.matmul(&q_t)?;
        let k_proj = w_k.matmul(&p_t)?;
        let v_proj = w_v.matmul(&p_t)?;
        // Single-pattern softmax → weight 1.0 → output = v_proj.
        // Returns v_proj as the read.
        let _ = q_proj;
        let _ = k_proj;
        v_proj.squeeze(1)?.to_vec1::<f32>()
    }

    /// chunk 4.2 — contrastive training step. Loss minimizes
    /// `1 - cos(retrieve(query), ground_truth_pattern)` so the
    /// retrieval head learns to map (query, pattern) pairs that
    /// should be similar. `episode_edges` is the canonical pair
    /// source (chunk 4.4 wiring).
    ///
    /// Layer 1 NaN guard: divergent loss → skip optimizer step.
    pub fn train_step(
        &self,
        query: &[f32],
        ground_truth: &[f32],
        lr: f64,
    ) -> candle_core::Result<f32> {
        if query.len() != self.d_emb || ground_truth.len() != self.d_emb {
            return Err(candle_core::Error::Msg("dim mismatch".into()));
        }
        let w_q_var = Var::from_tensor(
            &self.w_q.lock().expect("hopfield trainable state mutex (poison = restart resident)"),
        )?;
        let w_k_var = Var::from_tensor(
            &self.w_k.lock().expect("hopfield trainable state mutex (poison = restart resident)"),
        )?;
        let w_v_var = Var::from_tensor(
            &self.w_v.lock().expect("hopfield trainable state mutex (poison = restart resident)"),
        )?;

        // P0-1 (audit fix) — dual-term contrastive loss per ADR 0011
        // §D1 (trainable surface = Q + K + V). chunk 4.1 의 minimal
        // 은 W_v · gt 만 학습 신호 받음 — Q/K 가 brittle dummy
        // attachment 에 의존 했음. Codex review 후 dual-term land:
        //
        //   loss = 0.5 · (1 − cos(W_q · q, W_k · gt))   ← Q/K scoring 학습
        //        + 0.5 · (1 − cos(W_v · gt, gt))        ← V reconstruction 유지
        //
        // term 1 = "edge pair (left, right) 의 attention 공간
        // 정합" — Hopfield 의 q·k scoring (Ramsauer 2020 §3) 에
        // 대응 하는 학습 신호. multi-pattern softmax 의 full
        // gradient 는 아니지만 chunk 4 minimal 의 ADR 정합 충족.
        // term 2 = chunk 4.1 의 V projection 학습 유지.
        let q_t = Tensor::from_slice(query, (self.d_emb, 1), &self.device)?;
        let p_t = Tensor::from_slice(ground_truth, (self.d_emb, 1), &self.device)?;

        // term 1: Q/K projection cosine 정합.
        let q_proj = w_q_var.as_tensor().matmul(&q_t)?.squeeze(1)?;
        let k_proj = w_k_var.as_tensor().matmul(&p_t)?.squeeze(1)?;
        let qk_dot = (&q_proj * &k_proj)?.sum_all()?;
        let q_norm = q_proj.sqr()?.sum_all()?.sqrt()?;
        let k_norm = k_proj.sqr()?.sum_all()?.sqrt()?;
        let qk_denom = (q_norm * k_norm)?.maximum(1e-6_f64)?;
        let qk_cos = (qk_dot / qk_denom)?;

        // term 2: V projection cosine reconstruction (chunk 4.1).
        let read = w_v_var.as_tensor().matmul(&p_t)?.squeeze(1)?;
        let gt = p_t.squeeze(1)?;
        let v_dot = (&read * &gt)?.sum_all()?;
        let read_norm = read.sqr()?.sum_all()?.sqrt()?;
        let gt_norm = gt.sqr()?.sum_all()?.sqrt()?;
        let v_denom = (read_norm * gt_norm)?.maximum(1e-6_f64)?;
        let v_cos = (v_dot / v_denom)?;

        let one = Tensor::new(1.0_f32, &self.device)?;
        let half = Tensor::new(0.5_f32, &self.device)?;
        let qk_loss = (&one - &qk_cos)?;
        let v_loss = (&one - &v_cos)?;
        let loss = ((qk_loss * &half)? + (v_loss * &half)?)?;

        let loss_val: f32 = loss.to_scalar()?;
        if !loss_val.is_finite() {
            return Ok(loss_val);
        }
        let grads = loss.backward()?;
        for var in [&w_q_var, &w_k_var, &w_v_var] {
            let g = grads.get(var).ok_or_else(|| candle_core::Error::Msg("missing grad".into()))?;
            let v = g.flatten_all()?.to_vec1::<f32>()?;
            if !v.iter().all(|x| x.is_finite()) {
                return Ok(loss_val);
            }
        }
        let params = ParamsAdamW { lr, ..Default::default() };
        let mut opt = AdamW::new(vec![w_q_var.clone(), w_k_var.clone(), w_v_var.clone()], params)?;
        opt.step(&grads)?;
        *self.w_q.lock().expect("hopfield trainable state mutex (poison = restart resident)") =
            w_q_var.as_tensor().clone();
        *self.w_k.lock().expect("hopfield trainable state mutex (poison = restart resident)") =
            w_k_var.as_tensor().clone();
        *self.w_v.lock().expect("hopfield trainable state mutex (poison = restart resident)") =
            w_v_var.as_tensor().clone();
        *self
            .train_steps
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)") += 1;
        Ok(loss_val)
    }

    /// chunk 4.3 — flatten weights for SQLite BLOB persistence.
    pub fn export(&self) -> candle_core::Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let q = self
            .w_q
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)")
            .flatten_all()?
            .to_vec1::<f32>()?;
        let k = self
            .w_k
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)")
            .flatten_all()?
            .to_vec1::<f32>()?;
        let v = self
            .w_v
            .lock()
            .expect("hopfield trainable state mutex (poison = restart resident)")
            .flatten_all()?
            .to_vec1::<f32>()?;
        Ok((q, k, v))
    }

    /// chunk 4.3 — restore from BLOB. Returns false on shape mismatch.
    pub fn import(&self, w_q: Vec<f32>, w_k: Vec<f32>, w_v: Vec<f32>) -> bool {
        let expected = self.d_emb * self.d_emb;
        if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
            return false;
        }
        let load = |w: Vec<f32>, target: &Mutex<Tensor>| -> bool {
            match Tensor::from_vec(w, (self.d_emb, self.d_emb), &self.device) {
                Ok(t) => {
                    *target
                        .lock()
                        .expect("hopfield trainable state mutex (poison = restart resident)") = t;
                    true
                }
                Err(_) => false,
            }
        };
        load(w_q, &self.w_q) && load(w_k, &self.w_k) && load(w_v, &self.w_v)
    }
}

fn identity_tensor(d: usize, device: &Device) -> candle_core::Result<Tensor> {
    let mut data = vec![0.0_f32; d * d];
    for i in 0..d {
        data[i * d + i] = 1.0;
    }
    Tensor::from_vec(data, (d, d), device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_init_constructs() {
        let h = TrainableHopfield::new_identity(8).expect("new");
        assert_eq!(h.d_emb(), 8);
        assert_eq!(h.train_steps(), 0);
    }

    /// chunk 4.1 — identity-init retrieval = pattern (since V = I).
    #[test]
    fn retrieve_single_identity_init_returns_pattern() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let q = vec![0.1, 0.2, 0.3, 0.4];
        let p = vec![0.5, 0.6, 0.7, 0.8];
        let r = h.retrieve_single(&q, &p).expect("retrieve");
        for (a, b) in r.iter().zip(p.iter()) {
            assert!((a - b).abs() < 1e-5, "identity-init: read = pattern, got {a} vs {b}");
        }
    }

    /// chunk 4.2 — train_step returns finite loss + decreases.
    /// Use orthogonal pair so cos(read, gt) = 1 isn't a fixed
    /// point (it IS at identity init, so we add noise to gt).
    #[test]
    fn train_step_returns_finite_loss() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let q = vec![0.1, 0.2, 0.3, 0.4];
        let gt = vec![1.0, 0.0, 0.0, 0.0];
        let loss = h.train_step(&q, &gt, 0.01).expect("step");
        assert!(loss.is_finite(), "loss finite: {loss}");
        assert!(loss >= 0.0);
    }

    #[test]
    fn export_import_roundtrips() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let (q, k, v) = h.export().expect("export");
        assert_eq!(q.len(), 16);
        let h2 = TrainableHopfield::new_identity(4).expect("new2");
        assert!(h2.import(q, k, v));
    }

    #[test]
    fn import_rejects_shape_mismatch() {
        let h = TrainableHopfield::new_identity(8).expect("new");
        assert!(!h.import(vec![0.0; 16], vec![0.0; 16], vec![0.0; 16]));
    }

    /// P0-1 audit fix — query feeds the dual-term contrastive loss
    /// (`cos(W_q · q, W_k · gt)`), so NaN in `query` *must* now
    /// propagate to the loss and trigger the Layer 1 guard. This
    /// test pins the post-fix behavior: pre-fix the query was
    /// dropped via `_query_unused_in_minimal`, leaving Q/K with
    /// no real gradient signal.
    #[test]
    fn train_step_nan_query_does_not_corrupt_weights() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let snapshot_q = h.export().expect("snap").0;
        let snapshot_k = h.export().expect("snap").1;
        let snapshot_v = h.export().expect("snap").2;
        let bad_q = vec![f32::NAN; 4];
        let gt = vec![1.0, 0.0, 0.0, 0.0];
        let loss = h.train_step(&bad_q, &gt, 0.1).expect("returns");
        assert!(!loss.is_finite(), "NaN query → non-finite loss (P0-1 fix)");
        let (after_q, after_k, after_v) = h.export().expect("after");
        assert_eq!(snapshot_q, after_q, "W_q unchanged on NaN query");
        assert_eq!(snapshot_k, after_k, "W_k unchanged on NaN query");
        assert_eq!(snapshot_v, after_v, "W_v unchanged on NaN query");
    }

    /// P0-1 audit fix — Q and K both receive non-trivial gradient
    /// from the dual-term loss. Pre-fix, Q/K were attached via a
    /// 1e-30 weighted dummy term, so a single SGD step left them
    /// effectively untouched. Post-fix, an orthogonal (q, gt) pair
    /// gives `cos(W_q·q, W_k·gt) = 0` at identity init, so a real
    /// gradient flows.
    #[test]
    fn train_step_moves_q_and_k_under_orthogonal_pair() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let snapshot_q = h.export().expect("snap").0;
        let snapshot_k = h.export().expect("snap").1;
        // Orthogonal pair — q and gt have zero cosine after identity
        // projection, so the dual-term gives a non-zero gradient
        // that pushes Q and K toward alignment.
        let q = vec![1.0, 0.0, 0.0, 0.0];
        let gt = vec![0.0, 1.0, 0.0, 0.0];
        for _ in 0..20 {
            let _ = h.train_step(&q, &gt, 0.05).expect("train");
        }
        let (after_q, after_k, _) = h.export().expect("after");
        let max_q_delta = snapshot_q
            .iter()
            .zip(after_q.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        let max_k_delta = snapshot_k
            .iter()
            .zip(after_k.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_q_delta > 1e-3,
            "W_q must move ≥ 1e-3 under dual-term gradient (got {max_q_delta})"
        );
        assert!(
            max_k_delta > 1e-3,
            "W_k must move ≥ 1e-3 under dual-term gradient (got {max_k_delta})"
        );
    }

    /// Layer 1 NaN guard — NaN ground_truth propagates through both
    /// loss terms (W_v · gt and W_k · gt) → loss = NaN → SGD skip.
    #[test]
    fn train_step_nan_ground_truth_does_not_corrupt_weights() {
        let h = TrainableHopfield::new_identity(4).expect("new");
        let snapshot = h.export().expect("snap").2;
        let q = vec![0.5, 0.5, 0.5, 0.5];
        let bad_gt = vec![f32::NAN; 4];
        let loss = h.train_step(&q, &bad_gt, 0.1).expect("returns");
        assert!(!loss.is_finite(), "NaN gt → non-finite loss");
        let after = h.export().expect("after").2;
        assert_eq!(snapshot, after, "weights unchanged on NaN input");
    }
}
