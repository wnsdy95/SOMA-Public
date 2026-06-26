//! Shell RC injection — terminal source activation without a pty.
//!
//! Discussion 0034 §A (shell RC half) ahead-of-schedule. The full
//! D85 pty driver (signal forwarding + OSC 133 consumer + tmux
//! interaction) is deferred to v1.2; this module delivers the
//! "every shell command becomes an episode" piece via the
//! shell's own post-command hook.
//!
//! Mechanics:
//!
//! * **bash**: `PROMPT_COMMAND` runs after every command; we
//!   append a function call.
//! * **zsh**: `precmd_functions+=(soma_ingest_hook)` runs the same
//!   function before each new prompt.
//! * **fish**: `function soma_ingest_hook --on-event
//!   fish_postexec` mirrors the same shape.
//!
//! Each hook captures the *previous* command + exit code via the
//! shell's history mechanism + `$?` and pipes them to
//! `soma ingest --source terminal --command <cmd> --exit-code N`.
//! If `SOMA_SESSION_ID` / `SOMA_PROJECT` are present (for example
//! after `eval "$(soma session start --client terminal)"`), the hook
//! stamps those values so multiple terminals stay isolated.
//! `soma ingest` is non-blocking enough that a 5 ms shell pause
//! per command is invisible.
//!
//! The injection lives inside an idempotent sentinel block:
//!
//!   # >>> soma shell-init >>>
//!   ...generated lines...
//!   # <<< soma shell-init <<<
//!
//! Re-running `install` overwrites the block in place; `uninstall`
//! removes it. Lines outside the block are never touched — so a
//! user's `oh-my-zsh` / `starship` / `powerlevel10k` config keeps
//! working.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The sentinel block markers. Idempotent re-injection assumes
/// these strings are unique across the shell rc file — they are
/// chosen to be visually distinct from other tool's blocks.
pub const BLOCK_BEGIN: &str = "# >>> soma shell-init >>>";
pub const BLOCK_END: &str = "# <<< soma shell-init <<<";

/// Shell flavour. We auto-detect from the rc filename or accept an
/// explicit override on the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// Map the rc file's basename to a shell flavour. Unknown names
    /// default to `Bash` — the most-conservative hook syntax.
    pub fn from_rc_filename(path: &Path) -> Self {
        match path.file_name().and_then(|s| s.to_str()).unwrap_or("") {
            ".zshrc" | "zshrc" => Shell::Zsh,
            "config.fish" => Shell::Fish,
            _ => Shell::Bash,
        }
    }

    /// Generate the body of the sentinel block — the shell-specific
    /// hook + the call into `soma ingest`. `binary_path` lets the
    /// hook target a specific binary so an out-of-PATH `soma` still
    /// works.
    pub fn render_hook(self, binary_path: &Path) -> String {
        let bin = shell_quote(&binary_path.display().to_string());
        match self {
            Shell::Bash => format!(
                r#"# soma shell-init (bash) — capture last command + exit code.
# Skips empty commands and the hook itself.
__soma_capture() {{
    local rc=$?
    local cmd
    local -a soma_scope
    soma_scope=()
    cmd=$(history 1 | sed 's/^ *[0-9]\+ *//')
    if [ -z "$cmd" ] || [[ "$cmd" == __soma_capture* ]]; then
        return $rc
    fi
    [ -n "${{SOMA_SESSION_ID:-}}" ] && soma_scope+=(--session "$SOMA_SESSION_ID")
    [ -n "${{SOMA_PROJECT:-}}" ] && soma_scope+=(--project "$SOMA_PROJECT")
    {bin} ingest --source terminal --command "$cmd" --exit-code "$rc" "${{soma_scope[@]}}" >/dev/null 2>&1 &
    return $rc
}}
case "$PROMPT_COMMAND" in
    *__soma_capture*) ;;
    "") PROMPT_COMMAND="__soma_capture" ;;
    *) PROMPT_COMMAND="__soma_capture; $PROMPT_COMMAND" ;;
