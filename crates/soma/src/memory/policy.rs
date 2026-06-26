//! Interpretable user-policy extractor.
//!
//! This module stores evidence-backed policy rows for
//! `ContextEnvelope.user_policy`. The default extractor is deterministic and
//! uses only local episode statistics. The durable product contract is the
//! cited `Policy` row, not any cloud API that could draft one.
//!
//! ## Architecture
//!
//! input:
//! * latest 200 episode preview (project-narrowed when scope set)
//! * top-N command tally (terminal episodes)
//! * exit_code distribution (success rate)
//! * project tag distribution
//!
//! output:
//! * `Policy { rule, evidence_episode_ids, confidence }` of N rules
//!   (typical 5-10).
//! * ContextEnvelope.user_policy rows.
//! * markdown render for hidden legacy CLAUDE.md migration/debug helpers.
//!
//! ## Invariants
//!
//! * **evidence chain**: 모든 rule 의 evidence_episode_ids 가
//!   storage 에 존재 하는 live episode id. forgotten episode 는
//!   evidence 자격 없음.
//! * **confidence cap**: HEURISTIC_CONFIDENCE_CAP (default 0.95) —
//!   LLM 이 1.0 반환 해도 cap. nat-lang rule 은 절대 deterministic
//!   아님 의 invariant 명시.
//! * **graceful fallback**: storage read failure or no strong deterministic
//!   signal returns `None`; callers keep the previous policy rows.
//!
//! ## Out of scope
//!
//! * `/correct <rule>` slash command + auto-detect.
//! * `belief_candidates` graph 의 contradicts edge ≥ corroborates
//!   기준 confidence 하락.
//! * `<project>/CLAUDE.md` auto-write.

use serde::{Deserialize, Serialize};

use crate::storage::{EpisodeId, Storage, StorageError};

pub const SELF_STATE_KIND: &str = "policy";

/// One extracted policy row. `confidence` is the LLM's self-
/// reported certainty, capped at `HEURISTIC_CONFIDENCE_CAP`.
/// `evidence_episode_ids` is the cohort the rule was derived from
/// — every id MUST resolve via `Storage::get_live_episode` for
/// the round-trip trace invariant to hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub rule: String,
    pub evidence_episode_ids: Vec<EpisodeId>,
    pub confidence: f32,
}

/// `confidence` cap. LLM occasionally returns `1.0` for rules with
/// strong-looking evidence; nat-lang rules are never *truly*
/// deterministic so we floor that cap. operator-visible reminder.
pub const HEURISTIC_CONFIDENCE_CAP: f32 = 0.95;

/// `evidence_episode_ids` per rule cap. The round-trip trace UI
/// needs a tight number to render and audit quickly.
pub const EVIDENCE_CAP_PER_RULE: usize = 5;

/// `extract_policies` output. `Some(rules)` on deterministic local signal;
/// `None` when there are no episodes, storage fails, or the recent window
/// lacks a strong enough repeated pattern. Caller (slow_loop / dashboard)
/// treats `None` as advisory and preserves prior rows.
pub fn extract_policies(storage: &Storage, project: Option<&str>) -> Option<Vec<Policy>> {
    let stats = match build_stats(storage, project) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "extract_policies: stats build failed");
            return None;
        }
    };
    if stats.episode_count == 0 {
        tracing::debug!("extract_policies: no episodes — skipping");
        return None;
    }

    let mut rules = deterministic_policies(&stats);
    normalize_rules(storage, &mut rules);
    if rules.is_empty() {
        tracing::debug!("extract_policies: no deterministic policy signal");
        return None;
    }
    Some(rules)
}

