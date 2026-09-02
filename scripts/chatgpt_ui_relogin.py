#!/usr/bin/env python3
"""Re-login ChatGPT UI pool profiles using Bitwarden secrets.

Pool slots are Chromium profiles. When a session expires, missions fail with
``auth_required`` and the operator previously recovered them over VNC. This
helper is the unattended replacement:

1. Load ``CHATGPT_USERNAME``, ``CHATGPT_PASSWORD``, and ``CHATGPT_OTP`` from
   Bitwarden Secrets Manager (Hermes's ``BWS_ACCESS_TOKEN``).
2. Log in once on a scratch copy of an idle dead profile.
3. Clone that authenticated profile over the other idle dead slots.

It never prints secret values, never bypasses CAPTCHA/Cloudflare, and never
touches a profile a mission currently holds. Failed missions are not retried;
this only repairs login state for the next dispatch.

The browser login itself lives in ``chatgpt_ui_login_steps.py`` as a
priority-ordered step registry: each tick snapshots the page and runs the
first matching handler, so field order and chrome variants do not need a
hard-coded sequence.
"""
from __future__ import annotations

import argparse
import asyncio
import base64
import fcntl
import hashlib
import hmac
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import Callable
from urllib.parse import parse_qs, urlparse

try:
    from chatgpt_ui_login_steps import ReloginError, run_login
except ImportError:  # unittest imports this module as scripts.chatgpt_ui_relogin
    from scripts.chatgpt_ui_login_steps import ReloginError, run_login

REQUIRED_SECRET_KEYS = ("CHATGPT_USERNAME", "CHATGPT_PASSWORD", "CHATGPT_OTP")
BACKEND_CONFIG = os.environ.get(
    "SANDBOXED_BACKEND_CONFIG", "/root/.sandboxed-sh/backend_config.json"
)
STATE_FILE = Path(
    os.environ.get("CHATGPT_POOL_HEALTH_STATE", "/var/lib/sandboxed-sh/chatgpt-pool-health.json")
)
SCRATCH = Path(os.environ.get("CHATGPT_POOL_RELOGIN_SCRATCH", "/tmp/chatgpt-pool-relogin"))
LOCK_FILE = Path(
    os.environ.get("CHATGPT_POOL_RELOGIN_LOCK", "/var/lib/sandboxed-sh/chatgpt-relogin.lock")
)
STATUS_FILE = Path(
    os.environ.get("CHATGPT_POOL_RELOGIN_STATUS", "/var/lib/sandboxed-sh/chatgpt-relogin.json")
)
DEFAULT_PROXY = "socks5://127.0.0.1:10880"
COOLDOWN_SECS = int(os.environ.get("CHATGPT_UI_RELOGIN_COOLDOWN_SECS", str(30 * 60)))
HERMES_ENV = "/var/lib/hermes-assistant/.env"
HOST_ENV_CANDIDATES = (
    "/etc/sandboxed-sh/sandboxed-sh-prod.env",
    "/etc/sandboxed_sh/sandboxed_sh.env",
    "/etc/open_agent/open_agent.env",
)


def resolve_host_env() -> str:
    override = os.environ.get("SANDBOXED_ENV_FILE")
    if override:
        return override
    for path in HOST_ENV_CANDIDATES:
        if os.path.isfile(path):
            return path
    return HOST_ENV_CANDIDATES[-1]


HOST_ENV = resolve_host_env()

OTP_SECRET_RE = re.compile(r"^[A-Z2-7]+=*$", re.I)


def log(message: str) -> None:
    print(f"chatgpt-ui-relogin: {message}", flush=True)


def env_value(key: str, env_files: tuple[str, ...] | None = None) -> str | None:
    if os.environ.get(key):
        return os.environ[key]
    files = env_files if env_files is not None else (HERMES_ENV, HOST_ENV)
    for path in files:
        if not path:
            continue
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    line = line.strip()
                    if line.startswith(f"{key}="):
                        return line.split("=", 1)[1].strip().strip('"').strip("'")
        except OSError:
            continue
    return None


def redact(message: str, secrets: list[str] | tuple[str, ...] = ()) -> str:
    out = message
    for value in secrets:
        if value and len(value) >= 4:
            out = out.replace(value, "***")
    return out


