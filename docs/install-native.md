# Installing Sandboxed.sh — Native (Bare Metal)

This guide covers installing Sandboxed.sh directly on a **dedicated Ubuntu 24.04
server** with systemd. This gives you the best performance and native
systemd-nspawn container isolation.

> **Looking for the easier path?** See the [Docker installation guide](install-docker.md)
> — one command gets you running on any OS.

Sandboxed.sh is the orchestrator/UI backend. **It does not run model inference**;
it executes OpenCode, Claude Code, and Amp **inside each workspace**
(host/container), so bash commands and file operations are scoped correctly. A
standalone OpenCode server is **optional** and only required if you want
centralized OpenCode services (provider/auth management, health checks, etc.).

> **For AI Agents**: Before starting this installation, ask the user to provide:
>
> 1. **Server IP address** (e.g., `1.2.3.4`)
> 2. **Domain name** pointing to that IP (e.g., `agent.example.com`)
> 3. **SSH access** credentials or key path for the server
> 4. **Library git repo URL** (or confirm using the template)
> 5. **Dashboard password** to set for authentication (or offer to generate one)
>
> **SSH Key Setup**: Most servers require SSH key authentication. If the user
> doesn't have one:
>
> 1. Generate a key (without passphrase for easier automation):
>    ```bash
>    ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N ""
>    ```
> 2. Display the public key for them to copy:
>    ```bash
>    cat ~/.ssh/id_ed25519.pub
>    ```
> 3. They need to add this key to their server. Options:
>    - **Hosting provider dashboard**: Most providers (Hetzner, DigitalOcean,
>      Vultr, etc.) have an "SSH Keys" section in their web console
>    - **Existing access**: If they can already log in:
>      `ssh-copy-id root@<server-ip>`
>
> Verify you have SSH access before proceeding:
> `ssh root@<server-ip> "hostname"`
>
> If connection fails, common causes:
>
> - The SSH key wasn't added to the server (check hosting provider's dashboard)
> - The key has a passphrase (agent tools may not handle passphrase prompts)
> - Firewall blocking port 22 (check hosting provider's firewall settings)

---

## 0) Assumptions

- Ubuntu 24.04 LTS, root SSH access
- A dedicated server (not shared hosting)
- You want:
  - Sandboxed.sh bound to: `0.0.0.0:3000`
  - (Optional) OpenCode server bound to localhost: `127.0.0.1:4096`
- You have a Git repo for your **Library** (skills/tools/agents/rules/MCP
  configs)

> **Recommendation**: Unless you know exactly what you need, install **all
> components** in this guide:
>
> - **Bun** (required for OpenCode plugins and Playwright MCP)
> - **systemd-container + debootstrap** (for isolated container workspaces)
> - **Desktop automation tools** (Xvfb, i3, Chromium, xdotool, etc.)
> - **Reverse proxy with SSL** (Caddy or Nginx + Certbot)
>
> Skipping components may limit functionality. The full installation uses ~2-3
> GB of disk space.

---

## 0.5) DNS & Domain Setup (before you begin)

Before starting the installation, ensure your domain is configured:

### 0.5.1 Point your domain to the server

Add an A record in your DNS provider:

```
agent.yourdomain.com → A → YOUR_SERVER_IP
```

Example with common providers:

- **Cloudflare**: DNS → Add Record → Type: A, Name: `agent`, IPv4:
  `YOUR_SERVER_IP`
- **Namecheap**: Advanced DNS → Add New Record → A Record
- **Route53**: Create Record → Simple routing → A record

### 0.5.2 Verify DNS propagation

Wait for DNS to propagate (usually 1-15 minutes), then verify:

```bash
# From your local machine
dig +short agent.yourdomain.com
# Should return your server IP

# Or use an online checker
curl -s "https://dns.google/resolve?name=agent.yourdomain.com&type=A" | jq .
```

### 0.5.3 SSH key for Library repo (if private)

If your Library repo is private, set up an SSH deploy key on the server:

```bash
# On the server
ssh-keygen -t ed25519 -C "sandboxed.sh-server" -f /root/.ssh/sandboxed.sh -N ""
cat /root/.ssh/sandboxed.sh.pub
# Copy this public key
```

Add the public key as a **deploy key** in your git provider:

- **GitHub**: Repository → Settings → Deploy keys → Add deploy key
- **GitLab**: Repository → Settings → Repository → Deploy keys

Configure SSH to use the key:

```bash
cat >> /root/.ssh/config <<'EOF'
Host github.com
    IdentityFile /root/.ssh/sandboxed.sh
    IdentitiesOnly yes
EOF

# Test the connection
ssh -T git@github.com
```

---

## 1) Install base OS dependencies

```bash
apt update
apt install -y \
  ca-certificates curl git jq unzip tar \
  build-essential pkg-config libssl-dev
```

**Container workspaces** (systemd-nspawn) — recommended for isolated
environments:

```bash
apt install -y systemd-container debootstrap
```

**Desktop automation** (Xvfb/i3/Chromium screenshots/OCR) — recommended for
browser control:

```bash
apt install -y xvfb i3 x11-utils xdotool scrot imagemagick chromium chromium-sandbox tesseract-ocr
```

See `docs/DESKTOP_SETUP.md` for i3 config and additional setup after
installation.

---

## 2) Install Bun (for bunx + Playwright MCP)

OpenCode is distributed as a binary, but:

- OpenCode plugins are installed internally via Bun
- Sandboxed.sh’s default Playwright MCP runner prefers `bunx`

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

## 3) Install OpenCode (optional server backend)

OpenCode server is optional for mission execution. Sandboxed.sh runs OpenCode
per-workspace via the CLI. Install the server if you want centralized
provider/auth management, health checks, or a shared OpenCode service.

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

Skip this section if you are not running a centralized OpenCode server.

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

Note: Sandboxed.sh will also keep OpenCode's global config updated (MCP + tool
allowlist) in: `~/.config/opencode/opencode.json`.

