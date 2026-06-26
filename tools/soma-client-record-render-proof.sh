#!/usr/bin/env bash
# soma-client-record-render-proof - record observed_in_client_render after real UI evidence.
#
# This mutates only the SOMA client-binding proof ledger. It requires a filled
# soma.in_client_render_evidence.v1 artifact captured after the private client
# visibly rendered the review UI.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-$HOME}"
MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-$ROOT/tools/client-bindings/$CLIENT-soma-binding.json.example}"
EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_RENDER_EVIDENCE_SOURCE:-private_client_operator_observed_${CLIENT}_observed_in_client_render}"
REVIEW_RENDER_REPORT="${SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT:-}"
RENDER_EVIDENCE="${SOMA_CLIENT_BINDING_RENDER_EVIDENCE:-}"
INSTALLED_CONFIG_OVERRIDE="${SOMA_CLIENT_BINDING_INSTALLED_CONFIG:-}"

if [[ "$CLIENT" == "cursor" ]]; then
    CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-${SOMA_CURSOR_CONFIG_ROOT:-$CONFIG_ROOT}}"
    MANIFEST="${SOMA_CLIENT_BINDING_MANIFEST:-${SOMA_CURSOR_BINDING_MANIFEST:-$MANIFEST}}"
    EVIDENCE_SOURCE="${SOMA_CLIENT_BINDING_RENDER_EVIDENCE_SOURCE:-${SOMA_CURSOR_RENDER_EVIDENCE_SOURCE:-$EVIDENCE_SOURCE}}"
    REVIEW_RENDER_REPORT="${SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT:-${SOMA_CURSOR_REVIEW_RENDER_REPORT:-$REVIEW_RENDER_REPORT}}"
    RENDER_EVIDENCE="${SOMA_CLIENT_BINDING_RENDER_EVIDENCE:-${SOMA_CURSOR_RENDER_EVIDENCE:-$RENDER_EVIDENCE}}"
    INSTALLED_CONFIG_OVERRIDE="${SOMA_CLIENT_BINDING_INSTALLED_CONFIG:-${SOMA_CURSOR_INSTALLED_CONFIG:-$INSTALLED_CONFIG_OVERRIDE}}"
fi

report_json() {
    python3 - "$@" <<'PY'
import json
import sys

status, reason, detail, client = sys.argv[1:5]
print(json.dumps(
    {
        "schema": "soma.client_render_proof_recording_report.v1",
        "client": client,
        "status": status,
        "records_proof": False,
        "reason": reason,
        "detail": detail,
        "trust_boundary": (
            "client_render_proof_recording_records_no_proof_until_structured_"
            "in_client_render_evidence_and_explicit_operator_confirmation_are_present"
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

require_confirmation SOMA_CONFIRM_IN_CLIENT_RENDER
require_confirmation SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE
require_file SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT "$REVIEW_RENDER_REPORT"
require_file SOMA_CLIENT_BINDING_RENDER_EVIDENCE "$RENDER_EVIDENCE"

TMPDIR="${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-$(mktemp -d)}"
cleanup_tmp=0
if [[ -z "${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-}" ]]; then
    cleanup_tmp=1
fi
trap 'if [[ "$cleanup_tmp" == "1" ]]; then rm -rf "$TMPDIR"; fi' EXIT

DISCOVER_REPORT="$TMPDIR/$CLIENT-installed-config-discovery.json"
PROOF_REPORT="${SOMA_CLIENT_BINDING_RENDER_PROOF_REPORT:-$TMPDIR/$CLIENT-render-proof.json}"
STATUS_REPORT="$TMPDIR/$CLIENT-proof-status-after-render.json"
PROOF_SESSION_REPORT="$TMPDIR/$CLIENT-proof-session-after-render.json"
ERROR_REPORT="$TMPDIR/$CLIENT-render-proof.err"

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
        "schema": "soma.client_render_proof_recording_report.v1",
        "client": client,
        "status": "not_ready",
        "records_proof": False,
        "reason": "no_eligible_installed_config",
        "discovery": discovery,
        "trust_boundary": (
            "client_render_proof_recording_refused_until_installed_config_"
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
    --proof-level observed_in_client_render \
    --evidence-source "$EVIDENCE_SOURCE" \
    --installed-config "$INSTALLED_CONFIG" \
    --review-render-report "$REVIEW_RENDER_REPORT" \
    --render-evidence "$RENDER_EVIDENCE" \
    --operator-confirm-in-client-render \
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
        "schema": "soma.client_render_proof_recording_report.v1",
        "client": client,
        "status": "recording_failed",
        "records_proof": False,
        "installed_config": installed_config,
        "discovery": discovery,
        "error": error_text,
        "trust_boundary": (
            "client_render_proof_recording_preserves_adapter_binding_proof_"
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
    "$REVIEW_RENDER_REPORT" \
    "$RENDER_EVIDENCE" \
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
    review_render_report,
    render_evidence,
    evidence_source,
    client,
) = sys.argv[1:]

def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

print(json.dumps(
    {
        "schema": "soma.client_render_proof_recording_report.v1",
        "client": client,
        "status": "recorded_observed_in_client_render",
        "records_proof": True,
        "proof_level": "observed_in_client_render",
        "evidence_source": evidence_source,
        "installed_config": installed_config,
        "review_render_report_path": review_render_report,
        "render_evidence_path": render_evidence,
        "discovery": load(discovery_path),
        "proof_report": load(proof_report_path),
        "status_report": load(status_report_path),
        "proof_session_after_render": load(proof_session_path),
        "trust_boundary": (
            "client_render_proof_recording_mutates_only_client_binding_proofs: "
            "it records UI-only observed_in_client_render after structured render "
            "evidence and explicit operator confirmation; it creates no claim "
            "verification event, promotes no cloud draft, applies no proposal, "
            "and does not claim review-action readiness"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
