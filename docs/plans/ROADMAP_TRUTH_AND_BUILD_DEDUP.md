# Plan: tracks as the only durable truth, builds durable and deduplicated

Status: proposal for review. Not yet implemented.
Scope: `sandboxed_sh` (backend, `assistant-mcp`, `palomactl`), the Hermes
desktop `projects-board` plugin, `dgx-spark-arbiter`, and the Hermes skills
that describe these tools.

This plan applies phases 1 to 4 of `AGENT_NATIVE_ROADMAP.md` to two concrete
defects. It deliberately adds the minimum durable state required by the two
different lifecycles involved: immutable evidence and mutable/recoverable job
execution. Deletion happens only after a compatibility window proves that the
replacement is live.

---

## 0. Observed defects (verified in code on 2026-09-02)

### 0.1 Roadmap totals are inconsistent

The desktop rail showed `ROADMAP · 16/26 DONE`, `No open items.` above ten
open rows, and duplicate rows `repair-pr-233` / `pr-233-repair`,
`UX1` / `ux1-pr229-cert`.

Root causes, all confirmed:

| # | Fact | Where |
|---|---|---|
| R1 | `project_tracks.status` is a free string. No CHECK, no NOT NULL. "Done" means someone wrote the literal `done`/`closed`. | `src/api/projects_store.rs:107-118` |
| R2 | The only writers of track status are `set_track`, `upsert_planned_tracks`, `patch_track`, reached by `POST /:slug/track`, `POST/PATCH/DELETE /:slug/tasks/*`. None requires evidence. No controller callback or reconciler writes status. | `projects_store.rs:1260-1348`, `projects_overview.rs:884-910, 1451-1523` |
| R3 | A mission tagged with an unknown `project.track` never creates a row. The row is synthesized at read time (`declared:false`, or literal `"untagged"`), and kept on the roadmap only while it has a live attempt. | `src/api/mission_horizon.rs:170-241`, `projects_overview.rs:3127-3151` |
| R4 | `declared` is `#[serde(skip)]`, so clients cannot see it and approximate it with "has a non-empty status". | `mission_horizon.rs:142`, desktop `api.ts:938-961` |
| R5 | Track keys are case-sensitive free strings in a `BTreeMap`. `UX1` and `ux1-pr229-cert` are two keys. Two dispatches with different spellings of the same intent make two rows. | `mission_horizon.rs:175-246` |
| R6 | "N/M done" is computed in five places with three vocabularies: `/tasks` summary, MCP `get_project.item_counts`, `project_health.rs` (mission-derived, ignores `project_tracks`), `/api/control/tracks`, and the desktop client `roadmapFromItems`. The desktop uses its own count first and the server `summary` only as fallback. | `projects_overview.rs:1289-1316`, `src/bin/assistant_mcp.rs:3843-3897`, `src/api/project_health.rs:26-88`, `src/api/routes.rs:792`, desktop `api.ts:1048-1071`, `project-rail.tsx:79-97` |
| R7 | Desktop `ProjectItem` has no `title` field, so on the primary `GET /projects/:slug` path the title falls back to `desired_state` then the raw key. `/tasks` computes a proper title server-side but is the fallback path. | desktop `api.ts:181-188, 906-922`; `projects_overview.rs:3182-3200` |
| R8 | `No open items.` is one i18n string used for two meanings: "no live missions in the roster row" (status card) and "empty roadmap" (roadmap section). | desktop `project-rail.tsx:167, 196`, `i18n.ts:278` |
| R9 | A client-side heuristic rewrites the whole list into `Wave 1..N` rows if any key matches `wave-N`. | desktop `api.ts:964-1046` |
| R10 | Hermes tracker Markdown is still a roster source (`read_trackers`, `list_markdown_slugs`), and `palomactl reconcile` compares Markdown to fixture files, never to `project_tracks`. | `projects_overview.rs:2038-2064, 2871-2945`; `src/bin/palomactl.rs:225-395` |
| R11 | `project_roadmap_proposals` is write-dead but still read into the roadmap. | `projects_store.rs:148-159`, `projects_overview.rs:3044-3097` |
| R12 | The track-level contracts described by the docs and skills — accepted evidence, reopen/invalidation, declared totals, unplanned attempts, inconsistencies, ownership lease, and `get_situation` — have no implementation. Build-wait leases do exist, but they are not track ownership. Only `project_decisions.evidence` (free JSON) resembles track evidence. | `skills/project-manager/SKILL.md:34-60`, `skills/hermes-mission-control/SKILL.md:124-137`, `skills/controllers-policy/SKILL.md:158-186` |

