/**
 * Dictation button for the composer: idle → recording → transcribing → text
 * handed back to the composer (never sent). Only rendered where native voice
 * is available; everywhere else `voiceAvailable()` stays false and the
 * composer shows no microphone.
 */
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import * as Ic from "./icons";
import { hasFocusScope } from "./focusScope";
import {
  VOICE_LANGUAGES,
  VOICE_MAX_SECONDS,
  VoiceError,
  probeVoice,
  setVoiceLanguage,
  startRecording,
  tauriVoiceBridge,
  toVoiceError,
  voiceLanguage,
  type Recorder,
  type RecorderFactory,
  type VoiceBridge,
  type VoiceCapability,
} from "./voice";

const [available, setAvailable] = createSignal(false);
const [capability, setCapability] = createSignal<VoiceCapability | null>(null);
/** True once the native side reported a usable local voice runtime. */
export const voiceAvailable = available;
export const voiceCapability = capability;

let probed = false;
/** Ask the native side once; safe to call from every composer. */
export function ensureVoiceProbe(bridge?: VoiceBridge | null) {
  if (probed) return;
  probed = true;
  void probeVoice(bridge === undefined ? tauriVoiceBridge() : bridge).then((cap) => {
    setCapability(cap);
    setAvailable(!!cap);
  });
}
/** Test hook. */
export function resetVoiceAvailability() {
  probed = false;
  setAvailable(false);
  setCapability(null);
}

export type VoiceStatus = "idle" | "starting" | "recording" | "transcribing";

export function describeVoiceError(e: VoiceError): string {
  switch (e.code) {
    case "permission":
    case "no_device":
    case "busy":
    case "unsupported":
      return e.message;
    case "python_missing":
      return "Voice runtime not installed — run orb/voice/install.sh.";
    case "model_missing":
      return "Speech model not downloaded — run orb/voice/install.sh --download-model.";
    case "deps_missing":
      return "Voice runtime is incomplete — rerun orb/voice/install.sh.";
    case "audio_too_long":
      return `Recording is over the ${VOICE_MAX_SECONDS}s limit.`;
    case "timeout":
      return "Transcription timed out.";
    case "worker_exited":
      return "Transcription stopped unexpectedly.";
    case "unsupported_language":
      return "That language is not supported.";
    default:
      return e.message || "Transcription failed.";
  }
}

const fmt = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