/// Render a Vec<Policy> to markdown. Each rule on its own line +
/// evidence ids + confidence percentage. Hidden legacy CLAUDE.md
/// migration/debug helpers render this from self_state on demand.
pub fn render_markdown(rules: &[Policy], project: Option<&str>) -> String {
    let mut out = String::new();
    match project {
        Some(p) => out.push_str(&format!("# SOMA policy — {p}\n\n")),
        None => out.push_str("# SOMA policy — global\n\n"),
    }
    if rules.is_empty() {
        out.push_str("_(no rules extracted yet — episode count or confidence too low)_\n");
        return out;
    }
    for (i, r) in rules.iter().enumerate() {
        out.push_str(&format!("{idx}. {rule}\n", idx = i + 1, rule = r.rule));
        let evidence: Vec<String> =
            r.evidence_episode_ids.iter().map(|id| id.to_string()).collect();
        out.push_str(&format!(
            "   - confidence: {pct:.0}% · evidence: episode {ids}\n",
            pct = r.confidence * 100.0,
            ids = evidence.join(", "),
        ));
    }
    out
}

pub fn upsert_policy_set(
    storage: &mut Storage,
    project: Option<&str>,
    rules: &[Policy],
) -> Result<(), StorageError> {
    let value = serde_json::json!({
        "project": project,
        "rules": rules,
    });
    let evidence = unique_evidence_ids(rules);
    storage.upsert_self_fact(SELF_STATE_KIND, &policy_key(project), &value.to_string(), &evidence)
}

pub fn read_policy_set(
    storage: &Storage,
    project: Option<&str>,
) -> Result<Vec<Policy>, StorageError> {
    let key = policy_key(project);
    for row in storage.read_all_self_facts()? {
        if row.kind != SELF_STATE_KIND || row.key != key {
            continue;
        }
        let value: StoredPolicySet = serde_json::from_str(&row.value_json).map_err(|e| {
            StorageError::Corrupt { detail: format!("policy self_state JSON: {e}") }
        })?;
        return Ok(value.rules);
    }
    Ok(Vec::new())
}

fn policy_key(project: Option<&str>) -> String {
    match project {
        Some(project) => format!("project:{project}"),
        None => "global".to_string(),
    }
}