### 3.2.1 Strong workspace skill isolation (recommended)

OpenCode discovers skills from global locations (e.g. `~/.opencode/skill`,
`~/.config/opencode/skill`) _and_ from the project/mission directory
`.opencode/skill`. To guarantee **per‑workspace** skill usage, run OpenCode with
an isolated HOME and keep global skill dirs empty.

1. Create an isolated OpenCode home:

```bash
mkdir -p /var/lib/opencode
```

2. Update `opencode.service` to use the isolated home:

```ini
Environment=HOME=/var/lib/opencode
Environment=XDG_CONFIG_HOME=/var/lib/opencode/.config
Environment=XDG_DATA_HOME=/var/lib/opencode/.local/share
Environment=XDG_CACHE_HOME=/var/lib/opencode/.cache
```

3. Point Sandboxed.sh at the same OpenCode config dir (see section 6):

```
OPENCODE_CONFIG_DIR=/var/lib/opencode/.config/opencode
```

4. Move any old global skills out of the way (optional but recommended):

```bash
mv /root/.opencode/skill /root/.opencode/skill.bak-$(date +%F) 2>/dev/null || true
mv /root/.config/opencode/skill /root/.config/opencode/skill.bak-$(date +%F) 2>/dev/null || true
```

5. Reload services:

```bash
systemctl daemon-reload
systemctl restart opencode.service
systemctl restart sandboxed_sh.service
```

Validation (on the server, from the repo root):

```bash
scripts/validate_skill_isolation.sh
```

### 3.3 Install oh-my-opencode (agent pack)

Install the default agent pack. **Always use `bunx`, never `npm install -g`**:

```bash
bunx oh-my-opencode install --no-tui --claude=max20 --gemini=yes
```

This installs the **Sisyphus** default agent (plus other personalities like
Oracle, Librarian, etc.).

