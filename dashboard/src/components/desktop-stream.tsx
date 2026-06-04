"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import type { MouseEvent, WheelEvent, KeyboardEvent } from "react";
import { cn } from "@/lib/utils";
import { getValidJwt } from "@/lib/auth";
import { getRuntimeApiBase } from "@/lib/settings";
import {
  AppWindow,
  MonitorOff,
  Play,
  Pause,
  RefreshCw,
  X,
  Maximize2,
  Minimize2,
  PictureInPicture2,
  Gauge,
  Keyboard,
  MousePointer2,
  ScanLine,
  SlidersHorizontal,
} from "lucide-react";

interface DesktopStreamProps {
  displayId?: string;
  displayServer?: string;
  compositor?: string;
  className?: string;
  onClose?: () => void;
  initialFps?: number;
  initialQuality?: number;
}

type ConnectionState = "connecting" | "connected" | "disconnected" | "error";
type ViewportMode = "fit" | "fill";

export function DesktopStream({
  displayId = ":99",
  displayServer,
  compositor,
  className,
  onClose,
  initialFps = 10,
  initialQuality = 70,
}: DesktopStreamProps) {
  const [connectionState, setConnectionState] =
    useState<ConnectionState>("connecting");
  const [isPaused, setIsPaused] = useState(false);
  const [frameCount, setFrameCount] = useState(0);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [inputErrorMessage, setInputErrorMessage] = useState<string | null>(
    null
  );
  const [fps, setFps] = useState(initialFps);
  const [quality, setQuality] = useState(initialQuality);
  const [viewportMode, setViewportMode] = useState<ViewportMode>("fit");
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [isPipActive, setIsPipActive] = useState(false);
  const [isPipSupported, setIsPipSupported] = useState(false);
  const streamBackend = (displayServer || "wayland").toLowerCase();
  const isWayland = streamBackend === "wayland";
  const backendLabel = isWayland ? "Wayland app stream" : "Legacy desktop stream";
  const compositorLabel = compositor
    ? compositor.toUpperCase()
    : isWayland
      ? "SWAY"
      : "Legacy";
  const streamTitle = isWayland ? "Interactive app surface" : "Legacy desktop surface";
  const latencyLabel = fps >= 24 ? "Low latency" : fps >= 12 ? "Balanced" : "Battery saver";

  const wsRef = useRef<WebSocket | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const pipVideoRef = useRef<HTMLVideoElement | null>(null);
  const pipStreamRef = useRef<MediaStream | null>(null);
  const connectionIdRef = useRef(0); // Guard against stale callbacks from old connections
  const moveRafRef = useRef<number | null>(null);
  const pendingMoveRef = useRef<{ x: number; y: number } | null>(null);
  const mouseDownRef = useRef(false);
  const mouseDragActiveRef = useRef(false);
  const mouseDownCoordsRef = useRef<{ x: number; y: number } | null>(null);
  const mouseDownButtonRef = useRef(1);
  const lastCoordsRef = useRef<{ x: number; y: number } | null>(null);
  const mouseDownSentRef = useRef(false);
  const holdTimeoutRef = useRef<number | null>(null);
  const clickTimeoutRef = useRef<number | null>(null);
  const pendingClickRef = useRef<{ x: number; y: number; time: number } | null>(
    null
  );

  // Refs to store current values without triggering reconnection on slider changes
  const fpsRef = useRef(initialFps);
  const qualityRef = useRef(initialQuality);

  // Keep refs in sync with state
  useEffect(() => {
    fpsRef.current = fps;
    qualityRef.current = quality;
  }, [fps, quality]);

  // Build WebSocket URL - uses refs to get current values without causing reconnections
  const buildWsUrl = useCallback(() => {
    const baseUrl = getRuntimeApiBase();

    // Convert https to wss, http to ws
    const wsUrl = baseUrl
      .replace("https://", "wss://")
      .replace("http://", "ws://");

    // Use refs for current values - refs don't trigger useCallback dependency changes
    const params = new URLSearchParams({
      display: displayId,
      fps: fpsRef.current.toString(),
      quality: qualityRef.current.toString(),
    });

    return `${wsUrl}/api/desktop/stream?${params}`;
  }, [displayId]);

  // Connect to WebSocket
  const connect = useCallback(() => {
    // Clean up existing connection
    if (wsRef.current) {
      wsRef.current.close();
    }

    // Increment connection ID to invalidate stale callbacks
    connectionIdRef.current += 1;
    const thisConnectionId = connectionIdRef.current;

    setConnectionState("connecting");
    setErrorMessage(null);
    setInputErrorMessage(null);

    const url = buildWsUrl();

    // Get JWT token using proper auth module
    const jwt = getValidJwt();
    const token = jwt?.token ?? null;

    // Create WebSocket with subprotocol auth
    const protocols = token ? ["sandboxed", `jwt.${token}`] : ["sandboxed"];
    const ws = new WebSocket(url, protocols);

    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      // Guard against stale callbacks from previous connections
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("connected");
      setErrorMessage(null);
      setInputErrorMessage(null);
    };

    ws.onmessage = (event) => {
      // Guard against stale callbacks
      if (connectionIdRef.current !== thisConnectionId) return;
      if (event.data instanceof ArrayBuffer) {
        // Binary data = JPEG frame
        const blob = new Blob([event.data], { type: "image/jpeg" });
        const imageUrl = URL.createObjectURL(blob);

        const img = new Image();
        img.onload = () => {
          const canvas = canvasRef.current;
          if (canvas) {
            const ctx = canvas.getContext("2d");
            if (ctx) {
              // Resize canvas to match image
              if (
                canvas.width !== img.width ||
                canvas.height !== img.height
              ) {
                canvas.width = img.width;
                canvas.height = img.height;
              }
              ctx.drawImage(img, 0, 0);
              setFrameCount((prev) => prev + 1);
              setInputErrorMessage(null);
            }
          }
          URL.revokeObjectURL(imageUrl);
        };
        img.onerror = () => {
          // Revoke URL on failed load to prevent memory leak
          URL.revokeObjectURL(imageUrl);
        };
        img.src = imageUrl;
      } else if (typeof event.data === "string") {
        // Text message = JSON (error or control response)
        try {
          const json = JSON.parse(event.data);
          if (json.error) {
            const message = json.message || json.error;
            if (json.error === "input_failed") {
              setInputErrorMessage(message);
            } else {
              setErrorMessage(message);
            }
          }
        } catch {
          // Ignore parse errors
        }
      }
    };

    ws.onerror = () => {
      // Guard against stale callbacks
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("error");
      setErrorMessage("Connection error");
    };

    ws.onclose = () => {
      // Guard against stale callbacks from previous connections
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("disconnected");
    };

    wsRef.current = ws;
  }, [buildWsUrl]);

  // Send command to server
  const sendCommand = useCallback((cmd: Record<string, unknown>) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(cmd));
    }
  }, []);

  const getCanvasCoords = useCallback(
    (event: { clientX: number; clientY: number }) => {
      const canvas = canvasRef.current;
      if (!canvas) return null;
      const rect = canvas.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return null;
      const x = Math.round(
        ((event.clientX - rect.left) * canvas.width) / rect.width
      );
      const y = Math.round(
        ((event.clientY - rect.top) * canvas.height) / rect.height
      );
      return {
        x: Math.max(0, Math.min(canvas.width - 1, x)),
        y: Math.max(0, Math.min(canvas.height - 1, y)),
      };
    },
    []
  );

  const sendMouseMove = useCallback(
    (x: number, y: number) => {
      sendCommand({ t: "move", x, y });
    },
    [sendCommand]
  );

  const handleMouseMove = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      const coords = getCanvasCoords(event);
      if (!coords) return;
      lastCoordsRef.current = coords;
      if (mouseDownRef.current && !mouseDragActiveRef.current) {
        const start = mouseDownCoordsRef.current;
        if (start) {
          const dx = coords.x - start.x;
          const dy = coords.y - start.y;
          if (Math.hypot(dx, dy) >= 3) {
            mouseDragActiveRef.current = true;
            if (!mouseDownSentRef.current) {
              pendingClickRef.current = null;
              if (clickTimeoutRef.current) {
                clearTimeout(clickTimeoutRef.current);
                clickTimeoutRef.current = null;
              }
              sendCommand({
                t: "mouse_down",
                x: start.x,
                y: start.y,
                button: mouseDownButtonRef.current,
              });
              mouseDownSentRef.current = true;
            }
            if (holdTimeoutRef.current) {
              clearTimeout(holdTimeoutRef.current);
              holdTimeoutRef.current = null;
            }
          }
        }
      }
      pendingMoveRef.current = coords;
      if (moveRafRef.current !== null) return;
      moveRafRef.current = requestAnimationFrame(() => {
        moveRafRef.current = null;
        if (pendingMoveRef.current) {
          sendMouseMove(pendingMoveRef.current.x, pendingMoveRef.current.y);
          pendingMoveRef.current = null;
        }
      });
    },
    [connectionState, getCanvasCoords, sendCommand, sendMouseMove]
  );

  const handleMouseDown = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      if (event.button !== 0) return;
      const coords = getCanvasCoords(event);
      if (!coords) return;
      mouseDownRef.current = true;
      mouseDragActiveRef.current = false;
      mouseDownCoordsRef.current = coords;
      mouseDownButtonRef.current = 1;
      lastCoordsRef.current = coords;
      mouseDownSentRef.current = false;
      if (holdTimeoutRef.current) {
        clearTimeout(holdTimeoutRef.current);
        holdTimeoutRef.current = null;
      }
      if (clickTimeoutRef.current) {
        clearTimeout(clickTimeoutRef.current);
        clickTimeoutRef.current = null;
      }
      holdTimeoutRef.current = window.setTimeout(() => {
        if (
          mouseDownRef.current &&
          !mouseDragActiveRef.current &&
          !mouseDownSentRef.current
        ) {
          pendingClickRef.current = null;
          sendCommand({
            t: "mouse_down",
            x: coords.x,
            y: coords.y,
            button: mouseDownButtonRef.current,
          });
          mouseDownSentRef.current = true;
        }
        holdTimeoutRef.current = null;
      }, 150);
      event.preventDefault();
      event.stopPropagation();
      containerRef.current?.focus();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

  const handleMouseUp = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (!mouseDownRef.current) return;
      if (connectionState !== "connected") return;
      if (event.button !== 0) return;
      const coords = getCanvasCoords(event) ?? lastCoordsRef.current;
      if (holdTimeoutRef.current) {
        clearTimeout(holdTimeoutRef.current);
        holdTimeoutRef.current = null;
      }
      mouseDownRef.current = false;
      mouseDownCoordsRef.current = null;
      if (!coords) return;
      if (mouseDragActiveRef.current) {
        mouseDragActiveRef.current = false;
      }
      if (mouseDownSentRef.current) {
        sendCommand({
          t: "mouse_up",
          x: coords.x,
          y: coords.y,
          button: mouseDownButtonRef.current,
        });
        mouseDownSentRef.current = false;
      } else {
        const now = Date.now();
        const pending = pendingClickRef.current;
        const isDouble =
          pending &&
          now - pending.time <= 250 &&
          Math.hypot(coords.x - pending.x, coords.y - pending.y) <= 4;
        if (isDouble) {
          if (clickTimeoutRef.current) {
            clearTimeout(clickTimeoutRef.current);
            clickTimeoutRef.current = null;
          }
          pendingClickRef.current = null;
          sendCommand({
            t: "click",
            x: coords.x,
            y: coords.y,
            button: mouseDownButtonRef.current,
            double: true,
          });
        } else {
          pendingClickRef.current = { x: coords.x, y: coords.y, time: now };
          if (clickTimeoutRef.current) {
            clearTimeout(clickTimeoutRef.current);
          }
          clickTimeoutRef.current = window.setTimeout(() => {
            const queued = pendingClickRef.current;
            if (!queued) return;
            sendCommand({
              t: "click",
              x: queued.x,
              y: queued.y,
              button: mouseDownButtonRef.current,
              double: false,
            });
            pendingClickRef.current = null;
            clickTimeoutRef.current = null;
          }, 250);
        }
      }
      event.preventDefault();
      event.stopPropagation();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

  const handleMouseLeave = useCallback(() => {
    if (!mouseDownRef.current) return;
    if (connectionState !== "connected") return;
    const coords = lastCoordsRef.current;
    if (holdTimeoutRef.current) {
      clearTimeout(holdTimeoutRef.current);
      holdTimeoutRef.current = null;
    }
    mouseDownRef.current = false;
    mouseDownCoordsRef.current = null;
    if (!coords) return;
    if (mouseDragActiveRef.current) {
      mouseDragActiveRef.current = false;
    }
    if (mouseDownSentRef.current) {
      sendCommand({
        t: "mouse_up",
        x: coords.x,
        y: coords.y,
        button: mouseDownButtonRef.current,
      });
      mouseDownSentRef.current = false;
    }
  }, [connectionState, sendCommand]);

  const handleAuxClick = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      if (event.button !== 1) return;
      const coords = getCanvasCoords(event);
      if (!coords) return;
      sendCommand({
        t: "click",
        x: coords.x,
        y: coords.y,
        button: 2,
        double: false,
      });
      event.preventDefault();
      event.stopPropagation();
      containerRef.current?.focus();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

  const handleContextMenu = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      const coords = getCanvasCoords(event);
      if (!coords) return;
      sendCommand({
        t: "click",
        x: coords.x,
        y: coords.y,
        button: 3,
        double: false,
      });
      event.preventDefault();
      event.stopPropagation();
      containerRef.current?.focus();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

  const handleWheel = useCallback(
    (event: WheelEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      const coords = getCanvasCoords(event);
      const scale = event.deltaMode === 1 ? 40 : event.deltaMode === 2 ? 360 : 1;
      sendCommand({
        t: "scroll",
        delta_x: Math.round(event.deltaX * scale),
        delta_y: Math.round(event.deltaY * scale),
        x: coords?.x ?? null,
        y: coords?.y ?? null,
      });
      event.preventDefault();
      event.stopPropagation();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

  const formatKeyForXdotool = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      let key = event.key;
      const modifiers: string[] = [];
      if (event.ctrlKey) modifiers.push("ctrl");
      if (event.altKey) modifiers.push("alt");
      if (event.metaKey) modifiers.push("super");
      if (event.shiftKey) modifiers.push("shift");

      switch (key) {
        case " ":
          key = "space";
          break;
        case "Enter":
          key = "Return";
          break;
        case "Backspace":
          key = "BackSpace";
          break;
        case "Escape":
          key = "Escape";
          break;
        case "Tab":
          key = "Tab";
          break;
        case "ArrowUp":
          key = "Up";
          break;
        case "ArrowDown":
          key = "Down";
          break;
        case "ArrowLeft":
          key = "Left";
          break;
        case "ArrowRight":
          key = "Right";
          break;
        case "PageUp":
          key = "Page_Up";
          break;
        case "PageDown":
          key = "Page_Down";
          break;
        case "Delete":
          key = "Delete";
          break;
        case "Home":
          key = "Home";
          break;
        case "End":
          key = "End";
          break;
        default:
          break;
      }

      if (key.length === 1) {
        key = key.toLowerCase();
      }

      if (modifiers.length) {
        return `${modifiers.join("+")}+${key}`;
      }
      return key;
    },
    []
  );

  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (connectionState !== "connected") return;
      if (event.key === "Shift" || event.key === "Control" || event.key === "Alt" || event.key === "Meta") {
        return;
      }
      const isPrintable = event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey;
      if (isPrintable) {
        sendCommand({ t: "type", text: event.key });
      } else {
        const formatted = formatKeyForXdotool(event);
        sendCommand({ t: "key", key: formatted });
      }
      event.preventDefault();
      event.stopPropagation();
    },
    [connectionState, formatKeyForXdotool, sendCommand]
  );

  // Control handlers
  const handlePause = useCallback(() => {
    setIsPaused(true);
    sendCommand({ t: "pause" });
  }, [sendCommand]);

  const handleResume = useCallback(() => {
    setIsPaused(false);
    sendCommand({ t: "resume" });
  }, [sendCommand]);

  const handleFpsChange = useCallback(
    (newFps: number) => {
      setFps(newFps);
      sendCommand({ t: "fps", fps: newFps });
    },
    [sendCommand]
  );

  const handleQualityChange = useCallback(
    (newQuality: number) => {
      setQuality(newQuality);
      sendCommand({ t: "quality", quality: newQuality });
    },
    [sendCommand]
  );

  const handleFullscreen = useCallback(() => {
    if (!containerRef.current) return;

    if (!isFullscreen) {
      // Don't set state here - let the fullscreenchange event handler do it
      // This prevents state desync if fullscreen request fails
      containerRef.current.requestFullscreen?.();
    } else {
      document.exitFullscreen?.();
    }
  }, [isFullscreen]);

  // Picture-in-Picture handler
  const handlePip = useCallback(async () => {
    if (!canvasRef.current) return;

    if (isPipActive && document.pictureInPictureElement) {
      // Exit PiP
      try {
        await document.exitPictureInPicture();
      } catch {
        // Ignore errors
      }
      return;
    }

    try {
      // Stop any existing stream tracks to prevent resource leaks
      if (pipStreamRef.current) {
        pipStreamRef.current.getTracks().forEach((track) => track.stop());
      }

      // Create a video element from canvas stream
      const canvas = canvasRef.current;
      const stream = canvas.captureStream(fps);
      pipStreamRef.current = stream;

      // Create or reuse video element
      if (!pipVideoRef.current) {
        const video = document.createElement("video");
        video.muted = true;
        video.autoplay = true;
        video.playsInline = true;
        // Attach PiP event listeners directly to the video element
        // These events fire on the video, not document, so we need to listen here
        video.addEventListener("enterpictureinpicture", () => setIsPipActive(true));
        video.addEventListener("leavepictureinpicture", () => setIsPipActive(false));
        pipVideoRef.current = video;
      }

      pipVideoRef.current.srcObject = stream;
      await pipVideoRef.current.play();

      // Request PiP
      await pipVideoRef.current.requestPictureInPicture();
    } catch (err) {
      console.error("Failed to enter Picture-in-Picture:", err);
    }
  }, [isPipActive, fps]);

  // Check PiP support on mount
  useEffect(() => {
    setTimeout(() => {
      setIsPipSupported(
        "pictureInPictureEnabled" in document && document.pictureInPictureEnabled
      );
    }, 0);
  }, []);

  // Cleanup PiP resources on unmount
  // Note: We don't forcibly exit PiP here to match iOS behavior where
  // PiP continues when the sheet is dismissed. The PiP will naturally
  // close when the WebSocket disconnects and the stream ends.
  useEffect(() => {
    return () => {
      // Only stop stream tracks if PiP is not active
      // This allows PiP to continue showing the last frame briefly
      if (!document.pictureInPictureElement && pipStreamRef.current) {
        pipStreamRef.current.getTracks().forEach((track) => track.stop());
      }
    };
  }, []);

  // Connect on mount
  useEffect(() => {
    const timeout = window.setTimeout(() => connect(), 0);
    return () => {
      window.clearTimeout(timeout);
      wsRef.current?.close();
    };
  }, [connect]);

  useEffect(() => {
    return () => {
      if (holdTimeoutRef.current) {
        clearTimeout(holdTimeoutRef.current);
      }
      if (clickTimeoutRef.current) {
        clearTimeout(clickTimeoutRef.current);
      }
    };
  }, []);

  // Listen for fullscreen changes and errors
  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(!!document.fullscreenElement);
    };
    const handleFullscreenError = () => {
      // Fullscreen request failed - ensure state reflects reality
      setIsFullscreen(false);
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    document.addEventListener("fullscreenerror", handleFullscreenError);
    return () => {
      document.removeEventListener("fullscreenchange", handleFullscreenChange);
      document.removeEventListener("fullscreenerror", handleFullscreenError);
    };
  }, []);

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      data-testid="app-stream-panel"
      data-stream-display={displayId}
      data-stream-backend={streamBackend}
      className={cn(
        "app-stream-surface relative flex min-h-[320px] flex-col overflow-hidden rounded-xl border border-white/[0.08] bg-[#08090b] shadow-[0_24px_80px_rgba(0,0,0,0.32)]",
        className
      )}
    >
      <div className="flex items-center justify-between gap-3 border-b border-white/[0.08] bg-white/[0.035] px-3 py-2.5">
        <div className="flex min-w-0 items-center gap-3">
          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-indigo-400/25 bg-indigo-400/12 text-indigo-200">
            <AppWindow className="h-[18px] w-[18px]" />
          </div>
          <div className="min-w-0">
            <div className="flex min-w-0 items-center gap-2">
              <span className="truncate text-sm font-semibold text-white/90">
                {streamTitle}
              </span>
              <span className="hidden rounded-md border border-white/[0.08] bg-black/30 px-1.5 py-0.5 font-mono text-[11px] text-white/55 sm:inline">
                {displayId}
              </span>
            </div>
            <div className="mt-0.5 flex min-w-0 items-center gap-2 text-[11px] text-white/45">
              <span className="truncate">{backendLabel}</span>
              <span className="text-white/20">/</span>
              <span>{compositorLabel}</span>
            </div>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <div
            className={cn(
              "hidden items-center gap-1.5 rounded-md border px-2 py-1 text-xs sm:flex",
              connectionState === "connected"
                ? "border-emerald-400/20 bg-emerald-400/10 text-emerald-300"
                : connectionState === "connecting"
                ? "border-amber-400/20 bg-amber-400/10 text-amber-300"
                : "border-red-400/20 bg-red-400/10 text-red-300"
            )}
          >
            <span
              className={cn(
                "h-1.5 w-1.5 rounded-full",
                connectionState === "connected"
                  ? "bg-emerald-300"
                  : connectionState === "connecting"
                  ? "animate-pulse bg-amber-300"
                  : "bg-red-300"
              )}
            />
            {connectionState === "connected"
              ? isPaused
                ? "Paused"
                : "Live"
              : connectionState === "connecting"
              ? "Connecting"
              : "Offline"}
          </div>
          {isPipSupported && (
            <button
              onClick={handlePip}
              disabled={connectionState !== "connected"}
              className={cn(
                "rounded-lg p-2 transition-colors",
                connectionState === "connected"
                  ? isPipActive
                    ? "bg-indigo-400/20 text-indigo-200 hover:bg-indigo-400/25"
                    : "text-white/55 hover:bg-white/10 hover:text-white"
                  : "text-white/30 cursor-not-allowed"
              )}
              title={isPipActive ? "Exit Picture-in-Picture" : "Picture-in-Picture"}
            >
              <PictureInPicture2 className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={handleFullscreen}
            className="rounded-lg p-2 text-white/55 transition-colors hover:bg-white/10 hover:text-white"
            title={isFullscreen ? "Exit fullscreen" : "Fullscreen"}
          >
            {isFullscreen ? (
              <Minimize2 className="w-4 h-4" />
            ) : (
              <Maximize2 className="w-4 h-4" />
            )}
          </button>
          {onClose && (
            <button
              onClick={onClose}
              className="rounded-lg p-2 text-white/55 transition-colors hover:bg-white/10 hover:text-white"
              title="Close"
            >
              <X className="w-4 h-4" />
            </button>
          )}
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2 border-b border-white/[0.06] bg-black/20 px-3 py-2 text-xs text-white/50">
        <span className="inline-flex items-center gap-1.5 rounded-md bg-white/[0.04] px-2 py-1">
          <Gauge className="h-3.5 w-3.5 text-indigo-200" />
          {latencyLabel}
        </span>
        <span className="inline-flex items-center gap-1.5 rounded-md bg-white/[0.04] px-2 py-1">
          <MousePointer2 className="h-3.5 w-3.5 text-white/45" />
          Pointer
        </span>
        <span className="inline-flex items-center gap-1.5 rounded-md bg-white/[0.04] px-2 py-1">
          <Keyboard className="h-3.5 w-3.5 text-white/45" />
          Keyboard
        </span>
        <span className="ml-auto hidden font-mono text-[11px] text-white/35 sm:inline">
          {frameCount} frames
        </span>
      </div>

      {/* App viewport */}
      <div className="flex min-h-[220px] flex-1 items-center justify-center bg-[radial-gradient(circle_at_center,rgba(99,102,241,0.08),transparent_42%),#020203] p-2">
        {connectionState === "connected" && !errorMessage ? (
          <div className="relative flex h-full w-full items-center justify-center overflow-hidden rounded-lg border border-white/[0.06] bg-black shadow-inner">
            <canvas
              ref={canvasRef}
              data-testid="app-stream-canvas"
              className={cn(
                "block",
                viewportMode === "fit"
                  ? "max-h-full max-w-full object-contain"
                  : "h-full w-full object-cover"
              )}
              onMouseMove={handleMouseMove}
              onMouseDown={handleMouseDown}
              onMouseUp={handleMouseUp}
              onMouseLeave={handleMouseLeave}
              onAuxClick={handleAuxClick}
              onContextMenu={handleContextMenu}
              onWheel={handleWheel}
            />
            {inputErrorMessage && (
              <div className="pointer-events-none absolute left-3 right-3 top-3 rounded-lg border border-amber-400/25 bg-black/75 px-3 py-2 text-xs text-amber-200 shadow-lg">
                {inputErrorMessage}
              </div>
            )}
          </div>
        ) : connectionState === "connecting" ? (
          <div className="h-full w-full p-4">
            <div className="flex h-full min-h-[220px] items-center justify-center rounded-lg border border-white/[0.06] bg-white/[0.03]">
              <div className="h-28 w-44 animate-pulse rounded-md border border-white/[0.05] bg-white/[0.04]" />
            </div>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-4 text-white/60 px-6 py-8">
            <MonitorOff className="w-14 h-14 text-red-400/50" />
            <div className="flex flex-col items-center gap-1.5 text-center">
              <h3 className="text-base font-medium text-white/80">
                {errorMessage?.includes("no longer available") ||
                errorMessage?.includes("session may have")
                  ? "App Stream Unavailable"
                  : "Connection Lost"}
              </h3>
              <p className="max-w-[280px] text-sm text-white/50 leading-relaxed">
                {errorMessage?.includes("no longer available") ||
                errorMessage?.includes("session may have")
                  ? `Session ${displayId} has been closed. Select another app stream above.`
                  : errorMessage || "Unable to connect to the app stream."}
              </p>
            </div>
            <button
              onClick={connect}
              className="flex items-center gap-2 rounded-lg bg-indigo-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-indigo-600"
            >
              <RefreshCw className="w-4 h-4" />
              Reconnect
            </button>
          </div>
        )}
      </div>

      <div className="border-t border-white/[0.08] bg-white/[0.035] px-3 py-3">
        <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
          <div className="flex items-center gap-2">
            <button
              onClick={isPaused ? handleResume : handlePause}
              disabled={connectionState !== "connected"}
              className={cn(
                "rounded-lg p-2 transition-colors",
                connectionState === "connected"
                  ? "bg-white/10 text-white hover:bg-white/20"
                  : "bg-white/5 text-white/30 cursor-not-allowed"
              )}
              title={isPaused ? "Resume" : "Pause"}
            >
              {isPaused ? (
                <Play className="w-5 h-5" />
              ) : (
                <Pause className="w-5 h-5" />
              )}
            </button>

            <button
              onClick={connect}
              className="rounded-lg bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
              title="Reconnect"
            >
              <RefreshCw className="w-4 h-4" />
            </button>

            <div className="ml-1 inline-flex overflow-hidden rounded-lg border border-white/[0.08] bg-black/20">
              {(["fit", "fill"] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setViewportMode(mode)}
                  aria-label={mode === "fit" ? "Fit surface" : "Fill surface"}
                  className={cn(
                    "inline-flex items-center gap-1.5 px-2.5 py-2 text-xs font-medium capitalize transition-colors",
                    viewportMode === mode
                      ? "bg-indigo-400/18 text-indigo-100"
                      : "text-white/45 hover:bg-white/[0.06] hover:text-white/75"
                  )}
                  title={mode === "fit" ? "Fit app to surface" : "Fill surface"}
                >
                  <ScanLine className="h-3.5 w-3.5" />
                  {mode}
                </button>
              ))}
            </div>
          </div>

          <div className="flex min-w-0 flex-1 flex-col gap-2 lg:flex-row lg:items-center xl:max-w-xl">
            <div className="flex min-w-0 flex-1 items-center gap-2">
              <span className="flex w-14 items-center gap-1 text-xs text-white/45">
                <SlidersHorizontal className="h-3.5 w-3.5" />
                FPS
              </span>
              <input
                type="range"
                min={1}
                max={30}
                value={fps}
                onChange={(e) => handleFpsChange(Number(e.target.value))}
                aria-label="Stream FPS"
                className="h-1 flex-1 cursor-pointer appearance-none rounded-full bg-white/20 accent-indigo-400 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-indigo-400"
              />
              <span className="text-xs text-white/60 w-6 text-right tabular-nums">
                {fps}
              </span>
            </div>

            <div className="flex min-w-0 flex-1 items-center gap-2">
              <span className="w-14 text-xs text-white/45">Quality</span>
              <input
                type="range"
                min={10}
                max={100}
                step={5}
                value={quality}
                onChange={(e) => handleQualityChange(Number(e.target.value))}
                aria-label="Stream quality"
                className="h-1 flex-1 cursor-pointer appearance-none rounded-full bg-white/20 accent-indigo-400 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-indigo-400"
              />
              <span className="text-xs text-white/60 w-8 text-right tabular-nums">
                {quality}%
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
