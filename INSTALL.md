# Installing Open Agent (Ubuntu 24.04, dedicated server)

This is the installation approach currently used on a **dedicated Ubuntu 24.04 server** (OpenCode + Open Agent running on the same machine, managed by `systemd`).

Open Agent is the orchestrator/UI backend. **It does not run model inference**; it delegates execution to an **OpenCode server** running locally (default `http://127.0.0.1:4096`).

---

## 0) Assumptions

- Ubuntu 24.04 LTS, root SSH access
- A dedicated server (not shared hosting)
- You want:
  - OpenCode server bound to localhost: `127.0.0.1:4096`
  - Open Agent bound to: `0.0.0.0:3000`
- You have a Git repo for your **Library** (skills/tools/agents/rules/MCP configs)

---

## 1) Install base OS dependencies

```bash
apt update
apt install -y \
  ca-certificates curl git jq unzip tar \
  build-essential pkg-config libssl-dev
```

If you plan to use container workspaces (systemd-nspawn), also install:

```bash
apt install -y systemd-container debootstrap
```

If you plan to use **desktop automation** tools (Xvfb/i3/Chromium screenshots/OCR), install:

```bash
apt install -y xvfb i3 x11-utils xdotool scrot imagemagick chromium chromium-sandbox tesseract-ocr
```

See `docs/DESKTOP_SETUP.md` for a full checklist and i3 config recommendations.

---

## 2) Install Bun (for bunx + Playwright MCP)

OpenCode is distributed as a binary, but:
- OpenCode plugins are installed internally via Bun
- Open Agent’s default Playwright MCP runner prefers `bunx`

Install Bun:

```bash
curl -fsSL https://bun.sh/install | bash

# Make bun/bunx available to systemd services
install -m 0755 /root/.bun/bin/bun /usr/local/bin/bun
install -m 0755 /root/.bun/bin/bunx /usr/local/bin/bunx

bun --version
bunx --version
```

---

## 3) Install OpenCode (server backend)

### 3.1 Install/Update the OpenCode binary

This installs the latest release into `~/.opencode/bin/opencode`:

```bash
curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path
```

Optional: pin a version (recommended for servers):

```bash
curl -fsSL https://opencode.ai/install | bash -s -- --version 1.1.8 --no-modify-path
```

Copy the binary into a stable system location used by `systemd`:

```bash
install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode
opencode --version
```

### 3.2 Create `systemd` unit for OpenCode

Create `/etc/systemd/system/opencode.service`:

```ini
[Unit]
Description=OpenCode Server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/opencode serve --port 4096 --hostname 127.0.0.1
WorkingDirectory=/root
Restart=always
RestartSec=10
Environment=HOME=/root

[Install]
WantedBy=multi-user.target
```

Enable + start:

```bash
systemctl daemon-reload
systemctl enable --now opencode.service
```

Test:

```bash
curl -fsSL http://127.0.0.1:4096/global/health | jq .
```

Note: Open Agent will also keep OpenCode's global config updated (MCP + tool allowlist) in:
`~/.config/opencode/opencode.json`.

### 3.2.1 Strong workspace skill isolation (recommended)

OpenCode discovers skills from global locations (e.g. `~/.opencode/skill`, `~/.config/opencode/skill`)
*and* from the project/mission directory `.opencode/skill`. To guarantee **per‑workspace** skill usage,
run OpenCode with an isolated HOME and keep global skill dirs empty.

1) Create an isolated OpenCode home:

```bash
mkdir -p /var/lib/opencode
```

2) Update `opencode.service` to use the isolated home:

```ini
Environment=HOME=/var/lib/opencode
Environment=XDG_CONFIG_HOME=/var/lib/opencode/.config
Environment=XDG_DATA_HOME=/var/lib/opencode/.local/share
Environment=XDG_CACHE_HOME=/var/lib/opencode/.cache
```

3) Point Open Agent at the same OpenCode config dir (see section 6):

```
OPENCODE_CONFIG_DIR=/var/lib/opencode/.config/opencode
```

4) Move any old global skills out of the way (optional but recommended):

```bash
mv /root/.opencode/skill /root/.opencode/skill.bak-$(date +%F) 2>/dev/null || true
mv /root/.config/opencode/skill /root/.config/opencode/skill.bak-$(date +%F) 2>/dev/null || true
```

5) Reload services:

```bash
systemctl daemon-reload
systemctl restart opencode.service
systemctl restart open_agent.service
```

Validation (on the server, from the repo root):

```bash
scripts/validate_skill_isolation.sh
```

### 3.3 Install oh-my-opencode (agent pack)

Install the default agent pack as root:

```bash
bunx oh-my-opencode install --no-tui
```

This installs the **Sisyphus** default agent (plus other personalities). To preserve plugin defaults:
Leave the Open Agent agent/model overrides unset to use the OpenCode / oh-my-opencode defaults.

Update strategy:
- Pin a version in your Library `plugins.json` (e.g., `oh-my-opencode@1.2.3`) to lock updates.
- Otherwise, the plugin can auto-update via OpenCode's install hook and Open Agent sync.

