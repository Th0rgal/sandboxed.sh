/**
 * Local voice input: the browser half.
 *
 * Recording happens in the webview (getUserMedia → PCM float → 16 kHz mono
 * 16-bit WAV, no MediaRecorder/ffmpeg so it works in WKWebView), the bytes go
 * to the Tauri side as a raw invoke body, and the Python worker there
 * transcribes on-device. Nothing is uploaded. See ../voice/README.md.
 */
import { createSignal } from "solid-js";

// ---------------------------------------------------------------------------
// Languages — the pinned checkpoint accepts exactly these and does not
// auto-detect, so every request names one. Default: the browser locale when
// it is supported, otherwise English. The choice is remembered.

export const VOICE_LANGUAGES: { code: string; label: string }[] = [
  { code: "en", label: "English" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "es", label: "Español" },
  { code: "it", label: "Italiano" },
  { code: "pt", label: "Português" },
  { code: "nl", label: "Nederlands" },
  { code: "pl", label: "Polski" },
  { code: "el", label: "Ελληνικά" },
  { code: "ar", label: "العربية" },
  { code: "ja", label: "日本語" },
  { code: "zh", label: "中文" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "ko", label: "한국어" },
];
const LANGUAGE_CODES = new Set(VOICE_LANGUAGES.map((l) => l.code));
const LANG_KEY = "orb.voiceLang";

export function isVoiceLanguage(code: string): boolean {
  return LANGUAGE_CODES.has(code);
}

/** First supported primary subtag among the given locales, else "en". */
export function defaultVoiceLanguage(locales?: readonly string[]): string {
  const list = locales ?? (typeof navigator !== "undefined" ? (navigator.languages?.length ? navigator.languages : [navigator.language]) : []);
  for (const loc of list) {
    const base = (loc ?? "").toLowerCase().split(/[-_]/)[0];
    if (isVoiceLanguage(base)) return base;
  }
  return "en";
}

function loadLanguage(): string {
  try {
    const v = localStorage.getItem(LANG_KEY);
    if (v && isVoiceLanguage(v)) return v;
  } catch {
    /* no storage */
  }
  return defaultVoiceLanguage();
}

const [voiceLanguage, setVoiceLanguageSignal] = createSignal(loadLanguage());
export { voiceLanguage };
export function setVoiceLanguage(code: string) {
  if (!isVoiceLanguage(code)) return;
  setVoiceLanguageSignal(code);
  try {
    localStorage.setItem(LANG_KEY, code);
  } catch {
    /* no storage */
  }
}
/** Test hook: re-read storage (setup clears it between tests). */
export function reloadVoiceLanguage() {
  setVoiceLanguageSignal(loadLanguage());
}

// ---------------------------------------------------------------------------
// Native bridge

export interface VoiceCapability {
  supported: boolean;
  reason: string | null;
  platform: string;
  arch: string;
  python: string;
  python_ready: boolean;
  model_repo: string;
  model_revision: string;
  model_dir: string;
  model_ready: boolean;
  worker: "off" | "warm" | "busy";
  languages: string[];
  max_seconds: number;
  idle_seconds: number;
}

export interface VoiceTranscript {
  text: string;
  language: string;
  duration_secs: number;
  infer_secs: number;
  load_secs: number;
}

export class VoiceError extends Error {
  code: string;
  constructor(code: string, message: string) {
    super(message);
    this.code = code;
  }
}

export interface VoiceBridge {
  capability(): Promise<VoiceCapability>;
  prewarm(): Promise<unknown>;
  transcribe(wav: Uint8Array, language: string): Promise<VoiceTranscript>;
  cancel(): Promise<boolean>;
}

type Invoke = (cmd: string, args?: unknown, options?: { headers?: Record<string, string> }) => Promise<unknown>;

function tauriInvoke(): Invoke | null {
  const g = window as unknown as {
    __TAURI__?: { core?: { invoke?: Invoke } };
    __TAURI_INTERNALS__?: { invoke?: Invoke };
  };
  return g.__TAURI__?.core?.invoke ?? g.__TAURI_INTERNALS__?.invoke ?? null;
}

/** Normalize whatever `invoke` rejected with into a VoiceError. */
export function toVoiceError(e: unknown): VoiceError {
  if (e instanceof VoiceError) return e;
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    const o = e as { code: unknown; message: unknown };
    return new VoiceError(String(o.code), String(o.message));
  }
  if (typeof e === "string") return new VoiceError("error", e);
  return new VoiceError("error", e instanceof Error ? e.message : String(e));
}

