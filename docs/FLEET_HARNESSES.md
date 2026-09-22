# Managed native harnesses

Core, Ashur, Babylon, Nippur, old-agent and DGX Spark use native executables
under `/opt/sandboxed-tools/<harness>/<version>/`. Public commands in
`/usr/local/bin` switch atomically after version/help probes as `sandboxed-node`.
No running harness is stopped, and no login credentials are copied.

Validated stable versions on 22 September 2026: OpenCode 1.18.32, Claude Code
2.1.278, Codex 0.155.1, Grok 1.0.40. Gemini CLI was removed from Core and
old-agent and disabled in production backend configuration; historical missions
and Google provider credentials are retained.

Install the two Python scripts and systemd units:

```sh
install -m 755 scripts/update-node-codex.py /usr/local/sbin/update-node-codex
install -m 755 scripts/update-node-harnesses.py /usr/local/sbin/update-node-harnesses
install -m 644 deploy/sandboxed-harness-update.service deploy/sandboxed-harness-update.timer /etc/systemd/system/
python3 /usr/local/sbin/update-node-harnesses
systemctl daemon-reload
systemctl disable --now sandboxed-codex-update.timer
systemctl enable --now sandboxed-harness-update.timer
```

The daily timer has a randomized two-hour offset. A per-host lock prevents
concurrent fleet updater runs. Downloads use official registries/releases:
Claude/Codex npm SHA-512 integrity and OpenCode GitHub release SHA-256 digests
are checked. Grok uses its official stable HTTPS artifact origin; upstream's
installer does not provide a separate signed checksum manifest.

Checks validate executable contracts, not account eligibility or successful
inference. Grok's managed login must be provisioned separately. See
`HARNESS_PERFORMANCE_AUDIT.md` for the observed Core authentication failure.

The reconciler retains two recent versions, the explicit rollback target and
any version mapped by a live process. If process inspection is incomplete it
skips cleanup. The pre-managed executable is also retained. Each host writes
`/opt/sandboxed-tools/harness-status.json`; inspect this and the service journal
for failures. A failed candidate probe leaves the existing executable selected.

To pin a tested fleet release, put a JSON object in a root-owned file, e.g.
`{"opencode":"1.18.32","claude":"2.1.278","codex":"0.155.1","grok":"1.0.40"}`,
and set `HARNESS_VERSIONS_FILE=/etc/sandboxed-harness-versions.json` in
`/etc/sandboxed-harness-update.env`. Without pins each run resolves stable latest.
Sandboxed's existing `harness_versions` settings separately control legacy
container bootstrap; host updates do not rewrite those settings every day.

For OpenCode/Claude/Grok rollback, validate `<harness>/previous --version`, then
atomically replace the `/usr/local/bin` symlink with that target. Pin the desired
version before the next reconciliation. Codex retains its own recent native
versions through the existing `update-node-codex` reconciler.

Official installation references: [Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code setup](https://code.claude.com/docs/en/setup),
[Grok Build](https://docs.x.ai/build/overview),
[OpenCode releases](https://github.com/anomalyco/opencode/releases).
