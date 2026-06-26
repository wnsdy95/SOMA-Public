//! D152 chunk 1.2 — View 1 (quality diagnostics) backend.
//!
//! Single source of truth for what the dashboard's Quality tab reads:
//! the `self_state.weights_*` BLOBs that optional quality modules write.
//! The numbers here are the same shape `soma inspect weights` returns —
//! the dashboard is the web mirror of that verb (ADR 0012 §A4).
//!
//! Output JSON shape (per request):
//!
//! ```json
//! {
//!   "context_envelope_disposition": { "module_class",
//!                                      "acceptance_rule",
//!                                      "metrics_boundary",
//!                                      "modules" },
//!   "mlstm":   { "dim", "train_steps", "saved_at_ns",
//!                "frobenius_delta": { "w_q", "w_k", "w_v" },
//!                "any_non_finite" } | null,
//!   "hopfield":{ ... },
//!   "anil":    { ... },
//!   "pc_layers":[{...}, ...]
//! }
//! ```
//!
//! Mock 0 / placebo 0 — every field is read live from SQLite. When
//! a row is absent (cold start before slow_loop ever wrote weights)
//! the field is `null` so the frontend can render an "unweighted"
//! state honestly instead of a fake zero curve.

use std::path::Path;

use serde_json::{json, Value};

use crate::storage::{Storage, StorageError};

/// Read the four cognitive weight tables and produce the same JSON
/// `cli::inspect::inspect_weights` produces. Reused by the dashboard
/// `/api/quality/weights` endpoint so a future change to the
/// inspect verb's semantics rolls into the dashboard automatically.
pub fn weights_snapshot(db_path: &Path) -> Result<Value, StorageError> {
    let store = Storage::open(db_path)?;
    Ok(weights_snapshot_with(&store))
}

/// Test-friendly variant that takes a borrowed `Storage`. The HTTP
/// handler uses the path-taking entry above; tests use this so
/// they can seed in-memory rows.
pub fn weights_snapshot_with(store: &Storage) -> Value {
    let frobenius_delta = |w: &[f32], dim: usize| -> f32 {
        let mut acc = 0.0_f32;
        for i in 0..dim {
            for j in 0..dim {
                let target = if i == j { 1.0 } else { 0.0 };
                let d = w[i * dim + j] - target;
                acc += d * d;
            }
        }
        acc.sqrt()
    };
    let frobenius_norm = |w: &[f32]| -> f32 { w.iter().map(|x| x * x).sum::<f32>().sqrt() };
    let any_non_finite_three = |a: &[f32], b: &[f32], c: &[f32]| -> bool {
        a.iter().chain(b.iter()).chain(c.iter()).any(|v| !v.is_finite())
    };

    let mlstm = match store.get_working_memory_weights().ok().flatten() {
        Some((dim, w_q, w_k, w_v, train_steps, saved_at_ns)) => json!({
            "dim": dim,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "frobenius_delta": {
                "w_q": frobenius_delta(&w_q, dim),
                "w_k": frobenius_delta(&w_k, dim),
                "w_v": frobenius_delta(&w_v, dim),
            },
            "any_non_finite": any_non_finite_three(&w_q, &w_k, &w_v),
        }),
        None => Value::Null,
    };

    let hopfield = match store.get_hopfield_weights().ok().flatten() {
        Some((dim, num_heads, w_q, w_k, w_v, train_steps, saved_at_ns)) => json!({
            "dim": dim,
            "num_heads": num_heads,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "frobenius_delta": {
                "w_q": frobenius_delta(&w_q, dim),
                "w_k": frobenius_delta(&w_k, dim),
                "w_v": frobenius_delta(&w_v, dim),
            },
            "any_non_finite": any_non_finite_three(&w_q, &w_k, &w_v),
        }),
        None => Value::Null,
    };

    let anil = match store.get_anil_head_weights().ok().flatten() {
        Some((d_emb, w, b, projects, train_steps, saved_at_ns)) => json!({
            "d_emb": d_emb,
            "num_classes": projects.len(),
            "projects": projects,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "w_norm": frobenius_norm(&w),
            "b_norm": frobenius_norm(&b),
            "any_non_finite": w.iter().chain(b.iter()).any(|v| !v.is_finite()),
        }),
        None => Value::Null,
    };

    let pc_layers: Vec<Value> = store
        .get_pc_predictor_layers()
        .unwrap_or_default()
        .into_iter()
        .map(|(layer_idx, d_in, d_out, w, train_steps, saved_at_ns)| {
            json!({
                "layer_idx": layer_idx,
                "d_in": d_in,
                "d_out": d_out,
                "train_steps": train_steps,
                "saved_at_ns": saved_at_ns,
                "w_norm": frobenius_norm(&w),
                "any_non_finite": w.iter().any(|v| !v.is_finite()),
            })
        })
        .collect();

    json!({
        "context_envelope_disposition": crate::context::disposition::context_quality_module_disposition(),
        "mlstm": mlstm,
        "hopfield": hopfield,
        "anil": anil,
        "pc_layers": pc_layers,
    })
}
