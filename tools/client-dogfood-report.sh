#!/usr/bin/env bash
# client-dogfood-report — real-client preflight plus isolated SOMA scope proof.
#
# This report intentionally separates:
#   - observable client/runtime readiness on this machine,
#   - SOMA MCP/context/capture contracts in an isolated HOME,
#   - private app-hook/render/review-action proof, which remains unproven until
#     release-grade client binding proof rows are recorded from real clients.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
JSON_OUT=""
JSON_OUT_EXPLICIT=0
JSON_OUT_DISABLED=0

usage() {
    cat <<'EOF'
Usage: tools/client-dogfood-report.sh [--json-out PATH] [--no-json-out]

Runs the local SOMA dogfood gate and prints a human-readable report.

Options:
  --json-out PATH  Write a machine-readable soma.client_dogfood_report.v1 summary.
                   Defaults to $HOME/.soma/reports/client-dogfood-latest.json.
  --no-json-out    Do not write the latest machine-readable summary.
  -h, --help       Show this help text.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --json-out)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "error: --json-out requires a path" >&2
                exit 2
            fi
            JSON_OUT="$2"
            JSON_OUT_EXPLICIT=1
            shift 2
            ;;
        --no-json-out)
            JSON_OUT_DISABLED=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! -x "$BIN" ]]; then
    (cd "$ROOT" && cargo build -p soma >/dev/null)
    BIN="$ROOT/target/debug/soma"
fi

if [[ ! -x "$BIN" ]]; then
    echo "soma binary not found at $BIN" >&2
    exit 1
fi
BIN_ABS="$(python3 - "$BIN" <<'PY'
import os
import sys

print(os.path.abspath(sys.argv[1]))
PY
)"

RUN_DIR="$(mktemp -d)"
REAL_HOME="${HOME:-}"
REAL_PRIVATE_SNAPSHOT_JSON="$RUN_DIR/real-private-client-release-snapshot.json"
if [[ $JSON_OUT_DISABLED -eq 0 && -z "$JSON_OUT" && -n "${REAL_HOME:-}" ]]; then
    JSON_OUT="$REAL_HOME/.soma/reports/client-dogfood-latest.json"
fi
export HOME="$RUN_DIR/home"
mkdir -p "$HOME"
BG_PIDS=()
cleanup() {
    local pid
    for pid in "${BG_PIDS[@]:-}"; do
        [[ -n "$pid" ]] || continue
        kill "$pid" >/dev/null 2>&1 || true
        wait "$pid" >/dev/null 2>&1 || true
    done
    rm -rf "$RUN_DIR"
}
trap cleanup EXIT

PASS=0
WARN=0
FAIL=0
CURRENT_SECTION="startup"
EVENTS_TSV="$RUN_DIR/check-events.tsv"
: > "$EVENTS_TSV"

section() {
    CURRENT_SECTION="$1"
    echo "[$CURRENT_SECTION]"
}

record_check() {
    local status="$1"
    local message="$2"
    printf '%s\t%s\t%s\n' "$status" "$CURRENT_SECTION" "$message" >> "$EVENTS_TSV"
}

pass() {
    echo "  ok   $1"
    record_check "pass" "$1"
    PASS=$((PASS + 1))
}

warn() {
    echo "  warn $1"
    record_check "warn" "$1"
    WARN=$((WARN + 1))
}

fail() {
    echo "  fail $1"
    record_check "fail" "$1"
    FAIL=$((FAIL + 1))
}

