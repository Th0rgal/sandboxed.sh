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

## Production rollout receipt — 22 September 2026

All six physical hosts passed the installed version/help checks above. The
unified daily timer is active everywhere, and the superseded Codex-only timer
is inactive. Gemini is absent from each host's command path.

Inference canaries passed for Claude Code on Core and OpenCode (`kimi/k3`) on
Core, Ashur, Nippur, old-agent and DGX Spark. These are availability probes,
not comparable performance benchmarks. Grok's Core inference remains blocked
on its managed login; successful executable checks do not imply authentication.

Babylon initially timed out. Its configured OVH IPv6 DNS resolver did not
answer, while the same provider's IPv4 resolver did. A persistent
`/etc/systemd/resolved.conf.d/70-sandboxed-reliable-dns.conf` now selects
`DNS=213.186.33.99` and `Domains=~.`; restarting only `systemd-resolved` restored
registry and backend hostname resolution in 0.53–0.60 seconds. Subsequent SSH
artifact transfers were still slow, so DNS alone is not evidence that all of
Babylon's network latency is resolved.

Later TCP observations confirmed substantial retransmissions on Babylon's SSH
and node API connections (for example 47,944 retransmitted bytes of 96,807 sent
on one SSH connection, congestion window down to one segment). Small ICMP
samples also lost all three probes to Core and two of three to Nippur. Ethernet
RX/TX error/drop counters were zero, CPU load was near zero, and the inspected
host firewall contained no matching traffic shaper. Direct IPv4, IPv6 and a
temporary SSH jump through Nippur did not yield a usable artifact transfer.
This establishes a network-path problem, not its provider-side cause. Do not
claim Babylon's runtime readiness from its successful harness install checks.

Babylon was cordoned after the final transfer attempt also stalled without SSH
QoS markings. It had no active or queued work. Core and all other nodes were
healthy and uncordoned; Core API and Hermes were active. Uncordon Babylon only
after a sustained connectivity check and the pending upload-route rollout pass.

On the later 22 September retry, a resumable artifact transfer completed. The
node upload binary was installed, an authenticated Core→Babylon upload passed
byte-exact readback, and Babylon was uncordoned. An Orb OpenCode/K3 mission
(`37010f5b-7418-4805-b429-2fa845cbe48f`) then completed in 28.9 seconds. No host
reboot was required. See `HARNESS_PERFORMANCE_AUDIT.md` for the remaining
network-cause uncertainty; the earlier cordon note is historical.
