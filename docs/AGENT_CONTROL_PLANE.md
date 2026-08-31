# The agent-native Paloma control plane

Status: target architecture and design constitution. The implementation sequence
is in [`AGENT_NATIVE_ROADMAP.md`](AGENT_NATIVE_ROADMAP.md).

Paloma should feel to a controlling agent like one coherent instrument, not a
collection of databases, dashboards, skills, crons, and mission tools. The
agent's job is to choose the best next intervention. The system's job is to
make the relevant reality cheap to perceive, safe to change, and hard to
misrepresent.

The governing loop is:

```text
intent -> observe -> decide -> act -> receive receipt -> reconcile -> learn
```

Every Paloma component exists to make one edge of this loop reliable. Hermes
holds the operator conversation and the coordinator's working judgment;
sandboxed.sh owns canonical structured project intent, authority, execution,
observations, and receipts. Hermes proposes intent changes through project
commands rather than maintaining a second writable intent store. Skills supply
policy. Projections make the whole loop legible to an agent and operator
without creating another source of truth.

## The design objective

Optimize for **verified progress per unit of attention, context, compute,
money, and operator interruption**.

This produces seven requirements:

1. The cheapest normal read returns a faithful, bounded situation.
2. Declared intent is never confused with observed reality.
3. Every consequential action has one owner, an authority basis, an
   idempotency key, and a receipt.
4. Completion means accepted evidence satisfies an explicit condition, not
   that an agent said it finished.
5. Waiting is represented as a wake condition, not recurring thought.
6. Detail is progressively disclosed; routine ticks do not replay history.
7. Experience accretes into reusable system knowledge with evidence, scope,
   tests, and decay—not ever-growing prompt folklore.

## The abstraction tower

Each layer answers one question and has one canonical identity. Higher layers
refer to lower layers by stable handles; they do not copy lower-layer state.

```text
Portfolio                    What deserves attention across the whole system?
  Project                    What durable outcome are we pursuing, under what grant?
    Track                    What independently satisfiable condition remains?
      Attempt (mission/job)  Who is trying which bounded intervention right now?
        Action               What external mutation or computation was requested?
          Receipt            What does the system prove happened?
            Evidence         Which immutable observation supports the claim?

Conversation route          Where does judgment and operator communication continue?
Resource lease              What scarce capacity/authority is temporarily reserved?
Knowledge item              What reusable lesson was promoted from experience?
```

The side objects are orthogonal. A conversation is not a project, a mission is
not a track, a track is not a branch, and a transcript is not evidence.

### Current-to-target map

The target model evolves the existing system; it does not discard it.

| Current mechanism | Target interpretation | Remaining gap |
|---|---|---|
| `projects` + `project_grant` | project intent and authority | typed success/abandonment conditions and revisions |
| `project_tracks`, proposals, boss-board tasks | track | one canonical track contract and verifier-bound acceptance |
| mission row | attempt | explicit generation, owner lease, and causal action |
| mission event / terminal evidence / PR or job handle | evidence candidate | normalized evidence references, freshness, and acceptance |
| decision row + tool response + placement record | partial action receipt | one durable receipt schema across all action kinds |
| `project_bindings` + Hermes route replica | conversation route | visible lag/reconciliation behind one authority contract |
| mission caps, writer flag, fleet slots | partial resource/ownership lease | uniform scoped leases with expiry and holder evidence |
| skills, runbooks, tests, incident notes | knowledge | provenance, promotion, supersession, and review lifecycle |

Compatibility projections may preserve old shapes during migration, but new
concepts should enter at the target layer rather than create another parallel
representation.

### Portfolio

The portfolio is a projection, not a new store. It ranks projects by attention
need and opportunity. It should answer, in one bounded read:

- what needs the operator;
- what can make progress now;
- what is consuming resources without progress;
- what changed since the previous cursor;
- where capacity would have the highest expected value.

### Project

A project is the durable control envelope:

- a stable slug and objective;
- success and abandonment conditions;
- autonomy and resource grant;
- active tracks and their dependencies;
- one canonical control-conversation route;
- open decisions and current attention state.

The project stores intent and authority. It does not duplicate mission logs,
GitHub state, or host health.

### Track

