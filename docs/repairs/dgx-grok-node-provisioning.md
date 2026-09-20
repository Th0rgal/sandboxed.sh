# DGX Spark: Grok Build as a remote-launch harness (proposal, not applied)

Status 2026-09-20: typed remote launches (`POST /api/control/missions` with
`backend` + `model_override` + `remote_node_id`) are planned server-side by
`plan_remote_harness`. Nodes are known to run only `claude` and `opencode`
(`REMOTE_NODE_HARNESSES`). Selecting `grok` for a node is refused with a
`400` naming the supported harnesses **before** any mission exists; there is
no fallback and no claim that Grok started.

Read-only verification on dgx-spark (`sandboxed-node.service`,
`User=sandboxed-node`, `HOME=/var/lib/sandboxed-node`):

- `sudo -n -u sandboxed-node command -v grok` → absent
- `test -f /var/lib/sandboxed-node/.grok/auth.json` → absent

Any `grok` present for another user/home does not count: raw jobs run as the
service user with `env_clear()` and `HOME=<mission dir>` (`remote_node::raw_command`).

## What the original user flow needs (Grok Build, `grok-4.6`, on the DGX)

Today the honest path is **OpenCode with `xai/grok-4.6`**: the node's
`opencode run --model openai/grok-4.6` talks to this core's proxy, which
routes `xai/…` to the configured xAI account. That keeps credentials on the
core and requires nothing new on the node.

If native Grok Build on the node is wanted, this is the setup to approve and
roll out with a server release (do not install or restart live now):

1. **Install the CLI for the service user** (official installer, pinned dir):
   ```bash
   sudo -u sandboxed-node -H bash -lc \
     'curl -fsSL https://x.ai/cli/install.sh | GROK_BIN_DIR=/var/lib/sandboxed-node/.local/bin bash'
   ```
   and add that dir to the node service `PATH` (drop-in
   `Environment=PATH=/var/lib/sandboxed-node/.local/bin:/usr/local/bin:/usr/bin:/bin`),
   because raw jobs inherit only `PATH`.
2. **Credentials.** The native ACP path needs a cached login
   (`~/.grok/auth.json`, method `cached_token`); `XAI_API_KEY` is the legacy
   `-p --output-format streaming-json` path only (`src/api/runners/grok.rs`).
   Raw jobs set `HOME` to the per-mission directory, so a login cached under
   the service home is invisible to the job. Two options, both server-side:
   - ship the token in the job **env** (`XAI_API_KEY`) minted/held by core,
     exactly like the proxy key for claude/opencode — legacy CLI path only; or
   - extend the node runner to bind a per-user credential dir into the job
     `HOME` for an allow-listed harness (node change + review).
   Either way the credential must stay out of the command line, mission
   events, ledger handles and `remote_job` read models (the node's own
   `jobs.payload_json` stores the submitted env at rest; keys are retired
   when the observer finishes).
3. **Backend change** (after 1–2 are live): add `"grok"` to
   `REMOTE_NODE_HARNESSES` and a `RemoteHarnessPlan::GrokBuild` that emits
   `grok -p <prompt> --output-format streaming-json --model <model>` with the
   env from step 2, plus a node capability label (`grok`) checked in
   `plan_remote_harness` so nodes without the CLI stay refused.

Until then the Orb should fail `grok` + remote node in preflight with the
backend's message, and offer OpenCode/`xai/grok-4.6` as the supported route.
