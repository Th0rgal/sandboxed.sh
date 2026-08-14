# Inference protocols and reasoning continuity

Sandboxed.sh exposes several inference protocols because no single wire format
preserves every provider's reasoning state faithfully.

The routing rule is:

> Prefer the provider-native stateful protocol when its capability is advertised
> and the route can guarantee provider/model/account affinity. Otherwise preserve
> the provider's explicit reasoning field over Chat Completions. Fall back to
> ordinary Chat Completions when neither option is available.

Protocol selection must use `GET /v1/capabilities`. Clients must not infer support
from a model-name prefix or silently translate one protocol into another.

## Endpoints

| Endpoint | Purpose |
|---|---|
| `POST /v1/chat/completions` | Portable OpenAI-compatible baseline and universal fallback |
| `POST /v1/responses` | Native OpenAI/xAI/Muse stateful inference |
| `POST /v1/messages` | Native Anthropic Messages inference |
| `GET /v1/capabilities` | Credential-aware protocol and continuity discovery |

Native endpoints rewrite only the selected `model`. Protocol-specific request
items, response bodies, and SSE events are relayed without Chat translation.

## Preferred protocol by provider and model family

| Provider / model family | Preferred protocol | Continuity contract | Fallback | Rationale |
|---|---|---|---|---|
| OpenAI reasoning models, including GPT-5.x | Responses | `previous_response_id` plus native response items | Chat Completions | Responses can retain provider reasoning items across tool turns |
| xAI Grok models that advertise Responses | Responses | `previous_response_id` and `function_call_output` | Chat Completions | Long tool loops benefit from native state; Chat remains the comparable baseline |
| Muse Spark models | Responses | `previous_response_id` and `function_call_output` | Chat Completions | Benchmark evidence shows material improvement from retained reasoning continuity |
| Anthropic Claude/Opus/Fable with a direct API-key route | Anthropic Messages | replay signed `thinking`/`redacted_thinking`, `tool_use`, and `tool_result` blocks unchanged | Chat Completions adapter | Messages is Claude's native structured protocol; Chat translation can discard signed thinking blocks |
| Kimi K3 and compatible Kimi thinking models | Chat Completions with `reasoning_content` replay | preserve the complete assistant message, including `reasoning_content` and `tool_calls` | ordinary Chat Completions | Kimi exposes continuity in an explicit Chat field; Responses emulation would add no native guarantee |
| Z.AI GLM, MiniMax, and other OpenAI-compatible models without advertised native state | Chat Completions | visible transcript only, plus any explicitly advertised replay field | ordinary Chat Completions | Do not claim stateful reasoning without a provider contract and verified route |
| OAuth/CLI adapters | Chat Completions unless native support is explicitly advertised | adapter-defined | Chat Completions | Subscription OAuth credentials are not automatically valid native API credentials |

This table is a policy default, not proof that the preferred protocol always
scores better. Each model/protocol pair remains a separate benchmark cohort.

## Capability discovery

`GET /v1/capabilities` reports each visible model or chain and its provider
entries. Relevant fields include:

- `chat_completions`
- `responses`
- `anthropic_messages`
- `previous_response_id`
- `reasoning_content_replay`
- `thinking_blocks_replay`
- `native_function_tools`
- `currently_available`

Protocol support and health are distinct:

- a configured capability remains visible while an account is temporarily in
  cooldown;
- `currently_available` reports whether at least one matching account is healthy;
- stateful continuity flags (`previous_response_id`, `thinking_blocks_replay`)
  are cleared unless the route guarantees singleton provider/model/account
  affinity. `reasoning_content_replay` is exempt: it is stateless — the client
  replays the reasoning field inside each Chat request — so it needs no account
  affinity and stays set whenever the route is otherwise usable.

A client should select a protocol in this order:

1. Fetch capabilities for the exact requested model or chain.
2. Prefer the native protocol from the policy table when its capability is true
   and `currently_available` is true.
3. For Kimi-style Chat extensions, preserve the advertised reasoning field in the
   complete assistant message on every subsequent tool turn.
4. Otherwise use Chat Completions.
5. If the preferred route is temporarily unavailable, either wait according to
   `Retry-After` or start a separately labelled Chat fallback session. Never move
   an existing stateful continuation to another provider, model, or account.

## Stateful affinity and fail-closed behavior

Response IDs, tool-call IDs, and signed thinking blocks are upstream-context
bound. A stateful continuation is accepted only when routing resolves to:

- one chain entry;
- one configured account;
- one currently resolved account.

If affinity cannot be guaranteed, the proxy returns
`stateful_affinity_required` rather than replaying state against a different
provider or account.

A Chat fallback is a **new session**. It must not be presented as continuation of
a native Responses or Messages session.

## Client responsibilities

### Responses clients

- retain the returned response ID;
- send it as `previous_response_id`;
- return tool results as `function_call_output` with the original call ID;
- do not rebuild a parallel Chat transcript and call it equivalent.

### Anthropic Messages clients

- retain signed `thinking` and `redacted_thinking` blocks unchanged;
- preserve their order relative to `tool_use` blocks;
- return matching `tool_result` blocks;
- do not convert thinking blocks to plain assistant text.

### Kimi Chat clients

- retain the complete assistant message;
- replay `reasoning_content`, `content`, and `tool_calls` unchanged;
- do not log the reasoning field and then omit it from the next request;
- keep this behavior capability-gated so unrelated models do not receive
  synthetic fields.

## Benchmark methodology

Wire protocol is part of cohort identity. Results may be compared, but must not
be silently merged when any of these differ:

- endpoint (`chat/completions`, `responses`, or `messages`);
- reasoning continuity policy;
- provider/model/account affinity policy;
- request shape and sampling policy;
- harness commit.

Every cohort should record:

- exact model ID and provider;
- protocol and native/adapted status;
- capability snapshot;
- harness and benchmark SHAs;
- panel hash and proof budget;
- request-shape policy;
- valid terminal verdicts, infrastructure retries, tokens, and requests.

Chat Completions remains the portable leaderboard baseline. Native-stateful
cohorts measure the best faithful integration available for a model. A gain in a
native cohort can reflect both better continuity and greater effective budget
utilization, so report score and cost together.

## Current evidence and planned evaluations

Observed evidence motivating this architecture:

- Muse Spark Chat and Responses produced materially different solve rates and
  token use on the same frozen STRAT-50 panel.
- Grok Chat failures were dominated by repetition and absent Lean submissions,
  motivating a separate Responses ablation rather than rewriting its Chat score.
- Kimi responses already contained `reasoning_content`; the client was dropping
  it before later tool turns.
- Anthropic's Chat adapter cannot faithfully expose every signed native thinking
  block, motivating a separate Messages cohort.

Ordered evaluation roadmap:

1. Keep Chat Completions cohorts as the common baseline.
2. Complete Kimi K3 with `reasoning_content` replay.
3. Run Grok via native xAI Responses after capability and affinity probes.
4. Run Claude Opus via native Anthropic Messages with thinking-block replay.
5. Run GPT-5.6 Sol via native Responses.
6. Consider GLM/MiniMax native-state experiments only when their provider contract
   advertises a real continuity mechanism.

Each lane requires an actual-shape preflight, native tool-call probe,
verifier-backed Lean canary, separate provenance, and infrastructure-invalid
retry exclusion before a full cohort is launched.
