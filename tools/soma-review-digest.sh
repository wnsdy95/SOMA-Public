#!/usr/bin/env bash
# soma-review-digest - reference read-only review notification renderer.
#
# This is the client-notification counterpart to `soma_review_digest`. It
# renders compact non-blocking review digest items without recording
# verification events or applying proposals.
#
# Environment:
#   SOMA_REVIEW_PROJECT=myapp
#   SOMA_REVIEW_SESSION_ID=client-session
#   SOMA_REVIEW_LIMIT=20
#   SOMA_REVIEW_CLIENT=generic|codex-app|cursor|continue|claude-code
#   SOMA_REVIEW_INCLUDE_QUEUE_ONLY=1
#   SOMA_REVIEW_FORMAT=json|markdown

set -euo pipefail

BIN="${SOMA_BIN:-soma}"

ARGS=(context review-digest)
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
if [[ "${SOMA_REVIEW_INCLUDE_QUEUE_ONLY:-0}" == "1" || "${SOMA_REVIEW_INCLUDE_QUEUE_ONLY:-}" == "true" ]]; then
    ARGS+=(--include-queue-only)
fi
if [[ -n "${SOMA_REVIEW_FORMAT:-}" ]]; then
    ARGS+=(--format "$SOMA_REVIEW_FORMAT")
fi
if [[ -n "${SOMA_DB:-}" ]]; then
    ARGS+=(--db-path "$SOMA_DB")
fi

exec "$BIN" "${ARGS[@]}"
