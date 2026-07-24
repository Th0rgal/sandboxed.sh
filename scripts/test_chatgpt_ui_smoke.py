#!/usr/bin/env python3

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SMOKE = ROOT / "scripts" / "chatgpt_ui_smoke.sh"


class ChatGptUiSmokeTests(unittest.TestCase):
    def test_uses_configured_python_and_driver(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            profile = root / "profile"
            profile.mkdir()
            driver = root / "driver.py"
            driver.write_text(
                "import json, sys\n"
                "request = json.loads(sys.stdin.readline())\n"
                "assert request['model'] == 'Visible model'\n"
                "print(json.dumps({'type': 'complete', 'content': "
                "'SANDBOXED_CHATGPT_UI_SMOKE_OK'}))\n",
                encoding="utf-8",
            )
            artifact = root / "result.json"
            env = {
                **os.environ,
                "CHATGPT_UI_PYTHON": sys.executable,
                "CHATGPT_UI_DRIVER": str(driver),
            }

            completed = subprocess.run(
                [str(SMOKE), str(profile), "Visible model", str(artifact)],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertTrue(json.loads(completed.stdout)["ok"])
            self.assertTrue(json.loads(artifact.read_text(encoding="utf-8"))["ok"])

    def test_rejects_missing_configured_python(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            profile = Path(temp_dir) / "profile"
            profile.mkdir()
            env = {
                **os.environ,
                "CHATGPT_UI_PYTHON": str(Path(temp_dir) / "missing-python"),
            }

            completed = subprocess.run(
                [str(SMOKE), str(profile)],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )

            self.assertEqual(completed.returncode, 2)
            self.assertIn("Python executable not found", completed.stderr)


if __name__ == "__main__":
    unittest.main()
