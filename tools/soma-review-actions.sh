#!/usr/bin/env bash
# soma-review-actions - reference client action-plan reader for SOMA reviews.
#
# This is the read-only counterpart to `soma_review_action`. It returns the
# flattened `action_options` contract from `soma context review-actions` so an
# editor wrapper can render review buttons without parsing prose. Set
# SOMA_REVIEW_FORMAT=markdown when the client wants the read-only operator
# guide instead of the JSON action plan.
#
# Environment:
#   SOMA_REVIEW_PROJECT=myapp
#   SOMA_REVIEW_SESSION_ID=client-session
#   SOMA_REVIEW_LIMIT=20
#   SOMA_REVIEW_INCLUDE_DISABLED=1
#   SOMA_REVIEW_FORMAT=json|markdown

set -euo pipefail

BIN="${SOMA_BIN:-soma}"

ARGS=(context review-actions)
if [[ -n "${SOMA_REVIEW_PROJECT:-}" ]]; then
    ARGS+=(--project "$SOMA_REVIEW_PROJECT")
fi
if [[ -n "${SOMA_REVIEW_SESSION_ID:-}" ]]; then
    ARGS+=(--session-id "$SOMA_REVIEW_SESSION_ID")
fi
if [[ -n "${SOMA_REVIEW_LIMIT:-}" ]]; then
    ARGS+=(--limit "$SOMA_REVIEW_LIMIT")
fi
if [[ "${SOMA_REVIEW_INCLUDE_DISABLED:-0}" == "1" ]]; then
    ARGS+=(--include-disabled)
fi
if [[ -n "${SOMA_REVIEW_FORMAT:-}" ]]; then
    ARGS+=(--format "$SOMA_REVIEW_FORMAT")
fi
if [[ -n "${SOMA_DB:-}" ]]; then
    ARGS+=(--db-path "$SOMA_DB")
fi

exec "$BIN" "${ARGS[@]}"
