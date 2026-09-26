import { createSignal } from "solid-js";
import { connectionVersion } from "./api";

export type PendingInteraction = { id: string; method: string };
const [requests, setRequests] = createSignal<Record<string, PendingInteraction>>({});
const keyFor = (mission: string) => `${connectionVersion()}:${mission}`;

/** The interactive request surface owns this observation, not the mission status. */
export function observeMissionInteraction(mission: string, request: PendingInteraction) {
  const key = keyFor(mission);
  setRequests(previous => ({ ...previous, [key]: request }));
  return () => setRequests(previous => {
    if (previous[key] !== request) return previous;
    const next = { ...previous };
    delete next[key];
    return next;
  });
}
export function pendingMissionInteraction(mission?: string) {
  return mission ? requests()[keyFor(mission)] : undefined;
}
