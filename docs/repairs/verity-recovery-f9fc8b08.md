# Verity runtime recovery repair

## Scope

Private checkout and branch `fix/verity-terminal-recovery-f9fc8b08`, based on
`d750841f`. Local repair-track identifier: `verity-terminal-recovery-f9fc8b08`;
this is not a claim of control-plane registration. No production mission was
cancelled, resumed, edited, or superseded for this work. No production files,
services, installed harnesses, Hermes checkout, or other writer branches were
modified. Deployment and merge are outside this repair's authorization.

## Causal architecture and changes

| Boundary | Failure | Change / disposition |
| --- | --- | --- |
| Actor cancellation → admission | A parked Blocked mission is considered terminal by the idempotent cancel shortcut, retaining native-goal ownership despite a successful response. | Exclude Blocked from that shortcut. Explicit cancellation takes the existing no-runner interruption path; admission still checks execution generations, remote work, pending admissions, and ownership. |
| Run settings → native CLI | A backend switch allocates a generic session UUID; Grok interprets transcript history as native continuation. | Store session identity per harness, preserve it across model changes and restarts, and attribute late session updates to the originating harness. First Grok handoff starts without an ID; return to a previous harness restores its ID. Refresh parallel-runner settings and ID together between turns. |
| Backend handoff → native agent selection | Settings reset the previous model but retain its agent, allowing OpenCode `build` to become native Claude's `--agent build`. | Clear an omitted agent when the backend changes. Preserve explicit custom-agent selection and same-backend settings. A real API/actor regression covers all three cases without starting execution. |
| Native recovery → retry | Missing Grok session/load falls back to latest-session continuation; incomplete ACP tools receive fabricated terminal results. | Load the exact session or return `native_continuity_required`. Streaming CLI uses `--resume <id>`, not new-session `--session-id` or unqualified `--continue`. Ambiguous prompt writes and unknown tool outcomes cannot trigger transport fallback; unfinished calls remain unresolved in the raw trace. |
| Workspace creation → Ready | Host workspace record is published before its root exists. | Create the host root before storing Ready; refuse a root that is an existing file. |
| Workspace preparation → source | Missing recorded mission directories can be recreated as configuration-only trees. | Require an existing recorded mission directory before preparation. Validate explicit working directories before configuration synchronization. Existing root-volume identity checks remain in force. |
| Durable command → cwd | Omitted cwd selects the workspace root (guest `/`), losing mission context. | Default to persisted mission working_directory or generated mission directory. Validate before installing wrappers. Preserve registered host scratch roots; explicit job overrides retain the workspace boundary. |
| Grok authentication → CLI | OAuth access tokens are exported as API keys, and unrelated OAuth refresh can prevent usable API-key authentication. | Keep API keys and native auth-file credentials separate. Avoid global legacy-auth cleanup during mission launch. Use the launcher's nspawn/fallback decision for native auth placement, require explicit host HOME for container fallback, and reject container HOME/auth-directory escapes. Install credentials via unique, private-before-write temporary files. Preserve workspace credentials when the host has no usable cache. |
| Error text → health | Numeric substring matching reads UUID fragment `b522` as HTTP 522. | Match complete numeric tokens. |
| Weekly exhaustion → classification | Weekly quota phrases lack rate-limit markers. | Add narrow exhaustion/reached phrases with positive and negative synthetic tests. The exact Fable receipt remains unverified. |
| Raw tool trace → diagnostics | Text, byte arrays, and rawOutput repeat the same output; diagnostics lack the promised result snippets. | Add bounded, deduplicated readable result snippets. Raw persisted events remain unchanged and available through get_mission_events. |
| Lake shim → remote allowlist | Reported installation sends an absolute Lake executable. | Base checkout already sends bare `lake`; retain the strict allowlist and remote_required policy. Offline source-transport regressions pass. No fleet wrapper refresh performed. |
| Writer prompt → native Codex goal | Oversized automatic /goal is rejected only after dispatch. | Validate the full Unicode objective (maximum 4,000 characters) at MCP promotion, API creation, and driver entry before native effects; never truncate it. |

## Validation

The original real actor cancellation regression failed on the pre-fix binary
(Blocked versus expected Interrupted), then passed with the cancellation fix.
Focused debug tests cover ownership counterexamples and concurrent successor
admission, file/memory/SQLite session handoffs and reloads, native Codex
continuity, Grok ACP protocol failures, workspace creation and source loss,
durable job cwd/ownership, authentication destination, and assistant diagnostics.
Final focused run: 125 library tests and all 69 assistant-MCP tests passed
(194 Rust tests total), with no empty filters or failures. This includes 21
Codex continuity tests, 13 Grok ACP tests, 17 Grok auth tests, 34 durable-job
tests, and 22 workspace execution tests. This is a focused suite, not the
entire repository test suite. A subsequent real API/actor agent-handoff
regression also passed. The Grok authentication suite subsequently passed all
20 tests, adding three regressions for execution-mode selection, auth-directory
symlink escape, and concurrent private credential installation. Distinct Rust
coverage is now 198 tests.

