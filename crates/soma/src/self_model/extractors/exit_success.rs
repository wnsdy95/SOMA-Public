//! exit_success extractor — group terminal episodes by head-token
//! and compute success rate. Discussion 0030 §I T.12b.
//!
//! One fact per distinct head-token with non-null `exit_code`:
//! key = head token, value = `{"success":<n>, "fail":<n>,
//! "total":<n>, "rate":<f32 in [0,1]>}`, evidence_ids = episodes
//! that contributed.

use crate::self_model::{SelfExtractor, SelfFact};
use crate::storage::{EpisodeId, EpisodeSource, Storage, StorageError};

pub struct ExitSuccessExtractor;

impl SelfExtractor for ExitSuccessExtractor {
    fn kind(&self) -> &'static str {
        "exit_success"
    }

    fn extract(&self, storage: &Storage) -> Result<Vec<SelfFact>, StorageError> {
        let episodes = storage.all_episodes()?;
        let mut groups: std::collections::HashMap<String, (u64, u64, Vec<EpisodeId>)> =
            std::collections::HashMap::new();

        for ep in &episodes {
            // D119 — typed enum compare, no string allocation.
            if ep.source != EpisodeSource::Terminal {
                continue;
            }
            let Some(cmd) = ep.command.as_deref() else {
                continue;
            };
            let Some(exit) = ep.exit_code else {
                continue;
            };
            let head = first_token(cmd);
            if head.is_empty() {
                continue;
            }
            let entry = groups.entry(head.to_string()).or_default();
            if exit == 0 {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
            entry.2.push(ep.id);
        }

        let mut out = Vec::with_capacity(groups.len());
        for (head, (success, fail, evidence)) in groups {
            let total = success + fail;
            let rate = if total == 0 { 0.0 } else { success as f32 / total as f32 };
            out.push(SelfFact {
                key: head,
                value: serde_json::json!({
                    "success": success,
                    "fail": fail,
                    "total": total,
                    "rate": rate,
                }),
                evidence_ids: evidence,
            });
        }
        // Stable order — keyed alphabetically so tests + `soma
        // profile` render don't flake.
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}
