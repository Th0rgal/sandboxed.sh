#!/usr/bin/env bash
set -euo pipefail

echo "== OpenCode service environment =="
if command -v systemctl >/dev/null 2>&1; then
  systemctl show opencode.service -p Environment || true
else
  echo "systemctl not available"
fi

OPENCODE_CONFIG_DIR=""
if [ -f /etc/open_agent/open_agent.env ]; then
  OPENCODE_CONFIG_DIR=$(grep -E '^OPENCODE_CONFIG_DIR=' /etc/open_agent/open_agent.env | tail -n1 | cut -d= -f2- || true)
fi

if [ -n "$OPENCODE_CONFIG_DIR" ]; then
  OPENCODE_HOME="$(cd "$(dirname "$OPENCODE_CONFIG_DIR")/.." && pwd -P)"
  echo "OpenCode home (derived): $OPENCODE_HOME"
else
  OPENCODE_HOME="${OPENCODE_HOME:-/var/lib/opencode}"
  echo "OpenCode home (default): $OPENCODE_HOME"
fi

echo "== Global skill directories =="
for path in \
  "/root/.opencode/skill" \
  "/root/.config/opencode/skill" \
  "${OPENCODE_HOME}/.opencode/skill" \
  "${OPENCODE_HOME}/.config/opencode/skill"
do
  if [ -d "$path" ]; then
    count=$(find "$path" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')
    echo "$path -> $count skill dir(s)"
  else
    echo "$path -> (missing)"
  fi
done

latest=$(find /root/.sandboxed-sh -type d -name 'mission-*' -path '*workspaces*' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n1 | cut -d' ' -f2- || true)
if [ -n "$latest" ]; then
  echo "== Latest mission dir =="
  echo "$latest"
  if [ -d "$latest/.opencode/skill" ]; then
    echo "Skills in latest mission:"
    ls -1 "$latest/.opencode/skill"
  else
    echo "No .opencode/skill in latest mission dir"
  fi
  if [ -f "$latest/.opencode/opencode.json" ]; then
    echo "Permission section in latest mission opencode.json:"
    grep -n "permission" "$latest/.opencode/opencode.json" || true
  fi
else
  echo "No mission directories found under /root/.sandboxed-sh"
fi
