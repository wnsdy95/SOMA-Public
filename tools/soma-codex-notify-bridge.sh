#!/usr/bin/env bash
# soma-codex-notify-bridge - Codex app notify hook -> SOMA lifecycle heartbeat.
#
# This wrapper is designed for Codex app's `notify` command array. It can chain
# an existing notify command while also emitting a bounded, proof-gated SOMA
# lifecycle heartbeat into the adapter spool.
#
# Example:
#   notify = [
#     "/path/to/SOMA/tools/soma-codex-notify-bridge.sh",
#     "--chain",
#     "/existing/CodexNotifyClient",
#     "turn-ended"
#   ]
#
# The emitted event proves nothing by itself. It becomes app-hook evidence only
# after `soma adapter-binding-proof --proof-level observed_app_hook` verifies the
# installed config, matching binding nonce/event source, temporal ordering, and
# explicit operator confirmation.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ -n "${SOMA_BIN:-}" ]]; then
    BIN="$SOMA_BIN"
elif [[ -x "$ROOT/target/debug/soma" ]]; then
    BIN="$ROOT/target/debug/soma"
else
    BIN="soma"
fi

CONFIG="${SOMA_CODEX_NOTIFY_INSTALLED_CONFIG:-$HOME/.codex/soma-installed-binding.json}"
LOG="${SOMA_CODEX_NOTIFY_LOG:-$HOME/.soma/log/codex-notify-bridge.log}"
EVENT="${SOMA_CODEX_NOTIFY_LIFECYCLE_EVENT:-turn_completed}"
SPOOL_OVERRIDE="${SOMA_CODEX_NOTIFY_JSONL:-}"
CHAIN_TIMEOUT_SECONDS="${SOMA_CODEX_NOTIFY_CHAIN_TIMEOUT_SECONDS:-5}"
CAPTURE_TIMEOUT_SECONDS="${SOMA_CODEX_NOTIFY_CAPTURE_TIMEOUT_SECONDS:-5}"
ARG_PREVIEW_BYTES="${SOMA_CODEX_NOTIFY_ARG_PREVIEW_BYTES:-2048}"
ARG_PREVIEW_MAX="${SOMA_CODEX_NOTIFY_ARG_PREVIEW_MAX:-12}"
LIFECYCLE_SCRIPT="${SOMA_CODEX_NOTIFY_LIFECYCLE_SCRIPT:-$ROOT/tools/soma-adapter-lifecycle.sh}"
LOCK_TTL_SECONDS="${SOMA_CODEX_NOTIFY_LOCK_TTL_SECONDS:-30}"
LOCK_DIR="${SOMA_CODEX_NOTIFY_LOCK_DIR:-$HOME/.soma/run/codex-notify-bridge.lock}"
ORIGINAL_ARGS=("$@")
CHAIN=()

mkdir -p "$(dirname "$LOG")" 2>/dev/null || true

log() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG" 2>/dev/null || true
}

stat_mtime_seconds() {
    stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null || date +%s
}

release_lock() {
    if [[ "${LOCK_HELD:-0}" == "1" ]]; then
        rm -rf "$LOCK_DIR" 2>/dev/null || true
    fi
}

acquire_capture_lock() {
    if [[ ! "$LOCK_TTL_SECONDS" =~ ^[0-9]+$ || ! "$LOCK_TTL_SECONDS" -gt 0 ]]; then
        return 0
    fi

    mkdir -p "$(dirname "$LOCK_DIR")" 2>/dev/null || true
    if mkdir "$LOCK_DIR" 2>/dev/null; then
        LOCK_HELD=1
        printf '%s\n' "$$" >"$LOCK_DIR/pid" 2>/dev/null || true
        trap release_lock EXIT
        return 0
    fi

    now="$(date +%s)"
    mtime="$(stat_mtime_seconds "$LOCK_DIR")"
    age=$((now - mtime))
    if [[ "$age" -gt "$LOCK_TTL_SECONDS" ]]; then
        rm -rf "$LOCK_DIR" 2>/dev/null || true
        if mkdir "$LOCK_DIR" 2>/dev/null; then
            LOCK_HELD=1
            printf '%s\n' "$$" >"$LOCK_DIR/pid" 2>/dev/null || true
            trap release_lock EXIT
            log "recovered_stale_lock path=$LOCK_DIR age_seconds=$age ttl_seconds=$LOCK_TTL_SECONDS"
            return 0
        fi
    fi

    log "skip_capture_lock_busy path=$LOCK_DIR age_seconds=$age ttl_seconds=$LOCK_TTL_SECONDS"
    return 1
}

