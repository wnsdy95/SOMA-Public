//! `soma diagnose` — single-shot JSON dump for support tickets.
//!
//! D129-cand close (R9 audit, 2026-04-30). Prints version + enabled
//! cargo features + resident liveness + DB stats + weight shapes +
//! ContextEnvelope disposition + cache counters + any sub-step failures
//! (`_errors` array). Always exits 0 — operators paste the JSON directly
//! into a bug report and we never fail-hard on a partial environment.

use serde_json::json;

/// Emit the diagnostic JSON to stdout. Returns `Ok(())` always.
pub fn run_blocking() -> std::io::Result<()> {
    let mut errors: Vec<String> = Vec::new();
    let features = enabled_features();

    let db_path = match crate::capture::ai_cli::resolve_db_path(None) {
        Ok(p) => Some(p),
        Err(e) => {
            errors.push(format!("resolve_db_path: {e}"));
            None
        }
    };

    let db_size_bytes = db_path.as_ref().and_then(|p| match std::fs::metadata(p) {
        Ok(m) => Some(m.len()),
        Err(e) => {
            errors.push(format!("db metadata: {e}"));
            None
        }
    });

    let (episode_count, weight_shapes) =
        match db_path.as_ref().map(|p| crate::storage::Storage::open(p)) {
            Some(Ok(store)) => {
                let count = store.counters().map(|(ep, _)| ep).unwrap_or_else(|e| {
                    errors.push(format!("counters: {e}"));
                    0
                });
                let shapes = collect_weight_shapes(&store, &mut errors);
                (Some(count), shapes)
            }
            Some(Err(e)) => {
                errors.push(format!("storage open: {e}"));
                (None, json!({}))
            }
            None => (None, json!({})),
        };

    let resident = collect_resident_status(&mut errors);

    let binary = crate::cli::binary_identity::collect_binary_identity_with_errors(&mut errors);

    let payload = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        "binary": binary,
        "features": features,
        "db_path": db_path.as_ref().map(|p| p.display().to_string()),
        "db_size_bytes": db_size_bytes,
        "episode_count": episode_count,
        "weight_shapes": weight_shapes,
        "context_envelope_disposition": crate::context::disposition::context_quality_module_disposition(),
        "resident": resident,
        "_errors": errors,
    });

    println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()));
    Ok(())
}

// `mut` may or may not be needed depending on which features
// are enabled at compile time; same for clippy's
// `vec_init_then_push` (triggers on all-features but not default).
#[allow(unused_mut, clippy::vec_init_then_push)]
fn enabled_features() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    #[cfg(feature = "cognitive")]
    v.push("cognitive");
    #[cfg(feature = "cognitive-train")]
    v.push("cognitive-train");
    #[cfg(feature = "embed-onnx")]
    v.push("embed-onnx");
    #[cfg(feature = "pty-capture")]
    v.push("pty-capture");
    #[cfg(feature = "llm-summary")]
    v.push("llm-summary");
    v
}

fn collect_weight_shapes(
    store: &crate::storage::Storage,
    errors: &mut Vec<String>,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    match store.get_working_memory_weights() {
        Ok(Some((dim, _, _, _, steps, _))) => {
            out.insert("mlstm".into(), json!({"dim": dim, "train_steps": steps}));
        }
        Ok(None) => {}
        Err(e) => errors.push(format!("get_working_memory_weights: {e}")),
    }
    match store.get_hopfield_weights() {
        Ok(Some((dim, num_heads, _, _, _, steps, _))) => {
            out.insert(
                "hopfield".into(),
                json!({"dim": dim, "num_heads": num_heads, "train_steps": steps}),
            );
        }
        Ok(None) => {}
        Err(e) => errors.push(format!("get_hopfield_weights: {e}")),
    }
    match store.get_anil_head_weights() {
        Ok(Some((d_emb, _, _, projects, steps, _))) => {
            out.insert(
                "anil".into(),
                json!({
                    "d_emb": d_emb,
                    "num_classes": projects.len(),
                    "train_steps": steps,
                }),
            );
        }
        Ok(None) => {}
        Err(e) => errors.push(format!("get_anil_head_weights: {e}")),
    }
    match store.get_pc_predictor_layers() {
        Ok(layers) => {
            let layer_meta: Vec<_> = layers
                .iter()
                .map(|(idx, d_in, d_out, _, steps, _)| {
                    json!({
                        "layer_idx": idx,
                        "d_in": d_in,
                        "d_out": d_out,
                        "train_steps": steps,
                    })
                })
                .collect();
            if !layer_meta.is_empty() {
                out.insert("pc_layers".into(), json!(layer_meta));
            }
        }
        Err(e) => errors.push(format!("get_pc_predictor_layers: {e}")),
    }
    serde_json::Value::Object(out)
}

#[cfg(unix)]
fn collect_resident_status(errors: &mut Vec<String>) -> serde_json::Value {
    use crate::cli::status;
    match status::resolve_socket_path() {
        Ok(socket) => {
            if !socket.exists() {
                return json!({ "state": "not_running", "socket": socket.display().to_string() });
            }
            // Reuse the status path's connect logic via a fresh tokio
            // current-thread runtime. Failures append to `_errors`
            // and the resident block returns "error".
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    errors.push(format!("tokio build: {e}"));
                    return json!({"state": "error", "socket": socket.display().to_string()});
                }
            };
            match runtime.block_on(status::send_status_for_diagnose(&socket)) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!("resident probe: {e}"));
                    json!({"state": "error", "socket": socket.display().to_string()})
                }
            }
        }
        Err(e) => {
            errors.push(format!("resolve_socket_path: {e}"));
            json!({"state": "error"})
        }
    }
}

// Hot-fix (2026-05-01) — Windows path takes the same param shape as
// the unix variant for caller symmetry, but never pushes (no resident
// to probe on Windows). `clippy::pedantic::ptr_arg` rightly flags
// `&mut Vec<String>` over `&mut [String]`; we keep the type for the
// cross-platform call-site and silence the platform-specific lint.
#[cfg(not(unix))]
#[allow(clippy::ptr_arg)]
fn collect_resident_status(_errors: &mut Vec<String>) -> serde_json::Value {
    json!({ "state": "not_unix" })
}
