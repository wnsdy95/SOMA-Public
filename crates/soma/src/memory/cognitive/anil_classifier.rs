//! ANIL classifier head — v1.2 chunk 2.1 (ADR 0009 §D7).
//!
//! ANIL = Almost No Inner Loop (Raghu 2020 `arXiv:1909.09157`):
//! frozen feature encoder (the active embedder produces 384/1024d
//! features) + trainable classifier head. SOMA's task is project
//! prediction — `episode.project` label, K-class single-label
//! cross-entropy.
//!
//! chunk 2.1 ships the struct + forward pass. Identity-init so
//! K=1 reduces to "predict the single project always", and
//! grow-K paths preserve already-learned rows. Training (chunk
//! 2.2), persistence (chunk 2.3), scheduler (chunk 2.4),
//! self_state attribution (chunk 2.5) follow.
//!
//! Feature-gated behind `cognitive-train` (shared with chunk 1).
//!
//! ADR 0015 disposition: this is a connected context quality
//! candidate. The `self_state("anil", "project_attribution")` row can
//! select default ContextEnvelope project scope when no explicit
//! project/session filter is present; the trainable path remains
//! explicit diagnostics.

#![cfg(feature = "cognitive-train")]

use std::sync::Mutex;

use candle_core::{DType, Device, Tensor, Var};
use candle_nn::{AdamW, Optimizer, ParamsAdamW};

/// Trainable single-linear ANIL head. Frozen encoder feeds into
/// `W_head: (K, d_emb)` + `b_head: (K,)` and softmax + CE-loss.
///
/// `projects` is the row-→-label mapping (sorted, persistent
/// alongside the weights). chunk 2.4's slow_loop grows K when a
/// new project shows up; row index is stable so prior training
/// isn't lost.
pub struct AnilClassifier {
    d_emb: usize,
    /// Trainable head — `(K, d_emb)` row-major. `Mutex<Tensor>`
    /// instead of `Var` because `ensure_project` grows the row
    /// count and `Var::set` enforces shape stability. Training
    /// (chunk 2.2) wraps the current Tensor in a fresh `Var`,
    /// runs forward + backward, then writes the updated Tensor
    /// back — same per-call optimizer pattern as
    /// `TrainableMLstm::train_step`.
    w_head: Mutex<Tensor>,
    /// Trainable bias — `(K,)`. Same Mutex<Tensor> pattern.
    b_head: Mutex<Tensor>,
    /// row-→-project mapping. Length must equal K = w_head.shape[0].
    projects: Mutex<Vec<String>>,
    /// chunk 2.3 — monotonic counter persisted alongside weights.
    train_steps: Mutex<u64>,
    device: Device,
}

impl AnilClassifier {
    /// Identity-init constructor with single project (K=1). Forward
    /// pass returns logit `[0.0]` for any input — softmax = `[1.0]`,
    /// the lone class is always predicted (degenerate but well-
    /// defined). chunk 2.4 grows `projects` as user data accumulates.
    pub fn new_seed(d_emb: usize, initial_project: &str) -> candle_core::Result<Self> {
        let device = Device::Cpu;
        // K=1, zero-init weights → logit always 0 → softmax = 1.0
        // → cross-entropy on the lone label = 0 → no gradient yet.
        // The first new project that joins the classifier moves
        // the head out of this fixed point.
        let w0 = Tensor::zeros((1, d_emb), DType::F32, &device)?;
        let b0 = Tensor::zeros(1, DType::F32, &device)?;
        Ok(Self {
            d_emb,
            w_head: Mutex::new(w0),
            b_head: Mutex::new(b0),
            projects: Mutex::new(vec![initial_project.to_string()]),
            train_steps: Mutex::new(0),
            device,
        })
    }

    pub fn d_emb(&self) -> usize {
        self.d_emb
    }

    pub fn num_classes(&self) -> usize {
        self.projects.lock().expect("anil trainable state mutex (poison = restart resident)").len()
    }

    pub fn projects(&self) -> Vec<String> {
        self.projects
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .clone()
    }

