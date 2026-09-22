# Local agents from Orb

Orb can already start Claude Code, Codex, Grok, and OpenCode, but only by
asking the connected sandboxed.sh backend to run them — on Core, or on a
remote node the fleet advertises (DGX Spark and the rest). This plan adds a
real **This computer** target: the same four CLIs, started on the machine
that is running Orb, using the installs and native sessions already on that
machine. Projects, their files, and the mission list stay on the backend.
sandboxed.sh does not have to be installed locally, and Orb does not vendor it.

## What exists today

The machine row in the new-agent composer is not a local runner.

- `MACHINES` in `orb/src/Machines.tsx` includes `{ id: "local", name: "This Mac" }`.
  That row is only shown while Orb is disconnected. On connect, `App.tsx`
  forces the selection back to Core (`if (newMachine() === "local") setNewMachine("core")`).
- A connected launch always `POST`s `/api/control/missions` (`createMission`).
  Core omits `remote_node_id`; any other id is a fleet node, and
  `remoteLaunchPreflight` refuses the launch unless `GET /api/remote-nodes`
  has confirmed that harness. An offline "local" launch only appends a demo
  transcript. It does not spawn a process.
- `@` mentions stay in the prompt as typed. `mentionedChips` turns a known
  project path into `{ kind, path }` and the backend copies the bytes onto
  the mission cwd under `.paloma/attach/` (`materialize` in
  `src/api/mission_payload.rs`). The CLI is then told, in a manifest, to read
  those paths. The `@` token itself is not rewritten.
- Project file bytes are already readable from the client:
  `GET /api/projects/:slug/file?path=`.
- The four harnesses, as the backend runs them today:
  - **Claude Code** (`claudecode`): `claude --print --output-format stream-json`,
    optional `--model` and `--session-id`. Binary override: `CLAUDE_CLI_PATH`.
  - **Codex** (`codex`): `codex app-server` (JSON-RPC). Continuity is a native
    thread id. The backend binds that thread to an isolated Codex home per
    mission (`docs/codex-native-continuity.md`), which is exactly the home the
    user does *not* want for a laptop session.
  - **Grok** (`grok`): the `grok` CLI. Login state lives in the CLI; the
    backend often cannot see it.
  - **OpenCode** (`opencode`): the OpenCode CLI, started per workspace by the
    runner in `src/api/runners/opencode.rs`. Heavier than a one-shot print.

## Decision

A small runner inside the Orb desktop process (Tauri), not a second
sandboxed.sh and not a remote node that pretends the laptop is part of the
fleet.

The laptop CLIs stay the ones the user already logged into (`~/.codex`,
`claude` login, `grok` login, OpenCode). The backend's per-mission home,
proxy keys, and OAuth refresh are not copied onto the machine. Detection
plus a settings override is how Orb learns what is installed.

The one server change is a placement flag so a local run still belongs to a
project and shows up in `listProjectMissions`, while the scheduler never
starts a runner for it.

## Settings

New card: **Settings → Local agents**. Stored on this machine (app config),
not in backend settings.

A Tauri command scans `claude`, `codex`, `grok`, and `opencode` on `PATH`,
runs the version flag each binary actually supports, and returns
`{ id, path, version } | { id, missing: true }`. Each row can override the
path. "Ready" means the binary executed and printed a version. Auth is shown
only when that CLI can report it without Orb reading credential files; Grok
stays "installed, login unknown" when the CLI will not say.

The harness menu, when the machine is This computer, lists only rows marked
installed (detected or overridden). Model ids can stay the ones the backend
already advertises for that harness, with the failure left to the CLI if it
rejects one. Fleet nodes keep today's `remote_launch` preflight and do not
consult this scan.

## Machine menu

While connected, the menu order is:

1. **This computer** — laptop icon, stays selected across reconnects.
2. **Core** — unchanged.
3. Fleet nodes — unchanged, still gated by `remoteLaunchPreflight`.

`create()` branches on the selection. This computer does not call
`getRemoteNodes` and does not send `remote_node_id`. Core and fleet nodes
keep the current body.

## The run still lives in the project

Creating a local agent still `POST`s `/api/control/missions` with the project
slug, title, harness, model, and prompt the user typed, plus:

```json
{ "placement": "client" }
```

The backend records the mission and returns it. It does not admit a runner,
create a remote job, or spawn a harness. Dispatch, resume, and the controller's
mission pickup must all treat `placement: "client"` as not theirs — the check
belongs in admission, not only in the create handler. Status changes
(`running`, terminal, cancelled) come back from the owning client on the
existing mission status path, or a narrow patch if that path assumes a
server-side runner.

The sidebar and project tree keep using `listProjectMissions`. No second
session list.

