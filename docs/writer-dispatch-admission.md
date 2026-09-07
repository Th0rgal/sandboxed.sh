# Writer dispatch admission

`continue_identity` is a trusted controller assertion about the objective and
an exact precondition on stored project, track and PR. It is not semantic proof
that a prompt continues the assigned work. A controller that copies current
tags onto an unrelated objective defeats the prose heuristic. The server still
checks identity equality and current ownership. An unrecorded PR stays null;
PR numbers in exclusions or collaborator context never grant ownership.

HTTP send and resume carry the assertion/explicit edits into the actor. They do
not edit mission metadata before enqueueing a command. The actor reloads the
mission under admission serialization shared with project/title edits and the
lease sweep. Lock order is process admission mutex, `.dispatch-admission.lock`,
PR writer lock, stores. The advisory file lock covers cooperating backend
processes using the same mission directory. Internal targeted wakes use the
same ownership admission without applying controller prose inference.

Admission checks resume state, the proposed assignment, PR exclusivity and track
ownership, including unchanged tracks, absent PR metadata, PR-only edits and
reader-to-writer promotion. Provisional acquisitions use distinct lease keys so
rollback never releases the original reader/writer lease. Title and assignment
fields share a single mission-store write. File-store writes publish the new
in-memory snapshot only after the disk replacement succeeds.

This is a journaled cross-store protocol, not a transaction spanning both SQLite
databases. `mission_dispatch_admissions` in `projects.db` records the previous
assignment, proposed assignment, provisional lease ids and admission phase before
identity persistence. The original leases remain held until the actor accepts
the message/resume. A known actor rejection restores identity/title
before the HTTP response; client disconnection does not cancel cleanup.

Successful acceptance retains the new lease and releases the old leases.
Cleanup failure after acceptance is not reported as a rejected dispatch. Both
leases remain fenced and an error is logged. Known `accepted`/`rejected` journal
phases are recovered before the next admission or project edit. If an outcome
was lost with the actor response or to a crash (`pending`), the server refuses with
`dispatch_recovery_required`: it cannot safely infer whether queued work was
accepted. The sweep preserves and renews these leases, and writer acquisition
honors the journal fence even past lease TTL. PR arbitration also fences both
the previous and proposed PR in unresolved journals, even without a track. Operator reconciliation is
required for an unknown outcome; do not erase that journal to bypass refusal.
This schema addition is additive. Rollback to a version that ignores these
fences requires resolving outstanding admissions first.

MCP resume sends one HTTP request containing the custom prompt and assignment.
It does not replay identity edits through a second send. The actor persists the
resume prompt before dequeue; failure of that enqueue rejects and rolls back.
If enqueue succeeded but dequeue persistence fails, the prompt remains durably
queued under the accepted assignment. Backend, MCP and Hermes contracts must be
rolled out together. Older backends do not provide this atomic resume contract.

Goal reporting remains independent: `mission_mode=task` may have
`goal_mode=true`; the bounded objective is a preview of durable goal state.
