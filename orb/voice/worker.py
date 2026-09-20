#!/usr/bin/env python3
"""Orb local voice worker.

A persistent child process the Orb desktop app talks to over stdio. It loads
the pinned MLX build of Cohere Transcribe once, keeps it warm, and transcribes
one utterance at a time. Nothing here opens a socket; audio arrives as bytes
on stdin and text leaves as JSON on stdout. Audio never leaves the machine.

Protocol v1 (one JSON object per line, UTF-8):

    request  {"id": 1, "op": "hello"}
             {"id": 2, "op": "load"}
             {"id": 3, "op": "status"}
             {"id": 4, "op": "transcribe", "language": "en", "audio_bytes": N}
                 ... immediately followed by exactly N raw bytes: a PCM WAV
                 file (16-bit, mono preferred; other rates are resampled).
             {"id": 5, "op": "unload"}
             {"id": 6, "op": "shutdown"}
    response {"id": 1, "ok": true, "result": {...}}
             {"id": 1, "ok": false, "error": {"code": "...", "message": "..."}}

stdout carries protocol frames only; diagnostics go to stderr. Requests are
handled strictly in order, so inference is serialized by construction.

Framing is bounded: a header line longer than MAX_HEADER_BYTES or an
audio_bytes claim over the hard cap is a protocol violation. The worker
answers with an error and exits (the stream cannot be re-framed safely, and
the Rust side restarts it), rather than trying to read the oversized body.

stderr becomes a persistent log file (<voice home>/logs/worker.log). Never
write transcripts or audio there: durations and timings only.

Environment:
    ORB_VOICE_MODEL_DIR   local snapshot directory of the pinned model.
    ORB_VOICE_MAX_SECS    longest utterance accepted (default and ceiling 120).
    ORB_VOICE_WARMUP      "0" skips the post-load warm-up inference.
    ORB_VOICE_FAKE        "1" uses a stub backend (tests / Linux CI); no MLX.
"""

from __future__ import annotations

import os

# The worker never reaches the network: the installer downloads the snapshot
# and everything else is local. Force this before any Hugging Face / MLX
# import so a stray default cannot turn a transcription into a download.
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"

import array
import io
import json
import math
import platform
import signal
import sys
import time
import wave
from typing import Any, BinaryIO, Dict, Optional, Tuple

PROTOCOL_VERSION = 1

MODEL_REPO = "MarkChen1214/cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit"
MODEL_REVISION = "553445e84959f9ec3fcd43443bce75ea05c400f3"

# Languages accepted by the pinned checkpoint (config.json: supported_languages).
# The model has no auto-detection: every request names one of these.
SUPPORTED_LANGUAGES = (
    "en", "fr", "de", "es", "it", "pt", "nl", "pl", "el", "ar", "ja", "zh", "vi", "ko",
)

DEFAULT_MAX_SECS = 120.0
MIN_SECS = 0.15
TARGET_SAMPLE_RATE = 16000
# Longest JSON header line the framing accepts (a real header is < 200 bytes).
MAX_HEADER_BYTES = 64 * 1024

# Silence gate, applied before the model sees anything. The checkpoint
# hallucinates on digital silence (two seconds of zeros reliably yields
# "Thank you."), so utterances with no signal are answered with "" locally.
# Thresholds are deliberately far below anything a live microphone captures
# (room noise through AGC peaks in the hundreds); quiet speech passes.
SILENCE_PEAK = 64  # int16 units, about -54 dBFS
SILENCE_RMS = 16.0  # int16 units, about -66 dBFS


