import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createSignal } from "solid-js";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { VoiceButton, describeVoiceError, ensureVoiceProbe, resetVoiceAvailability, voiceAvailable } from "../src/VoiceButton";
import { VoiceError, encodeWav, reloadVoiceLanguage, resetVoiceProbe, setVoiceLanguage, type Recorder, type RecorderOptions, type VoiceBridge, type VoiceCapability } from "../src/voice";

const cap = (over: Partial<VoiceCapability> = {}): VoiceCapability => ({
  supported: true,
  reason: null,
  platform: "macos",
  arch: "aarch64",
  python: "/py",
  python_ready: true,
  model_repo: "r",
  model_revision: "v",
  model_dir: "/m",
  model_ready: true,
  worker: "off",
  languages: ["en", "fr"],
  max_seconds: 120,
  idle_seconds: 600,
  ...over,
});

function fakeBridge(over: Partial<VoiceBridge> = {}) {
  const bridge = {
    capability: vi.fn(async () => cap()),
    prewarm: vi.fn(async () => ({ loaded: true })),
    transcribe: vi.fn(async () => ({ text: "hello world", language: "en", duration_secs: 1, infer_secs: 0.1, load_secs: 0 })),
    cancel: vi.fn(async () => true),
    ...over,
  };
  return bridge as VoiceBridge & typeof bridge;
}

/** A recorder whose stop() we control from the test. */
function fakeRecorder() {
  const wav = encodeWav(new Float32Array(1600).fill(0.1), 16000);
  const state = { opts: null as RecorderOptions | null, cancelled: 0, stopped: 0, empty: false, fail: null as unknown };
  const factory = vi.fn(async (opts: RecorderOptions): Promise<Recorder> => {
    if (state.fail) throw state.fail;
    state.opts = opts;
    return {
      startedAt: Date.now(),
      async stop() {
        state.stopped++;
        return state.empty ? new Uint8Array(0) : wav;
      },
      cancel() {
        state.cancelled++;
      },
    };
  });
  return { factory, state, wav };
}

const button = () => screen.getByRole("button", { name: /Dictate|Stop recording|Transcribing|Starting/ });
const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  resetVoiceProbe();
  resetVoiceAvailability();
  reloadVoiceLanguage();
  setVoiceLanguage("en");
});
afterEach(() => {
  vi.useRealTimers();
});

describe("availability", () => {
  it("is off in a plain browser and on when the native side reports support", async () => {
    ensureVoiceProbe(null);
    await flush();
    expect(voiceAvailable()).toBe(false);
    resetVoiceAvailability();
    resetVoiceProbe();
    ensureVoiceProbe(fakeBridge());
    await waitFor(() => expect(voiceAvailable()).toBe(true));
  });
  it("stays hidden when unsupported (Linux, Intel) or the probe fails", async () => {
    ensureVoiceProbe(fakeBridge({ capability: async () => cap({ supported: false, reason: "no" }) }));
    await flush();
    expect(voiceAvailable()).toBe(false);
    resetVoiceAvailability();
    resetVoiceProbe();
    ensureVoiceProbe(fakeBridge({ capability: async () => { throw new Error("ipc"); } }));
    await flush();
    expect(voiceAvailable()).toBe(false);
  });
});

