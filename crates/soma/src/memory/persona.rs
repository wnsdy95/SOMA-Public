//! Legacy context/profile helper artifacts.
//!
//! These files predate the ContextEnvelope reset and keep their
//! historical filenames for schema continuity. They are now descriptive
//! helper artifacts: users and legacy hooks can read them for
//! user/project context, but they are not the product identity, a
//! first-person companion contract, or the canonical cloud-LLM read
//! path. Current clients should use MCP `soma://context/*`.
//!
//! Two outputs (different audiences):
//!
//! * `identity.md` — long-form (≤ 1000 token) profile/context prose.
//!   The user reads this directly to inspect what SOMA has inferred.
//!   Updated every slow_loop cycle.
//! * `persona-card.md` — short-form (≤ 400 token), preserved for
//!   disabled legacy prompt-injection hook flows.
//!
//! D153 phase 1 ([[0013-cloud-llm-context-replacement]]) — the
//! legacy context-card surface is now **project-scoped**. Per active
//! project a separate card lives at
//! `~/.soma/self/persona-cards/<project>.md` (current-thread
//! filtered to that project's episodes; trait/values stay
//! user-level per A7). The historical single
//! `~/.soma/self/persona-card.md` is preserved as the *global*
//! card for cross-project / unknown-project fallback (A3).
//!
//! Both come from the same source: self_state aggregates + recent
//! episodes + belief_candidates + prior artifacts for continuity.
//! Optional quality modules may influence them only when they improve
//! ContextEnvelope-relevant context. `llm-summary` is intentionally
//! not wired into these artifacts; it remains a legacy slow-loop
//! narrative diagnostic, while the current cloud/local bridge admits
//! optional LLM text only as cited ContextEnvelope `compiler_notes`.

use std::path::{Path, PathBuf};

use crate::storage::{Storage, StorageError};

/// Where the legacy context/profile artifacts live. `~/.soma/self/`.
fn self_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".soma").join("self"))
}

/// Per-project persona-card directory. `~/.soma/self/persona-cards/`.
fn project_cards_dir() -> Option<PathBuf> {
    self_dir().map(|d| d.join("persona-cards"))
}

/// Path to the long-form identity document. User-readable.
pub fn identity_path() -> Option<PathBuf> {
    self_dir().map(|d| d.join("identity.md"))
}

/// Path to the global (cross-project) legacy card. Hook-injected
/// when the cwd has no project-specific card yet.
pub fn persona_card_path() -> Option<PathBuf> {
    self_dir().map(|d| d.join("persona-card.md"))
}

/// Path to the project-specific legacy card for `project`.
/// `None` returns the global card path (A3 fallback).
pub fn persona_card_path_for_project(project: Option<&str>) -> Option<PathBuf> {
    match project {
        None => persona_card_path(),
        Some(p) => {
            let safe = sanitize_project_name(p)?;
            project_cards_dir().map(|d| d.join(format!("{safe}.md")))
        }
    }
}

/// D147 — re-export of `crate::project::current_name` so existing
/// callers (`cli::persona`) stay on the `memory::persona` symbol
/// they were importing while the canonical implementation lives in
/// the dedicated module.
pub use crate::project::current_name as current_project_name;

