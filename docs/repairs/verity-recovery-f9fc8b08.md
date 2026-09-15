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
| Workspace preparation → source | Missing recorded mission directories can be recreated as configuration-only trees. | Require an existing recorded mission directory unless an intact explicit worktree has a verified persisted owner. Validate explicit working directories before configuration synchronization. Existing root-volume identity checks remain in force. |
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

The completion/session-event race is repaired by passing the actor's mission
store through both launch paths. Grok awaits native-session persistence before
sending an ACP prompt and before emitting a streaming session notification.
Broadcast delivery is no longer required for the next turn to read that ID.
A failed ACP persistence write refuses the prompt and transport fallback.

The subprocess regression leaves actor notifications unread, runs two ACP turns
through file and SQLite stores, and verifies session/new followed by session/load.
It also checks streaming persistence and a failed write that accepts no prompt.

Validation: all 22 Grok runner tests passed in a bounded debug scope; all
library test targets compiled with the main and parallel launch plumbing.

## Review follow-up: authoritative explicit worktrees

A generated mission directory can be auxiliary configuration when a worker uses
another mission's verified worktree. Preparation may recreate that auxiliary
directory only when the explicit source directory still exists and its persisted
owner/root verifies. Without that evidence, the recorded source-loss fence still
rejects preparation. The runner passes its already-resolved saved working
directory to this guarded preparation path.

Durable jobs with omitted cwd now validate a saved host worktree on another
registered root through its persisted owner. An arbitrary cwd override does not
gain this exception. Missing worktrees and existing but unregistered paths are
rejected before wrapper installation or process launch.

Validation: all 58 workspace and 35 durable-job tests passed in bounded debug
scopes, including host/container auxiliary-directory recovery, missing/unregistered
source refusal, cross-root saved cwd, and explicit-override rejection. All library
test targets compiled; cargo fmt --all and git diff --check passed.

## Review follow-up: garbage collection and resumability

Both GC phases retain source directories for Completed, Acknowledged, Failed,
Interrupted, Blocked, AwaitingUser and Paused missions, regardless of retention age. Those statuses
still support recovery/replies, so automatic source deletion conflicts with the
source-loss guard. Same-workspace short-ID collisions now prefer these protected
statuses over collectible terminal entries. Only NotFeasible retains status-based retention eligibility; orphan cleanup
still requires its existing independent checks.

The legacy long-stop cutoff remains in the configuration/report shape for
compatibility but no longer authorizes source deletion for resumable/replyable
missions. This deliberately increases disk retention; operators must settle or
explicitly handle retained work rather than expect automatic source recreation.
Previously deleted source still requires restoration or a verified explicit
worktree; no intentional-deletion bypass is introduced.

All seven GC tests passed in a bounded debug scope, including protected
statuses past retention and short-ID collisions. No production GC sweep or
configuration change was performed.

## Residual architecture: compute policy and diagnostic attribution

The operator verified that workspace `1ec51105` (legacy `dumbcontracts`, Ubuntu
profile without Lean) returned `local_allowed` for project `verity-core`.
`Workspace::compute_policy()` consults explicit workspace configuration and then
workspace name/path and Lean-related defaults; it does not consult durable
project requirements. The operator reports that f43 launched heavy local Lean
on agent-core in `missions.slice`, cancelled it, shallow-updated
`config.compute_policy=remote_required`, and resumed native Grok preserving edits.
This repair did not perform those operations.

Unfixed follow-up: resolve and enforce durable project compute requirements at
admission, including native resume, queued turns and durable jobs. Legacy naming
or a weaker workspace default must not downgrade a project's remote requirement.
Acceptance requires policy-source receipts and refusal of local heavy work when
required remote execution is unavailable. Keep this separate from the current
cancellation/recovery repair.

