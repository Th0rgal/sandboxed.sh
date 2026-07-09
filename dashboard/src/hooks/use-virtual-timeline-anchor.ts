"use client";

import type { Virtualizer } from "@tanstack/react-virtual";
import {
  type RefObject,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

type UseVirtualTimelineAnchorArgs<TScrollElement extends HTMLElement> = {
  scrollElementRef: RefObject<TScrollElement | null>;
  virtualizer: Virtualizer<TScrollElement, Element>;
  itemCount: number;
  changeKey: string;
  resetKey?: string | null;
  bottomThreshold?: number;
};

export function useVirtualTimelineAnchor<TScrollElement extends HTMLElement>({
  scrollElementRef,
  virtualizer,
  itemCount,
  changeKey,
  resetKey,
  bottomThreshold = 96,
}: UseVirtualTimelineAnchorArgs<TScrollElement>) {
  const [isAtBottom, setIsAtBottom] = useState(true);
  const isAtBottomRef = useRef(true);
  const rafRef = useRef<number | null>(null);
  const contentObserverRef = useRef<ResizeObserver | null>(null);

  const cancelPendingScroll = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  const scheduleBottomCorrection = useCallback(
    (targetIndex: number, behavior?: ScrollBehavior) => {
      cancelPendingScroll();
      rafRef.current = requestAnimationFrame(() => {
        virtualizer.scrollToIndex(targetIndex, {
          align: "end",
          behavior,
        });
        // Authoritative re-target in the same frame: scrollToIndex computes
        // its offset from the virtualizer's cached scroll-rect, which can
        // lag the real clientHeight (e.g. the composer just grew and shrank
        // the container) — landing above the bottom by exactly that delta.
        // Since the second rAF below is routinely cancelled by changeKey
        // churn during streaming, that short jump would otherwise stick.
        {
          const el = scrollElementRef.current;
          if (el) el.scrollTo({ top: el.scrollHeight, behavior });
        }
        rafRef.current = requestAnimationFrame(() => {
          rafRef.current = null;
          virtualizer.scrollToIndex(targetIndex, {
            align: "end",
            behavior,
          });
          const el = scrollElementRef.current;
          if (el) {
            el.scrollTop = el.scrollHeight;
          }
        });
      });
    },
    [cancelPendingScroll, scrollElementRef, virtualizer],
  );

  // Callback ref for the virtualizer's sizer element (the tall content div
  // whose height tracks `getTotalSize()`). A row expanding by default, a tool
  // result landing in an already-open row, or heavy markdown that only
  // settles its height after paint all grow this element *without* changing
  // `changeKey` — so neither the layout-effect correction nor tanstack's
  // (disabled) size-change compensation re-pins. Observing the sizer catches
  // every such growth; the `isAtBottomRef` gate means a user who has scrolled
  // up is never dragged back down.
  const registerContent = useCallback(
    (node: HTMLElement | null) => {
      contentObserverRef.current?.disconnect();
      contentObserverRef.current = null;
      if (!node || typeof ResizeObserver === "undefined") return;
      const observer = new ResizeObserver(() => {
        if (!isAtBottomRef.current) return;
        const el = scrollElementRef.current;
        if (el) el.scrollTop = el.scrollHeight;
      });
      observer.observe(node);
      contentObserverRef.current = observer;
    },
    [scrollElementRef],
  );

  useEffect(
    () => () => {
      contentObserverRef.current?.disconnect();
      contentObserverRef.current = null;
    },
    [],
  );

  const updateAnchorFromScroll = useCallback(() => {
    const el = scrollElementRef.current;
    if (!el) return;

    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const atBottom = distanceFromBottom <= bottomThreshold;
    isAtBottomRef.current = atBottom;
    setIsAtBottom(atBottom);
  }, [bottomThreshold, scrollElementRef]);

  const scrollToBottom = useCallback(
    (behavior: ScrollBehavior = "smooth") => {
      if (itemCount <= 0) return;
      isAtBottomRef.current = true;
      setIsAtBottom(true);
      virtualizer.scrollToIndex(itemCount - 1, {
        align: "end",
        behavior,
      });
      scheduleBottomCorrection(itemCount - 1, behavior);
    },
    [itemCount, scheduleBottomCorrection, virtualizer],
  );

  useEffect(() => {
    const el = scrollElementRef.current;
    if (!el) return;
    updateAnchorFromScroll();
    el.addEventListener("scroll", updateAnchorFromScroll, { passive: true });
    // The scroll element itself resizes when the composer below it grows or
    // shrinks with the draft (auto-growing textarea). The browser keeps
    // scrollTop on resize, so a user pinned at the bottom silently drifts
    // off it by the composer delta — and the next stream correction then
    // "jumps". Re-pin on resize while at bottom; otherwise just refresh the
    // anchor state.
    let resizeObserver: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => {
        if (isAtBottomRef.current) {
          el.scrollTop = el.scrollHeight;
        } else {
          updateAnchorFromScroll();
        }
      });
      resizeObserver.observe(el);
    }
    return () => {
      el.removeEventListener("scroll", updateAnchorFromScroll);
      resizeObserver?.disconnect();
    };
  }, [scrollElementRef, updateAnchorFromScroll]);

  useEffect(() => {
    isAtBottomRef.current = true;
    setIsAtBottom(true);
    if (itemCount > 0) {
      scheduleBottomCorrection(itemCount - 1, "instant");
    }
    return cancelPendingScroll;
    // Reset only when the timeline identity changes; ordinary item changes
    // are handled by the anchor-preserving layout effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetKey]);

  useLayoutEffect(() => {
    if (!isAtBottomRef.current) {
      return;
    }
    if (itemCount <= 0) return;
    scheduleBottomCorrection(itemCount - 1);

    return cancelPendingScroll;
  }, [cancelPendingScroll, changeKey, itemCount, scheduleBottomCorrection]);

  return {
    isAtBottom,
    scrollToBottom,
    registerContent,
  };
}
