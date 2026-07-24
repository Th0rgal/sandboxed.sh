# ChatGPT UI harness (experimental)

`chatgpt_ui` runs a mission turn through the operator's existing ChatGPT web
session. It consumes the subscription allowance shown in that account's web UI,
not Codex CLI/API allowance. This is browser automation, not an OpenAI API.

## Status and limitations

- The integration is experimental and opt-in. ChatGPT markup, rollout flags,
  model labels, account entitlements, rate limits, and anti-automation controls
  can change without notice.
- It supports text turns and translates driver text, diagnostic, tool-call,
  tool-result, completion, and error events. The current ChatGPT web UI does not
  expose sandboxed.sh's local tools. A response with an unresolved tool call is
  failed rather than falsely reported complete.
- Conversation continuation currently sends sandboxed.sh's bounded text
  history in a fresh ChatGPT conversation. It does not persist ChatGPT
  conversation URLs.
- There is no CAPTCHA bypass, fingerprint spoofing, or anti-bot evasion.
- Check the applicable ChatGPT terms and your organization's policy. UI
  automation can lead to challenges, throttling, or account restrictions.

## Install and provision

Install Playwright in a dedicated environment beside the sandboxed.sh service
account:

```bash
python3 -m venv /opt/sandboxed-sh/chatgpt-ui-venv
/opt/sandboxed-sh/chatgpt-ui-venv/bin/pip install playwright
/opt/sandboxed-sh/chatgpt-ui-venv/bin/playwright install chromium
```

Create a dedicated profile directory outside repositories and mission
workspaces, with access restricted to the service account:

```bash
install -d -m 700 /var/lib/sandboxed-sh/chatgpt-profile
```

Provision login interactively using Playwright/Chromium with that exact
`user-data-dir`. Close the browser before starting missions; Chromium locks a
profile while it is open. Complete MFA or CAPTCHA yourself. Never copy profile
contents into a repository, support bundle, screenshot, or mission artifact.

Configure the backend through `PUT /api/backends/chatgpt_ui/config`:

```json
{
  "enabled": true,
  "settings": {
    "profile_dir": "/var/lib/sandboxed-sh/chatgpt-profile",
    "driver_path": "/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py",
    "python_path": "/opt/sandboxed-sh/chatgpt-ui-venv/bin/python",
    "browser": "chromium",
    "headless": true,
    "timeout_secs": 900,
    "model": "GPT-5.6 Pro"
  }
}
```

`profile_dir` must exist, be absolute, and be outside the sandboxed.sh working
directory. `driver_path` must be the absolute installed path of the included
driver script.
Timeouts are clamped to 30–7200 seconds. A cross-process profile lock rejects
concurrent use; configure a distinct dedicated profile for each concurrent
mission.

## Model selection

Set `model` to the exact label visibly offered by the account. The driver opens
the model picker and requires an exact text match; it fails with a compatibility
diagnostic instead of silently substituting another model. A requested label
being present proves only that the UI exposed and selected that label. It does
not independently prove backend model identity or deep-research capabilities.

For a one-turn operator smoke test:

```bash
scripts/chatgpt_ui_smoke.sh /var/lib/sandboxed-sh/chatgpt-profile "GPT-5.6 Pro"
```

Run without a model argument to test the account's current UI default.

## Diagnostics

- `auth_required`: provision or refresh login interactively.
- Auth status in the settings API stays unknown until a driver turn loads a
  blank page; directory presence is never treated as proof of authentication.
- `requested model is not visibly available`: verify the exact current label
  and account entitlement; UI rollouts are account-specific.
- `compatibility=chatgpt-ui-v1`: the versioned selector contract failed. Capture
  only a clean, new chat with no sidebar identity or private history visible.
- Profile lock errors: close every browser using the profile, or configure a
  separate dedicated profile for each concurrent mission.
- `dependency_missing`: install Playwright and its selected browser.
- timeout/rate-limit errors: wait for the account's UI allowance to recover;
  sandboxed.sh cannot read or predict subscription quota.

## Security boundary

The Rust process passes only the profile path, prompt, requested visible model
label, and timeout to the driver. It clears inherited environment variables
except `PATH`; the driver never enumerates or exports cookies, tokens, local
storage, browser databases, profile files, account identity, or prior chats.
The profile is still a powerful bearer credential: isolate it, back it up only
under your own encrypted credential policy, and revoke sessions from ChatGPT if
the host is compromised.

Every turn navigates to a new-chat surface and refuses to send if assistant
content is already present. Streaming events carry accumulated text snapshots,
but success requires an explicit `complete` event, a clean driver exit, and
balanced tool events.

## Inspiration and provenance

The architecture was informed by CatGPT-Gateway at commit
`79a1b69d429fa9951d796289b60470fb595cbb42` (MIT, copyright GautamVhavle).
No source code was copied or vendored. This adapter is an original,
smaller process protocol integrated with sandboxed.sh's lifecycle rather than
an OpenAI-compatible proxy.
