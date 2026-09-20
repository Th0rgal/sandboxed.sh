#!/usr/bin/env bash
# Typed remote launch canary: the Orb request shape against a real node.
#
# Creates a mission with prompt + remote_node_id + backend + model_override
# (no raw remote_command, no client-minted key), then watches the mission's
# `remote_job` placement block, execution lease and status until terminal.
# Prints no secrets. Run only against a backend where the coordinator has
# approved the launch (dev first: SANDBOXED_CORE_URL=http://127.0.0.1:3001).
#
#   SANDBOXED_CORE_URL=https://agent-backend.thomas.md \
#   DASHBOARD_JWT=... \
#   scripts/remote-launch-canary.sh dgx-spark opencode xai/grok-4.6 test \
#     "Print the hostname and nvidia-smi -L, then stop."
set -euo pipefail

node="${1:?node id (e.g. dgx-spark)}"
backend="${2:?backend (claudecode|opencode)}"
model="${3:?model override (e.g. claude-opus-5 or xai/grok-4.6)}"
project="${4:?project slug}"
prompt="${5:?prompt}"
base="${SANDBOXED_CORE_URL:?SANDBOXED_CORE_URL}"
jwt="${DASHBOARD_JWT:?DASHBOARD_JWT}"
key="canary-$(date -u +%Y%m%dT%H%M%SZ)-$RANDOM"

body=$(jq -cn --arg p "$prompt" --arg n "$node" --arg b "$backend" --arg m "$model" \
  --arg proj "$project" --arg k "$key" \
  '{title:"remote launch canary", prompt:$p, remote_node_id:$n, backend:$b,
    model_override:$m, project:$proj, idempotency_key:$k}')

echo "POST /api/control/missions node=$node backend=$backend model=$model idempotency_key=$key"
created=$(curl -sS -X POST "$base/api/control/missions" \
  -H "Authorization: Bearer $jwt" -H "Content-Type: application/json" -d "$body")
echo "$created" | jq '{id, status, backend, model_override, remote_job, execution: .execution.state}'
id=$(echo "$created" | jq -r '.id // empty')
[ -n "$id" ] || { echo "create failed: $created" >&2; exit 1; }

echo "retry with the same idempotency_key must coalesce (x-coalesced-with):"
curl -sS -D - -o /dev/null -X POST "$base/api/control/missions" \
  -H "Authorization: Bearer $jwt" -H "Content-Type: application/json" -d "$body" \
  | grep -i "x-coalesced-with" || echo "  (no coalesce header — investigate before trusting retries)"

for _ in $(seq 1 120); do
  m=$(curl -sS "$base/api/control/missions/$id" -H "Authorization: Bearer $jwt")
  status=$(echo "$m" | jq -r .status)
  echo "$(date -u +%H:%M:%S) status=$status reason=$(echo "$m" | jq -r '.terminal_reason // "-"') \
phase=$(echo "$m" | jq -r '.remote_job.phase // "-"') node_state=$(echo "$m" | jq -r '.remote_job.node_state // "-"') \
lease=$(echo "$m" | jq -r '.execution.state // "-"')"
  case "$status" in completed|failed|interrupted|cancelled) break ;; esac
  sleep 5
done

echo "events:"
curl -sS "$base/api/control/missions/$id/events?limit=50" -H "Authorization: Bearer $jwt" \
  | jq -r '.[] | "\(.sequence) \(.event_type): \(.content | tostring | .[0:160])"'
echo "expect: a user_message first, a mission_status_changed 'Dispatched job … (<harness>/<model>; node state: queued)', no orphan_no_runner, and a terminal assistant_message with the node log tail."
