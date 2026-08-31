# Agent-native Paloma roadmap

This plan implements the architecture in
[`AGENT_CONTROL_PLANE.md`](AGENT_CONTROL_PLANE.md). It is ordered by leverage:
first make reality cheap and unambiguous to read, then make actions atomic,
then automate verification and learning. Existing APIs remain compatible until
their consumers have migrated.

## Success measures

The redesign is successful when:

- a routine controller tick uses one bounded situation read and at most one
  intent-level mutation;
- unchanged waiting projects consume no model turns;
- duplicate dispatches and branch-writer conflicts are rejected by construction;
- every completion shown as successful links to accepted evidence at the exact
  governed artifact version;
- every operator escalation states the unavailable authority or missing fact;
- project, dashboard, desktop, and controller surfaces agree from one projection;
- median context, model calls, and wall-clock cost per accepted track decrease
  without reducing verified completion rate;
- operational lessons have provenance and executable coverage, and obsolete
  rules can be found and retired.

Baseline these before Phase 1: tool calls and bytes per tick, full mission-list
scans, duplicate attempts, polling ticks, unrouted callbacks, stale-head false
completions, unverified terminal claims, operator questions, and cost per
accepted track.

## Phase 0 — Name the constitution

Goal: stop conceptual drift while implementation continues.

- Adopt the abstraction tower and epistemic classes from the architecture doc.
- Publish a glossary/schema map for existing fields to target concepts.
- Mark each replicated field with authority and reconciliation direction.
- Classify current docs as constitutional, operational, API reference,
  historical migration, or incident record.
- Add architecture checks to design review: source of truth, identifier,
  receipt, freshness, idempotency, wake path, and retirement path.

Exit: new design work can name its layer and cannot introduce an unnamed
writable truth.

## Phase 1 — One bounded situation read

Goal: remove reconstruction work from the model.

- Add `GET /api/control/situation` and MCP `get_situation` for portfolio and
  project scopes.
- Project existing roster, grant, tracks, decisions, mission horizon, route,
  fleet constraints, and latest receipts into the four epistemic classes.
- Add monotonic cursors, `since_cursor`, deterministic ordering, caps, omission
  counts, and `source_unavailable` diagnostics.
- Add a server-derived attention queue and richer states: ready, executing,
  waiting, blocked, paused, complete, inconsistent.
- Make `get_project` a compatibility projection over the same builder.
- Add contract tests that compare dashboard, MCP, and HTTP projections.

Exit: a controller can choose its next action without scanning tracker markdown,
global missions, raw events, or GitHub independently in the normal case.

## Phase 2 — Receipts and evidence

Goal: make truth and completion explicit.

- Introduce stable receipt and evidence-reference schemas.
- Normalize existing mission IDs, terminal evidence, PR heads, build/job IDs,
  deployment versions, decision rows, and placement records into receipts.
- Give acceptance criteria verifier types and artifact-version binding.
- Derive track satisfaction from accepted evidence.
- Invalidate head-bound evidence automatically when the governed head changes.
- Surface “not checked,” “source unavailable,” and “claim only” distinctly.

Exit: every satisfied track explains which evidence closed each criterion; a
terminal mission without evidence cannot close a track.

## Phase 3 — Atomic intent-level actions

Goal: make the safe path the shortest path.

- Add `propose_action` and `execute_action` with typed action kinds.
- Fold dispatch, project/track linkage, owner lease, decision declaration,
  callback route, and status effects into one transaction.
- Add object revisions and stale-write conflict responses.
- Add durable idempotency keys and return prior receipts on retries.
- Compute effective grant server-side and return the exact permitting or
  denying rule.
- Retain granular MCP tools as primitives, but route agent-facing skills to the
  intent-level surface.

Exit: a dispatch cannot become invisible between `start_mission` and
`link_mission_to_project`, and retrying a timed-out mutation cannot duplicate it.

## Phase 4 — Ownership, leases, and event-driven waiting

Goal: eliminate phantom owners and reasoning-as-polling.

- Add controller, track, writer-domain, provider, and compute leases with
  holder, scope, expiry, and renewal evidence.
