#!/usr/bin/env bash
# real-cli-dogfood-probe - opt-in real Codex/Claude CLI MCP capture probe.
#
# This script intentionally does not bypass Codex/Claude permissions. It tries a
# bounded non-interactive MCP `soma_capture_turn` call from the real CLI host and
# reports whether the client actually wrote a SOMA episode, whether the host
# blocked on approval/auth, or whether the tool was not visible.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOMA_BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CODEX_BIN="${CODEX_BIN:-codex}"
CLAUDE_CODE_BIN="${CLAUDE_CODE_BIN:-claude}"
CLIENT="all"
PROJECT="${SOMA_PROJECT:-$(basename "${PWD:-SOMA}")}"
JSON_OUT=""
NO_JSON_OUT=false

usage() {
    cat <<'EOF'
Usage: tools/real-cli-dogfood-probe.sh [--client codex-cli|claude-code|all] [--project NAME] [--json-out PATH] [--no-json-out]

Runs a bounded real CLI dogfood probe:
  - Codex CLI: launches `tools/soma-codex-cli.sh ... exec` and asks the real host
    to call SOMA MCP `soma_capture_turn`.
  - Claude Code CLI: launches `tools/soma-claude-code-cli.sh --print` with only
    `mcp__soma__soma_capture_turn` allowed.

The probe records no client-binding proof rows and never promotes cloud drafts.
If the client refuses the write tool because approval or auth is missing, that
status is reported instead of being treated as success.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --client)
            CLIENT="${2:-}"
            shift 2
            ;;
        --project)
            PROJECT="${2:-}"
            shift 2
            ;;
        --json-out)
            JSON_OUT="${2:-}"
            shift 2
            ;;
        --no-json-out)
            JSON_OUT=""
            NO_JSON_OUT=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$CLIENT" in
    all|codex-cli|claude-code) ;;
    *)
        echo "error: --client must be codex-cli, claude-code, or all" >&2
        exit 2
        ;;
esac

if [[ "$NO_JSON_OUT" != "true" && -z "$JSON_OUT" && -n "${HOME:-}" ]]; then
    JSON_OUT="$HOME/.soma/reports/real-cli-dogfood-latest.json"
fi

if [[ ! -x "$SOMA_BIN" ]]; then
    if [[ -x "$HOME/.cargo/bin/soma" ]]; then
        SOMA_BIN="$HOME/.cargo/bin/soma"
    else
        (cd "$ROOT" && cargo build -p soma >/dev/null)
        SOMA_BIN="$ROOT/target/debug/soma"
    fi
fi

RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/soma-real-cli-dogfood.XXXXXX")"
EVENTS_JSONL="$RUN_DIR/events.jsonl"
: > "$EVENTS_JSONL"

now_ns() {
    date +%s%N
}

json_append() {
    python3 - "$EVENTS_JSONL" "$@" <<'PY'
import json
import sys

path = sys.argv[1]
pairs = sys.argv[2:]
row = {}
for pair in pairs:
    key, value = pair.split("=", 1)
    if value == "true":
        row[key] = True
    elif value == "false":
        row[key] = False
    elif value.isdigit():
        row[key] = int(value)
    else:
        row[key] = value
with open(path, "a", encoding="utf-8") as f:
    f.write(json.dumps(row, separators=(",", ":")) + "\n")
PY
}

recall_has_marker() {
    local project="$1"
    local session="$2"
    local marker="$3"
    local recall_json="$RUN_DIR/recall-${session}.json"
    "$SOMA_BIN" recall \
        --project "$project" \
        --session-id "$session" \
        --query "$marker" \
        --limit 5 \
        --format json >"$recall_json" 2>/dev/null || return 1
    python3 - "$recall_json" "$marker" <<'PY'
import json
import sys

path, marker = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
for hit in data.get("hits", []):
    preview = str(hit.get("preview") or "")
    if marker in preview:
        raise SystemExit(0)
raise SystemExit(1)
PY
}

detect_codex_status() {
    local exit_code="$1"
    local jsonl="$2"
    local err="$3"
    if grep -q '"status":"completed"' "$jsonl" && ! grep -q '"error":' "$jsonl"; then
        printf 'mcp_call_completed'
    elif grep -qi 'user cancelled MCP tool call' "$jsonl" "$err"; then
        printf 'mcp_write_approval_required'
    elif grep -qi 'failed to initialize in-process app-server client: Operation not permitted\|Operation not permitted.*app-server\|failed to open state db.*unable to open database file' "$jsonl" "$err"; then
        printf 'host_permission_blocked'
    elif grep -qi 'No such file or directory\|command not found' "$err"; then
        printf 'runtime_missing'
    elif [[ "$exit_code" != "0" ]]; then
        printf 'cli_invocation_failed'
    else
        printf 'mcp_capture_not_observed'
    fi
}

