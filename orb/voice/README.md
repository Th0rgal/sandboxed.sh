# Orb local voice input (macOS, Apple Silicon)

Dictate into the composer. Audio is recorded in the webview, transcribed on the
Mac by the pinned MLX build of Cohere Transcribe, and inserted as editable text.
Nothing is sent anywhere, nothing is auto-submitted.

```
 webview (SolidJS)              Tauri (Rust)                      Python worker (venv)
 ─────────────────              ────────────                      ────────────────────
 VoiceButton in Composer        voice.rs                          voice/worker.py
   getUserMedia → PCM float ──▶ voice_transcribe (raw WAV body) ─▶ JSON line + WAV bytes on stdin
   16 kHz mono 16-bit WAV       spawn-once, serialize, timeouts    apply vendored quant patch
   ◀── text inserted at caret ◀── {text, timings} ◀────────────── mlx_audio.stt.load(snapshot).generate(...)
                                idle reaper frees the model
```

## Files

| Path | Role |
| --- | --- |
| `orb/src/Voice.tsx` | `VoiceButton`: idle → recording → transcribing → `onText`; language chip; cancel/Esc; error bubble. `voiceAvailable()` gates the mic everywhere. |
| `orb/src/voice.ts` | Recorder (ScriptProcessorNode → resample → WAV), Tauri bridge (raw body + `x-language` header), language preference, caret insertion. |
| `orb/src/App.tsx` | Minimal wiring: the composer's empty-state button is the mic when voice exists, dictation lands at the caret, `scope` per conversation. |
| `orb/src-tauri/src/voice.rs` | Worker lifecycle and the five commands; compiles and is unit-tested on Linux, reports `supported` only on macOS/aarch64. |
| `orb/src-tauri/permissions/voice.toml` + `capabilities/default.json` | The app has an ACL manifest, so the commands must be allowed explicitly (`allow-voice`). |
| `orb/src-tauri/Info.plist` | `NSMicrophoneUsageDescription`; Tauri embeds it into the dev binary and merges it into the bundle. |
| `orb/src-tauri/Entitlements.plist` | `com.apple.security.device.audio-input`, referenced from `tauri.conf.json` for packaged builds. |
| `orb/voice/worker.py` | Persistent stdio worker; embedded into the binary with `include_str!` and written to the voice home on spawn, so dev and packaged builds ship the same code. |
| `orb/voice/mlx_audio_cohere_quant_patch.py` | Vendored loader patch from the model card (hash-pinned; header explains what it does). |
| `orb/voice/install.sh` / `requirements.txt` | Reproducible isolated venv + explicit model download. No global installs. |
| `orb/voice/smoke.py` | End-to-end smoke test and micro-benchmark over the real protocol. |
| `orb/voice/test_worker.py` | Stdlib protocol/audio tests (run anywhere). |

## Paths (all overridable)

| What | Default | Override |
| --- | --- | --- |
| Voice home | `~/Library/Application Support/Orb/voice` | `ORB_VOICE_HOME` |
| Python | `<voice home>/.venv/bin/python` | `ORB_VOICE_PYTHON` |
| Model snapshot | `~/.cache/huggingface/hub/models--MarkChen1214--cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit/snapshots/553445e84959f9ec3fcd43443bce75ea05c400f3` | `ORB_VOICE_MODEL_DIR`, `HF_HUB_CACHE`, `HF_HOME` |
| Worker sources | `<voice home>/runtime/` (rewritten on every spawn) | — |
| Worker log | `<voice home>/logs/worker.log` (stderr of the worker) | — |
| Idle release | 10 minutes | `ORB_VOICE_IDLE_SECS` |

Pins: model `MarkChen1214/cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit` @
`553445e84959f9ec3fcd43443bce75ea05c400f3`; mlx-audio git
`77a6cfcaba9fcb246c9302f7196c05147501bd62`. The worker sets `HF_HUB_OFFLINE=1`
and only ever loads the local snapshot directory. The patch must be applied
before `mlx_audio.stt.load`; the worker imports the vendored copy, never the
one inside the cache.

## Install (Mac)

```sh
orb/voice/install.sh --download-model --check
```

Creates the venv with Python 3.10–3.13 (prefers `python3.11`), installs the
pinned `mlx-audio[stt]`, downloads the snapshot into the standard HF cache,
verifies the snapshot's patch hash against the vendored copy, and runs the
smoke test. Re-run with `--recreate` to rebuild the venv.

## Runtime behaviour

- **Lazy, warm, bounded.** The worker starts on the first press of the mic; the
  model is loaded while you are still speaking (`voice_prewarm`), so the first
  utterance hides most of the cold start. One request at a time; the mutex
  around the worker is the serializer. Recordings stop automatically at 120 s;
  Rust rejects anything longer or malformed from the WAV header alone.
- **Timeouts and cancel.** Load ≤ 240 s, transcribe ≤ 90 s, hello ≤ 30 s; a
  watchdog kills the child at the deadline. Cancel while transcribing kills the
  worker (next use pays a ~2 s cold start); cancel while recording just drops
  the audio. Any worker error tears the worker down so the next call starts
  clean.
- **Idle release.** A reaper thread stops the worker after 10 minutes without
  use, giving back the ~1 GB the model keeps resident.
