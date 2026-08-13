#!/usr/bin/env bash
set -euo pipefail

repo="${1:-/usr/local/lib/hermes-agent}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
origin_patch="${script_dir}/../patches/hermes/api-server-session-cron-delivery.patch"
durability_patch="${script_dir}/../patches/hermes/sessiondb-durable-delivery.patch"
mission_route_py="${script_dir}/../patches/hermes/mission_status_route.py"
mission_route_test="${script_dir}/../patches/hermes/test_mission_status_route.py"
mission_webhook_patch="${script_dir}/../patches/hermes/mission-complete-route-origin-webhook.patch"
mission_cronjob_patch="${script_dir}/../patches/hermes/mission-complete-route-origin-cronjob.patch"

if [[ ! -d "${repo}/.git" ]]; then
  echo "Hermes git install not found at ${repo}" >&2
  exit 1
fi

cd "${repo}"

changed=0
apply_one() {
  local patch_file="$1"
  local label="$2"
  if git apply --unidiff-zero --reverse --check "${patch_file}" >/dev/null 2>&1; then
    echo "Hermes ${label} patch is already applied."
    return
  fi
  git apply --unidiff-zero --check "${patch_file}"
  git apply --unidiff-zero "${patch_file}"
  changed=1
}

apply_one "${origin_patch}" "API-session origin"
apply_one "${durability_patch}" "SessionDB durable-delivery"

install_file() {
  local src="$1"
  local dest="$2"
  if [[ -f "${dest}" ]] && cmp -s "${src}" "${dest}"; then
    echo "Hermes $(basename "${dest}") is already current."
    return
  fi
  mkdir -p "$(dirname "${dest}")"
  cp "${src}" "${dest}"
  changed=1
}

install_file "${mission_route_py}" "${repo}/gateway/platforms/mission_status_route.py"
install_file "${mission_route_test}" "${repo}/tests/gateway/test_mission_status_route.py"
apply_one "${mission_webhook_patch}" "mission-complete webhook route"
apply_one "${mission_cronjob_patch}" "cron origin-less deliver=origin"

if [[ "${changed}" -eq 0 ]]; then
  exit 0
fi

git add \
  cron/scheduler.py \
  gateway/run.py \
  gateway/platforms/webhook.py \
  gateway/platforms/mission_status_route.py \
  hermes_state.py \
  tools/cronjob_tools.py \
  tests/cron/test_scheduler.py \
  tests/gateway/test_mission_status_route.py \
  tests/test_sessiondb_delivery_spool.py
git \
  -c user.name="Sandboxed.sh" \
  -c user.email="noreply@sandboxed.sh" \
  commit -m "fix: route mission-complete into the dedicated session"

echo "Applied Hermes durable session-delivery patches. Restart Hermes after testing."
