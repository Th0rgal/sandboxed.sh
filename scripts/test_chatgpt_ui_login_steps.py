#!/usr/bin/env python3

from __future__ import annotations

import unittest

from scripts.chatgpt_ui_login_steps import (
    LOGIN_STEPS,
    AnotherAccountStep,
    LoginContext,
    LoginStep,
    ReloginError,
    UiSnapshot,
    describe_snapshot,
    select_step,
)


def ctx(**fields) -> LoginContext:
    context = LoginContext(
        creds={
            "CHATGPT_USERNAME": "user@example.com",
            "CHATGPT_PASSWORD": "s3cret-pass",
            "CHATGPT_OTP": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        },
        totp_fn=lambda _secret: "123456",
    )
    for name, value in fields.items():
        setattr(context, name, value)
    return context


def chosen(snap: UiSnapshot, **fields) -> str | None:
    step = select_step(snap, ctx(**fields))
    return None if step is None else step.name


class SelectStepTests(unittest.TestCase):
    def test_step_names_are_unique(self):
        names = [step.name for step in LOGIN_STEPS]
        self.assertEqual(names, list(dict.fromkeys(names)))

    def test_cloudflare_wins_over_login_button(self):
        self.assertEqual(
            chosen(
                UiSnapshot(
                    has_cloudflare=True,
                    has_login_button=True,
                    labels=("Log in",),
                )
            ),
            "cloudflare",
        )

    def test_authenticated_shell_short_circuits(self):
        self.assertEqual(
            chosen(UiSnapshot(url="https://chatgpt.com/", has_account_shell=True)),
            "authenticated",
        )

    def test_auth_url_is_not_treated_as_already_signed_in(self):
        self.assertIsNone(
            chosen(
                UiSnapshot(
                    url="https://auth.openai.com/email-otp",
                    has_account_shell=True,
                )
            )
        )

    def test_classic_email_then_password_then_otp(self):
        self.assertEqual(chosen(UiSnapshot(has_email=True)), "email")
        self.assertEqual(
            chosen(UiSnapshot(has_password=True), filled={"email"}),
            "password",
        )
        self.assertEqual(
            chosen(UiSnapshot(has_otp=True), filled={"email", "password"}),
            "otp",
        )

    def test_combined_email_and_password_form(self):
        self.assertEqual(
            chosen(UiSnapshot(has_email=True, has_password=True)),
            "email_and_password",
        )

    def test_password_only_returning_session(self):
        self.assertEqual(chosen(UiSnapshot(has_password=True)), "password")

    def test_otp_only_verify_its_you(self):
        self.assertEqual(chosen(UiSnapshot(has_otp=True)), "otp")

    def test_already_filled_email_is_not_refilled(self):
        self.assertIsNone(chosen(UiSnapshot(has_email=True), filled={"email"}))

    def test_otp_can_be_retried_after_totp_window(self):
        self.assertIsNone(
            chosen(
                UiSnapshot(has_otp=True),
                filled={"otp"},
                last_otp_at=100.0,
                now=lambda: 110.0,
            )
        )
        self.assertEqual(
            chosen(
                UiSnapshot(has_otp=True),
                filled={"otp"},
                last_otp_at=100.0,
                now=lambda: 130.0,
            ),
            "otp",
        )

    def test_another_account_label_is_enough_to_detect_picker(self):
        self.assertEqual(
            chosen(
                UiSnapshot(
                    has_account_picker=True,
                    labels=("Fricoben", "Log in to another account", "Create account"),
                )
            ),
            "account_picker",
        )

    def test_picker_matches_on_another_account_label_even_without_flag(self):
        self.assertEqual(
            chosen(
                UiSnapshot(
                    labels=("Fricoben", "Log in to another account", "Create account")
                )
            ),
            "account_picker",
        )

    def test_account_picker_beats_login_button(self):
        self.assertEqual(
            chosen(
                UiSnapshot(
                    has_account_picker=True,
                    has_login_button=True,
                    labels=("Log in", "user@x.com"),
                )
            ),
            "account_picker",
        )

    def test_passkey_overlay(self):
        self.assertEqual(chosen(UiSnapshot(has_passkey=True)), "passkey")

    def test_cookie_banner(self):
        self.assertEqual(chosen(UiSnapshot(has_cookie=True)), "cookie_banner")

    def test_stay_signed_in(self):
        self.assertEqual(chosen(UiSnapshot(has_stay_signed_in=True)), "stay_signed_in")

    def test_choose_authenticator_over_email(self):
        self.assertEqual(chosen(UiSnapshot(has_choose_totp=True)), "choose_totp")

    def test_french_login_button(self):
        self.assertEqual(
            chosen(UiSnapshot(has_login_button=True, labels=("Se connecter",))),
            "login_button",
        )

    def test_email_code_mfa_fails_closed(self):
        self.assertEqual(
            chosen(UiSnapshot(has_email_code=True, has_otp=False)),
            "email_code_mfa",
        )

    def test_phone_mfa_fails_closed(self):
        self.assertEqual(chosen(UiSnapshot(has_phone=True)), "phone_mfa")

    def test_credential_error_fails_closed(self):
        self.assertEqual(
            chosen(UiSnapshot(has_credential_error=True, has_password=True)),
            "credential_error",
        )

    def test_sso_only_when_no_password_form(self):
        self.assertEqual(
            chosen(UiSnapshot(has_sso=True, labels=("Continue with Google",))),
            "sso_only",
        )
        self.assertEqual(
            chosen(UiSnapshot(has_sso=True, has_email=True)),
            "email",
        )

    def test_login_button_is_clicked_once(self):
        self.assertIsNone(
            chosen(
                UiSnapshot(has_login_button=True),
                filled={"login_clicked"},
                last_login_click_at=100.0,
                now=lambda: 104.0,
            )
        )

    def test_stuck_login_button_falls_back_after_hydration_window(self):
        self.assertEqual(
            chosen(
                UiSnapshot(has_login_button=True),
                filled={"login_clicked"},
                last_login_click_at=100.0,
                now=lambda: 105.0,
            ),
            "login_button",
        )

    def test_login_href_counts_as_login_button(self):
        self.assertEqual(
            chosen(UiSnapshot(has_login_button=True, url="https://chatgpt.com/")),
            "login_button",
        )

    def test_login_button_ignored_once_a_field_is_visible(self):
        self.assertEqual(
            chosen(UiSnapshot(has_login_button=True, has_email=True)),
            "email",
        )


