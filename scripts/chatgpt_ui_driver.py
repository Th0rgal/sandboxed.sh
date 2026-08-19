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
# Account/bootstrap hydration can lag substantially behind the composer on the
# current ChatGPT shell.  Keep this bounded, but do not classify a healthy
# account as UI-incompatible merely because the intelligence pill missed the
# old 15-second window.
MODEL_PICKER_READY_TIMEOUT_MS = 45_000
SEND_CONTROL_TESTIDS = (
    '[data-testid="send-button"]',
    '[data-testid="composer-send-button"]',
)
STOP_CONTROL_TESTIDS = (
    '[data-testid="stop-button"]',
    '[data-testid="composer-stop-button"]',
)
# Anchored so unrelated page chrome ("Resend", "Nonstop…") cannot match; the
# lookup is additionally scoped to the composer form, keeping controls such as
# a sidebar "Send feedback" out of reach.
SEND_BUTTON_NAME = re.compile(r"^send\b", re.I)
STOP_BUTTON_NAME = re.compile(r"^stop\b", re.I)
INTELLIGENCE_LABELS = ("Instant", "5.5", "Medium", "High", "Extra High", "Pro")
# Current composer power slider: Instant, Medium, High, Extra High, Pro.
INTELLIGENCE_SLIDER_LABELS = ("Instant", "Medium", "High", "Extra High", "Pro")
PRO_MODEL_ALIASES = {
    "gpt-5.6-pro",
    "gpt 5.6 pro",
    "gpt-5.6 pro",
    "gpt 5.6-pro",
}


class TransportUnavailable(Exception):
    pass


class RateLimited(Exception):
    """ChatGPT rendered its explicit account request-rate interstitial."""


RATE_LIMIT_HEADING = "Too many requests"
RATE_LIMIT_MODAL_TESTID = "modal-conversation-history-rate-limit"
# Opaque conversation route: `/c/<id>`. This is the only page-derived value the
# durability protocol ever emits — never titles, prompts, or response text.
CONVERSATION_PATH_RE = re.compile(r"^/c/[A-Za-z0-9-]{8,64}$")
CHATGPT_HOSTS = {"chatgpt.com", "chat.openai.com"}
CLOUDFLARE_TITLE = re.compile(r"just a moment|verifying", re.I)
CLOUDFLARE_BODY = re.compile(
    r"verify you are human|verifying\.\.\.|just a moment", re.I
)
ACCOUNT_PICKER_HEADING = re.compile(r"choose an account to continue", re.I)
SAVED_ACCOUNT_EMAIL = re.compile(r"[^@\s]+@[^@\s]+\.[^@\s]+")
ACCOUNT_PICKER_SKIP = re.compile(
    r"log in to another account|create account|remove account|sign up for free|^log in$",
    re.I,
)
LOGIN_BUTTON_NAME = re.compile(r"^(log in|sign in)$", re.I)


class ResumeNotFound(Exception):
    """The recorded conversation route is not reachable from this profile."""


class ResumeMismatch(Exception):
    """The conversation exists but does not hold exactly this prompt."""


def emit(event_type: str, **payload) -> None:
    print(json.dumps({"type": event_type, **payload}, separators=(",", ":")), flush=True)


def fail(code: str, message: str, *, stage: str | None = None) -> None:
    payload = {"code": code, "message": message}
    if stage:
        payload["stage"] = stage
    emit("error", **payload)


def conversation_path_from_url(url: str) -> str | None:
    """Return the opaque `/c/<id>` route for a ChatGPT conversation URL."""
    try:
        parsed = urlparse(url)
    except ValueError:
        return None
    if parsed.netloc.lower() not in CHATGPT_HOSTS:
        return None
    if CONVERSATION_PATH_RE.fullmatch(parsed.path or ""):
        return parsed.path
    return None


def resume_conversation_path(resume) -> str | None:
    """Validate a resume request and return its conversation route."""
    if not isinstance(resume, dict):
        return None
    path = resume.get("conversation_path")
    if isinstance(path, str) and CONVERSATION_PATH_RE.fullmatch(path):
        return path
    return None


