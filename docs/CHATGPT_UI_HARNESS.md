# ChatGPT UI harness (experimental)

`chatgpt_ui` runs a mission turn through the operator's existing ChatGPT web
session. It consumes the subscription allowance shown in that account's web UI,
not Codex CLI/API allowance. This is browser automation, not an OpenAI API.
It does not reuse OpenAI/Codex OAuth credentials from backend settings: login
state lives only in the dedicated Playwright browser profile configured below.

Operational pool policy (capacity, read-only Pro lanes, retry and writer
rules) is versioned in [`policy/CHATGPT_UI_POOL_POLICY.md`](policy/CHATGPT_UI_POOL_POLICY.md)
and machine-checked by `scripts/policy_lint.py`.

## Status and limitations

- The integration is experimental and opt-in. ChatGPT markup, rollout flags,
  model labels, account entitlements, rate limits, and anti-automation controls
  can change without notice.
- It supports text turns and translates driver text, diagnostic, tool-call,
  tool-result, artifact, completion, and error events. The current ChatGPT web
  UI does not expose sandboxed.sh's local tools. A response with an unresolved
  tool call is failed rather than falsely reported complete.
- Downloadable files in the latest assistant response are copied into the
  mission workspace and exposed as ordinary sandboxed.sh shared files. A turn
  accepts at most 8 files and 50 MiB total; paths are canonicalized and must
  remain inside that mission workspace.
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
install -D -m 0755 scripts/chatgpt_ui_driver.py \
  /opt/sandboxed-sh/scripts/chatgpt_ui_driver.py
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
    "proxy_server": "socks5://127.0.0.1:10880",
    "headless": false,
    "display": ":93",
    "timeout_secs": 14400,
    "launch_interval_secs": 30,
    "model": "gpt-5.6-pro"
  }
}
```

`profile_dir` must exist, be absolute, and be outside the sandboxed.sh working
directory. `driver_path` must be the absolute installed path of the included
driver script.
`proxy_server` is optional and applies only to the browser process. Supported
schemes are `http`, `https`, `socks5`, and `socks5h`; URLs containing embedded
credentials are rejected.
When anti-bot checks reject headless Chromium, set `headless` to `false` and
configure `display` with a dedicated Xvfb display such as `:93`.
Timeouts must be between 30–86400 seconds; values outside that accepted range
are rejected before launch. A cross-process profile lock rejects concurrent use;
configure a distinct dedicated profile for each concurrent mission.
`launch_interval_secs` (default 30, accepted range 5–300) spaces new browser
navigation/submission starts across the shared profile pool. It does not limit
already-running Pro conversations. If ChatGPT renders its exact “Too many
requests” interstitial, the runtime opens a ten-minute account-wide circuit;
new turns wait instead of probing additional profiles.

## Model selection

Use the canonical `gpt-5.6-pro` model ID for ChatGPT Pro. The driver maps it to
the current visible `Pro` intelligence option, clicks that exact radio item, and
verifies that the composer picker changed to `Pro`. Other values remain exact
visible-label lookups for compatibility. Selection failure is terminal; the
driver never silently substitutes another model. A selected label proves only
what the account UI exposed and selected, not independent backend model
identity or deep-research capabilities.

For a one-turn operator smoke test:

```bash
CHATGPT_UI_PYTHON=/opt/sandboxed-sh/chatgpt-ui-venv/bin/python \
CHATGPT_UI_DRIVER=/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py \
CHATGPT_UI_PROXY=socks5://127.0.0.1:10880 \
CHATGPT_UI_HEADLESS=false DISPLAY=:93 \
  scripts/chatgpt_ui_smoke.sh \
  /var/lib/sandboxed-sh/chatgpt-profile gpt-5.6-pro
```

Run without a model argument to test the account's current UI default.
`CHATGPT_UI_PYTHON` must point at the interpreter where Playwright was
installed; the smoke script otherwise uses `python3`. `CHATGPT_UI_DRIVER` is
optional when running the script from the repository, where the adjacent
driver is selected automatically. `CHATGPT_UI_PROXY` is optional and should
match `proxy_server` when the account requires a stable external egress.
`CHATGPT_UI_HEADLESS=false` requires a working `DISPLAY`.

## Diagnostics

- `auth_required`: provision or refresh login interactively.
- Auth status in the settings API stays unknown until a driver turn loads a
  blank page; directory presence is never treated as proof of authentication.
- `requested model is not visibly available`: verify the exact current label
  and account entitlement; UI rollouts are account-specific.
- `stage=composer_model_picker_not_ready`: the authenticated composer appeared,
  but its lazily hydrated model control did not become visible within 15
  seconds. Retry once after checking the browser/proxy path; do not silently
  use the current default model.
- `compatibility=chatgpt-ui-v2`: the versioned selector/download contract
  failed. Capture
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

When ChatGPT presents an attached file, the driver follows only scoped artifact
links or the filename-labelled artifact preview control in the latest assistant
message. It then activates the preview's exact `Download` action. Arbitrary
external links and generic page buttons are never followed. The Rust runner
revalidates every receipt, converts it to a `<file>` tag, and the assistant MCP
exposes it through `list_mission_shared_files` and `download_shared_file`.

## Inspiration and provenance

The architecture was informed by CatGPT-Gateway at commit
`79a1b69d429fa9951d796289b60470fb595cbb42` (MIT, copyright GautamVhavle).
No source code was copied or vendored. This adapter is an original,
smaller process protocol integrated with sandboxed.sh's lifecycle rather than
an OpenAI-compatible proxy.
