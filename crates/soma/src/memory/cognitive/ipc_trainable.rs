//! Trainable iPC predictor — v1.2 chunk 3 (ADR 0010).
//!
//! Multi-layer predictive coding with trainable top-down predictors
//! `W_l: (d_l, d_{l+1})`. Each layer's prediction error
//! `ε_l = latent_l - W_l · latent_{l+1}` is the iPC training signal;
//! free energy `F = Σ‖ε_l‖²` minimization drives the optimizer.
//!
//! `PaperPC` (frozen-weight inference) ships the identity-init
//! baseline; this module wraps that with `Var`-backed predictors
//! so chunk 3.4's slow_loop can train them on the user's episode
//! flow.
//!
//! Feature-gated behind `cognitive-train`.
//!
//! ADR 0015 disposition: ingest-time frozen iPC free-energy is wired
//! to cited `ContextEnvelope.open_decisions` anomalies. This trainable
//! predictor remains an explicit diagnostic until learned free-energy
//! improves that envelope-quality output.

#![cfg(feature = "cognitive-train")]

use std::sync::Mutex;

use candle_core::{Device, Tensor, Var};
use candle_nn::{AdamW, Optimizer, ParamsAdamW};

/// Multi-layer iPC predictor stack. `dims[l]` = layer-l latent
/// dim. `predictors[l]: Var (dims[l], dims[l+1])` is the
/// top-down projection. Identity-init = first `min(d_l, d_{l+1})`
/// rows are an identity submatrix (pseudo-identity for non-square
/// shapes); rest are zeros.
pub struct TrainablePc {
    dims: Vec<usize>,
    /// `predictors[l]` projects `latent_{l+1}` (d_{l+1}-dim) →
    /// predicted `latent_l` (d_l-dim). Length = `dims.len() - 1`.
    predictors: Vec<Mutex<Tensor>>,
    train_steps: Mutex<u64>,
    device: Device,
}

impl TrainablePc {
    pub fn new(dims: Vec<usize>) -> candle_core::Result<Self> {
        if dims.len() < 2 {
            return Err(candle_core::Error::Msg("TrainablePc needs at least 2 layer dims".into()));
        }
        let device = Device::Cpu;
        let mut predictors = Vec::with_capacity(dims.len() - 1);
        for l in 0..dims.len() - 1 {
            let d_in = dims[l + 1];
            let d_out = dims[l];
            let w = pseudo_identity(d_out, d_in, &device)?;
            predictors.push(Mutex::new(w));
        }
        Ok(Self { dims, predictors, train_steps: Mutex::new(0), device })
    }

    pub fn dims(&self) -> &[usize] {
        &self.dims
    }

    pub fn num_layers(&self) -> usize {
        self.dims.len()
    }

    pub fn train_steps(&self) -> u64 {
        *self.train_steps.lock().expect("ipc trainable state mutex (poison = restart resident)")
    }

    pub fn set_train_steps(&self, n: u64) {
        *self.train_steps.lock().expect("ipc trainable state mutex (poison = restart resident)") =
            n;
    }

    /// Free energy `F = Σ‖ε_l‖²` over the supplied multi-layer
    /// latents. Same shape signature as `PaperPC::compute_free_
    /// energy` so `salience.rs` can swap.
    pub fn compute_free_energy(&self, latents: &[Vec<f32>]) -> candle_core::Result<f32> {
        if latents.len() != self.dims.len() {
            return Err(candle_core::Error::Msg(format!(
                "latents.len() = {} but dims.len() = {}",
                latents.len(),
                self.dims.len()
            )));
        }
        for (l, lat) in latents.iter().enumerate() {
            if lat.len() != self.dims[l] {
                return Err(candle_core::Error::Msg(format!(
                    "latent layer {l} dim {} ≠ expected {}",
                    lat.len(),
                    self.dims[l]
                )));
            }
        }

        let mut total = 0.0_f32;
        for l in 0..self.dims.len() - 1 {
            let lat_top = Tensor::from_slice(&latents[l + 1], (self.dims[l + 1], 1), &self.device)?;
            let w = self.predictors[l]
                .lock()
                .expect("ipc trainable state mutex (poison = restart resident)");
            let pred = w.matmul(&lat_top)?.squeeze(1)?;
            let actual = Tensor::from_slice(&latents[l], self.dims[l], &self.device)?;
            let eps = (actual - pred)?;
            let sq = eps.sqr()?.sum_all()?;
            total += sq.to_scalar::<f32>()?;
        }
        Ok(total)
    }

