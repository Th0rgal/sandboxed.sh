"use client";

import { useSearchParams } from "next/navigation";

import ControlClient from "./control-client";
import { HermesConversation } from "./components/HermesConversation";

/**
 * `/control` hosts two conversation kinds behind one route:
 *  - `?mission=<uuid>`  → native sandboxed.sh mission (ControlClient)
 *  - `?session=<id>`    → Hermes session (HermesConversation)
 *
 * The branch happens here, above ControlClient, so the mission monolith
 * (its stores, SSE stream, and ~60 hooks) never mounts for a Hermes view.
 */
export default function ConversationRouter() {
  const searchParams = useSearchParams();
  const hermesSessionId = searchParams.get("session");
  if (hermesSessionId) {
    return (
      <div className="flex h-[calc(100vh-3rem)] flex-col lg:h-screen">
        <HermesConversation
          key={hermesSessionId}
          sessionId={hermesSessionId}
        />
      </div>
    );
  }
  return <ControlClient />;
}
