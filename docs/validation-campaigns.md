# Validation campaigns

Validation campaigns pin one candidate and evaluate a project-owned DAG of
gates. Development gates may reuse a persistent workspace cache; only required
`clean` gates can produce a certifying campaign.

Projects define `.sandboxed/validation.toml`:

```toml
version = 1
project = "verity"
profile = "lean-4.31"

[[gates]]
id = "compiler-targeted"
command = ["lake", "build", "Compiler"]
mode = "incremental"
timeout_secs = 1800

[[gates]]
id = "ci-local"
command = ["lake", "build"]
dependencies = ["compiler-targeted"]
mode = "clean"
timeout_secs = 7200
```

Gate `dependencies` unlock only on an exact-head pass of every dependency. A
required gate may not depend on an optional gate: the matrix is rejected at
validation time, because an optional gate's failure would leave the required
dependant pending forever without failing the campaign. When set,
`candidate.expected_head` must equal `candidate.commit` — exact receipts
certify the pinned candidate commit, never a different revision.

Create a campaign with `POST /api/validation-campaigns/from-workspace`, claim
dispatchable gates (status `ready` or `stale`, as listed by the `/ready`
endpoint) with a typed mission/workspace/remote execution reference, then
submit their receipts. A receipt records actual toolchain, cache provenance,
exit code, artifacts, diagnostics, observed head and, for dirty-overlay
candidates, the source bundle digest. A passed receipt is classified
`exact_head` only when it attests the candidate's expected head, the
candidate's `source_bundle_digest` exactly (a bundle-less candidate rejects a
bundle-attesting receipt and vice versa), and the gate's pinned `toolchain`
when one is configured; any other pass is `stale`. Passed stale receipts are
retained as evidence but never unlock dependent gates and never certify. Stale
gates remain claimable, so a later exact-head execution can replace the stale
evidence. Once a campaign is marked merged it is terminal: late receipts never
recompute its status and its gates can no longer be claimed.

The durable outbox emits only `candidate_changed`, `blocker_changed`,
`campaign_terminal`, and `merged`. When the Paloma/Hermes webhook is configured,
delivery uses stable event IDs, exponential retries, HMAC signing and a dead
letter threshold.
