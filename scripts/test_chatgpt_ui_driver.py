#!/usr/bin/env python3

import asyncio
import unittest

from scripts.chatgpt_ui_driver import (
    complete_saved_account_picker,
    is_cloudflare_challenge_title,
    is_saved_account_choice,
    wait_out_cloudflare,
    MODEL_PICKER_READY_TIMEOUT_MS,
    SEND_BUTTON_NAME,
    SEND_CONTROL_TESTIDS,
    STOP_BUTTON_NAME,
    STOP_CONTROL_TESTIDS,
    RateLimited,
    RATE_LIMIT_MODAL_TESTID,
    TransportUnavailable,
    choose_intelligence_model,
    click_send_control,
    close_context_quietly,
    continued_conversation_path,
    conversation_path_from_url,
    download_control_key,
    downloadable_href,
    establish_continued_chat,
    locate_composer_control,
    model_selection,
    normalized_prompt,
    raise_if_rate_limited,
    resume_conversation_path,
    safe_download_name,
)


class ClosedContext:
    async def close(self) -> None:
        raise RuntimeError("already closed")


class SelectedModelButton:
    def __init__(self) -> None:
        self.wait_timeout = None
        self.clicked = False

    async def wait_for(self, *, state, timeout) -> None:
        self.wait_timeout = (state, timeout)

    async def is_visible(self) -> bool:
        return True

    async def inner_text(self) -> str:
        return "Pro"

    async def click(self) -> None:
        self.clicked = True


class SingleModelPicker:
    def __init__(self, button) -> None:
        self.first = button
        self.button = button

    async def count(self) -> int:
        return 1

    def nth(self, index):
        if index != 0:
            raise IndexError(index)
        return self.button


class ComposerPage:
    def __init__(self, picker) -> None:
        self.picker = picker
        self.selector = None

    def locator(self, selector):
        self.selector = selector
        return self.picker


class FakeControl:
    def __init__(self, visible: bool) -> None:
        self.visible = visible

    @property
    def last(self):
        return self

    @property
    def first(self):
        return self

    def nth(self, _index):
        return self

    async def count(self) -> int:
        return 1 if self.visible else 0

    async def is_visible(self) -> bool:
        return self.visible


class ClickableControl(FakeControl):
    def __init__(self, failures: int = 0) -> None:
        super().__init__(True)
        self.failures = failures
        self.clicks = []

    async def click(self, **kwargs) -> None:
        self.clicks.append(kwargs)
        if not kwargs.get("trial") and self.failures:
            self.failures -= 1
            raise TimeoutError("composer was replaced")


class FakeComposerForm:
    def __init__(self, control: FakeControl) -> None:
        self.control = control
        self.requested_role = None
        self.requested_name = None

    def get_by_role(self, role, name=None):
        self.requested_role = role
        self.requested_name = name
        return self.control


class FakeComposerPage:
    class Dialog(FakeControl):
        def __init__(self, rate_limited: bool) -> None:
            super().__init__(rate_limited)
            self.rate_limited = rate_limited

        async def inner_text(self, timeout) -> str:
            if self.rate_limited:
                return "Too many requests\nWe've temporarily limited access to your conversations"
            return ""

    class Request:
        def __init__(self, reachable: bool) -> None:
            self.reachable = reachable

        async def get(self, _url, timeout) -> object:
            if not self.reachable:
                raise TimeoutError("proxy unavailable")
            return type("Response", (), {"status": 403})()

    def __init__(
        self,
        testid_visible: bool,
        fallback_visible: bool,
        network_reachable: bool = True,
        rate_limited: bool = False,
    ) -> None:
        self.testid_control = FakeControl(testid_visible)
        self.form = FakeComposerForm(FakeControl(fallback_visible))
        self.selectors = []
        self.request = self.Request(network_reachable)
        self.rate_limited = rate_limited

    def locator(self, selector):
        self.selectors.append(selector)
        if selector == "form":
            return self.form
        if selector == f'[data-testid="{RATE_LIMIT_MODAL_TESTID}"]:visible':
            return self.Dialog(self.rate_limited)
        if selector == '[role="dialog"], [aria-modal="true"]':
            return self.Dialog(self.rate_limited)
        return self.testid_control

    async def wait_for_timeout(self, _timeout) -> None:
        return None

    def get_by_role(self, role, name=None, exact=False):
        return FakeControl(
            self.rate_limited
            and role == "heading"
            and name == "Too many requests"
            and exact
        )


