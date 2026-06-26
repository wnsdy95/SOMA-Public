#!/usr/bin/env bash
# soma-client-release-proof-runbook - inspect or record a private client proof chain.
#
# Default mode is read-only. Set SOMA_CLIENT_RELEASE_PROOF_MODE=record_ready to
# delegate to the narrow generic proof recorders for app-hook, render, and
# review-action rows when their artifacts and confirmation flags are present.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-$HOME}"
PROJECT_ROOT="${SOMA_CLIENT_BINDING_PROJECT_ROOT:-$ROOT}"
EVENT_JSONL="${SOMA_CLIENT_BINDING_EVENT_JSONL:-$HOME/.soma/adapter/events.jsonl}"
LOG_ROOT="${SOMA_CLIENT_BINDING_LOG_ROOT:-}"
MANIFEST_OVERRIDE="${SOMA_CLIENT_BINDING_MANIFEST:-}"
MODE="${SOMA_CLIENT_RELEASE_PROOF_MODE:-read_only}"
REVIEW_RENDER_REPORT="${SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT:-}"
RENDER_EVIDENCE="${SOMA_CLIENT_BINDING_RENDER_EVIDENCE:-}"
REVIEW_ACTION_REPORT="${SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT:-}"

usage() {
    cat <<EOF
Usage: tools/soma-client-release-proof-runbook.sh [OPTIONS]

Inspect or record a private client proof chain. Default mode is read-only.

Options:
  --client CLIENT
  --soma-bin PATH
  --manifest PATH
  --config-root PATH
  --project-root PATH
  --event-jsonl PATH
  --log-root PATH
  --mode read_only|record_ready
  --review-render-report PATH
  --render-evidence PATH
  --review-action-report PATH
  -h, --help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --client)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --client" >&2
                exit 2
            fi
            CLIENT="$2"
            shift 2
            ;;
        --soma-bin|--bin)
            if [[ $# -lt 2 ]]; then
                echo "missing value for $1" >&2
                exit 2
            fi
            BIN="$2"
            shift 2
            ;;
        --manifest)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --manifest" >&2
                exit 2
            fi
            MANIFEST_OVERRIDE="$2"
            shift 2
            ;;
        --config-root)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --config-root" >&2
                exit 2
            fi
            CONFIG_ROOT="$2"
            shift 2
            ;;
        --project-root)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --project-root" >&2
                exit 2
            fi
            PROJECT_ROOT="$2"
            shift 2
            ;;
        --event-jsonl)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --event-jsonl" >&2
                exit 2
            fi
            EVENT_JSONL="$2"
            shift 2
            ;;
        --log-root)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --log-root" >&2
                exit 2
            fi
            LOG_ROOT="$2"
            shift 2
            ;;
        --mode)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --mode" >&2
                exit 2
            fi
            MODE="$2"
            shift 2
            ;;
        --review-render-report)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --review-render-report" >&2
                exit 2
            fi
            REVIEW_RENDER_REPORT="$2"
            shift 2
            ;;
        --render-evidence)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --render-evidence" >&2
                exit 2
            fi
            RENDER_EVIDENCE="$2"
            shift 2
            ;;
        --review-action-report)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --review-action-report" >&2
                exit 2
            fi
            REVIEW_ACTION_REPORT="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

MANIFEST="${MANIFEST_OVERRIDE:-$ROOT/tools/client-bindings/$CLIENT-soma-binding.json.example}"

if [[ "$MODE" != "read_only" && "$MODE" != "record_ready" ]]; then
    python3 - "$MODE" "$CLIENT" <<'PY'
import json
import sys

mode, client = sys.argv[1:3]
print(json.dumps(
    {
        "schema": "soma.client_release_proof_runbook.v1",
        "client": client,
        "status": "refused_invalid_mode",
        "mode": mode,
        "records_proof": False,
        "allowed_modes": ["read_only", "record_ready"],
        "trust_boundary": "invalid_mode_records_no_proof",
    },
    indent=2,
    sort_keys=True,
))
PY
    exit 2
fi

TMPDIR="${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-$(mktemp -d)}"
cleanup_tmp=0
if [[ -z "${SOMA_CLIENT_BINDING_PROOF_TMPDIR:-}" ]]; then
    cleanup_tmp=1
fi
trap 'if [[ "$cleanup_tmp" == "1" ]]; then rm -rf "$TMPDIR"; fi' EXIT