    /// One AdamW step on `F = Σ‖ε_l‖²`. Returns the scalar loss.
    /// Layer 1 NaN guard mirrors mLSTM/ANIL.
    pub fn train_step(&self, latents: &[Vec<f32>], lr: f64) -> candle_core::Result<f32> {
        if latents.len() != self.dims.len() {
            return Err(candle_core::Error::Msg("latents shape mismatch".into()));
        }
        for (l, lat) in latents.iter().enumerate() {
            if lat.len() != self.dims[l] {
                return Err(candle_core::Error::Msg(format!("latent layer {l} dim mismatch")));
            }
        }

        // Snapshot every predictor into a fresh Var (per-call optimizer
        // pattern, same as mLSTM and ANIL chunks).
        let vars: Vec<Var> = self
            .predictors
            .iter()
            .map(|m| {
                let w = m.lock().expect("ipc trainable state mutex (poison = restart resident)");
                Var::from_tensor(&w)
            })
            .collect::<candle_core::Result<Vec<_>>>()?;

        // Forward — accumulate Σ‖ε_l‖².
        let mut total: Option<Tensor> = None;
        for l in 0..self.dims.len() - 1 {
            let lat_top = Tensor::from_slice(&latents[l + 1], (self.dims[l + 1], 1), &self.device)?;
            let pred = vars[l].as_tensor().matmul(&lat_top)?.squeeze(1)?;
            let actual = Tensor::from_slice(&latents[l], self.dims[l], &self.device)?;
            let eps = (actual - pred)?;
            let sq = eps.sqr()?.sum_all()?;
            total = Some(match total {
                Some(prev) => (prev + sq)?,
                None => sq,
            });
        }
        let loss = total.ok_or_else(|| candle_core::Error::Msg("no layers to train".into()))?;
        let loss_val: f32 = loss.to_scalar()?;
        if !loss_val.is_finite() {
            return Ok(loss_val);
        }

        let grads = loss.backward()?;
        for v in &vars {
            let g = grads.get(v).ok_or_else(|| candle_core::Error::Msg("missing grad".into()))?;
            let g_vec = g.flatten_all()?.to_vec1::<f32>()?;
            if !g_vec.iter().all(|x| x.is_finite()) {
                return Ok(loss_val);
            }
        }

        let params = ParamsAdamW { lr, ..Default::default() };
        let mut opt = AdamW::new(vars.clone(), params)?;
        opt.step(&grads)?;

        for (l, var) in vars.into_iter().enumerate() {
            *self.predictors[l]
                .lock()
                .expect("ipc trainable state mutex (poison = restart resident)") =
                var.as_tensor().clone();
        }
        *self.train_steps.lock().expect("ipc trainable state mutex (poison = restart resident)") +=
            1;
        Ok(loss_val)
    }

    /// chunk 3.3 — flatten predictor `l` for SQLite BLOB persistence.
    pub fn export_layer(&self, l: usize) -> candle_core::Result<Vec<f32>> {
        if l >= self.predictors.len() {
            return Err(candle_core::Error::Msg(format!("layer {l} out of range")));
        }
        self.predictors[l]
            .lock()
            .expect("ipc trainable state mutex (poison = restart resident)")
            .flatten_all()?
            .to_vec1::<f32>()
    }

