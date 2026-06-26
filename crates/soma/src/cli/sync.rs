//! `soma sync-claudemd` — legacy CLAUDE.md migration/debug helper.
//!
//! The command splices a SOMA-owned section into `<cwd>/CLAUDE.md`
//! using the historical context/profile helper plus current self_state
//! policy rows. It is hidden from the core CLI surface and should not be
//! treated as the cloud-LLM read path; canonical delivery is MCP
//! `soma://context/*`.

#[derive(Debug)]
pub enum SyncError {
    Path(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Path(m) => write!(f, "path: {m}"),
            SyncError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}

pub fn exit_code_for(e: &SyncError) -> i32 {
    match e {
        SyncError::Path(_) => 3,
        SyncError::Io(_) => 5,
    }
}

/// Run `soma sync-claudemd`. Resolves the cwd → project name
/// (`crate::project::current_name`) → legacy context/profile path
/// (`memory::persona::persona_card_path_for_project`) → self_state policy rows
/// → splice into `<cwd>/CLAUDE.md`.
pub fn run_sync_claudemd(target_project: Option<String>) -> Result<(), SyncError> {
    let project = target_project.or_else(crate::project::current_name);
    let cwd = std::env::current_dir().map_err(SyncError::Io)?;
    let claudemd_path = cwd.join("CLAUDE.md");

    // Legacy context/profile body — project-scoped if available, global
    // fallback otherwise.
    let profile_path = crate::memory::persona::persona_card_path_for_project(project.as_deref())
        .ok_or_else(|| SyncError::Path("home directory not resolvable".into()))?;
    let profile_body = std::fs::read_to_string(&profile_path).unwrap_or_else(|_| String::new());

    // Policy markdown is rendered from self_state on demand. Slow-loop updates
    // the canonical `policy` rows but no longer emits ~/.soma/policy/*.md.
    let policy_body = read_policy_markdown(project.as_deref());

    let section = crate::memory::claudemd::build_soma_section(&profile_body, &policy_body);
    let existing = std::fs::read_to_string(&claudemd_path).ok();
    let merged = crate::memory::claudemd::splice_section(existing.as_deref(), &section);
    crate::memory::claudemd::write_claudemd(&claudemd_path, &merged).map_err(SyncError::Io)?;

    println!("soma: synced CLAUDE.md");
    println!("  project: {}", project.as_deref().unwrap_or("(global)"));
    println!("  path:    {}", claudemd_path.display());
    println!("  context profile: {} bytes", profile_body.chars().count());
    println!("  policy:  {} bytes", policy_body.chars().count());
    Ok(())
}

fn read_policy_markdown(project: Option<&str>) -> String {
    let db_path = match crate::capture::ai_cli::resolve_db_path(None) {
        Ok(path) => path,
        Err(e) => {
            tracing::debug!(error = %e, "sync-claudemd: policy db path unavailable");
            return String::new();
        }
    };
    read_policy_markdown_from_db(&db_path, project)
}

fn read_policy_markdown_from_db(db_path: &std::path::Path, project: Option<&str>) -> String {
    let storage = match crate::storage::Storage::open(db_path) {
        Ok(storage) => storage,
        Err(e) => {
            tracing::debug!(error = %e, "sync-claudemd: policy storage unavailable");
            return String::new();
        }
    };
    let rules = match crate::memory::policy::read_policy_set(&storage, project) {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(error = %e, "sync-claudemd: policy self_state read failed");
            return String::new();
        }
    };
    if rules.is_empty() {
        String::new()
    } else {
        crate::memory::policy::render_markdown(&rules, project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::policy::{upsert_policy_set, Policy};
    use crate::storage::Storage;

    #[test]
    fn policy_markdown_reads_self_state_not_policy_file_mirror() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&db_path).expect("storage");
        upsert_policy_set(
            &mut storage,
            Some("myapp"),
            &[Policy {
                rule: "Run cargo fmt before review.".to_string(),
                evidence_episode_ids: vec![7],
                confidence: 0.91,
            }],
        )
        .expect("upsert policy");
        drop(storage);

        let md = read_policy_markdown_from_db(&db_path, Some("myapp"));
        assert!(md.contains("# SOMA policy — myapp"), "{md}");
        assert!(md.contains("Run cargo fmt before review."), "{md}");
        assert!(md.contains("evidence: episode 7"), "{md}");
    }
}