class WorkerError(Exception):
    def __init__(self, code: str, message: str, req_id: Any = None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.req_id = req_id  # set when the failing frame's id is known before dispatch


class ProtocolError(WorkerError):
    """The stream can no longer be trusted to be framed; answer, then exit."""


def log(msg: str) -> None:
    sys.stderr.write(f"[orb-voice] {msg}\n")
    sys.stderr.flush()


# --------------------------------------------------------------------------
# Paths

def default_model_dir() -> str:
    hub = os.environ.get("HF_HUB_CACHE")
    if not hub:
        hub = os.path.join(os.environ.get("HF_HOME") or os.path.expanduser("~/.cache/huggingface"), "hub")
    repo_dir = "models--" + MODEL_REPO.replace("/", "--")
    return os.path.join(hub, repo_dir, "snapshots", MODEL_REVISION)


def resolve_model_dir() -> str:
    return os.environ.get("ORB_VOICE_MODEL_DIR") or default_model_dir()


def model_present(model_dir: str) -> bool:
    return all(os.path.exists(os.path.join(model_dir, f)) for f in ("config.json", "model.safetensors"))


def max_seconds() -> float:
    """Utterance limit from the environment, clamped to (0, DEFAULT_MAX_SECS].

    The value also sizes the framing hard cap, so it must be finite and
    positive; anything else ("inf", "nan", "-5", "1e9", garbage) falls back
    to the default rather than lifting the bound.
    """
    raw = os.environ.get("ORB_VOICE_MAX_SECS")
    if raw is None:
        return DEFAULT_MAX_SECS
    try:
        secs = float(raw)
    except ValueError:
        return DEFAULT_MAX_SECS
    if not math.isfinite(secs) or secs <= 0:
        return DEFAULT_MAX_SECS
    return min(secs, DEFAULT_MAX_SECS)


# --------------------------------------------------------------------------
# Audio

def decode_wav(data: bytes) -> Tuple[array.array, int]:
    """Return (mono int16 samples, sample_rate) from PCM WAV bytes.

    Only 16-bit PCM is accepted; that is what the Orb recorder produces. Multi
    channel input is averaged down to mono. Stdlib only, no numpy needed here.
    """
    try:
        with wave.open(io.BytesIO(data), "rb") as wf:
            channels = wf.getnchannels()
            width = wf.getsampwidth()
            rate = wf.getframerate()
            frames = wf.getnframes()
            raw = wf.readframes(frames)
    except (wave.Error, EOFError) as e:
        raise WorkerError("bad_wav", f"not a PCM WAV file: {e}") from None
    if width != 2:
        raise WorkerError("bad_wav", f"expected 16-bit PCM, got {width * 8}-bit")
    if channels < 1 or rate <= 0:
        raise WorkerError("bad_wav", "invalid channel count or sample rate")
    samples = array.array("h")
    samples.frombytes(raw[: (len(raw) // (2 * channels)) * 2 * channels])
    if sys.byteorder != "little":
        samples.byteswap()
    if channels > 1:
        mono = array.array("h", bytes(len(samples) // channels * 2))
        for i in range(len(mono)):
            base = i * channels
            mono[i] = int(sum(samples[base : base + channels]) / channels)
        samples = mono
    return samples, rate


def check_duration(samples: array.array, rate: int) -> float:
    secs = len(samples) / float(rate)
    if secs > max_seconds():
        raise WorkerError("audio_too_long", f"{secs:.1f}s exceeds the {max_seconds():.0f}s limit")
    return secs


def signal_level(samples: array.array) -> Tuple[int, float]:
    """(peak, rms) of int16 samples, in int16 units."""
    if not samples:
        return 0, 0.0
    peak = max(abs(max(samples)), abs(min(samples)))
    if hasattr(math, "sumprod"):  # 3.12+: C speed over the whole utterance
        energy = math.sumprod(samples, samples)
    else:
        energy = sum(s * s for s in samples)
    return peak, math.sqrt(energy / len(samples))


def is_silent(samples: array.array) -> bool:
    """True only when there is essentially no signal at all; see SILENCE_*."""
    peak, rms = signal_level(samples)
    return peak <= SILENCE_PEAK and rms <= SILENCE_RMS


# --------------------------------------------------------------------------
# Backends

class FakeBackend:
    """Deterministic stand-in so the protocol can be tested without MLX."""

    name = "fake"

    def __init__(self) -> None:
        self.loaded = False
        self.model_dir = resolve_model_dir()

    def load(self) -> Dict[str, Any]:
        self.loaded = True
        return {"load_secs": 0.0, "warmup_secs": 0.0}

    def transcribe(self, samples: array.array, rate: int, language: str, punctuation: bool) -> str:
        secs = len(samples) / float(rate)
        peak = max((abs(s) for s in samples), default=0)
        if peak == 0:
            return ""
        return f"[fake {language}] {secs:.2f}s peak={peak}"

    def unload(self) -> None:
        self.loaded = False

    def stats(self) -> Dict[str, Any]:
        return {"peak_memory_bytes": 0}


class MlxBackend:
    """The real thing: mlx-audio + the vendored Cohere quantization patch."""

    name = "mlx"

    def __init__(self) -> None:
        self.loaded = False
        self.model = None
        self.model_dir = resolve_model_dir()

    def _import(self):
        try:
            import mlx.core as mx  # noqa: F401
            import numpy as np  # noqa: F401
        except ImportError as e:
            raise WorkerError(
                "deps_missing",
                f"mlx / numpy are not importable from {sys.executable}: {e}. Run orb/voice/install.sh.",
            ) from None
        try:
            # Vendored copy of the model card's loader patch; it must be
            # installed before mlx_audio.stt.load builds the model.
            sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
            from mlx_audio_cohere_quant_patch import apply_patch
            apply_patch()
            from mlx_audio.stt import load
        except ImportError as e:
            raise WorkerError(
                "deps_missing",
                f"mlx-audio is not importable from {sys.executable}: {e}. Run orb/voice/install.sh.",
            ) from None
        return load

    def load(self) -> Dict[str, Any]:
        if self.loaded:
            return {"load_secs": 0.0, "warmup_secs": 0.0, "cached": True}
        if not model_present(self.model_dir):
            raise WorkerError(
                "model_missing",
                f"model snapshot not found at {self.model_dir}. Run orb/voice/install.sh --download-model.",
            )
        load = self._import()
        t0 = time.perf_counter()
        self.model = load(self.model_dir)
        load_secs = time.perf_counter() - t0
        warm = 0.0
        if os.environ.get("ORB_VOICE_WARMUP", "1") != "0":
            import numpy as np

            t1 = time.perf_counter()
            silence = np.zeros(TARGET_SAMPLE_RATE // 2, dtype=np.float32)
            try:
                self.model.generate(audio=silence, sample_rate=TARGET_SAMPLE_RATE, language="en", punctuation=True)
            except Exception as e:  # warm-up is best effort
                log(f"warm-up failed: {e}")
            warm = time.perf_counter() - t1
        self.loaded = True
        log(f"model loaded in {load_secs:.3f}s (warm-up {warm:.3f}s) from {self.model_dir}")
        return {"load_secs": round(load_secs, 3), "warmup_secs": round(warm, 3), "cached": False}

    def transcribe(self, samples: array.array, rate: int, language: str, punctuation: bool) -> str:
        import numpy as np

        audio = np.frombuffer(samples.tobytes(), dtype=np.int16).astype(np.float32) / 32768.0
        result = self.model.generate(audio=audio, sample_rate=rate, language=language, punctuation=punctuation)
        return (result.text or "").strip()

    def unload(self) -> None:
        self.model = None
        self.loaded = False
        try:
            import gc

            import mlx.core as mx

            gc.collect()
            mx.clear_cache()
        except Exception:
            pass

    def stats(self) -> Dict[str, Any]:
        try:
            import mlx.core as mx

            return {"peak_memory_bytes": int(mx.get_peak_memory()), "active_memory_bytes": int(mx.get_active_memory())}
        except Exception:
            return {"peak_memory_bytes": None}


def make_backend():
    if os.environ.get("ORB_VOICE_FAKE") == "1":
        return FakeBackend()
    return MlxBackend()


# --------------------------------------------------------------------------
# Request handling

class Worker:
    def __init__(self, backend=None) -> None:
        self.backend = backend or make_backend()
        self.started = time.time()
        self.requests = 0
        self.transcriptions = 0
        self.last_infer_secs: Optional[float] = None

    def handle(self, req: Dict[str, Any], payload: bytes) -> Dict[str, Any]:
        op = req.get("op")
        self.requests += 1
        if op == "hello":
            return {
                "protocol": PROTOCOL_VERSION,
                "backend": self.backend.name,
                "python": platform.python_version(),
                "platform": sys.platform,
                "machine": platform.machine(),
                "model_repo": MODEL_REPO,
                "model_revision": MODEL_REVISION,
                "model_dir": self.backend.model_dir,
                "model_present": model_present(self.backend.model_dir),
                "languages": list(SUPPORTED_LANGUAGES),
                "max_seconds": max_seconds(),
            }
        if op == "load":
            return {"loaded": True, **self.backend.load()}
        if op == "status":
            return {
                "loaded": self.backend.loaded,
                "uptime_secs": round(time.time() - self.started, 1),
                "requests": self.requests,
                "transcriptions": self.transcriptions,
                "last_infer_secs": self.last_infer_secs,
                **self.backend.stats(),
            }
        if op == "transcribe":
            return self.transcribe(req, payload)
        if op == "unload":
            self.backend.unload()
            return {"loaded": False}
        if op == "shutdown":
            return {"bye": True}
        raise WorkerError("bad_request", f"unknown op {op!r}")

    def transcribe(self, req: Dict[str, Any], payload: bytes) -> Dict[str, Any]:
        language = req.get("language") or "en"
        if not isinstance(language, str) or language not in SUPPORTED_LANGUAGES:
            raise WorkerError("unsupported_language", f"language {language!r} is not one of {', '.join(SUPPORTED_LANGUAGES)}")
        punctuation = bool(req.get("punctuation", True))
        if not payload:
            raise WorkerError("bad_request", "transcribe needs audio_bytes")
        samples, rate = decode_wav(payload)
        secs = check_duration(samples, rate)
        load_info = {}
        if not self.backend.loaded:
            load_info = self.backend.load()
        infer = 0.0
        if secs < MIN_SECS or is_silent(samples):
            # Too short or no signal: the model would only hallucinate.
            text = ""
        else:
            t0 = time.perf_counter()
            text = self.backend.transcribe(samples, rate, language, punctuation)
            infer = time.perf_counter() - t0
        self.transcriptions += 1
        self.last_infer_secs = round(infer, 3)
        # Timings only; the transcript itself must never reach the log file.
        log(f"transcribe {secs:.2f}s audio lang={language} infer={infer:.3f}s chars={len(text)}")
        return {
            "text": text,
            "language": language,
            "duration_secs": round(secs, 3),
            "sample_rate": rate,
            "infer_secs": round(infer, 3),
            "load_secs": load_info.get("load_secs", 0.0),
        }


# --------------------------------------------------------------------------
# Framing

def read_exact(stream: BinaryIO, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            raise WorkerError("bad_request", f"audio payload truncated ({len(buf)}/{n} bytes)")
        buf.extend(chunk)
    return bytes(buf)


def payload_hard_cap() -> int:
    """Largest audio_bytes claim honoured: 4 bytes/sample at 48 kHz, plus headers."""
    return int(max_seconds() * 4 * 48000) + 1_000_000


def read_request(stream: BinaryIO) -> Optional[Tuple[Dict[str, Any], bytes]]:
    """Read one frame. Returns None at EOF.

    Raises WorkerError on a malformed but well-framed request (the loop keeps
    serving) and ProtocolError when the framing itself is violated (an
    overlong header or an oversized payload claim), after which serve() exits.
    """
    line = stream.readline(MAX_HEADER_BYTES + 1)
    if not line:
        return None
    if len(line) > MAX_HEADER_BYTES or (len(line) == MAX_HEADER_BYTES and not line.endswith(b"\n")):
        raise ProtocolError("bad_request", f"header line exceeds {MAX_HEADER_BYTES} bytes")
    line = line.strip()
    if not line:
        return {}, b""
    try:
        req = json.loads(line.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as e:
        raise WorkerError("bad_request", f"malformed JSON frame: {e}") from None
    if not isinstance(req, dict):
        raise WorkerError("bad_request", "frame must be a JSON object")
    n = req.get("audio_bytes", 0)
    if not isinstance(n, int) or n < 0:
        raise WorkerError("bad_request", "audio_bytes must be a non-negative integer")
    if n > payload_hard_cap():
        # Do not read (or allocate) the claimed body: report and let serve()
        # exit, the parent restarts the worker with a clean stream.
        raise ProtocolError("audio_too_long", f"payload of {n} bytes exceeds the hard cap", req.get("id"))
    payload = read_exact(stream, n) if n else b""
    return req, payload


def write_response(stream: BinaryIO, obj: Dict[str, Any]) -> None:
    stream.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
    stream.flush()


def serve(stdin: BinaryIO, stdout: BinaryIO, worker: Optional[Worker] = None) -> int:
    worker = worker or Worker()
    log(f"worker ready pid={os.getpid()} backend={worker.backend.name} python={sys.executable}")
    while True:
        req_id: Any = None
        try:
            frame = read_request(stdin)
            if frame is None:
                log("stdin closed; exiting")
                return 0
            req, payload = frame
            if not req:
                continue
            req_id = req.get("id")
            result = worker.handle(req, payload)
            write_response(stdout, {"id": req_id, "ok": True, "result": result})
            if req.get("op") == "shutdown":
                return 0
        except ProtocolError as e:
            rid = e.req_id if e.req_id is not None else req_id
            write_response(stdout, {"id": rid, "ok": False, "error": {"code": e.code, "message": e.message}})
            log(f"protocol violation, exiting: {e.message}")
            return 2
        except WorkerError as e:
            rid = e.req_id if e.req_id is not None else req_id
            write_response(stdout, {"id": rid, "ok": False, "error": {"code": e.code, "message": e.message}})
        except MemoryError:
            write_response(stdout, {"id": req_id, "ok": False, "error": {"code": "internal", "message": "out of memory"}})
            return 3
        except Exception as e:  # keep serving; report the failure
            log(f"unhandled error: {type(e).__name__}: {e}")
            write_response(
                stdout,
                {"id": req_id, "ok": False, "error": {"code": "internal", "message": f"{type(e).__name__}: {e}"}},
            )


def main() -> int:
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    except (AttributeError, ValueError):
        pass
    return serve(sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    sys.exit(main())
