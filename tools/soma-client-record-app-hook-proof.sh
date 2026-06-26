#!/usr/bin/env bash
# soma-client-record-app-hook-proof - record observed_app_hook after real client evidence.
#
# This script mutates only the SOMA client-binding proof ledger. It refuses to
# record unless the generic readiness probe sees an eligible installed config and
# matching private spool event, and the operator supplies explicit real-client
# and release-grade evidence confirmations.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
DEFAULT_MANIFEST="$ROOT/tools/client-bindings/$CLIENT-soma-binding.json.example"
MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-$DEFAULT_MANIFEST}"
CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-$HOME}"
PROJECT_ROOT="${SOMA_CLIENT_BINDING_PROJECT_ROOT:-$ROOT}"
EVENT_JSONL="${SOMA_CLIENT_BINDING_EVENT_JSONL:-$HOME/.soma/adapter/events.jsonl}"
LOG_ROOT="${SOMA_CLIENT_BINDING_LOG_ROOT:-}"
CHECKPOINT="${SOMA_CLIENT_BINDING_PROOF_EVENT_CHECKPOINT:-${SOMA_CLIENT_BINDING_EVENT_CHECKPOINT:-}}"
PROOF_DRAIN_DB="${SOMA_CLIENT_BINDING_PROOF_DRAIN_DB:-}"
EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_APP_HOOK_EVIDENCE_SOURCE:-private_client_operator_observed_${CLIENT}_observed_app_hook}"

if [[ "$CLIENT" == "cursor" ]]; then
    MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-${SOMA_CURSOR_BINDING_MANIFEST:-$MANIFEST}}"
    CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-${SOMA_CURSOR_CONFIG_ROOT:-$CONFIG_ROOT}}"
    PROJECT_ROOT="${SOMA_CLIENT_BINDING_PROJECT_ROOT:-${SOMA_CURSOR_PROJECT_ROOT:-$PROJECT_ROOT}}"
    EVENT_JSONL="${SOMA_CLIENT_BINDING_EVENT_JSONL:-${SOMA_CURSOR_EVENT_JSONL:-$EVENT_JSONL}}"
    LOG_ROOT="${SOMA_CLIENT_BINDING_LOG_ROOT:-${SOMA_CURSOR_LOG_ROOT:-$HOME/Library/Application Support/Cursor/logs}}"
    CHECKPOINT="${SOMA_CLIENT_BINDING_PROOF_EVENT_CHECKPOINT:-${SOMA_CURSOR_PROOF_EVENT_CHECKPOINT:-${SOMA_CLIENT_BINDING_EVENT_CHECKPOINT:-${SOMA_CURSOR_EVENT_CHECKPOINT:-$CHECKPOINT}}}}"
    PROOF_DRAIN_DB="${SOMA_CLIENT_BINDING_PROOF_DRAIN_DB:-${SOMA_CURSOR_PROOF_DRAIN_DB:-$PROOF_DRAIN_DB}}"
    EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_APP_HOOK_EVIDENCE_SOURCE:-${SOMA_CURSOR_APP_HOOK_EVIDENCE_SOURCE:-$EVIDENCE_SOURCE}}"
fi

CLIENT_CONFIRM_KEY="SOMA_CONFIRM_REAL_$(printf '%s' "$CLIENT" | tr '[:lower:]-' '[:upper:]_')_HOOK"

