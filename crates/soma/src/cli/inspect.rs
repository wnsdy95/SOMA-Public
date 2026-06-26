//! `soma inspect` — read-side admin surface.
//!
//! Pre-D86 the verb was an exit-7 stub (D92 P2 fix). This module
//! wires the existing Storage read API into a CLI surface so an
//! operator can:
//!
//!   * `soma inspect episode --id N` — single episode + metadata
//!   * `soma inspect vector --id N` — episode_vectors row dim/preview
//!   * `soma inspect pin --id N` — note_pins entry
//!   * `soma inspect edges --id N` — episode_edges neighborhood
//!   * `soma inspect narrative` — legacy context/profile diagnostic paragraph
//!   * `soma inspect centroid` — legacy context/profile centroid diagnostic
//!
//! Output format: `json` (default, structured) or `markdown` (human).

use std::path::PathBuf;

use crate::cli::InspectArgs;
use crate::storage::{Storage, StorageError};

#[derive(Debug, Clone)]
pub struct InspectContext {
    pub db_path: PathBuf,
}

#[derive(Debug)]
pub enum InspectError {
    Path(String),
    Storage(StorageError),
    BadInput(String),
    NotFound(String),
}

impl std::fmt::Display for InspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InspectError::Path(m) => write!(f, "path: {m}"),
            InspectError::Storage(e) => write!(f, "storage: {e}"),
            InspectError::BadInput(m) => write!(f, "bad input: {m}"),
            InspectError::NotFound(m) => write!(f, "not found: {m}"),
        }
    }
}

impl std::error::Error for InspectError {}

impl From<StorageError> for InspectError {
    fn from(e: StorageError) -> Self {
        InspectError::Storage(e)
    }
}

/// Run one `soma inspect` invocation. Returns the rendered text.
pub fn run_inspect(args: &InspectArgs, ctx: &InspectContext) -> Result<String, InspectError> {
    let store = Storage::open(&ctx.db_path)?;
    let value = match args.kind.as_str() {
        "episode" => inspect_episode(&store, expect_id(args)?, args.include_forgotten)?,
        "vector" => inspect_vector(&store, expect_id(args)?)?,
        "pin" => inspect_pin(&store, expect_id(args)?)?,
        "edges" => inspect_edges(&store, expect_id(args)?)?,
        "narrative" => inspect_narrative(&store)?,
        "centroid" => inspect_centroid(&store)?,
        "weights" => inspect_weights(&store)?,
        other => {
            return Err(InspectError::BadInput(format!(
                "unknown kind `{other}` — accepted: episode/vector/pin/edges/weights; legacy diagnostics: narrative/centroid"
            )))
        }
    };
    let rendered = match args.format.as_str() {
        "json" => serde_json::to_string_pretty(&value)
            .map_err(|e| InspectError::Storage(StorageError::Corrupt { detail: e.to_string() }))?,
        "markdown" => render_markdown(&args.kind, &value),
        other => {
            return Err(InspectError::BadInput(format!(
                "unknown --format `{other}` — accepted: json / markdown"
            )))
        }
    };
    Ok(rendered)
}

fn expect_id(args: &InspectArgs) -> Result<i64, InspectError> {
    args.id.ok_or_else(|| InspectError::BadInput("--id is required for this kind".into()))
}

fn inspect_episode(
    store: &Storage,
    id: i64,
    include_forgotten: bool,
) -> Result<serde_json::Value, InspectError> {
    // `get_episode` doesn't surface forgotten/access/summary fields
    // — those columns post-date the StoredEpisode struct. We pull
    // them via the helper APIs and merge into the JSON shape so
    // operators see one unified view.
    let ep =
        store.get_episode(id)?.ok_or_else(|| InspectError::NotFound(format!("episode #{id}")))?;
    let access = store.access_metadata(id)?;
    let summary = store.summary_metadata(id)?;
    let forgotten = store.forgotten_status(id)?;
    if !include_forgotten && forgotten.is_some() {
        return Err(InspectError::NotFound(format!(
            "episode #{id} is forgotten (use --include-forgotten to surface)"
        )));
    }
    Ok(serde_json::json!({
        "id": ep.id,
        "ts_start_ns": ep.ts_start_ns,
        "ts_end_ns": ep.ts_end_ns,
        "duration_ms": ep.duration_ms,
        "source": ep.source,
        "session_id": ep.session_id,
        "prompt_text": ep.prompt_text,
        "response_text": ep.response_text,
        "command": ep.command,
        "exit_code": ep.exit_code,
        "cwd": ep.cwd,
        "git_branch": ep.git_branch,
        "project": ep.project,
        "digest": ep.digest,
        "memory_tier": ep.memory_tier,
        "salience": ep.salience,
        "access_count": access.map(|a| a.0),
        "last_access_ns": access.map(|a| a.1),
        "summary_count": summary.as_ref().map(|s| s.0),
        "summary_signature": summary.as_ref().and_then(|s| s.1.clone()),
        "forgotten_at_ns": forgotten.as_ref().map(|f| f.0),
        "forgotten_reason": forgotten.as_ref().and_then(|f| f.1.clone()),
    }))
}