fn unique_evidence_ids(rules: &[Policy]) -> Vec<EpisodeId> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for rule in rules {
        for id in &rule.evidence_episode_ids {
            if seen.insert(*id) {
                out.push(*id);
            }
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct StoredPolicySet {
    rules: Vec<Policy>,
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Stats {
    episode_count: usize,
    project: Option<String>,
    /// `(prompt_or_command_preview, episode_id)` pairs for deterministic
    /// project-scope evidence.
    samples: Vec<(String, EpisodeId)>,
    /// Top-N command tally (terminal episodes).
    top_commands: Vec<CommandTally>,
    /// 0..=command_total terminal exit-code 0 count.
    success_count: usize,
    /// All terminal episode count.
    command_total: usize,
    /// Terminal episodes with a non-zero exit status.
    failed_commands: Vec<EpisodeId>,
}

#[derive(Debug, Clone)]
struct CommandTally {
    command: String,
    count: usize,
    evidence_episode_ids: Vec<EpisodeId>,
}

fn build_stats(storage: &Storage, project: Option<&str>) -> Result<Stats, StorageError> {
    let recent = storage.recent_episodes(200)?;
    let filtered: Vec<_> = match project {
        Some(p) => recent.into_iter().filter(|ep| ep.project.as_deref() == Some(p)).collect(),
        None => recent,
    };

    let mut samples = Vec::with_capacity(filtered.len());
    let mut cmd_counts: std::collections::HashMap<String, CommandTally> =
        std::collections::HashMap::new();
    let mut success_count = 0_usize;
    let mut command_total = 0_usize;
    let mut failed_commands = Vec::new();
    for ep in &filtered {
        let head = ep
            .prompt_text
            .as_deref()
            .or(ep.command.as_deref())
            .or(ep.response_text.as_deref())
            .unwrap_or("");
        if !head.is_empty() {
            let preview: String = head.chars().take(120).collect();
            samples.push((preview, ep.id));
        }
        if let Some(cmd) = ep.command.as_deref() {
            command_total += 1;
            if matches!(ep.exit_code, Some(0)) {
                success_count += 1;
            } else {
                failed_commands.push(ep.id);
            }
            // First word as the command name.
            if let Some(first) = cmd.split_whitespace().next() {
                let entry = cmd_counts.entry(first.to_string()).or_insert_with(|| CommandTally {
                    command: first.to_string(),
                    count: 0,
                    evidence_episode_ids: Vec::new(),
                });
                entry.count += 1;
                if entry.evidence_episode_ids.len() < EVIDENCE_CAP_PER_RULE {
                    entry.evidence_episode_ids.push(ep.id);
                }
            }
        }
    }
    let mut top_commands: Vec<CommandTally> = cmd_counts.into_values().collect();
    top_commands.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.command.cmp(&b.command)));
    top_commands.truncate(10);

    Ok(Stats {
        episode_count: filtered.len(),
        project: project.map(|s| s.to_string()),
        samples,
        top_commands,
        success_count,
        command_total,
        failed_commands,
    })
}

fn deterministic_policies(stats: &Stats) -> Vec<Policy> {
    let mut rules = Vec::new();

    if let Some(top) = stats.top_commands.first() {
        if top.count >= 2 && !top.evidence_episode_ids.is_empty() {
            let scope = stats
                .project
                .as_deref()
                .map(|project| format!("Project `{project}` recent terminal work"))
                .unwrap_or_else(|| "Recent terminal work".to_string());
            rules.push(Policy {
                rule: format!(
                    "{scope} repeatedly uses `{}`; keep `{}` command context and diagnostics in scope.",
                    top.command, top.command
                ),
                evidence_episode_ids: top.evidence_episode_ids.clone(),
                confidence: confidence_from_repetition(top.count),
            });
        }
    }

    let failure_count = stats.command_total.saturating_sub(stats.success_count);
    if stats.command_total >= 3 && failure_count >= 2 && !stats.failed_commands.is_empty() {
        rules.push(Policy {
            rule: "Recent terminal work includes repeated non-zero exits; surface failure output and follow-up fixes before assuming success.".to_string(),
            evidence_episode_ids: stats
                .failed_commands
                .iter()
                .take(EVIDENCE_CAP_PER_RULE)
                .copied()
                .collect(),
            confidence: 0.72,
        });
    }

    if let Some(project) = stats.project.as_deref() {
        if stats.episode_count >= 3 && !stats.samples.is_empty() {
            rules.push(Policy {
                rule: format!(
                    "Project `{project}` has active recent local context; prefer project-scoped ContextEnvelope evidence for work in this project."
                ),
                evidence_episode_ids: stats
                    .samples
                    .iter()
                    .take(EVIDENCE_CAP_PER_RULE)
                    .map(|(_, id)| *id)
                    .collect(),
                confidence: 0.66,
            });
        }
    }

    rules
}

fn confidence_from_repetition(count: usize) -> f32 {
    let confidence = 0.55 + (count.min(6) as f32 * 0.05);
    confidence.min(0.85)
}

fn normalize_rules(storage: &Storage, rules: &mut Vec<Policy>) {
    for rule in rules.iter_mut() {
        if rule.confidence > HEURISTIC_CONFIDENCE_CAP {
            rule.confidence = HEURISTIC_CONFIDENCE_CAP;
        }
        rule.evidence_episode_ids.truncate(EVIDENCE_CAP_PER_RULE);
        rule.evidence_episode_ids.retain(|id| matches!(storage.get_live_episode(*id), Ok(Some(_))));
    }
    rules.retain(|rule| !rule.evidence_episode_ids.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Episode, EpisodeSource};

    fn append_terminal(
        storage: &mut Storage,
        project: &str,
        command: &str,
        exit_code: i32,
        ts: i64,
    ) -> EpisodeId {
        let ep = Episode {
            ts_start_ns: ts,
            ts_end_ns: ts,
            duration_ms: 0,
            source: EpisodeSource::Terminal,
            session_id: Some("policy-test".to_string()),
            prompt_text: None,
            response_text: None,
            command: Some(command.to_string()),
            stdout: None,
            exit_code: Some(exit_code),
            cwd: None,
            git_branch: None,
            project: Some(project.to_string()),
            digest: None,
        };
        storage.append_episode(&ep).expect("append terminal")
    }

    #[test]
    fn extract_policies_uses_deterministic_command_evidence() {
        let mut storage = Storage::open_in_memory().expect("open");
        let first = append_terminal(&mut storage, "SOMA", "cargo test -p soma", 0, 1);
        let second = append_terminal(&mut storage, "SOMA", "cargo test --lib", 0, 2);
        let other = append_terminal(&mut storage, "other", "cargo test --ignored", 0, 3);

        let rules = extract_policies(&storage, Some("SOMA")).expect("rules");
        let cargo_rule =
            rules.iter().find(|rule| rule.rule.contains("`cargo`")).expect("cargo repetition rule");

        assert!(cargo_rule.evidence_episode_ids.contains(&first));
        assert!(cargo_rule.evidence_episode_ids.contains(&second));
        assert!(!cargo_rule.evidence_episode_ids.contains(&other));
        assert!(cargo_rule.confidence <= HEURISTIC_CONFIDENCE_CAP);
    }

    #[test]
    fn extract_policies_surfaces_repeated_failures_without_cloud_llm() {
        let mut storage = Storage::open_in_memory().expect("open");
        let failed_a = append_terminal(&mut storage, "SOMA", "cargo test -p soma", 101, 1);
        let failed_b = append_terminal(&mut storage, "SOMA", "cargo clippy -p soma", 101, 2);
        append_terminal(&mut storage, "SOMA", "cargo fmt --check", 0, 3);

        let rules = extract_policies(&storage, Some("SOMA")).expect("rules");
        let failure_rule =
            rules.iter().find(|rule| rule.rule.contains("non-zero exits")).expect("failure rule");

        assert!(failure_rule.evidence_episode_ids.contains(&failed_a));
        assert!(failure_rule.evidence_episode_ids.contains(&failed_b));
    }

    #[test]
    fn extract_policies_returns_none_without_repeated_signal() {
        let mut storage = Storage::open_in_memory().expect("open");
        append_terminal(&mut storage, "SOMA", "cargo fmt --check", 0, 1);

        let rules = extract_policies(&storage, Some("SOMA"));

        assert!(rules.is_none(), "single command should not create a policy");
    }

    #[test]
    fn render_empty_rules_emits_warning_line() {
        let md = render_markdown(&[], Some("aenv"));
        assert!(md.contains("# SOMA policy — aenv"));
        assert!(md.contains("no rules extracted"));
    }

    #[test]
    fn render_rules_includes_evidence_and_confidence() {
        let rules = vec![Policy {
            rule: "jy 는 한국어 prose 를 선호".to_string(),
            evidence_episode_ids: vec![10, 20, 30],
            confidence: 0.87,
        }];
        let md = render_markdown(&rules, None);
        assert!(md.contains("# SOMA policy — global"));
        assert!(md.contains("jy 는 한국어 prose 를 선호"));
        assert!(md.contains("87%"));
        assert!(md.contains("episode 10, 20, 30"));
    }

    #[test]
    fn policy_set_round_trips_through_self_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let rules = vec![Policy {
            rule: "Prefer concise Korean status updates.".to_string(),
            evidence_episode_ids: vec![10, 20, 10],
            confidence: 0.81,
        }];

        upsert_policy_set(&mut storage, Some("SOMA"), &rules).expect("upsert policy");
        let loaded = read_policy_set(&storage, Some("SOMA")).expect("read policy");
        let rows = storage.read_all_self_facts().expect("self facts");
        let policy_row = rows
            .iter()
            .find(|row| row.kind == SELF_STATE_KIND && row.key == "project:SOMA")
            .expect("policy row");
        let evidence: Vec<EpisodeId> =
            serde_json::from_str(&policy_row.evidence_ids_json).expect("evidence ids");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].rule, rules[0].rule);
        assert_eq!(evidence, vec![10, 20]);
    }
}
