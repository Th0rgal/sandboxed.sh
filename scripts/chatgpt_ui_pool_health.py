#!/usr/bin/env python3
"""Detect logged-out ChatGPT UI pool slots before missions do.

Pool slots are separate Chromium profile directories, so login state is
per-slot and expires in waves. Nothing else notices: a dead slot fails its
mission in ~5-10s, `profile-pool` still reports the slot `available` (that field
is a health record, not proof of login), and because slot acquisition prefers
the head of the list, one dead profile at the front starves the whole pool.

Each run probes a couple of slots round-robin, on a *copy* of the profile so a
live mission is never disturbed, and alerts through the same HMAC-signed Paloma
webhook `disk_watch` uses. A full sweep of 12 slots takes ~2h at the default
cadence, which is fast enough for a failure mode that unfolds over days.

Deliberately conservative about what counts as dead:

* The account button's `aria-label` is "Open profile menu" whether or not you
  are signed in — judging on it reports every slot dead. The authenticated nav
  (Library + Scheduled) is the signal that actually discriminates.
* Cloudflare's interstitial is its own state, not a logout. Back-to-back
  browser launches from one SOCKS exit trip it, so probes are paced and a
  challenge never overwrites a known state or raises an alert.
* A logged-out profile still greets the user by name from a stale
  personalization cookie, so "Hey, <name>" proves nothing either.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import hmac
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

BACKEND_CONFIG = os.environ.get(
    "SANDBOXED_BACKEND_CONFIG", "/root/.sandboxed-sh/backend_config.json"
)
ENV_FILE = os.environ.get("SANDBOXED_ENV_FILE", "/etc/open_agent/open_agent.env")
STATE_FILE = Path(
    os.environ.get("CHATGPT_POOL_HEALTH_STATE", "/var/lib/sandboxed-sh/chatgpt-pool-health.json")
)
SCRATCH = Path(os.environ.get("CHATGPT_POOL_HEALTH_SCRATCH", "/tmp/chatgpt-pool-health"))
DEFAULT_PROXY = "socks5://127.0.0.1:10880"

DOM_PROBE = """() => {
  const visible = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none';
  };
  const texts = [...document.querySelectorAll('button,a')]
    .filter(visible)
    .map((e) => (e.innerText || '').trim())
    .filter(Boolean);
  const body = document.body ? document.body.innerText || '' : '';
  const title = document.title || '';
  return {
    login_visible: texts.some((t) => /^(log in|se connecter|sign up|s'inscrire)$/i.test(t)),
    authed_nav: texts.includes('Library') || texts.includes('Scheduled'),
    account_picker: /choose an account to continue|welcome back|log in to another account/i.test(body),
    challenge: /verify you are human|verifying\\.\\.\\.|just a moment/i.test(body)
      || /just a moment|verifying/i.test(title)
      || !!document.querySelector('iframe[src*="challenges.cloudflare.com"]'),
  };
}"""


def log(message: str) -> None:
    print(f"chatgpt-pool-health: {message}", flush=True)


def env_value(key: str) -> str | None:
    if os.environ.get(key):
        return os.environ[key]
    # Never `source` the env file — a later line breaks under bash. Read the
    # single key we need.
    try:
        with open(ENV_FILE, "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if line.startswith(f"{key}="):
                    return line.split("=", 1)[1].strip().strip('"').strip("'")
    except OSError:
        pass
    return None


def pool_slots() -> tuple[list[str], str]:
    with open(BACKEND_CONFIG, "r", encoding="utf-8") as handle:
        raw = json.load(handle)
    entries = raw if isinstance(raw, list) else raw.get("backends", [])
    for entry in entries:
        if isinstance(entry, dict) and entry.get("id") == "chatgpt_ui":
            settings = entry.get("settings", {})
            slots = [settings["profile_dir"]] if settings.get("profile_dir") else []
            slots += list(settings.get("profile_dirs") or [])
            return slots, settings.get("proxy_server") or DEFAULT_PROXY
    raise SystemExit("chatgpt_ui backend not found in config")


def load_state() -> dict:
    try:
        return json.loads(STATE_FILE.read_text())
    except (OSError, ValueError):
        return {"cursor": 0, "slots": {}}


def save_state(state: dict) -> None:
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    lock_path = STATE_FILE.parent / "chatgpt-pool-health.lock"
    with lock_path.open("a+") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        try:
            current = load_state()
            merged_slots = dict(current.get("slots", {}))
            for name, candidate in state.get("slots", {}).items():
                existing = merged_slots.get(name, {})
                if float(candidate.get("checked_at", 0)) >= float(
                    existing.get("checked_at", 0)
                ):
                    merged_slots[name] = candidate
            merged = dict(current)
            merged.update(state)
            merged["slots"] = merged_slots
            tmp = STATE_FILE.with_name(
                f".{STATE_FILE.name}.{os.getpid()}.{time.time_ns()}.tmp"
            )
            tmp.write_text(json.dumps(merged, indent=2) + "\n")
            tmp.replace(STATE_FILE)
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def is_saved_account_choice(label: str) -> bool:
    """Match a remembered account card, never login/signup controls."""
    text = (label or "").strip()
    if not text or re.search(
        r"log in to another account|sign up|create account|continue with|use another|se connecter|s'inscrire",
        text,
        re.I,
    ):
        return False
    return bool(re.search(r"[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}", text, re.I))


def classify_probe(found: dict) -> str:
    """Only post-picker authenticated navigation is positive evidence."""
    if found.get("challenge"):
        return "challenge"
    if found.get("account_picker"):
        return "unknown"
    if found.get("login_visible"):
        return "logged_out"
    if found.get("authed_nav"):
        return "logged_in"
    return "unknown"


def evaluate_dom_probe(page) -> dict | None:
    """Navigation invalidates one JS context; retry it instead of failing."""
    try:
        return page.evaluate(DOM_PROBE)
    except Exception as exc:  # noqa: BLE001 - Playwright errors are optional here
        message = str(exc).lower()
        if "execution context was destroyed" in message or "because of a navigation" in message:
            return None
        raise


def complete_saved_account_picker(page, settle_ms: int) -> dict:
    """Select the remembered account and classify the resulting page."""
    buttons = page.get_by_role("button")
    selected = False
    for index in range(buttons.count()):
        button = buttons.nth(index)
        try:
            if not button.is_visible():
                continue
            label = button.inner_text()
        except Exception:  # noqa: BLE001 - a malformed node is inconclusive
            continue
        if not is_saved_account_choice(label):
            continue
        button.click(timeout=8_000)
        selected = True
        break
    if not selected:
        return {
            "challenge": False,
            "account_picker": True,
            "login_visible": False,
            "authed_nav": False,
        }

    deadline = time.time() + max(settle_ms, 5_000) / 1000.0
    found = {}
    while time.time() < deadline:
        candidate = evaluate_dom_probe(page)
        if candidate is None:
            page.wait_for_timeout(500)
            continue
        found = candidate
        if found.get("challenge"):
            return found
        if not found.get("account_picker") and (
            found.get("login_visible") or found.get("authed_nav")
        ):
            return found
        page.wait_for_timeout(500)
    return found


def slot_in_use(profile_dir: str) -> bool:
    # A mission holds the profile for the whole turn; copying it mid-write
    # would probe a torn profile and could report a false logout.
    result = subprocess.run(
        ["pgrep", "-f", f"user-data-dir={profile_dir}( |$)"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def probe(profile_dir: str, proxy: str, settle_ms: int) -> str:
    from playwright.sync_api import sync_playwright

    src = Path(profile_dir)
    dst = SCRATCH / src.name
    shutil.rmtree(dst, ignore_errors=True)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    subprocess.run(["cp", "-a", str(src), str(dst)], check=True)
    for lock in list(dst.glob("Singleton*")):
        try:
            lock.unlink()
        except OSError:
            pass

    try:
        with sync_playwright() as pw:
            context = pw.chromium.launch_persistent_context(
                user_data_dir=str(dst),
                headless=False,
                viewport={"width": 1440, "height": 1000},
                args=["--disable-background-networking"],
                proxy={"server": proxy},
            )
            try:
                page = context.pages[0] if context.pages else context.new_page()
                page.goto("https://chatgpt.com/", wait_until="domcontentloaded", timeout=90_000)
                # The welcome-back picker is often 15–20s after domcontentloaded.
                # Classifying at the first paint treats overlay Log in as logout.
                deadline = time.time() + max(settle_ms, 0) / 1000.0
                found = {"challenge": False, "account_picker": False, "login_visible": False, "authed_nav": False}
                while True:
                    candidate = evaluate_dom_probe(page)
                    if candidate is None:
                        if time.time() >= deadline:
                            break
                        page.wait_for_timeout(500)
                        continue
                    found = candidate
                    if found.get("account_picker"):
                        found = complete_saved_account_picker(page, settle_ms)
                        break
                    if found.get("authed_nav") and not found.get("login_visible"):
                        break
                    if time.time() >= deadline:
                        break
                    page.wait_for_timeout(500)
            finally:
                context.close()
    except Exception as exc:  # noqa: BLE001 - a probe failure is never fatal
        log(f"{src.name}: probe error: {type(exc).__name__}: {exc}")
        return "unknown"
    finally:
        shutil.rmtree(dst, ignore_errors=True)

    return classify_probe(found)


def deliver_webhook(payload: dict) -> None:
    url = env_value("PALOMA_WEBHOOK_FORWARD_URL")
    if not url:
        log("no PALOMA_WEBHOOK_FORWARD_URL; alert not delivered")
        return
    body = json.dumps(payload).encode()
    request = urllib.request.Request(url, data=body, method="POST")
    request.add_header("Content-Type", "application/json")
    secret = env_value("PALOMA_WEBHOOK_SECRET")
    if secret:
        digest = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
        request.add_header("X-Hub-Signature-256", f"sha256={digest}")
    for attempt in range(1, 4):
        try:
            with urllib.request.urlopen(request, timeout=30) as resp:
                if 200 <= resp.status < 300:
                    return
                log(f"webhook non-success status={resp.status} attempt={attempt}")
        except Exception as exc:  # noqa: BLE001
            log(f"webhook send failed attempt={attempt}: {type(exc).__name__}: {exc}")
        time.sleep(0.25 * attempt)


def alert(slot: str, state: str, healthy: int, total: int, recovered: bool) -> None:
    if recovered:
        message = (
            f"ChatGPT UI pool slot {slot} is signed in again "
            f"({healthy}/{total} slots healthy)."
        )
    else:
        message = (
            f"ChatGPT UI pool slot {slot} is signed OUT — missions routed to it will "
            f"fail in seconds ({healthy}/{total} slots known healthy). "
            f"Re-login one profile and clone it over the dead ones."
        )
    deliver_webhook(
        {
            "type": "chatgpt_pool_alert",
            "event_id": str(uuid.uuid4()),
            "level": "ok" if recovered else "critical",
            "slot": slot,
            "state": state,
            "healthy_slots": healthy,
            "total_slots": total,
            "message": message,
            "occurred_at": datetime.now(timezone.utc).isoformat(),
        }
    )
    log(message)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--count",
        type=int,
        default=int(os.environ.get("CHATGPT_POOL_HEALTH_COUNT", "2")),
        help="slots to probe this run (round-robin across the pool)",
    )
    parser.add_argument(
        "--repeat-hours",
        type=float,
        default=float(os.environ.get("CHATGPT_POOL_HEALTH_REPEAT_HOURS", "12")),
        help="minimum delay before re-alerting on a slot that is still dead",
    )
    parser.add_argument(
        "--pace-seconds",
        type=float,
        default=float(os.environ.get("CHATGPT_POOL_HEALTH_PACE_SECONDS", "25")),
        help="delay between probes; back-to-back launches trip Cloudflare",
    )
    parser.add_argument("--settle-ms", type=int, default=25000)
    parser.add_argument("--slot", action="append", help="probe these slots instead of rotating")
    args = parser.parse_args()

    slots, proxy = pool_slots()
    if not slots:
        log("pool is empty")
        return 0

    state = load_state()
    by_name = {Path(s).name: s for s in slots}
    if args.slot:
        selected = [by_name[name] for name in args.slot if name in by_name]
    else:
        cursor = int(state.get("cursor", 0)) % len(slots)
        count = max(1, min(args.count, len(slots)))
        selected = [slots[(cursor + i) % len(slots)] for i in range(count)]
        state["cursor"] = (cursor + count) % len(slots)

    now = time.time()
    for index, profile_dir in enumerate(selected):
        name = Path(profile_dir).name
        if slot_in_use(profile_dir):
            log(f"{name}: in use by a mission, skipped")
            continue
        if index:
            time.sleep(args.pace_seconds)

        observed = probe(profile_dir, proxy, args.settle_ms)
        entry = state.setdefault("slots", {}).setdefault(name, {})
        previous = entry.get("state")
        log(f"{name}: {observed}")

        # An inconclusive read must never overwrite a known state, or a
        # Cloudflare challenge would silently "clear" a dead slot.
        if observed in {"challenge", "unknown"}:
            entry["last_inconclusive_at"] = now
            continue

        entry["state"] = observed
        entry["checked_at"] = now
        entry["verdict_version"] = 2
        entry["source"] = "post-picker-probe-v2"
        if previous != observed:
            entry["since"] = now

        healthy = sum(
            1 for slot in state["slots"].values() if slot.get("state") == "logged_in"
        )
        if observed == "logged_out":
            last = entry.get("last_alert_at")
            if last is None or (now - last) >= args.repeat_hours * 3600:
                alert(name, observed, healthy, len(slots), recovered=False)
                entry["last_alert_at"] = now
        elif observed == "logged_in" and entry.pop("last_alert_at", None) is not None:
            alert(name, observed, healthy, len(slots), recovered=True)

    save_state(state)
    healthy = [n for n, s in state.get("slots", {}).items() if s.get("state") == "logged_in"]
    dead = [n for n, s in state.get("slots", {}).items() if s.get("state") == "logged_out"]
    log(f"known healthy={len(healthy)} dead={len(dead)}{' ' + ','.join(dead) if dead else ''}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
