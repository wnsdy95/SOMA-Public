#!/usr/bin/env bash
# SOMA stop-hook — capture last user prompt + assistant response into
# the SOMA episode store every time a Claude Code turn ends.
#
# Deploy:
#   cp tools/claude-code-stop-hook.sh ~/.claude/hooks/soma-stop.sh
#   chmod +x ~/.claude/hooks/soma-stop.sh
#
# Then register in ~/.claude/settings.json under `hooks.Stop` so
# Claude Code spawns this script on every turn end:
#   {
#     "hooks": {
#       "Stop": [{
#         "matcher": "*",
#         "hooks": [{"type": "command", "command": "/Users/<you>/.claude/hooks/soma-stop.sh"}]
#       }]
#     }
#   }
#
# Claude Code passes a JSON envelope on stdin:
#   { "session_id": "...", "transcript_path": "...", "hook_event_name": "Stop", ... }
#
# The transcript is JSONL — one event per line. We pull the last
# user message + the last assistant response, then pipe to
# `soma ingest --json -`.
#
# Failure modes are advisory: any error is logged to ~/.soma/log/
# stop-hook.log and the hook returns 0 so it never blocks the user's
# Claude turn.

set -u
LOG="$HOME/.soma/log/stop-hook.log"
mkdir -p "$(dirname "$LOG")"

log() { echo "[$(date '+%H:%M:%S')] $*" >> "$LOG"; }

ENV_JSON=$(cat 2>/dev/null || true)
if [[ -z "$ENV_JSON" ]]; then
    log "no stdin"
    exit 0
fi

SESSION=$(echo "$ENV_JSON" | jq -r '.session_id // empty' 2>/dev/null)
if [[ -n "${SOMA_SESSION_ID:-}" ]]; then
    SESSION="$SOMA_SESSION_ID"
fi
TRANSCRIPT=$(echo "$ENV_JSON" | jq -r '.transcript_path // empty' 2>/dev/null)

if [[ -z "$TRANSCRIPT" || ! -f "$TRANSCRIPT" ]]; then
    log "no transcript at '$TRANSCRIPT'"
    exit 0
fi

# Pull last user + last assistant text from the JSONL transcript.
#
# IMPORTANT 1: Claude Code transcript lines do NOT start with
# `{"type":"...`. The actual first key is `parentUuid`, with `type`
# appearing later in the object. Stream through jq's `select(.type==...)`
# so key order doesn't matter.
#
# IMPORTANT 2: A `type="user"` line may be:
#   * a `tool_result` reply from an agent tool invocation (.message.content
#     is `[{type:"tool_result",...}]`) — NOT the human's prompt.
#   * a system frame (`<system-reminder>...</system-reminder>` wrapping
#     a turn-level reminder) — NOT the human's prompt either.
#   * a real human turn — `.message.content` is either a plain string OR
#     `[{type:"text", text:...}]`.
# Filter out the first two so the hook doesn't ingest garbage as
# "what the user said this turn". Earlier hook captured 4480 tool_result
# lines as user prompts; cleaning that up was the whole point of v2.
LAST_USER=$(jq -r '
    select(.type=="user")
    | (
        if (.message.content | type) == "string"
        then .message.content
        elif (.message.content | type) == "array"
        then [.message.content[]? | select(.type=="text") | .text] | join(" ")
        else empty
        end
      )
    | select(. != null and . != "")
    | select(startswith("<system-reminder>") | not)
    | select(startswith("</task-notification") | not)
    | select(startswith("<command-name>") | not)
  ' "$TRANSCRIPT" 2>/dev/null \
    | grep -v '^$' \
    | tail -1 \
    | head -c 4000)

LAST_ASSISTANT=$(jq -r '
    select(.type=="assistant")
    | [.message.content[]? | select(.type=="text") | .text] | join(" ")
  ' "$TRANSCRIPT" 2>/dev/null \
    | grep -v '^$' \
    | tail -1 \
    | head -c 8000)

if [[ -z "$LAST_USER" && -z "$LAST_ASSISTANT" ]]; then
    log "session=$SESSION transcript=$TRANSCRIPT — no user/assistant text parsed"
    exit 0
fi

PROJECT="${SOMA_PROJECT:-$(basename "${PWD:-unknown}")}"
GIT_BRANCH=$(git -C "${PWD:-/}" branch --show-current 2>/dev/null || echo "")

PAYLOAD=$(jq -nc \
    --arg session "$SESSION" \
    --arg prompt "$LAST_USER" \
    --arg response "$LAST_ASSISTANT" \
    --arg project "$PROJECT" \
    --arg branch "$GIT_BRANCH" \
    '{
        source: "claude-code",
        session_id: $session,
        prompt_text: $prompt,
        response_text: $response,
        project: $project,
        git_branch: $branch
    }')

if echo "$PAYLOAD" | "$HOME/.cargo/bin/soma" ingest --source claude-code --json - 2>>"$LOG"; then
    log "session=$SESSION ingested"
else
    log "session=$SESSION ingest FAILED"
fi

exit 0