**Important**: If you enabled strong workspace skill isolation (section 3.2.1),
the OpenCode service runs with `HOME=/var/lib/opencode`. Install oh-my-opencode
with the correct HOME to ensure version detection works:

```bash
# Install with isolated HOME (required if using section 3.2.1)
mkdir -p /var/lib/opencode/.config/opencode
HOME=/var/lib/opencode bunx oh-my-opencode install --no-tui --claude=max20 --gemini=yes

# Also copy/create configs if needed
cp /root/.config/opencode/opencode.json /var/lib/opencode/.config/opencode/ 2>/dev/null || true
systemctl restart opencode.service

# Verify agents are loaded
curl -s http://127.0.0.1:4096/agent | jq '.[].name'
# Should show: Sisyphus, oracle, librarian, etc.
```

To preserve plugin defaults: leave the Sandboxed.sh agent/model overrides unset to
use the OpenCode / oh-my-opencode defaults.

Update strategy:

- Pin a version in your Library `plugins.json` (e.g., `oh-my-opencode@1.2.3`) to
  lock updates.
- Otherwise, the plugin can auto-update via OpenCode's install hook and Open
  Agent sync.
- **After updating oh-my-opencode**, re-copy the configs if using isolated home.

### 3.4 Install opencode-gemini-auth (optional, for Google OAuth)

If you want to authenticate with Google accounts (Gemini plans/quotas including
free tier) via OAuth instead of API keys:

```bash
bunx opencode-gemini-auth install
```

This enables OAuth-based Google authentication, allowing users to leverage their
existing Gemini plan directly within OpenCode. Features include:

- OAuth flow with Google accounts
- Automatic Cloud project provisioning
- Support for thinking capabilities (Gemini 2.5/3)

To authenticate via CLI (useful for testing):

```bash
opencode auth login
# Select Google provider, then "OAuth with Google (Gemini CLI)"
```

For dashboard OAuth integration, see the Settings page which handles this flow
via the API.

---

## 3.5) Configure Amp (Sourcegraph's coding agent)

Amp is Sourcegraph's frontier coding agent with multi-model support. Unlike
Claude Code and OpenCode which can use your own API keys, **Amp requires paid
credits purchased through Sourcegraph**.

### 3.5.1 Get Amp API credentials

1. Visit [ampcode.com](https://ampcode.com) and create an account
2. Purchase credits (Amp does not have a free tier for API access)
3. Generate an API key from your account dashboard

### 3.5.2 Configure Amp in Sandboxed.sh

**Option A: Via Dashboard Settings (recommended)**

1. Go to **Settings → AI Backends**
2. Find the "Amp" backend and click to configure
3. Enter your `AMP_API_KEY`
4. Choose default mode: `smart` (full capability) or `rush` (faster, cheaper)

**Option B: Via environment file**

Add to `/etc/sandboxed_sh/sandboxed_sh.env`:

```bash
AMP_API_KEY=sgamp_user_XXXXX...
```

### 3.5.3 Amp modes

| Mode      | Description                                 |
| --------- | ------------------------------------------- |
| **smart** | Full capability mode with extended thinking |
| **rush**  | Faster responses, lower cost, less thorough |

### 3.5.4 Using CLIProxyAPI (cost optimization)

To route Amp requests through a local proxy (e.g.,
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)), set the `AMP_URL`
environment variable:

```bash
# In /etc/sandboxed_sh/sandboxed_sh.env
AMP_URL=http://localhost:8080
```

This redirects Amp CLI requests to your proxy instead of ampcode.com.

### 3.5.5 Troubleshooting Amp

**"Insufficient credits" error**: Amp requires paid credits. Purchase credits at
ampcode.com.

**"Network timeout" error**: This was a known issue in earlier versions. Update
to the latest Sandboxed.sh which includes the fix.

**CLI not found**: Sandboxed.sh auto-installs the Amp CLI via bun/npm. Ensure bun
or npm is available in your PATH.

---

## 4) Install Sandboxed.sh (Rust backend)

### 4.1 Install Rust toolchain

