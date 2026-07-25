#!/usr/bin/env python3
"""Versioned, conservative ChatGPT UI driver for sandboxed.sh.

Protocol: one JSON request on stdin, NDJSON events on stdout. This helper
references a profile by path but never reads, enumerates, or exports its files.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import mimetypes
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

COMPAT_VERSION = "chatgpt-ui-v2"
CHATGPT_URL = "https://chatgpt.com/"
MAX_DOWNLOAD_FILES = 8
MAX_DOWNLOAD_BYTES = 50 * 1024 * 1024
MODEL_PICKER_READY_TIMEOUT_MS = 15_000
INTELLIGENCE_LABELS = ("Instant", "5.5", "Medium", "High", "Extra High", "Pro")
PRO_MODEL_ALIASES = {
    "gpt-5.6-pro",
    "gpt 5.6 pro",
    "gpt-5.6 pro",
    "gpt 5.6-pro",
}


def emit(event_type: str, **payload) -> None:
    print(json.dumps({"type": event_type, **payload}, separators=(",", ":")), flush=True)


def fail(code: str, message: str) -> None:
    emit("error", code=code, message=message)


def model_selection(requested: str) -> tuple[str, str]:
    """Return the exact visible picker label and canonical model identifier."""
    normalized = " ".join(requested.strip().lower().split())
    if normalized in PRO_MODEL_ALIASES:
        return "Pro", "gpt-5.6-pro"
    return requested.strip(), requested.strip()


async def choose_intelligence_model(page, label: str) -> bool:
    """Select a current composer intelligence option without touching the sidebar."""
    # The current ChatGPT shell hydrates the composer in two phases: the
    # textbox can be ready several seconds before the model pill is attached.
    # Wait for the composer-scoped control instead of snapshotting its count
    # immediately and incorrectly falling back to the legacy model picker.
    picker_buttons = page.locator(
        'form button.__composer-pill[aria-haspopup="menu"]:visible'
    )
    try:
        await picker_buttons.first.wait_for(
            state="visible", timeout=MODEL_PICKER_READY_TIMEOUT_MS
        )
    except Exception:
        emit("diagnostic", message="stage=composer_model_picker_not_ready")
        return False

    for index in range(await picker_buttons.count()):
        button = picker_buttons.nth(index)
        try:
            if not await button.is_visible():
                continue
            current = (await button.inner_text()).strip()
            if current not in INTELLIGENCE_LABELS:
                continue
            if current == label:
                emit("diagnostic", message="stage=model_already_selected")
                return True
            await button.click()
            overlay = page.locator(
                '[data-testid="composer-intelligence-picker-content"]:visible'
            ).last
            await overlay.wait_for(state="visible", timeout=3_000)
            option = overlay.get_by_role("menuitemradio", name=label, exact=True)
            await option.wait_for(state="visible", timeout=3_000)
            await option.click()
            selected = button.get_by_text(label, exact=True)
            await selected.wait_for(state="visible", timeout=3_000)
            return True
        except Exception:
            continue
    emit("diagnostic", message="stage=composer_model_option_unavailable")
    return False


async def choose_model(page, requested: str) -> str:
    """Select a verified current option or exact legacy model label."""
    if not requested:
        return ""
    visible_label, canonical_model = model_selection(requested)
    if visible_label in INTELLIGENCE_LABELS and await choose_intelligence_model(
        page, visible_label
    ):
        return canonical_model

    # Legacy ChatGPT rollouts expose a dedicated model picker. Keep this exact
    # lookup as a compatibility path, but never scan arbitrary buttons whose
    # aria-label may be a private conversation title.
    candidates = [
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
                option = overlay.get_by_text(visible_label, exact=True).last
                await option.wait_for(state="visible", timeout=3000)
                await option.click()
                return canonical_model
        except Exception:
            continue
    raise RuntimeError("requested model is not visibly available in the model picker")


def safe_download_name(value: str, index: int) -> str:
    basename = Path(value).name.strip()
    if not basename:
        basename = f"chatgpt-artifact-{index}"
    safe = re.sub(r"[^A-Za-z0-9._ -]+", "_", basename).strip(" .")
    return (safe or f"chatgpt-artifact-{index}")[:180]


def downloadable_href(value: str | None) -> bool:
    if not value:
        return False
    if value.startswith(("sandbox:", "blob:")):
        return True
    parsed = urlparse(value)
    host = parsed.netloc.lower()
    if not host and value.startswith("/"):
        return "/files/" in parsed.path or "/download" in parsed.path
    return (
        host in {"chatgpt.com", "chat.openai.com"}
        and ("/files/" in parsed.path or "/download" in parsed.path)
    ) or host.endswith(".oaiusercontent.com")


def download_control_key(
    tag_name: str,
    href: str | None,
    aria_label: str | None,
    class_name: str | None,
    text: str | None,
) -> str | None:
    """Identify a narrowly scoped ChatGPT artifact control."""
    if tag_name.lower() == "a" and downloadable_href(href):
        return f"href:{href}"
    if tag_name.lower() != "button" or "behavior-btn" not in (class_name or "").split():
        return None
    label = (aria_label or text or "").strip()
    # Current ChatGPT artifact entities are buttons labelled with the generated
    # filename. Requiring a filename suffix avoids clicking generic entity,
    # citation, or action buttons that happen to share the behavior class.
    if not label or not Path(label).suffix or "/" in label or "\\" in label:
        return None
    return f"button:{label}"


async def collect_downloads(page, response, download_dir: Path) -> None:
    """Download bounded assistant-generated artifacts and emit typed receipts."""
    controls = response.locator('a[href], button.behavior-btn[aria-label]')
    seen_controls: set[str] = set()
    used_names: set[str] = set()
    total_bytes = 0
    emitted = 0
    for index in range(await controls.count()):
        if emitted >= MAX_DOWNLOAD_FILES:
            break
        control = controls.nth(index)
        tag_name = await control.evaluate("(element) => element.tagName")
        href = await control.get_attribute("href")
        aria_label = await control.get_attribute("aria-label")
        class_name = await control.get_attribute("class")
        text = await control.inner_text()
        control_key = download_control_key(
            tag_name, href, aria_label, class_name, text
        )
        if control_key is None or control_key in seen_controls:
            continue
        seen_controls.add(control_key)
        preview_open = False
        try:
            if tag_name.lower() == "button":
                # Current ChatGPT opens an artifact preview first. The preview
                # owns the actual browser download action.
                await control.click()
                preview_open = True
                download_button = page.get_by_role(
                    "button", name="Download", exact=True
                ).last
                await download_button.wait_for(state="visible", timeout=5_000)
                async with page.expect_download(timeout=15_000) as pending:
                    await download_button.click(no_wait_after=True)
            else:
                async with page.expect_download(timeout=15_000) as pending:
                    await control.click(no_wait_after=True)
            download = await pending.value
            name = safe_download_name(download.suggested_filename, emitted + 1)
            stem = Path(name).stem
            suffix = Path(name).suffix
            candidate = name
            duplicate = 2
            while candidate in used_names or (download_dir / candidate).exists():
                candidate = safe_download_name(f"{stem}-{duplicate}{suffix}", emitted + 1)
                duplicate += 1
            destination = download_dir / candidate
            await download.save_as(destination)
            size = destination.stat().st_size
            if size > MAX_DOWNLOAD_BYTES or total_bytes + size > MAX_DOWNLOAD_BYTES:
                destination.unlink(missing_ok=True)
                emit("diagnostic", message="stage=artifact_size_limit")
                continue
            total_bytes += size
            emitted += 1
            used_names.add(candidate)
            content_type = mimetypes.guess_type(candidate)[0] or "application/octet-stream"
            emit(
                "artifact",
                path=str(destination),
                name=candidate,
                content_type=content_type,
                size_bytes=size,
            )
        except Exception:
            emit("diagnostic", message="stage=artifact_download_skipped")
        finally:
            if preview_open:
                try:
                    await page.keyboard.press("Escape")
                except Exception:
                    pass


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
    emit("diagnostic", message="stage=page_loaded")
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
    emit("diagnostic", message="stage=account_confirmed")

    # A direct navigation to the root is the stable new-chat primitive. The
    # sidebar's "New chat" control is rollout-dependent and can be a button,
    # link, or client-side action that does not emit a navigation event.
    await assert_blank_chat(page)
    emit("diagnostic", message="stage=blank_route")

    composer = page.locator("#prompt-textarea").first
    if not await composer.count():
        composer = page.get_by_role("textbox").last
    await composer.wait_for(state="visible", timeout=20_000)
    emit("diagnostic", message="stage=composer_ready")
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
    requested_download_dir = request.get("download_dir")
    download_dir = None
    if requested_download_dir is not None:
        candidate = Path(str(requested_download_dir))
        if not candidate.is_absolute():
            fail("invalid_request", "download_dir must be an absolute path")
            return
        candidate.mkdir(parents=True, exist_ok=True)
        download_dir = candidate.resolve()
    emit("diagnostic", message=f"compatibility={COMPAT_VERSION}; browser={args.browser}")

    context = None
    stage = "launch"
    try:
        async with async_playwright() as playwright:
            browser_type = getattr(playwright, args.browser, None)
            if browser_type is None:
                fail("invalid_config", f"unsupported browser: {args.browser}")
                return
            try:
                launch_options = {
                    "user_data_dir": str(Path(args.profile_dir).resolve()),
                    "headless": args.headless == "true",
                    "viewport": {"width": 1440, "height": 1000},
                    "args": ["--disable-background-networking"],
                    "accept_downloads": True,
                }
                if download_dir is not None:
                    launch_options["downloads_path"] = str(download_dir)
                if args.proxy_server:
                    launch_options["proxy"] = {"server": args.proxy_server}
                context = await browser_type.launch_persistent_context(
                    **launch_options,
                )
            except Exception:
                fail(
                    "browser_launch",
                    "Browser failed to launch. Verify the browser install, profile permissions, and that no other browser is using the profile.",
                )
                return

            page = context.pages[0] if context.pages else await context.new_page()
            stage = "fresh_chat"
            baseline = await establish_fresh_chat(page)
            stage = "model_selection"
            model_used = await choose_model(page, requested_model)
            stage = "blank_chat_check"
            await assert_blank_chat(page)
            stage = "composer"
            composer = page.locator("#prompt-textarea").first
            if not await composer.count():
                composer = page.get_by_role("textbox").last
            await composer.fill(message)
            stage = "send"
            send = page.get_by_role("button", name=re.compile(r"send", re.I)).last
            await send.wait_for(state="visible", timeout=10_000)
            await send.click()

            stage = "response"
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
                    if download_dir is not None:
                        stage = "artifacts"
                        await collect_downloads(
                            page, responses.nth(count - 1), download_dir
                        )
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
            f"{COMPAT_VERSION}: UI check failed at {stage} ({type(exc).__name__}); verify selectors against a blank, non-private chat",
        )
    finally:
        if context is not None:
            await close_context_quietly(context)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile-dir", required=True)
    parser.add_argument("--browser", choices=("chromium", "firefox", "webkit"), default="chromium")
    parser.add_argument("--headless", choices=("true", "false"), default="true")
    parser.add_argument("--proxy-server")
    args = parser.parse_args()
    try:
        request = json.loads(sys.stdin.readline())
    except (json.JSONDecodeError, EOFError):
        fail("invalid_request", "expected one JSON request on stdin")
        return
    asyncio.run(run(args, request))


if __name__ == "__main__":
    main()
