#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Generate an Android release signing key and upload GitHub Actions secrets.

Usage:
  scripts/setup_android_release_secrets.sh [options]

Options:
  --repo OWNER/REPO      GitHub repository. Defaults to `gh repo view` in this repo.
  --keystore PATH       Local keystore path. Defaults to android_dashboard/keys/release.jks.
  --alias NAME          Key alias. Defaults to sandboxed.
  --dname NAME          X.509 distinguished name for the signing key.
  --validity DAYS       Key validity in days. Defaults to 9125.
  --force               Overwrite an existing local keystore and env backup.
  -h, --help            Show this help.

Required tools:
  gh, git, keytool, openssl, base64

The script sets these GitHub Actions secrets:
  ANDROID_RELEASE_KEYSTORE_BASE64
  ANDROID_RELEASE_KEYSTORE_PASSWORD
  ANDROID_RELEASE_KEY_ALIAS
  ANDROID_RELEASE_KEY_PASSWORD

It also writes a local backup to android_dashboard/keys/release-secrets.env.
Keep both the keystore and backup file private. They are ignored by git.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required but was not found in PATH"
}

shell_quote() {
  printf "%q" "$1"
}

repo=""
keystore=""
key_alias="sandboxed"
dname="CN=Sandboxed.sh Android Release, OU=Android, O=Sandboxed.sh, L=Unknown, ST=Unknown, C=US"
validity_days="9125"
force="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      [[ $# -ge 2 ]] || die "--repo requires OWNER/REPO"
      repo="$2"
      shift 2
      ;;
    --keystore)
      [[ $# -ge 2 ]] || die "--keystore requires a path"
      keystore="$2"
      shift 2
      ;;
    --alias)
      [[ $# -ge 2 ]] || die "--alias requires a value"
      key_alias="$2"
      shift 2
      ;;
    --dname)
      [[ $# -ge 2 ]] || die "--dname requires a value"
      dname="$2"
      shift 2
      ;;
    --validity)
      [[ $# -ge 2 ]] || die "--validity requires a number of days"
      validity_days="$2"
      shift 2
      ;;
    --force)
      force="true"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

[[ "${validity_days}" =~ ^[0-9]+$ ]] || die "--validity must be a positive integer"

require_tool gh
require_tool git
require_tool keytool
require_tool openssl
require_tool base64

repo_root="$(git rev-parse --show-toplevel)"
cd "${repo_root}"

if [[ -z "${repo}" ]]; then
  repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
fi
[[ -n "${repo}" ]] || die "could not determine GitHub repository; pass --repo OWNER/REPO"

if [[ -z "${keystore}" ]]; then
  keystore="android_dashboard/keys/release.jks"
fi

keystore_dir="$(dirname "${keystore}")"
backup_env="${keystore_dir}/release-secrets.env"

if [[ -e "${keystore}" && "${force}" != "true" ]]; then
  die "${keystore} already exists. Refusing to replace an app signing key; pass --force only if this is intentional."
fi
if [[ -e "${backup_env}" && "${force}" != "true" ]]; then
  die "${backup_env} already exists. Refusing to overwrite local signing backup; pass --force only if this is intentional."
fi

echo "Checking GitHub CLI authentication..."
gh auth status >/dev/null

echo "Checking repository access for ${repo}..."
gh repo view "${repo}" >/dev/null

mkdir -p "${keystore_dir}"
chmod 700 "${keystore_dir}"

store_password="$(openssl rand -base64 32 | tr -d '\n')"
key_password="$(openssl rand -base64 32 | tr -d '\n')"

echo "Generating Android release keystore at ${keystore}..."
tmp_keystore="$(mktemp "${keystore_dir}/.release.jks.XXXXXX")"
rm -f "${tmp_keystore}"
cleanup() {
  rm -f "${tmp_keystore}"
}
trap cleanup EXIT

keytool -genkeypair -v \
  -keystore "${tmp_keystore}" \
  -storepass "${store_password}" \
  -keypass "${key_password}" \
  -alias "${key_alias}" \
  -keyalg RSA \
  -keysize 2048 \
  -validity "${validity_days}" \
  -dname "${dname}" \
  -storetype JKS

mv -f "${tmp_keystore}" "${keystore}"
chmod 600 "${keystore}"

cat > "${backup_env}" <<EOF
# Local backup for the Android release signing key.
# Keep this file private. It is ignored by git.
export RELEASE_KEYSTORE=$(shell_quote "$(cd "$(dirname "${keystore}")" && pwd)/$(basename "${keystore}")")
export RELEASE_KEYSTORE_PASSWORD=$(shell_quote "${store_password}")
export RELEASE_KEY_ALIAS=$(shell_quote "${key_alias}")
export RELEASE_KEY_PASSWORD=$(shell_quote "${key_password}")
EOF
chmod 600 "${backup_env}"

keystore_base64="$(base64 < "${keystore}" | tr -d '\n')"

echo "Uploading GitHub Actions secrets to ${repo}..."
printf '%s' "${keystore_base64}" | gh secret set ANDROID_RELEASE_KEYSTORE_BASE64 --repo "${repo}"
printf '%s' "${store_password}" | gh secret set ANDROID_RELEASE_KEYSTORE_PASSWORD --repo "${repo}"
printf '%s' "${key_alias}" | gh secret set ANDROID_RELEASE_KEY_ALIAS --repo "${repo}"
printf '%s' "${key_password}" | gh secret set ANDROID_RELEASE_KEY_PASSWORD --repo "${repo}"

echo
echo "Done."
echo "GitHub secrets were written to ${repo}."
echo "Local keystore: ${keystore}"
echo "Local backup env: ${backup_env}"
echo
echo "To build a signed release locally:"
echo "  source ${backup_env}"
echo "  cd android_dashboard && ./gradlew :app:assembleRelease"