fn inspect_vector(store: &Storage, id: i64) -> Result<serde_json::Value, InspectError> {
    let model_id = crate::memory::embed::select_embedder().model_id();
    let rows = store.vectors_for_model(model_id)?;
    let (_, vec) = rows
        .into_iter()
        .find(|(eid, _)| *eid == id)
        .ok_or_else(|| InspectError::NotFound(format!("vector for episode #{id}")))?;
    let preview: Vec<f32> = vec.iter().take(8).copied().collect();
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    Ok(serde_json::json!({
        "episode_id": id,
        "model_id": model_id,
        "dim": vec.len(),
        "norm": norm,
        "preview_first_8": preview,
    }))
}

fn inspect_pin(store: &Storage, id: i64) -> Result<serde_json::Value, InspectError> {
    let pinned = store.is_pinned(id)?;
    let all = store.pinned_episode_ids()?;
    if !pinned {
        return Ok(serde_json::json!({
            "episode_id": id,
            "pinned": false,
        }));
    }
    Ok(serde_json::json!({
        "episode_id": id,
        "pinned": true,
        "all_pinned": all,
    }))
}

fn inspect_edges(store: &Storage, id: i64) -> Result<serde_json::Value, InspectError> {
    let neighbors = store.edges_for(id, 32)?;
    let edges: Vec<serde_json::Value> = neighbors
        .into_iter()
        .map(|(other, sim)| serde_json::json!({ "episode_id": other, "similarity": sim }))
        .collect();
    Ok(serde_json::json!({
        "episode_id": id,
        "edge_count": edges.len(),
        "edges": edges,
    }))
}

fn inspect_narrative(store: &Storage) -> Result<serde_json::Value, InspectError> {
    let row = store.get_narrative()?;
    Ok(match row {
        Some((p, ts, kind)) => serde_json::json!({
            "paragraph_md": p,
            "synthesized_at_ns": ts,
            "kind": kind,
        }),
        None => serde_json::json!({ "paragraph_md": "", "synthesized_at_ns": 0, "kind": "rule" }),
    })
}

fn inspect_centroid(store: &Storage) -> Result<serde_json::Value, InspectError> {
    let row = store.get_user_centroid()?;
    Ok(match row {
        Some((centroid, count)) => {
            let preview: Vec<f32> = centroid.iter().take(8).copied().collect();
            serde_json::json!({
                "episode_count": count,
                "dim": centroid.len(),
                "preview_first_8": preview,
            })
        }
        None => serde_json::json!({ "episode_count": 0, "dim": 0, "primed": false }),
    })
}

