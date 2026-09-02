#!/usr/bin/env python3
"""Modular ChatGPT login steps for ``chatgpt_ui_relogin``.

OpenAI's login wizard does not have a stable field order or chrome. Some
turns ask email then password then TOTP; others open on a welcome-back
picker, a passkey interstitial, a combined email+password form, or a
"verify it's you" TOTP page. Locale (en/fr) and Cloudflare change the
markup further.

``run_login`` therefore does not encode a sequence. Each tick it snapshots
the page and dispatches to the first matching ``LoginStep``. Adding a newly
observed UI is:

1. Extend ``UiSnapshot`` / ``observe()`` if you need a new signal.
2. Write a ``LoginStep`` subclass (``name``, ``matches``, ``apply``).
3. Insert it in ``LOGIN_STEPS`` — fatals first, then dismissals, then fills.
4. Add a ``select_step`` test with a ``UiSnapshot`` in
   ``test_chatgpt_ui_login_steps.py``.

Never log ``UiSnapshot.body`` or credential values.
"""
from __future__ import annotations

import asyncio
import re
import time
from dataclasses import dataclass, field
from typing import Callable, Sequence
from urllib.parse import urlparse

try:
    from chatgpt_ui_driver import (
        CHATGPT_URL,
        CLOUDFLARE_BODY,
        TransportUnavailable,
        account_shell_visible,
        complete_saved_account_picker,
        is_cloudflare_challenge_title,
        wait_out_cloudflare,
    )
except ImportError:
    from scripts.chatgpt_ui_driver import (
        CHATGPT_URL,
        CLOUDFLARE_BODY,
        TransportUnavailable,
        account_shell_visible,
        complete_saved_account_picker,
        is_cloudflare_challenge_title,
        wait_out_cloudflare,
    )

CONTINUE = "continue"
DONE = "logged_in"
ALREADY = "already_authenticated"

LOGIN_CLICK_NAME = re.compile(
    r"^(log in|sign in|se connecter|connexion|anmelden|iniciar sesi[oó]n)$", re.I
)
CONTINUE_NAME = re.compile(
    r"^(continue|next|log in|sign in|verify|submit|authenticate|continuer|"
    r"suivant|valider|se connecter|weiter|continuar)$",
    re.I,
)
PASSKEY_SKIP_NAME = re.compile(
    r"not now|skip|use (a )?password|try another way|cancel|plus tard|"
    r"utiliser un mot de passe|une autre m[eé]thode",
    re.I,
)
COOKIE_NAME = re.compile(
    r"^(accept( all)?( cookies)?|i agree|allow all|tout accepter|"
    r"accepter( tout)?|j'accepte)$",
    re.I,
)
STAY_SIGNED_IN_NAME = re.compile(
    r"stay (signed|logged) in|trust this (device|browser)|yes, continue|"
    r"rester connect[eé]|faire confiance",
    re.I,
)
CHOOSE_TOTP_NAME = re.compile(
    r"authenticator|authentication app|app d['’ ]?authentification|"
    r"application d['’ ]?authentification|totp",
    re.I,
)
ANOTHER_ACCOUNT_NAME = re.compile(
    r"log in to another account|use another account|un autre compte|"
    r"another account",
    re.I,
)
EMAIL_CODE_BODY = re.compile(
    r"check your email|we (emailed|sent) you a code|enter the code we sent|"
    r"code (envoy[eé]|par e-?mail)|v[eé]rifiez votre e-?mail",
    re.I,
)
PHONE_BODY = re.compile(
    r"phone number|text (you )?a code|sms code|num[eé]ro de t[eé]l[eé]phone|"
    r"code par (sms|texte)",
    re.I,
)
CREDENTIAL_ERROR = re.compile(
    r"incorrect (email|password|code)|wrong password|did not match|"
    r"invalid (email|password|code)|code is incorrect|"
    r"mot de passe incorrect|e-?mail incorrect|code incorrect",
    re.I,
)
ACCOUNT_PICKER_BODY = re.compile(
    r"choose an account to continue|welcome back|choisissez un compte|"
    r"choisir un compte|log in to another account",
    re.I,
)
SSO_LABEL = re.compile(
    r"continue with (google|apple|microsoft)|google|apple|microsoft", re.I
)
EMAIL_IN_TEXT = re.compile(r"[^@\s]+@[^@\s]+\.[^@\s]+")

