import { beforeEach, describe, expect, it } from "vitest";
import { peekReadyTranscript, putTranscript, putTranscriptItems } from "../src/missionCache";
import { cacheReset } from "../src/pageCache";
import type { StreamItem } from "../src/transcriptModel";

const user = (text: string): StreamItem => ({ kind: "user", key: "u", text });

beforeEach(() => cacheReset());

describe("transcript first-paint cache", () => {
  it("does not treat live SSE patches as ready", () => {
    putTranscriptItems("m1", [user("hi")]);
    expect(peekReadyTranscript("m1")).toBeUndefined();
  });
  it("treats an event-log snapshot as ready", () => {
    putTranscript("m1", { items: [user("hi")], stream: [], fromLog: true });
    expect(peekReadyTranscript("m1")?.items).toEqual([user("hi")]);
  });
  it("keeps the log snapshot when live items arrive", () => {
    putTranscript("m1", { items: [user("hi")], stream: [], fromLog: true });
    putTranscriptItems("m1", [user("hi"), { kind: "text", key: "t", text: "mashed", live: false }]);
    expect(peekReadyTranscript("m1")?.items).toEqual([user("hi")]);
  });
});
