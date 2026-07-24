#!/usr/bin/env python3

import asyncio
import unittest

from scripts.chatgpt_ui_driver import close_context_quietly


class ClosedContext:
    async def close(self) -> None:
        raise RuntimeError("already closed")


class ChatGptUiDriverTests(unittest.TestCase):
    def test_cleanup_preserves_existing_protocol_result(self) -> None:
        asyncio.run(close_context_quietly(ClosedContext()))


if __name__ == "__main__":
    unittest.main()
