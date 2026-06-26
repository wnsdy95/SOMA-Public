//! Legacy CLAUDE.md SOMA-section migration/debug helper.
//!
//! This module came from ADR 0013's CLAUDE.md prompt-injection path,
//! now superseded by the ContextEnvelope reset. It still supports
//! users who want a marker-bounded SOMA section in a project
//! `CLAUDE.md`, but it is not the canonical cloud-LLM read path.
//! Current clients should consume MCP `soma://context/*` resources and
//! active tools.
//!
//! ## Section invariant
//!
//! CLAUDE.md 안에 SOMA 가 own 하는 영역 은 markers 사이 만:
//!
//! ```text
//! <!-- SOMA-BEGIN (auto-generated, do not edit by hand) -->
//! ...
//! <!-- SOMA-END -->
//! ```
//!
//! markers 밖 의 user-written content 는 보존 — sync 가 *replace
//! between markers* 의 idempotent operation. 첫 sync 시 markers 가
//! 없으면 file 끝에 append.
//!
//! ## Output structure
//!
//! SOMA-section 의 본문 = legacy context/profile preview + self_state-derived
//! policy markdown 합본. 사용자 가 git diff 로 변화 review.

use std::path::Path;

/// SOMA-owned section 의 BEGIN marker. CLAUDE.md 안에 정확히 한
/// 번 만 존재 해야 함 — 다중 등장 시 첫 번째 만 used (이후 는
/// 보존, sync 가 race 발생 시).
pub const SOMA_BEGIN: &str = "<!-- SOMA-BEGIN (auto-generated, do not edit by hand) -->";
pub const SOMA_END: &str = "<!-- SOMA-END -->";

/// Compose the SOMA-section body. `context_profile` is the project-
/// scoped legacy profile text (synthesize_persona_card output); `policy_md`
/// is the rendered policy list (memory::policy::render_markdown output)
/// generated from self_state on demand. Either can be empty — legacy
/// callers only emit what's available.
pub fn build_soma_section(context_profile: &str, policy_md: &str) -> String {
    let mut out = String::new();
    out.push_str(SOMA_BEGIN);
    out.push('\n');
    out.push_str("<!-- Source: SOMA's legacy context/profile helper + policy extractor. -->\n");
    out.push_str(
        "<!-- Edit your own context outside the SOMA markers; this block is overwritten. -->\n\n",
    );
    if !context_profile.trim().is_empty() {
        out.push_str("## SOMA context profile\n\n");
        out.push_str(context_profile.trim());
        out.push_str("\n\n");
    }
    if !policy_md.trim().is_empty() {
        out.push_str("## Extracted policies\n\n");
        out.push_str(policy_md.trim());
        out.push_str("\n\n");
    }
    out.push_str(SOMA_END);
    out.push('\n');
    out
}

/// Splice the SOMA-section into a CLAUDE.md body.
///
/// * `existing` is `None` (file doesn't exist yet) — return
///   `soma_section + "\n"`.
/// * `existing` contains both markers — replace the substring
///   between them (inclusive) with the new section.
/// * `existing` exists but no markers — append `\n\n + soma_section`
///   to the end.
/// * Markers in non-canonical order (END before BEGIN) → preserve
///   user content unchanged + append a fresh section. fail-safe.
pub fn splice_section(existing: Option<&str>, soma_section: &str) -> String {
    let body = match existing {
        None => return format!("{soma_section}\n"),
        Some(b) => b,
    };
    let begin_pos = body.find(SOMA_BEGIN);
    let end_pos = body.find(SOMA_END);
    match (begin_pos, end_pos) {
        (Some(b), Some(e)) if e > b => {
            // Replace [b, e + END.len()] with the new section.
            let prefix = &body[..b];
            let suffix = &body[e + SOMA_END.len()..];
            // Trim a single trailing newline from prefix + leading
            // from suffix to avoid blank-line creep across re-syncs.
            let prefix_trim = prefix.trim_end_matches('\n');
            let suffix_trim = suffix.trim_start_matches('\n');
            let mut out = String::with_capacity(body.len());
            out.push_str(prefix_trim);
            if !prefix_trim.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(soma_section);
            if !suffix_trim.is_empty() {
                out.push('\n');
                out.push_str(suffix_trim);
                out.push('\n');
            }
            out
        }
        _ => {
            // No markers (or non-canonical order) — append fresh
            // section at end. fail-safe: never destroy user
            // content. soma_section 자체 가 trailing `\n` 가지고
            // 끝나므로 추가 newline 안 push (idempotent across
            // 재호출).
            let trimmed = body.trim_end_matches('\n');
            let mut out = String::with_capacity(body.len() + soma_section.len() + 4);
            out.push_str(trimmed);
            if !trimmed.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(soma_section);
            out
        }
    }
}

