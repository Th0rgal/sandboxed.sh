import unittest

from scripts.chatgpt_ui_pool_health import (
    classify_probe,
    evaluate_dom_probe,
    is_saved_account_choice,
)


class ChatgptUiPoolHealthTests(unittest.TestCase):
    def test_account_picker_is_not_authentication_evidence(self) -> None:
        self.assertEqual(
            classify_probe(
                {
                    "account_picker": True,
                    "authed_nav": False,
                    "challenge": False,
                    "login_visible": True,
                }
            ),
            "unknown",
        )

    def test_post_picker_login_is_logged_out(self) -> None:
        self.assertEqual(
            classify_probe(
                {
                    "account_picker": False,
                    "authed_nav": False,
                    "challenge": False,
                    "login_visible": True,
                }
            ),
            "logged_out",
        )

    def test_only_authenticated_navigation_is_logged_in(self) -> None:
        self.assertEqual(
            classify_probe(
                {
                    "account_picker": False,
                    "authed_nav": True,
                    "challenge": False,
                    "login_visible": False,
                }
            ),
            "logged_in",
        )

    def test_saved_account_filter_rejects_login_controls(self) -> None:
        self.assertTrue(is_saved_account_choice("Thomas\nthomas@example.com"))
        self.assertFalse(is_saved_account_choice("Thomas"))
        self.assertFalse(is_saved_account_choice("Log in to another account"))
        self.assertFalse(is_saved_account_choice("Continue with Google"))

    def test_navigation_context_loss_is_retried(self) -> None:
        class NavigatingPage:
            def evaluate(self, _script):
                raise RuntimeError(
                    "Execution context was destroyed, most likely because of a navigation"
                )

        self.assertIsNone(evaluate_dom_probe(NavigatingPage()))


if __name__ == "__main__":
    unittest.main()