esac
"#
            ),
            Shell::Zsh => format!(
                r#"# soma shell-init (zsh) — capture last command + exit code.
__soma_capture() {{
    local rc=$?
    local cmd="$1"
    local -a soma_scope
    soma_scope=()
    [ -z "$cmd" ] && return $rc
    [ -n "${{SOMA_SESSION_ID:-}}" ] && soma_scope+=(--session "$SOMA_SESSION_ID")
    [ -n "${{SOMA_PROJECT:-}}" ] && soma_scope+=(--project "$SOMA_PROJECT")
    {bin} ingest --source terminal --command "$cmd" --exit-code "$rc" "${{soma_scope[@]}}" >/dev/null 2>&1 &!
}}
autoload -Uz add-zsh-hook 2>/dev/null
if typeset -f add-zsh-hook >/dev/null; then
    add-zsh-hook preexec __soma_capture
fi
"#
            ),
            Shell::Fish => format!(
                r#"# soma shell-init (fish) — capture last command + exit code.
function __soma_capture --on-event fish_postexec
    set -l rc $status
    set -l cmd $argv[1]
    set -l soma_scope
    test -z "$cmd"; and return $rc
    test -n "$SOMA_SESSION_ID"; and set -a soma_scope --session "$SOMA_SESSION_ID"
    test -n "$SOMA_PROJECT"; and set -a soma_scope --project "$SOMA_PROJECT"
    {bin} ingest --source terminal --command "$cmd" --exit-code "$rc" $soma_scope >/dev/null 2>&1 &
    return $rc
end
"#
            ),
        }
    }
}

/// Default rc paths per shell, anchored at `$HOME`.
pub fn default_rc_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".bashrc"),
        home.join(".zshrc"),
        home.join(".config").join("fish").join("config.fish"),
    ]
}