if [[ "${1:-}" == "--chain" || "${1:-}" == "--" ]]; then
    shift
    CHAIN=("$@")
fi

if [[ ${#CHAIN[@]} -gt 0 ]]; then
    CHAIN_RESULT="$(
        python3 - "$CHAIN_TIMEOUT_SECONDS" "${CHAIN[@]}" <<'PY'
import os
import signal
import subprocess
import sys

timeout_raw = sys.argv[1]
command = sys.argv[2:]

try:
    timeout_seconds = float(timeout_raw)
except ValueError:
    timeout_seconds = 3.0

if timeout_seconds < 0:
    timeout_seconds = 3.0

if timeout_seconds == 0:
    print("skipped")
    sys.exit(0)

try:
    child = subprocess.Popen(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        preexec_fn=os.setsid,
    )
except Exception as exc:
    print(f"spawn_failed:{type(exc).__name__}:{exc}")
    sys.exit(125)

try:
    status = child.wait(timeout=timeout_seconds)
except subprocess.TimeoutExpired:
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        child.wait(timeout=1)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
    print("timeout")
    sys.exit(124)

print(f"exit:{status}")
sys.exit(0 if status == 0 else 1)
PY
    )"
    chain_status=$?
    case "$chain_status" in
        0)
            log "chain_ok result=$CHAIN_RESULT timeout_seconds=$CHAIN_TIMEOUT_SECONDS command=${CHAIN[0]}"
            ;;
        124)
            log "chain_timeout timeout_seconds=$CHAIN_TIMEOUT_SECONDS command=${CHAIN[0]}"
            ;;
        *)
            log "chain_failed status=$chain_status result=$CHAIN_RESULT command=${CHAIN[0]}"
            ;;
    esac
fi

if ! acquire_capture_lock; then
    exit 0
fi

if [[ ! -f "$CONFIG" ]]; then
    log "skip_no_installed_config path=$CONFIG"
    exit 0
fi

CONFIG_VALUES="$(
    python3 - "$CONFIG" "$HOME" "$SPOOL_OVERRIDE" <<'PY'
import json
import os
import sys

config_path, home, spool_override = sys.argv[1:4]

with open(config_path, "r", encoding="utf-8") as f:
    data = json.load(f)

hook = data.get("lifecycle_hook") or {}
env = hook.get("env") or {}

def expand_runtime(value):
    if not isinstance(value, str):
        return value
    replacements = {
        "$HOME": home,
        "${HOME}": home,
        "$PWD": os.environ.get("PWD", ""),
        "${PWD}": os.environ.get("PWD", ""),
        "$SOMA_PROJECT": os.environ.get("SOMA_PROJECT", ""),
        "${SOMA_PROJECT}": os.environ.get("SOMA_PROJECT", ""),
        "$SOMA_SESSION_ID": os.environ.get("SOMA_SESSION_ID", ""),
        "${SOMA_SESSION_ID}": os.environ.get("SOMA_SESSION_ID", ""),
        "$CODEX_THREAD_ID": os.environ.get("CODEX_THREAD_ID", ""),
        "${CODEX_THREAD_ID}": os.environ.get("CODEX_THREAD_ID", ""),
    }
    expanded = value
    for token, replacement in replacements.items():
        expanded = expanded.replace(token, replacement)
    return expanded

def first(*values, default=""):
    for value in values:
        expanded = expand_runtime(value)
        if isinstance(expanded, str) and expanded.strip():
            return expanded
    return expand_runtime(default)