### 0.2 Builds are not durable or deduplicated on the Spark lane; pollers multiply

| # | Fact | Where |
|---|---|---|
| B1 | Two offload lanes exist. The fleet lane (`POST /api/remote-build`) has durable handles, receipts, a content identity, and dedup. The Spark lane (`POST /api/spark/offload`) has none: one blocking `curl --max-time 5400`, a server-side 60 min poll loop, an arbiter whose `JOBS` dict is in memory and lost on restart. | `src/api/remote_build.rs`, `src/remote_node/job_ledger.rs`; `src/api/spark.rs:233-517`, `dgx-spark-arbiter/arbiter/arbiter.py:51, 251-259`, `client/spark-build:34-59` |
| B2 | `RemoteJobIdentity` includes repository, commit, cwd, argv, artifacts, toolchain, and bundle digest, but has no schema version or base tree SHA. The commit participates directly, so identical content under a different commit is a different build. | `job_ledger.rs:37-56`, `remote_build.rs:454-485` |
| B3 | The fleet ledger is two JSON files under one working dir, single-process by assumption. | `job_ledger.rs:244-258` |
| B4 | A second submission of an active identity gets `409 REMOTE_VALIDATION_ALREADY_ACTIVE`. A harness that retries or spawns a helper therefore sees an error and invents a poller. | `remote_build.rs:910-1017, 1092-1120` |
| B5 | The bundled wrapper polls the status endpoint every 3 s from inside the workspace. The "no pollers" rule exists only as prose in skills. | `scripts/remote-lean-build:726-782`, `skills/controllers-policy/SKILL.md:299-303`, `skills/verity-feature-implementation/SKILL.md:540-542` |
| B6 | `waiting_remote_job` already parks one mission on one job and `current_remote_build_wait_handle` enforces one owning handle per mission. Nothing stops a *second mission* or a subagent on the same track from submitting or polling. | `src/api/mission_store/mod.rs:276-290`, `job_ledger.rs:183`, `src/api/control/mod.rs:20539-20598` |
| B7 | "EIP" is the `EIP-8282 Audit` project (Lido line of work), built through the same Verity Lean pipeline (`lake build`, `cwd_rel: verity`). Not a separate build config. | `ops/paloma-overnight-watch.service:2`, `docs/REMOTE_NODES.md:400-410` |

---

## 1. Corrected model: a receipt is not a running job

The original proposal put `active` and terminal build states in an immutable
`receipts` row. Those requirements contradict each other. It also omitted the
unique constraint required by its stated idempotency rule. Use two durable
objects, matching the constitution's action → receipt distinction:

1. `remote_jobs` is mutable execution state: submission, lease, heartbeat,
   placement, cancellation, terminalization.
2. `receipts` is append-only evidence emitted by a completed action or an
   explicit observation. It never represents a live process.

`projects.db` can hold both initially because it is the control plane's local
SQLite authority, but the store API must expose transactions and migrations;
callers must not reach across raw SQLite connections. Do not delete the JSON
ledgers until dual-write, backfill, restart recovery, and parity checks pass.

