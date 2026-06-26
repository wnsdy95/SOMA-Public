#!/usr/bin/env bash
# soma-codex-notify-install - install the Codex app notify bridge safely.
#
# This script rewrites the top-level Codex `notify` array so the SOMA notify
# bridge runs first and chains the previous notify command. It strips stale
# nested previous-notify bridge payloads so the bridge is not called twice. It is
# idempotent and writes a timestamped backup before touching the config unless
# --dry-run is set.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${SOMA_CODEX_NOTIFY_CONFIG:-$HOME/.codex/config.toml}"
BRIDGE="${SOMA_CODEX_NOTIFY_BRIDGE:-$ROOT/tools/soma-codex-notify-bridge.sh}"
DRY_RUN=0

usage() {
    cat <<USAGE
usage: $0 [--config PATH] [--bridge PATH] [--dry-run]

Installs tools/soma-codex-notify-bridge.sh into Codex app's top-level notify
array while preserving the previous notify command behind --chain and removing
stale nested previous-notify entries that point back to the same bridge.

The JSON report includes reload_check. Set SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK=1
to skip local process inspection, or SOMA_CODEX_NOTIFY_PS_OUTPUT to a fixture
path/literal when testing.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --config)
            CONFIG="${2:?missing --config path}"
            shift 2
            ;;
        --bridge)
            BRIDGE="${2:?missing --bridge path}"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

python3 - "$CONFIG" "$BRIDGE" "$DRY_RUN" <<'PY'
import json
import os
import re
import shutil
import subprocess
import sys
import time
import ast
from datetime import datetime

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None

config_path, bridge_path, dry_run_raw = sys.argv[1:4]
dry_run = dry_run_raw == "1"
schema = "soma.codex_notify_install.v1"
trust_boundary = (
    "codex_notify_install_edits_only_the_local_codex_notify_array; "
    "it records no proof row, creates no verification event, promotes no cloud "
    "draft, and does not prove Codex app invoked the hook"
)

if not os.path.isfile(config_path):
    raise SystemExit(f"Codex config not found: {config_path}")
if not os.path.isfile(bridge_path):
    raise SystemExit(f"SOMA notify bridge not found: {bridge_path}")
if not os.access(bridge_path, os.X_OK):
    raise SystemExit(f"SOMA notify bridge is not executable: {bridge_path}")

with open(config_path, "rb") as f:
    original_bytes = f.read()
original = original_bytes.decode("utf-8")

def fallback_top_level_notify(text):
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            break
        match = re.match(r"^notify\s*=\s*(\[[^\n]*\])\s*(?:#.*)?$", line)
        if match:
            try:
                return ast.literal_eval(match.group(1))
            except Exception as exc:
                raise SystemExit(
                    "python3 lacks tomllib and top-level notify must be a "
                    "simple one-line string array"
                ) from exc
        if re.match(r"^notify\s*=", line):
            raise SystemExit(
                "python3 lacks tomllib and top-level notify must be a simple "
                "one-line string array"
            )
    return None

if tomllib is not None:
    parsed = tomllib.loads(original)
    old_notify = parsed.get("notify")
else:
    old_notify = fallback_top_level_notify(original)
if old_notify is not None and not (
    isinstance(old_notify, list) and all(isinstance(item, str) for item in old_notify)
):
    raise SystemExit("top-level notify must be an array of strings")

old_notify = list(old_notify or [])
def previous_notify_points_to_bridge(raw):
    if bridge_path in raw:
        return True
    try:
        decoded = json.loads(raw)
    except Exception:
        return False
    return bool(
        isinstance(decoded, list)
        and decoded
        and isinstance(decoded[0], str)
        and decoded[0] == bridge_path
    )

def strip_stale_bridge_previous_notify(values):
    stripped = []
    index = 0
    removed = []
    while index < len(values):
        token = values[index]
        if (
            token == "--previous-notify"
            and index + 1 < len(values)
            and previous_notify_points_to_bridge(values[index + 1])
        ):
            removed.extend(values[index : index + 2])
            index += 2
            continue
        stripped.append(token)
        index += 1
    return stripped, removed

chained_notify, removed_stale_previous_notify = strip_stale_bridge_previous_notify(old_notify)
already_installed = bool(old_notify) and old_notify[0] == bridge_path
if already_installed:
    new_notify = old_notify
else:
    new_notify = [bridge_path]
    if chained_notify:
        new_notify.extend(["--chain", *chained_notify])

def toml_array(values):
    return "[" + ", ".join(json.dumps(value) for value in values) + "]"

notify_line = f"notify = {toml_array(new_notify)}"
changed = new_notify != old_notify
backup_path = None
updated = original