```bash
curl -fsSL https://sh.rustup.rs | sh -s -- -y
source /root/.cargo/env
rustc --version
cargo --version
```

### 4.2 Deploy the repository

On the server we keep the repo under `/opt/sandboxed_sh/vaduz-v1`. **This must be
a git clone** (not just copied files) for the dashboard's one-click update
system to work.

```bash
mkdir -p /opt/sandboxed_sh
cd /opt/sandboxed_sh
git clone <YOUR_SANDBOXED_SH_REPO_URL> vaduz-v1
```

> **Important**: The update system in Settings relies on git tags to detect new
> releases. If you deploy via rsync without `.git`, the "Update Available"
> button won't work. Always use `git clone` for production deployments.

For local development, you can rsync to a _different_ path (e.g.,
`/root/sandboxed_sh`) for rapid iteration, but keep the git clone at
`/opt/sandboxed_sh/vaduz-v1` for the update system:

```bash
# Fast dev loop (to a separate path)
rsync -az --delete \
  --exclude target --exclude .git --exclude dashboard/node_modules \
  /path/to/local-dev/ \
  root@<server-ip>:/root/sandboxed_sh/

# The git clone at /opt/sandboxed_sh/vaduz-v1 is used by the update system
```

If you need to specify a custom SSH key, add `-e "ssh -i ~/.ssh/your_key"`.

### 4.3 Build and install binaries

```bash
cd /opt/sandboxed_sh/vaduz-v1
source /root/.cargo/env

# Debug build (fast) - recommended for rapid iteration
cargo build --bin sandboxed_sh
install -m 0755 target/debug/sandboxed_sh /usr/local/bin/sandboxed_sh

# Or: Release build (slower compile, faster runtime)
# cargo build --release --bin sandboxed_sh
# install -m 0755 target/release/sandboxed_sh /usr/local/bin/sandboxed_sh

# Optional: build MCP helpers if you want legacy workspace/desktop tools
# cargo build --release --bin workspace-mcp --bin desktop-mcp
# install -m 0755 target/release/workspace-mcp /usr/local/bin/workspace-mcp
# install -m 0755 target/release/desktop-mcp /usr/local/bin/desktop-mcp
```

---

## 5) Bootstrap the Library (config repo)

Sandboxed.sh expects a git-backed **Library** repo. At runtime it will:

- clone it into `LIBRARY_PATH` (default: `{WORKING_DIR}/.sandboxed-sh.sh/library`)
- ensure the `origin` remote matches `LIBRARY_REMOTE`
- pull/sync as needed

### 5.1 Create your own library repo from the template

Template:

- https://github.com/Th0rgal/sandboxed-library-template

One way to bootstrap:

```bash
# On your machine
git clone git@github.com:Th0rgal/sandboxed.sh-library-template.git sandboxed.sh-library
cd sandboxed.sh-library

# Point it at your own repo
git remote set-url origin git@github.com:<your-org>/<your-library-repo>.git

# Push to your remote (choose main/master as you prefer)
git push -u origin HEAD:main
```

### 5.2 Configure Sandboxed.sh to use it

**Option A: Via Dashboard Settings (recommended)**

After starting Sandboxed.sh, go to **Settings** in the dashboard and set the
Library Remote URL. This is the preferred method as it persists the setting to
disk and allows runtime updates without restart.

**Option B: Via environment variable (initial default)**

Set in `/etc/sandboxed_sh/sandboxed_sh.env`:

- `LIBRARY_REMOTE=git@github.com:<your-org>/<your-library-repo>.git` (used as
  initial default if not configured in Settings)
- optional: `LIBRARY_PATH=/root/.sandboxed-sh.sh/library`

### 5.3 Library encryption key

Workspace templates can mark env vars as **encrypted**. When saved, those values
are encrypted with AES-256-GCM and stored in the git repo as
`<encrypted v="1">...</encrypted>`. The plaintext is only visible in the
dashboard and when deployed to workspaces.

