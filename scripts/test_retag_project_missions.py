#!/usr/bin/env python3
"""Tests for the project retag transformation.

The interesting cases are all about what the script must *refuse* to do: eat a
neighbouring family, drop a suffix, or change something twice.
"""

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "retag", Path(__file__).with_name("retag_project_missions.py")
)
retag = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retag)


def mission(mission_id, project, track=None):
    return {"id": mission_id, "project": project, "track": track}


class PlanChangeTests(unittest.TestCase):
    def test_suffix_moves_into_the_track(self):
        change = retag.plan_change(mission("m1", "verity-phase1d", "core-c3"), "verity")
        self.assertEqual(change, ("m1", "verity", "phase1d/core-c3"))

    def test_suffix_becomes_the_track_when_there_was_none(self):
        change = retag.plan_change(mission("m2", "verity-core"), "verity")
        self.assertEqual(change, ("m2", "verity", "core"))

    def test_already_collapsed_is_left_alone(self):
        self.assertIsNone(retag.plan_change(mission("m3", "verity", "core"), "verity"))

    def test_case_variant_is_normalised_without_touching_the_track(self):
        change = retag.plan_change(mission("m4", "Verity", "core"), "verity")
        self.assertEqual(change, ("m4", "verity", "core"))

    def test_a_different_family_is_untouched(self):
        self.assertIsNone(retag.plan_change(mission("m5", "lido-core"), "verity"))

    def test_the_match_is_hyphen_anchored(self):
        # `verityx` shares a prefix but is a different project; collapsing it
        # would silently merge two unrelated bodies of work.
        self.assertIsNone(retag.plan_change(mission("m6", "verityx-core"), "verity"))
        self.assertIsNone(retag.plan_change(mission("m7", "x-verity-core"), "verity"))

    def test_missions_without_a_project_are_skipped(self):
        self.assertIsNone(retag.plan_change(mission("m8", None), "verity"))
        self.assertIsNone(retag.plan_change(mission("m9", "  "), "verity"))

    def test_running_twice_is_a_no_op(self):
        first = retag.plan_change(mission("m10", "verity-phase1d", "core-c3"), "verity")
        after = mission("m10", first[1], first[2])
        self.assertIsNone(retag.plan_change(after, "verity"))

    def test_distinct_suffixes_cannot_collide(self):
        a = retag.plan_change(mission("a", "verity-core", "x"), "verity")
        b = retag.plan_change(mission("b", "verity-phase1d", "x"), "verity")
        self.assertNotEqual(a[2], b[2])


class OverlapGuardTests(unittest.TestCase):
    def test_a_family_that_contains_another_is_rejected(self):
        with self.assertRaises(SystemExit) as caught:
            retag.reject_overlapping(["verity", "verity-benchmark"])
        self.assertIn("prefix", str(caught.exception))

    def test_order_does_not_matter(self):
        with self.assertRaises(SystemExit):
            retag.reject_overlapping(["verity-benchmark", "verity"])

    def test_unrelated_families_are_accepted(self):
        retag.reject_overlapping(["verity", "lido", "beal"])


class JournalTests(unittest.TestCase):
    class FakeApi:
        def __init__(self, fail_ids=()):
            self.calls = []
            self.fail_ids = set(fail_ids)

        def set_project(self, mission_id, project, track):
            if mission_id in self.fail_ids:
                raise RuntimeError("boom")
            self.calls.append((mission_id, project, track))

    def test_journal_records_the_original_values(self):
        api = self.FakeApi()
        before = {"m1": {"project": "verity-core", "track": None}}
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "j.jsonl")
            ok, failed = retag.apply_changes(
                api, [("m1", "verity", "core")], before, path, workers=2
            )
            self.assertEqual((ok, failed), (1, 0))
            entry = json.loads(Path(path).read_text().strip())
        self.assertEqual(entry["from"], {"project": "verity-core", "track": None})
        self.assertEqual(entry["to"], {"project": "verity", "track": "core"})

    def test_one_failure_does_not_abort_the_rest(self):
        api = self.FakeApi(fail_ids={"m1"})
        changes = [("m1", "verity", "a"), ("m2", "verity", "b")]
        before = {c[0]: {"project": "verity-" + c[2], "track": None} for c in changes}
        with tempfile.TemporaryDirectory() as tmp:
            ok, failed = retag.apply_changes(
                api, changes, before, os.path.join(tmp, "j.jsonl"), workers=2
            )
        self.assertEqual((ok, failed), (1, 1))
        self.assertEqual([c[0] for c in api.calls], ["m2"])

    def test_undo_restores_from_the_journal(self):
        api = self.FakeApi()
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "j.jsonl")
            Path(path).write_text(json.dumps({
                "id": "m1",
                "from": {"project": "verity-core", "track": None},
                "to": {"project": "verity", "track": "core"},
            }) + "\n")
            retag.undo(api, path, workers=1)
        self.assertEqual(api.calls, [("m1", "verity-core", None)])


class SuggestionTests(unittest.TestCase):
    def test_single_slug_roots_are_not_proposed_as_families(self):
        from collections import Counter

        counts = Counter({"oraxen": 5, "verity": 10, "verity-core": 3})
        roots = [root for root, _ in retag.suggest_families(counts)]
        self.assertIn("verity", roots)
        self.assertNotIn("oraxen", roots)


if __name__ == "__main__":
    unittest.main(verbosity=2)
