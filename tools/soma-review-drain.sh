#!/usr/bin/env bash
# soma-review-drain - reference safe review-drain runner for SOMA clients.
#
# This wrapper intentionally exposes only the verified non-destructive drain
# policy. It never supplies verification evidence and never enables destructive
# decay/forget application.

set -euo pipefail

SOMA_BIN="${SOMA_BIN:-soma}"

ARGS=(context review-drain)

if [[ -n "${SOMA_REVIEW_PROJECT:-}" ]]; then
  ARGS+=(--project "$SOMA_REVIEW_PROJECT")
fi

if [[ -n "${SOMA_REVIEW_SESSION_ID:-}" ]]; then
  ARGS+=(--session-id "$SOMA_REVIEW_SESSION_ID")
fi

if [[ -n "${SOMA_REVIEW_LIMIT:-}" ]]; then
  ARGS+=(--limit "$SOMA_REVIEW_LIMIT")
fi

if [[ "${SOMA_REVIEW_DRY_RUN:-0}" == "1" || "${SOMA_REVIEW_DRY_RUN:-}" == "true" ]]; then
  ARGS+=(--dry-run)
fi

if [[ -n "${SOMA_DB:-}" ]]; then
  ARGS+=(--db-path "$SOMA_DB")
fi

exec "$SOMA_BIN" "${ARGS[@]}"