- **Languages.** The checkpoint accepts 14 languages and does not auto-detect:
  en, fr, de, es, it, pt, nl, pl, el, ar, ja, zh, vi, ko. The default is the
  first supported browser locale, else English; the chip next to the mic
  changes it and the choice is remembered (`orb.voiceLang`).
- **Composer.** Dictation is inserted at the caret with the typed draft kept on
  both sides, then focus returns to the textarea. Nothing is sent. A result
  that arrives after the composer switched conversation, was unmounted, or was
  cancelled is dropped. The mic is only rendered when the native side reports
  support, so browsers and non-Mac builds never show it.
- **Errors** are shown in a small bubble over the button: microphone denied /
  missing / busy, runtime or model not installed (with the command to run),
  too long, timed out, no speech detected.

## Protocol (v1)

One JSON object per line on stdin; `transcribe` is followed by exactly
`audio_bytes` raw bytes. One JSON object per line on stdout. Ops: `hello`,
`load`, `status`, `transcribe {language, punctuation, audio_bytes}`, `unload`,
`shutdown`. Errors carry `code` ∈ `bad_request | bad_wav | audio_too_long |
unsupported_language | model_missing | deps_missing | internal`. The Rust side
adds `python_missing | unsupported | timeout | worker_exited | protocol`.

## Verification commands

Anywhere (Linux CI included):

```sh
cd orb
pnpm install --frozen-lockfile
pnpm build && pnpm test                         # 45 tests incl. voice.test.ts / voice.test.tsx
python3 -m unittest voice/test_worker.py -v     # 23 protocol/audio tests, stub backend
python3 voice/smoke.py --fake                   # protocol round trip through a child process
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml # 9 tests incl. a Rust↔worker.py round trip
```

On the Mac (real model):

```sh
orb/voice/install.sh --download-model --check
python3 orb/voice/smoke.py --runs 5             # load time, cold/warm inference, peak MLX memory
python3 orb/voice/smoke.py --wav ~/clip.wav --language fr
cd orb && pnpm tauri dev                        # press the mic, speak, stop, edit, send
tail -f "$HOME/Library/Application Support/Orb/voice/logs/worker.log"
```

Reference numbers from the coordinator's Mac (pinned snapshot, this venv):
model load 1.02 s, bundled 5.44 s demo clip transcribed correctly, cold
utterance 2.32 s / warm 0.35 s, peak MLX memory 0.95 GB.

## macOS verification still to do (cannot run on Linux)

- [ ] `pnpm tauri dev`: first mic press prompts for microphone access; the
      prompt shows the `NSMicrophoneUsageDescription` text (dev binary carries
      the embedded Info.plist).
- [ ] Record → stop → text appears at the caret, draft preserved, nothing sent;
      language chip switches EN/FR and the transcript follows.
- [ ] Esc while recording cancels; click while transcribing cancels; the log
      shows the worker being killed and respawned on the next use.
- [ ] Leave Orb idle > 10 min: worker process gone (`pgrep -f runtime/worker.py`),
      next dictation cold-starts.
- [ ] `pnpm tauri build` with `bundle.active` enabled: packaged app also prompts
      and records (entitlement + usage description in the bundle).
- [ ] Confirm `navigator.mediaDevices` exists under the packaged `tauri://`
      origin (WKWebView treats it as secure; wry grants the capture request).

### Optional shared engine (`voiced`)

When `~/Library/Application Support/md.thomas.voice/voiced.sock` is present,
Orb first connects to `voiced` using the unchanged protocol v1. It checks the
protocol, daemon identity, and pinned model repo/revision before using it.
`ORB_VOICE_SOCKET` overrides the path; `ORB_VOICE_SOCKET=off` forces the private
worker. An absent, stale, or incompatible daemon falls back to Orb's existing
runtime. Orb does not install or manage voiced and does not require Murmure.

The shared handshake permits 240 seconds because it can queue behind another
client's model load. Existing load (240 s) and transcription (90 s when warm)
timeouts are unchanged. Cancellation, errors, and idle release close only Orb's
connection; they never signal the daemon PID. A cancelled shared handshake does
not fall back to starting a private worker. Subsequent requests reconnect.

Capabilities report `shared: true` while connected. Machine metrics label the
resident daemon as **Cohere · shared speech engine**, separate from Orb's native
process tree. This reports the full daemon residency, not an allocation to Orb.
The daemon controls model unloading, so Orb's warm state is its last observation.

The worker, quantization patch, protocol, model pin, and venv location are
unchanged. Do not recreate the shared venv while voiced uses it. Installation
and daemon lifecycle remain in Murmure's `scripts/install-voiced.sh`.

Opt-in native smoke tests (16 kHz mono PCM WAV containing speech):

```sh
cd orb/src-tauri
ORB_VOICE_TEST_WAV=/path/to/speech.wav cargo test voice::tests::installed_voiced_transcribe_cancel_reconnect -- --ignored --exact
ORB_VOICE_SOCKET=off ORB_VOICE_TEST_WAV=/path/to/speech.wav cargo test voice::tests::installed_private_worker_fallback -- --ignored --exact
```

The first connects to the installed daemon and checks transcription, cancel,
reconnection and stable daemon PID. The second explicitly starts and releases
Orb's private worker. Neither test installs or kills voiced.
