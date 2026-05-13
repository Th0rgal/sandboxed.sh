sandboxed.sh is the safe runtime for autonomous on-chain AI agents.

This PR adds a read-only OKX security skill integration to the existing sandboxed.sh product. The angle is deliberately safety-first: autonomous agents can run OKX risk checks unattended inside isolated Linux workspaces without exposing wallet secrets to the model, the host, or unrelated missions.

What changed:

- Bundled a read-only `okx-security` Library skill for OKX OnchainOS security checks.
- Added the `autonomous-transaction-safety-check` workspace template for demo missions.
- Seeded those Library items through shared Library initialization so the same skill is injected into Claude Code, OpenCode, and Amp mission environments.
- Added focused tests for bundled Library seeding and cross-harness skill materialization.
- Updated the README hero and added `DEMO.md` with a 1-3 minute recording shot list.

Why this matters for OKX:

- Read-only is the feature: token, DApp, transaction, signature, and approval risk checks without signing or broadcasting.
- Isolation matches the threat model: each autonomous run gets a separate workspace boundary.
- This is a shipped product, not a hackathon toy: sandboxed.sh already has users, docs, a dashboard, iOS app, Git-backed Library, encrypted secrets, and multi-runtime mission orchestration.

Screenshot:

![OKX security risk report running inside sandboxed.sh Mission Control](https://raw.githubusercontent.com/Th0rgal/sandboxed.sh/hackathon/okx-integration/screenshots/okx-security-report.png)

No new dependencies.

Try it in 60 seconds:

```bash
git clone https://github.com/Th0rgal/sandboxed.sh.git
cd sandboxed.sh
cp .env.example .env
docker compose up -d
npx --yes @xagt/agent-plugin@latest setup --target all
# In the dashboard, create a workspace from:
# "autonomous-transaction-safety-check"
```
