#!/usr/bin/env python3
"""Prove ChatGPT UI credential login without writing the live pool.

Copies an idle warm profile (Cloudflare cookies already present), signs out,
then runs the real step registry. An empty profile through the SOCKS exit
usually dies on Turnstile and does not test email/password/OTP.
"""
from __future__ import annotations

import asyncio
import re
import shutil
import sys
from pathlib import Path

SCRIPTS = Path("/opt/sandboxed-sh/scripts")
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))
LOCAL = Path(__file__).resolve().parent
if str(LOCAL) not in sys.path:
    sys.path.insert(0, str(LOCAL))

from chatgpt_ui_driver import CHATGPT_URL, TransportUnavailable, wait_out_cloudflare
from chatgpt_ui_login_steps import (
    LOGIN_STEPS,
    AnotherAccountStep,
    ReloginError,
    click_named,
    describe_snapshot,
    observe,
    run_login,
)
from chatgpt_ui_relogin import (
    copy_profile,
    fetch_bws_secrets,
    pool_slots,
    slot_in_use,
    totp_code,
)

SCRATCH = Path("/tmp/chatgpt-relogin-verify-profile")
LOGOUT_NAME = re.compile(
    r"^(log out|sign out|se d[eé]connecter|d[eé]connexion)$", re.I
)
SIGNOUT_URLS = (
    "https://chatgpt.com/api/auth/signout",
    "https://chatgpt.com/auth/logout",
    "https://chatgpt.com/logout",
)


def log(message: str) -> None:
    print(f"chatgpt-ui-relogin: {message}", flush=True)


def idle_source() -> str:
    slots, _proxy = pool_slots()
    for path in slots:
        if not slot_in_use(path):
            return path
    raise ReloginError("no_idle_slot", "every ChatGPT UI profile is in use by a mission")


def signed_out_snapshot(snap) -> bool:
    return bool(
        snap.has_login_button
        or snap.has_email
        or snap.has_password
        or snap.has_otp
        or snap.has_account_picker
    )


async def signed_out(page) -> bool:
    return signed_out_snapshot(await observe(page))


async def sign_out(page) -> None:
    log("stage=open")
    await page.goto(CHATGPT_URL, wait_until="domcontentloaded", timeout=90_000)
    try:
        await wait_out_cloudflare(page, timeout_ms=90_000)
    except TransportUnavailable as error:
        raise ReloginError("challenge", "Cloudflare interstitial did not clear") from error
    snap = None
    for attempt in range(40):
        snap = await observe(page)
        if attempt == 0:
            log("stage=open " + describe_snapshot(snap))
        if snap.has_account_shell:
            break
        if signed_out_snapshot(snap):
            log("stage=already_signed_out " + describe_snapshot(snap))
            return
        await page.wait_for_timeout(500)
    else:
        raise ReloginError(
            "unrecognized_ui",
            "cloned profile never showed account chrome or a login form "
            + describe_snapshot(snap or await observe(page)),
        )
    log("stage=signout")
    for url in SIGNOUT_URLS:
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=45_000)
        except Exception:
            continue
        await page.wait_for_timeout(2_000)
        try:
            await wait_out_cloudflare(page, timeout_ms=30_000)
        except TransportUnavailable:
            continue
        if await signed_out(page):
            log("stage=signed_out")
            return
    await page.goto(CHATGPT_URL, wait_until="domcontentloaded", timeout=90_000)
    try:
        profile = page.locator(
            '[data-testid="accounts-profile-button"], '
            'button[aria-label*="account" i], button[aria-label*="profile" i]'
        )
        if await profile.count():
            await profile.first.click(timeout=8_000)
            await page.wait_for_timeout(800)
    except Exception:
        pass
    if await click_named(page, LOGOUT_NAME):
        await page.wait_for_timeout(2_000)
        if await signed_out(page):
            log("stage=signed_out")
            return
    raise ReloginError("signout_failed", "could not sign out the cloned profile")


async def probe() -> str:
    creds = fetch_bws_secrets()
    source = idle_source()
    _slots, proxy = pool_slots()
    log(f"loaded Bitwarden secrets; cloning idle {Path(source).name} (no pool write)")
    copy_profile(Path(source), SCRATCH)
    from playwright.async_api import async_playwright

    async with async_playwright() as playwright:
        options = {
            "user_data_dir": str(SCRATCH),
            "headless": False,
            "viewport": {"width": 1440, "height": 1000},
            "args": ["--disable-background-networking"],
        }
        if proxy:
            options["proxy"] = {"server": proxy}
        context = await playwright.chromium.launch_persistent_context(**options)
        try:
            page = context.pages[0] if context.pages else await context.new_page()
            await sign_out(page)
            # Skip the saved-account card so this probe must use Bitwarden
            # email/password/OTP rather than restoring a leftover cookie.
            steps = (AnotherAccountStep(),) + tuple(
                step for step in LOGIN_STEPS if step.name != "account_picker"
            )
            result = await run_login(
                page, creds, totp_fn=totp_code, timeout_s=120, steps=steps
            )
            return result
        finally:
            try:
                await context.close()
            except Exception:
                pass


def main() -> int:
    try:
        result = asyncio.run(probe())
        log(f"RESULT={result}")
        if result == "logged_in":
            return 0
        log("cloned profile did not complete a credential login after sign-out")
        return 1
    except ReloginError as error:
        log(f"{error.code}: {error}")
        return 1
    except Exception as error:  # noqa: BLE001
        log(f"{type(error).__name__}: {error}")
        return 1
    finally:
        shutil.rmtree(SCRATCH, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
