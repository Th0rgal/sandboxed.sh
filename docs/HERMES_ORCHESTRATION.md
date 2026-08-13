# Hermes orchestration

How Hermes and sandboxed.sh work together to run autonomous projects for days
without a human in the loop.

[`HERMES_ASSISTANT_MIGRATION.md`](HERMES_ASSISTANT_MIGRATION.md) describes the
migration that made Hermes the assistant runtime. This document describes the
orchestration layer that runs on top of it: how a project keeps one durable
conversation, how work is attributed back to it, and what makes progress
visible.

---

## The split

**Hermes owns the conversation.** It wakes on a schedule, reasons, talks to the
operator, and decides what to do next. Its state lives in `state.db`.

**sandboxed.sh owns the execution and the record.** It runs missions in isolated
workspaces, stores their metadata, and answers questions about what happened.

Neither side duplicates the other's job. Hermes never stores mission state.
sandboxed.sh never decides what to work on.

---

## Four objects

| object | lives in | what it is |
|---|---|---|
| **session** | Hermes `state.db` | a durable conversation, e.g. `20260804_103847_86ca5c` |
| **mission** | sandboxed.sh | one unit of work, in a workspace, with an agent and a model |
| **controller** | Hermes cron | a job that drives one project, one agent turn per tick |
| **route** | Hermes `projects.db` | binds a project to the session it reports into |

A session carries a **source**: `desktop`, `webhook`, `cli`, `tui`,
`api_server`, `telegram`, `cron`. The source decides how a report reaches it.

A mission carries three tags that make it findable: `project`, `track`, and
`origin_session_id`.

---

## The cycle

```
  cron ──fires──▶ controller ──dispatches──▶ missions (sandboxed.sh)
                      │                            │
                      │                            │ origin_session_id
                      ▼                            ▼
                 delivery ──────────▶ session ◀── the operator reads here
```

1. The cron fires the controller on its interval.
2. The controller reads live state: PRs, CI, missions in flight.
3. It decides, then dispatches missions through the `sandboxed_assistant` MCP
   server.
4. It writes a report.
5. The report is delivered into the project's bound session.

---

## Project routes

A controller does not name a session. It declares `deliver: project:verity`,
and the route store resolves that name.

Routes are **explicit or nothing**. With no route bound, delivery fails and says
so. The system never falls back to "whichever conversation is open", because
that fallback is precisely the misrouting the store exists to prevent.

This replaced a worse arrangement. Each cron tick used to open a throwaway
session and end it, so a project accumulated one dead conversation per tick —
Verity had 52 for a single project. A route makes it one project, one
conversation.

```bash
# bind (Hermes side)
project_route_set project=verity session_id=20260804_103847_86ca5c

# bind (sandboxed.sh side, drives the board's Conversation button)
PUT /api/projects/verity/conversation  {"session_id": "..."}
```

`bind_route` validates both ends: the project must exist and be unarchived, and
the session must exist with a routable source. An operator can override the
source check with `allow_unroutable_source=True` when a human has adopted a
machine-created session as a real working conversation — that override exists
because no property of the row distinguishes an adopted session from a
throwaway one.

---

## Attribution: `origin_session_id`

Every mission a controller dispatches is stamped with the conversation it came
from. A Hermes plugin adds it automatically, so the model never has to pass its
own session id — it does not reliably know it, and a wrong id is worse than
none.

The plugin refuses to stamp an id prefixed `cron_`. A per-tick session dies with
its tick, so a mission filed under one is unreachable forever. An unstamped
mission is honestly unattributed and can be adopted later.

The `mission-complete` webhook must **not** open a throwaway
`webhook:mission-complete:<delivery>` session when it can route. After HMAC
auth: deliver into `origin_session` (follow continuations) if that session
exists; otherwise resolve `project:<slug>` via the explicit route store.
Only an unroutable payload may spawn an isolated webhook conversation.
Coldcard `acfb03d2` (2026-08-13) finished `Codex CLI not found` in a
throwaway session; the dedicated conversation stayed silent until Thomas
asked.