LOGIN_HREF_SELECTOR = (
    'a[href*="auth/login"], a[href*="/login"], a[href*="auth.openai.com"], '
    '[data-testid="login-button"], [data-testid="hero-login-button"]'
)
AUTH_LOGIN_URL = "https://chatgpt.com/auth/login"
EMAIL_SELECTOR = (
    "input[type='email'], input[name='username'], input[name='email'], "
    "input#email-input, input[autocomplete='username'], input[autocomplete='email']"
)
PASSWORD_SELECTOR = (
    "input[type='password'], input[name='password'], input#password, "
    "input[autocomplete='current-password']"
)
OTP_SELECTOR = (
    "input[autocomplete='one-time-code'], input[name='code'], "
    "input[name='otp'], input[inputmode='numeric'], "
    "input[aria-label*='code' i], input[aria-label*='authenticator' i]"
)
PHONE_SELECTOR = "input[type='tel'], input[name='phone'], input[autocomplete='tel']"
EMAIL_LABEL = re.compile(r"^(email|e-?mail|username|adresse e-?mail|identifiant)$", re.I)
PASSWORD_LABEL = re.compile(r"^(password|mot de passe|passwort|contrase[nñ]a)$", re.I)


class ReloginError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def log(message: str) -> None:
    print(f"chatgpt-ui-relogin: {message}", flush=True)


@dataclass
class UiSnapshot:
    """Structured, secret-free view of the current login surface."""

    url: str = ""
    title: str = ""
    body: str = field(default="", repr=False)
    labels: tuple[str, ...] = ()
    has_email: bool = False
    has_password: bool = False
    has_otp: bool = False
    has_login_button: bool = False
    has_account_picker: bool = False
    has_account_shell: bool = False
    has_passkey: bool = False
    has_cloudflare: bool = False
    has_email_code: bool = False
    has_phone: bool = False
    has_cookie: bool = False
    has_stay_signed_in: bool = False
    has_choose_totp: bool = False
    has_credential_error: bool = False
    has_sso: bool = False
    has_continue: bool = False


@dataclass
class LoginContext:
    creds: dict[str, str]
    totp_fn: Callable[[str], str]
    filled: set[str] = field(default_factory=set)
    applied: list[str] = field(default_factory=list)
    last_login_click_at: float = 0.0
    last_otp_at: float = 0.0
    now: Callable[[], float] = time.time


class LoginStep:
    """One recoverable login surface. Subclass and append to LOGIN_STEPS."""

    name = "step"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        raise NotImplementedError

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        raise NotImplementedError


def _safe_labels(labels: Sequence[str]) -> tuple[str, ...]:
    cleaned = []
    for raw in labels:
        text = EMAIL_IN_TEXT.sub("***", (raw or "").strip())
        if text:
            cleaned.append(text[:80])
        if len(cleaned) >= 20:
            break
    return tuple(cleaned)


def describe_snapshot(snap: UiSnapshot) -> str:
    path = urlparse(snap.url or "").path or "/"
    labels = ",".join(snap.labels[:8]) or "none"
    return f"url={path} title={snap.title!r} buttons={labels}"


def on_auth_host(url: str) -> bool:
    host = (urlparse(url or "").netloc or "").lower()
    return "/auth/" in (url or "") or host.startswith("auth.") or "auth.openai.com" in host


async def first_visible(locator, timeout_ms: int = 0):
    deadline = time.time() + max(timeout_ms, 0) / 1000.0
    while True:
        try:
            count = await locator.count()
        except Exception:
            count = 0
        for index in range(count):
            node = locator.nth(index)
            try:
                if await node.is_visible():
                    return node
            except Exception:
                continue
        if time.time() >= deadline:
            return None
        await asyncio.sleep(0.2)


async def any_visible(page, selector: str) -> bool:
    try:
        return await first_visible(page.locator(selector), timeout_ms=0) is not None
    except Exception:
        return False


async def labeled_visible(page, pattern: re.Pattern) -> bool:
    try:
        return await first_visible(page.get_by_label(pattern), timeout_ms=0) is not None
    except Exception:
        return False


