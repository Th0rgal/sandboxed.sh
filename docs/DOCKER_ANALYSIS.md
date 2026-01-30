# Dockerization Analysis for Sandboxed.sh

## Executive Summary

Sandboxed.sh consists of three deployable components: a **Rust backend**
(orchestrator + API), a **Next.js dashboard** (frontend), and optional **MCP
helper binaries**. Today it runs on bare-metal Ubuntu 24.04 with systemd-nspawn
for workspace isolation. This report analyzes what a Docker-based deployment
would look like, what trade-offs it introduces, and where the hard boundaries
are.

**Key finding**: Dockerizing the backend + dashboard for simple deployment is
straightforward. Running systemd-nspawn _inside_ Docker is possible and requires
`--privileged`. This works on both Linux hosts and macOS (Docker Desktop provides
a real Linux VM with full kernel namespace/cgroup support). Without
`--privileged`, container workspaces gracefully degrade to host-mode execution.
A single `docker compose up` can serve both backend and dashboard.

---

## 1. Component Inventory

| Component | Technology | Build | Runtime Dependencies |
|-----------|-----------|-------|---------------------|
| **Backend** (`sandboxed_sh`) | Rust (Tokio + Axum) | `cargo build` | git, curl, npm/bun (for harness auto-install) |
| **Dashboard** | Next.js 16 + React 19 | `bun build` / `next build` | Node/Bun runtime |
| **desktop-mcp** | Rust binary | `cargo build` | Xvfb, i3, xdotool, scrot, tesseract |
| **workspace-mcp** | Rust binary | `cargo build` | (none beyond backend) |
| **Container workspaces** | systemd-nspawn | debootstrap | systemd-container, Linux kernel |

---

## 2. Proposed Docker Architecture

### 2.1 Image Strategy: Two Images

**Image 1: `sandboxed.sh/backend`** (Rust backend + MCP binaries)

Multi-stage build:
1. **Builder stage** (`rust:1.75-bookworm`): compile `sandboxed_sh`,
   `desktop-mcp`, `workspace-mcp`
2. **Runtime stage** (`debian:bookworm-slim`): minimal runtime with git, curl,
   npm/bun, and the compiled binaries

**Image 2: `sandboxed.sh/dashboard`** (Next.js frontend)

Multi-stage build:
1. **Builder stage** (`oven/bun:1`): `bun install && bun run build`
2. **Runtime stage** (`oven/bun:1-slim` or `node:20-slim`): `next start`

### 2.2 Compose Topology

```yaml
services:
  backend:
    image: sandboxed.sh/backend
    ports:
      - "3000:3000"
    volumes:
      - sandboxed.sh-data:/root/.sandboxed-sh.sh    # SQLite, library, workspaces
      - /var/run/docker.sock:/var/run/docker.sock  # optional: DinD
    env_file: .env

  dashboard:
    image: sandboxed.sh/dashboard
    ports:
      - "3001:3000"
    environment:
      - NEXT_PUBLIC_API_URL=http://backend:3000
    depends_on:
      - backend

volumes:
  sandboxed.sh-data:
```

### 2.3 Single Combined Image (Alternative)

For simpler deployment, a single image could run both backend and dashboard
behind a lightweight reverse proxy (Caddy). This avoids cross-origin issues and
lets users do `docker run -p 443:443 sandboxed.sh/all-in-one`. The trade-off is a
larger image and coupling the frontend release cycle to the backend.

---

## 3. The systemd-nspawn Question

### 3.1 Current Architecture

Sandboxed.sh uses systemd-nspawn for workspace isolation:

```
nspawn_available() → checks cfg!(target_os = "linux") && /usr/bin/systemd-nspawn exists
```

- Container rootfs created via `debootstrap --variant=minbase`
- Execution via `systemd-nspawn -D <root> --chdir <workspace>`
- Running containers entered via `nsenter --target <PID>`
- Management via `machinectl`

