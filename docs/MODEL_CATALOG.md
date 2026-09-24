# Model discovery and fallback snapshots

The catalog is a discovery aid, not proof that inference will succeed. A model
listed by an API can still reject a request because of protocol, quota,
subscription tier or parameters. Catalog operations never send inference requests.

## Sources and selection

The model_discovery module discovers **each enabled connection independently**.
Its identity includes the account UUID, exact base endpoint and access profile.
Only their SHA-256 fingerprint is persisted; credentials, endpoints and account
names are not stored in observations. A different route cannot reuse old results.

Selection happens per connection, then lists are unioned for the provider picker:

1. Complete successful discovery replaces that connection's snapshot.
2. Partial discovery supplements its compatible snapshot.
3. Failed, malformed or empty responses preserve the last successful discovery,
   marked as stale.
4. Without a successful discovery, use the compatible bundled snapshot.
5. Manually typed IDs remain possible and unverified.

Removing/disabling a connection excludes its observations when rendering, and
removes them from persisted state on refresh. An account union means at least
one account listed a model, not all accounts. It is never a universal entitlement.

Public models.dev data remains available for unconnected-provider exploration.
For connected providers it does not add selectable models beyond the effective
connection lists and cannot establish account access.

### Adapter matrix

| Access | Discovery | Completeness |
|---|---|---|
| OpenAI API | Configured endpoint /models, API bearer | Complete after pagination |
| Anthropic API | Configured /models, x-api-key and version header | Complete after pagination |
| Google API key | Configured Gemini /models, x-goog-api-key | Paginated; generateContent models only |
| Kimi Coding | Coding /models, account credential and coding User-Agent | Complete after pagination |
| OpenRouter API | Configured /models | Complete after pagination |
| xAI, Cerebras, Z.ai, MiniMax, Muse, other OpenAI-compatible APIs | Configured /models | Conservatively partial |
| Custom routers, including Spark | Configured /models | Complete after pagination |
| OpenAI/Anthropic/Google/xAI OAuth | Explicit unsupported_oauth status | Profile snapshot; no OAuth sent to API-key catalog |
| Bedrock, Azure, Cohere, Copilot | Explicit unsupported status | No invented generic adapter |

A provider returning just a selected model must remain partial. New complete
adapters require endpoint-contract tests. Unsupported OAuth discovery is explicit;
local harness model availability is a separate concern and can depend on CLI version.

The default endpoint for OpenAI-compatible discovery is shared with the inference
proxy. Z.ai defaults to https://api.z.ai/api/coding/paas/v4, not BigModel.
Its standard API endpoint uses a separate api profile. Unknown endpoint overrides
use a private custom profile with manually configured fallback models.

Legacy API keys in OpenCode files/environment become independent connections
when no enabled API-key store connection exists for that provider. OAuth-only
credentials in those files are never treated as API keys.

Discovery runs at startup and every MODEL_CATALOG_REFRESH_SECS (default 600).
Zero disables periodic refresh, not startup discovery. Refreshes are serialized.
Probes are limited to four concurrent calls, ten pages, ten seconds per page and
8 MB per response. Redirects are rejected. Errors expose a stable category or
HTTP status, never upstream response bodies.

## Runtime state and API

Runtime state: .sandboxed-sh/model-discovery.json under the backend working
directory. Temporary-file plus atomic-rename writes preserve successful observations
across restarts. This file is operational state, not source control.

Authenticated endpoints:

- POST /api/providers/catalog/refresh: refresh discovery and catalog.
- GET /api/providers/discovery: connection status, effective source, timestamps,
  last successful models/completeness and effective models.
- GET /api/providers/snapshots: portable snapshots from currently successful
  public-provider observations. Failed/stale routes are skipped, even when
  runtime selection still uses their previous results.
- Existing /api/providers and backend-model endpoints share this selection policy.

Attempt status is discovered, error or unsupported. Effective source is
discovery, stale_discovery or snapshot. An HTTP 429 is not an unsupported-model verdict.

Orb exposes Refresh models in the chain editor, with suggestion labels
“Listed by provider”, “Previously listed · refresh failed” and “Fallback / unverified”.
These labels never claim successful inference. Existing explicit chain tests are
separate and may consume quota; a successful chain does not validate each entry.

## Versioned fallback data

- catalog/providers.json: display metadata.
- catalog/snapshots/<provider>-<profile>.json: fallback model data.
- catalog/schema.json: portable snapshot schema.
- build.rs: automatically embeds every snapshot JSON file at compilation.

Initial files are curated migrations of the previous Rust lists, not fabricated
live discoveries. API/subscription profiles are separate even when seed lists
overlap. Snapshots contain no credentials, account IDs, emails or private URLs.

IDs are exact upstream IDs. Context size is metadata, not an invented suffix.
The validator rejects Z.ai glm-5.3[1m]; use glm-5.3. Compatibility normalization
remains in the inference proxy for old saved configurations.

## Operator workflow

Set SANDBOXED_API_URL and SANDBOXED_API_TOKEN (dashboard/API JWT). Do not include
tokens in commands, exports or checked-in files.

~~~sh
palomactl models discover --connected
palomactl models export --source last-success --output /tmp/model-export
palomactl models validate /tmp/model-export
palomactl models diff --snapshot-dir catalog/snapshots --from /tmp/model-export
palomactl models snapshot update --from /tmp/model-export --snapshot-dir catalog/snapshots
palomactl models validate catalog/snapshots
git diff -- catalog/
~~~

Export into a **fresh directory**. Export reads the last completed successful
observations; it does not itself refresh. Names are sanitized to model IDs and
descriptions omitted. Private routes and provider-returned free text are excluded.
Invalid IDs (URLs, emails, control characters) prevent a profile's export.

Diff reports additions, metadata changes and absences. Absence is authoritative
only for a complete observation. Snapshot update merges partial input, replaces
models for complete input, and preserves profiles missing from the input.
Empty exports and invalid input fail before file modifications.
Individual files are replaced atomically; a directory update is not a multi-file
transaction. Inspect the git diff before committing.

Discovery/export never edits chains, deploys, changes credentials or automatically
updates the repository. A reviewed snapshot change takes effect after rebuild/deploy.

## Testing and maintenance

~~~sh
cargo test --lib model_catalog
cargo test --lib model_discovery
cargo test --lib api::providers::tests
cargo test --bin palomactl models
cargo run --bin palomactl -- models validate catalog/snapshots
~~~

Tests cover complete/partial precedence, stale and empty response handling,
account/route isolation, restart persistence, pagination, redirect protection,
safe exports and non-destructive updates. HTTP tests use local mock servers.

Add a provider's profile, adapter/auth method and pagination/completeness tests
together. Do not assume that HTTP 200 means the list is complete. When reliable
discovery is unavailable, expose the limitation and retain a reviewed snapshot.

## Local Claude Code rejects a listed model

`[claude-code:unrecognized_model]` is emitted by the harness, independently of
provider catalog discovery. Check `claude --version` on the machine running the
conversation, run `claude update`, then retry. On 2026-09-24, the local 2.1.278
installation rejected `claude-opus-5-5`; after updating to 2.1.281, a minimal
request with that exact ID succeeded. This verifies that installation and account,
not a universal minimum-version or account-access guarantee. Do not replace a
valid upstream ID with an invented alias to work around an outdated CLI.
