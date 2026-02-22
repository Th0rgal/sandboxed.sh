#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

BASE_URL="${SANDBOXED_SH_BASE_URL:-}"
TOKEN="${SANDBOXED_SH_TOKEN:-}"
MISSION_ID=""
OUT_DIR="${ROOT_DIR}/output/debug-bundles"
PAGE_SIZE=200
MAX_EVENTS=2000

usage() {
  cat <<USAGE
Usage:
  scripts/mission_debug_bundle.sh [options]

Collects mission diagnostics from the control API and writes a compressed bundle.

Options:
  --base-url URL          Backend base URL (env: SANDBOXED_SH_BASE_URL)
  --token TOKEN           Control API bearer token (env: SANDBOXED_SH_TOKEN)
  --mission-id UUID       Mission ID to export (required)
  --out-dir PATH          Output directory (default: output/debug-bundles)
  --page-size N           Events page size (default: 200)
  --max-events N          Maximum events to export (default: 2000)
  -h, --help              Show this help

Example:
  scripts/mission_debug_bundle.sh \
    --base-url https://agent-backend-dev.example.com \
    --token <token> \
    --mission-id 11111111-2222-3333-4444-555555555555
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-url)
      BASE_URL="${2:-}"
      shift 2
      ;;
    --token)
      TOKEN="${2:-}"
      shift 2
      ;;
    --mission-id)
      MISSION_ID="${2:-}"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="${2:-}"
      shift 2
      ;;
    --page-size)
      PAGE_SIZE="${2:-}"
      shift 2
      ;;
    --max-events)
      MAX_EVENTS="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi
if ! command -v tar >/dev/null 2>&1; then
  echo "error: tar is required" >&2
  exit 1
fi

if [[ -z "$BASE_URL" ]]; then
  echo "Missing --base-url or SANDBOXED_SH_BASE_URL" >&2
  exit 1
fi
if [[ -z "$TOKEN" ]]; then
  echo "Missing --token or SANDBOXED_SH_TOKEN" >&2
  exit 1
fi
if [[ -z "$MISSION_ID" ]]; then
  echo "Missing --mission-id" >&2
  exit 1
fi
if ! [[ "$PAGE_SIZE" =~ ^[0-9]+$ ]] || [[ "$PAGE_SIZE" -le 0 ]]; then
  echo "--page-size must be a positive integer" >&2
  exit 1
fi
if ! [[ "$MAX_EVENTS" =~ ^[0-9]+$ ]] || [[ "$MAX_EVENTS" -le 0 ]]; then
  echo "--max-events must be a positive integer" >&2
  exit 1
fi

BASE_URL="${BASE_URL%/}"
TIMESTAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
BUNDLE_DIR="${OUT_DIR}/mission-debug-${MISSION_ID}-${TIMESTAMP}"
RAW_DIR="${BUNDLE_DIR}/raw"
EVENTS_DIR="${RAW_DIR}/events"
mkdir -p "$EVENTS_DIR"

FAILURES=0

api_get() {
  local endpoint="$1"
  local out_file="$2"
  local tmp_body
  tmp_body="$(mktemp)"

  local status
  status="$(curl -sS \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Accept: application/json" \
    -w "%{http_code}" \
    "${BASE_URL}${endpoint}" \
    -o "${tmp_body}" || true)"

  if [[ "$status" =~ ^2[0-9][0-9]$ ]]; then
    mv "$tmp_body" "$out_file"
    return 0
  fi

  FAILURES=$((FAILURES + 1))
  jq -n \
    --arg endpoint "$endpoint" \
    --arg status "$status" \
    --rawfile body "$tmp_body" \
    '{endpoint: $endpoint, http_status: $status, response_body: $body}' >"$out_file"
  rm -f "$tmp_body"
  return 1
}

# Snapshot mission-level endpoints.
api_get "/api/control/missions/${MISSION_ID}" "${RAW_DIR}/mission.json" || true
api_get "/api/control/missions/${MISSION_ID}/tree" "${RAW_DIR}/mission_tree.json" || true
api_get "/api/control/missions/${MISSION_ID}/automations" "${RAW_DIR}/automations.json" || true
api_get "/api/control/missions/${MISSION_ID}/automation-executions" "${RAW_DIR}/automation_executions.json" || true
api_get "/api/control/progress" "${RAW_DIR}/progress.json" || true
api_get "/api/control/diagnostics/opencode" "${RAW_DIR}/opencode_diagnostics.json" || true