### 3.2 Can systemd-nspawn Run Inside Docker?

**Yes.** `--privileged` is the simplest path. It works on both Linux hosts and
macOS (Docker Desktop runs a full Linux VM with a real kernel, not a syscall
translation layer like WSL1).

systemd-nspawn requires:
- **`--privileged`** or fine-grained capabilities (`CAP_SYS_ADMIN`,
  `CAP_NET_ADMIN`, `CAP_MKNOD`, plus others)
- **`/proc` and `/sys` access** (Docker provides these but nspawn may need
  writable mounts)
- **`seccomp` profile disabled or loosened** (nspawn uses syscalls Docker blocks
  by default)
- A **Linux kernel** (provided natively on Linux, or via Docker Desktop's VM
  on macOS)

Required Docker run flags:

```bash
docker run --privileged \
  --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  sandboxed.sh/backend
```

Or with granular capabilities:

```bash
docker run \
  --cap-add SYS_ADMIN \
  --cap-add NET_ADMIN \
  --cap-add MKNOD \
  --security-opt seccomp=unconfined \
  --security-opt apparmor=unconfined \
  --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  sandboxed.sh/backend
```

**Verdict**: It works with `--privileged` on both Linux hosts and macOS (Docker
Desktop). It is effectively "containers inside containers" (nspawn inside
Docker), which is a supported configuration. On macOS the extra VM layer adds
some I/O overhead during `debootstrap` but steady-state execution is
comparable. The `debootstrap` rootfs writes to a Docker volume.

### 3.3 Alternative: Replace nspawn with Docker-in-Docker

Instead of running systemd-nspawn inside Docker, the workspace isolation layer
could optionally use Docker itself:

| Aspect | systemd-nspawn (current) | Docker workspace (alternative) |
|--------|--------------------------|-------------------------------|
| **Create** | `debootstrap` → directory | `docker build` → image |
| **Execute** | `systemd-nspawn -D <dir>` | `docker run -v <workspace>:/work` |
| **Enter running** | `nsenter --target <PID>` | `docker exec <container>` |
| **Manage** | `machinectl` | `docker` CLI / API |
| **Networking** | `--network-veth` | Docker networks |
| **Image size** | ~150 MB (minbase) | ~80 MB (slim images) |

This would require a new execution backend (a `DockerExec` alongside
`NspawnExec`), but the `WorkspaceExec` abstraction already cleanly separates
host vs container execution. The refactor surface is primarily in:

- `src/nspawn.rs` → new `src/docker_workspace.rs`
- `src/workspace_exec.rs` → add `Docker` variant to execution dispatch
- `src/workspace.rs` → workspace build pipeline for Docker images

The existing `WorkspaceType::Container` + `allow_container_fallback()` pattern
means this could be additive rather than replacing nspawn.

### 3.4 Recommendation

For the Docker image, **support three modes**:

1. **Host workspaces only** (default in Docker): No container isolation. The
   agent executes in the backend container directly. Works everywhere including
   macOS.
2. **nspawn inside Docker** (`--privileged`): Full container workspace support.
   Works on Linux and macOS (Docker Desktop runs a Linux VM with a real kernel,
   so `--privileged` grants access to namespaces and cgroups). Performance
   overhead on macOS due to the VM layer, but functionally correct.
3. **Docker-in-Docker** (future): Mount Docker socket, use Docker API for
   workspace isolation. More natural fit for Docker deployments.

The existing `SANDBOXED_SH_ALLOW_CONTAINER_FALLBACK` env var already handles
graceful degradation. Setting it to `true` in the Docker image's default env
makes host-mode the default without requiring code changes.

---

## 4. macOS Compatibility

### 4.1 What Works on macOS (via Docker Desktop)

