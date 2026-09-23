import { describe, it, expect, vi, afterEach } from "vitest";
import {
  VOICE_LANGUAGES,
  VoiceError,
  defaultVoiceLanguage,
  describeMediaError,
  encodeWav,
  insertAtCaret,
  probeVoice,
  reloadVoiceLanguage,
  resampleLinear,
  resetVoiceProbe,
  setVoiceLanguage,
  startRecording,
  tauriVoiceBridge,
  toVoiceError,
  voiceLanguage,
} from "../src/voice";

const tauri = (invoke: (cmd: string, args?: unknown, options?: { headers?: Record<string, string> }) => Promise<unknown>) => {
  (window as unknown as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
};
afterEach(() => {
  delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
  resetVoiceProbe();
});

describe("language preference", () => {
  it("defaults to the first supported browser locale, else English", () => {
    expect(defaultVoiceLanguage(["fr-FR", "en-US"])).toBe("fr");
    expect(defaultVoiceLanguage(["pt-BR"])).toBe("pt");
    expect(defaultVoiceLanguage(["zh-Hans-CN"])).toBe("zh");
    expect(defaultVoiceLanguage(["sv-SE", "de"])).toBe("de");
    expect(defaultVoiceLanguage(["sv-SE"])).toBe("en");
    expect(defaultVoiceLanguage([])).toBe("en");
  });
  it("offers exactly the checkpoint's 14 languages", () => {
    expect(VOICE_LANGUAGES.map((l) => l.code).sort()).toEqual(["ar", "de", "el", "en", "es", "fr", "it", "ja", "ko", "nl", "pl", "pt", "vi", "zh"]);
  });
  it("persists a valid choice and ignores unknown codes", () => {
    setVoiceLanguage("fr");
    expect(voiceLanguage()).toBe("fr");
    expect(localStorage.getItem("orb.voiceLang")).toBe("fr");
    setVoiceLanguage("xx");
    expect(voiceLanguage()).toBe("fr");
    localStorage.setItem("orb.voiceLang", "ja");
    reloadVoiceLanguage();
    expect(voiceLanguage()).toBe("ja");
    localStorage.clear();
    reloadVoiceLanguage();
    expect(voiceLanguage()).toBe(defaultVoiceLanguage());
  });
});

describe("wav encoding", () => {
  it("writes a 16 kHz mono 16-bit PCM header and clips samples", () => {
    const wav = encodeWav(new Float32Array([0, 0.5, 1, -1, 2, -2]), 16000);
    const v = new DataView(wav.buffer);
    const tag = (o: number) => String.fromCharCode(...wav.slice(o, o + 4));
    expect(tag(0)).toBe("RIFF");
    expect(tag(8)).toBe("WAVE");
    expect(tag(12)).toBe("fmt ");
    expect(v.getUint16(20, true)).toBe(1); // PCM
    expect(v.getUint16(22, true)).toBe(1); // mono
    expect(v.getUint32(24, true)).toBe(16000);
    expect(v.getUint32(28, true)).toBe(32000); // byte rate
    expect(v.getUint16(34, true)).toBe(16);
    expect(tag(36)).toBe("data");
    expect(v.getUint32(40, true)).toBe(12);
    expect(v.getUint32(4, true)).toBe(36 + 12);
    expect(wav.length).toBe(44 + 12);
    expect(v.getInt16(44, true)).toBe(0);
    expect(v.getInt16(46, true)).toBe(Math.round(0.5 * 0x7fff));
    expect(v.getInt16(48, true)).toBe(0x7fff);
    expect(v.getInt16(50, true)).toBe(-0x8000);
    expect(v.getInt16(52, true)).toBe(0x7fff);
    expect(v.getInt16(54, true)).toBe(-0x8000);
  });
  it("resamples linearly and is a no-op at equal rates", () => {
    const ramp = Float32Array.from({ length: 48000 }, (_, i) => i / 48000);
    const out = resampleLinear(ramp, 48000, 16000);
    expect(out.length).toBe(16000);
    expect(out[0]).toBe(0);
    expect(out[8000]).toBeCloseTo(0.5, 3);
    expect(out[15999]).toBeCloseTo(ramp[47997], 3);
    expect(resampleLinear(ramp, 16000, 16000)).toBe(ramp);
  });
});

describe("caret insertion", () => {
  it("keeps the draft on both sides and pads with spaces only where needed", () => {
    expect(insertAtCaret("", 0, 0, "hello")).toEqual({ value: "hello", caret: 5 });
    expect(insertAtCaret("fix the", 7, 7, "tests")).toEqual({ value: "fix the tests", caret: 13 });
    expect(insertAtCaret("fix the ", 8, 8, "tests")).toEqual({ value: "fix the tests", caret: 13 });
    expect(insertAtCaret("fix bug", 4, 4, "the")).toEqual({ value: "fix the bug", caret: 7 });
    expect(insertAtCaret("fix bug", 4, 7, "everything")).toEqual({ value: "fix everything", caret: 14 });
    expect(insertAtCaret("abc", 99, 99, "d")).toEqual({ value: "abc d", caret: 5 });
  });
});

describe("tauri bridge", () => {
  it("is absent outside Tauri and the probe resolves to null", async () => {
    expect(tauriVoiceBridge()).toBeNull();
    expect(await probeVoice()).toBeNull();
  });
  it("sends the WAV as a raw body with the language header", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "voice_capability") return { supported: true, languages: ["en"] };
      if (cmd === "voice_transcribe") return { text: "bonjour", language: "fr", duration_secs: 1, infer_secs: 0.1, load_secs: 0 };
      return true;
    });
    tauri(invoke);
    const b = tauriVoiceBridge()!;
    const wav = encodeWav(new Float32Array(16), 16000);
    const r = await b.transcribe(wav, "fr");
    expect(r.text).toBe("bonjour");
    expect(invoke).toHaveBeenCalledWith("voice_transcribe", wav, { headers: { "x-language": "fr" } });
    expect((await probeVoice())?.supported).toBe(true);
    await b.prewarm();
    await b.cancel();
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(["voice_transcribe", "voice_capability", "voice_prewarm", "voice_cancel"]);
  });
  it("hides the microphone when the native side says unsupported", async () => {
    tauri(async () => ({ supported: false, reason: "Linux" }));
    expect(await probeVoice()).toBeNull();
  });
  it("maps native errors onto VoiceError codes", async () => {
    tauri(async () => {
      throw { code: "model_missing", message: "no snapshot" };
    });
    await expect(tauriVoiceBridge()!.transcribe(new Uint8Array(0), "en")).rejects.toMatchObject({ code: "model_missing", message: "no snapshot" });
    expect(toVoiceError("boom")).toMatchObject({ code: "error", message: "boom" });
    expect(toVoiceError(new Error("x"))).toMatchObject({ code: "error", message: "x" });
    expect(toVoiceError(new VoiceError("k", "m")).code).toBe("k");
  });
});