```sql
CREATE TABLE IF NOT EXISTS receipts (
  id               TEXT PRIMARY KEY,          -- uuid
  idempotency_key  TEXT NOT NULL UNIQUE,
  request_hash     TEXT NOT NULL,              -- detects key reuse with another body
  kind             TEXT NOT NULL,             -- versioned application enum
  project_slug     TEXT,
  track_id         TEXT,
  criterion_id     TEXT,
  subject_type     TEXT NOT NULL,              -- build|pr|command|migration|...
  subject_id       TEXT NOT NULL,              -- immutable external handle
  outcome          TEXT NOT NULL CHECK (outcome IN
                    ('succeeded','failed','cancelled','observed','invalidated')),
  actor_type       TEXT NOT NULL,              -- mission|operator|controller|system
  actor_id         TEXT NOT NULL,
  verifier         TEXT,
  supersedes_receipt_id TEXT REFERENCES receipts(id),
  observed_at      TEXT NOT NULL,
  payload          TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
  created_at       TEXT NOT NULL,
  FOREIGN KEY (project_slug) REFERENCES projects(slug) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS receipts_track_time
  ON receipts(project_slug, track_id, observed_at);
CREATE INDEX IF NOT EXISTS receipts_subject
  ON receipts(subject_type, subject_id);

CREATE TABLE IF NOT EXISTS remote_jobs (
  job_id                  TEXT PRIMARY KEY,
  idempotency_key         TEXT NOT NULL UNIQUE,
  request_hash            TEXT NOT NULL,
  identity_version        INTEGER NOT NULL,
  identity_hash           TEXT NOT NULL,
  canonical_mission_id    TEXT NOT NULL,
  node_id                 TEXT,
  state                   TEXT NOT NULL CHECK (state IN
                          ('submitting','accepted','running','cancelling',
                           'succeeded','failed','cancelled','lost')),
  submission_sequence     INTEGER NOT NULL,
  request_payload         TEXT NOT NULL CHECK (json_valid(request_payload)),
  accepted_at             TEXT,
  heartbeat_at            TEXT,
  finished_at             TEXT,
  terminal_receipt_id     TEXT REFERENCES receipts(id),
  created_at              TEXT NOT NULL,
  updated_at              TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS one_live_job_per_identity
  ON remote_jobs(identity_version, identity_hash)
  WHERE state IN ('submitting','accepted','running','cancelling');

CREATE TABLE IF NOT EXISTS remote_job_subscribers (
  job_id          TEXT NOT NULL REFERENCES remote_jobs(job_id) ON DELETE CASCADE,
  mission_id      TEXT NOT NULL,
  wake_required   INTEGER NOT NULL DEFAULT 0,
  wake_state      TEXT NOT NULL DEFAULT 'pending'
                  CHECK (wake_state IN ('pending','delivered','suppressed')),
  attached_at     TEXT NOT NULL,
  delivered_at    TEXT,
  PRIMARY KEY (job_id, mission_id)
);
```

The subscriber table is required: attaching mission B to mission A's build is
not safe unless terminal delivery to both waiters is durable and idempotent.
An `attach` receipt may audit that action, but cannot replace the subscription.

`project_decisions.evidence` is migrated to receipt references, then retained
as a compatibility projection for one release. It is not deleted in the first
migration. Every persisted request/payload is credential-free: repository URLs
are sanitized and secret values are neither hashed into identity nor stored.

---

## 2. Workstream A: tracks are the only durable plan inventory

Tracks are the authority for what work exists. Missions remain the authority
for attempts, receipts for observations, and `get_situation` for derived state.
Do not copy live ownership into `project_tracks`.

### A1. Give tracks stable identity and separate lifecycle from derived state

Rebuild `project_tracks` transactionally; this is not an `ensure_column`
migration because SQLite cannot add the required checks or change the primary
key safely.

```sql
CREATE TABLE project_tracks_v2 (
  id                   TEXT PRIMARY KEY,          -- stable uuid
  slug                 TEXT NOT NULL REFERENCES projects(slug) ON DELETE CASCADE,
  track_key            TEXT NOT NULL,
  title                TEXT NOT NULL,
  desired_state        TEXT,
  lifecycle            TEXT NOT NULL DEFAULT 'active'
                       CHECK (lifecycle IN ('active','cancelled')),
  origin               TEXT NOT NULL DEFAULT 'declared'
                       CHECK (origin IN ('declared','imported','absorbed')),
  explicit_blocker     TEXT,                      -- structured judgment, nullable
  acceptance_criteria  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(acceptance_criteria)),
  depends_on           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on)),
  revision             INTEGER NOT NULL DEFAULT 0,
  created_at           TEXT NOT NULL,
  updated_at           TEXT NOT NULL,
  UNIQUE (slug, track_key)
);
```

