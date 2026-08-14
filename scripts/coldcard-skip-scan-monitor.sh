#!/bin/bash
# Hermes `--monitor-script` is a path only — it does not split arguments.
# A value of `coldcard-skip-scan-status.sh --monitor` is looked up as one
# filename and the watch dies with "Script not found" (2026-08-14).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
exec "$here/coldcard-skip-scan-status.sh" --monitor
