"""Protocol and audio-handling tests for the Orb voice worker (stdlib only).

    python3 -m unittest orb/voice/test_worker.py -v

These run anywhere; the real MLX backend is exercised on a Mac by smoke.py.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import struct
import subprocess
import sys
import unittest
import wave
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import worker  # noqa: E402


def make_wav(seconds=0.5, rate=16000, channels=1, width=2, amplitude=8000):
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(width)
        wf.setframerate(rate)
        n = int(seconds * rate)
        if width == 2:
            frame = struct.pack("<h", amplitude) * channels
        else:
            frame = bytes([128 + amplitude // 256]) * channels
        wf.writeframes(frame * n)
    return buf.getvalue()


def frame(req: dict, payload: bytes = b"") -> bytes:
    if payload:
        req = {**req, "audio_bytes": len(payload)}
    return (json.dumps(req) + "\n").encode() + payload


def run_frames(*frames: bytes):
    out = io.BytesIO()
    code = worker.serve(io.BytesIO(b"".join(frames)), out, worker.Worker(worker.FakeBackend()))
    lines = [json.loads(l) for l in out.getvalue().decode().splitlines() if l.strip()]
    return code, lines


class DecodeWav(unittest.TestCase):
    def test_mono_16bit(self):
        samples, rate = worker.decode_wav(make_wav(0.25))
        self.assertEqual(rate, 16000)
        self.assertEqual(len(samples), 4000)
        self.assertEqual(samples[0], 8000)

    def test_stereo_is_averaged(self):
        samples, rate = worker.decode_wav(make_wav(0.1, channels=2))
        self.assertEqual(len(samples), 1600)
        self.assertEqual(samples[0], 8000)

    def test_other_rates_pass_through(self):
        samples, rate = worker.decode_wav(make_wav(0.1, rate=48000))
        self.assertEqual(rate, 48000)
        self.assertEqual(len(samples), 4800)

    def test_rejects_8bit(self):
        with self.assertRaises(worker.WorkerError) as cm:
            worker.decode_wav(make_wav(0.1, width=1))
        self.assertEqual(cm.exception.code, "bad_wav")

    def test_rejects_garbage(self):
        with self.assertRaises(worker.WorkerError) as cm:
            worker.decode_wav(b"not a wav at all")
        self.assertEqual(cm.exception.code, "bad_wav")

    def test_duration_bound(self):
        samples, rate = worker.decode_wav(make_wav(2.0))
        with mock.patch.dict(os.environ, {"ORB_VOICE_MAX_SECS": "1"}):
            with self.assertRaises(worker.WorkerError) as cm:
                worker.check_duration(samples, rate)
        self.assertEqual(cm.exception.code, "audio_too_long")
        self.assertAlmostEqual(worker.check_duration(samples, rate), 2.0)


class Protocol(unittest.TestCase):
    def test_hello_reports_pins_and_languages(self):
        code, lines = run_frames(frame({"id": 1, "op": "hello"}))
        self.assertEqual(code, 0)
        self.assertEqual(lines[0]["id"], 1)
        self.assertTrue(lines[0]["ok"])
        r = lines[0]["result"]
        self.assertEqual(r["protocol"], 1)
        self.assertEqual(r["model_revision"], "553445e84959f9ec3fcd43443bce75ea05c400f3")
        self.assertEqual(len(r["languages"]), 14)
        self.assertEqual(r["backend"], "fake")

    def test_transcribe_round_trip_and_lazy_load(self):
        wav = make_wav(0.5)
        code, lines = run_frames(
            frame({"id": 1, "op": "status"}),
            frame({"id": 2, "op": "transcribe", "language": "fr"}, wav),
            frame({"id": 3, "op": "status"}),
            frame({"id": 4, "op": "shutdown"}),
        )
        self.assertEqual(code, 0)
        self.assertFalse(lines[0]["result"]["loaded"])
        res = lines[1]
        self.assertTrue(res["ok"], res)
        self.assertEqual(res["result"]["language"], "fr")
        self.assertAlmostEqual(res["result"]["duration_secs"], 0.5)
        self.assertIn("[fake fr]", res["result"]["text"])
        self.assertTrue(lines[2]["result"]["loaded"])
        self.assertEqual(lines[2]["result"]["transcriptions"], 1)
        self.assertEqual(lines[3]["result"], {"bye": True})

    def test_silence_yields_empty_text(self):
        code, lines = run_frames(frame({"id": 1, "op": "transcribe", "language": "en"}, make_wav(0.5, amplitude=0)))
        self.assertEqual(lines[0]["result"]["text"], "")

    def test_too_short_skips_inference(self):
        code, lines = run_frames(frame({"id": 1, "op": "transcribe", "language": "en"}, make_wav(0.05)))
        self.assertTrue(lines[0]["ok"])
        self.assertEqual(lines[0]["result"]["text"], "")

    def test_unsupported_language(self):
        code, lines = run_frames(frame({"id": 7, "op": "transcribe", "language": "xx"}, make_wav(0.2)))
        self.assertFalse(lines[0]["ok"])
        self.assertEqual(lines[0]["id"], 7)
        self.assertEqual(lines[0]["error"]["code"], "unsupported_language")

    def test_language_is_required_to_be_explicit_default_en(self):
        code, lines = run_frames(frame({"id": 1, "op": "transcribe"}, make_wav(0.2)))
        self.assertEqual(lines[0]["result"]["language"], "en")

    def test_bad_json_keeps_serving(self):
        code, lines = run_frames(b"{not json\n", frame({"id": 2, "op": "hello"}))
        self.assertFalse(lines[0]["ok"])
        self.assertEqual(lines[0]["error"]["code"], "bad_request")
        self.assertTrue(lines[1]["ok"])

    def test_unknown_op(self):
        code, lines = run_frames(frame({"id": 1, "op": "dance"}))
        self.assertEqual(lines[0]["error"]["code"], "bad_request")

    def test_truncated_payload_is_reported(self):
        wav = make_wav(0.2)
        code, lines = run_frames(frame({"id": 1, "op": "transcribe", "language": "en"}, wav)[:-10])
        self.assertFalse(lines[0]["ok"])
        self.assertEqual(lines[0]["error"]["code"], "bad_request")
        self.assertEqual(code, 0)  # EOF after the broken frame ends the loop cleanly

    def test_payload_over_hard_cap_is_rejected(self):
        code, lines = run_frames(frame({"id": 1, "op": "transcribe", "language": "en", "audio_bytes": 10**9}))
        self.assertEqual(lines[0]["error"]["code"], "audio_too_long")

    def test_unload_then_reload(self):
        wav = make_wav(0.2)
        code, lines = run_frames(
            frame({"id": 1, "op": "load"}),
            frame({"id": 2, "op": "unload"}),
            frame({"id": 3, "op": "status"}),
            frame({"id": 4, "op": "transcribe", "language": "en"}, wav),
        )
        self.assertTrue(lines[0]["result"]["loaded"])
        self.assertFalse(lines[1]["result"]["loaded"])
        self.assertFalse(lines[2]["result"]["loaded"])
        self.assertTrue(lines[3]["ok"])


class Subprocess(unittest.TestCase):
    """The real stdio path: pipes, framing, and exit on stdin close."""

    def test_child_process_round_trip(self):
        env = dict(os.environ, ORB_VOICE_FAKE="1", PYTHONUNBUFFERED="1")
        p = subprocess.Popen([sys.executable, os.path.join(HERE, "worker.py")], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        wav = make_wav(0.3)
        p.stdin.write(frame({"id": 1, "op": "hello"}))
        p.stdin.write(frame({"id": 2, "op": "transcribe", "language": "de"}, wav))
        p.stdin.flush()
        hello = json.loads(p.stdout.readline())
        res = json.loads(p.stdout.readline())
        self.assertEqual(hello["result"]["protocol"], 1)
        self.assertIn("[fake de]", res["result"]["text"])
        p.stdin.close()
        self.assertEqual(p.wait(timeout=10), 0)
        self.assertIn(b"stdin closed", p.stderr.read())
        p.stdout.close()
        p.stderr.close()

    def test_model_dir_env_is_honoured(self):
        env = dict(os.environ, ORB_VOICE_FAKE="1", ORB_VOICE_MODEL_DIR="/nonexistent/snapshot")
        out = subprocess.run([sys.executable, os.path.join(HERE, "worker.py")], input=frame({"id": 1, "op": "hello"}), capture_output=True, env=env, timeout=20)
        hello = json.loads(out.stdout.splitlines()[0])
        self.assertEqual(hello["result"]["model_dir"], "/nonexistent/snapshot")
        self.assertFalse(hello["result"]["model_present"])


class ModelPaths(unittest.TestCase):
    def test_default_model_dir_follows_hf_cache(self):
        with mock.patch.dict(os.environ, {"HF_HOME": "/x/hf"}, clear=False):
            os.environ.pop("HF_HUB_CACHE", None)
            os.environ.pop("ORB_VOICE_MODEL_DIR", None)
            self.assertEqual(
                worker.resolve_model_dir(),
                "/x/hf/hub/models--MarkChen1214--cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit/snapshots/553445e84959f9ec3fcd43443bce75ea05c400f3",
            )

    def test_real_backend_reports_missing_model_without_importing_mlx(self):
        with mock.patch.dict(os.environ, {"ORB_VOICE_MODEL_DIR": "/nonexistent/snapshot"}):
            b = worker.MlxBackend()
            with self.assertRaises(worker.WorkerError) as cm:
                b.load()
        self.assertEqual(cm.exception.code, "model_missing")


class VendoredPatch(unittest.TestCase):
    def test_body_matches_upstream_hash(self):
        path = os.path.join(HERE, "mlx_audio_cohere_quant_patch.py")
        with open(path, "rb") as f:
            text = f.read()
        header, _, body = text.partition(b'"""Runtime fixes')
        self.assertIn(b"sha256   91beeed75b1c6de97c33f30a48ac508bc22eac0689ee792e8204e7dc9ad6313a", header)
        digest = hashlib.sha256(b'"""Runtime fixes' + body).hexdigest()
        self.assertEqual(digest, "91beeed75b1c6de97c33f30a48ac508bc22eac0689ee792e8204e7dc9ad6313a")

    def test_patch_defines_apply_patch_without_importing_mlx_at_module_level(self):
        # apply_patch() must exist; importing the module needs mlx, so parse only.
        import ast

        with open(os.path.join(HERE, "mlx_audio_cohere_quant_patch.py")) as f:
            tree = ast.parse(f.read())
        names = {n.name for n in tree.body if isinstance(n, ast.FunctionDef)}
        self.assertIn("apply_patch", names)


if __name__ == "__main__":
    unittest.main()