**How the key works:**

- A single encryption key protects all encrypted values in the library.
- The key is stored at `{WORKING_DIR}/.sandboxed-sh.sh/private_key` (typically
  `/root/.sandboxed-sh.sh/private_key`).
- On first save of any encrypted value, Sandboxed.sh **auto-generates** the key if
  none exists.
- The key can also be set manually via the `PRIVATE_KEY` environment variable
  (hex-encoded, 32 bytes / 64 hex chars).

**Sharing the key across servers:**

All servers that use the same Library repo **must share the same encryption
key**, otherwise they cannot decrypt each other's encrypted values.

The recommended way to share the key is via **Backup & Restore** in the
dashboard (Settings page). The backup archive includes the encryption key along
with all other settings and credentials.

To set up a new server with an existing library:

1. On the **source** server: go to **Settings → Backup & Restore** and click
   **Download Backup**.
2. On the **new** server: go to **Settings → Backup & Restore** and upload the
   backup file via **Restore Backup**.
3. The encryption key, AI provider credentials, workspace definitions, and all
   other settings are restored automatically.

Alternatively, copy the key file manually:

```bash
# Copy from source to new server
scp root@source-server:/root/.sandboxed-sh.sh/private_key root@new-server:/root/.sandboxed-sh.sh/private_key
```

Or set it via the environment:

```bash
# In /etc/sandboxed_sh/sandboxed_sh.env on the new server
PRIVATE_KEY=<64-hex-char-key-from-source-server>
```

---

## 6) Configure Sandboxed.sh (env file)

Create `/etc/sandboxed_sh/sandboxed_sh.env`:

```bash
mkdir -p /etc/sandboxed_sh
chmod 700 /etc/sandboxed_sh
```

Example (fill in your real values):

```bash
cat > /etc/sandboxed_sh/sandboxed_sh.env <<'EOF'
# OpenCode backend (optional; if set, must match opencode.service)
OPENCODE_BASE_URL=http://127.0.0.1:4096
OPENCODE_PERMISSIVE=true
# Optional: keep Sandboxed.sh writing OpenCode global config into the isolated home
# (recommended if you enabled strong workspace skill isolation in section 3.2.1).
# OPENCODE_CONFIG_DIR=/var/lib/opencode/.config/opencode

# Server bind
HOST=0.0.0.0
PORT=3000

# Default filesystem root for Sandboxed.sh (agent still has full system access)
WORKING_DIR=/root
LIBRARY_PATH=/root/.sandboxed-sh.sh/library
# Library remote (optional, can also be set via dashboard Settings page)
LIBRARY_REMOTE=git@github.com:<your-org>/<your-library-repo>.git

# Auth (set DEV_MODE=false on real deployments)
DEV_MODE=false
DASHBOARD_PASSWORD=change-me
JWT_SECRET=change-me-to-a-long-random-string
JWT_TTL_DAYS=30

# Dashboard Console (local shell)
# No SSH configuration required.

# Default model (provider/model). If omitted or not in provider/model format,
# Sandboxed.sh won’t force a model and OpenCode will use its own defaults.

# Desktop tools (optional)
DESKTOP_ENABLED=true
DESKTOP_RESOLUTION=1920x1080
EOF
```

---

## 7) Create `systemd` unit for Sandboxed.sh

Create `/etc/systemd/system/sandboxed_sh.service`:

```ini
[Unit]
Description=OpenAgent (cloud orchestrator)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
Group=root
EnvironmentFile=/etc/sandboxed_sh/sandboxed_sh.env
WorkingDirectory=/root
ExecStart=/usr/local/bin/sandboxed_sh
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

If you want a **workspace** to egress via a residential IP, the recommended
pattern is:

1. Run a Tailscale **exit node** at home.
2. Use a workspace template that installs and starts Tailscale inside the
   container.

### 8.1 Enable the exit node at home

On the home server:

```bash
tailscale up --advertise-exit-node
```

Approve it in the Tailscale admin console (Machines → your node → “Approve exit
node”).

### 8.2 Use the `residential` workspace template

This repo ships a sample template at:

```
library-template/workspace-template/residential.json
```

It installs Tailscale and adds helper scripts:

- `sandboxed.sh-network-up` (brings up host0 veth + DHCP + DNS)
- `sandboxed.sh-tailscale-up` (starts tailscaled + sets exit node)
- `sandboxed.sh-tailscale-check` (prints Tailscale status + public IP)

Set these **workspace env vars** (not global env):

- `TS_AUTHKEY` (auth key for that workspace)
- `TS_EXIT_NODE` (node name like `umbrel` or its 100.x IP)
- Optional: `TS_ACCEPT_DNS=true|false`, `TS_EXIT_NODE_ALLOW_LAN=false`,
  `TS_STATE_DIR=/var/lib/tailscale`

Then inside the workspace:

```bash
sandboxed.sh-tailscale-up
sandboxed.sh-tailscale-check
```

If the public IP matches your home ISP, the exit node is working.

### 8.3 Host NAT for veth networking (required)

`systemd-nspawn --network-veth` needs DHCP + NAT on the host. Without this,
containers won’t reach the internet or Tailscale control plane.

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

Enable forwarding + NAT (replace `<ext_if>` with your public interface, e.g.
`enp0s31f6`):

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

If those aren’t enabled, Tailscale will fail or affect the host instead of the
workspace.

Enable + start:

```bash
systemctl daemon-reload
systemctl enable --now sandboxed_sh.service
```

Test:

```bash
curl -fsSL http://127.0.0.1:3000/api/health | jq .
```

---

## 8) Optional: Desktop automation dependencies

If you want browser/desktop automation on Ubuntu, run:

```bash
cd /opt/sandboxed_sh/vaduz-v1
bash scripts/install_desktop.sh
```

Or follow `docs/DESKTOP_SETUP.md`.

---

## 9) Updating

### 9.1 Update Sandboxed.sh via Dashboard (recommended)

The Settings page shows available updates for Sandboxed.sh, OpenCode, and
oh-my-opencode. When a new version is available:

1. Go to **Settings → System Components**
2. If "Update Available" appears, click the **Update** button
3. The update will:
   - Fetch the latest git tags from the repository
   - Checkout the newest release tag
   - Build the binaries (debug mode for faster compile)
   - Install and restart the service

**Requirements for one-click updates:**

- The repository at `/opt/sandboxed_sh/vaduz-v1` must be a git clone (not rsync'd
  files)
- Create GitHub releases with version tags (e.g., `0.2.1` or `v0.2.1`) to
  trigger update detection
- The server needs SSH access to pull from GitHub (deploy key configured in
  section 0.5.3)

### 9.2 Update Sandboxed.sh manually (CLI)

```bash
cd /opt/sandboxed_sh/vaduz-v1
git fetch --tags origin
git checkout <version-tag>  # e.g., v0.2.1
source /root/.cargo/env
cargo build --bin sandboxed_sh --bin workspace-mcp --bin desktop-mcp
install -m 0755 target/debug/sandboxed_sh /usr/local/bin/sandboxed_sh
install -m 0755 target/debug/workspace-mcp /usr/local/bin/workspace-mcp
install -m 0755 target/debug/desktop-mcp /usr/local/bin/desktop-mcp
systemctl restart sandboxed_sh.service
```

Optional: if you no longer use legacy workspace/desktop MCP tools, build only
`sandboxed_sh`:

```bash
cargo build --bin sandboxed_sh
install -m 0755 target/debug/sandboxed_sh /usr/local/bin/sandboxed_sh
systemctl restart sandboxed_sh.service
```

Or to follow the latest master branch:

```bash
cd /opt/sandboxed_sh/vaduz-v1
git pull origin master
# ... build and install as above
```

### 9.3 Update OpenCode (optional server binary)

```bash
# Optionally pin a version
curl -fsSL https://opencode.ai/install | bash -s -- --version 1.1.8 --no-modify-path
install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode
systemctl restart opencode.service
curl -fsSL http://127.0.0.1:4096/global/health | jq .
```

### 9.4 Update oh-my-opencode

oh-my-opencode is installed via `bunx` and cached in the service's HOME
directory. Updates can be triggered from the dashboard (Settings → System
Components) or manually:

```bash
# Run with the same HOME as the Sandboxed.sh service
# Check your /etc/sandboxed_sh/sandboxed_sh.env for the correct HOME value
source /etc/sandboxed_sh/sandboxed_sh.env
bunx oh-my-opencode@latest install --no-tui --claude=yes --gemini=yes --copilot=no
```

**Important:** Do NOT install oh-my-opencode globally via `npm install -g`. This
creates version detection conflicts because the global binary in PATH takes
precedence over bunx cache. Always use `bunx` which caches packages in the HOME
directory.

#### Troubleshooting version detection issues

If the dashboard shows an old version after updating, check for stale
installations:

```bash
# 1. Check for npm global install
npm ls -g oh-my-opencode
# If found, remove it:
npm uninstall -g oh-my-opencode

