import { describe, expect, it } from "vitest";

import {
  extractStateSignatureKey,
  stateSignatureKeyFromMessages,
  stripHermesControlTrailers,
  stripStateSignature,
} from "./hermes-state-signature";

const SIGNATURE =
  "[STATE_SIGNATURE: lean-silicon|phase-c-bridges|7a1c0aab/2cf06f78/874ea558|external-ci-wait|external-ci|formal-and-lint+gds-settling]";

describe("stripStateSignature", () => {
  it("removes a trailing signature and surrounding whitespace", () => {
    const body = `Phase C is progressing.\n\n${SIGNATURE}\n\n\n`;
    expect(stripStateSignature(body)).toBe("Phase C is progressing.");
  });

  it("removes stacked trailing signatures", () => {
    const body = `Done.\n\n[STATE_SIGNATURE: a|b]\n[STATE_SIGNATURE: c|d]\n`;
    expect(stripStateSignature(body)).toBe("Done.");
  });

  it("removes a trailing CTRL status trailer", () => {
    const body =
      "Formal repair is active.\n\n[CTRL: lean-silicon | mode=active | wait=0 | next=repair reachability]";
    expect(stripHermesControlTrailers(body)).toBe("Formal repair is active.");
  });

  it("removes mixed control trailers in either order", () => {
    expect(
      stripHermesControlTrailers(`Done.\n${SIGNATURE}\n[CTRL: lean-silicon | mode=active]`),
    ).toBe("Done.");
    expect(
      stripHermesControlTrailers(`Done.\n[CTRL: lean-silicon | mode=active]\n${SIGNATURE}`),
    ).toBe("Done.");
  });

  it("leaves CTRL metadata quoted mid-message intact", () => {
    const body = "The old format was [CTRL: project | mode=active] in reports.";
    expect(stripHermesControlTrailers(body)).toBe(body);
  });

  it("leaves a signature quoted mid-message intact", () => {
    const body = `The trailer looks like ${SIGNATURE} and routes the reply.`;
    expect(stripStateSignature(body)).toBe(body);
  });

  it("returns a signature-only body as empty", () => {
    expect(stripStateSignature(`${SIGNATURE}\n`).trim()).toBe("");
  });

  it("is a no-op on plain text", () => {
    expect(stripStateSignature("hello world")).toBe("hello world");
  });
});

describe("extractStateSignatureKey", () => {
  it("returns the first field of the signature", () => {
    expect(extractStateSignatureKey(`Report.\n${SIGNATURE}`)).toBe(
      "lean-silicon",
    );
  });

  it("returns the LAST signature's key when several appear", () => {
    const body = "[STATE_SIGNATURE: old-key|x]\ntext\n[STATE_SIGNATURE: new-key|y]";
    expect(extractStateSignatureKey(body)).toBe("new-key");
  });

  it("returns null when no signature is present", () => {
    expect(extractStateSignatureKey("no trailer here")).toBeNull();
  });
});

describe("stateSignatureKeyFromMessages", () => {
  it("scans backwards through assistant messages", () => {
    const key = stateSignatureKeyFromMessages([
      { role: "assistant", content: `old\n[STATE_SIGNATURE: stale-key|x]` },
      { role: "user", content: "next task" },
      { role: "assistant", content: `latest\n${SIGNATURE}` },
      { role: "user", content: "thanks" },
    ]);
    expect(key).toBe("lean-silicon");
  });

  it("ignores user messages and returns null without signatures", () => {
    expect(
      stateSignatureKeyFromMessages([
        { role: "user", content: "[STATE_SIGNATURE: not-from-assistant|x]" },
        { role: "assistant", content: "plain reply" },
      ]),
    ).toBeNull();
  });
});