async def collect_labels(page) -> tuple[str, ...]:
    labels: list[str] = []
    for role in ("button", "link"):
        try:
            locator = page.get_by_role(role)
            count = await locator.count()
        except Exception:
            continue
        for index in range(min(count, 24)):
            try:
                node = locator.nth(index)
                if not await node.is_visible():
                    continue
                text = (await node.inner_text() or "").strip()
            except Exception:
                continue
            if text:
                labels.append(text.split("\n")[0])
    return _safe_labels(labels)


def _label_match(labels: Sequence[str], pattern: re.Pattern) -> bool:
    return any(pattern.search(label) for label in labels)


def frames_of(page):
    try:
        frames = list(page.frames)
        if frames:
            return frames
    except Exception:
        pass
    return [page]


def merge_snapshots(base: UiSnapshot, extra: UiSnapshot) -> UiSnapshot:
    flags = {
        field: bool(getattr(base, field) or getattr(extra, field))
        for field in (
            "has_email",
            "has_password",
            "has_otp",
            "has_login_button",
            "has_account_picker",
            "has_account_shell",
            "has_passkey",
            "has_cloudflare",
            "has_email_code",
            "has_phone",
            "has_cookie",
            "has_stay_signed_in",
            "has_choose_totp",
            "has_credential_error",
            "has_sso",
            "has_continue",
        )
    }
    return UiSnapshot(
        url=base.url or extra.url,
        title=base.title or extra.title,
        body=base.body or extra.body,
        labels=_safe_labels(tuple(base.labels) + tuple(extra.labels)),
        **flags,
    )


async def observe_one(page) -> UiSnapshot:
    url = ""
    title = ""
    body = ""
    try:
        url = page.url or ""
    except Exception:
        url = ""
    try:
        title = await page.title()
    except Exception:
        title = ""
    try:
        body = (await page.inner_text("body") or "")[:8000]
    except Exception:
        body = ""
    labels = await collect_labels(page)
    otp_digits = 0
    try:
        singles = page.locator("input[maxlength='1']")
        count = await singles.count()
        for index in range(count):
            try:
                if await singles.nth(index).is_visible():
                    otp_digits += 1
            except Exception:
                continue
    except Exception:
        otp_digits = 0
    has_otp = otp_digits >= 4 or await any_visible(page, OTP_SELECTOR)
    return UiSnapshot(
        url=url,
        title=title,
        body=body,
        labels=labels,
        has_email=await any_visible(page, EMAIL_SELECTOR)
        or await labeled_visible(page, EMAIL_LABEL),
        has_password=await any_visible(page, PASSWORD_SELECTOR)
        or await labeled_visible(page, PASSWORD_LABEL),
        has_otp=has_otp,
        has_login_button=_label_match(labels, LOGIN_CLICK_NAME)
        or any(LOGIN_CLICK_NAME.fullmatch(line.strip()) for line in body.splitlines())
        or await any_visible(page, LOGIN_HREF_SELECTOR),
        has_account_picker=bool(ACCOUNT_PICKER_BODY.search(body))
        or _label_match(labels, ANOTHER_ACCOUNT_NAME),
        has_account_shell=await account_shell_visible(page),
        has_passkey=bool(
            re.search(
                r"passkey|cl[eé] d.acc[eè]s|fingerprint|webauthn",
                body,
                re.I,
            )
        )
        or _label_match(
            labels,
            re.compile(r"use (a )?password|utiliser un mot de passe", re.I),
        ),
        has_cloudflare=is_cloudflare_challenge_title(title)
        or bool(CLOUDFLARE_BODY.search(body)),
        has_email_code=bool(EMAIL_CODE_BODY.search(body)),
        has_phone=await any_visible(page, PHONE_SELECTOR) or bool(PHONE_BODY.search(body)),
        has_cookie=_label_match(labels, COOKIE_NAME),
        has_stay_signed_in=_label_match(labels, STAY_SIGNED_IN_NAME),
        has_choose_totp=_label_match(labels, CHOOSE_TOTP_NAME),
        has_credential_error=bool(CREDENTIAL_ERROR.search(body)),
        has_sso=_label_match(labels, SSO_LABEL),
        has_continue=_label_match(labels, CONTINUE_NAME),
    )


