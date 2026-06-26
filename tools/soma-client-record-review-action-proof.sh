#!/usr/bin/env bash
# soma-client-record-review-action-proof - record observed_review_action.
#
# This mutates only the SOMA client-binding proof ledger. It requires a
# storage-gated soma_review_action report produced by activating a rendered
# review control in the private client, and adapter-binding-proof must link
# that control_id to a prior observed_in_client_render proof in the same DB.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-$HOME}"
MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-$ROOT/tools/client-bindings/$CLIENT-soma-binding.json.example}"
EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_REVIEW_ACTION_EVIDENCE_SOURCE:-private_client_operator_observed_${CLIENT}_observed_review_action}"
REVIEW_ACTION_REPORT="${SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT:-}"
INSTALLED_CONFIG_OVERRIDE="${SOMA_CLIENT_BINDING_INSTALLED_CONFIG:-}"

if [[ "$CLIENT" == "cursor" ]]; then
    CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-${SOMA_CURSOR_CONFIG_ROOT:-$CONFIG_ROOT}}"
    MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-${SOMA_CURSOR_BINDING_MANIFEST:-$MANIFEST}}"
    EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_REVIEW_ACTION_EVIDENCE_SOURCE:-${SOMA_CURSOR_REVIEW_ACTION_EVIDENCE_SOURCE:-$EVIDENCE_SOURCE}}"
    REVIEW_ACTION_REPORT="${SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT:-${SOMA_CURSOR_REVIEW_ACTION_REPORT:-$REVIEW_ACTION_REPORT}}"
    INSTALLED_CONFIG_OVERRIDE="${SOMA_CLIENT_BINDING_INSTALLED_CONFIG:-${SOMA_CURSOR_INSTALLED_CONFIG:-$INSTALLED_CONFIG_OVERRIDE}}"
fi

