"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import { cn } from "@/lib/utils";
import { getValidJwt } from "@/lib/auth";
import { getRuntimeApiBase } from "@/lib/settings";
import {
  Monitor,
  Play,
  Pause,
  RefreshCw,
  X,
  Settings,
  Maximize2,
  Minimize2,
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

  const wsRef = useRef<WebSocket | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const connectionIdRef = useRef(0); // Guard against stale callbacks from old connections

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
          />
        ) : connectionState === "connecting" ? (
          <div className="flex flex-col items-center gap-3 text-white/60">
            <Monitor className="w-12 h-12 animate-pulse" />
            <span className="text-sm">Connecting to desktop...</span>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-3 text-white/60">
            <Monitor className="w-12 h-12 text-red-400/60" />
            <span className="text-sm text-red-400">
              {errorMessage || "Connection lost"}
            </span>
            <button
              onClick={connect}
              className="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-indigo-500 text-white text-sm hover:bg-indigo-600 transition-colors"
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