Acceptance criteria become objects with stable IDs and verifier/freshness
requirements, not just strings. `derived_state` (`ready`, `executing`,
`waiting`, `blocked`, `satisfied`, `claim_only`, `inconsistent`) is computed
from lifecycle, dependencies, explicit blockers, live attempts, and current
evidence. It is never stored as another writable truth.

Normalize keys on every write (lowercase ASCII, `[a-z0-9-]`, collapsed
dashes), but do not assume normalization proves semantic identity. Preserve
old spellings in `project_track_aliases(track_id, alias_key, reason)`.
External references such as GitHub PRs belong in
`project_track_refs(track_id, kind, repository, number)`. A shared PR is a
matching hint, not an automatic merge: two legitimate tracks may govern the
same PR.

Legacy mapping:

- `cancelled` → lifecycle `cancelled`;
- `done|closed` → lifecycle `active` plus a `legacy_import` claim receipt;
- NULL, empty, `running`, `in-progress`, unknown → lifecycle `active` plus a
  reconciliation correction.

A legacy claim does not increment the verified-satisfied count.

### A2. Absorb missions at write time with a crash-safe reservation

The mission store and `projects.db` are separate authorities, so mission
creation plus track linkage cannot honestly be described as one SQLite
transaction. Implement an idempotent action/saga:

```sql
CREATE TABLE IF NOT EXISTS track_leases (
  id               TEXT PRIMARY KEY,
  track_id         TEXT NOT NULL,
  mutation_domain  TEXT NOT NULL,
  attempt_id       TEXT NOT NULL,
  mode             TEXT NOT NULL CHECK (mode IN ('reader','writer')),
  state            TEXT NOT NULL CHECK (state IN ('reserved','active','released','expired')),
  lease_until      TEXT NOT NULL,
  idempotency_key  TEXT NOT NULL UNIQUE,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS track_leases_domain
  ON track_leases(track_id, mutation_domain, state, lease_until);
```

Writer/read conflict checks and lease insertion happen under
`BEGIN IMMEDIATE`; the index accelerates lookup but is not itself sufficient
to express reader/writer exclusion.

1. Resolve canonical project and explicit `track_id` (preferred) or normalized
   key/alias.
2. Reserve a writer lease for `(track_id, mutation_domain)` using the action's
   idempotency key.
3. Create the mission with the generated attempt ID and durable track link.
4. Confirm the lease; on failure, compensate it. Startup reconciliation
   repairs the two crash windows.
5. If the key is unknown, create `origin='absorbed'` before reserving it.

Do not persist `owner_mission_id` on the track: derive owners from live mission
links and leases. Do not trust a caller-provided `parallel_reader: true`.
Read-only concurrency is allowed only for an intent/backend combination the
server marks non-mutating; writers need disjoint mutation-domain leases.

During one compatibility release, a project mission with no track is absorbed
under a generated key and emits a reconciliation warning. After all callers
send `track_id`/`track`, reject it with `400 TRACK_REQUIRED`; then delete the
read-time `untagged` synthesis and `declared` flag.

### A3. Satisfaction is derived from current accepted evidence

New endpoint and MCP tool:

```text
POST /api/projects/:slug/tracks/:track/accept
{
  "idempotency_key": "...",
  "expected_revision": 12,
  "evidence": [{
    "criterion_id": "tests",
    "kind": "pr_head_review",
    "subject_id": "owner/repo#233@<head-sha>",
    "payload": {...}
  }]
}
```

In one `projects.db` transaction, validate the expected revision, validate the
evidence schema/verifier, insert immutable receipts, and bump the revision.
Retrying the same idempotency key returns the prior response. A different body
with the same key returns `409 IDEMPOTENCY_MISMATCH`.

Head-bound evidence staleness is part of this workstream, not deferred. Store
the exact repository/PR/head in the receipt and invalidate it when the observed
head changes. The derived track state then reopens automatically; no mutable
`satisfied_by_receipt` pointer or manual `reopen` is needed. A manual
invalidation endpoint appends an `invalidated` receipt with reason and
authority.

