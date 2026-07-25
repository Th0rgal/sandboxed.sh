#!/usr/bin/env python3

import json
import re
import shutil
import tempfile
import unittest
from pathlib import Path

from scripts.policy_lint import (
    DRIVER_SOURCE,
    HARNESS_DOC,
    POLICY_DOC,
    POLICY_JSON,
    POLICY_SCHEMA,
    POOL_SOURCE,
    RUNTIME_SOURCE,
    SKILL_DOC,
    lint,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
LINTED_FILES = (
    POLICY_JSON,
    POLICY_SCHEMA,
    POLICY_DOC,
    HARNESS_DOC,
    RUNTIME_SOURCE,
    POOL_SOURCE,
    DRIVER_SOURCE,
    SKILL_DOC,
)


class PolicyLintRepoTest(unittest.TestCase):
    def test_repository_policy_is_clean(self):
        self.assertEqual(lint(REPO_ROOT), [])


class PolicyLintMutationTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        for relative in LINTED_FILES:
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(REPO_ROOT / relative, target)

    def tearDown(self):
        self._tmp.cleanup()

    def mutate_policy(self, mutate):
        path = self.root / POLICY_JSON
        policy = json.loads(path.read_text(encoding="utf-8"))
        mutate(policy)
        path.write_text(json.dumps(policy), encoding="utf-8")

    def assert_error(self, fragment):
        errors = lint(self.root)
        self.assertTrue(
            any(fragment in error for error in errors),
            f"expected an error containing {fragment!r}, got {errors!r}",
        )

    def test_clean_copy_passes(self):
        self.assertEqual(lint(self.root), [])

    def test_auth_failure_must_never_retry(self):
        self.mutate_policy(
            lambda p: p["retry"]["auth_failure"].__setitem__("max_automatic_retries", 1)
        )
        self.assert_error("retry.auth_failure.max_automatic_retries")

    def test_auth_quarantine_must_match_pool_runtime(self):
        self.mutate_policy(
            lambda p: p["retry"]["auth_failure"].__setitem__(
                "slot_quarantine_secs", 900
            )
        )
        self.assert_error("retry.auth_failure.slot_quarantine_secs")

    def test_global_browser_launch_must_not_quarantine_a_profile(self):
        self.mutate_policy(
            lambda p: p["retry"]["browser_launch"].__setitem__(
                "quarantine_selected_profile", True
            )
        )
        self.assert_error("retry.browser_launch.quarantine_selected_profile")

    def test_compatibility_retry_is_exactly_once(self):
        self.mutate_policy(
            lambda p: p["retry"]["compatibility_failure"].__setitem__(
                "max_automatic_retries", 2
            )
        )
        self.assert_error("retry.compatibility_failure.max_automatic_retries")

    def test_compatibility_retry_requires_different_slot(self):
        self.mutate_policy(
            lambda p: p["retry"]["compatibility_failure"].__setitem__(
                "require_different_slot", False
            )
        )
        self.assert_error("require_different_slot")

    def test_capacity_must_stay_live(self):
        self.mutate_policy(lambda p: p["capacity"].__setitem__("static_limit", 6))
        self.assert_error("capacity.static_limit")

    def test_acquire_strategy_must_match_health_aware_pool(self):
        self.mutate_policy(
            lambda p: p["capacity"].__setitem__("acquire_strategy", "first_free_slot")
        )
        self.assert_error("capacity.acquire_strategy")

    def test_source_of_truth_paths_are_pinned(self):
        self.mutate_policy(
            lambda p: p["source_of_truth"].__setitem__(
                "runtime", "src/api/runners/chatgpt_ui.rs"
            )
        )
        self.assert_error("source_of_truth.runtime")

    def test_writers_must_not_be_concurrent(self):
        self.mutate_policy(
            lambda p: p["writers"].__setitem__("max_concurrent_per_workspace", 2)
        )
        self.assert_error("writers.max_concurrent_per_workspace")

    def test_boolean_const_rejects_integer_zero(self):
        self.mutate_policy(
            lambda p: p["writers"].__setitem__("chatgpt_ui_may_write", 0)
        )
        self.assert_error("expected const False, got 0")

    def test_integer_invariant_rejects_boolean_true(self):
        self.mutate_policy(
            lambda p: p["writers"].__setitem__("max_concurrent_per_workspace", True)
        )
        self.assert_error("expected 1, got True")

    def test_pro_lane_must_be_read_only(self):
        self.mutate_policy(
            lambda p: p["lanes"]["read_only_pro"].__setitem__("writer", True)
        )
        self.assert_error("lanes.read_only_pro.writer")

    def test_pro_lane_model_is_pinned(self):
        self.mutate_policy(
            lambda p: p["lanes"]["read_only_pro"].__setitem__("model", "gpt-4o")
        )
        self.assert_error("lanes.read_only_pro.model")

    def test_lean_validation_must_stay_independent(self):
        self.mutate_policy(
            lambda p: p["lean"].__setitem__("validator_must_differ_from_writer", False)
        )
        self.assert_error("lean.validator_must_differ_from_writer")

    def test_unexpected_key_is_rejected(self):
        self.mutate_policy(lambda p: p.__setitem__("extra", {"surprise": True}))
        self.assert_error("unexpected key 'extra'")

    def test_timeout_limits_must_match_runtime(self):
        self.mutate_policy(
            lambda p: p["runtime_limits"]["timeout_secs"].__setitem__("max", 7200)
        )
        self.assert_error("runtime clamp")

    def test_artifact_limits_must_match_runtime(self):
        self.mutate_policy(
            lambda p: p["runtime_limits"]["artifacts_per_turn"].__setitem__(
                "max_files", 4
            )
        )
        self.assert_error("max_files")

    def test_compatibility_signal_must_match_driver(self):
        driver = self.root / DRIVER_SOURCE
        driver.write_text(
            driver.read_text(encoding="utf-8").replace(
                'COMPAT_VERSION = "chatgpt-ui-v2"', 'COMPAT_VERSION = "chatgpt-ui-v3"'
            ),
            encoding="utf-8",
        )
        self.assert_error("retry.compatibility_failure.signal")

    def test_stale_harness_doc_timeout_range_is_caught(self):
        doc = self.root / HARNESS_DOC
        doc.write_text(
            doc.read_text(encoding="utf-8").replace(
                "must be between 30–86400 seconds", "must be between 30–7200 seconds"
            ),
            encoding="utf-8",
        )
        self.assert_error("states accepted range 30-7200")

    def test_policy_doc_version_must_match(self):
        doc = self.root / POLICY_DOC
        doc.write_text(
            re.sub(
                r"^Version: .*$",
                "Version: 0.0.1",
                doc.read_text(encoding="utf-8"),
                count=1,
                flags=re.MULTILINE,
            ),
            encoding="utf-8",
        )
        self.assert_error("version 0.0.1")

    def test_policy_doc_runtime_table_must_match(self):
        doc = self.root / POLICY_DOC
        doc.write_text(
            doc.read_text(encoding="utf-8").replace(
                "| `timeout_secs` default | 14400 |",
                "| `timeout_secs` default | 900 |",
            ),
            encoding="utf-8",
        )
        self.assert_error("timeout_secs default")

    def test_skill_must_declare_matching_policy_version(self):
        skill = self.root / SKILL_DOC
        skill.write_text(
            skill.read_text(encoding="utf-8").replace(
                "policy_version: 1.0.0", "policy_version: 0.9.0"
            ),
            encoding="utf-8",
        )
        self.assert_error("policy_version 0.9.0")

    def test_skill_without_version_fails(self):
        skill = self.root / SKILL_DOC
        content = skill.read_text(encoding="utf-8")
        content = content.replace("version: 1.0.0\n", "", 1)
        skill.write_text(content, encoding="utf-8")
        self.assert_error("missing frontmatter 'version:'")


if __name__ == "__main__":
    unittest.main()
