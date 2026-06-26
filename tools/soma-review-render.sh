#!/usr/bin/env bash
# soma-review-render - reference client-specific review render plan reader.
#
# This compiles digest/actions/batch-template guidance into one read-only client
# render plan. It never records verification, never applies proposals, and never
# acknowledges notifications; clients should call the ack wrapper only after a
# visible digest render.
#
# Environment:
#   SOMA_REVIEW_PROJECT=myapp
#   SOMA_REVIEW_SESSION_ID=client-session
#   SOMA_REVIEW_LIMIT=20
#   SOMA_REVIEW_CLIENT=generic|codex-app|cursor|continue|claude-code
#   SOMA_REVIEW_INCLUDE_DISABLED=1
#   SOMA_REVIEW_FORMAT=json|markdown|html
#   SOMA_REVIEW_WRITE_REPORT=/path/to/review-render.json

set -euo pipefail

BIN="${SOMA_BIN:-soma}"

ARGS=(context review-render)
if [[ -n "${SOMA_REVIEW_PROJECT:-}" ]]; then
    ARGS+=(--project "$SOMA_REVIEW_PROJECT")
fi
if [[ -n "${SOMA_REVIEW_SESSION_ID:-}" ]]; then
    ARGS+=(--session-id "$SOMA_REVIEW_SESSION_ID")
fi
if [[ -n "${SOMA_REVIEW_LIMIT:-}" ]]; then
    ARGS+=(--limit "$SOMA_REVIEW_LIMIT")
fi
if [[ -n "${SOMA_REVIEW_CLIENT:-}" ]]; then
    ARGS+=(--client "$SOMA_REVIEW_CLIENT")
fi
if [[ "${SOMA_REVIEW_INCLUDE_DISABLED:-0}" == "1" || "${SOMA_REVIEW_INCLUDE_DISABLED:-}" == "true" ]]; then
    ARGS+=(--include-disabled)
fi
if [[ -n "${SOMA_REVIEW_FORMAT:-}" ]]; then
    ARGS+=(--format "$SOMA_REVIEW_FORMAT")
fi
if [[ -n "${SOMA_REVIEW_WRITE_REPORT:-}" ]]; then
    ARGS+=(--write-report "$SOMA_REVIEW_WRITE_REPORT")
fi
if [[ -n "${SOMA_DB:-}" ]]; then
    ARGS+=(--db-path "$SOMA_DB")
fi

exec "$BIN" "${ARGS[@]}"