# 2. Check for stale binaries in common locations
which oh-my-opencode
ls -la /usr/local/bin/oh-my-opencode
ls -la /usr/bin/oh-my-opencode
# Remove any found:
rm -f /usr/local/bin/oh-my-opencode /usr/bin/oh-my-opencode

# 3. Check NVM paths (if using NVM for Node.js)
ls /root/.nvm/versions/node/*/bin/oh-my-opencode 2>/dev/null
# Remove any found:
rm -f /root/.nvm/versions/node/*/bin/oh-my-opencode

# 4. Clear bun cache for the service's HOME
# Use the same HOME as configured in /etc/sandboxed_sh/sandboxed_sh.env
source /etc/sandboxed_sh/sandboxed_sh.env
rm -rf $HOME/.bun/install/cache/oh-my-opencode*

# 5. Reinstall and verify
bunx oh-my-opencode@latest install --no-tui --claude=yes --gemini=yes --copilot=no
bunx oh-my-opencode --version
```

The service detects versions by running `bunx oh-my-opencode --version`. If
there's a binary earlier in PATH, it will report that version instead of the
bunx-cached version.

## 10) Production Security (TLS + Reverse Proxy)

For production deployments, **always** put Sandboxed.sh behind a reverse proxy
with TLS. The backend serves HTTP only and should never be exposed directly to
the internet.

### 10.1 Caddy (recommended - automatic HTTPS)

Caddy automatically obtains and renews Let's Encrypt certificates.

Install Caddy:

```bash
apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list
apt update && apt install caddy
```

Create `/etc/caddy/Caddyfile`:

```
agent.yourdomain.com {
    reverse_proxy localhost:3000
}
```

Enable and start:

```bash
systemctl enable --now caddy
```

Caddy will automatically obtain TLS certificates for your domain.

### 10.2 Nginx (manual certificate setup)

Install Nginx and Certbot:

```bash
apt install -y nginx certbot python3-certbot-nginx
```

Create `/etc/nginx/sites-available/sandboxed.sh`:

```nginx
server {
    listen 80;
    server_name agent.yourdomain.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # SSE support (for mission streaming)
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 86400s;
    }
}
```

Enable the site and obtain certificates:

```bash
ln -s /etc/nginx/sites-available/sandboxed.sh /etc/nginx/sites-enabled/
nginx -t && systemctl reload nginx
certbot --nginx -d agent.yourdomain.com
```

### 10.3 Firewall

Block direct access to port 3000 from the internet:

```bash
# Allow only localhost to reach Sandboxed.sh directly
iptables -A INPUT -p tcp --dport 3000 -s 127.0.0.1 -j ACCEPT
iptables -A INPUT -p tcp --dport 3000 -j DROP
```

---

## 11) Authentication Modes

Sandboxed.sh supports three authentication modes:

| Mode              | Environment Variables              | Use Case                      |
| ----------------- | ---------------------------------- | ----------------------------- |
| **Disabled**      | `DEV_MODE=true`                    | Local development only        |
| **Single Tenant** | `DASHBOARD_PASSWORD`, `JWT_SECRET` | Personal server, one user     |
| **Multi-User**    | `SANDBOXED_SH_USERS`, `JWT_SECRET`   | Shared server, multiple users |

### 11.1 Single Tenant (default for production)

Set a strong password and JWT secret:

```bash
# Generate a random JWT secret
JWT_SECRET=$(openssl rand -base64 32)

