//! tool_use extractor — counts command head-tokens across terminal
//! episodes. Discussion 0030 §I T.12a.
//!
//! Emits one fact with key `top_n`: JSON
//! `{"commands":[{"cmd":..., "count":...}, ...], "total":...}`
//! sorted by count DESC, top 10.

use crate::self_model::{SelfExtractor, SelfFact};
use crate::storage::{EpisodeId, EpisodeSource, Storage, StorageError};

pub struct ToolUseExtractor;

impl SelfExtractor for ToolUseExtractor {
    fn kind(&self) -> &'static str {
        "tool_use"
    }

    fn extract(&self, storage: &Storage) -> Result<Vec<SelfFact>, StorageError> {
        let episodes = storage.all_episodes()?;
        let mut counts: std::collections::HashMap<String, (u64, Vec<EpisodeId>)> =
            std::collections::HashMap::new();
        let mut total = 0u64;

        for ep in &episodes {
            // D119 — typed enum compare, no string allocation.
            if ep.source != EpisodeSource::Terminal {
                continue;
            }
            let Some(cmd) = ep.command.as_deref() else {
                continue;
            };
            let head = first_token(cmd);
            if head.is_empty() {
                continue;
            }
            let entry = counts.entry(head.to_string()).or_default();
            entry.0 += 1;
            entry.1.push(ep.id);
            total += 1;
        }

        let mut sorted: Vec<(String, u64, Vec<EpisodeId>)> =
            counts.into_iter().map(|(k, (c, e))| (k, c, e)).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sorted.truncate(10);

        let mut evidence_all = Vec::new();
        let mut commands = Vec::with_capacity(sorted.len());
        for (cmd, count, evidence) in &sorted {
            commands.push(serde_json::json!({"cmd": cmd, "count": count}));
            evidence_all.extend(evidence.iter().copied());
        }

        Ok(vec![SelfFact {
            key: "top_n".into(),
            value: serde_json::json!({"commands": commands, "total": total}),
            evidence_ids: evidence_all,
        }])
    }
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}
