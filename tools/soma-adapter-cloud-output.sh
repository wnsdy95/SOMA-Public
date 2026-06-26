#!/usr/bin/env bash
# soma-adapter-cloud-output - normalized cloud response capture entrypoint for
# editor adapters and watcher wrappers.
#
# Input: one JSON object on stdin matching `soma adapter-cloud-output --json -`:
#   {
#     "task_frame_id": 123,
#     "handoff_id": "soma-handoff:v1:<copy from soma-cloud-context>",
#     "protocol_contract": "soma-cloud-context",
#     "artifact_version": 1,
#     "task_frame_query": "optional when task_frame_id is present",
#     "project": "optional project scope",
#     "session_id": "optional client session",
#     "output_text": "...",
#     "decision": "accept",
#     "extracted_claims": [{"text": "..."}],
#     "proposal_reason": "..."
#   }
#
# The script forwards to `soma adapter-cloud-output`, which stores cloud model
# claims as untrusted `cloud_draft` records tied to the shaping TaskFrame. If
# `task_frame_id` is omitted, SOMA builds a TaskFrame from `task_frame_query`
# and local evidence before capture. It optionally queues a review proposal,
# but never verifies or promotes the claims.
# When the response was produced from a `soma-cloud-context` artifact, adapters
# should echo `handoff_id`, `protocol_contract`, and `artifact_version` from
# that artifact; mismatches are rejected before claim capture. The echo binds
# the response to the context artifact, but is not verification evidence.
# By default failures are advisory and the script exits 0 so a client hook
# cannot block the editor turn. Set SOMA_ADAPTER_CAPTURE_STRICT=1 in smoke
# tests to make failures non-zero.

set -u

STRICT="${SOMA_ADAPTER_CAPTURE_STRICT:-0}"
LOG="$HOME/.soma/log/adapter-cloud-output.log"
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

OUT=$(printf '%s\n' "$PAYLOAD" | "$BIN" adapter-cloud-output --json - 2>&1)
CODE=$?
if [[ "$CODE" -eq 0 ]]; then
    log "adapter-cloud-output captured=$OUT"
    exit 0
fi

finish_error "adapter-cloud-output failed exit=$CODE output=$OUT"
