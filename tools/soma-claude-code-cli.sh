#!/usr/bin/env bash
# Launch Claude Code inside a SOMA-managed session scope.
#
# Pair this with tools/claude-code-stop-hook.sh. When SOMA_SESSION_ID is set,
# the stop hook stamps Claude turns with that managed session instead of the
# raw Claude transcript id, so terminal commands and Claude turns line up.

set -euo pipefail

SOMA_BIN="${SOMA_BIN:-soma}"
CLAUDE_CODE_BIN="${CLAUDE_CODE_BIN:-claude}"
PROJECT="${SOMA_PROJECT:-$(basename "${PWD:-unknown}")}"

if [[ -z "${SOMA_SESSION_ID:-}" ]]; then
    eval "$("$SOMA_BIN" session start --client claude-code --project "$PROJECT" --shell bash)"
else
    export SOMA_CLIENT="${SOMA_CLIENT:-claude-code}"
    export SOMA_PROJECT="${SOMA_PROJECT:-$PROJECT}"
fi

exec "$CLAUDE_CODE_BIN" "$@"
