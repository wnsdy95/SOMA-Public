#!/usr/bin/env bash
# soma-codex-app-capture - reference Codex app lifecycle capture wrapper.
#
# Input: one Codex app/private adapter JSON object on stdin. This wrapper does
# not read Codex app internals by itself and does not prove the app invoked it.
# It only fixes the release-grade handoff shape a Codex app integration must
# call: assistant_response/turn JSON -> soma-adapter-lifecycle -> adapter spool.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export SOMA_ADAPTER_LIFECYCLE_CLIENT="${SOMA_ADAPTER_LIFECYCLE_CLIENT:-codex-app}"
export SOMA_ADAPTER_LIFECYCLE_EVENT="${SOMA_ADAPTER_LIFECYCLE_EVENT:-assistant_response}"
export SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE="${SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE:-codex-app_private_lifecycle_hook}"
export SOMA_ADAPTER_LIFECYCLE_JSONL="${SOMA_ADAPTER_LIFECYCLE_JSONL:-${SOMA_ADAPTER_SPOOL_JSONL:-$HOME/.soma/adapter/events.jsonl}}"

exec "$ROOT/tools/soma-adapter-lifecycle.sh"
