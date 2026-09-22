import { expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { Transcript } from "../src/Transcript";
import { applyStreamEvent, buildTranscript, visibleTranscript } from "../src/transcriptModel";
import { storedToStream, type StreamEvent } from "../src/stream";

const cancelled = (sequence: number) => storedToStream({
  id: sequence, sequence, event_id: `cancel-${sequence}`, event_type: "assistant_message",
  timestamp: "", content: "Cancelled", metadata: { success: false, resumable: true },
})!;
const resume = (id: string, queued = false): StreamEvent => ({
  type: "user_message", data: { id, content: "You were interrupted, resume your work.", queued },
});

it("replays repeated cancellations with only the current attempt's notice visible", () => {
  const events = [cancelled(49), resume("first"), cancelled(76)];
  const items = buildTranscript(events);
  const { container } = render(() => <Transcript items={items} />);
  expect(container.textContent?.match(/Mission cancelled/g)).toHaveLength(1);
  expect(container.textContent).not.toContain("Mission failed");
  expect(items.filter(item => item.kind === "error")).toHaveLength(2);

  const resumed = applyStreamEvent(items, resume("second"));
  expect(visibleTranscript(resumed).filter(item => item.kind === "error")).toHaveLength(0);
  expect(resumed).toEqual(buildTranscript([...events, resume("second")]));
});

it("dismisses the notice live only when a queued follow-up is delivered", () => {
  const [items, setItems] = createSignal(buildTranscript([cancelled(49)]));
  const { container } = render(() => <Transcript items={items()} />);
  setItems(items => applyStreamEvent(items, resume("retry", true)));
  expect(container.textContent).toContain("Mission cancelled");
  setItems(items => applyStreamEvent(items, resume("retry")));
  expect(container.textContent).not.toContain("Mission cancelled");
});

it("preserves diagnostic errors and labels actual failures correctly", () => {
  const items = buildTranscript([
    { type: "error", data: { message: "Transport unavailable" } },
    resume("retry"),
    { type: "assistant_message", data: { success: false, content: "Build failed" } },
  ]);
  const { container } = render(() => <Transcript items={items} />);
  expect(container.textContent).toContain("Transport unavailable");
  expect(container.textContent).toContain("Mission failed");
  expect(container.textContent).toContain("Build failed");
});
