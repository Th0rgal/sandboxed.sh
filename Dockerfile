# =============================================================================
# Open Agent — All-in-One Docker Image
# =============================================================================
# Multi-stage build: Rust backend + Next.js dashboard + runtime with AI CLIs
#
# Build:   docker build -t openagent .
# Run:     docker compose up
# =============================================================================

# ---------------------------------------------------------------------------
# Stage 1: Rust builder
# ---------------------------------------------------------------------------
FROM rust:1.88-bookworm AS rust-builder

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./

# Create stub source so cargo can resolve deps
RUN mkdir -p src/bin \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > src/bin/desktop_mcp.rs \
    && echo "fn main() {}" > src/bin/workspace_mcp.rs \
    && cargo build --release --lib 2>/dev/null || true \
    && cargo build --release 2>/dev/null || true

# Copy real source and build
COPY src/ src/
RUN cargo build --release --bin open_agent --bin desktop-mcp --bin workspace-mcp

# ---------------------------------------------------------------------------
# Stage 2: Dashboard builder
# ---------------------------------------------------------------------------
FROM oven/bun:1 AS dashboard-builder

WORKDIR /build/dashboard

COPY dashboard/package.json dashboard/bun.lock ./
RUN bun install --frozen-lockfile

COPY dashboard/ .

# Empty API URL — the Caddyfile proxies /api to the backend at runtime
ENV NEXT_PUBLIC_API_URL=""
RUN bun run build

# ---------------------------------------------------------------------------
# Stage 3: Runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# -- Core system deps --------------------------------------------------------
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl jq unzip openssh-client ca-certificates gnupg \
    # nspawn / container workspaces
    systemd-container debootstrap \
    # Desktop automation
    xvfb i3 x11-utils x11-xserver-utils xdotool scrot imagemagick \
    tesseract-ocr at-spi2-core \
    fonts-liberation fonts-dejavu-core fonts-noto \
    # Chromium
    chromium \
    && rm -rf /var/lib/apt/lists/*

# -- Node.js 20 (for Next.js standalone + Claude Code via npm) ---------------
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# -- Bun (for bunx + OpenCode plugins) ---------------------------------------
RUN curl -fsSL https://bun.sh/install | bash \
    && install -m 0755 /root/.bun/bin/bun /usr/local/bin/bun \
    && install -m 0755 /root/.bun/bin/bunx /usr/local/bin/bunx

# -- Caddy (reverse proxy) ---------------------------------------------------
RUN curl -fsSL 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
      | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/caddy-stable-archive-keyring.gpg] https://dl.cloudsmith.io/public/caddy/stable/deb/debian any-version main" \
      > /etc/apt/sources.list.d/caddy-stable.list \
    && apt-get update && apt-get install -y --no-install-recommends caddy \
    && rm -rf /var/lib/apt/lists/*

# -- Copy Rust binaries from builder -----------------------------------------
COPY --from=rust-builder /build/target/release/open_agent /usr/local/bin/open_agent
COPY --from=rust-builder /build/target/release/desktop-mcp /usr/local/bin/desktop-mcp
COPY --from=rust-builder /build/target/release/workspace-mcp /usr/local/bin/workspace-mcp

# -- Copy dashboard standalone build ------------------------------------------
COPY --from=dashboard-builder /build/dashboard/.next/standalone /opt/dashboard
COPY --from=dashboard-builder /build/dashboard/.next/static /opt/dashboard/.next/static
COPY --from=dashboard-builder /build/dashboard/public /opt/dashboard/public

# -- Pre-install AI harness CLIs (optional — agents still work if these fail) -
RUN npm install -g @anthropic-ai/claude-code@latest \
    && echo "[docker] Claude Code CLI installed: $(claude --version 2>/dev/null || echo 'unknown')" \
    || echo "[docker] WARNING: Claude Code CLI install failed (will be installed on first mission)"
RUN curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path \
    && install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode \
    && echo "[docker] OpenCode CLI installed: $(opencode --version 2>/dev/null || echo 'unknown')" \
    || echo "[docker] WARNING: OpenCode CLI install failed (will be installed on first mission)"
RUN npm install -g @anthropic-ai/amp@latest \
    && echo "[docker] Amp CLI installed: $(amp --version 2>/dev/null || echo 'unknown')" \
    || echo "[docker] WARNING: Amp CLI install failed (will be installed on first mission)"

# -- i3 config (from install_desktop.sh) -------------------------------------
RUN mkdir -p /root/.config/i3
COPY docker/i3config /root/.config/i3/config

# -- Caddy config + entrypoint -----------------------------------------------
COPY docker/Caddyfile /etc/caddy/Caddyfile
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# -- Working directories ------------------------------------------------------
RUN mkdir -p /root/.openagent /root/.claude /root/work/screenshots

# -- Environment defaults -----------------------------------------------------
ENV HOST=127.0.0.1 \
    PORT=3000 \
    WORKING_DIR=/root \
    DEV_MODE=true \
    DESKTOP_ENABLED=true \
    DESKTOP_RESOLUTION=1920x1080 \
    GIT_TERMINAL_PROMPT=0

EXPOSE 80
VOLUME ["/root/.openagent", "/root/.claude"]

ENTRYPOINT ["/entrypoint.sh"]
