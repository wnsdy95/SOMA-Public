//! Scope selection helpers for optional context quality modules.

use serde::Deserialize;

use crate::storage::{Storage, StorageError};

const ANIL_SELF_STATE_KIND: &str = "anil";
const ANIL_PROJECT_ATTRIBUTION_KEY: &str = "project_attribution";
const ANIL_SCOPE_MIN_PROBABILITY: f32 = 0.80;
const ANIL_SCOPE_MIN_MARGIN: f32 = 0.20;

/// Select a project scope from ANIL only when the caller did not
/// provide an explicit project/session filter. Low-confidence or
/// ambiguous attribution stays `None`, preserving the default current
/// scope.
pub(crate) fn inferred_project_scope_from_anil(
    storage: &Storage,
) -> Result<Option<String>, StorageError> {
    let Some(row) = storage
        .read_all_self_facts()?
        .into_iter()
        .find(|row| row.kind == ANIL_SELF_STATE_KIND && row.key == ANIL_PROJECT_ATTRIBUTION_KEY)
    else {
        return Ok(None);
    };

    let attribution: ProjectAttribution = serde_json::from_str(&row.value_json).map_err(|e| {
        StorageError::Corrupt { detail: format!("ANIL project_attribution JSON: {e}") }
    })?;
    if attribution.episode_count == 0 {
        return Ok(None);
    }

    let mut entries = attribution
        .distribution
        .into_iter()
        .filter(|entry| {
            !entry.project.trim().is_empty()
                && entry.probability.is_finite()
                && (0.0..=1.0).contains(&entry.probability)
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort_by(|a, b| {
        b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal)
    });

    let top = &entries[0];
    let runner_up = entries.get(1).map(|entry| entry.probability).unwrap_or(0.0);
    if top.probability < ANIL_SCOPE_MIN_PROBABILITY {
        return Ok(None);
    }
    if top.probability - runner_up < ANIL_SCOPE_MIN_MARGIN {
        return Ok(None);
    }

    Ok(Some(top.project.clone()))
}

#[derive(Debug, Deserialize)]
struct ProjectAttribution {
    #[allow(dead_code)]
    k: usize,
    episode_count: usize,
    distribution: Vec<ProjectProbability>,
}

#[derive(Debug, Deserialize)]
struct ProjectProbability {
    project: String,
    probability: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    #[test]
    fn anil_scope_selects_clear_top_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let attribution = serde_json::json!({
            "k": 2,
            "episode_count": 4,
            "distribution": [
                { "project": "myapp", "probability": 0.10 },
                { "project": "other", "probability": 0.90 }
            ]
        });
        storage
            .upsert_self_fact("anil", "project_attribution", &attribution.to_string(), &[])
            .expect("save ANIL attribution");

        let selected = inferred_project_scope_from_anil(&storage).expect("select");

        assert_eq!(selected.as_deref(), Some("other"));
    }

    #[test]
    fn anil_scope_ignores_ambiguous_project_distribution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let attribution = serde_json::json!({
            "k": 2,
            "episode_count": 4,
            "distribution": [
                { "project": "myapp", "probability": 0.55 },
                { "project": "other", "probability": 0.45 }
            ]
        });
        storage
            .upsert_self_fact("anil", "project_attribution", &attribution.to_string(), &[])
            .expect("save ANIL attribution");

        let selected = inferred_project_scope_from_anil(&storage).expect("select");

        assert!(selected.is_none(), "ambiguous ANIL attribution must not choose scope");
    }
}