def continued_conversation_path(conversation) -> str | None:
    """Validate a completed-turn conversation affinity request."""
    return resume_conversation_path(conversation)


def normalized_prompt(text: str) -> str:
    """Whitespace-insensitive prompt identity used only in memory."""
    return " ".join(text.split())


def model_selection(requested: str) -> tuple[str, str]:
    """Return the exact visible picker label and canonical model identifier."""
    normalized = " ".join(requested.strip().lower().split())
    if normalized in PRO_MODEL_ALIASES:
        return "Pro", "gpt-5.6-pro"
    return requested.strip(), requested.strip()


async def locate_composer_control(page, testid_selectors, accessible_name):
    """Find a composer control by stable test id, else composer-scoped role.

    Returns ``(locator_or_none, used_fallback)``. Never scans the whole page
    by loose text, which could match private sidebar content.
    """
    for selector in testid_selectors:
        control = page.locator(selector).last
        try:
            if await control.count() and await control.is_visible():
                return control, False
        except Exception:
            continue
    control = page.locator("form").get_by_role("button", name=accessible_name).last
    try:
        if await control.count() and await control.is_visible():
            return control, True
    except Exception:
        pass
    return None, False


async def raise_if_rate_limited(page) -> None:
    """Detect the stable, account-wide ChatGPT rate-limit heading.

    Keep this exact and narrow: arbitrary page text is private conversation
    data and must neither be logged nor used for fuzzy classification.
    """
    # Current ChatGPT renders the account-wide limit as a modal whose overlay
    # blocks every composer click. Its stable test id is stronger evidence
    # than copy text and lets us classify the limit even while the modal body
    # is still hydrating or its wording is being A/B tested.
    modal = page.locator(
        f'[data-testid="{RATE_LIMIT_MODAL_TESTID}"]:visible'
    ).first
    try:
        if await modal.count() and await modal.is_visible():
            raise RateLimited()
    except RateLimited:
        raise
    except Exception:
        pass

    heading = page.get_by_role(
        "heading", name=RATE_LIMIT_HEADING, exact=True
    ).first
    try:
        if await heading.count() and await heading.is_visible():
            raise RateLimited()
    except RateLimited:
        raise
    except Exception:
        # A missing/transient locator is not rate-limit evidence.
        pass
    # Some ChatGPT rollouts render the warning inside a portal without heading
    # semantics. Scope the fallback to modal surfaces so quoted conversation
    # text can never open the account-wide circuit.
    try:
        dialogs = page.locator('[role="dialog"], [aria-modal="true"]')
        for index in range(await dialogs.count()):
            dialog = dialogs.nth(index)
            if not await dialog.is_visible():
                continue
            dialog_text = await dialog.inner_text(timeout=2_000)
            if (
                RATE_LIMIT_HEADING in dialog_text
                and "temporarily limited access" in dialog_text
            ):
                raise RateLimited()
    except RateLimited:
        raise
    except Exception:
        return


async def click_send_control(page) -> bool:
    """Click the composer send control after proving it is actionable.

    ChatGPT occasionally replaces the hydrated composer node while the prompt
    is being filled. Re-resolve the locator once instead of letting Playwright
    spend its full default timeout on a detached element. The trial click is
    intentionally side-effect free; we never use force or an Enter fallback,
    either of which could duplicate an ambiguously submitted prompt.
    """
    used_fallback = False
    click_failures = 0
    for _ in range(20):  # allow ~10 seconds for hydration to attach the control
        control, fallback = await locate_composer_control(
            page, SEND_CONTROL_TESTIDS, SEND_BUTTON_NAME
        )
        used_fallback = used_fallback or fallback
        if control is None:
            await page.wait_for_timeout(500)
            continue
        try:
            await control.click(trial=True, timeout=5_000)
            await control.click(timeout=8_000, no_wait_after=True)
            return used_fallback
        except Exception:
            click_failures += 1
            if click_failures >= 2:
                break
            await page.wait_for_timeout(1_000)
    await raise_if_rate_limited(page)
    # Reaching this point means the authenticated ChatGPT shell and composer
    # were already hydrated. A separate APIRequest probe does not share the
    # persistent browser context reliably and used to misclassify a disabled
    # send control as a proxy outage, triggering a harmful automatic retry.
    raise RuntimeError("composer send control is not actionable")


