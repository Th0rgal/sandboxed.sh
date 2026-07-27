#!/usr/bin/env bash
set -euo pipefail

proxy_url="${CHATGPT_UI_PROXY_URL:-socks5h://127.0.0.1:10880}"
probe_url="${CHATGPT_UI_PROXY_PROBE_URL:-https://chatgpt.com/}"
failure_file="${CHATGPT_UI_PROXY_FAILURE_FILE:-/run/chatgpt-ui-dgx-proxy-health.failures}"
failure_threshold="${CHATGPT_UI_PROXY_FAILURE_THRESHOLD:-2}"

if curl --silent --show-error --output /dev/null --max-time 15 \
  --proxy "$proxy_url" "$probe_url"; then
  printf '0\n' >"$failure_file"
  exit 0
fi

failures=0
if [[ -r "$failure_file" ]]; then
  read -r failures <"$failure_file" || failures=0
fi
if [[ ! "$failures" =~ ^[0-9]+$ ]]; then
  failures=0
fi
failures=$((failures + 1))
printf '%s\n' "$failures" >"$failure_file"

if (( failures < failure_threshold )); then
  logger -t chatgpt-ui-proxy-health \
    "ChatGPT proxy probe failed (${failures}/${failure_threshold}); preserving the tunnel"
  exit 0
fi

logger -t chatgpt-ui-proxy-health \
  "ChatGPT proxy probe failed ${failures} consecutive times; restarting the tunnel"
systemctl restart chatgpt-ui-dgx-proxy.service
printf '0\n' >"$failure_file"