/// Restrict a project name to a filesystem-safe slug. Returns None
/// if the name has no usable characters. Keeps lowercase alphanum +
/// `-` + `_`; everything else collapses to `-`. Two-or-more
/// consecutive `-` collapse to one.
fn sanitize_project_name(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.chars() {
        let mapped =
            if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '-' };
        if mapped == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Synthesize the long-form identity document. Always returns rule-
/// based prose at minimum; LLM-tuned when feature on + secret loaded.
///
/// The identity contains 5 sections:
///
/// 1. Header — descriptive observation period and episode count.
/// 2. Values — top 3 traits inferred from CLAUDE.md and self_state.
/// 3. Working style — most-used commands, project clusters, exit
///    success rate, ingest cadence.
/// 4. Current thread — what the user is in the middle of (recent
///    episode topics + project_norms top entries).
/// 5. Open questions / contradictions — recent belief_candidates
///    with kind = `contradicts`.
pub fn synthesize_identity(storage: &Storage) -> Result<String, StorageError> {
    let observed_days = observed_days_since_first_episode(storage)?;
    let total_episodes = storage.counters().map(|(ep, _)| ep).unwrap_or(0);

    let narrative = crate::memory::narrative::synthesize_paragraph(storage)?;

    let project_top = top_projects(storage)?;
    let recent_thread = current_thread_summary(storage, None)?;
    let contradictions = recent_contradictions_summary(storage)?;

    let mut out = String::new();
    out.push_str("# SOMA context profile\n\n");
    out.push_str(&format!(
        "SOMA has observed the user's local work for **{observed_days} days**; \
         {total_episodes} episodes are captured so far.\n\n"
    ));

    out.push_str("## Observed working style\n\n");
    out.push_str(&narrative);
    out.push_str("\n\n");

    if !project_top.is_empty() {
        out.push_str("## Current focus areas\n\n");
        for (proj, count) in project_top.iter().take(3) {
            out.push_str(&format!("- **{proj}** — {count} episodes\n"));
        }
        out.push('\n');
    }

    if !recent_thread.is_empty() {
        out.push_str("## Current thread\n\n");
        out.push_str(&recent_thread);
        out.push_str("\n\n");
    }

    if !contradictions.is_empty() {
        out.push_str("## Open contradictions\n\n");
        out.push_str(&contradictions);
        out.push_str("\n\n");
    }

    out.push_str("---\n");
    out.push_str(
        "_본 문서 는 매 slow_loop cycle (≈ 1h) 마다 자동 갱신. \
                 사용자 가 직접 편집 하지 않음 — 직접 갱신 은 \
                 `soma persona regen`._\n",
    );

    Ok(out)
}

/// Synthesize the short-form legacy context/profile card. Token budget ≤ 400
/// is preserved for disabled legacy prompt-injection hook flows.
///
/// `project = None` produces the global card (cross-project view —
/// `Active projects` line lists top-3, `Current thread` covers all
/// recent episodes). `project = Some("aenv")` narrows the
/// `Active project` line to that one and filters `Current thread`
/// to that project's episodes only. Trait/values stay user-level
/// (A7 — same identity across projects).
pub fn synthesize_persona_card(
    storage: &Storage,
    project: Option<&str>,
) -> Result<String, StorageError> {
    let observed_days = observed_days_since_first_episode(storage)?;
    let total_episodes = storage.counters().map(|(ep, _)| ep).unwrap_or(0);

    let recent_thread = current_thread_summary(storage, project)?;
    let project_top = top_projects(storage)?;

    let mut out = String::new();
    // Descriptive opening. The earlier "You are SOMA, ..." was
    // prescriptive — when this card was injected into a cloud-LLM
    // session via `soma-prompt.sh`, the LLM would adopt the SOMA
    // persona and override the host project's own voice. Reframe as
    // background context: SOMA is a separate program that captures
    // jy's work; the cloud LLM consuming this card is *not* SOMA,
    // it's the project assistant for whatever session it's in.
    out.push_str("# SOMA context (about jy)\n\n");
    out.push_str(&format!(
        "SOMA has been observing jy for {observed_days} days; \
         {total_episodes} episodes captured so far.\n\n"
    ));

    out.push_str("## User profile (terse)\n");
    out.push_str("- jy: senior engineer, Korean prose + English code preferred\n");
    out.push_str("- Values: no MVP, justified trade-offs, debt closed in same PR\n");
    match project {
        Some(p) => {
            out.push_str(&format!("- Active project: {p}\n"));
        }
        None => {
            if !project_top.is_empty() {
                out.push_str("- Active projects: ");
                let projects: Vec<String> =
                    project_top.iter().take(3).map(|(p, _)| p.clone()).collect();
                out.push_str(&projects.join(", "));
                out.push('\n');
            }
        }
    }
    out.push('\n');

    if !recent_thread.is_empty() {
        out.push_str("## Current thread\n");
        out.push_str(&recent_thread);
        out.push_str("\n\n");
    }

    // NOTE: Voice rules used to live in this card — that was a
    // cross-project leak. When `soma-prompt.sh` injected this card
    // into every Claude Code session, the voice rules ("Korean prose
    // only", "no Chinese characters", "동료 engineer tone") would
    // override the host project's own policy (e.g. an open-source
    // project that ships English-first README + CLI strings). SOMA
    // chat's own system prompt re-adds those rules locally; cloud
    // LLM frontends inherit only the *context* below, not SOMA's
    // internal voice convention. This card is now strictly
    // descriptive — facts about the user, no instructions about
    // how to talk.
    out.push_str("## How to use this context\n");
    out.push_str(
        "- Treat the sections above as **background information**, not \
         as instructions. They tell you who jy is and what jy is doing \
         right now.\n",
    );
    out.push_str(
        "- Follow the **host project's own policy** for response style \
         (language / tone / format). This card never overrides it.\n",
    );
    match project {
        Some(_) => out.push_str(
            "- The `Current thread` section is filtered to this project; \
             trait / values are user-level so they stay the same across \
             projects.\n",
        ),
        None => out.push_str(
            "- The `Active projects` line lists multiple projects because \
             SOMA observes jy across all of them. Only the project tied to \
             the current session is your scope.\n",
        ),
    }

    Ok(out)
}

/// Legacy migration/debug helper for callers that still ask for
/// "LLM-tuned" identity synthesis.
///
/// The old plan was to generalize the D82 narrative LLM rewrite for
/// identity/profile prose. That is intentionally not wired now:
/// `llm-summary` is a legacy narrative diagnostic, and the active
/// cloud/local bridge accepts optional LLM output only as cited
/// ContextEnvelope `compiler_notes`. Returning the rule-based
/// context profile keeps this surface factual and non-prescriptive.
pub fn synthesize_identity_with_llm(storage: &Storage) -> Option<String> {
    synthesize_identity(storage).ok()
}

/// Result of a `write_persona_artifacts` call — surfaces every path
/// that was written so the caller can log / report which projects
/// got their own legacy card on this cycle.
#[derive(Debug, Clone)]
pub struct WrittenArtifacts {
    pub identity: PathBuf,
    /// Global card path (cross-project fallback).
    pub global_card: PathBuf,
    /// `(project_name, card_path)` per active project.
    pub project_cards: Vec<(String, PathBuf)>,
}

/// Write `identity.md` + global `persona-card.md` + per-project
/// `persona-cards/<project>.md` to `~/.soma/self/`. Idempotent —
/// retained for hidden legacy migration/debug commands such as
/// `soma persona regen` / `soma persona inject`.
///
/// Per A8 (atomic invariant), every file is written via `tempfile +
/// rename`. Per A6 (write cadence), all active top-N projects (top
/// 5 by recent episode count) plus the global card flip in the same
/// explicit call, so legacy readers see a consistent set of cards.
pub fn write_persona_artifacts(storage: &Storage) -> Result<WrittenArtifacts, StorageError> {
    // D169 actual fix — defensive guard 의 episode_count > 0 만
    // 으로는 *test fixture* 가 통과 (3 episode seed, project="p",
    // text "alpha"/"beta"/"gamma"). production HOME 에 그 fixture
    // 의 결과 가 적힌 사례 (`p.md` 가 905 days observed / 3
    // episodes). 진짜 invariant 는 *storage 의 db_path 가
    // production canonical 인지*. Storage 가 read 한 db 가 정확히
    // `~/.soma/soma.db` 가 아니면 production self/ overwrite 거부.
    let canonical = dirs::home_dir().map(|h| h.join(".soma").join("soma.db")).ok_or_else(|| {
        StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "home directory not resolvable",
        ))
    })?;
    let supplied = storage.db_path();
    let supplied_resolved = supplied.canonicalize().unwrap_or_else(|_| supplied.to_path_buf());
    let canonical_resolved = canonical.canonicalize().unwrap_or(canonical.clone());
    if supplied_resolved != canonical_resolved && supplied != canonical {
        return Err(StorageError::Corrupt {
            detail: format!(
                "refuse to write legacy context/profile artifacts — storage db_path ({}) is not the canonical \
                 production path ({}). Test paths must use write_persona_artifacts_to() with an \
                 explicit base_dir.",
                supplied.display(),
                canonical.display()
            ),
        });
    }
    let dir = self_dir().ok_or_else(|| {
        StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "home directory not resolvable",
        ))
    })?;
    write_persona_artifacts_to(storage, &dir)
}

