#!/usr/bin/env python3
"""Install the SOMA local dev-data destination into Continue config.yaml.

This script only edits Continue's local config file. It does not start the
collector, record proof rows, create verification events, or promote memory.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import sys
import time
from typing import Any


SCHEMA = "soma.continue_devdata_install.v1"
DESTINATION_NAME = "SOMA local dev-data bridge"
DEFAULT_PROFILE_NAME = "SOMA local Continue config"
DEFAULT_PROFILE_VERSION = "0.0.1"
REQUIRED_PROFILE_FIELDS = ["name", "version"]
DEFAULT_EVENTS = ["chatInteraction", "editInteraction", "editOutcome", "quickEdit"]
TRUST_BOUNDARY = (
    "continue_devdata_install_edits_only_local_continue_config; it records no "
    "proof row, creates no verification event, starts no collector, promotes no "
    "cloud draft, and cannot substitute for a real Continue hook event"
)


def root_dir() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parents[1]


def default_config_path() -> str:
    return str(pathlib.Path.home() / ".continue" / "config.yaml")


def default_collector_path() -> str:
    return str(root_dir() / "tools" / "soma-continue-devdata-collector.py")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Install SOMA's local Continue dev-data destination."
    )
    parser.add_argument("--config", default=os.environ.get("SOMA_CONTINUE_CONFIG", default_config_path()))
    parser.add_argument("--collector", default=os.environ.get("SOMA_CONTINUE_COLLECTOR", default_collector_path()))
    parser.add_argument("--host", default=os.environ.get("SOMA_CONTINUE_DEVDATA_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("SOMA_CONTINUE_DEVDATA_PORT", "8766")),
    )
    parser.add_argument("--schema-version", default="0.2.0")
    parser.add_argument("--level", choices=["all", "noCode"], default="all")
    parser.add_argument("--event", action="append", dest="events", help="Continue dev-data event to include. Repeatable.")
    parser.add_argument("--dry-run", action="store_true", help="Report the planned edit without writing.")
    parser.add_argument("--write", action="store_true", help="Write the config edit.")
    return parser.parse_args()


def destination_url(host: str, port: int) -> str:
    return f"http://{host}:{port}/continue-devdata"


def yaml_entry(
    *,
    destination: str,
    schema_version: str,
    level: str,
    events: list[str],
    indent: str = "  ",
) -> list[str]:
    lines = [
        f"{indent}- name: {DESTINATION_NAME}\n",
        f"{indent}  destination: {destination}\n",
        f"{indent}  schema: {schema_version}\n",
        f"{indent}  level: {level}\n",
        f"{indent}  events:\n",
    ]
    lines.extend(f"{indent}    - {event}\n" for event in events)
    return lines


def config_snippet(
    *,
    destination: str,
    schema_version: str,
    level: str,
    events: list[str],
) -> str:
    return (
        f"name: {DEFAULT_PROFILE_NAME}\n"
        f"version: {DEFAULT_PROFILE_VERSION}\n"
        "data:\n"
    ) + "".join(
        yaml_entry(
            destination=destination,
            schema_version=schema_version,
            level=level,
            events=events,
        )
    )


def line_starts_top_level_data(line: str) -> bool:
    stripped = line.strip()
    return bool(line and not line.startswith((" ", "\t")) and stripped == "data:")


def has_top_level_yaml_key(text: str, key: str) -> bool:
    for line in text.splitlines():
        stripped = line.lstrip()
        if not stripped or stripped.startswith("#") or len(stripped) != len(line):
            continue
        found, sep, _ = stripped.partition(":")
        if sep and found.strip() == key:
            return True
    return False


def missing_profile_fields(text: str) -> list[str]:
    return [field for field in REQUIRED_PROFILE_FIELDS if not has_top_level_yaml_key(text, field)]


def ensure_profile_header(text: str) -> tuple[str, bool, list[str]]:
    missing = missing_profile_fields(text)
    if not missing:
        return text, False, []
    header = []
    if "name" in missing:
        header.append(f"name: {DEFAULT_PROFILE_NAME}\n")
    if "version" in missing:
        header.append(f"version: {DEFAULT_PROFILE_VERSION}\n")
    if text and not text.startswith("\n"):
        header.append("\n")
    return "".join(header) + text, True, missing


def insert_into_existing_data(lines: list[str], entry: list[str]) -> tuple[list[str], str]:
    for index, line in enumerate(lines):
        if line_starts_top_level_data(line):
            updated = lines[: index + 1] + entry + lines[index + 1 :]
            return updated, "insert_into_existing_data"
    suffix = []
    if lines and not lines[-1].endswith("\n"):
        lines[-1] = lines[-1] + "\n"
    if lines and lines[-1].strip():
        suffix.append("\n")
    updated = lines + suffix + ["data:\n"] + entry
    return updated, "append_top_level_data"


def existing_destination_installed(text: str, destination: str) -> bool:
    return destination in text or DESTINATION_NAME in text


def plan_update(
    original: str | None,
    *,
    destination: str,
    schema_version: str,
    level: str,
    events: list[str],
) -> tuple[str, bool, str, bool, list[str]]:
    if original is None or not original.strip():
        return (
            config_snippet(
                destination=destination,
                schema_version=schema_version,
                level=level,
                events=events,
            ),
            True,
            "create_config_with_data",
            True,
            REQUIRED_PROFILE_FIELDS.copy(),
        )

    if existing_destination_installed(original, destination):
        updated, profile_changed, profile_missing = ensure_profile_header(original)
        if profile_changed:
            return updated, True, "repair_profile_header", True, profile_missing
        return original, False, "already_installed", False, []

    lines = original.splitlines(keepends=True)
    entry = yaml_entry(
        destination=destination,
        schema_version=schema_version,
        level=level,
        events=events,
    )
    updated, strategy = insert_into_existing_data(lines, entry)
    updated_text, profile_changed, profile_missing = ensure_profile_header("".join(updated))
    return updated_text, True, strategy, profile_changed, profile_missing


def write_config(path: pathlib.Path, contents: str, original_exists: bool) -> str | None:
    path.parent.mkdir(parents=True, exist_ok=True)
    backup_path = None
    if original_exists:
        stamp = time.strftime("%Y%m%d%H%M%S")
        backup_path = f"{path}.bak-soma-continue-devdata-{stamp}"
        shutil.copy2(path, backup_path)
    tmp_path = path.with_name(f"{path.name}.tmp-soma-continue-devdata-{os.getpid()}")
    with open(tmp_path, "w", encoding="utf-8") as f:
        f.write(contents)
    os.replace(tmp_path, path)
    return backup_path


def collector_command(collector: str, host: str, port: int) -> list[str]:
    return [collector, "--host", host, "--port", str(port)]


def main() -> int:
    args = parse_args()
    if args.write and args.dry_run:
        raise SystemExit("--write and --dry-run are mutually exclusive")
    if not args.write and not args.dry_run:
        raise SystemExit("choose --dry-run or --write")

    events = args.events or list(DEFAULT_EVENTS)
    destination = destination_url(args.host, args.port)
    config_path = pathlib.Path(args.config).expanduser()
    collector = str(pathlib.Path(args.collector).expanduser())

    original = None
    original_exists = config_path.exists()
    if original_exists:
        original = config_path.read_text(encoding="utf-8")

    updated, changed, strategy, profile_header_changed, profile_missing_fields = plan_update(
        original,
        destination=destination,
        schema_version=args.schema_version,
        level=args.level,
        events=events,
    )

    backup_path = None
    if args.write and changed:
        backup_path = write_config(config_path, updated, original_exists)

    report: dict[str, Any] = {
        "schema": SCHEMA,
        "status": "updated" if args.write and changed else "already_installed" if not changed else "would_update",
        "dry_run": args.dry_run,
        "changed": changed,
        "merge_strategy": strategy,
        "profile_header_changed": profile_header_changed,
        "profile_missing_required_fields": profile_missing_fields,
        "profile_required_fields_present_after": not missing_profile_fields(updated),
        "config_path": str(config_path),
        "config_exists": original_exists,
        "backup_path": backup_path,
        "destination": destination,
        "data_entry": {
            "name": DESTINATION_NAME,
            "destination": destination,
            "schema": args.schema_version,
            "level": args.level,
            "events": events,
        },
        "collector_command": collector_command(collector, args.host, args.port),
        "reload_required": changed,
        "next_step": (
            "Start the collector, reload Continue or its host editor, run a real Continue "
            "chat/edit/review action, then use tools/soma-client-hook-readiness.sh before "
            "recording observed_app_hook proof."
        ),
        "trust_boundary": TRUST_BOUNDARY,
    }
    if args.dry_run:
        report["planned_config"] = updated

    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
