#!/bin/bash

# Xcode Cloud post-clone script
# This script runs after the repository is cloned but before the build starts
# It installs XcodeGen and generates the Xcode project from project.yml

set -euo pipefail

echo "=== Installing XcodeGen ==="
if command -v xcodegen >/dev/null 2>&1; then
  echo "XcodeGen already installed: $(xcodegen --version)"
else
  XCODEGEN_VERSION="${XCODEGEN_VERSION:-2.41.0}"
  XCODEGEN_URL="https://github.com/yonaskolb/XcodeGen/releases/download/${XCODEGEN_VERSION}/XcodeGen-${XCODEGEN_VERSION}.zip"
  XCODEGEN_BIN_DIR="${HOME}/.local/bin"

  echo "Downloading XcodeGen ${XCODEGEN_VERSION} from GitHub releases..."
  mkdir -p "${XCODEGEN_BIN_DIR}"

  TMP_DIR="$(mktemp -d)"
  cleanup() {
    rm -rf "${TMP_DIR}"
  }
  trap cleanup EXIT

  if curl -fsSL --retry 3 --retry-delay 2 -o "${TMP_DIR}/xcodegen.zip" "${XCODEGEN_URL}"; then
    unzip -q "${TMP_DIR}/xcodegen.zip" -d "${TMP_DIR}"
    chmod +x "${TMP_DIR}/xcodegen"
    mv "${TMP_DIR}/xcodegen" "${XCODEGEN_BIN_DIR}/xcodegen"
    export PATH="${XCODEGEN_BIN_DIR}:${PATH}"
    echo "XcodeGen installed to ${XCODEGEN_BIN_DIR}"
  else
    echo "Failed to download XcodeGen release. Falling back to Homebrew."
    # Avoid Homebrew auto-update (which can fail on ghcr.io in Xcode Cloud).
    export HOMEBREW_NO_AUTO_UPDATE=1
    export HOMEBREW_NO_ENV_HINTS=1
    export HOMEBREW_NO_INSTALL_FROM_API=1
    brew install xcodegen
  fi
fi

echo "=== Generating Xcode Project ==="
cd "$CI_PRIMARY_REPOSITORY_PATH/ios_dashboard"
xcodegen generate

echo "=== Project generated successfully ==="
ls -la *.xcodeproj