/// D167 close — explicit-base-dir variant. Test harnesses pass a
/// tempdir so they don't have to mutate the process-wide `HOME`
/// env (the prior approach raced with sibling cargo test threads
/// reading the same env: a HOME-redirect mid-window let an unrelated
/// test fall through to the redirected tempdir, build a 0-state
/// pack against the empty DB there, and silently overwrite the
/// real `~/.soma/self/persona-card.md` with `0 days / 0 episodes`).
/// The production entry above stays the canonical surface for
/// runtime callers; tests reach for this one.
pub fn write_persona_artifacts_to(
    storage: &Storage,
    base_dir: &Path,
) -> Result<WrittenArtifacts, StorageError> {
    // D169 close — 0-episode storage refuses to overwrite cards.
    // Pre-fix some unknown caller path (cargo install / build /
    // doctest 추정) was sneaking in with a fresh tempdir storage
    // and overwriting production HOME's persona-card.md with a
    // *0 days / 0 episodes* card every build cycle. The invariant
    // here is: write_persona_artifacts is meaningful only when
    // there's at least one episode; an empty storage means the
    // caller has the wrong DB and the production cards must not
    // be touched. Returning an Err lets the caller (slow_loop /
    // cli persona regen) log + skip, while doctest / accidental
    // call paths surface a loud failure instead of silent data
    // loss.
    //
    // D169 trace instrumentation — every invocation emits a
    // tracing event with the resolved base_dir, cwd, and
    // episode_count so the next mystery overwrite leaves a
    // breadcrumb in the rolling daily log identifying the caller
    // path. Backtrace embedded in the Err detail when the
    // 0-episode invariant fires.
    let episode_count = storage.counters().map(|(ep, _)| ep).unwrap_or(0);
    let cwd = std::env::current_dir().unwrap_or_default();
    tracing::info!(
        target: "soma::persona::write",
        base_dir = %base_dir.display(),
        cwd = %cwd.display(),
        episode_count,
        "write_persona_artifacts_to invocation"
    );
    if episode_count == 0 {
        let bt = std::backtrace::Backtrace::capture();
        return Err(StorageError::Corrupt {
            detail: format!(
                "refuse to write legacy context/profile artifacts to {} — storage has 0 episodes \
                 (would overwrite real persona-card with empty placeholder). \
                 cwd={}, backtrace=\n{}",
                base_dir.display(),
                cwd.display(),
                bt
            ),
        });
    }
    let dir = base_dir.to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let cards_dir = dir.join("persona-cards");
    std::fs::create_dir_all(&cards_dir)?;

    let identity = synthesize_identity(storage)?;
    let global_card = synthesize_persona_card(storage, None)?;

    let project_top = top_projects(storage)?;
    let active: Vec<(String, String, PathBuf)> = project_top
        .iter()
        .take(5)
        .filter_map(|(p, _)| {
            let safe = sanitize_project_name(p)?;
            let path = cards_dir.join(format!("{safe}.md"));
            Some((p.clone(), safe, path))
        })
        .collect();

    let id_path = dir.join("identity.md");
    let card_path = dir.join("persona-card.md");

    atomic_write(&id_path, identity.as_bytes())?;
    atomic_write(&card_path, global_card.as_bytes())?;

    let mut project_cards = Vec::with_capacity(active.len());
    for (display_name, _safe, path) in &active {
        let body = synthesize_persona_card(storage, Some(display_name.as_str()))?;
        atomic_write(path, body.as_bytes())?;
        project_cards.push((display_name.clone(), path.clone()));
    }

    Ok(WrittenArtifacts { identity: id_path, global_card: card_path, project_cards })
}