async def observe(page) -> UiSnapshot:
    snap = await observe_one(page)
    for frame in frames_of(page):
        try:
            if frame is page or frame == getattr(page, "main_frame", None):
                continue
        except Exception:
            continue
        try:
            snap = merge_snapshots(snap, await observe_one(frame))
        except Exception:
            continue
    return snap


async def click_named_in(target, pattern: re.Pattern) -> bool:
    for getter in (
        lambda: target.get_by_role("button", name=pattern),
        lambda: target.get_by_role("link", name=pattern),
        lambda: target.get_by_text(pattern),
    ):
        try:
            locator = getter()
        except Exception:
            continue
        node = await first_visible(locator, timeout_ms=800)
        if node is None:
            continue
        try:
            await node.click(timeout=8_000)
            return True
        except Exception:
            try:
                await node.click(timeout=4_000, force=True)
                return True
            except Exception:
                continue
    return False


async def click_named(page, pattern: re.Pattern) -> bool:
    for frame in frames_of(page):
        if await click_named_in(frame, pattern):
            return True
    return False


async def click_login_control(page) -> bool:
    if await click_named(page, LOGIN_CLICK_NAME):
        return True
    node = None
    for frame in frames_of(page):
        node = await first_visible(frame.locator(LOGIN_HREF_SELECTOR), timeout_ms=400)
        if node is not None:
            break
    if node is None:
        return False
    try:
        await node.click(timeout=8_000)
        return True
    except Exception:
        try:
            await node.click(timeout=4_000, force=True)
            return True
        except Exception:
            return False


async def submit_continue(page, field=None) -> None:
    if await click_named(page, CONTINUE_NAME):
        return
    if field is not None:
        try:
            await field.press("Enter")
            return
        except Exception:
            pass
    raise ReloginError("login_failed", "no Continue/Log in control was visible")


async def fill_first(page, selector: str, label: re.Pattern, value: str):
    for frame in frames_of(page):
        node = await first_visible(frame.locator(selector), timeout_ms=800)
        if node is None:
            try:
                node = await first_visible(frame.get_by_label(label), timeout_ms=400)
            except Exception:
                node = None
        if node is None:
            continue
        await node.fill(value)
        return node
    return None


async def fill_otp(page, code: str) -> bool:
    for frame in frames_of(page):
        singles = frame.locator("input[maxlength='1']")
        visible = []
        try:
            count = await singles.count()
        except Exception:
            count = 0
        for index in range(count):
            node = singles.nth(index)
            try:
                if await node.is_visible():
                    visible.append(node)
            except Exception:
                continue
        if len(visible) >= 4:
            for node, digit in zip(visible, code):
                await node.fill(digit)
            return True
        box = await first_visible(frame.locator(OTP_SELECTOR), timeout_ms=800)
        if box is None:
            continue
        await box.fill(code)
        return True
    return False


def select_step(
    snap: UiSnapshot,
    ctx: LoginContext,
    steps: Sequence[LoginStep] | None = None,
) -> LoginStep | None:
    for step in steps or LOGIN_STEPS:
        if step.matches(snap, ctx):
            return step
    return None


class AuthenticatedStep(LoginStep):
    name = "authenticated"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        if snap.has_cloudflare or snap.has_login_button or snap.has_account_picker:
            return False
        if on_auth_host(snap.url):
            return False
        return snap.has_account_shell

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        return ALREADY if not ctx.filled else DONE


class CloudflareStep(LoginStep):
    name = "cloudflare"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_cloudflare

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        try:
            # Auth.openai.com after email submit often sits on "Just a moment..."
            # longer than a ChatGPT home probe. Still fail closed if Turnstile
            # never clears — no checkbox clicking.
            await wait_out_cloudflare(page, timeout_ms=90_000)
        except TransportUnavailable as error:
            raise ReloginError("challenge", "Cloudflare interstitial did not clear") from error
        return CONTINUE


class CredentialErrorStep(LoginStep):
    name = "credential_error"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_credential_error

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        raise ReloginError(
            "login_failed",
            "ChatGPT rejected the credentials or TOTP (no retry in this run)",
        )


class EmailCodeMfaStep(LoginStep):
    name = "email_code_mfa"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_email_code and not snap.has_otp

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        raise ReloginError(
            "mfa_unsupported",
            "ChatGPT asked for an emailed code; TOTP-only relogin cannot continue",
        )


