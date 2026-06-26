#!/usr/bin/env bash
# soma-review-batch-template - reference batch payload template reader.
#
# This is the read-only counterpart to `soma_review_batch`. It composes enabled
# verification actions from `soma context review-batch-template` into a
# dry-run-first payload that editor clients can show, edit, and submit.
#
# Environment:
#   SOMA_REVIEW_PROJECT=myapp
#   SOMA_REVIEW_SESSION_ID=client-session
#   SOMA_REVIEW_LIMIT=20
#   SOMA_REVIEW_ACTION=confirm|contradict|supersede|inconclusive
#   SOMA_REVIEW_TARGET_TYPE=any|claim|proposal
#   SOMA_REVIEW_VERIFIER=tool
#   SOMA_REVIEW_EVIDENCE_KIND=test
#   SOMA_REVIEW_EVIDENCE_ID=unit-test-run
#   SOMA_REVIEW_EVIDENCE_SOURCE=client

set -euo pipefail

BIN="${SOMA_BIN:-soma}"

ARGS=(context review-batch-template)
if [[ -n "${SOMA_REVIEW_PROJECT:-}" ]]; then
    ARGS+=(--project "$SOMA_REVIEW_PROJECT")
fi
if [[ -n "${SOMA_REVIEW_SESSION_ID:-}" ]]; then
    ARGS+=(--session-id "$SOMA_REVIEW_SESSION_ID")
fi
if [[ -n "${SOMA_REVIEW_LIMIT:-}" ]]; then
    ARGS+=(--limit "$SOMA_REVIEW_LIMIT")
fi
if [[ -n "${SOMA_REVIEW_ACTION:-}" ]]; then
    ARGS+=(--action "$SOMA_REVIEW_ACTION")
fi
if [[ -n "${SOMA_REVIEW_TARGET_TYPE:-}" ]]; then
    ARGS+=(--target-type "$SOMA_REVIEW_TARGET_TYPE")
fi
if [[ -n "${SOMA_REVIEW_VERIFIER:-}" ]]; then
    ARGS+=(--verifier "$SOMA_REVIEW_VERIFIER")
fi
if [[ -n "${SOMA_REVIEW_EVIDENCE_KIND:-}" ]]; then
    ARGS+=(--evidence-kind "$SOMA_REVIEW_EVIDENCE_KIND")
fi
if [[ -n "${SOMA_REVIEW_EVIDENCE_ID:-}" ]]; then
    ARGS+=(--evidence-id "$SOMA_REVIEW_EVIDENCE_ID")
fi
if [[ -n "${SOMA_REVIEW_EVIDENCE_SOURCE:-}" ]]; then
    ARGS+=(--evidence-source "$SOMA_REVIEW_EVIDENCE_SOURCE")
fi
if [[ -n "${SOMA_DB:-}" ]]; then
    ARGS+=(--db-path "$SOMA_DB")
fi

exec "$BIN" "${ARGS[@]}"
