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

Create a campaign with `POST /api/validation-campaigns/from-workspace`, claim
ready gates with a typed mission/workspace/remote execution reference, then
submit their receipts. A receipt records actual toolchain, cache provenance,
exit code, artifacts, diagnostics and observed head. Passed stale receipts are
retained as evidence but never unlock dependent gates.

The durable outbox emits only `candidate_changed`, `blocker_changed`,
`campaign_terminal`, and `merged`. When the Paloma/Hermes webhook is configured,
delivery uses stable event IDs, exponential retries, HMAC signing and a dead
letter threshold.