Commands used, with the installed stable toolchain and a private target directory:

```sh
cargo fmt --all
cargo test --lib --bin assistant-mcp --no-run
# Run bounded filters on the resulting debug test executables.
python3 scripts/test_remote_lean_build_source.py
```

Builds ran in private transient systemd scopes with CARGO_BUILD_JOBS=2,
MemoryMax=12G, CPUQuota=200%, and CPUWeight=20. Test processes ran in bounded
scopes. The system Cargo is too old for lockfile v4; no lockfile downgrade was
made. Python remote-source tests use fake curl (29 passed), not live builders.
Grok version/help was read locally: 0.2.93 documents --session-id as new-session
only and --resume as existing-session resume. No provider request was made.

## Remaining evidence and limits

- The inherited-agent handoff bug is fixed and tested. The exact native Claude
  `build` versus valid `claude` report still needs its error and field (`agent`,
  `backend`, or `config_profile`) to establish whether it is this bug. No
  speculative alias/profile rewrite is included; custom native agents remain
  possible.
- Fable's exact weekly-quota receipt and provider-specific recovery/reset
  behavior are not certified by synthetic phrase tests.
- No live Grok authentication comparison against the reported container 0.1.211
  was performed. Typed credentials and HOME mapping are locally testable;
  provider acceptance requires a separately authorized isolated smoke test.
- A missing recorded mission directory is fenced. Loss of a nested repository
  inside an otherwise existing mission directory, without an explicit saved
  working_directory, is not reconstructible from root metadata alone.
- Native Grok unknown outcomes require reconciliation. This patch prevents
  automatic fallback/replay; it does not supply an operator tool to certify an
  unknown effect or reconstruct a missing native session. Raw receipts are
  retained. Codex's existing durable unknown-outcome fence remains intact.
- Session history now includes a SQLite table (created with IF NOT EXISTS) and
  a backward-compatible file-snapshot field. Backups must include this state.
  Mission-only export/import portability is not extended by this repair.
- Host/container path tests and existing container environment tests do not
  substitute for a live nspawn deployment check. No shared writable .lake,
  command allowlist relaxation, fallback into /root, or runtime compute-policy relaxation was
  introduced.

## Rollout constraints

Review and merge must be done by another owner. Before an authorized rollout,
back up mission/session storage and validate workspace root availability. An
isolated harness smoke test should verify exact-session reattachment and native
authentication in the intended host/container version. Existing protected
missions must not be used as smoke tests. This PR does not release any live
production lease or refresh any deployed remote-build wrapper.

## Review follow-up: initial preparation retry

Review identified an ordering gap in both preparation entry points: the root
registry was committed before creating the mission directory. A failed initial
mkdir therefore made the next attempt look like source loss. Persist placement
after directory preparation succeeds, before wrapper/configuration writes.
Recorded directories that later disappear still fail closed; no registry entries
are deleted or inferred to be disposable.

The new regression reproduced the failure before the fix. It covers both
preparation entry points and host/container workspace layouts by forcing initial
directory creation to fail, removing the obstacle, and retrying preparation.
This prevents new incomplete entries; an already-recorded missing directory
still requires source/provisioning evidence before any recovery decision.

Validation: all 57 `workspace::tests::` tests passed in a bounded debug scope,
including the existing lost-source guard. `cargo fmt --all` and
`git diff --check` passed.

## Review follow-up: streaming resume refusal

The streaming transport now classifies a failed exact-session resume reporting
`No session found` or `Session not found` as `native_continuity_required`, matching
ACP's reconciliation fence. Cancellation retains its explicit terminal reason.
First-session and authentication failures retain their existing classification.
A real subprocess regression exercises both missing-session messages and those
negative cases; all 21 Grok runner tests passed in a bounded debug scope.

Legacy file/SQLite stores now clear preallocated UUID-v4 Grok IDs only for
untouched pending records: creation/update/status timestamps must agree, with no
resume/terminal evidence, native-session mapping, history, run or tree. SQLite
also checks the complete event table, not the bounded history projection. The
file migration is persisted before returning the store; SQLite uses a single
conditional UPDATE. Ambiguous or previously executed records retain their IDs
and require reconciliation. No live mission is migrated by this repair work.

Reopen regressions cover an untouched placeholder plus confirmed native IDs,
history, durable runs, blocked status, changed timestamps and opaque IDs, across
both stores and two reopens. No lease is released by the migration.

Validation: all 104 mission-store tests passed in a bounded debug scope,
including legacy migration/reopen and existing lease/storage regressions.

## Review follow-up: Hermes attribution

Hermes origin_session_id is operator-conversation attribution, not a native
Grok session receipt. It no longer excludes an otherwise untouched placeholder
from migration. The file/SQLite reopen regression now includes attribution on
both eligible and ineligible records and verifies that attribution survives.
Both harness-session tests passed in a bounded debug scope.

The newly reported completion/session-event ordering race remains under repair:
queued work must not start until the generated native ID is persisted.
