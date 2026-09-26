# Orb side agents (`/btw`)

`/btw` opens an independent agent conversation in the side panel. Settings → Btw
selects its harness and model; the defaults are OpenCode and `builtin/smart`.
Settings apply to the next question. Changing the harness, model, or source
placement creates a fresh agent session with the previous side conversation as
context.

The agent runs on the source mission's host and in its working directory. It
has the harness's normal tools and permissions, with no extra Orb tool filter
or token budget. Files are shared: edits are immediately visible to the main
agent. Side questions do not enter the main agent's prompt or stop its run.

Each turn includes the recorded main conversation, the current visible
transcript, and side conversation history. This is historical context, not a
live subscription while the side agent is answering. Attachments are uploaded
to the target host and their paths passed to the agent; local images also use
native harness attachment arguments.

Core creates the side mission through `POST /api/control/missions/:id/btw/agent`.
Only that internal launch path permits the source workspace to remain occupied.
Side missions carry `btw-parent:<id>` and are excluded from Orb's conversation lists, project folders, and global archives. They remain accessible by ID for the side panel's transcript and lifecycle operations. Archive pagination counts raw rows before filtering, so hidden sessions cannot skip subsequent pages.
Local runs use the ordinary client-run protocol with a distinct run and harness
session identity. Remote runs use the regular mission runner.

The panel stores its conversation and child mission identity in connection-scoped
local storage. Reload reconnects to the stored child. Closing the panel hides it;
Stop cancels only the side agent. Local execution still requires the owning Orb
process/computer. Clearing local application storage loses the panel association;
cross-device history discovery is not implemented yet.

Verification: frontend unit tests cover separate child creation, continuation,
local placement, and cancellation; WebKit tests cover panel persistence and
attachments. The opt-in `btw_same_workspace_roundtrip` native test launches real
OpenCode through Core's client-run protocol, reads a fixture using tools, and
checks the working directory and captured session identity.
