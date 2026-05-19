import type { Mission, StoredEvent } from "@/lib/api";
import { eventsToItemsImpl, type ChatItem } from "./events-reducer";

type EventsWorkerRequest = {
  id: number;
  events: StoredEvent[];
  mission?: Mission | null;
};

type EventsWorkerResponse =
  | {
      id: number;
      ok: true;
      items: ChatItem[];
    }
  | {
      id: number;
      ok: false;
      error: string;
    };

self.onmessage = (message: MessageEvent<EventsWorkerRequest>) => {
  const { id, events, mission } = message.data;
  try {
    const items = eventsToItemsImpl(events, mission);
    self.postMessage({ id, ok: true, items } satisfies EventsWorkerResponse);
  } catch (error) {
    self.postMessage({
      id,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    } satisfies EventsWorkerResponse);
  }
};
