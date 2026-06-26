#!/usr/bin/env bash
# soma-adapter-lifecycle - normalize raw editor lifecycle hooks to SOMA spool.
#
# Input: one raw lifecycle JSON object on stdin. The wrapper forwards to
# `soma adapter-lifecycle`, which emits/appends a normalized `{kind,payload}`
# event for the checkpointed `soma adapter-spool` watcher. It does not ingest
# directly and does not promote cloud output.
#
# Useful environment variables:
#   SOMA_BIN                         path to soma binary
#   SOMA_ADAPTER_LIFECYCLE_CLIENT    codex-app, cursor, continue, claude-code, ...
#   SOMA_ADAPTER_LIFECYCLE_EVENT     turn_completed, assistant_response, auto
#   SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE private app-hook source marker
#   SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE per-install binding nonce marker
#   SOMA_ADAPTER_LIFECYCLE_HOOK_ADAPTER  optional hook adapter marker
#   SOMA_ADAPTER_LIFECYCLE_JSONL     spool path
#   SOMA_ADAPTER_LIFECYCLE_PROJECT   project default
#   SOMA_ADAPTER_LIFECYCLE_SESSION   session_id default
#   SOMA_ADAPTER_LIFECYCLE_CWD       cwd default
#   SOMA_ADAPTER_LIFECYCLE_BRANCH    git_branch default
#   SOMA_ADAPTER_LIFECYCLE_TIMEOUT_SECONDS  max wrapper child runtime

set -euo pipefail

BIN="${SOMA_BIN:-soma}"
SPOOL="${SOMA_ADAPTER_LIFECYCLE_JSONL:-${SOMA_ADAPTER_SPOOL_JSONL:-$HOME/.soma/adapter/events.jsonl}}"
LOG="$HOME/.soma/log/adapter-lifecycle.log"
LIFECYCLE_TIMEOUT_SECONDS="${SOMA_ADAPTER_LIFECYCLE_TIMEOUT_SECONDS:-5}"
mkdir -p "$(dirname "$LOG")"

log() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG"
}

finish_error() {
    log "$1"
    exit 0
}

PAYLOAD="$(cat || true)"
if [[ -z "${PAYLOAD//[[:space:]]/}" ]]; then
    finish_error "empty lifecycle payload"
fi

ARGS=(adapter-lifecycle --json - --jsonl "$SPOOL" --format report)

if [[ -n "${SOMA_ADAPTER_LIFECYCLE_CLIENT:-${SOMA_CLIENT:-}}" ]]; then
    ARGS+=(--client "${SOMA_ADAPTER_LIFECYCLE_CLIENT:-${SOMA_CLIENT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_EVENT:-}" ]]; then
    ARGS+=(--event "$SOMA_ADAPTER_LIFECYCLE_EVENT")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE:-}" ]]; then
    ARGS+=(--event-source "$SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE:-}" ]]; then
    ARGS+=(--binding-nonce "$SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_HOOK_ADAPTER:-}" ]]; then
    ARGS+=(--hook-adapter "$SOMA_ADAPTER_LIFECYCLE_HOOK_ADAPTER")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_PROJECT:-${SOMA_PROJECT:-}}" ]]; then
    ARGS+=(--project "${SOMA_ADAPTER_LIFECYCLE_PROJECT:-${SOMA_PROJECT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_SESSION:-${SOMA_SESSION_ID:-}}" ]]; then
    ARGS+=(--session-id "${SOMA_ADAPTER_LIFECYCLE_SESSION:-${SOMA_SESSION_ID:-}}")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_CWD:-}" ]]; then
    ARGS+=(--cwd "$SOMA_ADAPTER_LIFECYCLE_CWD")
fi
if [[ -n "${SOMA_ADAPTER_LIFECYCLE_BRANCH:-}" ]]; then
    ARGS+=(--git-branch "$SOMA_ADAPTER_LIFECYCLE_BRANCH")
fi
if [[ "${SOMA_ADAPTER_LIFECYCLE_FSYNC:-0}" == "1" ]]; then
    ARGS+=(--fsync)
fi

PAYLOAD_FILE="$(mktemp "${TMPDIR:-/tmp}/soma-adapter-lifecycle.XXXXXX")" || finish_error "mktemp failed for lifecycle payload"
cleanup_payload() {
    rm -f "$PAYLOAD_FILE" 2>/dev/null || true
}
trap cleanup_payload EXIT
printf '%s\n' "$PAYLOAD" >"$PAYLOAD_FILE" || finish_error "write lifecycle payload temp file failed"

if OUT=$(python3 - "$LIFECYCLE_TIMEOUT_SECONDS" "$PAYLOAD_FILE" "$BIN" "${ARGS[@]}" <<'PY' 2>&1
import os
import signal
import subprocess
import sys

timeout_raw = sys.argv[1]
payload_path = sys.argv[2]
command = sys.argv[3:]

try:
    timeout_seconds = float(timeout_raw)
except ValueError:
    timeout_seconds = 5.0

if timeout_seconds < 0:
    timeout_seconds = 5.0

if timeout_seconds == 0:
    print("skipped")
    sys.exit(0)

try:
    with open(payload_path, "r", encoding="utf-8") as f:
        payload = f.read()
except Exception as exc:
    print(f"payload_read_failed:{type(exc).__name__}:{exc}")
    sys.exit(125)

try:
    child = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )
except Exception as exc:
    print(f"spawn_failed:{type(exc).__name__}:{exc}")
    sys.exit(125)

try:
    output, _ = child.communicate(payload, timeout=timeout_seconds)
except subprocess.TimeoutExpired:
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        output, _ = child.communicate(timeout=1)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        output, _ = child.communicate()
    if output:
        print(output, end="")
    print("timeout")
    sys.exit(124)

if output:
    print(output, end="")
sys.exit(child.returncode)
PY
); then
    log "adapter-lifecycle normalized=$OUT"
    exit 0
else
    CODE=$?
fi

if [[ "$CODE" -eq 124 ]]; then
    finish_error "adapter-lifecycle timeout timeout_seconds=$LIFECYCLE_TIMEOUT_SECONDS output=$OUT"
fi

finish_error "adapter-lifecycle failed exit=$CODE output=$OUT"
