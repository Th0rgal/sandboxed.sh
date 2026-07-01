"use client";

import { useEffect, useRef } from "react";
import type { MissionStatus } from "@/lib/api/missions";

/**
 * Hex colors matching the mission status dot palette used in the UI.
 */
const STATUS_COLORS: Record<MissionStatus, string> = {
  pending: "#fbbf24", // amber-400
  active: "#818cf8", // indigo-400
  awaiting_user: "#f472b6", // pink-400, reserved for "needs user"
  acknowledged: "#34d399", // emerald-400
  completed: "#34d399", // emerald-400
  // Failure statuses all share `bg-red-400` in `STATUS_DOT_COLORS`
  // (`lib/mission-status.ts`); the favicon dot now matches so the
  // status indicator is consistent across the UI.
  failed: "#f87171", // red-400
  interrupted: "#f87171", // red-400
  blocked: "#f87171", // red-400
  not_feasible: "#f87171", // red-400
  waiting_background: "#818cf8", // indigo-400, work still running in background
};

/** Status dot radius & position (bottom-right, on a 64×64 canvas). */
const DOT_RADIUS = 10;
const DOT_X = 52;
const DOT_Y = 52;

/**
 * Attention badge (top-right): drawn only while a mission is waiting for the
 * user to answer a prompt. Bigger than the status dot and in a distinct
 * corner so a pending question is unmistakable even in a background tab.
 */
const ATTENTION_RADIUS = 12;
const ATTENTION_X = 52;
const ATTENTION_Y = 12;
const ATTENTION_COLOR = "#f472b6"; // pink-400, the "needs user" hue

/** Always use the SVG source as the base image for canvas drawing. */
const BASE_FAVICON = "/favicon.svg";

/** Data attribute to identify our managed link element. */
const DATA_ATTR = "data-favicon-status";

/**
 * Dynamically overlays a coloured status dot on the favicon.
 *
 * Creates its own <link> element after Next.js-managed favicon links. Do not
 * remove framework-managed head nodes: React tracks those as hoistable
 * resources and expects to own their lifecycle during route updates.
 */
export function useFaviconStatus(
  status: MissionStatus | null,
  isRunning: boolean,
  awaitingUser = false,
) {
  const cachedImg = useRef<HTMLImageElement | null>(null);
  const originalLinks = useRef(new Map<HTMLLinkElement, { href: string; type: string | null }>());

  const restoreOriginalLinks = () => {
    originalLinks.current.forEach((original, link) => {
      if (!document.head.contains(link)) return;
      link.href = original.href;
      if (original.type === null) {
        link.removeAttribute("type");
      } else {
        link.type = original.type;
      }
    });
    originalLinks.current.clear();
  };

  useEffect(() => {
    // No active mission: remove only our managed dynamic icon.
    if (!status) {
      restoreOriginalLinks();
      // Remove our managed link
      document.querySelector(`link[${DATA_ATTR}]`)?.remove();
      return;
    }

    const color = isRunning ? "#818cf8" : STATUS_COLORS[status];

    const applyFavicon = (img: HTMLImageElement) => {
      const size = 64;
      const canvas = document.createElement("canvas");
      canvas.width = size;
      canvas.height = size;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      ctx.drawImage(img, 0, 0, size, size);

      // Dark border matching favicon background
      ctx.beginPath();
      ctx.arc(DOT_X, DOT_Y, DOT_RADIUS + 2, 0, Math.PI * 2);
      ctx.fillStyle = "#121214";
      ctx.fill();

      // Colored status dot
      ctx.beginPath();
      ctx.arc(DOT_X, DOT_Y, DOT_RADIUS, 0, Math.PI * 2);
      ctx.fillStyle = color;
      ctx.fill();

      // Attention badge (top-right) when the agent is paused on a question.
      if (awaitingUser) {
        ctx.beginPath();
        ctx.arc(ATTENTION_X, ATTENTION_Y, ATTENTION_RADIUS + 2, 0, Math.PI * 2);
        ctx.fillStyle = "#121214";
        ctx.fill();

        ctx.beginPath();
        ctx.arc(ATTENTION_X, ATTENTION_Y, ATTENTION_RADIUS, 0, Math.PI * 2);
        ctx.fillStyle = ATTENTION_COLOR;
        ctx.fill();
      }

      const dataUrl = canvas.toDataURL("image/png");

      document
        .querySelectorAll<HTMLLinkElement>(`link[rel="icon"]:not([${DATA_ATTR}])`)
        .forEach((link) => {
          if (!originalLinks.current.has(link)) {
            originalLinks.current.set(link, {
              href: link.href,
              type: link.getAttribute("type"),
            });
          }
          link.type = "image/png";
          link.href = dataUrl;
        });

      // Create or update our managed link
      let managed = document.querySelector<HTMLLinkElement>(`link[${DATA_ATTR}]`);
      if (!managed) {
        managed = document.createElement("link");
        managed.rel = "icon";
        managed.setAttribute(DATA_ATTR, "true");
      }
      managed.type = "image/png";
      managed.href = dataUrl;
      if (!document.head.contains(managed)) {
        document.head.appendChild(managed);
      }
    };

    let cancelled = false;

    const apply = () => {
      if (cancelled) return;
      if (cachedImg.current) {
        applyFavicon(cachedImg.current);
      } else {
        const img = new Image();
        img.crossOrigin = "anonymous";
        img.src = BASE_FAVICON;
        img.onload = () => {
          if (cancelled) return;
          cachedImg.current = img;
          applyFavicon(img);
        };
      }
    };

    apply();

    // Re-apply when tab becomes visible (Chrome tab restore, wake from sleep, etc.)
    const onVisibility = () => {
      if (document.visibilityState === "visible") apply();
    };
    document.addEventListener("visibilitychange", onVisibility);
    const observer = new MutationObserver(() => apply());
    observer.observe(document.head, { childList: true });

    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", onVisibility);
      observer.disconnect();
    };
  }, [status, isRunning, awaitingUser]);

  // Cleanup on full unmount: restore originals, remove managed link
  useEffect(() => {
    return () => {
      restoreOriginalLinks();
      document.querySelector(`link[${DATA_ATTR}]`)?.remove();
    };
  }, []);
}