describe("VoiceButton flow", () => {
  it("records, stops, transcribes and hands the text back without sending", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    const onText = vi.fn();
    const onActive = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} onActive={onActive} scope="a" />);
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    expect(bridge.prewarm).toHaveBeenCalledTimes(1);
    expect(rec.state.opts?.maxSeconds).toBe(120);
    expect(onActive).toHaveBeenLastCalledWith(true);
    expect(screen.queryByRole("button", { name: "Dictation language" })).toBeNull(); // chip hidden while recording
    expect(document.querySelector(".voice-wave")).toBeTruthy();
    const lastBar = document.querySelector(".voice-wave span:last-child") as HTMLElement;
    rec.state.opts?.onLevel?.(0.9);
    await waitFor(() => expect(Number(lastBar.style.getPropertyValue("--v"))).toBeGreaterThan(0));
    expect(document.querySelector(".voice-wave span:last-child")).toBe(lastBar);
    // A missing audio callback must decay rather than draw a zero between packets.
    await new Promise(resolve => setTimeout(resolve, 50));
    expect(Number(lastBar.style.getPropertyValue("--v"))).toBeGreaterThan(0);
    rec.state.opts?.onLevel?.(0.2);
    fireEvent.click(button()); // stop
    await waitFor(() => expect(onText).toHaveBeenCalledWith("hello world"));
    expect(bridge.transcribe).toHaveBeenCalledTimes(1);
    expect(bridge.transcribe.mock.calls[0][0]).toBe(rec.wav);
    expect(bridge.transcribe.mock.calls[0][1]).toBe("en");
    expect(rec.state.stopped).toBe(1);
    await waitFor(() => expect(button().getAttribute("title")).toBe("Dictate (EN)"));
    expect(onActive).toHaveBeenLastCalledWith(false);
  });

  it("uses the chosen language and remembers it", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation language" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Français/ }));
    expect(screen.getByRole("button", { name: "Dictation language" }).textContent).toBe("FR");
    expect(localStorage.getItem("orb.voiceLang")).toBe("fr");
    expect(button().getAttribute("title")).toBe("Dictate (FR)");
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(bridge.transcribe).toHaveBeenCalled());
    expect(bridge.transcribe.mock.calls[0][1]).toBe("fr");
  });

  it("Escape cancels a recording; nothing is transcribed", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(rec.state.cancelled).toBe(1);
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    await flush();
    expect(bridge.transcribe).not.toHaveBeenCalled();
    expect(onText).not.toHaveBeenCalled();
  });

  it("clicking while transcribing cancels: the late result is dropped and the worker is told", async () => {
    let resolve!: (v: { text: string; language: string; duration_secs: number; infer_secs: number; load_secs: number }) => void;
    const bridge = fakeBridge({ transcribe: vi.fn(() => new Promise((r) => (resolve = r))) });
    const rec = fakeRecorder();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("title")).toMatch(/Transcribing/));
    fireEvent.click(button());
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    expect(bridge.cancel).toHaveBeenCalledTimes(1);
    resolve({ text: "too late", language: "en", duration_secs: 1, infer_secs: 1, load_secs: 0 });
    await flush();
    expect(onText).not.toHaveBeenCalled();
  });

  it("drops a result that arrives after the composer moved to another conversation", async () => {
    let resolve!: (v: { text: string; language: string; duration_secs: number; infer_secs: number; load_secs: number }) => void;
    const bridge = fakeBridge({ transcribe: vi.fn(() => new Promise((r) => (resolve = r))) });
    const rec = fakeRecorder();
    const onText = vi.fn();
    const [scope, setScope] = createSignal("m:1");
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} scope={scope()} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(bridge.transcribe).toHaveBeenCalled());
    setScope("m:2");
    resolve({ text: "for the other chat", language: "en", duration_secs: 1, infer_secs: 1, load_secs: 0 });
    await flush();
    expect(onText).not.toHaveBeenCalled();
    await waitFor(() => expect(button().getAttribute("title")).toBe("Dictate (EN)"));
  });

  it("navigating to another conversation while recording cancels it; nothing is transcribed", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    const onText = vi.fn();
    const onActive = vi.fn();
    const [scope, setScope] = createSignal("m:1");
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} onActive={onActive} scope={scope()} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    setScope("m:2");
    expect(rec.state.cancelled).toBe(1);
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    expect(onActive).toHaveBeenLastCalledWith(false);
    await flush();
    expect(bridge.transcribe).not.toHaveBeenCalled();
    expect(onText).not.toHaveBeenCalled();
    // The button still works for the new conversation.
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(onText).toHaveBeenCalledWith("hello world"));
  });

  it("drops a recorder whose permission prompt resolves after the conversation changed", async () => {
    let grant!: (r: Recorder) => void;
    const cancelled = vi.fn();
    const factory = vi.fn((): Promise<Recorder> => new Promise((r) => (grant = r)));
    const bridge = fakeBridge();
    const [scope, setScope] = createSignal("m:1");
    render(() => <VoiceButton bridge={bridge} recorder={factory} onText={() => {}} scope={scope()} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("title")).toBe("Starting microphone…"));
    setScope("m:2");
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    grant({ startedAt: Date.now(), stop: async () => new Uint8Array(0), cancel: cancelled });
    await flush();
    expect(cancelled).toHaveBeenCalledTimes(1);
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
  });

  it("a slow permission prompt from a cancelled start cannot replace or stop the restarted recording", async () => {
    const wav = encodeWav(new Float32Array(1600).fill(0.2), 16000);
    const pending: Array<{ resolve: (r: Recorder) => void; reject: (e: unknown) => void }> = [];
    const stale = { startedAt: Date.now(), stop: vi.fn(async () => wav), cancel: vi.fn() };
    const fresh = { startedAt: Date.now(), stop: vi.fn(async () => wav), cancel: vi.fn() };
    const factory = vi.fn(
      (): Promise<Recorder> =>
        factory.mock.calls.length === 1
          ? new Promise((resolve, reject) => pending.push({ resolve, reject })) // first prompt: hangs
          : Promise.resolve(fresh),
    );
    const bridge = fakeBridge();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={factory} onText={onText} scope="a" />);
    fireEvent.click(button()); // start #1: waits on the prompt
    await waitFor(() => expect(button().getAttribute("title")).toBe("Starting microphone…"));
    fireEvent.click(button()); // cancel while starting
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    fireEvent.click(button()); // start #2: granted immediately
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    expect(factory).toHaveBeenCalledTimes(2);
    // The old prompt finally resolves: its recorder is released, ours is untouched.
    pending[0].resolve(stale);
    await flush();
    expect(stale.cancel).toHaveBeenCalledTimes(1);
    expect(fresh.cancel).not.toHaveBeenCalled();
    expect(button().getAttribute("aria-pressed")).toBe("true");
    expect(screen.queryByRole("alert")).toBeNull();
    fireEvent.click(button()); // stop: transcribes the fresh recording only
    await waitFor(() => expect(onText).toHaveBeenCalledWith("hello world"));
    expect(fresh.stop).toHaveBeenCalledTimes(1);
    expect(stale.stop).not.toHaveBeenCalled();
    expect(bridge.transcribe).toHaveBeenCalledTimes(1);
  });

  it("a denied permission from a cancelled start does not surface on the restarted recording", async () => {
    let reject!: (e: unknown) => void;
    const rec = fakeRecorder();
    const factory = vi.fn((opts: RecorderOptions): Promise<Recorder> =>
      factory.mock.calls.length === 1 ? new Promise((_, rej) => (reject = rej)) : rec.factory(opts),
    );
    const bridge = fakeBridge();
    render(() => <VoiceButton bridge={bridge} recorder={factory} onText={() => {}} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("title")).toBe("Starting microphone…"));
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    reject(new VoiceError("permission", "Microphone access was denied."));
    await flush();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(button().getAttribute("aria-pressed")).toBe("true");
    expect(rec.state.cancelled).toBe(0);
  });

  it("unmounting mid-recording releases the microphone and mid-transcription cancels", async () => {
    const bridge = fakeBridge({ transcribe: vi.fn(() => new Promise(() => {})) });
    const rec = fakeRecorder();
    const onText = vi.fn();
    const [show, setShow] = createSignal(true);
    render(() => <>{show() && <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />}</>);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    setShow(false);
    expect(rec.state.cancelled).toBe(1);
    setShow(true);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(bridge.transcribe).toHaveBeenCalled());
    setShow(false);
    expect(bridge.cancel).toHaveBeenCalledTimes(1);
    expect(onText).not.toHaveBeenCalled();
  });

  it("shows a permission error when the microphone is refused", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    rec.state.fail = new VoiceError("permission", "Microphone access was denied.");
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={() => {}} />);
    fireEvent.click(button());
    await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/denied/));
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
  });

  it("surfaces install problems from the native side and recovers", async () => {
    const bridge = fakeBridge({
      prewarm: vi.fn(async () => { throw { code: "model_missing", message: "x" }; }),
      transcribe: vi.fn(async () => { throw { code: "model_missing", message: "x" }; }),
    });
    const rec = fakeRecorder();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />);
    fireEvent.click(button());
    await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/install\.sh --download-model/));
    expect(rec.state.cancelled).toBe(1); // recording aborted as soon as the warm-up failed
    expect(button().getAttribute("title")).toBe("Dictate (EN)");
    expect(onText).not.toHaveBeenCalled();
  });

  it("reports silence instead of inserting an empty string", async () => {
    const bridge = fakeBridge({ transcribe: vi.fn(async () => ({ text: "   ", language: "en", duration_secs: 1, infer_secs: 0.1, load_secs: 0 })) });
    const rec = fakeRecorder();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(button());
    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe("No speech detected."));
    expect(onText).not.toHaveBeenCalled();
  });

  it("stops by itself at the cap and still transcribes", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    const onText = vi.fn();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={onText} />);
    fireEvent.click(button());
    await waitFor(() => expect(button().getAttribute("aria-pressed")).toBe("true"));
    rec.state.opts!.onAutoStop!();
    await waitFor(() => expect(onText).toHaveBeenCalledWith("hello world"));
  });

  it("does nothing while disabled", async () => {
    const bridge = fakeBridge();
    const rec = fakeRecorder();
    render(() => <VoiceButton bridge={bridge} recorder={rec.factory} onText={() => {}} disabled />);
    fireEvent.click(button());
    await flush();
    expect(rec.factory).not.toHaveBeenCalled();
    expect(bridge.prewarm).not.toHaveBeenCalled();
  });
});

describe("error copy", () => {
  it("turns native codes into actionable messages", () => {
    expect(describeVoiceError(new VoiceError("python_missing", ""))).toMatch(/install\.sh/);
    expect(describeVoiceError(new VoiceError("audio_too_long", ""))).toMatch(/120s/);
    expect(describeVoiceError(new VoiceError("timeout", ""))).toMatch(/timed out/);
    expect(describeVoiceError(new VoiceError("weird", "raw message"))).toBe("raw message");
  });
});
