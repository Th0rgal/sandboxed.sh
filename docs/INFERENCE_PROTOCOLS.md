# Inference protocols and reasoning continuity

Sandboxed.sh exposes several inference protocols because no single wire format
preserves every provider's reasoning state faithfully.

The routing rule is:

> Start from the measured policy in the table below. Prefer a provider-native
> stateful protocol only when it is the selected policy, its capability is
> advertised, and the route can guarantee provider/model/account affinity.
> Provider-specific Chat replay is opt-in evidence, not an automatic upgrade;
> otherwise use ordinary Chat Completions.

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
| OpenAI reasoning models, including GPT-5.x | Chat Completions | visible transcript | capability-gated Responses as an explicit experimental cohort | Chat is the completed comparable baseline; Responses can retain provider reasoning items but must win a completed cohort before promotion |
| xAI Grok models that advertise Responses | Chat Completions | visible transcript | capability-gated Responses as an explicit experimental cohort | Chat is the completed baseline; the planned Responses cohort will test whether native state improves long tool loops |
| Muse Spark models | Responses | `previous_response_id` and `function_call_output` | Chat Completions | Benchmark evidence shows material improvement from retained reasoning continuity |
| Anthropic Claude/Opus/Fable with a direct API-key route | Chat Completions adapter | visible transcript | capability-gated Anthropic Messages as an explicit experimental cohort | Chat is the completed baseline; Messages preserves signed thinking blocks but must be measured before promotion |
| Kimi K3 and compatible Kimi thinking models | Ordinary Chat Completions | visible transcript | Chat with capability-gated `reasoning_content` replay as an explicit experimental mode | On STRAT-50 v0.2, ordinary Chat scored 14/50 versus 12/50 with replay, while using fewer tokens; fidelity support remains available but is not the empirical default |
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
2. For a stateful native session, require all three signals: the endpoint
   capability, `currently_available`, and its protocol-specific continuity flag.
   Responses requires `responses` plus `previous_response_id`; Anthropic Messages
   requires `anthropic_messages` plus `thinking_blocks_replay`. If the endpoint is
   available but its continuity flag is false, it may be used only as an
   explicitly stateless cohort; do not start a session that expects continuation.
3. For Kimi, use ordinary Chat Completions by default even when
   `reasoning_content_replay` is advertised. Enable replay only for an explicitly
   selected, separately labelled experimental cohort; that cohort must require
   the capability and preserve the complete assistant message on every subsequent
   tool turn.
4. Otherwise use ordinary Chat Completions.
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

- default to ordinary Chat transcript continuity (`content` and `tool_calls`)
  without replaying `reasoning_content`, even when replay is advertised;
- enable `reasoning_content` replay only through an explicit experimental-cohort
  configuration, never by capability discovery alone;
- in that replay cohort, retain and replay `reasoning_content`, `content`, and
  `tool_calls` unchanged in the complete assistant message;
- keep replay capability-gated so unrelated models do not receive synthetic
  fields.

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

Chat Completions remains the portable leaderboard baseline. A model's recommended
integration is the highest-scoring completed comparable cohort, not automatically
the most stateful protocol. Native-stateful and replay cohorts remain available as
separate evidence even when they lose. A gain can reflect both better continuity
and greater effective budget utilization, so report score and cost together.

## Current evidence and planned evaluations

Observed evidence motivating this architecture:

- Muse Spark Chat and Responses produced materially different solve rates and
  token use on the same frozen STRAT-50 panel.
- Grok Chat failures were dominated by repetition and absent Lean submissions,
  motivating a separate Responses ablation rather than rewriting its Chat score.
- Kimi responses already contained `reasoning_content`; the client was dropping
  it before later tool turns. Preserving it was protocol-faithful but did not
  improve this benchmark: ordinary Chat scored 14/50 at 1,595,122 tokens, while
  replay scored 12/50 at 1,868,907 tokens. Ordinary Chat therefore remains the
  recommended Kimi integration for this measured workload.
- Anthropic's Chat adapter cannot faithfully expose every signed native thinking
  block, motivating a separate Messages cohort.

Ordered evaluation roadmap:

1. Keep Chat Completions cohorts as the common baseline.
2. Retain ordinary Chat as the Kimi K3 winner; keep the completed
   `reasoning_content` replay cohort as a separately labelled negative ablation.
3. Run Grok via native xAI Responses after capability and affinity probes.
4. Run Claude Opus via native Anthropic Messages with thinking-block replay.
5. Run GPT-5.6 Sol via native Responses.
6. Consider GLM/MiniMax native-state experiments only when their provider contract
   advertises a real continuity mechanism.

Each lane requires an actual-shape preflight, native tool-call probe,
verifier-backed Lean canary, separate provenance, and infrastructure-invalid
retry exclusion before a full cohort is launched.
