#!/usr/bin/env bash
set -euo pipefail

repo="${1:-/usr/local/lib/hermes-agent}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
patch_file="${script_dir}/../patches/hermes/api-server-session-cron-delivery.patch"

if [[ ! -d "${repo}/.git" ]]; then
  echo "Hermes git install not found at ${repo}" >&2
  exit 1
fi

cd "${repo}"

if git apply --unidiff-zero --reverse --check "${patch_file}" >/dev/null 2>&1; then
  echo "Hermes API-session delivery patch is already applied."
  exit 0
fi

git apply --unidiff-zero --check "${patch_file}"
git apply --unidiff-zero "${patch_file}"
git add cron/scheduler.py tests/cron/test_scheduler.py
git \
  -c user.name="Sandboxed.sh" \
  -c user.email="noreply@sandboxed.sh" \
  commit -m "fix(cron): deliver API jobs to their source session"

echo "Applied Hermes API-session delivery patch. Restart Hermes after testing."