def intelligence_slider_index(label: str) -> int | None:
    try:
        return INTELLIGENCE_SLIDER_LABELS.index(label)
    except ValueError:
        return None


async def select_intelligence_slider(page, overlay, slider, pill, label: str) -> bool:
    """Move the composer power slider to Instant/Medium/High/Extra High/Pro."""
    target = intelligence_slider_index(label)
    if target is None:
        return False
    simple = overlay.locator('[data-testid="composer-model-picker-slider-simple-view"]')
    try:
        await slider.focus()
    except Exception:
        pass
    for _ in range(10):
        raw = await slider.get_attribute("aria-valuenow")
        try:
            now = int(raw) if raw is not None else -1
        except (TypeError, ValueError):
            now = -1
        simple_text = ""
        if await simple.count():
            try:
                simple_text = (await simple.inner_text()).strip()
            except Exception:
                simple_text = ""
        current = simple_text.split(",")[0].strip() if simple_text else ""
        if current == label or now == target:
            await page.keyboard.press("Escape")
            try:
                await pill.get_by_text(label, exact=True).wait_for(
                    state="visible", timeout=3_000
                )
            except Exception:
                if (await pill.inner_text()).strip() != label:
                    return False
            return True
        if now < 0:
            return False
        await slider.press("ArrowRight" if now < target else "ArrowLeft")
        await page.wait_for_timeout(250)
    return False


async def choose_intelligence_model(page, label: str) -> bool:
    """Select a current composer intelligence option without touching the sidebar."""
    # The current ChatGPT shell hydrates the composer in two phases: the
    # textbox can be ready several seconds before the model pill is attached.
    # Wait for the composer-scoped control instead of snapshotting its count
    # immediately and incorrectly falling back to the legacy model picker.
    # Scope to the composer and require a menu-bearing button, but do not
    # depend on ChatGPT's private ``__composer-pill`` CSS class.  The visible
    # label is still checked against the strict intelligence allowlist below,
    # so unrelated composer menus cannot be selected.
    picker_buttons = page.locator('form button[aria-haspopup="menu"]:visible')
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
            slider = overlay.locator('[role="slider"]')
            if await slider.count() and await slider.first.is_visible():
                if await select_intelligence_slider(
                    page, overlay, slider.first, button, label
                ):
                    return True
                continue
            option = overlay.get_by_role("menuitemradio", name=label, exact=True)
            await option.wait_for(state="visible", timeout=3_000)
            await option.click()
            selected = button.get_by_text(label, exact=True)
            await selected.wait_for(state="visible", timeout=3_000)
            return True
        except Exception:
            continue
    await raise_if_rate_limited(page)
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
        await raise_if_rate_limited(page)
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
    await raise_if_rate_limited(page)
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


def is_cloudflare_challenge_title(title: str | None) -> bool:
    return bool(CLOUDFLARE_TITLE.search(title or ""))


def is_saved_account_choice(label: str) -> bool:
    """True for a welcome-back saved-account card, never for Log in / Sign up."""
    text = (label or "").strip()
    if not text or ACCOUNT_PICKER_SKIP.search(text):
        return False
    return bool(SAVED_ACCOUNT_EMAIL.search(text))


async def login_chrome_visible(page) -> bool:
    login = page.get_by_role("button", name=LOGIN_BUTTON_NAME)
    return bool(await login.count() and await login.first.is_visible())


async def account_shell_visible(page) -> bool:
    """Account-only chrome. Images/Deep research also exist on the logged-out home."""
    account_controls = page.locator(
        '[data-testid="accounts-profile-button"], '
        'button[aria-label*="account" i], '
        'button[aria-label*="profile" i]'
    )
    for index in range(await account_controls.count()):
        if await account_controls.nth(index).is_visible():
            return True
    nav_evidence = page.locator(
        'a[href*="/library"], a[href*="/scheduled"], '
        '[data-testid="create-scheduled-task-button"]'
    )
    for index in range(await nav_evidence.count()):
        if await nav_evidence.nth(index).is_visible():
            return True
    return False