client = first(
    hook.get("client"),
    data.get("client"),
    env.get("SOMA_ADAPTER_LIFECYCLE_CLIENT"),
    default="codex-app",
)
event_source = first(
    env.get("SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE"),
    hook.get("event_source"),
    default=f"{client}_private_lifecycle_hook",
)
binding_nonce = first(env.get("SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE"), hook.get("binding_nonce"))
jsonl = first(
    spool_override,
    env.get("SOMA_ADAPTER_LIFECYCLE_JSONL"),
    default="$HOME/.soma/adapter/events.jsonl",
)
project = first(
    os.environ.get("SOMA_ADAPTER_LIFECYCLE_PROJECT"),
    os.environ.get("SOMA_PROJECT"),
    env.get("SOMA_ADAPTER_LIFECYCLE_PROJECT"),
    hook.get("project"),
    data.get("project"),
)
session_id = first(
    os.environ.get("SOMA_ADAPTER_LIFECYCLE_SESSION"),
    os.environ.get("SOMA_SESSION_ID"),
    env.get("SOMA_ADAPTER_LIFECYCLE_SESSION"),
    hook.get("session_id"),
    data.get("session_id"),
    os.environ.get("CODEX_THREAD_ID"),
)
cwd = first(
    os.environ.get("SOMA_ADAPTER_LIFECYCLE_CWD"),
    env.get("SOMA_ADAPTER_LIFECYCLE_CWD"),
    hook.get("cwd"),
    data.get("cwd"),
    os.environ.get("PWD"),
)

print(f"CLIENT={client}")
print(f"EVENT_SOURCE={event_source}")
print(f"BINDING_NONCE={binding_nonce}")
print(f"JSONL={jsonl}")
print(f"PROJECT={project}")
print(f"SESSION_ID={session_id}")
print(f"CWD={cwd}")
PY
)"
config_status=$?

if [[ $config_status -ne 0 ]]; then
    log "skip_bad_installed_config status=$config_status path=$CONFIG"
    exit 0
fi

CLIENT="codex-app"
EVENT_SOURCE="codex-app_private_lifecycle_hook"
BINDING_NONCE=""
JSONL="$HOME/.soma/adapter/events.jsonl"
CONFIG_PROJECT=""
CONFIG_SESSION_ID=""
CONFIG_CWD=""
while IFS='=' read -r key value; do
    case "$key" in
        CLIENT) CLIENT="$value" ;;
        EVENT_SOURCE) EVENT_SOURCE="$value" ;;
        BINDING_NONCE) BINDING_NONCE="$value" ;;
        JSONL) JSONL="$value" ;;
        PROJECT) CONFIG_PROJECT="$value" ;;
        SESSION_ID) CONFIG_SESSION_ID="$value" ;;
        CWD) CONFIG_CWD="$value" ;;
    esac
done <<<"$CONFIG_VALUES"

if [[ "$CLIENT" != "codex-app" ]]; then
    log "skip_wrong_client client=$CLIENT path=$CONFIG"
    exit 0
fi

if [[ -z "$BINDING_NONCE" ]]; then
    log "skip_missing_binding_nonce path=$CONFIG"
    exit 0
fi

SESSION_ID="${SOMA_ADAPTER_LIFECYCLE_SESSION:-${SOMA_SESSION_ID:-${CONFIG_SESSION_ID:-${CODEX_THREAD_ID:-codex-app-notify}}}}"
PROJECT="${SOMA_ADAPTER_LIFECYCLE_PROJECT:-${SOMA_PROJECT:-${CONFIG_PROJECT:-$(basename "${PWD:-codex-app}")}}}"
CWD_VALUE="${SOMA_ADAPTER_LIFECYCLE_CWD:-${CONFIG_CWD:-${PWD:-}}}"

PAYLOAD="$(
    python3 - \
        "$CLIENT" \
        "$EVENT" \
        "$SESSION_ID" \
        "$PROJECT" \
        "$CWD_VALUE" \
        "$EVENT_SOURCE" \
        "$BINDING_NONCE" \
        "$CONFIG" \
        "$ARG_PREVIEW_BYTES" \
        "$ARG_PREVIEW_MAX" \
        "${ORIGINAL_ARGS[@]}" <<'PY'
import hashlib
import json
import sys
import time

(
    client,
    event,
    session_id,
    project,
    cwd,
    event_source,
    binding_nonce,
    config_path,
    arg_preview_bytes_raw,
    arg_preview_max_raw,
    *notify_args,
) = sys.argv[1:]