notify_re = re.compile(r"(?m)^notify\s*=\s*\[[^\n]*\]\s*$")
if changed:
    if notify_re.search(original):
        updated = notify_re.sub(notify_line, original, count=1)
    else:
        insert_at = 0
        lines = original.splitlines(keepends=True)
        for index, line in enumerate(lines):
            if line.lstrip().startswith("["):
                insert_at = index
                break
        else:
            insert_at = len(lines)
        lines.insert(insert_at, notify_line + "\n")
        updated = "".join(lines)

    if not dry_run:
        stamp = time.strftime("%Y%m%d%H%M%S")
        backup_path = f"{config_path}.bak-soma-notify-{stamp}"
        shutil.copy2(config_path, backup_path)
        tmp_path = f"{config_path}.tmp-soma-notify-{os.getpid()}"
        with open(tmp_path, "w", encoding="utf-8") as f:
            f.write(updated)
        os.replace(tmp_path, config_path)

def process_table_lines():
    fixture = os.environ.get("SOMA_CODEX_NOTIFY_PS_OUTPUT")
    if fixture:
        if os.path.isfile(fixture):
            with open(fixture, "r", encoding="utf-8") as f:
                return {
                    "status": "available",
                    "source": "env_file:SOMA_CODEX_NOTIFY_PS_OUTPUT",
                    "lines": f.read().splitlines(),
                }
        return {
            "status": "available",
            "source": "env_literal:SOMA_CODEX_NOTIFY_PS_OUTPUT",
            "lines": fixture.splitlines(),
        }
    if os.environ.get("SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK") == "1":
        return {
            "status": "skipped",
            "source": "env:SOMA_CODEX_NOTIFY_SKIP_PROCESS_CHECK",
            "lines": [],
        }
    try:
        proc = subprocess.run(
            ["ps", "-axo", "pid,lstart,command"],
            check=True,
            capture_output=True,
            text=True,
        )
    except Exception as exc:
        return {
            "status": "unavailable",
            "source": "ps",
            "error": str(exc),
            "lines": [],
        }
    return {"status": "available", "source": "ps", "lines": proc.stdout.splitlines()}

def codex_reload_check(config_mtime):
    ps_data = process_table_lines()
    trust = (
        "codex_notify_reload_check_is_local_diagnostic_only; it records no "
        "proof row, creates no verification event, promotes no cloud draft, "
        "and cannot substitute for a real Codex app hook event"
    )
    if ps_data["status"] != "available":
        return {
            "source": ps_data["source"],
            "status": ps_data["status"],
            "config_mtime_unix": int(config_mtime),
            "codex_desktop_process_count": 0,
            "stale_codex_desktop_process_count": 0,
            "stale_processes": [],
            "restart_recommended": False,
            "error": ps_data.get("error"),
            "trust_boundary": trust,
        }

    desktop_processes = []
    for raw in ps_data["lines"]:
        stripped = raw.strip()
        if not stripped or stripped.startswith("PID "):
            continue
        parts = stripped.split(None, 6)
        if len(parts) < 7:
            continue
        pid_raw, *started_parts, command = parts
        if "/Applications/Codex.app/Contents/MacOS/Codex" not in command:
            continue
        try:
            started = datetime.strptime(" ".join(started_parts), "%a %b %d %H:%M:%S %Y")
            started_unix = int(started.timestamp())
            pid = int(pid_raw)
        except ValueError:
            continue
        desktop_processes.append({
            "pid": pid,
            "started_at_unix": started_unix,
            "started_before_config": started_unix < config_mtime,
            "command": command,
        })

    stale = [process for process in desktop_processes if process["started_before_config"]]
    if stale:
        status = "restart_recommended"
    elif desktop_processes:
        status = "codex_app_started_after_config"
    else:
        status = "codex_app_not_running"
    return {
        "source": ps_data["source"],
        "status": status,
        "config_mtime_unix": int(config_mtime),
        "codex_desktop_process_count": len(desktop_processes),
        "stale_codex_desktop_process_count": len(stale),
        "stale_processes": stale[:5],
        "restart_recommended": bool(stale),
        "trust_boundary": trust,
    }

reload_check = codex_reload_check(os.path.getmtime(config_path))

report = {
    "schema": schema,
    "config_path": config_path,
    "bridge_path": bridge_path,
    "dry_run": dry_run,
    "changed": changed,
    "already_installed": already_installed,
    "backup_path": backup_path,
    "old_notify": old_notify,
    "chained_notify": chained_notify,
    "removed_stale_previous_notify": removed_stale_previous_notify,
    "new_notify": new_notify,
    "reload_hint": "Quit or restart the stale Codex app process, reopen it, then complete a real turn so the desktop app reloads the notify config and invokes the bridge; `open -a Codex` alone does not force a running app to reload config, and this installer cannot create app-hook proof by itself.",
    "reload_check": reload_check,
    "trust_boundary": trust_boundary,
}
print(json.dumps(report, indent=2, sort_keys=True))
PY
