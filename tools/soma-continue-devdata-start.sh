#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COLLECTOR="$ROOT/tools/soma-continue-devdata-collector.py"

ACTION="${1:-start}"
if [[ $# -gt 0 ]]; then
  shift
fi

HOST="${SOMA_CONTINUE_DEVDATA_HOST:-127.0.0.1}"
PORT="${SOMA_CONTINUE_DEVDATA_PORT:-8766}"
SOMA_BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
if [[ ! -x "$SOMA_BIN" ]]; then
  SOMA_BIN="soma"
fi
JSONL="${SOMA_ADAPTER_EVENT_JSONL:-$HOME/.soma/adapter/events.jsonl}"
BINDING_CONFIG="${SOMA_CONTINUE_BINDING_CONFIG:-$HOME/.continue/soma-installed-binding.json}"
PID_FILE="${SOMA_CONTINUE_DEVDATA_PID_FILE:-$HOME/.soma/run/continue-devdata-collector.pid}"
LOG_FILE="${SOMA_CONTINUE_DEVDATA_LOG_FILE:-$HOME/.soma/logs/continue-devdata-collector.log}"
EXTRA_ARGS=()

json_string() {
  python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"
}

emit_json() {
  local status="$1"
  local pid="${2:-}"
  local listening="${3:-false}"
  local message="${4:-}"
  printf '{"schema":"soma.continue_devdata_collector_launcher.v1","status":%s,"pid":%s,"host":%s,"port":%s,"listening":%s,"pid_file":%s,"log_file":%s,"message":%s}\n' \
    "$(json_string "$status")" \
    "${pid:-null}" \
    "$(json_string "$HOST")" \
    "$PORT" \
    "$listening" \
    "$(json_string "$PID_FILE")" \
    "$(json_string "$LOG_FILE")" \
    "$(json_string "$message")"
}

usage() {
  cat <<EOF
Usage: tools/soma-continue-devdata-start.sh [start|status|stop] [OPTIONS]

Starts or manages the local Continue dev-data collector without recording any
proof row. The collector must still receive a real Continue dev-data POST before
observed_app_hook proof can be recorded.

Options:
  --host HOST
  --port PORT
  --soma-bin PATH
  --jsonl PATH
  --binding-config PATH
  --pid-file PATH
  --log-file PATH
  --project NAME
  --cwd PATH
  --session-id ID
  --fsync
  --quiet
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --host)
      HOST="$2"
      shift 2
      ;;
    --port)
      PORT="$2"
      shift 2
      ;;
    --soma-bin)
      SOMA_BIN="$2"
      shift 2
      ;;
    --jsonl)
      JSONL="$2"
      shift 2
      ;;
    --binding-config)
      BINDING_CONFIG="$2"
      shift 2
      ;;
    --pid-file)
      PID_FILE="$2"
      shift 2
      ;;
    --log-file)
      LOG_FILE="$2"
      shift 2
      ;;
    --project|--cwd|--session-id)
      EXTRA_ARGS+=("$1" "$2")
      shift 2
      ;;
    --fsync|--quiet)
      EXTRA_ARGS+=("$1")
      shift
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

pid_from_file() {
  if [[ -f "$PID_FILE" ]]; then
    tr -d '[:space:]' <"$PID_FILE"
  fi
}

pid_running() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

tcp_listening() {
  python3 - "$HOST" "$PORT" <<'PY' >/dev/null 2>&1
import socket
import sys

host = sys.argv[1]
port = int(sys.argv[2])
with socket.create_connection((host, port), timeout=0.25):
    pass
PY
}

wait_for_tcp() {
  local i
  for i in {1..50}; do
    if tcp_listening; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

case "$ACTION" in
  start)
    mkdir -p "$(dirname "$PID_FILE")" "$(dirname "$LOG_FILE")" "$(dirname "$JSONL")"
    existing_pid="$(pid_from_file || true)"
    if pid_running "$existing_pid"; then
      if tcp_listening; then
        emit_json "already_running" "$existing_pid" "true" "collector is already listening"
      else
        emit_json "already_running" "$existing_pid" "false" "process exists but TCP probe is not listening yet"
      fi
      exit 0
    fi
    if tcp_listening; then
      emit_json "already_listening" "" "true" "collector TCP endpoint is already listening without a managed pid file"
      exit 0
    fi
    spawn_command=(
      "$COLLECTOR"
      --host "$HOST"
      --port "$PORT"
      --soma-bin "$SOMA_BIN"
      --jsonl "$JSONL"
      --binding-config "$BINDING_CONFIG"
    )
    if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
      spawn_command+=("${EXTRA_ARGS[@]}")
    fi
    pid="$(python3 - "$PID_FILE" "$LOG_FILE" "${spawn_command[@]}" <<'PY'
import pathlib
import subprocess
import sys

pid_file = pathlib.Path(sys.argv[1])
log_file = pathlib.Path(sys.argv[2])
command = sys.argv[3:]
with log_file.open("ab", buffering=0) as log:
    proc = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        close_fds=True,
        start_new_session=True,
    )
pid_file.write_text(f"{proc.pid}\n", encoding="utf-8")
print(proc.pid)
PY
)"
    if wait_for_tcp; then
      emit_json "started" "$pid" "true" "collector is listening"
    else
      emit_json "starting" "$pid" "false" "collector process started but TCP probe did not become ready within 5s"
    fi
    ;;
  status)
    pid="$(pid_from_file || true)"
    if pid_running "$pid"; then
      if tcp_listening; then
        emit_json "running" "$pid" "true" "collector process and TCP endpoint are ready"
      else
        emit_json "running" "$pid" "false" "collector process exists but TCP endpoint is not accepting connections"
      fi
    elif tcp_listening; then
      emit_json "orphaned_listening" "" "true" "collector TCP endpoint is listening, but no managed pid file is present"
    else
      emit_json "stopped" "" "false" "collector process is not running"
    fi
    ;;
  stop)
    pid="$(pid_from_file || true)"
    if pid_running "$pid"; then
      kill "$pid"
      rm -f "$PID_FILE"
      emit_json "stopped" "$pid" "false" "collector process was signaled to stop"
    else
      rm -f "$PID_FILE"
      emit_json "stopped" "" "false" "collector process was not running"
    fi
    ;;
  -h|--help)
    usage
    ;;
  *)
    echo "unknown action: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