class HydratingLocator:
    def __init__(self, counts) -> None:
        self.counts = iter(counts)
        self.last = 0

    @property
    def first(self):
        return self

    def nth(self, _index):
        return self

    async def count(self) -> int:
        self.last = next(self.counts, self.last)
        return self.last

    async def wait_for(self, **_kwargs) -> None:
        return None

    async def is_visible(self) -> bool:
        return self.last > 0


class HydratingConversationPage:
    def __init__(self) -> None:
        self.url = ""
        self.user_messages = HydratingLocator([0, 0, 1])
        self.assistant_messages = HydratingLocator([0, 1, 1])
        self.composer = HydratingLocator([1])
        self.login = HydratingLocator([0])
        self.account = HydratingLocator([1])
        self.waits = 0

    async def goto(self, url, **_kwargs) -> None:
        self.url = url

    async def title(self) -> str:
        return "ChatGPT"

    async def inner_text(self, _selector) -> str:
        return ""

    def locator(self, selector):
        if selector == f'[data-testid="{RATE_LIMIT_MODAL_TESTID}"]:visible':
            return HydratingLocator([0])
        if selector == '[data-message-author-role="user"]':
            return self.user_messages
        if selector == '[data-message-author-role="assistant"]':
            return self.assistant_messages
        if selector == "#prompt-textarea":
            return self.composer
        if selector.startswith('[data-testid="accounts-profile-button"]'):
            return self.account
        raise AssertionError(selector)

    async def wait_for_timeout(self, _timeout) -> None:
        self.waits += 1

    def get_by_role(self, role, name=None, exact=False):
        if role == "heading":
            return HydratingLocator([0])
        if role == "button" and name is not None:
            return self.login
        return self.composer

    def get_by_text(self, _text, exact=False):
        return HydratingLocator([0 if exact else 0])


class CloudflareClearingPage:
    def __init__(self) -> None:
        self.titles = ["Just a moment...", "Just a moment...", "ChatGPT"]
        self.bodies = ["Verifying...", "Verifying...", "Welcome back"]
        self.index = 0

    async def title(self) -> str:
        return self.titles[min(self.index, len(self.titles) - 1)]

    async def inner_text(self, _selector) -> str:
        return self.bodies[min(self.index, len(self.bodies) - 1)]

    async def wait_for_timeout(self, _timeout) -> None:
        self.index += 1


class StuckCloudflarePage:
    async def title(self) -> str:
        return "Just a moment..."

    async def inner_text(self, _selector) -> str:
        return "Verify you are human"

    async def wait_for_timeout(self, _timeout) -> None:
        return None


class FakePickerButton:
    def __init__(self, text: str, visible: bool = True) -> None:
        self.text = text
        self.visible = visible
        self.clicked = False

    async def inner_text(self) -> str:
        return self.text

    async def is_visible(self) -> bool:
        return self.visible

    @property
    def first(self):
        return self

    async def count(self) -> int:
        return 1 if self.visible else 0

    async def click(self, **_kwargs) -> None:
        self.clicked = True


class FakePickerButtons:
    def __init__(self, buttons) -> None:
        self.buttons = buttons

    async def count(self) -> int:
        return len(self.buttons)

    def nth(self, index):
        return self.buttons[index]


class AccountPickerHeading:
    def __init__(self, page) -> None:
        self.page = page

    async def count(self) -> int:
        return 1 if self.page.heading_visible else 0

    @property
    def first(self):
        return self

    async def is_visible(self) -> bool:
        return self.page.heading_visible


