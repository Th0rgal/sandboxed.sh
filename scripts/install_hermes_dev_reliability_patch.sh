#!/usr/bin/env bash
set -euo pipefail

source_repo="${HERMES_SOURCE_REPO:-/usr/local/lib/hermes-agent}"
dev_repo="${HERMES_DEV_REPO:-/usr/local/lib/hermes-agent-dev}"
dev_unit="${HERMES_DEV_UNIT:-hermes-assistant-dev.service}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
dropin_source="${script_dir}/../deploy/systemd/hermes-assistant-dev-isolated.conf"
dropin_dir="/etc/systemd/system/${dev_unit}.d"
dropin_dest="${dropin_dir}/70-isolated-checkout.conf"

if [[ ! -d "${source_repo}/.git" ]]; then
  echo "Hermes source checkout not found at ${source_repo}" >&2
  exit 1
fi

source_revision="$(git -C "${source_repo}" rev-parse HEAD)"
if [[ ! -d "${dev_repo}/.git" ]]; then
  git clone --no-hardlinks "${source_repo}" "${dev_repo}"
fi

if [[ -n "$(git -C "${dev_repo}" status --porcelain)" ]]; then
  echo "Hermes dev checkout has uncommitted changes: ${dev_repo}" >&2
  exit 1
fi

git -C "${dev_repo}" fetch "${source_repo}" "${source_revision}"
git -C "${dev_repo}" checkout --detach "${source_revision}"
"${script_dir}/apply_hermes_session_delivery_patch.sh" "${dev_repo}"

(
  cd "${dev_repo}"
  uv sync --frozen --no-dev
  .venv/bin/python -m py_compile hermes_state.py gateway/run.py cron/scheduler.py
)

install -d -m 0755 "${dropin_dir}"
install -m 0644 "${dropin_source}" "${dropin_dest}"
systemctl daemon-reload

echo "Hermes dev installed at ${dev_repo} from ${source_revision}."
echo "Production checkout ${source_repo} was not modified."
echo "Start or restart ${dev_unit} after its targeted tests pass."