try:
    arg_preview_bytes = int(arg_preview_bytes_raw)
except ValueError:
    arg_preview_bytes = 2048
if arg_preview_bytes < 0:
    arg_preview_bytes = 2048

try:
    arg_preview_max = int(arg_preview_max_raw)
except ValueError:
    arg_preview_max = 12
if arg_preview_max < 0:
    arg_preview_max = 12

def bounded_arg(value):
    raw = value.encode("utf-8", errors="replace")
    sha256 = hashlib.sha256(raw).hexdigest()
    preview_raw = raw[:arg_preview_bytes]
    return {
        "text": preview_raw.decode("utf-8", errors="replace"),
        "bytes": len(raw),
        "truncated": len(raw) > len(preview_raw),
        "sha256": sha256,
    }

bounded_args = [bounded_arg(arg) for arg in notify_args[:arg_preview_max]]
summary = "Codex app notify hook observed: turn-ended"
payload = {
    "event": event,
    "client": client,
    "source": client,
    "thread_id": session_id,
    "session_id": session_id,
    "project": project,
    "cwd": cwd,
    "prompt_text": summary,
    "response_text": summary,
    "hook_adapter": "codex_notify_bridge",
    "event_source": event_source,
    "binding_nonce": binding_nonce,
    "installed_config_path": config_path,
    "notify_arg_count": len(notify_args),
    "notify_args_preview": bounded_args,
    "notify_args_omitted_count": max(0, len(notify_args) - len(bounded_args)),
    "observed_by": "soma-codex-notify-bridge",
    "observed_at_ns": time.time_ns(),
    "trust_boundary": (
        "notify_bridge_heartbeat_is_not_cloud_output_and_not_verification; "
        "observed_app_hook still requires installed-config/event proof plus "
        "operator confirmation"
    ),
}
print(json.dumps(payload, separators=(",", ":")))
PY
)"
payload_status=$?

if [[ $payload_status -ne 0 || -z "${PAYLOAD//[[:space:]]/}" ]]; then
    log "skip_payload_build_failed status=$payload_status"
    exit 0
fi

OUT="$(
    SOMA_CODEX_NOTIFY_PAYLOAD="$PAYLOAD" \
    SOMA_CODEX_NOTIFY_CAPTURE_TIMEOUT_SECONDS="$CAPTURE_TIMEOUT_SECONDS" \
    SOMA_BIN="$BIN" \
    SOMA_ADAPTER_LIFECYCLE_CLIENT="$CLIENT" \
    SOMA_ADAPTER_LIFECYCLE_EVENT="$EVENT" \
    SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE="$EVENT_SOURCE" \
    SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE="$BINDING_NONCE" \
    SOMA_ADAPTER_LIFECYCLE_JSONL="$JSONL" \
    SOMA_ADAPTER_LIFECYCLE_PROJECT="$PROJECT" \
    SOMA_ADAPTER_LIFECYCLE_SESSION="$SESSION_ID" \
    SOMA_ADAPTER_LIFECYCLE_CWD="$CWD_VALUE" \
    python3 - "$LIFECYCLE_SCRIPT" <<'PY'
import os
import signal
import subprocess
import sys

script = sys.argv[1]
payload = os.environ.get("SOMA_CODEX_NOTIFY_PAYLOAD", "")
timeout_raw = os.environ.get("SOMA_CODEX_NOTIFY_CAPTURE_TIMEOUT_SECONDS", "5")

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
    child = subprocess.Popen(
        [script],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=os.environ.copy(),
        start_new_session=True,
    )
except Exception as exc:
    print(f"spawn_failed:{type(exc).__name__}:{exc}")
    sys.exit(125)

try:
    output, _ = child.communicate(payload + "\n", timeout=timeout_seconds)
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
)"
status=$?

if [[ $status -eq 0 ]]; then
    log "soma_lifecycle_ok jsonl=$JSONL"
elif [[ $status -eq 124 ]]; then
    log "soma_lifecycle_timeout timeout_seconds=$CAPTURE_TIMEOUT_SECONDS output=$OUT"
else
    log "soma_lifecycle_failed status=$status output=$OUT"
fi

exit 0
