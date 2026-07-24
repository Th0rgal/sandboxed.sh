# Hermes Assistant Migration

This document captures the target architecture for replacing the built-in
Telegram assistant path with a standalone Hermes assistant connected to
sandboxed.sh over MCP.

## Current Architecture

The existing Telegram assistant is not just a workspace. It is a backend-owned
assistant stack:

- `src/api/telegram.rs` owns Telegram webhook routing, trigger filtering,
  per-chat mission creation, file download, streaming edits, Paloma commands,
  proactive mission cards, scheduled messages, workflow relays, and memory.
- `src/api/control/mod.rs` exposes the Telegram CRUD endpoints and routes webhook
  updates into `ControlCommand::UserMessage`.
- `dashboard/src/app/assistant/page.tsx` is the new top-level UI surface,
  while `dashboard/src/app/settings/telegram/page.tsx` redirects for
  compatibility.
- Standalone bot mode creates a placeholder assistant mission and then one
  assistant-mode mission per Telegram chat.

This makes Telegram the assistant runtime. The agent loop, memory policy,
Telegram transport, mission steering, and proactive notification logic are all
coupled inside sandboxed.sh.

## Target Architecture

Hermes should become the assistant runtime. Sandboxed.sh should become the
workspace, mission, model-routing, and control provider.

```text
Telegram / other chat
  -> Hermes messaging gateway
  -> Hermes agent session + memory
  -> assistant-mcp
  -> sandboxed.sh control/workspace/model APIs
```

The assistant should run from a dedicated sandboxed workspace, preferably the
existing `assistant` container workspace. It should be managed as a persistent
service, not as an assistant-mode mission that receives every Telegram message.

This matches the upstream Hermes shape as of 2026-05-29: Hermes has a single
messaging gateway process for Telegram and other platforms, supports Telegram
text, images, files, typing, and streaming edits, supports custom
OpenAI-compatible model endpoints with `provider: custom`, and loads stdio or
HTTP MCP servers from `mcp_servers` in `config.yaml`.

References:

- https://hermes-agent.nousresearch.com/docs/user-guide/messaging/
- https://hermes-agent.nousresearch.com/docs/user-guide/messaging/telegram/
- https://hermes-agent.nousresearch.com/docs/user-guide/configuration/
- https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp/

## Experimental MCP

This branch adds `assistant-mcp`, a narrow MCP server for Hermes. It intentionally
does not expose deployment or durable-job tools.

Tools:

- `list_active_missions`
- `list_missions`
- `get_mission`
- `get_mission_digest`
- `get_mission_events`

`get_mission` and `get_mission_digest` both return the compact mission digest.
Use bounded `get_mission_events` pages when transcript or trace details are
actually required; full histories are not injected by the status tools.

- `get_mission_health` — diagnose stalls/loops/errors + a one-line recommendation
- `get_mission_diagnostics` — tool-call timeline, repeated calls, error events
- `start_mission`
- `list_mission_shared_files`
- `download_shared_file`
- `send_message_to_mission`
- `update_mission_settings` — switch backend/model/effort/agent for the next turn (between-turns)
- `resume_mission` — restart an interrupted/blocked/failed mission, optionally with a steering hint
- `cancel_mission`
- `list_workspaces`
- `get_workspace`
- `create_workspace`
- `update_workspace`
- `delete_workspace` — requires explicit confirmation
- `list_workspace_templates`
- `get_workspace_template`
- `save_workspace_template`
- `delete_workspace_template` — requires explicit confirmation
- `rebuild_workspace_from_template` — reapplies template fields and force-rebuilds; requires explicit confirmation
- `workspace_bash`

Configuration:

```text
SANDBOXED_API_URL=https://agent-backend-dev.thomas.md
SANDBOXED_API_TOKEN=<token when auth is enabled>
ASSISTANT_DEFAULT_WORKSPACE_ID=<workspace uuid>
HERMES_SANDBOXED_API_URL=https://agent-backend-dev.thomas.md
HERMES_SANDBOXED_API_TOKEN=<optional static token>
JWT_SECRET=<preferred for auth-enabled deployments; assistant-mcp mints service JWTs>
HERMES_ASSISTANT_USER_ID=<single-tenant mission owner, usually default in prod and dev in dev>
HERMES_DEFAULT_WORKSPACE_ID=<workspace uuid>
```

Security choices from the dev smoke:

- Tool output is recursively scrubbed for secret-like keys and values.
- Mission list tools return compact summaries instead of raw mission rows.
- Detailed mission/event access requires explicit tool calls.

For exceptional read-only research or design-conflict questions, Hermes may
start `backend=chatgpt_ui` with `model_override=gpt-5.6-pro` and `writer=false`.
The mission is asynchronous like every other Sandboxed mission. Generated
ChatGPT files are available through `list_mission_shared_files` followed by
`download_shared_file`; repository work remains owned by normal coding
backends.

## Readiness Checks

After deploying `assistant-mcp`, sandboxed.sh exposes its install status through
the existing system components endpoint. The Assistant dashboard reads the same
component record, so this API is the operator source of truth for the MCP bridge:

```bash
curl -fsS https://agent-backend-dev.thomas.md/api/system/components \
  | jq -c '.components[] | select(.name == "assistant_mcp")'
```

Expected dev output once the bridge is installed:

```json
{"name":"assistant_mcp","version":"0.1.0","installed":true,"update_available":null,"path":"/usr/local/bin/assistant-mcp","status":"ok"}
```