export function VoiceButton(p: {
  /** Receives the dictated text; the composer inserts it and does not send. */
  onText: (text: string) => void;
  /** Identity of the conversation this button belongs to; a result for another scope is dropped. */
  scope?: string;
  /** Recording/transcribing state, so the composer can keep the button mounted while text is typed. */
  onActive?: (active: boolean) => void;
  disabled?: boolean;
  /** Injection points for tests. */
  bridge?: VoiceBridge | null;
  recorder?: RecorderFactory;
}) {
  const bridge = () => (p.bridge === undefined ? tauriVoiceBridge() : p.bridge);
  const record = () => p.recorder ?? startRecording;
  const [status, setStatus] = createSignal<VoiceStatus>("idle");
  const [elapsed, setElapsed] = createSignal(0);
  const [level, setLevel] = createSignal(0);
  const [error, setError] = createSignal<string | null>(null);
  const [langOpen, setLangOpen] = createSignal(false);
  let rec: Recorder | null = null;
  let tick: number | undefined;
  let errTimer: number | undefined;
  let token = 0; // bumps on every start/cancel; late results with an old token are dropped
  let disposed = false;

  const setState = (s: VoiceStatus) => {
    setStatus(s);
    p.onActive?.(s !== "idle");
  };
  const showError = (e: unknown) => {
    const ve = e instanceof VoiceError ? e : toVoiceError(e);
    setError(describeVoiceError(ve));
    clearTimeout(errTimer);
    errTimer = window.setTimeout(() => setError(null), 8000);
  };
  const stopTimer = () => {
    clearInterval(tick);
    tick = undefined;
  };

  const start = async () => {
    const b = bridge();
    if (!b) return;
    setError(null);
    setLangOpen(false);
    const my = ++token;
    setState("starting");
    // Warm the model while the user is still talking so the cold start is
    // hidden behind the utterance. Failures surface when they stop.
    let warmFailure: VoiceError | null = null;
    const warm = b.prewarm().catch((e) => {
      warmFailure = toVoiceError(e);
    });
    try {
      rec = await record()({
        onLevel: setLevel,
        maxSeconds: VOICE_MAX_SECONDS,
        onAutoStop: () => void finish(),
      });
    } catch (e) {
      if (my !== token || disposed) return;
      rec = null;
      setState("idle");
      showError(e);
      return;
    }
    if (my !== token || disposed) {
      rec.cancel();
      rec = null;
      return;
    }
    setState("recording");
    setElapsed(0);
    const t0 = rec.startedAt;
    tick = window.setInterval(() => setElapsed(Date.now() - t0), 250);
    void warm.then(() => {
      if (warmFailure && my === token && status() === "recording") {
        cancelRecording();
        showError(warmFailure);
      }
    });
  };

  const finish = async () => {
    const r = rec;
    const b = bridge();
    if (!r || !b || status() !== "recording") return;
    const my = token;
    const scope = p.scope;
    rec = null;
    stopTimer();
    setState("transcribing");
    try {
      const wav = await r.stop();
      if (my !== token || disposed) return;
      if (wav.length === 0) throw new VoiceError("empty", "Nothing was recorded.");
      const result = await b.transcribe(wav, voiceLanguage());
      if (my !== token || disposed) return; // cancelled or navigated away meanwhile
      if (scope !== p.scope) return; // composer now belongs to another conversation
      const text = result.text.trim();
      if (!text) {
        showError(new VoiceError("empty", "No speech detected."));
      } else {
        p.onText(text);
      }
    } catch (e) {
      if (my !== token || disposed) return;
      showError(e);
    } finally {
      if (my === token && !disposed) setState("idle");
    }
  };

  const cancelRecording = () => {
    token++;
    rec?.cancel();
    rec = null;
    stopTimer();
    setState("idle");
  };

  const cancelTranscribing = () => {
    token++;
    setState("idle");
    void bridge()?.cancel().catch(() => {});
  };

  const cancel = () => {
    if (status() === "recording" || status() === "starting") cancelRecording();
    else if (status() === "transcribing") cancelTranscribing();
  };

  const toggle = () => {
    if (p.disabled) return;
    switch (status()) {
      case "idle":
        void start();
        break;
      case "starting":
        cancelRecording();
        break;
      case "recording":
        void finish();
        break;
      case "transcribing":
        cancelTranscribing();
        break;
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape" || e.defaultPrevented || hasFocusScope()) return;
    if (langOpen()) {
      e.stopPropagation();
      setLangOpen(false);
      return;
    }
    if (status() !== "idle") {
      e.stopPropagation();
      cancel();
    }
  };
  const onPointerDownOutside = () => setLangOpen(false);
  onMount(() => {
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("pointerdown", onPointerDownOutside);
  });
  onCleanup(() => {
    disposed = true;
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("pointerdown", onPointerDownOutside);
    clearTimeout(errTimer);
    if (status() === "transcribing") void bridge()?.cancel().catch(() => {});
    cancelRecording();
  });

  const title = () => {
    switch (status()) {
      case "idle":
        return `Dictate (${voiceLanguage().toUpperCase()})`;
      case "starting":
        return "Starting microphone…";
      case "recording":
        return "Stop recording";
      case "transcribing":
        return "Transcribing… click to cancel";
    }
  };

  return (
    <span class={`voice ${status()}`} onPointerDown={(e) => e.stopPropagation()} onClick={(e) => e.stopPropagation()}>
      <Show when={status() === "idle"}>
        <span class="voice-lang-wrap">
          <button
            type="button"
            class={`voice-lang ${langOpen() ? "on" : ""}`}
            title="Dictation language"
            aria-label="Dictation language"
            aria-haspopup="menu"
            aria-expanded={langOpen()}
            disabled={p.disabled}
            onClick={() => setLangOpen(!langOpen())}
          >
            {voiceLanguage().toUpperCase()}
          </button>
          <Show when={langOpen()}>
            <div class="menu voice-menu" role="menu" aria-label="Dictation language">
              <For each={VOICE_LANGUAGES}>
                {(l) => (
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={l.code === voiceLanguage()}
                    class={`menu-item ${l.code === voiceLanguage() ? "on" : ""}`}
                    onClick={() => {
                      setVoiceLanguage(l.code);
                      setLangOpen(false);
                    }}
                  >
                    <span class="pick-name">{l.label}</span>
                    <span class="pick-meta">{l.code.toUpperCase()}</span>
                    <span class="pick-check">{l.code === voiceLanguage() ? "✓" : ""}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </span>
      </Show>
      <Show when={status() === "recording" || status() === "starting"}>
        <span class="voice-time" aria-live="off">
          {status() === "starting" ? "…" : fmt(elapsed())}
        </span>
      </Show>
      <button
        type="button"
        class={`send voice-btn ${status()}`}
        title={title()}
        aria-label={title()}
        aria-pressed={status() === "recording"}
        disabled={p.disabled}
        onClick={toggle}
      >
        <Show when={status() === "idle"}>
          <Ic.MicIcon size={15} />
        </Show>
        <Show when={status() === "starting" || status() === "transcribing"}>
          <Ic.Spinner size={14} />
        </Show>
        <Show when={status() === "recording"}>
          <span class="voice-dot" style={{ transform: `scale(${1 + level() * 0.9})` }} />
        </Show>
      </button>
      <Show when={error()}>
        <div class="voice-err" role="alert">
          {error()}
        </div>
      </Show>
    </span>
  );
}
