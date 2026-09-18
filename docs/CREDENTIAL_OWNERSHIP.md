# OAuth credential ownership

Anthropic and OpenAI OAuth refresh tokens rotate: each refresh returns a new
refresh token and revokes the previous one. Two processes holding copies of the
same token race each other, and the loser is stuck with `invalid_grant` until
the user logs in again. That was the cause of the recurring "login expired"
state for Claude Code and Codex.

## Policy

`src/api/oauth_owner.rs` decides who owns a provider's OAuth credential.

- **CLIProxyAPI owns** every provider it holds a refreshable auth file for
  (`claude-*.json`, `codex-*.json`, `xai-*.json` under `CLI_PROXY_AUTH_DIR`).
  sandboxed.sh never refreshes, mints or rotates tokens for those providers:
  the proactive refresher skips them, every on-demand refresher is a no-op,
  and harnesses are pointed at the proxy with the proxy key instead of
  receiving an OAuth file.
- **sandboxed.sh owns** API-key providers, Kimi OAuth, Google OAuth, and any
  OAuth provider the proxy has no file for.
- The Grok CLI harness keeps its own cached login in `~/.grok/auth.json`
  (its ACP transport accepts nothing else). Treat that as a second login on
  the same account, never a copy of the proxy's token.

Switch: `SANDBOXED_OAUTH_OWNER=cli-proxy` (default) or `legacy` (sandboxed.sh
refreshes its own copies, pre-refactor behaviour). `CLAUDE_CODE_DISABLE_CLI_PROXY=1`
also forces legacy.

## What each harness does when the proxy owns the credential

| Harness | Mechanism |
|---|---|
| Claude Code | `ANTHROPIC_BASE_URL`/`ANTHROPIC_API_KEY` point at the proxy; any mission `.claude/.credentials.json` is deleted; host tiers are not read, copied or back-synced. |
| Codex | `auth.json` in apikey mode with the proxy key; `config.toml` gets `model_provider = "cliproxy"` and a `[model_providers.cliproxy]` section (`wire_api = "responses"`, `env_key = "OPENAI_API_KEY"`); `OPENAI_API_KEY` env carries the proxy key. Rotation sees one `cliproxy` credential; CLIProxyAPI rotates across its own accounts. |
| OpenCode | `opencode.json` gets native `anthropic`/`openai` blocks with `options.baseURL` = proxy `/v1` and the proxy key; the workspace `auth.json` entries become `type: "api"` with the proxy key; the OAuth plugins are not installed. |
| Inference proxy (`/v1/*`) | Anthropic OAuth records are no longer hoisted to a Bearer token; all Anthropic OAuth traffic takes the existing CLIProxyAPI branch. |

Containers: CLIProxyAPI listens on the host loopback. A container workspace
needs a shared network to reach it; host workspaces always can.

## Operator checklist

- Log every account into CLIProxyAPI (`cli-proxy-api -login`, `-codex-login`,
  `-grok-login`) rather than into the dashboard; the dashboard login flows
  still write sandboxed.sh's own store and are only used in legacy mode.
- `journalctl -u sandboxed-sh-prod | grep invalid_grant` should stay empty.
- `stat /var/lib/cli-proxy-api/auth/*.json` should keep advancing.
