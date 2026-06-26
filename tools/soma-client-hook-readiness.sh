#!/usr/bin/env bash
# soma-client-hook-readiness - read-only private client app-hook readiness probe.
#
# This script is intentionally generic across codex-app, cursor, and continue.
# It records no proof rows. It checks installed binding config discovery,
# adapter spool metadata, optional client logs, and the SOMA proof-session card
# so an operator can see the next real-client proof step.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-$HOME}"
PROJECT_ROOT="${SOMA_CLIENT_BINDING_PROJECT_ROOT:-$ROOT}"
EVENT_JSONL="${SOMA_CLIENT_BINDING_EVENT_JSONL:-$HOME/.soma/adapter/events.jsonl}"
LOG_ROOT="${SOMA_CLIENT_BINDING_LOG_ROOT:-}"
MANIFEST_OVERRIDE="${SOMA_CLIENT_BINDING_MANIFEST:-}"
WAIT_SECONDS="${SOMA_CLIENT_BINDING_WAIT_SECONDS:-}"
WAIT_INTERVAL_MS="${SOMA_CLIENT_BINDING_WAIT_INTERVAL_MS:-}"

usage() {
    cat <<EOF
Usage: tools/soma-client-hook-readiness.sh [OPTIONS]

Read-only private client app-hook readiness probe. Records no proof rows.

Options:
  --client CLIENT
  --soma-bin PATH
  --manifest PATH
  --config-root PATH
  --project-root PATH
  --event-jsonl PATH
  --log-root PATH
  --wait-seconds SECONDS
  --wait-interval-ms MILLIS
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
        --wait-seconds)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --wait-seconds" >&2
                exit 2
            fi
            WAIT_SECONDS="$2"
            shift 2
            ;;
        --wait-interval-ms)
            if [[ $# -lt 2 ]]; then
                echo "missing value for --wait-interval-ms" >&2
                exit 2
            fi
            WAIT_INTERVAL_MS="$2"
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

DEFAULT_MANIFEST="$ROOT/tools/client-bindings/$CLIENT-soma-binding.json.example"
MANIFEST="${MANIFEST_OVERRIDE:-$DEFAULT_MANIFEST}"

if [[ "$CLIENT" == "cursor" ]]; then
    MANIFEST="${MANIFEST_OVERRIDE:-${SOMA_CURSOR_BINDING_MANIFEST:-$MANIFEST}}"
    CONFIG_ROOT="${SOMA_CLIENT_BINDING_CONFIG_ROOT:-${SOMA_CURSOR_CONFIG_ROOT:-$CONFIG_ROOT}}"
    PROJECT_ROOT="${SOMA_CLIENT_BINDING_PROJECT_ROOT:-${SOMA_CURSOR_PROJECT_ROOT:-$PROJECT_ROOT}}"
    EVENT_JSONL="${SOMA_CLIENT_BINDING_EVENT_JSONL:-${SOMA_CURSOR_EVENT_JSONL:-$EVENT_JSONL}}"
    LOG_ROOT="${SOMA_CLIENT_BINDING_LOG_ROOT:-${SOMA_CURSOR_LOG_ROOT:-$HOME/Library/Application Support/Cursor/logs}}"
fi

if [[ -n "$WAIT_SECONDS" ]]; then
    export SOMA_CLIENT_BINDING_WAIT_SECONDS="$WAIT_SECONDS"
fi
if [[ -n "$WAIT_INTERVAL_MS" ]]; then
    export SOMA_CLIENT_BINDING_WAIT_INTERVAL_MS="$WAIT_INTERVAL_MS"
fi

python3 - \
    "$ROOT" \
    "$BIN" \
    "$CLIENT" \
    "$CONFIG_ROOT" \
    "$PROJECT_ROOT" \
    "$LOG_ROOT" \
    "$EVENT_JSONL" \
    "$MANIFEST" <<'PY'
import json
import os
import pathlib
import re
import subprocess
import sys
import time

(
    root,
    bin_path,
    client,
    config_root,
    project_root,
    log_root,
    event_jsonl,
    manifest,
) = sys.argv[1:]


def load_manifest():
    try:
        with open(manifest, "r", encoding="utf-8") as f:
            data = json.load(f)
        event_source = (
            data.get("lifecycle", {}).get("event_source")
            or f"{client}_private_lifecycle_hook"
        )
        return data, event_source
    except Exception as exc:
        return {"error": str(exc)}, f"{client}_private_lifecycle_hook"


manifest_json, expected_event_source = load_manifest()
project_hook_path = str(pathlib.Path(project_root) / ".cursor" / "hooks.json")


def run_soma(args):
    proc = subprocess.run(
        [bin_path, *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=os.environ.copy(),
    )
    parsed = None
    json_error = None
    if proc.stdout.strip():
        try:
            parsed = json.loads(proc.stdout)
        except Exception as exc:
            json_error = str(exc)
    return {
        "ok": proc.returncode == 0 and json_error is None,
        "returncode": proc.returncode,
        "stderr": proc.stderr.strip(),
        "stdout": proc.stdout.strip() if json_error else None,
        "json_error": json_error,
        "json": parsed,
    }


def discover_installed_config():
    return run_soma(
        [
            "adapter-binding-proof",
            "--discover-installed-config",
            "--manifest",
            manifest,
            "--client",
            client,
            "--config-root",
            config_root,
        ]
    )


def proof_session():
    return run_soma(
        [
            "adapter-binding-proof",
            "--proof-session",
            "--manifest",
            manifest,
            "--client",
            client,
            "--config-root",
            config_root,
        ]
    )


def compact_discovery(discovery_result):
    data = discovery_result.get("json") or {}
    candidates = []
    for candidate in data.get("candidates", []) if isinstance(data, dict) else []:
        checks = candidate.get("checks") or {}
        candidates.append(
            {
                "path": candidate.get("path"),
                "exists": candidate.get("exists"),
                "eligible_for_observed_app_hook": candidate.get(
                    "eligible_for_observed_app_hook"
                ),
                "missing_requirements": candidate.get("missing_requirements") or [],
                "binding_nonce": checks.get("binding_nonce"),
                "fingerprint": checks.get("fingerprint"),
                "modified_at_ns": checks.get("modified_at_ns"),
            }
        )
    return {
        "ok": discovery_result.get("ok"),
        "client": data.get("client") if isinstance(data, dict) else None,
        "config_root": data.get("config_root") if isinstance(data, dict) else config_root,
        "expected_event_source": data.get("expected_event_source")
        if isinstance(data, dict)
        else expected_event_source,
        "candidates_found": data.get("candidates_found", 0)
        if isinstance(data, dict)
        else 0,
        "eligible_candidates": data.get("eligible_candidates", 0)
        if isinstance(data, dict)
        else 0,
        "candidates": candidates,
        "error": discovery_result.get("stderr") if not discovery_result.get("ok") else None,
    }


def compact_proof_session(session_result):
    data = session_result.get("json") or {}
    session = {}
    if isinstance(data, dict):
        proof_session = data.get("proof_session") or {}
        if isinstance(proof_session, dict) and "status" in proof_session:
            session = proof_session
        elif isinstance(proof_session, dict):
            session = proof_session.get("proof_session") or {}
    runbook = session.get("runbook") or {}
    return {
        "ok": session_result.get("ok"),
        "status": session.get("status"),
        "release_gate": session.get("release_gate"),
        "next_step_id": session.get("next_step_id"),
        "next_operator_step_id": (session.get("next_operator_step") or {}).get("id"),
        "blocked_proof_levels": session.get("blocked_proof_levels") or [],
        "completed_proof_levels": session.get("completed_proof_levels") or [],
        "runbook_schema": runbook.get("schema"),
        "runbook_target_next_step_id": runbook.get("target_next_step_id"),
        "error": session_result.get("stderr") if not session_result.get("ok") else None,
    }


def normalized_path(path):
    return str(path).replace("\\", "/")


def known_private_client_target_relpaths(client_name):
    client_key = client_name.lower()
    mapping = {
        "codex-app": [".codex/soma-installed-binding.json"],
        "cursor": [".cursor/soma-installed-binding.json"],
        "continue": [".continue/soma-installed-binding.json"],
        "claude-code": [".claude/soma-installed-binding.json"],
    }
    return mapping.get(client_key, [f".{client_key}/soma-installed-binding.json"])


private_client_target_relpaths = known_private_client_target_relpaths(client)


def is_private_client_target_path(path):
    normalized = normalized_path(path).strip("/")
    return any(
        normalized == relpath or normalized.endswith(f"/{relpath}")
        for relpath in private_client_target_relpaths
    )


CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY = (
    "codex_notify_reload_check_is_read_only: compares local Codex app process "
    "start time with notify config mtime only; records no proof row, creates no "
    "verification event, installs no hook, promotes no cloud draft, and cannot "
    "substitute for a real Codex app hook event"
)

CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY = (
    "continue_extension_config_check_is_read_only: inspects local Continue "
    "extension config visibility only; records no proof row, creates no "
    "verification event, installs no hook, promotes no cloud draft, and cannot "
    "substitute for a real Continue hook event"
)


def parse_ps_lstart_unix(value):
    try:
        return int(time.mktime(time.strptime(value, "%a %b %d %H:%M:%S %Y")))
    except Exception:
        return None


def codex_process_table_lines():
    fixture = os.environ.get("SOMA_CODEX_NOTIFY_PS_OUTPUT")
    if fixture:
        fixture_path = pathlib.Path(fixture)
        if fixture_path.is_file():
            try:
                return {
                    "source": "env_file:SOMA_CODEX_NOTIFY_PS_OUTPUT",
                    "status": "available",
                    "lines": fixture_path.read_text(encoding="utf-8", errors="replace").splitlines(),
                    "error": None,
                }
            except Exception as exc:
                return {
                    "source": "env_file:SOMA_CODEX_NOTIFY_PS_OUTPUT",
                    "status": "unavailable",
                    "lines": [],
                    "error": str(exc),
                }
        return {
            "source": "env_literal:SOMA_CODEX_NOTIFY_PS_OUTPUT",
            "status": "available",
            "lines": fixture.splitlines(),
            "error": None,
        }
    if os.environ.get("SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK") == "1":
        return {
            "source": "env:SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK",
            "status": "skipped",
            "lines": [],
            "error": None,
        }
    try:
        proc = subprocess.run(
            ["ps", "-axo", "pid,lstart,command"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except Exception as exc:
        return {"source": "ps", "status": "unavailable", "lines": [], "error": str(exc)}
    if proc.returncode != 0:
        return {
            "source": "ps",
            "status": "unavailable",
            "lines": [],
            "error": proc.stderr.strip(),
        }
    return {
        "source": "ps",
        "status": "available",
        "lines": proc.stdout.splitlines(),
        "error": None,
    }


def codex_desktop_process_from_ps_line(line, config_mtime_unix):
    parts = line.split()
    if len(parts) < 7:
        return None
    try:
        pid = int(parts[0])
    except Exception:
        return None
    started_at = parse_ps_lstart_unix(" ".join(parts[1:6]))
    if started_at is None:
        return None
    command = " ".join(parts[6:])
    if command != "/Applications/Codex.app/Contents/MacOS/Codex":
        return None
    return {
        "pid": pid,
        "started_at_unix": started_at,
        "started_before_config": started_at < config_mtime_unix,
        "command": command,
    }


def codex_notify_reload_check():
    if client != "codex-app":
        return None
    config_path = pathlib.Path(
        os.environ.get("SOMA_CODEX_NOTIFY_CONFIG")
        or str(pathlib.Path(config_root) / ".codex" / "config.toml")
    )
    config_path_display = str(config_path)
    try:
        config_mtime_unix = int(config_path.stat().st_mtime)
    except Exception as exc:
        return {
            "source": "config_metadata",
            "status": "config_missing" if not config_path.exists() else "config_metadata_unavailable",
            "config_path": config_path_display,
            "config_mtime_unix": None,
            "codex_desktop_process_count": 0,
            "stale_codex_desktop_process_count": 0,
            "restart_recommended": False,
            "stale_processes": [],
            "error": str(exc),
            "trust_boundary": CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
        }

    process_table = codex_process_table_lines()
    if process_table.get("status") != "available":
        return {
            "source": process_table.get("source"),
            "status": process_table.get("status"),
            "config_path": config_path_display,
            "config_mtime_unix": config_mtime_unix,
            "codex_desktop_process_count": 0,
            "stale_codex_desktop_process_count": 0,
            "restart_recommended": False,
            "stale_processes": [],
            "error": process_table.get("error"),
            "trust_boundary": CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
        }

    processes = [
        process
        for line in process_table.get("lines", [])
        for process in [codex_desktop_process_from_ps_line(line, config_mtime_unix)]
        if process is not None
    ]
    stale = [process for process in processes if process.get("started_before_config")]
    if stale:
        status = "restart_recommended"
    elif not processes:
        status = "codex_app_not_running"
    else:
        status = "codex_app_started_after_config"
    return {
        "source": process_table.get("source"),
        "status": status,
        "config_path": config_path_display,
        "config_mtime_unix": config_mtime_unix,
        "codex_desktop_process_count": len(processes),
        "stale_codex_desktop_process_count": len(stale),
        "restart_recommended": bool(stale),
        "stale_processes": stale[:5],
        "error": None,
        "trust_boundary": CODEX_NOTIFY_RELOAD_CHECK_TRUST_BOUNDARY,
    }


def continue_extension_config_check():
    if client != "continue":
        return None
    extension_check = continue_extension_installation_check()
    env_config = os.environ.get("SOMA_CONTINUE_CONFIG") or os.environ.get(
        "SOMA_CONTINUE_CONFIG_PATH"
    )
    if env_config:
        candidate_paths = [pathlib.Path(env_config)]
    else:
        continue_dir = pathlib.Path(config_root) / ".continue"
        candidate_paths = [
            continue_dir / "mcpServers" / "soma.json",
            continue_dir / "config.yaml",
            continue_dir / "config.yml",
            continue_dir / "config.json",
            continue_dir / "config.ts",
        ]
    candidate_path_strings = [str(path) for path in candidate_paths]
    recommended_config_path = (
        candidate_path_strings[0]
        if candidate_path_strings
        else "~/.continue/mcpServers/soma.json"
    )
    mcp_config_command = [
        bin_path,
        "mcp-config",
        "--client",
        "continue",
        "--command",
        bin_path,
    ]

    def continue_next_step(status):
        if status == "config_present_soma_mcp_seen":
            if not extension_check.get("extension_observed"):
                return (
                    "Continue can see a SOMA MCP server file or mcpServers entry, "
                    "but no local Continue extension install was observed; install or enable "
                    "Continue, reload the editor, run a real turn, then rerun this readiness probe."
                )
            return (
                "Continue can see a SOMA MCP server file or mcpServers entry; "
                "reload Continue, run a real turn, then rerun this readiness probe."
            )
        if status == "config_present_soma_mcp_profile_invalid":
            return (
                "Continue can see SOMA MCP config, but the local Continue profile "
                "config.yaml/config.yml is rejected because required top-level fields "
                "such as name/version are missing or unreadable; run "
                "tools/soma-continue-devdata-install.py --dry-run, write the repair "
                "if correct, reload Continue, then rerun this readiness probe."
            )
        if status == "config_profile_invalid":
            return (
                "Repair Continue's config.yaml/config.yml top-level name/version "
                "fields, write the mcp_config_command output if SOMA MCP is still "
                "missing, reload Continue, then rerun this readiness probe."
            )
        if status == "config_present_soma_mcp_missing":
            return (
                "Write the mcp_config_command output to "
                f"{recommended_config_path}, reload Continue, then complete a real turn."
            )
        if status == "config_unreadable":
            return (
                f"Make {recommended_config_path} readable, write the mcp_config_command "
                "output there if needed, reload Continue, then rerun readiness."
            )
        return (
            f"Create {recommended_config_path} from mcp_config_command, reload Continue, "
            "then complete a real turn before recording observed_app_hook proof."
        )

    def profile_has_top_level_key(text, required_key):
        for line in text.splitlines():
            trimmed = line.lstrip()
            if not trimmed or trimmed.startswith("#") or len(trimmed) != len(line):
                continue
            key, sep, _ = trimmed.partition(":")
            if sep and key.strip() == required_key:
                return True
        return False

    def profile_missing_required_fields(text):
        return [
            field
            for field in ["name", "version"]
            if not profile_has_top_level_key(text, field)
        ]

    def profile_config_check():
        profile_paths = [
            path
            for path in candidate_paths
            if path.name.lower() in {"config.yaml", "config.yml"}
        ]
        first_unreadable = None
        for config_path in [path for path in profile_paths if path.exists()]:
            try:
                text = config_path.read_text(encoding="utf-8", errors="replace")
            except Exception as exc:
                if first_unreadable is None:
                    first_unreadable = (str(config_path), str(exc))
                continue
            missing = profile_missing_required_fields(text)
            if not missing:
                return {
                    "profile_config_status": "profile_config_required_fields_seen",
                    "profile_config_path": str(config_path),
                    "profile_config_required_fields_present": True,
                    "profile_config_missing_required_fields": [],
                    "profile_config_error": None,
                }
            return {
                "profile_config_status": "profile_config_missing_required_fields",
                "profile_config_path": str(config_path),
                "profile_config_required_fields_present": False,
                "profile_config_missing_required_fields": missing,
                "profile_config_error": None,
            }
        if first_unreadable is not None:
            path, error = first_unreadable
            return {
                "profile_config_status": "profile_config_unreadable",
                "profile_config_path": path,
                "profile_config_required_fields_present": False,
                "profile_config_missing_required_fields": ["name", "version"],
                "profile_config_error": error,
            }
        return {
            "profile_config_status": "profile_config_not_present",
            "profile_config_path": None,
            "profile_config_required_fields_present": True,
            "profile_config_missing_required_fields": [],
            "profile_config_error": None,
        }

    def profile_config_blocks(profile_check):
        return profile_check.get("profile_config_status") in {
            "profile_config_missing_required_fields",
            "profile_config_unreadable",
        }

    def status_with_profile(base_status, profile_check):
        if not profile_config_blocks(profile_check):
            return base_status
        if base_status == "config_present_soma_mcp_seen":
            return "config_present_soma_mcp_profile_invalid"
        if base_status in {"config_present_soma_mcp_missing", "config_missing"}:
            return "config_profile_invalid"
        return base_status

    def soma_mcp_flags(path, text):
        lowered = text.lower()
        has_model_context_protocol = "modelcontextprotocol" in lowered
        has_mcp_servers = "mcpservers" in lowered or "/mcpservers/" in str(path).lower()
        has_soma_server = ("soma" in lowered or path.stem.lower() == "soma") and (
            "mcp-serve" in lowered or has_model_context_protocol or has_mcp_servers
        )
        return has_model_context_protocol, has_mcp_servers, has_soma_server

    profile_check = profile_config_check()
    first_existing_missing = None
    first_unreadable = None
    for config_path in [path for path in candidate_paths if path.exists()]:
        try:
            text = config_path.read_text(encoding="utf-8", errors="replace")
        except Exception as exc:
            if first_unreadable is None:
                first_unreadable = {
                    "source": "continue_config_scan",
                    "status": "config_unreadable",
                    "candidate_paths": candidate_path_strings,
                    "config_path": str(config_path),
                    **profile_check,
                    **extension_check,
                    "recommended_config_path": recommended_config_path,
                    "mcp_config_command": mcp_config_command,
                    "merge_required": True,
                    "next_step": continue_next_step("config_unreadable"),
                    "has_model_context_protocol": False,
                    "has_mcp_servers": False,
                    "has_soma_server": False,
                    "restart_or_reload_recommended": False,
                    "error": str(exc),
                    "trust_boundary": CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
                }
            continue
        (
            has_model_context_protocol,
            has_mcp_servers,
            has_soma_server,
        ) = soma_mcp_flags(config_path, text)
        base_status = (
            "config_present_soma_mcp_seen"
            if (has_model_context_protocol or has_mcp_servers) and has_soma_server
            else "config_present_soma_mcp_missing"
        )
        status = status_with_profile(base_status, profile_check)
        check = {
            "source": "continue_config_scan",
            "status": status,
            "candidate_paths": candidate_path_strings,
            "config_path": str(config_path),
            **profile_check,
            **extension_check,
            "recommended_config_path": recommended_config_path,
            "mcp_config_command": mcp_config_command,
            "merge_required": status != "config_present_soma_mcp_seen",
            "next_step": continue_next_step(status),
            "has_model_context_protocol": has_model_context_protocol,
            "has_mcp_servers": has_mcp_servers,
            "has_soma_server": has_soma_server,
            "restart_or_reload_recommended": status == "config_present_soma_mcp_seen",
            "error": None,
            "trust_boundary": CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
        }
        if status == "config_present_soma_mcp_seen":
            return check
        if first_existing_missing is None:
            first_existing_missing = check

    if first_existing_missing is not None:
        return first_existing_missing
    if first_unreadable is not None:
        return first_unreadable
    return {
        "source": "continue_config_scan",
        "status": "config_missing",
        "candidate_paths": candidate_path_strings,
        "config_path": None,
        **profile_check,
        **extension_check,
        "recommended_config_path": recommended_config_path,
        "mcp_config_command": mcp_config_command,
        "merge_required": True,
        "next_step": continue_next_step("config_missing"),
        "has_model_context_protocol": False,
        "has_mcp_servers": False,
        "has_soma_server": False,
        "restart_or_reload_recommended": False,
        "error": None,
        "trust_boundary": CONTINUE_EXTENSION_CONFIG_TRUST_BOUNDARY,
    }


def continue_extension_installation_check():
    env_path = os.environ.get("SOMA_CONTINUE_EXTENSION_PATH")
    if env_path:
        candidate_roots = [pathlib.Path(env_path)]
    else:
        root = pathlib.Path(config_root)
        candidate_roots = [
            root / ".vscode" / "extensions",
            root / ".cursor" / "extensions",
            root / "Library" / "Application Support" / "Code" / "User" / "globalStorage",
            root / "Library" / "Application Support" / "Cursor" / "User" / "globalStorage",
        ]

    def is_continue_path(path):
        return "continue" in path.name.lower()

    extension_paths = []
    for root in candidate_roots:
        if not root.exists():
            continue
        if is_continue_path(root):
            extension_paths.append(root)
            continue
        if not root.is_dir():
            continue
        try:
            for child in root.iterdir():
                if is_continue_path(child):
                    extension_paths.append(child)
        except Exception:
            continue
    extension_path_strings = sorted({str(path) for path in extension_paths})
    observed = bool(extension_path_strings)
    return {
        "extension_installation_status": (
            "extension_observed" if observed else "extension_not_observed"
        ),
        "extension_candidate_roots": [str(path) for path in candidate_roots],
        "extension_paths": extension_path_strings,
        "extension_observed": observed,
        "extension_next_step": (
            "Continue extension installation is locally observable; reload the extension/editor, "
            "run a real Continue turn, then rerun this readiness probe."
            if observed
            else "No local Continue extension install was observed in common VS Code/Cursor "
            "extension paths; install or enable Continue, reload the editor, run a real turn, "
            "then rerun this readiness probe."
        ),
    }


def summarize_logs():
    if not log_root:
        return {
            "available": False,
            "log_root": log_root,
            "client_log_observation": False,
            "reason": "no_log_root_configured",
        }
    root_path = pathlib.Path(log_root)
    files = []
    if root_path.exists():
        try:
            files = sorted(
                [path for path in root_path.rglob("*") if path.is_file()],
                key=lambda path: path.stat().st_mtime,
                reverse=True,
            )
        except Exception:
            files = []

    loaded_re = re.compile(r"Loaded\s+(\d+)\s+project hook\(s\) for steps:\s*(.*)$")
    requested_re = re.compile(r"Hook step requested:\s*([A-Za-z0-9_-]+)")
    found_re = re.compile(r"Found\s+(\d+)\s+hook\(s\) to execute for step:\s*([A-Za-z0-9_-]+)")
    merged_re = re.compile(r"Merged\s+(\d+)\s+valid response\(s\) for step\s+([A-Za-z0-9_-]+)")

    project_config_seen = False
    latest_project_log = None
    loaded_steps = []
    loaded_project_hook_count = 0
    requested_steps = []
    found_steps = []
    merged_steps = []
    event_source_seen = False
    error_lines = []

    for path in files[:30]:
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except Exception:
            continue
        if expected_event_source in text:
            event_source_seen = True
            latest_project_log = latest_project_log or str(path)
        project_related = client != "cursor" or project_hook_path in text or "Project config path" in text
        if client == "cursor" and project_hook_path in text:
            project_config_seen = True
            latest_project_log = latest_project_log or str(path)
        if not project_related:
            continue
        for line in text.splitlines():
            loaded_match = loaded_re.search(line)
            if loaded_match:
                loaded_project_hook_count = max(
                    loaded_project_hook_count,
                    int(loaded_match.group(1)),
                )
                loaded_steps = [
                    step.strip()
                    for step in loaded_match.group(2).split(",")
                    if step.strip()
                ]
            requested_match = requested_re.search(line)
            if requested_match:
                requested_steps.append(requested_match.group(1))
            found_match = found_re.search(line)
            if found_match:
                found_steps.append(
                    {
                        "step": found_match.group(2),
                        "hook_count": int(found_match.group(1)),
                    }
                )
            merged_match = merged_re.search(line)
            if merged_match:
                merged_steps.append(
                    {
                        "step": merged_match.group(2),
                        "valid_response_count": int(merged_match.group(1)),
                    }
                )
            lower = line.lower()
            if "error" in lower or "failed" in lower:
                error_lines.append(line[-500:])

    client_log_observation = (
        event_source_seen
        or len(requested_steps) > 0
        or len(found_steps) > 0
        or len(merged_steps) > 0
    )
    loaded_step_set = {step for step in loaded_steps if step}
    requested_step_set = {step for step in requested_steps if step}
    configured_requested_steps = sorted(loaded_step_set.intersection(requested_step_set))
    unconfigured_requested_steps = sorted(requested_step_set.difference(loaded_step_set))
    configured_found_steps = [
        item for item in found_steps if item.get("step") in loaded_step_set
    ]
    configured_merged_steps = [
        item for item in merged_steps if item.get("step") in loaded_step_set
    ]
    return {
        "available": root_path.exists(),
        "log_root": log_root,
        "project_hook_path": project_hook_path if client == "cursor" else None,
        "log_file_count_scanned": len(files[:30]),
        "latest_project_log": latest_project_log,
        "project_config_seen": project_config_seen,
        "loaded_project_hook_count": loaded_project_hook_count,
        "loaded_steps": sorted(set(loaded_steps)),
        "hook_step_requested_count": len(requested_steps),
        "hook_steps_requested": sorted(set(requested_steps)),
        "configured_hook_step_requested_count": len(configured_requested_steps),
        "configured_hook_steps_requested": configured_requested_steps,
        "unconfigured_hook_steps_requested": unconfigured_requested_steps,
        "found_steps": found_steps[-5:],
        "configured_found_steps": configured_found_steps[-5:],
        "merged_steps": merged_steps[-5:],
        "configured_merged_steps": configured_merged_steps[-5:],
        "event_source_seen": event_source_seen,
        "client_log_observation": client_log_observation,
        "recent_error_lines": error_lines[-5:],
    }


ALLOWED_CLOCK_SKEW_NS = 1_000_000_000
NON_RELEASE_DOGFOOD_MARKERS = (
    "dogfood",
    "local-dogfood",
    "soma-test",
    "soma_continue_collector_ok",
)


def candidate_modified_at_floor_by_nonce(candidates):
    floor_by_nonce = {}
    for candidate in candidates:
        nonce = candidate.get("binding_nonce")
        modified_at = candidate.get("modified_at_ns")
        if not nonce or not isinstance(modified_at, int):
            continue
        floor_by_nonce[nonce] = max(floor_by_nonce.get(nonce, 0), modified_at)
    return floor_by_nonce


def temporal_binding_ok(observed_at_ns, event_file_modified_at_ns, config_modified_at_ns):
    if not isinstance(observed_at_ns, int) or not isinstance(config_modified_at_ns, int):
        return False
    if observed_at_ns + ALLOWED_CLOCK_SKEW_NS < config_modified_at_ns:
        return False
    if (
        isinstance(event_file_modified_at_ns, int)
        and event_file_modified_at_ns + ALLOWED_CLOCK_SKEW_NS < config_modified_at_ns
    ):
        return False
    return True


def non_release_manual_marker(value):
    if not isinstance(value, str):
        return False
    normalized = value.strip().lower()
    return (
        "manual_debug" in normalized
        or "manual-template" in normalized
        or "manual_template" in normalized
        or "non_release" in normalized
        or "non-release" in normalized
    )


def non_release_manual_event(summary):
    if not isinstance(summary, dict):
        return False
    return non_release_manual_marker(summary.get("hook_adapter")) or non_release_manual_marker(
        summary.get("manual_invocation_policy")
    )


def non_release_test_marker(value):
    if not isinstance(value, str):
        return False
    normalized = value.strip().lower()
    return any(marker in normalized for marker in NON_RELEASE_DOGFOOD_MARKERS)


def non_release_test_event(summary):
    if not isinstance(summary, dict):
        return False
    if summary.get("collector_release_grade_candidate") is False:
        return True
    return any(
        non_release_test_marker(summary.get(key))
        for key in [
            "continue_profile_id",
            "session_id",
            "thread_id",
            "model_provider",
            "model_name",
            "model_title",
            "prompt_text",
            "response_text",
            "output_text",
        ]
    )


def summarize_spool(eligible_binding_nonces, binding_nonce_modified_at_floor):
    path = pathlib.Path(event_jsonl)
    event_count = 0
    matching_count = 0
    matching_nonce_count = 0
    matching_manual_debug_count = 0
    matching_manual_debug_nonce_count = 0
    matching_non_release_test_count = 0
    matching_non_release_test_nonce_count = 0
    matching_temporal_count = 0
    matching_nonce_temporal_count = 0
    latest_matching = None
    latest_manual_debug = None
    latest_non_release_test = None
    latest_matching_temporal = None
    latest_event = None
    temporal_failures = []
    if not path.exists():
        return {
            "path": event_jsonl,
            "exists": False,
            "event_count": 0,
            "matching_private_event_count": 0,
            "matching_private_binding_nonce_count": 0,
            "matching_private_non_release_manual_event_count": 0,
            "matching_private_non_release_manual_binding_nonce_count": 0,
            "matching_private_non_release_test_event_count": 0,
            "matching_private_non_release_test_binding_nonce_count": 0,
            "matching_private_temporal_binding_count": 0,
            "matching_private_binding_nonce_temporal_count": 0,
            "temporal_binding_failures": [],
            "latest_matching_event": None,
            "latest_manual_debug_event": None,
            "latest_non_release_test_event": None,
            "latest_matching_temporal_event": None,
            "latest_event": None,
        }
    try:
        event_file_modified_at_ns = path.stat().st_mtime_ns
    except Exception:
        event_file_modified_at_ns = None
    try:
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                try:
                    event = json.loads(line)
                except Exception:
                    continue
                event_count += 1
                payload = event.get("payload") if isinstance(event, dict) else {}
                if not isinstance(payload, dict):
                    payload = {}
                binding_nonce = payload.get("binding_nonce") or event.get("binding_nonce")
                payload_client = payload.get("client") or payload.get("source")
                summary = {
                    "kind": event.get("kind"),
                    "schema": event.get("schema"),
                    "writer_contract": event.get("writer_contract"),
                    "observed_at_ns": event.get("observed_at_ns"),
                    "client": payload_client,
                    "event_source": payload.get("event_source"),
                    "binding_nonce": binding_nonce,
                    "hook_adapter": payload.get("hook_adapter"),
                    "manual_invocation_policy": payload.get("manual_invocation_policy"),
                    "collector_release_grade_candidate": payload.get(
                        "collector_release_grade_candidate"
                    ),
                    "collector_release_grade_reasons": payload.get(
                        "collector_release_grade_reasons"
                    ),
                    "continue_profile_id": payload.get("continue_profile_id"),
                    "session_id": payload.get("session_id"),
                    "thread_id": payload.get("thread_id"),
                    "model_provider": payload.get("model_provider"),
                    "model_name": payload.get("model_name"),
                    "model_title": payload.get("model_title"),
                    "prompt_text": payload.get("prompt_text"),
                    "response_text": payload.get("response_text"),
                    "output_text": payload.get("output_text"),
                    "has_prompt_text": bool(payload.get("prompt_text")),
                    "has_response_text": bool(payload.get("response_text")),
                    "has_output_text": bool(payload.get("output_text")),
                }
                latest_event = summary
                private_match = (
                    summary["schema"] == "soma.adapter_spool_event.v1"
                    and summary["writer_contract"] == "soma_adapter_spool_append_v1"
                    and str(summary["client"]).lower() == client.lower()
                    and summary["event_source"] == expected_event_source
                    and isinstance(summary["observed_at_ns"], int)
                )
                if private_match:
                    if non_release_manual_event(summary):
                        matching_manual_debug_count += 1
                        latest_manual_debug = summary
                        if binding_nonce in eligible_binding_nonces:
                            matching_manual_debug_nonce_count += 1
                        latest_event = summary
                        continue
                    if non_release_test_event(summary):
                        matching_non_release_test_count += 1
                        latest_non_release_test = summary
                        if binding_nonce in eligible_binding_nonces:
                            matching_non_release_test_nonce_count += 1
                        latest_event = summary
                        continue
                    matching_count += 1
                    latest_matching = summary
                    config_modified_at_ns = binding_nonce_modified_at_floor.get(binding_nonce)
                    binding_temporal_ok = temporal_binding_ok(
                        summary.get("observed_at_ns"),
                        event_file_modified_at_ns,
                        config_modified_at_ns,
                    )
                    if binding_temporal_ok:
                        matching_temporal_count += 1
                        latest_matching_temporal = summary
                    if binding_nonce in eligible_binding_nonces:
                        matching_nonce_count += 1
                        if binding_temporal_ok:
                            matching_nonce_temporal_count += 1
                        else:
                            temporal_failures.append(
                                {
                                    "binding_nonce": binding_nonce,
                                    "observed_at_ns": summary.get("observed_at_ns"),
                                    "event_file_modified_at_ns": event_file_modified_at_ns,
                                    "installed_config_modified_at_ns": config_modified_at_ns,
                                    "reason": "event observed_at_ns and event file modified_at must be at or after installed config modified_at",
                                }
                            )
    except Exception as exc:
        return {
            "path": event_jsonl,
            "exists": True,
            "error": str(exc),
            "event_count": event_count,
            "matching_private_event_count": matching_count,
            "matching_private_binding_nonce_count": matching_nonce_count,
            "matching_private_non_release_manual_event_count": matching_manual_debug_count,
            "matching_private_non_release_manual_binding_nonce_count": matching_manual_debug_nonce_count,
            "matching_private_non_release_test_event_count": matching_non_release_test_count,
            "matching_private_non_release_test_binding_nonce_count": matching_non_release_test_nonce_count,
            "matching_private_temporal_binding_count": matching_temporal_count,
            "matching_private_binding_nonce_temporal_count": matching_nonce_temporal_count,
            "temporal_binding_failures": temporal_failures[-5:],
            "latest_matching_event": latest_matching,
            "latest_manual_debug_event": latest_manual_debug,
            "latest_non_release_test_event": latest_non_release_test,
            "latest_matching_temporal_event": latest_matching_temporal,
            "latest_event": latest_event,
        }
    return {
        "path": event_jsonl,
        "exists": True,
        "event_file_modified_at_ns": event_file_modified_at_ns,
        "event_count": event_count,
        "matching_private_event_count": matching_count,
        "matching_private_binding_nonce_count": matching_nonce_count,
        "matching_private_non_release_manual_event_count": matching_manual_debug_count,
        "matching_private_non_release_manual_binding_nonce_count": matching_manual_debug_nonce_count,
        "matching_private_non_release_test_event_count": matching_non_release_test_count,
        "matching_private_non_release_test_binding_nonce_count": matching_non_release_test_nonce_count,
        "matching_private_temporal_binding_count": matching_temporal_count,
        "matching_private_binding_nonce_temporal_count": matching_nonce_temporal_count,
        "temporal_binding_failures": temporal_failures[-5:],
        "latest_matching_event": latest_matching,
        "latest_manual_debug_event": latest_manual_debug,
        "latest_non_release_test_event": latest_non_release_test,
        "latest_matching_temporal_event": latest_matching_temporal,
        "latest_event": latest_event,
    }


def bounded_int_env(name, default, minimum, maximum):
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        value = int(raw)
    except Exception:
        return default
    return max(minimum, min(maximum, value))


def summarize_spool_with_optional_wait(
    eligible_binding_nonces, binding_nonce_modified_at_floor, release_gate_passed
):
    wait_seconds = bounded_int_env("SOMA_CLIENT_BINDING_WAIT_SECONDS", 0, 0, 600)
    interval_ms = bounded_int_env("SOMA_CLIENT_BINDING_WAIT_INTERVAL_MS", 500, 100, 5000)
    started = time.time_ns()
    condition = "matching_private_binding_nonce_temporal_binding_seen"
    if wait_seconds <= 0 or release_gate_passed:
        spool_snapshot = summarize_spool(eligible_binding_nonces, binding_nonce_modified_at_floor)
        status = "skipped_release_gate_passed" if release_gate_passed and wait_seconds > 0 else "not_requested"
        return spool_snapshot, {
            "requested": wait_seconds > 0,
            "status": status,
            "condition": condition,
            "wait_seconds": wait_seconds,
            "interval_ms": interval_ms,
            "elapsed_ms": 0,
            "trust_boundary": (
                "client_hook_wait_is_read_only: polls adapter spool metadata only; "
                "records no proof row, creates no verification event, promotes no "
                "cloud draft, and cannot substitute for real observed_app_hook proof"
            ),
        }

    deadline = time.monotonic() + wait_seconds
    spool_snapshot = summarize_spool(eligible_binding_nonces, binding_nonce_modified_at_floor)
    while spool_snapshot.get("matching_private_binding_nonce_temporal_count", 0) <= 0:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(interval_ms / 1000.0, remaining))
        spool_snapshot = summarize_spool(eligible_binding_nonces, binding_nonce_modified_at_floor)

    elapsed_ms = max(0, int((time.time_ns() - started) / 1_000_000))
    status = (
        "satisfied"
        if spool_snapshot.get("matching_private_binding_nonce_temporal_count", 0) > 0
        else "timeout"
    )
    return spool_snapshot, {
        "requested": True,
        "status": status,
        "condition": condition,
        "wait_seconds": wait_seconds,
        "interval_ms": interval_ms,
        "elapsed_ms": elapsed_ms,
        "trust_boundary": (
            "client_hook_wait_is_read_only: polls adapter spool metadata only; "
            "records no proof row, creates no verification event, promotes no "
            "cloud draft, and cannot substitute for real observed_app_hook proof"
        ),
    }


discovery_raw = discover_installed_config()
session_raw = proof_session()
discovery = compact_discovery(discovery_raw)
eligible_candidates = [
    candidate
    for candidate in discovery.get("candidates", [])
    if candidate.get("eligible_for_observed_app_hook") is True
]
eligible_setup_artifact_candidates = [
    candidate
    for candidate in eligible_candidates
    if not is_private_client_target_path(candidate.get("path") or "")
]
eligible_private_client_target_candidates = [
    candidate
    for candidate in eligible_candidates
    if is_private_client_target_path(candidate.get("path") or "")
]
temporal_reference_candidates = (
    eligible_private_client_target_candidates
    if eligible_private_client_target_candidates
    else eligible_candidates
)
eligible_binding_nonces = {
    candidate.get("binding_nonce")
    for candidate in eligible_candidates
    if candidate.get("binding_nonce")
}
binding_nonce_modified_at_floor = candidate_modified_at_floor_by_nonce(
    temporal_reference_candidates
)
logs = summarize_logs()
session = compact_proof_session(session_raw)
completed_proof_levels = set(session.get("completed_proof_levels") or [])
release_gate_passed = session.get("release_gate") == "pass"
codex_reload = codex_notify_reload_check()
continue_config = continue_extension_config_check()
spool, wait_observation = summarize_spool_with_optional_wait(
    eligible_binding_nonces, binding_nonce_modified_at_floor, release_gate_passed
)

installed_config_ready = len(eligible_candidates) > 0
matching_private_event_seen = spool["matching_private_event_count"] > 0
matching_binding_nonce_seen = spool["matching_private_binding_nonce_count"] > 0
app_hook_temporal_binding_seen = spool.get("matching_private_binding_nonce_temporal_count", 0) > 0
cursor_project_hook_loaded = (
    logs.get("project_config_seen") is True
    and logs.get("loaded_project_hook_count", 0) >= 1
)
cursor_real_hook_requested = (
    logs.get("event_source_seen") is True
    or logs.get("configured_hook_step_requested_count", 0) > 0
    or len(logs.get("configured_found_steps") or []) > 0
    or len(logs.get("configured_merged_steps") or []) > 0
)
cursor_log_gate_passed = client != "cursor" or (
    cursor_project_hook_loaded and cursor_real_hook_requested
)
ready_to_record_app_hook = (
    installed_config_ready
    and matching_private_event_seen
    and matching_binding_nonce_seen
    and app_hook_temporal_binding_seen
    and cursor_log_gate_passed
    and "observed_app_hook" not in completed_proof_levels
    and not release_gate_passed
)

if release_gate_passed:
    next_action = "client_binding_release_gate_passed"
elif not installed_config_ready:
    next_action = f"write_or_install_{client}_installed_config"
elif not matching_private_event_seen:
    if codex_reload and codex_reload.get("restart_recommended"):
        next_action = "restart_or_reopen_codex_app_before_real_hook"
    elif continue_config and continue_config.get("status") in {
        "config_missing",
        "config_present_soma_mcp_missing",
        "config_present_soma_mcp_profile_invalid",
        "config_profile_invalid",
        "config_unreadable",
    }:
        next_action = "merge_continue_mcp_config_before_real_hook"
    elif continue_config and not continue_config.get("extension_observed"):
        next_action = "install_or_enable_continue_extension_before_real_hook"
    else:
        next_action = f"trigger_real_{client}_client_hook_to_write_private_spool_event"
elif not matching_binding_nonce_seen:
    next_action = "align_spool_event_binding_nonce_with_installed_config"
elif not app_hook_temporal_binding_seen:
    next_action = f"trigger_fresh_real_{client}_client_hook_after_current_installed_config"
elif client == "cursor" and not cursor_project_hook_loaded:
    next_action = "open_soma_workspace_in_cursor_and_check_hooks_settings"
elif client == "cursor" and not cursor_real_hook_requested:
    next_action = "start_a_real_cursor_agent_session_to_trigger_sessionStart_or_afterAgentResponse"
elif ready_to_record_app_hook:
    next_action = "record_observed_app_hook_from_real_event_after_operator_confirmation"
elif session.get("release_gate") != "pass":
    next_action = session.get("next_step_id") or "record_observed_app_hook_from_real_event"
else:
    next_action = "client_binding_release_gate_passed"

eligible_config_paths = [candidate.get("path") for candidate in eligible_candidates if candidate.get("path")]
candidate_paths = [
    candidate.get("path")
    for candidate in discovery.get("candidates", [])
    if candidate.get("path")
]
eligible_setup_artifact_paths = [
    candidate.get("path")
    for candidate in eligible_setup_artifact_candidates
    if candidate.get("path")
]
eligible_private_client_target_paths = [
    candidate.get("path")
    for candidate in eligible_private_client_target_candidates
    if candidate.get("path")
]
private_client_target_candidate_paths = [
    path for path in candidate_paths if is_private_client_target_path(path)
]
installation_warnings = []
if installed_config_ready and not eligible_private_client_target_paths:
    installation_warnings.append("private_client_target_config_not_discovered")
sorted_binding_nonces = sorted(eligible_binding_nonces)
primary_binding_nonce = sorted_binding_nonces[0] if len(sorted_binding_nonces) == 1 else None
required_event_contract = {
    "schema": "soma.adapter_spool_event.v1",
    "writer_contract": "soma_adapter_spool_append_v1",
    "client": client,
    "event_source": expected_event_source,
    "binding_nonces": sorted_binding_nonces,
    "observed_at_ns": "required_integer",
    "source_boundary": "real_private_client_hook_only",
}
latest_spool_observation = spool.get("latest_event")
relevant_spool_observation = (
    spool.get("latest_matching_event")
    or spool.get("latest_manual_debug_event")
    or spool.get("latest_non_release_test_event")
    or latest_spool_observation
)
latest_spool_mismatches = []
if isinstance(relevant_spool_observation, dict):
    if relevant_spool_observation.get("schema") != required_event_contract["schema"]:
        latest_spool_mismatches.append("schema")
    if relevant_spool_observation.get("writer_contract") != required_event_contract["writer_contract"]:
        latest_spool_mismatches.append("writer_contract")
    if str(relevant_spool_observation.get("client")).lower() != client.lower():
        latest_spool_mismatches.append("client")
    if relevant_spool_observation.get("event_source") != expected_event_source:
        latest_spool_mismatches.append("event_source")
    if relevant_spool_observation.get("binding_nonce") not in sorted_binding_nonces:
        latest_spool_mismatches.append("binding_nonce")
    if not isinstance(relevant_spool_observation.get("observed_at_ns"), int):
        latest_spool_mismatches.append("observed_at_ns")
    if non_release_manual_event(relevant_spool_observation):
        latest_spool_mismatches.append("manual_debug_non_release_hook_adapter")
    if non_release_test_event(relevant_spool_observation):
        latest_spool_mismatches.append("dogfood_or_synthetic_test_event")
elif spool.get("exists"):
    latest_spool_mismatches.append("latest_event_unreadable")
else:
    latest_spool_mismatches.append("event_jsonl_missing")
base_command_env = [
    "env",
    f"SOMA_CLIENT_BINDING_CLIENT={client}",
    f"SOMA_CLIENT_BINDING_MANIFEST={manifest}",
    f"SOMA_CLIENT_BINDING_CONFIG_ROOT={config_root}",
    f"SOMA_CLIENT_BINDING_PROJECT_ROOT={project_root}",
    f"SOMA_CLIENT_BINDING_EVENT_JSONL={event_jsonl}",
    f"SOMA_CLIENT_BINDING_EVENT_SOURCE={expected_event_source}",
]
if primary_binding_nonce:
    base_command_env.append(f"SOMA_CLIENT_BINDING_NONCE={primary_binding_nonce}")
if log_root:
    base_command_env.append(f"SOMA_CLIENT_BINDING_LOG_ROOT={log_root}")
record_command = [
    *base_command_env,
    "SOMA_CONFIRM_REAL_CLIENT_HOOK=1",
    "SOMA_CONFIRM_RELEASE_GRADE_EVIDENCE=1",
    "tools/soma-client-record-app-hook-proof.sh",
]
readiness_command = [
    *base_command_env,
    "tools/soma-client-hook-readiness.sh",
]
wait_command = [
    *base_command_env,
    "SOMA_CLIENT_BINDING_WAIT_SECONDS=30",
    "tools/soma-client-hook-readiness.sh",
]


def lifecycle_integration_template():
    lifecycle = manifest_json.get("lifecycle") if isinstance(manifest_json, dict) else {}
    if not isinstance(lifecycle, dict):
        lifecycle = {}
    lifecycle_env = lifecycle.get("environment") if isinstance(lifecycle.get("environment"), dict) else {}
    lifecycle_event = (
        lifecycle_env.get("SOMA_ADAPTER_LIFECYCLE_EVENT")
        or (lifecycle.get("sample_event") or {}).get("event")
        or "assistant_response"
    )
    lifecycle_wrapper = lifecycle.get("wrapper") or (
        "tools/soma-codex-app-capture.sh"
        if client == "codex-app"
        else "tools/soma-adapter-lifecycle.sh"
    )
    binding_nonce_value = primary_binding_nonce or "<installed-binding-nonce>"
    lifecycle_env_template = {
        "SOMA_ADAPTER_LIFECYCLE_CLIENT": client,
        "SOMA_ADAPTER_LIFECYCLE_EVENT": lifecycle_event,
        "SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE": expected_event_source,
        "SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE": binding_nonce_value,
        "SOMA_ADAPTER_LIFECYCLE_JSONL": event_jsonl,
    }
    sample_event = lifecycle.get("sample_event")
    if not isinstance(sample_event, dict):
        sample_event = {}
    else:
        sample_event = dict(sample_event)
    sample_event.setdefault("event", lifecycle_event)
    sample_event.setdefault("client", client)
    sample_event.setdefault("project", pathlib.Path(project_root).name or "soma-client-binding")
    sample_event.setdefault("session_id", f"{client}-private-hook-session")
    sample_event.setdefault("cwd", project_root)
    sample_event.setdefault("hook_adapter", "manual_debug_non_release_template")
    sample_event.setdefault("manual_invocation_policy", "non_release_debug_only")
    if lifecycle_event == "assistant_response":
        sample_event.setdefault("prompt_text", "Private client prompt text seen locally.")
        sample_event.setdefault(
            "output_text",
            "Private client assistant output remains draft evidence until trusted verification.",
        )
        sample_event.setdefault("enqueue_proposal", True)
        sample_event.setdefault(
            "proposal_reason",
            "Private client integration should expose cloud output only as draft claims.",
        )
    else:
        sample_event.setdefault("prompt_text", "Private client prompt text seen locally.")
        sample_event.setdefault(
            "response_text",
            "Private client turn reaches SOMA through the lifecycle wrapper.",
        )

    wrapper_command_template = [
        "env",
        *[f"{key}={value}" for key, value in lifecycle_env_template.items()],
        lifecycle_wrapper,
    ]
    return {
        "schema": "soma.private_client_hook_integration_template.v1",
        "client": client,
        "read_only": True,
        "records_proof": False,
        "creates_verification_event": False,
        "promotes_cloud_draft": False,
        "manual_invocation_policy": "non_release_debug_only",
        "wrapper": lifecycle_wrapper,
        "wrapper_command_template": wrapper_command_template,
        "environment": lifecycle_env_template,
        "stdin_event_template": sample_event,
        "expected_spool_contract": required_event_contract,
        "eligible_installed_config_paths": eligible_config_paths,
        "eligible_binding_nonces": sorted_binding_nonces,
        "operator_next_step": (
            "Wire this command into the private client's native lifecycle/hook path, "
            "reload that client, perform a real client action, then rerun readiness_command."
        ),
        "trust_boundary": (
            "private_client_hook_integration_template_is_guidance_only: it renders the "
            "wrapper/env/stdin contract a private client should call; it records no proof "
            "row, creates no verification event, promotes no cloud draft, and a manual "
            "terminal invocation is non-release debug evidence unless the private client "
            "actually invoked the hook and the operator later confirms release-grade evidence"
        ),
    }


private_client_hook_integration_template = lifecycle_integration_template()
if release_gate_passed:
    operator_title = "Client binding release gate passed"
    operator_instruction = (
        "Release-grade app-hook, in-client render, and review-action proof rows "
        "already replay cleanly; no app-hook proof recording command is needed."
    )
    blocking_reasons = []
elif not installed_config_ready:
    operator_title = "Install proof-free private client binding config"
    operator_instruction = (
        f"Render or install the {client} binding config, then rerun this readiness probe."
    )
    blocking_reasons = ["eligible_installed_config_missing"]
elif not matching_private_event_seen:
    operator_title = "Trigger the real private client hook"
    operator_instruction = (
        f"Open {client} and run a real action that should call SOMA; wait for "
        f"{event_jsonl} to contain event_source={expected_event_source}."
    )
    blocking_reasons = ["matching_private_event_missing"]
    if codex_reload and codex_reload.get("restart_recommended"):
        operator_title = "Quit/reopen Codex app"
        operator_instruction = (
            "Quit or restart the stale Codex app process so it reloads the "
            "patched notify config, reopen it, then complete a real turn and "
            f"wait for {event_jsonl} to contain event_source={expected_event_source}. "
            "`open -a Codex` alone is only a reopen hint and does not force a "
            "running app to reload the notify config."
        )
        blocking_reasons.append("codex_notify_restart_recommended")
    if continue_config and continue_config.get("status") in {
        "config_missing",
        "config_present_soma_mcp_missing",
        "config_present_soma_mcp_profile_invalid",
        "config_profile_invalid",
        "config_unreadable",
    }:
        if continue_config.get("status") in {
            "config_present_soma_mcp_profile_invalid",
            "config_profile_invalid",
        }:
            operator_instruction = (
                "Continue config.yaml/config.yml is present but rejected by Continue "
                "because required top-level name/version fields are missing or unreadable; "
                "repair the profile config, reload Continue, then "
                f"complete a real turn and wait for {event_jsonl} to contain "
                f"event_source={expected_event_source}."
            )
        else:
            operator_instruction = (
                "Continue extension config is not visibly wired to SOMA; write the "
                "generated MCP server JSON to the Continue mcpServers directory, reload Continue, then "
                f"complete a real turn and wait for {event_jsonl} to contain "
                f"event_source={expected_event_source}."
            )
        blocking_reasons.append("continue_extension_config_not_visible")
    elif continue_config and not continue_config.get("extension_observed"):
        operator_title = "Install or enable Continue extension"
        operator_instruction = (
            "Continue MCP config is visible, but no local Continue extension "
            "installation was observed in VS Code/Cursor extension paths; "
            "install or enable Continue, reload the editor, then complete a real "
            f"turn and wait for {event_jsonl} to contain "
            f"event_source={expected_event_source}."
        )
        blocking_reasons.append("continue_extension_installation_not_observed")
elif not matching_binding_nonce_seen:
    operator_title = "Trigger hook with the installed binding nonce"
    operator_instruction = (
        "A private event exists, but it does not carry one of the eligible "
        "installed-config binding nonces; reinstall the binding config or trigger "
        "the client again after confirming the nonce."
    )
    blocking_reasons = ["matching_binding_nonce_missing"]
elif not app_hook_temporal_binding_seen:
    operator_title = "Trigger a fresh private client hook"
    operator_instruction = (
        "A matching private event exists, but it is older than the current "
        "installed config; trigger the real client again so the event "
        "observed_at_ns and event file modified_at are at or after the installed "
        "config modified_at before recording observed_app_hook."
    )
    blocking_reasons = ["private_hook_temporal_binding_failed"]
elif not cursor_log_gate_passed:
    operator_title = "Confirm Cursor loaded and requested the hook"
    operator_instruction = (
        "Cursor-specific logs have not yet shown the project hook load/request; "
        "open the SOMA workspace in Cursor and start a real agent session."
    )
    blocking_reasons = ["cursor_log_gate_missing"]
elif ready_to_record_app_hook:
    operator_title = "Record observed_app_hook proof"
    operator_instruction = (
        "Matching real-client event evidence is present; run the recorder only "
        "after explicit real-client and release-grade operator confirmation."
    )
    blocking_reasons = []
else:
    operator_title = "Continue proof-session runbook"
    operator_instruction = (
        f"Continue the proof-session at {next_action}; this readiness probe remains read-only."
    )
    blocking_reasons = session.get("blocked_proof_levels") or []


def client_display_name(client_name):
    return {
        "codex-app": "Codex app",
        "cursor": "Cursor",
        "continue": "Continue",
    }.get(client_name, client_name)


def summarize_operator_action():
    visibility = {
        "setup": "ready" if len(eligible_setup_artifact_paths) > 0 else "missing",
        "target": "present" if len(eligible_private_client_target_paths) > 0 else "not_discovered",
    }
    lines = [
        f"status={operator_title}; next_action={next_action}",
        (
            "installed_config="
            f"{'ready' if installed_config_ready else 'missing'}; "
            f"setup_artifact={visibility['setup']}; private_target_config={visibility['target']}"
        ),
        f"expected_event_source={expected_event_source}; event_jsonl={event_jsonl}",
    ]
    if isinstance(relevant_spool_observation, dict):
        mismatch_text = ",".join(latest_spool_mismatches) if latest_spool_mismatches else "none"
        lines.append(
            "relevant_spool_event="
            f"client={relevant_spool_observation.get('client')} "
            f"event_source={relevant_spool_observation.get('event_source')} "
            f"binding_nonce={relevant_spool_observation.get('binding_nonce')} "
            f"observed_at_ns={relevant_spool_observation.get('observed_at_ns')} "
            f"mismatches={mismatch_text}"
        )
    else:
        mismatch_text = ",".join(latest_spool_mismatches) if latest_spool_mismatches else "none"
        lines.append(f"relevant_spool_event=none; mismatches={mismatch_text}")
    if codex_reload:
        lines.append(
            "codex_notify_reload="
            f"status={codex_reload.get('status')} "
            f"restart_recommended={str(codex_reload.get('restart_recommended')).lower()} "
            f"stale_processes={codex_reload.get('stale_codex_desktop_process_count')}"
        )
    if continue_config:
        lines.append(
            "continue_extension_config="
            f"status={continue_config.get('status')} "
            f"has_mcpServers={str(continue_config.get('has_mcp_servers')).lower()} "
            f"has_modelContextProtocol={str(continue_config.get('has_model_context_protocol')).lower()} "
            f"has_soma_server={str(continue_config.get('has_soma_server')).lower()} "
            f"extension_status={continue_config.get('extension_installation_status')} "
            f"extension_observed={str(continue_config.get('extension_observed')).lower()} "
            f"recommended_config={continue_config.get('recommended_config_path')}"
        )
    if client == "cursor":
        def join_or_none(values):
            values = [str(value) for value in (values or []) if str(value)]
            return ",".join(values) if values else "none"

        lines.append(
            "cursor_hooks="
            f"project_config_seen={str(logs.get('project_config_seen')).lower()} "
            f"loaded_steps={join_or_none(logs.get('loaded_steps'))} "
            f"requested_steps={join_or_none(logs.get('hook_steps_requested'))} "
            f"configured_requested_steps={join_or_none(logs.get('configured_hook_steps_requested'))} "
            f"unconfigured_requested_steps={join_or_none(logs.get('unconfigured_hook_steps_requested'))}"
        )
    if ready_to_record_app_hook:
        lines.append(
            "next=matching real-client event evidence is present; run record_command only after explicit real-client and release-grade confirmation"
        )
    elif release_gate_passed:
        lines.append("next=release-grade app-hook/render/review-action proof is already complete")
    elif not matching_private_event_seen:
        if codex_reload and codex_reload.get("restart_recommended"):
            lines.append(
                "next=quit or restart the stale Codex app process, reopen it, complete a real turn, then rerun readiness_command"
            )
        elif continue_config and continue_config.get("status") in {
            "config_missing",
            "config_present_soma_mcp_missing",
            "config_present_soma_mcp_profile_invalid",
            "config_profile_invalid",
            "config_unreadable",
        }:
            if continue_config.get("status") in {
                "config_present_soma_mcp_profile_invalid",
                "config_profile_invalid",
            }:
                lines.append(
                    "next=repair Continue config.yaml/config.yml top-level name/version fields, reload Continue, complete a real turn, then rerun readiness_command"
                )
            else:
                lines.append(
                    "next=write Continue MCP server JSON for SOMA, reload Continue, complete a real turn, then rerun readiness_command"
                )
        elif continue_config and not continue_config.get("extension_observed"):
            lines.append(
                "next=install or enable the Continue extension in VS Code/Cursor, reload the editor, complete a real turn, then rerun readiness_command"
            )
        else:
            lines.append(
                f"next=open {client_display_name(client)} and run a real action that should call SOMA, then rerun readiness_command"
            )
    elif not matching_binding_nonce_seen:
        lines.append(
            "next=trigger the client again with the installed binding nonce or reinstall the binding config"
        )
    elif not app_hook_temporal_binding_seen:
        if client == "cursor" and not cursor_log_gate_passed:
            lines.append(
                "next=matching event is stale and Cursor logs have not shown a request for a configured hook step; start a real Cursor agent session that triggers sessionStart or afterAgentResponse, then rerun readiness_command"
            )
        else:
            lines.append(
                "next=matching event is stale relative to installed config; trigger a fresh real client hook, then rerun readiness_command"
            )
    elif client == "cursor" and not cursor_log_gate_passed:
        lines.append(
            "next=Cursor project hook is loaded, but logs have not shown a request for a configured hook step; start a real Cursor agent session that triggers sessionStart or afterAgentResponse, then rerun readiness_command"
        )
    else:
        lines.append("next=continue the proof-session runbook; this probe remains read-only")
    return lines


summary_lines = summarize_operator_action()
operator_status = (
    "complete"
    if release_gate_passed
    else ("ready_to_record" if ready_to_record_app_hook else "blocked")
)
effective_record_command = record_command if ready_to_record_app_hook else None

report = {
    "schema": "soma.client_hook_readiness_probe.v1",
    "client": client,
    "project_root": project_root,
    "config_root": config_root,
    "manifest_path": manifest,
    "checked_at_ns": time.time_ns(),
    "read_only": True,
    "expected_event_source": expected_event_source,
    "manifest": {
        "loaded": "error" not in manifest_json,
        "client": manifest_json.get("client"),
        "error": manifest_json.get("error"),
    },
    "status": operator_status,
    "next_action": next_action,
    "blocking_reasons": blocking_reasons,
    "ready_to_record_observed_app_hook": ready_to_record_app_hook,
    "readiness_command": readiness_command,
    "wait_command": wait_command,
    "record_command": effective_record_command,
    "installed_config": discovery,
    "client_logs": logs,
    "adapter_spool": spool,
    "wait_observation": wait_observation,
    "proof_session": session,
    "codex_notify_reload_check": codex_reload,
    "continue_extension_config_check": continue_config,
    "private_client_hook_integration_template": private_client_hook_integration_template,
    "derived": {
        "installed_config_ready": installed_config_ready,
        "matching_private_event_seen": matching_private_event_seen,
        "matching_private_binding_nonce_seen": matching_binding_nonce_seen,
        "app_hook_temporal_binding_seen": app_hook_temporal_binding_seen,
        "matching_private_binding_nonce_temporal_count": spool.get(
            "matching_private_binding_nonce_temporal_count", 0
        ),
        "cursor_project_hook_loaded": cursor_project_hook_loaded,
        "cursor_real_hook_requested": cursor_real_hook_requested,
        "cursor_log_gate_passed": cursor_log_gate_passed,
        "observed_app_hook_already_recorded": "observed_app_hook" in completed_proof_levels,
        "release_gate_passed": release_gate_passed,
        "operator_confirmation_required": not release_gate_passed,
        "release_grade_confirmation_required": not release_gate_passed,
        "codex_notify_restart_recommended": bool(
            codex_reload and codex_reload.get("restart_recommended")
        ),
        "continue_extension_config_visible": bool(
            continue_config
            and continue_config.get("status") == "config_present_soma_mcp_seen"
        ),
        "ready_to_record_observed_app_hook": ready_to_record_app_hook,
        "next_action": next_action,
    },
    "operator_action_card": {
        "schema": "soma.client_hook_operator_action_card.v1",
        "read_only": True,
        "client": client,
        "title": operator_title,
        "status": operator_status,
        "next_action": next_action,
        "instruction": operator_instruction,
        "summary_lines": summary_lines,
        "expected_event_source": expected_event_source,
        "event_jsonl_path": event_jsonl,
        "watch_command": ["tail", "-f", event_jsonl],
        "wait_command": wait_command,
        "required_event_contract": required_event_contract,
        "private_client_hook_integration_template": private_client_hook_integration_template,
        "latest_spool_observation": latest_spool_observation,
        "relevant_spool_observation": relevant_spool_observation,
        "latest_spool_mismatches": latest_spool_mismatches,
        "codex_notify_reload_check": codex_reload,
        "continue_extension_config_check": continue_config,
        "installation_visibility": {
            "setup_artifact_ready": len(eligible_setup_artifact_paths) > 0,
            "private_client_target_config_present": len(eligible_private_client_target_paths) > 0,
            "known_private_client_target_relpaths": private_client_target_relpaths,
            "eligible_setup_artifact_paths": eligible_setup_artifact_paths,
            "eligible_private_client_target_paths": eligible_private_client_target_paths,
            "private_client_target_candidate_paths": private_client_target_candidate_paths,
            "warnings": installation_warnings,
            "trust_boundary": (
                "installation_visibility_is_diagnostic_only: setup artifacts and "
                "known target config files are path visibility only; target config "
                "presence does not prove the private client invoked the hook; only "
                "matching real-client event evidence plus explicit operator "
                "confirmation can record observed_app_hook"
            ),
        },
        "eligible_installed_config_paths": eligible_config_paths,
        "eligible_binding_nonces": sorted_binding_nonces,
        "blocking_reasons": blocking_reasons,
        "readiness_command": readiness_command,
        "record_command": effective_record_command,
        "requires_operator_confirmation": not release_gate_passed,
        "requires_release_grade_confirmation": not release_gate_passed,
        "trust_boundary": (
            "operator_action_card_is_guidance_only: it records no proof row and "
            "does not prove private client behavior until the separate recorder "
            "runs with matching event evidence and explicit confirmations; "
            "manual wrapper invocations are debug observations and cannot "
            "substitute for the real private client hook"
        ),
    },
    "trust_boundary": (
        "client_hook_readiness_probe_is_read_only: reads installed-config "
        "discovery, optional client logs, adapter spool metadata, and "
        "proof-session status only; records no proof row, creates no "
        "verification event, promotes no cloud draft, applies no proposal, "
        "and does not prove private client behavior beyond cited local "
        "artifacts plus later explicit operator confirmation"
    ),
}

print(json.dumps(report, indent=2, sort_keys=True))
PY