/// Inject (or replace) the sentinel block in `rc_path`. Creates the
/// file (and parent dir) if it doesn't exist. Returns whether the
/// file's contents changed — `false` means the block was already
/// present and identical.
pub fn inject_block(rc_path: &Path, binary_path: &Path) -> io::Result<bool> {
    if let Some(parent) = rc_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = match fs::read_to_string(rc_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let shell = Shell::from_rc_filename(rc_path);
    let hook = shell.render_hook(binary_path);
    let new_block = format!("{BLOCK_BEGIN}\n{hook}{BLOCK_END}\n");

    let updated = if let Some((before, after)) = split_around_block(&existing) {
        format!("{before}{new_block}{after}")
    } else if existing.is_empty() {
        new_block
    } else {
        let sep = if existing.ends_with('\n') { "" } else { "\n" };
        format!("{existing}{sep}\n{new_block}")
    };

    if updated == existing {
        return Ok(false);
    }
    atomic_write(rc_path, &updated)?;
    Ok(true)
}

/// Remove the sentinel block (if present) from `rc_path`. Returns
/// `false` when the file didn't exist or didn't contain the block.
pub fn remove_block(rc_path: &Path) -> io::Result<bool> {
    let existing = match fs::read_to_string(rc_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let Some((before, after)) = split_around_block(&existing) else {
        return Ok(false);
    };
    let updated = format!("{}{}", before.trim_end_matches('\n'), after);
    let updated = if updated.is_empty() || updated.ends_with('\n') {
        updated
    } else {
        format!("{updated}\n")
    };
    atomic_write(rc_path, &updated)?;
    Ok(true)
}

/// Locate the sentinel block (BEGIN .. END) and return the text
/// before + after it. Returns `None` if the block is absent or
/// malformed (begin without end).
fn split_around_block(content: &str) -> Option<(&str, &str)> {
    let begin = content.find(BLOCK_BEGIN)?;
    let after_begin = &content[begin..];
    let end_rel = after_begin.find(BLOCK_END)?;
    let end_abs = begin + end_rel + BLOCK_END.len();
    // Consume one trailing newline so the rest of the file isn't
    // left with a leading blank line.
    let mut tail = end_abs;
    if content.as_bytes().get(tail) == Some(&b'\n') {
        tail += 1;
    }
    Some((&content[..begin], &content[tail..]))
}

/// Atomic write — temp file + rename, mirrors plist write_plist
/// pattern (D1 §E).
fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("rc");
    let tmp = parent.join(format!(".{filename}.tmp.{}", std::process::id()));
    {
        let mut f = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Shell-quote a path so it survives spaces in the rc file.
/// Single-quote wrap + escape any embedded single quotes.
fn shell_quote(s: &str) -> String {
    if !s.chars().any(|c| c == ' ' || c == '\'' || c == '"' || c == '$') {
        return s.to_string();
    }
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_binary() -> PathBuf {
        PathBuf::from("/usr/local/bin/soma")
    }

    #[test]
    fn shell_detection_from_filename() {
        assert_eq!(Shell::from_rc_filename(Path::new(".zshrc")), Shell::Zsh);
        assert_eq!(Shell::from_rc_filename(Path::new(".bashrc")), Shell::Bash);
        assert_eq!(Shell::from_rc_filename(Path::new("config.fish")), Shell::Fish);
        assert_eq!(Shell::from_rc_filename(Path::new(".profile")), Shell::Bash);
    }

    #[test]
    fn render_hook_includes_binary_and_subcommand() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let hook = shell.render_hook(&fake_binary());
            assert!(hook.contains("/usr/local/bin/soma"), "binary path embedded");
            assert!(hook.contains("ingest --source terminal"), "ingest call embedded");
            assert!(hook.contains("--exit-code"), "exit code threaded through");
        }
    }

    #[test]
    fn inject_into_empty_file_creates_block() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        let changed = inject_block(&rc, &fake_binary()).unwrap();
        assert!(changed);

        let body = fs::read_to_string(&rc).unwrap();
        assert!(body.contains(BLOCK_BEGIN));
        assert!(body.contains(BLOCK_END));
        assert!(body.contains("add-zsh-hook"));
    }

    #[test]
    fn inject_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        inject_block(&rc, &fake_binary()).unwrap();
        let changed = inject_block(&rc, &fake_binary()).unwrap();
        assert!(!changed, "re-running inject is a no-op");
        let body = fs::read_to_string(&rc).unwrap();
        let count = body.matches(BLOCK_BEGIN).count();
        assert_eq!(count, 1, "exactly one block sentinel");
    }

    #[test]
    fn inject_preserves_existing_content_around_block() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        fs::write(&rc, "# my custom config\nexport FOO=bar\n").unwrap();

        inject_block(&rc, &fake_binary()).unwrap();
        let body = fs::read_to_string(&rc).unwrap();
        assert!(body.starts_with("# my custom config\nexport FOO=bar\n"));
        assert!(body.contains(BLOCK_BEGIN));
    }

    #[test]
    fn remove_strips_block_and_keeps_rest() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        fs::write(&rc, "alias ll='ls -la'\n").unwrap();

        inject_block(&rc, &fake_binary()).unwrap();
        let removed = remove_block(&rc).unwrap();
        assert!(removed);

        let body = fs::read_to_string(&rc).unwrap();
        assert!(body.contains("alias ll"), "user lines preserved");
        assert!(!body.contains(BLOCK_BEGIN));
        assert!(!body.contains(BLOCK_END));
    }

    #[test]
    fn remove_on_missing_file_is_graceful() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        let removed = remove_block(&rc).unwrap();
        assert!(!removed);
    }

    #[test]
    fn remove_on_file_without_block_is_noop() {
        let tmp = TempDir::new().unwrap();
        let rc = tmp.path().join(".zshrc");
        fs::write(&rc, "alias ll='ls -la'\n").unwrap();
        let removed = remove_block(&rc).unwrap();
        assert!(!removed);
        let body = fs::read_to_string(&rc).unwrap();
        assert!(body.contains("alias ll"));
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(shell_quote("/usr/local/bin/soma"), "/usr/local/bin/soma");
        assert_eq!(shell_quote("/Users/jy/My Apps/soma"), "'/Users/jy/My Apps/soma'");
        assert_eq!(shell_quote("/tmp/it's/soma"), r"'/tmp/it'\''s/soma'");
    }

    #[test]
    fn default_rc_paths_covers_three_shells() {
        let home = Path::new("/home/u");
        let paths = default_rc_paths(home);
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p.ends_with(".bashrc")));
        assert!(paths.iter().any(|p| p.ends_with(".zshrc")));
        assert!(paths.iter().any(|p| p.ends_with("config.fish")));
    }
}