/** The Tauri-backed bridge, or null when not running inside Orb's webview. */
export function tauriVoiceBridge(): VoiceBridge | null {
  const invoke = tauriInvoke();
  if (!invoke) return null;
  const call = async <T>(cmd: string, args?: unknown, options?: { headers?: Record<string, string> }): Promise<T> => {
    try {
      return (await invoke(cmd, args, options)) as T;
    } catch (e) {
      throw toVoiceError(e);
    }
  };
  return {
    capability: () => call<VoiceCapability>("voice_capability"),
    prewarm: () => call("voice_prewarm"),
    // Raw body: Tauri sends ArrayBuffer/typed-array args as bytes, not JSON.
    transcribe: (wav, language) => call<VoiceTranscript>("voice_transcribe", wav, { headers: { "x-language": language } }),
    cancel: () => call<boolean>("voice_cancel"),
  };
}

let capabilityPromise: Promise<VoiceCapability | null> | null = null;
/** Probe once per session; null means "no native voice here" (browser, non-Mac). */
export function probeVoice(bridge: VoiceBridge | null = tauriVoiceBridge()): Promise<VoiceCapability | null> {
  if (!capabilityPromise) {
    capabilityPromise = (async () => {
      if (!bridge) return null;
      try {
        const cap = await bridge.capability();
        return cap.supported ? cap : null;
      } catch {
        return null;
      }
    })();
  }
  return capabilityPromise;
}
export function resetVoiceProbe() {
  capabilityPromise = null;
}

// ---------------------------------------------------------------------------
// Audio helpers

export const VOICE_SAMPLE_RATE = 16000;
export const VOICE_MAX_SECONDS = 120;

/** Linear-interpolation resampler; good enough for speech going down to 16 kHz. */
export function resampleLinear(input: Float32Array, from: number, to: number): Float32Array {
  if (from === to || input.length === 0) return input;
  const ratio = from / to;
  const outLen = Math.max(1, Math.round(input.length / ratio));
  const out = new Float32Array(outLen);
  for (let i = 0; i < outLen; i++) {
    const pos = i * ratio;
    const i0 = Math.floor(pos);
    const i1 = Math.min(i0 + 1, input.length - 1);
    const t = pos - i0;
    out[i] = input[i0] * (1 - t) + input[i1] * t;
  }
  return out;
}

/** Mono 16-bit PCM WAV. */
export function encodeWav(samples: Float32Array, sampleRate: number): Uint8Array {
  const buf = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(buf);
  const str = (o: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i));
  };
  str(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  let o = 44;
  for (let i = 0; i < samples.length; i++, o += 2) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    v.setInt16(o, Math.round(s < 0 ? s * 0x8000 : s * 0x7fff), true);
  }
  return new Uint8Array(buf);
}

export function concatFloat32(chunks: Float32Array[]): Float32Array {
  let n = 0;
  for (const c of chunks) n += c.length;
  const out = new Float32Array(n);
  let o = 0;
  for (const c of chunks) {
    out.set(c, o);
    o += c.length;
  }
  return out;
}

/** Insert dictated text at the caret, keeping the typed draft on both sides. */
export function insertAtCaret(value: string, start: number, end: number, text: string): { value: string; caret: number } {
  const s = Math.max(0, Math.min(start, value.length));
  const e = Math.max(s, Math.min(end, value.length));
  const before = value.slice(0, s);
  const after = value.slice(e);
  const lead = before && !/\s$/.test(before) ? " " : "";
  const trail = after && !/^\s/.test(after) ? " " : "";
  const head = before + lead + text;
  return { value: head + trail + after, caret: head.length };
}

// ---------------------------------------------------------------------------
// Recorder

export interface Recorder {
  /** Stop and return the WAV. Resolves to an empty array if nothing was captured. */
  stop(): Promise<Uint8Array>;
  /** Stop and discard. */
  cancel(): void;
  startedAt: number;
}

export interface RecorderOptions {
  /** RMS level in 0..1, a few times per second. */
  onLevel?: (level: number) => void;
  /** Hard stop after this many seconds; the returned promise from stop() is the way to collect. */
  maxSeconds?: number;
  onAutoStop?: () => void;
}

export type RecorderFactory = (opts: RecorderOptions) => Promise<Recorder>;

export function describeMediaError(e: unknown): VoiceError {
  const name = (e as { name?: string })?.name ?? "";
  if (name === "NotAllowedError" || name === "SecurityError" || name === "PermissionDeniedError") {
    return new VoiceError("permission", "Microphone access was denied. Allow Orb in System Settings → Privacy & Security → Microphone.");
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return new VoiceError("no_device", "No microphone was found.");
  }
  if (name === "NotReadableError" || name === "AbortError") {
    return new VoiceError("busy", "The microphone is in use by another app.");
  }
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
    return new VoiceError("unsupported", "Microphone capture is not available in this window.");
  }
  return toVoiceError(e);
}

type AudioContextCtor = new (opts?: { sampleRate?: number }) => AudioContext;

/**
 * Capture from the default microphone via ScriptProcessorNode (deprecated but
 * the one path that works unchanged in WKWebView, Chromium and jsdom-mocked
 * tests) and produce a 16 kHz mono WAV on stop.
 */