The same endpoint also reports the external Hermes runtime as
`hermes_assistant` when `hermes-assistant-dev.service` or
`hermes-assistant.service` is installed on the host. It is ready only when the
systemd service is loaded and active; otherwise the Assistant dashboard keeps the
runtime card in a cutover-pending state.

For an end-to-end stdio MCP smoke against dev, run:

```bash
scripts/assistant_mcp_smoke.sh --base-url https://agent-backend-dev.thomas.md
```

After `hermes-assistant-dev.service` is installed and active, add
`--require-hermes-runtime` to make the same smoke fail unless the runtime service
is also ready.

```bash
scripts/assistant_mcp_smoke.sh \
  --base-url https://agent-backend-dev.thomas.md \
  --require-hermes-runtime
```

The component is ready for Hermes only when `installed` is `true` and `status` is
`ok`. Gateway and runtime readiness still need to come from the Hermes service
itself, because this repository only owns the sandboxed.sh API/UI and MCP bridge.

## Model Routing

Hermes should use the sandboxed.sh OpenAI-compatible proxy as its model
endpoint:

```yaml
model:
  provider: custom
  base_url: https://agent-backend.thomas.md/v1
  api_key: ${SANDBOXED_PROXY_KEY}
  model: builtin/smart
```

This keeps GLM, MiniMax, custom inference, and fallback chains in one place:
the existing Routing UI and `/v1/models` proxy surface.
`builtin/smart` is the safer Hermes default because it currently starts with a
MiniMax route that emits visible OpenAI-compatible `message.content`; GLM 5.1
can emit long `reasoning_content` before visible text, which some Hermes
gateway flows treat as an empty provider response.

## Telegram Migration

Hermes already has a Telegram-capable messaging gateway, so the preferred cutover
is not to port sandboxed.sh's Telegram webhook code into Hermes. Instead:

1. Keep the old Telegram backend path active behind compatibility flags.
2. Add an `Assistant` dashboard tab that controls Hermes config, service status,
   selected model chain, MCP permissions, and compatibility gateway settings.
3. Configure Hermes gateway for Telegram using the existing bot token and
   allowed users.
4. Point Hermes at `assistant-mcp` for sandboxed mission control.
5. Disable sandboxed.sh webhook registration for that bot after Hermes is live,
   because Telegram only supports one active webhook per bot.
6. Migrate or archive Paloma-specific tables after command parity is tested.

## Deployment Shape

Deploy these artifacts to dev first:

- `sandboxed-sh-dev` only if backend APIs/UI change.
- `assistant-mcp` beside the existing MCP binaries.
- A persistent `hermes-assistant-dev.service` running in the assistant workspace.

This repository does not ship the Hermes runtime, but it now defines the
sandboxed.sh side of the service contract:

- `docs/examples/hermes-assistant-dev.env.example` lists the API, model proxy,
  Telegram gateway, and MCP bridge variables Hermes needs.
- `docs/examples/hermes-config.yaml.example` points Hermes at the sandboxed.sh
  `/v1` model proxy and registers `assistant-mcp` as a stdio MCP.
- `docs/examples/hermes-assistant-dev.service.example` shows a minimal systemd
  shape for `hermes gateway --accept-hooks run`.

## Dev Runtime Install Checklist

The dev host has been tested with Hermes `v0.15.1` installed at
`/usr/local/bin/hermes`. Use the example files as the sandboxed.sh-side
contract:

```bash
install -d -m 0755 /etc/sandboxed-sh
install -d -m 0755 /var/lib/hermes-assistant-dev/workspace
install -m 0600 docs/examples/hermes-assistant-dev.env.example \
  /etc/sandboxed-sh/hermes-assistant-dev.env
$EDITOR /etc/sandboxed-sh/hermes-assistant-dev.env

install -m 0600 docs/examples/hermes-config.yaml.example \
  /var/lib/hermes-assistant-dev/config.yaml
$EDITOR /var/lib/hermes-assistant-dev/config.yaml

install -m 0644 docs/examples/hermes-assistant-dev.service.example \
  /etc/systemd/system/hermes-assistant-dev.service
$EDITOR /etc/systemd/system/hermes-assistant-dev.service

systemctl daemon-reload
systemctl enable --now hermes-assistant-dev.service
systemctl status hermes-assistant-dev.service --no-pager
```

On the current dev deployment, sandboxed.sh runtime state lives under:

```text
/var/lib/sandboxed-sh-dev/.sandboxed-sh/workspaces.json
/var/lib/sandboxed-sh-dev/.sandboxed-sh/missions/missions-dev.db
```

The active legacy Telegram row can be used to seed `TELEGRAM_BOT_TOKEN`.
If `allowed_chat_ids` is empty, configure `TELEGRAM_ALLOWED_USERS` before prod.
`GATEWAY_ALLOW_ALL_USERS=true` is acceptable only for a temporary dev smoke.

Then verify sandboxed.sh can see the runtime:

```bash
curl -fsS https://agent-backend-dev.thomas.md/api/system/components \
  | jq -c '.components[] | select(.name == "hermes_assistant")'

scripts/assistant_mcp_smoke.sh \
  --base-url https://agent-backend-dev.thomas.md \
  --require-hermes-runtime
```

Do not move Telegram webhook ownership until the runtime component reports
`installed: true` and `status: "ok"`.

Only promote to production after:

- Hermes gateway can receive Telegram DMs.
- `assistant-mcp` can list missions and start a dev mission.
- Routing uses `builtin/smart` or the selected custom chain.
- Secret redaction is verified on workspace and mission outputs.
- Old webhook and Hermes gateway are not both claiming the same bot token.