READINESS_REPORT="$TMPDIR/$CLIENT-hook-readiness.json"
STATUS_BEFORE="$TMPDIR/$CLIENT-proof-status-before.json"
PROOF_SESSION_BEFORE="$TMPDIR/$CLIENT-proof-session-before.json"
STATUS_AFTER="$TMPDIR/$CLIENT-proof-status-after.json"
PROOF_SESSION_AFTER="$TMPDIR/$CLIENT-proof-session-after.json"
APP_HOOK_RECORD="$TMPDIR/$CLIENT-app-hook-record.json"
RENDER_RECORD="$TMPDIR/$CLIENT-render-record.json"
REVIEW_ACTION_RECORD="$TMPDIR/$CLIENT-review-action-record.json"
APP_HOOK_EXIT="$TMPDIR/$CLIENT-app-hook-record.exit"
RENDER_EXIT="$TMPDIR/$CLIENT-render-record.exit"
REVIEW_ACTION_EXIT="$TMPDIR/$CLIENT-review-action-record.exit"

run_readiness() {
    SOMA_BIN="$BIN" \
    SOMA_CLIENT_BINDING_CLIENT="$CLIENT" \
    SOMA_CLIENT_BINDING_CONFIG_ROOT="$CONFIG_ROOT" \
    SOMA_CLIENT_BINDING_PROJECT_ROOT="$PROJECT_ROOT" \
    SOMA_CLIENT_BINDING_LOG_ROOT="$LOG_ROOT" \
    SOMA_CLIENT_BINDING_EVENT_JSONL="$EVENT_JSONL" \
    SOMA_CLIENT_BINDING_MANIFEST="$MANIFEST" \
        "$ROOT/tools/soma-client-hook-readiness.sh" >"$READINESS_REPORT"
}

run_status_reports() {
    "$BIN" adapter-binding-proof \
        --status \
        --client "$CLIENT" \
        --manifest "$MANIFEST" >"$STATUS_AFTER"
    "$BIN" adapter-binding-proof \
        --proof-session \
        --client "$CLIENT" \
        --manifest "$MANIFEST" \
        --config-root "$CONFIG_ROOT" >"$PROOF_SESSION_AFTER"
}

run_readiness
"$BIN" adapter-binding-proof \
    --status \
    --client "$CLIENT" \
    --manifest "$MANIFEST" >"$STATUS_BEFORE"
"$BIN" adapter-binding-proof \
    --proof-session \
    --client "$CLIENT" \
    --manifest "$MANIFEST" \
    --config-root "$CONFIG_ROOT" >"$PROOF_SESSION_BEFORE"
cp "$STATUS_BEFORE" "$STATUS_AFTER"
cp "$PROOF_SESSION_BEFORE" "$PROOF_SESSION_AFTER"

json_field() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

path, pointer = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
value = data
for part in pointer.strip("/").split("/"):
    if part == "":
        continue
    if isinstance(value, list):
        value = value[int(part)]
    else:
        value = value.get(part)
    if value is None:
        break
if isinstance(value, bool):
    print("1" if value else "0")
elif isinstance(value, (list, dict)):
    print(json.dumps(value, separators=(",", ":")))
elif value is None:
    print("")
else:
    print(value)
PY
}

proof_completed() {
    python3 - "$PROOF_SESSION_AFTER" "$1" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
completed = set(data.get("proof_session", {}).get("completed_proof_levels", []))
print("1" if sys.argv[2] in completed else "0")
PY
}

artifact_present() {
    local path="$1"
    [[ -n "$path" && -f "$path" ]]
}

write_not_attempted() {
    local path="$1"
    local level="$2"
    local reason="$3"
    python3 - "$level" "$reason" "$CLIENT" <<'PY' >"$path"
import json
import sys

level, reason, client = sys.argv[1:4]
print(json.dumps(
    {
        "status": "not_attempted",
        "client": client,
        "proof_level": level,
        "records_proof": False,
        "reason": reason,
        "trust_boundary": "not_attempted_records_no_proof",
    },
    indent=2,
    sort_keys=True,
))
PY
    printf '0\n' >"${path}.exit"
}

write_not_attempted "$APP_HOOK_RECORD" "observed_app_hook" "mode_read_only_or_not_needed"
write_not_attempted "$RENDER_RECORD" "observed_in_client_render" "mode_read_only_or_not_needed"
write_not_attempted "$REVIEW_ACTION_RECORD" "observed_review_action" "mode_read_only_or_not_needed"
mv "$APP_HOOK_RECORD.exit" "$APP_HOOK_EXIT"
mv "$RENDER_RECORD.exit" "$RENDER_EXIT"
mv "$REVIEW_ACTION_RECORD.exit" "$REVIEW_ACTION_EXIT"