Diagnostic `572ed047` observed a host tool subprocess in
`/system.slice/sandboxed-sh-prod.service` with `memory.max=20GiB`. The historical
receipt lacks PID/start-time/ancestry evidence identifying the native harness.
A later read-only cwd scan found no matching process but cannot reconstruct
historical ancestry. Verify the actual harness PID, start time, ancestors and
cgroup independently before concluding a harness scope escape. Compute placement
and cgroup placement are separate: f43's local workload was in `missions.slice`.

The diagnostic host required remote execution and had a wrapper, but exposed no
fleet/submission MCP discovery. No benchmark ran. Inspected runner source cleans
outputs; the deployed binary was not attested. Remaining bounded work includes
host launch-scope coverage, catalog exposure under its existing owner, and
deployed-binary/job receipts before measurement. Grok environment propagation
is addressed by the paired-diagnostic follow-up below.
These are measurement blockers; they do not establish that Lean isolation is slow.
Detailed evidence and acceptance criteria are saved in
`output/architecture-572ed047-assessment.md` in this repair workspace. No message
was sent to protected be5.

## CI fixture follow-up

Run `34968992644` on `cc1f491d` passed 2,285 library tests and failed two. The
admission fixture injected ownership into a snapshot concurrently replaced by
the idle actor; it now supplies a separate published snapshot to admission.
The synthetic Codex process rewrote its JSON state in place and could be stopped
mid-write at a checkpoint; it now atomically replaces the snapshot. Both tests
retain their ownership/native-continuity assertions. These changes affect test
fixtures only; validation of the new head is reported in the PR.

Local validation: all 73 admission tests and 21 Codex continuity tests passed;
both previously failing tests also passed ten additional runs each.
`cargo fmt --all` and `git diff --check` passed.

## Review follow-up: terminal reply targets

Completed and Acknowledged are explicitly reactivated by user messages, so they
also retain source in both GC phases and receive collision protection. The GC
regression checks all ten running/recoverable/replyable statuses against the
control-plane reactivation predicate (with Active/Paused handled explicitly),
and verifies that NotFeasible remains collectible after retention. This further
increases retained disk usage; a future archival/deletion lifecycle must make
source availability explicit before allowing recovery. No such lifecycle or
production cleanup change is deployed by this PR.

## Review follow-up: explicit host Grok credentials

Native-file authentication now treats an explicit host HOME as an account
boundary, including container fallback. Launch preparation validates that HOME
but does not import a default-home or service-global auth cache, whether the
workspace file is present or missing. This prevents unrelated global credentials
from overwriting the workspace account and invalidating native-session identity.
The native CLI handles missing/invalid credentials in that explicitly selected
HOME. Default container auth synchronization retains its existing behavior.

The isolated regression supplies distinct synthetic workspace/global accounts,
checks preservation for host and container-fallback launches, then removes the
workspace credential and verifies that no global credential is imported. No live
credential was read or modified by the regression. This does not certify the
reported live Grok 401 or replace the outstanding isolated provider smoke test.

All 21 Grok auth tests passed locally in a bounded debug scope.
`cargo fmt --all` and `git diff --check` passed.

## Review follow-up: pre-activity legacy Grok records

Unused-placeholder migration accepts a missing legacy last_status_change_at
(file omission or SQLite NULL) when every other untouched/execution-evidence
check passes. A present changed status timestamp still blocks migration. The
file/SQLite regression now runs the complete positive/negative matrix both with
and without the activity timestamp, through two store reopens. Native mappings,
prior history/events, runs, blocked/touched records and opaque IDs remain fenced;
Hermes origin attribution remains preserved.

The file fixture now writes the actual flattened activity field. All 104
mission-store tests passed in a bounded debug scope; formatting and diff checks
passed.

## Paired diagnostic follow-up: native Grok remote environment

The operator reports that resumed native Grok f43, with workspace
`remote_required`, saw `remote-lean-build` exit 75 because REMOTE_BUILD_URL was
unset. The owning durable workspace_bash job
`caa4174f-6b88-5507-af9e-a594f6c5c0aa` on the same workspace/mission reported
URL, TOKEN, MISSION_ID and POLICY present (booleans only). This establishes an
available credentialed executor path and isolates an environment-preparation
difference; it is not evidence of blanket remote-service unavailability. The
operator is using that durable route without copying credentials. This repair
did not contact or mutate that mission/job.

