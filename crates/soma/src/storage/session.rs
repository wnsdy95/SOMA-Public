//! Session id factories — typed source for the `session_id`
//! prefix invariant. Multiple capture paths emit `session_id`
//! into `episodes` / `chat_recall_trace`; each path uses a
//! distinct prefix so dashboard / cohort queries can group by
//! origin:
//!
//! * `chat-<ns>` — historical soma chat REPL session rows.
//! * `cli-recall-<ns>` — `soma recall` invocation (CLI direct or
//!   prompt-hook's SOMA_INJECT_RECALL=1 path).
//! * `<uuid>` — Claude Code / Codex CLI/App / Cursor / Continue (external IDE
//!   stamps a session uuid; we round-trip unchanged).
//! * `soma-<client>-<project>-<pid>-<ns>` — SOMA-managed terminal /
//!   CLI sessions started with `soma session start`.
//!
//! Free-form session-id format calls were a typo-trap. Centralising
//! active factories here closes that drift and mirrors the AuditReason
//! / SelfStateKind pattern.
//!
//! No enum (yet) — sites that *consume* the session_id either
//! treat it as opaque or read the prefix string directly. When a
//! third use case arrives (e.g. cursor stop-hook session
//! attribution), promote to enum + variant + payload struct.

/// `session_id` for a `soma recall` CLI invocation. Distinct
/// prefix from `chat-` so the dashboard's active-path highlight
/// doesn't conflate CLI direct queries with REPL turns.
pub fn cli_recall(ns_now: i64) -> String {
    format!("cli-recall-{ns_now}")
}

/// `session_id` for a SOMA-managed terminal or cloud-CLI process.
///
/// These IDs are intentionally opaque to readers but readable for operators:
/// the timestamp makes collisions impractical, while `client` and `project`
/// keep multi-terminal traces inspectable without a join.
pub fn managed_cli(client: &str, project: Option<&str>, ns_now: i64, pid: u32) -> String {
    let client = session_component(client).unwrap_or_else(|| "terminal".to_string());
    let project =
        project.and_then(session_component).unwrap_or_else(|| "unknown-project".to_string());
    format!("soma-{client}-{project}-{pid}-{ns_now}")
}

fn session_component(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    while out.starts_with('-') {
        out.remove(0);
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_recall_prefixed_distinctly() {
        let s = cli_recall(1_700_000_000_000_000_000);
        assert!(s.starts_with("cli-recall-"), "got {s}");
        assert!(!s.starts_with("chat-"), "must NOT collide with chat- prefix");
    }

    #[test]
    fn managed_cli_is_readable_and_distinct() {
        let s = managed_cli("codex-cli", Some("SOMA"), 1_700_000_000_000_000_000, 42);
        assert_eq!(s, "soma-codex-cli-soma-42-1700000000000000000");
    }

    #[test]
    fn managed_cli_sanitizes_components() {
        let s = managed_cli("Claude Code", Some("AI Agent 전용 메신저"), 9, 7);
        assert_eq!(s, "soma-claude-code-ai-agent-7-9");
    }
}
