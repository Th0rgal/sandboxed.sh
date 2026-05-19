import { useSyncExternalStore } from "react";
import type { ControlRunState, Mission } from "@/lib/api";
import type { ChatItem } from "./events-reducer";

export type StreamDiagnosticsState = {
  phase: "idle" | "connecting" | "open" | "streaming" | "closed" | "error";
  url: string | null;
  status?: number;
  contentType?: string | null;
  cacheControl?: string | null;
  transferEncoding?: string | null;
  contentEncoding?: string | null;
  server?: string | null;
  via?: string | null;
  lastEventAt?: number;
  lastChunkAt?: number;
  bytes: number;
  lastError?: string | null;
};

type SetState<T> = T | ((prev: T) => T);

function createSliceStore<T>(initialValue: T) {
  let value = initialValue;
  const listeners = new Set<() => void>();

  return {
    getSnapshot: () => value,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    set: (next: SetState<T>) => {
      const resolved =
        typeof next === "function" ? (next as (prev: T) => T)(value) : next;
      if (Object.is(resolved, value)) return;
      value = resolved;
      listeners.forEach((listener) => listener());
    },
  };
}

function useSliceStore<T>(
  store: ReturnType<typeof createSliceStore<T>>,
): [T, (next: SetState<T>) => void] {
  return [useSyncExternalStore(store.subscribe, store.getSnapshot), store.set];
}

export const controlItemsStore = createSliceStore<ChatItem[]>([]);
export const controlQueueStore = createSliceStore(0);
export const controlThinkingStore = createSliceStore<{
  manuallyHidden: boolean;
  panelOpen: boolean;
}>({
  manuallyHidden: false,
  panelOpen: false,
});
export const controlStreamingDiagnosticsStore =
  createSliceStore<StreamDiagnosticsState>({
    phase: "idle",
    url: null,
    bytes: 0,
    lastError: null,
  });
export const controlViewingMissionStore = createSliceStore<{
  currentMission: Mission | null;
  viewingMission: Mission | null;
  viewingMissionId: string | null;
  runState: ControlRunState;
  runStateMissionId: string | null;
}>({
  currentMission: null,
  viewingMission: null,
  viewingMissionId: null,
  runState: "idle",
  runStateMissionId: null,
});

export function useControlItemsStore() {
  return useSliceStore(controlItemsStore);
}

export function useControlQueueStore() {
  return useSliceStore(controlQueueStore);
}

export function useControlThinkingStore() {
  return useSliceStore(controlThinkingStore);
}

export function useControlStreamingDiagnosticsStore() {
  return useSliceStore(controlStreamingDiagnosticsStore);
}

export function useControlViewingMissionStore() {
  return useSliceStore(controlViewingMissionStore);
}
