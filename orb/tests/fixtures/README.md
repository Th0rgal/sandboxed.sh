`hermes-jobs.json` is captured output from the real `cron.jobs` implementation in
[Th0rgal/hermes-agent at 882881de](https://github.com/Th0rgal/hermes-agent/tree/882881de28e3b71f330a530d9b1463fac9472ef8).
`generate.py` records the actual create, update and trigger results inside a new
temporary `HERMES_HOME`; it never starts a scheduler, session, or agent. IDs, clock
values and temporary workdirs are output from Hermes, not hand-authored records.
The one-shot input is deliberately far in the future so regeneration is safe.

The pinned sandboxed.sh submodule (60def691) predates reasoning/failure-delivery
fields. The current Hermes production source above has those storage fields,
but its REST whitelist still drops them. The companion
`patches/hermes/orb-cron-api-fields.patch` exposes the storage fields through
create/update; it also permits `context_from`, Hermes' actual representation of
continuity (`self`). It is a source patch for that Hermes revision, not applied
to any running service. No native session ID is used for continuity.