class SnapshotDescribeTests(unittest.TestCase):
    def test_describe_does_not_include_body_or_raw_email_labels(self):
        snap = UiSnapshot(
            url="https://auth.openai.com/sign-in?token=abc",
            title="Sign in",
            body="secret user@example.com should never be logged",
            labels=("Continue", "***"),
        )
        text = describe_snapshot(snap)
        self.assertIn("url=/sign-in", text)
        self.assertNotIn("user@example.com", text)
        self.assertNotIn("should never be logged", text)


    def test_optional_another_account_step_overrides_picker(self):
        snap = UiSnapshot(has_account_picker=True, has_login_button=True)
        self.assertEqual(chosen(snap), "account_picker")
        self.assertEqual(
            select_step(
                snap,
                ctx(),
                steps=(AnotherAccountStep(),)
                + tuple(step for step in LOGIN_STEPS if step.name != "account_picker"),
            ).name,
            "another_account",
        )


class RegistryExtensionTests(unittest.TestCase):
    def test_new_step_can_be_prepended_without_rewriting_the_loop(self):
        class NewCaptcha(LoginStep):
            name = "new_captcha"

            def matches(self, snap, ctx):
                return "are you a robot" in (snap.body or "").lower()

            async def apply(self, page, ctx, snap):
                raise ReloginError("challenge", "new captcha")

        snap = UiSnapshot(body="Are you a robot?", has_login_button=True)
        self.assertEqual(select_step(snap, ctx()).name, "login_button")
        self.assertEqual(
            select_step(snap, ctx(), steps=(NewCaptcha(),) + LOGIN_STEPS).name,
            "new_captcha",
        )


if __name__ == "__main__":
    unittest.main()
