#!/usr/bin/env bash
set -euo pipefail

# Post-deploy fleet smoke (P#9). One command confirms the critical mission loop
# is alive and the new project/awaiting/health surfaces respond. Creates a
# disposable probe mission, exercises the project + status endpoints against it,
# then deletes it. Read-only against everything else.
#
# Usage:
#   scripts/prod-smoke-fleet.sh --base-url URL --token JWT
#   BASE_URL=... TOKEN=... scripts/prod-smoke-fleet.sh
#
# Options / env:
#   --base-url URL   Backend base URL (env: BASE_URL, HERMES_SANDBOXED_API_URL)
#   --token JWT      Control API bearer token (env: TOKEN, HERMES_SANDBOXED_API_TOKEN)
#   --keep           Do not delete the probe mission at the end
#   -h, --help       Show this help

BASE_URL="${BASE_URL:-${HERMES_SANDBOXED_API_URL:-http://127.0.0.1:8080}}"
TOKEN="${TOKEN:-${HERMES_SANDBOXED_API_TOKEN:-}}"
KEEP_PROBE=0

usage() { sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-url) BASE_URL="${2:-}"; shift 2 ;;
    --token) TOKEN="${2:-}"; shift 2 ;;
    --keep) KEEP_PROBE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }
[[ -n "$BASE_URL" ]] || { echo "Missing --base-url / BASE_URL" >&2; exit 2; }

auth=()
[[ -n "$TOKEN" ]] && auth=(-H "Authorization: Bearer $TOKEN")

pass=0
fail=0
ok()   { echo "  ok   $*"; pass=$((pass + 1)); }
bad()  { echo "  FAIL $*" >&2; fail=$((fail + 1)); }

# GET helper: prints body, returns curl exit status.
get() { curl -fsS "${auth[@]}" "$BASE_URL$1"; }

echo "== fleet smoke against $BASE_URL =="

# 1) Liveness
if get /api/health | jq -e '.status == "ok"' >/dev/null 2>&1; then
  ok "/api/health"
else
  bad "/api/health"
fi

if fleet="$(get /api/health/fleet 2>/dev/null)"; then
  if echo "$fleet" | jq -e '.control_responsive == true' >/dev/null 2>&1; then
    ok "/api/health/fleet control_responsive=$(echo "$fleet" | jq -r '.control_responsive') \
running=$(echo "$fleet" | jq -r '.running') queue=$(echo "$fleet" | jq -r '.queue_depth') \
webhook=$(echo "$fleet" | jq -r '.webhook_forwarder_configured') \
offload=$(echo "$fleet" | jq -r '.offload_configured')"
  else
    bad "/api/health/fleet control not responsive: $fleet"
  fi
else
  bad "/api/health/fleet unreachable"
fi

# 2) Mission list
if get "/api/control/missions?limit=1" | jq -e 'type == "array"' >/dev/null 2>&1; then
  ok "/api/control/missions list"
else
  bad "/api/control/missions list"
fi

# 3) Project tagging + status lifecycle on a disposable probe mission.
probe_id=""
cleanup() {
  if [[ -n "$probe_id" && "$KEEP_PROBE" == "0" ]]; then
    curl -fsS -X DELETE "${auth[@]}" "$BASE_URL/api/control/missions/$probe_id" >/dev/null 2>&1 \
      && echo "  cleaned up probe $probe_id" \
      || echo "  WARN could not delete probe $probe_id" >&2
  fi
}
trap cleanup EXIT

probe="$(curl -fsS -X POST "${auth[@]}" -H 'Content-Type: application/json' \
  -d '{"title":"[smoke] fleet probe","project":"smoke-test","tags":["smoke","probe"]}' \
  "$BASE_URL/api/control/missions" 2>/dev/null || true)"
probe_id="$(echo "$probe" | jq -r '.id // empty')"

if [[ -n "$probe_id" ]]; then
  ok "create probe mission $probe_id"
  if echo "$probe" | jq -e '.project == "smoke-test" and (.tags | index("smoke"))' >/dev/null 2>&1; then
    ok "project tags set at creation"
  else
    bad "project tags missing on create: $(echo "$probe" | jq -c '{project,tags}')"
  fi

  # Filter by project should surface the probe.
  if get "/api/control/missions?project=smoke-test&limit=50" \
      | jq -e --arg id "$probe_id" 'any(.[]; .id == $id)' >/dev/null 2>&1; then
    ok "project filter returns probe"
  else
    bad "project filter did not return probe"
  fi

  # Update project metadata via the dedicated endpoint.
  if curl -fsS -X POST "${auth[@]}" -H 'Content-Type: application/json' \
      -d '{"track":"smoke-track","intent":"smoke-intent","github_pr":2061}' \
      "$BASE_URL/api/control/missions/$probe_id/project" \
      | jq -e '.track == "smoke-track" and .intent == "smoke-intent" and .github_pr == 2061' >/dev/null 2>&1; then
    ok "POST /project updates metadata"
  else
    bad "POST /project did not update metadata"
  fi

  # Acknowledge: must move out of the active flow without relaunching anything.
  if curl -fsS -X POST "${auth[@]}" -H 'Content-Type: application/json' \
      -d '{"status":"acknowledged"}' \
      "$BASE_URL/api/control/missions/$probe_id/status" >/dev/null 2>&1; then
    sleep 1
    st="$(get "/api/control/missions/$probe_id" | jq -r '.status')"
    if [[ "$st" == "acknowledged" ]]; then
      ok "status acknowledge (now: $st)"
    else
      bad "status after acknowledge is '$st', expected acknowledged"
    fi
  else
    bad "POST /status acknowledged failed"
  fi
else
  bad "create probe mission (response: $(echo "$probe" | head -c 200))"
fi

# 4) Usage/quota endpoint (non-fatal: optional surface).
if get "/api/ai/providers" | jq -e 'type == "array" or type == "object"' >/dev/null 2>&1; then
  ok "/api/ai/providers"
else
  echo "  warn /api/ai/providers did not return JSON (non-fatal)"
fi

echo "== fleet smoke: $pass passed, $fail failed =="
[[ "$fail" -eq 0 ]]
