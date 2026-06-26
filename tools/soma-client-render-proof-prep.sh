#!/usr/bin/env bash
# soma-client-render-proof-prep - materialize proof-free render artifacts.
#
# This helper prepares the files needed before observed_in_client_render can be
# recorded from a real private client UI. Existing artifacts are reused without
# overwrite. It never records proof, verifies a claim, applies a proposal, or
# promotes a cloud draft.

set -euo pipefail

ROOT="${SOMA_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BIN="${SOMA_BIN:-$ROOT/target/debug/soma}"
CLIENT="${SOMA_CLIENT_BINDING_CLIENT:-${SOMA_CLIENT:-cursor}}"
MANIFEST_OVERRIDE="${SOMA_CLIENT_BINDING_MANIFEST:-}"
ARTIFACT_DIR="${SOMA_CLIENT_BINDING_ARTIFACT_DIR:-}"
PROJECT="${SOMA_REVIEW_PROJECT:-}"
SESSION_ID="${SOMA_REVIEW_SESSION_ID:-}"
LIMIT="${SOMA_REVIEW_LIMIT:-20}"

usage() {
    cat <<'EOF'
Usage: tools/soma-client-render-proof-prep.sh [OPTIONS]

Prepare proof-free review-render artifacts for a real private-client render pass.
Existing artifacts are reused without overwrite; missing artifacts are created.

Options:
  --client CLIENT        codex-app, cursor, continue, claude-code, or generic
  --soma-bin PATH        soma binary to invoke
  --manifest PATH        client binding manifest
  --artifact-dir PATH    output directory; refuses to overwrite existing files
  --project PROJECT      optional review scope
  --session-id ID        optional review scope
  --limit N              review-render item limit
  -h, --help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --client)
            [[ $# -ge 2 ]] || { echo "missing value for --client" >&2; exit 2; }
            CLIENT="$2"
            shift 2
            ;;
        --soma-bin|--bin)
            [[ $# -ge 2 ]] || { echo "missing value for $1" >&2; exit 2; }
            BIN="$2"
            shift 2
            ;;
        --manifest)
            [[ $# -ge 2 ]] || { echo "missing value for --manifest" >&2; exit 2; }
            MANIFEST_OVERRIDE="$2"
            shift 2
            ;;
        --artifact-dir)
            [[ $# -ge 2 ]] || { echo "missing value for --artifact-dir" >&2; exit 2; }
            ARTIFACT_DIR="$2"
            shift 2
            ;;
        --project)
            [[ $# -ge 2 ]] || { echo "missing value for --project" >&2; exit 2; }
            PROJECT="$2"
            shift 2
            ;;
        --session-id)
            [[ $# -ge 2 ]] || { echo "missing value for --session-id" >&2; exit 2; }
            SESSION_ID="$2"
            shift 2
            ;;
        --limit)
            [[ $# -ge 2 ]] || { echo "missing value for --limit" >&2; exit 2; }
            LIMIT="$2"
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
if [[ -z "$ARTIFACT_DIR" ]]; then
    RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
    ARTIFACT_DIR="$ROOT/.soma/client-evidence/$CLIENT/render-prep-$RUN_ID"
fi

REVIEW_RENDER_JSON="$ARTIFACT_DIR/review-render.json"
REVIEW_RENDER_MD="$ARTIFACT_DIR/review-render.md"
REVIEW_RENDER_HTML="$ARTIFACT_DIR/review-render.html"
RENDER_EVIDENCE="$ARTIFACT_DIR/render-evidence.json"
RENDER_EVIDENCE_TEMPLATE="$ARTIFACT_DIR/render-evidence-template.json"
RENDER_EVIDENCE_TEMPLATE_STDOUT="$ARTIFACT_DIR/render-evidence-template.out.json"

for path in "$REVIEW_RENDER_JSON" "$REVIEW_RENDER_MD" "$REVIEW_RENDER_HTML" "$RENDER_EVIDENCE" "$RENDER_EVIDENCE_TEMPLATE"
do
    if [[ -e "$path" && ! -s "$path" ]]; then
        python3 - "$CLIENT" "$ARTIFACT_DIR" "$path" <<'PY'
import json
import sys

client, artifact_dir, path = sys.argv[1:4]
print(json.dumps(
    {
        "schema": "soma.client_render_proof_prep.v1",
        "client": client,
        "status": "refused_empty_existing_artifact",
        "artifact_dir": artifact_dir,
        "existing_path": path,
        "records_proof": False,
        "trust_boundary": "render_proof_prep_refuses_empty_existing_artifacts_and_records_no_proof",
    },
    indent=2,
    sort_keys=True,
))
PY
        exit 2
    fi
done

mkdir -p "$ARTIFACT_DIR"

RENDER_ARGS=(context review-render --client "$CLIENT" --limit "$LIMIT")
if [[ -n "$PROJECT" ]]; then
    RENDER_ARGS+=(--project "$PROJECT")
fi
if [[ -n "$SESSION_ID" ]]; then
    RENDER_ARGS+=(--session-id "$SESSION_ID")
fi

if [[ ! -e "$REVIEW_RENDER_JSON" ]]; then
    "$BIN" "${RENDER_ARGS[@]}" --format json --write-report "$REVIEW_RENDER_JSON" >/dev/null
fi
if [[ ! -e "$REVIEW_RENDER_MD" ]]; then
    "$BIN" "${RENDER_ARGS[@]}" --format markdown >"$REVIEW_RENDER_MD"
fi
if [[ ! -e "$REVIEW_RENDER_HTML" ]]; then
    "$BIN" "${RENDER_ARGS[@]}" --format html >"$REVIEW_RENDER_HTML"
fi
if [[ ! -e "$RENDER_EVIDENCE" && -e "$RENDER_EVIDENCE_TEMPLATE" ]]; then
    cp "$RENDER_EVIDENCE_TEMPLATE" "$RENDER_EVIDENCE"
fi
if [[ ! -e "$RENDER_EVIDENCE" ]]; then
    tmp_stdout="$ARTIFACT_DIR/.render-evidence-template.out.$$.$RANDOM.json"
    "$BIN" adapter-binding-proof \
        --render-render-evidence \
        --client "$CLIENT" \
        --manifest "$MANIFEST" \
        --review-render-report "$REVIEW_RENDER_JSON" \
        --write-render-evidence "$RENDER_EVIDENCE" \
        >"$tmp_stdout"
    if [[ ! -e "$RENDER_EVIDENCE_TEMPLATE" ]]; then
        cp "$RENDER_EVIDENCE" "$RENDER_EVIDENCE_TEMPLATE"
    fi
    if [[ ! -e "$RENDER_EVIDENCE_TEMPLATE_STDOUT" ]]; then
        mv "$tmp_stdout" "$RENDER_EVIDENCE_TEMPLATE_STDOUT"
    else
        rm -f "$tmp_stdout"
    fi
elif [[ ! -e "$RENDER_EVIDENCE_TEMPLATE" ]]; then
    cp "$RENDER_EVIDENCE" "$RENDER_EVIDENCE_TEMPLATE"
elif [[ ! -e "$RENDER_EVIDENCE_TEMPLATE_STDOUT" ]]; then
    python3 - "$CLIENT" "$RENDER_EVIDENCE_TEMPLATE" >"$RENDER_EVIDENCE_TEMPLATE_STDOUT" <<'PY'
import json
import sys

client, template = sys.argv[1:3]
print(json.dumps(
    {
        "schema": "soma.client_render_proof_prep.template_stdout.v1",
        "client": client,
        "status": "existing_render_evidence_template_reused",
        "render_evidence_template": template,
        "records_proof": False,
        "trust_boundary": "template_stdout_marker_records_no_proof_and_reuses_existing_template_without_overwrite",
    },
    indent=2,
    sort_keys=True,
))
PY
fi

INSTALLED_CONFIG=""
case "$CLIENT" in
    codex-app) INSTALLED_CONFIG="$HOME/.codex/soma-installed-binding.json" ;;
    cursor) INSTALLED_CONFIG="$HOME/.cursor/soma-installed-binding.json" ;;
    continue) INSTALLED_CONFIG="$HOME/.continue/soma-installed-binding.json" ;;
    claude-code) INSTALLED_CONFIG="$HOME/.claude/soma-installed-binding.json" ;;
esac

python3 - \
    "$CLIENT" \
    "$ARTIFACT_DIR" \
    "$MANIFEST" \
    "$REVIEW_RENDER_JSON" \
    "$REVIEW_RENDER_MD" \
    "$REVIEW_RENDER_HTML" \
    "$RENDER_EVIDENCE" \
    "$RENDER_EVIDENCE_TEMPLATE" \
    "$RENDER_EVIDENCE_TEMPLATE_STDOUT" \
    "$BIN" \
    "$INSTALLED_CONFIG" <<'PY'
import json
import os
import sys

(
    client,
    artifact_dir,
    manifest,
    review_render_json,
    review_render_md,
    review_render_html,
    render_evidence,
    render_evidence_template,
    render_evidence_template_stdout,
    soma_bin,
    installed_config,
) = sys.argv[1:12]

record_command = [
    soma_bin,
    "adapter-binding-proof",
    "--manifest",
    manifest,
    "--client",
    client,
    "--proof-level",
    "observed_in_client_render",
]
if installed_config:
    record_command.extend(["--installed-config", installed_config])
record_command.extend(
    [
        "--review-render-report",
        review_render_json,
        "--render-evidence",
        render_evidence,
        "--evidence-source",
        f"private_client_operator_observed_{client}_observed_in_client_render",
        "--operator-confirm-in-client-render",
        "--operator-confirm-release-grade-evidence",
    ]
)

print(json.dumps(
    {
        "schema": "soma.client_render_proof_prep.v1",
        "client": client,
        "status": "ready_for_visible_client_render",
        "artifact_dir": artifact_dir,
        "records_proof": False,
        "creates_verification_event": False,
        "promotes_cloud_draft": False,
        "applies_proposal": False,
        "overwrite_policy": "reuse_existing_artifacts_without_overwrite",
        "artifacts": {
            "review_render_json": review_render_json,
            "review_render_markdown": review_render_md,
            "review_render_html": review_render_html,
            "render_evidence": render_evidence,
            "render_evidence_template": render_evidence_template,
            "render_evidence_template_stdout": render_evidence_template_stdout,
        },
        "next_steps": [
            f"Open {review_render_md} or {review_render_html} in the real {client} UI.",
            f"After the surface is visibly rendered, replace placeholders in {render_evidence} from that real UI observation.",
            "Record observed_in_client_render only with explicit operator confirmation and release-grade evidence.",
        ],
        "record_command_after_filled_evidence": record_command,
        "trust_boundary": (
            "client_render_proof_prep_is_read_only: materializes review-render and "
            "render-evidence placeholder/template artifacts only, reusing existing artifacts without "
            "overwrite; records no proof row, creates no verification event, promotes no "
            "cloud draft, applies no proposal, and cannot prove private-client rendering "
            "until a real visible UI observation is filled and recorded with explicit "
            "operator confirmation"
        ),
    },
    indent=2,
    sort_keys=True,
))
PY