sandboxed.sh completes the picture: when a mission is created from a bound
conversation without a `project`, the server fills it in from the binding. An
explicit value always wins, including a deliberate blank. An unbound session
yields nothing — a wrong tag is worse than a missing one, because a wrong tag is
believed.

---

## Continuations

A conversation grows until Hermes compresses it and forks a **continuation**.
The successor carries an incremented title: `Verity dev #28`, `Lido #23`,
`Avancement du benchmark Verity #2`.

Routes follow continuations automatically: resolution walks the chain forward
and repoints every route bound to the old id, atomically.

**A continuation inherits the source of whoever resumed it.** Measured on one
production host: `desktop` 150, `tui` 96, `webhook` 45, `cli` 4. So resuming a
conversation from a terminal changes its source to `cli`, and the source decides
whether delivery works.

> When a route target looks dead, the question is not "where else can I deliver"
> but **"what succeeded it"**. The `#N` suffix is the successor convention.

---

## Delivery

Two families, and the distinction is load-bearing.

**Live-adapter sessions** — `telegram`, `discord` — receive a message through
their channel. A project route may not target them: that would let a project
silently re-target a chat thread.

**Transcript sessions** — everything in the gateway's
`NON_MESSAGING_SESSION_SURFACES` — have no adapter once their originating turn
ended. Their delivery surface is the persisted transcript, and the report is
appended to it.

The set is derived from that registry rather than hand-listed, because it is
default-deny (an unrecognised source counts as messaging) and because a
hand-kept list drops deliveries every time an operator touches a conversation
from a new surface.

---

## Seeing what happens

Three read paths, all built on the mission tags.

### Inventory

```
GET /api/control/missions?project=verity
GET /api/control/missions?project_prefix=verity   # the whole family
GET /api/control/missions?track=core-c3
GET /api/control/missions?origin_session_id=20260804_103847_86ca5c
GET /api/control/missions/resolve?id=6e4b117c     # short ids resolve
```

`project` is exact. `project_prefix` is hyphen-anchored: `verity` covers
`verity-core` and `verity-phase1d`, and never `verityx`.

### Health rollup

`GET /api/projects/overview` returns a `health` block per project: one verdict
per track, worst first — `failing`, `overdue`, `active`, `idle`, `done`.

Two distinctions carry the weight. *Active* means work is genuinely in flight,
which is narrower than "not terminal": `awaiting_user`, `acknowledged` and
`paused` are parked, and counting them as active is how a stalled track
disguises itself as a busy one. And a track that failed once and is already
retrying reads `active`, not `failing`, so real failures are not buried.

### State timeline

A controller ends its report with a trailer:

```
[STATE_SIGNATURE: verity|phase1-stack|7dba916|clean-ready|ci-failures-3-prs]
```

The **first field routes**; everything after it **describes the state**. A
background ingestor folds these into a durable timeline every 60 seconds,
collapsing consecutive repeats into one row with an observation count.

```
GET /api/projects/verity/state
```

### Provider depth

```
POST /api/model-routing/chains/builtin%2Fsmart/test          # does it work?
POST /api/model-routing/chains/builtin%2Fsmart/test?all=true # how deep?
```

Plain `test` reports `ok: true` as soon as the first entry answers. `?all=true`
probes every entry by its qualified `provider/model` id and returns
`entries_live` / `entries_total`. A chain with one live entry works and has no
fallback, and only the second call can tell you that.

---

## Design rules learned the hard way

These are not style preferences. Each one comes from an outage where every
dashboard said the system was healthy.

**A failure that reports success is the expensive kind.** Controllers ticked
`last_status: ok` while dispatching nothing. Routes resolved correctly while
every delivery was discarded. A chain answered `ok: true` with one live provider
of three. Prefer a signal that can say "I could not ask" over one that returns
an empty result.

**Distinguish "the server is down" from "the server said no".** An MCP circuit
breaker counted application answers — a benign `404`, an `Invalid UUID` — as
transport failures. The breaker is per *server*, so three of them took
`start_mission` offline with everything else.

**Prefer a denylist or a derived registry to an allowlist.** Three separate
gates were built as allowlists of the cases already seen — the breaker, the
delivery gate, the session sources. Each one dropped the next case that arrived.
Enumerating what you have seen does not cover what you have not.

