# Native Codex goal stop lifecycle

The observed defect left an outer run active after the native goals database
reported `blocked`. The translator previously terminated only for `complete`
and `budgetLimited`; forwarding the status event alone did not release the
app-server driver. This change treats a native stop as a drained, resumable
run rather than successful completion.

## Local contract evidence

The installed `codex-cli 0.153.0` generated these experimental types with
`codex app-server generate-ts --experimental --out <temporary directory>`:

```typescript
export type ThreadGoalStatus = "active" | "paused" | "blocked" | "usageLimited" | "budgetLimited" | "complete";
export type ThreadGoalUpdatedNotification = { threadId: string, turnId: string | null, goal: ThreadGoal };
export type ThreadGoalSetParams = { threadId: string, objective?: string | null, status?: ThreadGoalStatus | null, tokenBudget?: number | null };
```

The generated schema establishes wire shape, not notification ordering. Tests
explicitly cover goal updates before turn start, during a turn, and after turn
completion. A stop with `turnId` waits for that turn's completion, all active
turns, and pending tool results. A stop with null `turnId` and no active work
can exit immediately. Unrelated/replayed turn completions cannot end an active
turn. Explicit `active` revokes a pending stop. Silence and idle observations
never establish a stop; there is no new timeout or live-build cancellation.

## Outer contract

`blocked`, `paused`, `usageLimited`, and `budgetLimited` become
`TerminalReason::NativeGoalStopped` only after the event consumer drains.
They preserve the exact native status/objective as terminal evidence and the
last final response. With no queued input the mission becomes `blocked` with
`terminal_reason=native_goal_stopped`. They never produce a successful goal
completion. The app-server shuts down before emitting stream completion;
shutdown does not call `thread/goal/clear`.

Accepted queued steering runs once in order through the existing actor and
ownership admission. Finished-turn automations and board retries do not
self-retry a native stop. With queued follow-ups the mission can remain active
and eventually park according to their results; the blocked response and goal
status events remain in history. Goal metadata and writer identity are not
cleared. Track lease sweeps renew parked native goals, and both live-store and
offline SQLite PR arbitration retain their writer claim. Other terminal
mission ownership semantics are unchanged. `goal_mode` records persistence,
not whether a driver is live.

Default resume of a persisted Codex goal reuses the full stored objective,
not the compact MCP preview. Explicit content stays exact; plain steering is
a single turn and an explicit `/goal` re-arms a loop with status `active`.
The existing driver still starts a fresh native thread per outer turn; this
patch does not introduce native-thread reuse or migrate native goal counters.
Original native goal records and mission transcript evidence are retained.

## Regression boundaries

Translator tests cover the native status family, notification ordering,
replayed turns, pending tools, no active work, reactivation, and non-goal
isolation. HTTP/actor tests run a local Python JSON-RPC app-server fixture
through the production Rust transport, driver, event consumer, durable queue,
main/parallel runner and SQLite mission store. A test-only provider-launch
seam avoids credentials and workspace provisioning; no live model is invoked.
The fixture emits blocked evidence and final text in controlled orders,
accepts two queued steers, and verifies same-objective default recovery and
retained ownership. All databases/files in those tests are temporary.
