# Native Codex continuity

A native `/goal` already iterates inside one running app-server. This change
also preserves that native thread when sandboxed.sh ends an outer run and
later resumes the mission. It does not replace the mission actor, ownership
admission, durable jobs, source pins, or independent review.

## Identity and rollout

New Codex missions bind their actual `thread.id` before any turn or goal
starts. The durable authority is
`<backend working_dir>/.sandboxed-sh/codex-sessions/<mission UUID>.json`.
The existing mission `session_id` stores a `codex-thread:<native id>`
projection; the local backend `Session.id` remains a separate transient
handle. A lost projection is repaired from the binding. A projected native
id without its binding is an error, never permission to start over.

The binding records mission/workspace ids, canonical cwd/HOME/CODEX_HOME,
directory device/inode identities, and a SHA-256 account fingerprint.
OAuth uses the stable ChatGPT account id, not access or refresh tokens.
No credentials are persisted in the binding. Selected model, effort and
fast tier are applied again on resume without replacing history. A new
account cannot transparently replace the account owning a bound thread;
the selector uses that account or reports its unavailability/capacity.
This restriction is independent of Hermes provider backups.

Existing unbound missions with history, a legacy session id, or native
rollout files keep the legacy path. They are not retroactively migrated or
blocked during rollout. Their logs explicitly identify legacy continuation.
The supported migration is a controlled checkpoint handoff to a new mission:
retain the old checkout/artifacts and objective, finish/release its ownership
through the existing handoff, then start the replacement with that checkpoint.
There is no automatic reconstruction of lost native history or native usage.
Never copy a transient handle into a native binding or delete a fence to
make a failed resume pass. Direct adoption of legacy native threads is not
implemented by this patch.

Back up the binding directory together with each mission HOME/CODEX_HOME and
the mission store. Moving/restoring directories changes identity and requires
explicit reconciliation; a path that happens to have the same name is not
accepted as the same native state.

After creating bound missions, a binary rollback to a backend predating this
contract is not transparent: that backend ignores bindings and the new blocked
reason. Keep those missions out of dispatch/resume until a compatible backend
is restored, while preserving their state and ownership. Restoring an old
mission database is not a safe substitute.

## Resume, stop and recovery

The initial ordinary turn receives the existing framed instructions. Later
turns use `thread/resume` with the native id and send only the current input;
the bounded reconstructed transcript is not injected again. Goal resume reads
the actual native goal first. Setting only its status preserves objective,
budget, tokens used and elapsed usage. A missing previously observed goal,
changed objective or exhausted budget cannot reset the counters. An exhausted
budget with a native stopped status retains that exact native evidence.

Cancellation pauses a bound goal and interrupts its turn, then stops the
process. It does not clear the goal. The attachment file lock remains held
until child shutdown is awaited, including initialization/priming failures.
A pause is reported only after `goal/get` confirms it; a failed or ambiguous
pause requires reconciliation. The attachment lock covers app-server lifetime,
not the earlier workspace/auth preparation; existing workspace and account
admission still protect that preparation.
A durable creation fence immediately before `thread/start` prevents another
thread when the creation response was lost. Pre-start handshake failures do
not create that fence.

The existing single transport reconnect remains the only in-run reconnect
engine. It reuses the same id/account/settings. Terminal receipts from a
resumed snapshot can recover a missed turn/goal stop. An unresolved turn or
tool result requires reconciliation. Tool notifications are journaled with
file and directory fsync; unknown outcomes remain fenced across process
restart. Completed receipts are interpreted, not commands replayed. This is
not an exactly-once guarantee for external side effects: native notification
delivery and arbitrary shell commands are not transactional with this journal.
Existing durable jobs and their idempotency keys remain necessary.

Binding/identity/reconciliation failures produce
`terminal_reason=codex_continuity_required`, park the mission as `blocked`,
retain track/PR ownership, and suppress finished-turn and board retries.
They do not claim a native terminal event. A missing active turn id or
unconfirmed steer is explicit; an undelivered hint is not automatically
repeated. The 30-second wait for a new turn id bounds hint delivery only,
not normal inference or a healthy tool operation.

## Validation and limits

Behavioral tests run real subprocess stdio against a synthetic JSON-RPC
app-server without network/model access. They cover two outer runs, current
input versus transcript, model overrides, goal counters, active-turn steering,
cancellation, EOF reconnect, missing receipts, identity loss, concurrent
attachment, early cleanup and journal fences. HTTP/actor tests cover both
serial and parallel routes, persisted blocked status, live/offline PR claims,
track lease renewal and suppression of a real AgentFinished automation.
Board settlement and restart recovery retain blocked dependencies.

Locally generated experimental schemas were checked for Codex 0.153.4 and
compute-node Codex 0.144.1. A separate compute canary on 0.144.1 created a
paused goal, stopped the process and resumed the same thread/cwd with an
identical goal and zero token usage. It invoked no model. That establishes
the native storage/RPC contract, not a full live mission acceptance test.
Fixtures do not emulate inference scheduling, OAuth services or nspawn.
