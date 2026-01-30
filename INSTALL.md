# Installing sandboxed.sh

There are two ways to install sandboxed.sh (formerly Sandboxed.sh):

## Docker (recommended for most users)

One command gets you a complete environment on any OS (Linux, macOS, Windows).

→ **[Docker installation guide](docs/install-docker.md)**

```bash
git clone https://github.com/Th0rgal/sandboxed-sh.git
cd sandboxed-sh
cp .env.example .env
docker compose up -d
```

## Native (bare metal)

For production servers running Ubuntu 24.04 with maximum performance and native
systemd-nspawn container isolation.

→ **[Native installation guide](docs/install-native.md)**

## Comparison

| | Docker | Native |
|---|---|---|
| **Best for** | Getting started, macOS, quick deployment | Production servers, max performance |
| **Platform** | Any OS with Docker | Ubuntu 24.04 LTS |
| **Setup time** | ~5 minutes | ~30 minutes |
| **Container workspaces** | Yes (with `privileged: true`) | Yes (native systemd-nspawn) |
| **Desktop automation** | Yes (headless Xvfb) | Yes (native X11 or Xvfb) |
| **Performance** | Good (slight overhead on macOS) | Best (native Linux) |
| **Updates** | `docker compose build && up -d` | Git pull + cargo build, or one-click from dashboard |