describe("recorder", () => {
  it("explains permission and device failures", () => {
    expect(describeMediaError({ name: "NotAllowedError" }).code).toBe("permission");
    expect(describeMediaError({ name: "NotFoundError" }).code).toBe("no_device");
    expect(describeMediaError({ name: "NotReadableError" }).code).toBe("busy");
    expect(describeMediaError(new Error("odd")).code).toBe("unsupported"); // jsdom has no mediaDevices
  });
  it("fails cleanly without getUserMedia", async () => {
    await expect(startRecording({})).rejects.toMatchObject({ code: "unsupported" });
  });
  it("captures processor buffers, resamples to 16 kHz and stops the tracks", async () => {
    const track = { stop: vi.fn() };
    const getUserMedia = vi.fn(async () => ({ getTracks: () => [track] }));
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia }, configurable: true });
    let processor: { onaudioprocess: ((ev: { inputBuffer: { getChannelData: () => Float32Array } }) => void) | null; connect: () => void; disconnect: () => void } | null = null;
    const closed = vi.fn(async () => {});
    class FakeContext {
      sampleRate = 48000;
      state = "running";
      destination = {};
      constructor(opts?: { sampleRate?: number }) {
        // Pretend the platform ignored the 16 kHz request, like Safari can.
        void opts;
      }
      resume = async () => {};
      close = closed;
      createMediaStreamSource = () => ({ connect: vi.fn(), disconnect: vi.fn() });
      createAnalyser = () => ({ fftSize: 1024, getFloatTimeDomainData: (data: Float32Array) => data.fill(0.25), disconnect: vi.fn() });
      createGain = () => ({ gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() });
      createScriptProcessor = () => {
        processor = { onaudioprocess: null, connect: vi.fn(), disconnect: vi.fn() };
        return processor;
      };
    }
    (window as unknown as { AudioContext: unknown }).AudioContext = FakeContext;
    try {
      const levels: number[] = [];
      const rec = await startRecording({ onLevel: (l) => levels.push(l) });
      expect(getUserMedia).toHaveBeenCalledWith({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }, video: false });
      const buf = new Float32Array(4800).fill(0.25);
      processor!.onaudioprocess!({ inputBuffer: { getChannelData: () => buf } });
      processor!.onaudioprocess!({ inputBuffer: { getChannelData: () => buf } });
      await new Promise(resolve => setTimeout(resolve, 50));
      expect(levels.length).toBeGreaterThan(0);
      const wav = await rec.stop();
      const v = new DataView(wav.buffer);
      expect(v.getUint32(24, true)).toBe(16000);
      expect(v.getUint32(40, true)).toBe(3200 * 2); // 9600 frames at 48 kHz → 3200 at 16 kHz
      expect(v.getInt16(44, true)).toBe(Math.round(0.25 * 0x7fff));
      expect(track.stop).toHaveBeenCalled();
      expect(closed).toHaveBeenCalled();
    } finally {
      delete (window as unknown as { AudioContext?: unknown }).AudioContext;
      Object.defineProperty(navigator, "mediaDevices", { value: undefined, configurable: true });
    }
  });
  it("releases the microphone and the context when the audio graph cannot be built", async () => {
    const track = { stop: vi.fn() };
    const getUserMedia = vi.fn(async () => ({ getTracks: () => [track] }));
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia }, configurable: true });
    const closed = vi.fn(async () => {});
    class FakeContext {
      sampleRate = 16000;
      state = "running";
      destination = {};
      resume = async () => {};
      close = closed;
      createMediaStreamSource = () => ({ connect: vi.fn(), disconnect: vi.fn() });
      createAnalyser = () => ({ fftSize: 1024, getFloatTimeDomainData: (data: Float32Array) => data.fill(0.25), disconnect: vi.fn() });
      createGain = () => ({ gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() });
      createScriptProcessor = () => {
        throw new Error("ScriptProcessorNode is not supported");
      };
    }
    (window as unknown as { AudioContext: unknown }).AudioContext = FakeContext;
    try {
      await expect(startRecording({})).rejects.toMatchObject({ code: "audio_setup", message: /ScriptProcessorNode/ });
      expect(track.stop).toHaveBeenCalledTimes(1);
      expect(closed).toHaveBeenCalledTimes(1);
    } finally {
      delete (window as unknown as { AudioContext?: unknown }).AudioContext;
      Object.defineProperty(navigator, "mediaDevices", { value: undefined, configurable: true });
    }
  });
  it("stops the tracks when no AudioContext constructor exists at all", async () => {
    const track = { stop: vi.fn() };
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia: async () => ({ getTracks: () => [track] }) }, configurable: true });
    const saved = (window as unknown as { AudioContext?: unknown }).AudioContext;
    delete (window as unknown as { AudioContext?: unknown }).AudioContext;
    try {
      await expect(startRecording({})).rejects.toMatchObject({ code: "unsupported" });
      expect(track.stop).toHaveBeenCalledTimes(1);
    } finally {
      if (saved) (window as unknown as { AudioContext?: unknown }).AudioContext = saved;
      Object.defineProperty(navigator, "mediaDevices", { value: undefined, configurable: true });
    }
  });
  it("auto-stops at the duration cap", async () => {
    const getUserMedia = vi.fn(async () => ({ getTracks: () => [] }));
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia }, configurable: true });
    let processor: { onaudioprocess: ((ev: { inputBuffer: { getChannelData: () => Float32Array } }) => void) | null } | null = null;
    class FakeContext {
      sampleRate = 16000;
      state = "running";
      destination = {};
      resume = async () => {};
      close = async () => {};
      createMediaStreamSource = () => ({ connect: vi.fn(), disconnect: vi.fn() });
      createAnalyser = () => ({ fftSize: 1024, getFloatTimeDomainData: (data: Float32Array) => data.fill(0.25), disconnect: vi.fn() });
      createGain = () => ({ gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() });
      createScriptProcessor = () => (processor = { onaudioprocess: null, connect: vi.fn(), disconnect: vi.fn() } as never);
    }
    (window as unknown as { AudioContext: unknown }).AudioContext = FakeContext;
    try {
      const onAutoStop = vi.fn();
      const rec = await startRecording({ maxSeconds: 1, onAutoStop });
      const buf = new Float32Array(16000);
      processor!.onaudioprocess!({ inputBuffer: { getChannelData: () => buf } });
      expect(onAutoStop).toHaveBeenCalledTimes(1);
      processor!.onaudioprocess!({ inputBuffer: { getChannelData: () => buf } }); // ignored after the cap
      const wav = await rec.stop();
      expect(new DataView(wav.buffer).getUint32(40, true)).toBe(16000 * 2);
    } finally {
      delete (window as unknown as { AudioContext?: unknown }).AudioContext;
      Object.defineProperty(navigator, "mediaDevices", { value: undefined, configurable: true });
    }
  });
});