Both Grok ACP and streaming-JSON already share environment preparation. That
function now calls `Workspace::remote_build_env(mission_id)`, the same factory
used by durable jobs and other native runners. It supplies the effective policy,
mission ID, endpoint, expiring scoped capability, wrapper PATH and configured
remote requirements. Workspace environment values cannot downgrade the supplied
policy or replace the runner's mission ID. Missing server configuration still
preserves remote_required without inventing an endpoint or token.

An isolated subprocess regression uses synthetic signing/provider values and
checks configured/unconfigured factories for host, container-fallback and
container layouts. Actual host/fallback WorkspaceExec child processes must
receive the policy, current mission identity and expected endpoint/token presence.
The test checks capability mission/expiry claims and wrapper PATH; existing
capability tests cover signature/scope validation. No network request, credential
copy, real native session or nspawn deployment is used by this regression.

The regression failed before the injection change (missing remote_required).
Production rollout and a native remote submission receipt are still outstanding;
this source fix does not attest a deployed binary or measure Lean performance.

Validation: all 23 Grok runner tests passed in a bounded debug scope.
`cargo fmt --all` and `git diff --check` passed.


## Native session generation fence

Backend attribution alone cannot reject a delayed session notification from an
older run of the same harness. Each spawned turn now captures its acquired
run ID and generation. Session notifications and Grok direct persistence carry
that immutable stamp. Memory, file and SQLite stores atomically compare it with
the latest acquired run before changing either the per-harness native identity
or its current-backend projection. Unattributed updates are accepted only for
records with no run history. The latest generation may drain its final event
after settlement; acquisition of any successor fences the older generation.
The actor updates its cached identity only after an accepted store write and a
matching cached run and backend.

Regressions cover fresh Grok and first Codex-to-Grok handoff with no native ID,
Grok-to-other-to-Grok restoration, old same-backend notifications, mismatched
run/generation pairs, and unattributed notifications after run acquisition.
A native ACP fixture checks that stale direct persistence preserves the
successor's ID and stops before accepting a prompt, with continuity required.
Unknown prompt/tool outcomes retain the existing no-replay behavior.

MCP start/resume descriptions and coordinator guidance now state the native
Codex goal limit of 4000 Unicode characters, including automatic writer
promotion. Larger instructions belong in referenced artifacts; rejection does
not truncate the goal.

The saved expanded HTTP fixture failure came from the host's lido-to-verity-lido
alias splitting lease keys. Its existing subprocess isolation removes
HERMES_PROJECTS_DIR. The exactly-one-writer and expected-owner assertions remain
intact; no production alias behavior is changed for the fixture.

Residuals remain: durable project compute requirements must be enforced without
legacy workspace naming; deployed native Grok remote environment and host auth
need live receipts; cgroup conclusions require actual harness PID ancestry,
not only a service-cgroup MCP subprocess. Fleet/submission discovery and deployed
runner attestation remain separate gaps. No Lean benchmark or isolation slowdown
is established. Metadata-touched legacy Grok UUIDs still need provenance before
migration. Hermes canonical enrollment/retry and Lido repair remain with their
owners. No deployment, merge, protected-mission contact or unknown-tool replay
was performed.


## Streaming Grok missing-session wording

Review 4016240180 identified `Session does not exist` as another native CLI
missing-session diagnostic. The installed Grok binary contains that literal
(read-only byte inspection, no native launch or provider request). Streaming
resume now classifies it as NativeContinuityRequired, alongside the existing
missing-session forms. The subprocess regression checks that exact message on
resume and confirms that a fresh-session failure remains an ordinary LlmError.
This changes classification only; it neither substitutes a new session nor
replays unknown tool outcomes.

Validation: all 23 Grok runner tests passed in the bounded debug scope;
`cargo fmt --all` and `git diff --check` passed.