    /// chunk 2.4 hook — append a project row if it's not already
    /// known. Returns `true` when a new row was added (caller saves
    /// the updated weights so the new row persists), `false` when
    /// the project was already in the mapping.
    pub fn ensure_project(&self, project: &str) -> candle_core::Result<bool> {
        let mut projects =
            self.projects.lock().expect("anil trainable state mutex (poison = restart resident)");
        if projects.iter().any(|p| p == project) {
            return Ok(false);
        }
        // Grow weights by one zero-init row + one zero bias element.
        // Existing rows preserved → prior training not lost.
        let new_w_row = Tensor::zeros((1, self.d_emb), DType::F32, &self.device)?;
        let mut w =
            self.w_head.lock().expect("anil trainable state mutex (poison = restart resident)");
        let stacked_w = Tensor::cat(&[&*w, &new_w_row], 0)?;
        *w = stacked_w;

        let new_b_elem = Tensor::zeros(1, DType::F32, &self.device)?;
        let mut b =
            self.b_head.lock().expect("anil trainable state mutex (poison = restart resident)");
        let stacked_b = Tensor::cat(&[&*b, &new_b_elem], 0)?;
        *b = stacked_b;

        projects.push(project.to_string());
        Ok(true)
    }

    /// Forward pass — `features` is the embedder's `dim()` output.
    /// Returns the K-dim softmax probability vector (sums to 1.0).
    /// chunk 2.5 calls this for each episode to attribute project
    /// likelihood.
    pub fn forward(&self, features: &[f32]) -> candle_core::Result<Vec<f32>> {
        if features.len() != self.d_emb {
            return Ok(Vec::new());
        }
        let x = Tensor::from_slice(features, (self.d_emb, 1), &self.device)?;
        let w = self.w_head.lock().expect("anil trainable state mutex (poison = restart resident)");
        let b = self.b_head.lock().expect("anil trainable state mutex (poison = restart resident)");
        // logit = W · x + b → (K, 1)
        let logit = w.matmul(&x)?;
        let logit = logit.squeeze(1)?; // (K,)
        let logit = logit.broadcast_add(&b)?;
        // softmax with stabilizer — subtract max before exp.
        let max = logit.max(0)?;
        let stabilized = logit.broadcast_sub(&max)?;
        let exp_l = stabilized.exp()?;
        let sum = exp_l.sum_all()?;
        let probs = exp_l.broadcast_div(&sum)?;
        probs.to_vec1::<f32>()
    }

    /// chunk 2.3 — flatten weights for SQLite BLOB persistence.
    /// Returns `(w_head_flat, b_head, projects)`. `w_head_flat` is
    /// row-major `(K * d_emb,)`.
    pub fn export(&self) -> candle_core::Result<(Vec<f32>, Vec<f32>, Vec<String>)> {
        let w = self
            .w_head
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .flatten_all()?
            .to_vec1::<f32>()?;
        let b = self
            .b_head
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .to_vec1::<f32>()?;
        let projects = self
            .projects
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .clone();
        Ok((w, b, projects))
    }

    /// chunk 2.3 — restore from a persisted snapshot. Returns
    /// `false` on shape mismatch (caller treats as fresh init).
    /// `w_head_flat.len()` must equal `projects.len() * d_emb`.
    pub fn import(&self, w_head_flat: Vec<f32>, b_head: Vec<f32>, projects: Vec<String>) -> bool {
        let k = projects.len();
        if k == 0 || w_head_flat.len() != k * self.d_emb || b_head.len() != k {
            return false;
        }
        let w_t = match Tensor::from_vec(w_head_flat, (k, self.d_emb), &self.device) {
            Ok(t) => t,
            Err(_) => return false,
        };
        let b_t = match Tensor::from_vec(b_head, k, &self.device) {
            Ok(t) => t,
            Err(_) => return false,
        };
        *self.w_head.lock().expect("anil trainable state mutex (poison = restart resident)") = w_t;
        *self.b_head.lock().expect("anil trainable state mutex (poison = restart resident)") = b_t;
        *self.projects.lock().expect("anil trainable state mutex (poison = restart resident)") =
            projects;
        true
    }

    pub fn train_steps(&self) -> u64 {
        *self.train_steps.lock().expect("anil trainable state mutex (poison = restart resident)")
    }

    pub fn set_train_steps(&self, n: u64) {
        *self.train_steps.lock().expect("anil trainable state mutex (poison = restart resident)") =
            n;
    }