| Feature | Works? | Notes |
|---------|--------|-------|
| Backend (API server) | Yes | Pure Rust, no OS-specific deps |
| Dashboard | Yes | Pure JS/Node |
| Host workspaces | Yes | Agent runs inside Linux container |
| SQLite database | Yes | `rusqlite` bundled |
| Git library sync | Yes | git available in container |
| Claude Code CLI | Yes | npm-installable inside container |
| OpenCode CLI | Yes | Binary available for linux/amd64 |
| Amp CLI | Yes | npm-installable inside container |
| OAuth flows | Yes | HTTP-only, no OS deps |

### 4.2 What Works with `--privileged` on macOS

Docker Desktop runs a real Linux VM (Apple Virtualization Framework or QEMU).
With `--privileged`, the Docker container gets access to the VM's Linux kernel
features (namespaces, cgroups, device nodes). This means:

| Feature | Works? | Notes |
|---------|--------|-------|
| **Container workspaces** (nspawn) | Yes | `--privileged` + `--cgroupns=host` needed. Performance overhead from VM layer (macOS → VM → Docker → nspawn) but functionally correct. `debootstrap` I/O is slower than native. |
| **Desktop automation** (Xvfb) | Yes | Xvfb runs inside the Docker container's Linux userspace. No host display needed (headless). |
| **Tailscale exit nodes** | Yes | `/dev/net/tun` + `CAP_NET_ADMIN` available in privileged mode. Network path is longer (extra VM NAT hop). |

### 4.3 What Does NOT Work on macOS (Even with `--privileged`)

| Feature | Why | Workaround |
|---------|-----|-----------|
| **Host X11 display streaming** | No X11 server on macOS host; Docker Desktop VM has no display | Use Xvfb inside container (headless) or VNC |
| **Shared host desktop** | The `DESKTOP_ENABLED` model assumes an X11 display on the host | Self-contained Xvfb inside the Docker image |

### 4.4 macOS Developer Experience

**Without `--privileged`** (simplest): A macOS user running `docker compose up`
gets everything except container workspace isolation. Missions execute inside the
Docker container directly (host mode). Fine for personal dev/demo use.

**With `--privileged`** (full feature parity): Adding `privileged: true` to the
compose file enables systemd-nspawn inside Docker Desktop's Linux VM. This gives
full container workspace isolation on macOS, with the only trade-off being
I/O performance during `debootstrap` and slightly higher memory usage from
the nested containerization layers.

---

## 5. Serving the Dashboard

### 5.1 Option A: Separate Container (Recommended for Production)

The dashboard runs as its own container. Benefits:
- Independent scaling and caching
- CDN-friendly (static assets served by Next.js)
- Can be replaced by Vercel deployment without touching backend

### 5.2 Option B: Backend Serves Dashboard (Simpler)

Embed the built dashboard as static files served by the Rust backend (e.g., via
`axum::routing::get_service` with `tower-http::services::ServeDir`). This would
require:
- Building the dashboard during Docker image build
- Adding a static file handler to the Axum router
- Setting `NEXT_PUBLIC_API_URL` to empty string (same-origin API)

**Trade-off**: Couples frontend and backend releases. Simplifies deployment to a
single container.

### 5.3 Option C: Built-in Reverse Proxy (All-in-One)

Include Caddy in the Docker image. Caddy proxies:
- `/` → Next.js dashboard (port 3001)
- `/api/*` → Rust backend (port 3000)

Both processes managed by a simple entrypoint script or supervisord. This is the
simplest UX: one container, one port, automatic TLS if given a domain.

---

## 6. Build Considerations

### 6.1 Rust Compilation

The Rust build is the bottleneck. Cargo downloads and compiles ~200 crates.

**Optimization strategies:**
- **cargo-chef** for layer caching: separate dependency fetch from source build
- **sccache** or **BuildKit cache mounts** for incremental builds
- Target `x86_64-unknown-linux-gnu` (and optionally
  `aarch64-unknown-linux-gnu` for ARM)
- The `rusqlite` `bundled` feature compiles SQLite from C source (avoids needing
  system libsqlite3)

