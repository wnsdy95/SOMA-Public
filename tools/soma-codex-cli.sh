#!/usr/bin/env bash
# Launch Codex CLI inside a SOMA-managed session scope.
#
# This wrapper does not modify Codex config or capture cloud output by itself.
# It ensures Codex, terminal shell-init capture, and SOMA adapter wrappers share
# the same SOMA_SESSION_ID/SOMA_CLIENT when used from one terminal.

set -euo pipefail

SOMA_BIN="${SOMA_BIN:-soma}"
CODEX_BIN="${CODEX_BIN:-codex}"
PROJECT="${SOMA_PROJECT:-$(basename "${PWD:-unknown}")}"

if [[ -z "${SOMA_SESSION_ID:-}" ]]; then
    eval "$("$SOMA_BIN" session start --client codex-cli --project "$PROJECT" --shell bash)"
else
    export SOMA_CLIENT="${SOMA_CLIENT:-codex-cli}"
    export SOMA_PROJECT="${SOMA_PROJECT:-$PROJECT}"
fi

exec "$CODEX_BIN" "$@"