if [[ "$MODE" == "record_ready" ]]; then
    if [[ "$(proof_completed observed_app_hook)" != "1" ]]; then
        if [[ "$(json_field "$READINESS_REPORT" /derived/ready_to_record_observed_app_hook)" == "1" ]]; then
            set +e
            SOMA_BIN="$BIN" \
            SOMA_CLIENT_BINDING_CLIENT="$CLIENT" \
            SOMA_CLIENT_BINDING_CONFIG_ROOT="$CONFIG_ROOT" \
            SOMA_CLIENT_BINDING_PROJECT_ROOT="$PROJECT_ROOT" \
            SOMA_CLIENT_BINDING_LOG_ROOT="$LOG_ROOT" \
            SOMA_CLIENT_BINDING_EVENT_JSONL="$EVENT_JSONL" \
            SOMA_CLIENT_BINDING_MANIFEST="$MANIFEST" \
            SOMA_CLIENT_BINDING_PROOF_EVENT_CHECKPOINT="$TMPDIR/$CLIENT-app-hook-proof-drain.offset" \
            SOMA_CLIENT_BINDING_PROOF_DRAIN_DB="$TMPDIR/$CLIENT-app-hook-proof-drain.db" \
                "$ROOT/tools/soma-client-record-app-hook-proof.sh" >"$APP_HOOK_RECORD"
            code=$?
            set -e
            printf '%s\n' "$code" >"$APP_HOOK_EXIT"
            run_status_reports
        else
            write_not_attempted "$APP_HOOK_RECORD" "observed_app_hook" "readiness_probe_not_ready"
            mv "$APP_HOOK_RECORD.exit" "$APP_HOOK_EXIT"
        fi
    fi

    if [[ "$(proof_completed observed_app_hook)" == "1" && "$(proof_completed observed_in_client_render)" != "1" ]]; then
        if artifact_present "$REVIEW_RENDER_REPORT" && artifact_present "$RENDER_EVIDENCE"; then
            set +e
            SOMA_BIN="$BIN" \
            SOMA_CLIENT_BINDING_CLIENT="$CLIENT" \
            SOMA_CLIENT_BINDING_CONFIG_ROOT="$CONFIG_ROOT" \
            SOMA_CLIENT_BINDING_MANIFEST="$MANIFEST" \
            SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT="$REVIEW_RENDER_REPORT" \
            SOMA_CLIENT_BINDING_RENDER_EVIDENCE="$RENDER_EVIDENCE" \
                "$ROOT/tools/soma-client-record-render-proof.sh" >"$RENDER_RECORD"
            code=$?
            set -e
            printf '%s\n' "$code" >"$RENDER_EXIT"
            run_status_reports
        else
            write_not_attempted "$RENDER_RECORD" "observed_in_client_render" "review_render_report_or_render_evidence_missing"
            mv "$RENDER_RECORD.exit" "$RENDER_EXIT"
        fi
    fi

    if [[ "$(proof_completed observed_in_client_render)" == "1" && "$(proof_completed observed_review_action)" != "1" ]]; then
        if artifact_present "$REVIEW_ACTION_REPORT"; then
            set +e
            SOMA_BIN="$BIN" \
            SOMA_CLIENT_BINDING_CLIENT="$CLIENT" \
            SOMA_CLIENT_BINDING_CONFIG_ROOT="$CONFIG_ROOT" \
            SOMA_CLIENT_BINDING_MANIFEST="$MANIFEST" \
            SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT="$REVIEW_ACTION_REPORT" \
                "$ROOT/tools/soma-client-record-review-action-proof.sh" >"$REVIEW_ACTION_RECORD"
            code=$?
            set -e
            printf '%s\n' "$code" >"$REVIEW_ACTION_EXIT"
            run_status_reports
        else
            write_not_attempted "$REVIEW_ACTION_RECORD" "observed_review_action" "review_action_report_missing"
            mv "$REVIEW_ACTION_RECORD.exit" "$REVIEW_ACTION_EXIT"
        fi
    fi
fi

python3 - \
    "$CLIENT" \
    "$MODE" \
    "$READINESS_REPORT" \
    "$STATUS_BEFORE" \
    "$PROOF_SESSION_BEFORE" \
    "$STATUS_AFTER" \
    "$PROOF_SESSION_AFTER" \
    "$APP_HOOK_RECORD" \
    "$APP_HOOK_EXIT" \
    "$RENDER_RECORD" \
    "$RENDER_EXIT" \
    "$REVIEW_ACTION_RECORD" \
    "$REVIEW_ACTION_EXIT" \
    "$REVIEW_RENDER_REPORT" \
    "$RENDER_EVIDENCE" \
    "$REVIEW_ACTION_REPORT" <<'PY'
import json
import os
import sys

(
    client,
    mode,
    readiness_path,
    status_before_path,
    proof_session_before_path,
    status_after_path,
    proof_session_after_path,
    app_hook_path,
    app_hook_exit_path,
    render_path,
    render_exit_path,
    review_action_path,
    review_action_exit_path,
    review_render_report,
    render_evidence,
    review_action_report,
) = sys.argv[1:]

def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

