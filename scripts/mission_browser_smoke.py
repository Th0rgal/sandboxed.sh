#!/usr/bin/env python3
"""Browser task smoke test via Mission API.

Creates missions, sends a browser/desktop task, and verifies streaming includes
browser-related tool calls/results plus a final assistant message.
"""

import argparse
import json
import os
import socket
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import List, Optional, Set

DEFAULT_BACKENDS = ["claudecode", "codex"]
REQUIRED_TOOL_PREFIXES = (
    "desktop_",
    "playwright_",
    "browser_",
    "mcp__desktop__",
    "mcp__playwright__",
)


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    sys.exit(1)


def env_or(name: str, default: Optional[str] = None) -> Optional[str]:
    value = os.environ.get(name)
    if value:
        return value
    return default


def http_json(method: str, url: str, token: str, payload: Optional[dict]) -> dict:
    data = None
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Content-Type", "application/json")
    req.add_header(
        "User-Agent",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = resp.read().decode("utf-8")
            if not body:
                return {}
            return json.loads(body)
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8")
        raise RuntimeError(f"HTTP {exc.code} from {url}: {body}")


def open_sse_stream(url: str, token: str, timeout: float) -> urllib.response.addinfourl:
    req = urllib.request.Request(url, method="GET")
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", "text/event-stream")
    req.add_header(
        "User-Agent",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    )
    return urllib.request.urlopen(req, timeout=timeout)


def parse_sse(stream, on_event, stop_event: threading.Event, timeout: float) -> None:
    event_type = None
    data_lines: List[str] = []
    while not stop_event.is_set():
        try:
            line = stream.readline()
        except socket.timeout:
            continue
        if not line:
            break
        decoded = line.decode("utf-8").rstrip("\n")
        if decoded.startswith(":"):
            continue
        if decoded == "":
            if event_type and data_lines:
                data = "\n".join(data_lines)
                on_event(event_type, data)
            event_type = None
            data_lines = []
            continue
        if decoded.startswith("event:"):
            event_type = decoded[len("event:") :].strip()
            continue
        if decoded.startswith("data:"):
            data_lines.append(decoded[len("data:") :].strip())


@dataclass
class StreamStats:
    assistant_messages: int = 0
    tool_calls: int = 0
    tool_results: int = 0
    tool_names: Set[str] = field(default_factory=set)
    errors: List[str] = field(default_factory=list)

    def saw_required_tool(self) -> bool:
        return any(name.startswith(REQUIRED_TOOL_PREFIXES) for name in self.tool_names)

    def ok(self) -> bool:
        return (
            self.assistant_messages >= 1
            and self.tool_calls >= 1
            and self.tool_results >= 1
            and self.saw_required_tool()
        )


class StreamWatcher(threading.Thread):
    def __init__(
        self,
        base_url: str,
        token: str,
        mission_id: str,
        stats: StreamStats,
        stop_event: threading.Event,
        timeout: float,
        verbose: bool,
    ) -> None:
        super().__init__(daemon=True)
        self.base_url = base_url
        self.token = token
        self.mission_id = mission_id
        self.stats = stats
        self.stop_event = stop_event
        self.timeout = timeout
        self.verbose = verbose

    def run(self) -> None:
        deadline = time.time() + self.timeout
        while not self.stop_event.is_set() and time.time() < deadline:
            try:
                with open_sse_stream(
                    f"{self.base_url}/api/control/stream",
                    self.token,
                    timeout=5,
                ) as stream:
                    parse_sse(stream, self.on_event, self.stop_event, timeout=5)
            except Exception as exc:
                self.stats.errors.append(str(exc))
                time.sleep(1)

    def on_event(self, event_type: str, raw_data: str) -> None:
        try:
            payload = json.loads(raw_data)
        except json.JSONDecodeError:
            return
        mission_id = payload.get("mission_id")
        if mission_id and mission_id != self.mission_id:
            return

        if event_type == "assistant_message":
            self.stats.assistant_messages += 1
        elif event_type == "tool_call":
            self.stats.tool_calls += 1
            name = payload.get("name")
            if name:
                self.stats.tool_names.add(name)
        elif event_type == "tool_result":
            self.stats.tool_results += 1

        if self.verbose:
            print(f"[{self.mission_id}] {event_type}: {raw_data}")


@dataclass
class MissionResult:
    backend: str
    mission_id: str
    stats: StreamStats
    assistant_text: str


def fetch_latest_assistant_text(base_url: str, token: str, mission_id: str) -> str:
    events = http_json(
        "GET",
        f"{base_url}/api/control/missions/{mission_id}/events?types=assistant_message&limit=50&offset=0",
        token,
        None,
    )
    if not isinstance(events, list) or not events:
        return ""
    # Pick highest sequence
    latest = max(events, key=lambda e: e.get("sequence", 0))
    return latest.get("content", "")


def run_backend(
    base_url: str,
    token: str,
    workspace_id: str,
    backend: str,
    timeout: float,
    verbose: bool,
) -> MissionResult:
    title = f"browser-smoke-{backend}-{int(time.time())}"
    payload = {
        "title": title,
        "workspace_id": workspace_id,
        "backend": backend,
    }
    mission = http_json("POST", f"{base_url}/api/control/missions", token, payload)
    mission_id = mission.get("id")
    if not mission_id:
        raise RuntimeError(f"Failed to create mission for {backend}: {mission}")

    http_json("POST", f"{base_url}/api/control/missions/{mission_id}/load", token, {})

    stats = StreamStats()
    stop_event = threading.Event()
    watcher = StreamWatcher(
        base_url,
        token,
        mission_id,
        stats,
        stop_event,
        timeout,
        verbose,
    )
    watcher.start()

    message = (
        "Use desktop tools (not bash) to open a browser. "
        "Steps: "
        "1) desktop_start_session with launch_browser=true and url=https://example.com "
        "2) desktop_screenshot (wait_seconds=2) "
        "3) desktop_get_text to read the page title text. "
        "Then reply with 'title: Example Domain' and stop the session with desktop_stop_session."
    )
    http_json("POST", f"{base_url}/api/control/message", token, {"content": message})

    deadline = time.time() + timeout
    while time.time() < deadline:
        if stats.ok():
            break
        time.sleep(0.5)

    stop_event.set()
    watcher.join(timeout=5)

    assistant_text = fetch_latest_assistant_text(base_url, token, mission_id)

    return MissionResult(
        backend=backend,
        mission_id=mission_id,
        stats=stats,
        assistant_text=assistant_text,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Smoke test browser task via Mission API")
    parser.add_argument(
        "--base-url",
        default=env_or("SANDBOXED_SH_DEV_URL"),
        help="Base URL for the backend (env: SANDBOXED_SH_DEV_URL)",
    )
    parser.add_argument(
        "--token",
        default=env_or("SANDBOXED_SH_TOKEN"),
        help="Auth token (env: SANDBOXED_SH_TOKEN)",
    )
    parser.add_argument(
        "--workspace-id",
        default=env_or("SANDBOXED_SH_WORKSPACE_ID"),
        help="Workspace UUID (env: SANDBOXED_SH_WORKSPACE_ID)",
    )
    parser.add_argument(
        "--backend",
        action="append",
        help="Backend to test (repeatable). Defaults to claudecode/codex.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=240,
        help="Seconds to wait per backend.",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print streaming events.",
    )
    parser.add_argument(
        "--allow-nonexample",
        action="store_true",
        help="Do not require 'Example Domain' in the assistant response.",
    )
    args = parser.parse_args()

    if not args.base_url:
        die("Missing --base-url or SANDBOXED_SH_DEV_URL")
    if not args.token:
        die("Missing --token or SANDBOXED_SH_TOKEN")
    if not args.workspace_id:
        die("Missing --workspace-id or SANDBOXED_SH_WORKSPACE_ID")

    base_url = args.base_url.rstrip("/")
    token = args.token
    workspace_id = args.workspace_id
    backends = args.backend or DEFAULT_BACKENDS

    results: List[MissionResult] = []
    failed = False

    for backend in backends:
        print(f"\n== Running browser task test for {backend} ==")
        try:
            result = run_backend(
                base_url=base_url,
                token=token,
                workspace_id=workspace_id,
                backend=backend,
                timeout=args.timeout,
                verbose=args.verbose,
            )
            results.append(result)
        except Exception as exc:
            failed = True
            print(f"{backend}: FAILED to run: {exc}")
            continue

    for result in results:
        stats = result.stats
        ok = stats.ok()
        if not args.allow_nonexample:
            if "example domain" not in result.assistant_text.lower():
                ok = False
        if not ok:
            failed = True
        print(
            f"{result.backend}: "
            f"assistant_messages={stats.assistant_messages} "
            f"tool_calls={stats.tool_calls} "
            f"tool_results={stats.tool_results} "
            f"browser_tools={stats.saw_required_tool()} "
            f"errors={len(stats.errors)} "
            f"example_domain={'yes' if 'example domain' in result.assistant_text.lower() else 'no'} "
            f"=> {'OK' if ok else 'FAIL'}"
        )

    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
