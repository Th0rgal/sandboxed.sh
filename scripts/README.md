# Sandboxed.sh Scripts

Small helper scripts for local development and packaging.

## Available Scripts

### smoke_harnesses_dev.sh
Unified dev smoke gate for model-routing and mission streaming.

Runs:
- `proxy_smoke.py` against `/v1/models` and `/v1/chat/completions`
- `mission_stream_smoke.py` across selected harness backends

Use `--help` for all options, including backend-specific model overrides and expected model assertions.

### harness_contract_tests.sh
Runs a curated set of fast cross-harness contract tests that guard event-conversion invariants and OpenCode SSE parsing behavior.
Used by CI job `harness-contract`.

### proxy_smoke.py
Smoke test for the OpenAI-compatible proxy routes:
- `GET /v1/models`
- `POST /v1/chat/completions` (streaming, and optional non-streaming)

### mission_stream_smoke.py
Mission API streaming smoke test across harnesses (`claudecode`, `opencode`, `codex` by default).

Validates:
- streaming thinking/text/tool events
- queued-message behavior
- assistant model metadata (with optional per-backend expectations)

### install_desktop.sh
Installs desktop automation dependencies on the host (used by the desktop MCP).

### generate_ios_icons.js
Generates iOS app icons for the SwiftUI dashboard.

### validate_skill_isolation.sh
Validates strong workspace skill isolation on the server (checks OpenCode env, global skill dirs, and latest mission skills).

### mission_debug_bundle.sh
Collects a mission-focused diagnostic bundle from control API endpoints (mission snapshot, events, tree, automations, progress, OpenCode diagnostics) and outputs a `.tar.gz` archive for triage.
