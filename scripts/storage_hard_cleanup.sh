#!/usr/bin/env bash
set -euo pipefail

mode="${1:---dry-run}"
if [[ "$mode" != "--dry-run" && "$mode" != "--apply" ]]; then
  echo "usage: $0 [--dry-run|--apply]" >&2
  exit 2
fi

data_root="${SANDBOXED_DATA_ROOT:-/root/.sandboxed-sh}"
opencode_cache="${OPENCODE_CACHE_ROOT:-/var/lib/opencode/.cache}"
hermes_home="${HERMES_HOME:-/var/lib/hermes-assistant}"
backup_days="${STORAGE_BACKUP_RETENTION_DAYS:-1}"
log_days="${STORAGE_LOG_RETENTION_DAYS:-3}"
cache_days="${STORAGE_CACHE_RETENTION_DAYS:-3}"
log_max_mib="${STORAGE_LOG_MAX_MIB:-512}"
journal_max="${STORAGE_JOURNAL_MAX_SIZE:-1G}"

delete_old_files() {
  local root="$1"
  local days="$2"
  shift 2
  [[ -d "$root" ]] || return 0
  if [[ "$mode" == "--apply" ]]; then
    find "$root" -xdev -type f "$@" -mtime "+$days" -print -delete
  else
    find "$root" -xdev -type f "$@" -mtime "+$days" -print
  fi
}

delete_old_tree_contents() {
  local root="$1"
  local days="$2"
  [[ -d "$root" ]] || return 0
  if [[ "$mode" == "--apply" ]]; then
    find "$root" -xdev -type f -mtime "+$days" -print -delete
    find "$root" -xdev -mindepth 1 -depth -type d -empty -print -delete
  else
    find "$root" -xdev -type f -mtime "+$days" -print
  fi
}

echo "sandboxed storage cleanup: $mode"

missions_dir="$data_root/missions"
if [[ -d "$missions_dir" ]]; then
  if [[ "$mode" == "--apply" ]]; then
    find "$missions_dir" -xdev -maxdepth 1 -type f \
      \( -name "*.bak.*" -o -name "*.backup-*" \) \
      -mtime "+$backup_days" -print -delete
  else
    find "$missions_dir" -xdev -maxdepth 1 -type f \
      \( -name "*.bak.*" -o -name "*.backup-*" \) \
      -mtime "+$backup_days" -print
  fi
fi

containers_dir="$data_root/containers"
if [[ -d "$containers_dir" ]]; then
  while IFS= read -r -d '' log_dir; do
    delete_old_files "$log_dir" "$log_days"
    while IFS= read -r -d '' oversized; do
      echo "$oversized"
      if [[ "$mode" == "--apply" ]]; then
        truncate -s 0 -- "$oversized"
      fi
    done < <(
      find "$log_dir" -xdev -type f -size "+${log_max_mib}M" -print0
    )
  done < <(
    find "$containers_dir" -xdev -type d \
      -path "*/root/.local/share/opencode/log" -print0
  )
fi

delete_old_tree_contents "$opencode_cache" "$cache_days"
delete_old_tree_contents "$hermes_home/workspace/.quarantine" "$cache_days"
delete_old_tree_contents "$hermes_home/backups/staging" "$backup_days"

if [[ "$mode" == "--apply" ]] && command -v journalctl >/dev/null 2>&1; then
  journalctl "--vacuum-size=$journal_max"
fi

df -h /
