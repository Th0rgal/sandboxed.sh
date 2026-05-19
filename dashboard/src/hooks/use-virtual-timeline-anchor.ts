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
      requestAnimationFrame(() => {
        virtualizer.scrollToIndex(itemCount - 1, {
          align: "end",
          behavior,
        });
        const el = scrollElementRef.current;
        if (el) {
          el.scrollTop = el.scrollHeight;
        }
      });
    },
    [itemCount, scrollElementRef, virtualizer],
  );

  useEffect(() => {
    const el = scrollElementRef.current;
    if (!el) return;
    updateAnchorFromScroll();
    el.addEventListener("scroll", updateAnchorFromScroll, { passive: true });
    return () => el.removeEventListener("scroll", updateAnchorFromScroll);
  }, [scrollElementRef, updateAnchorFromScroll]);

  useEffect(() => {
    isAtBottomRef.current = true;
    setIsAtBottom(true);
    if (itemCount > 0) {
      rafRef.current = requestAnimationFrame(() => {
        virtualizer.scrollToIndex(itemCount - 1, {
          align: "end",
          behavior: "instant",
        });
      });
    }
    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
    // Reset only when the timeline identity changes; ordinary item changes
    // are handled by the anchor-preserving layout effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetKey]);

  useLayoutEffect(() => {
    if (!isAtBottomRef.current) {
      return;
    }
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }

    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (itemCount <= 0) return;

      virtualizer.scrollToIndex(itemCount - 1, { align: "end" });
      requestAnimationFrame(() => {
        virtualizer.scrollToIndex(itemCount - 1, { align: "end" });
        const el = scrollElementRef.current;
        if (el) {
          el.scrollTop = el.scrollHeight;
        }
      });
    });

    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [changeKey, itemCount, scrollElementRef, virtualizer]);

  return {
    isAtBottom,
    scrollToBottom,
  };
}