    /// chunk 2.2 — one AdamW step on cross-entropy loss for the
    /// `(features, label_idx)` pair. Returns scalar loss.
    ///
    /// Layer 1 NaN guard: divergent loss → skip optimizer step,
    /// caller (slow_loop chunk 2.4) tracks `diverged` count.
    /// `label_idx` must be in `0..num_classes`; out-of-bounds →
    /// caller did `ensure_project` first, so this is a programmer
    /// error (return `Err`).
    pub fn train_step(
        &self,
        features: &[f32],
        label_idx: usize,
        lr: f64,
    ) -> candle_core::Result<f32> {
        if features.len() != self.d_emb {
            return Ok(0.0);
        }
        let k = self.num_classes();
        if k == 0 || label_idx >= k {
            return Err(candle_core::Error::Msg(format!(
                "label_idx {label_idx} out of range [0, {k})"
            )));
        }

        // Take Tensor snapshots out of the Mutex into fresh Vars
        // so candle's autograd has a graph root to back-prop into.
        // Same per-call optimizer pattern as TrainableMLstm.
        let w_var = {
            let w =
                self.w_head.lock().expect("anil trainable state mutex (poison = restart resident)");
            Var::from_tensor(&w)?
        };
        let b_var = {
            let b =
                self.b_head.lock().expect("anil trainable state mutex (poison = restart resident)");
            Var::from_tensor(&b)?
        };

        let x = Tensor::from_slice(features, (self.d_emb, 1), &self.device)?;
        let logit = w_var.as_tensor().matmul(&x)?.squeeze(1)?;
        let logit = logit.broadcast_add(b_var.as_tensor())?;
        // log_softmax + nll = cross-entropy. Use the candle_nn
        // utility-equivalent: max-stabilize, log, gather label.
        let max = logit.max(0)?;
        let stabilized = logit.broadcast_sub(&max)?;
        let exp_l = stabilized.exp()?;
        let sum = exp_l.sum_all()?;
        let log_softmax = stabilized.broadcast_sub(&sum.log()?)?;
        // Loss = -log_softmax[label_idx]. Index via narrow on dim 0.
        let label_log_prob = log_softmax.narrow(0, label_idx, 1)?.squeeze(0)?;
        let loss = label_log_prob.neg()?;

        let loss_val: f32 = loss.to_scalar()?;
        if !loss_val.is_finite() {
            return Ok(loss_val);
        }

        let grads = loss.backward()?;
        // Sanity NaN check on grads.
        for var in [&w_var, &b_var] {
            let g =
                grads.get(var).ok_or_else(|| candle_core::Error::Msg("missing gradient".into()))?;
            let g_vec = g.flatten_all()?.to_vec1::<f32>()?;
            if !g_vec.iter().all(|v| v.is_finite()) {
                return Ok(loss_val);
            }
        }

        let params = ParamsAdamW { lr, beta1: 0.9, beta2: 0.999, eps: 1e-8, weight_decay: 0.0 };
        let mut opt = AdamW::new(vec![w_var.clone(), b_var.clone()], params)?;
        opt.step(&grads)?;

        // Write the updated weights back to the Mutex<Tensor> store.
        *self.w_head.lock().expect("anil trainable state mutex (poison = restart resident)") =
            w_var.as_tensor().clone();
        *self.b_head.lock().expect("anil trainable state mutex (poison = restart resident)") =
            b_var.as_tensor().clone();
        *self
            .train_steps
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)") += 1;
        Ok(loss_val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_init_constructs_with_one_class() {
        let m = AnilClassifier::new_seed(8, "soma").expect("seed init");
        assert_eq!(m.d_emb(), 8);
        assert_eq!(m.num_classes(), 1);
        assert_eq!(m.projects(), vec!["soma".to_string()]);
        assert_eq!(m.train_steps(), 0);
    }

    /// chunk 2.1 — K=1 forward pass returns `[1.0]` regardless of
    /// input. softmax of zero logit is 1.0 in 1-class case.
    #[test]
    fn forward_with_seed_init_returns_unit_probability() {
        let m = AnilClassifier::new_seed(4, "soma").expect("seed");
        let x = vec![0.5_f32, -0.3, 0.7, -0.1];
        let probs = m.forward(&x).expect("forward");
        assert_eq!(probs.len(), 1);
        assert!((probs[0] - 1.0).abs() < 1e-5, "K=1 softmax = 1.0, got {}", probs[0]);
    }

    #[test]
    fn ensure_project_grows_weights_zero_init() {
        let m = AnilClassifier::new_seed(4, "soma").expect("seed");
        let added = m.ensure_project("myapp").expect("grow");
        assert!(added, "new project added");
        assert_eq!(m.num_classes(), 2);
        assert_eq!(m.projects(), vec!["soma".to_string(), "myapp".to_string()]);

        // Re-adding existing returns false.
        let added2 = m.ensure_project("soma").expect("idempotent");
        assert!(!added2, "duplicate not added");

        // K=2 forward returns 2 probabilities summing to 1.
        let x = vec![1.0, 0.0, 0.0, 0.0];
        let probs = m.forward(&x).expect("forward K=2");
        assert_eq!(probs.len(), 2);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "softmax sums to 1: {sum}");
        // Both rows are zero-init → uniform distribution.
        assert!((probs[0] - 0.5).abs() < 1e-5);
        assert!((probs[1] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn forward_rejects_wrong_dim() {
        let m = AnilClassifier::new_seed(8, "soma").expect("seed");
        let bad = vec![1.0_f32; 4];
        let probs = m.forward(&bad).expect("forward");
        assert!(probs.is_empty(), "wrong-dim returns empty");
    }

    #[test]
    fn export_import_roundtrips() {
        let m = AnilClassifier::new_seed(4, "alpha").expect("seed");
        m.ensure_project("beta").expect("grow");
        m.set_train_steps(7);

        let (w, b, projects) = m.export().expect("export");
        assert_eq!(w.len(), 2 * 4);
        assert_eq!(b.len(), 2);
        assert_eq!(projects, vec!["alpha".to_string(), "beta".to_string()]);

        let m2 = AnilClassifier::new_seed(4, "placeholder").expect("seed2");
        let ok = m2.import(w, b, projects);
        assert!(ok, "import succeeds");
        assert_eq!(m2.num_classes(), 2);
        assert_eq!(m2.projects(), vec!["alpha".to_string(), "beta".to_string()]);
    }

    /// chunk 2.2 — train_step on (features, label) drives loss
    /// down. K=2, 100 epochs of pure label-0, loss must drop.
    #[test]
    fn train_step_reduces_loss_over_epochs() {
        let m = AnilClassifier::new_seed(4, "alpha").expect("seed");
        m.ensure_project("beta").expect("grow");

        // Non-trivial features so the gradient isn't zero at zero-init.
        let x = vec![0.5_f32, -0.3, 0.7, 0.1];
        let label = 0_usize;

        let initial = m.train_step(&x, label, 0.0).expect("loss probe");
        let mut last = initial;
        for _ in 0..100 {
            last = m.train_step(&x, label, 0.05).expect("train step");
        }

        assert!(initial > 0.0, "non-trivial features → non-zero initial CE loss");
        assert!(last < initial, "100 epoch loss must drop. initial={initial} last={last}");
        assert_eq!(m.train_steps(), 101, "100 SGD steps + 1 probe");
    }

    /// chunk 2.2 — NaN-feature input → loss is NaN, weights stay
    /// at last-known-good (Layer 1 guard). Mirrors mLSTM safety.
    #[test]
    fn train_step_nan_feature_does_not_corrupt_weights() {
        let m = AnilClassifier::new_seed(4, "alpha").expect("seed");
        m.ensure_project("beta").expect("grow");

        let snapshot: Vec<f32> = m
            .w_head
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .flatten_all()
            .unwrap()
            .to_vec1()
            .unwrap();
        let bad = vec![f32::NAN; 4];
        let loss = m.train_step(&bad, 0, 0.1).expect("train_step returns");
        assert!(loss.is_nan(), "NaN feature → NaN loss");
        let after: Vec<f32> = m
            .w_head
            .lock()
            .expect("anil trainable state mutex (poison = restart resident)")
            .flatten_all()
            .unwrap()
            .to_vec1()
            .unwrap();
        assert_eq!(snapshot, after, "weights unchanged after NaN-loss step");
    }

    #[test]
    fn train_step_rejects_out_of_range_label() {
        let m = AnilClassifier::new_seed(4, "alpha").expect("seed");
        let x = vec![0.5_f32, 0.5, 0.5, 0.5];
        // K=1, label=1 is out of range.
        let res = m.train_step(&x, 1, 0.1);
        assert!(res.is_err(), "out-of-range label is a programmer error");
    }

    #[test]
    fn import_rejects_shape_mismatch() {
        let m = AnilClassifier::new_seed(4, "alpha").expect("seed");
        // K = 2 projects but only K*d_emb-1 weights → mismatch.
        let ok = m.import(vec![0.0; 7], vec![0.0; 2], vec!["a".into(), "b".into()]);
        assert!(!ok, "shape mismatch rejected");
    }
}