**Budgets expire silently.** A cron job that spends its `repeat` budget sets
`enabled: false` with `last_status: ok`. Two controllers died that way and
nothing looked wrong for hours. Check the budget, not just the status.

**A tool description is the only thing an autonomous agent knows.** One
understated description led a controller to report a campaign blocked because it
believed an `acknowledged` mission could not be woken. It could.

---

## The guard contract

Every kill-switch in the stack — watchdogs, loop detectors, auth preflights,
circuit breakers — obeys three rules, each paid for by an incident where a
guard fired and the agent downstream **invented a cause** ("transport bug",
"GitHub is disabled here", "the pool needs 12 logins re-provisioned"):

1. **Evidence, not a verdict.** The guard names what it *observed* — the
   repeated substring, the selectors probed and the page they were probed on,
   the measured idle time — in its message, its logs, and the mission's
   `terminal_evidence` field, which rides the completion webhook next to
   `terminal_reason`.
2. **A guard that cannot check says so.** Silence reads as "checked, nothing
   found".
3. **The work survives the verdict.** Partial output is preserved below the
   notice, never replaced by it.

Compliance at the time of writing:

| guard | evidence | says-when-blind | preserves work |
|---|---|---|---|
| cron idle watchdog (600s) | ✓ `last_activity` | ✓ | n/a |
| degenerate-stream detector | ✓ repeated substring | n/a | ✓ since the adjacency fix |
| chatgpt_ui auth preflight | ✓ probed selectors + URL | ✓ fails closed loudly | n/a |
| MCP circuit breaker | ✓ triggering error class (denylist) | ✓ | n/a |
| skill-body guard | ✓ | ✓ warns with the skill name | n/a |
| webhook forwarder | ✓ marker divergence is the evidence | ✓ reconcile log | ✓ durable markers |

For agents, the mirror rule lives in `controllers-policy`: a `terminal_reason`
without evidence is missing data to report — never an invitation to guess.

## Known limitations

**Two stores hold the project↔conversation link.** Hermes `projects.db` follows
continuations automatically; the sandboxed.sh `project_bindings` table does not.
They drift apart every time a conversation rolls over, and the sandboxed.sh side
must be re-pointed by hand.

**Project tags drift back.** Missions are often tagged from a tracker filename
rather than the family slug, so families re-fragment over time.
`scripts/retag_project_missions.py` collapses them (dry-run by default,
journalled, with `--undo`), and `project_prefix` is the durable read-side
answer.

**Fallback depth is invisible by default.** Only `test?all=true` reveals it.
Consider running it on a schedule.

---

## The Hermes fork

The deployment fork lives at
[`Th0rgal/hermes-agent`](https://github.com/Th0rgal/hermes-agent) and is
vendored here as a git submodule at `third_party/hermes-agent`, pinned to its
`production` branch.

It is a reference, not a build input: nothing in this repository compiles or
imports it, and CI does not fetch it. It is here so the gateway, cron scheduler
and MCP plugin that this document describes can be read alongside the server
that answers them.

```bash
# fetch it
git submodule update --init third_party/hermes-agent

# clone with it
git clone --recurse-submodules https://github.com/Th0rgal/sandboxed.sh.git

# move the pin to the current production tip
git -C third_party/hermes-agent fetch origin production
git -C third_party/hermes-agent checkout origin/production
git add third_party/hermes-agent && git commit -m "chore: bump hermes-agent pin"
```

Changes to the fork follow its own `FORK.md`: branch off `production`, mark the
commit `[fork-delta]`, open a PR, rebase-merge. Production tracks
`origin/production`:

```bash
git fetch origin && git reset --hard origin/production
systemctl restart hermes-assistant hermes-dashboard
```

The pieces this document refers to:

| path in the fork | what it does |
|---|---|
| `cron/scheduler.py` | fires controllers, resolves `deliver: project:<slug>` |
| `hermes_cli/project_routes.py` | the route store, continuation migration |
| `hermes_cli/web_routers/missions.py` | serves a conversation's missions to clients |
| `tools/mcp_tool.py` | MCP client, circuit breaker |
| `gateway/session_context.py` | session surfaces, the transcript/messaging split |