`set_project_track` may change intent/lifecycle/blocker fields but rejects
`done`, `closed`, and `satisfied`.

### A4. Build `get_situation` first, then make every reader a projection

Implement the roadmap's phase-1 `SituationBuilder` and expose
`GET /api/control/situation` plus MCP `get_situation`. `get_project`, roster,
`project_health`, `/api/control/tracks`, and compatibility `/tasks` all call
the same builder.

Canonical summary:

```text
TrackSummary {
  total, verified_satisfied, claim_only, open, blocked, cancelled,
  live_attempts, source_unavailable, as_of, cursor
}
```

`total` excludes cancelled tracks. The displayed completion ratio is
`verified_satisfied/total`; `claim_only` is shown separately and never counted
as done. A source failure is not rendered as zero.

Canonical item shape shared by HTTP and MCP:

```json
{
  "id": "...",
  "key": "ux1",
  "title": "UX1 deploy and obtain explicit Lido acceptance",
  "lifecycle": "active",
  "derived_state": "executing",
  "origin": "declared",
  "owner": {"attempt_id": "...", "lease_until": "..."},
  "acceptance": {"verified": 1, "total": 2, "claim_only": false},
  "depends_on": [],
  "revision": 3,
  "updated_at": "..."
}
```

Keep `GET /projects/:slug/tasks` for at least one compatibility release, add
deprecation telemetry/header, and make it a lossless projection of the same
builder. Remove it only after MCP, desktop, scripts, and access logs show no
remaining consumer. The generic `/api/tasks` mobile endpoint and boss-board
task endpoints are unrelated and must not be removed.

### A5. Import Markdown and dead proposals without writing Markdown

`palomactl import-trackers --api <url> [--dry-run]`:

- reads Hermes tracker Markdown and `.paloma/projects/**.md`;
- imports `project_roadmap_proposals` before that table is retired;
- parses roadmap items, normalizes keys, and produces an explicit ambiguity
  report rather than guessing semantic duplicates;
- writes legacy checked items as claim receipts with `source_path`, line,
  `source_hash`, and parser version;
- stores import hashes in `project_imports`, not Markdown front matter;
- is idempotent on `(project, source_path, source_hash, parser_version)`.

Run `--dry-run`, back up `projects.db`, import, compare counts/items, then flip
a feature flag so Markdown no longer contributes roster slugs. Keep rollback
read support for one release. `palomactl reconcile` becomes read-only drift
reporting against the live API and stops treating fixtures as authority.

Migration/import corrections are immutable `reconcile` receipts. Acknowledging
one appends a separate `reconcile_ack` receipt; it does not mutate evidence.

### A6. Desktop and skill migration

Backend compatibility ships first. Then, in the separate `hermes-agent` repo:

- render only server `summary` and canonical items;
- remove `roadmapFromItems`, `itemBelongsOnRoadmap`, `collapseWaveProgram`, and
  the `/tasks` fallback after the compatibility endpoint is unused;
- show verified, claim-only, open, blocked, cancelled, and source-unavailable
  distinctly;
- keep invalidation frames and 15 s refetch as recovery, not authority.

Update `project-manager`, `sandboxed-sh-missions`,
`hermes-mission-control`, and `controllers-policy` with the MCP change.
`start_mission` requires a track after the compatibility window; acceptance
requires `accept_project_track`; only `get_situation`/`get_project.summary`
may be quoted. Implement the phase-1 tools currently described by the docs;
do not delete `get_situation` merely because it is currently phantom.

---

## 3. Workstream B: durable builds, exact deduplication, one observer

### B1. Versioned content identity

Identity is a canonical, versioned JSON document, hashed byte-for-byte:

```text
{
  version, repository_identity, base_tree_sha, overlay_digest,
  argv, cwd_rel, artifacts, toolchain, builder_image_digest,
  build_protocol_version, behavior_env_digest
}
```

Do not "normalize" shell commands: preserve the exact argv vector. Canonicalize
and validate `cwd_rel`; sort/deduplicate artifact selectors; hash only an
explicit allowlist of behavior-affecting non-secret environment inputs. The
wrapper supplies `HEAD^{tree}`; core recomputes the overlay digest from the
uploaded canonical bundle; the node verifies the checked-out base tree before
execution. Commit SHA remains provenance, not content identity.

