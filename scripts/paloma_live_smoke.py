#!/usr/bin/env python3
"""Run Paloma live-smoke checks from a real Telegram user account.

This script is intentionally client-side: it uses Telethon with the user's
Telegram session and treats the bot like a real user would. It can verify a
deployed bot, or a local checkout once its Telegram channel/webhook points at
the running local server.

Required env is the same as scripts/telegram_user_smoke.py:
  TELEGRAM_API_ID, TELEGRAM_API_HASH, TELEGRAM_PHONE, TELEGRAM_SESSION

Typical usage:
  TELEGRAM_CHAT=ana_lfgbot python3 scripts/paloma_live_smoke.py --dm-chat ana_lfgbot

For alert-reply feedback checks, pass --alert-message-id with a recent Paloma
alert message in the DM.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import re
import sqlite3
import sys
import time
from dataclasses import dataclass, field
from getpass import getpass
from pathlib import Path
from typing import Optional
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

try:
    from telethon import TelegramClient, events
    from telethon.errors import SessionPasswordNeededError
except ImportError as exc:
    TelegramClient = None  # type: ignore[assignment]
    events = None  # type: ignore[assignment]
    SessionPasswordNeededError = None  # type: ignore[assignment]
    TELETHON_IMPORT_ERROR = exc
else:
    TELETHON_IMPORT_ERROR = None


def env_or(name: str, default: Optional[str] = None) -> Optional[str]:
    value = os.environ.get(name)
    return value if value else default


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    sys.exit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run Paloma Telegram live-smoke checks from a real user account.",
    )
    parser.add_argument("--api-id", type=int, default=int(env_or("TELEGRAM_API_ID", "0") or "0"))
    parser.add_argument("--api-hash", default=env_or("TELEGRAM_API_HASH"))
    parser.add_argument("--phone", default=env_or("TELEGRAM_PHONE"))
    parser.add_argument(
        "--session",
        default=env_or(
            "TELEGRAM_SESSION",
            str(Path.home() / ".cache" / "sandboxed-sh" / "telegram-user-smoke"),
        ),
    )
    parser.add_argument("--dm-chat", default=env_or("TELEGRAM_CHAT", "ana_lfgbot"))
    parser.add_argument("--bot-username", default=env_or("TELEGRAM_FROM_USER", "ana_lfgbot"))
    parser.add_argument("--shared-chat", default=env_or("PALOMA_SHARED_CHAT"))
    parser.add_argument("--alert-message-id", type=int, default=None)
    parser.add_argument("--watch-seconds", type=int, default=int(env_or("TELEGRAM_WATCH_SECONDS", "45") or "45"))
    parser.add_argument(
        "--preflight-only",
        action="store_true",
        help="Check local prerequisites without connecting to Telegram or sending messages.",
    )
    parser.add_argument(
        "--mission-db",
        default=env_or("PALOMA_MISSION_DB", "/root/.sandboxed-sh/missions/missions-dev.db"),
        help="SQLite mission DB to inspect for configured Telegram channels during preflight.",
    )
    parser.add_argument(
        "--api-base",
        default=env_or("PALOMA_API_BASE", "http://127.0.0.1:3000"),
        help="Sandboxed.sh API base URL to probe during preflight.",
    )
    parser.add_argument(
        "--api-token",
        default=env_or("PALOMA_API_TOKEN"),
        help="Optional API bearer token for authenticated control-endpoint preflight probes.",
    )
    parser.add_argument("--code", default=None)
    parser.add_argument("--password", default=None)
    parser.add_argument(
        "--skip-shared",
        action="store_true",
        help="Skip shared-chat /summary silence/allowance checks.",
    )
    parser.add_argument(
        "--skip-alert-feedback",
        action="store_true",
        help="Skip reply-to-alert feedback checks.",
    )
    return parser.parse_args()


def http_get_text(url: str, token: Optional[str] = None) -> tuple[int, str]:
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = Request(url, headers=headers)
    try:
        with urlopen(request, timeout=10) as response:
            body = response.read(2048).decode("utf-8", errors="replace")
            return response.status, body
    except HTTPError as exc:
        body = exc.read(2048).decode("utf-8", errors="replace")
        return exc.code, body
    except URLError as exc:
        return 0, str(exc)


def count_configured_telegram_channels(db_path: str) -> tuple[Optional[int], str]:
    path = Path(db_path).expanduser()
    if not path.exists():
        return None, f"mission DB not found: {path}"
    try:
        with sqlite3.connect(path) as conn:
            table_count = conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='telegram_channels'",
            ).fetchone()[0]
            if table_count == 0:
                return None, "telegram_channels table missing"
            channel_count = conn.execute("SELECT COUNT(*) FROM telegram_channels").fetchone()[0]
            return int(channel_count), "ok"
    except sqlite3.Error as exc:
        return None, f"sqlite error: {exc}"


def run_preflight(args: argparse.Namespace) -> list[CheckResult]:
    results: list[CheckResult] = []
    results.append(CheckResult("telegram_api_id", bool(args.api_id), "set" if args.api_id else "missing"))
    results.append(CheckResult("telegram_api_hash", bool(args.api_hash), "set" if args.api_hash else "missing"))

    session_path = Path(args.session).expanduser()
    session_file = session_path if session_path.suffix == ".session" else Path(f"{session_path}.session")
    results.append(
        CheckResult(
            "telegram_session_file",
            session_file.exists(),
            str(session_file) if session_file.exists() else f"missing: {session_file}",
        )
    )

    channel_count, channel_detail = count_configured_telegram_channels(args.mission_db)
    results.append(
        CheckResult(
            "local_telegram_channels",
            bool(channel_count and channel_count > 0),
            f"count={channel_count}" if channel_count is not None else channel_detail,
        )
    )

    api_base = args.api_base.rstrip("/")
    health_status, health_body = http_get_text(f"{api_base}/api/health")
    results.append(
        CheckResult(
            "api_health",
            health_status == 200 and '"status":"ok"' in health_body,
            f"status={health_status}",
        )
    )

    if args.api_token:
        control_status, control_body = http_get_text(f"{api_base}/api/control/telegram/bots", args.api_token)
        results.append(
            CheckResult(
                "api_control_auth",
                control_status == 200,
                f"status={control_status}, body={control_body[:80]!r}",
            )
        )
    else:
        results.append(CheckResult("api_control_auth", False, "missing PALOMA_API_TOKEN/--api-token"))

    return results


def looks_numeric_chat(raw: str) -> bool:
    return bool(raw) and (raw.isdigit() or (raw.startswith("-") and raw[1:].isdigit()))


def normalize_username(username: Optional[str]) -> Optional[str]:
    return username.lstrip("@").lower() if username else None


async def ensure_authorized(
    client: TelegramClient,
    phone: Optional[str],
    code: Optional[str],
    password: Optional[str],
) -> None:
    await client.connect()
    if await client.is_user_authorized():
        return
    if not phone:
        die("Phone number is required for first-time authorization.")
    sent = await client.send_code_request(phone)
    login_code = code or input("Telegram login code: ").strip()
    try:
        await client.sign_in(phone=phone, code=login_code, phone_code_hash=sent.phone_code_hash)
    except SessionPasswordNeededError:
        login_password = password or getpass("Telegram 2FA password: ")
        await client.sign_in(password=login_password)


async def resolve_chat(client: TelegramClient, raw_chat: str):
    if not raw_chat:
        die("Chat is required.")
    target = int(raw_chat) if looks_numeric_chat(raw_chat) else raw_chat
    try:
        return await client.get_entity(target)
    except Exception as exc:
        die(f"Failed to resolve chat '{raw_chat}': {exc}")


@dataclass
class CheckResult:
    name: str
    passed: bool
    detail: str = ""


@dataclass
class BotWatcher:
    client: TelegramClient
    entity: object
    bot_username: str
    messages: list[tuple[int, str]] = field(default_factory=list)

    async def __aenter__(self):
        target_username = normalize_username(self.bot_username)

        @self.client.on(events.NewMessage(chats=self.entity))
        async def handler(event) -> None:
            sender = await event.get_sender()
            username = normalize_username(getattr(sender, "username", None))
            if target_username and username != target_username:
                return
            text = (event.raw_text or "").strip()
            self.messages.append((event.message.id, text))
            print(f"[bot:{event.message.id}] {text}")

        self._handler = handler
        return self

    async def __aexit__(self, exc_type, exc, tb):
        self.client.remove_event_handler(self._handler)

    async def wait_for(self, pattern: str, seconds: int, after_count: int = 0) -> Optional[tuple[int, str]]:
        compiled = re.compile(pattern, re.I | re.S)
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            for message_id, text in self.messages[after_count:]:
                if compiled.search(text):
                    return message_id, text
            await asyncio.sleep(0.5)
        return None

    def count(self) -> int:
        return len(self.messages)


async def send_and_expect(
    client: TelegramClient,
    watcher: BotWatcher,
    entity,
    name: str,
    text: str,
    expect: str,
    watch_seconds: int,
    reply_to: Optional[int] = None,
) -> CheckResult:
    before = watcher.count()
    sent = await client.send_message(entity, text, reply_to=reply_to)
    print(f"[sent:{sent.id}] {text}")
    match = await watcher.wait_for(expect, watch_seconds, before)
    if match:
        return CheckResult(name, True, f"reply_id={match[0]}")
    return CheckResult(name, False, f"no bot reply matching {expect!r}")


async def send_and_expect_silence(
    client: TelegramClient,
    watcher: BotWatcher,
    entity,
    name: str,
    text: str,
    watch_seconds: int,
) -> CheckResult:
    before = watcher.count()
    sent = await client.send_message(entity, text)
    print(f"[sent:{sent.id}] {text}")
    await asyncio.sleep(watch_seconds)
    after = watcher.count()
    if after == before:
        return CheckResult(name, True, "no bot reply")
    return CheckResult(name, False, f"unexpected bot replies={after - before}")


async def run_smoke(args: argparse.Namespace) -> list[CheckResult]:
    if TELETHON_IMPORT_ERROR is not None:
        die(
            "Telethon is not installed. Install it with:\n"
            "  python3 -m pip install telethon"
        )
    if not args.api_id:
        die("Missing Telegram API ID. Set TELEGRAM_API_ID or pass --api-id.")
    if not args.api_hash:
        die("Missing Telegram API hash. Set TELEGRAM_API_HASH or pass --api-hash.")

    session_path = Path(args.session).expanduser()
    session_path.parent.mkdir(parents=True, exist_ok=True)
    client = TelegramClient(str(session_path), args.api_id, args.api_hash)
    results: list[CheckResult] = []

    try:
        await ensure_authorized(client, args.phone, args.code, args.password)
        me = await client.get_me()
        print(f"Authorized as @{normalize_username(getattr(me, 'username', None)) or 'unknown'}")

        dm = await resolve_chat(client, args.dm_chat)
        async with BotWatcher(client, dm, args.bot_username) as watcher:
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "dm_status",
                    "/status",
                    r"(meaningful changes|no meaningful changes|mission|status)",
                    args.watch_seconds,
                )
            )
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "dm_missions",
                    "/missions",
                    r"(mission|active|running|no active)",
                    args.watch_seconds,
                )
            )
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "dm_summary",
                    "/summary",
                    r"(summary|mission|no mission|couldn't|cannot|failed|active|awaiting|completed|interrupted|blocked|not feasible)",
                    args.watch_seconds,
                )
            )
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "dm_why",
                    "/why",
                    r"(decision|paloma|why|no recent)",
                    args.watch_seconds,
                )
            )
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "usage_approve",
                    "/approve",
                    r"usage: /approve",
                    args.watch_seconds,
                )
            )
            results.append(
                await send_and_expect(
                    client,
                    watcher,
                    dm,
                    "usage_send",
                    "/send latest",
                    r"usage: /send",
                    args.watch_seconds,
                )
            )

            burst_before = watcher.count()
            burst_messages = ["status", "what changed since my last check?", "/missions"]
            for text in burst_messages:
                sent = await client.send_message(dm, text)
                print(f"[sent:{sent.id}] {text}")
            deadline = time.monotonic() + args.watch_seconds
            while time.monotonic() < deadline and watcher.count() - burst_before < 3:
                await asyncio.sleep(0.5)
            results.append(
                CheckResult(
                    "burst_serialization",
                    watcher.count() - burst_before >= 3,
                    f"bot_replies={watcher.count() - burst_before}",
                )
            )

            if args.skip_alert_feedback:
                results.append(CheckResult("alert_feedback", True, "skipped"))
            elif args.alert_message_id:
                feedback_cases = [
                    ("feedback_high_interest", "keep me posted on this", r"(keep|posted|updates|watch)"),
                    ("feedback_failure_only", "only tell me if this fails", r"(fail|failure|only)"),
                    ("feedback_mute", "mute this", r"(mute|muted|quiet)"),
                    ("feedback_restore_high_interest", "keep me posted on this", r"(keep|posted|updates|watch)"),
                ]
                for name, text, expect in feedback_cases:
                    results.append(
                        await send_and_expect(
                            client,
                            watcher,
                            dm,
                            name,
                            text,
                            expect,
                            args.watch_seconds,
                            reply_to=args.alert_message_id,
                        )
                    )
            else:
                results.append(
                    CheckResult(
                        "alert_feedback",
                        False,
                        "missing --alert-message-id for reply-to-alert feedback checks",
                    )
                )

        if not args.skip_shared:
            if not args.shared_chat:
                results.append(CheckResult("shared_summary", False, "missing --shared-chat"))
            else:
                shared = await resolve_chat(client, args.shared_chat)
                async with BotWatcher(client, shared, args.bot_username) as shared_watcher:
                    results.append(
                        await send_and_expect_silence(
                            client,
                            shared_watcher,
                            shared,
                            "shared_plain_summary_silence",
                            "/summary",
                            max(5, min(args.watch_seconds, 15)),
                        )
                    )
                    results.append(
                        await send_and_expect_silence(
                            client,
                            shared_watcher,
                            shared,
                            "shared_mention_summary_silence",
                            f"@{args.bot_username.lstrip('@')} /summary",
                            max(5, min(args.watch_seconds, 15)),
                        )
                    )
        else:
            results.append(CheckResult("shared_summary", True, "skipped"))

        return results
    finally:
        await client.disconnect()


def main() -> int:
    args = parse_args()
    if args.preflight_only:
        results = run_preflight(args)
        heading = "Paloma live smoke preflight results:"
    else:
        results = asyncio.run(run_smoke(args))
        heading = "Paloma live smoke results:"
    print(f"\n{heading}")
    failed = []
    for result in results:
        status = "PASS" if result.passed else "FAIL"
        print(f"  {status:<4} {result.name}: {result.detail}")
        if not result.passed:
            failed.append(result)
    if failed:
        print("\nMissing evidence:")
        for result in failed:
            print(f"  - {result.name}: {result.detail}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