detect_claude_status() {
    local exit_code="$1"
    local jsonl="$2"
    local err="$3"
    if grep -q 'SOMA_CLAUDE_CODE_DOGFOOD_DONE' "$jsonl" && ! grep -qi '"is_error":true' "$jsonl"; then
        printf 'mcp_call_completed'
    elif grep -qi 'oauth_org_not_allowed\|disabled Claude subscription access\|Not logged in\|Please run /login\|authentication_failed' "$jsonl" "$err"; then
        printf 'auth_blocked'
    elif grep -qi 'mcp__soma__soma_capture_turn' "$jsonl"; then
        printf 'mcp_tool_visible_but_not_executed'
    elif grep -qi 'No such file or directory\|command not found' "$err"; then
        printf 'runtime_missing'
    elif [[ "$exit_code" != "0" ]]; then
        printf 'cli_invocation_failed'
    else
        printf 'mcp_capture_not_observed'
    fi
}

run_codex_probe() {
    local stamp marker session work jsonl err last status observed
    stamp="$(now_ns)"
    marker="SOMA_CODEX_CLI_REAL_DOGFOOD_${stamp}"
    session="codex-cli-real-dogfood-${stamp}"
    work="$RUN_DIR/codex-work"
    jsonl="$RUN_DIR/codex-cli.jsonl"
    err="$RUN_DIR/codex-cli.err"
    last="$RUN_DIR/codex-cli-last.txt"
    mkdir -p "$work"

    set +e
    CODEX_BIN="$CODEX_BIN" SOMA_BIN="$SOMA_BIN" "$ROOT/tools/soma-codex-cli.sh" \
        --ask-for-approval never \
        exec \
        --ephemeral \
        --skip-git-repo-check \
        -C "$work" \
        --json \
        --output-last-message "$last" \
        "This is a SOMA real Codex CLI dogfood probe. Do not edit files. Do not run shell commands. Use the MCP tool from server 'soma' named 'soma_capture_turn' exactly once with arguments: source='codex-cli', project='$PROJECT', session_id='$session', prompt_text='$marker prompt', response_text='$marker response'. After the tool succeeds, reply exactly SOMA_CODEX_CLI_DOGFOOD_DONE. If the tool is unavailable or blocked, reply SOMA_CODEX_CLI_DOGFOOD_MCP_UNAVAILABLE with one short reason." \
        >"$jsonl" 2>"$err"
    local exit_code=$?
    set -e

    status="$(detect_codex_status "$exit_code" "$jsonl" "$err")"
    observed=false
    if recall_has_marker "$PROJECT" "$session" "$marker"; then
        status="capture_observed"
        observed=true
    fi
    json_append \
        client=codex-cli \
        status="$status" \
        exit_code="$exit_code" \
        observed_local_capture="$observed" \
        project="$PROJECT" \
        session_id="$session" \
        marker="$marker" \
        jsonl_path="$jsonl" \
        stderr_path="$err" \
        last_message_path="$last" \
        next_action="$(codex_next_action "$status")" \
        trust_boundary="real_cli_dogfood_probe_is_observational: uses the real Codex CLI host without bypassing permissions; records no client-binding proof row, creates no verification event, installs no hook, and only a successful client MCP soma_capture_turn call may create an ordinary local capture episode"
}

codex_next_action() {
    case "$1" in
        capture_observed) printf 'rerun soma clients --project %s to see codex-cli observed_local_capture' "$PROJECT" ;;
        mcp_write_approval_required) printf 'open an interactive Codex CLI session and explicitly approve the SOMA soma_capture_turn MCP write, or use a user-approved dedicated dogfood run' ;;
        host_permission_blocked) printf 'rerun this probe from a normal user terminal with Codex CLI app-server/state access; sandboxed Codex sessions cannot prove real Codex CLI capture' ;;
        runtime_missing) printf 'install or expose codex on PATH, then rerun this probe' ;;
        *) printf 'inspect the jsonl/stderr paths from this report and rerun after fixing Codex CLI MCP access' ;;
    esac
}

