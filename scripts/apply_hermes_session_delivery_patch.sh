#!/usr/bin/env bash
set -euo pipefail

repo="${1:-/usr/local/lib/hermes-agent}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
origin_patch="${script_dir}/../patches/hermes/api-server-session-cron-delivery.patch"
durability_patch="${script_dir}/../patches/hermes/sessiondb-durable-delivery.patch"

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

if [[ "${changed}" -eq 0 ]]; then
  exit 0
fi

git add \
  cron/scheduler.py \
  gateway/run.py \
  hermes_state.py \
  tests/cron/test_scheduler.py \
  tests/test_sessiondb_delivery_spool.py
git \
  -c user.name="Sandboxed.sh" \
  -c user.email="noreply@sandboxed.sh" \
  commit -m "fix(sessiondb): spool and replay controller deliveries"

echo "Applied Hermes durable session-delivery patches. Restart Hermes after testing."
