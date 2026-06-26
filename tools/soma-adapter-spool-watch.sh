#!/usr/bin/env bash
# soma-adapter-spool-watch - reference watcher for normalized editor JSONL events.
#
# Usage:
#   SOMA_ADAPTER_SPOOL_JSONL="$HOME/.soma/adapter/events.jsonl" \
#     tools/soma-adapter-spool-watch.sh
#
# Event shape, one JSON object per line:
#   {"kind":"turn","payload":{... adapter-capture payload ...}}
#   {"kind":"cloud_output","payload":{... adapter-cloud-output payload ...}}
#
# The watcher is intentionally format-stable and editor-agnostic. Cursor,
# Continue, or a user wrapper only needs to append normalized JSONL events; SOMA
# owns checkpointing and forwards each event through the same trust-boundary
# capture paths as the CLI/MCP tools.

set -u

LOG="$HOME/.soma/log/adapter-spool-watch.log"
mkdir -p "$(dirname "$LOG")"

log() { echo "[$(date '+%H:%M:%S')] $*" >> "$LOG"; }

BIN="${SOMA_BIN:-}"
if [[ -z "$BIN" ]]; then
    if command -v soma >/dev/null 2>&1; then
        BIN="$(command -v soma)"
    else
        BIN="$HOME/.cargo/bin/soma"
    fi
fi
if [[ ! -x "$BIN" ]]; then
    log "soma binary not executable at '$BIN'"
    exit 1
fi

SPOOL="${SOMA_ADAPTER_SPOOL_JSONL:-$HOME/.soma/adapter/events.jsonl}"
if [[ -n "${SOMA_ADAPTER_SPOOL_CHECKPOINT:-}" ]]; then
    CHECKPOINT="$SOMA_ADAPTER_SPOOL_CHECKPOINT"
else
    CHECKPOINT="${SPOOL%.*}.offset"
fi
POLL_SECONDS="${SOMA_ADAPTER_SPOOL_POLL_SECONDS:-1}"
STRICT="${SOMA_ADAPTER_CAPTURE_STRICT:-0}"

mkdir -p "$(dirname "$SPOOL")"
touch "$SPOOL"

while true; do
    OUT=$("$BIN" adapter-spool --jsonl "$SPOOL" --checkpoint "$CHECKPOINT" 2>&1)
    CODE=$?
    if [[ "$CODE" -eq 0 ]]; then
        log "adapter-spool drained=$OUT"
    else
        log "adapter-spool failed exit=$CODE output=$OUT"
        if [[ "$STRICT" == "1" ]]; then
            exit "$CODE"
        fi
    fi
    sleep "$POLL_SECONDS"
done