A track is the smallest durable unit that can independently become satisfied,
blocked, cancelled, or obsolete. It carries:

- a stable key and desired condition;
- machine-checkable acceptance criteria where possible;
- dependencies on other tracks or external conditions;
- exactly one semantic owner or an explicit `unowned` state;
- a current attempt, if any;
- a wake condition and next-check deadline when waiting;
- an evidence policy describing what can close it.

“Fix PR #42” can be a track. “Campaign” is usually a project. “Run Codex” is
an attempt, not a track.

### Attempt

A mission is one attempt to advance a track. Attempts are disposable and may
fail without corrupting the project model. They inherit the track's objective
and constraints but record their own backend, workspace, model, resource
lease, branch, and terminal outcome.

At most one attempt owns a mutation domain such as a branch. Parallel readers
are fine. Parallel writers require disjoint leases.

### Action, receipt, and evidence

An action is a requested mutation or computation. Its result is a receipt, not
a prose assertion. A useful receipt records:

- action kind, target, actor, authority basis, and idempotency key;
- start/end timestamps and terminal disposition;
- immutable external handles: mission UUID, commit SHA, PR URL/head, build ID,
  node/job ID, deployment version;
- evidence references and verification time;
- cost and resource usage when available.

Evidence is immutable or content-addressed whenever practical. It has a source,
observation time, freshness class, and verifier. A mission transcript can point
to evidence; it is not automatically evidence itself.

## One situation model

The primary agent read should be `get_situation`, exposed through the MCP and a
matching HTTP endpoint. It is a materialized projection over existing stores,
not a new authority. Its response is bounded and layered:

```json
{
  "as_of": "...",
  "cursor": "opaque-monotonic-cursor",
  "scope": {"project": "verity-core"},
  "attention": [{"kind": "decision", "ref": "...", "severity": "high"}],
  "intent": {"objective": "...", "grant_version": 7},
  "tracks": [{
    "key": "parser-soundness",
    "desired": "all acceptance checks pass at an immutable head",
    "derived_state": "executing",
    "owner": {"attempt_id": "...", "lease_until": "..."},
    "wake": null,
    "acceptance": {"satisfied": 2, "total": 4},
    "latest_receipts": ["receipt:..."]
  }],
  "resources": {"binding_constraints": [], "warnings": []},
  "changes": [],
  "omitted": {"tracks": 0, "receipts": 14},
  "links": {"track_detail": "...", "evidence": "..."}
}
```

The response separates four epistemic classes:

| Class | Meaning | Examples |
|---|---|---|
| **intent** | what should be true | objective, desired state, grant |
| **observation** | what a source measured | PR head, process exit, node capacity |
| **derivation** | deterministic conclusion from observations | overdue, lease conflict, acceptance 3/4 |
| **judgment** | a controller's defeasible interpretation | best next action, hypothesis, priority |

Every observation includes `observed_at`, source, and freshness. Every
derivation exposes its inputs or rule version. Judgment is explicitly labeled
and never overwrites observation.

### Progressive disclosure

Normal operation uses three read depths:

1. `get_situation(scope, since_cursor)` — attention, deltas, and the working
   set; target a small fixed payload.
2. `get_track(key)` / `get_attempt(id)` — one bounded object with recent
   receipts and omissions declared.
3. `get_evidence(ref)` / paged events — raw detail only for dispute,
   diagnosis, or audit.

All list APIs should support `since_cursor`, server-side filters, stable sort,
caps, and explicit omission counts. An empty result must distinguish “nothing
matched,” “source unavailable,” and “not checked.”

## The control protocol

A controller tick should be a cheap transaction against a changing world:

1. Read `get_situation(project, since_cursor)`.
2. If there is a pending callback or operator decision, reconcile it first.
3. Select the highest-value ready track without a valid owner.
4. Preview the intended action when it crosses policy or resource boundaries.
5. Execute one idempotent control command.
6. Persist the returned receipt and expected wake condition.
7. End. Do not poll while the system can wake the conversation on change.

The next tick receives the previous cursor and a compact delta. A full snapshot
is required only after cursor expiry, schema change, or detected inconsistency.

### Intent-level commands

The long-term MCP should expose a small control vocabulary:

- `get_situation`
- `get_track`
- `propose_action`
- `execute_action`
- `reconcile_receipt`
- `answer_decision`
- `get_evidence`

`execute_action` is a typed command such as `dispatch_attempt`, `steer_attempt`,
`merge_pr`, `pause_project`, or `revise_plan`. The server resolves lower-level
calls, validates the grant and leases, assigns an idempotency key, applies all
related writes atomically, and returns a receipt.

Existing granular tools remain as implementation primitives and compatibility
surfaces. Agents should not have to remember that dispatch currently means
`start_mission` followed by `link_mission_to_project`, a decision-ledger write,
and a status update. One logical act must not require four fallible writes.

### Optimistic concurrency and idempotency

Every mutable project object has a revision. Commands accept the revision or
situation cursor they were based on. A stale command fails with a compact
conflict response containing the changed fields and a fresh cursor.

Every external mutation carries a stable logical `action_id` and a canonical
request fingerprint. The effective idempotency key is derived from project,
`action_id`, and fingerprint. Retrying the same action after a timeout returns
the original receipt rather than launching a duplicate mission or repeating a
merge. Reusing an `action_id` with a different fingerprint is a typed conflict,
never an implicit retry. Successive steer or plan-revision commands therefore
use distinct action IDs even within one attempt generation; controllers may
derive deterministic IDs from the transition or wake event they are handling.

Idempotency crosses the crash boundary. Before an external effect, the command
service durably records the action as `prepared` with its ID, fingerprint,
target, and expected outcome. It then uses the provider's idempotency token when
available and advances the record through `dispatched` to `confirmed` with the
receipt. Recovery reconciles `prepared` or `dispatched` records against the
provider or target before retrying. If the outcome cannot be proved, the action
becomes `ambiguous` and blocks automatic repetition until reconciliation or an
explicit operator decision. An in-memory receipt cache is never sufficient.

### Wake conditions

Waiting is data:

```json
{
  "kind": "github_check_terminal",
  "subject": "owner/repo#42@abc123",
  "deadline": "...",
  "fallback": "reconcile_ci_timeout"
}
```

Event delivery wakes the bound conversation when the condition changes.
Deadlines are safety nets, not polling intervals. A controller that has no
ready action and a valid wake condition should consume no model turn.

## Authority, ownership, and resources

The grant should be typed and compositional. Its effective authority is the
intersection of:

- operator grant;
- project scope;
- action-kind policy;
- target-specific restrictions;
- current resource budget;
- separation-of-duties requirements.

The system computes and returns `allowed`, `requires_review`, or `forbidden`
with the exact rule and remediation. Models should not infer authority by
re-reading prose.

Ownership uses explicit leases over semantic mutation domains:

- project controller lease;
- track owner lease;
- repository/branch writer lease;
- scarce provider or compute slot lease.

Leases have holders, scopes, expiry, renewal evidence, and a monotonically
increasing fencing generation. Every mutation gateway validates the presented
generation against the current scope before accepting a write, so an expired
holder cannot resume and mutate after reassignment. Where a target cannot
validate fencing (for example, a direct Git credential), Paloma must revoke or
terminate the old writer and prove that revocation before granting the next
lease. Expiry alone never authorizes concurrent writers. “Another controller
owns it” is true only when a live fenced lease or observable action says so.

Resource policy should expose constraints and marginal cost, not a giant raw
fleet dump. Placement returns a reasoned receipt: chosen node/provider,
alternatives considered, binding constraint, expected cost, and fallback.

Budgets are enforced at action boundaries and aggregated by project, track,
attempt, provider, and outcome. The useful metric is cost per accepted receipt,
not merely token count or mission count.

## Completion and truth

Each acceptance criterion declares a verifier class:

- `external_state`: GitHub/API/database state;
- `command`: command, environment identity, exit status, artifact digest;
- `review`: reviewer identity plus immutable head;
- `operator`: explicit owner decision;
- `manual`: named evidence and expiration.

A track becomes `satisfied` only when all required criteria have fresh accepted
evidence at the governed artifact version. If automation advances a PR head,
head-bound evidence becomes stale automatically. A terminal mission merely ends
an attempt.

Project mode should be a derivation wherever possible. The underlying facts are
independent facets; the single display mode evaluates them in this explicit
precedence order so every surface projects the same result:

1. `inconsistent`: authoritative sources disagree or are unavailable;
2. `complete`: project acceptance is satisfied;
3. `paused`: the grant says pause;
4. `executing`: at least one valid live owner;
5. `ready`: at least one unblocked track has no owner;
6. `blocked`: with no executing or ready track, at least one open track lacks a
   valid wake path;
7. `waiting`: every open track has a valid wake condition.

These predicates are total under the project invariants: after excluding an
owner and a ready action, any track without a wake path makes the aggregate
`blocked`; otherwise all open tracks are waiting. No open tracks without
satisfied project acceptance is an invariant violation and therefore
`inconsistent`.

For example, a project with an owned running track and a second unowned ready
track is `executing`, while retaining `ready_track_count > 0` as a facet for
scheduling and diagnostics. Mode does not erase the facts used to derive it.

Human-friendly `active` can remain a display grouping. The richer derived state
prevents “active but doing nothing” from being a valid steady state.

## Failure semantics

Failures are typed by layer:

- `transport`: the request could not reach or complete with the service;
- `rejection`: the service answered and refused the request;
- `execution`: the attempt ran and failed;
- `verification`: claimed output could not be proved;
- `policy`: authority or safety denied the action;
- `capacity`: a resource was unavailable;
- `inconsistency`: sources disagree;
- `unknown`: required evidence was not collected.

Each failure carries observed evidence, retryability, blast radius, and a
recommended next control action. Guards report observations and blind spots;
they never fabricate a root cause.

## Accretive intelligence

Paloma should improve from operation without turning every incident into more
prompt text. Knowledge promotion follows a ladder:

```text
episode -> candidate lesson -> validated pattern -> policy/guard -> regression
```

A knowledge item records:

- the triggering receipts and evidence;
- the generalized rule and its scope;
- counterexamples and invalidation conditions;
- confidence, owner, review date, and supersedes links;
- its executable form, if any: test, linter, policy rule, tool description,
  routing hint, or runbook.

Promotion rules:

1. Keep one-off facts in the episode/receipt, not a global skill.
2. Promote repeated or high-severity patterns only after independent evidence.
3. Prefer an executable guard or schema constraint over prose.
4. Keep the smallest skill text that explains when and why to use the guard.
5. Attach a regression or expiry review to every promoted operational rule.
6. Record supersession; do not append contradictory folklore indefinitely.

Successful plans accrete too. A completed track graph can become a versioned
template with parameterized acceptance criteria, resource profile, and outcome
statistics. Future planning retrieves the closest validated template rather
than replaying old transcripts.

## Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| Hermes | conversation, judgment, controller scheduling, operator interaction | mission truth, fleet truth, duplicated project execution state |
| sandboxed.sh | project intent store, attempts, leases, observations, receipts, projections, isolated execution | autonomous prioritization or unreviewed judgment |
| mission harness | bounded work in one workspace | project authority, canonical status, final verification |
| Library/skills | versioned policies and reusable procedures | volatile project state or secrets |
| dashboard/mobile/CLI | projections and explicit commands | independent business logic or shadow state |
| fleet/arbiter | capacity, placement, job receipts | project priority beyond supplied policy inputs |

The system may replicate for availability or integration, but every replicated
field must name its authority, reconciliation direction, lag signal, and repair
mechanism. “One bind, two readers” is acceptable; two writable truths are not.

## Design laws

1. One concept, one canonical identifier, one authority.
2. Intent, observation, derivation, and judgment stay distinguishable.
3. One logical action produces one atomic state transition and one receipt.
4. No success without evidence; no blocker without a failed available action or
   an explicit unavailable dependency.
5. No silent omission: unavailable and unchecked are data.
6. No polling when a durable wake condition exists.
7. No broad scan when a cursor or indexed projection can answer.
8. No prose-only invariant when a type, constraint, lease, or test can enforce it.
9. No permanent lesson without provenance, scope, and a retirement path.
10. Human and agent surfaces read the same projections and invoke the same
    commands.

These laws are the test for future features. A feature that adds another state
store, asks the model to join raw records, or requires periodic reasoning to
notice a deterministic condition should be redesigned before implementation.
