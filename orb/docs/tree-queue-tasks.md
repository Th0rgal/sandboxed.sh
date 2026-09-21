# Tree, queued follow-ups and tasks

The live sidebar uses `TreeNode` children and stable IDs for projects, missions,
Finished groups, folders and files. `visibleTree` derives depth, sibling position,
ancestor continuation columns and explicit expanded-parent links. One connector
renderer handles every row, including loading and empty notes. Lazy loading,
project actions, selection, prefetch and tooltips remain; arrow keys navigate the
visible hierarchy, skipping noninteractive loading/empty notes. PR #923’s folder
creation actions, mission-ID copy menus, project settings and editor changes are
preserved on the updated base. Legacy demo rows keep their separate styles.

User transcript rows retain the server message ID and a monotonic queued →
delivered state. HTTP receipts, SSE, stored history and the queue snapshot join by
ID. Identical text is never an identity. Queued events do not close the current
assistant response; delivery moves the row after that response. Pending rows sit
above the composer in order, without a hide/cancel control. Rejection keeps the
draft and attachments; retrying an uncertain HTTP result in the mounted view
reuses its client message ID. A queue read failure leaves history readable and
reports that pending state could not refresh.

## Durable queue evidence

The existing SQLite `control_queue` snapshot is persisted before busy-queue
acceptance and restored by the control actor. `GetQueue` exposes main and parallel
runner queues. Queued user events are deliberately omitted from stored history
(`src/api/mission_store/sqlite.rs::log_event`); dispatched events retain the ID.

Scheduled/capacity deferrals previously combined plain prompts and minted a new
ID at dispatch. Their existing `deferred_goal` value now also contains a versioned
identity envelope. Each accepted prompt and its original ID are written together,
including the initial create prompt. Scheduler retries preserve the batch; native
goal rewrites preserve its identities. The harness receives only the prompt.
Public SSE/history carry structured `messages` metadata, without the persistence
trailer. `/api/control/queue?mission_id=…` includes these pending deferrals and
validates the mission against the authenticated user's control store. It also
includes durable `inflight` proof after run acquisition, closing the gap before
the asynchronous transcript logger catches up; unscoped queue reads retain their
pending-only contract. A duplicate
HTTP receipt reports the actual queued state instead of claiming delivery.
Admission recognizes constituent IDs inside restored pending and consumed
scheduler batches while retaining one outer queue entry. Once a runner finishes
and leaves the snapshot, stored user events retain the original receipt IDs for
retry deduplication after a later restart. A failed receipt-history read refuses
new dispatch rather than risking duplicate execution.

No database migration is required. Existing plain deferred prompts still run, but
pre-upgrade individual IDs cannot be recovered from text that never stored them.
Only newly accepted envelopes provide that stronger reconciliation guarantee.
Status changes and absence from a queue snapshot are never delivery evidence.
The UI retains known pending entries until a delivered ID is observed; external
queue removal/cancellation has no v1 UI lifecycle and no local dismiss action.

The HTTP regression reopens SQLite with a new actor, retries the same accepted ID,
then lets the scheduler dispatch through the native driver fixture. It checks
ordered original IDs in replay metadata, exact immutable attachment versions in
the actual turn cwd, and absence of transport metadata in harness input. It also
checks unknown-mission rejection and retry after dispatch, goal clearing and a
second restart. A separate restored-batch regression checks pending and consumed
IDs without serializing aliases or adding another delivery. PR #922's immutable attachment snapshots and
steer acknowledgments remain intact. Public create/follow-up/resume content rejects
the reserved attachment-reference prefix before accepting work, with a clear 400
error and retained Orb draft; server-generated references still fail closed when
their immutable snapshot is missing. The shared tree model is named
`treeModel.ts` to avoid case-insensitive resolution conflicts with `Tree.tsx`.

## Work and task adapters

Work summaries recognize the actual native read/search, shell and edit names,
including Claude capitalization, OpenCode camel-case file arguments and Codex's
synthetic `bash` events. File counts require explicit unique targets; unknown
targets use operation counts. Unclassified tools remain counted as other tools.
Running shimmer and expandable raw arguments/results remain.

TodoWrite, update_plan and todowrite have explicit schema adapters. The Codex
translator now retains native `turn/plan/updated` notifications (previously
dropped), normalizing `inProgress` while retaining raw notification details. Invalid or
unknown-status payloads cannot replace the latest valid list. An empty valid list
clears it. The current list and completed/total progress are outside the work fold;
Tasks scrolls to it. Raw historical tool calls remain available. Source declarations,
pinned upstream links and fixture provenance live in `tests/fixtures/task-sources`.
Claude/Codex values are schema-derived compatibility fixtures; the OpenCode value
comes from its actual tool test. None is claimed to be a production capture.

## Plan viewing is gated off

This backend's `AgentEvent` contract (`src/api/control/events.rs`) has no verified
plan-artifact reference. The native Codex translator forwards tool names/arguments
and synthetic command events (`src/backend/codex/mod.rs`), without an artifact
locator. The available Orb reader is `readProjectFile` / `src/api/project_files.rs`,
which reads project-hosted files, not arbitrary mission-workspace artifacts.
There is therefore no supported discovery-to-reader path for a mission plan.
No Plan button, `/plan` command, Build mode, filename inference or checklist-title
inference is exposed. Checklist navigation works independently. A future viewer
requires a verified artifact reference bound to a supported read-only reader.

## Verification

- `pnpm test`, `pnpm build` (includes typecheck), full Playwright suite.
- Unit coverage: tree continuation/termination at arbitrary depth, source-shaped
  todo parsing, malformed lists, work counts, receipt/SSE/history ordering,
  identical text, stale queued replay, coalesced identities and attachments.
- Browser coverage: six Finished children in both themes; five nested levels,
  files/missions, loading/empty/collapsed/selected states, keyboard navigation;
  combined queue/checklist UI, rejection, retry, reload and reconnect delivery.
- Rust: formatter/check; deferred envelope tests; HTTP actor/admission regressions
  including the restart/dispatch test; existing payload/steer regressions.

Browser evidence is emitted to `orb/test-results/orb-tree-*.png` and
`orb/test-results/orb-queue-tasks-*.png` and included in the CI Playwright artifact.
See the PR body for final command results and mission-local retained screenshots.