require_confirmation() {
    local generic_key="$1"
    local client_key="$2"
    if [[ "${!generic_key:-}" == "1" || "${!client_key:-}" == "1" ]]; then
        return 0
    fi
    python3 - "$generic_key" "$client_key" "$CLIENT" <<'PY'
import json
import sys

generic_key, client_key, client = sys.argv[1:4]
print(json.dumps(
    {
        "schema": "soma.client_app_hook_recording_report.v1",
        "status": "refused_missing_operator_confirmation",
        "client": client,
        "missing_confirmation": [generic_key, client_key],
        "records_proof": False,
        "trust_boundary": (
            "client_app_hook_recording_requires_explicit_operator_confirmation: "
            "the script records no proof row unless a real-client hook confirmation "
            "and SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1 are both present"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 2
}

require_release_confirmation() {
    if [[ "${SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE:-}" == "1" ]]; then
        return 0
    fi
    python3 - "$CLIENT" <<'PY'
import json
import sys

client = sys.argv[1]
print(json.dumps(
    {
        "schema": "soma.client_app_hook_recording_report.v1",
        "status": "refused_missing_operator_confirmation",
        "client": client,
        "missing_confirmation": ["SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE"],
        "records_proof": False,
        "trust_boundary": (
            "client_app_hook_recording_requires_release_grade_confirmation: "
            "the script records no proof row unless release-grade evidence is "
            "explicitly operator-confirmed"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 2
}

require_confirmation SOMA_CONFIRM_REAL_CLIENT_HOOK "$CLIENT_CONFIRM_KEY"
require_release_confirmation

TMPDIR="${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-$(mktemp -d)}"
cleanup_tmp=0
if [[ -z "${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-}" ]]; then
    cleanup_tmp=1
fi
trap 'if [[ "$cleanup_tmp" == "1" ]]; then rm -rf "$TMPDIR"; fi' EXIT

if [[ -z "$CHECKPOINT" ]]; then
    CHECKPOINT="$TMPDIR/$CLIENT-hook-proof-drain.offset"
fi
if [[ -z "$PROOF_DRAIN_DB" ]]; then
    PROOF_DRAIN_DB="$TMPDIR/$CLIENT-hook-proof-drain.db"
fi

READINESS_REPORT="$TMPDIR/$CLIENT-hook-readiness.json"
READINESS_EXTRACT="$TMPDIR/$CLIENT-hook-readiness-extract.json"
DRAIN_REPORT="${SOMA_CLIENT_BINDING_DRAIN_REPORT:-$TMPDIR/$CLIENT-hook-drain.json}"
APP_HOOK_PROOF_REPORT="${SOMA_CLIENT_BINDING_APP_HOOK_PROOF_REPORT:-$TMPDIR/$CLIENT-app-hook-proof.json}"
STATUS_REPORT="$TMPDIR/$CLIENT-proof-status.json"
PROOF_SESSION_REPORT="$TMPDIR/$CLIENT-proof-session-after-app-hook.json"
ERROR_REPORT="$TMPDIR/$CLIENT-app-hook-proof.err"

SOMA_BIN="$BIN" \
SOMA_CLIENT_BINDING_CLIENT="$CLIENT" \
SOMA_CLIENT_BINDING_CONFIG_ROOT="$CONFIG_ROOT" \
SOMA_CLIENT_BINDING_PROJECT_ROOT="$PROJECT_ROOT" \
SOMA_CLIENT_BINDING_LOG_ROOT="$LOG_ROOT" \
SOMA_CLIENT_BINDING_EVENT_JSONL="$EVENT_JSONL" \
SOMA_CLIENT_BINDING_MANIFEST="$MANIFEST" \
    "$ROOT/tools/soma-client-hook-readiness.sh" >"$READINESS_REPORT"

python3 - "$READINESS_REPORT" >"$READINESS_EXTRACT" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)

installed_config = ""
for candidate in data.get("installed_config", {}).get("candidates", []):
    if candidate.get("eligible_for_observed_app_hook") is True:
        installed_config = candidate.get("path") or ""
        break

derived = data.get("derived", {})
spool = data.get("adapter_spool", {})
print(json.dumps(
    {
        "ready": derived.get("ready_to_record_observed_app_hook") is True,
        "installed_config": installed_config,
        "next_action": derived.get("next_action") or "",
        "matching_private_event_count": spool.get("matching_private_event_count", 0),
        "matching_private_binding_nonce_count": spool.get(
            "matching_private_binding_nonce_count", 0
        ),
    },
    sort_keys=True,
))
PY

readiness_field() {
    python3 - "$READINESS_EXTRACT" "$1" <<'PY'
import json
import sys

path, key = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
value = data.get(key, "")
if isinstance(value, bool):
    print("1" if value else "0")
else:
    print(value)
PY
}

READY="$(readiness_field ready)"
INSTALLED_CONFIG="$(readiness_field installed_config)"
NEXT_ACTION="$(readiness_field next_action)"
MATCHING_COUNT="$(readiness_field matching_private_event_count)"
MATCHING_NONCE_COUNT="$(readiness_field matching_private_binding_nonce_count)"

if [[ "$READY" != "1" || -z "$INSTALLED_CONFIG" ]]; then
    python3 - \
        "$READINESS_REPORT" \
        "$NEXT_ACTION" \
        "$MATCHING_COUNT" \
        "$MATCHING_NONCE_COUNT" \
        "$CLIENT" <<'PY'
import json
import sys

readiness_path, next_action, matching_count, matching_nonce_count, client = sys.argv[1:6]
with open(readiness_path, "r", encoding="utf-8") as f:
    readiness = json.load(f)
print(json.dumps(
    {
        "schema": "soma.client_app_hook_recording_report.v1",
        "status": "not_ready",
        "client": client,
        "records_proof": False,
        "next_action": next_action,
        "matching_private_event_count": int(matching_count),
        "matching_private_binding_nonce_count": int(matching_nonce_count),
        "readiness": readiness,
        "trust_boundary": (
            "client_app_hook_recording_refused_until_readiness_probe_observes_"
            "eligible_installed_config_and_matching_private_spool_event"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 1
fi

mkdir -p \
    "$(dirname "$CHECKPOINT")" \
    "$(dirname "$DRAIN_REPORT")" \
    "$(dirname "$APP_HOOK_PROOF_REPORT")" \
    "$(dirname "$PROOF_DRAIN_DB")"

"$BIN" adapter-spool \
    --jsonl "$EVENT_JSONL" \
    --checkpoint "$CHECKPOINT" \
    --db-path "$PROOF_DRAIN_DB" >"$DRAIN_REPORT"

set +e
"$BIN" adapter-binding-proof \
    --manifest "$MANIFEST" \
    --client "$CLIENT" \
    --proof-level observed_app_hook \
    --evidence-source "$EVIDENCE_SOURCE" \
    --event-jsonl "$EVENT_JSONL" \
    --drain-report "$DRAIN_REPORT" \
    --installed-config "$INSTALLED_CONFIG" \
    --operator-confirm-real-app-invocation \
    --operator-confirm-release-grade-evidence >"$APP_HOOK_PROOF_REPORT" 2>"$ERROR_REPORT"
code=$?
set -e

if [[ "$code" -ne 0 ]]; then
    python3 - "$READINESS_REPORT" "$DRAIN_REPORT" "$ERROR_REPORT" "$CLIENT" <<'PY'
import json
import sys

readiness_path, drain_path, error_path, client = sys.argv[1:5]
with open(readiness_path, "r", encoding="utf-8") as f:
    readiness = json.load(f)
with open(drain_path, "r", encoding="utf-8") as f:
    drain = json.load(f)
with open(error_path, "r", encoding="utf-8") as f:
    error_text = f.read().strip()
print(json.dumps(
    {
        "schema": "soma.client_app_hook_recording_report.v1",
        "status": "recording_failed",
        "client": client,
        "records_proof": False,
        "readiness": readiness,
        "drain_report": drain,
        "error": error_text,
        "trust_boundary": "client_app_hook_recording_failed_before_any_success_claim",
    },
    indent=2,
    sort_keys=True,
))
PY
    exit "$code"
fi

"$BIN" adapter-binding-proof \
    --status \
    --client "$CLIENT" \
    --manifest "$MANIFEST" >"$STATUS_REPORT"

"$BIN" adapter-binding-proof \
    --proof-session \
    --client "$CLIENT" \
    --manifest "$MANIFEST" \
    --config-root "$CONFIG_ROOT" >"$PROOF_SESSION_REPORT"

python3 - \
    "$READINESS_REPORT" \
    "$DRAIN_REPORT" \
    "$APP_HOOK_PROOF_REPORT" \
    "$STATUS_REPORT" \
    "$PROOF_SESSION_REPORT" \
    "$CLIENT" \
    "$CHECKPOINT" \
    "$PROOF_DRAIN_DB" \
    "$EVIDENCE_SOURCE" <<'PY'
import json
import sys

(
    readiness_path,
    drain_path,
    proof_path,
    status_path,
    proof_session_path,
    client,
    checkpoint,
    proof_drain_db,
    evidence_source,
) = sys.argv[1:]

def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

proof = load(proof_path)
print(json.dumps(
    {
        "schema": "soma.client_app_hook_recording_report.v1",
        "status": "recorded_observed_app_hook",
        "client": client,
        "records_proof": True,
        "proof_id": proof.get("proof_id"),
        "proof_level": proof.get("proof_level"),
        "evidence_source": evidence_source,
        "checkpoint": checkpoint,
        "readiness": load(readiness_path),
        "drain_report": load(drain_path),
        "proof_report": proof,
        "proof_drain_db_path": proof_drain_db,
        "status_report": load(status_path),
        "proof_session_after_app_hook": load(proof_session_path),
        "trust_boundary": (
            "client_app_hook_recording_records_only_observed_app_hook: it does "
            "not claim in-client render, review action, claim verification, "
            "proposal apply, cloud draft promotion, or digest acknowledgement"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