class PhoneMfaStep(LoginStep):
    name = "phone_mfa"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_phone and not snap.has_email and not snap.has_password

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        raise ReloginError(
            "mfa_unsupported",
            "ChatGPT asked for SMS/phone verification; TOTP-only relogin cannot continue",
        )


class SsoOnlyStep(LoginStep):
    name = "sso_only"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return (
            snap.has_sso
            and not snap.has_email
            and not snap.has_password
            and not snap.has_otp
            and not snap.has_login_button
        )

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        raise ReloginError(
            "login_failed",
            "ChatGPT only offered SSO; email/password form was not visible",
        )


class CookieBannerStep(LoginStep):
    name = "cookie_banner"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_cookie

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        await click_named(page, COOKIE_NAME)
        return CONTINUE


class PasskeyStep(LoginStep):
    name = "passkey"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_passkey

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        if not await click_named(page, PASSKEY_SKIP_NAME):
            raise ReloginError("login_failed", "passkey prompt could not be skipped")
        return CONTINUE


class StaySignedInStep(LoginStep):
    name = "stay_signed_in"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_stay_signed_in

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        if not await click_named(page, STAY_SIGNED_IN_NAME):
            await click_named(page, CONTINUE_NAME)
        return CONTINUE


class ChooseTotpStep(LoginStep):
    name = "choose_totp"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_choose_totp and not snap.has_otp

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        if not await click_named(page, CHOOSE_TOTP_NAME):
            raise ReloginError("login_failed", "authenticator-app option was not clickable")
        return CONTINUE


class AccountPickerStep(LoginStep):
    name = "account_picker"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_account_picker or _label_match(snap.labels, ANOTHER_ACCOUNT_NAME)

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        try:
            if await complete_saved_account_picker(page, timeout_ms=25_000):
                return CONTINUE
        except PermissionError:
            pass
        if await click_named(page, ANOTHER_ACCOUNT_NAME):
            return CONTINUE
        raise ReloginError(
            "login_failed",
            "welcome-back account picker is visible but no saved account could be selected",
        )


class AnotherAccountStep(LoginStep):
    """Optional: skip the saved-account card and force email/password.

    Not in LOGIN_STEPS. Credential probes prepend it and drop account_picker.
    """

    name = "another_account"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_account_picker and "another_account" not in ctx.filled

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        ctx.filled.add("another_account")
        if not await click_named(page, ANOTHER_ACCOUNT_NAME):
            raise ReloginError(
                "login_failed",
                "account picker had no 'log in to another account' control",
            )
        return CONTINUE


class EmailAndPasswordStep(LoginStep):
    name = "email_and_password"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        if not (snap.has_email and snap.has_password):
            return False
        return "email" not in ctx.filled or "password" not in ctx.filled

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        email = await fill_first(
            page, EMAIL_SELECTOR, EMAIL_LABEL, ctx.creds["CHATGPT_USERNAME"]
        )
        password = await fill_first(
            page, PASSWORD_SELECTOR, PASSWORD_LABEL, ctx.creds["CHATGPT_PASSWORD"]
        )
        if email is None and password is None:
            raise ReloginError("login_failed", "email+password form was detected but not fillable")
        if email is not None:
            ctx.filled.add("email")
        if password is not None:
            ctx.filled.add("password")
        await submit_continue(page, password or email)
        return CONTINUE


class EmailStep(LoginStep):
    name = "email"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_email and "email" not in ctx.filled

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        field = await fill_first(
            page, EMAIL_SELECTOR, EMAIL_LABEL, ctx.creds["CHATGPT_USERNAME"]
        )
        if field is None:
            raise ReloginError("login_failed", "email field was detected but not fillable")
        ctx.filled.add("email")
        await submit_continue(page, field)
        return CONTINUE


class PasswordStep(LoginStep):
    name = "password"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        return snap.has_password and "password" not in ctx.filled

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        field = await fill_first(
            page, PASSWORD_SELECTOR, PASSWORD_LABEL, ctx.creds["CHATGPT_PASSWORD"]
        )
        if field is None:
            raise ReloginError("login_failed", "password field was detected but not fillable")
        ctx.filled.add("password")
        await submit_continue(page, field)
        return CONTINUE


