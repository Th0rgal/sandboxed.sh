#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 ABSOLUTE_PROFILE_DIR [MODEL_LABEL] [SAFE_RESULT_JSON]" >&2
  exit 2
fi

profile_dir=$1
model=${2:-}
artifact=${3:-}
python_bin=${CHATGPT_UI_PYTHON:-python3}
driver=${CHATGPT_UI_DRIVER:-"$(dirname "$0")/chatgpt_ui_driver.py"}
if [[ $profile_dir != /* || ! -d $profile_dir ]]; then
  echo "profile directory must be an existing absolute path" >&2
  exit 2
fi
if [[ $python_bin == */* ]]; then
  if [[ ! -x $python_bin ]]; then
    echo "ChatGPT UI Python executable not found: $python_bin" >&2
    exit 2
  fi
elif ! command -v "$python_bin" >/dev/null 2>&1; then
  echo "ChatGPT UI Python executable not found in PATH: $python_bin" >&2
  exit 2
fi
if [[ ! -f $driver ]]; then
  echo "ChatGPT UI driver not found: $driver" >&2
  exit 2
fi

printf '{"type":"run","message":"Reply with exactly: SANDBOXED_CHATGPT_UI_SMOKE_OK","model":%s,"timeout_ms":120000}\n' \
  "$("$python_bin" -c 'import json,sys; print(json.dumps(sys.argv[1] or None))' "$model")" |
  "$python_bin" "$driver" \
    --profile-dir "$profile_dir" --browser chromium --headless true |
  "$python_bin" -c '
import json, pathlib, sys
events = [json.loads(line) for line in sys.stdin if line.strip()]
complete = next((event for event in reversed(events) if event.get("type") == "complete"), None)
ok = bool(complete and complete.get("content", "").strip() == "SANDBOXED_CHATGPT_UI_SMOKE_OK")
summary = {"ok": ok, "complete_signal": complete is not None, "event_types": [event.get("type") for event in events]}
encoded = json.dumps(summary, separators=(",", ":")) + "\n"
artifact = sys.argv[1]
if artifact:
    path = pathlib.Path(artifact)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(encoded, encoding="utf-8")
sys.stdout.write(encoded)
raise SystemExit(0 if ok else 1)
' "$artifact"
