#!/usr/bin/env python3
"""Versioned, conservative ChatGPT UI driver for sandboxed.sh.

Protocol: one JSON request on stdin, NDJSON events on stdout. This helper
references a profile by path but never reads, enumerates, or exports its files.
"""

import argparse
import asyncio
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

COMPAT_VERSION = "chatgpt-ui-v1"
CHATGPT_URL = "https://chatgpt.com/"


def emit(event_type: str, **payload) -> None:
    print(json.dumps({"type": event_type, **payload}, separators=(",", ":")), flush=True)


def fail(code: str, message: str) -> None:
    emit("error", code=code, message=message)


async def choose_model(page, requested: str) -> str:
    """Select an exact visible model label; never infer aliases."""
    if not requested:
        return ""
    candidates = [
        page.get_by_role("button", name=re.compile(r"(model|chatgpt)", re.I)).first,
        page.locator('[data-testid="model-switcher-dropdown-button"]').first,
    ]
    for candidate in candidates:
        try:
            if await candidate.is_visible(timeout=1500):
                await candidate.click()
                break
        except Exception:
            continue
    else:
        raise RuntimeError("model picker not found")
    # Keep the lookup inside the picker overlay. A global text lookup could
    # click a same-named private conversation in the sidebar.
    overlays = [
        page.get_by_role("menu").last,
        page.get_by_role("dialog").last,
        page.locator('[data-radix-popper-content-wrapper]:visible').last,
    ]
    for overlay in overlays:
        try:
            if await overlay.is_visible(timeout=1000):
                option = overlay.get_by_text(requested, exact=True).last
                await option.wait_for(state="visible", timeout=3000)
                await option.click()
                return requested
        except Exception:
            continue
    raise RuntimeError("requested model is not visibly available in the model picker")


async def assert_blank_chat(page) -> None:
    """Reject any route or rendered content that could belong to a prior chat."""
    parsed = urlparse(page.url)
    if parsed.netloc != "chatgpt.com" or parsed.path not in ("", "/"):
        raise RuntimeError("new-chat navigation did not reach a blank route")
    if await page.locator('[data-message-author-role="assistant"]').count():
        raise RuntimeError("fresh-chat baseline contains prior content")


async def close_context_quietly(context) -> None:
    try:
        await context.close()
    except Exception:
        pass


async def establish_fresh_chat(page) -> int:
    """Prove an authenticated, settled blank chat before observing responses."""
    await page.goto(CHATGPT_URL, wait_until="domcontentloaded", timeout=60_000)
    login = page.get_by_role("button", name=re.compile(r"log in|sign in", re.I)).first
    if "/auth/" in page.url or (await login.count() and await login.is_visible()):
        raise PermissionError("login required")

    # Anonymous ChatGPT can expose a working composer, so its presence is not
    # authentication evidence. Require an account-only control and fail closed
    # when a UI rollout makes that evidence unavailable.
    account_controls = page.locator(
        '[data-testid="accounts-profile-button"], '
        'button[aria-label*="account" i], '
        'button[aria-label*="profile" i]'
    )
    authenticated = False
    for index in range(await account_controls.count()):
        if await account_controls.nth(index).is_visible():
            authenticated = True
            break
    if not authenticated:
        raise PermissionError("authenticated account control not found")

    new_chat = page.get_by_role("link", name=re.compile(r"new chat", re.I)).first
    if await new_chat.count() and await new_chat.is_visible():
        await new_chat.click()
    await page.wait_for_url(re.compile(r"^https://chatgpt\.com/(?:\?.*)?$"), timeout=20_000)

    composer = page.locator("#prompt-textarea").first
    if not await composer.count():
        composer = page.get_by_role("textbox").last
    await composer.wait_for(state="visible", timeout=20_000)
    # Let hydration and lazy conversation rendering settle before taking the
    # baseline. Nothing from this page is emitted before the prompt is sent.
    await page.wait_for_timeout(2_000)
    await assert_blank_chat(page)
    return 0


async def run(args, request) -> None:
    try:
        from playwright.async_api import async_playwright
    except ImportError:
        fail(
            "dependency_missing",
            "Python package playwright is required; install it and its selected browser",
        )
        return

    message = request.get("message")
    if not isinstance(message, str) or not message.strip():
        fail("invalid_request", "message must be a non-empty string")
        return
    timeout_ms = max(30_000, min(int(request.get("timeout_ms", 900_000)), 7_200_000))
    requested_model = request.get("model") or ""
    emit("diagnostic", message=f"compatibility={COMPAT_VERSION}; browser={args.browser}")

    context = None
    try:
        async with async_playwright() as playwright:
            browser_type = getattr(playwright, args.browser, None)
            if browser_type is None:
                fail("invalid_config", f"unsupported browser: {args.browser}")
                return
            try:
                context = await browser_type.launch_persistent_context(
                    user_data_dir=str(Path(args.profile_dir).resolve()),
                    headless=args.headless == "true",
                    viewport={"width": 1440, "height": 1000},
                    args=["--disable-background-networking"],
                )
            except Exception:
                fail(
                    "browser_launch",
                    "Browser failed to launch. Verify the browser install, profile permissions, and that no other browser is using the profile.",
                )
                return

            page = context.pages[0] if context.pages else await context.new_page()
            baseline = await establish_fresh_chat(page)
            model_used = await choose_model(page, requested_model)
            await assert_blank_chat(page)
            composer = page.locator("#prompt-textarea").first
            if not await composer.count():
                composer = page.get_by_role("textbox").last
            await composer.fill(message)
            send = page.get_by_role("button", name=re.compile(r"send", re.I)).last
            await send.wait_for(state="visible", timeout=10_000)
            await send.click()

            last = ""
            stable = 0
            deadline = asyncio.get_running_loop().time() + timeout_ms / 1000
            while asyncio.get_running_loop().time() < deadline:
                responses = page.locator('[data-message-author-role="assistant"]')
                count = await responses.count()
                if count > baseline:
                    text = (await responses.nth(count - 1).inner_text()).strip()
                    if text and text != last:
                        last, stable = text, 0
                        emit("text_delta", content=text)
                    elif text:
                        stable += 1
                stop = page.get_by_role("button", name=re.compile(r"stop", re.I)).last
                stop_visible = await stop.count() and await stop.is_visible()
                if last and count > baseline and not stop_visible and stable >= 2:
                    emit("complete", content=last, model=model_used or None)
                    return
                await asyncio.sleep(0.75)
            fail("timeout", f"ChatGPT response did not complete within {timeout_ms // 1000} seconds")
    except PermissionError:
        fail(
            "auth_required",
            "ChatGPT login is required in the configured profile; provision it interactively",
        )
    except Exception as exc:
        fail(
            "compatibility",
            f"{COMPAT_VERSION}: UI check failed ({type(exc).__name__}); verify selectors against a blank, non-private chat",
        )
    finally:
        if context is not None:
            await close_context_quietly(context)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile-dir", required=True)
    parser.add_argument("--browser", choices=("chromium", "firefox", "webkit"), default="chromium")
    parser.add_argument("--headless", choices=("true", "false"), default="true")
    args = parser.parse_args()
    try:
        request = json.loads(sys.stdin.readline())
    except (json.JSONDecodeError, EOFError):
        fail("invalid_request", "expected one JSON request on stdin")
        return
    asyncio.run(run(args, request))


if __name__ == "__main__":
    main()
