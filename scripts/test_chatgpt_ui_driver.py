#!/usr/bin/env python3

import asyncio
import unittest

from scripts.chatgpt_ui_driver import (
    MODEL_PICKER_READY_TIMEOUT_MS,
    choose_intelligence_model,
    close_context_quietly,
    download_control_key,
    downloadable_href,
    model_selection,
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


if __name__ == "__main__":
    unittest.main()