    /// chunk 3.3 — restore predictor `l` from BLOB. Returns false on
    /// shape mismatch.
    pub fn import_layer(&self, l: usize, w_flat: Vec<f32>) -> bool {
        if l >= self.predictors.len() {
            return false;
        }
        let d_out = self.dims[l];
        let d_in = self.dims[l + 1];
        if w_flat.len() != d_out * d_in {
            return false;
        }
        let t = match Tensor::from_vec(w_flat, (d_out, d_in), &self.device) {
            Ok(t) => t,
            Err(_) => return false,
        };
        *self.predictors[l]
            .lock()
            .expect("ipc trainable state mutex (poison = restart resident)") = t;
        true
    }
}

fn pseudo_identity(d_out: usize, d_in: usize, device: &Device) -> candle_core::Result<Tensor> {
    let mut data = vec![0.0_f32; d_out * d_in];
    let m = d_out.min(d_in);
    for i in 0..m {
        data[i * d_in + i] = 1.0;
    }
    Tensor::from_vec(data, (d_out, d_in), device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_constructs_layer_predictors() {
        let pc = TrainablePc::new(vec![8, 4, 2]).expect("new");
        assert_eq!(pc.num_layers(), 3);
        assert_eq!(pc.dims(), &[8, 4, 2]);
    }

    #[test]
    fn rejects_too_few_layers() {
        assert!(TrainablePc::new(vec![8]).is_err());
    }

    #[test]
    fn free_energy_zero_when_latents_consistent_with_pseudo_identity() {
        // Construct latents where layer-l = top-d_l portion of layer-{l-1}
        // such that pseudo-identity predictor reproduces it exactly.
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        // l1 = [a, b], pseudo_identity: pred = [a, b, 0, 0]
        let l0 = vec![1.0, 2.0, 0.0, 0.0];
        let l1 = vec![1.0, 2.0];
        let fe = pc.compute_free_energy(&[l0, l1]).expect("fe");
        assert!(fe.abs() < 1e-5, "consistent latents → F ≈ 0, got {fe}");
    }

    #[test]
    fn free_energy_nonzero_under_mismatch() {
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        let l0 = vec![1.0, 2.0, 3.0, 4.0];
        let l1 = vec![10.0, 20.0];
        let fe = pc.compute_free_energy(&[l0, l1]).expect("fe");
        assert!(fe > 0.0, "mismatch → F > 0, got {fe}");
    }

    #[test]
    fn train_step_reduces_loss() {
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        let l0 = vec![0.5_f32, -0.3, 0.7, 0.1];
        let l1 = vec![0.2, 0.4];
        let initial = pc.train_step(&[l0.clone(), l1.clone()], 0.0).expect("probe");
        let mut last = initial;
        for _ in 0..50 {
            last = pc.train_step(&[l0.clone(), l1.clone()], 0.05).expect("step");
        }
        assert!(initial > 0.0);
        assert!(last < initial, "50-epoch loss must drop. initial={initial} last={last}");
    }

    #[test]
    fn export_import_roundtrips_layer() {
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        let l = pc.export_layer(0).expect("export");
        assert_eq!(l.len(), 4 * 2);
        let pc2 = TrainablePc::new(vec![4, 2]).expect("new");
        assert!(pc2.import_layer(0, l));
    }

    #[test]
    fn import_layer_rejects_shape_mismatch() {
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        assert!(!pc.import_layer(0, vec![0.0; 7]));
    }

    #[test]
    fn train_step_nan_input_does_not_corrupt_predictors() {
        let pc = TrainablePc::new(vec![4, 2]).expect("new");
        let snapshot = pc.export_layer(0).expect("snap");
        let bad = vec![f32::NAN; 4];
        let l1 = vec![0.0, 0.0];
        let loss = pc.train_step(&[bad, l1], 0.1).expect("returns");
        assert!(loss.is_nan() || !loss.is_finite());
        let after = pc.export_layer(0).expect("after");
        assert_eq!(snapshot, after, "predictors unchanged on NaN");
    }
}
