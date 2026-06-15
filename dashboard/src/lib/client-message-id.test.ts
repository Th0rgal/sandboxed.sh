import { afterEach, describe, expect, it, vi } from "vitest";

import { createClientMessageId } from "./client-message-id";

describe("createClientMessageId", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("uses crypto.randomUUID when available", () => {
    expect(
      createClientMessageId({
        randomUUID: () => "11111111-2222-4333-8444-555555555555",
        getRandomValues: (array) => array,
      }),
    ).toBe("11111111-2222-4333-8444-555555555555");
  });

  it("falls back to a v4 UUID when randomUUID is unavailable", () => {
    const id = createClientMessageId({
      getRandomValues: (array) => {
        array.set([
          0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
          0xbb, 0xcc, 0xdd, 0xee, 0xff,
        ]);
        return array;
      },
    });

    expect(id).toBe("00112233-4455-4677-8899-aabbccddeeff");
  });

  it("falls back to a Math.random v4 UUID when no crypto source exists", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);

    const id = createClientMessageId(
      {} as Parameters<typeof createClientMessageId>[0],
    );

    expect(id).toBe("00000000-0000-4000-8000-000000000000");
    expect(id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
  });
});
