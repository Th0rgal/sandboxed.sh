#!/usr/bin/env python3
"""Deterministic chatgpt_ui protocol fixture; never starts a browser."""

import argparse
import json
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("--profile-dir", required=True)
parser.add_argument("--browser")
parser.add_argument("--headless")
parser.parse_args()
request = json.loads(sys.stdin.readline())
message = request.get("message", "")

def emit(kind, **data):
    print(json.dumps({"type": kind, **data}), flush=True)

emit("diagnostic", message="compatibility=mock-v1")
if message == "__error__":
    emit("error", code="auth_required", message="deterministic auth failure")
elif message == "__timeout__":
    time.sleep(60)
elif message == "__partial__":
    emit("text_delta", content="partial response must not succeed")
elif message == "__tools__":
    emit("tool_call", id="mock-tool", name="mock", args={"safe": True})
    emit("tool_result", id="mock-tool", name="mock", result={"ok": True})
    emit("complete", content="mock tool response", model="mock-model")
else:
    response = f"mock response: {message}"
    emit("text_delta", content=response[: max(1, len(response) // 2)])
    emit("text_delta", content=response)
    emit("complete", content=response, model=request.get("model") or "mock-model")
