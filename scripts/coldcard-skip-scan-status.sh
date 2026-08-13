#!/bin/bash
# Machine-readable Coldcard skip-scan status on DGX Spark.
#
# Controllers MUST call this instead of inventing a local `pgrep`.
# 2026-08-13: the watch ran `pgrep coldcard_skip` on agent-core and reported
# DEAD. The 2.75B `scan.log` is the older generic pass; skip-aware progress
# is `coldcard_skip.log`. Pass `--monitor` for a coarse signature
# (status+matches+100M bucket) so Hermes does not wake every pad.
set -euo pipefail

KEY="${COLDCARD_DGX_SSH_KEY:-/root/.ssh/agent-core-dgx-tunnel}"
TARGET="${COLDCARD_DGX_SSH:-th0rgal@100.77.4.93}"

if [[ ! -f "$KEY" ]]; then
  echo "STATUS=UNKNOWN"
  echo "REASON=missing_ssh_key"
  echo "KEY=$KEY"
  exit 0
fi

remote_out=$(
  ssh -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=yes "$TARGET" 'bash -s' <<'REMOTE'
set +e
status=DEAD
if pgrep -x coldcard_skip >/dev/null 2>&1; then
  status=LIVE
elif pgrep -f '/tmp/coldcard/coldcard_skip( |$)' >/dev/null 2>&1; then
  status=LIVE
fi

# Skip-aware progress is coldcard_skip.log. scan.log is the older generic
# 2^32 pass — a higher pad there is not this campaign.
log=""
if [ -f /tmp/coldcard/coldcard_skip.log ]; then
  log=/tmp/coldcard/coldcard_skip.log
elif [ -f /tmp/coldcard/scan.log ]; then
  log=/tmp/coldcard/scan.log
fi

last=""
age=999999
if [ -n "$log" ]; then
  last=$(tail -1 "$log" 2>/dev/null)
  now=$(date +%s)
  mt=$(stat -c %Y "$log" 2>/dev/null || echo 0)
  age=$((now - mt))
fi

# A fresh log is liveness even if pgrep missed the binary name.
if [ "$status" = DEAD ] && [ "$age" -lt 120 ]; then
  status=LIVE
  reason=log_heartbeat
else
  reason=ok
fi

hits=""
if [ -s /tmp/coldcard/scan_results.txt ]; then
  status=HIT
  hits=$(head -c 400 /tmp/coldcard/scan_results.txt | tr "\n" " ")
fi

case "$last" in
  *SCAN\ COMPLETE*) status=COMPLETE ;;
esac

pad=""
total=""
matches=""
rate=""
if printf "%s" "$last" | grep -Eq "\[[0-9]+/[0-9]+\]"; then
  pad=$(printf "%s" "$last" | sed -n "s/.*\[\([0-9][0-9]*\)\/\([0-9][0-9]*\)\].*/\1/p")
  total=$(printf "%s" "$last" | sed -n "s/.*\[\([0-9][0-9]*\)\/\([0-9][0-9]*\)\].*/\2/p")
fi
if printf "%s" "$last" | grep -q "matches="; then
  matches=$(printf "%s" "$last" | sed -n "s/.*matches=\([0-9][0-9]*\).*/\1/p")
fi
if printf "%s" "$last" | grep -q "rate="; then
  rate=$(printf "%s" "$last" | sed -n "s/.*rate=\([^ ]*\).*/\1/p")
fi

printf "STATUS=%s\n" "$status"
printf "REASON=%s\n" "$reason"
printf "LOG=%s\n" "$log"
printf "LOG_AGE_SECS=%s\n" "$age"
printf "PAD=%s\n" "$pad"
printf "TOTAL=%s\n" "$total"
printf "MATCHES=%s\n" "$matches"
printf "RATE=%s\n" "$rate"
printf "LAST=%s\n" "$last"
if [ -n "$hits" ]; then
  printf "HITS=%s\n" "$hits"
fi
if [ "$status" = DEAD ]; then
  printf "RESTART=cd /tmp/coldcard && python3 -c 'import os,subprocess; os.chdir(\"/tmp/coldcard\"); subprocess.Popen([\"./coldcard_skip\",\"0\",\"4294967295\",\"65536\",\"3103\",\"3161\"], stdout=open(\"coldcard_skip.log\",\"a\"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)'\n"
fi
REMOTE
) || {
  echo "STATUS=UNKNOWN"
  echo "REASON=ssh_failed"
  exit 0
}

if [[ "${1:-}" == "--monitor" ]]; then
  status=$(printf "%s\n" "$remote_out" | awk -F= '/^STATUS=/{print $2; exit}')
  matches=$(printf "%s\n" "$remote_out" | awk -F= '/^MATCHES=/{print $2; exit}')
  pad=$(printf "%s\n" "$remote_out" | awk -F= '/^PAD=/{print $2; exit}')
  bucket=$((${pad:-0} / 100000000))
  printf "%s matches=%s bucket=%s\n" "${status:-UNKNOWN}" "${matches:-0}" "$bucket"
  exit 0
fi
printf "%s\n" "$remote_out"