write_json_report() {
    python3 - "$JSON_OUT" "$ROOT" "$BIN" "${REAL_HOME:-}" "$HOME" "$PASS" "$WARN" "$FAIL" "$EVENTS_TSV" "$REAL_PRIVATE_SNAPSHOT_JSON" <<'PY'
import datetime
import json
import sys
import time
from collections import OrderedDict
from pathlib import Path

out_path, workspace, soma_bin, real_home, run_home, passed, warned, failed, events_path, snapshot_path = sys.argv[1:]
events = []
sections = OrderedDict()
with open(events_path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if not line:
            continue
        try:
            status, section, message = line.split("\t", 2)
        except ValueError:
            continue
        events.append({"status": status, "section": section, "message": message})
        counts = sections.setdefault(section, {"section": section, "pass": 0, "warn": 0, "fail": 0})
        counts[status] = counts.get(status, 0) + 1

def status_for(section_names):
    subset = [sections[name] for name in section_names if name in sections]
    if not subset:
        return "not_run"
    if any(item["fail"] for item in subset):
        return "fail"
    if any(item["warn"] for item in subset):
        return "warn"
    return "pass"

required_private_release_clients = ["codex-app", "cursor", "continue"]
private_release_required_proof_levels = [
    "observed_app_hook",
    "observed_in_client_render",
    "observed_review_action",
]

def fallback_private_release_snapshot(status="not_captured", error=None):
    return {
        "schema": "soma.real_private_app_release_snapshot.v1",
        "source": "tools/client-dogfood-report.sh",
        "status": status,
        "real_home": real_home or None,
        "ready": False,
        "ready_clients": [],
        "pending_clients": required_private_release_clients,
        "unavailable_clients": required_private_release_clients,
        "required_proof_levels": private_release_required_proof_levels,
        "client_count": 0,
        "private_app_client_count": 0,
        "release_ready_count": 0,
        "clients": [],
        "operator_status": None,
        "operator_primary_next_step": None,
        "operator_primary_next_command": [],
        "operator_blocked_claims": [],
        "operator_safe_to_claim": [],
        "operator_private_app_restart_commands": [],
        "operator_private_app_collector_start_commands": [],
        "operator_private_app_wait_commands": [],
        "render_evidence_artifact_scans": [],
        "error": error or "real private app release snapshot was not captured",
        "trust_boundary": (
            "real_private_app_release_snapshot_is_read_only: missing or invalid "
            "snapshot records no proof row, creates no verification event, "
            "installs no hook, promotes no cloud draft, and cannot substitute "
            "for stored release-grade client-binding proof rows"
        ),
    }

def load_private_release_snapshot(path):
    try:
        with open(path, "r", encoding="utf-8") as f:
            snapshot = json.load(f)
    except OSError as err:
        return fallback_private_release_snapshot("not_captured", str(err))
    except json.JSONDecodeError as err:
        return fallback_private_release_snapshot("invalid_json", str(err))
    if snapshot.get("schema") != "soma.real_private_app_release_snapshot.v1":
        return fallback_private_release_snapshot(
            "invalid_schema",
            "expected schema soma.real_private_app_release_snapshot.v1",
        )
    for key, default in [
        ("ready_clients", []),
        ("pending_clients", required_private_release_clients),
        ("unavailable_clients", []),
        ("required_proof_levels", private_release_required_proof_levels),
        ("clients", []),
        ("operator_primary_next_command", []),
        ("operator_blocked_claims", []),
        ("operator_safe_to_claim", []),
        ("operator_private_app_restart_commands", []),
        ("operator_private_app_collector_start_commands", []),
        ("operator_private_app_wait_commands", []),
        ("render_evidence_artifact_scans", []),
    ]:
        if not isinstance(snapshot.get(key), list):
            snapshot[key] = default
    if "ready" not in snapshot:
        snapshot["ready"] = False
    return snapshot

real_private_snapshot = load_private_release_snapshot(snapshot_path)
real_snapshot_status = str(real_private_snapshot.get("status") or "not_captured")
real_snapshot_usable = real_snapshot_status in {"ready", "partial", "pending"}
real_ready_clients = [
    str(client)
    for client in real_private_snapshot.get("ready_clients", [])
    if isinstance(client, str)
]
real_pending_clients = [
    str(client)
    for client in real_private_snapshot.get("pending_clients", [])
    if isinstance(client, str)
]
if not real_snapshot_usable:
    real_ready_clients = []
    real_pending_clients = required_private_release_clients
private_release_ready = bool(real_snapshot_usable and real_private_snapshot.get("ready"))
private_release_status = "ready" if private_release_ready else "pending"
real_operator_status = real_private_snapshot.get("operator_status")
real_operator_primary_next_step = real_private_snapshot.get("operator_primary_next_step")
real_operator_primary_next_command = [
    str(part)
    for part in real_private_snapshot.get("operator_primary_next_command", [])
    if isinstance(part, str)
]
real_render_evidence_artifact_scans = [
    scan
    for scan in real_private_snapshot.get("render_evidence_artifact_scans", [])
    if isinstance(scan, dict) and isinstance(scan.get("client"), str)
]

def list_value(value):
    return value if isinstance(value, list) else []

def object_by_client(value):
    result = {}
    for item in list_value(value):
        if isinstance(item, dict) and isinstance(item.get("client"), str):
            result[item["client"]] = item
    return result

restart_commands_by_client = object_by_client(
    real_private_snapshot.get("operator_private_app_restart_commands")
)
collector_start_commands_by_client = object_by_client(
    real_private_snapshot.get("operator_private_app_collector_start_commands")
)
wait_commands_by_client = object_by_client(real_private_snapshot.get("operator_private_app_wait_commands"))

def infer_private_app_action_id(row):
    goal_status = row.get("goal_status")
    next_step = row.get("proof_session_next_step_id")
    if goal_status == "private_app_trigger_hook_required" or next_step == "trigger_private_client_hook":
        return "trigger_real_private_client_hook_to_write_private_spool_event"
    if goal_status == "private_app_release_grade_proof_ready":
        return "client_binding_release_gate_passed"
    return None

def action_label(action_id, client):
    labels = {
        "restart_or_reopen_codex_app_before_real_hook": "Quit/reopen Codex app",
        "start_continue_devdata_collector_before_real_hook": "Start Continue dev-data collector",
        "trigger_real_private_client_hook_to_write_private_spool_event": f"Trigger real {client} hook",
        "client_binding_release_gate_passed": "Release gate passed",
    }
    return labels.get(action_id, action_id.replace("_", " ").title()) if action_id else None

real_operator_pending_actions = []
for row in list_value(real_private_snapshot.get("clients")):
    if not isinstance(row, dict) or row.get("ready_for_private_client_claim"):
        continue
    client = row.get("client")
    if not isinstance(client, str):
        continue
    restart_command = row.get("restart_command")
    if not isinstance(restart_command, dict):
        restart_command = restart_commands_by_client.get(client)
    collector_start_command = row.get("collector_start_command")
    if not isinstance(collector_start_command, dict):
        collector_start_command = collector_start_commands_by_client.get(client)
    wait_command = row.get("wait_command_card")
    if not isinstance(wait_command, dict):
        wait_command = wait_commands_by_client.get(client)
    operator_next_action_id = (
        row.get("operator_next_action_id")
        or (restart_command or {}).get("operator_next_action_id")
        or (collector_start_command or {}).get("operator_next_action_id")
        or (wait_command or {}).get("operator_next_action_id")
        or infer_private_app_action_id(row)
    )
    external_action_safety = (
        row.get("external_action_safety")
        or (restart_command or {}).get("external_action_safety")
        or (collector_start_command or {}).get("external_action_safety")
        or (wait_command or {}).get("external_action_safety")
    )
    real_operator_pending_actions.append({
        "client": client,
        "goal_status": row.get("goal_status"),
        "operator_next_action_id": operator_next_action_id,
        "operator_next_action_label": row.get("operator_next_action_label")
        or action_label(operator_next_action_id, client),
        "release_gate_blockers": list_value(row.get("release_gate_blockers")),
        "missing_proof_levels": list_value(row.get("missing_proof_levels")),
        "has_restart_command": isinstance(restart_command, dict),
        "restart_requires_separate_terminal": (restart_command or {})
        .get("execution_safety", {})
        .get("run_from_separate_terminal_required") if isinstance(restart_command, dict) else None,
        "has_collector_start_command": isinstance(collector_start_command, dict),
        "has_wait_command": isinstance(wait_command, dict),
        "render_evidence_artifact_scan": row.get("render_evidence_artifact_scan")
        if isinstance(row.get("render_evidence_artifact_scan"), dict)
        else None,
        "external_action_safety": external_action_safety
        if isinstance(external_action_safety, dict) else None,
        "trust_boundary": (
            "dogfood_real_private_app_snapshot_action_is_read_only: mirrors "
            "operator guidance from the real HOME proof-ledger snapshot but "
            "records no proof row, creates no verification event, installs no "
            "hook, and cannot satisfy private client release gates"
        ),
    })
objectives = [
    {
        "objective": "client_mcp_context_capture",
        "status": status_for([
            "client MCP readiness",
            "installed CLI wrappers",
            "installed CLI MCP registration",
            "MCP/context/capture explicit path",
            "per-client explicit MCP capture matrix",
            "per-client explicit capture matrix",
        ]),
        "evidence_sections": [
            "client MCP readiness",
            "installed CLI wrappers",
            "installed CLI MCP registration",
            "MCP/context/capture explicit path",
            "per-client explicit MCP capture matrix",
            "per-client explicit capture matrix",
        ],
    },
    {
        "objective": "semantic_learning_review",
        "status": status_for(["semantic learning review guardrail"]),
        "evidence_sections": ["semantic learning review guardrail"],
    },
    {
        "objective": "multi_terminal_persona_project_scope",
        "status": status_for([
            "multi-terminal persona/project isolation",
            "persona-local adapter spool",
        ]),
        "evidence_sections": [
            "multi-terminal persona/project isolation",
            "persona-local adapter spool",
        ],
    },
    {
        "objective": "private_client_proof_session_readiness",
        "status": status_for(["private client proof-session readiness"]),
        "evidence_sections": ["private client proof-session readiness"],
    },
]

passed_i = int(passed)
warned_i = int(warned)
failed_i = int(failed)
generated_at = datetime.datetime.now(datetime.timezone.utc)
report = {
    "schema": "soma.client_dogfood_report.v1",
    "source": "tools/client-dogfood-report.sh",
    "generated_at": generated_at.isoformat().replace("+00:00", "Z"),
    "generated_at_unix_ms": int(time.time() * 1000),
    "workspace": workspace,
    "soma_bin": soma_bin,
    "real_home": real_home or None,
    "run_home": run_home,
    "status": "fail" if failed_i else ("ready_with_warnings" if warned_i else "ready"),
    "summary": {
        "pass": passed_i,
        "warn": warned_i,
        "fail": failed_i,
    },
    "private_app_release_proof": {
        "status": private_release_status,
        "ready": private_release_ready,
        "ready_clients": real_ready_clients,
        "pending_clients": real_pending_clients,
        "required_proof_levels": private_release_required_proof_levels,
        "source_snapshot_status": real_snapshot_status,
        "blocking_reason": (
            "dogfood operator flow is proof-free; real Codex app/Cursor/Continue "
            "release readiness is replayed only from the read-only real HOME "
            "proof ledger snapshot and still requires stored release-grade "
            "app-hook, in-client-render, and review-action proof rows"
        ),
        "trust_boundary": (
            "private_app_release_proof_is_not_created_by_dogfood_report: this "
            "script can validate setup guidance, proof-session flow, and replay "
            "the real HOME proof ledger snapshot, but it records no proof row, "
            "creates no verification event, installs no hook, promotes no cloud "
            "draft, and cannot prove private client behavior beyond cited stored "
            "proof rows"
        ),
    },
    "release_private_app_proof_status": private_release_status,
    "release_private_app_proof_ready": private_release_ready,
    "release_private_app_proof_ready_clients": real_ready_clients,
    "release_private_app_proof_pending_clients": real_pending_clients,
    "release_private_app_required_proof_levels": private_release_required_proof_levels,
    "real_private_app_release_snapshot": real_private_snapshot,
    "real_private_app_release_status": real_snapshot_status,
    "real_private_app_release_ready": private_release_ready,
    "real_private_app_release_ready_clients": real_ready_clients,
    "real_private_app_release_pending_clients": real_pending_clients,
    "real_private_app_release_operator_status": real_operator_status,
    "real_private_app_release_operator_primary_next_step": real_operator_primary_next_step,
    "real_private_app_release_operator_primary_next_command": real_operator_primary_next_command,
    "real_private_app_release_pending_actions": real_operator_pending_actions,
    "real_private_app_render_evidence_artifact_scans": real_render_evidence_artifact_scans,
    "objectives": objectives,
    "sections": list(sections.values()),
    "events": events,
    "trust_boundary": (
        "client_dogfood_json_report_is_observational: records only this script's local "
        "check outcomes; it does not create client-binding proof rows, install hooks, "
        "create verification events, or promote cloud drafts"
    ),
}

target = Path(out_path)
target.parent.mkdir(parents=True, exist_ok=True)
target.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
}

run_step() {
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        pass "$name"
    else
        fail "$name"
    fi
}

expect_json_field() {
    local name="$1"
    local file="$2"
    local expr="$3"
    if python3 - "$file" "$expr" <<'PY' >/dev/null
import json
import sys

path, expr = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
helpers = {
    "__builtins__": {},
    "all": all,
    "any": any,
    "dict": dict,
    "isinstance": isinstance,
    "len": len,
    "list": list,
    "set": set,
    "str": str,
    "data": data,
}
if not eval(expr, helpers, {}):
    raise SystemExit(1)
PY
    then
        pass "$name"
    else
        fail "$name"
    fi
}

expect_text() {
    local name="$1"
    local file="$2"
    local pattern="$3"
    if grep -qE "$pattern" "$file"; then
        pass "$name"
    else
        fail "$name"
    fi
}

json_get() {
    local file="$1"
    local expr="$2"
    python3 - "$file" "$expr" <<'PY'
import json
import sys

path, expr = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
value = eval(expr, {"__builtins__": {}}, {"data": data})
if value is None:
    raise SystemExit(1)
print(value)
PY
}

pick_free_port() {
    python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

wait_for_file_pattern() {
    local file="$1"
    local pattern="$2"
    python3 - "$file" "$pattern" <<'PY'
import pathlib
import re
import sys
import time

path = pathlib.Path(sys.argv[1])
pattern = re.compile(sys.argv[2])
deadline = time.monotonic() + 10
last_text = ""
while time.monotonic() < deadline:
    try:
        last_text = path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        last_text = ""
    if pattern.search(last_text):
        raise SystemExit(0)
    time.sleep(0.1)
print(f"timed out waiting for {path} to contain {pattern.pattern}; last={last_text[-500:]}", file=sys.stderr)
raise SystemExit(1)
PY
}

loopback_ipv4_probe_status() {
    python3 - <<'PY'
import errno
import socket

try:
    with socket.create_connection(("127.0.0.1", 9), timeout=0.2):
        print("connected")
except OSError as exc:
    if exc.errno == errno.EADDRNOTAVAIL:
        print("addr_not_available")
    elif exc.errno == errno.EPERM:
        print("operation_not_permitted")
    elif exc.errno == errno.ECONNREFUSED:
        print("available")
    else:
        print(f"other_error:{exc.errno}:{exc}")
PY
}

post_continue_devdata() {
    local port="$1"
    local out="$2"
    python3 - "$port" "$out" <<'PY'
import json
import sys
import urllib.request

port, out_path = int(sys.argv[1]), sys.argv[2]
payload = {
    "name": "chatInteraction",
    "schema": "0.2.0",
    "level": "all",
    "profileId": "soma-dogfood",
    "data": {
        "prompt": "Continue live collector dogfood prompt",
        "completion": "Continue live collector dogfood response",
        "modelProvider": "soma-dogfood",
        "modelName": "dogfood-model",
        "sessionId": "continue-live-collector-dogfood",
    },
}
request = urllib.request.Request(
    f"http://127.0.0.1:{port}/continue-devdata",
    data=json.dumps(payload).encode("utf-8"),
    headers={"Content-Type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(request, timeout=5) as response:
    body = response.read().decode("utf-8")
with open(out_path, "w", encoding="utf-8") as f:
    f.write(body)
    f.write("\n")
PY
}

write_real_private_snapshot_unavailable() {
    local snapshot="$1"
    local status="$2"
    local error="$3"
    python3 - "$snapshot" "${REAL_HOME:-}" "$status" "$error" <<'PY'
import json
import sys
from pathlib import Path

snapshot_path, real_home, status, error = sys.argv[1:]
required_clients = ["codex-app", "cursor", "continue"]
report = {
    "schema": "soma.real_private_app_release_snapshot.v1",
    "source": "tools/client-dogfood-report.sh",
    "status": status,
    "real_home": real_home or None,
    "ready": False,
    "ready_clients": [],
    "pending_clients": required_clients,
    "unavailable_clients": required_clients,
    "required_proof_levels": [
        "observed_app_hook",
        "observed_in_client_render",
        "observed_review_action",
    ],
    "client_count": 0,
    "private_app_client_count": 0,
    "release_ready_count": 0,
    "clients": [],
    "operator_status": None,
    "operator_primary_next_step": None,
    "operator_primary_next_command": [],
    "operator_blocked_claims": [],
    "operator_safe_to_claim": [],
    "operator_private_app_restart_commands": [],
    "operator_private_app_collector_start_commands": [],
    "operator_private_app_wait_commands": [],
    "error": error,
    "trust_boundary": (
        "real_private_app_release_snapshot_is_read_only: records only a "
        "best-effort replay of `soma clients` under the user's real HOME; "
        "it records no proof row, creates no verification event, installs no "
        "hook, promotes no cloud draft, and cannot substitute for stored "
        "release-grade client-binding proof rows"
    ),
}
Path(snapshot_path).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
}

capture_real_private_release_snapshot() {
    section "real private app release snapshot"
    local raw="$RUN_DIR/real-private-clients.json"
    local err="$RUN_DIR/real-private-clients.err"
    if [[ -z "${REAL_HOME:-}" ]]; then
        write_real_private_snapshot_unavailable "$REAL_PRIVATE_SNAPSHOT_JSON" "no_real_home" "HOME was empty before the isolated dogfood HOME was created"
        echo "  info status=no_real_home ready_clients=none pending_clients=codex-app,cursor,continue"
        return
    fi
    if env HOME="$REAL_HOME" "$BIN" clients --client all --json --command "$BIN" >"$raw" 2>"$err"; then
        python3 - "$REAL_PRIVATE_SNAPSHOT_JSON" "$REAL_HOME" "$raw" <<'PY'
import json
import sys
from pathlib import Path

snapshot_path, real_home, raw_path = sys.argv[1:]
required_clients = ["codex-app", "cursor", "continue"]
required_proof_levels = [
    "observed_app_hook",
    "observed_in_client_render",
    "observed_review_action",
]
with open(raw_path, "r", encoding="utf-8") as f:
    data = json.load(f)
summary = data.get("summary", {}) if isinstance(data, dict) else {}
rows = data.get("clients", []) if isinstance(data, dict) else []
operator_card = data.get("operator_card", {}) if isinstance(data, dict) else {}
if not isinstance(operator_card, dict):
    operator_card = {}
proof_storage_unavailable = bool(summary.get("proof_storage_unavailable")) or (
    data.get("proof_storage_status") == "unavailable"
)
proof_storage_error = data.get("proof_storage_error") or (
    "proof storage unavailable while replaying real HOME snapshot"
    if proof_storage_unavailable
    else None
)
by_client = {
    row.get("client"): row
    for row in rows
    if isinstance(row, dict) and row.get("client") in required_clients
}

def list_value(value):
    return value if isinstance(value, list) else []

def dict_by_client(items):
    if not isinstance(items, list):
        return {}
    return {
        item.get("client"): item
        for item in items
        if isinstance(item, dict) and isinstance(item.get("client"), str)
    }

next_actions_by_client = dict_by_client(operator_card.get("private_app_next_actions"))
restart_commands_by_client = dict_by_client(operator_card.get("private_app_restart_commands"))
collector_start_commands_by_client = dict_by_client(
    operator_card.get("private_app_collector_start_commands")
)
wait_commands_by_client = dict_by_client(operator_card.get("private_app_wait_commands"))
release_plan_by_client = dict_by_client(operator_card.get("private_app_release_plan"))

def operator_action_field(client, key, default=None):
    action = next_actions_by_client.get(client)
    if isinstance(action, dict) and action.get(key) is not None:
        return action.get(key)
    plan = release_plan_by_client.get(client)
    if isinstance(plan, dict) and plan.get(key) is not None:
        return plan.get(key)
    return default

def render_evidence_artifact_scan(row):
    if not isinstance(row, dict):
        return None
    summary = row.get("artifact_repair_summary")
    if not isinstance(summary, dict):
        return None
    scan = summary.get("render_evidence_artifact_scan")
    return scan if isinstance(scan, dict) else None

snapshot_rows = []
ready_clients = []
unavailable_clients = []
render_evidence_artifact_scans = []
for client in required_clients:
    row = by_client.get(client)
    if row is None:
        unavailable_clients.append(client)
        snapshot_rows.append({
            "client": client,
            "present": False,
            "ready_for_private_client_claim": False,
            "goal_status": "missing_client_row",
            "private_capture_status": "missing_client_row",
            "proof_session_status": "missing_client_row",
            "proof_session_release_gate": "missing_client_row",
            "proof_session_next_step_id": None,
            "proof_session_runbook_step_count": 0,
            "operator_next_action_id": operator_action_field(client, "operator_next_action_id"),
            "operator_next_action_label": operator_action_field(client, "operator_next_action_label"),
            "operator_next_step": operator_action_field(client, "next_step"),
            "release_gate_blockers": list_value(operator_action_field(client, "release_gate_blockers", [])),
            "missing_proof_levels": list_value(operator_action_field(client, "missing_proof_levels", [])),
            "restart_command": restart_commands_by_client.get(client),
            "collector_start_command": collector_start_commands_by_client.get(client),
            "wait_command_card": wait_commands_by_client.get(client),
            "next_command": [],
        })
        continue
    render_scan = render_evidence_artifact_scan(row)
    if render_scan is not None:
        render_evidence_artifact_scans.append({
            "client": client,
            "source": render_scan.get("source"),
            "status": render_scan.get("status"),
            "placeholder_count": render_scan.get("placeholder_count"),
            "path": render_scan.get("path"),
            "missing_requirements": list_value(render_scan.get("missing_requirements")),
            "records_proof": bool(render_scan.get("records_proof")),
            "creates_verification_event": bool(render_scan.get("creates_verification_event")),
            "promotes_cloud_draft": bool(render_scan.get("promotes_cloud_draft")),
            "trust_boundary": render_scan.get("trust_boundary"),
        })
    ready = bool(row.get("ready_for_private_client_claim"))
    if ready:
        ready_clients.append(client)
    runbook_steps = row.get("proof_session_runbook_steps")
    if isinstance(runbook_steps, list):
        runbook_step_count = len(runbook_steps)
    else:
        runbook_step_count = int(row.get("proof_session_runbook_step_count") or 0)
    snapshot_rows.append({
        "client": client,
        "present": True,
        "ready_for_private_client_claim": ready,
        "goal_status": row.get("goal_status"),
        "private_capture_status": row.get("private_capture_status"),
        "proof_session_status": row.get("proof_session_status"),
        "proof_session_release_gate": row.get("proof_session_release_gate"),
        "proof_session_next_step_id": row.get("proof_session_next_step_id"),
        "proof_session_runbook_step_count": runbook_step_count,
        "operator_next_action_id": operator_action_field(client, "operator_next_action_id"),
        "operator_next_action_label": operator_action_field(client, "operator_next_action_label"),
        "operator_next_step": operator_action_field(client, "next_step") or row.get("next_step"),
        "release_gate_blockers": list_value(operator_action_field(client, "release_gate_blockers", [])),
        "missing_proof_levels": list_value(operator_action_field(client, "missing_proof_levels", [])),
        "restart_command": restart_commands_by_client.get(client),
        "collector_start_command": collector_start_commands_by_client.get(client),
        "wait_command_card": wait_commands_by_client.get(client),
        "next_command": row.get("next_command") or [],
        "artifact_repair_status": (
            row.get("artifact_repair_summary", {}).get("status")
            if isinstance(row.get("artifact_repair_summary"), dict)
            else None
        ),
        "render_evidence_artifact_scan": render_scan,
        "trust_boundary": row.get("trust_boundary"),
    })

if proof_storage_unavailable:
    ready_clients = []
    pending_clients = required_clients
    unavailable_clients = required_clients
    status = "unavailable"
elif not [client for client in required_clients if client not in ready_clients]:
    pending_clients = []
    status = "ready"
elif ready_clients:
    pending_clients = [client for client in required_clients if client not in ready_clients]
    status = "partial"
else:
    pending_clients = required_clients
    status = "pending"
report = {
    "schema": "soma.real_private_app_release_snapshot.v1",
    "source": "tools/client-dogfood-report.sh",
    "status": status,
    "real_home": real_home or None,
    "ready": not pending_clients and not proof_storage_unavailable,
    "ready_clients": ready_clients,
    "pending_clients": pending_clients,
    "unavailable_clients": unavailable_clients,
    "required_proof_levels": required_proof_levels,
    "client_count": int(summary.get("client_count") or len(rows)),
    "private_app_client_count": int(summary.get("private_app_client_count") or len(snapshot_rows)),
    "release_ready_count": len(ready_clients),
    "proof_storage_status": data.get("proof_storage_status"),
    "proof_storage_unavailable": proof_storage_unavailable,
    "clients": snapshot_rows,
    "operator_status": operator_card.get("status"),
    "operator_primary_next_step": operator_card.get("primary_next_step"),
    "operator_primary_next_command": list_value(operator_card.get("primary_next_command")),
    "operator_blocked_claims": list_value(operator_card.get("blocked_claims")),
    "operator_safe_to_claim": list_value(operator_card.get("safe_to_claim")),
    "operator_private_app_restart_commands": list_value(
        operator_card.get("private_app_restart_commands")
    ),
    "operator_private_app_collector_start_commands": list_value(
        operator_card.get("private_app_collector_start_commands")
    ),
    "operator_private_app_wait_commands": list_value(operator_card.get("private_app_wait_commands")),
    "render_evidence_artifact_scans": render_evidence_artifact_scans,
    "error": proof_storage_error,
    "trust_boundary": (
        "real_private_app_release_snapshot_is_read_only: replays `soma clients` "
        "under the user's real HOME only to cite existing proof-ledger state; it "
        "records no proof row, creates no verification event, installs no hook, "
        "promotes no cloud draft, and cannot substitute for stored release-grade "
        "client-binding proof rows"
    ),
}
Path(snapshot_path).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
        local snapshot_status
        local ready_clients
        local pending_clients
        snapshot_status="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "data['status']" 2>/dev/null || printf unavailable)"
        ready_clients="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "','.join(data.get('ready_clients') or ['none'])" 2>/dev/null || printf none)"
        pending_clients="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "','.join(data.get('pending_clients') or ['none'])" 2>/dev/null || printf unknown)"
        echo "  info status=$snapshot_status ready_clients=$ready_clients pending_clients=$pending_clients"
    else
        local error_text
        error_text="$(tr '\n' ' ' <"$err" | sed 's/[[:space:]]\{1,\}/ /g' | cut -c 1-500)"
        write_real_private_snapshot_unavailable "$REAL_PRIVATE_SNAPSHOT_JSON" "unavailable" "$error_text"
        echo "  info status=unavailable ready_clients=none pending_clients=codex-app,cursor,continue"
    fi
}

print_mcp_readiness() {
    local client="$1"
    local file="$2"
    python3 - "$client" "$file" <<'PY'
import json
import sys

client, path = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
check = data["check"]
readiness = check["readiness"]
runtime = readiness["client_runtime"]
card = readiness["card"]
runtime_path = runtime.get("path") or "-"
print(
    f"    {client}: valid={check['valid']} "
    f"readiness={readiness['status']} "
    f"runtime={runtime['target']}:{runtime['status']} path={runtime_path} "
    f"private_capture_ready={readiness['private_capture_ready']}"
)
print(f"      card: {card['headline']}")
launch_probe_command = runtime.get("launch_probe_command")
if launch_probe_command:
    print(f"      runtime_launch_probe: {' '.join(str(part) for part in launch_probe_command)}")
launch_probe_note = runtime.get("launch_probe_note")
if launch_probe_note:
    print(f"      runtime_launch_probe_note: {launch_probe_note}")
print(f"      next: {readiness['next_step']}")
PY
}

print_real_render_evidence_scan_summary() {
    local file="$1"
    python3 - "$file" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
scans = data.get("render_evidence_artifact_scans")
if not isinstance(scans, list) or not scans:
    print("  info render_evidence_scans=none")
    raise SystemExit(0)
for scan in scans:
    if not isinstance(scan, dict):
        continue
    client = scan.get("client") or "unknown"
    status = scan.get("status") or "unknown"
    placeholders = scan.get("placeholder_count")
    path = scan.get("path") or "-"
    print(
        "  info render_evidence_scan "
        f"client={client} status={status} placeholders={placeholders} path={path}"
    )
PY
}

wrapper_version_check() {
    local label="$1"
    local runtime_path="$2"
    local wrapper="$3"
    local env_name="$4"
    if [[ -z "$runtime_path" || "$runtime_path" == "-" ]]; then
        warn "$label runtime missing; wrapper launch skipped"
        return
    fi
    if env SOMA_BIN="$BIN" "$env_name=$runtime_path" "$wrapper" --version >/dev/null 2>&1; then
        pass "$label wrapper launches installed runtime"
    else
        fail "$label wrapper failed to launch installed runtime"
    fi
}

wrapper_scope_env_check() {
    local label="$1"
    local runtime_path="$2"
    local wrapper="$3"
    local env_name="$4"
    local expected_client="$5"
    if [[ -z "$runtime_path" || "$runtime_path" == "-" ]]; then
        warn "$label runtime missing; wrapper scope-env check skipped"
        return
    fi
    local report="$RUN_DIR/${expected_client}.wrapper-env.txt"
    local db_path="$RUN_DIR/${expected_client}.wrapper-scope.db"
    if env \
        SOMA_BIN="$BIN" \
        SOMA_DB="$db_path" \
        SOMA_PROJECT="SOMA" \
        "$env_name=/usr/bin/env" \
        "$wrapper" >"$report" 2>"$report.err"; then
        pass "$label wrapper emits child environment"
        expect_text "$label wrapper exports SOMA_CLIENT" "$report" \
            "^SOMA_CLIENT=${expected_client}$"
        expect_text "$label wrapper exports SOMA_PROJECT" "$report" \
            "^SOMA_PROJECT=SOMA$"
        expect_text "$label wrapper exports SOMA_DB" "$report" \
            "^SOMA_DB=${db_path}$"
        expect_text "$label wrapper exports SOMA_SESSION_ID" "$report" \
            "^SOMA_SESSION_ID=soma-${expected_client}-soma-[0-9]+-[0-9]+$"
    else
        fail "$label wrapper failed to emit child environment"
    fi
}

echo "=== SOMA real-client dogfood report ==="
echo "workspace: $ROOT"
echo "soma_bin:  $BIN"
echo "real_home: ${REAL_HOME:-<unset>}"
echo "run_home:  $HOME"
echo

capture_real_private_release_snapshot
print_real_render_evidence_scan_summary "$REAL_PRIVATE_SNAPSHOT_JSON"
expect_json_field "real private snapshot render evidence scans stay proof-free" \
    "$REAL_PRIVATE_SNAPSHOT_JSON" \
    "all(scan.get('records_proof') is False and scan.get('creates_verification_event') is False and scan.get('promotes_cloud_draft') is False and 'records no proof row' in scan.get('trust_boundary', '') for scan in data.get('render_evidence_artifact_scans', []))"
expect_json_field "real private snapshot render evidence scans preserve placeholder status" \
    "$REAL_PRIVATE_SNAPSHOT_JSON" \
    "all(scan.get('status') in {'missing_file', 'invalid_json', 'unreadable', 'missing_path', 'template_placeholders_present', 'observation_incomplete', 'filled_observation_candidate'} and isinstance(scan.get('missing_requirements', []), list) for scan in data.get('render_evidence_artifact_scans', []))"

echo
section "client MCP readiness"
for client in claude-code codex-cli codex-app cursor continue; do
    report="$RUN_DIR/${client}.mcp-check.json"
    if "$BIN" mcp-config --client "$client" --command "$BIN" --check >"$report"; then
        pass "mcp-config --check $client"
        print_mcp_readiness "$client" "$report"
        runtime_status="$(json_get "$report" "data['check']['readiness']['client_runtime']['status']" 2>/dev/null || printf unknown)"
        if [[ "$runtime_status" == "missing" ]]; then
            warn "$client runtime missing; MCP registration is checked but wrapper/app launch is not dogfooded"
        fi
        expect_json_field "$client keeps MCP registration ready separate" "$report" \
            "data['check']['readiness']['mcp_registration_ready'] is True"
        expect_json_field "$client does not claim private capture" "$report" \
            "data['check']['readiness']['private_capture_ready'] is False"
        brief_report="$RUN_DIR/${client}.mcp-check.brief.txt"
        if "$BIN" mcp-config --client "$client" --command "$BIN" --check --brief >"$brief_report"; then
            pass "mcp-config --check --brief $client"
            expect_text "$client brief names MCP config handoff" "$brief_report" \
                'SOMA MCP config brief'
            expect_text "$client brief does not claim private capture" "$brief_report" \
                'private_capture_ready: false'
            expect_text "$client brief names cloud draft promotion blocker" "$brief_report" \
                'cloud draft truth, verification, or L3/L4 promotion'
            if [[ "$client" == "codex-app" || "$client" == "cursor" || "$client" == "continue" ]]; then
                expect_text "$client brief points at proof-session brief" "$brief_report" \
                    "soma adapter-binding-proof --proof-session --client $client --brief"
            fi
        else
            fail "mcp-config --check --brief $client"
        fi
    else
        fail "mcp-config --check $client"
    fi
done

aggregate_brief="$RUN_DIR/all.mcp-check.brief.txt"
if "$BIN" mcp-config --all --command "$BIN" --check --brief >"$aggregate_brief"; then
    pass "mcp-config --all --check --brief"
    expect_text "aggregate brief covers five clients" "$aggregate_brief" \
        'clients: 5'
    expect_text "aggregate brief keeps private capture unproven" "$aggregate_brief" \
        'private_capture_unproven: 5'
    expect_text "aggregate brief lists private proof-session command" "$aggregate_brief" \
        "proof_session: $BIN_ABS adapter-binding-proof --proof-session --client continue --brief"
else
    fail "mcp-config --all --check --brief"
fi

codex_runtime="$(json_get "$RUN_DIR/codex-cli.mcp-check.json" "data['check']['readiness']['client_runtime'].get('path')" 2>/dev/null || printf '-')"
claude_runtime="$(json_get "$RUN_DIR/claude-code.mcp-check.json" "data['check']['readiness']['client_runtime'].get('path')" 2>/dev/null || printf '-')"

echo
section "installed CLI wrappers"
wrapper_version_check "Codex CLI" "$codex_runtime" "$ROOT/tools/soma-codex-cli.sh" CODEX_BIN
wrapper_version_check "Claude Code CLI" "$claude_runtime" "$ROOT/tools/soma-claude-code-cli.sh" CLAUDE_CODE_BIN
wrapper_scope_env_check "Codex CLI" "$codex_runtime" "$ROOT/tools/soma-codex-cli.sh" CODEX_BIN codex-cli
wrapper_scope_env_check "Claude Code CLI" "$claude_runtime" "$ROOT/tools/soma-claude-code-cli.sh" CLAUDE_CODE_BIN claude-code

echo
section "installed CLI MCP registration"
codex_cli_mcp_registration() {
    if [[ -z "$codex_runtime" || "$codex_runtime" == "-" ]]; then
        return 1
    fi
    local home="$RUN_DIR/codex-cli-mcp-home"
    mkdir -p "$home"
    env HOME="$home" "$codex_runtime" mcp add soma -- "$BIN" mcp-serve >/dev/null
    env HOME="$home" "$codex_runtime" mcp list >"$home/mcp-list.txt"
    grep -q "soma" "$home/mcp-list.txt"
    env HOME="$home" "$codex_runtime" mcp get soma >"$home/mcp-get.txt"
    grep -q "mcp-serve" "$home/mcp-get.txt"
}
claude_code_mcp_registration() {
    if [[ -z "$claude_runtime" || "$claude_runtime" == "-" ]]; then
        return 1
    fi
    local home="$RUN_DIR/claude-code-mcp-home"
    mkdir -p "$home"
    env HOME="$home" "$claude_runtime" mcp add soma -- "$BIN" mcp-serve >/dev/null
    env HOME="$home" "$claude_runtime" mcp list >"$home/mcp-list.txt"
    grep -q "soma:" "$home/mcp-list.txt"
    env HOME="$home" "$claude_runtime" mcp get soma >"$home/mcp-get.txt"
    grep -q "mcp-serve" "$home/mcp-get.txt"
}
if [[ -z "$codex_runtime" || "$codex_runtime" == "-" ]]; then
    warn "Codex CLI runtime missing; real MCP registration skipped"
else
    run_step "Codex CLI accepts SOMA MCP registration" codex_cli_mcp_registration
fi
if [[ -z "$claude_runtime" || "$claude_runtime" == "-" ]]; then
    warn "Claude Code CLI runtime missing; real MCP registration skipped"
else
    run_step "Claude Code CLI accepts SOMA MCP registration" claude_code_mcp_registration
fi

echo
section "MCP/context/capture explicit path"
mcp_initialize() {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
        | "$BIN" mcp-serve 2>/dev/null | head -1 | grep -q '"protocolVersion"'
}
run_step "mcp-serve initializes" mcp_initialize
run_step "explicit capture writes a turn" "$BIN" ingest --source codex-cli \
    --prompt "dogfood explicit capture prompt" \
    --response "dogfood explicit capture response" \
    --project dogfood-explicit \
    --session dogfood-explicit-session
run_step "context render sees explicit project" "$BIN" context render \
    --project dogfood-explicit \
    --format json

echo
section "per-client explicit MCP capture matrix"
mcp_capture_for_client() {
    local client="$1"
    local session="dogfood-${client}-mcp-session"
    local token="dogfood-${client}-mcp-token"
    printf '{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"soma_capture_turn","arguments":{"source":"%s","project":"dogfood-mcp-matrix","session_id":"%s","prompt_text":"%s prompt","response_text":"%s response"}}}\n' \
        "$client" "$session" "$token" "$token" \
        | "$BIN" mcp-serve 2>/dev/null \
        | head -1 \
        | grep -q "$session"
    "$BIN" recall \
        --query "$token" \
        --limit 3 \
        --format json \
        | grep -q "$token"
}
for client in claude-code codex-cli codex-app cursor continue; do
    run_step "explicit MCP capture/recall $client" mcp_capture_for_client "$client"
done

echo
section "per-client explicit capture matrix"
adapter_capture_for_client() {
    local client="$1"
    local session="dogfood-${client}-explicit-session"
    local token="dogfood-${client}-explicit-adapter-token"
    printf '{"source":"%s","project":"dogfood-client-matrix","session_id":"%s","prompt_text":"%s prompt","response_text":"%s response"}' \
        "$client" "$session" "$token" "$token" \
        | "$BIN" adapter-capture --json - >/dev/null
    "$BIN" recall \
        --query "$token" \
        --limit 3 \
        --format json \
        | grep -q "$token"
}
for client in claude-code codex-cli codex-app cursor continue; do
    run_step "explicit adapter capture/recall $client" adapter_capture_for_client "$client"
done

echo
section "semantic learning review guardrail"
semantic_db="$RUN_DIR/semantic-review.db"
semantic_payload="$RUN_DIR/semantic-cloud-draft-payload.json"
semantic_capture="$RUN_DIR/semantic-cloud-draft-capture.json"
semantic_learning="$RUN_DIR/semantic-learning.json"
semantic_learning_brief="$RUN_DIR/semantic-learning.brief.txt"
semantic_clients="$RUN_DIR/semantic-clients.json"
semantic_render="$RUN_DIR/semantic-review-render.json"
semantic_hardening="$RUN_DIR/semantic-hardening.json"
python3 - "$semantic_payload" <<'PY'
import json
import sys

payload_path = sys.argv[1]
claim = "Dogfood cloud draft must stay blocked until local verification."
with open(payload_path, "w", encoding="utf-8") as f:
    json.dump(
        {
            "task_frame_query": "Dogfood semantic review cloud draft blocker",
            "project": "dogfood-semantic",
            "session_id": "dogfood-semantic-session",
            "client": "codex-cli",
            "output_text": claim,
            "extracted_claims": [{"text": claim}],
            "proposal_action": "propose_promotion",
            "proposal_target_lifecycle_state": "long_term_memory",
            "proposal_reason": "dogfood semantic review keeps cloud draft unpromoted",
            "enqueue_proposal": True,
            "allow_local_private_projection": True,
            "local_private_projection_reason": (
                "client dogfood uses an isolated synthetic TaskFrame only to prove "
                "review UX and cloud_draft blocking"
            ),
        },
        f,
        separators=(",", ":"),
    )
    f.write("\n")
PY
if "$BIN" adapter-cloud-output --json "$semantic_payload" --db-path "$semantic_db" >"$semantic_capture"; then
    pass "semantic dogfood captures cloud output as draft"
    expect_json_field "semantic cloud output capture creates draft claim" "$semantic_capture" \
        "len(data['claim_ids']) >= 1 and data['verification_event_ids'] == [] and data['proposal_id'] is not None and data['trust_boundary'] == 'cloud_output_is_cloud_draft_until_verified'"
else
    fail "semantic dogfood captures cloud output as draft"
fi
if "$BIN" learning \
    --project dogfood-semantic \
    --session-id dogfood-semantic-session \
    --client codex-cli \
    --json \
    --db-path "$semantic_db" >"$semantic_learning"; then
    pass "semantic learning status surfaces cloud draft blocker"
    expect_json_field "learning status blocks cloud draft before L3/L4" "$semantic_learning" \
        "data['summary']['cloud_draft_blocked_count'] >= 1 and data['review_surface']['primary_surface'] == 'review_render' and len(data['cloud_draft_blockers']) >= 1"
    expect_json_field "learning operator card prioritizes cloud draft review" "$semantic_learning" \
        "data['operator_card']['source'] == 'soma_learning.operator_card.v1' and data['operator_card']['status'] == 'cloud_draft_blocked' and data['operator_card']['review_surface'] == 'review_render' and data['operator_card']['review_counts']['cloud_draft_blockers'] >= 1 and 'review-render' in ' '.join(data['operator_card']['primary_next_command']) and 'records no proof' in data['operator_card']['trust_boundary']"
    expect_json_field "learning cloud draft blocker exposes evidence-gated action" "$semantic_learning" \
        "any('soma_review_action' in blocker.get('mcp_tools', []) and 'cloud_draft_blocker_is_review_only' in blocker.get('trust_boundary', '') for blocker in data['cloud_draft_blockers'])"
    expect_json_field "learning review cards expose cloud draft as blocked card" "$semantic_learning" \
        "any(card.get('source') == 'soma_learning.review_card.v1' and card.get('lane') == 'cloud_draft_blockers' and card.get('target') == 'cloud_draft' and card.get('status') == 'blocked_until_independent_verification' and card.get('blocks_l4_promotion') is True and 'review-action' in card.get('primary_command', []) and 'cannot become L3/L4' in card.get('trust_boundary', '') for card in data.get('review_cards', []))"
    expect_json_field "learning review cards expose projection and evidence policy" "$semantic_learning" \
        "any(card.get('lane') == 'cloud_draft_blockers' and 'review_queue_blocker' in card.get('projection_path', '') and 'independent' in card.get('evidence_rule', '') and 'cloud draft text is forbidden' in card.get('evidence_rule', '') and 'local_observation' in card.get('accepted_verifier_types', []) and 'cloud_draft' in card.get('forbidden_evidence_sources', []) and 'client_binding_status' in card.get('forbidden_evidence_sources', []) for card in data.get('review_cards', []))"
    expect_json_field "learning promotion matrix blocks cloud drafts" "$semantic_learning" \
        "any(row.get('source') == 'soma_learning.promotion_matrix.v1' and row.get('target') == 'cloud_draft' and row.get('status') == 'blocked_until_independent_verification' and row.get('blocks_l4_promotion') is True and 'independent' in row.get('required_evidence', '') and 'forbidden evidence' in row.get('required_evidence', '') for row in data.get('promotion_matrix', []))"
    expect_json_field "learning review lanes expose lifecycle queues" "$semantic_learning" \
        "all(any(row.get('lane') == lane for row in data.get('review_lanes', [])) for lane in ['l4_semantic_fact_candidates', 'cloud_draft_blockers', 'policy_projection', 'belief_review']) and any(row.get('lane') == 'cloud_draft_blockers' and row.get('status') == 'blocked_until_independent_verification' and row.get('count', 0) >= 1 and 'cannot promote to L3/L4' in row.get('trust_boundary', '') for row in data.get('review_lanes', [])) and any(row.get('lane') == 'l4_semantic_fact_candidates' and any(part == 'semantic-proposals' for part in row.get('command', [])) for row in data.get('review_lanes', [])) and any(row.get('lane') == 'belief_review' and any(part == 'review-digest' for part in row.get('command', [])) for row in data.get('review_lanes', []))"
else
    fail "semantic learning status surfaces cloud draft blocker"
fi
if "$BIN" learning \
    --project dogfood-semantic \
    --session-id dogfood-semantic-session \
    --client codex-cli \
    --brief \
    --db-path "$semantic_db" >"$semantic_learning_brief"; then
    pass "semantic learning brief surfaces cloud draft blocker"
    expect_text "learning brief names semantic handoff" "$semantic_learning_brief" \
        'SOMA semantic learning brief'
    expect_text "learning brief blocks cloud draft before L3/L4" "$semantic_learning_brief" \
        'status: cloud_draft_blocked'
    expect_text "learning brief points at review render" "$semantic_learning_brief" \
        "render: $BIN_ABS context review-render"
    expect_text "learning brief names no semantic write boundary" "$semantic_learning_brief" \
        'writes no semantic_fact'
    expect_text "learning brief lists semantic review lanes" "$semantic_learning_brief" \
        'review_lanes:'
    expect_text "learning brief lists cloud draft blocker lane" "$semantic_learning_brief" \
        'cloud_draft_blockers status=blocked_until_independent_verification'
    expect_text "learning brief lists L4 candidate lane" "$semantic_learning_brief" \
        'l4_semantic_fact_candidates'
    expect_text "learning brief lists belief review lane" "$semantic_learning_brief" \
        'belief_review'
else
    fail "semantic learning brief surfaces cloud draft blocker"
fi
if "$BIN" clients --json --client codex-cli --command "$BIN" --db-path "$semantic_db" >"$semantic_clients"; then
    pass "client readiness mirrors semantic review blocker"
    expect_json_field "clients semantic review status is blocked" "$semantic_clients" \
        "data['semantic_review']['status'] == 'blocked_cloud_draft_verification' and data['semantic_review']['primary_surface'] == 'review_render' and data['semantic_review']['cloud_draft_blocked_count'] >= 1"
    expect_json_field "clients semantic review next command renders review controls" "$semantic_clients" \
        "'review-render' in ' '.join(data['semantic_review']['review_render_command']) and 'soma_review_render' in data['semantic_review']['next_mcp_tools']"
    expect_json_field "clients semantic review mirrors review cards" "$semantic_clients" \
        "any(card.get('source') == 'soma_clients.semantic_review_card.v1' and card.get('lane') == 'cloud_draft_blockers' and card.get('target') == 'cloud_draft' and card.get('status') == 'blocked_until_independent_verification' and card.get('blocks_l4_promotion') is True and 'review-action' in card.get('primary_command', []) and 'promotes no cloud draft' in card.get('trust_boundary', '') for card in data['semantic_review'].get('review_cards', []))"
    expect_json_field "clients semantic review mirrors review card evidence policy" "$semantic_clients" \
        "any(card.get('source') == 'soma_clients.semantic_review_card.v1' and card.get('lane') == 'cloud_draft_blockers' and 'verified_l3_candidate_only_after_independent_evidence' in card.get('projection_path', '') and 'cloud draft text is forbidden' in card.get('evidence_rule', '') and 'correction' in card.get('accepted_verifier_types', []) and 'cloud_output_text' in card.get('forbidden_evidence_sources', []) for card in data['semantic_review'].get('review_cards', []))"
    expect_json_field "clients semantic review mirrors promotion matrix" "$semantic_clients" \
        "any(row.get('source') == 'soma_clients.semantic_promotion_matrix.v1' and row.get('target') == 'cloud_draft' and row.get('status') == 'blocked_until_independent_verification' and row.get('blocks_l4_promotion') is True and 'promotes no cloud draft' in row.get('trust_boundary', '') for row in data['semantic_review'].get('promotion_matrix', []))"
    expect_json_field "clients semantic review mirrors review lanes" "$semantic_clients" \
        "all(any(row.get('source') == 'soma_clients.semantic_review_lane.v1' and row.get('lane') == lane and 'mirrors soma learning review_lanes' in row.get('trust_boundary', '') and 'promotes no cloud draft' in row.get('trust_boundary', '') for row in data['semantic_review'].get('review_lanes', [])) for lane in ['l4_semantic_fact_candidates', 'cloud_draft_blockers', 'policy_projection', 'belief_review']) and any(row.get('lane') == 'cloud_draft_blockers' and row.get('status') == 'blocked_until_independent_verification' and row.get('count', 0) >= 1 and 'do not use cloud output as evidence' in row.get('next_action', '') for row in data['semantic_review'].get('review_lanes', []))"
else
    fail "client readiness mirrors semantic review blocker"
fi
semantic_clients_brief="$RUN_DIR/semantic-clients.brief.txt"
if "$BIN" clients --brief --client codex-cli --command "$BIN" --db-path "$semantic_db" >"$semantic_clients_brief"; then
    pass "client readiness brief mirrors semantic review lanes"
    expect_text "clients brief lists semantic lanes" "$semantic_clients_brief" \
        'semantic lanes:'
    expect_text "clients brief lists cloud draft blocker lane" "$semantic_clients_brief" \
        'cloud_draft_blockers status=blocked_until_independent_verification'
    expect_text "clients brief lists L4 candidate lane" "$semantic_clients_brief" \
        'l4_semantic_fact_candidates'
    expect_text "clients brief lists belief review lane" "$semantic_clients_brief" \
        'belief_review'
else
    fail "client readiness brief mirrors semantic review lanes"
fi
if "$BIN" context review-render \
    --project dogfood-semantic \
    --session-id dogfood-semantic-session \
    --client cursor \
    --format json \
    --db-path "$semantic_db" >"$semantic_render"; then
    pass "review-render compiles semantic blocker controls"
    expect_json_field "review-render exposes pending claim action controls" "$semantic_render" \
        "data['client'] == 'cursor' and data['workbench']['counts']['pending_claims'] >= 1 and data['workbench']['counts']['evidence_required_actions'] >= 1 and len(data['interaction_contract']['actions']) >= 1"
    expect_json_field "review-render keeps cloud draft evidence forbidden" "$semantic_render" \
        "'cloud_draft' in data['workbench']['evidence_policy']['forbidden_evidence_sources'] and 'do_not_submit_cloud_draft_as_evidence' in data['interaction_contract']['global_guardrails']"
else
    fail "review-render compiles semantic blocker controls"
fi
if "$BIN" context hardening-report \
    --project dogfood-semantic \
    --session-id dogfood-semantic-session \
    --client cursor \
    --require-review-queue-clear \
    --skip-client-binding \
    --db-path "$semantic_db" >"$semantic_hardening"; then
    pass "hardening blocks release on semantic cloud draft"
    expect_json_field "hardening semantic review backlog is blocking" "$semantic_hardening" \
        "data['passed'] is False and data['review_queue_clear_required'] is True and data['review_backlog']['semantic_review_status'] == 'blocked_cloud_draft_verification' and data['review_backlog']['cloud_draft_blocked_count'] >= 1"
    expect_json_field "hardening control plan points to semantic review render" "$semantic_hardening" \
        "any(step.get('gate') == 'review_backlog_clear' and step.get('primary_mcp_tool') == 'soma_review_render' and any(check.get('check_id') == 'render_learning_status' for check in step.get('preflight_checks', [])) for step in data['control_plan']['steps'])"
else
    fail "hardening blocks release on semantic cloud draft"
fi

echo
section "multi-terminal persona/project isolation"
terminal_a() {
    eval "$("$BIN" call dogfood_alpha --create --shell bash)"
    printf '%s\n' "$SOMA_DB" >"$RUN_DIR/alpha-db.txt"
    eval "$("$BIN" session start --client codex-cli --project dogfood-project-a --shell bash)"
    printf '%s\n' "$SOMA_SESSION_ID" >"$RUN_DIR/alpha-session.txt"
    "$BIN" session status --json >"$RUN_DIR/alpha-session-status.json"
    "$BIN" ingest --source terminal --command "alpha terminal dogfood token" --exit-code 0
    eval "$("$BIN" session start --client codex-cli --project dogfood-project-c --shell bash)"
    printf '%s\n' "$SOMA_SESSION_ID" >"$RUN_DIR/alpha-project-c-session.txt"
    "$BIN" session status --json >"$RUN_DIR/alpha-project-c-session-status.json"
    "$BIN" ingest --source terminal --command "alpha second project token" --exit-code 0
    "$BIN" inspect episode --id 1 --format json >"$RUN_DIR/alpha-episode-1.json"
    "$BIN" inspect episode --id 2 --format json >"$RUN_DIR/alpha-episode-2.json"
    "$BIN" recall --query "alpha terminal dogfood token" --project dogfood-project-a \
        --limit 5 --format json >"$RUN_DIR/alpha-project-a-recall.json"
    "$BIN" recall --query "alpha second project token" --project dogfood-project-c \
        --session-id "$SOMA_SESSION_ID" --limit 5 --format json \
        >"$RUN_DIR/alpha-project-c-session-recall.json"
    "$BIN" recall --query "alpha second project token" --project dogfood-project-a \
        --limit 5 --format json >"$RUN_DIR/alpha-project-a-negative-recall.json"
    "$BIN" projects --json >"$RUN_DIR/alpha-projects.json"
    "$BIN" projects --brief >"$RUN_DIR/alpha-projects.brief.txt"
    "$BIN" projects --project dogfood-project-c --json \
        >"$RUN_DIR/alpha-project-c-provenance.json"
    "$BIN" projects --project dogfood-project-c --format brief \
        >"$RUN_DIR/alpha-project-c-provenance.brief.txt"
}
terminal_b() {
    eval "$("$BIN" call dogfood_beta --create --shell bash)"
    printf '%s\n' "$SOMA_DB" >"$RUN_DIR/beta-db.txt"
    eval "$("$BIN" session start --client claude-code --project dogfood-project-b --shell bash)"
    printf '%s\n' "$SOMA_SESSION_ID" >"$RUN_DIR/beta-session.txt"
    "$BIN" session status --json >"$RUN_DIR/beta-session-status.json"
    "$BIN" ingest --source terminal --command "beta terminal dogfood token" --exit-code 0
    "$BIN" inspect episode --id 1 --format json >"$RUN_DIR/beta-episode-1.json"
}
run_step "terminal A creates isolated persona/session/project" terminal_a
run_step "terminal B creates isolated persona/session/project" terminal_b

alpha_db="$(cat "$RUN_DIR/alpha-db.txt")"
beta_db="$(cat "$RUN_DIR/beta-db.txt")"
alpha_session="$(cat "$RUN_DIR/alpha-session.txt")"
alpha_project_c_session="$(cat "$RUN_DIR/alpha-project-c-session.txt")"
beta_session="$(cat "$RUN_DIR/beta-session.txt")"

if [[ "$alpha_db" != "$beta_db" ]]; then
    pass "persona DB paths differ"
else
    fail "persona DB paths differ"
fi
if [[ "$alpha_session" != "$beta_session" ]]; then
    pass "terminal session ids differ"
else
    fail "terminal session ids differ"
fi
if [[ "$alpha_session" != "$alpha_project_c_session" ]]; then
    pass "same persona uses separate sessions for separate projects"
else
    fail "same persona uses separate sessions for separate projects"
fi

expect_json_field "alpha first project provenance retained" "$RUN_DIR/alpha-episode-1.json" \
    "data['project'] == 'dogfood-project-a' and data['session_id'] == '$alpha_session'"
expect_json_field "alpha second project provenance retained in same persona" "$RUN_DIR/alpha-episode-2.json" \
    "data['project'] == 'dogfood-project-c' and data['session_id'] == '$alpha_project_c_session'"
expect_json_field "beta project provenance retained" "$RUN_DIR/beta-episode-1.json" \
    "data['project'] == 'dogfood-project-b' and data['session_id'] == '$beta_session'"
expect_json_field "alpha session status exposes persona-local scope" "$RUN_DIR/alpha-session-status.json" \
    "data['scope']['session_id'] == '$alpha_session' and data['scope']['client'] == 'codex-cli' and data['scope']['project'] == 'dogfood-project-a' and data['persona_scope']['active_persona'] == 'dogfood_alpha' and data['persona_scope']['activation_status'] == 'active_persona_isolated_store' and data['persona_scope']['db_path'] == '$alpha_db' and data['persona_scope'].get('adapter_spool_jsonl', '').endswith('/dogfood_alpha/adapter/events.jsonl')"
expect_json_field "alpha project C session status exposes same persona separate project" "$RUN_DIR/alpha-project-c-session-status.json" \
    "data['scope']['session_id'] == '$alpha_project_c_session' and data['scope']['client'] == 'codex-cli' and data['scope']['project'] == 'dogfood-project-c' and data['persona_scope']['active_persona'] == 'dogfood_alpha' and data['persona_scope']['db_path'] == '$alpha_db'"
expect_json_field "beta session status exposes separate persona-local scope" "$RUN_DIR/beta-session-status.json" \
    "data['scope']['session_id'] == '$beta_session' and data['scope']['client'] == 'claude-code' and data['scope']['project'] == 'dogfood-project-b' and data['persona_scope']['active_persona'] == 'dogfood_beta' and data['persona_scope']['activation_status'] == 'active_persona_isolated_store' and data['persona_scope']['db_path'] == '$beta_db' and data['persona_scope'].get('adapter_spool_jsonl', '').endswith('/dogfood_beta/adapter/events.jsonl')"
expect_json_field "alpha DB did not receive beta terminal token" "$RUN_DIR/alpha-episode-1.json" \
    "'beta terminal dogfood token' not in str(data)"
expect_json_field "beta DB did not receive alpha terminal token" "$RUN_DIR/beta-episode-1.json" \
    "'alpha terminal dogfood token' not in str(data)"
expect_json_field "alpha project-scoped recall keeps project A hits" "$RUN_DIR/alpha-project-a-recall.json" \
    "data['project'] == 'dogfood-project-a' and len(data['hits']) >= 1 and not any(hit.get('project') != 'dogfood-project-a' for hit in data['hits']) and any('alpha terminal dogfood token' in hit.get('preview', '') for hit in data['hits'])"
expect_json_field "alpha session/project recall keeps project C in same persona" "$RUN_DIR/alpha-project-c-session-recall.json" \
    "data['project'] == 'dogfood-project-c' and data['session_id'] == '$alpha_project_c_session' and len(data['hits']) >= 1 and not any(hit.get('project') != 'dogfood-project-c' or hit.get('session_id') != '$alpha_project_c_session' for hit in data['hits']) and any('alpha second project token' in hit.get('preview', '') for hit in data['hits'])"
expect_json_field "alpha project-scoped recall excludes other project memory" "$RUN_DIR/alpha-project-a-negative-recall.json" \
    "not any('alpha second project token' in hit.get('preview', '') for hit in data['hits'])"
expect_json_field "alpha projects view keeps both project experiences in one persona" "$RUN_DIR/alpha-projects.json" \
    "data['active_persona'] == 'dogfood_alpha' and data['project_count'] == 2 and data['scoped_episode_count'] == 2 and any(row.get('project') == 'dogfood-project-a' and row.get('session_count') == 1 for row in data['projects']) and any(row.get('project') == 'dogfood-project-c' and row.get('session_count') == 1 for row in data['projects'])"
expect_json_field "alpha projects scope integrity keeps sessions project-local" "$RUN_DIR/alpha-projects.json" \
    "data['scope_integrity']['project_provenance_status'] == 'complete' and data['scope_integrity']['session_project_status'] == 'single_project_sessions' and data['scope_integrity']['cross_project_session_count'] == 0 and data['scope_integrity']['cross_project_sessions'] == []"
expect_json_field "alpha scope verification index proves current scoped terminal and clean sessions" "$RUN_DIR/alpha-projects.json" \
    "data['scope_verification']['source'] == 'soma_projects.scope_verification_index.v1' and data['scope_verification']['current_scope_ready'] is True and data['scope_verification']['current_scope_status'] == 'project_scoped_capture_ready' and data['scope_verification']['active_persona'] == 'dogfood_alpha' and data['scope_verification']['current_project'] == 'dogfood-project-c' and data['scope_verification']['current_session_id'] == '$alpha_project_c_session' and data['scope_verification']['project_provenance_status'] == 'complete' and data['scope_verification']['session_project_status'] == 'single_project_sessions' and data['scope_verification']['cross_project_session_count'] == 0 and data['scope_verification']['scope_review_status'] == 'ready' and data['scope_verification']['clean_capture_commands']"
expect_text "alpha projects brief shows project-scoped terminal ready" "$RUN_DIR/alpha-projects.brief.txt" \
    'Current scope: status=project_scoped_capture_ready ready=true client=codex-cli project=dogfood-project-c'
expect_text "alpha projects brief lists project A experience" "$RUN_DIR/alpha-projects.brief.txt" \
    'dogfood-project-a episodes=1'
expect_text "alpha projects brief lists project C experience" "$RUN_DIR/alpha-projects.brief.txt" \
    'dogfood-project-c episodes=1'
expect_json_field "alpha projects filter keeps project C evidence only" "$RUN_DIR/alpha-project-c-provenance.json" \
    "data['project_filter'] == 'dogfood-project-c' and data['project_count'] == 1 and data['projects'][0]['project'] == 'dogfood-project-c' and data['projects'][0]['evidence_episode_ids'] == [2]"
expect_json_field "alpha projects filter scope integrity narrows to one project" "$RUN_DIR/alpha-project-c-provenance.json" \
    "data['scope_integrity']['session_project_status'] == 'single_project_sessions' and data['scope_integrity']['cross_project_session_count'] == 0"
expect_json_field "alpha filtered scope verification narrows to current project" "$RUN_DIR/alpha-project-c-provenance.json" \
    "data['scope_verification']['current_scope_ready'] is True and data['scope_verification']['current_project'] == 'dogfood-project-c' and data['scope_verification']['current_session_id'] == '$alpha_project_c_session' and data['scope_verification']['project_provenance_status'] == 'complete' and data['scope_verification']['session_project_status'] == 'single_project_sessions' and data['scope_verification']['cross_project_session_count'] == 0 and data['scope_verification']['scope_review_status'] == 'ready'"
expect_text "alpha filtered projects brief narrows project handoff" "$RUN_DIR/alpha-project-c-provenance.brief.txt" \
    'Scope integrity: project=complete session=single_project_sessions cross_project_sessions=0'

echo
section "persona-local adapter spool"
persona_spool() {
    eval "$("$BIN" call dogfood_alpha --shell bash)"
    eval "$("$BIN" session attach --session-id "$alpha_session" --client cursor --project dogfood-project-a --shell bash)"
    printf '{"prompt_text":"alpha adapter prompt","response_text":"alpha adapter response"}' \
        | env SOMA_BIN="$BIN" \
            SOMA_ADAPTER_SPOOL_KIND=turn \
            SOMA_ADAPTER_CAPTURE_SOURCE=cursor \
            "$ROOT/tools/soma-adapter-spool-append.sh" >/dev/null
    "$BIN" adapter-spool \
        --jsonl "$SOMA_ADAPTER_SPOOL_JSONL" \
        --checkpoint "$SOMA_ADAPTER_SPOOL_CHECKPOINT" >/dev/null
    "$BIN" recall --query "alpha adapter response" --limit 3 --format json \
        | grep -q "alpha adapter response"
}
run_step "persona-local adapter spool writes/drains/recalls" persona_spool

echo
section "private client proof-session readiness"
for client in codex-app cursor continue; do
    proof="$RUN_DIR/${client}.proof-session.json"
    if "$BIN" adapter-binding-proof --proof-session --client "$client" --json >"$proof"; then
        pass "proof-session renders $client"
        python3 - "$client" "$proof" <<'PY'
import json
import sys

client, path = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
session = data["proof_session"]
print(
    f"    {client}: release_gate={session['release_gate']} "
    f"ready_for_private_client_claim={session['ready_for_private_client_claim']} "
    f"next_step={session['next_step_id']}"
)
PY
        expect_json_field "$client proof-session stays read-only/unproven" "$proof" \
            "data['proof_session']['ready_for_private_client_claim'] is False"
        expect_json_field "$client proof-session starts at installed-config setup" "$proof" \
            "data['proof_session']['next_step_id'] == 'render_or_write_installed_config'"
    else
        fail "proof-session renders $client"
        continue
    fi

    installed_config="$HOME/.soma/client-bindings/${client}-installed-binding.json"
    installed_config_report="$RUN_DIR/${client}.installed-config.json"
    mkdir -p "$(dirname "$installed_config")"
    if "$BIN" adapter-binding-proof \
        --render-installed-config \
        --client "$client" \
        --write-installed-config "$installed_config" \
        --json >"$installed_config_report"; then
        pass "proof-free installed config renders $client"
        expect_json_field "$client installed config is app-hook eligible" "$installed_config_report" \
            "data['eligible_for_observed_app_hook'] is True and data['wrote_file'] is True"
        expect_json_field "$client installed config stays proof-free" "$installed_config_report" \
            "'records no proof row' in data['trust_boundary']"
    else
        fail "proof-free installed config renders $client"
        continue
    fi

    proof_after="$RUN_DIR/${client}.proof-session-after-installed-config.json"
    if "$BIN" adapter-binding-proof --proof-session --client "$client" --json >"$proof_after"; then
        pass "proof-session advances to hook trigger $client"
        python3 - "$client" "$proof_after" <<'PY'
import json
import sys

client, path = sys.argv[1:]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
session = data["proof_session"]
print(
    f"    {client}: installed_configs={data['installed_config_eligible_candidates']} "
    f"target_configs={data['private_client_target_eligible_candidates']} "
    f"generated_binding_nonce={data['generated_binding_nonce']} "
    f"next_step={session['next_step_id']}"
)
PY
        expect_json_field "$client proof-session reuses installed binding nonce" "$proof_after" \
            "data['generated_binding_nonce'] is False and data['installed_config_eligible_candidates'] >= 1"
        expect_json_field "$client proof-session separates setup artifact from private target config" "$proof_after" \
            "data['setup_artifact_eligible_candidates'] >= 1 and data['private_client_target_eligible_candidates'] == 0 and len(data['eligible_setup_artifact_paths']) >= 1 and len(data.get('eligible_private_client_target_paths', [])) == 0 and any(path.endswith('.${client%%-*}/soma-installed-binding.json') or path.endswith('.codex/soma-installed-binding.json') for path in data['private_client_target_candidate_paths'])"
        expect_json_field "$client proof-session waits for real private hook" "$proof_after" \
            "data['proof_session']['next_step_id'] == 'trigger_private_client_hook' and data['proof_session']['ready_for_private_client_claim'] is False"
        expect_json_field "$client proof-session records no proof during setup" "$proof_after" \
            "data['proofs_found'] == 0"
    else
        fail "proof-session advances to hook trigger $client"
        continue
    fi

    proof_list="$RUN_DIR/${client}.binding-proofs.json"
    if "$BIN" adapter-binding-proof --list --client "$client" --json >"$proof_list"; then
        expect_json_field "$client installed-config setup leaves proof ledger empty" "$proof_list" \
            "len(data['proofs']) == 0"
    else
        fail "$client installed-config setup leaves proof ledger empty"
    fi

    hook_readiness="$RUN_DIR/${client}.hook-readiness.json"
    if SOMA_BIN="$BIN" \
        SOMA_CLIENT_BINDING_CLIENT="$client" \
        SOMA_CLIENT_BINDING_CONFIG_ROOT="$HOME" \
        SOMA_CLIENT_BINDING_EVENT_JSONL="$HOME/.soma/adapter/events.jsonl" \
        "$ROOT/tools/soma-client-hook-readiness.sh" >"$hook_readiness"; then
        pass "$client hook readiness emits operator action card"
        expect_json_field "$client hook readiness card is read-only" "$hook_readiness" \
            "data['status'] == 'blocked' and data['next_action'] == data['operator_action_card']['next_action'] and data['blocking_reasons'] == data['operator_action_card']['blocking_reasons'] and data['operator_action_card']['schema'] == 'soma.client_hook_operator_action_card.v1' and data['operator_action_card']['read_only'] is True and 'records no proof row' in data['operator_action_card']['trust_boundary']"
        expect_json_field "$client hook readiness card blocks before real event" "$hook_readiness" \
            "data['operator_action_card']['status'] == 'blocked' and 'matching_private_event_missing' in data['operator_action_card']['blocking_reasons'] and (('$client' != 'continue' and '$client' != 'codex-app' and data['operator_action_card']['next_action'] == 'trigger_real_${client}_client_hook_to_write_private_spool_event') or ('$client' == 'codex-app' and data['operator_action_card']['next_action'] in {'restart_or_reopen_codex_app_before_real_hook', 'trigger_real_codex-app_client_hook_to_write_private_spool_event'} and ('codex_notify_restart_recommended' not in data['operator_action_card']['blocking_reasons'] or data['operator_action_card']['next_action'] == 'restart_or_reopen_codex_app_before_real_hook')) or ('$client' == 'continue' and data['operator_action_card']['next_action'] in {'merge_continue_mcp_config_before_real_hook', 'install_or_enable_continue_extension_before_real_hook', 'trigger_real_continue_client_hook_to_write_private_spool_event'} and ('continue_extension_config_not_visible' in data['operator_action_card']['blocking_reasons'] or 'continue_extension_installation_not_observed' in data['operator_action_card']['blocking_reasons'])))"
        expect_json_field "$client hook readiness card summarizes blocker for humans" "$hook_readiness" \
            "any('installed_config=ready' in line for line in data['operator_action_card']['summary_lines']) and any('event_jsonl=' in line for line in data['operator_action_card']['summary_lines']) and any('rerun readiness_command' in line for line in data['operator_action_card']['summary_lines'])"
        if [ "$client" = "continue" ]; then
            expect_json_field "$client hook readiness card distinguishes extension config visibility" "$hook_readiness" \
                "data['continue_extension_config_check']['status'] == 'config_missing' and data['continue_extension_config_check']['extension_installation_status'] == 'extension_not_observed' and data['continue_extension_config_check']['extension_observed'] is False and data['derived']['continue_extension_config_visible'] is False and any('continue_extension_config=status=config_missing' in line and 'extension_status=extension_not_observed' in line for line in data['operator_action_card']['summary_lines']) and 'Continue extension config is not visibly wired' in data['operator_action_card']['instruction'] and 'records no proof row' in data['continue_extension_config_check']['trust_boundary']"
        fi
        expect_json_field "$client hook readiness card names event source and nonce" "$hook_readiness" \
            "data['operator_action_card']['expected_event_source'] == '${client}_private_lifecycle_hook' and data['operator_action_card']['event_jsonl_path'].endswith('.soma/adapter/events.jsonl') and len(data['operator_action_card']['eligible_binding_nonces']) == 1 and data['operator_action_card']['eligible_binding_nonces'][0].startswith('soma-bind-') and data['operator_action_card']['record_command'] is None"
        expect_json_field "$client hook readiness card exposes required event contract" "$hook_readiness" \
            "data['operator_action_card']['required_event_contract']['schema'] == 'soma.adapter_spool_event.v1' and data['operator_action_card']['required_event_contract']['writer_contract'] == 'soma_adapter_spool_append_v1' and data['operator_action_card']['required_event_contract']['client'] == '${client}' and data['operator_action_card']['required_event_contract']['event_source'] == '${client}_private_lifecycle_hook' and data['operator_action_card']['required_event_contract']['source_boundary'] == 'real_private_client_hook_only'"
        expect_json_field "$client hook readiness card exposes proof-free integration template" "$hook_readiness" \
            "data['private_client_hook_integration_template'] == data['operator_action_card']['private_client_hook_integration_template'] and data['private_client_hook_integration_template']['schema'] == 'soma.private_client_hook_integration_template.v1' and data['private_client_hook_integration_template']['client'] == '${client}' and data['private_client_hook_integration_template']['read_only'] is True and data['private_client_hook_integration_template']['records_proof'] is False and data['private_client_hook_integration_template']['creates_verification_event'] is False and data['private_client_hook_integration_template']['promotes_cloud_draft'] is False and data['private_client_hook_integration_template']['manual_invocation_policy'] == 'non_release_debug_only' and data['private_client_hook_integration_template']['stdin_event_template']['hook_adapter'] == 'manual_debug_non_release_template' and data['private_client_hook_integration_template']['stdin_event_template']['manual_invocation_policy'] == 'non_release_debug_only' and data['private_client_hook_integration_template']['environment']['SOMA_ADAPTER_LIFECYCLE_CLIENT'] == '${client}' and data['private_client_hook_integration_template']['environment']['SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE'] == '${client}_private_lifecycle_hook' and data['private_client_hook_integration_template']['environment']['SOMA_ADAPTER_LIFECYCLE_JSONL'].endswith('.soma/adapter/events.jsonl') and data['private_client_hook_integration_template']['wrapper_command_template'][0] == 'env' and data['private_client_hook_integration_template']['wrapper_command_template'][-1].startswith('tools/soma-') and data['private_client_hook_integration_template']['expected_spool_contract'] == data['operator_action_card']['required_event_contract'] and 'manual terminal invocation is non-release debug evidence' in data['private_client_hook_integration_template']['trust_boundary']"
        expect_json_field "$client hook readiness card exposes read-only spool watch command" "$hook_readiness" \
            "data['operator_action_card']['watch_command'][-1].endswith('.soma/adapter/events.jsonl') and 'manual wrapper invocations are debug observations' in data['operator_action_card']['trust_boundary']"
        expect_json_field "$client hook readiness card exposes bounded wait command" "$hook_readiness" \
            "data['wait_observation']['requested'] is False and data['wait_command'] == data['operator_action_card']['wait_command'] and 'SOMA_CLIENT_BINDING_WAIT_SECONDS=30' in data['operator_action_card']['wait_command'] and data['operator_action_card']['wait_command'][-1] == 'tools/soma-client-hook-readiness.sh'"
        expect_json_field "$client hook readiness card separates setup artifact from private target config" "$hook_readiness" \
            "data['operator_action_card']['installation_visibility']['setup_artifact_ready'] is True and data['operator_action_card']['installation_visibility']['private_client_target_config_present'] is False and 'private_client_target_config_not_discovered' in data['operator_action_card']['installation_visibility']['warnings'] and 'does not prove the private client invoked the hook' in data['operator_action_card']['installation_visibility']['trust_boundary']"
    else
        fail "$client hook readiness emits operator action card"
    fi
done

cursor_render_prep="$RUN_DIR/cursor-render-proof-prep.json"
cursor_render_prep_dir="$RUN_DIR/cursor-render-proof-prep"
if tools/soma-client-render-proof-prep.sh \
    --client cursor \
    --soma-bin "$BIN" \
    --manifest "$ROOT/tools/client-bindings/cursor-soma-binding.json.example" \
    --artifact-dir "$cursor_render_prep_dir" >"$cursor_render_prep"; then
    pass "cursor render proof prep materializes proof-free artifacts"
    expect_json_field "cursor render proof prep is proof-free handoff" "$cursor_render_prep" \
        "data['schema'] == 'soma.client_render_proof_prep.v1' and data['client'] == 'cursor' and data['status'] == 'ready_for_visible_client_render' and data['records_proof'] is False and data['creates_verification_event'] is False and data['promotes_cloud_draft'] is False and data['applies_proposal'] is False and 'records no proof row' in data['trust_boundary']"
    expect_json_field "cursor render proof prep writes durable render artifacts" "$cursor_render_prep" \
        "all(data['artifacts'].get(key, '').startswith('$cursor_render_prep_dir/') for key in ['review_render_json', 'review_render_markdown', 'review_render_html', 'render_evidence', 'render_evidence_template'])"
    expect_json_field "cursor render proof prep points at guarded proof command" "$cursor_render_prep" \
        "data['record_command_after_filled_evidence'][0] in {'$BIN', '$BIN_ABS'} and data['record_command_after_filled_evidence'][0] != 'soma' and 'observed_in_client_render' in data['record_command_after_filled_evidence'] and '--operator-confirm-in-client-render' in data['record_command_after_filled_evidence'] and '--operator-confirm-release-grade-evidence' in data['record_command_after_filled_evidence'] and '$cursor_render_prep_dir/render-evidence.json' in data['record_command_after_filled_evidence']"
    for artifact in review-render.json review-render.md review-render.html render-evidence.json render-evidence-template.json; do
        if [[ -s "$cursor_render_prep_dir/$artifact" ]]; then
            pass "cursor render proof prep writes $artifact"
        else
            fail "cursor render proof prep writes $artifact"
        fi
    done
    cursor_render_prep_reuse="$RUN_DIR/cursor-render-proof-prep-reuse.json"
    if tools/soma-client-render-proof-prep.sh \
        --client cursor \
        --soma-bin "$BIN" \
        --manifest "$ROOT/tools/client-bindings/cursor-soma-binding.json.example" \
        --artifact-dir "$cursor_render_prep_dir" >"$cursor_render_prep_reuse"; then
        pass "cursor render proof prep reuses existing artifacts without overwrite"
        expect_json_field "cursor render proof prep reuse stays proof-free" "$cursor_render_prep_reuse" \
            "data['status'] == 'ready_for_visible_client_render' and data['overwrite_policy'] == 'reuse_existing_artifacts_without_overwrite' and data['records_proof'] is False and data['creates_verification_event'] is False and 'without overwrite' in data['trust_boundary']"
    else
        fail "cursor render proof prep reuses existing artifacts without overwrite"
    fi
else
    fail "cursor render proof prep materializes proof-free artifacts"
fi

clients_after="$RUN_DIR/clients-after-installed-config.json"
if "$BIN" clients --json --command "$BIN" >"$clients_after"; then
    pass "client readiness sees installed-config hook-trigger step"
    expect_json_field "client readiness emits operator card" "$clients_after" \
        "data['operator_card']['source'] == 'soma_clients.operator_card.v1' and data['operator_card']['status'] == 'private_app_proof_pending' and data['operator_card']['primary_next_command'] and 'records no proof' in data['operator_card']['trust_boundary']"
    expect_json_field "client readiness emits readiness index" "$clients_after" \
        "data['readiness_index']['source'] == 'soma_clients.readiness_index.v1' and data['readiness_index']['status'] == data['operator_card']['status'] and data['readiness_index']['semantic_review_status'] == data['semantic_review']['status'] and data['readiness_index']['blocked_private_clients'] == data['operator_card']['blocked_private_clients'] and data['readiness_index']['private_app_restart_commands'] == data['operator_card']['private_app_restart_commands'] and data['readiness_index']['private_app_collector_start_commands'] == data['operator_card']['private_app_collector_start_commands'] and data['readiness_index']['primary_next_command'] == data['operator_card']['primary_next_command'] and 'records no proof' in data['readiness_index']['trust_boundary']"
    expect_json_field "client readiness emits client binding proof matrix index" "$clients_after" \
        "data['client_binding']['source'] == 'soma_clients.client_binding_readiness_index.v1' and data['client_binding']['status'] == data['operator_card']['status'] and data['client_binding']['ready'] is False and data['client_binding']['proof_storage_status'] == 'available' and data['client_binding']['proof_storage_unavailable'] is False and data['client_binding']['private_app_next_actions'] == data['operator_card']['private_app_next_actions'] and data['client_binding']['required_client_proof_matrix'] == data['operator_card']['private_app_release_proof_checklist'] and data['client_binding']['release_snapshot'] == data['private_app_release_snapshot'] and len(data['client_binding']['required_client_proof_matrix']) == 3 and all(checklist['client'] in {'codex-app', 'cursor', 'continue'} and checklist['status'] == 'pending' and checklist.get('next_proof_step_id') == 'trigger_private_client_hook' and checklist.get('next_required_proof_level') == 'observed_app_hook' and checklist.get('next_command') for checklist in data['client_binding']['required_client_proof_matrix']) and 'records no proof row' in data['client_binding']['trust_boundary']"
    expect_json_field "client readiness hardening command probes HOME target configs" "$clients_after" \
        "data['operator_card']['strict_private_client_hardening_command'][-2:] == ['--client-binding-config-root', '$HOME'] and data['readiness_index']['strict_private_client_hardening_command'] == data['operator_card']['strict_private_client_hardening_command'] and not data['operator_card']['strict_private_client_hardening_command'][-1].endswith('.soma/client-bindings')"
    expect_json_field "client readiness action matrix exposes operator next action" "$clients_after" \
        "data['operator_card'].get('private_app_next_actions') and all(action.get('operator_next_action_id') and action.get('operator_next_action_label') for action in data['operator_card']['private_app_next_actions']) and data['readiness_index'].get('private_app_next_actions') == data['operator_card'].get('private_app_next_actions')"
    expect_json_field "client readiness action matrix exposes current-session action safety" "$clients_after" \
        "data['operator_card'].get('private_app_next_actions') and all(action.get('current_session_action_safety', {}).get('source') == 'soma_clients.current_session_action_safety.v1' and action.get('current_session_action_safety', {}).get('action_targets_current_session') in (True, False) and action.get('current_session_action_safety', {}).get('action_safe_in_current_session') in (True, False) and action.get('current_session_action_safety', {}).get('recommended_execution_context') in {'current_session_ok', 'separate_terminal_or_after_reopening_client'} and 'never executes commands' in action.get('current_session_action_safety', {}).get('trust_boundary', '') and 'promotes cloud drafts' in action.get('current_session_action_safety', {}).get('trust_boundary', '') for action in data['operator_card']['private_app_next_actions']) and data['readiness_index'].get('private_app_next_actions') == data['operator_card'].get('private_app_next_actions')"
    expect_json_field "client readiness mirrors proof-session runbook steps" "$clients_after" \
        "all(any(row['client'] == client and any(step.get('source') == 'soma_clients.private_app_proof_session_runbook_step.v1' and step.get('id') == 'trigger_private_client_hook' and step.get('ready_now') is True and step.get('records_proof') is False and 'tools/soma-client-hook-readiness.sh' in step.get('command', []) and 'records no proof row' in step.get('trust_boundary', '') for step in row.get('proof_session_runbook_steps', [])) and any(step.get('id') == 'record_observed_app_hook' and step.get('records_proof') is True and step.get('mcp_tool') == 'soma_client_binding_record_proof' and '\"proof_level\":\"observed_app_hook\"' in step.get('mcp_arguments_json', '') for step in row.get('proof_session_runbook_steps', [])) for row in data['clients']) for client in ['codex-app', 'cursor', 'continue'])"
    expect_json_field "client readiness mirrors runbook step external action safety" "$clients_after" \
        "all(any(row['client'] == client and any(step.get('id') == 'trigger_private_client_hook' and step.get('external_action_safety', {}).get('source') == 'soma_clients.private_app_external_action_safety.v1' and step.get('external_action_safety', {}).get('classification') == 'real_private_client_action_may_send_prompt_to_provider' and step.get('external_action_safety', {}).get('requires_operator_confirmation_before_submission') is True and step.get('external_action_safety', {}).get('may_transmit_prompt_to_provider') is True and 'API keys' in step.get('external_action_safety', {}).get('forbidden_inputs', []) and 'submits no prompt' in step.get('external_action_safety', {}).get('trust_boundary', '') for step in row.get('proof_session_runbook_steps', [])) for row in data['clients']) for client in ['codex-app', 'cursor', 'continue'])"
    expect_json_field "client readiness operator card primary command installs target config" "$clients_after" \
        "data['operator_card']['primary_next_command'] and '--render-installed-config' in data['operator_card']['primary_next_command'] and '--write-installed-config' in data['operator_card']['primary_next_command'] and any(str(part).endswith('.codex/soma-installed-binding.json') for part in data['operator_card']['primary_next_command'])"
    expect_json_field "client readiness operator card blocks private app claims" "$clients_after" \
        "set(data['operator_card']['blocked_private_clients']) == {'codex-app', 'cursor', 'continue'} and any('Automatic private capture' in claim for claim in data['operator_card']['blocked_claims'])"
    expect_json_field "client readiness operator card names runtime diagnostics" "$clients_after" \
        "len(data['operator_card'].get('runtime_missing_clients', [])) == data['summary']['runtime_missing_count'] and all(cmd and cmd[0] == 'which' for cmd in data['operator_card'].get('runtime_check_commands', [])) and 'codex-app' in data['operator_card'].get('runtime_not_cli_detectable_clients', [])"
    expect_json_field "client readiness surfaces Continue extension config visibility" "$clients_after" \
        "'continue' in data['operator_card'].get('continue_extension_config_not_visible_clients', []) and data['readiness_index'].get('continue_extension_config_not_visible_clients') == data['operator_card'].get('continue_extension_config_not_visible_clients') and any(row['client'] == 'continue' and row.get('continue_extension_config_check', {}).get('status') == 'config_missing' and 'records no proof row' in row.get('continue_extension_config_check', {}).get('trust_boundary', '') for row in data['clients'])"
    expect_json_field "client readiness sees no app-hook recordable event before real hook" "$clients_after" \
        "data['summary'].get('private_app_record_app_hook_next_count') == 0"
    expect_json_field "client readiness suppresses top-level wait commands before target config" "$clients_after" \
        "data['operator_card'].get('private_app_wait_commands') == [] and data['operator_card'].get('private_app_restart_commands') == [] and data['operator_card'].get('private_app_collector_start_commands') == [] and data['readiness_index'].get('private_app_wait_commands') == data['operator_card'].get('private_app_wait_commands') and data['readiness_index'].get('private_app_restart_commands') == data['operator_card'].get('private_app_restart_commands') and data['readiness_index'].get('private_app_collector_start_commands') == data['operator_card'].get('private_app_collector_start_commands')"
    expect_json_field "client readiness release checklist exposes read-only runbooks" "$clients_after" \
        "len(data['operator_card'].get('private_app_release_proof_checklist', [])) == 3 and data['readiness_index'].get('private_app_release_proof_checklist') == data['operator_card'].get('private_app_release_proof_checklist') and all('tools/soma-client-release-proof-runbook.sh' in checklist.get('release_runbook_command', []) and 'SOMA_CLIENT_RELEASE_PROOF_MODE=read_only' in checklist.get('release_runbook_command', []) and checklist.get('next_required_proof_level') == 'observed_app_hook' and checklist.get('next_proof_step_id') and 'records no proof row' in checklist.get('trust_boundary', '') for checklist in data['operator_card']['private_app_release_proof_checklist'])"
    expect_json_field "client readiness top-level next commands expose pending release runbooks" "$clients_after" \
        "all(any(f'SOMA_CLIENT_BINDING_CLIENT={client}' in command and 'tools/soma-client-release-proof-runbook.sh' in command and 'SOMA_CLIENT_RELEASE_PROOF_MODE=read_only' in command for command in data.get('next_commands', [])) for client in ['codex-app', 'cursor', 'continue'])"
    for client in codex-app cursor continue; do
        expect_json_field "$client client-readiness next step is trigger hook" "$clients_after" \
            "any(row['client'] == '$client' and row['goal_status'] == 'private_app_trigger_hook_required' and row.get('proof_session_next_step_id') == 'trigger_private_client_hook' and row.get('installed_config_eligible_candidates', 0) >= 1 for row in data['clients'])"
        expect_json_field "$client client-readiness separates setup artifact from private target config" "$clients_after" \
            "any(row['client'] == '$client' and row.get('installed_config_setup_artifact_eligible_candidates', 0) >= 1 and row.get('installed_config_private_target_eligible_candidates', 0) == 0 and row.get('eligible_setup_artifact_paths', []) != [] and row.get('private_client_target_candidate_paths', []) != [] and 'no known private-client target config was discovered' in row.get('next_step', '') for row in data['clients'])"
        expect_json_field "$client client-readiness offers target config install command" "$clients_after" \
            "any(row['client'] == '$client' and any('--render-installed-config' in command and '--write-installed-config' in command and any(str(part).endswith('.${client%%-*}/soma-installed-binding.json') or str(part).endswith('.codex/soma-installed-binding.json') for part in command) for command in row.get('next_commands', [])) for row in data['clients'])"
        expect_json_field "$client client-readiness exposes hook evidence hints" "$clients_after" \
            "any(row['client'] == '$client' and row.get('expected_event_source') == '${client}_private_lifecycle_hook' and row.get('binding_nonce', '').startswith('soma-bind-') and row.get('generated_binding_nonce') is False and any('private_client_target_config' in reason for reason in row.get('proof_session_blocking_reasons', [])) and any('event_jsonl' in reason for reason in row.get('proof_session_blocking_reasons', [])) for row in data['clients'])"
        expect_json_field "$client client-readiness exposes proof-free hook integration template" "$clients_after" \
            "any(row['client'] == '$client' and row.get('private_hook_integration_template', {}).get('source') == 'soma_clients.private_hook_integration_template.v1' and row['private_hook_integration_template']['client'] == '$client' and row['private_hook_integration_template']['records_proof'] is False and row['private_hook_integration_template']['creates_verification_event'] is False and row['private_hook_integration_template']['promotes_cloud_draft'] is False and row['private_hook_integration_template']['manual_invocation_policy'] == 'non_release_debug_only' and row['private_hook_integration_template']['stdin_event_template_json'].find('manual_debug_non_release_template') >= 0 and row['private_hook_integration_template']['environment']['SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE'] == '${client}_private_lifecycle_hook' and row['private_hook_integration_template']['environment']['SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE'].startswith('soma-bind-') and row['private_hook_integration_template']['expected_spool_contract'] == row['private_event_contract'] and 'manual terminal invocation is non-release debug evidence' in row['private_hook_integration_template']['trust_boundary'] for row in data['clients']) and any(template['client'] == '$client' and template['records_proof'] is False for template in data['operator_card'].get('private_app_hook_integration_templates', [])) and data['readiness_index'].get('private_app_hook_integration_templates') == data['operator_card'].get('private_app_hook_integration_templates')"
        expect_json_field "$client client-readiness hook readiness command keeps proof-session context" "$clients_after" \
            "any(row['client'] == '$client' and any('tools/soma-client-hook-readiness.sh' in command and any(str(part).startswith('SOMA_CLIENT_BINDING_MANIFEST=') for part in command) and any(str(part).startswith('SOMA_CLIENT_BINDING_EVENT_JSONL=') for part in command) for command in row.get('next_commands', [])) for row in data['clients'])"
        expect_json_field "$client client-readiness release runbook keeps proof-session context" "$clients_after" \
            "any(checklist['client'] == '$client' and 'tools/soma-client-release-proof-runbook.sh' in checklist.get('release_runbook_command', []) and 'SOMA_CLIENT_RELEASE_PROOF_MODE=read_only' in checklist.get('release_runbook_command', []) and any(str(part).startswith('SOMA_CLIENT_BINDING_EVENT_JSONL=') for part in checklist.get('release_runbook_command', [])) and (any(str(part).startswith('SOMA_CLIENT_BINDING_MANIFEST=') for part in checklist.get('release_runbook_command', [])) or (any(str(part).startswith('SOMA_CLIENT_BINDING_EVENT_SOURCE=') for part in checklist.get('release_runbook_command', [])) and any(str(part).startswith('SOMA_CLIENT_BINDING_NONCE=') for part in checklist.get('release_runbook_command', [])))) for checklist in data['operator_card'].get('private_app_release_proof_checklist', []))"
        expect_json_field "$client client-readiness probes default event jsonl" "$clients_after" \
            "any(row['client'] == '$client' and row.get('event_jsonl_path', '').endswith('.soma/adapter/events.jsonl') and row.get('event_jsonl_probe_status') == 'not_found' for row in data['clients'])"
    done
    clients_after_brief="$RUN_DIR/clients-after-installed-config.brief.txt"
    if "$BIN" clients --brief --command "$BIN" >"$clients_after_brief"; then
        pass "client readiness brief summarizes client binding proof matrix"
        expect_text "client readiness brief names compact proof matrix" "$clients_after_brief" \
            'Client binding proof matrix: ready=false proof_storage=available rows=3'
        expect_text "client readiness brief lists codex-app binding row" "$clients_after_brief" \
            'binding codex-app: status=pending'
        expect_text "client readiness brief lists cursor binding row" "$clients_after_brief" \
            'binding cursor: status=pending'
        expect_text "client readiness brief lists continue binding row" "$clients_after_brief" \
            'binding continue: status=pending'
        expect_text "client readiness brief exposes proof matrix next command" "$clients_after_brief" \
            'next_command:'
    else
        fail "client readiness brief summarizes client binding proof matrix"
    fi
else
    fail "client readiness sees installed-config hook-trigger step"
fi

cursor_target_config="$HOME/.cursor/soma-installed-binding.json"
cursor_target_config_report="$RUN_DIR/cursor.target-installed-config.json"
cursor_target_clients="$RUN_DIR/clients-after-cursor-target-config.json"
cursor_binding_nonce="$(json_get "$RUN_DIR/cursor.proof-session-after-installed-config.json" "data['binding_nonce']" 2>/dev/null || true)"
mkdir -p "$(dirname "$cursor_target_config")"
if [[ -n "$cursor_binding_nonce" ]] && "$BIN" adapter-binding-proof \
    --render-installed-config \
    --client cursor \
    --binding-nonce "$cursor_binding_nonce" \
    --write-installed-config "$cursor_target_config" \
    --json >"$cursor_target_config_report"; then
    pass "cursor target installed config renders proof-free"
    expect_json_field "cursor target installed config reuses setup nonce" "$cursor_target_config_report" \
        "data['binding_nonce'] == '$cursor_binding_nonce' and data['wrote_file'] is True and 'records no proof row' in data['trust_boundary']"
    if "$BIN" clients --json --command "$BIN" >"$cursor_target_clients"; then
        pass "client readiness promotes actionable target hook wait command"
        expect_json_field "client readiness top-level wait command requires target config" "$cursor_target_clients" \
            "set(wait['client'] for wait in data['operator_card'].get('private_app_wait_commands', [])) == {'cursor'} and data['readiness_index'].get('private_app_wait_commands') == data['operator_card'].get('private_app_wait_commands')"
        expect_json_field "cursor actionable wait command is proof-free and bounded" "$cursor_target_clients" \
            "any(wait['client'] == 'cursor' and wait['goal_status'] == 'private_app_trigger_hook_required' and wait['operator_next_action_id'] == 'trigger_real_private_client_hook_to_write_private_spool_event' and wait['restart_recommended'] is False and wait['expected_event_source'] == 'cursor_private_lifecycle_hook' and wait['binding_nonce'] == '$cursor_binding_nonce' and wait['event_jsonl_path'].endswith('.soma/adapter/events.jsonl') and 'SOMA_CLIENT_BINDING_WAIT_SECONDS=30' in wait['wait_command'] and wait['wait_command'][-1] == 'tools/soma-client-hook-readiness.sh' and wait.get('watch_command', [])[-1].endswith('.soma/adapter/events.jsonl') and 'cannot substitute for observed_app_hook evidence' in wait['trust_boundary'] for wait in data['operator_card'].get('private_app_wait_commands', []))"
        expect_json_field "cursor target config switches row to private target visibility" "$cursor_target_clients" \
            "any(row['client'] == 'cursor' and row.get('installed_config_private_target_eligible_candidates', 0) >= 1 and any(path.endswith('.cursor/soma-installed-binding.json') for path in row.get('eligible_private_client_target_paths', [])) and row.get('proof_session_next_step_id') == 'trigger_private_client_hook' for row in data['clients'])"
    else
        fail "client readiness promotes actionable target hook wait command"
    fi
else
    fail "cursor target installed config renders proof-free"
fi

continue_target_config="$HOME/.continue/soma-installed-binding.json"
continue_mcp_config="$HOME/.continue/mcpServers/soma.json"
continue_devdata_config="$HOME/.continue/config.yaml"
continue_extension_dir="$RUN_DIR/continue.continue-dogfood-extension"
continue_target_config_report="$RUN_DIR/continue.target-installed-config.json"
continue_collector_down_proof="$RUN_DIR/continue.proof-session-collector-down.json"
continue_collector_down_clients="$RUN_DIR/clients-continue-collector-down.json"
continue_collector_listening_clients="$RUN_DIR/clients-continue-collector-listening.json"
continue_binding_nonce="$(json_get "$RUN_DIR/continue.proof-session-after-installed-config.json" "data['binding_nonce']" 2>/dev/null || true)"
mkdir -p "$(dirname "$continue_target_config")" "$(dirname "$continue_mcp_config")" "$continue_extension_dir"
printf '{"type":"stdio","command":"%s","args":["mcp-serve"]}\n' "$BIN" >"$continue_mcp_config"
cat >"$continue_devdata_config" <<'EOF'
name: SOMA local Continue config
version: 0.0.1
data:
  - name: SOMA local dev-data bridge
    destination: http://127.0.0.1:8766/continue-devdata
    schema: 0.2.0
    level: all
    events:
      - chatInteraction
      - editInteraction
      - editOutcome
      - quickEdit
EOF
if [[ -n "$continue_binding_nonce" ]] && "$BIN" adapter-binding-proof \
    --render-installed-config \
    --client continue \
    --binding-nonce "$continue_binding_nonce" \
    --write-installed-config "$continue_target_config" \
    --json >"$continue_target_config_report"; then
    pass "continue target installed config renders proof-free"
    expect_json_field "continue target installed config reuses setup nonce" "$continue_target_config_report" \
        "data['binding_nonce'] == '$continue_binding_nonce' and data['wrote_file'] is True and 'records no proof row' in data['trust_boundary']"
    if SOMA_CONTINUE_DEVDATA_COLLECTOR_STATUS=not_listening \
        "$BIN" adapter-binding-proof --proof-session --client continue --json >"$continue_collector_down_proof"; then
        pass "continue proof-session waits for collector before hook"
        expect_json_field "continue proof-session exposes collector-first next step" "$continue_collector_down_proof" \
            "data['proof_session']['next_step_id'] == 'start_continue_devdata_collector_before_real_hook' and data['proof_session']['next_operator_step']['id'] == 'start_continue_devdata_collector_before_real_hook' and data['proof_session']['ready_for_private_client_claim'] is False and data['proofs_found'] == 0"
        expect_json_field "continue proof-session collector command is proof-free and bound" "$continue_collector_down_proof" \
            "'tools/soma-continue-devdata-start.sh' in data['proof_session']['next_command'] and 'start' in data['proof_session']['next_command'] and '--soma-bin' in data['proof_session']['next_command'] and '--jsonl' in data['proof_session']['next_command'] and '--binding-config' in data['proof_session']['next_command'] and '$continue_target_config' in data['proof_session']['next_command'] and any(str(part).endswith('.soma/adapter/events.jsonl') for part in data['proof_session']['next_command']) and 'records no client-binding proof row' in data['proof_session']['next_operator_step']['trust_boundary']"
        expect_json_field "continue proof-session runbook includes collector guidance" "$continue_collector_down_proof" \
            "data['proof_session']['runbook']['target_next_step_id'] == 'start_continue_devdata_collector_before_real_hook' and any(step['id'] == 'start_continue_devdata_collector_before_real_hook' and step['ready_now'] is True and step['records_proof'] is False and step['evidence_kind'] == 'continue_devdata_collector' and 'tools/soma-continue-devdata-start.sh' in step.get('command', []) and 'start' in step.get('command', []) and '$continue_target_config' in step.get('command', []) and any(str(part).endswith('.soma/adapter/events.jsonl') for part in step.get('command', [])) and 'records no client-binding proof row' in step.get('trust_boundary', '') for step in data['proof_session']['runbook']['steps'])"
    else
        fail "continue proof-session waits for collector before hook"
    fi
    if SOMA_CONTINUE_EXTENSION_PATH="$continue_extension_dir" \
        SOMA_CONTINUE_DEVDATA_COLLECTOR_STATUS=not_listening \
        "$BIN" clients --json --command "$BIN" --client continue >"$continue_collector_down_clients"; then
        pass "continue collector-down readiness renders"
        expect_json_field "continue collector-down exposes liveness fields" "$continue_collector_down_clients" \
            "data['clients'][0]['client'] == 'continue' and data['clients'][0]['continue_extension_config_check']['status'] == 'config_present_soma_mcp_seen' and data['clients'][0]['continue_extension_config_check']['extension_observed'] is True and data['clients'][0]['continue_extension_config_check']['devdata_destination_visible'] is True and data['clients'][0]['continue_extension_config_check']['devdata_collector_status'] == 'not_listening' and data['clients'][0]['continue_extension_config_check']['devdata_collector_listening'] is False and data['clients'][0]['continue_extension_config_check']['devdata_collector_host'] == '127.0.0.1' and data['clients'][0]['continue_extension_config_check']['devdata_collector_port'] == 8766"
        expect_json_field "continue collector-down makes collector start primary action" "$continue_collector_down_clients" \
            "data['operator_card']['primary_next_command'] and 'tools/soma-continue-devdata-collector.py' in data['operator_card']['primary_next_command'] and '--soma-bin' in data['operator_card']['primary_next_command'] and ('$BIN' in data['operator_card']['primary_next_command'] or '$BIN_ABS' in data['operator_card']['primary_next_command']) and '--jsonl' in data['operator_card']['primary_next_command'] and any(str(part).endswith('.soma/adapter/events.jsonl') for part in data['operator_card']['primary_next_command']) and '--binding-config' in data['operator_card']['primary_next_command'] and '$continue_target_config' in data['operator_card']['primary_next_command'] and any(any(str(part).endswith('.soma/adapter/events.jsonl') for part in command) and '$continue_target_config' in command for command in data.get('next_commands', [])) and any(action['client'] == 'continue' and action['operator_next_action_id'] == 'start_continue_devdata_collector_before_real_hook' and action['operator_next_action_label'] == 'Start Continue dev-data collector' and 'continue_devdata_collector_not_listening' in action['release_gate_blockers'] for action in data['operator_card']['private_app_next_actions']) and any(command['client'] == 'continue' and command['operator_next_action_id'] == 'start_continue_devdata_collector_before_real_hook' and command['collector_status'] == 'not_listening' and command['collector_listening'] is False and command['devdata_destination_visible'] is True and 'tools/soma-continue-devdata-collector.py' in command['start_command'] and '$continue_target_config' in command['start_command'] and any(str(part).endswith('.soma/adapter/events.jsonl') for part in command['start_command']) and 'tools/soma-client-hook-readiness.sh' in command.get('follow_up_wait_command', []) and 'records no proof row' in command.get('trust_boundary', '') for command in data['operator_card'].get('private_app_collector_start_commands', [])) and data['readiness_index'].get('private_app_collector_start_commands') == data['operator_card'].get('private_app_collector_start_commands')"
        expect_json_field "continue collector-down exposes managed launcher command" "$continue_collector_down_clients" \
            "any(action['client'] == 'continue' and 'tools/soma-continue-devdata-start.sh' in action.get('continue_devdata_collector_managed_start_command', []) and 'start' in action.get('continue_devdata_collector_managed_start_command', []) and '$continue_target_config' in action.get('continue_devdata_collector_managed_start_command', []) and any(str(part).endswith('.soma/adapter/events.jsonl') for part in action.get('continue_devdata_collector_managed_start_command', [])) for action in data['operator_card']['private_app_next_actions']) and any(command['client'] == 'continue' and 'tools/soma-continue-devdata-start.sh' in command.get('managed_start_command', []) and 'start' in command.get('managed_start_command', []) and '$continue_target_config' in command.get('managed_start_command', []) and any(str(part).endswith('.soma/adapter/events.jsonl') for part in command.get('managed_start_command', [])) for command in data['operator_card'].get('private_app_collector_start_commands', []))"
        expect_json_field "continue collector-down defers release hook wait" "$continue_collector_down_clients" \
            "data['operator_card'].get('private_app_wait_commands') == [] and any(command['client'] == 'continue' for command in data['operator_card'].get('private_app_collector_start_commands', [])) and any(checklist['client'] == 'continue' and 'continue_devdata_collector_not_listening' in checklist['release_gate_blockers'] for checklist in data['operator_card']['private_app_release_proof_checklist'])"
    else
        fail "continue collector-down readiness renders"
    fi
    if SOMA_CONTINUE_EXTENSION_PATH="$continue_extension_dir" \
        SOMA_CONTINUE_DEVDATA_COLLECTOR_STATUS=listening \
        "$BIN" clients --json --command "$BIN" --client continue >"$continue_collector_listening_clients"; then
        pass "continue collector-listening readiness renders"
        expect_json_field "continue collector-listening advances to hook wait" "$continue_collector_listening_clients" \
            "data['clients'][0]['continue_extension_config_check']['devdata_collector_status'] == 'listening' and data['clients'][0]['continue_extension_config_check']['devdata_collector_listening'] is True and data['operator_card'].get('private_app_collector_start_commands') == [] and data['readiness_index'].get('private_app_collector_start_commands') == data['operator_card'].get('private_app_collector_start_commands') and 'tools/soma-continue-devdata-collector.py' not in data['operator_card']['primary_next_command'] and 'tools/soma-client-hook-readiness.sh' in data['operator_card']['primary_next_command'] and any(wait['client'] == 'continue' and wait['operator_next_action_id'] == 'trigger_real_private_client_hook_to_write_private_spool_event' and 'tools/soma-client-hook-readiness.sh' in wait['wait_command'] for wait in data['operator_card'].get('private_app_wait_commands', []))"
        expect_json_field "continue collector-listening keeps real proof boundary" "$continue_collector_listening_clients" \
            "any(action['client'] == 'continue' and action['operator_next_action_id'] == 'trigger_real_private_client_hook_to_write_private_spool_event' and 'real_private_hook_event_missing' in action['release_gate_blockers'] and 'continue_devdata_collector_not_listening' not in action['release_gate_blockers'] for action in data['operator_card']['private_app_next_actions']) and any('real Continue' in data['operator_card']['primary_next_step'] for _ in [0])"
    else
        fail "continue collector-listening readiness renders"
    fi

    continue_live_jsonl="$RUN_DIR/continue-live-collector-events.jsonl"
    continue_live_collector_log="$RUN_DIR/continue-live-collector.log"
    continue_live_clients="$RUN_DIR/clients-continue-live-collector.json"
    continue_live_post_response="$RUN_DIR/continue-live-collector-post-response.json"
    continue_live_proofs="$RUN_DIR/continue-live-binding-proofs-after-post.json"
    loopback_ipv4_status="$(loopback_ipv4_probe_status)"
    if [[ "$loopback_ipv4_status" == "addr_not_available" || "$loopback_ipv4_status" == "operation_not_permitted" ]]; then
        warn "continue live collector proof skipped: IPv4 loopback unavailable ($loopback_ipv4_status)"
    else
        continue_live_port="$(pick_free_port)"
        cat >"$continue_devdata_config" <<EOF
name: SOMA local Continue config
version: 0.0.1
data:
  - name: SOMA local dev-data bridge
    destination: http://127.0.0.1:$continue_live_port/continue-devdata
    schema: 0.2.0
    level: all
    events:
      - chatInteraction
      - editInteraction
      - editOutcome
      - quickEdit
EOF
        SOMA_BIN="$BIN" "$ROOT/tools/soma-continue-devdata-collector.py" \
            --host 127.0.0.1 \
            --port "$continue_live_port" \
            --soma-bin "$BIN" \
            --jsonl "$continue_live_jsonl" \
            --binding-config "$continue_target_config" \
            --project dogfood-continue-live \
            --cwd "$ROOT" \
            --session-id continue-live-collector-dogfood \
            >"$continue_live_collector_log" 2>&1 &
        continue_live_collector_pid="$!"
        BG_PIDS+=("$continue_live_collector_pid")
        if wait_for_file_pattern "$continue_live_collector_log" '"status":"listening"'; then
            pass "continue live collector starts TCP listener"
            if SOMA_CONTINUE_EXTENSION_PATH="$continue_extension_dir" \
                SOMA_CONTINUE_DEVDATA_HOST=127.0.0.1 \
                SOMA_CONTINUE_DEVDATA_PORT="$continue_live_port" \
                "$BIN" clients --json --command "$BIN" --client continue >"$continue_live_clients"; then
                pass "continue live collector readiness renders"
                expect_json_field "continue live collector is detected by TCP probe" "$continue_live_clients" \
                    "data['clients'][0]['continue_extension_config_check']['devdata_destination_visible'] is True and data['clients'][0]['continue_extension_config_check']['devdata_collector_status'] == 'listening' and data['clients'][0]['continue_extension_config_check']['devdata_collector_listening'] is True and data['clients'][0]['continue_extension_config_check']['devdata_collector_host'] == '127.0.0.1' and data['clients'][0]['continue_extension_config_check']['devdata_collector_port'] == $continue_live_port and 'tools/soma-continue-devdata-collector.py' not in data['operator_card']['primary_next_command'] and 'tools/soma-client-hook-readiness.sh' in data['operator_card']['primary_next_command']"
            else
                fail "continue live collector readiness renders"
            fi
            if post_continue_devdata "$continue_live_port" "$continue_live_post_response"; then
                pass "continue live collector accepts dev-data POST"
                expect_json_field "continue live collector POST stays proof-free non-release dogfood observation" "$continue_live_post_response" \
                    "data['ok'] is True and data['client'] == 'continue' and data['event_source'] == 'continue_private_lifecycle_hook' and data['collector_release_grade_candidate'] is False and 'dogfood_or_synthetic_test_event' in data['collector_release_grade_reasons'] and data['adapter_lifecycle']['contract'] == 'client_lifecycle_events_normalize_to_adapter_spool_without_direct_promotion' and data['adapter_lifecycle']['append']['kind'] == data['adapter_lifecycle']['normalized_kind'] and data['adapter_lifecycle']['append']['appended_bytes'] > 0"
                run_step "continue live collector writes private event jsonl" grep -q "continue_private_lifecycle_hook" "$continue_live_jsonl"
                run_step "continue live collector stamps installed binding nonce" grep -q "$continue_binding_nonce" "$continue_live_jsonl"
                if "$BIN" adapter-binding-proof --list --client continue --json >"$continue_live_proofs"; then
                    expect_json_field "continue live collector POST records no proof row" "$continue_live_proofs" \
                        "len(data['proofs']) == 0"
                else
                    fail "continue live collector POST records no proof row"
                fi
            else
                fail "continue live collector accepts dev-data POST"
            fi
        else
            fail "continue live collector starts TCP listener"
        fi
    fi
else
    fail "continue target installed config renders proof-free"
fi

clients_degraded_db_dir="$RUN_DIR/not-a-db"
mkdir -p "$clients_degraded_db_dir"
clients_degraded="$RUN_DIR/clients-proof-storage-unavailable.json"
if "$BIN" clients --json --command "$BIN" --db-path "$clients_degraded_db_dir" >"$clients_degraded"; then
    pass "client readiness degraded proof storage renders recovery commands"
    expect_json_field "client readiness degraded proof storage keeps MCP visible" "$clients_degraded" \
        "data['proof_storage_status'] == 'unavailable' and data['summary']['proof_storage_unavailable'] is True and data['summary']['mcp_registration_ready_count'] == 5"
    expect_json_field "client readiness degraded readiness index mirrors proof storage" "$clients_degraded" \
        "data['readiness_index']['proof_storage_unavailable'] is True and data['readiness_index']['status'] == 'proof_storage_unavailable' and data['readiness_index']['mcp_ready_clients'] == data['operator_card']['mcp_ready_clients']"
    expect_json_field "client readiness degraded proof storage blocks private claims" "$clients_degraded" \
        "data['operator_card']['status'] == 'proof_storage_unavailable' and any('proof storage is unreadable' in claim for claim in data['operator_card']['blocked_claims'])"
    expect_json_field "client readiness degraded proof storage offers recovery commands" "$clients_degraded" \
        "any('--db-path' in cmd and any('soma-client-readiness-diagnostic-' in part for part in cmd) for cmd in data['operator_card'].get('proof_storage_recovery_commands', [])) and any('--db-path' in cmd and '<readable-soma.db>' in cmd for cmd in data['operator_card'].get('proof_storage_recovery_commands', [])) and any('diagnose' in cmd for cmd in data['operator_card'].get('proof_storage_recovery_commands', []))"
else
    fail "client readiness degraded proof storage renders recovery commands"
fi

echo
echo "=== summary ==="
echo "  pass: $PASS"
echo "  warn: $WARN"
echo "  fail: $FAIL"
if [[ -f "$REAL_PRIVATE_SNAPSHOT_JSON" ]]; then
    snapshot_status="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "data['status']" 2>/dev/null || printf unavailable)"
    snapshot_ready="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "data['ready']" 2>/dev/null || printf false)"
    snapshot_ready_clients="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "','.join(data.get('ready_clients') or ['none'])" 2>/dev/null || printf none)"
    snapshot_pending_clients="$(json_get "$REAL_PRIVATE_SNAPSHOT_JSON" "','.join(data.get('pending_clients') or ['unknown'])" 2>/dev/null || printf unknown)"
    echo "  real private app release snapshot: $snapshot_status ready=$snapshot_ready ready_clients=$snapshot_ready_clients pending_clients=$snapshot_pending_clients"
fi
echo "  private app release proof: read-only snapshot only; this run records no proof rows"
if [[ $JSON_OUT_DISABLED -eq 1 ]]; then
    if [[ -n "${REAL_HOME:-}" ]]; then
        echo "  json: disabled by --no-json-out; latest artifact not updated at $REAL_HOME/.soma/reports/client-dogfood-latest.json"
    else
        echo "  json: disabled by --no-json-out; latest artifact not updated"
    fi
elif [[ -n "$JSON_OUT" ]]; then
    JSON_WRITE_ERR="$RUN_DIR/json-write.err"
    if write_json_report 2>"$JSON_WRITE_ERR"; then
        echo "  json: $JSON_OUT"
    elif [[ $JSON_OUT_EXPLICIT -eq 1 ]]; then
        echo "  json: failed to write $JSON_OUT" >&2
        cat "$JSON_WRITE_ERR" >&2
        exit 1
    else
        echo "  json: skipped; could not write default $JSON_OUT" >&2
    fi
fi
if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
if [[ $WARN -gt 0 ]]; then
    echo "  DOGFOOD READY (operator flow, with warnings)"
else
    echo "  DOGFOOD READY (operator flow)"
fi
if [[ "${snapshot_ready:-false}" == "True" || "${snapshot_ready:-false}" == "true" ]]; then
    echo "  PRIVATE APP RELEASE PROOF READY (from stored real-home proof ledger snapshot)"
else
    echo "  PRIVATE APP RELEASE PROOF PENDING"
fi