fn atomic_write(target: &Path, body: &[u8]) -> Result<(), StorageError> {
    let parent = target.parent().ok_or_else(|| {
        StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no parent directory",
        ))
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no file name",
        ))
    })?;
    let tmp = parent.join(format!(".{}.tmp-{}", file_name.to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn observed_days_since_first_episode(storage: &Storage) -> Result<i64, StorageError> {
    // Heuristic: oldest episode's ts_start_ns → days.
    let recent = storage.recent_episodes(10_000)?;
    let oldest_ns = recent.iter().map(|e| e.ts_start_ns).min().unwrap_or(0);
    if oldest_ns <= 0 {
        return Ok(0);
    }
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let elapsed_ns = (now_ns - oldest_ns).max(0);
    Ok(elapsed_ns / 86_400_000_000_000)
}

fn top_projects(storage: &Storage) -> Result<Vec<(String, usize)>, StorageError> {
    let recent = storage.recent_episodes(500)?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for ep in recent {
        if let Some(p) = ep.project {
            *counts.entry(p).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(sorted)
}

/// `project = Some(p)` filters recent episodes to that project
/// before forming the thread summary. `None` returns the global
/// last-5 view (any project).
fn current_thread_summary(
    storage: &Storage,
    project: Option<&str>,
) -> Result<String, StorageError> {
    // Pull a wider window when filtering — the tail end of the
    // 5-episode global view may straddle multiple projects, so we
    // need enough headroom to find 5 from a single project.
    let window = if project.is_some() { 200 } else { 5 };
    let recent = storage.recent_episodes(window)?;
    if recent.is_empty() {
        return Ok(String::new());
    }
    let filtered: Vec<&crate::storage::StoredEpisode> = recent
        .iter()
        .filter(|ep| match project {
            None => true,
            Some(p) => ep.project.as_deref() == Some(p),
        })
        .take(5)
        .collect();
    if filtered.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    for (i, ep) in filtered.iter().enumerate() {
        let preview = first_line_of_episode(ep);
        if preview.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{idx}. [{src}] {preview}\n",
            idx = i + 1,
            src = ep.source,
            preview = preview.chars().take(120).collect::<String>(),
        ));
    }
    Ok(out)
}

fn first_line_of_episode(ep: &crate::storage::StoredEpisode) -> String {
    if let Some(p) = ep.prompt_text.as_deref() {
        return p.lines().next().unwrap_or("").to_string();
    }
    if let Some(c) = ep.command.as_deref() {
        return c.lines().next().unwrap_or("").to_string();
    }
    String::new()
}

fn recent_contradictions_summary(storage: &Storage) -> Result<String, StorageError> {
    let contradictions = storage.recent_contradictions(5)?;
    if contradictions.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    for c in contradictions {
        let evidence = c.evidence.unwrap_or_else(|| "<no evidence>".to_string());
        out.push_str(&format!(
            "- ep {a} ↔ ep {b}: {evidence} (score {score:.2})\n",
            a = c.episode_a_id,
            b = c.episode_b_id,
            score = c.score,
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_project_name_preserves_simple_slug() {
        assert_eq!(sanitize_project_name("aenv"), Some("aenv".into()));
        assert_eq!(sanitize_project_name("agent-24h-news"), Some("agent-24h-news".into()));
        assert_eq!(sanitize_project_name("SOMA"), Some("soma".into()));
    }

    #[test]
    fn sanitize_project_name_collapses_unsafe_chars() {
        assert_eq!(sanitize_project_name("foo/bar"), Some("foo-bar".into()));
        assert_eq!(sanitize_project_name("foo bar  baz"), Some("foo-bar-baz".into()));
        assert_eq!(sanitize_project_name("..hidden"), Some("hidden".into()));
    }

    #[test]
    fn sanitize_project_name_rejects_empty_after_strip() {
        assert_eq!(sanitize_project_name(""), None);
        assert_eq!(sanitize_project_name("///"), None);
        assert_eq!(sanitize_project_name("   "), None);
    }

    #[test]
    fn persona_card_path_for_project_routes_to_subdir() {
        let some = persona_card_path_for_project(Some("aenv"));
        let none = persona_card_path_for_project(None);
        if let (Some(p), Some(g)) = (some, none) {
            assert!(p.ends_with("persona-cards/aenv.md"), "got {}", p.display());
            assert!(g.ends_with("self/persona-card.md"), "got {}", g.display());
        }
    }

    #[test]
    fn legacy_identity_llm_helper_returns_rule_based_context_profile() {
        use crate::storage::{Episode, EpisodeSource, Storage};

        fn ep(ts: i64, command: &str) -> Episode {
            Episode {
                ts_start_ns: ts,
                ts_end_ns: ts,
                duration_ms: 0,
                source: EpisodeSource::Terminal,
                session_id: Some("identity-test".into()),
                prompt_text: None,
                response_text: None,
                command: Some(command.into()),
                stdout: None,
                exit_code: Some(0),
                cwd: None,
                git_branch: None,
                project: Some("soma".into()),
                digest: None,
            }
        }

        let mut storage = Storage::open_in_memory().expect("open in-memory storage");
        for (idx, command) in ["cargo build", "cargo test", "git status"].iter().enumerate() {
            storage.append_episode(&ep(idx as i64 + 1, command)).expect("append episode");
        }

        let profile = synthesize_identity_with_llm(&storage).expect("context profile");
        assert!(profile.starts_with("# SOMA context profile"), "{profile}");
        assert!(profile.contains("## Observed working style"), "{profile}");
    }
}
