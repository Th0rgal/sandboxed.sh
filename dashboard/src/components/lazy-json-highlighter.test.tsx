import { describe, it, expect, vi } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { LazyJsonHighlighter } from "./lazy-json-highlighter";

// Mock the heavy syntax highlighter modules so tests run instantly
vi.mock("react-syntax-highlighter", () => ({
  Prism: ({ children }: { children: string }) => (
    <pre data-testid="syntax-highlighter">{children}</pre>
  ),
}));
vi.mock("react-syntax-highlighter/dist/esm/styles/prism", () => ({
  oneDark: {},
}));

describe("LazyJsonHighlighter", () => {
  it("renders plain <pre> immediately before highlighter loads", () => {
    const { container } = render(
      <LazyJsonHighlighter>{'{"key": "value"}'}</LazyJsonHighlighter>
    );
    const pre = container.querySelector("pre");
    expect(pre).not.toBeNull();
    expect(pre!.textContent).toBe('{"key": "value"}');
  });

  it("loads syntax highlighter asynchronously", async () => {
    const { container } = render(
      <LazyJsonHighlighter>{'{"async": true}'}</LazyJsonHighlighter>
    );
    // After the dynamic import resolves, the highlighted version appears
    await waitFor(() => {
      const highlighted = container.querySelector(
        '[data-testid="syntax-highlighter"]'
      );
      expect(highlighted).not.toBeNull();
      expect(highlighted!.textContent).toBe('{"async": true}');
    });
  });

  it("applies custom background and text color to the plain fallback", () => {
    const { container } = render(
      <LazyJsonHighlighter
        background="rgba(239, 68, 68, 0.1)"
        textColor="rgb(248, 113, 113)"
      >
        error text
      </LazyJsonHighlighter>
    );
    const pre = container.querySelector("pre");
    expect(pre).not.toBeNull();
    expect(pre!.style.background).toBe("rgba(239, 68, 68, 0.1)");
    expect(pre!.style.color).toBe("rgb(248, 113, 113)");
  });
});
