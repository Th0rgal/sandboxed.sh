#!/bin/bash
set -e

# =============================================================================
# Open Agent — Docker Entrypoint
# Starts: Rust backend, Next.js dashboard, Caddy (PID 1)
# =============================================================================

cleanup() {
    echo "[entrypoint] shutting down..."
    kill "$BACKEND_PID" "$DASHBOARD_PID" 2>/dev/null || true
    wait
}
trap cleanup SIGTERM SIGINT

# -- Git / SSH setup ----------------------------------------------------------
# SSH keys may be mounted read-only from the host. Set up a writable
# known_hosts and pre-populate with GitHub/GitLab keys if needed.
if [ -d /root/.ssh ]; then
    KNOWN_HOSTS="/root/.ssh/known_hosts"
    # If the mount is read-only, use a writable location
    if ! touch "$KNOWN_HOSTS" 2>/dev/null; then
        KNOWN_HOSTS="/root/.ssh_known_hosts"
        cp /root/.ssh/known_hosts "$KNOWN_HOSTS" 2>/dev/null || true
        export GIT_SSH_COMMAND="ssh -o UserKnownHostsFile=$KNOWN_HOSTS"
    fi
    # Add common forge host keys if missing
    if ! grep -q "github.com" "$KNOWN_HOSTS" 2>/dev/null; then
        echo "[entrypoint] adding GitHub/GitLab SSH host keys"
        ssh-keyscan -t ed25519,rsa github.com gitlab.com >> "$KNOWN_HOSTS" 2>/dev/null || true
    fi
fi

# Desktop sessions are started on demand by desktop-mcp/tools as isolated
# headless Wayland compositors. The container entrypoint intentionally does not
# start a shared desktop.


# -- Start Rust backend -------------------------------------------------------
echo "[entrypoint] starting sandboxed-sh backend on ${HOST:-127.0.0.1}:${PORT:-3000}"
sandboxed-sh &
BACKEND_PID=$!

# Wait for backend to become healthy before starting dashboard/Caddy
echo "[entrypoint] waiting for backend health..."
for i in $(seq 1 30); do
    if curl -sf http://127.0.0.1:${PORT:-3000}/api/health >/dev/null 2>&1; then
        echo "[entrypoint] backend ready"
        break
    fi
    if [ "$i" -eq 30 ]; then
        echo "[entrypoint] WARNING: backend not healthy after 15s, continuing anyway"
    fi
    sleep 0.5
done

# -- Start Next.js dashboard --------------------------------------------------
echo "[entrypoint] starting Next.js dashboard on port 3001"
cd /opt/dashboard
PORT=3001 HOSTNAME=127.0.0.1 node server.js &
DASHBOARD_PID=$!
cd /

# -- Start Caddy (foreground — PID 1) ----------------------------------------
echo "[entrypoint] starting Caddy reverse proxy on :80"
exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
