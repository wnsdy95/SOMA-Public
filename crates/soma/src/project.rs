//! D147 — single source of truth for "what project is the user in
//! right now?". The project name scopes capture-side episode stamps,
//! ContextEnvelope reads, and legacy context/profile migration helpers.
//! Any future tweak (e.g. git-remote disambiguation, slug normalization)
//! must land here and flow through every caller.
//!
//! Strategy is deliberately the same as before: `basename($PWD)`.
//! The capture path (soma-stop.sh, shell-init hooks) writes the
//! same value into `episodes.project`, so equality matching against
//! recall results works without extra normalization. Any future
//! change here MUST also flow through the capture-side stamp or the
//! recall lens silently breaks.
//!
//! Out of scope (D147.5 follow-up):
//! * git-remote disambiguation for two different repos sharing the
//!   same basename (e.g. `~/code/foo` and `~/work/foo`). Adding a
//!   git-remote slug here without also reshaping the capture path
//!   would create a recall-lens mismatch — a separate chunk owns
//!   that coordinated change.

use std::path::Path;

/// Resolve the current project name from the working directory.
/// Returns `None` if the cwd has no usable basename (e.g. the
/// process was started from `/`).
pub fn current_name() -> Option<String> {
    let cwd = std::env::current_dir().ok();
    name_from_path(cwd.as_deref())
}

/// Test-friendly entry — separates the env::current_dir call from
/// the basename logic so unit tests don't have to chdir the test
/// process.
pub fn name_from_path(cwd: Option<&Path>) -> Option<String> {
    cwd.and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn name_from_cwd_returns_basename_for_normal_path() {
        let p = PathBuf::from("/Users/example/code/projects/SOMA");
        assert_eq!(name_from_path(Some(&p)), Some("SOMA".to_string()));
    }

    #[test]
    fn name_from_cwd_handles_trailing_slash() {
        // PathBuf::from("/foo/bar/") still has file_name() = "bar".
        let p = PathBuf::from("/foo/bar/");
        assert_eq!(name_from_path(Some(&p)), Some("bar".to_string()));
    }

    #[test]
    fn name_from_cwd_returns_none_for_root() {
        let p = PathBuf::from("/");
        assert_eq!(name_from_path(Some(&p)), None);
    }

    #[test]
    fn name_from_cwd_returns_none_when_unresolved() {
        assert_eq!(name_from_path(None), None);
    }
}
