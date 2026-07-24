#!/usr/bin/env python3
"""Deterministic protocol tests; no browser or profile contents are accessed."""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

DRIVER = Path(__file__).with_name("chatgpt_ui_mock_driver.py")


def run_driver(message):
    with tempfile.TemporaryDirectory() as profile:
        request = json.dumps({"message": message, "model": None}) + "\n"
        result = subprocess.run(
            ["python3", str(DRIVER), "--profile-dir", profile],
            input=request,
            text=True,
            capture_output=True,
            timeout=5,
            check=True,
        )
    return [json.loads(line) for line in result.stdout.splitlines()]


class MockDriverProtocolTests(unittest.TestCase):
    def test_success_requires_explicit_complete(self):
        events = run_driver("hello")
        self.assertEqual(events[-1]["type"], "complete")
        self.assertEqual(events[-1]["content"], "mock response: hello")

    def test_partial_stream_has_no_complete(self):
        events = run_driver("__partial__")
        self.assertEqual([event["type"] for event in events], ["diagnostic", "text_delta"])

    def test_auth_failure_is_typed(self):
        events = run_driver("__error__")
        self.assertEqual(events[-1]["code"], "auth_required")

    def test_tool_events_are_balanced(self):
        events = run_driver("__tools__")
        self.assertEqual(
            [event["type"] for event in events],
            ["diagnostic", "tool_call", "tool_result", "complete"],
        )


if __name__ == "__main__":
    unittest.main()
