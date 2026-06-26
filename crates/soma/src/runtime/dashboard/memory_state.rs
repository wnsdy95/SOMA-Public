//! D152 chunk 1.4 — View 3 (memory state snapshot) backend.
//!
//! Shape `GET /api/memory/state` returns:
//!
//! ```json
//! {
//!   "totals": { "episodes": u64, "vectors": u64 },
//!   "by_source":  [{ "key", "count" }, ...],
//!   "by_project": [{ "key", "count" }, ...],
//!   "beliefs":    { "contradictions_recent": [{...}, ...] },
//!   "context_profile": { "card_preview": String|null,
//!                        "identity_preview": String|null },
//!   "persona":    { ... } // legacy alias for dashboard clients
//! }
//! ```
//!
//! Mock 0 / placebo 0. Aggregates over the last 500 episodes
//! client-side rather than adding a SQL `GROUP BY` — the table is
//! small (≤ ~10 K rows for v1.x dogfooding) so the in-memory
//! aggregate is faster than maintaining a third index.

use std::path::Path;

use serde_json::{json, Value};

use crate::storage::{Storage, StorageError};

const RECENT_WINDOW: usize = 500;
const RECENT_CONTRADICTIONS: usize = 8;
/// D168 follow-up — 이전 600 char cap 폐지. dashboard panel 의
/// max-height 22rem + overflow auto 가 콘텐츠 가시화 처리. file
/// 자체 가 1-2 KB 수준 이라 bandwidth 부담 없음. user redirect:
/// "문서가 이렇게 짤리는 이유는?".
const PREVIEW_CAP_BYTES: usize = 32 * 1024;

pub fn memory_state_snapshot(db_path: &Path) -> Result<Value, StorageError> {
    let store = Storage::open(db_path)?;
    Ok(memory_state_snapshot_with(&store))
}

/// D164 close — last-30-day note-pin timeline for the dashboard's
/// View 3. Returns `{ "days": [{ "day_ts": i64, "count": u64 }, ...] }`
/// oldest-first. Cold DB → empty array.
pub fn note_pin_timeline(db_path: &Path) -> Result<Value, StorageError> {
    let store = Storage::open(db_path)?;
    Ok(note_pin_timeline_with(&store, 30))
}

pub fn note_pin_timeline_with(store: &Storage, days: u32) -> Value {
    let rows = store.note_pin_timeline_days(days).unwrap_or_default();
    let arr: Vec<Value> = rows
        .into_iter()
        .map(|(day_ts, count)| json!({ "day_ts": day_ts, "count": count }))
        .collect();
    json!({ "days": arr, "window_days": days })
}

pub fn memory_state_snapshot_with(store: &Storage) -> Value {
    let totals = match store.counters() {
        Ok((ep, vec)) => json!({ "episodes": ep, "vectors": vec }),
        Err(_) => json!({ "episodes": 0, "vectors": 0 }),
    };

    let recent = store.recent_episodes(RECENT_WINDOW).unwrap_or_default();
    let mut by_source: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut by_project: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for ep in &recent {
        *by_source.entry(ep.source.to_string()).or_insert(0) += 1;
        if let Some(p) = ep.project.as_deref() {
            *by_project.entry(p.to_string()).or_insert(0) += 1;
        }
    }
    let by_source = sorted_pairs(by_source);
    let by_project = sorted_pairs(by_project);

    let contradictions: Vec<Value> = store
        .recent_contradictions(RECENT_CONTRADICTIONS)
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            json!({
                "episode_a_id": c.episode_a_id,
                "episode_b_id": c.episode_b_id,
                "score": c.score,
                "evidence": c.evidence,
                "created_at_ns": c.created_at_ns,
            })
        })
        .collect();
    // D164.5 close — corroborates list 도 읽어서 belief graph 가
    // 두 kind 모두 cover. dashboard 의 belief panel 이 chronological
    // column 으로 layout.
    let corroborations: Vec<Value> = store
        .recent_beliefs_of_kind(
            crate::memory::beliefs::BeliefKind::Corroborates,
            RECENT_CONTRADICTIONS,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            json!({
                "episode_a_id": c.episode_a_id,
                "episode_b_id": c.episode_b_id,
                "score": c.score,
                "evidence": c.evidence,
                "created_at_ns": c.created_at_ns,
            })
        })
        .collect();

    let context_profile = context_profile_preview();

    json!({
        "totals": totals,
        "by_source": by_source,
        "by_project": by_project,
        "beliefs": {
            "contradictions_recent": contradictions,
            "corroborations_recent": corroborations,
        },
        "context_profile": context_profile.clone(),
        "persona": context_profile,
        "window": RECENT_WINDOW,
    })
}

fn sorted_pairs(map: std::collections::HashMap<String, u64>) -> Vec<Value> {
    let mut v: Vec<(String, u64)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.into_iter().map(|(k, c)| json!({ "key": k, "count": c })).collect()
}

fn context_profile_preview() -> Value {
    let card = read_capped(crate::memory::persona::persona_card_path().as_deref());
    let identity = read_capped(crate::memory::persona::identity_path().as_deref());
    // Legacy context/profile helper token cost 표시. 정확한 tokenization
    // 은 model-specific (Claude tokenizer 별, ollama sentencepiece
    // 별) 이라 char count + 추정 (한국어 prose ~2 char/token, 영문
    // ~4 char/token, 보수적 으로 3 평균) 으로 approximation.
    // 한 turn cost 의 order-of-magnitude 가시화.
    let card_chars = card.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let identity_chars = identity.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let est_tokens = |chars: usize| -> usize { chars.div_ceil(3) };
    json!({
        "card_preview": card,
        "identity_preview": identity,
        "card_chars": card_chars,
        "identity_chars": identity_chars,
        "card_est_tokens": est_tokens(card_chars),
        "identity_est_tokens": est_tokens(identity_chars),
    })
}

fn read_capped(path: Option<&std::path::Path>) -> Option<String> {
    let p = path?;
    let body = std::fs::read_to_string(p).ok()?;
    // PREVIEW_CAP_BYTES 는 bandwidth 안전 net (만일 file 이
    // 비정상적으로 커도 32KB 까지). 정상 dogfooding 에서는
    // profile card / identity 모두 1-2 KB 라 cap 도달 0.
    if body.len() > PREVIEW_CAP_BYTES {
        let truncated: String = body.chars().take(PREVIEW_CAP_BYTES / 4).collect();
        Some(truncated + "\n\n… (truncated — file > 32 KiB)")
    } else {
        Some(body)
    }
}
