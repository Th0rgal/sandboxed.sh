#!/usr/bin/env bash
set -euo pipefail

# Reproducible production template for the dedicated Verity workspace. Keep
# every external checkout pinned: a template rebuild must not silently change
# Lean, the MCP server, or the agent guidance underneath an active roadmap.
readonly VERITY_REF="538c4a9ce2baa25b56062bdc727eb0191ad9e67f"
readonly LEAN_LSP_MCP_REF="83b0286574ac46101567f2971c29d55abb2483f9"
readonly LEAN4_SKILLS_REF="5a331e22c7f9416ef40594e2f076f91306c78b16"
readonly UV_VERSION="0.11.28"
readonly ROOT="/workspace/verity"

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  build-essential ca-certificates ccache cmake curl git gnupg jq libffi-dev \
  libgmp-dev ninja-build pkg-config python3 python3-venv ripgrep zlib1g-dev

if ! command -v gh >/dev/null 2>&1; then
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
    -o /etc/apt/keyrings/githubcli-archive-keyring.gpg
  chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
  printf '%s\n' \
    "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
    > /etc/apt/sources.list.d/github-cli.list
  apt-get update -qq
  apt-get install -y --no-install-recommends gh
fi

if ! command -v uv >/dev/null 2>&1; then
  curl -LsSf https://astral.sh/uv/${UV_VERSION}/install.sh \
    | env UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh
fi
test "$(uv --version | awk '{print $2}')" = "$UV_VERSION"

mkdir -p "$ROOT"/{base,bin,tools,worktrees}

clone_pinned() {
  local url="$1" dest="$2" ref="$3"
  if [[ ! -d "$dest/.git" ]]; then
    rm -rf "$dest"
    git clone "$url" "$dest"
  fi
  git -C "$dest" fetch --tags origin
  git -C "$dest" checkout --detach "$ref"
  test "$(git -C "$dest" rev-parse HEAD)" = "$ref"
}

clone_pinned https://github.com/lfglabs-dev/verity.git "$ROOT/base" "$VERITY_REF"
clone_pinned https://github.com/oOo0oOo/lean-lsp-mcp.git \
  "$ROOT/tools/lean-lsp-mcp" "$LEAN_LSP_MCP_REF"
clone_pinned https://github.com/cameronfreer/lean4-skills.git \
  "$ROOT/tools/lean4-skills" "$LEAN4_SKILLS_REF"

export ELAN_HOME="$ROOT/.elan"
if [[ ! -x "$ELAN_HOME/bin/elan" ]]; then
  curl --proto '=https' --tlsv1.2 -sSf \
    https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh \
    | sh -s -- -y --default-toolchain none
fi
export UV_TOOL_DIR="$ROOT/.uv-tools"
export UV_TOOL_BIN_DIR="$ROOT/bin"
export PATH="$ELAN_HOME/bin:$UV_TOOL_BIN_DIR:$PATH"

elan toolchain install "$(<"$ROOT/base/lean-toolchain")"
elan default "$(<"$ROOT/base/lean-toolchain")"
uv tool install --force --from "$ROOT/tools/lean-lsp-mcp" lean-lsp-mcp

# The MCP registry is shared across workspaces, so its command path must be
# stable while project/tool locations come from each workspace's explicit
# non-secret allowlist.
install -m 0755 /dev/stdin /usr/local/bin/lean-lsp-mcp-workspace <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
: "${LEAN_PROJECT_PATH:?LEAN_PROJECT_PATH is required}"
exec lean-lsp-mcp --lean-project-path "$LEAN_PROJECT_PATH" "$@"
SCRIPT

# Warm and validate the canonical checkout. Workers never use this mutable
# `.lake` tree: verity-isolated-clone below creates a fresh package graph for
# every mission. This deliberately chooses complete isolation over hardlinks,
# shared worktree caches, or partially copied package repositories.
(
  cd "$ROOT/base"
  lake exe cache get
  lake build
)

install -m 0755 /dev/stdin /usr/local/bin/verity-isolated-clone <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
readonly ROOT="/workspace/verity"
name="${1:?usage: verity-isolated-clone <name> [git-ref] [branch]}"
ref="${2:-origin/main}"
branch="${3:-}"
if [[ ! "$name" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "invalid clone name: $name" >&2
  exit 2
fi
dest="$ROOT/worktrees/$name"
if [[ -e "$dest" ]]; then
  echo "destination already exists: $dest" >&2
  exit 3
fi

# --dissociate shares download work only. Git objects and the complete `.lake`
# package tree are private to this mission after the command returns.
git clone --reference-if-able "$ROOT/base" --dissociate \
  https://github.com/lfglabs-dev/verity.git "$dest"
git -C "$dest" fetch origin "$ref"
if [[ -n "$branch" ]]; then
  git -C "$dest" checkout -B "$branch" FETCH_HEAD
else
  git -C "$dest" checkout --detach FETCH_HEAD
fi
rm -rf "$dest/.lake"
(
  cd "$dest"
  lake exe cache get
  mkdir -p .lake
  printf '%s\n' "$name" > .lake/sandboxed-cache-owner
  test -d .lake/packages
  test ! -L .lake/packages
)
printf '%s\n' "$dest"
SCRIPT

install -m 0755 /dev/stdin /usr/local/bin/verity-audit <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
repo="${1:-$PWD}"
cd "$repo"
printf 'head=%s\n' "$(git rev-parse HEAD)"
printf 'toolchain=%s\n' "$(<lean-toolchain)"
lean --version
lake --version
gh auth status --hostname github.com >/dev/null
python3 scripts/check_paths.py
git status --short
SCRIPT

# Fail template creation rather than registering a workspace with half-working
# proof tooling.
test "$(<"$ROOT/base/lean-toolchain")" = "leanprover/lean4:v4.24.0"
lean --version | grep -F 'version 4.24.0'
lake --version
lean-lsp-mcp --help >/dev/null
LEAN_PROJECT_PATH="$ROOT/base" /usr/local/bin/lean-lsp-mcp-workspace --help >/dev/null
gh --version >/dev/null
test "$(git -C "$ROOT/base" rev-parse HEAD)" = "$VERITY_REF"