async def wait_out_cloudflare(page, timeout_ms: int = 45_000) -> None:
    """Wait for ChatGPT's Cloudflare interstitial to clear.

    Headed Chromium through the DGX SOCKS exit often sits on
    ``Just a moment...`` / ``Verifying...`` for several seconds. Treating
    that page as a logout quarantines every pool slot.
    """
    waited = False
    for _ in range(max(1, timeout_ms // 500)):
        title = ""
        try:
            title = await page.title()
        except Exception:
            title = ""
        body = ""
        try:
            body = await page.inner_text("body")
        except Exception:
            body = ""
        if not is_cloudflare_challenge_title(title) and not CLOUDFLARE_BODY.search(body or ""):
            if waited:
                emit("diagnostic", message="stage=cloudflare_cleared")
            return
        if not waited:
            emit("diagnostic", message="stage=cloudflare_wait")
            waited = True
        await page.wait_for_timeout(500)
    raise TransportUnavailable("Cloudflare interstitial did not clear")


async def complete_saved_account_picker(page, timeout_ms: int = 25_000) -> bool:
    """Click the stored account on ChatGPT's welcome-back picker.

    After a CF challenge (or a cookie refresh) ChatGPT shows
    ``Welcome back / Choose an account to continue`` with the profile's
    saved account. The card is a ``div[role=button]``, not a ``<button>``,
    and the overlay often lands 15–20s after ``domcontentloaded``. A visible
    ``Log in`` chrome on that overlay used to be classified as
    ``auth_required``.
    """
    heading = page.get_by_text(ACCOUNT_PICKER_HEADING)
    appeared = False
    for _ in range(max(1, timeout_ms // 250)):
        if await heading.count() and await heading.first.is_visible():
            appeared = True
            break
        if not await login_chrome_visible(page):
            return False
        await page.wait_for_timeout(250)
    if not appeared:
        return False
    emit("diagnostic", message="stage=account_picker")
    # Native ``<button>`` and the welcome-back account card (div role=button).
    buttons = page.get_by_role("button")
    for index in range(await buttons.count()):
        button = buttons.nth(index)
        try:
            if not await button.is_visible():
                continue
            label = await button.inner_text()
        except Exception:
            continue
        if not is_saved_account_choice(label):
            continue
        await button.click(timeout=8_000)
        emit("diagnostic", message="stage=account_picker_selected")
        for _ in range(80):
            heading_up = bool(
                await heading.count() and await heading.first.is_visible()
            )
            if (
                not heading_up
                and not await login_chrome_visible(page)
                and await account_shell_visible(page)
            ):
                return True
            await page.wait_for_timeout(250)
        if await account_shell_visible(page):
            return True
        raise PermissionError("login required")
    raise PermissionError(
        "welcome-back account picker is visible but no saved account could be selected"
    )


async def verify_authentication(page) -> None:
    """Require account-only evidence of an authenticated session."""
    if "/auth/" in page.url or await login_chrome_visible(page):
        if await complete_saved_account_picker(page):
            emit("diagnostic", message="stage=account_confirmed")
            return
        if "/auth/" in page.url or await login_chrome_visible(page):
            raise PermissionError("login required")

    if await account_shell_visible(page):
        emit("diagnostic", message="stage=account_confirmed")
        return
    # Guard contract: name what was probed and where. Without this, the
    # 2026-08-06 false positive read as "login required" and the dispatching
    # agent asked the operator to re-provision 12 accounts that were fine.
    raise PermissionError(
        "authenticated account control not found: no visible match for "
        "accounts-profile-button / account / profile aria-labels, nor for the "
        f"Library/Scheduled nav, on {page.url!r} (title {await page.title()!r}). "
        "If ChatGPT's UI was redesigned again, the selector lists in "
        "verify_authentication need the new evidence — the session cookie may "
        "still be perfectly valid."
    )


async def establish_resumed_chat(page, conversation_path: str, message: str) -> int:
    """Reattach to a recorded conversation and prove it holds this prompt.

    All verification happens in memory; nothing from the page is emitted. The
    conversation is expected to hold exactly one user message (mission turns
    always start from a blank chat), and that message must equal the prompt
    this run was asked to submit — otherwise reattaching would return someone
    else's response.
    """
    await page.goto(
        f"https://chatgpt.com{conversation_path}",
        wait_until="domcontentloaded",
        timeout=60_000,
    )
    emit("diagnostic", message="stage=resume_route")
    await wait_out_cloudflare(page)
    await complete_saved_account_picker(page)
    await raise_if_rate_limited(page)
    await verify_authentication(page)
    # Unknown or deleted conversations redirect away from the recorded route.
    await page.wait_for_timeout(2_000)
    parsed = urlparse(page.url)
    if parsed.netloc.lower() not in CHATGPT_HOSTS or parsed.path != conversation_path:
        raise ResumeNotFound()
    user_messages = page.locator('[data-message-author-role="user"]')
    count = await user_messages.count()
    if count == 0:
        raise ResumeNotFound()
    if count != 1:
        raise ResumeMismatch()
    text = await user_messages.first.inner_text()
    if normalized_prompt(text) != normalized_prompt(message):
        raise ResumeMismatch()
    emit("diagnostic", message="stage=resume_verified")
    return 0


async def establish_continued_chat(page, conversation_path: str) -> int:
    """Open a mission-owned completed conversation for its next message."""
    await page.goto(
        f"https://chatgpt.com{conversation_path}",
        wait_until="domcontentloaded",
        timeout=60_000,
    )
    emit("diagnostic", message="stage=continuation_route")
    await wait_out_cloudflare(page)
    await complete_saved_account_picker(page)
    await raise_if_rate_limited(page)
    await verify_authentication(page)
    parsed = urlparse(page.url)
    if parsed.netloc.lower() not in CHATGPT_HOSTS or parsed.path != conversation_path:
        raise ResumeNotFound()
    user_messages = page.locator('[data-message-author-role="user"]')
    responses = page.locator('[data-message-author-role="assistant"]')
    # The route and account shell settle before the historical messages are
    # hydrated. A fixed two-second delay was flaky under concurrent profiles:
    # the same conversation became visible on an immediate retry. Wait for
    # positive evidence from both sides of the completed turn instead.
    for _ in range(40):
        if await user_messages.count() > 0 and await responses.count() > 0:
            break
        await page.wait_for_timeout(500)
    else:
        raise ResumeNotFound()
    composer = page.locator("#prompt-textarea").first
    if not await composer.count():
        composer = page.get_by_role("textbox").last
    await composer.wait_for(state="visible", timeout=20_000)
    emit("diagnostic", message="stage=continuation_verified")
    return await responses.count()


async def establish_fresh_chat(page) -> int:
    """Prove an authenticated, settled blank chat before observing responses."""
    await page.goto(CHATGPT_URL, wait_until="domcontentloaded", timeout=60_000)
    emit("diagnostic", message="stage=page_loaded")
    await wait_out_cloudflare(page)
    await complete_saved_account_picker(page)
    await raise_if_rate_limited(page)
    await verify_authentication(page)

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

    probe_only = request.get("type") == "probe"
    message = request.get("message")
    if not probe_only and (not isinstance(message, str) or not message.strip()):
        fail("invalid_request", "message must be a non-empty string")
        return
    timeout_ms = max(
        30_000, min(int(request.get("timeout_ms", 14_400_000)), 86_400_000)
    )
    requested_model = request.get("model") or ""
    durability = request.get("durability") is True
    resume_request = request.get("resume")
    continuation_request = request.get("conversation")
    resume_path = None
    continuation_path = None
    if resume_request is not None and continuation_request is not None:
        fail("invalid_request", "resume and conversation are mutually exclusive")
        return
    if resume_request is not None:
        resume_path = resume_conversation_path(resume_request)
        if resume_path is None:
            fail(
                "invalid_request",
                "resume.conversation_path must be an opaque /c/<id> route",
            )
            return
    if continuation_request is not None:
        continuation_path = continued_conversation_path(continuation_request)
        if continuation_path is None:
            fail(
                "invalid_request",
                "conversation.conversation_path must be an opaque /c/<id> route",
            )
            return
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
            if probe_only:
                stage = "recovery_probe"
                await establish_fresh_chat(page)
                stage = "model_selection"
                await choose_model(page, requested_model)
                await raise_if_rate_limited(page)
                await assert_blank_chat(page)
                emit("probe_ready")
                return
            if resume_path is not None:
                stage = "resume"
                baseline = await establish_resumed_chat(page, resume_path, message)
                # The prompt was submitted by a prior run; the picker state it
                # used is part of the conversation and must not be touched.
                model_used = (
                    model_selection(requested_model)[1] if requested_model else ""
                )
            elif continuation_path is not None:
                stage = "continuation"
                baseline = await establish_continued_chat(page, continuation_path)
                stage = "model_selection"
                model_used = await choose_model(page, requested_model)
                stage = "composer"
                composer = page.locator("#prompt-textarea").first
                if not await composer.count():
                    composer = page.get_by_role("textbox").last
                await composer.fill(message)
                stage = "send"
                if await click_send_control(page):
                    emit("diagnostic", message="stage=send_button_fallback")
            else:
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
                if await click_send_control(page):
                    emit("diagnostic", message="stage=send_button_fallback")

            stage = "response"
            last = ""
            stable = 0
            stop_fallback_reported = False
            submitted_emitted = resume_path is not None
            deadline = asyncio.get_running_loop().time() + timeout_ms / 1000
            while asyncio.get_running_loop().time() < deadline:
                await raise_if_rate_limited(page)
                if durability and not submitted_emitted:
                    # The URL flips to the opaque conversation route once the
                    # prompt is accepted. That route is the only durable
                    # pointer this driver ever reports.
                    submitted_route = conversation_path_from_url(page.url)
                    if submitted_route is not None:
                        submitted_emitted = True
                        emit("submitted", conversation_path=submitted_route)
                responses = page.locator('[data-message-author-role="assistant"]')
                count = await responses.count()
                if count > baseline:
                    text = (await responses.nth(count - 1).inner_text()).strip()
                    if text and text != last:
                        last, stable = text, 0
                        emit("text_delta", content=text)
                    elif text:
                        stable += 1
                stop, stop_fallback = await locate_composer_control(
                    page, STOP_CONTROL_TESTIDS, STOP_BUTTON_NAME
                )
                if stop_fallback and not stop_fallback_reported:
                    stop_fallback_reported = True
                    emit("diagnostic", message="stage=stop_button_fallback")
                stop_visible = stop is not None
                if last and count > baseline and not stop_visible and stable >= 2:
                    if durability and not submitted_emitted:
                        emit(
                            "diagnostic",
                            message="stage=conversation_ref_unavailable",
                        )
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
    except RateLimited:
        fail(
            "rate_limited",
            "ChatGPT temporarily limited this account after requests were made too quickly; the shared account circuit is cooling down",
            stage=stage,
        )
    except ResumeNotFound:
        fail(
            "continuation_not_found" if stage == "continuation" else "resume_not_found",
            "the recorded conversation is not reachable from this profile",
        )
    except ResumeMismatch:
        fail(
            "resume_mismatch",
            "the recorded conversation does not hold exactly this prompt",
        )
    except Exception as exc:
        if isinstance(exc, TransportUnavailable):
            fail(
                "transport_unavailable",
                "ChatGPT is unreachable through the configured browser proxy",
                stage=stage,
            )
            return
        fail(
            "compatibility",
            f"{COMPAT_VERSION}: UI check failed at {stage} ({type(exc).__name__}); verify selectors against a blank, non-private chat",
            stage=stage,
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