### 3.4 Install opencode-gemini-auth (optional, for Google OAuth)

If you want to authenticate with Google accounts (Gemini plans/quotas including free tier) via OAuth instead of API keys:

```bash
bunx opencode-gemini-auth install
```

This enables OAuth-based Google authentication, allowing users to leverage their existing Gemini plan directly within OpenCode. Features include:
- OAuth flow with Google accounts
- Automatic Cloud project provisioning
- Support for thinking capabilities (Gemini 2.5/3)

To authenticate via CLI (useful for testing):

```bash
opencode auth login
# Select Google provider, then "OAuth with Google (Gemini CLI)"
```

For dashboard OAuth integration, see the Settings page which handles this flow via the API.

---

## 4) Install Open Agent (Rust backend)

### 4.1 Install Rust toolchain

```bash
curl -fsSL https://sh.rustup.rs | sh -s -- -y
source /root/.cargo/env
rustc --version
cargo --version
```

### 4.2 Deploy the repository

On the server we keep the repo under:

```bash
mkdir -p /opt/open_agent
cd /opt/open_agent
git clone <YOUR_OPEN_AGENT_REPO_URL> vaduz-v1
```

If you develop locally, a fast deploy loop is to `rsync` source to the server and build on the server (debug builds are much faster):

```bash
rsync -az --delete \
  --exclude target --exclude .git --exclude dashboard/node_modules \
  -e "ssh -i ~/.ssh/cursor" \
  /path/to/vaduz-v1/ \
  root@<server-ip>:/opt/open_agent/vaduz-v1/
```

### 4.3 Build and install binaries

```bash
cd /opt/open_agent/vaduz-v1
source /root/.cargo/env

# Debug build (fast) - recommended for rapid iteration
cargo build --bin open_agent --bin host-mcp --bin desktop-mcp
install -m 0755 target/debug/open_agent /usr/local/bin/open_agent
install -m 0755 target/debug/host-mcp /usr/local/bin/host-mcp
install -m 0755 target/debug/desktop-mcp /usr/local/bin/desktop-mcp

# Or: Release build (slower compile, faster runtime)
# cargo build --release --bin open_agent --bin host-mcp --bin desktop-mcp
# install -m 0755 target/release/open_agent /usr/local/bin/open_agent
# install -m 0755 target/release/host-mcp /usr/local/bin/host-mcp
# install -m 0755 target/release/desktop-mcp /usr/local/bin/desktop-mcp
```

---

## 5) Bootstrap the Library (config repo)

Open Agent expects a git-backed **Library** repo. At runtime it will:
- clone it into `LIBRARY_PATH` (default: `{WORKING_DIR}/.openagent/library`)
- ensure the `origin` remote matches `LIBRARY_REMOTE`
- pull/sync as needed

### 5.1 Create your own library repo from the template

Template:
- https://github.com/Th0rgal/openagent-library-template

One way to bootstrap:

```bash
# On your machine
git clone git@github.com:Th0rgal/openagent-library-template.git openagent-library
cd openagent-library

# Point it at your own repo
git remote set-url origin git@github.com:<your-org>/<your-library-repo>.git

# Push to your remote (choose main/master as you prefer)
git push -u origin HEAD:main
```

### 5.2 Configure Open Agent to use it

Set in `/etc/open_agent/open_agent.env`:
- `LIBRARY_REMOTE=git@github.com:<your-org>/<your-library-repo>.git`
- optional: `LIBRARY_PATH=/root/.openagent/library`

---

## 6) Configure Open Agent (env file)

Create `/etc/open_agent/open_agent.env`:

```bash
mkdir -p /etc/open_agent
chmod 700 /etc/open_agent
```

Example (fill in your real values):

```bash
cat > /etc/open_agent/open_agent.env <<'EOF'
# OpenCode backend (must match opencode.service)
OPENCODE_BASE_URL=http://127.0.0.1:4096
OPENCODE_PERMISSIVE=true
# Optional: keep Open Agent writing OpenCode global config into the isolated home
# (recommended if you enabled strong workspace skill isolation in section 3.2.1).
# OPENCODE_CONFIG_DIR=/var/lib/opencode/.config/opencode

# Server bind
HOST=0.0.0.0
PORT=3000

# Default filesystem root for Open Agent (agent still has full system access)
WORKING_DIR=/root
LIBRARY_PATH=/root/.openagent/library
LIBRARY_REMOTE=git@github.com:<your-org>/<your-library-repo>.git

# Auth (set DEV_MODE=false on real deployments)
DEV_MODE=false
DASHBOARD_PASSWORD=change-me
JWT_SECRET=change-me-to-a-long-random-string
JWT_TTL_DAYS=30

# Dashboard Console (local shell)
# No SSH configuration required.

# Default model (provider/model). If omitted or not in provider/model format,
# Open Agent won’t force a model and OpenCode will use its own defaults.

# Desktop tools (optional)
DESKTOP_ENABLED=true
DESKTOP_RESOLUTION=1920x1080
EOF
```

