# Provider usage in Orb

The Providers page loads subscription-key usage on its first visit as well as
OAuth usage. MiniMax and Z.AI do not require an OAuth account to expose their
coding-plan quota. Compact rows show used percentages; expanded rows show meters
and provider reset dates. Zero is valid; missing data is never rendered as zero
or interpreted as exhaustion.

- MiniMax: `GET https://www.minimax.io/v1/token_plan/remains` with the stored
  provider key. `current_*_remaining_percent` is remaining quota, so Orb inverts
  it to used percentage. Reset timestamps are milliseconds. Business response
  2062 means no active Token Plan on this key. Quota checks never send a chat
  completion to test connectivity.
- Z.AI: the existing quota monitor reports `zai_5h_used_percent` and
  `zai_weekly_used_percent`, plus reset epochs and plan. Orb uses those fields;
  old token-percentage fields remain a fallback for older backends.
- Meta Muse: the existing API-key integration is pay-as-you-go. A Muse Code
  subscription is a distinct CLI account/credential; no subscription quota is
  inferred from a generic API key. Connecting Muse Code accounts and reading
  their subscription usage is not implemented by this change.

Sources checked September 26, 2026:
- https://platform.minimax.io/docs/token-plan/faq
- https://dev.meta.ai/docs/muse-code/auth
- https://dev.meta.ai/help/subscriptions/what-is-a-muse-code-subscription