/// `soma inspect weights`. Surfaces optional quality-module weight rows
/// (mLSTM working memory / Hopfield Q/K/V / ANIL classifier head /
/// iPC predictor layers) so the operator can inspect diagnostic drift
/// in one command.
///
/// ADR 0015 boundary: this command is diagnostic only. Weight drift,
/// train_steps, and layer norms do not prove ContextEnvelope quality
/// unless a retained module changes ranking, scoping, compression,
/// conflict detection, or evidence selection.
///
/// D100-cand close (2026-04-29) — pre-fix only the mLSTM row was
/// surfaced; DOGFOODING-LOG had a `hopfield Δ` column with no source
/// (operator had to direct-SQL probe). Each kind contributes:
///
/// * `mlstm` — `Some` when chunk 1.3 persisted at least once.
///   Frobenius `Δ_{q,k,v}` from identity (chunk 1.1 init).
/// * `hopfield` — `Some` when chunk 4.3 persisted at least once.
///   Frobenius `Δ_{q,k,v}` from identity (chunk 4.1 init).
/// * `anil` — `Some` when chunk 2.3 persisted. `W_head` + bias
///   are NOT identity-initialized (random small init), so we
///   surface plain Frobenius norm + projects[] for the class
///   labels.
/// * `pc_layers` — array, one entry per layer (chunk 3.3). Each
///   layer's W is `(d_in, d_out)` non-square, so plain norm.
///
/// Each kind also surfaces `any_non_finite` (3-layer NaN guard
/// invariant) so a corruption mid-training is visible.
///
/// Back-compat note — the historical N3 shape (top-level `dim`,
/// `train_steps`, `frobenius_delta_from_identity`) was renamed to
/// the `mlstm` sub-object. The chunk-1.3 persistence test was
/// updated in lockstep.
fn inspect_weights(store: &Storage) -> Result<serde_json::Value, InspectError> {
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
    let any_nan_three = |a: &[f32], b: &[f32], c: &[f32]| -> bool {
        a.iter().chain(b.iter()).chain(c.iter()).any(|v| !v.is_finite())
    };

    // D101-cand close (2026-04-29) — mirror the runtime's
    // `load_trained_projections` decision so `recall_active` answers
    // "if I run `soma recall` now, will the trained projections be
    // applied?". Embedder dim must match the persisted row dim, all
    // values must be finite, and the row must exist. Same logic
    // appears in `memory/cognitive/hopfield_backend.rs::load_trained_
    // projections` and `runtime/scheduler/slow_loop.rs::compute_
    // working_memory_read`.
    let primary_dim = crate::memory::embed::select_embedder().dim();
    let derive_recall_active =
        |row_dim: usize, w_q: &[f32], w_k: &[f32], w_v: &[f32]| -> &'static str {
            if row_dim != primary_dim {
                return "frozen (dim mismatch with embedder)";
            }
            let expected = row_dim * row_dim;
            if w_q.len() != expected || w_k.len() != expected || w_v.len() != expected {
                return "frozen (shape mismatch)";
            }
            if any_nan_three(w_q, w_k, w_v) {
                return "frozen (corrupt: NaN/Inf)";
            }
            "trained"
        };

    let mlstm = match store.get_working_memory_weights()? {
        Some((dim, w_q, w_k, w_v, train_steps, saved_at_ns)) => serde_json::json!({
            "dim": dim,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "frobenius_delta_from_identity": {
                "w_q": frobenius_delta(&w_q, dim),
                "w_k": frobenius_delta(&w_k, dim),
                "w_v": frobenius_delta(&w_v, dim),
            },
            "any_non_finite": any_nan_three(&w_q, &w_k, &w_v),
            "recall_active": derive_recall_active(dim, &w_q, &w_k, &w_v),
        }),
        None => serde_json::Value::Null,
    };

    let hopfield = match store.get_hopfield_weights()? {
        Some((dim, num_heads, w_q, w_k, w_v, train_steps, saved_at_ns)) => serde_json::json!({
            "dim": dim,
            "num_heads": num_heads,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "frobenius_delta_from_identity": {
                "w_q": frobenius_delta(&w_q, dim),
                "w_k": frobenius_delta(&w_k, dim),
                "w_v": frobenius_delta(&w_v, dim),
            },
            "any_non_finite": any_nan_three(&w_q, &w_k, &w_v),
            "recall_active": derive_recall_active(dim, &w_q, &w_k, &w_v),
        }),
        None => serde_json::Value::Null,
    };

    let anil = match store.get_anil_head_weights()? {
        Some((d_emb, w, b, projects, train_steps, saved_at_ns)) => serde_json::json!({
            "d_emb": d_emb,
            "num_classes": projects.len(),
            "projects": projects,
            "train_steps": train_steps,
            "saved_at_ns": saved_at_ns,
            "w_norm": frobenius_norm(&w),
            "b_norm": frobenius_norm(&b),
            "any_non_finite": w.iter().chain(b.iter()).any(|v| !v.is_finite()),
        }),
        None => serde_json::Value::Null,
    };

    let pc_layers: Vec<serde_json::Value> = store
        .get_pc_predictor_layers()?
        .into_iter()
        .map(|(layer_idx, d_in, d_out, w, train_steps, saved_at_ns)| {
            serde_json::json!({
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

    Ok(serde_json::json!({
        "context_envelope_disposition": crate::context::disposition::context_quality_module_disposition(),
        "mlstm": mlstm,
        "hopfield": hopfield,
        "anil": anil,
        "pc_layers": pc_layers,
    }))
}

fn render_markdown(kind: &str, v: &serde_json::Value) -> String {
    let mut out = format!("# Inspect: {kind}\n\n");
    out.push_str("```json\n");
    out.push_str(&serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string()));
    out.push_str("\n```\n");
    out
}

pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, InspectError> {
    if let Some(p) = cli_override {
        return Ok(PathBuf::from(p));
    }
    if let Ok(env) = std::env::var("SOMA_DB") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let home =
        dirs::home_dir().ok_or_else(|| InspectError::Path("home dir not resolvable".into()))?;
    Ok(home.join(".soma").join("soma.db"))
}

pub fn exit_code_for(e: &InspectError) -> i32 {
    match e {
        InspectError::BadInput(_) => 1,
        InspectError::Storage(_) => 2,
        InspectError::Path(_) => 3,
        InspectError::NotFound(_) => 4,
    }
}