export const startRecording: RecorderFactory = async (opts) => {
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
    throw new VoiceError("unsupported", "Microphone capture is not available in this window.");
  }
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      video: false,
    });
  } catch (e) {
    throw describeMediaError(e);
  }
  const Ctor = ((window as unknown as { AudioContext?: AudioContextCtor; webkitAudioContext?: AudioContextCtor }).AudioContext ??
    (window as unknown as { webkitAudioContext?: AudioContextCtor }).webkitAudioContext) as AudioContextCtor | undefined;
  if (!Ctor) {
    stream.getTracks().forEach((t) => t.stop());
    throw new VoiceError("unsupported", "Web Audio is not available in this window.");
  }
  // Everything from the context up to the graph wiring can throw (Safari's
  // sample-rate quirks, a suspended context that will not resume, a webview
  // without ScriptProcessorNode). Whatever already exists must be released,
  // or the microphone indicator stays on with no way to turn it off.
  let ctx: AudioContext | null = null;
  let source!: MediaStreamAudioSourceNode;
  let processor!: ScriptProcessorNode;
  let sink!: GainNode;
  let analyser: AnalyserNode | undefined;
  let meterFrame: number | undefined;
  let envelope = 0;
  const chunks: Float32Array[] = [];
  let frames = 0;
  let finished = false;
  try {
    try {
      ctx = new Ctor({ sampleRate: VOICE_SAMPLE_RATE });
    } catch {
      ctx = new Ctor();
    }
    if (ctx.state === "suspended") await ctx.resume().catch(() => {});
    source = ctx.createMediaStreamSource(stream);
    // 32 ms at 16 kHz: large buffers made the live meter pulse between silences.
    processor = ctx.createScriptProcessor(512, 1, 1);
    sink = ctx.createGain();
    sink.gain.value = 0; // Safari only pumps the processor when it reaches the destination.
    const maxFrames = (opts.maxSeconds ?? VOICE_MAX_SECONDS) * ctx.sampleRate;
    processor.onaudioprocess = (ev) => {
      if (finished) return;
      const data = ev.inputBuffer.getChannelData(0);
      chunks.push(new Float32Array(data));
      frames += data.length;
      if (frames >= maxFrames) {
        finished = true;
        opts.onAutoStop?.();
      }
    };
    if (opts.onLevel) {
      analyser = ctx.createAnalyser();
      analyser.fftSize = 1024;
      source.connect(analyser);
      const samples = new Float32Array(analyser.fftSize);
      let previous = performance.now();
      const meter = (now: number) => {
        if (finished) return;
        analyser!.getFloatTimeDomainData(samples);
        let sum = 0;
        for (const sample of samples) sum += sample * sample;
        const rms = Math.sqrt(sum / samples.length);
        // A perceptual range keeps ordinary speech visible without amplifying silence.
        const target = Math.max(0, Math.min(1, (20 * Math.log10(Math.max(rms, 1e-6)) + 55) / 45));
        const dt = Math.min(100, Math.max(0, now - previous));
        previous = now;
        const tau = target > envelope ? 45 : 180;
        envelope += (target - envelope) * (1 - Math.exp(-dt / tau));
        opts.onLevel!(envelope);
        meterFrame = requestAnimationFrame(meter);
      };
      meterFrame = requestAnimationFrame(meter);
    }
    source.connect(processor);
    processor.connect(sink);
    sink.connect(ctx.destination);
  } catch (e) {
    if (meterFrame !== undefined) cancelAnimationFrame(meterFrame);
    stream.getTracks().forEach((t) => t.stop());
    if (ctx) void ctx.close().catch(() => {});
    if (e instanceof VoiceError) throw e;
    const reason = e instanceof Error ? e.message : String(e);
    throw new VoiceError("audio_setup", `Microphone setup failed${reason ? `: ${reason}` : "."}`);
  }
  const audio = ctx;

  const teardown = () => {
    finished = true;
    if (meterFrame !== undefined) cancelAnimationFrame(meterFrame);
    analyser?.disconnect();
    processor.onaudioprocess = null;
    try {
      source.disconnect();
      processor.disconnect();
      sink.disconnect();
    } catch {
      /* already gone */
    }
    stream.getTracks().forEach((t) => t.stop());
    void audio.close().catch(() => {});
  };
  return {
    startedAt: Date.now(),
    async stop() {
      teardown();
      const all = concatFloat32(chunks);
      chunks.length = 0;
      if (all.length === 0) return new Uint8Array(0);
      return encodeWav(resampleLinear(all, audio.sampleRate, VOICE_SAMPLE_RATE), VOICE_SAMPLE_RATE);
    },
    cancel() {
      teardown();
      chunks.length = 0;
    },
  };
};