report_json() {
    python3 - "$@" <<'PY'
import json
import sys

status, reason, detail, client = sys.argv[1:5]
print(json.dumps(
    {
        "schema": "soma.client_review_action_proof_recording_report.v1",
        "client": client,
        "status": status,
        "records_proof": False,
        "reason": reason,
        "detail": detail,
        "trust_boundary": (
            "client_review_action_proof_recording_records_no_proof_until_a_"
            "storage_gated_review_action_report_and_explicit_operator_confirmation_are_present"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
}

require_confirmation() {
    local key="$1"
    if [[ "${!key:-}" != "1" ]]; then
        report_json "refused_missing_operator_confirmation" "$key" \
            "required operator confirmation env flag is not set to 1" "$CLIENT"
        exit 2
    fi
}

require_file() {
    local key="$1"
    local path="$2"
    if [[ -z "$path" ]]; then
        report_json "refused_missing_artifact" "$key" \
            "required artifact path env var is empty" "$CLIENT"
        exit 2
    fi
    if [[ ! -f "$path" ]]; then
        report_json "refused_missing_artifact" "$key" \
            "required artifact path does not exist" "$CLIENT"
        exit 2
    fi
}

require_confirmation SOMA_CONFIRM_REVIEW_ACTION
require_confirmation SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE
require_file SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT "$REVIEW_ACTION_REPORT"

TMPDIR="${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-$(mktemp -d)}"
cleanup_tmp=0
if [[ -z "${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-}" ]]; then
    cleanup_tmp=1
fi
trap 'if [[ "$cleanup_tmp" == "1" ]]; then rm -rf "$TMPDIR"; fi' EXIT

DISCOVER_REPORT="$TMPDIR/$CLIENT-installed-config-discovery.json"
PROOF_REPORT="${SOMA_CLIENT_BINDING_REVIEW_ACTION_PROOF_REPORT:-$TMPDIR/$CLIENT-review-action-proof.json}"
STATUS_REPORT="$TMPDIR/$CLIENT-proof-status-after-review-action.json"
PROOF_SESSION_REPORT="$TMPDIR/$CLIENT-proof-session-after-review-action.json"
ERROR_REPORT="$TMPDIR/$CLIENT-review-action-proof.err"

if [[ -n "$INSTALLED_CONFIG_OVERRIDE" ]]; then
    INSTALLED_CONFIG="$INSTALLED_CONFIG_OVERRIDE"
    python3 - "$INSTALLED_CONFIG" "$CLIENT" <<'PY' >"$DISCOVER_REPORT"
import json
import sys

path, client = sys.argv[1:3]
print(json.dumps(
    {
        "schema": "soma.client_installed_config_override.v1",
        "client": client,
        "override": True,
        "eligible_candidates": 1,
        "candidates": [{"path": path, "eligible_for_observed_app_hook": True}],
        "trust_boundary": "installed_config_override_is_checked_by_adapter_binding_proof_before_recording",
    },
    indent=2,
    sort_keys=True,
))
PY
else
    "$BIN" adapter-binding-proof \
        --discover-installed-config \
        --manifest "$MANIFEST" \
        --client "$CLIENT" \
        --config-root "$CONFIG_ROOT" >"$DISCOVER_REPORT"
    INSTALLED_CONFIG="$(python3 - "$DISCOVER_REPORT" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
for candidate in data.get("candidates", []):
    if candidate.get("eligible_for_observed_app_hook") is True:
        print(candidate.get("path") or "")
        break
else:
    print("")
PY
)"
fi

if [[ -z "$INSTALLED_CONFIG" ]]; then
    python3 - "$DISCOVER_REPORT" "$CLIENT" <<'PY'
import json
import sys

discovery_path, client = sys.argv[1:3]
with open(discovery_path, "r", encoding="utf-8") as f:
    discovery = json.load(f)
print(json.dumps(
    {
        "schema": "soma.client_review_action_proof_recording_report.v1",
        "client": client,
        "status": "not_ready",
        "records_proof": False,
        "reason": "no_eligible_installed_config",
        "discovery": discovery,
        "trust_boundary": (
            "client_review_action_proof_recording_refused_until_installed_config_"
            "preflight_finds_a_client_binding_candidate"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 1
fi

mkdir -p "$(dirname "$PROOF_REPORT")"
if ! "$BIN" adapter-binding-proof \
    --manifest "$MANIFEST" \
    --client "$CLIENT" \
    --proof-level observed_review_action \
    --evidence-source "$EVIDENCE_SOURCE" \
    --installed-config "$INSTALLED_CONFIG" \
    --review-action-report "$REVIEW_ACTION_REPORT" \
    --operator-confirm-review-action \
    --operator-confirm-release-grade-evidence >"$PROOF_REPORT" 2>"$ERROR_REPORT"; then
    python3 - "$DISCOVER_REPORT" "$ERROR_REPORT" "$INSTALLED_CONFIG" "$CLIENT" <<'PY'
import json
import sys

discovery_path, error_path, installed_config, client = sys.argv[1:5]
with open(discovery_path, "r", encoding="utf-8") as f:
    discovery = json.load(f)
with open(error_path, "r", encoding="utf-8") as f:
    error_text = f.read().strip()
print(json.dumps(
    {
        "schema": "soma.client_review_action_proof_recording_report.v1",
        "client": client,
        "status": "recording_failed",
        "records_proof": False,
        "installed_config": installed_config,
        "discovery": discovery,
        "error": error_text,
        "trust_boundary": (
            "client_review_action_proof_recording_preserves_adapter_binding_proof_"
            "validation_failures_and_records_no_proof_on_failure"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 1
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
    "$DISCOVER_REPORT" \
    "$PROOF_REPORT" \
    "$STATUS_REPORT" \
    "$PROOF_SESSION_REPORT" \
    "$INSTALLED_CONFIG" \
    "$REVIEW_ACTION_REPORT" \
    "$EVIDENCE_SOURCE" \
    "$CLIENT" <<'PY'
import json
import sys

(
    discovery_path,
    proof_report_path,
    status_report_path,
    proof_session_path,
    installed_config,
    review_action_report,
    evidence_source,
    client,
) = sys.argv[1:]

def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

print(json.dumps(
    {
        "schema": "soma.client_review_action_proof_recording_report.v1",
        "client": client,
        "status": "recorded_observed_review_action",
        "records_proof": True,
        "proof_level": "observed_review_action",
        "evidence_source": evidence_source,
        "installed_config": installed_config,
        "review_action_report_path": review_action_report,
        "discovery": load(discovery_path),
        "proof_report": load(proof_report_path),
        "status_report": load(status_report_path),
        "proof_session_after_review_action": load(proof_session_path),
        "trust_boundary": (
            "client_review_action_proof_recording_mutates_only_client_binding_proofs: "
            "it records observed_review_action after a storage-gated review action "
            "report, linked prior render proof, and explicit operator confirmation; "
            "it creates no extra claim verification event, promotes no cloud draft, "
            "and applies no proposal"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