# Export mission events with pagination.
: >"${RAW_DIR}/events.ndjson"
TOTAL_EVENTS=0
PAGE=0
OFFSET=0

while [[ "$TOTAL_EVENTS" -lt "$MAX_EVENTS" ]]; do
  PAGE_FILE="${EVENTS_DIR}/page-$(printf "%04d" "$PAGE").json"
  if ! api_get "/api/control/missions/${MISSION_ID}/events?limit=${PAGE_SIZE}&offset=${OFFSET}" "$PAGE_FILE"; then
    break
  fi

  COUNT="$(jq 'length' "$PAGE_FILE" 2>/dev/null || echo 0)"
  if ! [[ "$COUNT" =~ ^[0-9]+$ ]]; then
    COUNT=0
  fi

  if [[ "$COUNT" -eq 0 ]]; then
    break
  fi

  jq -c '.[]' "$PAGE_FILE" >>"${RAW_DIR}/events.ndjson"

  TOTAL_EVENTS=$((TOTAL_EVENTS + COUNT))
  OFFSET=$((OFFSET + COUNT))
  PAGE=$((PAGE + 1))

  if [[ "$COUNT" -lt "$PAGE_SIZE" ]]; then
    break
  fi
done

jq -n \
  --arg generated_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
  --arg mission_id "$MISSION_ID" \
  --arg base_url "$BASE_URL" \
  --argjson total_events "$TOTAL_EVENTS" \
  --argjson max_events "$MAX_EVENTS" \
  --argjson page_size "$PAGE_SIZE" \
  --argjson failures "$FAILURES" \
  '{generated_at: $generated_at, mission_id: $mission_id, base_url: $base_url, total_events_exported: $total_events, max_events: $max_events, page_size: $page_size, failed_requests: $failures}' >"${BUNDLE_DIR}/bundle_meta.json"

if [[ -s "${RAW_DIR}/events.ndjson" ]]; then
  jq -s '
    {
      total_events: length,
      by_type: (group_by(.event_type) | map({event_type: .[0].event_type, count: length}) | sort_by(.event_type)),
      first_timestamp: (map(.timestamp) | min),
      last_timestamp: (map(.timestamp) | max),
      terminal_error_events: [ .[] | select(.event_type == "error") | {sequence, timestamp, content} ]
    }
  ' "${RAW_DIR}/events.ndjson" >"${BUNDLE_DIR}/events_summary.json"
else
  jq -n '{total_events: 0, by_type: [], terminal_error_events: []}' >"${BUNDLE_DIR}/events_summary.json"
fi

# Mission summary (if mission endpoint succeeded)
if jq -e 'has("id")' "${RAW_DIR}/mission.json" >/dev/null 2>&1; then
  jq '{
    id,
    title,
    status,
    backend,
    model_override,
    model_effort,
    terminal_reason,
    created_at,
    updated_at,
    interrupted_at,
    resumable,
    history_entries: (.history | length)
  }' "${RAW_DIR}/mission.json" >"${BUNDLE_DIR}/mission_summary.json"
fi

cat >"${BUNDLE_DIR}/README.txt" <<TXT
Mission Debug Bundle

Mission ID: ${MISSION_ID}
Generated at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
Base URL: ${BASE_URL}

Contents:
- bundle_meta.json: bundle generation metadata and request failure count
- mission_summary.json: mission status snapshot and terminal_reason
- events_summary.json: event counts, timeline bounds, and error excerpts
- raw/: full API payloads for mission, tree, automations, progress, diagnostics, and paged events

Notes:
- Authentication token is never stored in this bundle.
- If failed_requests > 0 in bundle_meta.json, inspect raw/*.json for endpoint-level error responses.
TXT

ARCHIVE_PATH="${OUT_DIR}/mission-debug-${MISSION_ID}-${TIMESTAMP}.tar.gz"
tar -czf "$ARCHIVE_PATH" -C "$OUT_DIR" "mission-debug-${MISSION_ID}-${TIMESTAMP}"

if [[ "$FAILURES" -gt 0 ]]; then
  echo "Bundle generated with ${FAILURES} request failure(s): ${ARCHIVE_PATH}"
else
  echo "Bundle generated: ${ARCHIVE_PATH}"
fi