def normalize_otp_secret(raw: str) -> tuple[str, int, int]:
    """Return (base32_secret, period, digits) from a raw secret or otpauth URI."""
    text = (raw or "").strip()
    if not text:
        raise ReloginError("missing_secrets", "CHATGPT_OTP is empty")
    period, digits = 30, 6
    if text.lower().startswith("otpauth://"):
        parsed = urlparse(text)
        query = parse_qs(parsed.query)
        secret = (query.get("secret") or [""])[0]
        if query.get("period"):
            try:
                period = int(query["period"][0])
            except ValueError as error:
                raise ReloginError("invalid_otp", "CHATGPT_OTP period is not an integer") from error
        if query.get("digits"):
            try:
                digits = int(query["digits"][0])
            except ValueError as error:
                raise ReloginError("invalid_otp", "CHATGPT_OTP digits is not an integer") from error
        text = secret
    text = re.sub(r"[\s-]+", "", text)
    if not OTP_SECRET_RE.fullmatch(text):
        raise ReloginError("invalid_otp", "CHATGPT_OTP is not a TOTP secret or otpauth URI")
    if period <= 0 or digits <= 0:
        raise ReloginError("invalid_otp", "CHATGPT_OTP period/digits must be positive")
    return text, period, digits


def totp_code(raw: str, for_time: int | float | None = None) -> str:
    secret, period, digits = normalize_otp_secret(raw)
    key = base64.b32decode(secret, casefold=True)
    counter = int((time.time() if for_time is None else for_time) // period)
    digest = hmac.new(key, struct.pack(">Q", counter), hashlib.sha1).digest()
    offset = digest[-1] & 0x0F
    number = struct.unpack(">I", digest[offset : offset + 4])[0] & 0x7FFFFFFF
    return str(number % (10**digits)).zfill(digits)


def fetch_bws_secrets(
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> dict[str, str]:
    token = env_value("BWS_ACCESS_TOKEN")
    if not token:
        raise ReloginError(
            "bws_unavailable",
            "BWS_ACCESS_TOKEN is not set (expected Hermes env or the process environment)",
        )
    env = {**os.environ, "BWS_ACCESS_TOKEN": token}
    listed = run(
        ["bws", "secret", "list", "--output", "json"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if listed.returncode != 0:
        raise ReloginError("bws_unavailable", "bws secret list failed")
    try:
        payload = json.loads(listed.stdout)
    except json.JSONDecodeError as error:
        raise ReloginError("bws_unavailable", "bws secret list returned invalid JSON") from error
    if not isinstance(payload, list):
        raise ReloginError("bws_unavailable", "bws secret list returned a non-list")

    by_key: dict[str, str] = {}
    ids: dict[str, str] = {}
    for item in payload:
        if not isinstance(item, dict):
            continue
        key = item.get("key")
        if not isinstance(key, str):
            continue
        value = item.get("value")
        if isinstance(value, str) and value:
            by_key[key] = value
        ident = item.get("id")
        if isinstance(ident, str) and ident:
            ids[key] = ident

    for key in REQUIRED_SECRET_KEYS:
        if by_key.get(key) or key not in ids:
            continue
        fetched = run(
            ["bws", "secret", "get", ids[key], "--output", "json"],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        if fetched.returncode != 0:
            continue
        try:
            body = json.loads(fetched.stdout)
        except json.JSONDecodeError:
            continue
        value = body.get("value") if isinstance(body, dict) else None
        if isinstance(value, str) and value:
            by_key[key] = value

    missing = [key for key in REQUIRED_SECRET_KEYS if not by_key.get(key)]
    if missing:
        raise ReloginError(
            "missing_secrets",
            "Bitwarden is missing " + ", ".join(missing),
        )
    return {key: by_key[key] for key in REQUIRED_SECRET_KEYS}


def pool_slots() -> tuple[list[str], str]:
    with open(BACKEND_CONFIG, "r", encoding="utf-8") as handle:
        raw = json.load(handle)
    entries = raw if isinstance(raw, list) else raw.get("backends", [])
    for entry in entries:
        if isinstance(entry, dict) and entry.get("id") == "chatgpt_ui":
            settings = entry.get("settings", {})
            slots = [settings["profile_dir"]] if settings.get("profile_dir") else []
            slots += list(settings.get("profile_dirs") or [])
            seen: list[str] = []
            for slot in slots:
                if slot and slot not in seen:
                    seen.append(slot)
            return seen, settings.get("proxy_server") or DEFAULT_PROXY
    raise ReloginError("invalid_config", "chatgpt_ui backend not found in config")


def load_state() -> dict:
    try:
        return json.loads(STATE_FILE.read_text())
    except (OSError, ValueError):
        return {"slots": {}}


def slot_in_use(profile_dir: str) -> bool:
    result = subprocess.run(
        ["pgrep", "-f", f"user-data-dir={profile_dir}( |$)"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def idle_dead_targets(
    slots: list[str],
    state: dict,
    *,
    force: bool = False,
    in_use: Callable[[str], bool] = slot_in_use,
) -> list[str]:
    by_name = {Path(slot).name: slot for slot in slots}
    recorded = state.get("slots") if isinstance(state.get("slots"), dict) else {}
    chosen: list[str] = []
    for name, path in by_name.items():
        if in_use(path):
            continue
        entry = recorded.get(name) if isinstance(recorded.get(name), dict) else {}
        if force or entry.get("state") == "logged_out":
            chosen.append(path)
    return chosen


def drop_singleton_locks(profile: Path) -> None:
    for lock in list(profile.glob("Singleton*")):
        try:
            lock.unlink()
        except OSError:
            pass


def copy_profile(src: Path, dst: Path) -> None:
    shutil.rmtree(dst, ignore_errors=True)
    dst.parent.mkdir(parents=True, exist_ok=True)
    if src.is_dir():
        subprocess.run(["cp", "-a", str(src), str(dst)], check=True)
    else:
        dst.mkdir(parents=True, exist_ok=True)
    drop_singleton_locks(dst)


def replace_profile(src: Path, dest: Path, *, in_use: Callable[[str], bool] = slot_in_use) -> bool:
    if in_use(str(dest)):
        log(f"{dest.name}: in use, not replaced")
        return False
    staging = dest.with_name(dest.name + ".relogin-new")
    previous = dest.with_name(dest.name + ".relogin-old")
    shutil.rmtree(staging, ignore_errors=True)
    shutil.rmtree(previous, ignore_errors=True)
    subprocess.run(["cp", "-a", str(src), str(staging)], check=True)
    drop_singleton_locks(staging)
    if in_use(str(dest)):
        shutil.rmtree(staging, ignore_errors=True)
        log(f"{dest.name}: became in use, not replaced")
        return False
    if dest.exists():
        dest.rename(previous)
    staging.rename(dest)
    shutil.rmtree(previous, ignore_errors=True)
    return True


def acquire_lock(path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = open(path, "w", encoding="utf-8")
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        handle.close()
        raise ReloginError("in_progress", "another relogin is already running") from error
    handle.write(str(os.getpid()))
    handle.flush()
    return handle


def check_cooldown(path: Path, now: float, *, force: bool) -> None:
    if force:
        return
    try:
        payload = json.loads(path.read_text())
    except (OSError, ValueError):
        return
    last = payload.get("last_attempt_at")
    if isinstance(last, (int, float)) and now - last < COOLDOWN_SECS:
        waited = int(now - last)
        raise ReloginError(
            "cooldown",
            f"last relogin attempt was {waited}s ago; wait {COOLDOWN_SECS}s between attempts",
        )


def record_status(path: Path, **fields) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    current: dict = {}
    try:
        loaded = json.loads(path.read_text())
        if isinstance(loaded, dict):
            current = loaded
    except (OSError, ValueError):
        current = {}
    current.update(fields)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(current, indent=2) + "\n")
    tmp.replace(path)


def start_relogin(
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> str:
    """Kick the oneshot unit without waiting. Used by pool health."""
    if os.environ.get("CHATGPT_UI_RELOGIN_AUTO", "1").strip().lower() in {
        "0",
        "false",
        "no",
        "off",
    }:
        return "disabled"
    unit = os.environ.get("CHATGPT_UI_RELOGIN_UNIT", "chatgpt-ui-relogin.service")
    completed = run(
        ["systemctl", "start", "--no-block", unit],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or str(completed.returncode)).strip()
        return f"failed:{detail}"
    return f"started:{unit}"


def direct_chromium_command(
    executable: str, profile_dir: Path, proxy: str, debugging_port: int
) -> list[str]:
    """Build the ordinary Chromium command used for interactive and repair runs.

    Playwright still attaches over the loopback-only DevTools endpoint to drive
    the login form, but it does not launch Chromium or inject its launch-time
    argument bundle. This keeps unattended repair on the same browser path as
    the operator-confirmed session without spoofing browser identity or trying
    to bypass a challenge.
    """
    command = [
        executable,
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--no-first-run",
        "--no-default-browser-check",
        "--window-size=1440,1000",
        "--remote-debugging-address=127.0.0.1",
        f"--remote-debugging-port={debugging_port}",
        f"--user-data-dir={profile_dir}",
    ]
    if proxy:
        command.append(f"--proxy-server={proxy}")
    command.append("about:blank")
    return command


def reserve_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


async def login_scratch(profile_dir: Path, proxy: str, creds: dict[str, str]) -> str:
    from playwright.async_api import async_playwright

    drop_singleton_locks(profile_dir)
    async with async_playwright() as playwright:
        port = reserve_loopback_port()
        command = direct_chromium_command(
            playwright.chromium.executable_path, profile_dir, proxy, port
        )
        process = await asyncio.create_subprocess_exec(
            *command,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.DEVNULL,
        )
        browser = None
        try:
            endpoint = f"http://127.0.0.1:{port}"
            for _ in range(120):
                if process.returncode is not None:
                    raise ReloginError("browser_launch", "Chromium exited before CDP was ready")
                try:
                    browser = await playwright.chromium.connect_over_cdp(
                        endpoint, timeout=1_000
                    )
                    break
                except Exception:
                    await asyncio.sleep(0.25)
            if browser is None:
                raise ReloginError("browser_launch", "Chromium CDP endpoint was not ready")
            if not browser.contexts:
                raise ReloginError("browser_launch", "Chromium exposed no persistent context")
            context = browser.contexts[0]
            page = context.pages[0] if context.pages else await context.new_page()
            return await run_login(page, creds, totp_fn=totp_code)
        finally:
            if browser is not None:
                try:
                    await browser.close()
                except Exception:
                    pass
            if process.returncode is None:
                process.terminate()
                try:
                    await asyncio.wait_for(process.wait(), timeout=10)
                except asyncio.TimeoutError:
                    process.kill()
                    await process.wait()


def repair_pool(creds: dict[str, str], *, force: bool, dry_run: bool) -> int:
    slots, proxy = pool_slots()
    if not slots:
        log("pool is empty")
        return 0
    targets = idle_dead_targets(slots, load_state(), force=force)
    if not targets:
        log("no idle dead slots" if not force else "no idle slots")
        return 0
    names = [Path(path).name for path in targets]
    log(f"targets={','.join(names)}")
    if dry_run:
        log("dry-run: secrets loaded, browser not launched")
        return 0

    now = time.time()
    check_cooldown(STATUS_FILE, now, force=force)
    record_status(STATUS_FILE, last_attempt_at=now, last_result="started", targets=names)

    source = Path(targets[0])
    scratch = SCRATCH / source.name
    copy_profile(source, scratch)
    try:
        result = asyncio.run(login_scratch(scratch, proxy, creds))
        replaced = []
        for path in targets:
            dest = Path(path)
            if replace_profile(scratch, dest):
                replaced.append(dest.name)
                log(f"cloned authenticated profile onto {dest.name}")
        record_status(
            STATUS_FILE,
            last_attempt_at=now,
            last_finished_at=time.time(),
            last_result=result,
            replaced=replaced,
        )
        log(f"repaired={','.join(replaced) or 'none'} via {result}")
        return 0
    except Exception as error:
        code = getattr(error, "code", type(error).__name__)
        record_status(
            STATUS_FILE,
            last_attempt_at=now,
            last_finished_at=time.time(),
            last_result=str(code),
        )
        raise
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--force",
        action="store_true",
        help="login even when pool-health has no logged_out slots (idle slots only)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="load Bitwarden secrets and print targets without launching a browser",
    )
    args = parser.parse_args()
    lock = None
    creds: dict[str, str] = {}
    try:
        lock = acquire_lock(LOCK_FILE)
        creds = fetch_bws_secrets()
        log("loaded Bitwarden secrets for " + ", ".join(REQUIRED_SECRET_KEYS))
        return repair_pool(creds, force=args.force, dry_run=args.dry_run)
    except ReloginError as error:
        log(f"{error.code}: {redact(str(error), list(creds.values()))}")
        return 2 if error.code in {"missing_secrets", "bws_unavailable", "invalid_otp"} else 1
    except Exception as error:  # noqa: BLE001 - never leak secret values on the way out
        log(redact(f"{type(error).__name__}: {error}", list(creds.values())))
        return 1
    finally:
        if lock is not None:
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
            lock.close()


if __name__ == "__main__":
    sys.exit(main())
