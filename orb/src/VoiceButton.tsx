/**
 * Dictation button for the composer: idle → recording → transcribing → text
 * handed back to the composer (never sent). Only rendered where native voice
 * is available; everywhere else `voiceAvailable()` stays false and the
 * composer shows no microphone.
 */
import { For, Index, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
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

const WAVE_BARS = 320;
const silentWave = () => Array.from({ length: WAVE_BARS }, () => 0);

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
  const [bars, setBars] = createSignal<number[]>(silentWave());
  const [error, setError] = createSignal<string | null>(null);
  const [langOpen, setLangOpen] = createSignal(false);
  let rec: Recorder | null = null;
  let tick: number | undefined;
  let errTimer: number | undefined;
  let waveTimer: number | undefined;
  let pendingLevel = 0;
  let token = 0; // bumps on every start/cancel; late results with an old token are dropped
  let disposed = false;
  /** Conversation the current recording was started for; fixed at start, checked at every hand-off. */
  let activeScope: string | undefined;

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
    setBars(silentWave());
    const my = ++token;
    const scope = p.scope;
    activeScope = scope;
    setState("starting");
    // Warm the model while the user is still talking so the cold start is
    // hidden behind the utterance. Failures surface when they stop.
    let warmFailure: VoiceError | null = null;
    const warm = b.prewarm().catch((e) => {
      warmFailure = toVoiceError(e);
    });
    // Still the recording the UI is showing, in the conversation it was started for.
    const live = () => my === token && !disposed && p.scope === scope;
    // The recorder stays local until it has been validated: the permission
    // prompt can outlive a cancel + restart, and a late result must never
    // replace or tear down the recording that now owns the button.
    let candidate: Recorder | null = null;
    try {
      candidate = await record()({
        onLevel: (l) => {
          if (!live()) return;
          pendingLevel = l;
        },
        maxSeconds: VOICE_MAX_SECONDS,
        onAutoStop: () => {
          if (candidate && rec === candidate) void finish();
        },
      });
    } catch (e) {
      if (my !== token || disposed) return; // superseded: a newer start owns the UI
      setState("idle");
      showError(e);
      return;
    }
    if (!live()) {
      // Cancelled, unmounted or navigated elsewhere while the prompt was up.
      candidate.cancel();
      if (my === token && !disposed) setState("idle");
      return;
    }
    rec = candidate;
    setBars(silentWave());
    pendingLevel = 0;
    waveTimer = window.setInterval(() => {
      const level = Math.min(1, Math.max(0, pendingLevel));
      // The analyser supplies a continuous envelope independently of WAV chunks.
      setBars(prev => [...prev.slice(1), level]);
    }, 40);
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
    const scope = activeScope;
    rec = null;
    stopTimer();
    clearInterval(waveTimer); waveTimer = undefined;
    setState("transcribing");
    try {
      const wav = await r.stop();
      if (my !== token || disposed) return;
      if (wav.length === 0) throw new VoiceError("empty", "Nothing was recorded.");
      const result = await b.transcribe(wav, voiceLanguage());
      if (my !== token || disposed) return; // cancelled or navigated away meanwhile
      if (scope !== p.scope) return; // composer now belongs to another conversation
      const text = result.text.trim();
      if (my === token && !disposed) setState("idle");
      if (!text) {
        showError(new VoiceError("empty", "No speech detected."));
      } else {
        // Insert after the composer field is shown again — WKWebView paints
        // a ghosted double glyph if we write into a `display:none` textarea.
        queueMicrotask(() => {
          if (my !== token || disposed || scope !== p.scope) return;
          p.onText(text);
        });
      }
    } catch (e) {
      if (my !== token || disposed) return;
      showError(e);
    } finally {
      if (my === token && !disposed && status() !== "idle") setState("idle");
    }
  };

  const cancelRecording = () => {
    token++;
    rec?.cancel();
    rec = null;
    stopTimer();
    clearInterval(waveTimer); waveTimer = undefined;
    pendingLevel = 0;
    setBars(silentWave());
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
  // The composer moved to another conversation: whatever is in flight was
  // for the old one, so drop it rather than dictate into the wrong chat.
  createEffect(
    on(
      () => p.scope,
      (scope) => {
        if (status() !== "idle" && scope !== activeScope) cancel();
      },
      { defer: true },
    ),
  );

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
    clearInterval(waveTimer);
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
        <button
          type="button"
          class="send voice-btn idle"
          title={title()}
          aria-label={title()}
          disabled={p.disabled}
          onClick={toggle}
        >
          <Ic.MicIcon size={15} />
        </button>
      </Show>
      <Show when={status() !== "idle"}>
        <div class="voice-wave" aria-hidden="true">
          <Index each={bars()}>{(v) => <span style={{ "--v": String(v()) }} />}</Index>
        </div>
        <span class="voice-time" aria-live="off">
          {status() === "starting" ? "…" : fmt(elapsed())}
        </span>
        <button
          type="button"
          class="voice-x"
          title={status() === "recording" ? "Cancel recording" : title()}
          aria-label={status() === "recording" ? "Cancel recording" : title()}
          onClick={cancel}
        >
          <Show when={status() === "transcribing"} fallback={<Ic.CloseIcon size={13} />}>
            <Ic.Spinner size={13} />
          </Show>
        </button>
        <Show when={status() === "recording"}>
          <button
            type="button"
            class="voice-ok"
            title="Stop recording"
            aria-label="Stop recording"
            aria-pressed="true"
            onClick={() => void finish()}
          >
            <Ic.CheckIcon size={13} />
          </button>
        </Show>
      </Show>
      <Show when={error()}>
        <div class="voice-err" role="alert">
          {error()}
        </div>
      </Show>
    </span>
  );
}
