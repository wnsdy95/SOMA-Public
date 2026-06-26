#!/usr/bin/env bash
# soma-adapter-capture - normalized AI turn capture entrypoint for editor
# adapters.
#
# Input: one JSON object on stdin using the `soma ingest --json` schema:
#   {
#     "source": "cursor" | "continue" | "claude-code" | "codex-cli" | "...",
#     "session_id": "...",
#     "prompt_text": "...",
#     "response_text": "...",
#     "project": "...",
#     "cwd": "...",
#     "git_branch": "..."
#   }
#
# The script forwards to `soma adapter-capture`, which enriches missing
# cwd/project/git_branch and writes through the normal ingest pipeline. By
# default failures are advisory and the script exits 0 so a client hook cannot
# block the editor turn. Set SOMA_ADAPTER_CAPTURE_STRICT=1 in smoke tests to
# make failures non-zero.

set -u

STRICT="${SOMA_ADAPTER_CAPTURE_STRICT:-0}"
LOG="$HOME/.soma/log/adapter-capture.log"
mkdir -p "$(dirname "$LOG")"

log() { echo "[$(date '+%H:%M:%S')] $*" >> "$LOG"; }

finish_error() {
    log "$*"
    if [[ "$STRICT" == "1" ]]; then
        exit 1
    fi
    exit 0
}

PAYLOAD=$(cat 2>/dev/null || true)
if [[ -z "$PAYLOAD" ]]; then
    finish_error "no stdin payload"
fi

BIN="${SOMA_BIN:-}"
if [[ -z "$BIN" ]]; then
    if command -v soma >/dev/null 2>&1; then
        BIN="$(command -v soma)"
    else
        BIN="$HOME/.cargo/bin/soma"
    fi
fi
if [[ ! -x "$BIN" ]]; then
    finish_error "soma binary not executable at '$BIN'"
fi

CMD=("$BIN" adapter-capture --json -)
if [[ -n "${SOMA_ADAPTER_SOURCE:-${SOMA_CLIENT:-}}" ]]; then
    CMD+=(--source "${SOMA_ADAPTER_SOURCE:-${SOMA_CLIENT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_PROJECT:-${SOMA_PROJECT:-}}" ]]; then
    CMD+=(--project "${SOMA_ADAPTER_PROJECT:-${SOMA_PROJECT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_SESSION_ID:-${SOMA_SESSION_ID:-}}" ]]; then
    CMD+=(--session-id "${SOMA_ADAPTER_SESSION_ID:-${SOMA_SESSION_ID:-}}")
fi

OUT=$(printf '%s\n' "$PAYLOAD" | "${CMD[@]}" 2>&1)
CODE=$?
if [[ "$CODE" -eq 0 ]]; then
    log "adapter-capture ingested episode=$OUT"
    exit 0
fi

finish_error "adapter-capture failed exit=$CODE output=$OUT"