run_claude_probe() {
    local stamp marker session config jsonl err status observed
    stamp="$(now_ns)"
    marker="SOMA_CLAUDE_CODE_REAL_DOGFOOD_${stamp}"
    session="claude-code-real-dogfood-${stamp}"
    config="$RUN_DIR/claude-mcp.json"
    jsonl="$RUN_DIR/claude-code.jsonl"
    err="$RUN_DIR/claude-code.err"

    "$SOMA_BIN" mcp-config --client claude-code --command "$SOMA_BIN" >"$config"
    set +e
    printf '%s\n' \
        "This is a SOMA real Claude Code CLI dogfood probe. Do not edit files. Do not run shell commands. Use only the MCP tool mcp__soma__soma_capture_turn exactly once with arguments: source='claude-code', project='$PROJECT', session_id='$session', prompt_text='$marker prompt', response_text='$marker response'. After the tool succeeds, reply exactly SOMA_CLAUDE_CODE_DOGFOOD_DONE. If the tool is unavailable or blocked, reply SOMA_CLAUDE_CODE_DOGFOOD_MCP_UNAVAILABLE with one short reason." \
        | CLAUDE_CODE_BIN="$CLAUDE_CODE_BIN" SOMA_BIN="$SOMA_BIN" "$ROOT/tools/soma-claude-code-cli.sh" \
            --print \
            --verbose \
            --output-format stream-json \
            --mcp-config "$config" \
            --allowedTools mcp__soma__soma_capture_turn \
            >"$jsonl" 2>"$err"
    local exit_code=$?
    set -e

    status="$(detect_claude_status "$exit_code" "$jsonl" "$err")"
    observed=false
    if recall_has_marker "$PROJECT" "$session" "$marker"; then
        status="capture_observed"
        observed=true
    fi
    json_append \
        client=claude-code \
        status="$status" \
        exit_code="$exit_code" \
        observed_local_capture="$observed" \
        project="$PROJECT" \
        session_id="$session" \
        marker="$marker" \
        jsonl_path="$jsonl" \
        stderr_path="$err" \
        next_action="$(claude_next_action "$status")" \
        trust_boundary="real_cli_dogfood_probe_is_observational: uses the real Claude Code CLI host with only mcp__soma__soma_capture_turn allowed; records no client-binding proof row, creates no verification event, installs no hook, and only a successful client MCP soma_capture_turn call may create an ordinary local capture episode"
}

claude_next_action() {
    case "$1" in
        capture_observed) printf 'rerun soma clients --project %s to see claude-code observed_local_capture' "$PROJECT" ;;
        auth_blocked) printf 'configure an Anthropic API key or ask the organization admin to enable Claude Code subscription access, then rerun this probe' ;;
        runtime_missing) printf 'install or expose claude on PATH, then rerun this probe' ;;
        *) printf 'inspect the jsonl/stderr paths from this report and rerun after fixing Claude Code MCP access' ;;
    esac
}

case "$CLIENT" in
    all)
        run_codex_probe
        run_claude_probe
        ;;
    codex-cli)
        run_codex_probe
        ;;
    claude-code)
        run_claude_probe
        ;;
esac

python3 - "$EVENTS_JSONL" "$JSON_OUT" "$RUN_DIR" <<'PY'
import json
import sys
import time
from pathlib import Path

events_path, json_out, artifact_dir = sys.argv[1:]
attempts = []
with open(events_path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if line:
            attempts.append(json.loads(line))

observed = [item["client"] for item in attempts if item.get("observed_local_capture")]
blocked = [
    item["client"]
    for item in attempts
    if item.get("status") in {"mcp_write_approval_required", "auth_blocked"}
    or item.get("status") == "host_permission_blocked"
]
failed = [
    item["client"]
    for item in attempts
    if item.get("status")
    not in {
        "capture_observed",
        "mcp_write_approval_required",
        "auth_blocked",
        "host_permission_blocked",
    }
]
report = {
    "schema": "soma.real_cli_dogfood_probe.v1",
    "source": "tools/real-cli-dogfood-probe.sh",
    "generated_at_unix_ms": int(time.time() * 1000),
    "status": "pass" if len(observed) == len(attempts) else ("blocked" if blocked and not failed else "fail"),
    "artifact_dir": artifact_dir,
    "observed_clients": observed,
    "blocked_clients": blocked,
    "failed_clients": failed,
    "attempts": attempts,
    "trust_boundary": (
        "real_cli_dogfood_probe_report_is_observational: reports real Codex/Claude "
        "CLI MCP capture attempts; it records no client-binding proof rows, creates "
        "no verification events, installs no hooks, and cannot promote cloud drafts"
    ),
}
text = json.dumps(report, indent=2) + "\n"
if json_out:
    Path(json_out).parent.mkdir(parents=True, exist_ok=True)
    Path(json_out).write_text(text, encoding="utf-8")
print(text, end="")
PY