class AccountPickerPage:
    def __init__(self, heading_visible: bool = True, picker_after: int = 0) -> None:
        self.heading_visible = heading_visible
        self.picker_after = picker_after
        self.waits = 0
        self.login = FakePickerButton("Log in")
        self.saved = FakePickerButton("Ada\nada@example.com")
        self.other = FakePickerButton("Log in to another account")

    def get_by_text(self, _text, exact=False):
        return AccountPickerHeading(self)

    def get_by_role(self, role, name=None, exact=False):
        if role != "button":
            raise AssertionError(role)
        if name is not None:
            return self.login
        return FakePickerButtons([self.login, self.saved, self.other])

    def locator(self, selector):
        if "library" in selector or "scheduled" in selector or "accounts-profile" in selector:
            if self.heading_visible:
                return FakePickerButtons([])
            return FakePickerButtons([FakePickerButton("Library")])
        raise AssertionError(selector)

    async def wait_for_timeout(self, _timeout) -> None:
        self.waits += 1
        if self.saved.clicked:
            self.heading_visible = False
            self.login.visible = False
        elif self.picker_after and self.waits >= self.picker_after:
            self.heading_visible = True


class ChatGptUiDriverTests(unittest.TestCase):
    def test_cloudflare_challenge_titles_are_classified(self) -> None:
        self.assertTrue(is_cloudflare_challenge_title("Just a moment..."))
        self.assertTrue(is_cloudflare_challenge_title("Verifying..."))
        self.assertFalse(is_cloudflare_challenge_title("ChatGPT"))
        self.assertFalse(is_cloudflare_challenge_title(None))

    def test_saved_account_choice_ignores_login_chrome(self) -> None:
        self.assertTrue(is_saved_account_choice("Ada\nada@example.com"))
        self.assertFalse(is_saved_account_choice("Log in"))
        self.assertFalse(is_saved_account_choice("Log in to another account"))
        self.assertFalse(is_saved_account_choice("Create account"))
        self.assertFalse(is_saved_account_choice("Sign up for free"))
        self.assertFalse(is_saved_account_choice("Remove account"))
        self.assertFalse(is_saved_account_choice(""))

    def test_cloudflare_wait_returns_once_the_interstitial_clears(self) -> None:
        page = CloudflareClearingPage()
        asyncio.run(wait_out_cloudflare(page, timeout_ms=2_000))
        self.assertGreaterEqual(page.index, 2)

    def test_cloudflare_wait_fails_closed_when_stuck(self) -> None:
        with self.assertRaises(TransportUnavailable):
            asyncio.run(wait_out_cloudflare(StuckCloudflarePage(), timeout_ms=1_000))

    def test_saved_account_picker_clicks_the_email_card_not_log_in(self) -> None:
        page = AccountPickerPage()
        selected = asyncio.run(complete_saved_account_picker(page))
        self.assertTrue(selected)
        self.assertTrue(page.saved.clicked)
        self.assertFalse(page.login.clicked)
        self.assertFalse(page.other.clicked)
        self.assertFalse(page.heading_visible)

    def test_saved_account_picker_waits_for_late_welcome_back_overlay(self) -> None:
        page = AccountPickerPage(heading_visible=False, picker_after=3)
        selected = asyncio.run(complete_saved_account_picker(page, timeout_ms=2_000))
        self.assertTrue(selected)
        self.assertGreaterEqual(page.waits, 3)
        self.assertTrue(page.saved.clicked)

    def test_explicit_rate_limit_heading_is_classified(self) -> None:
        page = FakeComposerPage(False, False, rate_limited=True)

        with self.assertRaises(RateLimited):
            asyncio.run(raise_if_rate_limited(page))

    def test_rate_limit_modal_testid_is_classified_before_picker_clicks(self) -> None:
        page = FakeComposerPage(False, False, rate_limited=True)

        with self.assertRaises(RateLimited):
            asyncio.run(raise_if_rate_limited(page))

        self.assertIn(
            f'[data-testid="{RATE_LIMIT_MODAL_TESTID}"]:visible', page.selectors
        )

    def test_cleanup_preserves_existing_protocol_result(self) -> None:
        asyncio.run(close_context_quietly(ClosedContext()))

    def test_continuation_waits_for_history_hydration(self) -> None:
        page = HydratingConversationPage()

        baseline = asyncio.run(
            establish_continued_chat(page, "/c/abc123def456")
        )

        self.assertEqual(baseline, 1)
        self.assertGreaterEqual(page.waits, 2)

    def test_pro_model_alias_maps_to_current_verified_picker_option(self) -> None:
        self.assertEqual(model_selection("gpt-5.6-pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("GPT-5.6 Pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("Extra High"), ("Extra High", "Extra High"))
        self.assertGreaterEqual(MODEL_PICKER_READY_TIMEOUT_MS, 45_000)

    def test_model_picker_waits_for_hydration_and_accepts_current_selection(
        self,
    ) -> None:
        button = SelectedModelButton()
        page = ComposerPage(SingleModelPicker(button))

        selected = asyncio.run(choose_intelligence_model(page, "Pro"))

        self.assertTrue(selected)
        self.assertEqual(
            page.selector,
            'form button[aria-haspopup="menu"]:visible',
        )
        self.assertEqual(
            button.wait_timeout, ("visible", MODEL_PICKER_READY_TIMEOUT_MS)
        )
        self.assertFalse(button.clicked)

    def test_composer_control_prefers_stable_test_ids(self) -> None:
        page = FakeComposerPage(testid_visible=True, fallback_visible=True)

        control, used_fallback = asyncio.run(
            locate_composer_control(page, SEND_CONTROL_TESTIDS, SEND_BUTTON_NAME)
        )

        self.assertIs(control, page.testid_control)
        self.assertFalse(used_fallback)
        self.assertNotIn("form", page.selectors)

    def test_composer_control_falls_back_to_the_composer_scoped_role(self) -> None:
        page = FakeComposerPage(testid_visible=False, fallback_visible=True)

        control, used_fallback = asyncio.run(
            locate_composer_control(page, STOP_CONTROL_TESTIDS, STOP_BUTTON_NAME)
        )

        self.assertIs(control, page.form.control)
        self.assertTrue(used_fallback)
        self.assertEqual(page.form.requested_role, "button")
        self.assertIs(page.form.requested_name, STOP_BUTTON_NAME)
        self.assertEqual(page.selectors[-1], "form")

    def test_composer_control_reports_absence_instead_of_guessing(self) -> None:
        page = FakeComposerPage(testid_visible=False, fallback_visible=False)

        control, used_fallback = asyncio.run(
            locate_composer_control(page, SEND_CONTROL_TESTIDS, SEND_BUTTON_NAME)
        )

        self.assertIsNone(control)
        self.assertFalse(used_fallback)

    def test_composer_control_names_are_anchored(self) -> None:
        self.assertIsNotNone(SEND_BUTTON_NAME.match("Send prompt"))
        self.assertIsNotNone(SEND_BUTTON_NAME.match("Send"))
        self.assertIsNone(SEND_BUTTON_NAME.match("Resend message"))
        self.assertIsNone(SEND_BUTTON_NAME.match("Sending options"))
        self.assertIsNotNone(STOP_BUTTON_NAME.match("Stop generating"))
        self.assertIsNone(STOP_BUTTON_NAME.match("Nonstop mode"))

    def test_send_click_re_resolves_once_after_hydration_replacement(self) -> None:
        page = FakeComposerPage(testid_visible=True, fallback_visible=False)
        page.testid_control = ClickableControl(failures=1)

        used_fallback = asyncio.run(click_send_control(page))

        self.assertFalse(used_fallback)
        self.assertEqual(len(page.testid_control.clicks), 4)
        self.assertTrue(page.testid_control.clicks[0]["trial"])
        self.assertTrue(page.testid_control.clicks[2]["trial"])
        self.assertTrue(page.testid_control.clicks[-1]["no_wait_after"])

    def test_send_click_never_forces_or_uses_keyboard_fallback(self) -> None:
        page = FakeComposerPage(testid_visible=True, fallback_visible=False)
        page.testid_control = ClickableControl(failures=2)

        with self.assertRaisesRegex(RuntimeError, "not actionable"):
            asyncio.run(click_send_control(page))

        self.assertTrue(page.testid_control.clicks)
        self.assertTrue(
            all(not click.get("force", False) for click in page.testid_control.clicks)
        )

    def test_send_click_does_not_misclassify_context_probe_as_transport(self) -> None:
        page = FakeComposerPage(
            testid_visible=True,
            fallback_visible=False,
            network_reachable=False,
        )
        page.testid_control = ClickableControl(failures=2)

        with self.assertRaisesRegex(RuntimeError, "not actionable"):
            asyncio.run(click_send_control(page))

    def test_download_links_are_limited_to_chatgpt_artifact_surfaces(self) -> None:
        self.assertTrue(downloadable_href("sandbox:/mnt/data/report.pdf"))
        self.assertTrue(
            downloadable_href("https://files.oaiusercontent.com/file-123/report.pdf")
        )
        self.assertTrue(downloadable_href("/backend-api/files/file-123"))
        self.assertFalse(downloadable_href("https://example.com/report.pdf"))
        self.assertFalse(downloadable_href("https://chatgpt.com/share/example"))

    def test_download_filename_cannot_escape_the_artifact_directory(self) -> None:
        self.assertEqual(safe_download_name("../../report.pdf", 1), "report.pdf")
        self.assertEqual(safe_download_name('bad"name.txt', 1), "bad_name.txt")

    def test_current_artifact_button_is_narrowly_identified(self) -> None:
        self.assertEqual(
            download_control_key(
                "BUTTON",
                None,
                "report.pdf",
                "behavior-btn entity-underline",
                "report.pdf",
            ),
            "button:report.pdf",
        )
        self.assertIsNone(
            download_control_key(
                "BUTTON", None, "Open citation", "behavior-btn", "Open citation"
            )
        )
        self.assertIsNone(
            download_control_key("BUTTON", None, "report.pdf", "generic", "report.pdf")
        )

    def test_conversation_path_is_only_extracted_from_chatgpt_routes(self) -> None:
        self.assertEqual(
            conversation_path_from_url("https://chatgpt.com/c/abc123def456"),
            "/c/abc123def456",
        )
        self.assertEqual(
            conversation_path_from_url(
                "https://chat.openai.com/c/0a1b2c3d-4e5f-6789-abcd-ef0123456789"
            ),
            "/c/0a1b2c3d-4e5f-6789-abcd-ef0123456789",
        )
        # Non-conversation routes, foreign hosts, and short ids never become
        # durability pointers.
        self.assertIsNone(conversation_path_from_url("https://chatgpt.com/"))
        self.assertIsNone(conversation_path_from_url("https://chatgpt.com/c/short"))
        self.assertIsNone(
            conversation_path_from_url("https://evil.example/c/abc123def456")
        )
        self.assertIsNone(
            conversation_path_from_url("https://chatgpt.com/share/abc123def456")
        )
        self.assertIsNone(
            conversation_path_from_url("https://chatgpt.com/c/abc123def456/extra")
        )

    def test_resume_requests_only_accept_opaque_conversation_routes(self) -> None:
        self.assertEqual(
            resume_conversation_path({"conversation_path": "/c/abc123def456"}),
            "/c/abc123def456",
        )
        self.assertIsNone(resume_conversation_path(None))
        self.assertIsNone(resume_conversation_path("/c/abc123def456"))
        self.assertIsNone(resume_conversation_path({"conversation_path": "/settings"}))
        self.assertIsNone(
            resume_conversation_path({"conversation_path": "/c/../../etc/passwd"})
        )
        self.assertIsNone(
            resume_conversation_path(
                {"conversation_path": "https://chatgpt.com/c/abc123def456"}
            )
        )
        self.assertEqual(
            continued_conversation_path({"conversation_path": "/c/abc123def456"}),
            "/c/abc123def456",
        )

    def test_prompt_identity_is_whitespace_insensitive_and_in_memory_only(
        self,
    ) -> None:
        self.assertEqual(
            normalized_prompt("hello\n  world\t"), normalized_prompt(" hello world")
        )
        self.assertNotEqual(normalized_prompt("hello"), normalized_prompt("hello!"))


if __name__ == "__main__":
    unittest.main()
