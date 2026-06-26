#!/usr/bin/env bash
# soma-review-digest-ack - acknowledge a rendered review digest notification.
#
# This updates only SOMA's review digest notification cooldown ledger. It never
# records verification events and never applies learning proposals.
#
# Environment:
#   SOMA_REVIEW_PROJECT=myapp
#   SOMA_REVIEW_SESSION_ID=client-session
#   SOMA_REVIEW_LIMIT=20
#   SOMA_REVIEW_CLIENT=generic|codex-app|cursor|continue|claude-code
#   SOMA_REVIEW_BATCH_KEY=l4_semantic_promotion
#   SOMA_REVIEW_COOLDOWN_SECONDS=3600

set -euo pipefail

BIN="${SOMA_BIN:-soma}"

ARGS=(context review-digest-ack)
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
if [[ -n "${SOMA_REVIEW_BATCH_KEY:-}" ]]; then
    ARGS+=(--batch-key "$SOMA_REVIEW_BATCH_KEY")
fi
if [[ -n "${SOMA_REVIEW_COOLDOWN_SECONDS:-}" ]]; then
    ARGS+=(--cooldown-seconds "$SOMA_REVIEW_COOLDOWN_SECONDS")
fi
if [[ -n "${SOMA_DB:-}" ]]; then
    ARGS+=(--db-path "$SOMA_DB")
fi

exec "$BIN" "${ARGS[@]}"