Follow-up messages in that transcript are not posted to `/api/control/message`
for the server to execute. The client runs them on the same local process or
native session, then appends the user text and the CLI output onto the mission
the transcript already renders. If no existing endpoint accepts an externally
produced event with its own id, add one append call, idempotent on that id.
The process ownership stays on the client. Stop kills the local process and
reports the terminal status; it does not go through remote-job cancellation.

## Native sessions

Local Codex uses the user's Codex home, not a per-mission `CODEX_HOME`.
"Use an existing session" accepts a thread id from that home (typed, or
picked if `codex` can list recent threads without Orb scraping the sqlite
file) and stores it on the mission. The next send is `thread/resume` with
only the new prompt. A thread the CLI reports as already busy is refused;
Orb does not become a second writer next to the Codex TUI.

Claude Code uses `--session-id` the same way. Grok and OpenCode start a new
local session until their resume flag is confirmed against the installed
binary; the UI says so instead of pretending the home session was reused.

The binding (mission id, harness, native session id, cwd) is kept in app
data and mirrored onto the mission record when the API has a session field.
Orb does not import `src/backend/codex`. The start/resume/turn subset is
reimplemented against a fake stdio server in tests.

## `@` files

On send, and only for a client placement:

1. Resolve mentions with the existing `mentionedChips`. An unknown `@word`
   stays prose and is not copied.
2. Read each file, and each folder up to the same depth and byte caps as
   `materialize`, through the project file API. Skip secret paths with the
   same name rules as `is_secret_path` (`.env`, keys, `.ssh`, credential
   files). Implement that filter in Orb; do not depend on the server crate.
3. Write the bytes under a local workspace for that project. The default is
   an app-data directory (`local-workspaces/<project-slug>/`), not a git
   checkout. A project setting can point this at a folder the user picked.
   Layout matches the server so the two stay comparable:
   `.paloma/attach/<rel>` and `.paloma/controller.md`.
4. Replace the mention in the **outgoing** prompt with the absolute local
   path, quoted when it contains spaces. `@notes/a.md` becomes that path.
   `@controller` becomes the controller snapshot path. The composer still
   shows what the user typed. The transcript shows the typed text and the
   path that was sent.
5. If a mentioned file cannot be read, or a write fails, the send is refused
   and the draft is kept. Nothing is spawned.

The webview does not get broad filesystem access. Tauri performs the writes
from the resolved relative paths under the workspace root, and rejects `..`,
absolute paths, and secret names before creating a file.

## Slices

Each slice is shippable on its own. Later slices do not start until the
earlier refusal rules are in place (a client mission must not be dispatched
by the backend).

1. **Scan and settings.** Tauri probe, Settings card, persistence. Tests on
   the parsed scan result and on override persistence.
2. **Picker.** This computer stays available while connected. Harness menu
   filters to installed CLIs. `create()` does not preflight a fleet node for
   this selection. Remote preflight tests stay as they are.
3. **`placement: "client"`.** Create accepts it, stores it, and skips runner
   admission and remote dispatch. A test proves a client mission is not
   picked up by the scheduler.
4. **Copy and rewrite.** Pure function: chips + downloaded bytes + root →
   files to write + rewritten prompt. Secret skip, folder cap, unknown
   mention left intact. Tauri write command.
5. **Claude Code.** Spawn the print/stream-json invocation with cwd set to
   the local workspace, map events onto the transcript, resume with
   `--session-id`, kill on stop.
6. **Codex.** app-server against the user's Codex home, bind the thread to
   the mission, resume, refuse a busy thread. This is the slice that makes
   existing Codex sessions usable from a project.
7. **Grok and OpenCode.** Same runner interface, after the non-interactive
   and resume flags of the installed binaries are pinned. OpenCode is started
   only when it is not already running; Orb does not install it.

## Out of scope

- Vendoring sandboxed.sh, or speaking the remote-node protocol from the laptop.
- Injecting the backend's proxy URL, OAuth tokens, or API keys into the local
  CLIs. The credential is whatever that CLI already has.
- Gemini. It is already gone from the harness picker.
- Writing local edits back into the project file store. That is a later
  explicit action, not a side effect of a mention.
- Waking the project controller when a local mission finishes. The mission is
  visible in the project; the callback wake is a separate change.
- Running two Orb windows as two writers of one native thread.

## Risks

- Codex app-server is the large piece. Keep the client to start, resume, one
  turn, and cancel. Do not port continuity fences, goal budgets, or the
  isolated-home binding.
- A `placement` value the scheduler ignores will start a second copy of the
  agent on Core. Slice 3's admission test is the gate for slices 5–7.
- Path rewrite must not fire on prose. `mentionedChips` is the only resolver.
- The default workspace is app data on purpose. Pointing it at a repository
  is a user choice, so a mention cannot write into a checkout by surprise.
