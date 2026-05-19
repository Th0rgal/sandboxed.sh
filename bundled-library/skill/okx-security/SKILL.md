---
name: okx-security
description: "Read-only OKX OnchainOS security checks for autonomous agents: token risk, DApp phishing, transaction pre-execution, signature safety, and approval exposure reports. Never signs, broadcasts, swaps, revokes, or stores wallet keys."
license: MIT
metadata:
  author: okx
  source: "okx/onchainos-skills"
  upstream_skill: okx-security
  sandboxed_profile: read-only
  homepage: "https://web3.okx.com/onchainos"
---

# OKX Security

Use this skill when the user asks whether a token, DApp URL, transaction, signature request, or wallet approval surface is safe.

This sandboxed.sh edition is read-only by design. It may inspect public chain data and OKX OnchainOS security verdicts, but it must never request private keys, sign messages, broadcast transactions, execute swaps, revoke approvals, or modify wallet state. If the upstream OKX docs mention a write action, treat it as out of scope and report the read-only finding instead.

## Allowed Commands

Run only OKX OnchainOS security inspection commands:

```bash
onchainos security token-scan --address <token> --chain <chain>
onchainos security dapp-scan --url <url>
onchainos security tx-scan --chain <chain> --from <address> --to <address> --data <calldata> --value <value>
onchainos security sig-scan --chain <chain> --address <address> --message <message>
onchainos security approvals --address <address> --chain <chain>
```

Do not run commands whose primary purpose is trading, sending, signing, broadcasting, revoking, wallet creation, or key management.

## Runtime Setup

Before the first `onchainos` command in a workspace:

1. Check whether `onchainos --version` works.
2. If it is missing, stop and ask the human to install the official OKX OnchainOS CLI from a pinned release or package manager they trust. Do not download and execute remote install scripts.
3. Run `onchainos --version` again only after the human confirms installation, and stop if the binary is still unavailable.

Do not ask for OKX account credentials or Agentic Wallet keys. Security commands can produce a useful report from public inputs.

## Reporting Format

Return a concise risk report:

- `Verdict`: `block`, `warn`, or `no-risk-detected`.
- `Scope`: token, DApp URL, transaction, signature, or approvals.
- `Signals`: the concrete OKX risk labels, phishing flags, simulation warnings, or approval exposures.
- `Sandbox boundary`: state that no keys, signing, broadcasts, swaps, or approval changes were used.
- `Next action`: what the human should inspect or avoid.

If a scan fails because of network, rate-limit, unsupported chain, or malformed input, never treat the missing result as safe. Say that the scan did not complete and list the exact command category that failed.