Estimated image sizes:
- Builder stage: ~2 GB (Rust toolchain + deps)
- Runtime stage: ~150 MB (binaries + system deps)
- Dashboard: ~200 MB (Node runtime + built assets)
- All-in-one: ~350 MB

### 6.2 Dashboard Build

The dashboard needs `bun` (or `node` + `npm`) to build. The
`NEXT_PUBLIC_API_URL` environment variable is baked in at build time. For Docker:
- Build with `NEXT_PUBLIC_API_URL=""` (empty) for same-origin deployments
- Or build with a placeholder and override at runtime via Next.js runtime config

### 6.3 Multi-Architecture

Both Rust and Next.js support `linux/amd64` and `linux/arm64`. Docker buildx
can produce multi-arch manifests. The OpenCode binary is only available for
`linux/amd64` currently, which may limit ARM deployments.

---

## 7. Data & State

### 7.1 Persistent Volumes

| Path | Content | Volume? |
|------|---------|---------|
| `~/.sandboxed-sh.sh/` | SQLite DB, library, container rootfs, runtime state | Yes (critical) |
| `~/.sandboxed-sh.sh/library/` | Cloned library repo | Yes (or re-clone on start) |
| `~/.sandboxed-sh.sh/containers/` | nspawn rootfs (if using containers) | Yes (large, ~150 MB each) |
| `~/.claude/` | Claude Code OAuth credentials | Yes (for token persistence) |
| `~/.config/opencode/` | OpenCode config | Yes |

A single named volume at `/root/.sandboxed-sh.sh` covers most state. Credentials
should be injected via env vars or a secrets volume.

### 7.2 Configuration Injection

All configuration is via environment variables (see `Config::from_env()`).
Docker users pass these via `--env-file` or compose `env_file:`. No config
files need to be mounted.

---

## 8. Risks and Limitations

### 8.1 Privileged Mode for Container Workspaces

Running `--privileged` Docker containers is a security concern. The Sandboxed.sh
backend runs as root and has full system access by design (it manages
workspaces, spawns harnesses, runs arbitrary agent code). In the nspawn model,
the host kernel provides isolation. In Docker `--privileged`, the container
effectively has host-level access.

**Mitigation**: Document that `--privileged` is only needed for container
workspaces. Host-mode workspaces work without it.

### 8.2 Git SSH Keys

The library sync requires git access (potentially via SSH). Docker containers
need the SSH key injected. Options:
- Mount `~/.ssh` as a read-only volume
- Use `SSH_AUTH_SOCK` forwarding
- Use HTTPS with a personal access token instead of SSH
- Use Docker secrets

### 8.3 Harness Auto-Install

On first mission, Sandboxed.sh auto-installs Claude Code / OpenCode / Amp CLIs
via npm or curl. This requires internet access from the container. For air-gapped
environments, pre-install these in the Docker image.

### 8.4 Desktop Automation

Desktop tools (Xvfb, i3, Chromium, xdotool, scrot, tesseract) add ~500 MB to
the image. Most Docker users won't need desktop automation. Consider:
- A separate `sandboxed.sh/backend-desktop` image variant with desktop deps
- Or a flag to optionally install desktop deps at container start

---

## 9. Proposed Dockerfile Sketches

### 9.1 Backend Dockerfile