# In /etc/sandboxed_sh/sandboxed_sh.env:
DEV_MODE=false
DASHBOARD_PASSWORD=your-strong-password-here
JWT_SECRET=$JWT_SECRET
JWT_TTL_DAYS=30
```

### 11.2 Multi-User Mode

For multiple users with separate credentials:

```bash
# In /etc/sandboxed_sh/sandboxed_sh.env:
DEV_MODE=false
SANDBOXED_SH_USERS='[
  {"username": "alice", "password": "alice-strong-password"},
  {"username": "bob", "password": "bob-strong-password"}
]'
JWT_SECRET=$(openssl rand -base64 32)
```

Note: Multi-user mode provides separate login credentials but does **not**
provide workspace or data isolation between users. All users see the same
missions and workspaces.

---

## 12) Dashboard Configuration

This guide installs the **backend** on your server. The dashboard (frontend) is
separate and you have several options:

| Option      | Best For                      | Setup                             |
| ----------- | ----------------------------- | --------------------------------- |
| **Vercel**  | Production, always accessible | Deploy `dashboard/` to Vercel     |
| **Local**   | Development, quick testing    | Run `bun dev` in dashboard folder |
| **iOS App** | Mobile access                 | Enter backend URL in app          |

### 12.1 Web Dashboard (Vercel)

Deploy the `dashboard/` folder to [Vercel](https://vercel.com):

1. Connect your repo to Vercel
2. Set the root directory to `dashboard`
3. Add environment variable: `NEXT_PUBLIC_API_URL=https://agent.yourdomain.com`
4. Deploy

The dashboard will connect to your backend server.

### 12.2 Web Dashboard (Local)

Run the dashboard locally on your machine:

```bash
cd dashboard
bun install
NEXT_PUBLIC_API_URL=https://agent.yourdomain.com bun dev
```

Then open `http://localhost:3000`.

### 12.3 iOS App

On first launch, the iOS app prompts for the server URL. Enter your backend URL
(e.g., `https://agent.yourdomain.com`).

To change later: **Menu (⋮) → Settings**

---

## 13) OAuth Provider Setup

Sandboxed.sh uses OAuth for AI provider authentication. The following providers
are pre-configured:

| Provider          | OAuth Client      | Setup Required                        |
| ----------------- | ----------------- | ------------------------------------- |
| **Anthropic**     | OpenCode's client | None (works out of the box)           |
| **OpenAI**        | Codex CLI client  | None (works out of the box)           |
| **Google/Gemini** | Gemini CLI client | Install `opencode-gemini-auth` plugin |

OAuth flows use copy-paste for the authorization code. The user:

1. Clicks "Authorize" in the dashboard
2. Completes OAuth in their browser
3. Copies the redirect URL back to the dashboard

---

## Checklist for Production Deployment

- [ ] Set `DEV_MODE=false`
- [ ] Set strong `DASHBOARD_PASSWORD` and `JWT_SECRET`
- [ ] Configure reverse proxy (Caddy or Nginx) with TLS
- [ ] Firewall port 3000 (only allow localhost)
- [ ] Pin OpenCode version for stability
- [ ] Set up your Library git repo
- [ ] Share encryption key across servers (backup/restore or manual copy)
- [ ] Test OAuth flows for AI providers