- Derive unowned-ready work and lease conflicts in the situation projection.
- Represent waits as typed predicates plus deadline and fallback action.
- Wake bound conversations on predicate transitions; coalesce duplicate events.
- Suppress cron model turns when the situation cursor has not changed and no
  deadline or ready work exists.
- Make callbacks carry the causal action/receipt and follow route continuations.

Exit: “owned,” “waiting,” and “ready” are machine-verifiable; an unchanged wait
costs storage and timers, not inference.

## Phase 5 — Resource and economic control

Goal: spend where it changes the outcome.

- Enforce typed per-project and per-action budgets at dispatch boundaries.
- Return placement decisions with binding constraints, alternatives, predicted
  cost, and fallback.
- Attribute tokens, wall time, provider quota, compute, retries, and operator
  interruptions to project/track/action/receipt.
- Rank ready work using expected value, urgency, confidence, marginal cost, and
  unlock value; preserve operator priority as an explicit input.
- Add circuit breakers at shared failure domains so one adapter defect does not
  burn every provider route.

Exit: the portfolio can explain both why work was selected and what accepted
progress cost.

## Phase 6 — Accretive knowledge

Goal: turn operations into a self-improving but governable system.

- Add a knowledge-item registry with provenance, scope, confidence,
  counterexamples, supersession, review date, and executable attachment.
- Generate candidate lessons from incident clusters and successful receipts;
  require review or repeated evidence before promotion.
- Prefer promotion into schemas, tests, guards, routing policy, and tool
  descriptions; keep skills as the compact decision layer.
- Mine completed track graphs into versioned planning templates with empirical
  success and cost statistics.
- Add staleness reports for rules whose evidence, APIs, model names, or fleet
  assumptions have expired.

Exit: Paloma can answer “what did we learn, why do we believe it, where is it
enforced, and when should it be reconsidered?”

## Phase 7 — Converge every surface

Goal: remove compatibility debt and shadow logic.

- Move dashboard, mobile, desktop, `palomactl`, and controller skills onto the
  shared situation/action contracts.
- Remove parsed text trailers after all consumers read structured state.
- Remove markdown as operational state; retain it for authored narrative and
  generated exports.
- Collapse route replicas behind one authority plus observable repair.
- Deprecate granular agent-facing mutations after telemetry shows no consumers.
- Archive historical migration docs and generate API/reference docs from the
  schemas where practical.

Exit: every surface shows the same situation and performs the same validated
commands; no dual-write compatibility path remains.

## Workstream ownership

| Workstream | Primary home | Dependencies |
|---|---|---|
| situation projection, receipts, actions, leases | `sandboxed_sh` | project and mission stores |
| conversation wakes, cursor-aware controller scheduling | `hermes-agent` | situation and callback contracts |
| policies and knowledge promotion | Library + Hermes skills | action/grant schemas |
| compute receipts and resource costs | `sandboxed_sh` + `dgx-spark-arbiter` | receipt schema |
| operator projections | dashboard/mobile/desktop | situation/action APIs |
| reconciliation/export | `palomactl` | shared projection |

Each phase should ship vertically for one canary project before fleet-wide
migration. The canary must exercise rollover routing, one writer lease, a
waiting predicate, a failed attempt, exact-head evidence invalidation, and a
successful accepted receipt.

## Non-goals and constraints

- Do not move prioritization into sandboxed.sh; deterministic derivation there
  supports, but does not replace, Hermes judgment.
- Do not create a universal event-sourced rewrite before the projections and
  receipts prove their value. Existing stores can be adapted incrementally.
- Do not inject full histories into controller context.
- Do not make confidence a decorative model-generated number; derive it from
  evidence class and verifier where possible.
- Do not let learning mutate policy automatically across scopes without a
  promotion gate and rollback path.
- Do not couple migration to production restarts; follow the existing guarded
  deployment path and restart Hermes only after compatible MCP deployment.

## Required tests per phase

Every phase needs:

- schema/contract tests for omissions, unavailable sources, and old clients;
- idempotency and crash-between-writes tests for mutations;
- continuation and callback-routing tests;
- stale-head and evidence-invalidation tests;
- property tests for grant intersection and lease conflict;
- cost/context regression telemetry;
- one end-to-end canary receipt that an operator can independently replay.