class OtpStep(LoginStep):
    name = "otp"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        if not snap.has_otp:
            return False
        if "otp" not in ctx.filled:
            return True
        return ctx.now() - ctx.last_otp_at >= 25

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        code = ctx.totp_fn(ctx.creds["CHATGPT_OTP"])
        if not await fill_otp(page, code):
            raise ReloginError("login_failed", "OTP control was visible but could not be filled")
        ctx.filled.add("otp")
        ctx.last_otp_at = ctx.now()
        await submit_continue(page)
        return CONTINUE


class LoginButtonStep(LoginStep):
    name = "login_button"

    def matches(self, snap: UiSnapshot, ctx: LoginContext) -> bool:
        if snap.has_email or snap.has_password or snap.has_otp or snap.has_account_picker:
            return False
        if "login_clicked" in ctx.filled:
            # The current landing page can acknowledge Playwright's click
            # without opening the login overlay or navigating. Give the UI a
            # short hydration window, then let apply() use the canonical auth
            # URL instead of leaving the state machine with no matching step.
            return snap.has_login_button and ctx.now() - ctx.last_login_click_at >= 5
        return snap.has_login_button

    async def apply(self, page, ctx: LoginContext, snap: UiSnapshot) -> str:
        if "login_clicked" in ctx.filled:
            try:
                await page.goto(AUTH_LOGIN_URL, wait_until="domcontentloaded", timeout=60_000)
            except Exception as error:
                raise ReloginError(
                    "login_failed",
                    "Log in click did not transition and the canonical auth route failed",
                ) from error
            return CONTINUE
        ctx.filled.add("login_clicked")
        ctx.last_login_click_at = ctx.now()
        clicked = await click_login_control(page)
        if not clicked and not on_auth_host(snap.url):
            try:
                await page.goto(AUTH_LOGIN_URL, wait_until="domcontentloaded", timeout=60_000)
            except Exception as error:
                raise ReloginError(
                    "login_failed",
                    "Log in control was not clickable and " + describe_snapshot(snap),
                ) from error
        # Do not wait here for the next surface. The overlay with the saved
        # account often lands after this step returns; run_login dispatches
        # account_picker on the following tick.
        return CONTINUE


# Priority is the sequencer. Fatals, then overlays, then whichever field is
# currently on screen. Insert new UI variants here; do not add a fixed order
# inside run_login().
LOGIN_STEPS: tuple[LoginStep, ...] = (
    AuthenticatedStep(),
    CloudflareStep(),
    CredentialErrorStep(),
    EmailCodeMfaStep(),
    PhoneMfaStep(),
    SsoOnlyStep(),
    CookieBannerStep(),
    PasskeyStep(),
    StaySignedInStep(),
    ChooseTotpStep(),
    AccountPickerStep(),
    EmailAndPasswordStep(),
    PasswordStep(),
    EmailStep(),
    OtpStep(),
    LoginButtonStep(),
)


async def run_login(
    page,
    creds: dict[str, str],
    *,
    totp_fn: Callable[[str], str],
    steps: Sequence[LoginStep] | None = None,
    timeout_s: float = 120,
) -> str:
    ctx = LoginContext(creds=creds, totp_fn=totp_fn)
    registry = tuple(steps or LOGIN_STEPS)
    await page.goto(CHATGPT_URL, wait_until="domcontentloaded", timeout=90_000)
    deadline = time.time() + timeout_s
    idle_since = time.time()
    last_name = None
    while time.time() < deadline:
        snap = await observe(page)
        step = select_step(snap, ctx, registry)
        if step is None:
            await page.wait_for_timeout(400)
            if time.time() - idle_since > 20:
                raise ReloginError(
                    "unrecognized_ui",
                    "no login step matched "
                    + describe_snapshot(snap)
                    + f" after {ctx.applied or ['nothing']}",
                )
            continue
        idle_since = time.time()
        if step.name != last_name:
            log(f"stage={step.name}")
            last_name = step.name
        outcome = await step.apply(page, ctx, snap)
        ctx.applied.append(step.name)
        if outcome in {DONE, ALREADY}:
            return outcome
        await page.wait_for_timeout(400)
    raise ReloginError(
        "login_failed",
        "timed out after steps " + ",".join(ctx.applied or ["none"]),
    )
