# Grok Build CLI router assessment for Verity v0.2

## Decision

Do **not** expose `grok-cli/grok-4.5` from `/v1/chat/completions` yet. The
connected Grok Build account works for native `grok` missions, but the current
CLI transport cannot satisfy OpenAI caller-tool semantics with a small adapter.
Advertising it would make a builtin Verity/lean-lsp-mcp result non-comparable:
the CLI executes its own tools instead of returning genuine
`assistant.tool_calls` for the caller to execute and return.

This remains separate from `xai/grok-4.5`, which is the xAI API-key route.
Grok Build OAuth/login must not be relabelled or forwarded as `XAI_API_KEY` to
the xAI OpenAI-compatible API.

## Source trace (origin/master `8c3cf73d`)

`src/api/runners/grok.rs` starts `grok agent stdio` and speaks ACP JSON-RPC:

1. `initialize`
2. `session/load` or `session/new` with `mcpServers: []`
3. `session/set_model`, including `grok-4.5`
4. `session/prompt`

`session/prompt` returns completion metadata; text is delivered by
`session/update`. The runner tracks `stopReason`, `_meta.modelId`, optional
token usage, a 180-second idle limit, and mission cancellation. Its
`tool_call`/`tool_call_update` handling only observes tools Grok selected and
executed: it emits internal `AgentEvent::ToolCall`/`ToolResult` after the fact.
The only inbound request it answers is `session/request_permission`.

The legacy `grok -p --output-format streaming-json` fallback is explicitly
documented in the source as hiding tool execution, so it cannot be a router
adapter.

`src/api/proxy.rs` is an OpenAI Chat Completions router. Existing subscription
adapters forward or translate a complete compatible request; none turns an
agent's internally-executed tools into caller tool calls. Its xAI credential
gate requires an API key, preserving the `b15c3c5` boundary that keeps Grok
Build OAuth out of `api.x.ai/v1/chat/completions`.

## Blocking protocol mismatch

Verity sends `tools`, needs a completion with stable OpenAI tool call ids,
names, and JSON arguments, executes Lean MCP itself, then sends ordered
`role: tool` results on a later request. This is a caller-driven multi-request
conversation.

MCP servers available to ACP are not an equivalent bridge: Grok invokes them
while `session/prompt` is in flight. To translate one, the router would need to
suspend an MCP invocation, atomically return `finish_reason: "tool_calls"`,
retain the ACP child, correlate the later caller tool result, and release the
blocked invocation. The current runner has no session registry, pause/resume
mechanism, or bounded pending-tool lifecycle. Its mission-owned session/tool
ids are not an OpenAI caller contract.

| Requirement | Current runner | Router adapter now |
| --- | --- | --- |
| Native account/cache, model, text, stop reason, optional usage | Yes | Potentially |
| Cancellation and timeout | Mission-scoped | New owner required |
| Caller-defined functions | No | **Blocking** |
| Genuine `assistant.tool_calls` | No | **Blocking** |
| Ordered later `role: tool` results | No | **Blocking** |

## Smallest viable slice

Before advertising a route, build an opt-in ACP external-tool bridge: isolated
CLI session per router conversation, ephemeral allowlisted MCP server generated
from OpenAI function schemas, opaque persisted pending-call state, and a later
request that validates ordered tool results then resumes the blocked MCP call.
It must bound context, result sizes, pending state, cancellation, cleanup, and
process lifetime; use only non-secret cache/account detection; and map CLI
failures to redacted OpenAI infrastructure errors.

Test that bridge with fake ACP/MCP processes for fresh/stale detection, model
rewrite, two-turn tool ordering, explicit non-streaming semantics (or SSE),
cancellation/timeout, error mapping, and secret redaction. Only then advertise
`grok-cli/grok-4.5`; never repurpose `xai/grok-4.5`.

## External confirmation

xAI documents `grok agent stdio` as ACP over stdin/stdout, with prompt metadata
and `session/update` text, and separately documents MCP servers as tools for
Grok. Neither specifies an OpenAI external function-call round trip. See the
[headless/ACP guide](https://docs.x.ai/build/cli/headless-scripting) and
[MCP server guide](https://docs.x.ai/build/features/mcp-servers).
