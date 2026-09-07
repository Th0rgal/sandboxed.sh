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

Assignment changes require quiescence. Send/resume retags and structured project
edits run in the owning actor and refuse with `assignment_busy` while that actor
holds an old runner, queued message or background task, a durable run or remote
job handle remains open, any deferred objective remains, or the mission is Active
or Pending. Stale remote heartbeats do not prove that the job stopped. Cancel
timeout keeps the runner registered until its aborted task actually finishes. Project/track/PR changes and assignment tag/intent changes
cannot transfer claims across that boundary. Same-assignment continuation may
still queue normally. Title-only edits do not change the assignment.

Ownership is independent of presentation status. Sweep and global PR arbitration
read accepted/tentative remote handles, durable nonterminal runs, pending targeted
queues and actor-owned work across live sessions, offline SQLite and legacy file
stores. A `Failed`, `Interrupted` or acknowledged-looking row does not retire an
unresolved execution. Store or ledger read failures keep the claims fenced.
Native `Blocked/native_goal_stopped` retains its existing ownership semantics.

Lease timestamps request renewal/reconciliation; they do not transfer ownership.
An overdue writer claim still excludes competing acquisition, reader promotion
and queued revalidation until reconciliation or an explicit rollback releases it.
Same-owner dispatch-key replay continues to use the unreleased claim.

Remote polling can present `Failed/remote_node_lost` after repeated observation
failure while still holding both claims and retrying cancellation. A cancellation
acknowledgment, a missing job record (404), a node-restart `lost` state or a stale
heartbeat is not termination proof. Raw-job, remote-build and recovery observers
retire handles only for confirmed `succeeded`, `failed` or `cancelled` execution
responses. The ledger rejects finalization with an unconfirmed state. Received
terminal proof is retained across transient persistence failures so cleanup can
retry even if the node subsequently becomes unreachable. Missing node
configuration or credentials leaves recovery pending; it does not erase the
handle. A job that remains only `lost` or missing stays fenced pending terminal
reconciliation. Do not delete its ledger entry to admit a replacement writer.

Node execution reports a terminal result only after its process group and any
configured containment scope have confirmed cleanup. Failed cleanup keeps the
job running and retries, including after command-wait errors. Zombie-only process
groups cannot execute and do not prevent cleanup; unavailable scope-manager
queries do. This guarantee requires the corrected node runner as well as the
controller: an older node's premature terminal response is not independently
verified by the controller.

Pending deferred objectives are retained under their original assignment;
retags are refused instead of concatenating a new assignment into old deferred
work. Stop and drain the old work before retagging, or create a separate mission.
Multiple retag attempts behind a running/queued turn are all refused. Consequently
dequeue cannot observe an assignment changed by these APIs while its old message
was waiting: the actor serializes edits with dequeue and keeps the assignment
fixed through the last queued turn. Dequeue also revalidates PR exclusivity and
checks/renews the original track claim under a SQLite write transaction; a
missing or replaced claim refuses execution instead of silently rebinding work. Direct database writers are outside this
cooperating-controller protocol.

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

Successful acceptance retains the new lease and releases the old leases. A retag
reaches this step only after quiescence was checked before mutation; acceptance
alone is not evidence that the old execution stopped.
Cleanup failure after acceptance is not reported as a rejected dispatch. Both
leases remain fenced and an error is logged. Known `accepted`/`rejected` journal
phases are recovered before the next admission or project edit. Legacy accepted
retags without the new lifetime marker stay fenced for operator reconciliation,
because their original acceptance did not establish quiescence. If an outcome
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
