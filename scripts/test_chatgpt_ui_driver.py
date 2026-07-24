#!/usr/bin/env python3

import asyncio
import unittest

from scripts.chatgpt_ui_driver import (
    close_context_quietly,
    download_control_key,
    downloadable_href,
    model_selection,
    safe_download_name,
)


class ClosedContext:
    async def close(self) -> None:
        raise RuntimeError("already closed")


class ChatGptUiDriverTests(unittest.TestCase):
    def test_cleanup_preserves_existing_protocol_result(self) -> None:
        asyncio.run(close_context_quietly(ClosedContext()))

    def test_pro_model_alias_maps_to_current_verified_picker_option(self) -> None:
        self.assertEqual(model_selection("gpt-5.6-pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("GPT-5.6 Pro"), ("Pro", "gpt-5.6-pro"))
        self.assertEqual(model_selection("Extra High"), ("Extra High", "Extra High"))

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
