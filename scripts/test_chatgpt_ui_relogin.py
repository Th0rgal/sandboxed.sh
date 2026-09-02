#!/usr/bin/env python3

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.chatgpt_ui_relogin import (
    REQUIRED_SECRET_KEYS,
    ReloginError,
    direct_chromium_command,
    env_value,
    fetch_bws_secrets,
    idle_dead_targets,
    normalize_otp_secret,
    redact,
    replace_profile,
    start_relogin,
    totp_code,
)


class FakeCompleted:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class TotpTests(unittest.TestCase):
    def test_rfc6238_sha1_vector(self):
        # RFC 6238 appendix B, ASCII secret "12345678901234567890" at T=59.
        secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        self.assertEqual(totp_code(secret, for_time=59), "287082")

    def test_otpauth_uri_and_spaced_secret(self):
        uri = "otpauth://totp/ChatGPT:user@example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=6&period=30"
        self.assertEqual(normalize_otp_secret(uri)[0], "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
        spaced = "GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ"
        self.assertEqual(normalize_otp_secret(spaced)[0], "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")

    def test_invalid_otp_error_does_not_echo_secret(self):
        with self.assertRaises(ReloginError) as raised:
            normalize_otp_secret("not a secret at all!!")
        self.assertEqual(raised.exception.code, "invalid_otp")
        self.assertNotIn("not a secret", str(raised.exception))


class SecretLoadingTests(unittest.TestCase):
    def test_required_keys_match_bitwarden(self):
        self.assertEqual(
            REQUIRED_SECRET_KEYS,
            ("CHATGPT_USERNAME", "CHATGPT_PASSWORD", "CHATGPT_OTP"),
        )

    def test_env_value_reads_only_named_key(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "hermes.env"
            path.write_text(
                "UNRELATED=should-not-leak\nBWS_ACCESS_TOKEN=token-value\n",
                encoding="utf-8",
            )
            env = {key: value for key, value in os.environ.items() if key != "BWS_ACCESS_TOKEN"}
            with patch.dict("os.environ", env, clear=True):
                self.assertEqual(
                    env_value("BWS_ACCESS_TOKEN", env_files=(str(path),)),
                    "token-value",
                )

    def test_fetch_bws_secrets_by_key_from_list(self):
        payload = [
            {"id": "1", "key": "CHATGPT_USERNAME", "value": "user@example.com"},
            {"id": "2", "key": "CHATGPT_PASSWORD", "value": "s3cret-pass"},
            {"id": "3", "key": "CHATGPT_OTP", "value": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"},
            {"id": "4", "key": "OTHER", "value": "ignore-me"},
        ]

        def run(cmd, **_kwargs):
            self.assertEqual(cmd[:3], ["bws", "secret", "list"])
            return FakeCompleted(stdout=json.dumps(payload))

        with patch("scripts.chatgpt_ui_relogin.env_value", return_value="token"):
            secrets = fetch_bws_secrets(run=run)
        self.assertEqual(secrets["CHATGPT_USERNAME"], "user@example.com")
        self.assertEqual(set(secrets), set(REQUIRED_SECRET_KEYS))

    def test_fetch_falls_back_to_secret_get_when_list_has_no_values(self):
        listed = [
            {"id": "u1", "key": "CHATGPT_USERNAME"},
            {"id": "p1", "key": "CHATGPT_PASSWORD"},
            {"id": "o1", "key": "CHATGPT_OTP"},
        ]
        values = {
            "u1": {"key": "CHATGPT_USERNAME", "value": "user@example.com"},
            "p1": {"key": "CHATGPT_PASSWORD", "value": "s3cret-pass"},
            "o1": {"key": "CHATGPT_OTP", "value": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"},
        }

        def run(cmd, **_kwargs):
            if cmd[:3] == ["bws", "secret", "list"]:
                return FakeCompleted(stdout=json.dumps(listed))
            self.assertEqual(cmd[:3], ["bws", "secret", "get"])
            return FakeCompleted(stdout=json.dumps(values[cmd[3]]))

        with patch("scripts.chatgpt_ui_relogin.env_value", return_value="token"):
            secrets = fetch_bws_secrets(run=run)
        self.assertEqual(secrets["CHATGPT_PASSWORD"], "s3cret-pass")

    def test_missing_secret_names_the_key_not_the_value(self):
        payload = [{"id": "1", "key": "CHATGPT_USERNAME", "value": "user@example.com"}]

        def run(cmd, **_kwargs):
            return FakeCompleted(stdout=json.dumps(payload))

        with patch("scripts.chatgpt_ui_relogin.env_value", return_value="token"):
            with self.assertRaises(ReloginError) as raised:
                fetch_bws_secrets(run=run)
        self.assertEqual(raised.exception.code, "missing_secrets")
        self.assertIn("CHATGPT_PASSWORD", str(raised.exception))
        self.assertIn("CHATGPT_OTP", str(raised.exception))
        self.assertNotIn("user@example.com", str(raised.exception))

    def test_bws_failure_does_not_include_command_output(self):
        def run(cmd, **_kwargs):
            return FakeCompleted(returncode=1, stderr="token=super-secret-token leaked")

        with patch("scripts.chatgpt_ui_relogin.env_value", return_value="token"):
            with self.assertRaises(ReloginError) as raised:
                fetch_bws_secrets(run=run)
        self.assertEqual(raised.exception.code, "bws_unavailable")
        self.assertNotIn("super-secret-token", str(raised.exception))

    def test_redact_replaces_long_secrets_only(self):
        message = "failed for user@example.com with s3cret-pass"
        self.assertEqual(
            redact(message, ["s3cret-pass", "ab"]),
            "failed for user@example.com with ***",
        )


class TargetAndCloneTests(unittest.TestCase):
    def test_idle_dead_skips_in_use_and_healthy(self):
        slots = ["/var/lib/p/one", "/var/lib/p/two", "/var/lib/p/three"]
        state = {
            "slots": {
                "one": {"state": "logged_out"},
                "two": {"state": "logged_out"},
                "three": {"state": "logged_in"},
            }
        }
        in_use = lambda path: path.endswith("two")
        self.assertEqual(
            idle_dead_targets(slots, state, in_use=in_use),
            ["/var/lib/p/one"],
        )

    def test_force_selects_every_idle_slot(self):
        slots = ["/var/lib/p/one", "/var/lib/p/two"]
        state = {"slots": {"one": {"state": "logged_in"}}}
        in_use = lambda path: path.endswith("two")
        self.assertEqual(
            idle_dead_targets(slots, state, force=True, in_use=in_use),
            ["/var/lib/p/one"],
        )

    def test_replace_profile_skips_in_use_destination(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            src = root / "src"
            dest = root / "dest"
            src.mkdir()
            (src / "cookie").write_text("authed", encoding="utf-8")
            dest.mkdir()
            (dest / "cookie").write_text("stale", encoding="utf-8")
            self.assertFalse(replace_profile(src, dest, in_use=lambda _path: True))
            self.assertEqual((dest / "cookie").read_text(encoding="utf-8"), "stale")

    def test_replace_profile_swaps_idle_destination(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            src = root / "src"
            dest = root / "dest"
            src.mkdir()
            (src / "cookie").write_text("authed", encoding="utf-8")
            dest.mkdir()
            (dest / "cookie").write_text("stale", encoding="utf-8")
            self.assertTrue(replace_profile(src, dest, in_use=lambda _path: False))
            self.assertEqual((dest / "cookie").read_text(encoding="utf-8"), "authed")
            self.assertFalse(dest.with_name("dest.relogin-old").exists())
            self.assertFalse(dest.with_name("dest.relogin-new").exists())


class BrowserLaunchTests(unittest.TestCase):
    def test_relogin_uses_direct_loopback_cdp_chromium(self):
        command = direct_chromium_command(
            "/opt/chrome", Path("/tmp/profile"), "socks5://127.0.0.1:10880", 9223
        )
        self.assertEqual(command[0], "/opt/chrome")
        self.assertIn("--remote-debugging-address=127.0.0.1", command)
        self.assertIn("--remote-debugging-port=9223", command)
        self.assertIn("--proxy-server=socks5://127.0.0.1:10880", command)
        self.assertIn("--user-data-dir=/tmp/profile", command)
        self.assertNotIn("--disable-background-networking", command)
        self.assertNotIn("--enable-automation", command)


class StartReloginTests(unittest.TestCase):
    def test_disabled_does_not_call_systemctl(self):
        calls = []

        def run(cmd, **_kwargs):
            calls.append(cmd)
            return FakeCompleted()

        with patch.dict("os.environ", {"CHATGPT_UI_RELOGIN_AUTO": "0"}):
            self.assertEqual(start_relogin(run=run), "disabled")
        self.assertEqual(calls, [])

    def test_starts_unit_without_blocking(self):
        def run(cmd, **_kwargs):
            self.assertEqual(cmd[:3], ["systemctl", "start", "--no-block"])
            self.assertEqual(cmd[3], "chatgpt-ui-relogin.service")
            return FakeCompleted()

        with patch.dict("os.environ", {"CHATGPT_UI_RELOGIN_AUTO": "1"}, clear=False):
            self.assertEqual(
                start_relogin(run=run), "started:chatgpt-ui-relogin.service"
            )


if __name__ == "__main__":
    unittest.main()
