"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import type { MouseEvent, WheelEvent, KeyboardEvent } from "react";
import { cn } from "@/lib/utils";
import { getValidJwt } from "@/lib/auth";
import { getRuntimeApiBase } from "@/lib/settings";
import {
  Monitor,
  MonitorOff,
  Play,
  Pause,
  RefreshCw,
  X,
  Settings,
  Maximize2,
  Minimize2,
  PictureInPicture2,
} from "lucide-react";

interface DesktopStreamProps {
  displayId?: string;
  className?: string;
  onClose?: () => void;
  initialFps?: number;
  initialQuality?: number;
}

type ConnectionState = "connecting" | "connected" | "disconnected" | "error";

export function DesktopStream({
  displayId = ":99",
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
  const [showControls, setShowControls] = useState(true);
  const [fps, setFps] = useState(initialFps);
  const [quality, setQuality] = useState(initialQuality);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [isPipActive, setIsPipActive] = useState(false);
  const [isPipSupported, setIsPipSupported] = useState(false);

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
  const suppressClickRef = useRef(false);
  const lastCoordsRef = useRef<{ x: number; y: number } | null>(null);

  // Refs to store current values without triggering reconnection on slider changes
  const fpsRef = useRef(initialFps);
  const qualityRef = useRef(initialQuality);

  // Keep refs in sync with state
  fpsRef.current = fps;
  qualityRef.current = quality;

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

    const url = buildWsUrl();

    // Get JWT token using proper auth module
    const jwt = getValidJwt();
    const token = jwt?.token ?? null;

    // Create WebSocket with subprotocol auth
    const protocols = token ? ["openagent", `jwt.${token}`] : ["openagent"];
    const ws = new WebSocket(url, protocols);

    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      // Guard against stale callbacks from previous connections
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("connected");
      setErrorMessage(null);
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
            setErrorMessage(json.message || json.error);
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
            suppressClickRef.current = true;
            sendCommand({
              t: "mouse_down",
              x: start.x,
              y: start.y,
              button: mouseDownButtonRef.current,
            });
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
      suppressClickRef.current = false;
      mouseDownCoordsRef.current = coords;
      mouseDownButtonRef.current = 1;
      lastCoordsRef.current = coords;
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
      const coords = getCanvasCoords(event) ?? lastCoordsRef.current;
      mouseDownRef.current = false;
      mouseDownCoordsRef.current = null;
      if (!coords) return;
      if (mouseDragActiveRef.current) {
        mouseDragActiveRef.current = false;
        suppressClickRef.current = true;
        sendCommand({
          t: "mouse_up",
          x: coords.x,
          y: coords.y,
          button: mouseDownButtonRef.current,
        });
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
    mouseDownRef.current = false;
    mouseDownCoordsRef.current = null;
    if (!coords) return;
    if (mouseDragActiveRef.current) {
      mouseDragActiveRef.current = false;
      suppressClickRef.current = true;
      sendCommand({
        t: "mouse_up",
        x: coords.x,
        y: coords.y,
        button: mouseDownButtonRef.current,
      });
    }
  }, [connectionState, sendCommand]);

  const handleClick = useCallback(
    (event: MouseEvent<HTMLCanvasElement>) => {
      if (connectionState !== "connected") return;
      if (suppressClickRef.current) {
        suppressClickRef.current = false;
        return;
      }
      const coords = getCanvasCoords(event);
      if (!coords) return;
      const isDouble = event.detail >= 2;
      sendCommand({
        t: "click",
        x: coords.x,
        y: coords.y,
        button: 1,
        double: isDouble,
      });
      event.preventDefault();
      event.stopPropagation();
      containerRef.current?.focus();
    },
    [connectionState, getCanvasCoords, sendCommand]
  );

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
      sendCommand({
        t: "scroll",
        delta_x: Math.round(event.deltaX),
        delta_y: Math.round(event.deltaY),
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
    setIsPipSupported(
      "pictureInPictureEnabled" in document && document.pictureInPictureEnabled
    );
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
    connect();
    return () => {
      wsRef.current?.close();
    };
  }, [connect]);

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
      className={cn(
        "relative flex flex-col bg-[#0a0a0a] rounded-xl overflow-hidden border border-white/[0.06]",
        className
      )}
      onMouseEnter={() => setShowControls(true)}
      onMouseLeave={() => setShowControls(false)}
    >
      {/* Header */}
      <div
        className={cn(
          "absolute top-0 left-0 right-0 z-10 flex items-center justify-between px-4 py-2 bg-gradient-to-b from-black/80 to-transparent transition-opacity duration-200",
          showControls ? "opacity-100" : "opacity-0"
        )}
      >
        <div className="flex items-center gap-3">
          <div
            className={cn(
              "flex items-center gap-2 text-xs",
              connectionState === "connected"
                ? "text-emerald-400"
                : connectionState === "connecting"
                ? "text-amber-400"
                : "text-red-400"
            )}
          >
            <div
              className={cn(
                "w-2 h-2 rounded-full",
                connectionState === "connected"
                  ? "bg-emerald-400"
                  : connectionState === "connecting"
                  ? "bg-amber-400 animate-pulse"
                  : "bg-red-400"
              )}
            />
            {connectionState === "connected"
              ? isPaused
                ? "Paused"
                : "Live"
              : connectionState === "connecting"
              ? "Connecting..."
              : "Disconnected"}
          </div>
          <span className="text-xs text-white/40 font-mono">{displayId}</span>
          <span className="text-xs text-white/30">{frameCount} frames</span>
        </div>

        <div className="flex items-center gap-2">
          {isPipSupported && (
            <button
              onClick={handlePip}
              disabled={connectionState !== "connected"}
              className={cn(
                "p-1.5 rounded-lg transition-colors",
                connectionState === "connected"
                  ? isPipActive
                    ? "bg-indigo-500/30 text-indigo-400 hover:bg-indigo-500/40"
                    : "hover:bg-white/10 text-white/60 hover:text-white"
                  : "text-white/30 cursor-not-allowed"
              )}
              title={isPipActive ? "Exit Picture-in-Picture" : "Picture-in-Picture"}
            >
              <PictureInPicture2 className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={handleFullscreen}
            className="p-1.5 rounded-lg hover:bg-white/10 text-white/60 hover:text-white transition-colors"
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
              className="p-1.5 rounded-lg hover:bg-white/10 text-white/60 hover:text-white transition-colors"
              title="Close"
            >
              <X className="w-4 h-4" />
            </button>
          )}
        </div>
      </div>

      {/* Canvas */}
      <div className="flex-1 flex items-center justify-center bg-black min-h-[200px]">
        {connectionState === "connected" && !errorMessage ? (
          <canvas
            ref={canvasRef}
            className="max-w-full max-h-full object-contain"
            onMouseMove={handleMouseMove}
            onMouseDown={handleMouseDown}
            onMouseUp={handleMouseUp}
            onMouseLeave={handleMouseLeave}
            onClick={handleClick}
            onAuxClick={handleAuxClick}
            onContextMenu={handleContextMenu}
            onWheel={handleWheel}
          />
        ) : connectionState === "connecting" ? (
          <div className="flex flex-col items-center gap-3 text-white/60">
            <Monitor className="w-12 h-12 animate-pulse" />
            <span className="text-sm">Connecting to desktop...</span>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-4 text-white/60 px-6 py-8">
            <MonitorOff className="w-14 h-14 text-red-400/50" />
            <div className="flex flex-col items-center gap-1.5 text-center">
              <h3 className="text-base font-medium text-white/80">
                {errorMessage?.includes("no longer available") ||
                errorMessage?.includes("session may have")
                  ? "Desktop Unavailable"
                  : "Connection Lost"}
              </h3>
              <p className="max-w-[280px] text-sm text-white/50 leading-relaxed">
                {errorMessage?.includes("no longer available") ||
                errorMessage?.includes("session may have")
                  ? `Display ${displayId} has been closed. Select another session from the dropdown above.`
                  : errorMessage || "Unable to connect to the desktop stream."}
              </p>
            </div>
            <button
              onClick={connect}
              className="flex items-center gap-2 px-4 py-2 rounded-lg bg-indigo-500 text-white text-sm font-medium hover:bg-indigo-600 transition-colors"
            >
              <RefreshCw className="w-4 h-4" />
              Reconnect
            </button>
          </div>
        )}
      </div>

      {/* Controls */}
      <div
        className={cn(
          "absolute bottom-0 left-0 right-0 z-10 p-4 bg-gradient-to-t from-black/80 to-transparent transition-opacity duration-200",
          showControls ? "opacity-100" : "opacity-0"
        )}
      >
        <div className="flex items-center justify-between gap-4">
          {/* Play/Pause */}
          <div className="flex items-center gap-2">
            <button
              onClick={isPaused ? handleResume : handlePause}
              disabled={connectionState !== "connected"}
              className={cn(
                "p-2 rounded-full transition-colors",
                connectionState === "connected"
                  ? "bg-white/10 hover:bg-white/20 text-white"
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
              className="p-2 rounded-full bg-white/10 hover:bg-white/20 text-white transition-colors"
              title="Reconnect"
            >
              <RefreshCw className="w-4 h-4" />
            </button>
          </div>

          {/* Sliders */}
          <div className="flex-1 flex items-center gap-6 max-w-md">
            <div className="flex-1 flex items-center gap-2">
              <span className="text-xs text-white/40 w-8">FPS</span>
              <input
                type="range"
                min={1}
                max={30}
                value={fps}
                onChange={(e) => handleFpsChange(Number(e.target.value))}
                className="flex-1 h-1 bg-white/20 rounded-full appearance-none cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:bg-indigo-500 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:cursor-pointer"
              />
              <span className="text-xs text-white/60 w-6 text-right tabular-nums">
                {fps}
              </span>
            </div>

            <div className="flex-1 flex items-center gap-2">
              <span className="text-xs text-white/40 w-12">Quality</span>
              <input
                type="range"
                min={10}
                max={100}
                step={5}
                value={quality}
                onChange={(e) => handleQualityChange(Number(e.target.value))}
                className="flex-1 h-1 bg-white/20 rounded-full appearance-none cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:bg-indigo-500 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:cursor-pointer"
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
