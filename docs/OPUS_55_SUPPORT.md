# Claude Opus 5.5

Added 23 September 2026. The shared provider catalog exposes native
`claude-opus-5-5` and OpenRouter's `anthropic/claude-opus-5.5` seed. Orb and the
web dashboard consume `/api/providers/backend-models`; they need a catalog
refresh, not a separate hardcoded model list. Existing explicit model choices
and configured defaults are preserved.

Claude Code uses effort with adaptive thinking for this model. The runner does
not inject a manual thinking budget and removes incompatible profile environment
settings. The stale-thinking recovery path also keeps adaptive thinking enabled
and retains effort. Native Messages forwarding preserves the harness request
and its signed blocks; no cross-model native session conversion was added.

The pricing table recognizes native, provider-qualified, dotted and dated IDs
without falling back to Opus 5 pricing. Published standard rates are $4 input,
$20 output, $5 five-minute cache write, and $0.20 cache read per million tokens.
As with other subscription models, these rates are token-cost estimates, not a
claim that a subscription incurs API billing.

References checked 23 September 2026:

- https://platform.claude.com/docs/en/models/opus-5-5/overview
- https://platform.claude.com/docs/en/models/opus-5-5/whats-new-opus-5-5
- https://openrouter.ai/anthropic/claude-opus-5.5

Validation: four targeted catalog/pricing/reasoning tests pass, as do the 13
cost tests and six model-policy tests (the pricing test overlaps these counts).
Production receipts are recorded below after live verification.

## Production verification

Backend commit `4a8fffc62124` deployed through the guarded deploy endpoint on
23 September 2026. Linux debug builds succeeded; fleet health is `ok` and
`sandboxed-sh-prod` / `hermes-assistant` are active.

- Core Claude Code mission `deacc772-e195-48d6-a37e-0826b2b389cf` initially
  exposed an obsolete 2.1.257 installer pin. The default is now a minimum of
  2.1.280; newer fleet-managed versions are accepted, while explicit operator
  pins retain exact matching. Regression test passes. After deployment, Bash
  returned 42 and a subsequent turn recalled 42. Native session JSONL records
  `claude-opus-5-5` as the response model.
- Orb's live model selector displayed Opus 5.5. A mission launched through
  Orb on Ashur, `6601f944-1099-4d97-bca9-ab78875bbd56`, ran the deterministic
  Bash test and returned 42; node job `956e380f-1b78-4b81-9c5a-0052bcea0fb4`
  succeeded with exit 0.
- OpenCode mission `38d8d9ab-d1e1-42b5-9107-070cdaf6f80b` exposed a separate
  multi-account export bug: the last stored Anthropic account overwrote a
  fresher OAuth token with an expired one. Export now retains the greatest
  expiry for each provider; a regression test covers both account orders.
  After that fix, the request reached Anthropic, but the installed
  `opencode-anthropic-auth` 0.0.13 plugin identifies itself as
  `claude-cli/2.1.2`. Anthropic rejects that version for Opus 5.5. OpenCode's
  subscription path remains unsupported in this validation; no user-agent
  rewrite was introduced to conceal this incompatibility.

The native Claude Code path is verified on Core and a remote node. The
OpenRouter catalog seed is present but OpenRouter inference was not tested.
Existing model defaults are unchanged. Orb and dashboard receive this addition
from the production catalog without a frontend-specific model-list release.
