#!/bin/bash
# Orb local voice input — macOS (Apple Silicon) installer.
#
# Creates an isolated Python virtualenv at
#   ~/Library/Application Support/Orb/voice/.venv        (override: ORB_VOICE_HOME)
# with mlx-audio pinned to the exact git revision the app was validated
# against, and (optionally) downloads the pinned model snapshot into the
# standard Hugging Face cache. Nothing is installed globally.
#
# Usage:
#   orb/voice/install.sh                     # venv + pinned deps
#   orb/voice/install.sh --download-model    # ...and fetch the model snapshot
#   orb/voice/install.sh --check             # run the protocol smoke test after
#   orb/voice/install.sh --python /opt/homebrew/bin/python3.11
#   orb/voice/install.sh --recreate          # rebuild the venv from scratch
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VOICE_HOME="${ORB_VOICE_HOME:-$HOME/Library/Application Support/Orb/voice}"
VENV="$VOICE_HOME/.venv"
MODEL_REPO="MarkChen1214/cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit"
MODEL_REVISION="553445e84959f9ec3fcd43443bce75ea05c400f3"
PATCH_SHA256="91beeed75b1c6de97c33f30a48ac508bc22eac0689ee792e8204e7dc9ad6313a"

DOWNLOAD=0
CHECK=0
RECREATE=0
PYTHON="${ORB_VOICE_PYTHON:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --download-model) DOWNLOAD=1 ;;
    --check) CHECK=1 ;;
    --recreate) RECREATE=1 ;;
    --python) shift; PYTHON="$1" ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
  echo "Orb voice input needs macOS on Apple Silicon (MLX). Nothing installed." >&2
  exit 1
fi

pick_python() {
  local c
  for c in "$PYTHON" python3.11 /opt/homebrew/bin/python3.11 /opt/homebrew/opt/python@3.11/bin/python3.11 python3.12 /opt/homebrew/bin/python3.12 python3; do
    [ -n "$c" ] || continue
    if command -v "$c" >/dev/null 2>&1; then
      if "$c" -c 'import sys; sys.exit(0 if (3, 10) <= sys.version_info[:2] <= (3, 13) else 1)' 2>/dev/null; then
        command -v "$c"; return 0
      fi
    fi
  done
  return 1
}

if [ "$RECREATE" = 1 ] && [ -d "$VENV" ]; then
  echo "Removing $VENV"
  rm -rf "$VENV"
fi

if [ ! -x "$VENV/bin/python" ]; then
  PY="$(pick_python)" || { echo "No Python 3.10–3.13 found. Install python@3.11 (brew install python@3.11) or pass --python." >&2; exit 1; }
  echo "Creating venv at $VENV with $PY ($("$PY" --version 2>&1))"
  mkdir -p "$VOICE_HOME"
  "$PY" -m venv "$VENV"
else
  echo "Using existing venv at $VENV ($("$VENV/bin/python" --version 2>&1))"
fi

# A venv is not guaranteed to carry pip: `uv venv` (and `python -m venv
# --without-pip`) leave it out, and then `python -m pip` fails with
# "No module named pip". Bootstrap it with ensurepip, or fall back to uv
# driving this interpreter; only give up when neither works.
INSTALLER=""
if "$VENV/bin/python" -m pip --version >/dev/null 2>&1; then
  INSTALLER=pip
elif "$VENV/bin/python" -m ensurepip --upgrade >/dev/null 2>&1 && "$VENV/bin/python" -m pip --version >/dev/null 2>&1; then
  echo "Bootstrapped pip into the venv with ensurepip"
  INSTALLER=pip
elif command -v uv >/dev/null 2>&1; then
  echo "The venv has no pip and ensurepip is unavailable; installing with uv"
  INSTALLER=uv
else
  echo "The venv at $VENV has no pip and it cannot be bootstrapped (no ensurepip, no uv). Rerun with --recreate or install uv." >&2
  exit 1
fi

install_requirements() {
  if [ "$INSTALLER" = uv ]; then
    uv pip install --quiet --python "$VENV/bin/python" -r "$HERE/requirements.txt"
  else
    "$VENV/bin/python" -m pip install --quiet --upgrade pip
    "$VENV/bin/python" -m pip install --quiet --no-cache-dir -r "$HERE/requirements.txt"
  fi
}

echo "Installing pinned dependencies with $INSTALLER (this pulls mlx-audio from git; needs git + Xcode CLT)"
install_requirements
"$VENV/bin/python" - <<'PY'
import importlib.metadata as m
import mlx.core as mx
print(f"  mlx {m.version('mlx')} · mlx-audio {m.version('mlx-audio')} · numpy {m.version('numpy')} · metal={mx.metal.is_available()}")
PY

if [ "$DOWNLOAD" = 1 ]; then
  echo "Downloading model snapshot $MODEL_REPO@$MODEL_REVISION into the Hugging Face cache"
  "$VENV/bin/python" - "$MODEL_REPO" "$MODEL_REVISION" <<'PY'
import sys
from huggingface_hub import snapshot_download
path = snapshot_download(sys.argv[1], revision=sys.argv[2])
print(f"  snapshot: {path}")
PY
fi

MODEL_DIR="$("$VENV/bin/python" -c "import sys; sys.path.insert(0, '$HERE'); import worker; print(worker.resolve_model_dir())")"
if [ -f "$MODEL_DIR/model.safetensors" ]; then
  echo "Model snapshot present: $MODEL_DIR"
  if [ -f "$MODEL_DIR/mlx_audio_cohere_quant_patch.py" ]; then
    GOT="$(shasum -a 256 "$MODEL_DIR/mlx_audio_cohere_quant_patch.py" | cut -d' ' -f1)"
    if [ "$GOT" = "$PATCH_SHA256" ]; then
      echo "Loader patch in snapshot matches the vendored copy (sha256 ok)"
    else
      echo "WARNING: snapshot patch sha256 $GOT differs from vendored $PATCH_SHA256; the worker uses the vendored copy" >&2
    fi
  fi
else
  echo "Model snapshot missing at $MODEL_DIR — rerun with --download-model" >&2
fi

if [ "$CHECK" = 1 ]; then
  echo "Running protocol smoke test"
  "$VENV/bin/python" "$HERE/smoke.py" --python "$VENV/bin/python"
fi

echo "Done. Paths:"
echo "  venv:   $VENV"
echo "  model:  $MODEL_DIR"
echo "  logs:   $VOICE_HOME/logs/worker.log (written when Orb starts the worker)"
