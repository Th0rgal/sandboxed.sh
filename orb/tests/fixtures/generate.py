"""Capture real cron records in a temporary HERMES_HOME, without running a scheduler.

Run with PYTHONPATH pointing to Th0rgal/hermes-agent at
882881de28e3b71f330a530d9b1463fac9472ef8, using Python with croniter, pyyaml,
and python-dotenv installed. No provider, session, or live job is contacted.
"""
import json
import os
import tempfile
from pathlib import Path

with tempfile.TemporaryDirectory(prefix="orb-hermes-fixtures-") as home:
    os.environ["HERMES_HOME"] = home
    os.environ["HERMES_TIMEZONE"] = "UTC"
    from cron import jobs

    common = dict(prompt="Summarize the local project notes.", name="Project notes",
                  skills=["project-notes"], model="gpt-5", provider="openai", workdir=home,
                  reasoning_effort="high", failure_deliver="local", deliver="local")
    Path(home, "config.yaml").write_text("model:\n  default: fixture-local-model\n  provider: custom\n  base_url: http://127.0.0.1:9/v1\n")
    snapshot = jobs.create_job(name="Saved local defaults", prompt=common["prompt"], schedule="every 1h")
    assert snapshot["model_snapshot"] == "fixture-local-model"
    assert snapshot["provider_snapshot"] == "custom"
    hourly = jobs.create_job(schedule="every 1h", repeat=8, context_from=["self"], **common)
    weekdays = jobs.create_job(schedule="0 9 * * 1-5", **common)
    once = jobs.create_job(schedule="2099-04-05T09:00:00+02:00", **common)
    updated = jobs.update_job(hourly["id"], {"repeat": {"times": 12, "completed": 3}})
    run = jobs.trigger_job(hourly["id"])
    Path(__file__).with_name("hermes-jobs.json").write_text(json.dumps(
        dict(hourly=hourly, weekdays=weekdays, once=once, updated=updated, run=run, snapshot=snapshot), indent=2) + "\n")