---

## 7) Create `systemd` unit for Open Agent

Create `/etc/systemd/system/open_agent.service`:

```ini
[Unit]
Description=OpenAgent (managed control plane)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
Group=root
EnvironmentFile=/etc/open_agent/open_agent.env
WorkingDirectory=/root
ExecStart=/usr/local/bin/open_agent
Restart=on-failure
RestartSec=2

# Agent needs full system access, minimal hardening
NoNewPrivileges=false
PrivateTmp=false
ProtectHome=false

[Install]
WantedBy=multi-user.target
```

---

## 8) Optional: Tailscale exit-node workspaces (residential IP)

If you want a **workspace** to egress via a residential IP, the recommended pattern is:
1) Run a Tailscale **exit node** at home.
2) Use a workspace template that installs and starts Tailscale inside the container.

### 8.1 Enable the exit node at home
On the home server:

```bash
tailscale up --advertise-exit-node
```

Approve it in the Tailscale admin console (Machines → your node → “Approve exit node”).

### 8.2 Use the `residential` workspace template
This repo ships a sample template at:

```
library-template/workspace-template/residential.json
```

It installs Tailscale and adds helper scripts:
- `openagent-network-up` (brings up host0 veth + DHCP + DNS)
- `openagent-tailscale-up` (starts tailscaled + sets exit node)
- `openagent-tailscale-check` (prints Tailscale status + public IP)

Set these **workspace env vars** (not global env):
- `TS_AUTHKEY` (auth key for that workspace)
- `TS_EXIT_NODE` (node name like `umbrel` or its 100.x IP)
- Optional: `TS_ACCEPT_DNS=true|false`, `TS_EXIT_NODE_ALLOW_LAN=false`, `TS_STATE_DIR=/var/lib/tailscale`

Then inside the workspace:

```bash
openagent-tailscale-up
openagent-tailscale-check
```

If the public IP matches your home ISP, the exit node is working.

### 8.3 Host NAT for veth networking (required)
`systemd-nspawn --network-veth` needs DHCP + NAT on the host. Without this, containers
won’t reach the internet or Tailscale control plane.

Create an override for `ve-*` interfaces:

```bash
cat >/etc/systemd/network/80-container-ve.network <<'EOF'
[Match]
Name=ve-*

[Network]
Address=10.88.0.1/24
DHCPServer=yes
EOF

systemctl restart systemd-networkd
```

Enable forwarding + NAT (replace `<ext_if>` with your public interface, e.g. `enp0s31f6`):

```bash
sysctl -w net.ipv4.ip_forward=1

iptables -t nat -A POSTROUTING -s 10.88.0.0/24 -o <ext_if> -j MASQUERADE
iptables -A FORWARD -s 10.88.0.0/24 -o <ext_if> -j ACCEPT
iptables -A FORWARD -d 10.88.0.0/24 -m state --state ESTABLISHED,RELATED -i <ext_if> -j ACCEPT
```

Persist the iptables rules using `iptables-persistent` (or migrate to nftables).

### 8.4 Notes for container workspaces
Tailscale inside a container requires:
- `/dev/net/tun` bound into the container
- `CAP_NET_ADMIN`
- A private network namespace (not host network)

If those aren’t enabled, Tailscale will fail or affect the host instead of the workspace.

Enable + start:

```bash
systemctl daemon-reload
systemctl enable --now open_agent.service
```

Test:

```bash
curl -fsSL http://127.0.0.1:3000/api/health | jq .
```

---

## 8) Optional: Desktop automation dependencies

If you want browser/desktop automation on Ubuntu, run:

```bash
cd /opt/open_agent/vaduz-v1
bash scripts/install_desktop.sh
```

Or follow `docs/DESKTOP_SETUP.md`.

---

## 9) Updating

### 9.1 Update Open Agent (build on server, restart service)

```bash
cd /opt/open_agent/vaduz-v1
git pull
source /root/.cargo/env
cargo build --bin open_agent --bin host-mcp --bin desktop-mcp
install -m 0755 target/debug/open_agent /usr/local/bin/open_agent
install -m 0755 target/debug/host-mcp /usr/local/bin/host-mcp
install -m 0755 target/debug/desktop-mcp /usr/local/bin/desktop-mcp
systemctl restart open_agent.service
```

### 9.2 Update OpenCode (replace binary, restart service)

```bash
# Optionally pin a version
curl -fsSL https://opencode.ai/install | bash -s -- --version 1.1.8 --no-modify-path
install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode
systemctl restart opencode.service
curl -fsSL http://127.0.0.1:4096/global/health | jq .
```

## Suggested improvements

- Put Open Agent behind a reverse proxy (Caddy/Nginx) with TLS and restrict who can reach `:3000`.
- Set `DEV_MODE=false` in production and use strong JWT secrets / multi-user auth.
- Run OpenCode on localhost only (already recommended) and keep it firewalled.
- Pin OpenCode/plugin versions for reproducible deployments.
