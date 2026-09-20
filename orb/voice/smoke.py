#!/usr/bin/env python3
"""Smoke test and micro-benchmark for the Orb voice worker.

Drives worker.py exactly the way the Tauri side does — as a child process over
stdio with the framed protocol — so a passing run means the whole Python half
works on this machine. Prints load time, per-utterance inference time, peak MLX
memory and the transcript.

    python3 orb/voice/smoke.py                         # venv python, demo wav
    python3 orb/voice/smoke.py --wav my.wav --language fr --runs 5
    python3 orb/voice/smoke.py --fake                  # protocol only, no MLX

The default audio is the demo clip shipped inside the pinned model snapshot
(demo/voxpopuli_test_en_demo.wav). With --fake or when no wav is available a
one-second synthetic tone is used.
"""

from __future__ import annotations

import argparse
import io
import json
import math
import os
import struct
import subprocess
import sys
import time
import wave

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import worker  # noqa: E402


def synth_wav(seconds: float = 1.0, rate: int = 16000, freq: float = 440.0) -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(rate)
        n = int(seconds * rate)
        wf.writeframes(b"".join(struct.pack("<h", int(12000 * math.sin(2 * math.pi * freq * i / rate))) for i in range(n)))
    return buf.getvalue()


class Client:
    def __init__(self, python: str, fake: bool):
        env = dict(os.environ, PYTHONUNBUFFERED="1", HF_HUB_OFFLINE="1")
        if fake:
            env["ORB_VOICE_FAKE"] = "1"
        self.proc = subprocess.Popen(
            [python, os.path.join(HERE, "worker.py")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            env=env,
        )
        self.next_id = 0

    def call(self, op: str, payload: bytes = b"", **fields):
        self.next_id += 1
        req = {"id": self.next_id, "op": op, **fields}
        if payload:
            req["audio_bytes"] = len(payload)
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write((json.dumps(req) + "\n").encode())
        if payload:
            self.proc.stdin.write(payload)
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise SystemExit(f"worker exited with {self.proc.wait()} during {op}")
        res = json.loads(line)
        if res.get("id") != self.next_id:
            raise SystemExit(f"response id mismatch: {res}")
        if not res.get("ok"):
            raise SystemExit(f"{op} failed: {res.get('error')}")
        return res["result"]

    def close(self):
        try:
            self.call("shutdown")
        finally:
            self.proc.wait(timeout=10)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--python", default=None, help="interpreter for the worker (default: the Orb voice venv, else this one)")
    ap.add_argument("--wav", default=None, help="16-bit PCM WAV to transcribe")
    ap.add_argument("--language", default="en", choices=worker.SUPPORTED_LANGUAGES)
    ap.add_argument("--runs", type=int, default=3, help="how many times to transcribe (first run is cold)")
    ap.add_argument("--fake", action="store_true", help="use the stub backend (no MLX)")
    ap.add_argument("--json", action="store_true", help="print a machine-readable summary")
    args = ap.parse_args()

    home = os.environ.get("ORB_VOICE_HOME") or os.path.expanduser("~/Library/Application Support/Orb/voice")
    venv_python = os.path.join(home, ".venv", "bin", "python")
    python = args.python or (venv_python if os.path.exists(venv_python) and not args.fake else sys.executable)

    wav_path = args.wav
    if not wav_path and not args.fake:
        candidate = os.path.join(worker.resolve_model_dir(), "demo", "voxpopuli_test_en_demo.wav")
        if os.path.exists(candidate):
            wav_path = candidate
    if wav_path:
        with open(wav_path, "rb") as f:
            audio = f.read()
    else:
        audio = synth_wav()

    print(f"python:  {python}")
    print(f"audio:   {wav_path or 'synthetic 1s tone'} ({len(audio)} bytes)")
    print(f"model:   {worker.resolve_model_dir()}")

    c = Client(python, args.fake)
    summary = {}
    try:
        hello = c.call("hello")
        print(f"hello:   protocol={hello['protocol']} backend={hello['backend']} python={hello['python']} model_present={hello['model_present']}")
        t0 = time.perf_counter()
        load = c.call("load")
        wall = time.perf_counter() - t0
        print(f"load:    {load.get('load_secs')}s model + {load.get('warmup_secs')}s warm-up (wall {wall:.3f}s)")
        summary.update(load_secs=load.get("load_secs"), warmup_secs=load.get("warmup_secs"), load_wall_secs=round(wall, 3))
        runs = []
        for i in range(args.runs):
            t0 = time.perf_counter()
            r = c.call("transcribe", audio, language=args.language)
            wall = time.perf_counter() - t0
            runs.append({"infer_secs": r["infer_secs"], "wall_secs": round(wall, 3)})
            label = "cold" if i == 0 else "warm"
            print(f"run {i + 1} ({label}): {r['duration_secs']}s audio → {r['infer_secs']}s inference (wall {wall:.3f}s)")
        print(f"text:    {r['text']!r}")
        status = c.call("status")
        peak = status.get("peak_memory_bytes")
        print(f"status:  loaded={status['loaded']} peak_memory={peak / 1e9:.3f} GB" if peak else f"status:  loaded={status['loaded']}")
        summary.update(runs=runs, text=r["text"], peak_memory_bytes=peak, duration_secs=r["duration_secs"])
    finally:
        c.close()
    if args.json:
        print(json.dumps(summary, ensure_ascii=False))
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