Identity versioning is mandatory so a future toolchain/image/input change
cannot silently reuse an old receipt.

### B2. Migrate the ledger with dual-write and crash recovery

Write every new handle to `remote_jobs` and the existing JSON ledger. Write
terminal outcomes to immutable `receipts` and the existing receipt JSON.
On startup, compare both stores, reconcile tentative submissions, restore
leases/subscribers, and emit a parity metric. Backfill old rows with
`identity_version=0`, for which successful-result reuse stays conservative.
Stop JSON writes only after at least one production restart and a zero-drift
window; delete JSON readers one release later.

Use SQLite uniqueness plus `BEGIN IMMEDIATE` for cross-task exclusion. The
existing in-process placement mutex remains only as an optimization, not the
correctness boundary.

### B3. Attach instead of 409, with durable subscribers

A submit for an existing live identity transactionally inserts/returns the
`remote_job_subscribers` row and responds `200 {attached:true,...}`. Only the
unique-index winner dispatches to a node. Every subscribed mission gets one
terminal delivery through the durable outbox semantics.

A matching successful receipt can be reused only when its identity version,
artifact requirements, and freshness policy match. `force_new` bypasses
successful reuse but never bypasses the one-live-job constraint.

### B4. Fold Spark into the fleet behind a compatibility wrapper

- Make DGX Spark execute leased `sandboxed-node` jobs.
- Keep the arbiter's memory gate, scoped limits, P0 preemption, and cooldown as
  a local slot provider invoked by the node.
- Change `spark-build` into a compatibility wrapper around
  `remote-lean-build --node dgx-spark` and retain its CLI/output contract for
  one release.
- After canaries and production parity, remove Spark `/build`, its in-memory
  `JOBS`, `src/api/spark.rs`, the rsync/poll loop, and `lean-remote.sh`.

The core ledger replaces control-plane job memory. It does not by itself make
the remote execution process durable: `sandboxed-node` plus the local arbiter
must persist/recover accepted job state across their own restart.

### B5. Exclusion uses explicit lineage and leases

Enforce three server-side constraints:

1. one live `remote_jobs` row per versioned identity; other callers attach;
2. one mutation-domain lease per track; a waiting writer keeps its lease while
   its build runs, while approved readers can coexist;
3. an unchanged durable wait predicate causes the controller pre-model check
   to skip the turn.

Never infer parent/child lineage from `origin_session_id`: it identifies the
Hermes control conversation and is shared by sibling missions. Add/use an
explicit `parent_mission_id`/`root_attempt_id` and the durable track link.

### B6. Remove harness pollers; retain one server observer per job

`remote-lean-build` submits once and the harness transition records
`waiting_remote_job`; it does not poll every three seconds and does not keep a
90-minute HTTP request open. Core owns one recoverable observer per job
(event/callback where supported, bounded reconciliation polling otherwise).
Terminalization writes the receipt and subscriber outbox in one transaction,
then wakes each mission and its bound conversation idempotently.

If the submit response is lost, retrying the same idempotency key returns the
same job. An exit-code convention alone is not the protocol; the mission
runner must recognize the structured waiting transition.

---

## 4. Rollout order and compatibility gates

| Step | Content | Size | Removal gate |
|---|---|---|---|
| 1 | `SituationBuilder`, canonical item/summary, `get_situation`; old APIs project it | L | contract parity across HTTP/MCP/dashboard fixtures |
| 2 | Stable track IDs/aliases/refs, lifecycle migration, claim receipts, dry-run importer | L | backup + per-project reconciliation approved |
| 3 | Idempotent accept/invalidate, verifier and automatic head staleness | L | stale-head and retry tests pass |
| 4 | Track reservation/link saga, absorption, explicit lineage; update MCP/skills | L | crash-window reconciliation tests pass |
| 5 | Deploy compatible backend, migrate data, then deploy desktop | M | telemetry shows no old projection consumers |
| 6 | Versioned build identity, SQL jobs/subscribers/receipts, dual-write | L | restart recovery and JSON/SQL parity window |
| 7 | Attach/wake semantics and passive harness | L | concurrent/restart/duplicate-delivery canaries |
| 8 | DGX fleet integration and arbiter persistence | L | Spark and fleet parity under preemption/restart |
| 9 | Remove compatibility code and old ledgers | M | rollback window elapsed; no legacy traffic |

