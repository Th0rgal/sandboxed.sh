# Installing sandboxed.sh with Docker

Docker is the easiest way to run sandboxed.sh (formerly Sandboxed.sh). One command gets you a complete environment with the Rust backend, Next.js dashboard, and all AI harness CLIs pre-installed.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) and Docker Compose installed
- (Optional) A domain name for production deployment with TLS

## Quick Start

```bash
git clone https://github.com/Th0rgal/sandboxed-sh.git
cd sandboxed-sh
cp .env.example .env
# Edit .env — at minimum, set DASHBOARD_PASSWORD and JWT_SECRET
docker compose up -d
```

Open [http://localhost:3000](http://localhost:3000) in your browser.

## Configuration

### Essential environment variables

Copy `.env.example` to `.env` and configure the values below. The full file contains additional optional settings.

#### Authentication

| Variable | Default | Description |
|---|---|---|
| `DEV_MODE` | `true` | Set to `false` in production to enforce authentication |
| `DASHBOARD_PASSWORD` | `change-me` | Password for dashboard login |
| `JWT_SECRET` | `change-me-to-a-long-random-string` | Secret used to sign JWT tokens |
| `JWT_TTL_DAYS` | `30` | How long JWT tokens remain valid |
| `SANDBOXED_SH_USERS` | _(unset)_ | Optional JSON array for multi-user auth (overrides `DASHBOARD_PASSWORD`) |

#### Library

| Variable | Default | Description |
|---|---|---|
| `LIBRARY_REMOTE` | `https://github.com/Th0rgal/sandboxed-library-template.git` | Git URL for your agent library. By default, clones the official template. Set this to your own fork or custom library (e.g. `git@github.com:your-org/agent-library.git`). Can also be changed via the dashboard Settings page. |
| `LIBRARY_PATH` | `/root/.sandboxed-sh.sh/library` | Local path where the library is cloned |

#### Server

| Variable | Default | Description |
|---|---|---|
| `HOST` | `0.0.0.0` | Bind address for the backend |
| `PORT` | `3000` | Backend listen port (inside the container) |
| `WORKING_DIR` | `/root` | Root directory for workspaces |
| `MAX_ITERATIONS` | `50` | Max tool-call iterations per mission |
| `MAX_PARALLEL_MISSIONS` | `1` | Number of missions that can run concurrently |

### Enabling container workspaces

By default, workspaces run in host/fallback mode — processes execute directly inside the Docker container. To enable full **systemd-nspawn isolation** (each workspace gets its own lightweight container), edit `docker-compose.yml` and uncomment the privileged lines:

```yaml
services:
  sandboxed.sh:
    # ...
    privileged: true
    cgroup: host
```

Then restart:

```bash
docker compose down && docker compose up -d
```

This works on **both Linux and macOS**. On macOS, Docker Desktop runs a Linux VM under the hood, so systemd-nspawn works inside it just fine.

### SSH keys for private git repos

The compose file mounts your host SSH keys read-only:

```yaml
volumes:
  - ~/.ssh:/root/.ssh:ro
```

On startup, the entrypoint automatically adds GitHub and GitLab host keys to `known_hosts`. If the mount is read-only (which it is by default), a writable copy is used instead so SSH operations work without modifying your host files.

If your library or workspaces use private git repos, ensure your SSH keys are at `~/.ssh` on the host.

### Persistent data

Two Docker volumes keep data across container restarts:

| Volume | Mount point | Contents |
|---|---|---|
| `sandboxed.sh-data` | `/root/.sandboxed-sh.sh` | SQLite database, library, container rootfs, settings |
| `claude-auth` | `/root/.claude` | Claude Code OAuth credentials |

To back up your data, use `docker volume inspect` to find the volume paths, or bind-mount them to host directories instead.

## What's included in the Docker image

The multi-stage build produces a single image with everything pre-installed:

- **Rust backend** — `sandboxed_sh`, `desktop-mcp`, `workspace-mcp` binaries
- **Next.js dashboard** — standalone build served on port 3001 internally
- **Caddy** — reverse proxy that unifies backend + dashboard on port 80
- **Claude Code CLI** — installed via npm
- **OpenCode CLI** — installed from opencode.ai
- **Amp CLI** — installed via npm
- **Bun** and **Node.js 20**
- **systemd-container + debootstrap** — for container workspace isolation
- **Desktop automation** — Xvfb, i3, Chromium, xdotool, scrot, ImageMagick, Tesseract OCR
- **Utilities** — git, curl, jq, SSH client, gnupg

## Production deployment

### With a domain and TLS

The Docker image serves HTTP on port 80. For production with TLS, use an external reverse proxy:

1. **Bind to localhost only** — change the port mapping in `docker-compose.yml`:
   ```yaml
   ports:
     - "127.0.0.1:3000:80"
   ```

2. **Set up a reverse proxy** on the host (Caddy, Nginx, etc.) with TLS pointing to `localhost:3000`.

3. **Harden the environment** in `.env`:
   ```bash
   DEV_MODE=false
   DASHBOARD_PASSWORD=<strong-password>
   JWT_SECRET=<long-random-string>
   ```

### Dashboard options

The Docker image includes the Next.js dashboard, but you can also:

- **Deploy the dashboard separately on Vercel** — set `NEXT_PUBLIC_API_URL` to your server's URL
- **Use the iOS app** — point it at your server URL

## Updating

```bash
cd sandboxed.sh
git pull
docker compose build
docker compose up -d
```

## Troubleshooting

| Problem | Fix |
|---|---|
| Container workspaces not working | Uncomment `privileged: true` and `cgroup: host` in `docker-compose.yml` |
| Permission denied on SSH keys | Ensure `~/.ssh` on the host is readable by your user |
| Port conflict on 3000 | Change the port mapping (e.g. `"8080:80"`) in `docker-compose.yml` |
| Build takes too long | Rust compilation is the bottleneck (~5–10 min first time). Subsequent builds use Docker layer caching. |
| Backend not starting | Check logs with `docker compose logs -f sandboxed.sh` |

## Comparison with native installation

Docker wraps the entire stack into a single container — backend, dashboard, reverse proxy, and all AI CLIs. Native installation gives you more control and slightly better performance.

For most users, **Docker is the right choice**. Consider native installation when you need maximum performance on a dedicated production server.

→ [Native installation guide](install-native.md)