def load_exit(path):
    with open(path, "r", encoding="utf-8") as f:
        return int(f.read().strip() or "0")

readiness = load(readiness_path)
status_before = load(status_before_path)
proof_session_before = load(proof_session_before_path)
status_after = load(status_after_path)
proof_session_after = load(proof_session_after_path)
recordings = {
    "observed_app_hook": {"exit_code": load_exit(app_hook_exit_path), "report": load(app_hook_path)},
    "observed_in_client_render": {"exit_code": load_exit(render_exit_path), "report": load(render_path)},
    "observed_review_action": {"exit_code": load_exit(review_action_exit_path), "report": load(review_action_path)},
}
records_proof = any(item["report"].get("records_proof") is True for item in recordings.values())
blocked = set(proof_session_after.get("proof_session", {}).get("blocked_proof_levels", []))
release_gate = proof_session_after.get("proof_session", {}).get("release_gate")

artifacts = {
    "review_render_report": {
        "path": review_render_report,
        "present": bool(review_render_report and os.path.isfile(review_render_report)),
    },
    "render_evidence": {
        "path": render_evidence,
        "present": bool(render_evidence and os.path.isfile(render_evidence)),
    },
    "review_action_report": {
        "path": review_action_report,
        "present": bool(review_action_report and os.path.isfile(review_action_report)),
    },
}

if release_gate == "pass":
    next_action = "run_strict_product_hardening_report"
elif "observed_app_hook" in blocked:
    next_action = readiness.get("derived", {}).get("next_action") or "trigger_private_client_hook"
elif "observed_in_client_render" in blocked:
    if not artifacts["review_render_report"]["present"] or not artifacts["render_evidence"]["present"]:
        next_action = "render_review_surface_and_capture_in_client_render_evidence"
    else:
        next_action = "record_observed_in_client_render_with_operator_confirmation"
elif "observed_review_action" in blocked:
    if not artifacts["review_action_report"]["present"]:
        next_action = "execute_rendered_review_control_and_capture_review_action_report"
    else:
        next_action = "record_observed_review_action_with_operator_confirmation"
else:
    next_action = proof_session_after.get("proof_session", {}).get("next_step_id") or "inspect_proof_session"

commands = {
    "durable_artifact_dir": [
        f"$HOME/.soma/client-evidence/{client}/<run-id>",
    ],
    "app_hook_readiness_probe": [
        "env",
        f"SOMA_CLIENT_BINDING_CLIENT={client}",
        "tools/soma-client-hook-readiness.sh",
    ],
    "record_app_hook_when_ready": [
        "env",
        f"SOMA_CLIENT_BINDING_CLIENT={client}",
        "SOMA_CONFIRM_REAL_CLIENT_HOOK=1",
        "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1",
        "tools/soma-client-record-app-hook-proof.sh",
    ],
    "record_render_after_visible_ui": [
        "env",
        f"SOMA_CLIENT_BINDING_CLIENT={client}",
        "SOMA_CONFIRM_IN_CLIENT_RENDER=1",
        "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1",
        f"SOMA_CLIENT_BINDING_REVIEW_RENDER_REPORT=$HOME/.soma/client-evidence/{client}/<run-id>/review-render.json",
        f"SOMA_CLIENT_BINDING_RENDER_EVIDENCE=$HOME/.soma/client-evidence/{client}/<run-id>/render-evidence.json",
        "tools/soma-client-record-render-proof.sh",
    ],
    "record_review_action_after_rendered_control": [
        "env",
        f"SOMA_CLIENT_BINDING_CLIENT={client}",
        "SOMA_CONFIRM_REVIEW_ACTION=1",
        "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1",
        f"SOMA_CLIENT_BINDING_REVIEW_ACTION_REPORT=$HOME/.soma/client-evidence/{client}/<run-id>/review-action.json",
        "tools/soma-client-record-review-action-proof.sh",
    ],
}

print(json.dumps(
    {
        "schema": "soma.client_release_proof_runbook.v1",
        "client": client,
        "status": "ready" if release_gate == "pass" else "pending",
        "mode": mode,
        "records_proof": records_proof,
        "next_action": next_action,
        "artifacts": artifacts,
        "readiness": readiness,
        "status_before": status_before,
        "proof_session_before": proof_session_before,
        "recordings": recordings,
        "status_after": status_after,
        "proof_session_after": proof_session_after,
        "operator_commands": commands,
        "trust_boundary": (
            "client_release_proof_runbook_is_read_only_by_default: record_ready "
            "only delegates to narrow generic recorder scripts, each of which "
            "requires explicit operator and release-grade evidence confirmation "
            "and records only client-binding proof rows; the runbook creates no "
            "claim verification event, promotes no cloud draft, applies no "
            "proposal, and cannot substitute for real private client app/render/"
            "action observation"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
