#!/usr/bin/env python3
"""User-account Telegram smoke helper for live bot testing.

This script uses a real Telegram user account via Telethon so we can test a bot
from the client side without depending on a bot token or webhook injection.

Typical flow:
1. Set TELEGRAM_API_ID / TELEGRAM_API_HASH from https://my.telegram.org
2. Run the script once to authorize the session with your phone number
3. Send a message to a target chat and optionally watch for Paloma's reply
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
import time
from getpass import getpass
from pathlib import Path
from typing import Optional

try:
    from telethon import TelegramClient, events
    from telethon.errors import SessionPasswordNeededError
except ImportError:
    print(
        "ERROR: Telethon is not installed. Install it with:\n"
        "  python3 -m pip install telethon",
        file=sys.stderr,
    )
    sys.exit(1)


def env_or(name: str, default: Optional[str] = None) -> Optional[str]:
    value = os.environ.get(name)
    if value:
        return value
    return default


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    sys.exit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Authenticate a Telegram user session, send a test message, and watch replies.",
    )
    parser.add_argument(
        "--api-id",
        type=int,
        default=int(env_or("TELEGRAM_API_ID", "0") or "0"),
        help="Telegram API ID from my.telegram.org (or TELEGRAM_API_ID).",
    )
    parser.add_argument(
        "--api-hash",
        default=env_or("TELEGRAM_API_HASH"),
        help="Telegram API hash from my.telegram.org (or TELEGRAM_API_HASH).",
    )
    parser.add_argument(
        "--phone",
        default=env_or("TELEGRAM_PHONE"),
        help="Phone number for the Telegram account, in international format.",
    )
    parser.add_argument(
        "--session",
        default=env_or(
            "TELEGRAM_SESSION",
            str(Path.home() / ".cache" / "sandboxed-sh" / "telegram-user-smoke"),
        ),
        help="Path to the Telethon session file.",
    )
    parser.add_argument(
        "--chat",
        default=env_or("TELEGRAM_CHAT"),
        help="Target chat: username, invite title already in dialogs, or numeric chat ID.",
    )
    parser.add_argument(
        "--send",
        default=None,
        help="Message text to send.",
    )
    parser.add_argument(
        "--reply-to",
        type=int,
        default=None,
        help="Optional Telegram message_id to reply to.",
    )
    parser.add_argument(
        "--watch-seconds",
        type=int,
        default=int(env_or("TELEGRAM_WATCH_SECONDS", "60") or "60"),
        help="How long to watch the chat for replies after sending.",
    )
    parser.add_argument(
        "--from-user",
        default=env_or("TELEGRAM_FROM_USER", "ana_lfgbot"),
        help="Only print incoming messages from this username when watching.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=10,
        help="How many recent messages to print before sending.",
    )
    parser.add_argument(
        "--print-history",
        action="store_true",
        help="Print the most recent messages from the target chat before sending.",
    )
    parser.add_argument(
        "--no-watch",
        action="store_true",
        help="Do not wait for replies after sending.",
    )
    parser.add_argument(
        "--code",
        default=None,
        help="Telegram login code if you don't want to type it interactively.",
    )
    parser.add_argument(
        "--password",
        default=None,
        help="Telegram 2FA password if enabled. Prefer interactive prompt.",
    )
    return parser.parse_args()


async def ensure_authorized(client: TelegramClient, phone: str, code: Optional[str], password: Optional[str]) -> None:
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


def looks_numeric_chat(raw: str) -> bool:
    if not raw:
        return False
    if raw.startswith("-"):
        return raw[1:].isdigit()
    return raw.isdigit()


async def resolve_chat(client: TelegramClient, raw_chat: str):
    if not raw_chat:
        die("--chat is required.")
    target = int(raw_chat) if looks_numeric_chat(raw_chat) else raw_chat
    try:
        return await client.get_entity(target)
    except Exception as exc:
        die(f"Failed to resolve chat '{raw_chat}': {exc}")


def normalize_username(username: Optional[str]) -> Optional[str]:
    if not username:
        return None
    return username.lstrip("@").lower()


async def print_recent_history(client: TelegramClient, entity, limit: int) -> None:
    rows = []
    async for message in client.iter_messages(entity, limit=limit):
        sender = await message.get_sender()
        username = normalize_username(getattr(sender, "username", None))
        label = f"@{username}" if username else getattr(sender, "first_name", "unknown")
        rows.append((message.id, label, (message.text or "").replace("\n", "\\n")))

    if not rows:
        print("No recent messages.")
        return

    print("Recent messages:")
    for message_id, label, text in reversed(rows):
        print(f"  {message_id:>8}  {label:<20} {text}")


async def watch_replies(
    client: TelegramClient,
    entity,
    watch_seconds: int,
    from_user: Optional[str],
    started_at: float,
) -> None:
    target_username = normalize_username(from_user)
    done = asyncio.Event()

    @client.on(events.NewMessage(chats=entity))
    async def handler(event) -> None:
        sender = await event.get_sender()
        username = normalize_username(getattr(sender, "username", None))
        if target_username and username != target_username:
            return
        if event.message.date.timestamp() + 1 < started_at:
            return
        text = (event.raw_text or "").strip()
        print(f"\n[{event.message.id}] @{username or 'unknown'}: {text}")

    try:
        await asyncio.wait_for(done.wait(), timeout=watch_seconds)
    except asyncio.TimeoutError:
        return
    finally:
        client.remove_event_handler(handler)


async def main_async(args: argparse.Namespace) -> int:
    if not args.api_id:
        die("Missing Telegram API ID. Set TELEGRAM_API_ID or pass --api-id.")
    if not args.api_hash:
        die("Missing Telegram API hash. Set TELEGRAM_API_HASH or pass --api-hash.")

    session_path = Path(args.session).expanduser()
    session_path.parent.mkdir(parents=True, exist_ok=True)

    client = TelegramClient(str(session_path), args.api_id, args.api_hash)
    try:
        await ensure_authorized(client, args.phone, args.code, args.password)
        me = await client.get_me()
        username = normalize_username(getattr(me, "username", None))
        print(f"Authorized as @{username or 'unknown'}")

        entity = await resolve_chat(client, args.chat)
        print(f"Target chat resolved: {getattr(entity, 'title', None) or getattr(entity, 'username', None) or getattr(entity, 'id', 'unknown')}")

        if args.print_history:
            await print_recent_history(client, entity, args.limit)

        started_at = time.time()
        if args.send:
            sent = await client.send_message(entity, args.send, reply_to=args.reply_to)
            print(f"Sent message_id={sent.id}")

        if not args.no_watch:
            print(f"Watching for replies for {args.watch_seconds}s...")
            await watch_replies(
                client,
                entity,
                args.watch_seconds,
                args.from_user,
                started_at,
            )

        return 0
    finally:
        await client.disconnect()


def main() -> int:
    args = parse_args()
    return asyncio.run(main_async(args))


if __name__ == "__main__":
    raise SystemExit(main())
