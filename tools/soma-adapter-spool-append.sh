#!/usr/bin/env bash
# soma-adapter-spool-append - append one normalized editor event to SOMA JSONL.
#
# Reads one payload JSON object from stdin and wraps it as a checkpointed spool
# event for `soma adapter-spool`. This is a reference writer contract for
# editor wrappers; it is not a private Codex app/Cursor/Continue lifecycle hook.
#
# Example turn event:
#   printf '{"prompt_text":"...","response_text":"..."}' \
#     SOMA_ADAPTER_SPOOL_KIND=turn SOMA_ADAPTER_CAPTURE_SOURCE=cursor \
#     tools/soma-adapter-spool-append.sh
#
# Example cloud output event:
#   printf '{"task_frame_query":"...","output_text":"..."}' \
#     SOMA_ADAPTER_SPOOL_KIND=cloud_output \
#     tools/soma-adapter-spool-append.sh

set -euo pipefail

BIN="${SOMA_BIN:-soma}"
SPOOL="${SOMA_ADAPTER_SPOOL_JSONL:-$HOME/.soma/adapter/events.jsonl}"
KIND="${SOMA_ADAPTER_SPOOL_KIND:-turn}"

ARGS=(adapter-spool-append --jsonl "$SPOOL" --kind "$KIND" --json -)

if [[ -n "${SOMA_ADAPTER_CAPTURE_SOURCE:-${SOMA_CLIENT:-}}" ]]; then
    ARGS+=(--source "${SOMA_ADAPTER_CAPTURE_SOURCE:-${SOMA_CLIENT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_PROJECT:-${SOMA_PROJECT:-}}" ]]; then
    ARGS+=(--project "${SOMA_ADAPTER_PROJECT:-${SOMA_PROJECT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_SESSION_ID:-${SOMA_SESSION_ID:-}}" ]]; then
    ARGS+=(--session-id "${SOMA_ADAPTER_SESSION_ID:-${SOMA_SESSION_ID:-}}")
fi
if [[ -n "${SOMA_ADAPTER_CWD:-}" ]]; then
    ARGS+=(--cwd "$SOMA_ADAPTER_CWD")
fi
if [[ -n "${SOMA_ADAPTER_GIT_BRANCH:-}" ]]; then
    ARGS+=(--git-branch "$SOMA_ADAPTER_GIT_BRANCH")
fi
if [[ -n "${SOMA_ADAPTER_CLIENT:-${SOMA_CLIENT:-}}" ]]; then
    ARGS+=(--client "${SOMA_ADAPTER_CLIENT:-${SOMA_CLIENT:-}}")
fi
if [[ -n "${SOMA_ADAPTER_BINDING_NONCE:-}" ]]; then
    ARGS+=(--binding-nonce "$SOMA_ADAPTER_BINDING_NONCE")
fi
if [[ "${SOMA_ADAPTER_SPOOL_FSYNC:-0}" == "1" ]]; then
    ARGS+=(--fsync)
fi

exec "$BIN" "${ARGS[@]}"
