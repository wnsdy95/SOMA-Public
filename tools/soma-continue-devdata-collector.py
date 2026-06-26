#!/usr/bin/env python3
"""Local Continue dev-data bridge for SOMA adapter lifecycle events.

Continue can POST dev-data events to a local HTTP destination. This collector
normalizes those POST bodies into SOMA's existing `adapter-lifecycle` contract
and appends them to the adapter JSONL spool. It intentionally records no proof
rows and performs no ingestion by itself.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import pathlib
import subprocess
import sys
import time
from typing import Any


CLIENT = "continue"
EVENT_SOURCE = "continue_private_lifecycle_hook"
CONTRACT = "continue_devdata_posts_to_soma_adapter_lifecycle_without_proof"
CONTINUE_DEVDATA_EVENTS = {
    "chatInteraction": {"0.2.0"},
    "editInteraction": {"0.2.0"},
    "editOutcome": {"0.2.0"},
    "quickEdit": {"0.1.0", "0.2.0"},
}
CONTINUE_DEVDATA_LEVELS = {"all", "noCode"}
NON_RELEASE_DOGFOOD_MARKERS = {
    "dogfood",
    "local-dogfood",
    "soma-test",
    "soma_continue_collector_ok",
}


def root_dir() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parents[1]


def default_soma_bin() -> str:
    env_bin = os.environ.get("SOMA_BIN", "").strip()
    if env_bin:
        return env_bin
    candidate = root_dir() / "target" / "debug" / "soma"
    if candidate.exists():
        return str(candidate)
    return "soma"


def default_jsonl() -> str:
    return str(pathlib.Path.home() / ".soma" / "adapter" / "events.jsonl")


def default_binding_config() -> str:
    return str(pathlib.Path.home() / ".continue" / "soma-installed-binding.json")


def load_binding_nonce(path: str) -> str | None:
    env_nonce = os.environ.get("SOMA_CONTINUE_BINDING_NONCE", "").strip()
    if env_nonce:
        return env_nonce
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        return None
    except Exception as exc:
        raise SystemExit(f"failed to read binding config {path}: {exc}") from exc

    nonce = data.get("binding_nonce") or data.get("bindingNonce")
    if isinstance(nonce, str) and nonce.strip():
        return nonce.strip()

    checks = data.get("checks")
    if isinstance(checks, dict):
        nested = checks.get("binding_nonce") or checks.get("bindingNonce")
        if isinstance(nested, str) and nested.strip():
            return nested.strip()

    proof = data.get("proof")
    if isinstance(proof, dict):
        nested = proof.get("binding_nonce") or proof.get("bindingNonce")
        if isinstance(nested, str) and nested.strip():
            return nested.strip()

    lifecycle_hook = data.get("lifecycle_hook")
    if isinstance(lifecycle_hook, dict):
        env = lifecycle_hook.get("env")
        if isinstance(env, dict):
            nested = env.get("SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE")
            if isinstance(nested, str) and nested.strip():
                return nested.strip()

    spool_append = data.get("spool_append")
    if isinstance(spool_append, dict):
        env = spool_append.get("env")
        if isinstance(env, dict):
            nested = env.get("SOMA_ADAPTER_BINDING_NONCE")
            if isinstance(nested, str) and nested.strip():
                return nested.strip()
    return None


def first_string(*values: Any) -> str | None:
    for value in values:
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def contains_non_release_dogfood_marker(*values: Any) -> bool:
    for value in values:
        if not isinstance(value, str):
            continue
        normalized = value.strip().lower()
        if any(marker in normalized for marker in NON_RELEASE_DOGFOOD_MARKERS):
            return True
    return False


def continue_dogfood_or_synthetic_signature(
    body: dict[str, Any],
    data: dict[str, Any],
) -> bool:
    return contains_non_release_dogfood_marker(
        body.get("profileId"),
        body.get("profile_id"),
        body.get("sessionId"),
        body.get("session_id"),
        body.get("prompt"),
        body.get("completion"),
        data.get("profileId"),
        data.get("profile_id"),
        data.get("sessionId"),
        data.get("session_id"),
        data.get("prompt"),
        data.get("input"),
        data.get("completion"),
        data.get("response_text"),
        data.get("output_text"),
        data.get("modelProvider"),
        data.get("modelName"),
        data.get("modelTitle"),
        data.get("model"),
    )


def event_name(body: dict[str, Any], data: dict[str, Any]) -> str:
    return (
        first_string(
            body.get("name"),
            body.get("eventName"),
            body.get("event"),
            data.get("eventName"),
            data.get("name"),
        )
        or "continue_devdata"
    )


def choose_lifecycle_event(name: str, data: dict[str, Any]) -> str:
    normalized = name.strip().lower()
    if normalized in {"chatinteraction", "editinteraction"}:
        return "assistant_response"
    if normalized in {"editoutcome", "quickedit"}:
        return "turn_completed"
    if first_string(data.get("completion"), data.get("response_text"), data.get("output_text")):
        return "assistant_response"
    return "turn_completed"


def continue_release_grade_assessment(
    body: dict[str, Any],
    data: dict[str, Any],
    name: str,
) -> tuple[bool, list[str]]:
    """Return whether a POST looks like Continue dev-data, not a generic curl."""

    reasons: list[str] = []
    if not isinstance(body.get("data"), dict):
        reasons.append("missing_continue_data_object")

    allowed_schemas = CONTINUE_DEVDATA_EVENTS.get(name)
    schema = body.get("schema")
    schema_string = schema.strip() if isinstance(schema, str) else None
    if allowed_schemas is None:
        reasons.append("unknown_continue_event_name")
    elif schema_string not in allowed_schemas:
        reasons.append("unsupported_or_missing_continue_schema")

    level = body.get("level")
    level_string = level.strip() if isinstance(level, str) else None
    if level_string not in CONTINUE_DEVDATA_LEVELS:
        reasons.append("unsupported_or_missing_continue_level")

    profile_id = first_string(body.get("profileId"), body.get("profile_id"))
    continue_data_signal = any(
        first_string(data.get(key))
        for key in [
            "prompt",
            "input",
            "completion",
            "response_text",
            "output_text",
            "modelProvider",
            "modelName",
            "modelTitle",
            "model",
            "filepath",
            "path",
        ]
    )
    continue_outcome_signal = isinstance(data.get("accepted"), bool)
    if not (profile_id or continue_data_signal or continue_outcome_signal):
        reasons.append("missing_continue_profile_or_event_data")

    if continue_dogfood_or_synthetic_signature(body, data):
        reasons.append("dogfood_or_synthetic_test_event")

    return not reasons, reasons


def normalize_post_body(
    body: dict[str, Any],
    *,
    binding_nonce: str | None,
    project: str | None,
    cwd: str | None,
    session_id: str | None,
) -> tuple[str, dict[str, Any]]:
    data = body.get("data")
    if not isinstance(data, dict):
        data = body

    name = event_name(body, data)
    release_grade_candidate, release_grade_reasons = continue_release_grade_assessment(
        body,
        data,
        name,
    )
    lifecycle_event = choose_lifecycle_event(name, data)
    prompt = first_string(data.get("prompt"), data.get("input"), body.get("prompt"))
    completion = first_string(
        data.get("completion"),
        data.get("response_text"),
        data.get("output_text"),
        body.get("completion"),
    )
    resolved_session = first_string(
        data.get("sessionId"),
        data.get("session_id"),
        body.get("sessionId"),
        body.get("session_id"),
        session_id,
    )
    filepath = first_string(data.get("filepath"), data.get("path"))

    protocol_contract = first_string(data.get("protocol_contract"), body.get("protocol_contract"))
    artifact_version = data.get("artifact_version") or body.get("artifact_version")

    payload: dict[str, Any] = {
        "client": CLIENT,
        "source": CLIENT,
        "event": lifecycle_event,
        "lifecycle_event": lifecycle_event,
        "event_source": EVENT_SOURCE,
        "hook_adapter": "continue_devdata_collector",
        "adapter_contract": CONTRACT,
        "observed_by": "soma-continue-devdata-collector",
        "observed_at_ns": time.time_ns(),
        "collector_release_grade_candidate": release_grade_candidate,
        "collector_release_grade_reasons": release_grade_reasons,
        "continue_event_name": name,
        "continue_schema": body.get("schema"),
        "continue_level": body.get("level"),
        "continue_profile_id": body.get("profileId"),
        "model_provider": data.get("modelProvider"),
        "model_name": data.get("modelName") or data.get("model"),
        "model_title": data.get("modelTitle") or data.get("model"),
        "prompt_text": prompt,
        "response_text": completion,
        "output_text": completion,
        "proposal_action": "request_verification",
        "proposal_reason": (
            "Continue dev-data capture is a private app observation; "
            "independent user/tool/local verification is required before promotion"
        ),
        "session_id": resolved_session,
        "thread_id": resolved_session,
        "project": project,
        "cwd": cwd,
        "filepath": filepath,
        "accepted": data.get("accepted"),
        "trust_boundary": (
            "continue_devdata_event_is_private_app_observation_only; "
            "it does not verify claims, record proof rows, promote drafts, or "
            "apply learning proposals; malformed or synthetic collector POSTs "
            "are marked non_release_debug_only and ignored by observed_app_hook "
            "release proof"
        ),
    }
    if not release_grade_candidate:
        payload["manual_invocation_policy"] = "non_release_debug_only"
        payload["non_release_reason"] = (
            "continue_devdata_payload_missing_release_grade_shape:"
            + ",".join(release_grade_reasons)
        )
    if binding_nonce:
        payload["binding_nonce"] = binding_nonce
    if protocol_contract and artifact_version is not None:
        payload["protocol_contract"] = protocol_contract
        payload["artifact_version"] = artifact_version

    return lifecycle_event, {k: v for k, v in payload.items() if v is not None}


def run_adapter_lifecycle(
    payload: dict[str, Any],
    *,
    lifecycle_event: str,
    soma_bin: str,
    jsonl: str,
    binding_nonce: str | None,
    project: str | None,
    cwd: str | None,
    session_id: str | None,
    fsync: bool,
) -> dict[str, Any]:
    command = [
        soma_bin,
        "adapter-lifecycle",
        "--json",
        "-",
        "--client",
        CLIENT,
        "--event",
        lifecycle_event,
        "--event-source",
        EVENT_SOURCE,
        "--hook-adapter",
        "continue_devdata_collector",
        "--jsonl",
        jsonl,
        "--format",
        "report",
    ]
    if binding_nonce:
        command.extend(["--binding-nonce", binding_nonce])
    if project:
        command.extend(["--project", project])
    if cwd:
        command.extend(["--cwd", cwd])
    if session_id:
        command.extend(["--session-id", session_id])
    if fsync:
        command.append("--fsync")

    proc = subprocess.run(
        command,
        input=json.dumps(payload, separators=(",", ":")) + "\n",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    parsed = None
    if proc.stdout.strip():
        try:
            parsed = json.loads(proc.stdout)
        except Exception:
            parsed = None
    return {
        "ok": proc.returncode == 0,
        "returncode": proc.returncode,
        "stdout": proc.stdout.strip(),
        "stderr": proc.stderr.strip(),
        "json": parsed,
    }


class Handler(http.server.BaseHTTPRequestHandler):
    server: "CollectorServer"

    def do_POST(self) -> None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.respond(411, {"ok": False, "error": "invalid content length"})
            return
        try:
            body = json.loads(self.rfile.read(length).decode("utf-8"))
        except Exception as exc:
            self.respond(400, {"ok": False, "error": f"invalid json: {exc}"})
            return
        if not isinstance(body, dict):
            self.respond(400, {"ok": False, "error": "POST body must be an object"})
            return

        lifecycle_event, payload = normalize_post_body(
            body,
            binding_nonce=self.server.binding_nonce,
            project=self.server.project,
            cwd=self.server.cwd,
            session_id=self.server.session_id,
        )
        result = run_adapter_lifecycle(
            payload,
            lifecycle_event=lifecycle_event,
            soma_bin=self.server.soma_bin,
            jsonl=self.server.jsonl,
            binding_nonce=self.server.binding_nonce,
            project=self.server.project,
            cwd=self.server.cwd,
            session_id=self.server.session_id,
            fsync=self.server.fsync,
        )
        status = 200 if result["ok"] else 502
        response = {
            "ok": result["ok"],
            "contract": CONTRACT,
            "client": CLIENT,
            "lifecycle_event": lifecycle_event,
            "event_source": EVENT_SOURCE,
            "collector_release_grade_candidate": payload.get(
                "collector_release_grade_candidate"
            ),
            "collector_release_grade_reasons": payload.get(
                "collector_release_grade_reasons",
            ),
            "jsonl": self.server.jsonl,
            "adapter_lifecycle": result.get("json"),
            "stderr": result.get("stderr") or None,
        }
        self.respond(status, response)
        if self.server.once:
            self.server.should_stop = True

    def log_message(self, fmt: str, *args: Any) -> None:
        if not self.server.quiet:
            super().log_message(fmt, *args)

    def respond(self, status: int, body: dict[str, Any]) -> None:
        encoded = json.dumps(body, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


class CollectorServer(http.server.HTTPServer):
    def __init__(self, address: tuple[str, int], args: argparse.Namespace) -> None:
        super().__init__(address, Handler)
        self.soma_bin = args.soma_bin
        self.jsonl = args.jsonl
        self.binding_nonce = load_binding_nonce(args.binding_config)
        self.project = args.project
        self.cwd = args.cwd
        self.session_id = args.session_id
        self.fsync = args.fsync
        self.once = args.once
        self.quiet = args.quiet
        self.should_stop = False


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Receive Continue dev-data POSTs and append SOMA lifecycle events."
    )
    parser.add_argument("--host", default=os.environ.get("SOMA_CONTINUE_DEVDATA_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("SOMA_CONTINUE_DEVDATA_PORT", "8766")),
    )
    parser.add_argument("--soma-bin", default=default_soma_bin())
    parser.add_argument("--jsonl", default=os.environ.get("SOMA_ADAPTER_EVENT_JSONL", default_jsonl()))
    parser.add_argument("--binding-config", default=default_binding_config())
    parser.add_argument("--project", default=os.environ.get("SOMA_PROJECT"))
    parser.add_argument("--cwd", default=os.environ.get("SOMA_CWD") or os.getcwd())
    parser.add_argument("--session-id", default=os.environ.get("SOMA_SESSION_ID"))
    parser.add_argument("--fsync", action="store_true")
    parser.add_argument("--once", action="store_true", help="Handle one POST and exit.")
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument(
        "--print-config-snippet",
        action="store_true",
        help="Print a Continue config.yaml data destination snippet and exit.",
    )
    parser.add_argument(
        "--normalize-json",
        help="Normalize one Continue dev-data JSON object from this path, or '-' for stdin, then exit.",
    )
    return parser.parse_args()


def print_config_snippet(host: str, port: int) -> None:
    destination = f"http://{host}:{port}/continue-devdata"
    snippet = {
        "data": [
            {
                "name": "SOMA local dev-data bridge",
                "destination": destination,
                "schema": "0.2.0",
                "level": "all",
                "events": ["chatInteraction", "editInteraction", "editOutcome", "quickEdit"],
            }
        ]
    }
    print(json.dumps(snippet, indent=2, sort_keys=True))


def main() -> int:
    args = parse_args()
    if args.print_config_snippet:
        print_config_snippet(args.host, args.port)
        return 0
    if args.normalize_json:
        if args.normalize_json == "-":
            raw = sys.stdin.read()
        else:
            with open(args.normalize_json, "r", encoding="utf-8") as f:
                raw = f.read()
        body = json.loads(raw)
        if not isinstance(body, dict):
            raise SystemExit("--normalize-json payload must be a JSON object")
        binding_nonce = load_binding_nonce(args.binding_config)
        lifecycle_event, payload = normalize_post_body(
            body,
            binding_nonce=binding_nonce,
            project=args.project,
            cwd=args.cwd,
            session_id=args.session_id,
        )
        print(
            json.dumps(
                {
                    "contract": CONTRACT,
                    "client": CLIENT,
                    "lifecycle_event": lifecycle_event,
                    "event_source": EVENT_SOURCE,
                    "payload": payload,
                },
                indent=2,
                sort_keys=True,
            )
        )
        return 0

    pathlib.Path(args.jsonl).expanduser().parent.mkdir(parents=True, exist_ok=True)
    server = CollectorServer((args.host, args.port), args)
    if not args.quiet:
        print(
            json.dumps(
                {
                    "status": "listening",
                    "contract": CONTRACT,
                    "client": CLIENT,
                    "url": f"http://{args.host}:{args.port}/continue-devdata",
                    "jsonl": args.jsonl,
                    "binding_nonce_present": bool(server.binding_nonce),
                },
                separators=(",", ":"),
            ),
            flush=True,
        )

    try:
        while not server.should_stop:
            server.handle_request()
    except KeyboardInterrupt:
        if not args.quiet:
            print(
                json.dumps(
                    {
                        "status": "stopped",
                        "contract": CONTRACT,
                        "client": CLIENT,
                        "reason": "keyboard_interrupt",
                    },
                    separators=(",", ":"),
                ),
                flush=True,
            )
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