```dockerfile
# Stage 1: Build Rust binaries
FROM rust:1.75-bookworm AS builder
WORKDIR /build

# Cache dependencies via cargo-chef
RUN cargo install cargo-chef
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1.75-bookworm AS cook
RUN cargo install cargo-chef
COPY --from=builder /build/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json

FROM rust:1.75-bookworm AS compile
WORKDIR /build
COPY --from=cook /build/target target
COPY --from=cook /usr/local/cargo /usr/local/cargo
COPY . .
RUN cargo build --release --bin sandboxed_sh --bin desktop-mcp --bin workspace-mcp

# Stage 2: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git jq unzip openssh-client \
    && rm -rf /var/lib/apt/lists/*

# Install bun (for harness auto-install + MCP plugins)
RUN curl -fsSL https://bun.sh/install | bash \
    && install -m 0755 /root/.bun/bin/bun /usr/local/bin/bun \
    && install -m 0755 /root/.bun/bin/bunx /usr/local/bin/bunx

# Install npm (needed for claude code install)
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

COPY --from=compile /build/target/release/sandboxed_sh /usr/local/bin/
COPY --from=compile /build/target/release/desktop-mcp /usr/local/bin/
COPY --from=compile /build/target/release/workspace-mcp /usr/local/bin/

# Default: host workspaces, no nspawn
ENV SANDBOXED_SH_ALLOW_CONTAINER_FALLBACK=true
ENV HOST=0.0.0.0
ENV PORT=3000
ENV WORKING_DIR=/root

EXPOSE 3000
VOLUME ["/root/.sandboxed-sh.sh"]

CMD ["sandboxed_sh"]
```

### 9.2 Dashboard Dockerfile

```dockerfile
FROM oven/bun:1 AS builder
WORKDIR /app
COPY dashboard/package.json dashboard/bun.lock* ./
RUN bun install --frozen-lockfile
COPY dashboard/ .
ENV NEXT_PUBLIC_API_URL=""
RUN bun run build

FROM oven/bun:1-slim
WORKDIR /app
COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static
COPY --from=builder /app/public ./public
EXPOSE 3000
CMD ["bun", "server.js"]
```

### 9.3 All-in-One with Caddy

```dockerfile
FROM sandboxed.sh/backend AS backend
FROM sandboxed.sh/dashboard AS dashboard

FROM debian:bookworm-slim
# Install Caddy
RUN apt-get update && apt-get install -y caddy && rm -rf /var/lib/apt/lists/*

COPY --from=backend /usr/local/bin/sandboxed_sh /usr/local/bin/
COPY --from=dashboard /app /opt/dashboard

# Caddy reverse proxy config
RUN echo ':80 { \n\
  handle /api/* { reverse_proxy localhost:3000 } \n\
  handle { reverse_proxy localhost:3001 } \n\
}' > /etc/caddy/Caddyfile

COPY entrypoint.sh /entrypoint.sh
CMD ["/entrypoint.sh"]
# entrypoint.sh starts sandboxed_sh, next, and caddy
```

---

## 10. Summary: Decision Matrix

| Deployment Goal | Approach | nspawn? | macOS? | Desktop? |
|----------------|----------|---------|--------|----------|
| **Quick demo / dev** | `docker compose up` (host workspaces) | No | Yes | No |
| **Full features (Linux)** | Docker + `--privileged` | Yes | N/A | Yes |
| **Full features (macOS)** | Docker Desktop + `--privileged` | Yes | Yes | Headless only |
| **Production (recommended)** | Bare metal (current) | Yes | N/A | Yes |
| **Air-gapped** | Pre-built image with CLIs baked in | Optional | N/A | Optional |
| **Future: Docker workspaces** | Docker-in-Docker via socket mount | No (replaced) | Yes | No |

### Bottom Line

Dockerization is viable and valuable for:
1. **Lowering the barrier to entry** (one command to try Sandboxed.sh)
2. **macOS users** who can't run the bare-metal install
3. **Reproducible deployments** without the 13-step install guide

Container workspaces via nspawn-inside-Docker work on both Linux and macOS
(Docker Desktop provides a real Linux VM) when running with `--privileged`. The
macOS path adds a VM indirection layer that increases I/O latency during
`debootstrap` but is functionally equivalent for steady-state agent execution.

For the simplest possible onboarding, a non-privileged `docker compose up` with
host-mode workspaces gives users the full experience minus workspace isolation.
Adding `privileged: true` upgrades to full nspawn support on any platform.
