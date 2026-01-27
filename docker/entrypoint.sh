#!/bin/bash
set -e

# =============================================================================
# Open Agent — Docker Entrypoint
# Starts: (optional) Xvfb+i3, Rust backend, Next.js dashboard, Caddy (PID 1)
# =============================================================================

cleanup() {
    echo "[entrypoint] shutting down..."
    kill "$BACKEND_PID" "$DASHBOARD_PID" 2>/dev/null || true
    [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null || true
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

# -- Optional: Desktop (Xvfb + i3) -------------------------------------------
if [ "${DESKTOP_ENABLED:-false}" = "true" ]; then
    DISPLAY_NUM="${DESKTOP_DISPLAY:-:99}"
    RESOLUTION="${DESKTOP_RESOLUTION:-1920x1080}"

    echo "[entrypoint] starting Xvfb on ${DISPLAY_NUM} at ${RESOLUTION}"
    Xvfb "$DISPLAY_NUM" -screen 0 "${RESOLUTION}x24" -ac +extension GLX +render -noreset &
    XVFB_PID=$!
    export DISPLAY="$DISPLAY_NUM"

    # Wait for X to be ready
    for i in $(seq 1 20); do
        if xdpyinfo -display "$DISPLAY_NUM" >/dev/null 2>&1; then
            break
        fi
        sleep 0.2
    done

    echo "[entrypoint] starting i3 window manager"
    i3 &

    # Disable screensaver/DPMS
    xset s off 2>/dev/null || true
    xset -dpms 2>/dev/null || true
    xset s noblank 2>/dev/null || true
    xsetroot -solid "#1a1a2e" 2>/dev/null || true
fi

# -- Start Rust backend -------------------------------------------------------
echo "[entrypoint] starting open_agent backend on ${HOST:-127.0.0.1}:${PORT:-3000}"
open_agent &
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