This is a multi-repository rollout, not one atomic PR: sandboxed.sh backend/MCP,
Hermes skills/config, `hermes-agent` desktop, and `dgx-spark-arbiter` need
separate commits and explicit compatibility gates.

Final deletions: read-time synthetic tracks, proposal table after import,
independent rollups, desktop roadmap heuristics, `/projects/:slug/tasks` after
telemetry, Markdown roster union, JSON build ledgers after parity, Spark build
API/in-memory jobs, and harness poll loops.

---

## 5. Required tests and production canaries

### Track/situation tests

- Legacy migration is transactional, restartable, backed up, and maps every
  old status to lifecycle plus reconciliation evidence.
- Normalized collisions never auto-merge semantic tracks; aliases and
  ambiguous PR references are reported deterministically.
- Unknown mission track is absorbed once; the two mission/link crash windows
  converge; stale writer leases expire/reconcile.
- A caller cannot self-declare read-only to bypass a writer lease.
- Acceptance is revision checked and idempotent; same key/different body is a
  conflict; missing or invalid criterion evidence cannot satisfy a track.
- A PR head change invalidates head-bound evidence and immediately changes the
  shared summary from satisfied to open/inconsistent.
- Claim-only never increments `verified_satisfied`.
- `get_situation`, `get_project`, roster, compatibility `/tasks`, MCP, and
  desktop agree byte-for-byte on canonical fields for the same fixture.
- Source unavailable is distinct from an empty project.
- Import dry-run/import/import-again is idempotent; proposal and Markdown
  counts reconcile before either legacy reader is disabled.

### Build tests

- Same content with a different commit message reuses a receipt; any change to
  overlay, argv, cwd, artifacts, toolchain, image, protocol, or behavior env
  changes identity.
- Core rejects a bundle whose declared base tree/overlay digest is not what
  the node executes.
- Concurrent identical submits dispatch exactly once and persist all
  subscribers; every subscriber receives exactly one wake.
- `force_new` cannot create a concurrent identical live job.
- Crash at each boundary (before/after node acceptance, terminal receipt,
  subscriber delivery) converges after restart without duplicate execution or
  lost wake.
- SQL/JSON dual-write parity survives a production restart before JSON is
  retired.
- `spark-build` compatibility output stays stable; DGX node and arbiter recover
  a running/preempted job; P0 still pauses CI/vLLM safely.
- A waiting build produces no harness poller and an unchanged wait predicate
  produces no controller model turn.

### Production canaries

1. Snapshot/backup `projects.db` and both JSON job ledgers.
2. Run tracker import in dry-run and retain only counts/ambiguities.
3. Enable canonical read projection for one project, compare every surface,
   then expand.
4. Enable build SQL dual-write without changing dispatch behavior; restart
   once through the guarded deploy path and require zero parity drift.
5. Enable attach/passive wait for one non-critical identity.
6. Canary DGX through the fleet while the old Spark route remains available.
7. Remove old readers/routes only after observed zero use and a rollback
   window.

## 6. Reviewer decisions resolved

1. **Blocked is derived.** Dependencies, a durable wait, or an explicit
   structured blocker are inputs; `blocked` itself is not another status
   writer.
2. **Head staleness ships with acceptance.** Deferring it would recreate false
   completion immediately after a force-push.
3. **`/projects/:slug/tasks` is deprecated, not immediately deleted.** Current
   in-tree consumers include `assistant-mcp`, Hermes skills, and the desktop
   fallback. Generic mobile `/api/tasks` and boss-board routes are different
   APIs.
4. **The wave heuristic can be retired after migration.** A read-only
   production query on 2026-09-02 found 19 `verity-lido` `wave-*` rows and all
   were `cancelled`; preserve their aliases/history but do not synthesize
   parent tracks from them.
