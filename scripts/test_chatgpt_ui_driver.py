#!/usr/bin/env python3

import asyncio
import unittest

from scripts.chatgpt_ui_driver import (
    MODEL_PICKER_READY_TIMEOUT_MS,
    SEND_BUTTON_NAME,
    SEND_CONTROL_TESTIDS,
    STOP_BUTTON_NAME,
    STOP_CONTROL_TESTIDS,
    choose_intelligence_model,
    close_context_quietly,
    conversation_path_from_url,
    download_control_key,
    downloadable_href,
    locate_composer_control,
    model_selection,
    normalized_prompt,
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

    async def count(self) -> int:
        return 1 if self.visible else 0

    async def is_visible(self) -> bool:
        return self.visible


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
    def __init__(self, testid_visible: bool, fallback_visible: bool) -> None:
        self.testid_control = FakeControl(testid_visible)
        self.form = FakeComposerForm(FakeControl(fallback_visible))
        self.selectors = []

    def locator(self, selector):
        self.selectors.append(selector)
        if selector == "form":
            return self.form
        return self.testid_control


class ChatGptUiDriverTests(unittest.TestCase):
    def test_cleanup_preserves_existing_protocol_result(self) -> None:
        asyncio.run(close_context_quietly(ClosedContext()))

    def test_pro_model_alias_maps_to_current_verified_picker_option(self) -> None:
        self.assertEqual(model_selection("gpt-5.6-pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("GPT-5.6 Pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("Extra High"), ("Extra High", "Extra High"))
        self.assertGreaterEqual(MODEL_PICKER_READY_TIMEOUT_MS, 10_000)

    def test_model_picker_waits_for_hydration_and_accepts_current_selection(
        self,
    ) -> None:
        button = SelectedModelButton()
        page = ComposerPage(SingleModelPicker(button))

        selected = asyncio.run(choose_intelligence_model(page, "Pro"))

        self.assertTrue(selected)
        self.assertEqual(
            page.selector,
            'form button.__composer-pill[aria-haspopup="menu"]:visible',
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

    def test_prompt_identity_is_whitespace_insensitive_and_in_memory_only(
        self,
    ) -> None:
        self.assertEqual(
            normalized_prompt("hello\n  world\t"), normalized_prompt(" hello world")
        )
        self.assertNotEqual(normalized_prompt("hello"), normalized_prompt("hello!"))


if __name__ == "__main__":
    unittest.main()