/// Atomic write to `<project>/CLAUDE.md` via tempfile + rename.
/// Pattern matches `memory::persona::atomic_write` (D ultrareview
/// round 2). Caller resolves `path` to the final destination.
pub fn write_claudemd(path: &Path, body: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "CLAUDE.md path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "CLAUDE.md path has no file name")
    })?;
    let tmp = parent.join(format!(".{}.tmp-{}", file_name.to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_section_with_both_inputs() {
        let s = build_soma_section("# context profile\n\nhello", "# policy\n\nrule 1");
        assert!(s.starts_with(SOMA_BEGIN));
        assert!(s.ends_with(&format!("{SOMA_END}\n")));
        assert!(s.contains("## SOMA context profile"));
        assert!(!s.contains("## SOMA persona"));
        assert!(s.contains("## Extracted policies"));
        assert!(s.contains("hello"));
        assert!(s.contains("rule 1"));
    }

    #[test]
    fn build_section_skips_empty_blocks() {
        let s = build_soma_section("", "rules");
        assert!(!s.contains("## SOMA context profile"));
        assert!(s.contains("## Extracted policies"));
        let s = build_soma_section("p", "");
        assert!(s.contains("## SOMA context profile"));
        assert!(!s.contains("## SOMA persona"));
        assert!(!s.contains("## Extracted policies"));
    }

    #[test]
    fn splice_into_empty_returns_section_only() {
        let s = build_soma_section("p", "r");
        let out = splice_section(None, &s);
        assert!(out.starts_with(SOMA_BEGIN));
    }

    #[test]
    fn splice_into_marker_free_appends() {
        let s = build_soma_section("p", "r");
        let user = "# my project\n\nuser-written rules";
        let out = splice_section(Some(user), &s);
        assert!(out.starts_with("# my project"));
        assert!(out.contains("user-written rules"));
        assert!(out.contains(SOMA_BEGIN));
        // user content 가 SOMA section 앞 에 그대로 있어야 함.
        let user_idx = out.find("user-written").unwrap();
        let soma_idx = out.find(SOMA_BEGIN).unwrap();
        assert!(user_idx < soma_idx);
    }

    #[test]
    fn splice_replaces_existing_section() {
        let old_section = build_soma_section("OLD persona", "OLD policy");
        let user_before = "# my project\n\nintro\n\n";
        let user_after = "\n\n## my own section\n\nfoo";
        let existing = format!("{user_before}{old_section}{user_after}");
        let new_section = build_soma_section("NEW persona", "NEW policy");
        let out = splice_section(Some(&existing), &new_section);
        // user prefix + suffix preserved.
        assert!(out.contains("intro"));
        assert!(out.contains("## my own section"));
        // OLD content gone.
        assert!(!out.contains("OLD persona"));
        assert!(!out.contains("OLD policy"));
        // NEW content present.
        assert!(out.contains("NEW persona"));
        assert!(out.contains("NEW policy"));
    }

    #[test]
    fn splice_idempotent_across_repeated_calls() {
        let s = build_soma_section("p", "r");
        let user = "# my project\n\nintro";
        let once = splice_section(Some(user), &s);
        let twice = splice_section(Some(&once), &s);
        // 두 번째 splice 가 동일 결과 (markers 인식 후 그대로
        // replace).
        assert_eq!(once, twice);
    }
}
