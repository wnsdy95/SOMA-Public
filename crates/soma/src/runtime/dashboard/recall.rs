//! D152 chunk 1.3 — View 2 (debug recall traces) backend.
//!
//! Reads the legacy-named `chat_recall_trace` table written by local debug
//! paths such as `soma recall` and historical local REPL rows. The dashboard polls
//! `/api/recall/recent` so the operator can inspect local retrieval behavior.
//! Cloud LLM clients use MCP ContextEnvelope resources/tools instead of this
//! diagnostic table.
//!
//! Output JSON (per request):
//!
//! ```json
//! {
//!   "traces": [
//!     { "id", "created_at_ns", "session_id", "project",
//!       "query_text", "pack_count", "duration_ms",
//!       "response_chars", "response_text",
//!       "top_k": [{ "episode_id", "raw_sim" }, ...] },
//!     ...
//!   ]
//! }
//! ```
//!
//! Mock 0 — every field reads from SQLite. Empty DB returns
//! `{ "traces": [] }`.

use std::path::Path;

use serde_json::{json, Value};

use crate::storage::{Storage, StorageError};

const DEFAULT_LIMIT: usize = 10;

pub fn recent_recall_snapshot(db_path: &Path) -> Result<Value, StorageError> {
    let store = Storage::open(db_path)?;
    Ok(recent_recall_snapshot_with(&store, DEFAULT_LIMIT))
}

pub fn recent_recall_snapshot_with(store: &Storage, limit: usize) -> Value {
    let traces = store.recent_chat_recall_traces(limit).unwrap_or_default();
    let arr: Vec<Value> = traces
        .into_iter()
        .map(|t| {
            // top_k_json is already a JSON array of
            // {"episode_id":i64,"raw_sim":f32}; pass it through as
            // structured Value so the API consumer doesn't have to
            // parse the string twice.
            let top_k: Value = serde_json::from_str(&t.top_k_json).unwrap_or_else(|_| json!([]));
            json!({
                "id": t.id,
                "created_at_ns": t.created_at_ns,
                "session_id": t.session_id,
                "project": t.project,
                "query_text": t.query_text,
                "pack_count": t.pack_count,
                "duration_ms": t.duration_ms,
                "response_chars": t.response_chars,
                "response_text": t.response_text,
                "top_k": top_k,
            })
        })
        .collect();
    json!({ "traces": arr })
}
